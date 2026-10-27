#!/usr/bin/env python3
"""No chain cursor outlives its file, on FAT32 and exFAT.

    python3 scripts/test-stale-cursor.py    (or `make test-stale-cursor`, which builds the image)

fsd's FAT32 and exFAT each keep a read cursor and a write cursor, keyed by a
file's first cluster, and drop both when a FAT write rewrites a link (at
every FAT write until 2026-10-18). `test-write-walk` and
`test-spawn-reads` grade what the cursors save, and both pass with the drop
removed, since each read or write sets its cursor again at its end. What the
drop guards is a sequence no `cp` makes: a file removed, its first cluster
given to a new file on a different chain, and the new file read or written
where the old cursor was. /bin/CURSORPROBE (programs/shellutils/cursorprobe)
makes it, with a pattern of file and offset in every byte, and reads every
file back.

The image is assembled here, from the same build/exfatpart.img and
build/esp.img as build/espexfat.img, with a third partition added: an empty
exFAT of 512-byte clusters made by newfs_exfat. One boot runs the probe on:

- the exFAT root, 4 KiB clusters: the read sequence (a whole-file write is
  at most SAFECOPY_MAX, 2 KiB, half a cluster there, so the write sequences
  cannot be built and the probe says `skip`);
- the FAT32 ESP at /mnt/f, 512-byte clusters: all four;
- the small exFAT at /mnt/x: all four, so exFAT's write cursor is graded.

Each volume's cluster size is read off the disk and passed to the probe; a
volume that should take the write sequences and has clusters too large for
them fails the rig rather than passing with nothing graded.

A sequence tests a stale cursor only if the allocator laid it out the way the
probe intends, which the probe cannot see. So after the boot the host reads
the files each sequence keeps (its witnesses W1 and W2, written just before A
and just after A's last cluster, and B, G or D) and checks the layout: A lay
between the witnesses, B starts on A's first cluster (the cursor's key), and
A's cursor cluster is G's (read) or D's (write), so not B's. A change in allocation then fails the rig instead of leaving every
sequence passing with nothing tested.

Every sequence builds its stale case by removing a file: a free, then ends
of chain written (`write-one` writes nothing else before the stale use). A
rule wrong only for a truncation or a relink passes it, since fsd makes
neither today.

Also on the small exFAT, an entry the exFAT spec forbids and a tool may
leave: the host gives P (NoFatChain) a size of 0 while it keeps its cluster,
whose FAT entry it points at Q's, and the guest writes past P's first
cluster with `writeat`. Q's bytes must be unchanged and P's on a chain of
its own: since 2026-10-19 fsd reads an entry of size 0 as holding no cluster,
whatever its set names, so the write takes a new one (from 2026-10-18
ensure_chained ended the named cluster first; before that the write followed
the entry into Q, and the narrowed cursor drop could keep a cursor that
write set on a chain it had cut, the max review's finding 1).

Fails with either drop removed from either filesystem's write_fat_entry,
each on its own sequences; with FAT32's free path not lowering its
next-free hint, where the probe passes every sequence with nothing tested
and the disk check refuses each; with the drop decided on the value written
rather than the entry it replaces, which only `write-one` sees; and with
the size-0 entry's named cluster kept where its set is read, Q's bytes. KEEP_IMAGE=1 keeps build/test-stale-cursor.img for a
look. One boot, about a minute.
"""
import importlib.util
import os
import re
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import exfatimg  # noqa: E402
import mbrdisk  # noqa: E402

Volume, set_checksum = exfatimg.Volume, exfatimg.set_checksum

_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

BUILD = os.path.join(ROOT, "build")
SMALL = os.path.join(BUILD, "test-stale-cursor-x.part")
IMAGE = os.path.join(BUILD, "test-stale-cursor.img")
ABI = os.path.join(ROOT, "syscall-abi", "src", "lib.rs")
READ_CLUSTERS = 4  # the probe's READ_CLUSTERS
SEQUENCES = (("read", "R"), ("write-in-place", "I"), ("write-append", "P"), ("write-one", "O"))


def safecopy_max():
    """SAFECOPY_MAX as syscall-abi defines it, the bound the probe's sequences are built on."""
    with open(ABI) as fh:
        m = re.search(r"^pub const SAFECOPY_MAX: u64 = ([0-9_]+);", fh.read(), re.M)
    if not m:
        sys.exit("test-stale-cursor: SAFECOPY_MAX not found in syscall-abi")
    return int(m.group(1).replace("_", ""))


def small_exfat():
    """An 8 MiB exFAT of 512-byte clusters (newfs_exfat -c 1) at SMALL, holding
    P and Q as macOS writes them (contiguous, NoFatChain), then P made the
    entry `plant_empty` describes."""
    with open(SMALL, "wb") as fh:
        fh.truncate(8 * 1024 * 1024)
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", SMALL],
                         capture_output=True, text=True)
    if att.returncode != 0:
        sys.exit(f"test-stale-cursor: hdiutil attach failed: {att.stderr.strip()}")
    dev = att.stdout.split()[0]
    failed = None
    try:
        mk = subprocess.run(["newfs_exfat", "-c", "1", "-v", "CURSORX", dev], capture_output=True, text=True)
        if mk.returncode != 0:
            failed = f"newfs_exfat failed: {mk.stderr.strip()}"
        else:
            failed = place_pq(dev)
    finally:
        det = subprocess.run(["hdiutil", "detach", dev], capture_output=True, text=True)
    if failed:
        sys.exit(f"test-stale-cursor: {failed}")
    if det.returncode != 0:
        sys.exit(f"test-stale-cursor: hdiutil detach {dev} failed: {det.stderr.strip()}")
    plant_empty()


P_BYTES = b"p" * 100
Q_BYTES = bytes((i * 13 + 5) & 0xFF for i in range(512))
P_WRITES = ((0, "aaa"), (600, "bbb"))  # the guest's writeats into P


def place_pq(dev):
    """P and Q written through macOS on `dev`; None, or what failed."""
    if subprocess.run(["diskutil", "mount", dev], capture_output=True).returncode != 0:
        return f"diskutil mount {dev} failed"
    info = subprocess.run(["diskutil", "info", dev], capture_output=True, text=True).stdout
    mp = re.search(r"Mount Point:\s*(\S.*)$", info, re.M)
    try:
        if not mp:
            return f"no mount point for {dev}"
        for name, data in (("P", P_BYTES), ("Q", Q_BYTES)):
            with open(os.path.join(mp.group(1), name), "wb") as fh:
                fh.write(data)
        subprocess.run(["find", mp.group(1), "-name", "._*", "-delete"], capture_output=True)
        for junk in (".fseventsd", ".Trashes", ".Spotlight-V100"):
            subprocess.run(["rm", "-rf", os.path.join(mp.group(1), junk)], capture_output=True)
    finally:
        subprocess.run(["diskutil", "unmount", dev], capture_output=True)
    return None


def plant_empty():
    """P made a NoFatChain entry of size 0 that still names its cluster, whose
    FAT entry is set to Q's cluster: the case the spec forbids (DataLength > 0
    with NoFatChain) and a tool may leave. A write to P past its first cluster
    then followed that entry into Q until 2026-10-18, when ensure_chained came
    to end the cluster first, and since 2026-10-19 the entry holds no cluster
    at all; the guest's writeats make that write."""
    with open(SMALL, "r+b") as fh:
        b = fh.read(512)
        sector = 1 << b[108]
        fat, _, heap, _, root = struct.unpack_from("<IIIII", b, 80)
        cluster = sector << b[109]
        fh.seek(heap * sector + (root - 2) * cluster)
        d = bytearray(fh.read(cluster))
        found = {}
        i = 0
        while i < len(d) and d[i]:
            count = d[i + 1] + 1 if d[i] == 0x85 else 1
            if d[i] == 0x85:
                stream = d[i + 32:i + 64]
                name = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
                found[name.decode("utf-16le")[:stream[3]]] = (i, count)
            i += 32 * count
        if set(found) < {"P", "Q"}:
            sys.exit(f"test-stale-cursor: macOS did not leave P and Q in the small exFAT's root: {sorted(found)}")
        firsts = {}
        for name, (at, _) in found.items():
            stream = d[at + 32:at + 64]
            if name in ("P", "Q") and not stream[1] & 0x02:
                sys.exit(f"test-stale-cursor: macOS wrote {name} FAT-chained, not NoFatChain: nothing to plant")
            firsts[name] = struct.unpack_from("<I", stream, 20)[0]
        at, count = found["P"]
        struct.pack_into("<Q", d, at + 32 + 8, 0)   # ValidDataLength
        struct.pack_into("<Q", d, at + 32 + 24, 0)  # DataLength
        struct.pack_into("<H", d, at + 2, set_checksum(d[at:at + 32 * count]))
        fh.seek(heap * sector + (root - 2) * cluster)
        fh.write(d)
        fh.seek(fat * sector + firsts["P"] * 4)
        fh.write(struct.pack("<I", firsts["Q"]))


def layout(vol, d, seq, sub, k):
    """The problems with the stale case `seq` built in `d`/`sub`, or [] when
    it is laid out as the probe intends (see the module doc).

    Both allocators hand out free clusters in ascending order (FAT32 from its
    hint, lowered to the lowest cluster freed; exFAT from the bitmap's first
    free bit), and nothing below W1 was free when it was written. So A's
    clusters, with F's in the write sequences, were the free clusters
    between W1 and W2, in ascending order, and once A and F are removed B and
    G or D take those clusters back first. Every cluster between the
    witnesses is then B's or the other file's, in A's interleaved order, or
    in use by something that was there before: a free one means the model
    does not describe the run."""
    def chain(name):
        return vol.file_chain(f"{d}/{sub}/{name}")

    w1, w2, b = chain("W1"), chain("W2"), chain("B")
    other, oname = (chain("G"), "G") if seq == "read" else (chain("D"), "D")
    problems = []
    if len(w1) != 1 or len(w2) != 1:
        return ["a witness is not one cluster", f"chains: W1 {w1}, W2 {w2}"]
    mine = set(b) | set(other)
    between = range(w1[0] + 1, w2[0])
    taken = [c for c in between if c in mine]  # A's and F's clusters, in allocation order
    # A's clusters, with F's: in allocation order all of A's in `read` and
    # `write-one`, and A0 F0 A1 F1 ... F(k-1) A(k-1) in the other two. A's
    # last, its cursor cluster, is the last taken in each.
    span = {"read": READ_CLUSTERS, "write-one": 2}.get(seq, 2 * k)
    free = [c for c in between if c not in mine and not vol.used(c)]
    if free:
        problems.append(f"clusters {free[:8]} between the witnesses are free: A and F did not take "
                        "every free cluster between them, so where A lay is not known")
    elif len(taken) != span:
        problems.append(f"{len(taken)} clusters between the witnesses are B's or {oname}'s, not {span}: "
                        "A and F did not lie between them, or B and the other file did not take them back")
    else:
        cursor = taken[-1]
        if b[0] != taken[0]:
            problems.append(f"B starts at cluster {b[0]}, not A's first ({taken[0]}): "
                            "the old cursor's key never matches")
        elif set(b) & set(other):
            problems.append(f"B and {oname} share clusters {sorted(set(b) & set(other))}: a cross-link")
        elif cursor not in other:
            # In the other file's chain, and the two are disjoint, so not B's.
            # Only the setup is judged here, never what the stale use did.
            problems.append(f"A's cursor cluster ({cursor}) is not in {oname}'s chain: the old cursor is not stale")
    if problems:
        problems.append(f"chains: W1 {w1}, W2 {w2}, B {b}, {oname} {other}")
    return problems


def grade(where, vol, d, writes, out, bound):
    """The checks for one run of the probe: its lines in `out`, then each
    sequence it ran checked on the disk."""
    lines = [ln.rstrip() for ln in re.findall(r"^cursorprobe: .*$", out, re.M)]
    for ln in lines:
        print(f"     {where}: {ln}")
    checks = []
    k = bound // vol.cluster
    if writes:
        checks.append((f"{where}: clusters of {vol.cluster} bytes take the write sequences "
                       f"(at most {bound // 2})", k >= 2))
    for seq, sub in SEQUENCES:
        # The probe's own rules: write-in-place and write-append need a
        # whole-file write of two clusters, write-one of one.
        runs = {"write-in-place": k >= 2, "write-append": k >= 2, "write-one": vol.cluster <= bound}.get(seq, True)
        want = "ok" if runs else "skip"
        said = [ln for ln in lines if re.match(rf"cursorprobe: (ok|skip|FAIL) {seq}\b", ln)]
        checks.append((f"{where}: {want} {seq}", len(said) == 1 and said[0].startswith(f"cursorprobe: {want} {seq}")))
        if want == "ok":
            try:
                problems = layout(vol, d, seq, sub, k)
            except ValueError as e:
                problems = [str(e)]
            checks.append((f"{where}: {seq} built the stale case on the disk", not problems))
            for p in problems:
                print(f"     {where}: {seq}: {p}")
    checks.append((f"{where}: the probe's summary is ok", lines[-1:] == ["cursorprobe: ok"]))
    return checks


def empty_entry(vol):
    """The checks of the planted entry on the small exFAT after the guest's
    writeats: Q's bytes as macOS wrote them, and P its own bytes on a chain of
    its own (the cluster it named, ended, then one allocated)."""
    names = vol.listing(vol.root)
    pf, psize, pchained = names.get("P", (0, 0, False))
    qf, _, _ = names.get("Q", (0, 0, False))
    end = max(o + len(t) for o, t in P_WRITES)
    want = bytearray(end)
    for o, t in P_WRITES:
        want[o:o + len(t)] = t.encode()
    try:
        chain = vol.chain(pf) if pchained else [pf]
        q = vol.data([qf])[:len(Q_BYTES)]
        p = vol.data(chain)[:end]
    except ValueError as e:
        return [("exfat-512 /mnt/x: the planted entry's files read off the disk", False)], [str(e)]
    notes = [f"P: first cluster {pf}, {psize} bytes, chain {chain}; Q: cluster {qf}"]
    return [
        ("exfat-512 /mnt/x: a write past a size-0 NoFatChain file's cluster leaves Q's bytes as they were",
         q == Q_BYTES),
        ("exfat-512 /mnt/x: P holds its own bytes on a chain of its own, Q's cluster not in it",
         pchained and psize == end and p == bytes(want) and qf not in chain),
    ], notes


def main() -> int:
    bound = safecopy_max()
    try:
        small_exfat()
        parts = mbrdisk.assemble(IMAGE, [
            (0x07, mbrdisk.exfat_payload(os.path.join(BUILD, "exfatpart.img")), False),
            (0x0B, mbrdisk.esp_payload(os.path.join(BUILD, "esp.img")), True),
            (0x07, mbrdisk.exfat_payload(SMALL), False),
        ])
    finally:
        if os.path.exists(SMALL):
            os.remove(SMALL)
    try:
        vols = [Volume(IMAGE, start) for start, _ in parts]
        # (where, directory, volume, whether the write sequences are graded)
        runs = [("exfat root", "/cprobe", vols[0], False),
                ("fat32 /mnt/f", "/mnt/f/cprobe", vols[1], True),
                ("exfat-512 /mnt/x", "/mnt/x/cprobe", vols[2], True)]
        steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f"), ("# ", "mount 2 /mnt/x")]
        prev = "# "
        for _, d, v, _ in runs:
            steps.append((prev, f"cursorprobe {d} {v.cluster}"))
            prev = r"cursorprobe: (ok|FAIL)\r?\n"
        for offset, text in P_WRITES:
            steps.append((prev, f"writeat /mnt/x/P {offset} {text}"))
            prev = "# "
        steps.append((prev, ""))
        # The image itself is what the stale-image guard reads: the ESP's
        # stamp and the exFAT root's /etc/srcid, both in the bytes that boot.
        guest = drive_qemu.Guest(IMAGE, label="test-stale-cursor: ")
        try:
            driven = guest.run(steps)
            out = guest.transcript().replace("\r\n", "\n")
            faults = guest.aborts()
        finally:
            guest.stop()
        with open(os.path.join(BUILD, "test-stale-cursor.txt"), "w") as fh:
            fh.write(out)

        checks = [("driven to the end", driven), ("no fault lines", faults == 0)]
        # Each run's lines: from its command to the next one's. The volumes
        # are read again after the boot, the guest's writes now on the disk.
        pieces = re.split(r"cursorprobe (/\S+) \d+\n", out)
        by_dir = dict(zip(pieces[1::2], pieces[2::2]))
        vols = [Volume(IMAGE, start) for start, _ in parts]
        for (where, d, _, writes), vol in zip(runs, vols):
            local = d.split("/", 3)[-1] if d.startswith("/mnt/") else d
            checks += grade(where, vol, local, writes, by_dir.get(d, ""), bound)
        more, notes = empty_entry(vols[2])
        checks += more
        for n in notes:
            print(f"     exfat-512 /mnt/x: {n}")
    finally:
        if os.environ.get("KEEP_IMAGE") != "1":
            os.remove(IMAGE)
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcript: build/test-stale-cursor.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

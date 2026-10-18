#!/usr/bin/env python3
"""No chain cursor outlives its file, on FAT32 and exFAT.

    python3 scripts/test-stale-cursor.py    (or `make test-stale-cursor`, which builds the image)

fsd's FAT32 and exFAT each keep a read cursor and a write cursor, keyed by a
file's first cluster, and drop both at every FAT write. `test-write-walk` and
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
  at most SAFECOPY_MAX, 2 KiB, one cluster there, so the write sequences
  cannot be built and the probe says `skip`);
- the FAT32 ESP at /mnt/f, 512-byte clusters: all three;
- the small exFAT at /mnt/x: all three, so exFAT's write cursor is graded.

Each volume's cluster size is read off the disk and passed to the probe; a
volume that should take the write sequences and has clusters too large for
them fails the rig rather than passing with nothing graded.

A sequence tests a stale cursor only if the allocator laid it out the way the
probe intends, which the probe cannot see. So after the boot the host reads
the files each sequence keeps (its witnesses W1 and W2, written just before A
and just after A's last cluster, and B, G or D) and checks the layout: A lay
between the witnesses, B starts on A's first cluster (the cursor's key), and
A's cursor cluster is not B's at that position but G's (read) or D's
(write). A change in allocation then fails the rig instead of leaving every
sequence passing with nothing tested.

Fails with either drop removed from either filesystem's write_fat_entry,
each on its own sequences. One boot, about a minute.
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
import mbrdisk  # noqa: E402

_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

BUILD = os.path.join(ROOT, "build")
SMALL = os.path.join(BUILD, "test-stale-cursor-x.part")
IMAGE = os.path.join(BUILD, "test-stale-cursor.img")
ABI = os.path.join(ROOT, "syscall-abi", "src", "lib.rs")
READ_CLUSTERS = 4  # the probe's READ_CLUSTERS
SEQUENCES = (("read", "R"), ("write-in-place", "I"), ("write-append", "P"))


def safecopy_max():
    """SAFECOPY_MAX as syscall-abi defines it, the bound the probe's sequences are built on."""
    with open(ABI) as fh:
        m = re.search(r"^pub const SAFECOPY_MAX: u64 = ([0-9_]+);", fh.read(), re.M)
    if not m:
        sys.exit("test-stale-cursor: SAFECOPY_MAX not found in syscall-abi")
    return int(m.group(1).replace("_", ""))


def small_exfat():
    """An empty 8 MiB exFAT of 512-byte clusters (newfs_exfat -c 1) at SMALL."""
    with open(SMALL, "wb") as fh:
        fh.truncate(8 * 1024 * 1024)
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", SMALL],
                         capture_output=True, text=True)
    if att.returncode != 0:
        sys.exit(f"test-stale-cursor: hdiutil attach failed: {att.stderr.strip()}")
    dev = att.stdout.split()[0]
    try:
        mk = subprocess.run(["newfs_exfat", "-c", "1", "-v", "CURSORX", dev], capture_output=True, text=True)
    finally:
        det = subprocess.run(["hdiutil", "detach", dev], capture_output=True, text=True)
    if mk.returncode != 0:
        sys.exit(f"test-stale-cursor: newfs_exfat failed: {mk.stderr.strip()}")
    if det.returncode != 0:
        sys.exit(f"test-stale-cursor: hdiutil detach {dev} failed: {det.stderr.strip()}")


class Volume:
    """A FAT32 or exFAT partition of a disk image as the host reads it: the
    cluster size, chains from the first FAT, and paths of live entries."""

    def __init__(self, image, start_lba):
        self.fh = open(image, "rb")
        base = start_lba * 512
        self.fh.seek(base)
        b = self.fh.read(512)
        self.exfat = b[3:11] == b"EXFAT   "
        if self.exfat:
            sector = 1 << b[108]
            fat, _, heap, self.count, self.root = struct.unpack_from("<IIIII", b, 80)
            self.cluster = sector << b[109]
            self.end = 0xFFFFFFF7
        else:
            sector = struct.unpack_from("<H", b, 11)[0]
            reserved, nfats = struct.unpack_from("<H", b, 14)[0], b[16]
            fatsz = struct.unpack_from("<I", b, 36)[0]
            self.root = struct.unpack_from("<I", b, 44)[0]
            fat, heap = reserved, reserved + nfats * fatsz
            self.count = fatsz * sector // 4
            self.cluster = b[13] * sector
            self.end = 0x0FFFFFF8
        self.fat = base + fat * sector
        self.heap = base + heap * sector
        if self.exfat:
            # The allocation bitmap's first cluster, from the root's 0x81 entry
            # (assumed contiguous, as fsd's exfat.rs assumes it).
            d = self.data(self.chain(self.root))
            hits = [struct.unpack_from("<I", d, i + 20)[0] for i in range(0, len(d), 32) if d[i] == 0x81]
            if not hits:
                raise ValueError("no allocation bitmap in the exFAT root")
            self.bitmap = hits[0]

    def used(self, c):
        """Whether cluster `c` is allocated: its FAT entry on FAT32, its
        allocation bitmap bit on exFAT (a contiguous file leaves its FAT
        entries 0)."""
        if not self.exfat:
            return self.entry(c) != 0
        self.fh.seek(self.heap + (self.bitmap - 2) * self.cluster + (c - 2) // 8)
        return bool(self.fh.read(1)[0] >> ((c - 2) % 8) & 1)

    def entry(self, c):
        self.fh.seek(self.fat + c * 4)
        v = struct.unpack("<I", self.fh.read(4))[0]
        return v if self.exfat else v & 0x0FFFFFFF

    def chain(self, first):
        out = [first]
        while 2 <= self.entry(out[-1]) < self.end:
            if len(out) > self.count:
                raise ValueError(f"the chain from cluster {first} is longer than the volume: a loop")
            out.append(self.entry(out[-1]))
        return out

    def data(self, chain):
        out = b""
        for c in chain:
            self.fh.seek(self.heap + (c - 2) * self.cluster)
            out += self.fh.read(self.cluster)
        return out

    def listing(self, first, chained=True):
        """{name: (first cluster, size, chained)} of the live entries of a directory."""
        d = self.data(self.chain(first) if chained else [first])
        out = {}
        if not self.exfat:
            for j in range(0, len(d), 32):
                e = d[j:j + 32]
                if e[0] == 0:
                    break
                if e[0] != 0xE5 and e[11] != 0x0F:
                    base, ext = e[:8].decode("latin1").rstrip(), e[8:11].decode("latin1").rstrip()
                    out[base + ("." + ext if ext else "")] = (
                        (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0],
                        struct.unpack_from("<I", e, 28)[0], True)
            return out
        i = 0
        while i < len(d) and d[i]:
            count = d[i + 1] + 1 if d[i] == 0x85 else 1
            if d[i] == 0x85:
                stream = d[i + 32:i + 64]
                name = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
                out[name.decode("utf-16le")[:stream[3]]] = (
                    struct.unpack_from("<I", stream, 20)[0], struct.unpack_from("<Q", stream, 24)[0],
                    not stream[1] & 0x02)
            i += 32 * count
        return out

    def file_chain(self, path):
        """`path`'s chain, its parts matched without case. Raises ValueError for
        a missing part or a contiguous file (fsd writes every file chained)."""
        first, chained = self.root, True
        for part in path.strip("/").split("/"):
            names = {k.upper(): v for k, v in self.listing(first, chained).items()}
            if part.upper() not in names:
                raise ValueError(f"no {path} on the disk")
            first, _, chained = names[part.upper()]
        if not chained:
            raise ValueError(f"{path} is contiguous: fsd writes every file FAT-chained")
        return self.chain(first)


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
    span = READ_CLUSTERS if seq == "read" else 2 * k
    # A's clusters in that order: all of them in `read`; in the write
    # sequences A0 F0 A1 F1 ... F(k-1) A(k-1), so A's last is the last taken.
    index = READ_CLUSTERS - 1 if seq == "read" else k - 1  # A's cursor cluster's position in A, and in B
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
        elif len(b) <= index or b[index] == cursor:
            problems.append(f"B's cluster {index} is A's cursor cluster ({cursor}): the old cursor is not stale")
        elif cursor not in other:
            problems.append(f"A's cursor cluster ({cursor}) is not in {oname}'s chain")
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
        want = "ok" if writes or seq == "read" else "skip"
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

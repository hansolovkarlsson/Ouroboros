#!/usr/bin/env python3
"""A FAT batch the disk refuses: its clusters come back by their bits, no cursor outlives its links.

    python3 scripts/test-batch-refused.py    (or `make test-batch-refused`, which builds the images)

exFAT sets a request's FAT entries in its FAT cache and writes them a sector
at a time (`in_fat_batch`, 2026-10-31). Two defects followed when the disk
refused one of those writes, both found by the medium reviews of #380:

- HEAD: a whole-file write's first cluster keeps its end of chain in the cache
  until the batch's first flush. Refused, the cluster's FAT entry on the disk
  is what it was before, and `write_chain`'s give-back walked from it, so a
  link another driver left in a free cluster's entry took the walk into a live
  file, whose clusters it freed. On a volume of 512-byte clusters a 2 KiB
  write walks four. The batch then said its entries were lost
  (`BatchFailed`), and the give-back leaked the chain instead; since
  2026-11-01 a failed batch gives back the clusters its allocator took by
  their bits alone (`give_back`), walking nothing.
- CURSOR: `write_at` sets its write cursor inside the batch, before the
  batch's last flush. Refused, the cursor named a cluster the file's link to
  never reached the disk, and a retried append resumed there: its bytes went
  into a cluster the chain on the disk does not hold. `flush_fat` now drops
  the cursors with the cache it drops.
- GAP (2026-11-01): `write_at` links clusters past a file's size as its
  batch goes, and when a data write was refused part way it gave back none
  of them (a leak older than the batch, the high review of #380).

No rig made a FAT write fail before this one. One boot: the firmware boots
build/esp.img from NVMe (which the kernel cannot drive), and virtio holds a
disk of three partitions (the exFAT root, the ESP, and an 8 MiB exFAT of
512-byte clusters made by newfs_exfat, X) through QEMU's blkdebug, with the
first write of each of two FAT sectors of X failing EIO (`once`). On X the
host plants:

- L, 2 KiB written by macOS (four clusters, contiguous);
- A, one cluster, FAT-chained, set by hand far from the clusters the boot
  allocates, so its FAT entry is in a sector of its own, SA;
- in the FAT entry of h, the first free cluster (the allocator's first find),
  a link to L's first cluster, as a driver may leave a freed cluster's entry.

h's FAT sector, SH, and SA fail their first write, and so does the data
sector of h+4, SD. Then, logged in, with X at /mnt/x:

- `cursorprobe whole /mnt/x/H 2048`: four clusters from h, their entries in
  SH, refused at the batch's end: the probe must say an I/O error, L must
  keep every bit, its bytes and its first cluster (no give-back walked h's
  planted link), no H in X's root, and h to h+3 must come back: the next
  three commands take them again, h by A's append, h+1 by W and h+2 and h+3
  by the gap fill, which the host reads off the disk after the boot;
- `echo x >> /mnt/x/A`: the append's link from A's cluster, in SA, refused
  at the batch's end: an I/O error, the cluster it took (h) given back;
- `echo x >> /mnt/x/A` again: answered, and A then FAT-chained over two
  clusters, its first as planted and its second h, holding its bytes and
  `x\\r\\n`;
- `echo w > /mnt/x/W` (one cluster, h+1), then `writeat /mnt/x/W 3000 y`:
  five clusters past W's one, linked as the gap fill goes, the fill's write
  to h+4 refused (SD): an I/O error, W's size and bytes as before, W's chain
  cut back to h+1 (its end of chain written again, or fsck_exfat finds the
  chain too long for the size and its clusters free) and h+2 to h+6 free in
  the bitmap, the links h+2 to h+4 left in free clusters.

Every plant is read back on the host before the boot, and fsck_exfat -n
passes X then and after the boot (nothing leaks; until 2026-11-01 HEAD's
four clusters did, and fsck was not asked). Fails with the give-back not
called (A's second cluster not h, W not on h+1, the fill's clusters
allocated, fsck's lost clusters), with W's end of chain not written back
(W's chain, and fsck's too many clusters) and with the cursors kept by both
flush_fat and the give-back (A's chain one cluster for its 515 bytes; either
drop alone passes, since 2026-11-01 each covers a refused batch). A batch of more runs than
fsd records (32), whose finds past them leak, and a refused write of the end
of chain, after which nothing is given back, are reached by no boot. A boot
QEMU's firmware never starts is INCONCLUSIVE (exit 100). About a minute.
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


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
Volume = exfatimg.Volume

RIG = "test-batch-refused"
BUILD = os.path.join(ROOT, "build")
SMALL = os.path.join(BUILD, f"{RIG}-x.part")
IMAGE = os.path.join(BUILD, f"{RIG}.img")
BLKDEBUG = os.path.join(BUILD, f"{RIG}.blkdebug")
ESP = os.path.join(BUILD, "esp.img")
SECTOR = 512
L_BYTES = bytes((i * 29 + 3) & 0xFF for i in range(2048))
A_BYTES = bytes((i * 17 + 11) & 0xFF for i in range(512))
A_AT = 1000  # A's cluster: its FAT entry in sector 7 of X's FAT, past every cluster the boot takes
# D, a directory in the root holding V and the directory E, which holds R:
# their entries in FAT sector 8, past every cluster the boot takes. E is in
# D, not the root, since the root's one sector holds 16 entries and a
# fourth planted set there would have the guest's W grow it.
D_AT, V_AT, E_AT, R_AT = 1100, 1101, 1102, 1103
V_BYTES, R_BYTES = b"v\r\n", b"r\r\n"
END = 0xFFFFFFFF


def hand_set(name, first, size, is_dir=False):
    """A FAT-chained entry set made by hand: a file of `size` bytes or a
    directory of one cluster, on `first`."""
    s = bytearray(exfatimg.entry_set(name))
    if is_dir:
        s[4] = 0x10  # FileAttributes: Directory
    s[33] = 0x01  # GeneralSecondaryFlags: AllocationPossible, FAT-chained
    struct.pack_into("<Q", s, 40, size)
    struct.pack_into("<I", s, 52, first)
    struct.pack_into("<Q", s, 56, size)
    struct.pack_into("<H", s, 2, exfatimg.set_checksum(s))
    return bytes(s)


def make_small():
    """X: an 8 MiB exFAT of 512-byte clusters with L written by macOS."""
    with open(SMALL, "wb") as fh:
        fh.truncate(8 * 1024 * 1024)
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", SMALL],
                         capture_output=True, text=True)
    if att.returncode != 0:
        return f"hdiutil attach failed: {att.stderr.strip()}"
    dev = att.stdout.split()[0]
    try:
        mk = subprocess.run(["newfs_exfat", "-c", "1", "-v", "BATCHX", dev], capture_output=True, text=True)
        if mk.returncode != 0:
            return f"newfs_exfat failed: {mk.stderr.strip()}"
        if subprocess.run(["diskutil", "mount", dev], capture_output=True).returncode != 0:
            return f"diskutil mount {dev} failed"
        info = subprocess.run(["diskutil", "info", dev], capture_output=True, text=True).stdout
        mp = re.search(r"Mount Point:\s*(\S.*)$", info, re.M)
        try:
            if not mp:
                return f"no mount point for {dev}"
            with open(os.path.join(mp.group(1), "L"), "wb") as fh:
                fh.write(L_BYTES)
            subprocess.run(["find", mp.group(1), "-name", "._*", "-delete"], capture_output=True)
            for junk in (".fseventsd", ".Trashes", ".Spotlight-V100"):
                subprocess.run(["rm", "-rf", os.path.join(mp.group(1), junk)], capture_output=True)
        finally:
            subprocess.run(["diskutil", "unmount", dev], capture_output=True)
    finally:
        subprocess.run(["hdiutil", "detach", dev], capture_output=True)
    return None


def plant(start):
    """A set by hand on its own cluster, chained, and h's FAT entry linked to
    L's first cluster. Answers (h, L's first cluster, the FAT's first
    sector and the heap's first sector in the image) or a reason it could not."""
    with Volume(IMAGE, start) as vol:
        if vol.cluster != SECTOR:
            return f"X's clusters are {vol.cluster} bytes, not 512"
        listed = {exfatimg.fold(k): v for k, v in vol.listing(vol.root).items()}
        if "l" not in listed:
            return "macOS left no L on X"
        l0 = listed["l"][0]
        if any(vol.used(c) for c in (A_AT,)) or vol.entry(A_AT) != 0:
            return f"cluster {A_AT} is not free for A"
        rchain = vol.chain(vol.root)
        d = bytearray(vol.data(rchain))
        end = exfatimg.root_end(d)
        fat_byte, heap_byte, bitmap = vol.fat, vol.heap, vol.bitmap
        if any(vol.used(c) or vol.entry(c) != 0 for c in (D_AT, V_AT, E_AT, R_AT)):
            return f"clusters {D_AT} to {R_AT} are not free for D, V, E and R"
    # The root must not grow, or every cluster the rig names moves. Its one
    # sector holds the volume's three entries, L's set, and two sets macOS
    # deleted (its own files'): A goes after them, D into the second
    # deleted run, and W, which the guest makes, into the first, where fsd
    # reuses deleted slots.
    a = hand_set("A", A_AT, len(A_BYTES))
    if end + len(a) + 32 > len(d):
        return "no room in X's root for A's set"
    d[end:end + len(a)] = a
    runs = [i for i in range(0, end - 64, 32)
            if all(d[j] & 0x80 == 0 and d[j] != 0 for j in (i, i + 32, i + 64))
            and (i == 0 or d[i - 32] & 0x80)]
    if len(runs) < 2:
        return f"X's root has {len(runs)} runs of three deleted slots, not two (for W and D)"
    d[runs[-1]:runs[-1] + 96] = hand_set("D", D_AT, SECTOR, is_dir=True)
    dsec = (hand_set("V", V_AT, len(V_BYTES)) + hand_set("E", E_AT, SECTOR, is_dir=True)).ljust(SECTOR, b"\0")
    esec = hand_set("R", R_AT, len(R_BYTES)).ljust(SECTOR, b"\0")
    with open(IMAGE, "r+b") as fh:
        for k, c in enumerate(rchain):
            fh.seek(heap_byte + (c - 2) * SECTOR)
            fh.write(d[k * SECTOR:(k + 1) * SECTOR])
        for c, data in ((A_AT, A_BYTES), (D_AT, dsec), (V_AT, V_BYTES), (E_AT, esec), (R_AT, R_BYTES)):
            fh.seek(heap_byte + (bitmap - 2) * SECTOR + (c - 2) // 8)
            b = fh.read(1)[0]
            fh.seek(-1, 1)
            fh.write(bytes([b | 1 << (c - 2) % 8]))
            fh.seek(fat_byte + c * 4)
            fh.write(struct.pack("<I", END))
            fh.seek(heap_byte + (c - 2) * SECTOR)
            fh.write(data.ljust(SECTOR, b"\0"))
    with Volume(IMAGE, start) as vol:
        h = next(c for c in range(2, vol.count + 2) if not vol.used(c))
    with open(IMAGE, "r+b") as fh:
        fh.seek(fat_byte + h * 4)
        fh.write(struct.pack("<I", l0))
    return h, l0, fat_byte // SECTOR, heap_byte // SECTOR


def answer(out, cmd, nth=0):
    """The lines after the `nth` typing of `cmd`, to the next prompt."""
    marker = "# " + cmd + "\r"
    pieces = out.split(marker)
    if len(pieces) <= nth + 1:
        return None
    lines = pieces[nth + 1].split("# ", 1)[0].splitlines()
    return "\n".join(ln for ln in lines if not ln.startswith("Ouroboros kernel:")).strip()


def run():
    why = make_small()
    if why:
        return [(f"before: {why}", False)], [], 0
    try:
        parts = mbrdisk.assemble(IMAGE, [
            (0x07, mbrdisk.exfat_payload(os.path.join(BUILD, "exfatpart.img")), False),
            (0x0B, mbrdisk.esp_payload(ESP), True),
            (0x07, mbrdisk.exfat_payload(SMALL), False),
        ])
    finally:
        os.remove(SMALL)
    start = parts[2][0]
    planted = plant(start)
    if isinstance(planted, str):
        return [(f"before: {planted}", False)], [], 0
    h, l0, fat_lba, heap_lba = planted
    sh, sa = fat_lba + h * 4 // SECTOR, fat_lba + A_AT * 4 // SECTOR
    sd = heap_lba + h + 4 - 2  # h+4's one sector: GAP's fill refused there
    sv, se = heap_lba + D_AT - 2, heap_lba + E_AT - 2  # D's and E's one sector: SET's and REPLACE's sets
    with Volume(IMAGE, start) as vol:
        listed = {exfatimg.fold(k): v for k, v in vol.listing(vol.root).items()}
        l_ok = listed.get("l", (None,))[0] == l0 and vol.data(list(range(l0, l0 + 4))) == L_BYTES
        a_ok = (listed.get("a") == (A_AT, len(A_BYTES), True) and vol.chain(A_AT) == [A_AT]
                and vol.data([A_AT]) == A_BYTES and vol.used(A_AT))
        h_ok = not vol.used(h) and vol.entry(h) == l0 and all(not vol.used(c) for c in range(h, h + 8))
        dl = {exfatimg.fold(k): v for k, v in vol.listing(D_AT).items()}
        el = {exfatimg.fold(k): v for k, v in vol.listing(E_AT).items()}
        dve_ok = (listed.get("d") == (D_AT, SECTOR, True) and dl.get("v") == (V_AT, 3, True)
                  and dl.get("e") == (E_AT, SECTOR, True) and el.get("r") == (R_AT, 3, True)
                  and vol.data([V_AT])[:3] == V_BYTES and vol.data([R_AT])[:3] == R_BYTES)
    ok, lines = exfatimg.fsck_clean(IMAGE, part="s3")
    checks = [
        (f"before: L on X at {l0}, four clusters, its bytes", l_ok),
        (f"before: A chained on cluster {A_AT} alone, its bytes, its bit set", a_ok),
        (f"before: h ({h}) and the seven after it free, h's entry linking to L's first cluster", h_ok),
        (f"before: D on {D_AT} holding V ({V_AT}) and E ({E_AT}), E holding R ({R_AT}), their bytes", dve_ok),
        (f"before: SH ({sh}) and SA ({sa}) are two sectors, h to h+4 all in SH",
         sh != sa and all(fat_lba + c * 4 // SECTOR == sh for c in range(h, h + 5))),
        ("before: fsck_exfat -n passes X with the plants", ok),
    ]
    notes = [f"before: {ln}" for ln in lines] if not ok else []
    if not all(c[1] for c in checks):
        return checks, notes, 0

    with open(BLKDEBUG, "w") as fh:
        for sector in (sh, sa, sd, sv, se):
            fh.write(f'[inject-error]\nevent = "write_aio"\niotype = "write"\nerrno = "5"\n'
                     f'sector = "{sector}"\nonce = "on"\n\n')
    cmds = ["mount 2 /mnt/x", "cursorprobe whole /mnt/x/H 2048", "echo x >> /mnt/x/A", "echo x >> /mnt/x/A",
            "echo w > /mnt/x/W", "writeat /mnt/x/W 3000 y", "writeat /mnt/x/D/V 3000 y", "write /mnt/x/D/E/R zzz"]
    steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in cmds] + [("# ", "")]
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, which it boots and
        # the kernel cannot drive, so the virtio disk goes in as the fallback.
        "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
        "-drive", f"file=blkdebug:{BLKDEBUG}:{IMAGE},format=raw,if=none,id=hd0,werror=report",
        "-device", "virtio-blk-device,drive=hd0",
    ]
    guest = drive_qemu.Guest(IMAGE, extra_args=extra, virtio_disk=False, stamp_from=ESP, label=f"{RIG}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        if os.path.exists(BLKDEBUG):
            os.remove(BLKDEBUG)
    with open(os.path.join(BUILD, f"{RIG}.txt"), "w") as fh:
        fh.write(out)
    if drive_qemu.boot_stalled(RIG, "boot", out, faults):
        return checks, notes, 1

    probe = answer(out, cmds[1]) or ""
    first, second = answer(out, cmds[2], 0) or "", answer(out, cmds[2], 1)
    made_w, gap = answer(out, cmds[4]), answer(out, cmds[5]) or ""
    set_answer, replace_answer = answer(out, cmds[6]) or "", answer(out, cmds[7]) or ""
    with Volume(IMAGE, start) as vol:
        listed = {exfatimg.fold(k): v for k, v in vol.listing(vol.root).items()}
        l_kept = (listed.get("l", (None,))[0] == l0 and all(vol.used(c) for c in range(l0, l0 + 4))
                  and vol.data(list(range(l0, l0 + 4))) == L_BYTES)
        a = listed.get("a")
        a_chain = vol.chain(A_AT) if a and a[2] else []
        a_data = vol.data(a_chain)[:a[1]] if a else b""
        w = listed.get("w")
        w_data = vol.data([w[0]])[:w[1]] if w and w[0] >= 2 else b""
        freed = [vol.used(c) for c in range(h + 2, h + 7)]
        w_end = vol.entry(h + 1)
        set_links = [vol.entry(c) for c in range(h + 3, h + 7)]
        h2_data = vol.data([h + 2])[:3]
        dl = {exfatimg.fold(k): v for k, v in vol.listing(D_AT).items()}
        el = {exfatimg.fold(k): v for k, v in vol.listing(E_AT).items()}
        v, r = dl.get("v"), el.get("r")
        v_kept = v == (V_AT, 3, True) and vol.data([V_AT])[:3] == V_BYTES
        v_end = vol.entry(V_AT)
        r_kept = r == (R_AT, 3, True) and vol.data([R_AT])[:3] == R_BYTES and vol.entry(R_AT) == END
    fsck_ok, fsck_lines = exfatimg.fsck_clean(IMAGE, part="s3")
    if not fsck_ok:
        notes += [f"after: {ln}" for ln in fsck_lines]
    checks += [
        ("driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"HEAD: the whole-file write answered an I/O error ({probe!r})",
         "cursorprobe: whole:" in probe and "I/O error" in probe),
        ("HEAD: L kept every bit, its first cluster and its bytes (no give-back walked h's stale link)", l_kept),
        (f"HEAD: h to h+3 given back: taken again by A's append (h), W (h+1) and SET (h+2, h+3) "
         f"(A {a_chain}, W {w}, SET's links on h+3 to h+6 {set_links})",
         a_chain[1:] == [h] and bool(w) and w[0] == h + 1 and set_links == [h + 4, h + 5, h + 6, END]),
        ("HEAD: no H in X's root", "h" not in listed),
        (f"CURSOR: the first append answered an I/O error ({first!r})", "I/O error" in first),
        (f"CURSOR: the retried append answered nothing ({second!r})", second == ""),
        (f"CURSOR: A FAT-chained over two clusters from {A_AT} ({a}, chain {a_chain})",
         bool(a) and a[0] == A_AT and a[1] == len(A_BYTES) + 3 and a[2] and len(a_chain) == 2),
        (f"CURSOR: A's second cluster is h ({h}), the one the refused append gave back",
         a_chain[1:] == [h]),
        ("CURSOR: A holds its bytes and x\\r\\n", a_data == A_BYTES + b"x\r\n"),
        (f"GAP: W made on h+1 ({h + 1}), three bytes, chained ({made_w!r}, {w})",
         made_w == "" and w == (h + 1, 3, True)),
        (f"GAP: the gap fill answered an I/O error ({gap!r})", "I/O error" in gap),
        ("GAP: W still three bytes, w\\r\\n", w_data == b"w\r\n"),
        (f"GAP: W's chain cut back to its one cluster (h+1's entry {w_end})", w_end == END),
        (f"GAP: h+2 to h+4 given back: taken again by SET, its links on h+3 to h+6 ({set_links})",
         set_links == [h + 4, h + 5, h + 6, END]),
        (f"SET: the gap fill whose set write was refused answered an I/O error ({set_answer!r})",
         "I/O error" in set_answer),
        (f"SET: V as planted, three bytes on {V_AT} ({v})", v_kept),
        (f"SET: V's chain cut back to {V_AT} alone (its entry {v_end})", v_end == END),
        (f"SET: h+2 to h+6 given back: h+2 taken again by REPLACE, holding zzz ({h2_data!r})", h2_data == b"zzz"),
        (f"REPLACE: the whole-file write whose set write was refused answered an I/O error ({replace_answer!r})",
         "I/O error" in replace_answer),
        (f"REPLACE: R as planted, three bytes on {R_AT} alone ({r})", r_kept),
        (f"after: h+2 to h+6 free in the bitmap, every cluster SET and REPLACE took given back ({freed})",
         not any(freed)),
        ("after: fsck_exfat -n passes X (nothing leaked)", fsck_ok),
        (f"no fault lines in QEMU's trace ({faults})", faults == 0),
    ]
    return checks, notes, 0


def main():
    try:
        checks, notes, stalled = run()
    finally:
        if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
            os.remove(IMAGE)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    for n in notes:
        print("      " + n)
    return drive_qemu.verdict(RIG, failed, stalled)


if __name__ == "__main__":
    sys.exit(main())

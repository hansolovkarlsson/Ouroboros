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

On a copy of build/espexfat.img with a third partition added, an empty exFAT
of 512-byte clusters made here by newfs_exfat, one boot runs the probe on:

- the exFAT root, 4 KiB clusters: the read sequence (a whole-file write is
  at most SAFECOPY_MAX, 2 KiB, one cluster there, so the write sequences
  cannot be built and the probe says `skip`);
- the FAT32 ESP at /mnt/f, 512-byte clusters: all three;
- the small exFAT at /mnt/x: all three, so exFAT's write cursor is graded.

Each volume's cluster size is read off the disk and passed to the probe; a
volume that should take the write sequences and has clusters too large for
them fails the rig rather than passing with nothing graded. Fails with
either drop removed from either filesystem's write_fat_entry. One boot,
about a minute.
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

SECTOR = 512
SAFECOPY_MAX = 2048
BUILD = os.path.join(ROOT, "build")
SMALL = os.path.join(BUILD, "test-stale-cursor-x.part")
IMAGE = os.path.join(BUILD, "test-stale-cursor.img")
SEQUENCES = ("read", "write-in-place", "write-append")


def small_exfat():
    """An empty 8 MiB exFAT of 512-byte clusters (newfs_exfat -c 1)."""
    with open(SMALL, "wb") as fh:
        fh.truncate(8 * 1024 * 1024)
    dev = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", SMALL],
                         check=True, capture_output=True, text=True).stdout.split()[0]
    try:
        subprocess.run(["newfs_exfat", "-c", "1", "-v", "CURSORX", dev], check=True, capture_output=True)
    finally:
        subprocess.run(["hdiutil", "detach", dev], check=True, capture_output=True)


def cluster_bytes(image):
    """[cluster bytes] of each MBR partition of `image`, in table order."""
    with open(image, "rb") as fh:
        mbr = fh.read(SECTOR)
        out = []
        for i in range(4):
            e = 0x1BE + i * 16
            if mbr[e + 4] == 0:
                continue
            fh.seek(struct.unpack_from("<I", mbr, e + 8)[0] * SECTOR)
            vbr = fh.read(SECTOR)
            out.append((1 << vbr[109]) * SECTOR if vbr[3:11] == b"EXFAT   " else vbr[13] * SECTOR)
        return out


def grade(where, cluster, writes, out):
    """The checks for one run of the probe, from its lines in `out`."""
    lines = [ln.rstrip() for ln in re.findall(r"^cursorprobe: .*$", out, re.M)]
    for ln in lines:
        print(f"     {where}: {ln}")
    checks = []
    if writes:
        checks.append((f"{where}: clusters of {cluster} bytes take the write sequences "
                       f"(at most {SAFECOPY_MAX // 2})", cluster <= SAFECOPY_MAX // 2))
    for seq in SEQUENCES:
        want = "ok" if writes or seq == "read" else "skip"
        said = [ln for ln in lines if re.match(rf"cursorprobe: (ok|skip|FAIL) {seq}\b", ln)]
        checks.append((f"{where}: {want} {seq}", len(said) == 1 and said[0].startswith(f"cursorprobe: {want} {seq}")))
    checks.append((f"{where}: the probe's summary is ok", lines[-1:] == ["cursorprobe: ok"]))
    return checks


def main() -> int:
    small_exfat()
    try:
        mbrdisk.assemble(IMAGE, [
            (0x07, mbrdisk.exfat_payload(os.path.join(BUILD, "exfatpart.img")), False),
            (0x0B, mbrdisk.esp_payload(os.path.join(BUILD, "esp.img")), True),
            (0x07, mbrdisk.exfat_payload(SMALL), False),
        ])
    finally:
        os.remove(SMALL)
    runs = []
    try:
        sizes = cluster_bytes(IMAGE)
        # (where, directory, cluster bytes, whether the write sequences are graded)
        runs = [("exfat root", "/cprobe", sizes[0], False),
                ("fat32 /mnt/f", "/mnt/f/cprobe", sizes[1], True),
                ("exfat-512 /mnt/x", "/mnt/x/cprobe", sizes[2], True)]
        steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f"), ("# ", "mount 2 /mnt/x")]
        prev = "# "
        for _, d, c, _ in runs:
            steps.append((prev, f"cursorprobe {d} {c}"))
            prev = r"cursorprobe: (ok|FAIL)\r?\n"
        steps.append((prev, ""))
        guest = drive_qemu.Guest(IMAGE, label="test-stale-cursor: ",
                                 stamp_from=os.path.join(BUILD, "espexfat.img"))
        try:
            driven = guest.run(steps)
            out = guest.transcript().replace("\r\n", "\n")
            faults = guest.aborts()
        finally:
            guest.stop()
    finally:
        os.remove(IMAGE)
    with open(os.path.join(BUILD, "test-stale-cursor.txt"), "w") as fh:
        fh.write(out)

    checks = [("driven to the end", driven), ("no fault lines", faults == 0)]
    # Each run's lines: from its command to the next one's.
    parts = re.split(r"cursorprobe (/\S+) \d+\n", out)
    by_dir = dict(zip(parts[1::2], parts[2::2]))
    for where, d, c, writes in runs:
        checks += grade(where, c, writes, by_dir.get(d, ""))
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcript: build/test-stale-cursor.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

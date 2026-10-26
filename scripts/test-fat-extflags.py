#!/usr/bin/env python3
"""FAT32 with mirroring off reads and writes its live FAT alone.

    python3 scripts/test-fat-extflags.py     (or `make test-fat-extflags`, which builds the image)

BPB_ExtFlags (boot sector bytes 40 to 41) says whether a FAT32 volume's FATs
are mirrored: with bit 7 clear every copy is kept in step and copy 0 is
read; with it set only the copy numbered in bits 0 to 3 is live, and a
driver reads and writes that one alone. Until 2026-10-26 fsd read copy 0
and wrote every copy whatever the flags said, and refused a volume whose
live copy was another (`make test-volume-bound` held that refusal). Now it
reads the live copy and, with mirroring off, writes it alone; a live copy
past BPB_NumFATs is still refused (test-volume-bound's SHAPES-D).

One boot. The firmware boots build/esp.img from NVMe (which the kernel
cannot drive), and virtio holds a disk the rig builds: espext2.img's ext2
root, then esp.img's FAT32 (two FATs) with its ExtFlags made 0x0081
(mirroring off, FAT 1 live) and /man/ls (1,182 bytes, three clusters) cut
short in FAT 0 alone: its first cluster's entry there made end of chain, its
chain in FAT 1 whole. The flag is not set on the ESP the firmware boots:
OVMF then finds no boot loader on it. Before the boot the host checks both.
Then, after `mount 1 /mnt/p1`, `cat /mnt/p1/man/ls` must print the page
through its last line (read through FAT 1: through FAT 0 it ends after the
first cluster), and `echo new > /mnt/p1/NEWF` then `cat /mnt/p1/NEWF` must
print `new`. After the boot FAT 0 must be
every byte as it was (no write reached it) and NEWF's cluster end of chain
in FAT 1. No fault lines; a boot QEMU's firmware never starts is
INCONCLUSIVE (exit 100). Fails with ExtFlags ignored (the page cut short and
FAT 0 written), with every copy written though mirroring is off (FAT 0
written) and with copy 0 read while the live copy is written (the page cut
short). About a minute. Run it whenever FAT32's mount_at reading
of BPB_ExtFlags, fat_place, fat_entry or write_fat_entry changes.
"""
import importlib.util
import os
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import fat32img  # noqa: E402
import mbrdisk  # noqa: E402

_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "test-fat-extflags.img")
ESP = os.path.join(ROOT, "build", "esp.img")
EXT2 = os.path.join(ROOT, "build", "espext2.img")
SECTOR = fat32img.SECTOR
EOC = 0x0FFFFFFF


def chain(vol, first, copy):
    out = [first]
    while vol.in_heap(out[-1]) and vol.in_heap(vol.entry(out[-1], copy)):
        out.append(vol.entry(out[-1], copy))
    return out


def fat_bytes(fh, vol, copy):
    fh.seek((vol.fat + copy * vol.fatsz) * SECTOR)
    return fh.read(vol.fatsz * SECTOR)


def answer(out, cmd):
    marker = "# " + cmd + "\r"
    if marker not in out:
        return None
    lines = out.split(marker, 1)[1].split("# ", 1)[0].splitlines()
    return "\n".join(ln for ln in lines if not ln.startswith("Ouroboros kernel:")).strip()


def partition(image, number):
    """The bytes of MBR partition `number` (1-based) of `image`."""
    with open(image, "rb") as fh:
        e = [e for e in mbrdisk.entries(fh.read(SECTOR)) if e[0] == number][0]
        fh.seek(e[2] * SECTOR)
        return fh.read(e[3] * SECTOR)


def run():
    layout = mbrdisk.assemble(IMAGE, [(0x83, partition(EXT2, 1), False), (0x0B, partition(ESP, 1), False)])
    start = layout[1][0]
    with open(IMAGE, "r+b") as fh:
        vol = fat32img.Fat32(fh, start)
        man = next(c for _, n, c, _ in vol.listing(vol.root) if n == "MAN")
        _, _, first, size = next(e for e in vol.listing(man) if e[1] == "LS")
        whole = chain(vol, first, 0)
        page = vol.read(whole, size).decode("ascii", "replace")
        # FAT 0 alone: the first cluster made the chain's end.
        fh.seek((vol.fat * SECTOR) + first * 4)
        fh.write(struct.pack("<I", EOC))
        fh.seek(start * SECTOR + 40)
        fh.write(struct.pack("<H", 0x0081))
        vol = fat32img.Fat32(fh, start)
        fat0 = fat_bytes(fh, vol, 0)
        last = [ln for ln in page.splitlines() if ln.strip()][-1].strip()
        checks = [(f"before: /man/ls is {len(whole)} clusters in FAT 1 and 1 in FAT 0, ExtFlags 0x0081",
                   len(whole) == 3 and chain(vol, first, 1) == whole and chain(vol, first, 0) == [first])]
    if not checks[0][1]:
        return checks, 0

    cmds = ["mount 1 /mnt/p1", "cat /mnt/p1/man/ls", "echo new > /mnt/p1/NEWF", "cat /mnt/p1/NEWF"]
    steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in cmds] + [("# ", "")]
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, as test-volume-bound
        # does: it boots the ESP there, and the kernel takes virtio.
        "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
        "-drive", f"file={IMAGE},format=raw,if=none,id=hd0",
        "-device", "virtio-blk-device,drive=hd0",
    ]
    guest = drive_qemu.Guest(IMAGE, extra_args=extra, virtio_disk=False, stamp_from=ESP,
                             label="test-fat-extflags: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", "test-fat-extflags.txt"), "w") as fh:
        fh.write(out)
    if drive_qemu.boot_stalled("test-fat-extflags", "boot", out, faults):
        return checks, 1

    with open(IMAGE, "rb") as fh:
        vol = fat32img.Fat32(fh, start)
        fat0_after = fat_bytes(fh, vol, 0)
        newf = next((e for e in vol.listing(vol.root) if e[1] == "NEWF"), None)
        newf_fat1 = vol.entry(newf[2], 1) if newf and newf[2] else None
    shown = answer(out, "cat /mnt/p1/man/ls") or ""
    checks += [
        (f"driven to the end, the ext2 root and then the FAT32 mounted ({answer(out, 'mount 1 /mnt/p1')!r})",
         driven and "fsd: ext2 mounted" in out and answer(out, "mount 1 /mnt/p1") == ""),
        (f"cat /mnt/p1/man/ls printed the page through its last line, {last!r} (read through FAT 1)",
         last in shown),
        (f"echo new > /mnt/p1/NEWF then cat of it printed new ({answer(out, 'cat /mnt/p1/NEWF')!r})",
         answer(out, "cat /mnt/p1/NEWF") == "new"),
        ("FAT 0 every byte as it was: no write reached it", fat0_after == fat0),
        (f"NEWF's cluster end of chain in FAT 1 ({newf_fat1 if newf_fat1 is None else hex(newf_fat1)})",
         newf_fat1 is not None and newf_fat1 >= 0x0FFFFFF8),
        (f"no fault lines in QEMU's trace ({faults})", faults == 0),
    ]
    return checks, 0


def main():
    try:
        checks, stalled = run()
    finally:
        if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
            os.remove(IMAGE)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    return drive_qemu.verdict("test-fat-extflags", failed, stalled)


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""A virtio disk with no MBR signature reaches fsd, below any USB stick.

    python3 scripts/test-blank-disk.py     (or `make test-blank-disk`, which builds the images)

Until 2026-10-23 `init_storage` dropped a virtio disk whose sector 0 read
without the `0x55 0xAA` boot signature, so a blank disk never reached fsd and
`partition` and `format` could not prepare it, and a disk `erase disk` had
zeroed was gone after a reboot. Now the kernel holds it as blank: not the
boot disk (a partition table and a volume boot sector both end in the
signature), installed only when no USB stick is attached, and below any
stick, the boot disk or not (Hans, 2026-10-23).

In every boot the firmware boots esp.img from NVMe, which the kernel cannot
drive, so the boot partition is known and on no disk the kernel drives.

- BLANK: a 64 MiB disk of zeros on virtio, no stick. The kernel must say it
  holds the disk as blank and install it as the fallback, naming why;
  `partition ext2`, `format ext2` and `mount -a` must end with `mount`
  reporting ext2 at LBA 2048. After the boot the host reads the disk: the
  boot signature, partition 1 of type 0x83 at LBA 2048, and the ext2 magic in
  its superblock.
- ERASE: two boots of one disk, a copy of esp.img signed as its own on
  virtio. The first, the disk installed as the fallback with FAT32 mounted,
  runs `unmount` and `erase disk`; the host must then find sector 0 zero. The
  second must hold it as blank and prepare it as in BLANK: the user-facing
  case, a disk erased and the machine rebooted.
- STICK: the 64 MiB blank disk on virtio and build/usbstick.img, which is
  not the boot disk, on USB. The stick must be installed and fsd mount its
  FAT32, the blank disk held and never installed. A disk held as not the
  boot disk would have the stick refused instead (`fallback` in
  test-boot-partition.py), so this is the check that the blank disk ranks
  below any stick.

Fails with the drop restored, as on main before 2026-10-23 (BLANK and
ERASE's second boot: no disk to partition), and with a blank disk holding a
stick back as a disk not the boot disk does (STICK: the stick refused, the
blank disk installed). Not covered: a blank disk with the boot partition
unknown to the kernel, where holding it differs from installing it first,
since QEMU's firmware always names the partition (as test-probe-ioerr's
SECTOR0 notes). About two minutes. Run it whenever `init_storage`,
`install_boot_disk` or `erase disk` changes.
"""
import importlib.util
import os
import re
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BUILD = os.path.join(ROOT, "build")
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
sys.path.insert(0, HERE)
import disksig  # noqa: E402

ESP = os.path.join(BUILD, "esp.img")
STICK = os.path.join(BUILD, "usbstick.img")
DISK = os.path.join(BUILD, "test-blank-disk.img")
STICK_COPY = os.path.join(BUILD, "test-blank-disk-stick.img")
BLANK_BYTES = 64 << 20
# A signature of the ERASE copy's own: a byte-for-byte copy of esp.img signed
# from its bytes would get esp.img's back (test-boot-partition.py's note).
OWN_SIGNATURE = 0x0B1A4C00
PREPARE = ["partition ext2", "format ext2", "mount -a", "mount"]

HELD = "virtio-blk sector 0 has no MBR signature (0x0000) - held as blank, installed only if no USB stick is attached"
FALLBACK = "virtio-blk installed as the fallback: blank, and no USB stick is attached"


def outputs(out, commands):
    """[output of each command], in order: the lines after its prompt line up
    to the next prompt (test-probe-ioerr.py's reader)."""
    got = []
    at = 0
    for cmd in commands:
        m = re.compile(rf"^(?:# )?{re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            got.append(None)
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        got.append(out[m.end():nxt.start() if nxt else len(out)].strip())
        at = nxt.start() if nxt else len(out)
    return got


def blank(path):
    with open(path, "wb") as fh:
        fh.truncate(BLANK_BYTES)


def boot(name, steps, stick=False):
    """One boot from esp.img on NVMe with DISK on virtio: (driven, transcript,
    stalled). The disk is written in place, so the host can read it after."""
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, which it boots and
        # the kernel cannot drive (test-boot-partition's fallback case).
        "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
    ]
    if stick:
        extra += [
            "-device", "qemu-xhci,id=xhci0",
            "-drive", f"file={STICK_COPY},format=raw,if=none,id=usbstick,snapshot=on",
            "-device", "usb-storage,drive=usbstick,bus=xhci0.0,port=1",
        ]
    guest = drive_qemu.Guest(DISK, extra_args=extra, stamp_from=ESP, label=f"test-blank-disk[{name}]: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(BUILD, f"test-blank-disk-{name.lower()}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults, drive_qemu.boot_stalled("test-blank-disk", name, out, faults)


def prepared(name, driven, out, faults):
    """The checks of a boot on a blank disk that partitions and formats it."""
    o = outputs(out, PREPARE)
    boot_log = out.split(f"# {PREPARE[0]}")[0]
    with open(DISK, "rb") as fh:
        mbr = fh.read(512)
        fh.seek(2048 * 512 + 1024 + 56)
        magic = fh.read(2)
    kind, start = mbr[0x1BE + 4], struct.unpack_from("<I", mbr, 0x1BE + 8)[0]
    return [
        (f"{name}: driven to the end, every command's output found", driven and None not in o),
        (f"{name}: the kernel held the disk as blank", HELD in boot_log),
        (f"{name}: and installed it as the fallback, naming why", FALLBACK in boot_log),
        (f"{name}: mount after partition, format and mount -a reports ext2 at LBA 2048 (got {o[3]!r})",
         (o[3] or "").startswith("ext2 mounted at partition LBA 2048 ")),
        (f"{name}: on the host, the boot signature, partition 1 type 0x83 at LBA 2048 (got {kind:#x} at {start})",
         mbr[510:512] == b"\x55\xaa" and kind == 0x83 and start == 2048),
        (f"{name}: on the host, the ext2 magic in its superblock", magic == b"\x53\xef"),
        (f"{name}: no fault lines", faults == 0),
    ]


def run_blank():
    blank(DISK)
    driven, out, faults, stalled = boot("BLANK", [("# ", c) for c in PREPARE] + [("# ", "")])
    return ([], True) if stalled else (prepared("BLANK", driven, out, faults), False)


def run_erase():
    with open(ESP, "rb") as src, open(DISK, "wb") as dst:
        dst.write(src.read())
    disksig.put_signature(DISK, OWN_SIGNATURE)
    pre = [("ERASE: the copy carries a signature of its own",
            disksig.read_signature(DISK) == OWN_SIGNATURE != disksig.read_signature(ESP))]
    steps = [("login:", "root"), ("assword", "root"), ("# ", "unmount"), ("# ", "erase disk"), ("# ", "")]
    driven, out, faults, stalled = boot("ERASE-1", steps)
    if stalled:
        return pre, True
    with open(DISK, "rb") as fh:
        sector0 = fh.read(512)
    checks = pre + [
        ("ERASE-1: driven to the end", driven),
        ("ERASE-1: the disk installed as the fallback, FAT32 mounted",
         "virtio-blk installed as the fallback: no USB stick is attached" in out and "FAT32 mounted" in out),
        ("ERASE-1: on the host, sector 0 zeroed by `erase disk`", sector0 == bytes(512)),
        ("ERASE-1: no fault lines", faults == 0),
    ]
    driven, out, faults, stalled = boot("ERASE-2", [("# ", c) for c in PREPARE] + [("# ", "")])
    if stalled:
        return checks, True
    return checks + prepared("ERASE-2", driven, out, faults), False


def run_stick():
    blank(DISK)
    with open(STICK, "rb") as src, open(STICK_COPY, "wb") as dst:
        dst.write(src.read())
    try:
        driven, out, faults, stalled = boot("STICK", [("login:", "")], stick=True)
    finally:
        os.remove(STICK_COPY)
    if stalled:
        return [], True
    return [
        ("STICK: reached the login prompt", driven),
        ("STICK: the kernel held the virtio disk as blank", HELD in out),
        ("STICK: the stick installed, though not the boot disk",
         "usb-msd block device installed" in out and "the USB stick is not the boot disk either" not in out),
        ("STICK: the blank disk never installed",
         "as the fallback" not in out and "block device installed for the filesystem server" not in out),
        ("STICK: fsd mounted the stick's FAT32", "fsd: FAT32 mounted" in out),
        ("STICK: no fault lines", faults == 0),
    ], False


def main():
    for f in (ESP, STICK):
        if not os.path.exists(f):
            sys.exit(f"{f} not found - run `make test-blank-disk`, which builds it")
    checks = []
    stalled = 0
    try:
        for run in (run_blank, run_erase, run_stick):
            got, stall = run()
            checks += got
            stalled += stall
    finally:
        if os.path.exists(DISK):
            os.remove(DISK)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    return drive_qemu.verdict("test-blank-disk", failed, stalled)


if __name__ == "__main__":
    sys.exit(main())

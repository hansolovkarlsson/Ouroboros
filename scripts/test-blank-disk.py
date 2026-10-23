#!/usr/bin/env python3
"""A blank virtio disk reaches fsd, below any USB stick.

    python3 scripts/test-blank-disk.py     (or `make test-blank-disk`, which builds the images)

Until 2026-10-23 `init_storage` dropped a virtio disk whose sector 0 read
without the `0x55 0xAA` boot signature, so a blank disk never reached fsd and
`partition` and `format` could not prepare it, and a disk `erase disk` had
zeroed was gone after a reboot. Now the kernel holds a blank disk, one with
no partition table and no volume (`parttable::is_blank`: no "EFI PART" at LBA
1, and a sector 0 with no signature, no entry fsd lists and no protective
entry), as not the boot disk, and ranks it below any stick, the boot disk or
not (Hans, 2026-10-23). A disk that is not blank goes to the boot-disk check
as before, whatever its signature.

Seven boots. In all but UNKNOWN the firmware boots esp.img from NVMe (or
espgpt.img, in GPT-UNSIGNED), which the kernel cannot drive, so the boot
partition is known and on no disk the kernel drives.

- BLANK: a 48 MiB disk of 0xFF, its four MBR entries zero (sector 0 not
  zero, and unsigned), no stick. The kernel must hold it as blank and install
  it as the fallback, naming why; fsd's boot line must name `partition` and
  `format`; `partition ext2`, `format ext2` and `mount -a` must end with
  `mount` reporting ext2 at LBA 2048. After the boot the host reads the
  table whole (one entry, type 0x83, LBA 2048 to the disk's end) and the ext2
  magic in its superblock.
- ERASE: two boots of one disk, a copy of esp.img signed as its own. The
  first, the disk installed as the fallback with FAT32 mounted, runs
  `unmount` and `erase disk`; the host must then find sector 0 zero. The
  second must hold it as blank and prepare it as BLANK does: a disk erased
  and the machine rebooted.
- STICK: a blank disk and build/usbstick.img, not the boot disk, on USB. The
  stick must be installed and `mount` report FAT32 on a disk of its size (the
  blank disk is 48 MiB, the stick 64), and the blank disk never be
  installed. A disk held as not the boot disk would have the stick
  refused instead (test-boot-partition.py's `fallback`), so this is the check
  that a blank disk ranks below any stick. Its premise, that esp.img and
  usbstick.img carry different signatures, is checked before the boot.
- GPT-UNSIGNED: espgpt.img booted from NVMe, a copy of it on virtio with its
  protective MBR's signature zeroed, and the stick. The copy is the boot
  disk (its GPT headers name the boot partition) and must be installed first,
  not held as blank; the stick must stay out.
- SIG-DAMAGED: a copy of esp.img signed as its own with its boot signature
  zeroed, its entry intact, and the stick. fsd mounts the entry (the table is
  read without the signature), so the disk is not blank: it must be held as
  not the boot disk, the stick refused, and `ls /` list the ESP's EFI.
- UNKNOWN: esp.img's FAT32 alone on NVMe with no partition table (a
  superfloppy), so the firmware's path names no partition and the boot
  partition is unknown, a blank disk on virtio and the stick: the stick must
  be installed and the blank disk never, where a disk that is not blank goes
  in first. Its premise, the kernel's `boot partition: unknown` line, is
  checked.

Fails with the drop, as on main before 2026-10-23 (BLANK and ERASE's second
boot: no disk to partition, nothing written), with a blank disk holding a
stick back as a disk not the boot disk does (STICK), with blank decided by
the signature alone, the first version of 2026-10-23 (GPT-UNSIGNED: the
stick installed over the boot disk; SIG-DAMAGED: the stick installed over
the disk fsd mounts), and with a blank disk asked of the boot-disk check
(UNKNOWN: the blank disk installed first). Not covered by a boot: a blank
disk whose first read fails and whose second reads (the kernel asks
`is_blank` twice); QEMU's blkdebug can fail a read once, but the firmware
reads sector 0 first and takes the one failure. Nor the blank disk beside a
stick whose init fails. About four minutes. Run it whenever `init_storage`,
`install_boot_disk`, `parttable::is_blank`, fsd's mount line or `erase
disk` changes.
"""
import importlib.util
import os
import re
import shutil
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
import mbrdisk  # noqa: E402

ESP = os.path.join(BUILD, "esp.img")
GPT = os.path.join(BUILD, "espgpt.img")
STICK = os.path.join(BUILD, "usbstick.img")
DISK = os.path.join(BUILD, "test-blank-disk.img")
FLOPPY = os.path.join(BUILD, "test-blank-disk-floppy.img")
# 48 MiB, not the stick's 64, so `mount` naming the disk's size tells them apart.
BLANK_BYTES = 48 << 20
# A signature of the copies' own: a byte-for-byte copy of esp.img signed
# from its bytes would get esp.img's back (test-boot-partition.py's note).
OWN_SIGNATURE = 0x0B1A4C00
PREPARE = ["partition ext2", "format ext2", "mount -a", "mount"]
# netd's last boot line when nothing is mounted: its retries print for some
# seconds after the prompt, and a line typed meanwhile is split by them.
NETD_SETTLED = "netd: no /etc/cluster/id - cannot make remote requests"
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)

HELD = "virtio-blk holds no partition table or volume - held as blank, installed only if no USB stick is"
FALLBACK = "virtio-blk installed as the fallback: blank, and no USB stick is attached"
NAMES_PREPARE = "the disk has no partition: `partition` then `format` prepare it"
STICK_IN = "usb-msd block device installed"
VIRTIO_IN = "block device installed for the filesystem server"


def outputs(out, commands):
    """[output of each command], in order: the lines after its prompt line up
    to the next prompt, the kernel's exit lines dropped (test-probe-ioerr.py's
    reader)."""
    got = []
    at = 0
    for cmd in commands:
        m = re.compile(rf"^(?:# )?{re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            got.append(None)
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        got.append(EXIT.sub("", out[m.end():nxt.start() if nxt else len(out)]).strip())
        at = nxt.start() if nxt else len(out)
    return got


def blank(path):
    """A disk of 0xFF with its four MBR entries zero: no signature, no table,
    and a sector 0 that is not zero, so a test of "all zero" would miss it."""
    with open(path, "wb") as fh:
        fh.write(b"\xff" * BLANK_BYTES)
        fh.seek(0x1BE)
        fh.write(bytes(64))


def copy_signed(source, path, damage_signature=False):
    shutil.copyfile(source, path)
    disksig.put_signature(path, OWN_SIGNATURE)
    if damage_signature:
        with open(path, "r+b") as fh:
            fh.seek(510)
            fh.write(bytes(2))


def boot(name, steps, nvme=ESP, stick=False):
    """One boot from `nvme` on NVMe with DISK on virtio: (driven, transcript,
    faults, stalled). The disk is written in place, so the host can read it
    after; the stick is a snapshot of build/usbstick.img, which keeps every
    write off it."""
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, which it boots and
        # the kernel cannot drive (test-boot-partition's fallback case).
        "-drive", f"file={nvme},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
    ]
    if stick:
        extra += [
            "-device", "qemu-xhci,id=xhci0",
            "-drive", f"file={STICK},format=raw,if=none,id=usbstick,snapshot=on",
            "-device", "usb-storage,drive=usbstick,bus=xhci0.0,port=1",
        ]
    # The stamp is the source image's: the superfloppy is cut from esp.img.
    stamp = ESP if nvme == FLOPPY else nvme
    guest = drive_qemu.Guest(DISK, extra_args=extra, stamp_from=stamp, label=f"test-blank-disk[{name}]: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(BUILD, f"test-blank-disk-{name.lower()}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults, drive_qemu.boot_stalled("test-blank-disk", name, out, faults)


def prepare_boot(name):
    """A boot of the blank DISK alone that partitions and formats it, and its
    checks; None when the firmware never started the kernel."""
    steps = [(NETD_SETTLED, drive_qemu.ENTER)] + [("# ", c) for c in PREPARE] + [("# ", "")]
    driven, out, faults, stalled = boot(name, steps)
    if stalled:
        return None
    o = outputs(out, PREPARE)
    boot_log = out.split(NETD_SETTLED)[0]
    with open(DISK, "rb") as fh:
        mbr = fh.read(512)
        fh.seek(2048 * 512 + 1024 + 56)
        magic = fh.read(2)
    table = mbrdisk.entries(mbr)
    want = [(1, 0x83, 2048, os.path.getsize(DISK) // 512 - 2048)]
    return [
        (f"{name}: driven to the end, every command's output found", driven and None not in o),
        (f"{name}: the kernel held the disk as blank", HELD in boot_log),
        (f"{name}: and installed it as the fallback, naming why", FALLBACK in boot_log),
        (f"{name}: fsd's boot line names `partition` and `format`", NAMES_PREPARE in boot_log),
        (f"{name}: mount after partition, format and mount -a reports ext2 at LBA 2048 (got {o[3]!r})",
         (o[3] or "").startswith("ext2 mounted at partition LBA 2048 ")),
        (f"{name}: on the host, the table is one ext2 entry to the disk's end (got {table})", table == want),
        (f"{name}: on the host, the ext2 magic in its superblock", magic == b"\x53\xef"),
        (f"{name}: no fault lines", faults == 0),
    ]


def run_blank():
    blank(DISK)
    checks = prepare_boot("BLANK")
    return ([], True) if checks is None else (checks, False)


def run_erase():
    copy_signed(ESP, DISK)
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
    got = prepare_boot("ERASE-2")
    return (checks, True) if got is None else (checks + got, False)


def listing(out):
    found = re.search(r"ls /\r?\n([\s\S]*?)# ", out)
    return found.group(1) if found else ""


def run_stick(name, nvme=ESP):
    """A blank disk beside a stick that is not the boot disk: the stick must
    go in, the blank disk never."""
    blank(DISK)
    pre = [(f"{name}: esp.img and usbstick.img carry different signatures, so the stick is not the boot disk",
            disksig.read_signature(ESP) != disksig.read_signature(STICK)),
           (f"{name}: the blank disk's size is not the stick's", os.path.getsize(DISK) != os.path.getsize(STICK))]
    # usbstick.img holds no /etc/passwd, so login starts a root session, and
    # no /bin, so the builtin `mount` names the disk mounted by its size.
    driven, out, faults, stalled = boot(name, [("# ", "mount"), ("# ", "")], nvme=nvme, stick=True)
    if stalled:
        return pre, True
    o = outputs(out, ["mount"])[0]
    stick_sectors = os.path.getsize(STICK) // 512
    checks = pre + [
        (f"{name}: driven to the end", driven),
        (f"{name}: the kernel held the virtio disk as blank", HELD in out),
        (f"{name}: the stick installed", STICK_IN in out),
        (f"{name}: the blank disk never installed", VIRTIO_IN not in out and "as the fallback" not in out),
        (f"{name}: `mount` reports FAT32 on a disk of the stick's size (got {o!r})",
         (o or "").startswith(f"FAT32 mounted at partition LBA 1 (disk {stick_sectors} sectors")),
        (f"{name}: no fault lines", faults == 0),
    ]
    return checks, False


def run_unknown():
    with open(ESP, "rb") as src, open(FLOPPY, "wb") as dst:
        src.seek(512)
        shutil.copyfileobj(src, dst)
    try:
        checks, stalled = run_stick("UNKNOWN", nvme=FLOPPY)
    finally:
        os.remove(FLOPPY)
    out = open(os.path.join(BUILD, "test-blank-disk-unknown.txt")).read()
    if not stalled:
        checks.insert(1, ("UNKNOWN: the kernel says the boot partition is unknown",
                          "boot partition: unknown" in out))
    return checks, stalled


def run_gpt_unsigned():
    shutil.copyfile(GPT, DISK)
    with open(DISK, "r+b") as fh:
        fh.seek(510)
        fh.write(bytes(2))
        fh.seek(512)
        header = fh.read(8)
    pre = [("GPT-UNSIGNED: the copy's GPT header is at LBA 1", header == b"EFI PART")]
    driven, out, faults, stalled = boot("GPT-UNSIGNED", [("login:", "")], nvme=GPT, stick=True)
    if stalled:
        return pre, True
    return pre + [
        ("GPT-UNSIGNED: reached the login prompt", driven),
        ("GPT-UNSIGNED: not held as blank", HELD not in out),
        ("GPT-UNSIGNED: installed first as the boot disk",
         "installed for the filesystem server, the boot disk (its boot partition at LBA 2048)" in out),
        ("GPT-UNSIGNED: the stick never installed", STICK_IN not in out),
        ("GPT-UNSIGNED: no fault lines", faults == 0),
    ], False


def run_sig_damaged():
    copy_signed(ESP, DISK, damage_signature=True)
    with open(DISK, "rb") as fh:
        sector0 = fh.read(512)
    pre = [("SIG-DAMAGED: the copy is unsigned, its entry intact",
            sector0[510:512] == bytes(2) and sector0[0x1BE + 4] == 0x0B)]
    steps = [("login:", "root"), ("assword", "root"), ("# ", "ls /"), ("# ", "")]
    driven, out, faults, stalled = boot("SIG-DAMAGED", steps, stick=True)
    if stalled:
        return pre, True
    return pre + [
        ("SIG-DAMAGED: driven to the end", driven),
        ("SIG-DAMAGED: not held as blank, held as not the boot disk",
         HELD not in out and "virtio-blk is not the boot disk - held" in out),
        ("SIG-DAMAGED: the stick refused and the disk installed as the fallback",
         "virtio-blk installed as the fallback: the USB stick is not the boot disk either" in out
         and STICK_IN not in out),
        ("SIG-DAMAGED: `ls /` lists the ESP's EFI, not the stick's USBTEST.TXT",
         bool(re.search(r"\bEFI\b", listing(out))) and "USBTEST" not in listing(out)),
        ("SIG-DAMAGED: no fault lines", faults == 0),
    ], False


def main():
    for f in (ESP, GPT, STICK):
        if not os.path.exists(f):
            sys.exit(f"{f} not found - run `make test-blank-disk`, which builds it")
    checks = []
    stalled = 0
    runs = (run_blank, run_erase, lambda: run_stick("STICK"), run_gpt_unsigned, run_sig_damaged, run_unknown)
    try:
        for run in runs:
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

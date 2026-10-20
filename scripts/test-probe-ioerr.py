#!/usr/bin/env python3
"""A read error inside a partition's probe is an I/O error, not "no filesystem".

    python3 scripts/test-probe-ioerr.py     (or `make test-probe-ioerr`, which builds the images)

fsd probes each partition as FAT32, exFAT and ext2. Until 2026-10-20 a read
that failed inside a probe read the same as a partition holding none of them
(`vfs::Filesystem::probe` answered `None` for both), so on a disk that was
there and failing, `mount -a` found "no filesystem" and had the kernel swap
in whatever USB stick was attached, putting the stick at `/` with the real
disk out of reach until a reboot, and `format` took the partition it could
not read as empty and laid a new filesystem over it. Only a read error in the
partition table was told apart (#324).

One boot. The firmware boots esp.img from NVMe, which the kernel cannot
drive, so the virtio disk goes in as the fallback block device. That disk is
a copy of build/espexfat.img with its ESP's partition entry zeroed, so it
holds one partition, exFAT, signed again (scripts/disksig.py) so it is not
taken for the boot disk. It is attached through QEMU's blkdebug driver, every
read of the exFAT root directory's first sector failing with EIO: a sector
the exFAT probe reads (`locate_bitmap`) and the partition table's reads
(sectors 0 and 1) never do, which the rig checks before the boot. On esp.img
there is no such sector, since its FAT32 starts at sector 1, where the table
looks for a GPT header, and FAT32's probe reads its first sector only. Beside
it on USB, a copy of build/usbstick.img (a FAT32 stick). As the fallback root
session (nothing mounts, so login has no accounts):

Two boots, each on fresh copies, each checking that fsd's boot line says the
mount failed with a disk I/O error:

- MOUNT: `mount -a` must answer that the disk gave an I/O error and was not
  replaced, with no `usb-msd block device installed` line, and the builtin
  `mount` after it must say nothing is mounted;
- FORMAT: `format ext2` must be refused, and after the boot the virtio
  copy's partition must be byte for byte as it was. The bytes are the check:
  on main before 2026-10-20 the format wrote ext2 over the partition and then
  failed on the sector that would not read, printing the same refusal.

No fault line in QEMU's trace. Fails with the probe answering `None` for a
read error, as on main before 2026-10-20 (MOUNT: the stick installed and
mounted; FORMAT: the partition overwritten). One boot each because the
format, when it goes ahead, leaves an ext2 that `mount -a` then mounts, and
the swap is never tried. That the stick can be swapped in at all is
test-mount-gate's check. About a minute and a half. Run it whenever fsd's
probe, mount, format or `mount -a`'s rescan changes.
"""
import importlib.util
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BUILD = os.path.join(ROOT, "build")
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

ESP = os.path.join(BUILD, "esp.img")
EXFAT = os.path.join(BUILD, "espexfat.img")
STICK = os.path.join(BUILD, "usbstick.img")
COPY = os.path.join(BUILD, "test-probe-ioerr.img")
STICK_COPY = os.path.join(BUILD, "test-probe-ioerr-stick.img")
BLKDEBUG = os.path.join(BUILD, "test-probe-ioerr.blkdebug")
BOOTS = {"MOUNT": ["mount -a", "mount"], "FORMAT": ["format ext2"]}
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)


def prepare():
    """The virtio copy, its one exFAT partition's start and the sector that
    fails: the root directory's first, from the volume's boot sector."""
    shutil.copyfile(EXFAT, COPY)
    with open(COPY, "r+b") as fh:
        mbr = bytearray(fh.read(512))
        assert mbr[510:512] == b"\x55\xaa", f"{EXFAT}: no MBR"
        assert mbr[0x1BE + 4] == 0x07, f"{EXFAT}: partition 1 is not exFAT"
        start = int.from_bytes(mbr[0x1BE + 8:0x1BE + 12], "little")
        mbr[0x1CE:0x1DE] = bytes(16)  # the ESP's entry: one partition left
        fh.seek(0)
        fh.write(mbr)
        fh.seek(start * 512)
        vbr = fh.read(512)
    assert vbr[3:11] == b"EXFAT   ", f"{EXFAT}: partition 1 holds no exFAT"
    heap = int.from_bytes(vbr[88:92], "little")
    root = int.from_bytes(vbr[96:100], "little")
    failing = start + heap + ((root - 2) << vbr[109])
    subprocess.run([sys.executable, os.path.join(HERE, "disksig.py"), "set", COPY], check=True,
                   stdout=subprocess.DEVNULL)
    return start, failing


def partition_bytes(image, start, sectors=64):
    with open(image, "rb") as fh:
        fh.seek(start * 512)
        return fh.read(sectors * 512)


def partition_bytes(image, start, sectors=64):
    with open(image, "rb") as fh:
        fh.seek(start * 512)
        return fh.read(sectors * 512)


def boot(name, commands):
    """One boot on fresh copies: (driven, transcript, faults, partition
    unchanged, partition start, failing sector)."""
    steps = [("# ", cmd) for cmd in commands] + [("# ", "")]
    guest = None
    try:
        start, failing = prepare()
        shutil.copyfile(STICK, STICK_COPY)
        before = partition_bytes(COPY, start)
        with open(BLKDEBUG, "w") as fh:
            fh.write(f'[inject-error]\nevent = "read_aio"\nerrno = "5"\nsector = "{failing}"\n')
        extra = [
            # bootindex=0 pins the firmware to the NVMe disk, which it boots
            # and the kernel cannot drive (test-boot-partition's fallback case).
            "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
            "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
            # The failing disk: every read of the exFAT root's first sector
            # answers EIO.
            "-drive", f"file=blkdebug:{BLKDEBUG}:{COPY},format=raw,if=none,id=hd0,rerror=report",
            "-device", "virtio-blk-device,drive=hd0",
            "-device", "qemu-xhci,id=xhci0",
            "-drive", f"file={STICK_COPY},format=raw,if=none,id=usbstick",
            "-device", "usb-storage,drive=usbstick,bus=xhci0.0,port=1",
        ]
        guest = drive_qemu.Guest(COPY, extra_args=extra, virtio_disk=False, stamp_from=ESP,
                                 label=f"test-probe-ioerr[{name}]: ")
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
        guest.stop()
        guest = None
        after = partition_bytes(COPY, start)
    finally:
        if guest:
            guest.stop()
        for f in (COPY, STICK_COPY, BLKDEBUG):
            if os.path.exists(f):
                os.remove(f)
    with open(os.path.join(BUILD, f"test-probe-ioerr-{name.lower()}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults, before == after, start, failing


def outputs(out, commands):
    """[output of each command], in order: the lines after its prompt line
    (`# cmd`) up to the next prompt, the kernel's exit lines dropped. netd's
    boot lines can land after a prompt, so a command may be echoed on a line
    of its own."""
    got = []
    at = 0
    for cmd in commands:
        m = re.compile(rf"^(?:# )?{re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            got.append(None)
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        block = out[m.end():nxt.start() if nxt else len(out)]
        at = nxt.start() if nxt else len(out)
        got.append(EXIT.sub("", block).strip())
    return got


def checks_of(name, commands):
    driven, out, faults, untouched, start, failing = boot(name, commands)
    o = outputs(out, commands)
    boot_log = out.split(f"# {commands[0]}")[0]
    checks = [
        # The table's reads are sectors 0 and 1 (MBR, GPT header): the error
        # must land past them, where only the probe reads.
        (f"the failing sector, {failing}, is read by the probe alone (partition at {start})",
         failing > start > 1),
        ("driven to the end, every command's output found", driven and None not in o),
        # fsd prints its failure line in three pieces, so another task's
        # line can land inside it: its two halves, before the first command.
        ("fsd's boot mount failed with a disk I/O error",
         "fsd: mount failed (" in boot_log
         and "disk I/O error) - disk commands won't work" in boot_log),
    ]
    if name == "MOUNT":
        checks += [
            (f"mount -a answered the I/O error, the disk not replaced (got {o[0]!r})",
             (o[0] or "").endswith("mount: the disk answered with an I/O error; it was not replaced by a USB stick")),
            ("no stick installed", "usb-msd block device installed" not in out),
            (f"mount after it: nothing mounted (got {o[1]!r})",
             (o[1] or "").startswith("nothing mounted")),
        ]
    else:
        checks += [
            (f"format ext2 refused (got {o[0]!r})",
             o[0] == "format: the partition is too small, or an I/O error occurred"),
            (f"the virtio copy's partition unchanged from LBA {start}", untouched),
        ]
    checks.append(("no fault lines", faults == 0))
    return [(f"{name}: {label}", ok) for label, ok in checks]


def main():
    for f in (ESP, EXFAT, STICK):
        if not os.path.exists(f):
            sys.exit(f"{f} not found - run `make test-probe-ioerr`, which builds it")
    checks = []
    for name, commands in BOOTS.items():
        checks += checks_of(name, commands)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    print(f"test-probe-ioerr: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

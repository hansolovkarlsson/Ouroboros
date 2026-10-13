#!/usr/bin/env python3
"""The boot partition the kernel names, against the disk's own table.

    python3 scripts/test-boot-partition.py   (or `make test-boot-partition`, which builds the images)

Step 1 of docs/roadmap/roadmap-one-disk.md: before the exit the kernel reads
the `HardDrive` node of its own image's device path (kernel/src/bootdisk.rs)
and logs `boot partition: N of MBR|GPT at LBA s, b blocks, ...`. Each boot
here compares that line with the partition this script finds by reading the
image's partition table on the host: the one FAT partition on an MBR disk
(type 0x0b or 0x0c), or the one entry of a GPT disk with its unique GUID.

Five boots, each to the login prompt:
  esp       build/esp.img on virtio-blk: partition 1, the whole disk
  gpt       build/espgpt.img: partition 1 of a GPT, named by its GUID
  ext2      build/espext2.img: the ESP is partition 2, after the root
  exfat     build/espexfat.img: the same with an exFAT root
  usb-boot  build/esp.img as a USB stick on QEMU's xHCI, no virtio disk:
            the path through a USB node, the case the Pi and Parallels boot

The ext2 and exFAT boots are the ones that can tell a kernel naming its boot
partition from one naming the first partition. The stick's QEMU arguments
and the stall rule are test-usb-hub.py's own (`layout`, `firmware_stalled`),
imported rather than copied: a usb-boot where QEMU's firmware stalls in its
own USB boot (no kernel line, a clean fault trace, UsbBootExecCmd last) is
INCONCLUSIVE, not a failed check and not a pass, and exits 100 as there;
anything else that keeps the kernel from printing fails. Every image's
expected line is read before the first boot, so an image this script cannot
read stops it before anything runs. About two minutes. Run it whenever
bootdisk.rs, or how an image lays out its partitions, changes.
"""
import importlib.util
import os
import re
import struct
import sys
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
_spec = importlib.util.spec_from_file_location("test_usb_hub", os.path.join(HERE, "test-usb-hub.py"))
usb_hub = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(usb_hub)

BUILD = os.path.join(ROOT, "build")
TRANSCRIPT = os.path.join(BUILD, "test-boot-partition-{}.txt")
# FAT32 (CHS and LBA) and the EFI System type: the partition the firmware
# boots on an MBR disk.
ESP_TYPES = (0x0B, 0x0C, 0xEF)
LINE = re.compile(r"boot partition: (.*)")


def expected(image):
    """The line's tail as the kernel should print it, from the image's own table."""
    with open(image, "rb") as fh:
        mbr = fh.read(512)
        fh.seek(512)
        gpt = fh.read(512)
    if gpt[:8] == b"EFI PART":
        entries_lba, count, size = struct.unpack_from("<QII", gpt, 72)
        with open(image, "rb") as fh:
            fh.seek(entries_lba * 512)
            table = fh.read(count * size)
        found = []
        for i in range(count):
            entry = table[i * size:(i + 1) * size]
            if entry[:16] != bytes(16):
                found.append((i + 1, entry))
        if len(found) != 1:
            raise SystemExit(f"{image}: expected one GPT entry, found {len(found)}")
        number, entry = found[0]
        first, last = struct.unpack_from("<QQ", entry, 32)
        guid = uuid.UUID(bytes_le=entry[16:32])
        return f"{number} of GPT at LBA {first}, {last - first + 1} blocks, partition GUID {guid}"
    signature = struct.unpack_from("<I", mbr, 440)[0]
    fats = []
    for i in range(4):
        entry = mbr[446 + 16 * i:462 + 16 * i]
        if entry[4] in ESP_TYPES:
            start, blocks = struct.unpack_from("<II", entry, 8)
            fats.append((i + 1, start, blocks))
    if len(fats) != 1:
        raise SystemExit(f"{image}: expected one ESP partition (type 0x0b, 0x0c or 0xef), found {len(fats)}")
    number, start, blocks = fats[0]
    sig = "none" if signature == 0 else f"{signature:#010x}"
    return f"{number} of MBR at LBA {start}, {blocks} blocks, disk signature {sig}"


def boot(label, image, usb, want):
    guest = drive_qemu.Guest(
        image,
        extra_args=usb_hub.layout(True, image) if usb else (),
        virtio_disk=not usb,
        label=f"test-boot-partition[{label}]: ",
    )
    try:
        reached = guest.run([("login:", "")])
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(label), "w") as fh:
        fh.write(out)
    if usb and usb_hub.firmware_stalled(out, faults):
        return None, want, "QEMU's firmware stalled in its own USB boot"
    got = LINE.search(out)
    got = got.group(1).strip() if got else "no line"
    return [
        (f"{label}: reached the login prompt", reached),
        (f"{label}: the kernel names the partition the table holds", got == want),
        (f"{label}: no fault lines", faults == 0),
    ], want, got


def main():
    boots = [
        ("esp", "esp.img", False),
        ("gpt", "espgpt.img", False),
        ("ext2", "espext2.img", False),
        ("exfat", "espexfat.img", False),
        ("usb-boot", "esp.img", True),
    ]
    # Every expectation first: an image this script cannot read stops the run
    # before any boot, not halfway through (the review of #285).
    planned = [(label, os.path.join(BUILD, image), usb) for label, image, usb in boots]
    wants = [expected(image) for _, image, _ in planned]
    failed = inconclusive = 0
    for (label, image, usb), want in zip(planned, wants):
        checks, want, got = boot(label, image, usb, want)
        print(f"     {label}: table says {want}")
        print(f"     {label}: kernel says {got}")
        if checks is None:
            print(f"INCONCLUSIVE {label}: {got}")
            inconclusive += 1
            continue
        for check, ok in checks:
            print(f"{'ok  ' if ok else 'FAIL'} {check}")
            failed += 0 if ok else 1
    print(f"transcripts: {os.path.relpath(TRANSCRIPT.format('<boot>'), ROOT)}")
    if failed:
        return 1
    return usb_hub.INCONCLUSIVE if inconclusive else 0


if __name__ == "__main__":
    sys.exit(main())

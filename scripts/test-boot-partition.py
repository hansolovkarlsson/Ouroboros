#!/usr/bin/env python3
"""The boot partition the kernel names, against the disk's own table.

    python3 scripts/test-boot-partition.py   (or `make test-boot-partition`, which builds the images)

Step 1 of docs/roadmap/roadmap-one-disk.md: before the exit the kernel reads
the `HardDrive` node of its own image's device path (kernel/src/bootdisk.rs)
and logs `boot partition: N of MBR|GPT at LBA s, b blocks, ...`. Each boot
here compares that line with the partition this script finds by reading the
image's partition table on the host: the one FAT partition on an MBR disk
(type 0x0b, 0x0c or 0xef), or the one entry of a GPT disk with its unique
GUID. The MBR images must also carry nonzero disk signatures, each
different from the others' (one-disk step 2, scripts/disksig.py), or the
run stops before any boot: that is what step 3 tells disks apart by.

Five boots to the login prompt:
  esp       build/esp.img on virtio-blk: partition 1, the whole disk
  gpt       build/espgpt.img: partition 1 of a GPT, named by its GUID
  ext2      build/espext2.img: the ESP is partition 2, after the root
  exfat     build/espexfat.img: the same with an exFAT root
  usb-boot  build/esp.img as a USB stick on QEMU's xHCI, no virtio disk:
            the path through a USB node, the case the Pi and Parallels boot

and two with a second, unrelated disk beside the boot disk (build/usbstick.img,
signed, holding USBTEST.TXT), for step 3, the block device chosen by the boot
partition (`main.rs`'s `install_boot_disk`):
  usb-boot-beside  esp.img on USB, usbstick.img on virtio: before step 3
                   virtio was always taken; now the kernel must hold it back
                   and install the stick, and `ls /` after login must show
                   the ESP's tree, not USBTEST.TXT
  virtio-beside    esp.img on virtio, usbstick.img on USB: virtio is the boot
                   disk and is installed, the stick is never asked

The ext2 and exFAT boots are the ones that can tell a kernel naming its boot
partition from one naming the first partition. The stick's QEMU arguments
and the stall rule are test-usb-hub.py's own (`layout`, `firmware_stalled`),
imported rather than copied: a usb-boot where QEMU's firmware stalls in its
own USB boot (no kernel line, a clean fault trace, UsbBootExecCmd last) is
INCONCLUSIVE, not a failed check and not a pass, and exits 100 as there;
anything else that keeps the kernel from printing fails. Every image's
expected line is read before the first boot, so an image this script cannot
read stops it before anything runs. About three minutes. Run it whenever
bootdisk.rs, main.rs's choice of block device, or how an image lays out its
partitions, changes.
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
sys.path.insert(0, HERE)
import disksig  # noqa: E402

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
    # Every MBR image carries a nonzero signature since one-disk step 2
    # (scripts/disksig.py): an unsigned one stops the run here, before any boot.
    why = disksig.check_signature(image)
    if why:
        raise SystemExit(f"{image}: {why}")
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


def boot(label, image, usb, want, beside=None):
    if beside and usb:
        # The unrelated disk on virtio; Guest attaches its `image` there, and
        # the stale-image guard reads the boot image's stamp.
        guest = drive_qemu.Guest(beside, extra_args=usb_hub.layout(True, image),
                                 stamp_from=image, label=f"test-boot-partition[{label}]: ")
    elif beside:
        guest = drive_qemu.Guest(image, extra_args=usb_hub.layout(True, beside),
                                 label=f"test-boot-partition[{label}]: ")
    else:
        guest = drive_qemu.Guest(
            image,
            extra_args=usb_hub.layout(True, image) if usb else (),
            virtio_disk=not usb,
            label=f"test-boot-partition[{label}]: ",
        )
    steps = [("login:", "")]
    if beside:
        steps = [("login:", "root"), ("assword", "root"), ("# ", "ls /"), ("# ", "echo after-ls"), ("# ", "")]
    try:
        reached = guest.run(steps)
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
    checks = [
        (f"{label}: reached the {'end' if beside else 'login prompt'}", reached),
        (f"{label}: the kernel names the partition the table holds", got == want),
        (f"{label}: no fault lines", faults == 0),
    ]
    if beside:
        listing = re.search(r"ls /\r?\n([\s\S]*?)echo after-ls", out)
        listing = listing.group(1) if listing else ""
        usb_installed = "usb-msd block device installed" in out
        virtio_installed = "block device installed for the filesystem server" in out
        if usb:
            checks += [
                (f"{label}: virtio-blk held back as not the boot disk", "virtio-blk is not the boot disk" in out),
                (f"{label}: the boot stick installed, virtio not", usb_installed and not virtio_installed),
            ]
        else:
            checks += [
                (f"{label}: virtio-blk installed, the stick not", virtio_installed and not usb_installed),
            ]
        checks += [
            (f"{label}: `ls /` shows the ESP's tree", bool(re.search(r"\bEFI\b", listing)) and "USBTEST" not in listing),
        ]
    return checks, want, got


def main():
    boots = [
        ("esp", "esp.img", False),
        ("gpt", "espgpt.img", False),
        ("ext2", "espext2.img", False),
        ("exfat", "espexfat.img", False),
        ("usb-boot", "esp.img", True),
        ("usb-boot-beside", "esp.img", True, "usbstick.img"),
        ("virtio-beside", "esp.img", False, "usbstick.img"),
    ]
    # Every expectation first: an image this script cannot read stops the run
    # before any boot, not halfway through (the review of #285).
    planned = [(b[0], os.path.join(BUILD, b[1]), b[2], os.path.join(BUILD, b[3]) if len(b) > 3 else None) for b in boots]
    try:
        wants = [expected(image) for _, image, _, _ in planned]
        disks = {image for _, image, _, _ in planned} | {beside for _, _, _, beside in planned if beside}
        mbr = {image for image in disks if not disksig.is_gpt(image)}
        sigs = {image: disksig.read_signature(image) for image in mbr}
        unsigned = [os.path.basename(image) for image in sorted(mbr) if sigs[image] == 0]
        if unsigned:
            raise SystemExit(f"test-boot-partition: unsigned images: {unsigned} (scripts/disksig.py set; `make` rebuilds its own)")
    except (OSError, ValueError) as e:
        raise SystemExit(f"test-boot-partition: {e}")
    # The MBR images booted must not share a signature: step 3 picks the block
    # device by it (the review of #286 moved this check from "the image's own
    # hash", which every boot's counter write broke, to the property itself).
    by_sig = {}
    for image in sorted(mbr):
        by_sig.setdefault(sigs[image], []).append(os.path.basename(image))
    shared = [names for names in by_sig.values() if len(names) > 1]
    if shared:
        raise SystemExit(f"images sharing a disk signature: {shared}")
    failed = inconclusive = 0
    for (label, image, usb, beside), want in zip(planned, wants):
        checks, want, got = boot(label, image, usb, want, beside)
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

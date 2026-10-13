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

and three with a second, unrelated disk beside the boot disk (build/usbstick.img,
signed, holding USBTEST.TXT), for step 3, the block device chosen by the boot
partition (`main.rs`'s `install_boot_disk`):
  usb-boot-beside  esp.img on USB, usbstick.img on virtio: before step 3
                   virtio was always taken; now the kernel must hold it back
                   and install the stick, and `ls /` after login must show
                   the ESP's tree, not USBTEST.TXT
  virtio-beside    esp.img on virtio, usbstick.img on USB: virtio is the boot
                   disk and is installed, the stick is never asked
  fallback         esp.img on NVMe, which the firmware boots and the kernel
                   cannot drive, usbstick.img on virtio and espext2.img on
                   USB: neither is the boot disk, so virtio is held, the stick
                   refused, and virtio installed as the fallback with that
                   reason (the review of #287 found no boot reached it)

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
    sig = f"{signature:#010x}"
    return f"{number} of MBR at LBA {start}, {blocks} blocks, disk signature {sig}"


# One boot: the image booted, the bus it is on, and any other disk attached,
# each named here so the checks before any boot cover every disk the boot
# attaches (the medium review of #287 found the fallback's stick hidden in
# the code that started it).
BOOTS = [
    # label, boot image, its bus, another disk on virtio, another on USB
    ("esp", "esp.img", "virtio", None, None),
    ("gpt", "espgpt.img", "virtio", None, None),
    ("ext2", "espext2.img", "virtio", None, None),
    ("exfat", "espexfat.img", "virtio", None, None),
    ("usb-boot", "esp.img", "usb", None, None),
    ("usb-boot-beside", "esp.img", "usb", "usbstick.img", None),
    ("virtio-beside", "esp.img", "virtio", None, "usbstick.img"),
    ("fallback", "esp.img", "nvme", "usbstick.img", "espext2.img"),
]


def boot(label, image, bus, want, on_virtio, on_usb):
    name = f"test-boot-partition[{label}]: "
    stick = image if bus == "usb" else on_usb
    extra = list(usb_hub.layout(True, stick)) if stick else []
    if bus == "nvme":
        # bootindex=0 pins the firmware to it: the stick beside it carries an
        # ESP of its own, and without it the firmware's order depends on the
        # order of these arguments (it booted the stick once).
        extra += ["-drive", f"file={image},format=raw,if=none,id=nvboot,snapshot=on",
                  "-device", "nvme,serial=boot,drive=nvboot,bootindex=0"]
    virtio = image if bus == "virtio" else on_virtio
    # Guest attaches its first argument on virtio; the stale-image guard reads
    # the boot image's stamp whatever bus it is on.
    guest = drive_qemu.Guest(virtio or image, extra_args=extra, virtio_disk=bool(virtio),
                             stamp_from=image, label=name)
    beside = bool(on_virtio or on_usb)
    reads_root = beside and bus != "nvme"
    steps = [("login:", "")]
    if reads_root:
        steps = [("login:", "root"), ("assword", "root"), ("# ", "ls /"), ("# ", "echo after-ls"), ("# ", "")]
    try:
        reached = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(label), "w") as fh:
        fh.write(out)
    if bus == "usb" and usb_hub.firmware_stalled(out, faults):
        return None, "QEMU's firmware stalled in its own USB boot"
    got = LINE.search(out)
    got = got.group(1).strip() if got else "no line"
    usb_installed = "usb-msd block device installed" in out
    virtio_installed = "block device installed for the filesystem server" in out
    held = "virtio-blk is not the boot disk" in out
    checks = [
        (f"{label}: reached the {'end' if reads_root else 'login prompt'}", reached),
        (f"{label}: the kernel names the partition the table holds", got == want),
        (f"{label}: no fault lines", faults == 0),
    ]
    if not beside:
        # Alone on its bus, the boot disk must match itself and be installed
        # directly: a matcher refusing its own boot disk would still reach the
        # login prompt, through the fallback (the review of #287).
        direct = usb_installed if bus == "usb" else virtio_installed
        checks.append((f"{label}: the boot disk installed as itself, not as a fallback",
                       direct and not held and "as the fallback" not in out))
    elif bus == "nvme":
        checks += [
            (f"{label}: virtio-blk held back as not the boot disk", held),
            (f"{label}: the stick refused, virtio installed as the fallback, with the reason",
             "virtio-blk installed as the fallback: the USB stick is not the boot disk either" in out
             and not usb_installed),
        ]
    else:
        if bus == "usb":
            checks += [
                (f"{label}: virtio-blk held back as not the boot disk", held),
                (f"{label}: the boot stick installed, virtio not", usb_installed and not virtio_installed),
            ]
        else:
            checks.append((f"{label}: virtio-blk installed, the stick not", virtio_installed and not usb_installed))
        listing = re.search(r"ls /\r?\n([\s\S]*?)echo after-ls", out)
        listing = listing.group(1) if listing else ""
        checks.append((f"{label}: `ls /` shows the ESP's tree",
                       bool(re.search(r"\bEFI\b", listing)) and "USBTEST" not in listing))
    return checks, got


def main():
    planned = [(label, os.path.join(BUILD, image), bus,
                os.path.join(BUILD, v) if v else None, os.path.join(BUILD, u) if u else None)
               for label, image, bus, v, u in BOOTS]
    # Every expectation first: an image this script cannot read stops the run
    # before any boot, not halfway through (the review of #285).
    try:
        wants = [expected(image) for _, image, _, _, _ in planned]
        disks = {d for row in planned for d in (row[1], row[3], row[4]) if d}
        mbr = {image for image in disks if not disksig.is_gpt(image)}
        sigs = {image: disksig.read_signature(image) for image in mbr}
    except (OSError, ValueError) as e:
        raise SystemExit(f"test-boot-partition: {e}")
    unsigned = [os.path.basename(image) for image in sorted(mbr) if sigs[image] == 0]
    if unsigned:
        raise SystemExit(f"test-boot-partition: unsigned images: {unsigned} (scripts/disksig.py set; `make` rebuilds its own)")
    # The MBR disks attached must not share a signature: step 3 picks the
    # block device by it (the review of #286 moved this check from "the
    # image's own hash", which every boot's counter write broke, to the
    # property itself).
    by_sig = {}
    for image in sorted(mbr):
        by_sig.setdefault(sigs[image], []).append(os.path.basename(image))
    shared = [names for names in by_sig.values() if len(names) > 1]
    if shared:
        raise SystemExit(f"images sharing a disk signature: {shared}")
    failed = inconclusive = 0
    for (label, image, bus, on_virtio, on_usb), want in zip(planned, wants):
        checks, got = boot(label, image, bus, want, on_virtio, on_usb)
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

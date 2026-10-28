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

and two of build/disk.img, one disk with the ESP as partition 1 and an ext2
root as partition 2, for step 4, `fsd` mounting the root because the kernel
tells it which partition it booted from (BLOCK_BOOT_START):
  disk             disk.img on virtio
  disk-usb         disk.img as a USB stick, the shape the Pi and Parallels
                   would boot: the stick must carry the start to `fsd` too
In both, `fsd` must mount ext2, and `ls /` after login must show the root's
tree, not the ESP's. Every boot checks the filesystem `fsd` mounted as `/`.

and three on copies this script makes, each disk with a signature of its own
(the reviews of #288): disk-format formats a copy of disk.img's root and
finds `/` empty and the ESP whole; disk-badroot zeroes the root's superblock,
`fsd` must say it fell back to the ESP read-only and refuse root's `touch`
there (decision D5), open /etc/passwd with O_RDONLY|O_CREAT and read it while
refusing a missing file EROFS at the open and one in a missing directory
ENOENT (/bin/CCREAT, since 2026-10-28),
and `format` must then take the damaged root, never the ESP; gpt-format runs `format` on a
copy of espgpt.img, which must be refused.

and four of build/disk-data.img, which this script builds: the ESP, an
exFAT data partition (MBR type 0x07), the ext2 root (0x83). `fsd` must take
the root by its type, not the data partition that comes first:
  disk-data          mounts the ext2 root
  disk-data-format   `format ext2` reformats the root, the data partition whole
  disk-data-badroot  its root's superblock zeroed: the ESP read-only (D5),
                     never the data partition as `/`, and `format` takes the
                     damaged root, the data partition and the ESP left whole
  disk-data-unknown  the same, the boot partition unknown to fsd (esp.img
                     booted from NVMe, this disk the fallback on virtio): an
                     untyped partition comes up read-only, and `format` still
                     takes the typed root

The ext2 and exFAT boots are the ones that can tell a kernel naming its boot
partition from one naming the first partition. The stick's QEMU arguments
are test-usb-hub.py's own (`layout`), imported rather than copied, and the
stall rule is drive-qemu.py's, every rig's: a boot QEMU's firmware never
handed to the kernel (no kernel line, a clean fault trace, the firmware's
UsbBootExecCmd or X64 line last) is INCONCLUSIVE, not a failed check and not
a pass, and exits 100 when no check failed; anything else that keeps the
kernel from printing fails. Every image's
expected line is read before the first boot, so an image this script cannot
read stops it before anything runs. About seven minutes. Run it whenever
bootdisk.rs, main.rs's choice of block device, or how an image lays out its
partitions, changes.
"""
import contextlib
import hashlib
import io
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
import mbrdisk  # noqa: E402

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
    # label, boot image, its bus, another disk on virtio, another on USB,
    # the filesystem fsd must mount as `/`, and whether the image is one disk
    # with the ESP first and a root beside it (step 4: `ls /` must show the
    # root's tree; a column, not the file's name, the high review of #288)
    ("esp", "esp.img", "virtio", None, None, "FAT32", False),
    ("gpt", "espgpt.img", "virtio", None, None, "FAT32", False),
    ("ext2", "espext2.img", "virtio", None, None, "ext2", False),
    ("exfat", "espexfat.img", "virtio", None, None, "exFAT", False),
    ("usb-boot", "esp.img", "usb", None, None, "FAT32", False),
    ("usb-boot-beside", "esp.img", "usb", "usbstick.img", None, "FAT32", False),
    ("virtio-beside", "esp.img", "virtio", None, "usbstick.img", "FAT32", False),
    ("fallback", "esp.img", "nvme", "usbstick.img", "espext2.img", "FAT32", False),
    # One disk, the ESP first and the ext2 root second (step 4): fsd must pass
    # over the boot partition and mount the root, on virtio and from a stick.
    ("disk", "disk.img", "virtio", None, None, "ext2", True),
    ("disk-usb", "disk.img", "usb", None, None, "ext2", True),
    # Copies of disk.img made by this script, each given a signature of its
    # own so no two disks here share one (the review of #288): one whose root's
    # superblock is zeroed, where fsd must say it fell back to the ESP rather
    # than run on it without a word; one that `format ext2` must reformat
    # the root of, not the ESP, the boot partition (the review found
    # find_partition took partition 1).
    ("disk-badroot", "disk-badroot.img", "virtio", None, None, "FAT32", False),
    ("disk-format", "disk-format.img", "virtio", None, None, "ext2", False),
    # A copy of espgpt.img: `format` must refuse a GPT disk, which it would
    # have laid a filesystem over from LBA 1 (the medium review of #288).
    ("gpt-format", "gpt-format.img", "virtio", None, None, "FAT32", False),
    # The root by type: the ESP, then an exFAT data partition (type 0x07),
    # then the ext2 root (0x83). Before the type was read, the data
    # partition, the first beside the ESP that mounted, was `/`. The root
    # must be; `format` must take the root and leave the data partition; and
    # with the root's superblock zeroed, fsd must fall back to the ESP
    # read-only (D5), not take the data partition as `/`.
    ("disk-data", "disk-data.img", "virtio", None, None, "ext2", True),
    ("disk-data-format", "disk-data-format.img", "virtio", None, None, "ext2", False),
    ("disk-data-badroot", "disk-data-badroot.img", "virtio", None, None, "FAT32", False),
    # The same damaged disk with the boot partition unknown to fsd: the
    # firmware boots esp.img from NVMe, which the kernel cannot drive, so the
    # virtio disk goes in as the fallback and BLOCK_BOOT_START answers 0. An
    # untyped partition may come up, read-only; `format` must still take the
    # typed root, not the ESP or the data partition that probe (the review of
    # #295 found both written over).
    ("disk-data-unknown", "esp.img", "nvme", "disk-data-unknown.img", None, "FAT32", False),
]


def mbrdisk_image(path, parts):
    """The bytes of an MBR disk of `parts`, assembled at `path`."""
    with contextlib.redirect_stdout(io.StringIO()):
        mbrdisk.assemble(path, parts)
    with open(path, "rb") as fh:
        return fh.read()


def bootdisk_entries(disk):
    return mbrdisk.entries(bytes(disk[:512]))


def make_copies():
    """disk-badroot.img and disk-format.img from disk.img, signed again."""
    source = os.path.join(BUILD, "disk.img")
    with open(source, "rb") as fh:
        disk = bytearray(fh.read())
    root_start = struct.unpack_from("<I", disk, 446 + 16 + 8)[0]
    bad = bytearray(disk)
    superblock = root_start * 512 + 1024
    bad[superblock:superblock + 1024] = bytes(1024)
    for name, data in (("disk-badroot.img", bad), ("disk-format.img", disk)):
        # A signature of the copy's own, from its name: disk-format.img is
        # byte for byte disk.img, so signing it from its bytes gives disk.img's
        # signature back, which the check below refuses.
        own = int.from_bytes(hashlib.sha256(name.encode()).digest()[:4], "little") or 1
        struct.pack_into("<I", data, 440, own)
        with open(os.path.join(BUILD, name), "wb") as fh:
            fh.write(data)
    # Three partitions, the root by type (each signed by name, as above).
    data = bytearray(mbrdisk_image(os.path.join(BUILD, "disk-data.img"), [
        (mbrdisk.ESP_TYPE, mbrdisk.esp_payload(os.path.join(BUILD, "esp.img")), True),
        (0x07, mbrdisk.exfat_payload(os.path.join(BUILD, "exfatpart.img")), False),
        (mbrdisk.EXT2_TYPE, mbrdisk.ext2_payload(os.path.join(BUILD, "ext2part.img")), False)]))
    root_start = bootdisk_entries(data)[2][2]
    bad = bytearray(data)
    bad[root_start * 512 + 1024:root_start * 512 + 2048] = bytes(1024)
    for name, image in (("disk-data.img", data), ("disk-data-format.img", bytearray(data)),
                        ("disk-data-badroot.img", bad), ("disk-data-unknown.img", bytearray(bad))):
        own = int.from_bytes(hashlib.sha256(name.encode()).digest()[:4], "little") or 1
        struct.pack_into("<I", image, 440, own)
        with open(os.path.join(BUILD, name), "wb") as fh:
            fh.write(image)
    # A GPT disk is named by its partition GUID, not the MBR signature, so
    # its copy needs none of its own: it is never attached beside espgpt.img.
    with open(os.path.join(BUILD, "espgpt.img"), "rb") as src, open(os.path.join(BUILD, "gpt-format.img"), "wb") as dst:
        dst.write(src.read())


def boot(label, image, bus, want, on_virtio, on_usb, root, one_disk):
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
    reads_root = (beside and bus != "nvme") or one_disk
    steps = [("login:", "")]
    if reads_root:
        steps = [("login:", "root"), ("assword", "root"), ("# ", "ls /"), ("# ", "echo after-ls"), ("# ", "")]
    if label == "disk-badroot":
        # Then format the damaged root: the format must take it, not the ESP
        # beside it, which is the only partition there that still probes (the
        # third review of #288 found the probe reaching the ESP).
        # First, as root, a write to the fallback `/` must be refused: it is
        # the ESP, mounted read-only (decision D5).
        # And O_RDONLY|O_CREAT (/bin/CCREAT, since 2026-10-28): an existing
        # file opens and reads, a missing one is EROFS at the open, not made,
        # and one in a missing directory is ENOENT (2), as on a writable tree.
        # FAT32 has no read-only gate of its own, so fsd's verb gate
        # (`refused_read_only`) is the only line here.
        steps = [("login:", "root"), ("assword", "root"), ("# ", "touch /rotest"),
                 ("# ", "ccreat /etc/passwd"), ("# ", "ccreat /rocreat"),
                 ("# ", "ccreat /nodir/x"), ("# ", "unmount"),
                 ("# ", "format ext2"), ("# ", "mount -a"), ("# ", "mount 0 /esp"), ("# ", "cd /esp/EFI"),
                 ("# ", "cd /"), ("# ", "")]
    if label == "gpt-format":
        steps = [("login:", "root"), ("assword", "root"), ("# ", "unmount"), ("# ", "format ext2"),
                 ("# ", "mount -a"), ("# ", "cd /EFI"), ("# ", "cd /"), ("# ", "")]
    if label == "disk-data-format":
        # Format the root, then: `/` is the empty ext2 (`cd /bin` refused),
        # and the exFAT data partition, table index 1, still holds its tree.
        steps = [("login:", "root"), ("assword", "root"), ("# ", "unmount"), ("# ", "format ext2"),
                 ("# ", "mount -a"), ("# ", "cd /bin"), ("# ", "mount 1 /data"),
                 ("# ", "cd /data/bin"), ("# ", "cd /"), ("# ", "")]
    if label in ("disk-data-badroot", "disk-data-unknown"):
        # The fallback is read-only; then `format` must take the damaged
        # typed root, table index 2, and leave the exFAT data partition,
        # index 1, and the ESP, index 0, whole.
        steps = [("login:", "root"), ("assword", "root"), ("# ", "touch /rotest"), ("# ", "unmount"),
                 ("# ", "format ext2"), ("# ", "mount -a"), ("# ", "mount 1 /data"), ("# ", "cd /data/bin"),
                 ("# ", "mount 0 /esp"), ("# ", "cd /esp/EFI"), ("# ", "cd /"), ("# ", "")]
    if label == "disk-format":
        # Format the root, then look with builtins, since the new root has no
        # /bin: `cd /bin` must fail on the empty ext2, and `cd /esp/EFI` must
        # succeed on partition index 0, the ESP, which the format left alone.
        steps = [("login:", "root"), ("assword", "root"), ("# ", "unmount"), ("# ", "format ext2"),
                 ("# ", "mount -a"), ("# ", "cd /bin"), ("# ", "mount 0 /esp"),
                 ("# ", "cd /esp/EFI"), ("# ", "cd /"), ("# ", "")]
    try:
        reached = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(label), "w") as fh:
        fh.write(out)
    # Every boot, not only one with a stick: the firmware's stall in its USB
    # boot hit virtio-beside once (2026-10-13), and its stall after its X64
    # line hit NVMe and virtio boots of other rigs (2026-10-21).
    if drive_qemu.boot_stalled("test-boot-partition", label, out, faults):
        return None, "QEMU's firmware never started the kernel"
    got = LINE.search(out)
    got = got.group(1).strip() if got else "no line"
    usb_installed = "usb-msd block device installed" in out
    virtio_installed = "block device installed for the filesystem server" in out
    held = "virtio-blk is not the boot disk" in out
    checks = [
        (f"{label}: reached the {'end' if reads_root else 'login prompt'}", reached),
        (f"{label}: the kernel names the partition the table holds", got == want),
        (f"{label}: no fault lines", faults == 0),
        (f"{label}: fsd mounts {root} as /", f"{root} mounted" in out),
    ]
    listing = re.search(r"ls /\r?\n([\s\S]*?)echo after-ls", out)
    listing = listing.group(1) if listing else ""
    if label == "disk-format":
        def answer(command):
            found = re.search(re.escape(command) + r"\r?\n([\s\S]*?)# ", out)
            return found.group(1).strip() if found else None
        bin_answer, efi_answer = answer("cd /bin"), answer("cd /esp/EFI")
        # The remount after the format, not the boot's: the line `mount -a`
        # itself prints (the medium review of #288 found the boot's line
        # alone satisfied the check).
        remount = out.split("format ext2", 1)[1] if "format ext2" in out else ""
        checks += [
            (f"{label}: `mount -a` after the format mounts ext2", "ext2 mounted" in remount),
            (f"{label}: `format ext2` reformatted the root (`cd /bin` refused)", bool(bin_answer)),
            (f"{label}: the ESP, partition 0, still holds EFI (`cd /esp/EFI` silent)", efi_answer == ""),
        ]
    elif label == "disk-data-format":
        def answer(command):
            found = re.search(re.escape(command) + r"\r?\n([\s\S]*?)# ", out)
            return found.group(1).strip() if found else None
        remount = out.split("format ext2", 1)[1] if "format ext2" in out else ""
        checks += [
            (f"{label}: `mount -a` after the format mounts ext2", "ext2 mounted" in remount),
            (f"{label}: `format ext2` reformatted the typed root (`cd /bin` refused)", bool(answer("cd /bin"))),
            (f"{label}: the exFAT data partition before it is whole (`cd /data/bin` silent)",
             answer("cd /data/bin") == ""),
        ]
    elif label in ("disk-data-badroot", "disk-data-unknown"):
        before, _, after = out.partition("format ext2")

        def answer(command):
            found = re.search(re.escape(command) + r"\r?\n([\s\S]*?)# ", after)
            return found.group(1).strip() if found else None
        if label == "disk-data-badroot":
            checks.append((f"{label}: fsd fell back to the boot partition, read-only, naming the typed root",
                           "the partition typed as the root did not mount - the boot partition is `/`, read-only"
                           in before and "exFAT mounted" not in before))
        else:
            checks.append((f"{label}: with the boot partition unknown, an untyped partition comes up read-only",
                           "the partition typed as the root did not mount - an untyped partition is `/`, read-only"
                           in before))
        checks += [
            (f"{label}: root's `touch /rotest` is refused as read-only",
             bool(re.search(r"touch /rotest\r?\n[^#]*read-only", before))),
            (f"{label}: `format ext2` takes the typed root, and `mount -a` mounts it",
             "ext2 mounted" in after),
            (f"{label}: the exFAT data partition is whole (`cd /data/bin` silent)", answer("cd /data/bin") == ""),
            (f"{label}: the ESP is whole (`cd /esp/EFI` silent)", answer("cd /esp/EFI") == ""),
        ]
    elif label == "gpt-format":
        after = out.split("format ext2", 1)[1] if "format ext2" in out else ""
        found = re.search(r"cd /EFI\r?\n([\s\S]*?)# ", out)
        checks += [
            (f"{label}: `format` refuses a GPT disk", "a GPT disk is not formatted here" in after),
            (f"{label}: the disk mounts after, its ESP whole (`cd /EFI` silent)",
             "FAT32 mounted" in after and found is not None and found.group(1).strip() == ""),
        ]
    elif label == "disk-badroot":
        before, _, after = out.partition("format ext2")
        found = re.search(r"cd /esp/EFI\r?\n([\s\S]*?)# ", after)
        checks += [
            (f"{label}: fsd says it fell back to the boot partition, read-only",
             "the boot partition is `/`, read-only" in before),
            (f"{label}: root's `touch /rotest` on the fallback is refused as read-only",
             bool(re.search(r"touch /rotest\r?\n[^#]*read-only", before))),
            (f"{label}: O_RDONLY|O_CREAT on the fallback opens /etc/passwd and reads it, and is EROFS (30) "
             f"for a missing /rocreat, not made",
             bool(re.search(r"ccreat /etc/passwd\r?\n[^#]*ccreat: /etc/passwd read [1-9]", before))
             and bool(re.search(r"ccreat /rocreat\r?\n[^#]*ccreat: /rocreat open errno 30\b", before))),
            (f"{label}: O_RDONLY|O_CREAT in a missing directory on the fallback is ENOENT (2), not EROFS",
             bool(re.search(r"ccreat /nodir/x\r?\n[^#]*ccreat: /nodir/x open errno 2\b", before))),
            (f"{label}: `format ext2` takes the damaged root, and `mount -a` mounts it",
             "ext2 mounted" in after and "no partition beside" not in after),
            (f"{label}: the ESP is whole after the format (`cd /esp/EFI` silent)",
             found is not None and found.group(1).strip() == ""),
        ]
    elif one_disk:
        # The root's tree, not the ESP's: the ESP has EFI at its top, the ext2
        # root has none (step 4's check; it fails with fsd taking partition 1).
        checks.append((f"{label}: `ls /` shows the root's tree, not the ESP's",
                       "bin/" in listing and not re.search(r"\bEFI\b", listing)))
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
            (f"{label}: {'the stick refused, ' if on_usb else ''}virtio installed as the fallback, with the reason",
             ("virtio-blk installed as the fallback: the USB stick is not the boot disk either" if on_usb
              else "virtio-blk installed as the fallback: no USB stick is attached") in out
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
        checks.append((f"{label}: `ls /` shows the ESP's tree",
                       bool(re.search(r"\bEFI\b", listing)) and "USBTEST" not in listing))
    return checks, got


def main():
    make_copies()
    planned = [(label, os.path.join(BUILD, image), bus,
                os.path.join(BUILD, v) if v else None, os.path.join(BUILD, u) if u else None, root, one)
               for label, image, bus, v, u, root, one in BOOTS]
    # Every expectation first: an image this script cannot read stops the run
    # before any boot, not halfway through (the review of #285).
    try:
        wants = [expected(image) for _, image, _, _, _, _, _ in planned]
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
    for (label, image, bus, on_virtio, on_usb, root, one_disk), want in zip(planned, wants):
        checks, got = boot(label, image, bus, want, on_virtio, on_usb, root, one_disk)
        print(f"     {label}: table says {want}")
        print(f"     {label}: kernel says {got}")
        if checks is None:
            inconclusive += 1
            continue
        for check, ok in checks:
            print(f"{'ok  ' if ok else 'FAIL'} {check}")
            failed += 0 if ok else 1
    print(f"transcripts: {os.path.relpath(TRANSCRIPT.format('<boot>'), ROOT)}")
    return drive_qemu.verdict("test-boot-partition", failed, inconclusive)


if __name__ == "__main__":
    sys.exit(main())

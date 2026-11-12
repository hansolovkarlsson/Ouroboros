#!/usr/bin/env python3
"""An SD card as the root disk: steps 1 to 4 of docs/roadmap/roadmap-sd-card.md.

    python3 scripts/test-sd.py    (or `make test-sd`, which builds the ESP and disk.img)

Each boot boots the ESP from `virtio-blk-pci`, a disk the firmware reads and
the kernel cannot (it drives virtio-mmio only), and puts a copy of
`build/disk.img` (the ESP and the ext2 root) on an SD card behind QEMU's
`sdhci-pci`, the only disk the kernel can drive, as the Pi's card and stick
are today. QEMU's firmware does not boot from an SD card itself. Two cards,
two boots each, then six more, ten boots in all:

- **A standard-capacity card**, the image padded to the smallest power of two
  of at least 128 MiB that holds it (QEMU wants a power of two, and makes a
  card of 2 GiB or less SDSC): addressed in bytes.
- **A high-capacity card**, the image in a sparse 4 GiB file (SDHC):
  addressed in 512-byte blocks.

**The first boot** of a card must name the card's kind on its kernel line,
install the card as the block device with its firmware partition named as
never written, mount the ext2 root from it, log in, read back `/etc/srcid`
exactly as the host's copy of the staged stamp has it, then write a file
(`echo ... > /sdnote`). Then the firmware partition, as `mount 0` gives it
on the Pi: `mount 0 /mnt/boot` must say the mount is read-only and why,
`ls` there must still list it, and `write /mnt/boot/sdtest hello` must be
refused as "read-only filesystem", by fsd's gate before any sector is tried:
no kernel line about a refused write in that boot. A second `mount 0`, which
binds the tree already there, must say the reason again. Until 2026-11-11 the
mount said nothing and the write said "device I/O error". Then power off.

**The second boot** of the same card must read that file back, written by the
first (step 2's writes, through `sdhci.rs`'s CMD24, out to the image and back
in through CMD17), and then try `unmount`, `erase disk`, `partition fat32`
and `format fat32`. `erase` and `partition` write sector 0, the table that
finds the firmware partition: each must be refused, the kernel's line saying
so at the first, and must say "read-only" and why, the driver's refusal carried up through `BLOCK_WRITE`
where no mount stands for fsd's gate to refuse at. `format` must not format
either, but that is no test of the guard: its target is the root (`mount`'s
rule), 24 MiB and too small for FAT32, refused before any write. Until
2026-11-11 this text and the check's name said the firmware partition; a
format that does reach it is the tenth boot's. After it the host compares the card
image's sectors from 0 to the firmware partition's end with disk.img's: byte
for byte the same, so the refusal is graded by the bytes, not by a line.

**A fifth boot**, a card whose table names nothing: disk.img's copy at the
SDSC size with its four MBR entries zeroed and its boot signature kept, so
it is not blank (`parttable::is_blank`) but lists no partition. The kernel
must keep the whole card, the one case `firmware_sectors` leaves to its
caller; with no root to mount the shell starts a root session, `partition
fat32` must be refused at sector 0, and the image must come out whole.

**A sixth boot**, the card beside a disk the kernel installs first: build/
esp.img on virtio-mmio, and disk.img's copy on the card. The card must still
be brought up and read (step 3, where on the Pi the stick stays the root),
each partition listed, partition 1 with its FAT32 boot sector read and
partition 2 named a root with its ext2 superblock read, the card named as not the boot disk, and not installed.

**A seventh boot**, the card as the boot disk beside a USB stick that is
not (step 4's ranking): disk.img itself booted from virtio-blk-pci (a
snapshot, never written), so the firmware's device path names its partition
1, which the card's byte-for-byte copy matches; build/esp.img on a USB stick
behind QEMU's xHCI. The card must be named the boot disk and installed ahead
of the stick, the stick found and not installed, `mount` must show the ext2
root on a disk of the card's size, and fsd must find the boot partition on
it. Before step 4 the stick went in first, and the card was read and left
out.

**An eighth boot**, the card as `make sdcard` stages it beside a stick
holding a root: build/esp.img booted from virtio-blk-pci and its copy on the
card, disk.img on the stick. The card is the boot disk but holds no root
partition past its firmware's, every sector of which is refused, so the
stick must go in and the card be left out, with a line saying why, and
`mount` must show the ext2 root on the stick's disk.

**A ninth boot**, disk.img booted and its copy on the card with the root's
ext2 superblock zeroed, build/esp.img on the stick: the card is the boot
disk and its partition 2 is typed a root, but holds no filesystem fsd
mounts, so the stick must go in and the card be left out.

**A tenth boot**, the firmware partition as the card's only one and the
card the only disk: build/esp.img's copy on the card, not booted from. fsd
mounts it as `/` and must say read-only and why before its mount line; a
write must be refused as "read-only filesystem" with no kernel line; then,
unmounted, `format fat32` targets that partition, the only one, and must
say "read-only" and why, the kernel's line naming the partition's first
sector as the first write it refused that boot; and the image must come
out whole.

No fault line in QEMU's trace on any boot. What it cannot see, each run as
a mutation and passing: QEMU models no data lines, so a 4-bit bus the card
was never told about (no ACMD6) reads the same as one it was; it completes a
write at once, so a write whose end is not waited for still lands; and it
models no clock rate. Those are the Pi's to show (step 3). Nor the card's
write-protect switch, which the driver reads at bring-up, at each write and
for `BLOCK_PROTECTED`: QEMU reports it for a read-only drive and refuses
such a drive as a card ("Cannot use read-only drive as SD card", tried
2026-11-11), so `WriteProtected` and a switched card's mount line are
graded by nothing here. Run it whenever kernel/src/sdhci.rs, pci::discover_sdhci, main.rs's
init_sd, install_sd_fallback or the order of the disk paths,
parttable::firmware_sectors, block.rs, the BLOCK_WRITE or BLOCK_PROTECTED
arms, fsd's disk.rs or vfs::Mount's reasons, or the shell's erase,
partition or format lines change. About ten minutes.
"""
import importlib.util
import os
import shutil
import struct
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
srcid = drive_qemu.srcid

ESP = os.path.join(ROOT, "build", "esp")
DISK = os.path.join(ROOT, "build", "disk.img")
STAMP = os.path.join(ESP, "EFI", "ORBS", "SRCID.TXT")
PROMPT = "# "
NOTE = "sd-step-2-written-by-the-first-boot"

# QEMU makes a card of 2 GiB or less standard-capacity (SDSC), a larger one
# high-capacity (SDHC), and wants every size a power of two.
SDSC_MAX = 2 << 30


def cards():
    """(name, file, size, the kernel's words for it), the sizes from
    disk.img's: the SDSC card the smallest power of two that holds it (at
    least 128 MiB), the SDHC card 4 GiB, each checked to hold the image
    whole, so a grown image is a refusal and not a cut root."""
    disk = os.path.getsize(DISK)
    sdsc = max(128 << 20, 1 << (disk - 1).bit_length())
    sdhc = 4 << 30
    if sdsc > SDSC_MAX or disk > sdhc:
        sys.exit(f"test-sd: disk.img is {disk} bytes, too big for a {SDSC_MAX}-byte SDSC card or a {sdhc}-byte SDHC one")
    return [
        ("sdsc", os.path.join(ROOT, "build", "test-sd-sdsc.img"), sdsc, "standard capacity, byte-addressed"),
        ("sdhc", os.path.join(ROOT, "build", "test-sd-sdhc.img"), sdhc, "high capacity, block-addressed"),
    ]


def card_image(path, size, source=DISK):
    """A copy of `source` (disk.img) grown to `size`, sparse past the
    image's end; never shrunk (cards() checks the size holds it)."""
    shutil.copyfile(source, path)
    assert os.path.getsize(path) <= size
    with open(path, "r+b") as fh:
        fh.truncate(size)


# fsd's line at a mount the card's driver would decline a write in.
KEPT = "fsd: mounted read-only: the disk's driver does not write here (an SD card's firmware partition or write-protect switch)"


def firmware_sectors():
    """(0, the end of disk.img's partition 1), from its MBR: the sectors the
    kernel must never write, the table and the firmware partition
    (parttable::firmware_sectors, the card not being the boot disk here and
    its table an MBR)."""
    with open(DISK, "rb") as fh:
        mbr = fh.read(512)
    start, count = struct.unpack_from("<II", mbr, 446 + 8)
    return 0, start + count


def sectors(path, start, count):
    with open(path, "rb") as fh:
        fh.seek(start * 512)
        return fh.read(count * 512)


def boot(name, tag, path, steps, login=True, virtio=None, boot_disk=None, stick=None):
    """One boot with `path` on the SD card, logged in as root, then each
    `(command, pattern)` of `steps` typed and its output up to the next
    prompt kept, then `shutdown`. Returns (driven, transcript, outputs, the
    fault count)."""
    # The ESP on virtio-blk-pci, which the kernel cannot drive; or, with
    # `virtio`, that image on virtio-mmio, which it can, and installs first;
    # or, with `boot_disk`, that whole image on virtio-blk-pci, booted from
    # and never written (a snapshot), so the firmware's device path names
    # its partition 1, which a byte-for-byte copy on the card matches.
    if virtio:
        esp = []
    elif boot_disk:
        esp = [
            "-drive", f"file={boot_disk},format=raw,if=none,id=esp,snapshot=on",
            "-device", "virtio-blk-pci,drive=esp",
        ]
    else:
        esp = [
            "-drive", f"file=fat:rw:{ESP},format=raw,media=disk,if=none,id=esp",
            "-device", "virtio-blk-pci,drive=esp",
        ]
    # With `stick`, that image on a USB stick behind QEMU's xHCI, a snapshot.
    usb = [
        "-device", "qemu-xhci,id=xhci0",
        "-drive", f"file={stick},format=raw,if=none,id=usbstick,snapshot=on",
        "-device", "usb-storage,drive=usbstick,bus=xhci0.0,port=1",
    ] if stick else []
    guest = drive_qemu.Guest(
        virtio or path, label=f"test-sd ({name} {tag}): ", virtio_disk=bool(virtio),
        stamp_from=None if virtio else (boot_disk or ESP),
        extra_args=[
            *esp,
            *usb,
            "-device", "sdhci-pci",
            "-drive", f"file={path},format=raw,if=none,id=sd0",
            "-device", "sd-card,drive=sd0",
        ],
    )
    outputs, out, faults, ok = [], "", None, False
    try:
        if login:
            ok = guest.wait_for("login:", timeout=120)
            ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        else:
            # No root, so no /etc/passwd: the shell starts a root session,
            # and netd's start-up lines follow its prompt; typed into them a
            # line is lost, so after netd's last, a bare Enter for a fresh one.
            ok = guest.wait_for("starting a root session", timeout=120)
            ok = ok and guest.wait_for("cannot make remote requests")
            if ok:
                guest.type_line("")
                ok = guest.wait_for(PROMPT)
        for command, pattern in steps:
            if not ok:
                break
            start = len(guest.transcript())
            guest.type_line(command)
            ok = guest.wait_for(pattern + r"[\s\S]*" + PROMPT)
            outputs.append(guest.transcript()[start:].replace("\r\n", "\n"))
        if ok:
            # Off by PSCI, so QEMU exits and the image is closed whole.
            guest.type_line("shutdown")
            deadline = time.time() + 30
            while guest.proc.poll() is None and time.time() < deadline:
                time.sleep(0.2)
            ok = guest.proc.poll() is not None
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", f"test-sd-{name}-{tag}.txt"), "w") as fh:
        fh.write(out)
    return ok, out, outputs, faults


def main() -> int:
    # disk.img is built from the same tree as the ESP the Guest checks.
    srcid.require_current(DISK, "test-sd: ")
    with open(STAMP) as fh:
        stamp = fh.read()
    first, count = firmware_sectors()
    kept = sectors(DISK, first, count)
    checks = []
    for name, path, size, kind in cards():
        card_image(path, size)
        try:
            ok1, out1, got1, faults1 = boot(name, "write", path, [
                ("cat /etc/srcid", r"\nend\r?\n"),
                (f"echo {NOTE} > /sdnote", ""),
                ("cat /sdnote", NOTE),
                ("mount 0 /mnt/boot", ""),
                ("ls /mnt/boot", "EFI"),
                ("write /mnt/boot/sdtest hello", "write: "),
                ("mount 0 /mnt/again", ""),
            ])
            ok2, out2, got2, faults2 = boot(name, "reread", path, [
                ("cat /sdnote", NOTE),
                ("unmount", "unmounted"),
                ("erase disk", "erase: "),
                ("partition fat32", "partition: "),
                ("format fat32", "format: "),
            ])
            untouched = sectors(path, first, count) == kept
        finally:
            # The SDHC card is 4 GiB to the filesystem, sparse or not.
            os.remove(path)
        reread = got2[0] if got2 else ""
        mounted = got1[3] if len(got1) > 3 else ""
        written = got1[5] if len(got1) > 5 else ""
        again = got1[6] if len(got1) > 6 else ""
        erased = got2[2] if len(got2) > 2 else ""
        partitioned = got2[3] if len(got2) > 3 else ""
        formatted = got2[4] if len(got2) > 4 else ""
        checks += [
            (f"{name}: both boots driven to the end and powered off", ok1 and ok2),
            (f"{name}: the kernel names the card ({kind})", "sd: SDHCI version field" in out1 and kind in out1),
            (f"{name}: the card is the block device", "sd: block device installed for the filesystem server" in out1),
            (f"{name}: sectors {first} to {first + count - 1} named as never written",
             f"sd: sectors {first} to {first + count - 1} hold the card's firmware partition or the table that finds it, never written" in out1),
            (f"{name}: the ext2 root mounted from it", "fsd: ext2 mounted" in out1),
            (f"{name}: /etc/srcid read back as the host has it", stamp.strip() != "" and bool(got1) and stamp in got1[0]),
            (f"{name}: a file written in the first boot reads back in the second", f"\n{NOTE}\n" in reread),
            (f"{name}: `mount 0` says the firmware partition is read-only, and why", KEPT in mounted),
            (f"{name}: a write there is refused as read-only, not as an I/O error",
             "write: read-only filesystem" in written and "I/O error" not in written),
            (f"{name}: a second `mount 0`, of the tree already there, says it again", KEPT in again),
            (f"{name}: refused by fsd's gate, no sector tried: no kernel line about a refused write in that boot",
             "sd: a write to sector" not in out1),
            (f"{name}: erase's write to sector 0 refused by the kernel, and said as read-only",
             "sd: a write to sector 0 refused" in erased and "erase: read-only: " in erased and "I/O error" not in erased),
            (f"{name}: partition's write to sector 0 refused by the kernel",
             "sd: a write to sector 0 refused" in out2 and "partitioned" not in partitioned),
            (f"{name}: partition says read-only, not an I/O error",
             "partition: read-only: " in partitioned and "I/O error" not in partitioned),
            (f"{name}: format does not format (its target the root, too small for FAT32)",
             bool(formatted) and "formatted" not in formatted),
            (f"{name}: sectors {first} to {first + count - 1} unchanged on the card", untouched),
            (f"{name}: no fault lines", faults1 == 0 and faults2 == 0),
        ]
    # The fifth boot: a table that lists nothing on a card that is not blank.
    path = os.path.join(ROOT, "build", "test-sd-notable.img")
    name, _, size, _ = cards()[0]
    card_image(path, size)
    with open(path, "r+b") as fh:
        fh.seek(446)
        fh.write(bytes(64))
    with open(path, "rb") as fh:
        before = fh.read()
    try:
        ok, out, got, faults = boot("notable", "partition", path, [("partition fat32", "partition: ")], login=False)
        with open(path, "rb") as fh:
            whole = fh.read() == before
    finally:
        os.remove(path)
    last = size // 512 - 1
    checks += [
        ("notable: driven to the end and powered off", ok),
        (f"notable: the whole card (sectors 0 to {last}) named as never written",
         f"sd: sectors 0 to {last} hold the card's firmware partition or the table that finds it, never written" in out),
        ("notable: partition's write to sector 0 refused by the kernel",
         "sd: a write to sector 0 refused" in out and bool(got) and "partitioned" not in got[0]),
        ("notable: partition says read-only, not an I/O error",
         bool(got) and "partition: read-only: " in got[0] and "I/O error" not in got[0]),
        ("notable: the card image unchanged", whole),
        ("notable: no fault lines", faults == 0),
    ]
    # The sixth boot: the card read beside a disk installed before it.
    path = os.path.join(ROOT, "build", "test-sd-beside.img")
    card_image(path, cards()[0][2])
    try:
        ok, out, _, faults = boot("beside", "virtio", path, [], virtio=os.path.join(ROOT, "build", "esp.img"))
    finally:
        os.remove(path)
    root_start = struct.unpack_from("<I", open(DISK, "rb").read(512), 446 + 16 + 8)[0]
    checks += [
        ("beside: driven to the end and powered off", ok),
        ("beside: the card brought up beside the installed disk", "sd: sector 0 read, signature 0x55aa" in out),
        (f"beside: partition 2 (LBA {root_start}) listed as a root, its ext2 superblock read",
         f"sd: MBR partition 2: LBA {root_start}," in out and "a root, its ext2 superblock read" in out),
        ("beside: partition 1 (LBA 2048) listed with its FAT32 boot sector read",
         "sd: MBR partition 1: LBA 2048," in out and "a FAT32 boot sector read" in out),
        ("beside: the card named as not the boot disk", "sd: the card is not the boot disk" in out),
        ("beside: the card not installed", "sd: the card is up and read, not installed: another disk is" in out
         and "sd: block device installed" not in out),
        ("beside: no fault lines", faults == 0),
    ]
    # The seventh boot: the card as the boot disk, beside a USB stick that
    # is not (step 4's ranking). disk.img booted from virtio-blk-pci, its copy
    # on the card, esp.img on the stick.
    path = os.path.join(ROOT, "build", "test-sd-ranked.img")
    card_image(path, cards()[0][2])
    try:
        ok, out, got, faults = boot("ranked", "stick", path, [("mount", "mounted at")],
                                    boot_disk=DISK, stick=os.path.join(ROOT, "build", "esp.img"))
    finally:
        os.remove(path)
    sectors_on_card = cards()[0][2] // 512
    checks += [
        ("ranked: driven to the end and powered off", ok),
        ("ranked: the stick found by the xHCI scan", "USB mass storage - activating after the scan" in out),
        ("ranked: the card named the boot disk", "sd: the card is the boot disk (its boot partition at LBA 2048)" in out),
        ("ranked: the card installed ahead of the stick",
         "sd: block device installed for the filesystem server, the boot disk, ahead of any USB stick" in out),
        ("ranked: the stick not installed, and the line says why",
         "usb-msd block device installed" not in out and "usb-msd not installed: the SD card is the boot disk" in out),
        (f"ranked: the ext2 root mounted from the card ({sectors_on_card} sectors)",
         bool(got) and f"ext2 mounted at partition LBA {root_start} (disk {sectors_on_card} sectors" in got[0]),
        ("ranked: fsd finds the boot partition on its disk", "fsd: the boot partition is on this disk, at LBA 2048" in out),
        ("ranked: no fault lines", faults == 0),
    ]
    # The eighth boot: the card as `make sdcard` stages it, the firmware's
    # partition alone, booted from, beside a stick holding a root. The card
    # is the boot disk but has no root of its own past its protected
    # sectors, so the stick goes in, as on the Pi before step 4.
    esp_img = os.path.join(ROOT, "build", "esp.img")
    path = os.path.join(ROOT, "build", "test-sd-fwonly.img")
    card_image(path, cards()[0][2], source=esp_img)
    try:
        ok, out, got, faults = boot("fwonly", "stick", path, [("mount", "mounted at")],
                                    boot_disk=esp_img, stick=DISK)
    finally:
        os.remove(path)
    esp_start = struct.unpack_from("<I", open(esp_img, "rb").read(512), 446 + 8)[0]
    disk_sectors = os.path.getsize(DISK) // 512
    checks += [
        ("fwonly: driven to the end and powered off", ok),
        ("fwonly: the card named the boot disk", f"sd: the card is the boot disk (its boot partition at LBA {esp_start})" in out),
        ("fwonly: the card said to hold no root of its own",
         "sd: the card is the boot disk but holds no root past its firmware's partition" in out),
        ("fwonly: the stick installed, the card not",
         "usb-msd block device installed" in out and "sd: the card is up and read, not installed: another disk is" in out
         and "sd: block device installed" not in out),
        (f"fwonly: the ext2 root mounted from the stick ({disk_sectors} sectors)",
         bool(got) and f"ext2 mounted at partition LBA {root_start} (disk {disk_sectors} sectors" in got[0]),
        ("fwonly: no fault lines", faults == 0),
    ]
    # The ninth boot: disk.img booted, its copy on the card with the root's
    # ext2 superblock zeroed, esp.img on the stick. The card is the boot disk
    # and its partition 2 is typed a root, but holds no filesystem fsd
    # mounts, so the stick goes in (the review of 0aa8c177: a typed root
    # alone had ranked the card ahead, and fsd then fell back to the
    # firmware's partition, every write refused).
    path = os.path.join(ROOT, "build", "test-sd-nofs.img")
    card_image(path, cards()[0][2])
    with open(path, "r+b") as fh:
        fh.seek((root_start + 2) * 512)
        fh.write(bytes(512))
    try:
        ok, out, got, faults = boot("nofs", "stick", path, [("mount", "mounted at")], boot_disk=DISK, stick=esp_img)
    finally:
        os.remove(path)
    esp_sectors = os.path.getsize(esp_img) // 512
    checks += [
        ("nofs: driven to the end and powered off", ok),
        ("nofs: the card named the boot disk", "sd: the card is the boot disk (its boot partition at LBA 2048)" in out),
        ("nofs: its root named a root with no ext2 superblock read", "a root, no ext2 superblock read" in out),
        ("nofs: the card said to hold no root with a filesystem",
         "sd: the card is the boot disk but holds no root past its firmware's partition" in out),
        ("nofs: the stick installed, the card not",
         "usb-msd block device installed" in out and "sd: the card is up and read, not installed: another disk is" in out
         and "sd: block device installed" not in out),
        (f"nofs: the root mounted from the stick ({esp_sectors} sectors)", bool(got) and f"(disk {esp_sectors} sectors" in got[0]),
        ("nofs: no fault lines", faults == 0),
    ]
    # The tenth boot: the firmware partition alone on the only disk, so `/`
    # is the partition no write reaches and `format` targets it.
    path = os.path.join(ROOT, "build", "test-sd-fwalone.img")
    card_image(path, cards()[0][2], source=esp_img)
    with open(path, "rb") as fh:
        before = fh.read()
    try:
        ok, out, got, faults = boot("fwalone", "format", path, [
            ("write /sdtest hello", "write: "),
            ("unmount", "unmounted"),
            ("format fat32", "format: "),
        ])
        with open(path, "rb") as fh:
            whole = fh.read() == before
    finally:
        os.remove(path)
    written = got[0] if got else ""
    formatted = got[2] if len(got) > 2 else ""
    refusals = [line for line in out.replace("\r\n", "\n").split("\n") if "sd: a write to sector" in line]
    checks += [
        ("fwalone: driven to the end and powered off", ok),
        ("fwalone: the card is the block device", "sd: block device installed for the filesystem server" in out),
        ("fwalone: `/` said to be read-only, and why, before its mount line",
         KEPT in out and "fsd: FAT32 mounted" in out and out.index(KEPT) < out.index("fsd: FAT32 mounted")),
        ("fwalone: a write to `/` refused as read-only, not as an I/O error",
         "write: read-only filesystem" in written and "I/O error" not in written),
        ("fwalone: format over the firmware partition says read-only, not an I/O error",
         "format: read-only: " in formatted and "I/O error" not in formatted),
        (f"fwalone: the kernel's first refused write is format's, at the partition's first sector ({esp_start})",
         bool(refusals) and f"sd: a write to sector {esp_start} refused" in refusals[0]),
        ("fwalone: the card image unchanged", whole),
        ("fwalone: no fault lines", faults == 0),
    ]
    failed = 0
    for check, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {check}")
        failed += 0 if good else 1
    print("transcripts: build/test-sd-{sdsc,sdhc}-{write,reread}.txt, build/test-sd-notable-partition.txt, build/test-sd-beside-virtio.txt, build/test-sd-ranked-stick.txt, build/test-sd-fwonly-stick.txt, build/test-sd-nofs-stick.txt, build/test-sd-fwalone-format.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

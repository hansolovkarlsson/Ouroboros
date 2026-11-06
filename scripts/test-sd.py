#!/usr/bin/env python3
"""An SD card as the root disk: steps 1 and 2 of docs/roadmap/roadmap-sd-card.md.

    python3 scripts/test-sd.py    (or `make test-sd`, which builds the ESP and disk.img)

Each boot boots the ESP from `virtio-blk-pci`, a disk the firmware reads and
the kernel cannot (it drives virtio-mmio only), and puts a copy of
`build/disk.img` (the ESP and the ext2 root) on an SD card behind QEMU's
`sdhci-pci`, the only disk the kernel can drive, as the Pi's card and stick
are today. QEMU's firmware does not boot from an SD card itself. Two cards,
two boots each, four boots in all:

- **A standard-capacity card**, the image padded to the smallest power of two
  of at least 128 MiB that holds it (QEMU wants a power of two, and makes a
  card of 2 GiB or less SDSC): addressed in bytes.
- **A high-capacity card**, the image in a sparse 4 GiB file (SDHC):
  addressed in 512-byte blocks.

**The first boot** of a card must name the card's kind on its kernel line,
install the card as the block device with its firmware partition named as
never written, mount the ext2 root from it, log in, read back `/etc/srcid`
exactly as the host's copy of the staged stamp has it, then write a file
(`echo ... > /sdnote`) and power off.

**The second boot** of the same card must read that file back, written by the
first (step 2's writes, through `sdhci.rs`'s CMD24, out to the image and back
in through CMD17), and then try `unmount`, `partition fat32` and `format
fat32`. `partition` writes sector 0, the table that finds the firmware
partition, and `format` lays a filesystem over the card's first partition,
the firmware's (the card is not the boot disk here): both must be refused,
the kernel's line saying so at the first. After it the host compares the card
image's sectors from 0 to the firmware partition's end with disk.img's: byte
for byte the same, so the refusal is graded by the bytes, not by a line.

**A fifth boot**, a card whose table names nothing: disk.img's copy at the
SDSC size with its four MBR entries zeroed and its boot signature kept, so
it is not blank (`parttable::is_blank`) but lists no partition. The kernel
must keep the whole card, the one case `firmware_sectors` leaves to its
caller; with no root to mount the shell starts a root session, `partition
fat32` must be refused at sector 0, and the image must come out whole.

No fault line in QEMU's trace on any boot. What it cannot see, each run as
a mutation and passing: QEMU models no data lines, so a 4-bit bus the card
was never told about (no ACMD6) reads the same as one it was; it completes a
write at once, so a write whose end is not waited for still lands; and it
models no clock rate. Those are the Pi's to show (step 3). Run it whenever kernel/src/sdhci.rs, pci::discover_sdhci, main.rs's
init_sd, parttable::firmware_partition or block.rs changes. About four
minutes.
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


def card_image(path, size):
    """A copy of disk.img grown to `size`, sparse past the image's end;
    never shrunk (cards() checks the size holds it)."""
    shutil.copyfile(DISK, path)
    assert os.path.getsize(path) <= size
    with open(path, "r+b") as fh:
        fh.truncate(size)


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


def boot(name, tag, path, steps, login=True):
    """One boot with `path` on the SD card, logged in as root, then each
    `(command, pattern)` of `steps` typed and its output up to the next
    prompt kept, then `shutdown`. Returns (driven, transcript, outputs, the
    fault count)."""
    guest = drive_qemu.Guest(
        path, label=f"test-sd ({name} {tag}): ", virtio_disk=False, stamp_from=ESP,
        extra_args=[
            "-drive", f"file=fat:rw:{ESP},format=raw,media=disk,if=none,id=esp",
            "-device", "virtio-blk-pci,drive=esp",
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
            ])
            ok2, out2, got2, faults2 = boot(name, "reread", path, [
                ("cat /sdnote", NOTE),
                ("unmount", "unmounted"),
                ("partition fat32", "partition: "),
                ("format fat32", "format: "),
            ])
            untouched = sectors(path, first, count) == kept
        finally:
            # The SDHC card is 4 GiB to the filesystem, sparse or not.
            os.remove(path)
        reread = got2[0] if got2 else ""
        partitioned = got2[2] if len(got2) > 2 else ""
        formatted = got2[3] if len(got2) > 3 else ""
        checks += [
            (f"{name}: both boots driven to the end and powered off", ok1 and ok2),
            (f"{name}: the kernel names the card ({kind})", "sd: SDHCI version field" in out1 and kind in out1),
            (f"{name}: the card is the block device", "sd: block device installed for the filesystem server" in out1),
            (f"{name}: sectors {first} to {first + count - 1} named as never written",
             f"sd: sectors {first} to {first + count - 1} hold the card's firmware partition or the table that finds it, never written" in out1),
            (f"{name}: the ext2 root mounted from it", "fsd: ext2 mounted" in out1),
            (f"{name}: /etc/srcid read back as the host has it", stamp.strip() != "" and bool(got1) and stamp in got1[0]),
            (f"{name}: a file written in the first boot reads back in the second", f"\n{NOTE}\n" in reread),
            (f"{name}: partition's write to sector 0 refused by the kernel",
             "sd: a write to sector 0 refused" in out2 and "partitioned" not in partitioned),
            (f"{name}: format over the firmware partition refused", bool(formatted) and "formatted" not in formatted),
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
        ("notable: the card image unchanged", whole),
        ("notable: no fault lines", faults == 0),
    ]
    failed = 0
    for check, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {check}")
        failed += 0 if good else 1
    print("transcripts: build/test-sd-{sdsc,sdhc}-{write,reread}.txt, build/test-sd-notable-partition.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

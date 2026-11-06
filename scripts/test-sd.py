#!/usr/bin/env python3
"""An SD card as the root disk: step 1 of docs/roadmap/roadmap-sd-card.md.

    python3 scripts/test-sd.py    (or `make test-sd`, which builds the ESP and disk.img)

Two QEMU boots. Each boots the ESP from `virtio-blk-pci`, a disk the
firmware reads and the kernel cannot (it drives virtio-mmio only), and puts
a copy of `build/disk.img` (the ESP and the ext2 root) on an SD card behind
QEMU's `sdhci-pci`, the only disk the kernel can drive, as the Pi's card and
stick are today. QEMU's firmware does not boot from an SD card itself.

1. **A standard-capacity card**, the image padded to 128 MiB (QEMU wants a
   power of two, and makes a card of 2 GiB or less SDSC): addressed in
   bytes.
2. **A high-capacity card**, the image in a sparse 4 GiB file (SDHC):
   addressed in 512-byte blocks.

Each must: name the card's kind on its kernel line, install the card as the
block device, mount the ext2 root from it, log in, and read back
`/etc/srcid` exactly as the host's copy of the staged stamp has it, with no
fault line in QEMU's trace. The one-sector reads through `sdhci.rs` are then
what every file came by. Writes are refused until step 2.

What it cannot see: QEMU models no data lines, so a 4-bit bus the card was
never told about (no ACMD6) reads the same as one it was; and no clock rate.
Those are the Pi's to show (step 3). Run it whenever kernel/src/sdhci.rs,
pci::discover_sdhci, main.rs's init_sd or block.rs changes. About two
minutes.
"""
import importlib.util
import os
import shutil
import sys

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

# QEMU makes a card of 2 GiB or less standard-capacity (SDSC), a larger one
# high-capacity (SDHC), and wants every size a power of two.
SDSC_MAX = 2 << 30


def power_of_two_at_least(n):
    p = 1
    while p < n:
        p *= 2
    return p


def cards():
    """(name, file, size, the kernel's words for it), the sizes from
    disk.img's: the SDSC card the smallest power of two that holds it (at
    least 128 MiB), the SDHC card 4 GiB, each checked to hold the image
    whole, so a grown image is a refusal and not a cut root."""
    disk = os.path.getsize(DISK)
    sdsc = max(128 << 20, power_of_two_at_least(disk))
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


def boot(name, path):
    """One boot with `path` on the SD card: the transcript, the `cat
    /etc/srcid` segment and the fault count."""
    guest = drive_qemu.Guest(
        path, label=f"test-sd ({name}): ", virtio_disk=False, stamp_from=ESP,
        extra_args=[
            "-drive", f"file=fat:rw:{ESP},format=raw,media=disk,if=none,id=esp",
            "-device", "virtio-blk-pci,drive=esp",
            "-device", "sdhci-pci",
            "-drive", f"file={path},format=raw,if=none,id=sd0",
            "-device", "sd-card,drive=sd0",
        ],
    )
    cat, out, faults, ok = "", "", None, False
    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            start = len(guest.transcript())
            guest.type_line("cat /etc/srcid")
            ok = guest.wait_for(r"\nend\r?\n[\s\S]*" + PROMPT)
            cat = guest.transcript()[start:]
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", f"test-sd-{name}.txt"), "w") as fh:
        fh.write(out)
    return ok, out, cat, faults


def main() -> int:
    # disk.img is built from the same tree as the ESP the Guest checks.
    srcid.require_current(DISK, "test-sd: ")
    with open(STAMP) as fh:
        stamp = fh.read()
    checks = []
    for name, path, size, kind in cards():
        card_image(path, size)
        try:
            ok, out, cat, faults = boot(name, path)
        finally:
            # The SDHC card is 4 GiB to the filesystem, sparse or not.
            os.remove(path)
        sent = cat.replace("\r\n", "\n")
        checks += [
            (f"{name}: driven to the end", ok),
            (f"{name}: the kernel names the card ({kind})", f"sd: SDHCI version field" in out and kind in out),
            (f"{name}: the card is the block device", "sd: block device installed for the filesystem server" in out),
            (f"{name}: the ext2 root mounted from it", "fsd: ext2 mounted" in out),
            (f"{name}: /etc/srcid read back as the host has it", stamp.strip() != "" and stamp in sent),
            (f"{name}: no fault lines", faults == 0),
        ]
    failed = 0
    for check, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {check}")
        failed += 0 if good else 1
    print("transcripts: build/test-sd-sdsc.txt, build/test-sd-sdhc.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""`make sdcard`'s boot counter, against a FAT32 image file.

    python3 scripts/test-sdcard.py   (or `make test-sdcard`, which stages build/esp)

scripts/sdcard.sh re-stages a Pi card's boot partition and replaces
EFI/ORBS whole. EFI/ORBS/BOOTID.TXT is the boot counter, and on the Pi it is
the counter's only store: the firmware keeps no variable for it (every Pi
boot since 2026-10-01). The build stages it as 0, so a re-stage that copied
it rolled the count back, and a later boot could hand out a number an earlier
one had (seen on the board 2026-11-07: boot 6, `make sdcard`, boot 1). The
script now keeps the larger of the card's value and the build's, as
scripts/bootstick.py does.

One image is made with hdiutil (FAT32 on an MBR disk) and attached, so the
volume sits under /Volumes as a card in a reader does and passes the
script's own guards. A file named RPI_EFI.fd at its root makes it a Pi card
the script takes as its own; the script keeps an existing RPI_EFI.fd, so no
firmware is fetched. Four runs, the card's EFI/ORBS set before each:

  card-larger   the card at 6, the build at 0: 6 is kept, and the script
                says so. Without the carry the card reads 0 (the board's
                rollback), so this is the case that fails on the old script.
  build-larger  the card at 6, an ESP tree staged at 9: 9, so the carry
                never lowers a count.
  not-a-record  the card's file is "6" and a newline, not 20 digits: the
                build's 0, and the script names the record it refused.
  no-counter    no EFI/ORBS on the card: the build's 0, and no line.

Each run must also have replaced the rest of EFI/ORBS (SRCID.TXT is the
build's, byte for byte), so the carry is not mistaken for a copy that never
happened, and must leave RPI_EFI.fd as it was.

macOS only (hdiutil, diskutil). Under a minute; run it whenever
scripts/sdcard.sh, the counter's record format (kernel/src/bootid.rs) or
where the ESP stages it (the Makefile's esp target) changes.
"""
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ESP_DIR = os.path.join(ROOT, "build", "esp")
WORK = os.path.join(ROOT, "build", "test-sdcard")
IMG = os.path.join(WORK, "card.dmg")
VOLNAME = "ORBSSDTEST"
FIRMWARE = b"not firmware, a stand-in the script must keep\n"

failures = []


def check(name, ok, detail=""):
    print(f"  {'ok  ' if ok else 'FAIL'} {name}{': ' + detail if detail and not ok else ''}")
    if not ok:
        failures.append(name)


def record(n):
    return b"%020d\n" % n


def attach():
    out = subprocess.run(["hdiutil", "attach", "-nobrowse", IMG],
                         check=True, capture_output=True, text=True).stdout
    dev, mount = None, None
    for line in out.splitlines():
        parts = line.split("\t")
        if parts and parts[0].strip().startswith("/dev/disk") and dev is None:
            dev = parts[0].strip()
        if len(parts) >= 3 and parts[-1].strip().startswith("/Volumes/"):
            mount = parts[-1].strip()
    if not dev or not mount:
        sys.exit(f"test-sdcard: hdiutil attach printed no device or mount point:\n{out}")
    return dev, mount


def stage(esp_dir, card):
    env = dict(os.environ, SDCARD=card, ESP_DIR=esp_dir,
               CACHE_DIR=os.path.join(ROOT, "build", "cache"), EJECT="0")
    env.pop("STICK", None)
    env.pop("FIRMWARE", None)
    p = subprocess.run([os.path.join(ROOT, "scripts", "sdcard.sh")], env=env,
                       capture_output=True, text=True)
    return p.returncode, p.stdout + p.stderr


def set_card(card, counter):
    orbs = os.path.join(card, "EFI", "ORBS")
    shutil.rmtree(os.path.join(card, "EFI"), ignore_errors=True)
    if counter is not None:
        os.makedirs(orbs)
        with open(os.path.join(orbs, "BOOTID.TXT"), "wb") as f:
            f.write(counter)
    subprocess.run(["sync"], check=True)


def read(path):
    with open(path, "rb") as f:
        return f.read()


def main():
    if sys.platform != "darwin":
        sys.exit("test-sdcard: macOS only (hdiutil, diskutil)")
    srcid = os.path.join(ESP_DIR, "EFI", "ORBS", "SRCID.TXT")
    if read(os.path.join(ESP_DIR, "EFI", "ORBS", "BOOTID.TXT")) != record(0):
        sys.exit("test-sdcard: build/esp's BOOTID.TXT is not the record of 0 this rig assumes (run make esp)")
    if not os.path.isfile(srcid):
        sys.exit("test-sdcard: build/esp has no EFI/ORBS/SRCID.TXT (run make esp)")
    if os.path.exists(os.path.join("/Volumes", VOLNAME)):
        sys.exit(f"test-sdcard: /Volumes/{VOLNAME} is already mounted; eject it first")

    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    # An ESP tree staged at 9, for the case where the build's value is the
    # larger: a copy of build/esp with only the counter changed.
    esp9 = os.path.join(WORK, "esp9")
    shutil.copytree(ESP_DIR, esp9, symlinks=True)
    with open(os.path.join(esp9, "EFI", "ORBS", "BOOTID.TXT"), "wb") as f:
        f.write(record(9))

    subprocess.run(["hdiutil", "create", "-quiet", "-size", "64m", "-fs", "MS-DOS FAT32",
                    "-volname", VOLNAME, "-layout", "MBRSPUD", IMG], check=True)
    dev, card = attach()
    try:
        with open(os.path.join(card, "RPI_EFI.fd"), "wb") as f:
            f.write(FIRMWARE)
        cases = [
            ("card-larger", ESP_DIR, record(6), record(6), "boot counter kept at 00000000000000000006"),
            ("build-larger", esp9, record(6), record(9), None),
            ("not-a-record", ESP_DIR, b"6\n", record(0), "is not a counter record"),
            ("no-counter", ESP_DIR, None, record(0), None),
        ]
        for name, esp, before, want, line in cases:
            print(f"{name}:")
            set_card(card, before)
            rc, out = stage(esp, card)
            check(f"{name}: sdcard.sh exits 0", rc == 0, out.strip()[-400:])
            got_path = os.path.join(card, "EFI", "ORBS", "BOOTID.TXT")
            got = read(got_path) if os.path.exists(got_path) else None
            check(f"{name}: the card's counter is {want.strip().decode()}", got == want,
                  f"read {got!r}")
            if line:
                check(f"{name}: the run says '{line}'", line in out, out.strip()[-400:])
            else:
                check(f"{name}: no counter line", "boot counter" not in out, out.strip()[-400:])
            check(f"{name}: SRCID.TXT is the build's",
                  read(os.path.join(card, "EFI", "ORBS", "SRCID.TXT")) == read(srcid))
            check(f"{name}: RPI_EFI.fd kept", read(os.path.join(card, "RPI_EFI.fd")) == FIRMWARE)
    finally:
        subprocess.run(["hdiutil", "detach", "-quiet", "-force", dev])
    shutil.rmtree(WORK, ignore_errors=True)

    if failures:
        print(f"test-sdcard: {len(failures)} check(s) failed")
        return 1
    print("test-sdcard: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())

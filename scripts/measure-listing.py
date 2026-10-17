#!/usr/bin/env python3
"""The sectors fsd reads to list a directory, measured.

    python3 scripts/measure-listing.py [DIR]    (or `make measure-listing`, which builds the images)

For each image (FAT32 build/esp.img, ext2 build/espext2.img, exFAT
build/espexfat.img), two boots of a copy, each logging in and then running
one command twice: `ls DIR/stdio.h`, one stat and no listing, and `ls DIR`
(DIR is /include unless given). QEMU traces every virtio-blk read
(`-trace enable=virtio_blk_handle_read`, into the -d log), so the reads are
counted by QEMU, not by anything in the guest. Both boots spawn the same
binary the same number of times, so the difference over the two runs is what
one listing of DIR reads. The baseline is `ls` on a file rather than another
command because spawning reads the binary 512 bytes a call, each call
resolving its path from the root: a different binary's size would swamp the
listing.

Not a pass/fail rig: a measurement, to be run on the tree and on a build
without the change being judged. Its first use (2026-10-17) was the early
stop of an NP_READDIR page's walk: /include is two pages, and one listing
read 58 sectors on FAT32, 919 on ext2 and 106 on exFAT before, 55, 883 and
100 after. About five minutes.
"""
import collections
import importlib.util
import os
import re
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGES = [("fat32", "esp.img"), ("ext2", "espext2.img"), ("exfat", "espexfat.img")]
RUNS = 2
READ = re.compile(rb"virtio_blk_handle_read .*?sector (\d+) nsectors (\d+)")


def reads(name, image, command):
    """Boot a copy of `image`, log in, run `command` RUNS times; the sectors
    read, by sector number, from QEMU's trace, or None if a step timed out."""
    copy = os.path.join(ROOT, "build", f"measure-listing-{name}.img")
    log = os.path.join(ROOT, "build", f"measure-listing-{name}.log")
    shutil.copyfile(os.path.join(ROOT, "build", image), copy)
    steps = [("login:", "root"), ("assword", "root")]
    steps += [("# ", command)] * RUNS + [("# ", "")]
    guest = drive_qemu.Guest(copy, extra_args=["-trace", "enable=virtio_blk_handle_read"],
                             intlog=log, label=f"measure-listing {name}: ",
                             stamp_from=os.path.join(ROOT, "build", image))
    try:
        driven = guest.run(steps)
    finally:
        guest.stop()
        os.remove(copy)
    if not driven:
        return None
    with open(log, "rb") as fh:
        hits = READ.findall(fh.read())
    count = collections.Counter()
    for sector, n in hits:
        for s in range(int(n)):
            count[int(sector) + s] += 1
    return count


def main() -> int:
    d = sys.argv[1] if len(sys.argv) > 1 else "/include"
    for name, image in IMAGES:
        base = reads(name, image, f"ls {d}/stdio.h")
        listed = reads(name, image, f"ls {d}")
        if base is None or listed is None:
            print(f"{name}: a boot timed out")
            return 1
        if not base:
            print(f"{name}: no virtio_blk_handle_read events in the trace: the observer sees nothing")
            return 1
        extra = sum(listed.values()) - sum(base.values())
        top = (listed - base).most_common(3)
        print(f"{name}: one `ls {d}` reads {extra / RUNS:.1f} sectors "
              f"(most read: {', '.join(f'{s} x{c / RUNS:g}' for s, c in top)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())

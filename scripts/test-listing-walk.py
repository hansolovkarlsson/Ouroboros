#!/usr/bin/env python3
"""A listing page's walk stops at the page's end, seen by QEMU's disk trace.

    python3 scripts/test-listing-walk.py    (or `make test-listing-walk`, which builds the images)

Since 2026-10-17 fsd stops walking a directory at the first entry that does
not fit an NP_READDIR page (`ReaddirFill::push` answers `Break`, and each
filesystem's `list_dir` stops on it). No listing check can see that: a
walker that ignored `Break` would give the same pages, since `ReaddirFill`
drops what is pushed after a full page. Only the disk can, so this rig counts
QEMU's `virtio_blk_handle_read` events (`-trace`, into the -d log): QEMU
counts the reads, not anything in the guest.

For each image (FAT32 build/esp.img, ext2 build/espext2.img, exFAT
build/espexfat.img), two boots of a copy, each logging in and typing one
command RUNS times:

- `echo /include/qqq*`: the shell lists /include page by page for the glob
  (the same `readdir_pages` loop as `ls`), matches nothing, and keeps the
  word as typed, so `echo` prints it;
- `echo /include/qqqq`: the same binary and an argument of the same length,
  and no listing.

Spawn stages a program 512 bytes a call, each resolving its path again, so
any other baseline (a different binary, or `ls` on a file, which walks
/include to that file) puts its own reads into the difference. These two
differ by the listings alone: the difference, per sector, over RUNS is one
listing of /include.

Both boots then type `echo /include/std*`, which must print exactly the
staged `std` names: a listing that failed would leave the glob literal too,
and read less, which would pass for a saving. And each `echo` must print its
word back.

The check: /include is two pages, so a listing asks fsd for four (page one,
the question past any end, page two, the empty page). Without the stop the
four do the same work, each resolving the path and walking the whole
directory, so every sector is read a multiple of four times a listing (an
ext2 inode table sector once for each entry it holds, in each walk), and
the counts share that factor. With it, page one's walk ends early and the
sectors past its end are read once less, so the counts must share no factor
above 1. A count that is not a whole number a listing (the two boots doing
other work apart from the listings) fails too. Fails on main before
2026-10-17 (the control: those images, every count a multiple of 4). The sectors
one listing reads are reported, not graded: 69 on FAT32, 888 on ext2 and
126 on exFAT, from 72, 924 and 132 on main before the stop (ext2's are
mostly an inode read for each entry walked, ROADMAP.md). The boots are deterministic: the same build
reads the same sectors each time. About five minutes.
"""
import collections
import importlib.util
import math
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
LISTED = "/include/qqq*"
UNLISTED = "/include/qqqq"
CHECK = "/include/std*"
READ = re.compile(rb"virtio_blk_handle_read .*?sector (\d+) nsectors (\d+)")


def std_names():
    """The staged names /include/std* must match."""
    d = os.path.join(ROOT, "build", "esp", "include")
    return sorted(f"/include/{n}" for n in os.listdir(d) if n.startswith("std"))


def boot(name, image, word):
    """Boot a copy of `image`, log in, `echo word` RUNS times and then the
    check glob. Returns (problems, sectors read by sector number)."""
    copy = os.path.join(ROOT, "build", f"test-listing-walk-{name}.img")
    log = os.path.join(ROOT, "build", f"test-listing-walk-{name}.log")
    shutil.copyfile(os.path.join(ROOT, "build", image), copy)
    steps = [("login:", "root"), ("assword", "root")]
    steps += [("# ", f"echo {word}")] * RUNS + [("# ", f"echo {CHECK}"), ("# ", "")]
    guest = drive_qemu.Guest(copy, extra_args=["-trace", "enable=virtio_blk_handle_read"],
                             intlog=log, label=f"test-listing-walk {name}: ",
                             stamp_from=os.path.join(ROOT, "build", image))
    try:
        driven = guest.run(steps)
        out = guest.transcript().replace("\r\n", "\n")
    finally:
        guest.stop()
        os.remove(copy)
    problems = []
    if not driven:
        problems.append("a step timed out")
    lines = out.split("\n")
    if lines.count(word) < RUNS:
        problems.append(f"`echo {word}` printed it back {lines.count(word)} times, not {RUNS}")
    want = std_names()
    if not any(sorted(line.split()) == want for line in lines):
        problems.append(f"`echo {CHECK}` did not print {' '.join(want)}: the listing failed")
    if guest.aborts():
        problems.append("fault lines in QEMU's trace")
    with open(log, "rb") as fh:
        hits = READ.findall(fh.read())
    count = collections.Counter()
    for sector, n in hits:
        for s in range(int(n)):
            count[int(sector) + s] += 1
    if not count:
        problems.append("no virtio_blk_handle_read events: the observer sees nothing")
    return problems, count


def main() -> int:
    failed = 0
    for name, image in IMAGES:
        p0, base = boot(name, image, UNLISTED)
        p1, listed = boot(name, image, LISTED)
        problems = p0 + p1
        diff = listed - base
        if sum(base.values()) > sum(listed.values()) or any(c % RUNS for c in diff.values()):
            problems.append("the two boots differ by more than the listings")
        per = [c // RUNS for c in diff.values()]
        common = math.gcd(*per) if per else 0
        total = sum(per)
        if not problems and common != 1:
            problems.append(f"every sector is read a multiple of {common} times a listing: "
                            "the walks are all alike, so no page's walk stopped at its end")
        verdict = "FAIL" if problems else "ok  "
        print(f"{verdict} {name}: one listing of /include reads {total} sectors, "
              f"over {len(per)} sectors, read a multiple of {common} times each")
        for p in problems:
            print(f"     {p}")
        failed += bool(problems)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

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

Each image gets a directory to list: /include on FAT32 (build/esp.img) and
exFAT (build/espexfat.img), two pages; on ext2 (build/espext2.img) /walk,
300 empty files the rig adds to its copy with e2fsprogs' debugfs, three
pages over four 1 KiB blocks. ext2 needs its own: since ext2's listing
stopped reading an inode for every entry (2026-10-17), /include is two
blocks there and page one ends in the second, so every walk reads both and
the stop has nothing to show. Two boots of each copy, each logging in and
typing one command RUNS times:

- `echo DIR/qqq*`: the shell lists DIR page by page for the glob (the same
  `readdir_pages` loop as `ls`), matches nothing, and keeps the word as
  typed, so `echo` prints it;
- `echo DIR/qqqq`: the same binary and an argument of the same length, and
  no listing.

Spawn stages a program 512 bytes a call, each resolving its path again, so
any other baseline (a different binary, or `ls` on a file, which walks DIR
to that file) puts its own reads into the difference. These two differ by
the listings alone: the difference, per sector, over RUNS is one listing.

Both boots then type a glob that must match (`/include/std*`, or
`/walk/f00*`) and must print exactly the names it should: a listing that
failed would leave the glob literal too, and read less, which would pass
for a saving. And each `echo` must print its word back.

The check: a listing of `p` pages asks fsd for `p + 2` (each page, the
question past any end, the empty page that ends it). Without the stop
those requests do the same work, each resolving the path and walking the
whole directory, so every sector is read a multiple of `p + 2` times a
listing and the counts share that factor. With it, the walks of the pages
before the last end early and the sectors past their ends are read fewer
times, so the counts must share no factor above 1. A count that is not a
whole number a listing (the two boots doing other work apart from the
listings) fails too. Controls: main before the stop fails every image
(factor 4 on /include), and a single walker ignoring `Break` fails its own
image alone.

On ext2 one listing must also read fewer sectors than /walk has entries:
until 2026-10-17 ext2's walk read every entry's inode, entries walked past
included, so a listing read at least one sector an entry. It takes the kind
from the entry's `file_type` byte now. Fails on main before that.

The sectors one listing reads are reported, not graded: 69 for /include on
FAT32 and 126 on exFAT (72 and 132 before the stop), and 74 for /walk on
ext2 (3,698 when its walk read an inode an entry). The boots are
deterministic: the same build reads the same sectors each time. About five
minutes; needs e2fsprogs (`brew install e2fsprogs`), as the ext2 image does.
"""
import collections
import importlib.util
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

RUNS = 2
SECTOR = 512
WALK_FILES = 300
READ = re.compile(rb"virtio_blk_handle_read .*?sector (\d+) nsectors (\d+)")


def std_names():
    """The staged names /include/std* must match."""
    d = os.path.join(ROOT, "build", "esp", "include")
    return sorted(f"/include/{n}" for n in os.listdir(d) if n.startswith("std"))


def walk_names():
    """The names /walk/f00* must match."""
    return [f"/walk/f{i:03d}" for i in range(10)]


def add_walk(copy):
    """Add /walk, WALK_FILES empty files, to the ext2 partition of `copy`:
    the partition (MBR type 0x83) taken out, changed by debugfs, written
    back."""
    with open(copy, "rb") as fh:
        mbr = fh.read(SECTOR)
    for i in range(4):
        entry = mbr[446 + 16 * i:462 + 16 * i]
        if entry[4] == 0x83:
            start, count = struct.unpack("<II", entry[8:16])
            break
    else:
        raise SystemExit(f"{copy}: no ext2 partition (MBR type 0x83)")
    prefix = subprocess.run(["brew", "--prefix", "e2fsprogs"], capture_output=True,
                            text=True).stdout.strip()
    debugfs = os.path.join(prefix, "sbin", "debugfs")
    with tempfile.TemporaryDirectory() as tmp:
        part, empty, cmds = (os.path.join(tmp, n) for n in ("part.img", "empty", "cmds"))
        with open(copy, "rb") as src, open(part, "wb") as dst:
            src.seek(start * SECTOR)
            dst.write(src.read(count * SECTOR))
        open(empty, "wb").close()
        with open(cmds, "w") as fh:
            fh.write("mkdir /walk\ncd /walk\n")
            fh.writelines(f"write {empty} f{i:03d}\n" for i in range(WALK_FILES))
        done = subprocess.run([debugfs, "-w", "-f", cmds, part], capture_output=True, text=True)
        if done.returncode != 0:
            raise SystemExit(f"debugfs failed: {done.stderr.strip()}")
        with open(part, "rb") as src, open(copy, "r+b") as dst:
            dst.seek(start * SECTOR)
            dst.write(src.read())


# (name, image, directory, glob that must match, its names, setup of the copy)
IMAGES = [
    ("fat32", "esp.img", "/include", "/include/std*", std_names, None),
    ("ext2", "espext2.img", "/walk", "/walk/f00*", walk_names, add_walk),
    ("exfat", "espexfat.img", "/include", "/include/std*", std_names, None),
]
# The images whose listing must read fewer sectors than its entries, and how
# many entries that is: ext2, which read an inode per entry walked.
UNDER_ONE_AN_ENTRY = {"ext2": WALK_FILES}


def boot(name, prepared, image, word, check, want):
    """Boot a copy of `prepared`, log in, `echo word` RUNS times and then the
    check glob. Returns (problems, sectors read by sector number)."""
    copy = os.path.join(ROOT, "build", f"test-listing-walk-{name}.img")
    log = os.path.join(ROOT, "build", f"test-listing-walk-{name}.log")
    shutil.copyfile(prepared, copy)
    steps = [("login:", "root"), ("assword", "root")]
    steps += [("# ", f"echo {word}")] * RUNS + [("# ", f"echo {check}"), ("# ", "")]
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
    if not any(sorted(line.split()) == want for line in lines):
        problems.append(f"`echo {check}` did not print {' '.join(want)}: the listing failed")
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
    for name, image, d, check, names, setup in IMAGES:
        prepared = os.path.join(ROOT, "build", f"test-listing-walk-{name}-prepared.img")
        shutil.copyfile(os.path.join(ROOT, "build", image), prepared)
        try:
            if setup:
                setup(prepared)
            want = names()
            p0, base = boot(name, prepared, image, f"{d}/qqqq", check, want)
            p1, listed = boot(name, prepared, image, f"{d}/qqq*", check, want)
        finally:
            os.remove(prepared)
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
        entries = UNDER_ONE_AN_ENTRY.get(name)
        if not p0 + p1 and entries is not None and total >= entries:
            problems.append(f"a listing of {entries} entries reads {total} sectors: "
                            "at least one an entry, as an inode read for each would")
        verdict = "FAIL" if problems else "ok  "
        print(f"{verdict} {name}: one listing of {d} reads {total} sectors, "
              f"over {len(per)} sectors, read a multiple of {common} times each")
        for p in problems:
            print(f"     {p}")
        failed += bool(problems)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

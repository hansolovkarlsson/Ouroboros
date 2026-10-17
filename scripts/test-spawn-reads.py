#!/usr/bin/env python3
"""Spawning a program resolves its path once a read, not once a 512 bytes.

    python3 scripts/test-spawn-reads.py    (or `make test-spawn-reads`, which builds the images)

The shell spawns a program by reading its file through fsd and staging it in
the kernel. Until 2026-10-17 it read 512 bytes a request (`NP_READ_AT`), and
fsd resolved the path from the root for each one, walking `/bin` again: on
FAT32 starting `ls` read about 1,040 sectors more than starting `echo`. Since
then the shell reads `SPAWN_READ` (8 KiB) a request through `NP_READ`, and
fsd resolves the path once a request and copies the file to the shell's grant
a `SAFECOPY_MAX` piece at a time (`vfs::read_pieces`), up to `NP_READ_MAX`.
No program's output can show that; only the disk can, so this rig counts
QEMU's virtio-blk reads (scripts/disktrace.py).

On each image (FAT32 build/esp.img, ext2 build/espext2.img, exFAT
build/espexfat.img), two boots of a copy, logging in and then typing
`ls /include/stdio.h` once, and three times: the per-sector difference over
the two extra runs is one run of `ls` (its spawn, its stat of one file, its
output). Each run must print `/include/stdio.h`.

The check: a resolution of `/bin/ls` reads `/bin`'s directory, the root
and (on FAT) the table between them. Resolving once a 512-byte chunk read
each of those sectors at least as many times as `ls` has chunks (65 for its
32,864 bytes): on main before 2026-10-17 a run read dozens of sectors 66 or
67 times, one of them on ext2 201 (an inode table block). No sector may be
read that often now. Other repeats are not resolution and stay under it: on
ext2 a block past the inode's twelve direct pointers is found through the
indirect block, read again for each (24 times for `ls`). The sectors one run
reads are reported: 199 on FAT32, 187 on ext2 and 290 on exFAT, from 1,201,
1,032 and 1,942 on main.

About three minutes.
"""
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disktrace  # noqa: E402

IMAGES = [("fat32", "esp.img"), ("ext2", "espext2.img"), ("exfat", "espexfat.img")]
COMMAND = "ls /include/stdio.h"
PRINTS = "/include/stdio.h"
BASE_RUNS, MORE_RUNS = 1, 3


def chunks():
    """`ls`'s 512-byte chunks, from the binary every image stages."""
    return math.ceil(os.path.getsize(os.path.join(ROOT, "build", "esp", "bin", "LS")) / 512)


def boot(name, image, runs):
    steps = [("login:", "root"), ("assword", "root")]
    steps += [("# ", COMMAND)] * runs + [("# ", "")]
    tag = f"test-spawn-reads-{name}-{runs}"
    problems, lines, count = disktrace.boot(tag, os.path.join(ROOT, "build", image), image, steps)
    if lines.count(PRINTS) < runs:
        problems.append(f"`{COMMAND}` printed {PRINTS} {lines.count(PRINTS)} times, not {runs}")
    return problems, count


def main() -> int:
    limit = chunks()
    failed = 0
    for name, image in IMAGES:
        p0, base = boot(name, image, BASE_RUNS)
        p1, more = boot(name, image, MORE_RUNS)
        differ, per = disktrace.per_run(base, more, MORE_RUNS - BASE_RUNS)
        problems = p0 + p1 + differ
        most = max(per.values(), default=0)
        total = sum(per.values())
        if not problems and most >= limit:
            problems.append(f"a sector is read {most} times a run, against {limit} chunks of "
                            "/bin/ls: its path is resolved once a chunk")
        verdict = "FAIL" if problems else "ok  "
        print(f"{verdict} {name}: one run of `{COMMAND}` reads {total} sectors, the most-read "
              f"{most} times ({limit} chunks of /bin/ls)")
        for p in problems:
            print(f"     {p}")
        failed += bool(problems)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

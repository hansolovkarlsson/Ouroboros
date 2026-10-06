#!/usr/bin/env python3
"""errno from the C file layer, on FAT32 and on ext2.

    python3 scripts/test-cerrno.py      (or `make test-cerrno`, which builds both images)

Step 2 of docs/roadmap/roadmap-c-hosting.md. `/bin/CERRNO` (libc/cerrno.c,
picolibc) provokes every failure a C program can reach in the file layer and
checks the errno POSIX names for it: a missing file, a path through a file, a
full fd table, a bad fd, an fd used against its mode, a bad lseek, a directory
read. Two boots, each of a COPY of the image:

- FAT32 (build/esp.img), as root.
- ext2 (build/espext2.img), as root, then as the ordinary `user`, which must
  get EACCES opening /etc/shadow (mode 0600): ext2 is the one image whose
  modes fsd enforces.

Graded on cerrno's own summary line (`cerrno: N checks, 0 failed`, matched
whole, with N at least the count below so a run cut short cannot pass), and
no fault line in QEMU's trace. About two minutes. Run it whenever
libc/src/file.c's error paths, or fsd's answers to them, change.
"""
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

# How many checks cerrno runs as root. A summary with fewer is a run that
# skipped some, which must not pass.
ROOT_CHECKS = 21
SUMMARY = r"cerrno: (\d+) checks, (\d+) failed\r?\n"


def boot(name, source, steps):
    image = os.path.join(ROOT, "build", f"test-cerrno-{name}.img")
    shutil.copyfile(os.path.join(ROOT, "build", source), image)
    guest = drive_qemu.Guest(image, label=f"test-cerrno {name}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(image)
    with open(os.path.join(ROOT, "build", f"test-cerrno-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def summaries(out):
    return [(int(n), int(f)) for n, f in re.findall(r"^cerrno: (\d+) checks, (\d+) failed\r?$", out, re.M)]


def main() -> int:
    root_steps = [("login:", "root"), ("assword", "root"), ("# ", "cerrno")]
    checks = []

    d, out, f = boot("fat32", "esp.img", root_steps + [(SUMMARY, "")])
    got = summaries(out)
    checks += [
        ("fat32: driven to the end", d),
        (f"fat32: root, {ROOT_CHECKS} checks, 0 failed", got == [(ROOT_CHECKS, 0)]),
        ("fat32: no fault lines", f == 0),
    ]
    fat32 = out

    d, out, f = boot("ext2", "espext2.img", root_steps + [
        (SUMMARY, "exit"),
        ("login:", "user"), ("assword", "user"), (r"\$ ", "cerrno /etc/shadow"),
        (SUMMARY, ""),
    ])
    got = summaries(out)
    checks += [
        ("ext2: driven to the end", d),
        (f"ext2: root, {ROOT_CHECKS} checks, 0 failed", got[:1] == [(ROOT_CHECKS, 0)]),
        ("ext2: user, /etc/shadow refused with EACCES", got[1:2] == [(1, 0)]),
        ("ext2: no fault lines", f == 0),
    ]
    for name, text in (("fat32", fat32), ("ext2", out)):
        for line in re.findall(r"^cerrno: (?:ok|FAIL).*$", text, re.M):
            print(f"     {name}: {line.rstrip()}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcripts: build/test-cerrno-fat32.txt, build/test-cerrno-ext2.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

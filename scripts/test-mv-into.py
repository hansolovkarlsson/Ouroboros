#!/usr/bin/env python3
"""A directory moved into itself by a case-only name, refused on every format.

    python3 scripts/test-mv-into.py     (or `make test-mv-into`, which builds the images)

Three boots, each of a COPY of the image: FAT32 (build/esp.img), exFAT
(build/espexfat.img) and ext2 (build/espext2.img). In each, as root:

- `mkdir /Docs`, `touch /Docs/keep.txt`, then `mv /Docs /docs`. On FAT32 and
  exFAT a stat of `/docs` finds `/Docs` itself, so `/bin/mv` sends the move
  as `/docs/Docs`: into the directory being moved. fsd's guard against a move
  into the mover's own subtree compared bytes until 2026-10-19, missed it, and
  the subtree was unlinked from the root and linked under itself, with success
  reported. Now `mv` must say `invalid name` and `ls /Docs` must still list
  `keep.txt`. On ext2, which matches names by bytes, `/docs` does not exist:
  the same `mv` is a plain rename and `ls /docs` must list `keep.txt`;
- `mkdir /Same`, `mv /Same /Same/sub`: the byte-equal case the guard always
  caught, refused with `invalid name` on all three;
- on ext2 only, `mkdir /A`, `mkdir /a`, `mv /A /a`: two directories, so the move
  goes into `/a` and `ls /a` must list `A`. A guard folding case on ext2 too
  would refuse it.

No fault line in QEMU's trace. Fails with the guard comparing bytes (the
FAT32 and exFAT `keep.txt` checks: the subtree gone) and with it folding case
on every format (ext2's `/A` into `/a`). About two minutes. Run it whenever
fsd's NP_MV guard, a filesystem's mv or its name matching, or `/bin/mv`'s
into-a-directory rewrite changes.
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

IMAGES = [("fat32", "esp.img", True), ("exfat", "espexfat.img", True), ("ext2", "espext2.img", False)]
# Each command's output is the text between its marker and the next one.
COMMON = [
    "mkdir /Docs",
    "touch /Docs/keep.txt",
    "mv /Docs /docs",
    "ls /Docs",
    "ls /docs",
    "mkdir /Same",
    "mv /Same /Same/sub",
]
EXT2_ONLY = ["mkdir /A", "mkdir /a", "mv /A /a", "ls /a"]


def boot(name, image, commands):
    copy = os.path.join(ROOT, "build", f"test-mv-into-{name}.img")
    shutil.copyfile(os.path.join(ROOT, "build", image), copy)
    steps = [("login:", "root"), ("assword", "root")]
    for i, cmd in enumerate(commands):
        steps += [("# ", f"echo ==m{i}=="), ("# ", cmd)]
    steps += [("# ", "echo ==end=="), ("==end==", "")]
    guest = drive_qemu.Guest(copy, label=f"test-mv-into {name}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(copy)
    with open(os.path.join(ROOT, "build", f"test-mv-into-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def outputs(out, commands):
    """{command: its output}, the text after the command's own line up to the
    next marker's echo."""
    got = {}
    for i, cmd in enumerate(commands):
        # The marker's OUTPUT line (`==m0==` alone), not its typed echo.
        m = re.search(rf"^==m{i}==\r?$", out, re.M)
        if not m:
            continue
        rest = out[m.end():]
        stop = re.search(rf"^.*echo ==(m{i + 1}|end)==", rest, re.M)
        block = rest[:stop.start()] if stop else rest
        got[cmd] = block.split(cmd, 1)[1] if cmd in block else ""
    return got


EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)


def text(block):
    """A command's own output, without the kernel's exit line."""
    return EXIT.sub("", block).strip()


def code(block):
    """The exit code the kernel reported for the command, or None."""
    m = EXIT.findall(block)
    return int(m[-1]) if m else None


def names(block):
    return {w.rstrip("/").lower() for w in text(block).split()}


def verdicts(name, folds, driven, out, faults, commands):
    o = outputs(out, commands)
    checks = [
        (f"{name}: driven to the end", driven),
        (f"{name}: every command's output found", len(o) == len(commands)),
        (f"{name}: mkdir /Docs and touch /Docs/keep.txt exit 0",
         code(o.get("mkdir /Docs", "")) == 0 and code(o.get("touch /Docs/keep.txt", "")) == 0),
        (f"{name}: mv /Same /Same/sub refused, invalid name, exit 1",
         text(o.get("mv /Same /Same/sub", "")) == "mv: invalid name" and code(o.get("mv /Same /Same/sub", "")) == 1),
        (f"{name}: no fault lines", faults == 0),
    ]
    if folds:
        checks += [
            (f"{name}: mv /Docs /docs refused, invalid name, exit 1",
             text(o.get("mv /Docs /docs", "")) == "mv: invalid name" and code(o.get("mv /Docs /docs", "")) == 1),
            (f"{name}: ls /Docs still lists keep.txt", "keep.txt" in names(o.get("ls /Docs", ""))),
        ]
    else:
        checks += [
            (f"{name}: mv /Docs /docs a plain rename, exit 0",
             text(o.get("mv /Docs /docs", "x")) == "" and code(o.get("mv /Docs /docs", "")) == 0),
            (f"{name}: ls /docs lists keep.txt", "keep.txt" in names(o.get("ls /docs", ""))),
            (f"{name}: mv /A /a exit 0 (into /a)", text(o.get("mv /A /a", "x")) == "" and code(o.get("mv /A /a", "")) == 0),
            (f"{name}: ls /a lists A", text(o.get("ls /a", "")).split() == ["A/"]),
        ]
    return checks


def main():
    failed = 0
    for name, image, folds in IMAGES:
        commands = COMMON + ([] if folds else EXT2_ONLY)
        driven, out, faults = boot(name, image, commands)
        for label, ok in verdicts(name, folds, driven, out, faults, commands):
            print(("PASS  " if ok else "FAIL  ") + label)
            failed += not ok
    print(f"test-mv-into: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

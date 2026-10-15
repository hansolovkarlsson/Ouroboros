#!/usr/bin/env python3
"""malloc's cost as the live set grows: `/bin/CMALLOC` on a booted image.

    python3 scripts/test-malloc.py      (or `make test-malloc`, which builds the image)

`cmalloc` (libc/cmalloc.c) times malloc, free and realloc at 500, 1000, 2000
and 4000 live blocks, with about as many free blocks scattered between them,
and prints the ratio of the largest set's cost per operation to the
smallest's. picolibc's nano-malloc keeps its free blocks on one
address-ordered list and walks it on every call, so its cost grows with the
set (DevTools's note, docs/handoffs/2026-10-14-from-devtools-nano-malloc.md);
newlib's binned malloc does not. One allocation in eight is a calloc of a
reused block, whose bytes must all read 0: newlib's calloc faulted on its
first call while picolibc was built without -fno-builtin-malloc
(libc/picolibc-cross.txt). The rig fails when the ratio reaches MAX_RATIO,
which the nano-malloc build does, and when a calloc faults or leaves a byte
set, which the binned build with the old cross file does (the two controls).

Each check must hold, and QEMU's own trace must hold no fault line. One boot,
about a minute. Run it whenever picolibc is rebuilt or its malloc options
change.
"""
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-malloc.txt")
# Eight times the live set: an allocator linear in its free blocks costs
# several times more per operation, one that bins them about the same.
MAX_RATIO = 2.0
SUMMARY = re.compile(r"^cmalloc: (\d+) live costs (\d+)\.(\d) times (\d+) live, (\d+) failed, (\d+) calloc bytes not 0", re.M)


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-malloc: ")
    try:
        steps = [("login:", "root"), ("assword", "root"), ("# ", "cmalloc"), ("not 0[\\s\\S]*# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    for line in re.findall(r"^cmalloc: .*$", out, re.M):
        print(f"     {line.rstrip()}")
    m = SUMMARY.search(out)
    ratio = float(f"{m.group(2)}.{m.group(3)}") if m else None
    checks = [
        ("driven to the end", driven),
        ("the summary line", m is not None),
        ("no allocation failed", m is not None and m.group(5) == "0"),
        ("every calloc'd byte 0", m is not None and m.group(6) == "0"),
        (f"the largest set costs under {MAX_RATIO} times the smallest", ratio is not None and ratio < MAX_RATIO),
        ("exited 0", re.search(r"task \d+ exited \(code 0\)", out) is not None),
        ("no fault lines", faults == 0),
    ]
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

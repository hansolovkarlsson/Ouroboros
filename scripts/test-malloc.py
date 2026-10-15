#!/usr/bin/env python3
"""picolibc's malloc on a booted image: `/bin/CMALLOC`, checked, then timed.

    python3 scripts/test-malloc.py      (or `make test-malloc`, which builds the image)

`cmalloc` (libc/cmalloc.c) first runs 20,000 random operations at each of
500, 1000, 2000 and 4000 slots, untimed: malloc, calloc, memalign, realloc
and free, every block written whole with its slot's byte. Every calloc'd
block must read 0 when it comes back, every memalign'd one must be aligned,
a realloc'd one must keep its bytes, and every block still live at the end
must hold its own byte throughout. The checks are for the two flags
libc/picolibc-cross.txt gives picolibc's malloc: without -fno-builtin-free
nano-malloc's realloc frees a remainder with no size and a later call loops
for good, so cmalloc never returns to the prompt (the libc.a shipped until
2026-10-15 did so at its 215th operation); without -fno-builtin-malloc
nano-malloc's memalign drops its size store, which the byte check sees.

Then it times malloc, calloc, realloc and free at the same counts, after an
untimed pass, the fastest of three, and prints the largest count's cost over
the smallest's. That ratio is reported, not graded: nano-malloc's walk of its
free list costs about 4.6 times at 4000 slots what it does at 500, which is
DevTools's note (docs/handoffs/2026-10-14-from-devtools-nano-malloc.md),
deferred to the per-program heap.

Checks: driven to the prompt after cmalloc (a hang fails here, after the
step timeout), its check line clean, the first exit line after the command
cmalloc's own with code 0, and no fault line in QEMU's trace. One boot,
about a minute. Run it whenever picolibc is rebuilt or its malloc options or
the cross file change.
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
CHECKED = re.compile(r"^cmalloc: checked \d+ ops at each of \d+ counts: (\d+) failed, (\d+) calloc bytes not 0, "
                     r"(\d+) memalign misaligned, (\d+) bytes not their block's", re.M)
# The first line that ends a task after the command: cmalloc's own end.
ENDED = re.compile(r"task \d+ (exited \(code (\d+)\)|killed[^\r\n]*)")


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-malloc: ")
    try:
        # The prompt after the command, not a word of the summary: a cmalloc
        # that dies is graded as soon as the shell is back.
        steps = [("login:", "root"), ("assword", "root"), ("# ", "cmalloc"), ("# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    for line in re.findall(r"^cmalloc: .*$", out, re.M):
        print(f"     {line.rstrip()}")
    # Only what follows the command: a boot task's own `exited (code 0)` is
    # not cmalloc's.
    typed = out.find("# cmalloc")
    after = out[typed:] if typed >= 0 else ""
    m = CHECKED.search(after)
    ended = ENDED.search(after)
    checks = [
        ("driven back to the prompt (a hang fails here)", driven),
        ("the check line", m is not None),
        ("no allocation failed", m is not None and m.group(1) == "0"),
        ("every calloc'd byte 0", m is not None and m.group(2) == "0"),
        ("every memalign'd block aligned", m is not None and m.group(3) == "0"),
        ("every live block holds its own bytes", m is not None and m.group(4) == "0"),
        ("cmalloc's own end is exit code 0", ended is not None and ended.group(2) == "0"),
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

#!/usr/bin/env python3
"""picolibc's malloc on a booted image: `/bin/CMALLOC`, checked, then timed.

    python3 scripts/test-malloc.py      (or `make test-malloc`, which builds the image)

`cmalloc` (libc/cmalloc.c) first runs 20,000 random operations at each of
500, 1000, 2000 and 4000 slots, untimed: malloc, calloc, memalign, realloc
and free, every block written whole with a pattern of its slot and offset
(never 0) and read back whole before it is freed, after a realloc over the
part kept, and at the end. Under nano-malloc what this catches is the hang:
without -fno-builtin-free (libc/picolibc-cross.txt) its realloc frees a
remainder with no size, without -fno-builtin-malloc its memalign drops its
size store, and a later call loops for good, so cmalloc never returns to the
prompt (the library shipped until 2026-10-15 hung so in a realloc). The
calloc and memalign counters cannot fail under nano (its malloc zeroes every
block, and its memalign aligns even on a broken heap); they hold another
allocator to the contract.

Then it times malloc, calloc, realloc and free at the same counts, after an
untimed pass, the fastest of three, and prints the largest count's cost over
the smallest's, reported, not graded: nano-malloc's walk of its free list
costs 4.6 to 4.8 times at 4000 slots what it does at 500, which is DevTools's
note (docs/handoffs/2026-10-14-from-devtools-nano-malloc.md), deferred to the
per-program heap. A refused allocation in the timed runs is counted and
graded.

Controls run 2026-10-15, each failing: the library shipped until then and one
built with only -fno-builtin-free (both hang); a calloc leaving one byte set;
a block handed out over another live block's memory (the pattern check); a
block written 48 bytes past its end (a hang, nano looping on the broken
headers); a timed malloc refusing one call in a hundred.

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
TIMED = re.compile(r"^cmalloc: timed: (\d+) failed", re.M)
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
    timed = TIMED.search(after)
    ended = ENDED.search(after)
    checks = [
        ("driven back to the prompt (a hang fails here)", driven),
        ("the check line", m is not None),
        ("no allocation failed", m is not None and m.group(1) == "0"),
        ("every calloc'd byte 0", m is not None and m.group(2) == "0"),
        ("every memalign'd block aligned", m is not None and m.group(3) == "0"),
        ("every block read back as written", m is not None and m.group(4) == "0"),
        ("no timed allocation failed", timed is not None and timed.group(1) == "0"),
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

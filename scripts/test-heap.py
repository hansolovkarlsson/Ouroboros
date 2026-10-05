#!/usr/bin/env python3
"""The user heap on a booted image: `/bin/CMEM` twice in one boot.

    python3 scripts/test-heap.py        (or `make test-heap`, which builds the image)

`cmem` (libc/cmem.c) checks that its heap is at least 1 MiB, that picolibc's
malloc holds nearly all of it live with every byte read back, and that every
byte of the heap read 0 before its first malloc. It runs twice because the
second run is the check on the loader's zeroing: a spawned program gets the
2 MB slot the last one to exit gave back, so the second `cmem` starts in the
first one's slot, its pattern still there unless the loader cleared it.

Each run must print its line ending `: ok` and exit with code 0, and QEMU's
own trace must hold no fault line. One boot, about a minute. Run it whenever
the loader's region layout (`HEAP_PAGES`, `populate_region`) or the runtime
region allocator changes.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-heap.txt")
RUNS = 2


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-heap: ")
    try:
        steps = [("login:", "root"), ("assword", "root")]
        steps += [("# ", "cmem")] * RUNS
        steps += [("# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    lines = re.findall(r"^cmem: .*$", out, re.M)
    checks = [
        ("driven to the end", driven),
        (f"{RUNS} cmem lines", len(lines) == RUNS),
        (f"each cmem line ok", len(lines) == RUNS and all(l.rstrip().endswith(": ok") for l in lines)),
        (f"each exited 0", len(re.findall(r"task \d+ exited \(code 0\)", out)) >= RUNS),
        ("no fault lines", faults == 0),
    ]
    for line in lines:
        print(f"     {line.rstrip()}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

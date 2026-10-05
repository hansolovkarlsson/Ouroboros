#!/usr/bin/env python3
"""argv in a C program, against the Rust program that reads the same store.

    python3 scripts/test-cargs.py        (or `make test-cargs`, which builds the image)

Step 1 of docs/roadmap/roadmap-c-hosting.md: crt0 builds `main(argc, argv)`
from GET_ARGC/GET_ARG. `/bin/CARGS` (libc/cargs.c, picolibc) prints the
vector in exactly `/bin/ARGS`'s format, so each pair of runs below must print
the same lines apart from argv[0], which is each program's name as typed.
Three pairs: a few arguments, none, and the fifteen the shell can pass at
most, so the vector is as long as a command line can make it.

Graded per pair: the Rust lines and the C lines equal once argv[0] is set
aside, the C argv[0] naming CARGS, its `argv[argc] is NULL` line, and its
exit code 0; and no fault line in QEMU's own trace. One boot, about a minute.
Run it whenever crt0.c or the kernel's argv store changes.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-cargs.txt")
CASES = [
    "alpha b 12345678901234567890",
    "",
    # The most the shell passes: it keeps 16 words (MAX_ARGS in
    # programs/shell/src/main.rs) and drops the rest without a word, so 15
    # arguments, each six letters, the longest vector a command line can stage.
    " ".join(chr(ord("a") + i) * 6 for i in range(15)),
]
# Marks each command's output off from the next one's in the transcript.
FENCE = "echo ==fence=="


def vector(block):
    """argc and the argv lines of one program's output, argv[0] aside."""
    argc = re.findall(r"^argc=(\d+)\r?$", block, re.M)
    argv = re.findall(r"^argv\[(\d+)\] = (.*?)\r?$", block, re.M)
    return argc, [(i, a) for i, a in argv if i != "0"], dict(argv).get("0")


def exit_code(block):
    """CARGS's exit code: the first exit line after its last output line. The
    block also holds the fence echo's exit line, which comes before."""
    m = re.search(r"^argv\[argc\] is.*?^Ouroboros kernel: task \d+ exited \(code (\d+)\)",
                  block, re.M | re.S)
    return m.group(1) if m else None


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-cargs: ")
    try:
        steps = [("login:", "root"), ("assword", "root")]
        for args in CASES:
            steps += [("# ", FENCE), ("# ", f"args {args}".rstrip()),
                      ("# ", FENCE), ("# ", f"cargs {args}".rstrip())]
        steps += [("# ", FENCE), ("# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    # The fence's own output lines split the transcript into the blocks
    # between them: the first block is the login, then Rust, C, Rust, C...
    blocks = re.split(r"^==fence==\r?$", out, flags=re.M)[1:]
    checks = [("driven to the end", driven),
              (f"{2 * len(CASES)} fenced blocks", len(blocks) >= 2 * len(CASES))]
    for n, args in enumerate(CASES):
        if len(blocks) < 2 * n + 2:
            break
        rust, c = blocks[2 * n], blocks[2 * n + 1]
        r_argc, r_argv, _ = vector(rust)
        c_argc, c_argv, c_zero = vector(c)
        want = str(1 + len(args.split()))
        label = f"case {n + 1} ({want} args)"
        checks += [
            (f"{label}: ARGS argc={want}", r_argc == [want]),
            (f"{label}: CARGS argc and argv[1..] equal ARGS's", (c_argc, c_argv) == (r_argc, r_argv)),
            (f"{label}: CARGS argv[0] names CARGS", c_zero is not None and c_zero.upper().endswith("CARGS")),
            (f"{label}: argv[argc] is NULL", "argv[argc] is NULL" in c),
            # CARGS's own exit line: the first one after its last output line.
            # The block also holds the fence echo's exit, which comes before.
            (f"{label}: CARGS exited 0", exit_code(c) == "0"),
        ]
    checks += [
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

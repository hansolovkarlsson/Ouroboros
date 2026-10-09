#!/usr/bin/env python3
"""argv in a C program, against the Rust program that reads the same store.

    python3 scripts/test-cargs.py        (or `make test-cargs`, which builds the image)

Step 1 of docs/roadmap/roadmap-c-hosting.md: crt0 builds `main(argc, argv)`
from GET_ARGC/GET_ARG. libc/cargs.c prints the vector in exactly `/bin/ARGS`'s
format, built twice because crt0 is compiled into both C libraries:
`/bin/CARGS` through picolibc and `/bin/CARGSH` through the hand-rolled libc.
Each case runs all three with the same arguments, and they must print the
same lines apart from argv[0], each program's name as typed. Three cases: a
few arguments, none, and fifteen, the most the shell passes (16 words with
the command's name). The shell's 128-byte line keeps every blob far below ARGV_MAX (512),
so crt0's byte bound is not reached here; only a program staging its own blob
could reach it.

Graded per C program and case: argc and argv[1..] equal ARGS's, argv[0] the
name typed, the `argv[argc] is NULL` line, and its exit code 0; and no fault
line in QEMU's own trace. And `cremote` and `cbig`, which take a remote path
as their argument, must refuse a local one with exit code 2. One boot, about
a minute.

And one word past the most is refused, not cut: 17 words (16 arguments) as a
command, as a pipeline's first stage and after `exec` must each print the
shell's `too many words` line, and ARGS must not run (no `argc=` line in the
block). Before 2026-10-09 the shell dropped the seventeenth
word and ran the rest.

Run it whenever crt0.c, the kernel's argv store or the shell's argv split
changes.
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
    # The most arguments the shell passes: 16 words (MAX_ARGS in
    # programs/shell/src/main.rs); one more is refused (TOO_MANY).
    " ".join(chr(ord("a") + i) * 6 for i in range(15)),
]
# The C programs, each compared with the Rust ARGS.
C_PROGRAMS = ["cargs", "cargsh"]
# The programs that take a remote path from argv and must refuse a local one:
# given one, their remote checks would test the local route and pass.
REFUSALS = [("cremote", "/EFI/ORBS/INIT.CFG"), ("cbig", "/man/grep")]
# One word past MAX_ARGS, in each place the shell builds an argv: the line
# typed, and the command the refusal must name.
SIXTEEN = " ".join(chr(ord("a") + i) for i in range(16))
TOO_MANY = [(f"args {SIXTEEN}", "args"), (f"args {SIXTEEN} | upper", "args"),
            (f"exec args {SIXTEEN}", "exec")]
# Marks each command's output off from the next one's in the transcript.
FENCE = "echo ==fence=="


def vector(block):
    """argc and the argv lines of one program's output, argv[0] aside."""
    argc = re.findall(r"^argc=(\d+)\r?$", block, re.M)
    argv = re.findall(r"^argv\[(\d+)\] = (.*?)\r?$", block, re.M)
    return argc, [(i, a) for i, a in argv if i != "0"], dict(argv).get("0")


def exit_code(block):
    """A C program's exit code: the first exit line after its last output line.
    The block also holds the fence echo's exit line, which comes before."""
    m = re.search(r"^argv\[argc\] is.*?^Ouroboros kernel: task \d+ exited \(code (\d+)\)",
                  block, re.M | re.S)
    return m.group(1) if m else None


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-cargs: ")
    try:
        steps = [("login:", "root"), ("assword", "root")]
        for args in CASES:
            for prog in ["args"] + C_PROGRAMS:
                steps += [("# ", FENCE), ("# ", f"{prog} {args}".rstrip())]
        for prog, path in REFUSALS:
            steps += [("# ", FENCE), ("# ", f"{prog} {path}")]
        for line, _ in TOO_MANY:
            steps += [("# ", FENCE), ("# ", line)]
        steps += [("# ", FENCE), ("# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    # The fence's own output lines split the transcript into the blocks
    # between them: the login, then per case ARGS and each C program in turn.
    per_case = 1 + len(C_PROGRAMS)
    blocks = re.split(r"^==fence==\r?$", out, flags=re.M)[1:]
    checks = [("driven to the end", driven),
              (f"{per_case * len(CASES)} fenced blocks", len(blocks) >= per_case * len(CASES))]
    for n, args in enumerate(CASES):
        if len(blocks) < per_case * (n + 1):
            break
        rust = blocks[per_case * n]
        r_argc, r_argv, _ = vector(rust)
        want = str(1 + len(args.split()))
        label = f"case {n + 1} ({want} args)"
        checks.append((f"{label}: ARGS argc={want}", r_argc == [want]))
        for k, prog in enumerate(C_PROGRAMS):
            c = blocks[per_case * n + 1 + k]
            c_argc, c_argv, c_zero = vector(c)
            checks += [
                (f"{label}: {prog} argc and argv[1..] equal ARGS's", (c_argc, c_argv) == (r_argc, r_argv)),
                (f"{label}: {prog} argv[0] = {prog}", c_zero == prog),
                (f"{label}: {prog} argv[argc] is NULL", "argv[argc] is NULL" in c),
                (f"{label}: {prog} exited 0", exit_code(c) == "0"),
            ]
    for k, (prog, path) in enumerate(REFUSALS):
        i = per_case * len(CASES) + k
        block = blocks[i] if i < len(blocks) else ""
        m = re.search(rf"^{prog}: {re.escape(path)} is not on a remote mount.*?"
                      r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)", block, re.M | re.S)
        checks.append((f"{prog} {path}: refused, exit 2", m is not None and m.group(1) == "2"))
    for k, (line, who) in enumerate(TOO_MANY):
        i = per_case * len(CASES) + len(REFUSALS) + k
        block = blocks[i] if i < len(blocks) else ""
        refused = re.search(rf"^{who}: too many words \(at most 16\b", block, re.M) is not None
        # The block also holds the fence echo's exit line, so a task exit
        # proves nothing; ARGS printing its argc is what running looks like.
        ran = "argc=" in block
        checks.append((f"`{line}`: refused, nothing run", refused and not ran))
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

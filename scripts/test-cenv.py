#!/usr/bin/env python3
"""The environment in a C program, against the Rust program that reads the same store.

    python3 scripts/test-cenv.py        (or `make test-cenv`, which builds the image)

Step 3 of docs/roadmap/roadmap-c-hosting.md: crt0 builds `environ` from
GET_ENVC/GET_ENV. libc/cenv.c prints it in exactly `/bin/PRINTENV`'s format,
built twice because crt0 is compiled into both C libraries: `/bin/CENV`
through picolibc, then getenv's answer for each argument, and `/bin/CENVH`
through the hand-rolled libc, which has no getenv.

Two rounds in one boot, each running printenv, cenvh and cenv: before any
`set` (the shell's own environment), and after four, the last a value of 128
bytes, the shell's longest (ENV_VALUE_SIZE), made by expanding a 64-byte one
twice since the input line is itself 128 bytes. Graded per round: both C
programs' NAME=VALUE lines equal printenv's, in order, and each exited 0; and
after the sets, printenv and cenv's getenv both answer FOO, SOURCE_DATE_EPOCH
(Proem's reason for this step) and PATH with the values this script set or the
shell's default, and a name never set as unset. The expected values are this
file's, never printenv's: taken from printenv, a variable it lost would turn
the check into "unset" and pass a C side lost the same way. No fault line in
QEMU's trace. About a minute. Run it whenever crt0.c or the kernel's env store
changes.

crt0 asks GET_ENV for up to 2047 bytes on its first entry, more than the
kernel's blanket 512-byte cap, so this rig also fails if GET_ENV's capacity is
refused above that cap (the control in #222).

A third block runs /bin/RDPROBE after `cd /EFI` and `bind /mnt /EFI`: it reads
all five per-task stores (GET_ARG, GET_ENV, GET_CWD, GET_NS, TASK_NAME)
through a 4 KiB buffer, and GET_ENV once with a capacity of u64::MAX, and
each answer is checked against what this script set up. Every other reader
passes 512 bytes or less, so this block is the only check that the kernel
accepts a buffer larger than a store.

Not reached: crt0's skip of an entry longer than the room left in its buffer.
The strings of any blob the kernel holds fit in ENV_MAX bytes, so only a
kernel answering a length past what it stores could take that branch.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-cenv.txt")
FENCE = "echo ==fence=="
HALF = "".join(chr(ord("a") + i % 26) for i in range(64))
DEFAULT_PATH = "/bin"  # the shell's own (programs/shell/src/main.rs)
# (name, as typed, as stored). LONGVAL is HALF expanded twice: 128 bytes.
SETS = [
    ("FOO", "bar", "bar"),
    ("SOURCE_DATE_EPOCH", "1700000000", "1700000000"),
    ("HALF", HALF, HALF),
    ("LONGVAL", "$HALF$HALF", HALF + HALF),
]
EXPECT = {name: stored for name, _, stored in SETS} | {"PATH": DEFAULT_PATH}
ASK = ["FOO", "SOURCE_DATE_EPOCH", "PATH", "LONGVAL", "NOSUCHVAR"]
PROGRAMS = ["printenv", "cenvh", f"cenv {' '.join(ASK)}"]
PROBE_DIR, PROBE_BIND, PROBE_ARG = "/EFI", "/mnt", "hello"


def env_lines(block):
    return re.findall(r"^([A-Za-z_][A-Za-z0-9_]*=.*?)\r?$", block, re.M)


def exit_code(block):
    """The program's exit code: the first exit line after the command's echo.
    The block also holds the fence echo's exit line, which comes before it."""
    m = re.search(r"^# (?!echo).*?^Ouroboros kernel: task \d+ exited \(code (\d+)\)", block, re.M | re.S)
    return m.group(1) if m else None


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-cenv: ")
    try:
        steps = [("login:", "root"), ("assword", "root")]
        for prog in PROGRAMS:
            steps += [("# ", FENCE), ("# ", prog)]
        for name, typed, _ in SETS:
            steps += [("# ", f"set {name}={typed}")]
        for prog in PROGRAMS:
            steps += [("# ", FENCE), ("# ", prog)]
        steps += [("# ", FENCE), ("# ", f"cd {PROBE_DIR}"), ("# ", f"bind {PROBE_BIND} {PROBE_DIR}"),
                  ("# ", f"rdprobe {PROBE_ARG}")]
        steps += [("# ", FENCE), ("# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    blocks = re.split(r"^==fence==\r?$", out, flags=re.M)[1:]
    n = len(PROGRAMS)
    checks = [("driven to the end", driven), (f"{2 * n + 1} fenced blocks", len(blocks) >= 2 * n + 1)]
    rounds = [("before set", blocks[0:n]), ("after set", blocks[n:2 * n])]
    for label, (rust, h, c) in [(lbl, b) for lbl, b in rounds if len(b) == n]:
        want = env_lines(rust)
        checks += [
            (f"{label}: printenv printed an environment", len(want) > 0),
            (f"{label}: cenvh's lines equal printenv's", env_lines(h) == want),
            (f"{label}: cenv's lines equal printenv's", env_lines(c) == want),
            (f"{label}: cenvh exited 0", exit_code(h) == "0"),
            (f"{label}: cenv exited 0", exit_code(c) == "0"),
        ]
    if len(blocks) >= 2 * n:
        rust, c = blocks[n], blocks[2 * n - 1]
        env = dict(line.split("=", 1) for line in env_lines(rust))
        for name, value in EXPECT.items():
            checks.append((f"after set: printenv shows {name} ({len(value)} bytes)",
                           env.get(name) == value))
        for name in ASK:
            if name in EXPECT:
                ok = f"getenv {name} = [{EXPECT[name]}]" in c
                checks.append((f"getenv {name} answers its value", ok))
            else:
                checks.append((f"getenv {name} answers unset", f"getenv {name} unset" in c))
    if len(blocks) >= 2 * n + 1:
        probe = blocks[2 * n]
        got = re.findall(r"^(arg1|env|envmax|cwd|ns|name)(=.*?| refused)\r?$", probe, re.M)
        lines = [k + v for k, v in got]
        envs = [v[1:] for k, v in got if k == "env" and v.startswith("=")]
        def one(key):
            vals = [v for k, v in got if k == key]
            return vals[0] if len(vals) == 1 else None
        checks += [
            ("rdprobe: GET_ARG through 4 KiB answers argument 1", one("arg1") == f"={PROBE_ARG}"),
            ("rdprobe: GET_ENV through 4 KiB answers every variable set",
             all(f"{k}={v}" in envs for k, v in EXPECT.items())),
            ("rdprobe: GET_ENV with capacity u64::MAX answers entry 0",
             bool(envs) and one("envmax") == f"={envs[0]}"),
            ("rdprobe: GET_CWD through 4 KiB answers the directory", one("cwd") == f"={PROBE_DIR}"),
            ("rdprobe: GET_NS through 4 KiB answers the binding",
             (one("ns") or "").startswith("=") and PROBE_BIND in one("ns") and PROBE_DIR in one("ns")),
            ("rdprobe: TASK_NAME through 4 KiB answers its own name", one("name") == "=rdprobe"),
            ("rdprobe: nothing refused", lines != [] and not any(l.endswith(" refused") for l in lines)),
            ("rdprobe exited 0", exit_code(probe) == "0"),
        ]
    checks.append(("no fault lines", faults == 0))
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

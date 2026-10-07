#!/usr/bin/env python3
"""A task's FP/SIMD registers across the kernel: /bin/FPPROBE on a booted image.

    python3 scripts/test-fpsimd.py      (or `make test-fpsimd`, which builds the image)

`fpprobe` (programs/shellutils/fpprobe) loads all 32 vector registers, a
non-default FPCR and a pattern of FPSR flags, crosses the kernel and reads
them back, twice: across a `YIELD` syscall, and across a spin the timer tick
preempts (it must span at least two ticks). The kernel saves `q0`-`q31`, FPCR and FPSR on every
resumable path since 2026-10-07; before that it saved only `x0`-`x30`, and
its own memcpy runs through `q0`.

Two runs in one boot: `fpprobe` alone, then `fpprobe | fpprobe -`, two
probes holding different FPCR and FPSR values, so the preemption case
switches between two tasks that both hold live FP state (`-` makes the second
copy the first's lines through, so all six reach the console). Alone, `YIELD`
finds nothing else runnable and does not switch: that run checks the
trampolines and the kernel's own use of `q0`, the pipeline checks the switch. Every run
must exit 0. Each printed line must be `[ok]`, and QEMU's own trace must hold
no fault line. One boot, about a minute. Run it whenever exceptions.rs's
trampolines, `Context`, or tasks.rs's switch paths change.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-fpsimd.txt")
# Two lines per probe: one alone, two in the pipeline.
LINES = 6
EXITS = 3


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-fpsimd: ")
    try:
        steps = [("login:", "root"), ("assword", "root"),
                 ("# ", "fpprobe"), ("# ", "fpprobe | fpprobe -"), ("# ", "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    lines = re.findall(r"^\[(?:ok|FAIL)\] +(?:syscall|preemption):.*$", out, re.M)
    # Only the exits after the first probe command: anything that exits
    # during boot or login is not a probe run.
    first = out.find("# fpprobe")
    exits = re.findall(r"task \d+ exited \(code (\d+)\)", out[first:]) if first >= 0 else []
    checks = [
        ("driven to the end", driven),
        (f"{LINES} fpprobe lines", len(lines) == LINES),
        ("each line [ok]", len(lines) == LINES and all(l.startswith("[ok]") for l in lines)),
        (f"{EXITS} runs, each exited 0", len(exits) == EXITS and all(c == "0" for c in exits)),
        ("no fault lines", faults == 0),
    ]
    for line in lines:
        print(f"     {line.rstrip()}")
    print(f"     exit codes: {', '.join(exits) or 'none'}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

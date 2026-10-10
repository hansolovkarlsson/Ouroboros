#!/usr/bin/env python3
"""The cost of a syscall, measured (multi-core step 3, decision 1).

    python3 scripts/measure-syscost.py    (or `make measure-syscost`, which builds the image)

Boots the image, logs in, runs /bin/SYSCOST three times and prints each
run's line and the median nanoseconds a call. Not a pass/fail rig: a
measurement, to be run on the tree and on a build with the FP/SIMD save
removed (empty the SAVE_FPSIMD and RESTORE_FPSIMD macros in
kernel/src/exceptions.rs), whose two figures the decision in exceptions.rs
cites. QEMU's figure is TCG's; the ratio is what carries to hardware.
"""
import importlib.util
import os
import re
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
PROMPT = "# "
LINE = re.compile(r"syscost: (\d+) GET_TICKS in (\d+) us, (\d+) ns each")


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="measure-syscost: ")
    try:
        ok = guest.wait_for("login:", timeout=150)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        for _ in range(3):
            if not ok:
                break
            guest.type_line("syscost")
            ok = guest.wait_for(r"ns each[\s\S]*" + PROMPT, timeout=120)
        out = guest.transcript()
    finally:
        guest.stop()
    runs = LINE.findall(out)
    for rounds, us, ns in runs:
        print(f"     {rounds} calls in {us} us: {ns} ns a call")
    if not ok or len(runs) < 3:
        print("FAIL the three runs did not complete")
        return 1
    print(f"median: {int(statistics.median(int(ns) for _, _, ns in runs))} ns a syscall")
    return 0


if __name__ == "__main__":
    sys.exit(main())

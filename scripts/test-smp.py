#!/usr/bin/env python3
"""Multi-core, on QEMU (docs/roadmap/roadmap-smp.md).

    python3 scripts/test-smp.py    (or `make test-smp`, which builds the image)

Step 1, the cores found: two boots, `-smp 4` and `-smp 1`. Before the exit
the boot log says `MADT: N cores (this core affinity A, mpidr M)` and then
one `MADT: core i: mpidr M, enabled` line per core. N must be the `-smp`
count, there must be N core lines with distinct MPIDRs, the boot core's
affinity must be among them (the raw `MPIDR_EL1` has bit 31 set, the table
records affinity fields only), every core must be enabled (QEMU's firmware offers them
all), and the shell must then come up and answer `help`, since the other
cores are not started yet and nothing else may change. QEMU's own trace
must hold no fault line. Step 2 extends this rig with the `core N up` lines.

A boot with no `Ouroboros kernel` line is the firmware stalling in its own
boot, which happens; the rig says INCONCLUSIVE for that boot and still does
not pass. About two minutes. Run it whenever madt.rs's GICC parse changes.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-smp-{smp}.txt")
PROMPT = "# "
COUNT_RE = re.compile(r"MADT: (\d+) cores(, more than fit listed)? \(this core affinity (0x[0-9a-f]+), mpidr 0x[0-9a-f]+\)")
CORE_RE = re.compile(r"MADT: core (\d+): mpidr (0x[0-9a-f]+), (enabled|disabled)")


def boot(smp):
    guest = drive_qemu.Guest(IMAGE, label=f"test-smp -smp {smp}: ", extra_args=["-smp", str(smp)])
    try:
        ok = guest.wait_for("login:", timeout=150)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            guest.type_line("help")
            ok = guest.wait_for(r"help[\s\S]*" + PROMPT)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(smp=smp), "w") as fh:
        fh.write(out)
    if "Ouroboros kernel" not in out:
        # The firmware stalled in its own boot and never loaded the kernel
        # (seen once here, and a known shape in test-usb-hub.py): not a
        # result, and not a pass either.
        print(f"INCONCLUSIVE -smp {smp}: the firmware never started the kernel; boot again")
    count = COUNT_RE.search(out)
    cores = CORE_RE.findall(out)
    mpidrs = [m for _, m, _ in cores]
    checks = [
        (f"-smp {smp}: driven to the shell, `help` answered", ok),
        (f"-smp {smp}: the log says {smp} cores" + (f" (it said {count.group(1)})" if count and count.group(1) != str(smp) else ""),
         count is not None and count.group(1) == str(smp) and not count.group(2)),
        (f"-smp {smp}: one core line per core, {smp} distinct MPIDRs" + (f" (saw {len(cores)}: {' '.join(mpidrs)})" if len(set(mpidrs)) != smp else ""),
         len(cores) == smp and len(set(mpidrs)) == smp
         and [int(i) for i, _, _ in cores] == list(range(smp))),
        (f"-smp {smp}: the boot core's affinity is among them", count is not None and count.group(3) in mpidrs),
        (f"-smp {smp}: every core enabled", len(cores) == smp and all(e == "enabled" for _, _, e in cores)),
        (f"-smp {smp}: no fault lines", faults == 0),
    ]
    return checks, faults


def main() -> int:
    checks = []
    for smp in (4, 1):
        c, _ = boot(smp)
        checks += c
    failed = 0
    for name, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        failed += 0 if good else 1
    print(f"transcripts: {os.path.relpath(TRANSCRIPT.format(smp='{4,1}'), ROOT)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Multi-core, on QEMU (docs/roadmap/roadmap-smp.md).

    python3 scripts/test-smp.py    (or `make test-smp`, which builds the image)

Four boots. In each, before the exit, the boot log says `MADT: N cores (this
core affinity A, mpidr M)` and one `MADT: core i: mpidr M, enabled` line per
core (step 1): N must be the `-smp` count, with N distinct MPIDRs, the boot
core's affinity among them, every core enabled. Then (step 2) the boot core
starts every other core through PSCI `CPU_ON`, and each must say
`smp: core i up at EL1 (...), parked`, the summary must say all started
cores are up, the shell must come up and answer `help`, and after a dwell
of DWELL seconds with the other cores parked it must still answer `echo`
(nothing on a parked core may disturb the boot core), with no fault line in
QEMU's own trace.

1. `-smp 4`, the dev-loop machine (an EL1 handoff; the secondaries enter at
   EL1 with the MMU off and turn it on in place).
2. `-smp 4`, `virt,virtualization=on` (an EL2 handoff, as the Raspberry
   Pi's; each secondary enters at EL2 and drops itself, the log must say
   the boot core `dropped from EL2 to EL1` and each core `up at EL1`).
3. `-smp 1`: one core, no `core up` line, the summary `0 of 0`, the shell
   as before.
4. `-smp 4` with the `\\SMPFAULT` flag file (an ESP directory copy, vvfat):
   core 1 takes an undefined instruction after its up line, the fault line
   must name it (`EXCEPTION core=1`), and the boot core's shell must still
   answer: a secondary's fault halts that core alone.

A boot with no `Ouroboros kernel` line is the firmware stalling in its own
boot, which happens; the rig says INCONCLUSIVE for that boot and does not
pass. About five minutes. Run it whenever smp.rs, el2.rs, madt.rs's GICC
parse, the GIC backends' per-core init or the console lock changes.
"""
import importlib.util
import os
import re
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
ESP = os.path.join(ROOT, "build", "esp")
TRANSCRIPT = os.path.join(ROOT, "build", "test-smp-{name}.txt")
PROMPT = "# "
DWELL = 10
COUNT_RE = re.compile(r"MADT: (\d+) cores(, more than fit listed)? \(this core affinity (0x[0-9a-f]+), mpidr 0x[0-9a-f]+\)")
CORE_RE = re.compile(r"MADT: core (\d+): mpidr (0x[0-9a-f]+), (enabled|disabled)")
UP_RE = re.compile(r"smp: core (\d+) up at EL(\d) \(mpidr 0x[0-9a-f]+, affinity 0x[0-9a-f]+\), parked")
SUMMARY_RE = re.compile(r"smp: (\d+) of (\d+) started cores up, (\d+) cores in all")


def boot(name, smp, machine="virt", esp=None, fault=False):
    """One boot: `esp` is an ESP directory to boot by vvfat instead of the
    image (for a boot flag), `fault` says core 1 is expected to fault."""
    if esp:
        guest = drive_qemu.Guest(
            os.path.join(esp, "boot"), virtio_disk=False, label=f"test-smp {name}: ",
            extra_args=["-smp", str(smp), "-drive", f"file=fat:rw:{esp},format=raw,media=disk,if=none,id=hd0",
                        "-device", "virtio-blk-device,drive=hd0"],
            machine=machine, stamp_from=esp)
    else:
        guest = drive_qemu.Guest(IMAGE, label=f"test-smp {name}: ", extra_args=["-smp", str(smp)], machine=machine)
    dwell_ok = False
    try:
        if esp:
            # A vvfat boot has no /etc, so the boot shell starts a root
            # session with no login.
            ok = guest.wait_for(r"shell ready[\s\S]*" + PROMPT, timeout=150)
        else:
            ok = guest.wait_for("login:", timeout=150)
            ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            guest.type_line("help")
            ok = guest.wait_for(r"help[\s\S]*" + PROMPT)
        if ok:
            # `help` again rather than a program: a vvfat boot has no /bin.
            time.sleep(DWELL)
            guest.type_line("help")
            dwell_ok = guest.wait_for(r"builtins:[\s\S]*" + PROMPT, timeout=20)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(name=name), "w") as fh:
        fh.write(out)
    if "Ouroboros kernel" not in out:
        return [(f"INCONCLUSIVE {name}: the firmware never started the kernel; boot again", False)]
    count = COUNT_RE.search(out)
    cores = CORE_RE.findall(out)
    mpidrs = [m for _, m, _ in cores]
    ups = UP_RE.findall(out)
    up_cores = sorted(int(i) for i, _ in ups)
    summary = SUMMARY_RE.search(out)
    others = smp - 1
    checks = [
        (f"{name}: driven to the shell, `help` answered", ok),
        (f"{name}: the log says {smp} cores" + (f" (it said {count.group(1)})" if count and count.group(1) != str(smp) else ""),
         count is not None and count.group(1) == str(smp) and not count.group(2)),
        (f"{name}: one MADT line per core, {smp} distinct MPIDRs" + (f" (saw {len(cores)}: {' '.join(mpidrs)})" if len(set(mpidrs)) != smp else ""),
         len(cores) == smp and len(set(mpidrs)) == smp and [int(i) for i, _, _ in cores] == list(range(smp))),
        (f"{name}: the boot core's affinity is among them", count is not None and count.group(3) in mpidrs),
        (f"{name}: every core enabled", len(cores) == smp and all(e == "enabled" for _, _, e in cores)),
        (f"{name}: every other core says up at EL1 and parked ({others} of them)" + (f" (saw cores {up_cores})" if up_cores != list(range(1, smp)) else ""),
         up_cores == list(range(1, smp)) and all(el == "1" for _, el in ups)),
        (f"{name}: the summary says {others} of {others} started cores up, {smp} in all",
         summary is not None and summary.groups() == (str(others), str(others), str(smp))),
        (f"{name}: the shell still answers after a {DWELL} s dwell with the other cores parked", dwell_ok),
    ]
    if machine != "virt":
        checks.append((f"{name}: the boot core dropped from EL2 to EL1", "dropped from EL2 to EL1" in out))
    if fault:
        checks += [
            (f"{name}: core 1 took the \\SMPFAULT instruction", "smp: core 1 taking the \\SMPFAULT undefined instruction" in out),
            (f"{name}: the fault line names core 1 (EXCEPTION core=1, an undefined instruction, esr 0x2000000)",
             re.search(r"EXCEPTION core=1 vector=\d+ esr_el1=0x2000000 ", out) is not None),
            (f"{name}: no other core's fault line", re.search(r"EXCEPTION core=(?!1 )", out) is None),
        ]
    else:
        checks.append((f"{name}: no fault lines", faults == 0 and "EXCEPTION" not in out))
    return checks


def main() -> int:
    checks = []
    checks += boot("smp4", 4)
    checks += boot("smp4-el2", 4, machine="virt,virtualization=on")
    checks += boot("smp1", 1)
    esp = os.path.join(ROOT, "build", "test-smp-esp")
    shutil.rmtree(esp, ignore_errors=True)
    shutil.copytree(ESP, esp)
    open(os.path.join(esp, "SMPFAULT"), "w").close()
    checks += boot("smp4-fault", 4, esp=esp, fault=True)
    failed = 0
    for name, good in checks:
        label = "ok  " if good else ("    " if name.startswith("INCONCLUSIVE") else "FAIL")
        print(f"{label} {name}")
        failed += 0 if good else 1
    print(f"transcripts: {os.path.relpath(TRANSCRIPT.format(name='*'), ROOT)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

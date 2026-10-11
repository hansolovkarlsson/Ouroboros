#!/usr/bin/env python3
"""Multi-core, on QEMU (docs/roadmap/roadmap-smp.md).

    python3 scripts/test-smp.py    (or `make test-smp`, which builds the image)

Six boots. In each, before the exit, the boot log says `MADT: N cores (this
core affinity A, mpidr M)` and one `MADT: core i: mpidr M, enabled` line per
core (step 1): N must be the `-smp` count, with N distinct MPIDRs, the boot
core's affinity among them, every core enabled. Then (step 2) the boot core
starts every other core through PSCI `CPU_ON`, and each must say
`smp: core i up at EL1 (...), idling` and then (step 4(b)) that it is
ticking, the summary must say all started cores are up, each must answer
the boot core's kick SGI only once the kernel lock is free (step 4(c): the
boot core holds the lock 50 ms after the send, and a core whose handler
ran under it would say so), the shell must come up and answer `help`, and after a dwell
of DWELL seconds with the other cores idling it must still answer
(nothing on another core may disturb the boot core), with no fault line in
QEMU's own trace. Then (step 4(d), the image boots) /bin/COREPROBE: alone
it must run on a secondary (on the one core under `-smp 1`) and see one
program running; `coreprobe | coreprobe -` must run both on secondaries and
each see two programs running at once (one at a time under `-smp 1`); and
`coreprobe calls` must reach fsd (on the boot core) from a secondary in
under CALL_BOUND_US a round trip, which it does only with the idle-core
kick; and `coreprobe hold`, started on a secondary, must end at Ctrl+C on that core,
as the boot core's tick asked (`task N ended on core C`), the shell
answering after. Under `-smp 1` the kill is local and no such line may
appear.

1. `-smp 4`, the dev-loop machine (an EL1 handoff; the secondaries enter at
   EL1 with the MMU off and turn it on in place).
2. `-smp 4`, `virt,virtualization=on` (an EL2 handoff, as the Raspberry
   Pi's; each secondary enters at EL2 and drops itself, the log must say
   the boot core `dropped from EL2 to EL1` and each core `up at EL1`).
3. `-smp 4`, `virt,gic-version=3`: the per-core GICv3 path (each core's
   redistributor found by its own MPIDR, the system-register interface on
   a secondary), which Parallels, the owed target, has.
4. `-smp 4`, `virt,virtualization=on,gic-version=3`: both at once, the
   one shape that runs the stub's EL2 GIC branch (`icc_sre_el2`,
   `ich_hcr_el2` for a core that will use the system registers).
5. `-smp 1`: one core, no `core up` line, the summary `0 of 0`, the shell
   as before.
6. `-smp 4` with the `\\SMPFAULT` flag file (an ESP directory copy, vvfat):
   the first core started takes an undefined instruction after its up
   line, the fault line must name that core (`EXCEPTION core=N`, read
   from the kernel's own `taking` line), the kernel must say that core
   `halted alone, holding nothing` and never `system halted`, and the boot
   core's shell must still answer: a fault on a core holding no kernel
   lock halts that core alone (a core inside an entry, or the boot core,
   halts the kernel, and every other core parks at its next entry). The
   halted core must not answer its kick, and the boot core must say so.

A boot with no `Ouroboros kernel` line is the firmware stalling in its own
boot, which happens; the rig says INCONCLUSIVE for that boot and does not
pass. About seven minutes. Run it whenever smp.rs, el2.rs, madt.rs's GICC
parse, the GIC backends' per-core init or SGIs, the kernel lock or the
console lock changes.
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
UP_RE = re.compile(r"smp: core (\d+) up at EL(\d) \(mpidr 0x[0-9a-f]+, affinity 0x[0-9a-f]+\), idling")
TICK_RE = re.compile(r"smp: core (\d+) ticking \(its first tick, from its idle loop at EL0\)")
PROBE_RE = re.compile(r"coreprobe: ran on cores? ([\d ]+), up to (\d+) programs running at once, (\d+) cores up, \d+ rounds")
HOLD_RE = re.compile(r"coreprobe: holding on core (\d+)")
CALLS_RE = re.compile(r"coreprobe: \d+ calls to fsd from core (\d+), (\d+) us each, (\d+) failed")
# A round trip to fsd from a secondary: 350-600 us on QEMU with the
# idle-core kick, about 46,000 us with it removed (each call then waits for
# the boot core's tick and the answer for the secondary's). The bound sits
# an order of magnitude from both.
CALL_BOUND_US = 5000
SUMMARY_RE = re.compile(r"smp: (\d+) of (\d+) started cores up, (\d+) cores in all")
KICK_RE = re.compile(r"smp: core (\d+) answered a kick from core (\d+), once the kernel lock was free")
KICK_FAIL_RE = re.compile(r"smp: core (\d+)(?: \(the boot core\))? (answered a kick while core \d+ held the kernel lock|cannot be kicked|cannot be sent an SGI|did not answer a kick within a second|answered every kick but was never seen waiting with one|is down \(it halted after coming up\), not kicked)")


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
    probe = {}
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
        if dwell_ok and not esp:
            probe = run_probes(guest)
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
    ticking = sorted(int(i) for i in TICK_RE.findall(out))
    taking = re.search(r"smp: core (\d+) taking the \\SMPFAULT undefined instruction", out)
    # The boot core's MADT index, from the table, not assumed 0: the kernel
    # takes whichever index lists its affinity (ACPI fixes no order on ARM).
    boot_index = next((int(i) for i, m, _ in cores if count and m == count.group(3)), 0)
    secondaries = [i for i in range(smp) if i != boot_index]
    # The core that takes the \SMPFAULT fault halts before its idle loop,
    # so it is the one core expected not to tick.
    expected_ticking = [i for i in secondaries if not (taking and i == int(taking.group(1)))]
    summary = SUMMARY_RE.search(out)
    kicked = sorted(int(i) for i, _ in KICK_RE.findall(out))
    kick_fails = KICK_FAIL_RE.findall(out)
    # The \SMPFAULT victim halted with every interrupt masked: the one core
    # expected not to answer, and to be named for it.
    # Either it halted before the boot core's ping reached it, and marked
    # itself down (step 4(d)), or after, and did not answer: a race, and
    # both lines say the same thing.
    victim_silent = [(i, why) for i, why in kick_fails if taking and i == taking.group(1)
                     and (why.startswith("did not answer") or why.startswith("is down"))]
    other_fails = [f"core {i} {why}" for i, why in kick_fails if (i, why) not in victim_silent]
    others = smp - 1
    checks = [
        (f"{name}: driven to the shell, `help` answered", ok),
        (f"{name}: the log says {smp} cores" + (f" (it said {count.group(1)})" if count and count.group(1) != str(smp) else ""),
         count is not None and count.group(1) == str(smp) and not count.group(2)),
        (f"{name}: one MADT line per core, {smp} distinct MPIDRs" + (f" (saw {len(cores)}: {' '.join(mpidrs)})" if len(set(mpidrs)) != smp else ""),
         len(cores) == smp and len(set(mpidrs)) == smp and [int(i) for i, _, _ in cores] == list(range(smp))),
        (f"{name}: the boot core's affinity is among them", count is not None and count.group(3) in mpidrs),
        (f"{name}: every core enabled", len(cores) == smp and all(e == "enabled" for _, _, e in cores)),
        (f"{name}: every other core says up at EL1 and idling ({others} of them)" + (f" (saw cores {up_cores})" if up_cores != secondaries else ""),
         up_cores == secondaries and all(el == "1" for _, el in ups)),
        (f"{name}: every other core took its first tick from its idle loop at EL0 ({len(expected_ticking)} of them)" + (f" (saw cores {ticking})" if ticking != expected_ticking else ""),
         ticking == expected_ticking),
        (f"{name}: the summary says {others} of {others} started cores up, {smp} in all",
         summary is not None and summary.groups() == (str(others), str(others), str(smp))),
        (f"{name}: every other core answered a kick SGI from the boot core once the kernel lock was free ({len(expected_ticking)} of them)"
         + (f" (saw cores {kicked})" if kicked != expected_ticking else ""),
         kicked == expected_ticking and all(src == str(boot_index) for _, src in KICK_RE.findall(out))),
        (f"{name}: no core answered a kick under the lock, or could not be kicked"
         + (f" (saw: {'; '.join(other_fails)})" if other_fails else ""), not other_fails),
        (f"{name}: the shell still answers after a {DWELL} s dwell with the other cores idling and ticking", dwell_ok),
    ]
    if not esp:
        checks += probe_checks(name, smp, boot_index, probe)
    if "virtualization=on" in machine:
        checks.append((f"{name}: the boot core dropped from EL2 to EL1", "dropped from EL2 to EL1" in out))
    if fault:
        victim = taking.group(1) if taking else "?"
        checks += [
            (f"{name}: a core took the \\SMPFAULT instruction (core {victim})", taking is not None),
            (f"{name}: the fault line names that core (EXCEPTION core={victim}, an undefined instruction, esr 0x2000000)",
             taking is not None and re.search(rf"EXCEPTION core={victim} vector=\d+ esr_el1=0x2000000 ", out) is not None),
            (f"{name}: no other core's fault line", taking is not None and re.search(rf"EXCEPTION core=(?!{victim} )", out) is None),
            (f"{name}: the halted core was not answered for: it did not answer its kick, or was down before it, and the boot core said so", len(victim_silent) == 1),
            (f"{name}: that core halted alone, holding nothing, and the kernel did not halt",
             taking is not None and f"core {victim} halted alone, holding nothing; the rest go on" in out and "system halted" not in out),
        ]
    else:
        checks.append((f"{name}: no fault lines", faults == 0 and "EXCEPTION" not in out))
    return checks


def run_probes(guest):
    """Step 4(d): /bin/COREPROBE alone, as a pair, and held then ended by
    Ctrl+C, each transcript cut out for its own checks."""
    got = {}
    guest.type_line("coreprobe")
    got["alone"] = guest.wait_for(r"rounds[\s\S]*" + PROMPT, timeout=60)
    got["alone_out"] = guest.transcript()
    guest.type_line("coreprobe | coreprobe -")
    got["pair"] = guest.wait_for(r"rounds[\s\S]*rounds[\s\S]*" + PROMPT, timeout=60)
    got["pair_out"] = guest.transcript()
    many = " | ".join(["coreprobe"] + ["coreprobe -"] * 3)
    guest.type_line(many)
    got["four"] = guest.wait_for(r"(?:rounds[\s\S]*){4}" + PROMPT, timeout=90)
    got["four_out"] = guest.transcript()
    guest.type_line("coreprobe calls")
    got["calls"] = guest.wait_for(r"failed[\s\S]*" + PROMPT, timeout=120)
    got["calls_out"] = guest.transcript()
    guest.type_line("coreprobe hold")
    held = guest.wait_for(r"holding on core \d+", timeout=30)
    time.sleep(1)
    guest.type_raw(b"\x03")
    held = held and guest.wait_for(r"terminated[\s\S]*" + PROMPT, timeout=30)
    guest.type_line("echo after-the-kill")
    got["hold"] = held and guest.wait_for(r"after-the-kill\s+[\s\S]*" + PROMPT, timeout=30)
    got["hold_out"] = guest.transcript()
    return got


def probe_checks(name, smp, boot_index, probe):
    """Every program runs on a secondary when there are any, two programs
    at once in a pipeline, and a program running on another core is ended
    there when the boot core's tick takes its Ctrl+C."""
    if not probe:
        return [(f"{name}: the core probes ran (the shell never answered after the dwell)", False)]
    alone_out = probe["alone_out"]
    pair_out = probe["pair_out"][len(alone_out):]
    four_out = probe["four_out"][len(probe["pair_out"]):]
    calls_out = probe["calls_out"][len(probe["four_out"]):]
    four = PROBE_RE.findall(four_out)
    most = max((int(r[1]) for r in four), default=0)
    hold_out = probe["hold_out"][len(probe["calls_out"]):]
    calls = CALLS_RE.search(calls_out)
    alone = PROBE_RE.findall(alone_out)
    pair = PROBE_RE.findall(pair_out)
    held = HOLD_RE.search(hold_out)
    secondaries = {str(i) for i in range(smp) if i != boot_index} or {str(boot_index)}
    def placed(reports):
        return all(set(r[0].split()) <= secondaries for r in reports)
    ended = re.search(r"Ctrl\+C - foreground task (\d+) terminated", hold_out)
    remote = re.search(r"task (\d+) ended on core (\d+), as another core asked", hold_out)
    many = smp > 1
    checks = [
        (f"{name}: coreprobe alone ran on " + ("a secondary core" if many else "the one core") + f", one program at once, {smp} cores up"
         + (f" (saw {alone})" if not (len(alone) == 1 and placed(alone) and alone[0][1:] == ("1", str(smp))) else ""),
         probe["alone"] and len(alone) == 1 and placed(alone) and alone[0][1:] == ("1", str(smp))),
        (f"{name}: `coreprobe | coreprobe -` ran " + ("on secondaries, two programs at once" if many else "on the one core, one at a time")
         + (f" (saw {pair})" if len(pair) != 2 else ""),
         probe["pair"] and len(pair) == 2 and placed(pair) and all(r[1] == ("2" if many else "1") for r in pair)),
        (f"{name}: four probes in one pipeline ran only on " + ("the secondaries, as many at once as there are of them" if many else "the one core, one at a time")
         + (f" (saw {four})" if len(four) != 4 or not placed(four) or most != len(secondaries) else ""),
         probe["four"] and len(four) == 4 and placed(four) and most == len(secondaries)),
        (f"{name}: `coreprobe calls` reached fsd from " + ("a secondary" if many else "the one core")
         + f" in under {CALL_BOUND_US} us a round trip" + (f" (core {calls.group(1)}, {calls.group(2)} us, {calls.group(3)} failed)" if calls else ""),
         probe["calls"] and calls is not None and calls.group(1) in secondaries
         and int(calls.group(2)) < CALL_BOUND_US and calls.group(3) == "0"),
        (f"{name}: `coreprobe hold` started on " + ("a secondary" if many else "the one core") + (f" (core {held.group(1)})" if held else ""),
         held is not None and held.group(1) in secondaries),
        (f"{name}: Ctrl+C ended it and the shell answered after", probe["hold"] and ended is not None),
    ]
    if many:
        checks.append((f"{name}: it was ended on the core it ran on, as the boot core asked"
                       + (f" (core {remote.group(2)})" if remote else ""),
                       remote is not None and held is not None and ended is not None
                       and remote.groups() == (ended.group(1), held.group(1))))
    else:
        checks.append((f"{name}: one core, so no kill was sent to another", remote is None))
    return checks


def main() -> int:
    checks = []
    checks += boot("smp4", 4)
    checks += boot("smp4-el2", 4, machine="virt,virtualization=on")
    checks += boot("smp4-gicv3", 4, machine="virt,gic-version=3")
    checks += boot("smp4-el2-gicv3", 4, machine="virt,virtualization=on,gic-version=3")
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

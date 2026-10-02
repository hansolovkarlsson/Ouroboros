#!/usr/bin/env python3
"""Test the early fault reporter (kernel/src/earlyfault.rs) on QEMU.

    python3 scripts/test-early-fault.py [--esp build/esp] [--keep] [--el2]

Two boots of a copy of the staged ESP, each graded from the serial console:

1. With the `EARLYFAULT` flag file at the ESP root (bootflags.rs), the
   kernel asks the firmware's own CopyMem to write at an address nothing
   maps, just before the xHCI takeover. The fault is taken inside the
   firmware's DXE core while the firmware's vectors are still installed,
   which is the case a Raspberry Pi 4's first serial boot showed with only
   the firmware's one line, `Synchronous Exception at 0x...`, to read. The
   boot must end in the reporter's dump: the reporter armed, an
   `EARLY EXCEPTION` line whose FAR is the address the kernel said it would
   write (read back from its own log line, so the test carries no copy of
   the constant), the ESR decoded as a data abort at the same EL, `elr`
   placed in a firmware image (the DXE core, named from its PE debug entry),
   at least one backtrace frame placed in the kernel by offset, the register
   rows BEFORE the first frame (the rows read nothing and the walk reads
   firmware memory, which faulted on a Pi 4 and lost the rows printed after
   it), and the halt line. The firmware's own line must NOT appear: the
   registered handler replaces the default one, so its presence would mean
   the registration did not take.

2. Without the flag, the control: the reporter arms, the boot leaves boot
   services, says which exception level it was handed off at, and reaches
   the shell, where `help` typed after a pause is answered (the shell task
   is scheduled and served, so the tick and the keyboard both work); no
   `EARLY EXCEPTION` and no kernel `EXCEPTION` line appears.

With --el2 both boots run on `-machine virt,virtualization=on`, where the
firmware hands the kernel off at EL2 as the Raspberry Pi's does (`make
run-el2`, `make test-el1-drop`). The fault boot is the same test: the
reporter's dump is the firmware's handler either way. The control is the
EL1 drop's check (kernel/src/el2.rs, docs/roadmap/roadmap-el1-drop.md): the
kernel must say `running at EL2 after the exit`, then `dropped from EL2 to
EL1`, and then behave as the plain control does. Before the drop this boot
reached `shell ready` and then faulted at address 0 through the firmware's
vectors: the first eret into task 0, made at EL2, restored the firmware's
stale ELR_EL2 and SPSR_EL2 (see el2.rs); so the control's dwell after the
shell line, and the typed `help`, are what can fail here.

Both boots also read QEMU's own -d int trace, the project's health bar
(drive-qemu.py's Guest.aborts): the control requires zero fault lines, and
a missing trace is a failure, not a pass. The serial transcript alone would
grade a post-drop fault the kernel survives (an EL0 task killed, a server
restarted) as a pass while the shell still answers.

Why both: the dump proves the handler is reached and reads the context the
firmware passes; the control proves arming it costs the ordinary boot
nothing (the firmware's IRQ handling, which its timer and events need,
keeps running under the kernel's registration). A boot that shows the
dump without the flag is a failure of the control, not a pass of the test.

The guest is drive-qemu.py's `Guest`, so the machine is the dev loop's
(`make run`), with the ESP copy on the vvfat drive in place of an image.
Each boot runs to its end state or a timeout, and both always run, so one
failure does not hide the other result. The transcripts are kept whenever
a grade failed, and with --keep; their directory is printed. Exit status 1
if either boot failed.
"""
import argparse
import importlib.util
import os
import re
import shutil
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

TIMEOUT = 90
# The control's dwell at the shell before `help` is typed: long enough for
# whatever follows `shell ready` to go wrong (before the drop, the fault came
# within a second of it).
DWELL = 5
# The lines a boot can end on, besides the shell: the reporter's halt and the
# firmware's one line (a fault under the firmware's vectors), the kernel's
# own report (a fault under its vectors, which halts through a silent loop),
# an EL0 task's fault, and the halt the drop takes on a level it has no path
# for. Named so a failed boot ends at its line instead of at TIMEOUT.
ENDINGS = r"halted in the early fault reporter|Synchronous Exception at|EXCEPTION vector=|EL0 FAULT|system halted"


def boot(esp, until, machine, then=None):
    """Boot the dev-loop machine on the ESP directory `esp` (vvfat) and
    return the serial transcript and the fault-line count from QEMU's trace
    (None if there was no trace) once `until` (a regex) matches or TIMEOUT
    seconds pass. With `then`, a line to type after a DWELL at the match,
    the transcript also holds the shell's answer (or its absence)."""
    guest = drive_qemu.Guest(
        os.path.join(esp, "boot"), virtio_disk=False, label=os.path.basename(esp),
        extra_args=["-drive", f"file=fat:rw:{esp},format=raw,media=disk,if=none,id=hd0",
                    "-device", "virtio-blk-device,drive=hd0"],
        machine=machine,
    )
    try:
        matched = guest.wait_for(until, timeout=TIMEOUT)
        # Only a boot that reached the shell, and has not ended since, gets
        # the dwell and the typed line: `until` also names the failure
        # lines, a fault can follow `shell ready` within milliseconds (the
        # pre-drop one did, before the driver's high-water mark moved past
        # it), and typing into a halted guest would put a `help` in a
        # transcript that never had a shell, and cost 25 s more per failed
        # run.
        so_far = guest.transcript()
        if matched and then and re.search(r"shell ready", so_far) and not re.search(ENDINGS, so_far):
            time.sleep(DWELL)
            guest.type_line(then)
            if not guest.wait_for(r"builtins:|" + ENDINGS, timeout=20):
                guest.report(f"!!! TIMEOUT waiting for the answer to {then!r} after the dwell")
        return guest.transcript(), guest.aborts()
    finally:
        guest.stop()


def grade(name, text, must, must_not, aborts=None, expect_aborts=None, order=()):
    """`expect_aborts`: None to leave QEMU's trace ungraded, "zero" to
    require a clean trace, "some" to require at least one fault line (the
    planted fault). A missing trace fails either way. `order`: (label,
    first, second) triples, each a pair of patterns whose first match must
    come before the other's."""
    ok = True
    for label, first, second in order:
        a, b = re.search(first, text, re.M), re.search(second, text, re.M)
        good = bool(a and b and a.start() < b.start())
        print(f"  {'ok  ' if good else 'FAIL'} {label}")
        ok &= good
    if expect_aborts is not None:
        if aborts is None:
            print("  FAIL QEMU trace: NO TRACE (health bar unavailable - this is not a pass)")
            ok = False
        else:
            want = aborts == 0 if expect_aborts == "zero" else aborts > 0
            print(f"  {'ok  ' if want else 'FAIL'} QEMU trace: {aborts} fault lines (Abort/SError), expected {expect_aborts}")
            ok &= want
    for label, pattern in must:
        hit = re.search(pattern, text, re.M)
        print(f"  {'ok  ' if hit else 'FAIL'} {label}" + (f": {hit.group(0).strip()[:110]}" if hit else ""))
        ok &= bool(hit)
    for label, pattern in must_not:
        hit = re.search(pattern, text, re.M)
        print(f"  {'FAIL' if hit else 'ok  '} no {label}" + (f": {hit.group(0).strip()[:110]}" if hit else ""))
        ok &= not hit
    print(f"{name}: {'PASS' if ok else 'FAIL'}")
    return ok


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--esp", default="build/esp", help="the staged ESP directory (make esp)")
    ap.add_argument("--keep", action="store_true", help="keep the transcripts even when both boots pass")
    ap.add_argument("--el2", action="store_true", help="hand the kernel off at EL2 (virtualization=on): the EL1 drop's rig")
    args = ap.parse_args()
    machine = "virt,virtualization=on" if args.el2 else "virt"
    handed_off = "EL2" if args.el2 else "EL1"
    if not os.path.isdir(args.esp):
        sys.exit(f"{args.esp} is not a directory; run `make esp` first")

    work = tempfile.mkdtemp(prefix="early-fault-")
    results = []

    # 1. The fault, with the flag set.
    esp = os.path.join(work, "esp-fault")
    shutil.copytree(args.esp, esp)
    open(os.path.join(esp, "EARLYFAULT"), "w").close()
    print("boot 1: EARLYFAULT set, expecting the reporter's dump")
    text, aborts = boot(esp, ENDINGS + r"|shell ready", machine)
    open(os.path.join(work, "fault.serial"), "w").write(text)
    planted = re.search(r"asking the firmware to copy to (0x[0-9a-f]+)", text)
    far = planted.group(1) if planted else "<the address the kernel logged>"
    results.append(grade("fault", text, [
        ("reporter armed", r"early fault reporter armed"),
        ("flag seen", r"boot flag \\EARLYFAULT is set"),
        ("planted address logged", r"asking the firmware to copy to 0x[0-9a-f]+"),
        ("exception line, FAR the planted address",
         r"EARLY EXCEPTION \(firmware vectors\) type=0 esr=0x[0-9a-f]+ far=" + re.escape(far) + r" elr=0x"),
        ("esr decoded as a same-EL data abort", r"esr: ec=0x25 \(data abort, same EL\), fsc=0x[0-9a-f]+ \(.*\), write"),
        ("elr placed in a firmware image", r"elr 0x[0-9a-f]+ = \S*DxeCore\S* \(firmware file [0-9a-f-]+\) @ 0x[0-9a-f]+ \+ 0x"),
        ("a frame placed in the kernel", r"frame \d+: fp=0x[0-9a-f]+ lr=0x[0-9a-f]+ = kernel \+ 0x"),
        ("register rows", r"x28=0x[0-9a-f]{16} x29=0x[0-9a-f]{16} x30=0x[0-9a-f]{16}"),
        ("halt line", r"halted in the early fault reporter"),
    ], [
        ("firmware's own line", r"Synchronous Exception at"),
        ("shell", r"shell ready"),
    ], aborts, "some", order=[
        ("register rows before the first frame", r"x28=0x[0-9a-f]{16}", r"frame 1: fp="),
    ]))

    # 2. The control, no flag.
    esp = os.path.join(work, "esp-plain")
    shutil.copytree(args.esp, esp)
    print(f"boot 2: no flag, handed off at {handed_off}, expecting the shell and its answer to `help`")
    text, aborts = boot(esp, r"shell ready|" + ENDINGS, machine, then="help")
    open(os.path.join(work, "plain.serial"), "w").write(text)
    results.append(grade("control", text, [
        ("reporter armed", r"early fault reporter armed"),
        ("boot services exited", r"exiting boot services"),
        (f"handed off at {handed_off}", rf"running at {handed_off} after the exit"),
        *([("dropped to EL1", r"dropped from EL2 to EL1, on our own tables and vectors")] if args.el2 else []),
        ("identity map installed", r"identity map installed, MMU running on our own tables"),
        ("shell", r"shell ready"),
        ("help answered after the dwell", r"^builtins: help"),
    ], [
        ("EARLY EXCEPTION", r"EARLY EXCEPTION"),
        ("kernel EXCEPTION", r"EXCEPTION vector="),
        ("EL0 fault, kill or restart", r"EL0 FAULT|killed after fault|restarted"),
        ("firmware's own line", r"Synchronous Exception at"),
    ], aborts, "zero"))

    passed = all(results)
    if args.keep or not passed:
        print(f"transcripts kept in {work}")
    else:
        shutil.rmtree(work)
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()

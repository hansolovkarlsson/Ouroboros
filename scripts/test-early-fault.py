#!/usr/bin/env python3
"""Test the early fault reporter (kernel/src/earlyfault.rs) on QEMU.

    python3 scripts/test-early-fault.py [--esp build/esp] [--keep]

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
   rows, and the halt line. The firmware's own line must NOT appear: the
   registered handler replaces the default one, so its presence would mean
   the registration did not take.

2. Without the flag, the control: the reporter arms, the boot leaves boot
   services and reaches the shell, and no `EARLY EXCEPTION` line appears.

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

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

TIMEOUT = 90


def boot(esp, until):
    """Boot the dev-loop machine on the ESP directory `esp` (vvfat) and
    return the serial transcript once `until` (a regex) matches or TIMEOUT
    seconds pass."""
    guest = drive_qemu.Guest(
        os.path.join(esp, "boot"), virtio_disk=False, label=os.path.basename(esp),
        extra_args=["-drive", f"file=fat:rw:{esp},format=raw,media=disk,if=none,id=hd0",
                    "-device", "virtio-blk-device,drive=hd0"],
    )
    try:
        guest.wait_for(until, timeout=TIMEOUT)
        return guest.transcript()
    finally:
        guest.stop()


def grade(name, text, must, must_not):
    ok = True
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
    args = ap.parse_args()
    if not os.path.isdir(args.esp):
        sys.exit(f"{args.esp} is not a directory; run `make esp` first")

    work = tempfile.mkdtemp(prefix="early-fault-")
    results = []

    # 1. The fault, with the flag set.
    esp = os.path.join(work, "esp-fault")
    shutil.copytree(args.esp, esp)
    open(os.path.join(esp, "EARLYFAULT"), "w").close()
    print("boot 1: EARLYFAULT set, expecting the reporter's dump")
    text = boot(esp, r"halted in the early fault reporter|Synchronous Exception at|shell ready")
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
    ]))

    # 2. The control, no flag.
    esp = os.path.join(work, "esp-plain")
    shutil.copytree(args.esp, esp)
    print("boot 2: no flag, expecting the shell")
    text = boot(esp, r"shell ready|halted in the early fault reporter|Synchronous Exception at")
    open(os.path.join(work, "plain.serial"), "w").write(text)
    results.append(grade("control", text, [
        ("reporter armed", r"early fault reporter armed"),
        ("boot services exited", r"exiting boot services"),
        ("shell", r"shell ready"),
    ], [
        ("EARLY EXCEPTION", r"EARLY EXCEPTION"),
        ("firmware's own line", r"Synchronous Exception at"),
    ]))

    passed = all(results)
    if args.keep or not passed:
        print(f"transcripts kept in {work}")
    else:
        shutil.rmtree(work)
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()

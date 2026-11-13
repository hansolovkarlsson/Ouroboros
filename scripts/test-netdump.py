#!/usr/bin/env python3
"""Test the Pi 4 Ethernet scan (kernel/src/netscan.rs) where there is no
such device: on QEMU.

    python3 scripts/test-netdump.py [--esp build/esp] [--keep]

The scan runs only under the `NETDUMP` flag file at the ESP root
(bootflags.rs) and reads a GENET's registers only when the ACPI tables
declare exactly one present. QEMU's `virt` machine declares none, and no
QEMU machine this kernel boots models the device
(docs/roadmap/roadmap-pi-net.md, "No emulator can stand in"), so what this
rig can hold is the half that matters everywhere but on a Pi: the scan
touches nothing it was not told is there. Two boots of a copy of the staged
ESP, each graded from the serial console and from QEMU's own trace:

1. With `NETDUMP`: the flag is seen, the scan says no GENET is declared and
   that no register was touched, no other line of the scan appears,
   and the boot goes on to the shell and answers `help`, with zero fault
   lines in QEMU's trace. A scan that read the Pi's address anyway would
   fault here, before the exit, through the firmware's vectors.

2. Without it, the control: no line of the scan appears at all, so the
   flag is what runs it, and the boot is the ordinary one.

What the scan reads from a real GENET (the registers, the MDIO bus, the
bounds on both) is the `genet` crate's and is tested on the host against a
model (`make test`). What a real GENET answers, only a board shows: step 2's
card round trip.

The boots and the grading are test-early-fault.py's, imported, so the two
rigs boot the same machine and read the same trace. Exit status 1 if either
boot failed; the transcripts are kept on a failure and with --keep.
"""
import argparse
import importlib.util
import os
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("early_fault", os.path.join(HERE, "test-early-fault.py"))
early = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(early)

# Lines only the scan prints, and of those, any but the two a machine with
# no GENET may print. Named by what is allowed, so a line added to
# netscan.rs is refused here without this list hearing of it.
SCAN_LINE = r"Ouroboros kernel: net: "
BEYOND_THE_DECLARATION = SCAN_LINE + r"(?!GENET declared present: |not exactly one GENET declared: no register touched)"
FAULTS = early.CLEAN_BOOT_MUST_NOT


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--esp", default="build/esp", help="the staged ESP directory (make esp)")
    ap.add_argument("--keep", action="store_true", help="keep the transcripts even when both boots pass")
    args = ap.parse_args()
    if not os.path.isdir(args.esp):
        sys.exit(f"{args.esp} is not a directory; run `make esp` first")
    # The staged kernel must be this tree's commit, as test-early-fault.py's
    # control requires: a rig run by hand on a stale ESP would pass for code
    # it never booted.
    ident, head = early.staged_build(args.esp)
    print(f"staged kernel: build {ident or '<not exactly one identity in the image>'}; this tree: {head}")
    identity = early.identity_checks(ident, head)
    work = tempfile.mkdtemp(prefix="netdump-")
    results = []

    esp = os.path.join(work, "esp-netdump")
    shutil.copytree(args.esp, esp)
    open(os.path.join(esp, "NETDUMP"), "w").close()
    print("boot 1: NETDUMP set on QEMU, expecting no GENET, no register touched, and the shell")
    text, aborts = early.boot(esp, r"shell ready|" + early.ENDINGS, "virt", then="help")
    open(os.path.join(work, "netdump.serial"), "w").write(text)
    results.append(early.grade("netdump", text, [
        *identity,
        ("flag seen", r"boot flag \\NETDUMP is set"),
        ("none declared", r"net: GENET declared present: 0 \(unreadable 0, absent 0, status unknown 0, dropped 0\)"),
        ("says it touched nothing", r"net: not exactly one GENET declared: no register touched"),
        ("boot services exited", r"exiting boot services"),
        ("shell", r"shell ready"),
        ("help answered after the dwell", r"^builtins: help"),
    ], [("a scan line beyond the declaration", BEYOND_THE_DECLARATION)] + FAULTS, aborts, "zero", order=[
        ("the scan's lines before the exit", r"no register touched", r"exiting boot services"),
    ]))

    esp = os.path.join(work, "esp-plain")
    shutil.copytree(args.esp, esp)
    print("boot 2: no flag, expecting no line of the scan and the shell")
    text, aborts = early.boot(esp, r"shell ready|" + early.ENDINGS, "virt", then="help")
    open(os.path.join(work, "plain.serial"), "w").write(text)
    results.append(early.grade("control", text, [
        *identity,
        ("shell", r"shell ready"),
        ("help answered after the dwell", r"^builtins: help"),
    ], [("any line of the scan", SCAN_LINE), ("the flag", r"NETDUMP")] + FAULTS, aborts, "zero"))

    passed = all(results)
    if args.keep or not passed:
        print(f"transcripts kept in {work}")
    else:
        shutil.rmtree(work)
    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()

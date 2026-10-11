#!/usr/bin/env python3
"""`/proc/cpus`: how many cores take tasks, read as a file.

    python3 scripts/test-proc-cpus.py      (or `make test-proc-cpus`, which builds the image)

Two boots of the FAT32 image, `-smp 1` and `-smp 4` (named here, so the
`SMP` environment does not apply). In each, after `mount -p /proc`:
`ls /proc` lists `cpus` beside the slot directories, `cat /proc/cpus` prints
the core count on a line of its own (1, then 4: the boot core and every
secondary that came up, the kernel's `CORE_INFO_UP`), `cat /proc/cpus/x` is
refused as not a directory (nothing lives under the file, and a path below
one answers as it does on a disk), and the shell still
answers afterwards. The count under `-smp 4` fails on a kernel where a
secondary did not come up, which `make test-smp` would also say. Two boots,
about a minute and a half. Run it whenever fsd's proc.rs or CORE_INFO
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-proc-cpus-{}.txt")
PROMPT = "# "


def boot(cores):
    guest = drive_qemu.Guest(IMAGE, extra_args=("-smp", str(cores)), label=f"test-proc-cpus[{cores}]: ")
    try:
        steps = [("login:", "root"), ("assword", "root"),
                 (PROMPT, "mount -p /proc"), (PROMPT, "ls /proc"), (PROMPT, "cat /proc/cpus"),
                 (PROMPT, "cat /proc/cpus/x"), (PROMPT, "echo after-cpus"), (PROMPT, "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(cores), "w") as fh:
        fh.write(out)
    after = out.split("ls /proc", 1)[1] if "ls /proc" in out else ""
    listing = after.split("cat /proc/cpus", 1)[0]
    # The first digit-only line between the echoed command and the next
    # prompt, not the very next line: under -smp 4 another core's kernel
    # line may land between the two (the high review of #270).
    block = re.search(r"cat /proc/cpus\r?\n([\s\S]*?)\n# ", out)
    cat = re.search(r"^(\d+)\r?$", block.group(1), re.M) if block else None
    under = re.search(r"cat /proc/cpus/x\r?\n([^\r\n]*)", out)
    return [
        (f"smp {cores}: driven to the end", driven),
        (f"smp {cores}: `ls /proc` lists cpus", bool(re.search(r"\bcpus\b", listing))),
        (f"smp {cores}: `cat /proc/cpus` prints {cores} on its own line", bool(cat) and cat.group(1) == str(cores)),
        (f"smp {cores}: `cat /proc/cpus/x` is not a directory", bool(under) and "not a directory" in under.group(1)),
        (f"smp {cores}: the shell answers after", bool(re.search(r"\nafter-cpus\r?\n", out))),
        (f"smp {cores}: no fault lines", faults == 0),
    ], (cat.group(1) if cat else "none", under.group(1) if under else "none")


def main() -> int:
    failed = 0
    for cores in (1, 4):
        checks, (cat, under) = boot(cores)
        print(f"     smp {cores}: cat /proc/cpus -> {cat}; cat /proc/cpus/x -> {under}")
        for name, ok in checks:
            print(f"{'ok  ' if ok else 'FAIL'} {name}")
            failed += 0 if ok else 1
    print(f"transcripts: {os.path.relpath(TRANSCRIPT.format('1'), ROOT)}, {os.path.relpath(TRANSCRIPT.format('4'), ROOT)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

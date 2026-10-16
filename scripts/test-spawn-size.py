#!/usr/bin/env python3
"""The size of a program file spawn accepts, on a copy of the image.

    python3 scripts/test-spawn-size.py      (or `make test-spawn-size`, which builds the image)

A program FILE is staged whole in the kernel (`SPAWN_STAGING_SIZE`,
syscall-abi) before `SPAWN` parses it. That bound was 128 KiB until
2026-10-16, an eighth of the room a loaded image has, and `/bin/CPP` was
107,648 bytes and growing. The host writes three copies of `/bin/ARGS`,
zero-padded past its end (the loader reads only what the program headers
name), into `/spawn` on a copy of the image, sized from the constant as the
kernel is:

- `past128`, 200 KiB, past the old bound: runs, `argv[1] = p1`;
- `atmax`, exactly `SPAWN_STAGING_SIZE`: runs, `argv[1] = p2`;
- `overmax`, one byte more: refused with the shell's message naming the
  bound in KB, and prints no argv;
- then `/bin/args p4` runs, so the refused spawn left the staging free.

The shell prints that message for any `SPAWN_STAGE` refusal, so what ties
`overmax`'s refusal to the bound is `atmax`, one byte shorter, running. Each
run's wall time is reported, not graded: a file is staged in 512-byte
chunks, one fsd read each, and the staging is held for all of it. No fault
line in QEMU's trace. The control: with the constant at 131072 (and the
kernel's assert that it covers the image bound taken out so it builds),
`past128` fails, while `atmax` and `overmax` pass at the old bound and the
message names 128KB, so the sizes follow the constant. One boot, about a
minute. Run it whenever SPAWN_STAGING_SIZE, SPAWN_STAGE or the shell's
spawn_path changes. It drives only the shell's loop: netd's `cpu` stages
the same way and no rig spawns a large file through it.
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
COPY = os.path.join(ROOT, "build", "test-spawn-size.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-spawn-size.txt")
ABI = os.path.join(ROOT, "syscall-abi", "src", "lib.rs")
OLD_BOUND = 128 * 1024


def staging_size():
    """SPAWN_STAGING_SIZE as syscall-abi defines it: digits and `*` only."""
    with open(ABI) as fh:
        m = re.search(r"^pub const SPAWN_STAGING_SIZE: usize = ([0-9_ *]+);", fh.read(), re.M)
    if not m:
        sys.exit("test-spawn-size: SPAWN_STAGING_SIZE not found in syscall-abi")
    size = 1
    for factor in m.group(1).replace("_", "").split("*"):
        size *= int(factor)
    return size


def main() -> int:
    size = staging_size()
    # past128 is 200 KiB whatever the constant says, so with the constant
    # back at the old bound it is refused and this rig fails (the control).
    files = {"past128": 200 * 1024, "atmax": size, "overmax": size + 1}

    shutil.copyfile(IMAGE, COPY)
    mp = drive_qemu.attach(COPY, readonly=False, prefix="test-spawn-size.")
    try:
        with open(os.path.join(mp, "bin", "ARGS"), "rb") as fh:
            args = fh.read()
        os.makedirs(os.path.join(mp, "spawn"), exist_ok=True)
        for name, length in files.items():
            with open(os.path.join(mp, "spawn", name), "wb") as fh:
                fh.write(args + bytes(length - len(args)))
        for dirpath, _, names in os.walk(mp):
            for f in names:
                if f.startswith("._"):
                    os.remove(os.path.join(dirpath, f))
    finally:
        drive_qemu.detach(mp)
    print(f"     /bin/ARGS is {len(args)} bytes; padded to {files['past128']}, {files['atmax']} and {files['overmax']}")

    runs = [("past128", "/spawn/past128 p1"), ("atmax", "/spawn/atmax p2"),
            ("overmax", "/spawn/overmax p3"), ("after", "/bin/args p4")]
    took = {}
    guest = drive_qemu.Guest(COPY, label="test-spawn-size: ")
    try:
        driven = guest.run([("login:", "root"), ("assword", "root"), ("# ", "")])
        for name, line in runs:
            if not driven:
                break
            # Timed from the line's Enter, so the paced typing is not in it;
            # each run's end is the shell's next prompt after its own echo.
            guest.type_line(line)
            start = time.monotonic()
            driven = guest.wait_for(re.escape(line) + r"\r?\n(?:.*\r?\n)*?# ")
            if driven:
                took[name] = time.monotonic() - start
        out = guest.transcript()
    finally:
        guest.stop()
    faults = guest.aborts()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    def block(line):
        """What the shell printed for `line`, up to the next prompt."""
        m = re.search(r"^# " + re.escape(line) + r"\r?\n((?:.*\r?\n)*?)# ", out, re.M)
        return m.group(1) if m else ""

    refusal = f"program file is over the kernel's {size // 1024}KB staging buffer"
    over = block("/spawn/overmax p3")
    checks = [
        ("driven to the end", driven),
        (f"past128 ({files['past128']} bytes, past the old {OLD_BOUND}) ran", "argv[1] = p1" in block("/spawn/past128 p1")),
        (f"atmax ({files['atmax']} bytes, the bound) ran", "argv[1] = p2" in block("/spawn/atmax p2")),
        (f"overmax ({files['overmax']} bytes) refused: {refusal!r}", refusal in over),
        ("overmax printed no argv", "argv[" not in over),
        ("a spawn after the refusal ran (the staging was given up)", "argv[1] = p4" in block("/bin/args p4")),
        ("no fault lines", faults == 0),
    ]
    for name, _ in runs:
        if name in took:
            print(f"     {name}: {took[name]:.1f} s")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

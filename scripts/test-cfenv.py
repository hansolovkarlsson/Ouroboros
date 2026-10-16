#!/usr/bin/env python3
"""The floating-point exception flags picolibc's math leaves, on a booted image.

    python3 scripts/test-cfenv.py      (or `make test-cfenv`, which builds the image)

`/bin/CPICO fenv` (libc/picodemo.c) clears the flags, makes one call, reads
them back with fetestexcept, and prints a line per check. Until 2026-10-16
picolibc was built without -frounding-math (libc/picolibc-cross.txt), and
clang computed both arms of a choice and selected one: round(1e308) and
fdim(-1e308, 1e308) returned the right value with OVERFLOW and INEXACT set.
One boot:

- `cpico fenv` prints each of CHECKS, every one `ok`, then `8 checks, 0
  failed`, and exits 0;
- the first four are the observer's own (flags feraiseexcept set, then
  cleared by feclearexcept, 1e308 * 10 raising OVERFLOW and INEXACT, 1 / 3
  raising INEXACT), so a fetestexcept that always answered 0, a feclearexcept
  that did nothing, or flags lost across the kernel, fail here and not
  silently in the four after;
- `cpico` with no argument still runs the demo to `done.`.

No fault line in QEMU's trace. The control: built against main's libc.a of
2026-10-15, the round and fdim(-1e308, 1e308) lines fail and the observer's
four pass. About a minute. Run it whenever picolibc is rebuilt or
libc/picolibc-cross.txt changes.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-cfenv.txt")
SUMMARY = r"cpico: (\d+) checks, (\d+) failed\r?\n"
# Each check's name as cpico prints it, in its order.
CHECKS = ["feraiseexcept raises", "feclearexcept clears", "1e308 * 10", "1 / 3", "round(1e308)", "round(-1e308)",
          "fdim(-1e308, 1e308)", "fdim(1e308, -1e308)"]


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-cfenv: ")
    try:
        # No wait for the prompt before `cpico`: it came right after the
        # summary, and wait_for's settle has already counted it as seen
        # (test-cclock.py).
        driven = guest.run([("login:", "root"), ("assword", "root"), ("# ", "cpico fenv"), (SUMMARY, ""),
                            ("", "cpico"), (r"done\.\r?\n", "")])
        out = guest.transcript()
    finally:
        guest.stop()
    # After stop: QEMU buffers its -d int trace (drive-qemu.py's main).
    faults = guest.aborts()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    parts = re.split(r"^# cpico\r?$", out, flags=re.M)
    fenv = re.split(r"^# cpico fenv\r?$", parts[0], flags=re.M)
    fenv = fenv[1] if len(fenv) > 1 else ""
    demo = parts[1] if len(parts) > 1 else ""
    checks = [("driven to the end", driven)]
    for name in CHECKS:
        m = re.search(r"^cpico: (ok|FAIL) " + re.escape(name) + r" = (.*?)\r?$", fenv, re.M)
        if m and m.group(1) == "FAIL":
            print(f"     {name} = {m.group(2)}")
        checks.append((f"cpico fenv: {name}", bool(m) and m.group(1) == "ok"))
    s = re.search(SUMMARY, fenv)
    checks.append((f"cpico fenv: {len(CHECKS)} checks, 0 failed",
                   bool(s) and (int(s.group(1)), int(s.group(2))) == (len(CHECKS), 0)))
    x = re.search(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)", fenv, re.M)
    checks.append(("cpico fenv exited 0", bool(x) and x.group(1) == "0"))
    checks.append(("cpico with no argument ran the demo to done.",
                   re.search(r"^qsort:\s+1 2 3 5 8 9\r?$", demo, re.M) is not None
                   and re.search(r"^done\.\r?$", demo, re.M) is not None))
    checks.append(("no fault lines", faults == 0))
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

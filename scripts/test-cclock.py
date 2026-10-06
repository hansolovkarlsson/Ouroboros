#!/usr/bin/env python3
"""The clock in a C program, against the host's clock.

    python3 scripts/test-cclock.py      (or `make test-cclock`, which builds the image)

Step 5 of docs/roadmap/roadmap-c-hosting.md: libc/pico/clock.c gives a
picolibc program `gettimeofday` and `clock_gettime` from the kernel's
MONOTONIC_US, so `time()` links and says 1970 plus uptime (there is no wall
clock yet). `/bin/CCLOCK` (libc/cclock.c) prints what it read and runs its own
checks. One boot, run twice with a gap the HOST times between the two:

- each run's own checks pass (`cclock: N checks, 0 failed`, N exactly
  CCLOCK_CHECKS), and it exits 0;
- its `ctime` line is what Python's own formatting makes of its `time=`
  second (a foreign formatter, not picolibc's);
- the guest clock advanced across the gap by what the host's clock did,
  within SLACK seconds (typing, echo and TCG make up the slack), and is not
  ahead of the host's time since QEMU started.

No fault line in QEMU's trace. About a minute. Run it whenever
libc/pico/clock.c or the kernel's MONOTONIC_US changes.
"""
import importlib.util
import os
import re
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-cclock.txt")
CCLOCK_CHECKS = 9
SUMMARY = r"cclock: (\d+) checks, (\d+) failed\r?\n"
GAP = 8.0    # seconds the host waits between the two runs
SLACK = 2.0  # seconds either way for typing, echo and emulation


def runs(out):
    """Each run's (time, us, ctime, checks, failed), in order."""
    found = []
    for block in re.split(r"^# cclock\r?$", out, flags=re.M)[1:]:
        t = re.search(r"^cclock: time=(\d+)\r?$", block, re.M)
        us = re.search(r"^cclock: us=(\d+)\r?$", block, re.M)
        ct = re.search(r"^cclock: ctime=(.*?)\r?$", block, re.M)
        s = re.search(r"^cclock: (\d+) checks, (\d+) failed\r?$", block, re.M)
        if t and us and ct and s:
            found.append((int(t.group(1)), int(us.group(1)), ct.group(1),
                          int(s.group(1)), int(s.group(2))))
    return found


def main() -> int:
    started = time.monotonic()
    guest = drive_qemu.Guest(IMAGE, label="test-cclock: ")
    try:
        driven = guest.run([("login:", "root"), ("assword", "root"), ("# ", "cclock"), (SUMMARY, "")])
        first_seen = time.monotonic()
        time.sleep(GAP)
        # No wait for the prompt: it came right after the first summary, and
        # wait_for's settle has already counted it as seen.
        driven = driven and guest.run([("", "cclock"), (SUMMARY, "")])
        second_seen = time.monotonic()
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    got = runs(out)
    checks = [("driven to the end", driven), ("two runs read", len(got) == 2)]
    exits = re.findall(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)", out, re.M)
    for i, (t, us, ct, n, failed) in enumerate(got[:2], 1):
        want = time.strftime("%a %b %e %H:%M:%S %Y", time.gmtime(t))
        checks += [
            (f"run {i}: {CCLOCK_CHECKS} checks, 0 failed", (n, failed) == (CCLOCK_CHECKS, 0)),
            (f"run {i}: ctime {ct!r} is the host's formatting of {t} s", ct == want),
            (f"run {i}: not ahead of the host's time since QEMU started",
             us / 1e6 <= (second_seen if i == 2 else first_seen) - started + SLACK),
        ]
    if len(got) == 2:
        guest_gap = (got[1][1] - got[0][1]) / 1e6
        host_gap = second_seen - first_seen
        print(f"     guest advanced {guest_gap:.2f} s while the host measured {host_gap:.2f} s")
        checks.append((f"the guest's clock kept the host's pace across the gap (within {SLACK} s)",
                       abs(guest_gap - host_gap) <= SLACK))
    checks += [
        ("both runs exited 0", exits[-2:] == ["0", "0"]),
        ("no fault lines", faults == 0),
    ]
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

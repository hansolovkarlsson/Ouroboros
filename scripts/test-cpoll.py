#!/usr/bin/env python3
"""poll for the keyboard in a C program (/bin/CPOLL, libc/cpoll.c).

    python3 scripts/test-cpoll.py    (or `make test-cpoll`, which builds the image)

DevTools's note docs/handoffs/2026-10-08-from-devtools-edit-poll.md: `poll` on
fd 0 with a timeout, so Edit can wait a moment for the rest of an escape
sequence and repeat a command until a key is pressed. One boot, the serial
line:

1. **`cpoll timing`**, nothing typed: timeout 0 answers 0 at once; timeout
   200 answers 0 after 200 to 260 ms (the wait is blocked in the kernel to
   a tick's precision, 20 ms, settled by Hans 2026-10-08, with room for a
   delayed tick); fd 1
   is ready for POLLOUT, a closed fd 9 is POLLNVAL (32), and with fd 1 ready
   fd 0 is answered too, at once, rather than skipped; no fds and timeout 100 sleeps 100 to 160 ms;
   a null array with one entry is EFAULT.
2. **`cpoll key`**: it writes `cpoll: waiting` with no newline, so the text
   shows only if poll flushed fd 1 before its wait; then `x` typed answers
   the timeout -1 wait and the read after it returns 120; `y` typed a second
   into the 3000 ms wait returns 121 in time.

3. **`cpoll key`, then**: `z` typed during a sleep, and a poll of fds 0
   and 1 together must answer fd 0 POLLIN beside the ready fd 1.
4. **`cpoll leave`**: it polls, sees `e`, and exits without reading; the
   shell must still get the `e` (`cho kq` typed next runs `echo kq`), since
   poll leaves the key in the kernel's queue.

QEMU's own trace must hold no fault line. About a minute. Run it whenever
libc's poll, <poll.h> or the read of fd 0 changes.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-cpoll.txt")
PROMPT = "# "


def type_raw(guest, data):
    for ch in data:
        guest.proc.stdin.write(bytes([ch]))
        guest.proc.stdin.flush()
        time.sleep(drive_qemu.TYPE_DELAY)


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-cpoll: ")
    seg = {}
    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            start = len(guest.transcript())
            guest.type_line("cpoll timing")
            ok = guest.wait_for(r"timing done[\s\S]*" + PROMPT)
            seg["timing"] = guest.transcript()[start:]
        if ok:
            start = len(guest.transcript())
            guest.type_line("cpoll key")
            ok = guest.wait_for("cpoll: waiting")
            if ok:
                time.sleep(0.5)
                type_raw(guest, b"x")
                ok = guest.wait_for(r"cpoll: key 120")
            if ok:
                time.sleep(1.0)
                type_raw(guest, b"y")
                ok = guest.wait_for("cpoll: type now")
            if ok:
                time.sleep(0.5)
                type_raw(guest, b"z")
                ok = guest.wait_for(PROMPT, timeout=30)
            seg["key"] = guest.transcript()[start:]
        if ok:
            # A key poll saw, and the program exited without reading: it is
            # still the shell's. `e` typed into the poll, the rest of the
            # line after the prompt; `echo kq` runs only if the `e` was kept.
            start = len(guest.transcript())
            guest.type_line("cpoll leave")
            ok = guest.wait_for("cpoll: leave waiting")
            if ok:
                time.sleep(0.5)
                type_raw(guest, b"e")
                ok = guest.wait_for(r"cpoll: leave 1[\s\S]*" + PROMPT, timeout=30)
            if ok:
                guest.type_line("cho kq")
                ok = guest.wait_for(PROMPT)
            seg["leave"] = guest.transcript()[start:]
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    timing = seg.get("timing", "")
    wait = re.search(r"cpoll: wait (\d+) ms=(\d+)", timing)
    sleep = re.search(r"cpoll: sleep (\d+) ms=(\d+)", timing)
    key = seg.get("key", "")
    checks = [
        ("driven to the end", ok),
        ("timeout 0 answers 0 at once", "cpoll: zero 0" in timing),
        ("timeout 200 answers 0 after 200-260 ms (to a tick, settled 2026-10-08)",
         wait is not None and wait.group(1) == "0" and 200 <= int(wait.group(2)) <= 260),
        ("fd 1 is ready for POLLOUT", "cpoll: out 1 revents=4" in timing),
        ("a closed fd is POLLNVAL", "cpoll: closed 1 revents=32" in timing),
        ("with fd 1 ready, fd 0 is answered too, without waiting",
         re.search(r"cpoll: both 1 key=0 out=4 ms=(\d+)", timing) is not None
         and int(re.search(r"cpoll: both 1 key=0 out=4 ms=(\d+)", timing).group(1)) < 50),
        ("no fds, timeout 100: a sleep of 100-160 ms",
         sleep is not None and sleep.group(1) == "0" and 100 <= int(sleep.group(2)) <= 160),
        ("a null array is EFAULT", "cpoll: null EFAULT" in timing),
        ("fd 1 flushed before the wait (`cpoll: waiting` shown)", "cpoll: waiting" in key),
        ("a key ends the -1 wait, and read returns it", "cpoll: key 120" in key),
        ("a key typed into the 3000 ms wait is read in time", "cpoll: key 121 in time" in key),
        ("a key typed before a poll of fds 0 and 1 is answered POLLIN beside fd 1",
         "cpoll: both-typed 2 key=1 out=4" in key),
        ("a key poll saw, left unread at exit, reaches the shell (`kq`)",
         "kq" in [l.strip() for l in seg.get("leave", "").splitlines()]),
        ("no fault lines", faults == 0),
    ]
    if wait or sleep:
        print(f"     wait: {wait.group(0) if wait else '-'}; sleep: {sleep.group(0) if sleep else '-'}")
    failed = 0
    for name, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        failed += 0 if good else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

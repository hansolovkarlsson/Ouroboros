#!/usr/bin/env python3
"""The kernel's keyboard queue: keys typed while a program is busy are kept.

    python3 scripts/test-kbd-queue.py   (or `make test-kbd-queue`, which builds the image)

Step 0 of docs/roadmap/roadmap-ctrl-c.md. The tick reads the keyboard for a
RUNNING foreground program (one not blocked in a read), so that Ctrl+C ends a
runaway loop; until 2026-10-07 it threw away every other byte it read. Now it
keeps them in a 64-byte queue (kernel/src/syscall.rs, read_keyboard_ahead) for
the program's next read. `/bin/READKEY spin` runs three seconds without
reading, then prints every byte waiting. Eight runs in one boot, with QEMU's
USB keyboard pressed through the monitor's `sendkey`:

1. **USB keys during the spin.** `hello`, Up and `x` must come back as all
   nine bytes, the arrow's three among them.
2. **Serial bytes during the spin.** `abc`, typed on the serial line a tenth
   of a second apart, must come back too (the PL011 has no receive FIFO, so a
   byte the tick read was the only copy).
3. **Ctrl+C during the spin still ends it.** The kernel says the foreground
   task was terminated, and READKEY prints no `got` line.
4. **Ctrl+C behind a full queue.** 72 letters, more than the queue holds,
   then Ctrl+C, inside a thirteen-second spin (`readkey spin 650`): it must
   still end the program, so the way out can never be stuck behind
   type-ahead, and none of the letters may reach the shell after it: a
   Ctrl+C flushes what was typed before it.
5. **Keys typed while the keyboard owner is blocked in a message wait**
   (`readkey spin 150 keep | readkey spin 0 msg`: the last stage, which owns
   the keyboard, waits in MSG_RECV until the first writes three seconds
   later): `abc` and Down must all come back. Until the review of #232 that
   wait threw a byte away at every tick. (A program writing to the console
   would not test it: the console server answers within the tick.)
6. **Type-ahead left by a program that exits** (`readkey spin 150 keep`):
   `echo kept` and Enter typed on the serial line during the spin must reach
   the shell whole and run. The shell's wait for its child also threw the
   first byte away until that review.

7. **A full queue keeps keys whole.** 62 letters, then Up, then `b`, during
   a spin: Up's three bytes do not fit in the two places left and the whole
   key is dropped, and `b` is kept; the spin reads the letters and `b`, with
   no `ESC [` cut from Up.
8. **The tail of a key does not outlive its owner.** `readkey one` reads
   only the ESC of Up and exits; `echo tail` typed next must run, with no
   `[A` before it. Steps 7 and 8 are step 3 of the Ctrl-C plan.

QEMU's own trace must hold no fault line. One boot, about two and a half
minutes.
Run it whenever the keyboard path in syscall.rs, the tick's read in tasks.rs,
or Ctrl+C's handling changes.
"""
import importlib.util
import os
import re
import socket
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
# In build/, not a temp directory: macOS caps a socket path at 104 bytes.
MONITOR = os.path.join(ROOT, "build", "test-kbd-queue.monitor")
TRANSCRIPT = os.path.join(ROOT, "build", "test-kbd-queue.txt")
KEY_DELAY = 0.15
# The flood's pace. The driver takes one keyboard report per poll (one
# interrupt buffer, re-armed after each), a key is two reports (press and
# release), and the kernel reads ahead once a tick: some 25 keys a second.
# Faster, QEMU's own keyboard queue overflows and drops keys before the kernel
# sees them, which tests QEMU, not this queue (found 2026-10-07, when the tick
# read only for an owner it had interrupted: 25 ms a key delivered 24 of 72,
# 60 ms delivered 42, and each lost the Ctrl+C). The limit is recorded on
# docs/ROADMAP.md.
FAST_DELAY = 0.12
FLOOD = 72
FULL = 62


def sendkeys(names, delay=KEY_DELAY, hold=None):
    """`hold` is sendkey's hold time in ms (QEMU's default is 100)."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as mon:
        mon.connect(MONITOR)
        for name in names:
            mon.sendall(f"sendkey {name}{'' if hold is None else f' {hold}'}\n".encode())
            time.sleep(delay)


def main() -> int:
    if os.path.exists(MONITOR):
        os.remove(MONITOR)
    guest = drive_qemu.Guest(
        IMAGE, label="test-kbd-queue: ",
        extra_args=[
            "-device", "qemu-xhci,id=xhci0",
            "-device", "usb-kbd,bus=xhci0.0",
            "-monitor", f"unix:{MONITOR},server,nowait",
        ],
    )
    got = {}
    killed = {}
    parts = {}
    try:
        ok = guest.run([("login:", "root"), ("assword", "root"), ("# ", "")])

        def spin(name, during, ticks=None, how=None, cmd=None):
            """Runs `readkey spin`, does `during` while it spins, and records
            the bytes it printed (None if no `got` line) and whether the kernel
            reported a Ctrl+C termination."""
            start = len(guest.transcript())
            line = cmd or ("readkey spin" + (f" {ticks or 150}" if ticks or how else "") + (f" {how}" if how else ""))
            guest.type_line(line)
            if not guest.wait_for("readkey: spinning"):
                return False
            during()
            if not guest.wait_for(r"\n# ", timeout=30):
                return False
            part = guest.transcript()[start:]
            m = re.search(r"readkey: got([ \d]*)", part)
            got[name] = [int(v) for v in m.group(1).split()] if m else None
            killed[name] = "Ctrl+C - foreground task" in part
            parts[name] = part
            return True

        def serial_type(data):
            for ch in data:
                guest.proc.stdin.write(bytes([ch]))
                guest.proc.stdin.flush()
                time.sleep(0.1)

        def serial_abc():
            serial_type(b"abc")

        ok = ok and spin("usb", lambda: sendkeys(["h", "e", "l", "l", "o", "up", "x"]))
        ok = ok and spin("serial", serial_abc)
        ok = ok and spin("ctrl-c", lambda: sendkeys(["ctrl-c"]))
        # The keyboard owner blocked in a message wait for three seconds: a
        # pipeline's last stage waiting in MSG_RECV for the first to write.
        ok = ok and spin("msg", lambda: sendkeys(["a", "b", "c", "down"]), cmd="readkey spin 150 keep | readkey spin 0 msg")
        # Exits without reading: the shell must get every byte, the first too.
        ok = ok and spin("keep", lambda: serial_type(b"echo kept\n"), how="keep")
        # Thirteen seconds, so the whole flood lands inside the spin.
        ok = ok and spin("flood", lambda: (sendkeys(["a"] * FLOOD, FAST_DELAY, hold=10), sendkeys(["ctrl-c"])), ticks=650)
        # A full queue keeps keys whole: 62 letters fill all but two places,
        # Up (three bytes) does not fit and is dropped entirely, and `b`,
        # which does, is kept.
        ok = ok and spin("full", lambda: (sendkeys(["a"] * FULL, FAST_DELAY, hold=10), sendkeys(["up", "b"])), ticks=650)
        if ok:
            # The tail of a key the last owner started never reaches the next:
            # `readkey one` takes the ESC of Up and exits.
            start = len(guest.transcript())
            guest.type_line("readkey one")
            ok = guest.wait_for("waiting for one byte")
            if ok:
                sendkeys(["up"])
                ok = guest.wait_for(r"readkey: read 27[\s\S]*\n# ", timeout=15)
            if ok:
                guest.type_line("echo tail")
                ok = guest.wait_for(r"\n# ", timeout=15)
            parts["one"] = guest.transcript()[start:]
        if ok:
            # What the flood left must not reach the shell: Ctrl+C flushes it.
            # An empty line, then the shell's answer to it, is the check.
            guest.type_line("")
            ok = guest.wait_for(r"\n# ", timeout=15)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    checks = [
        ("driven to the end", ok),
        ("USB keys typed during the spin all kept (hello, Up, x: 9 bytes)",
         got.get("usb") == list(b"hello\x1b[Ax")),
        ("serial bytes typed during the spin kept (abc)", got.get("serial") == list(b"abc")),
        ("Ctrl+C during the spin still ends it", killed.get("ctrl-c") is True and got.get("ctrl-c") is None),
        (f"Ctrl+C behind {FLOOD} queued letters still ends it",
         killed.get("flood") is True and got.get("flood") is None),
        ("Ctrl+C flushes what was queued: none of the flood reaches the shell",
         "aaa" not in out[out.find("readkey spin 650"):].split("terminated", 1)[-1]),
        ("keys typed while the owner is blocked in a message wait kept (abc, Down: 6 bytes)", got.get("msg") == list(b"abc\x1b[B")),
        ("type-ahead left by a program that exits reaches the shell whole (`echo kept` runs)",
         "keep" in parts and re.search(r"^kept\r?$", parts["keep"], re.M) is not None),
        ("a full queue drops a whole key (62 letters, Up, b: the letters and b, nothing of Up)",
         got.get("full") == [97] * FULL + [98]),
        ("the tail of a key the last owner started never reaches the shell (`echo tail` runs)",
         "one" in parts and re.search(r"^tail\r?$", parts["one"], re.M) is not None and "[A" not in parts["one"]),
        ("no fault lines", faults == 0),
    ]
    for name in ("usb", "serial"):
        print(f"     {name}: {got.get(name)}")
    failed = 0
    for name, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        failed += 0 if good else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

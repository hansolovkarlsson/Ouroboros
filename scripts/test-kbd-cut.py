#!/usr/bin/env python3
"""Cut keys in the kernel's keyboard queue: step 3 of the Ctrl-C plan.

    python3 scripts/test-kbd-cut.py    (or `make test-kbd-cut`, which builds the image)

docs/roadmap/roadmap-ctrl-c.md, "Step 3's second half": every byte reaches a
reader through one queue whose parser knows where each key starts. One boot,
with QEMU's USB keyboard (monitor sendkey) and the serial line; after each
case the shell must run a line typed next, whole:

1. **A cut key is not handed on** (E3). `readkey one` reads one byte and
   exits; Up on the USB keyboard gives it `ESC`, and the `[A` behind it must
   not reach the shell: `echo k1` typed next prints `k1`.
2. **A bare Escape ends after a quiet moment** (E2). `readkey one` reads a
   bare `ESC` from the serial line and exits; half a second later `echo k2`
   prints `k2` (without the interval, the `e` was dropped as the Escape's
   rest).
3. **The kill's flush eats nothing after it** (E1). `readkey spin 100 keep`
   runs without reading; `ESC` and then Ctrl+C on the serial line end it, and
   `echo k3` typed next prints `k3`.
4. **TCSAFLUSH discards keys typed ahead.** `ctermios flush` spins raw while
   `echo bad` is typed, then restores with TCSAFLUSH: `echo k4` typed next
   prints `k4`, not `badecho k4`.
5. **TCSADRAIN keeps them.** `ctermios drain`, `echo k5` typed during its
   spin, then Enter: `k5`.

Each fails with its part of the kernel removed. That a USB key is read whole
(no serial byte inside it) has no check here: it needs a serial byte to
arrive between two reads of one USB report. QEMU's own trace must hold no
fault line. One boot, about a minute and a half. Run it whenever the
keyboard queue in syscall.rs, a change of keyboard owner in tasks.rs or
KBD_MODE's flush changes.
"""
import importlib.util
import os
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
MONITOR = os.path.join(ROOT, "build", "test-kbd-cut.monitor")
TRANSCRIPT = os.path.join(ROOT, "build", "test-kbd-cut.txt")
KEY_DELAY = 0.3
PROMPT = "# "


def sendkeys(names):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as mon:
        mon.connect(MONITOR)
        for name in names:
            mon.sendall(f"sendkey {name}\n".encode())
            time.sleep(KEY_DELAY)


def type_raw(guest, data):
    """Bytes on the serial line, paced like Guest.type_line, no Enter added."""
    for ch in data:
        guest.proc.stdin.write(bytes([ch]))
        guest.proc.stdin.flush()
        time.sleep(drive_qemu.TYPE_DELAY)


def main() -> int:
    if os.path.exists(MONITOR):
        os.remove(MONITOR)
    guest = drive_qemu.Guest(
        IMAGE, label="test-kbd-cut: ",
        extra_args=[
            "-device", "qemu-xhci,id=xhci0",
            "-device", "usb-kbd,bus=xhci0.0",
            "-monitor", f"unix:{MONITOR},server,nowait",
        ],
    )
    seg = {}

    def then_line(name, line):
        """Type `line` at the prompt and keep what it printed."""
        start = len(guest.transcript())
        guest.type_line(line)
        ok = guest.wait_for(PROMPT)
        seg[name] = guest.transcript()[start:]
        return ok

    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        # 1. A cut key is not handed on.
        if ok:
            guest.type_line("readkey one")
            ok = guest.wait_for(r"readkey: one\?")
            if ok:
                sendkeys(["up"])
                ok = guest.wait_for(r"readkey: one 27[\s\S]*" + PROMPT)
            ok = ok and then_line("cut", "echo k1")
        # 2. A bare Escape ends after a quiet moment.
        if ok:
            guest.type_line("readkey one")
            ok = guest.wait_for(r"readkey: one\?")
            if ok:
                type_raw(guest, b"\x1b")
                ok = guest.wait_for(r"readkey: one 27[\s\S]*" + PROMPT)
            if ok:
                time.sleep(0.5)
                ok = then_line("esc", "echo k2")
        # 3. The kill's flush eats nothing after it.
        if ok:
            guest.type_line("readkey spin 100 keep")
            ok = guest.wait_for("readkey: spinning")
            if ok:
                type_raw(guest, b"\x1b")
                time.sleep(0.3)
                type_raw(guest, b"\x03")
                ok = guest.wait_for(r"Ctrl\+C - foreground task[\s\S]*" + PROMPT, timeout=30)
            ok = ok and then_line("kill", "echo k3")
        # 4. TCSAFLUSH discards keys typed ahead.
        if ok:
            guest.type_line("ctermios flush")
            ok = guest.wait_for("ctermios: spinning")
            if ok:
                type_raw(guest, b"echo bad")
                ok = guest.wait_for(r"ctermios: flushed[\s\S]*" + PROMPT, timeout=30)
            ok = ok and then_line("flush", "echo k4")
        # 5. TCSADRAIN keeps them.
        if ok:
            guest.type_line("ctermios drain")
            ok = guest.wait_for("ctermios: spinning")
            if ok:
                type_raw(guest, b"echo k5")
                ok = guest.wait_for(r"ctermios: drained[\s\S]*" + PROMPT, timeout=30)
            ok = ok and then_line("drain", "")
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    def printed(name, word):
        return word in [line.strip() for line in seg.get(name, "").splitlines()]

    checks = [
        ("driven to the end", ok),
        ("a cut USB key's rest is not handed to the shell (`k1`)", printed("cut", "k1")),
        ("a letter typed after a bare Escape is kept (`k2`)", printed("esc", "k2")),
        ("nothing typed after a kill is eaten (`k3`)", printed("kill", "k3")),
        ("TCSAFLUSH discards keys typed ahead (`k4`, no `bad`)",
         printed("flush", "k4") and "bad" not in seg.get("flush", "")),
        ("TCSADRAIN keeps them (`k5`)", printed("drain", "k5")),
        ("no fault lines", faults == 0),
    ]
    failed = 0
    for name, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        failed += 0 if good else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""The navigation and function keys, on QEMU's USB keyboard.

    python3 scripts/test-nav-keys.py    (or `make test-nav-keys`, which builds the image)

Item 2 of docs/handoffs/2026-10-05-from-edit-editor-console.md: the USB
keyboard sends the VT100/xterm sequences for Up, Down, Right, Left, Home, End,
Page Up, Page Down, Delete, F2 and F3 (kernel/src/xhci.rs, keycode_to_bytes),
which it dropped before. Three checks in one boot:

1. **The bytes.** `/bin/READKEY` prints every byte it reads with its value.
   The eleven keys are pressed through the QEMU monitor's `sendkey`, which
   reaches the guest ONLY as a USB keyboard report, then `q`; the values
   printed must be the eleven sequences, in order, and nothing else.
2. **The shell's line editor swallows them** (programs/shell/src/keyseq.rs).
   `echo a` typed on the serial line, then Up, Left, Delete, Home, End and F2
   on the USB keyboard, then `b` and Enter: the shell must print `ab`, where
   without the filter it printed the sequences' tails as text.
3. **Login and the serial path.** The user name is typed on the serial line as
   `ro`, `ESC [ D`, `ot`, so login succeeds only if its reader swallows the
   sequence; and `echo c`, `ESC [ A`, `ESC [ 5 ~`, `d` must print `cd`.

QEMU's own trace must hold no fault line. One boot, about a minute. Run it
whenever xhci.rs's key mapping, the shell's line editor or login's reader
changes.
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
MONITOR = os.path.join(ROOT, "build", "test-nav-keys.monitor")
TRANSCRIPT = os.path.join(ROOT, "build", "test-nav-keys.txt")
KEY_DELAY = 0.3

# QEMU sendkey name -> the bytes the key must send.
KEYS = [
    ("up", b"\x1b[A"), ("down", b"\x1b[B"), ("right", b"\x1b[C"), ("left", b"\x1b[D"),
    ("home", b"\x1b[H"), ("end", b"\x1b[F"), ("pgup", b"\x1b[5~"), ("pgdn", b"\x1b[6~"),
    ("delete", b"\x1b[3~"), ("f2", b"\x1bOQ"), ("f3", b"\x1bOR"),
]
IN_LINE = ["up", "left", "delete", "home", "end", "f2"]


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
        IMAGE, label="test-nav-keys: ",
        extra_args=[
            "-device", "qemu-xhci,id=xhci0",
            "-device", "usb-kbd,bus=xhci0.0",
            "-monitor", f"unix:{MONITOR},server,nowait",
        ],
    )
    steps_done = []
    try:
        ok = guest.wait_for("login:", timeout=120)
        if ok:
            type_raw(guest, b"ro\x1b[Dot\n")
            ok = guest.wait_for("assword")
        if ok:
            guest.type_line("root")
            ok = guest.wait_for("# ")
            if ok:
                steps_done.append("login")
        if ok:
            guest.type_line("readkey")
            ok = guest.wait_for("press keys")
        if ok:
            sendkeys([name for name, _ in KEYS] + ["q"])
            # The farewell and the prompt after it in one match: a wait for
            # the farewell alone marks the prompt as seen too, and a second
            # wait for it then never matches.
            ok = guest.wait_for(r"readkey: bye[\s\S]*# ", timeout=30)
            if ok:
                steps_done.append("readkey")
        if ok:
            type_raw(guest, b"echo a")
            time.sleep(drive_qemu.SETTLE)
            sendkeys(IN_LINE)
            guest.type_line("b")
            ok = guest.wait_for("# ")
            if ok:
                steps_done.append("usb line")
        if ok:
            type_raw(guest, b"echo c\x1b[A\x1b[5~d\n")
            ok = guest.wait_for("# ")
            if ok:
                steps_done.append("serial line")
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    session = out[out.find("readkey"):] if "readkey" in out else ""
    readkey_part = session[:session.find("readkey: bye")] if "readkey: bye" in session else ""
    got = [int(v) for v in re.findall(r"key: .  \((\d+)\)", readkey_part)]
    want = list(b"".join(seq for _, seq in KEYS))
    lines = [l.strip() for l in out.splitlines()]
    checks = [
        ("driven to the end", len(steps_done) == 4),
        ("login took `ro ESC[D ot` as root", "login" in steps_done and "Login incorrect" not in out),
        (f"readkey read the eleven sequences ({len(want)} bytes)", got == want),
        ("the shell printed `ab` (USB keys in the line)", "ab" in lines),
        ("the shell printed `cd` (serial sequences in the line)", "cd" in lines),
        ("no fault lines", faults == 0),
    ]
    if got != want:
        print(f"     readkey bytes: {got}")
        print(f"     expected:      {want}")
    failed = 0
    for name, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        failed += 0 if good else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

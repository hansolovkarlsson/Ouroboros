#!/usr/bin/env python3
"""The navigation and function keys, on QEMU's USB keyboard.

    python3 scripts/test-nav-keys.py    (or `make test-nav-keys`, which builds the image)

Item 2 of docs/handoffs/closed/2026-10-05-from-edit-editor-console.md: the USB
keyboard sends the VT100/xterm sequences for Up, Down, Right, Left, Home, End,
Page Up, Page Down, Delete, F2 and F3 (kernel/src/xhci.rs, keycode_to_bytes),
which it dropped before, and Tab (keycode_to_ascii, unmapped until
2026-10-09). Six checks in one boot:

1. **The bytes.** `/bin/READKEY` prints every byte it reads with its value.
   The eleven keys are pressed through the QEMU monitor's `sendkey`, which
   reaches the guest ONLY as a USB keyboard report, then Tab, then `q`; the
   values printed must be the eleven sequences and a 9, in order, and nothing
   else.
2. **The shell's line editor swallows them** (the `keyseq` crate). `echo a`
   typed on the serial line, then Up, Left, `x`, Delete, Home, End and F2 on
   the USB keyboard, then `b` and Enter: the shell must print `axb`. The `x`
   is what proves the USB keys reached the shell at all; without the filter
   it printed the sequences' tails as text.
3. **Login and the serial path.** The user name is typed on the serial line as
   `ro`, `ESC [ D`, `ot`, so login succeeds only if its reader swallows the
   sequence; and `echo c`, `ESC [ A`, `ESC [ 5 ~`, `d` must print `cd`.
4. **`more` takes a sequence as one key** (`ulib::read_key`): one Down at
   `--More--` shows one more screen, where byte by byte it showed three.
5. **Tab completes on the USB keyboard**: `echo /include/el` typed on the
   serial line, Tab on the USB keyboard, Enter: the shell must print
   `/include/elf.h` (the only name there starting `el`), where without the
   mapping it printed `/include/el`.
6. **`ulib::read_line` agrees with login**: `useradd navu` with the password
   typed as `pa`, `ESC [ D`, `ss`, then a login as navu typing `pass`, which
   succeeds only if both readers drop the sequence. The run boots a copy of
   the image, so the user never reaches `build/esp.img`.

QEMU's own trace must hold no fault line. One boot, about a minute. Run it
whenever xhci.rs's key mapping, the shell's line editor or login's reader
changes.
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
# A USB letter among them, so `axb` proves the USB keys reached the shell
# (an `ab` alone would also pass if none of them had).
IN_LINE = ["up", "left", "x", "delete", "home", "end", "f2"]
# Tab, read by readkey after the eleven keys, and pressed in the line.
TAB = ("tab", b"\t")
COPY = os.path.join(ROOT, "build", "test-nav-keys.img")


def main() -> int:
    if os.path.exists(MONITOR):
        os.remove(MONITOR)
    # A copy: the run adds a user, which must not reach the image other
    # rigs boot.
    shutil.copy(IMAGE, COPY)
    guest = drive_qemu.Guest(
        COPY, label="test-nav-keys: ",
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
            guest.type_raw(b"ro\x1b[Dot\n")
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
            drive_qemu.sendkeys(MONITOR, [name for name, _ in KEYS] + [TAB[0], "q"], KEY_DELAY)
            # The farewell and the prompt after it in one match: a wait for
            # the farewell alone marks the prompt as seen too, and a second
            # wait for it then never matches.
            ok = guest.wait_for(r"readkey: bye[\s\S]*# ", timeout=30)
            if ok:
                steps_done.append("readkey")
        if ok:
            guest.type_raw(b"echo a")
            time.sleep(drive_qemu.SETTLE)
            drive_qemu.sendkeys(MONITOR, IN_LINE, KEY_DELAY)
            guest.type_line("b")
            ok = guest.wait_for("# ")
            if ok:
                steps_done.append("usb line")
        if ok:
            guest.type_raw(b"echo /include/el")
            time.sleep(drive_qemu.SETTLE)
            drive_qemu.sendkeys(MONITOR, [TAB[0]], KEY_DELAY)
            time.sleep(drive_qemu.SETTLE)
            guest.type_line("")
            ok = guest.wait_for("# ")
            if ok:
                steps_done.append("usb tab")
        if ok:
            guest.type_raw(b"echo c\x1b[A\x1b[5~d\n")
            ok = guest.wait_for("# ")
            if ok:
                steps_done.append("serial line")
        if ok:
            # One Down at the pager is one key: one more screen, then q.
            guest.type_line("more /include/elf.h")
            ok = guest.wait_for("--More--")
            if ok:
                drive_qemu.sendkeys(MONITOR, ["down"], KEY_DELAY)
                ok = guest.wait_for("--More--")
            if ok:
                time.sleep(1.5)  # time for any further screens to show
                drive_qemu.sendkeys(MONITOR, ["q"], KEY_DELAY)
                ok = guest.wait_for("# ")
            if ok:
                steps_done.append("more")
        if ok:
            # A password set through ulib::read_line with an arrow typed in
            # it, then typed plain at login: the two readers must agree.
            guest.type_line("useradd navu")
            ok = guest.wait_for("assword")
            if ok:
                guest.type_raw(b"pa\x1b[Dss\n")
                ok = guest.wait_for("assword")
            if ok:
                guest.type_raw(b"pa\x1b[Dss\n")
                ok = guest.wait_for("# ")
            if ok:
                guest.type_line("logout")
                ok = guest.wait_for("login:")
            if ok:
                guest.type_line("navu")
                ok = guest.wait_for("assword")
            if ok:
                guest.type_line("pass")
                ok = guest.wait_for(r"\$ |Login incorrect")
            if ok:
                steps_done.append("useradd")
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    session = out[out.find("readkey"):] if "readkey" in out else ""
    readkey_part = session[:session.find("readkey: bye")] if "readkey: bye" in session else ""
    got = [int(v) for v in re.findall(r"key: .  \((\d+)\)", readkey_part)]
    want = list(b"".join(seq for _, seq in KEYS) + TAB[1])
    lines = [l.strip() for l in out.splitlines()]
    # The first login only: the useradd step logs in again later.
    first_login = out[:out.find("readkey")] if "readkey" in out else out
    more_part = out[out.find("more /include/elf.h"):] if "more /include/elf.h" in out else ""
    more_part = more_part[:more_part.find("useradd navu")] if "useradd navu" in more_part else more_part
    after_useradd = out[out.find("useradd navu"):] if "useradd navu" in out else ""
    checks = [
        ("driven to the end", len(steps_done) == 7),
        ("login took `ro ESC[D ot` as root", "login" in steps_done and "Login incorrect" not in first_login),
        (f"readkey read the eleven sequences and Tab ({len(want)} bytes)", got == want),
        ("the shell printed `axb` (USB keys in the line)", "axb" in lines),
        ("Tab on the USB keyboard completed `/include/el` to `/include/elf.h`", "/include/elf.h" in lines),
        ("the shell printed `cd` (serial sequences in the line)", "cd" in lines),
        ("one Down at `more` is one screen (two prompts)", more_part.count("--More--") == 2),
        ("a password typed with ESC [ D at useradd logs in typed plain",
         "useradd" in steps_done and "Login incorrect" not in after_useradd and "$ " in after_useradd),
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

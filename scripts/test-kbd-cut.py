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
   bare `ESC` from the serial line and exits; half a second later `Ok2` must
   reach the shell whole (`unknown command: Ok2`). Without the interval the
   `O` and the `k` were dropped as an SS3 sequence's rest. The line starts
   with `O` because since 2026-10-09 `keyseq` keeps any other letter after a
   bare ESC whatever the interval.
3. **The kill's flush eats nothing after it** (E1). `readkey spin 100 keep`
   runs without reading; `ESC` and then Ctrl+C on the serial line end it, and
   `Ok3` typed next reaches the shell whole (`unknown command: Ok3`); an `O`
   for the reason in check 2.
4. **TCSAFLUSH discards keys typed ahead.** `ctermios flush` spins raw while
   `echo bad` is typed, then restores with TCSAFLUSH: `echo k4` typed next
   prints `k4`, not `badecho k4`.
5. **TCSADRAIN keeps them.** `ctermios drain`, `echo k5` typed during its
   spin, then Enter: `k5`.
6. **A USB Escape is a whole key at once.** `readkey one` reads the Escape
   key on the USB keyboard and exits; `O` (Shift+O), pressed on the USB
   keyboard 0.3 s after it, well inside the second a USB key may stay open,
   must reach the shell: `k6` typed next on the serial line gives `unknown
   command: Ok6`. Without the driver marking the key's last byte, the `O`
   was trimmed as the start of an SS3 sequence, and the `k` with it.

Each fails with its part of the kernel removed, except E1's rule alone in
check 3: the 0.3 s before the Ctrl+C is past E2's interval, so E2 already
closed the ESC, and check 3 fails only with E1 and E2 removed together. E1 is
checked on the host, where bytes can be pushed at chosen ticks
(`keyseq::KeyQueue`'s tests, in `make test`). That a USB key is read whole
(no serial byte inside it) has no check here: it needs a serial byte to
arrive between two reads of one USB report. QEMU's own trace must hold no
fault line. One boot, about a minute and a half. Run it whenever the
keyboard queue in syscall.rs, a change of keyboard owner in tasks.rs or
KBD_MODE's flush changes.
"""
import importlib.util
import os
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
                drive_qemu.sendkeys(MONITOR, ["up"], KEY_DELAY)
                ok = guest.wait_for(r"readkey: one 27[\s\S]*" + PROMPT)
            ok = ok and then_line("cut", "echo k1")
        # 2. A bare Escape ends after a quiet moment.
        if ok:
            guest.type_line("readkey one")
            ok = guest.wait_for(r"readkey: one\?")
            if ok:
                guest.type_raw(b"\x1b")
                ok = guest.wait_for(r"readkey: one 27[\s\S]*" + PROMPT)
            if ok:
                time.sleep(0.5)
                ok = then_line("esc", "Ok2")
        # 3. The kill's flush eats nothing after it.
        if ok:
            guest.type_line("readkey spin 100 keep")
            ok = guest.wait_for("readkey: spinning")
            if ok:
                guest.type_raw(b"\x1b")
                time.sleep(0.3)
                guest.type_raw(b"\x03")
                ok = guest.wait_for(r"Ctrl\+C - foreground task[\s\S]*" + PROMPT, timeout=30)
            ok = ok and then_line("kill", "Ok3")
        # 4. TCSAFLUSH discards keys typed ahead.
        if ok:
            guest.type_line("ctermios flush")
            ok = guest.wait_for("ctermios: spinning")
            if ok:
                guest.type_raw(b"echo bad")
                ok = guest.wait_for(r"ctermios: flushed[\s\S]*" + PROMPT, timeout=30)
            ok = ok and then_line("flush", "echo k4")
        # 5. TCSADRAIN keeps them.
        if ok:
            guest.type_line("ctermios drain")
            ok = guest.wait_for("ctermios: spinning")
            if ok:
                guest.type_raw(b"echo k5")
                ok = guest.wait_for(r"ctermios: drained[\s\S]*" + PROMPT, timeout=30)
            ok = ok and then_line("drain", "")
        # 6. A USB Escape is a whole key at once.
        if ok:
            guest.type_line("readkey one")
            ok = guest.wait_for(r"readkey: one\?")
            if ok:
                drive_qemu.sendkeys(MONITOR, ["esc", "shift-o"], KEY_DELAY)
                ok = guest.wait_for(r"readkey: one 27[\s\S]*" + PROMPT)
            if ok:
                time.sleep(0.5)
                ok = then_line("usbesc", "k6")
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
        ("an `O` typed after a bare Escape is kept (`Ok2`)", "unknown command: Ok2" in seg.get("esc", "")),
        ("nothing typed after a kill is eaten (`Ok3`)", "unknown command: Ok3" in seg.get("kill", "")),
        ("TCSAFLUSH discards keys typed ahead (`k4`, no `bad`)",
         printed("flush", "k4") and "bad" not in seg.get("flush", "")),
        ("TCSADRAIN keeps them (`k5`)", printed("drain", "k5")),
        ("an `O` typed after a USB Escape is kept (`Ok6`)", "unknown command: Ok6" in seg.get("usbesc", "")),
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

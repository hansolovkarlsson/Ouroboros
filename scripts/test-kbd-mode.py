#!/usr/bin/env python3
"""The keyboard modes: Ctrl+C as a key in raw mode, Ctrl+\\ as the way out.

    python3 scripts/test-kbd-mode.py    (or `make test-kbd-mode`, which builds the image)

Step 1 of docs/roadmap/roadmap-ctrl-c.md: `KBD_MODE` (syscall 70) puts a task
in raw mode, where Ctrl+C (0x03) reaches it as a byte, and Ctrl+\\ (0x1c) ends
a foreground program in every mode. `/bin/READKEY raw` sets raw and echoes
each key with its value; `/bin/READKEY mode` prints the mode it starts in.
One boot, each key typed on the serial line and, where it matters, pressed
on QEMU's USB keyboard through the monitor's `sendkey`:

1. **Raw: Ctrl+C is a key.** `readkey raw` prints `(3)` for Ctrl+C on the
   serial line and again for Ctrl+C on the USB keyboard, then Ctrl+\\ on the
   serial line ends it: no `readkey: bye`, no `(28)`.
2. **Raw: Ctrl+\\ on the USB keyboard ends it** (xhci.rs's Ctrl+\\ mapping).
3. **The mode ends with the task.** `readkey mode` after those two prints
   `readkey: mode cooked`; it runs in the slot they ran in.
4. **Cooked: Ctrl+C still ends it**, and so does Ctrl+\\: two plain
   `readkey`s, neither printing `(3)` or `(28)`.
5. **The boot shell ignores both bytes**: `echo e`, Ctrl+\\, Ctrl+C, `f`
   typed on one line prints `ef`.
6. **Ctrl+\\ interrupts the boot shell's stuck `wait`**: `exec /bin/recv`, which
   blocks for good, then `wait` on it and Ctrl+\\; the wait must print
   `wait: interrupted`.

Every kill must also name the key pressed in the kernel's kill line, and
step 2 is repeated with Ctrl on the ISO key beside left Shift (`less`, HID
0x64). Step 3 checks that `readkey mode` ran in a slot a raw `readkey` ran
in, since otherwise a cooked answer proves nothing.

Each check fails with its part of the kernel removed: the raw test in
`interrupt_key_check` (1), the 0x1c arm there (1, 2 and 4), the USB mapping
(2), the reset in `end_task` (3), Ctrl+\\ in `keyboard_interrupts_wait` (6).
QEMU's own trace must hold no fault line.
About a minute. Run it whenever `interrupt_key_check`, `KBD_MODE`, xhci.rs's
Ctrl mapping or the death path's per-task resets change.
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
MONITOR = os.path.join(ROOT, "build", "test-kbd-mode.monitor")
TRANSCRIPT = os.path.join(ROOT, "build", "test-kbd-mode.txt")
KEY_DELAY = 0.3
ETX, FS = b"\x03", b"\x1c"
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
        IMAGE, label="test-kbd-mode: ",
        extra_args=[
            "-device", "qemu-xhci,id=xhci0",
            "-device", "usb-kbd,bus=xhci0.0",
            "-monitor", f"unix:{MONITOR},server,nowait",
        ],
    )
    # Each step's slice of the transcript, from its command to its prompt.
    seg = {}

    def step(name, command, banner, keys):
        """Run `command`, wait for `banner`, do `keys()`, wait for the prompt.
        With no keys the program exits by itself, so the banner and the
        prompt are matched together: a match marks everything before it
        seen, and a second wait for the prompt would never match."""
        start = len(guest.transcript())
        guest.type_line(command)
        if keys is None:
            ok = guest.wait_for(banner + r"[\s\S]*" + PROMPT)
        else:
            if not guest.wait_for(banner):
                return False
            keys()
            ok = guest.wait_for(PROMPT, timeout=30)
        seg[name] = guest.transcript()[start:]
        return ok

    def serial_then_usb_ctrl_c_then_serial_fs():
        type_raw(guest, ETX)
        time.sleep(KEY_DELAY)
        sendkeys(["ctrl-c"])
        time.sleep(KEY_DELAY)
        type_raw(guest, FS)

    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        ok = ok and step("raw", "readkey raw", "readkey: raw", serial_then_usb_ctrl_c_then_serial_fs)
        ok = ok and step("raw usb", "readkey raw", "readkey: raw", lambda: sendkeys(["ctrl-backslash"]))
        # QEMU's `less` is the ISO key beside left Shift (HID 0x64). The other
        # ISO backslash position (0x32) has no sendkey of its own: QEMU sends
        # 0x31 for it, so it is not reachable from here.
        ok = ok and step("raw iso", "readkey raw", "readkey: raw", lambda: sendkeys(["ctrl-less"]))
        ok = ok and step("mode", "readkey mode", "readkey: mode", None)
        ok = ok and step("cooked c", "readkey", "press keys", lambda: type_raw(guest, ETX))
        ok = ok and step("cooked fs", "readkey", "press keys", lambda: type_raw(guest, FS))
        if ok:
            start = len(guest.transcript())
            type_raw(guest, b"echo e" + FS + ETX + b"f\n")
            ok = guest.wait_for(PROMPT)
            seg["shell"] = guest.transcript()[start:]
        if ok:
            # The boot shell's stuck `wait`: `recv` blocks for good, and
            # Ctrl+\ must interrupt the wait as Ctrl+C does
            # (keyboard_interrupts_wait), the target left running.
            guest.type_line("exec /bin/recv")
            ok = guest.wait_for(PROMPT)
            start = len(guest.transcript())
            guest.type_line("ps")
            ok = ok and guest.wait_for(r"recv[\s\S]*" + PROMPT)
            found = re.search(r"task (\d+): blocked \(waiting\)\s+\S*recv",
                              guest.transcript()[start:], re.IGNORECASE)
            ok = ok and found is not None
            if ok:
                recv_slot = found.group(1)
                start = len(guest.transcript())
                guest.type_line(f"wait {recv_slot}")
                time.sleep(1.5)
                type_raw(guest, FS)
                ok = guest.wait_for(r"wait: interrupted[\s\S]*" + PROMPT, timeout=20)
                seg["wait"] = guest.transcript()[start:]
                guest.type_line(f"kill {recv_slot}")
                ok = guest.wait_for(PROMPT) and ok
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    def ended(name, key):
        """The step's readkey ended without its own farewell, killed by the
        kernel, whose kill line names `key`, the key that was pressed."""
        s = seg.get(name, "")
        return (bool(s) and "readkey: bye" not in s and PROMPT in s
                and f"{key} - foreground task" in s)

    raw_slots = set(re.findall(r"readkey: raw in slot (\d+)", out))
    mode = re.search(r"readkey: mode (\w+) in slot (\d+)", seg.get("mode", ""))

    raw = seg.get("raw", "")
    checks = [
        ("driven to the end", ok),
        ("raw: Ctrl+C read as 3, serial and USB", raw.count("(3)") == 2),
        ("raw: serial Ctrl+\\ ended it, never read", ended("raw", "Ctrl+\\") and "(28)" not in raw),
        ("raw: USB Ctrl+\\ ended it, never read",
         ended("raw usb", "Ctrl+\\") and "(28)" not in seg.get("raw usb", "")),
        ("raw: Ctrl on the ISO key (HID 0x64) ended it", ended("raw iso", "Ctrl+\\")),
        ("the next task in a raw task's slot starts cooked",
         mode is not None and mode.group(1) == "cooked" and mode.group(2) in raw_slots),
        ("cooked: Ctrl+C ended it, never read",
         ended("cooked c", "Ctrl+C") and "(3)" not in seg.get("cooked c", "")),
        ("cooked: Ctrl+\\ ended it, never read",
         ended("cooked fs", "Ctrl+\\") and "(28)" not in seg.get("cooked fs", "")),
        ("Ctrl+\\ interrupted the boot shell's stuck wait", "wait: interrupted" in seg.get("wait", "")),
        ("the boot shell ignored both bytes (`ef`)",
         "ef" in [l.strip() for l in seg.get("shell", "").splitlines()]),
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

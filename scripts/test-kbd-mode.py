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
   blocks for good, then `wait` on it, 70 letters (the wait reads them into
   the 64-byte keyboard queue, which fills) and Ctrl+\\; the wait must print
   `wait: interrupted`.

Every kill must also name the key pressed in the kernel's kill line. Ctrl
on the ISO key beside left Shift (`less`, HID 0x64) must NOT end a raw
program: that key has no US position, and an uncatchable kill on Ctrl+<
would be a trap. `readkey spin 100 raw`, busy in raw mode when Ctrl+C is
typed, must read it back as 3: the tick read it ahead and queued it as a
byte. `readkey spin 100 rawkeep` exits in raw mode with a
Ctrl+C typed during its spin still queued: the shell's wait on it must
collect its exit (no zombie in `ps`), since that Ctrl+C was a byte when it
was read and must not interrupt the shell; `echo qk` typed behind the Ctrl+C
must then run as the shell's next line, which proves the queue held both.
A second stuck `wait` takes an Esc and then Ctrl+\\; `echo zq`, typed while
`readkey spin 100 keep` is busy afterwards, must reach the shell whole: the
key parser saw the interrupt byte end the Esc, so no `e` is eaten as its
rest. The order of the waits' checks (an ended child, or a reply already
there, before an interrupt) has no check here: it needs a key in the gap
between a program ending and the shell's next poll. Step 3 checks that `readkey
mode` ran in a slot a raw `readkey` ran in, since otherwise a cooked
answer proves nothing.

Each check fails with its part of the kernel removed: the raw test in
`interrupt_key_check` (1), the 0x1c arm there (1, 2 and 4), the USB mapping
(2), the reset in `end_task` (3), Ctrl+\\ in `keyboard_interrupts_wait` (6),
the boot shell's interrupt decided as it is read rather than found in the
queue (the zombie check; queued, it is lost behind a full queue, check 6),
0x64 in the USB map (the `<>` check), and the parser fed the boot shell's
interrupt byte (the `zq` check). The ISO backslash key, 0x32,
cannot be sent by QEMU, so its mapping is checked only on hardware.
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
        # QEMU's `less` is the ISO key beside left Shift (HID 0x64), which
        # has no US position: Ctrl on it must NOT be the way out, so readkey
        # lives on to read `q`. (The ISO key in the backslash position, 0x32,
        # is Ctrl+\ but has no sendkey: QEMU sends 0x31 for it.)
        ok = ok and step("raw iso", "readkey raw", "readkey: raw", lambda: sendkeys(["ctrl-less", "q"]))
        # A raw owner busy when Ctrl+C is typed: the tick reads it ahead and
        # must queue it as the byte 3, for the program's next read.
        if ok:
            start = len(guest.transcript())
            guest.type_line("readkey spin 100 raw")
            ok = guest.wait_for("readkey: spinning")
            if ok:
                type_raw(guest, ETX)
                ok = guest.wait_for(r"readkey: got[^\n]*\n[\s\S]*" + PROMPT, timeout=30)
            seg["rawspin"] = guest.transcript()[start:]
        # A raw owner's Ctrl+C left queued when it exits is a byte (settled
        # when read), not an interrupt for the shell that gets the keyboard
        # back: the shell's WAIT on the program must collect its exit, so no
        # zombie is left for `ps` to show.
        if ok:
            start = len(guest.transcript())
            guest.type_line("readkey spin 100 rawkeep")
            ok = guest.wait_for("readkey: spinning")
            if ok:
                # `echo qk` behind the Ctrl+C: kept in the queue past the
                # exit, the shell reads it as its next line, which proves the
                # queue held the Ctrl+C too when the shell's wait ran.
                type_raw(guest, ETX + b"echo qk")
                ok = guest.wait_for(r"readkey: kept[\s\S]*" + PROMPT, timeout=30)
            if ok:
                type_raw(guest, b"\n")
                ok = guest.wait_for(PROMPT)
            if ok:
                guest.type_line("ps")
                # Up to the prompt, so every slot `ps` lists is looked at,
                # however many there are.
                ok = guest.wait_for(r"task 0:[\s\S]*" + PROMPT)
            seg["rawkeep"] = guest.transcript()[start:]
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
                # 70 letters first: the wait reads them ahead into the
                # keyboard queue (64 bytes), so the Ctrl+\ arrives with the
                # queue full and must still interrupt.
                type_raw(guest, b"x" * 70)
                type_raw(guest, FS)
                ok = guest.wait_for(r"wait: interrupted[\s\S]*" + PROMPT, timeout=20)
                seg["wait"] = guest.transcript()[start:]
            if ok:
                # An Esc read ahead into the queue, then Ctrl+\: the key
                # parser must see the interrupt byte, or it is left inside
                # the Esc and eats the next byte queued, here the `e` of
                # `echo zq` typed while `readkey spin 100 keep` is busy.
                guest.type_line(f"wait {recv_slot}")
                time.sleep(1.5)
                type_raw(guest, b"\x1b")
                time.sleep(0.5)
                type_raw(guest, FS)
                ok = guest.wait_for(r"wait: interrupted[\s\S]*" + PROMPT, timeout=20)
            if ok:
                start = len(guest.transcript())
                guest.type_line("readkey spin 100 keep")
                ok = guest.wait_for("readkey: spinning")
                if ok:
                    type_raw(guest, b"echo zq")
                    ok = guest.wait_for(r"readkey: kept[\s\S]*" + PROMPT, timeout=30)
                if ok:
                    type_raw(guest, b"\n")
                    ok = guest.wait_for(PROMPT)
                seg["esc"] = guest.transcript()[start:]
            if found is not None:
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
        ("raw: Ctrl on the ISO `<>` key (HID 0x64) did not end it",
         "readkey: bye" in seg.get("raw iso", "") and "foreground task" not in seg.get("raw iso", "")),
        ("raw and busy: a Ctrl+C read ahead by the tick is queued as 3",
         re.search(r"readkey: got 3\s", seg.get("rawspin", "")) is not None),
        ("a raw owner's queued Ctrl+C left no zombie (the shell's wait collected the exit)",
         "task 0:" in seg.get("rawkeep", "") and ": exited" not in seg.get("rawkeep", "")
         and "qk" in [l.strip() for l in seg.get("rawkeep", "").splitlines()]),
        ("the next task in a raw task's slot starts cooked",
         mode is not None and mode.group(1) == "cooked" and mode.group(2) in raw_slots),
        ("cooked: Ctrl+C ended it, never read",
         ended("cooked c", "Ctrl+C") and "(3)" not in seg.get("cooked c", "")),
        ("cooked: Ctrl+\\ ended it, never read",
         ended("cooked fs", "Ctrl+\\") and "(28)" not in seg.get("cooked fs", "")),
        ("Ctrl+\\ interrupted the boot shell's stuck wait, behind a full queue",
         "wait: interrupted" in seg.get("wait", "")),
        ("after Esc then Ctrl+\\ at the wait, the next key queued is not eaten (`zq`)",
         "zq" in [l.strip() for l in seg.get("esc", "").splitlines()]),
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

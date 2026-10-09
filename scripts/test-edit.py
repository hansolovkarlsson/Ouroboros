#!/usr/bin/env python3
"""DevTools's editor on Ouroboros: /bin/edit, built from EDIT_DIR.

    python3 scripts/test-edit.py    (or `make test-edit`, which builds the image)

DevTools's note docs/handoffs/2026-10-08-from-devtools-edit-poll.md, its "Done
when", on the serial console (where the screen's size is unknown and Edit
falls back to 80 by 24). One boot, on a copy of the image, so the file the
run saves never reaches build/esp.img:

1. `edit hello.txt` opens the editor: its status line names the file.
2. `abcdef` typed, `^Q S` to the line's start, then `^Q Q ^G`: WordStar's
   repeat runs "delete the character under the cursor" again and again, on
   Edit's paced timeout (`poll`), until a key stops it; after two seconds
   `^E` (up, which does nothing on the first line) stops it.
3. `hi`, then Left as the serial line sends it (`ESC [ D`, read through
   Edit's 50 ms escape timeout, `poll` again), then `X`.
4. `^K D` saves and exits, and `cat hello.txt` prints `hXi`: the repeat
   ate all of `abcdef` (one delete would leave `bcdef` after it), the
   arrow moved the cursor, and the save wrote the file.

QEMU's own trace must hold no fault line. About a minute. Needs Edit's
sources at EDIT_DIR. Run it whenever libc's poll, termios or read of fd 0
changes, or when DevTools changes Edit's Ouroboros platform file.
"""
import importlib.util
import os
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
COPY = os.path.join(ROOT, "build", "test-edit.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-edit.txt")
PROMPT = "# "


def type_raw(guest, data):
    for ch in data:
        guest.proc.stdin.write(bytes([ch]))
        guest.proc.stdin.flush()
        time.sleep(drive_qemu.TYPE_DELAY)


def ctrl(letter):
    return bytes([ord(letter.upper()) - 64])


def main() -> int:
    shutil.copy(IMAGE, COPY)
    guest = drive_qemu.Guest(COPY, label="test-edit: ")
    seg = {}
    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            start = len(guest.transcript())
            guest.type_line("edit hello.txt")
            # The status line: the name and Edit's `Line 1  Col 1`, which no
            # error naming the file would print.
            ok = guest.wait_for(r"hello\.txt[\s\S]*Line 1  Col 1", timeout=30)
            seg["open"] = guest.transcript()[start:]
        if ok:
            time.sleep(1.0)
            type_raw(guest, b"abcdef")
            time.sleep(0.3)
            type_raw(guest, ctrl("q") + b"s")
            time.sleep(0.3)
            type_raw(guest, ctrl("q") + b"q" + ctrl("g"))
            time.sleep(2.0)
            type_raw(guest, ctrl("e"))
            time.sleep(0.5)
            type_raw(guest, b"hi")
            type_raw(guest, b"\x1b[D")
            time.sleep(0.3)
            type_raw(guest, b"X")
            time.sleep(0.3)
            type_raw(guest, ctrl("k") + b"d")
            ok = guest.wait_for(PROMPT, timeout=30)
        if ok:
            start = len(guest.transcript())
            guest.type_line("cat hello.txt")
            ok = guest.wait_for(PROMPT)
            seg["cat"] = guest.transcript()[start:]
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    cat_lines = [line.strip() for line in seg.get("cat", "").splitlines()]
    checks = [
        ("driven to the end", ok),
        ("`edit hello.txt` opens the editor (its status line: the name, Line 1 Col 1)",
         "Line 1  Col 1" in seg.get("open", "")),
        ("the saved file holds `hXi`: the repeat, the arrow and the save", "hXi" in cat_lines),
        ("no fault lines", faults == 0),
    ]
    if "hXi" not in cat_lines:
        print(f"     cat printed: {cat_lines[1:4]}")
    failed = 0
    for name, good in checks:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        failed += 0 if good else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

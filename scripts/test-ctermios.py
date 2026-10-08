#!/usr/bin/env python3
"""Terminal settings in a C program: tcgetattr and tcsetattr.

    python3 scripts/test-ctermios.py    (or `make test-ctermios`, which builds the image)

Step 2 of docs/roadmap/roadmap-ctrl-c.md, for item 4 of
docs/handoffs/2026-10-05-from-edit-editor-console.md: a C program turns raw
mode on the POSIX way, with `tcsetattr` and ISIG cleared, and reads Ctrl+C as
a key. /bin/CTERMIOS (libc/ctermios.c) uses Edit's own recipe, flag for flag.
One boot:

1. **`ctermios check`**: the initial settings (ISIG set, VMIN 1, VINTR 3,
   VQUIT 28), Edit's recipe set with TCSADRAIN, every flag read back, the
   saved settings restored with TCSAFLUSH and read back, ISIG following the
   kernel's mode when it is set directly with KBD_MODE, and the errors:
   EBADF on fd 9, ENOTTY on an open file, EINVAL for an unknown action,
   EFAULT for a null pointer.
2. **`ctermios raw`**: in raw mode Ctrl+C typed on the serial line is read
   as 3; after `q` it restores the saved settings, and the next Ctrl+C ends
   it (the kernel's kill line, and no `ctermios: bye`).
3. **`ctermios check` again**: starts with ISIG set, the raw mode having
   ended with the program that set it.

QEMU's own trace must hold no fault line. About a minute. Run it whenever
libc's termios, sys/termios.h or KBD_MODE changes.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-ctermios.txt")
PROMPT = "# "
ETX = b"\x03"
CHECK_LINES = [
    "initial ok", "set ok", "readback ok", "restore ok", "kernel ok",
    "fd 9 EBADF", "file ENOTTY", "action EINVAL", "null EFAULT",
]


def type_raw(guest, data):
    """Bytes on the serial line, paced like Guest.type_line, no Enter added."""
    for ch in data:
        guest.proc.stdin.write(bytes([ch]))
        guest.proc.stdin.flush()
        time.sleep(drive_qemu.TYPE_DELAY)


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-ctermios: ")
    seg = {}
    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            start = len(guest.transcript())
            guest.type_line("ctermios check")
            ok = guest.wait_for(r"check done[\s\S]*" + PROMPT)
            seg["check"] = guest.transcript()[start:]
        if ok:
            start = len(guest.transcript())
            guest.type_line("ctermios raw")
            ok = guest.wait_for("ctermios: raw")
            if ok:
                type_raw(guest, ETX)
                ok = guest.wait_for("ctermios: byte 3")
            if ok:
                type_raw(guest, b"q")
                ok = guest.wait_for("ctermios: restored")
            if ok:
                type_raw(guest, ETX)
                ok = guest.wait_for(PROMPT, timeout=30)
            seg["raw"] = guest.transcript()[start:]
        if ok:
            start = len(guest.transcript())
            guest.type_line("ctermios check")
            ok = guest.wait_for(r"check done[\s\S]*" + PROMPT)
            seg["again"] = guest.transcript()[start:]
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    check = seg.get("check", "")
    raw = seg.get("raw", "")
    checks = [("driven to the end", ok)]
    for line in CHECK_LINES:
        checks.append((f"check: {line}", f"ctermios: {line}" in check))
    checks += [
        ("raw: Ctrl+C read as 3 after tcsetattr cleared ISIG", "ctermios: byte 3" in raw),
        ("restored: Ctrl+C ended it",
         "ctermios: restored" in raw and "Ctrl+C - foreground task" in raw
         and "ctermios: bye" not in raw),
        ("the next program starts with ISIG set", "ctermios: initial ok" in seg.get("again", "")),
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

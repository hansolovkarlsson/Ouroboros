#!/usr/bin/env python3
"""Terminal settings in a C program: tcgetattr and tcsetattr.

    python3 scripts/test-ctermios.py    (or `make test-ctermios`, which builds the image)

Step 2 of docs/roadmap/roadmap-ctrl-c.md, for item 4 of
docs/handoffs/closed/2026-10-05-from-edit-editor-console.md: a C program turns raw
mode on the POSIX way, with `tcsetattr` and ISIG cleared, and reads Ctrl+C as
a key. /bin/CTERMIOS (libc/ctermios.c) uses Edit's own recipe, flag for flag.
One boot:

1. **`ctermios check`**: the initial settings (ISIG, OPOST and ONLCR set,
   VMIN 1, VINTR 3, VQUIT 28), Edit's recipe set with TCSADRAIN and read back
   with OPOST still set (the console always translates), flags the console
   cannot apply (ECHO, ICANON, ICRNL, OCRNL, VTIME 5) accepted and reported
   off by tcgetattr (it shows what is in force, as POSIX has it), the
   saved settings restored with TCSAFLUSH and read back, ISIG following the
   kernel's mode when it is set directly with KBD_MODE, and the errors:
   EBADF on fd 9, ENOTTY on an open file, EINVAL for an unknown action,
   EFAULT for a null pointer.
2. **`ctermios raw`**: in raw mode Ctrl+C is read as 3, typed on the serial
   line and on QEMU's USB keyboard (monitor sendkey); after `q` it restores
   the saved settings, and the next Ctrl+C ends it (the kernel's kill line,
   and no `ctermios: bye`).
3. **Died raw**: another `ctermios raw` ended by Ctrl+\\ while still raw, and
   then `ctermios check` must start with ISIG set: the mode ended with the
   program, by the kernel's reset (the runs before turned cooked themselves).
4. **New lines on the serial line** (ONLCR, which tcgetattr reports set; the
   kernel's console::write_bytes and console::write_out):
   `cat /include/sys/termios.h | head 3`, a
   file whose lines end in a bare LF, and `ctermios nl <chunk>`, five lines ending
   each a different way (a `\r` and its `\n` in two writes, a bare `\n`,
   `\r\n`, a bare `\n` by PUTC, a `\r` by PUTC between two writes through
   cond), then two lines of `<chunk>` and `<chunk> - 1` bytes before a bare
   `\n`, the inserted `\r` just past the kernel's chunk and at its last byte;
   `<chunk>` is read from console.rs's `const CHUNK` and checked to fit one
   write (`FS_DATA_MAX`, read from syscall-abi), so the edge follows the
   kernel's value. All must reach the serial line with every `\n` after a
   `\r`, no `\r\r`, and each line whole with its `\r\n`. Before 2026-11-05 a file's
   lines went out as they are and stepped down a serial terminal one column
   further each line.

QEMU's own trace must hold no fault line. About a minute. Run it whenever
libc's termios, sys/termios.h, KBD_MODE or the kernel's console::write_bytes,
console::write_out or console::CHUNK changes.
"""
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-ctermios.txt")
# In build/, not a temp directory: macOS caps a socket path at 104 bytes.
MONITOR = os.path.join(ROOT, "build", "test-ctermios.monitor")
KEY_DELAY = 0.3
PROMPT = "# "
ETX = b"\x03"
CHECK_LINES = [
    "initial ok", "set ok", "readback ok", "honest ok", "restore ok", "kernel ok",
    "fd 9 EBADF", "file ENOTTY", "action EINVAL", "null EFAULT",
]


def source_constant(path, pattern):
    """The one integer `pattern` (a regex with one group) finds in the file
    at `path`, relative to the repository; `None` unless exactly one line
    matches, so a renamed or duplicated constant fails the rig rather than
    leaving it on a stale value."""
    with open(os.path.join(ROOT, path)) as fh:
        found = re.findall(pattern, fh.read(), re.MULTILINE)
    return int(found[0]) if len(found) == 1 else None


CHUNK = source_constant("kernel/src/console.rs", r"^const CHUNK: usize = (\d+);")
FS_DATA_MAX = source_constant("syscall-abi/src/lib.rs", r"^pub const FS_DATA_MAX: u64 = (\d+);")


def output_of(seg):
    """A command's own output: past the shell's echo of the typed line, up to
    the prompt that follows it, so the echo and the prompt cannot pass or
    fail a check about the command."""
    first = seg.find("\n")
    last = seg.rfind(PROMPT)
    return seg[first + 1:last] if 0 <= first < last else ""


def newlines_clean(text):
    """Every `\n` after a `\r`, and no `\r` doubled: what the console's ONLCR
    sends, whichever path a line came by."""
    return re.search(r"(?<!\r)\n", text) is None and "\r\r" not in text


def main() -> int:
    if os.path.exists(MONITOR):
        os.remove(MONITOR)
    guest = drive_qemu.Guest(
        IMAGE, label="test-ctermios: ",
        extra_args=[
            "-device", "qemu-xhci,id=xhci0",
            "-device", "usb-kbd,bus=xhci0.0",
            "-monitor", f"unix:{MONITOR},server,nowait",
        ],
    )
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
                guest.type_raw(ETX)
                ok = guest.wait_for("ctermios: byte 3")
            if ok:
                drive_qemu.sendkeys(MONITOR, ["ctrl-c"], KEY_DELAY)
                ok = guest.wait_for("ctermios: byte 3")
            if ok:
                guest.type_raw(b"q")
                ok = guest.wait_for("ctermios: restored")
            if ok:
                guest.type_raw(ETX)
                ok = guest.wait_for(PROMPT, timeout=30)
            seg["raw"] = guest.transcript()[start:]
        if ok:
            # Ended while still raw (Ctrl+\, never a key): the mode must end
            # with the program, so the next one starts with ISIG set. The runs
            # above both turned cooked themselves before ending, so only this
            # one shows the kernel's reset.
            start = len(guest.transcript())
            guest.type_line("ctermios raw")
            ok = guest.wait_for("ctermios: raw")
            if ok:
                guest.type_raw(b"\x1c")
                ok = guest.wait_for(PROMPT, timeout=30)
            seg["died raw"] = guest.transcript()[start:]
        if ok:
            start = len(guest.transcript())
            guest.type_line("ctermios check")
            ok = guest.wait_for(r"check done[\s\S]*" + PROMPT)
            seg["again"] = guest.transcript()[start:]
        if ok:
            start = len(guest.transcript())
            guest.type_line("cat /include/sys/termios.h | head 3")
            ok = guest.wait_for(r"_SYS_TERMIOS_H[\s\S]*_SYS_TERMIOS_H[\s\S]*" + PROMPT)
            seg["newline"] = guest.transcript()[start:]
        if ok:
            start = len(guest.transcript())
            guest.type_line(f"ctermios nl {CHUNK or 0}")
            ok = guest.wait_for(r"nl done[\s\S]*" + PROMPT)
            seg["nl"] = guest.transcript()[start:]
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
        ("raw: Ctrl+C read as 3 after tcsetattr cleared ISIG, serial and USB",
         raw.count("ctermios: byte 3") == 2),
        ("restored: Ctrl+C ended it",
         "ctermios: restored" in raw and "Ctrl+C - foreground task" in raw
         and "ctermios: bye" not in raw),
        ("raw: Ctrl+\\ ended it while raw", "Ctrl+\\ - foreground task" in seg.get("died raw", "")),
        ("after a program died raw, the next starts with ISIG set",
         "ctermios: initial ok" in seg.get("again", "")),
        ("a file's LF-only lines reach the serial line as CR LF",
         "#define _SYS_TERMIOS_H\r\n" in output_of(seg.get("newline", ""))
         and newlines_clean(output_of(seg.get("newline", "")))),
        ("nl: every line ending reaches the serial line as one CR LF",
         all(f"nl-{x}\r\n" in output_of(seg.get("nl", "")) for x in "abcde")
         and newlines_clean(output_of(seg.get("nl", "")))),
        (f"nl: the kernel's chunk ({CHUNK} bytes, console.rs) fits one write ({FS_DATA_MAX}, syscall-abi)",
         CHUNK is not None and FS_DATA_MAX is not None and 5 <= CHUNK and CHUNK + 1 <= FS_DATA_MAX),
        ("nl: a \\r inserted just past the kernel's chunk and at its last byte",
         CHUNK is not None
         and "nl-f" + "x" * (CHUNK - 4) + "\r\n" in output_of(seg.get("nl", ""))
         and "nl-g" + "x" * (CHUNK - 5) + "\r\n" in output_of(seg.get("nl", ""))),
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

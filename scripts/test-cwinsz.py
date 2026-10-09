#!/usr/bin/env python3
"""The console's size, read by an ordinary program: two boots.

    python3 scripts/test-cwinsz.py      (or `make test-cwinsz`, which builds the image)

Item 3 of docs/handoffs/closed/2026-10-05-from-edit-editor-console.md. `CON_INFO`'s
size fields are open to every task (kernel/src/syscall.rs); a C program asks
with `ioctl(fd, TIOCGWINSZ, &ws)` (libc/src/file.c), and a Rust one with
`ulib::screen_size`, which `more` now pages by.

1. **Serial console** (no framebuffer): `/bin/CWINSZ` must say rows=0 cols=0
   on fds 0, 1 and 2 (a serial terminal's size is unknown, as on Linux when
   it was never set), EBADF for an fd never opened, ENOTTY for an open file
   and for another request; and `cwinsz | upper` must say ENOTTY for fds 1
   and 2, which are a pipe there, while fd 0 still answers.
2. **Framebuffer** (`-device ramfb`, read back by QMP screendump with
   test-cond-vt.py's decoder): CWINSZ's fds 0 to 2 must say the screen's own
   grid, the screendump's width and height over 8; and `more /include/elf.h`
   must fill the screen, the file's first line on the top row and `--More--`
   on the bottom one, where a fixed 23-line page left the shell's output
   above it. Then a file whose first line is 110 characters, wider than the
   screen, then elf.h: the long line must fill the top two rows, the page
   end a row sooner, and nothing scroll off, since `more` counts screen rows
   and not lines. This boot writes that file, so it boots a copy of the
   image.

QEMU's own trace must hold no fault line in either boot. Two boots, about a
minute and a half. Run it whenever CON_INFO, ioctl, ulib::screen_size or
more's paging changes.
"""
import importlib.util
import os
import re
import shutil
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
vt = load("test_cond_vt", "test-cond-vt.py")

IMAGE = os.path.join(ROOT, "build", "esp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-cwinsz.txt")
ELF_H = os.path.join(ROOT, "build", "esp", "include", "elf.h")
# The framebuffer boot writes a file, so it boots a copy.
COPY = os.path.join(ROOT, "build", "test-cwinsz.img")
LONG = 110  # wider than the 100-column screen: the line takes two rows


def serial_boot():
    guest = drive_qemu.Guest(IMAGE, label="test-cwinsz serial: ")
    try:
        driven = guest.run([
            ("login:", "root"), ("assword", "root"),
            ("# ", "cwinsz"), ("# ", "cwinsz | upper"), ("# ", ""),
        ])
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    lines = [l.strip() for l in out.splitlines()]
    want = [
        "cwinsz: fd 0 rows=0 cols=0", "cwinsz: fd 1 rows=0 cols=0", "cwinsz: fd 2 rows=0 cols=0",
        "cwinsz: fd 9 EBADF", "cwinsz: file ENOTTY", "cwinsz: request ENOTTY",
    ]
    piped = [
        "CWINSZ: FD 0 ROWS=0 COLS=0", "CWINSZ: FD 1 ENOTTY", "CWINSZ: FD 2 ENOTTY",
        "CWINSZ: FD 9 EBADF", "CWINSZ: FILE ENOTTY", "CWINSZ: REQUEST ENOTTY",
    ]
    checks = [
        ("serial: driven to the end", driven),
        ("serial: CWINSZ's six answers, size unknown (0 by 0)", all(w in lines for w in want)),
        ("serial: piped, fds 1 and 2 are ENOTTY and fd 0 still answers", all(p in lines for p in piped)),
        ("serial: no fault lines", faults == 0),
    ]
    return checks, out


def framebuffer_boot():
    font = vt.load_font()
    if os.path.exists(vt.QMP):
        os.remove(vt.QMP)
    shutil.copy(IMAGE, COPY)
    guest = drive_qemu.Guest(
        COPY, label="test-cwinsz framebuffer: ",
        extra_args=["-device", "ramfb", "-qmp", f"unix:{vt.QMP},server,nowait"],
    )
    cols = rows = 0
    sizes = more_screen = wide_screen = None
    try:
        qmp = vt.Qmp(vt.QMP)

        def snap():
            if os.path.exists(vt.SHOT):
                os.remove(vt.SHOT)
            qmp.call("screendump", filename=vt.SHOT)
            for _ in range(20):
                if os.path.exists(vt.SHOT) and os.path.getsize(vt.SHOT) > 0:
                    time.sleep(0.2)
                    return vt.text(vt.decode(vt.SHOT, font))
                time.sleep(0.1)
            raise SystemExit("test-cwinsz: screendump wrote nothing")

        def wait_screen(pattern, timeout=90):
            rx = re.compile(pattern)
            deadline = time.time() + timeout
            while time.time() < deadline:
                lines = snap()
                if any(rx.search(l) for l in lines):
                    time.sleep(drive_qemu.SETTLE)
                    return lines
                time.sleep(0.5)
            guest.report(f"!!! TIMEOUT waiting on the screen for {pattern!r}")
            return None

        if guest.wait_for("shell ready", timeout=120) and wait_screen(r"^login:"):
            guest.type_line("root")
            if wait_screen(r"assword"):
                guest.type_line("root")
                lines = wait_screen(r"^#$")
                if lines:
                    w, h, _ = vt.read_ppm(vt.SHOT)
                    cols, rows = w // vt.CELL, h // vt.CELL
                    guest.type_line("cwinsz")
                    lines = wait_screen(r"^cwinsz: request")
                    if lines:
                        sizes = [l for l in lines if l.startswith("cwinsz: fd ")]
                        guest.type_line("more /include/elf.h")
                        lines = wait_screen(r"^--More--")
                        if lines:
                            more_screen = lines
                            guest.type_line("q")
                            wait_screen(r"^#$")
                            # A first line wider than the screen: it takes
                            # two rows, so the page must end a row sooner.
                            guest.type_line("echo " + "x" * LONG + " > /w.txt")
                            wait_screen(r"^#$")
                            guest.type_line("cat /include/elf.h >> /w.txt")
                            wait_screen(r"^#$")
                            guest.type_line("more /w.txt")
                            lines = wait_screen(r"^--More--")
                            if lines:
                                wide_screen = lines
                                guest.type_line("q")
                                wait_screen(r"^#$")
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(ELF_H, errors="replace") as fh:
        # cond draws nothing for a tab (it drops non-printing bytes), so the
        # screen shows each line with its tabs gone.
        elf = [l.rstrip("\n").replace("\t", "").rstrip() for l in fh]
    grid = f"rows={rows} cols={cols}"
    checks = [
        (f"framebuffer: fds 0 to 2 say the screen's grid ({grid})",
         sizes is not None and all(f"cwinsz: fd {n} {grid}" in sizes for n in (0, 1, 2))),
        ("framebuffer: more fills the screen (elf.h's line 1 on top, --More-- at the bottom)",
         more_screen is not None and more_screen[-1] == "--More--"
         and more_screen[0] == elf[0][:cols].rstrip() and more_screen[rows - 2] == elf[rows - 2][:cols].rstrip()),
        ("framebuffer: a line wider than the screen takes two rows, and the page still starts with it",
         wide_screen is not None and wide_screen[0] == "x" * cols and wide_screen[1] == "x" * (LONG - cols)
         and wide_screen[-1] == "--More--" and wide_screen[rows - 2] == elf[rows - 4][:cols].rstrip()),
        ("framebuffer: no fault lines", faults == 0),
    ]
    detail = (sizes or []) + ([f"top: {more_screen[0]!r}", f"bottom: {more_screen[-1]!r}"] if more_screen else [])
    detail += [f"wide top: {wide_screen[0][:20]!r}... ({len(wide_screen[0])}), next: {wide_screen[1]!r}"] if wide_screen else []
    return checks, detail


def main() -> int:
    checks, out = serial_boot()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)
    fb_checks, detail = framebuffer_boot()
    checks += fb_checks
    for line in detail:
        print(f"     {line}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript (serial boot): {os.path.relpath(TRANSCRIPT, ROOT)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""The console server's escape sequences on a framebuffer, read off the pixels.

    python3 scripts/test-cond-vt.py     (or `make test-cond-vt`, which builds the image)

Item 1 of docs/handoffs/closed/2026-10-05-from-edit-editor-console.md: cond's
framebuffer backend must act on `CSI row;col H`, `CSI K`, `CSI J`, `CSI 7 m` and
`CSI 0 m`, and act on `CSI ? 25 l/h` (since 2026-10-11 cond draws a cursor, the
cell inverted, and those two hide and show it). QEMU gets a framebuffer from
`-device ramfb`, and then cond renders the shell's output there and nowhere
else, so the serial line shows only the kernel's own boot log. This rig reads
the screen instead: a QMP `screendump` of the guest's display, cut into 8x8
cells, each cell decoded back to a character with cond's OWN font table
(programs/servers/cond/src/font.rs, parsed here), plain or inverted. A cell
that matches no glyph decodes as `?`, so a half-drawn or misplaced glyph
cannot pass for a right one.

It logs in by reading the screen for `login:` and the prompt, runs
`/bin/VTPROBE COLS ROWS` with the size the screendump has, then compares rows
1 to 13 and the bottom-right cell with the screen VTPROBE's doc comment says
it draws (modelled in `expected()` below, step by step). Every differing cell
is printed. QEMU's own trace must hold no fault line. One boot, about a
minute. Run it whenever cond's framebuffer backend, its font, or the kernel's
FB_* primitives change.

After the model's check, `vtprobe hold` writes `CSI 7 m` and `held` and
waits for a key; Ctrl+C kills it, and the shell's next prompt must be drawn in
normal video (its reset after a kill, step 4 of docs/roadmap/roadmap-ctrl-c.md).
Then the cursor: after `clear` the prompt's row must carry it, the cell after
`# ` inverted (a blank cell inverted is a block); `vtprobe hide` writes
`CSI ? 25 l` and `hidden` and waits, and its row must carry no inverted cell;
Ctrl+C kills it, and the shell's next prompt must carry the cursor again (its
reset after a kill shows it). The first fails with FB_CURSOR a no-op in the
kernel, the second with cond ignoring `CSI ? 25 l`, the third with the shell's
reset not writing `CSI ? 25 h`. Last, `vtprobe leave` exits with the cursor
hidden and video reversed, which the prompt after it keeps (as on Unix), and
`clear` must bring both back: the prompt after it carries the cursor and is
normal; fails with clear's `CSI 0 m` `CSI ? 25 h` prefix removed.

`--keep DIR` leaves the final screendump (PPM) in DIR for a look by eye.
"""
import importlib.util
import json
import os
import re
import shutil
import socket
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
FONT = os.path.join(ROOT, "programs", "servers", "cond", "src", "font.rs")
# In build/, not a temp directory: macOS caps a socket path at 104 bytes.
QMP = os.path.join(ROOT, "build", "test-cond-vt.qmp")
SHOT = os.path.join(ROOT, "build", "test-cond-vt.ppm")
TRANSCRIPT = os.path.join(ROOT, "build", "test-cond-vt.txt")
CELL = 8
BLANK = " "
CHECKED_ROWS = 13


def load_font():
    """cond's glyph table, keyed by the cell's 64 pixels (1 lit, 0 dark, row by
    row, leftmost first): key -> character. Bit 0 of a row byte is the
    leftmost pixel, as font.rs's own doc comment says."""
    glyphs = {}
    rx = re.compile(r"\[((?:0x[0-9a-f]{2},\s*){7}0x[0-9a-f]{2})\],\s*//\s*0x([0-9a-f]{2})")
    with open(FONT) as fh:
        for m in rx.finditer(fh.read()):
            rows = [int(b, 16) for b in m.group(1).split(",")]
            key = bytes((row >> dx) & 1 for row in rows for dx in range(CELL))
            glyphs[key] = chr(int(m.group(2), 16))
    if len(glyphs) < 90:
        raise SystemExit(f"test-cond-vt: read only {len(glyphs)} glyphs from {FONT}")
    return glyphs


class Qmp:
    def __init__(self, path):
        deadline = time.time() + 30
        while True:
            try:
                self.sock = socket.socket(socket.AF_UNIX)
                self.sock.connect(path)
                break
            except OSError:
                if time.time() > deadline:
                    raise
                time.sleep(0.5)
        self.f = self.sock.makefile("rw")
        self.f.readline()
        self.call("qmp_capabilities")

    def call(self, command, **arguments):
        msg = {"execute": command}
        if arguments:
            msg["arguments"] = arguments
        self.f.write(json.dumps(msg) + "\n")
        self.f.flush()
        while True:
            reply = json.loads(self.f.readline())
            if "return" in reply or "error" in reply:
                return reply


def read_ppm(path):
    with open(path, "rb") as fh:
        data = fh.read()
    m = re.match(rb"P6\s+(\d+)\s+(\d+)\s+(\d+)\s", data)
    if not m:
        raise SystemExit(f"test-cond-vt: {path} is not a P6 PPM")
    w, h = int(m.group(1)), int(m.group(2))
    return w, h, data[m.end():]


LIT = bytes(1 if v > 127 else 0 for v in range(256))
FLIP = bytes.maketrans(b"\x00\x01", b"\x01\x00")


def decode(path, font):
    """The screen as rows of (char, reverse) cells. A pixel is lit when its
    first channel is; cond draws pure white or pure black, so one channel
    decides, and a pixel of any other colour would decode as `?` anyway."""
    w, h, px = read_ppm(path)
    lit = px[0::3].translate(LIT)
    rows = []
    for r in range(h // CELL):
        row = []
        for c in range(w // CELL):
            x0 = c * CELL
            key = b"".join(
                lit[(r * CELL + dy) * w + x0:(r * CELL + dy) * w + x0 + CELL]
                for dy in range(CELL)
            )
            if key in font:
                row.append((font[key], False))
            elif key.translate(FLIP) in font:
                row.append((font[key.translate(FLIP)], True))
            else:
                row.append(("?", False))
        rows.append(row)
    return rows


def text(rows):
    return ["".join(ch for ch, _ in row).rstrip() for row in rows]


def expected(cols, rows):
    """The screen VTPROBE draws, 0-based, built from its doc comment's steps."""
    g = [[(chr(ord("a") + r % 26), False)] * cols for r in range(rows)]

    def put(r, c, s, rev=False):
        for i, ch in enumerate(s):
            g[r][c + i] = (ch, rev)

    def blank(r, c0, c1):
        for c in range(c0, c1):
            g[r][c] = (BLANK, False)

    blank(0, 0, cols)                       # 2. CSI 2;5 H, CSI 1 J
    blank(1, 0, 5)
    blank(2, 9, cols)                       # 3. CSI K, CSI 1 K, CSI 2 K at column 10
    blank(3, 0, 10)
    blank(4, 0, cols)
    put(5, 4, "REV", True)                  # 4. reverse video
    put(5, 7, "nor")
    put(5, 10, "x", True)
    put(5, 11, "y")
    put(5, 12, "z", True)
    put(5, 13, "w")
    put(6, 2, "ok")                         # 5. cursor hide and show draw nothing
    put(7, 0, "r8")                         # 6. omitted parameters, ignored sequences
    put(0, 4, "d")
    put(0, 0, "h")
    put(0, 1, "q")
    put(8, cols - 2, "XZ")                  # 7. backspace with a wrap pending
    blank(9, 19, cols)                      # 8. CSI 10;20 H, CSI J
    for r in range(10, rows):
        blank(r, 0, cols)
    put(10, cols - 1, "P")                  # 9. the deferred wrap, taken
    put(11, 0, "Q")
    put(12, 0, "e")                         # 10. sequences that draw nothing
    put(rows - 1, cols - 1, "c")            # 11. clamped to the bottom-right cell
    return g


def show(cell):
    ch, rev = cell
    return f"{ch!r}{' reversed' if rev else ''}"


def main() -> int:
    keep = None
    if len(sys.argv) == 3 and sys.argv[1] == "--keep":
        keep = sys.argv[2]
    elif len(sys.argv) != 1:
        print(__doc__.strip().splitlines()[2], file=sys.stderr)
        return 2
    font = load_font()
    if os.path.exists(QMP):
        os.remove(QMP)
    guest = drive_qemu.Guest(
        IMAGE, label="test-cond-vt: ",
        extra_args=["-device", "ramfb", "-qmp", f"unix:{QMP},server,nowait"],
    )
    screen = None
    held_screen = None
    cursor_screen = None
    hidden_screen = None
    back_screen = None
    left_screen = None
    clear_screen = None
    driven = False
    try:
        qmp = Qmp(QMP)

        def snap():
            if os.path.exists(SHOT):
                os.remove(SHOT)
            qmp.call("screendump", filename=SHOT)
            for _ in range(20):
                if os.path.exists(SHOT) and os.path.getsize(SHOT) > 0:
                    time.sleep(0.2)
                    return decode(SHOT, font)
                time.sleep(0.1)
            raise SystemExit("test-cond-vt: screendump wrote nothing")

        def wait_until(fits, what, timeout=90):
            """The first screen that `fits`, read again a settle later: the frame
            that matched can be mid-write (the shell sends its prompt as two
            one-byte messages and cond settles the cursor after each), so the
            graded frame is the fresh one when it still fits. When it no longer
            fits the state was transient (a command's echo that the program
            then cleared), and the frame that matched is the answer."""
            deadline = time.time() + timeout
            while time.time() < deadline:
                first = snap()
                if fits(text(first)):
                    time.sleep(drive_qemu.SETTLE)
                    settled = snap()
                    return settled if fits(text(settled)) else first
                time.sleep(0.5)
            guest.report(f"!!! TIMEOUT waiting on the screen for {what}")
            return None

        def wait_screen(pattern, timeout=90):
            rx = re.compile(pattern)
            return wait_until(lambda lines: any(rx.search(l) for l in lines), repr(pattern), timeout)

        def wait_after(word, timeout=30):
            """The screen once the shell's prompt is below the row a probe
            wrote `word` at the start of: the probe has ended, killed or
            exited, and the shell has drawn what it draws after."""
            def fits(lines):
                at = next((i for i, l in enumerate(lines) if l.startswith(word)), None)
                return at is not None and "#" in lines[at + 1:]
            return wait_until(fits, f"a prompt below {word!r}", timeout)

        def wait_cleared(timeout=90):
            """A screen `clear` has just produced: the prompt and nothing else.
            A bare `#` anywhere would also match the prompt that was on screen
            before `clear` ran."""
            return wait_until(lambda lines: [l for l in lines if l] == ["#"], "a cleared screen", timeout)

        if (guest.wait_for("shell ready", timeout=120)
                and wait_screen(r"^login:")):
            guest.type_line("root")
            if wait_screen(r"assword"):
                guest.type_line("root")
                rows = wait_screen(r"^#$")
                if rows:
                    cols, nrows = len(rows[0]), len(rows)
                    command = f"vtprobe {cols} {nrows}"
                    guest.type_line(command)
                    # The command's echo shows first; VTPROBE's clear takes it
                    # away, and it parks the cursor on row 14, so the shell's
                    # next prompt lands there or below once it has exited.
                    # Waiting for the echo first is what keeps the login
                    # prompt, already on screen, from passing for that one.
                    if wait_screen(re.escape(command)):
                        deadline = time.time() + 60
                        while time.time() < deadline:
                            rows = snap()
                            lines = text(rows)
                            if (not any(command in line for line in lines)
                                    and "#" in lines[CHECKED_ROWS:]):
                                time.sleep(drive_qemu.SETTLE)
                                screen = snap()
                                driven = True
                                break
                            time.sleep(0.5)
                    # Step 4 of the Ctrl-C plan: a program killed with reverse
                    # video on. `vtprobe hold` writes CSI 7 m and `held`, then
                    # waits for a key; Ctrl+C kills it, and the shell must
                    # draw its next prompt in normal video.
                    if driven:
                        guest.type_line("clear")
                        if wait_cleared():
                            guest.type_line("vtprobe hold")
                            if wait_screen(r"^held"):
                                guest.proc.stdin.write(b"\x03")
                                guest.proc.stdin.flush()
                                held_screen = wait_after("held")
                    # The cursor: on the prompt after a clear; gone while
                    # `vtprobe hide` holds it hidden; back on the prompt the
                    # shell draws after Ctrl+C kills it.
                    if held_screen is not None:
                        guest.type_line("clear")
                        cursor_screen = wait_cleared()
                        if cursor_screen:
                            guest.type_line("vtprobe hide")
                            hidden_screen = wait_screen(r"^hidden")
                            if hidden_screen:
                                guest.proc.stdin.write(b"\x03")
                                guest.proc.stdin.flush()
                                back_screen = wait_after("hidden")
                                # A program that exits with the cursor hidden
                                # and video reversed leaves both; `clear` is
                                # the way back.
                                if back_screen is not None:
                                    guest.type_line("vtprobe leave")
                                    left_screen = wait_after("left")
                                    if left_screen is not None:
                                        guest.type_line("clear")
                                        clear_screen = wait_cleared()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(guest.transcript())
        if screen:
            fh.write("\n--- the screen at the end ---\n")
            fh.write("\n".join(text(screen)) + "\n")
    if keep and os.path.exists(SHOT):
        shutil.copy(SHOT, keep)

    bad = []
    if screen:
        cols, nrows = len(screen[0]), len(screen)
        want = expected(cols, nrows)
        cells = [(r, c) for r in range(CHECKED_ROWS) for c in range(cols)]
        cells.append((nrows - 1, cols - 1))
        for r, c in cells:
            if screen[r][c] != want[r][c]:
                bad.append(f"row {r + 1} col {c + 1}: {show(screen[r][c])}, expected {show(want[r][c])}")
        for line in text(screen)[:CHECKED_ROWS]:
            print(f"     |{line}")
    held_reversed = prompt_normal = False
    if held_screen:
        lines = text(held_screen)
        at = next(i for i, l in enumerate(lines) if "held" in l)
        col = lines[at].index("held")
        held_reversed = all(held_screen[at][col + k][1] for k in range(4))
        prompt_at = max(i for i, l in enumerate(lines) if l == "#")
        prompt_normal = prompt_at > at and held_screen[prompt_at][0] == ("#", False)
        print(f"     |{lines[at]}")
        print(f"     |{lines[prompt_at]}  (prompt reversed: {held_screen[prompt_at][0][1]})")
    def last_prompt_row(rows):
        """The cells of the lowest row that reads as a bare prompt, or None."""
        if not rows:
            return None
        lines = text(rows)
        prompt_at = max((i for i, l in enumerate(lines) if l == "#"), default=None)
        return None if prompt_at is None else rows[prompt_at]

    def cursor_on_prompt(rows):
        """The last prompt row's cell after `# ` is a blank cell inverted, and
        it is the row's only inverted cell."""
        row = last_prompt_row(rows)
        return row is not None and row[2] == (BLANK, True) and sum(1 for _, rev in row if rev) == 1

    def no_cursor_on_hidden(rows):
        """The row `vtprobe hide` wrote `hidden` on has no inverted cell."""
        if not rows:
            return False
        lines = text(rows)
        at = next((i for i, l in enumerate(lines) if l.startswith("hidden")), None)
        return at is not None and not any(rev for _, rev in rows[at])

    def prompt_left_changed(rows):
        """The last prompt row after `vtprobe leave`: `# ` drawn reversed and no
        cursor block after it."""
        row = last_prompt_row(rows)
        return row is not None and row[0] == ("#", True) and row[2] == (BLANK, False)

    cursor_drawn = cursor_on_prompt(cursor_screen)
    cursor_hidden = no_cursor_on_hidden(hidden_screen)
    cursor_back = cursor_on_prompt(back_screen) and (
        back_screen is not None and no_cursor_on_hidden(back_screen))
    left_kept = prompt_left_changed(left_screen)
    clear_restores = cursor_on_prompt(clear_screen) and clear_screen[0][0] == ("#", False)
    checks = [
        ("driven to the end", driven and clear_screen is not None),
        (f"rows 1-{CHECKED_ROWS} and the bottom-right cell as modelled", driven and not bad),
        ("vtprobe hold drew `held` reversed", held_reversed),
        ("after Ctrl+C killed it, the shell's prompt is in normal video", prompt_normal),
        ("the cursor is drawn after the prompt (the cell inverted, the row's only one)", cursor_drawn),
        ("vtprobe hide (CSI ? 25 l) takes the cursor off its row", cursor_hidden),
        ("after Ctrl+C killed it, the shell's prompt has the cursor again", cursor_back),
        ("a program that exits with the cursor hidden and video reversed leaves both (as on Unix)", left_kept),
        ("clear shows the cursor and restores normal video before it clears", clear_restores),
        ("no fault lines", faults == 0),
    ]
    for line in bad[:40]:
        print(f"     {line}")
    if len(bad) > 40:
        print(f"     ... and {len(bad) - 40} more cells")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

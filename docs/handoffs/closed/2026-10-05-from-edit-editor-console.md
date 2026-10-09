# What a full-screen editor needs from the console and keyboard

- **From:** Edit
- **To:** Ouroboros
- **Date:** 2026-10-05
- **Kind:** requirement
- **Status:** done
- **Blocks:** Edit running on Ouroboros, the port that follows Edit's
  stage 1 (`~/Projects/Edit/docs/ROADMAP.md`, "The port to Ouroboros")

## What is asked

Edit (`~/Projects/Edit`) is a small full-screen console editor in C,
modeless and WordStar-style. It is built in the Mac terminal first and then
ported here, as the editor your roadmap already plans for: items 1 and 3,
and item c "A text editor + full-screen terminal control". This note fixes
exactly what that editor will use, so the console work can be sized and done
in parallel. The list is deliberately small.

1. **The console server (`cond`), framebuffer backend, should interpret:**
   - `ESC [ row ; col H`, cursor to row and column (1-based, both optional,
     default 1);
   - `ESC [ K`, clear from the cursor to the end of the line;
   - `ESC [ 2 J`, clear the screen;
   - `ESC [ 7 m` and `ESC [ 0 m` (and `ESC [ m`), reverse video on and off;
   - `ESC [ ? 25 l` and `ESC [ ? 25 h`, hide and show the cursor. These two
     only avoid flicker, so ignoring them is acceptable; they must not draw
     anything, though.

   Edit writes nothing else: no scroll regions, no colour, no insert or delete
   line. It redraws changed rows instead.
2. **The USB keyboard should send the usual VT100/xterm sequences** for the
   keys it drops today: the arrows (`ESC [ A/B/C/D`), Home and End
   (`ESC [ H`, `ESC [ F`), Page Up and Page Down (`ESC [ 5 ~`, `ESC [ 6 ~`),
   Delete (`ESC [ 3 ~`), F2 and F3 (`ESC O Q`, `ESC O R`). Edit also works
   without them, through the WordStar Ctrl keys, so this is second in order.
3. **An ordinary program needs to read the screen size,** in columns and
   rows, on both backends.
4. **A program needs a way to receive Ctrl-C (0x03) as a key.** In WordStar,
   ^C is "page down". A per-program opt-out of the kill would do, for
   example a call that says "this foreground program handles 0x03 itself".
5. **`main(int argc, char **argv)`,** so `edit file.txt` works. Until then
   Edit starts empty and opens a file with ^KO, so this blocks nothing.

`rename` and `unlink` in libc would let Edit save safely, by writing a temp
file and renaming it over the original. They are already asked for by
Proem's notes. Without them Edit saves with `O_TRUNC`, which works today.

## Why

Read from the tree on branch `fsd/large-write`, 2026-10-05:

- `programs/servers/cond/src/main.rs:147-202`: `put_char` understands only
  CSI `J` (line 187, clears the whole screen whatever the parameter) and
  `H` (line 192, always goes home, row and column ignored). Every other
  final byte is dropped.
- `kernel/src/xhci.rs:980`: `keycode_to_ascii` maps letters, digits,
  symbols, Enter, Backspace (0x08), Space and Ctrl+A to Ctrl+Z, and returns
  `None` for every other keycode, so the arrows, function keys and
  navigation keys never reach a program.
- `syscall-abi/src/lib.rs:412-419`: `CON_INFO` gives columns and rows but
  is gated to the console server, so an ordinary program cannot ask.
  `programs/fileutils/more/src/main.rs:16` assumes about 24 rows for this
  reason.
- `kernel/src/syscall.rs:218-226`: Ctrl-C is intercepted at the single
  choke point for keyboard input and swallowed, and the foreground program
  is killed.
- `libc/src/crt0.c:6-9`: `_start` calls `main(void)`.
- `docs/ROADMAP.md:2530-2541`, item c, already plans the console and its
  first editor together, and suggests developing on QEMU serial first. Edit
  will do that. The framebuffer is where this note's work lies.

## Done when

Each of points 1 to 4 is handled in the source (`cond`, the keyboard
mapping, a screen-size call open to ordinary programs, a Ctrl-C opt-out),
and a program in Ouroboros's tree shows it working on the framebuffer
console: it writes each sequence from point 1 and prints the bytes each key
from point 2 sends. Edit checks this by reading that program and the reply,
and then by running Edit itself on Ouroboros. Point 5 is done when a C
program's `main` receives its arguments.

## Reply

Accepted 2026-10-05 by Ouroboros, for later: on `docs/ROADMAP.md` under
"What Edit asks of Ouroboros", not started.

Checked against `main` at `ca4afc3`, and every citation holds: `cond`'s
`put_char` acts on CSI `J` and `H` only, both ignoring their parameters;
`keycode_to_ascii` (`kernel/src/xhci.rs:980`) returns `None` for the
navigation and function keys; `CON_INFO` is gated to `CON_TASK`; Ctrl-C is
intercepted and swallowed in `syscall.rs`'s keyboard choke point; `crt0.c`
calls `main(void)`; `more` assumes 24 rows.

What shapes the work. Points 1 to 3 are additive. Point 4 changes the one
place every keyboard path passes through, and the Ctrl-C kill is the
escape hatch from a runaway foreground program, so the opt-out needs a
design that keeps a way out (a second key, or the opt-out ending with the
program); it gets a design note before code. Point 5 is a loader and
`crt0` change: the kernel already passes argv to Rust programs
(`ulib`'s argv), so it is a matter of handing the same to C.

One correction: Proem's notes ask for `unlink` (accepted 2026-10-01), not
`rename`. `rename` is not asked for yet; if Edit wants the temp-file save,
a note asking for it is the way.

Noted 2026-10-06 by Ouroboros: Edit moved into DevTools, so this request is
DevTools's now (`2026-10-06-from-devtools-edit-moved.md`, closed). The
**Blocks** line's section is `~/Projects/DevTools/docs/ROADMAP.md`, under
**Edit**, "The port to Ouroboros", and the source paths it names are under
`~/Projects/DevTools/edit/`. Items 1 to 4 stay accepted and not started on
`docs/ROADMAP.md`; item 5 was done by #219. Further replies about the editor
go to DevTools's `docs/handoffs/`.

Progress 2026-10-07, Ouroboros: item 1 is built (#228, on top of #227).
`cond`'s framebuffer backend acts on `CSI row;col H` (1-based, either
omitted means 1, clamped to the screen), `CSI K` and `CSI J` in all three
forms (0, 1, 2; `J` does not move the cursor, as on a VT100), and `CSI m`
with 7 for reverse video and 0, 27 or no parameter for normal; any private
sequence such as `CSI ? 25 l/h` is parsed and draws nothing, and so does
any other escape: `ESC ( B`, an OSC up to BEL or `ESC \`, and `CSI 3 J`
(there is no scrollback). Three things Edit should know. Erasing blanks to
black even with reverse on. The last column wraps late, as on xterm: a
glyph in the last column leaves the cursor there and the wrap happens at
the next glyph, so Edit can fill the bottom row without the screen
scrolling. And a backspace while that wrap is pending is NOT xterm's: it
only cancels the wrap and leaves the cursor on the last column, where xterm
moves to the column before; the shell's `BS ' ' BS` erase depends on it. `/bin/VTPROBE COLS ROWS`
writes every sequence, and `make test-cond-vt` checks the screen it draws,
cell by cell, against a model. Items 2 to 4 are still open.

Merged 2026-10-07 (#228, `379b898`), and DevTools told by notice:
`~/Projects/DevTools/docs/handoffs/2026-10-07-from-ouroboros-editor-console-item-1.md`.

Progress 2026-10-07, Ouroboros: item 2 is built (#229). The USB keyboard
sends `ESC [ A`/`B`/`C`/`D` for Up, Down, Right and Left, `ESC [ H` and
`ESC [ F` for Home and End, `ESC [ 5 ~`, `ESC [ 6 ~` and `ESC [ 3 ~` for Page
Up, Page Down and Delete, and `ESC O Q` and `ESC O R` for F2 and F3, each
sequence queued whole. Shift, Ctrl and Alt do not change them (xterm's
`ESC [ 1 ; 5 A` forms are not sent). The Escape key itself still sends
nothing, and neither does Tab. `/bin/READKEY` prints each byte a key sends,
and `make test-nav-keys` presses the eleven keys on QEMU's USB keyboard and
checks the bytes. One hazard Edit should know until item 4 is designed: if
a key arrives while Edit is running rather than waiting in a read (busy
redrawing), the kernel's tick consumes one byte of it, looking for Ctrl-C,
and drops it; a sequence can then reach Edit as `[A` or `A`. Items 3 and 4
are still open.

Merged 2026-10-07 (#229, `cec028c`), and DevTools told by notice:
`~/Projects/DevTools/docs/handoffs/2026-10-07-from-ouroboros-editor-keys-item-2.md`.

Progress 2026-10-07, Ouroboros: item 3 is built (#230). A C program asks
the POSIX way, `ioctl(STDOUT_FILENO, TIOCGWINSZ, &ws)` with `<sys/ioctl.h>`,
so Edit's `plat_screen_size` in `platform_posix.c` works unchanged. On the
framebuffer it answers the screen's character grid (100 by 75 on QEMU's
800x600 `ramfb`). On a serial console it answers 0 rows and 0 columns, as
Linux does for a terminal whose size was never set, and Edit's existing
fallback to 80 by 24 applies. A descriptor that is not the console (stdout
piped, an open file) is `ENOTTY`. `/bin/CWINSZ` prints every answer, and
`make test-cwinsz` checks them on both backends. Item 4 is still open.

Merged 2026-10-07 (#230, `363913f`), and DevTools told by notice:
`~/Projects/DevTools/docs/handoffs/2026-10-07-from-ouroboros-editor-size-item-3.md`.

Progress 2026-10-07, Ouroboros: item 4 has its design note,
`docs/roadmap/roadmap-ctrl-c.md`, and no code yet. In short: `tcsetattr`
with `ISIG` cleared puts the program in a raw mode where 0x03 reaches it
(so `plat_raw_on` would work unchanged), and Ctrl+\ stays the way out in
every mode, so it would never reach Edit. Hans settled its four decisions
the same day, and DevTools is asked whether Edit binds ^\ before any of it
is built: `~/Projects/DevTools/docs/handoffs/2026-10-07-from-ouroboros-ctrl-backslash.md`.

Answered 2026-10-08 by DevTools: Edit does not bind ^\, so Ctrl+\ as the
way out stands and item 4 waits on nothing but its own build
(`~/Projects/DevTools/docs/handoffs/closed/2026-10-07-from-ouroboros-ctrl-backslash.md`).
Item 4 is still open.

Progress 2026-10-08, Ouroboros: item 4's first half is built (#235). A
program in raw mode, `KBD_MODE` (syscall 70) with `KBD_RAW`, reads Ctrl-C
as the byte 3, on the serial line and the USB keyboard; Ctrl+\ ends a
foreground program in every mode, and the USB keyboard now sends it.
`/bin/READKEY raw` shows both, and `make test-kbd-mode` checks them. What
Edit uses, `tcsetattr` with `ISIG` cleared, is step 2 of
`docs/roadmap/roadmap-ctrl-c.md` and not built yet, so item 4 stays open.

Merged 2026-10-08 (#235, `346e96b`).

Progress 2026-10-08, Ouroboros: what Edit uses for item 4 is built (#236).
`tcgetattr` and `tcsetattr` are in the C library, `<termios.h>` (through
`<sys/termios.h>`, Linux's values), so the termios calls in `plat_raw_on`
and `plat_raw_off` work unchanged: `ISIG` cleared reads Ctrl+C as 3, and
restoring the saved settings makes Ctrl+C end the program again. Every
other flag is accepted and changes nothing, since the console already
behaves as raw mode asks, and `tcgetattr` reports the console as it is (so
Edit's recipe reads back exactly as set). Four things Edit should know:
Ctrl+\ ends Edit in every mode and is never a key; `TCSAFLUSH` keeps keys
typed ahead, as `TCSADRAIN` does; the rest of `<termios.h>` (speeds,
`tcflush`) is not provided; and `platform_posix.c` as a whole does not yet
build here, since `plat_raw_on` also calls `sigaction` (SIGWINCH, SIGHUP,
SIGTERM) and the file includes `<poll.h>`, neither of which this C library
has (found by the review of #236). There are no signals to deliver, so an
Ouroboros platform file would leave those out; a note asking for something
else is the way, if Edit needs it.
`/bin/CTERMIOS` runs Edit's recipe flag for flag, and `make test-ctermios`
checks it. Item 4 stays open until steps 3 and 4 of the plan are built.

Merged 2026-10-08 (#236, `b4408c5`), and step 4 of the plan, the shell's
reset to normal video after a killed program (#237, `db7762f`).

Progress 2026-10-08, Ouroboros: the last step of item 4 is built (#238).
The kernel's keyboard queue no longer hands a key's rest to the next
program (an arrow read in part by a program that exits leaves nothing for
the shell), nothing typed after a Ctrl+C or Ctrl+\ is eaten, a bare Escape
from a serial terminal counts as a key of its own after 40 ms, and
`TCSAFLUSH` now discards keys typed ahead, as POSIX has it, so Edit's
`plat_raw_off` leaves nothing it was typed for to the shell. When #238 is
merged, item 4 is done, and DevTools gets the plan's notice.

Done 2026-10-08 by Ouroboros: every item is built and merged. Item 1 #228
(`379b898`), item 2 #229 (`cec028c`), item 3 #230 (`363913f`), item 5 #219,
and item 4 in four steps: #235 (`346e96b`), #236 (`b4408c5`), #237
(`db7762f`) and #238 (`26ff3ef`). DevTools is told by notice:
`~/Projects/DevTools/docs/handoffs/2026-10-08-from-ouroboros-editor-ctrl-c-item-4.md`,
which also says what the rest of `platform_posix.c` would need (`sigaction`,
`<poll.h>`). The note moves to `docs/handoffs/closed/`.

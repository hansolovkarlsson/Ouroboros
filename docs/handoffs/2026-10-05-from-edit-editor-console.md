# What a full-screen editor needs from the console and keyboard

- **From:** Edit
- **To:** Ouroboros
- **Date:** 2026-10-05
- **Kind:** requirement
- **Status:** accepted
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

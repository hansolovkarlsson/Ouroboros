# A read of the keyboard that can time out, and Edit built for the image

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-08
- **Kind:** requirement
- **Status:** accepted
- **Blocks:** Edit running on Ouroboros

## What is asked

Two things, the second needing the first. They follow your notice
`2026-10-08-from-ouroboros-editor-ctrl-c-item-4.md`, which said "If Edit
needs something there, a note asking for it is the way."

1. **`poll` for fd 0**, declared in `<poll.h>` as POSIX has it
   (`struct pollfd`, `nfds_t`, `POLLIN`, and `int poll(struct pollfd *,
   nfds_t, int timeout_ms)`). What Edit needs of it, and no more:
   - one `pollfd`, fd 0, `POLLIN`;
   - `timeout_ms` of -1 waits for a byte, 0 does not wait, and a positive
     one waits at most that long, to within about 10 ms: Edit's shortest is
     50 ms;
   - the answer is 1 with `revents` `POLLIN` when a byte is waiting for the
     keyboard's owner, so that the `read(0, ...)` after it does not block,
     and 0 when the time runs out with none;
   - **it flushes what is buffered for fd 1 before it waits**, as `read`
     of fd 0 does (the stdin/stdout tie in `libc/src/file.c`), so the screen
     Edit has just drawn is up while it waits.

   Any other descriptor, or more than one, may be refused as you think
   right, so long as the refusal is a return of -1 or `POLLNVAL` and not a
   hang.

2. **`/bin/edit` in the image, built as `cpp-bin` builds cpp**: from
   `../DevTools/edit/src` (or wherever an `EDIT_DIR` says), every `*.c`
   there except `platform_posix.c`, compiled with `CFLAGS_OS` and
   `PICO_INC` and linked with picolibc and the port. `platform_ouroboros.c`
   is the platform file for Ouroboros; it is DevTools's and is only read
   here, as cpp's sources are. Optional or required as you choose, as
   `NEED_CPP` does for cpp.

## Why

Edit reads a key with a timeout in two places. The key decoder waits 50 ms
after a byte that may begin an escape sequence (an arrow, F2), and `^Q Q`,
WordStar's repeat, runs a command again every 25 ms to 225 ms until a key
is pressed (`edit/src/main.c`, line 466, `plat_read_byte(25 * pace)`). On
the Mac that is `poll` (`edit/src/platform_posix.c`, `plat_read_byte`).

This C library has no way to wait for a key with a time limit. `read(0,
...)` blocks in `READ_CHAR`; `TRY_READ_CHAR` and `MONOTONIC_US` are system
calls a program could spin on, but `libc/include/sys.h` is the hand-rolled
library's and not picolibc-shaped, and a raw `TRY_READ_CHAR` would skip the
flush of fd 1, leaving the screen unshown. Your `sys/termios.h` says VMIN
and VTIME change nothing, so termios cannot do it either. `poll` is the
POSIX way, and it keeps Edit's Ouroboros platform file the same shape as
the Mac's.

DevTools now has that file, `edit/src/platform_ouroboros.c`, and a check,
`make -C edit ouroboros-build`, which compiles Edit for Ouroboros with
`CFLAGS_OS` read from your Makefile and picolibc's headers and the port's
by `-I`, then looks up every symbol the objects leave undefined in
`third_party/picolibc-prebuilt/lib/libc.a` and in the functions defined in
`libc/src/*.c` and `libc/pico/*.c`. Today, at your `d3d6c47`:

    $ make -C edit ouroboros-build
    src/platform_ouroboros.c:17:10: fatal error: 'poll.h' file not found

With a stand-in `poll.h` that only declares `poll`, every file compiles
with no warnings and the symbol check finds everything but `poll`:

    ouroboros-build: as-ouroboros: 9 files compile with no warnings
    ouroboros-build: symbols: not in picolibc or Ouroboros's port: poll
    ouroboros-build: edit-warnings: 9 files compile with no warnings

So `poll` is the only thing missing. What else differs from the Mac, the
platform file does without: no `sigaction` (nothing to catch here), no
`link`, `fsync` or `fchmod` (the backup is the old file renamed, as your
`rename` from `2026-10-05-from-edit-rename.md` allows), and no bracketed
paste sequence.

Two things the file relies on that you may want to know: `open` ignores
`O_EXCL` (the file does not ask for it), and a save does no `fsync`, so it
takes a file to be on the disk once `close` and `rename` have returned.

## Done when

- In DevTools, `make -C edit ouroboros-build` prints the three lines above
  with `symbols: all N found, in picolibc or Ouroboros's port` in place of
  the second.
- In an image built from your `main`, `edit hello.txt` opens the editor
  full screen; an arrow key moves the cursor; `^Q Q` repeats a command
  until a key stops it, and a digit typed during it changes its pace;
  `^K D` saves and returns to the shell, and `cat hello.txt` shows what was
  typed.

## Reply

Accepted 2026-10-08 by Ouroboros, and started the same day: on
`docs/ROADMAP.md` under "What Edit asks of Ouroboros", as "`poll` and
`/bin/edit`".

Checked against `main` at `5da222c`: there is no `<poll.h>` on the picolibc
side (and picolibc's prebuilt `libc.a` defines no `poll`), and `open` in
`libc/src/file.c` ignores `O_EXCL`, as the note says.

The plan. `poll` goes in the C library over what the kernel already has, no
new system call: fd 0's readiness is a `TRY_READ_CHAR`, and the byte it
takes is held for the next `read(0, ...)`, so `poll` answering 1 means that
read does not block; a timeout of -1 is a blocking `READ_CHAR`; a positive
one waits on `MONOTONIC_US`, giving the core away (`YIELD`) between looks.
It flushes fd 1 first, as `read(0, ...)` does. Any descriptor but fd 0 is
answered `POLLNVAL` at once. `TCSAFLUSH` discards the held byte too, since
it is input typed ahead. `/bin/edit` is built as `cpp-bin` builds cpp, from
`EDIT_DIR` (default `../DevTools/edit`), optional unless `NEED_EDIT=required`,
and staged only when the sources are there.

Progress 2026-10-08, Ouroboros: both are built (#240). `poll` and `<poll.h>`
are in the C library as planned above; `make test-cpoll` measures a 200 ms
wait at 200 ms and a key ending a -1 wait. `make edit-bin` builds Edit from
`EDIT_DIR` with no warnings, staged as `/bin/edit`, and `make test-edit` runs
most of your "Done when" on the serial console: `edit hello.txt` opens,
`^Q Q ^G` repeats until a key stops it, Left moves the cursor, `^K D` saves,
and `cat` shows the text. It does not check that a digit typed during
`^Q Q` changes the pace; that part of your check is still yours to run. One thing your platform file may want to know: `poll`
flushes the C library's buffer for fd 1, the one `write` fills, not stdio's
above it (as on Unix), so text written with `printf` needs an `fflush`.

Revised 2026-10-08 after #240's three high reviews: a timed `poll` no
longer spins and no longer takes the key. A new system call,
`KEY_WAIT_UNTIL` (71), blocks until a key is waiting or the deadline passes,
and leaves the key in the kernel for `read`, so a program that polls and
exits without reading leaves it for the shell. **One departure from the
note: the timing is to a tick, about 20 ms, not the 10 ms asked for.** The
kernel looks at a blocked task once a tick, so a wait ends at the first
tick at or after its deadline (a 200 ms wait measures about 213 ms), and
later when a tick is delayed or another program is busy. Meeting 10 ms
meant spinning on the clock for the last stretch, which burned a core
through `^Q Q`'s 25 ms polls; Hans chose tick precision over that. For
Edit, the 50 ms escape wait becomes up to about 70 ms, and `^Q Q`'s pace
steps of 25 ms land on 20 ms ticks. If that is not good enough for Edit, a
note saying so is the way: a finer kernel timer is the fix, not a spin. Descriptors other
than 0 are now answered as POSIX has them (fds 1 and 2 ready for `POLLOUT`,
an open file ready, `POLLNVAL` only for one not open), and fd 0 is answered
even when another entry is already ready.

# Ctrl-C as a key: the plan

**Item 4 of DevTools's editor note, designed before any code.** Edit
(`~/Projects/DevTools/edit/`) is modeless and WordStar-style, and in WordStar
^C is "page down". On Ouroboros a 0x03 never reaches a program: the kernel
takes it, at the one place every keystroke passes, as the order to end the
foreground program. The note
(`docs/handoffs/2026-10-05-from-edit-editor-console.md`) asks for "a
per-program opt-out of the kill", and Ouroboros's reply of 2026-10-05 put
the condition on it: the kill is the only way out of a runaway program, so
an opt-out must keep a way out.

**Status 2026-10-07: step 0 built (#232), step 3 half built (#233: whole
keys in the queue; the tail on a change of owner not yet); steps 1, 2 and 4
not started. The four decisions (D1 to D4)
were settled by Hans the same day, each as recommended. D1's condition is
met: DevTools answered 2026-10-08 that Edit does not bind ^\ and plans no
use of it (only ^P ^\, a literal 0x1c, is lost, and Edit accepts that), so
nothing now waits before step 1:
`~/Projects/DevTools/docs/handoffs/closed/2026-10-07-from-ouroboros-ctrl-backslash.md`.**
Grounded in the code at `cec028c`.

## How Ctrl-C works today

- **One choke point.** `syscall.rs`'s `poll_keyboard_byte(reader)` is the
  only place a keystroke is consumed: the read syscalls (`READ_CHAR`,
  `TRY_READ_CHAR`), the scheduler's wake-check for a task blocked on the
  keyboard, and the tick. It answers only the keyboard owner
  (`tasks::input_owner`), reads the serial console first and then the USB
  keyboard's queue, and hands every byte to `tasks::interrupt_key_check`.
- **What a 0x03 does there.** When the owner is not the boot shell, the byte
  is swallowed, and the owner is either *terminated* (a foreground command:
  `FOREGROUND_COMMAND` set at `FG`, or a task blocked in `WAIT` on it;
  `PENDING_KILL` is marked and `on_tick` does the kill) or *detached* (a
  session handed the keyboard by a bare `fg`: the keyboard goes back to whoever
  handed it over, and the session lives). At the boot shell a 0x03 is an
  ordinary byte the line editor ignores.
- **The tick reads too.** `on_tick` polls one byte for a *running* foreground
  task (one not blocked in a read), so a Ctrl-C reaches a runaway loop that
  never reads. Any other byte it reads is thrown away.
- **Nothing is delivered.** There are no signals; a program cannot catch
  Ctrl-C to save first. Ctrl+Z and Ctrl+\ are not special: Ctrl+Z (0x1a)
  already reaches programs, and Ctrl+\ produces nothing on the USB keyboard
  (`keycode_to_ascii`'s Ctrl branch maps letters only).

Two hazards in the same path, from the high review of #229, belong in this
design because its fix touches the same code:

- **The tick drops a byte.** A program that is running rather than blocked
  in a read when a key arrives (an editor redrawing) loses one byte of it to
  the tick. Before #229 that lost a whole key; now it can be a byte from the
  middle of `ESC [ A`, and the program reads `[A` or `A` as text.
- **A sequence can be split between owners.** A key's sequence is queued
  whole but handed out a byte at a time, so a program killed between the ESC
  and the rest leaves the tail for the next owner.

## What Edit asks for, in its own terms

Edit's Mac version enters raw mode the POSIX way (`platform_posix.c`,
`plat_raw_on`): `tcgetattr`, then `tcsetattr` with `ISIG` cleared, "so ^C, ^Z
and ^\ are keys, not signals", along with `ICANON`, `ECHO`, `IEXTEN`, `IXON`,
`ICRNL` and `OPOST`. `plat_raw_off` restores the saved settings, and it runs
at exit too. If Ouroboros gives a C program the same two calls, with `ISIG`
meaning what it means on Unix, the port needs no new idea at all.

## The design

**1. A per-task keyboard mode, `KBD_MODE` (syscall 70).** `KBD_MODE(mode)`
sets the calling task's mode: `KBD_COOKED` (0, the default) or `KBD_RAW`
(1); `KBD_MODE(KBD_QUERY)` reads it. In raw mode, `interrupt_key_check` lets
0x03 through as an ordinary byte. The mode is per task, so it ends with the
program by construction: it is cleared on death with the slot's other state
(`tasks.rs`'s death path, beside `clear_env`), a spawned child starts cooked,
and it matters only while the task owns the keyboard. A task sets only its
own mode, so a program cannot take Ctrl-C away from another.

**2. A way out that raw mode does not touch: Ctrl+\ (0x1c).** In every
mode, 0x1c does what 0x03 does today: terminate a foreground command, detach
a session. It is Unix's quit key, so it is the one a user reaches for after
Ctrl-C fails, and a raw program cannot opt out of it. The USB keyboard's Ctrl
branch learns Ctrl+\ (HID 0x31) so that it can be typed; a host terminal on
QEMU's serial line already sends 0x1c for it. (D1, D2.)

**3. `termios` in the C library.** Picolibc's `<termios.h>` includes a
`<sys/termios.h>` the prebuilt picolibc does not have, so the header is
Ouroboros's, in `libc/pico/include/sys/termios.h` beside #230's
`sys/ioctl.h`. `tcgetattr` and `tcsetattr` on the console fds keep a
`struct termios` per process. `ISIG` maps onto `KBD_MODE`: set is cooked,
clear is raw. Every other flag is stored and read back but changes nothing,
because nothing in Ouroboros implements it: input is already a byte at a
time with no echo and no line editing in the kernel (`ICANON`, `ECHO`), Enter
already arrives as 13 (`ICRNL`), there is no flow control (`IXON`) and no
output translation (`OPOST`). The note in the header says so, flag by flag.
On a descriptor that is not the console, `ENOTTY`, as `ioctl` does. Edit's
`plat_raw_on` and `plat_raw_off` then work unchanged; the one difference
from Unix to tell DevTools is that ^\ is never a key.

**4. The tick keeps what it reads.** A small input queue in the kernel (64
bytes) sits between the devices and `poll_keyboard_byte`'s callers. The tick
still reads, so a Ctrl-C (or Ctrl+\) reaches a runaway loop, but a byte that
is not the way out goes into the queue instead of being dropped, and the
next read takes it from there. This also helps the serial console, whose
PL011 has no receive FIFO: a byte read at the tick is kept rather than
overrun. It closes the first hazard.

**5. A cut sequence is not left for the next owner.** When the keyboard
changes owner because the old one died, the kernel drops an *incomplete*
escape sequence from the head of the queue, using the `keyseq` crate
(#229), which is pure and `no_std` and so builds into the kernel. Complete
keys typed ahead stay, as they do on Unix. It closes the second hazard. (D3.)

**6. The shell resets reverse video after a kill.** A program killed between
`ESC [ 7 m` and `ESC [ 0 m` leaves `cond` drawing inverted (recorded with the
review of #228). When the shell's `WAIT` returns `TASK_KILLED_STATUS`, it
writes `ESC [ 0 m` before its prompt. Cheap, and right on a serial terminal
too. Not a kernel job: the kernel does not know what the program wrote.

## Decisions (settled by Hans, 2026-10-07, each as recommended)

- **D1. The way out in raw mode.** *Settled: Ctrl+\ (0x1c), always.* The
  alternatives: three Ctrl-C within a second (no new key to learn, but Edit's
  ^C is page down, and holding it to scroll would kill the editor); or no way
  out (the opt-out ends with the program, and a runaway raw program is ended
  only by `kill` from another shell, which needs a second session the
  console does not have). DevTools confirmed 2026-10-08 that Edit does not
  bind ^\ (WordStar does not); its ^P ^\, which inserts a literal 0x1c,
  ends Edit here instead, a cost Edit accepts.
- **D2. Ctrl+\ reserved in cooked mode too.** *Settled: yes*, so the way
  out is one key whatever the mode, and a user never has to know which mode
  a program is in. The cost is that 0x1c never reaches any program.
- **D3. What happens to type-ahead when the owner dies.** *Settled: keep
  complete keys, drop only an incomplete sequence at the head.* The
  alternatives: flush everything (simplest, and loses what the user typed
  for the shell while the program ran); or keep everything (the current
  behaviour, and the second hazard).
- **D4. Who may set raw.** *Settled: a task sets its own mode, whether or
  not it owns the keyboard now*, so a program can set raw before it is
  handed the keyboard. The alternative, only the current owner, is
  stricter, but buys nothing: the mode has no effect until the task owns the
  keyboard, and ends with it.

## Out of scope

- **Signals.** A program cannot catch Ctrl-C to save first. That is a
  delivery mechanism (a signal, or a "Ctrl-C arrived" byte in cooked mode)
  and a larger design; raw mode covers the editor.
- **Ctrl-C and pipelines.** What Ctrl-C means for a pipeline (kill the group,
  detach) is an open question on `docs/ROADMAP.md`; raw mode applies to the
  task that owns the keyboard, as today's kill does.
- **Escape on the USB keyboard.** It sends nothing, and mapping it collides
  with every sequence's first byte; recorded on the roadmap separately.

## Steps, each with its check

0. **The input queue (design 4).** The tick's byte goes into the queue.
   Check: a C probe that spins between reads, keys pressed during the spin
   by QEMU's `sendkey`, every byte read back; fails with the tick's drop
   restored. *Built 2026-10-07 (#232): `syscall.rs`'s
   `read_keyboard_ahead` and `KBD_QUEUE`, bounded per call because it runs
   with interrupts masked; the probe is `/bin/READKEY spin [ticks]
   [io|keep]` rather than a C program, and `make test-kbd-queue` its rig.
   Its high review found that the waits for a child's exit and for a
   message also read the keyboard for the owner and threw each byte away,
   so an editor blocked in `con_write` still lost keys: both now read ahead
   (`keyboard_interrupts_wait`), and the tick reads ahead for the owner
   whoever it interrupted. **A Ctrl+C now flushes the queue**, as Unix
   flushes pending input on an interrupt: a typed `rm foo` and Enter, then
   Ctrl+C, must not run in the shell after the kill. That refines D3, which
   keeps type-ahead when a program ENDS; it is flagged for Hans. A full
   queue can still cut a sequence (a byte dropped mid-key); that is step
   3's, which brings `keyseq` into the kernel. Found on the way: the driver
   takes one USB keyboard report per poll, so a busy program keeps up with
   about 25 keys a second, and QEMU's keyboard drops what is faster
   (recorded on `docs/ROADMAP.md`).*
1. **`KBD_MODE`, Ctrl+\ (design 1 and 2).** Check: `/bin/READKEY raw` (a new
   mode of the existing probe) prints 3 for Ctrl-C and dies on Ctrl+\;
   cooked, Ctrl-C still kills; a raw program's child starts cooked; the mode
   ends with the task. Each fails with its part removed.
2. **`termios` (design 3).** Check: a C probe sets raw through `tcsetattr`,
   reads Ctrl-C as 3, restores, and is killed by Ctrl-C; `tcgetattr` reads
   back every flag it set; `ENOTTY` on a file.
3. **The cut sequence (design 5).** Check: a probe reads only the ESC of an
   arrow, is killed, and the shell's next line holds nothing of the tail.
   Also here: a full queue must refuse a whole key rather than keep the
   front of one (found by the high review of #232). *Half built 2026-10-07
   (#233): the queue parses what it holds with `keyseq` (now a kernel
   dependency) and takes a key that does not fit back out whole, a control
   byte ending a cut key kept, and a Ctrl+C flush that cuts a key drops its
   rest; checked in `make test-kbd-queue`. **The tail on a change of owner
   is not built.** A first version kept a second parse of the bytes handed
   to readers and dropped a key's rest when the owner changed; its high
   review found that state going stale (a Ctrl+C flush left it mid-key, so
   the shell lost the first letter typed after a kill; a serial terminal's
   bare Escape left it waiting for a tail that never came, so the next key
   was swallowed; and its drop loop ran unbounded with interrupts masked),
   and it was taken out rather than patched. The design to build instead,
   the review's: one parser, the queue's, which records where each key
   starts; every byte reaches readers through the queue; an owner change
   trims the queue's front to the next key start, and drops a key's rest as
   it arrives only within the same tick, since a bare ESC's next key comes
   later and a sequence's rest does not.*
4. **The shell's reset (design 6).** Check: on `ramfb`, a probe killed in
   reverse video, then the prompt drawn normal (`test-cond-vt.py`'s decoder).

Then a notice to DevTools: items 1 to 4 done, the ^\ difference, and how
`plat_raw_on` behaves here.

# The boot log: the plan

*Asked by Hans on 2026-11-20, before Pi net step 10: "I only have one
serial port to read. We probably need to create a boot log and driver log
functionality so it's possible to download the log files without having a
serial port." It closes `ROADMAP.md`'s open item "A kernel log buffer and
`dmesg`" (the framebuffer section) and `gap-analysis.md`'s "Logging
framework / `dmesg` / syslog" row, both open since before the Pi.*

## Why

Every board is read today over its serial port, by eye, from a capture on
the Mac (`screenlog.0`). Pi net step 10 puts two boards on one switch, and
the Mac has one serial adapter. A cluster of N boards cannot have N serial
cables, and a board run headless with no cable at all says nothing of its
boot to anyone. On a framebuffer console the case is worse: `cond` clears
the kernel's lines within seconds of the boot (`ROADMAP.md`, the
`MADT: 4 cores` lines not seen on Parallels, 2026-10-12).

What is wanted is the console's output kept by the board itself, readable
from the board, from another node and from the Mac, and kept on the card
for a board that hangs once it is up.

**Done when** a Pi 4 with no serial cable boots, and its whole console
output from the firmware's handoff to the present is read from the Mac
through the 9P export, matching a serial capture of the same boot line for
line; the same is read from a second board through `mount -r`; and after a
hang planted once `fsd` is up, the card read on the Mac holds the boot's
lines up to within a few seconds of the hang.

**What it does not do:** show a board that dies before `fsd` and `netd`
serve. That is the serial port's, or the framebuffer's, and stays so. For
step 10 itself, a second USB-serial adapter on the Mac is the cheaper
answer and the only one for an early hang; this arc is for every boot
after that.

## What is known (read 2026-11-20, from the code)

- **Every byte the console shows after the exit passes one function.**
  `console.rs`'s `with_console` takes the console lock, lifts `cond`'s
  cursor, writes and releases: the kernel's `println!`, the fault
  reporters' `println_force!`, `PUTC`, and `CON_WRITE`, which is how `cond`
  writes for every server and the shell when the console is a byte stream
  (serial, as on the Pi). So one hook there catches the kernel's lines and
  everything userland prints, in the order the console shows them.
- **Except on a framebuffer.** There `cond` draws glyphs itself through
  `FB_BLIT` (`fbdev.rs`); userland's text never passes `with_console`, and
  only the kernel's own lines do. Parallels and a Pi with HDMI and no
  serial console are that case.
- **Before the exit the lines go elsewhere.** `main.rs` logs through
  `uefi::helpers::init()`'s logger, which writes to the firmware's console;
  the kernel's own console is installed only after the exit. Those lines
  (the build, the timer, the image, the console found, the MADT, the
  loader, the GENET declared, about forty on a Pi boot) are the ones most
  wanted after a bad boot.
- **A boot is about 10.5 KB to the login prompt** (boot 50 in
  `screenlog.0`, firmware handoff to `login:`), and grows with each
  command run and each driver line.
- **`/proc` is `fsd`'s synthetic arm** (`programs/servers/fsd/src/proc.rs`),
  each file built from a syscall (`CORE_INFO_UP` for `/proc/cpus`,
  `TASK_STACK_FREE` for `/proc/<n>/stackfree`). A file there is reachable
  everywhere a path is: the shell, `mount -r` from another node, the 9P
  export, and `scripts/np9p_client.py` on the Mac, the foreign observer
  the cluster rigs already use.
- **The HTTP server reads as `nobody`** (`netd`, `As::Anonymous`), so a
  root-only file is not served on port 80, by design (it once served
  `/etc/shadow`).
- **The console echoes what is typed.** A command line, with any argument
  in it, would be in the log. Passwords are not echoed (login, `passwd`),
  so they are not.
- **Each boot is numbered** (`bootid.rs`, the persisted counter, `BOOT_ID`),
  so a file per boot can be named for it.
- **The kernel cannot write a file.** `fsd` owns the disk (`BLOCK_*` answers
  it alone, `make test-mount-gate`); anything put on the card goes through
  `fsd`.

## The design

1. **One ring, filled where the console is written.** A byte ring in kernel
   `.bss`, written by `with_console` under the console lock it already
   takes, so another core's write cannot interleave inside it, and before
   the quiet test (`set_quiet` silences the console, not the record). Each
   write is a copy into the ring and a moving index: no allocation, no
   lock of its own, nothing that can fault.
2. **The lines from before the exit go in too.** The kernel installs its
   own `log::Log` in place of `uefi::helpers`' logger, formatting each
   record once and handing it to the firmware's console and to the ring. The
   ring is a static, so it is there from the first line.
3. **A syscall reads it, from an offset.** `LOG_READ(offset, buf, len)`
   answers bytes from the ring at an absolute offset (bytes written since
   boot) and the current end, or says the offset has been overwritten, so a
   reader can follow it without missing or repeating a byte. Gated to `fsd`,
   which serves it as a file.
4. **`/proc/log`, root-only.** `fsd`'s `/proc` arm serves the ring as a
   file of mode 0400, so it reads like any other: `cat /proc/log` on the
   board, `cat /mnt/b/proc/log` from another node, and
   `np9p_client.py` from the Mac, each as root. Not over HTTP.
5. **A copy on the card that outlives a hang.** `/var/log/boot.<N>` on the
   root, `N` the boot number, appended every few seconds with what the ring
   gained, by `fsd` (which owns the disk and already runs for the whole
   boot) or by a small task of its own (decision D3). A board that hangs
   once it is up leaves its log on the card up to the last flush. The
   oldest files are removed past a count, so the card does not fill.

## Decisions for Hans

- **D1. The ring's size and shape.** *Recommended:* 256 KB, the first 64 KB
  kept whole (the boot, which a long session would otherwise overwrite)
  and the rest a ring. *Instead:* one plain ring, simpler, where a long
  session loses the boot's lines in memory (the card's copy keeps them).
- **D2. What goes in.** *Recommended:* exactly what the console shows, no
  more, so the log and a serial capture agree line for line, which is the
  check. *Instead:* also a driver level that never reaches the console (a
  verbose GENET or xHCI), which needs levels and a filter, and is a
  separate step if wanted.
- **D3. Who writes the card's copy.** *Recommended:* `fsd`, on a timer in
  its loop: no new server, no new restart rig, and it owns the disk. Its
  cost is that a wedged `fsd` stops the flush, which is also when a disk
  write would fail anyway. *Instead:* a small `logd`, which keeps `fsd`
  unchanged but is a fifth supervised server.
- **D4. Who may read it.** *Recommended:* root only (0400), since it holds
  every command line typed. *Instead:* world-readable, as Linux's
  `dmesg_restrict=0`, and served on port 80.
- **D5. The framebuffer case.** *Recommended:* out of this arc; the ring
  holds the kernel's lines there, and userland's text on a framebuffer is a
  later step (`cond` would copy what it draws through a `CON_LOG` call).
  *Instead:* in this arc, at the cost of a change to `cond`.
- **D6. The card copy in this arc, or after.** *Recommended:* in it: a hung
  board is the case the log exists for. *Instead:* steps 1 to 4 first, the
  card's copy its own arc.

## Steps, each with its check

0. **The ring, on the host.** A pure crate (`logring`) holding the ring's
   logic over a byte buffer the caller lends: the write, the kept head
   (D1), the read from an absolute offset, the overwritten answer.
   **Check:** host tests in `make test`, each mutated: a write that wraps,
   a read across the wrap, an offset just overwritten, the head kept past
   a wrap.
1. **The kernel fills it.** The ring in `.bss`, written in `with_console`
   and by the kernel's own logger before the exit (design 1 and 2).
   **Check:** on QEMU, a rig boots and compares the ring, read by a probe
   program, with the serial transcript from the firmware's handoff;
   controlled with each hook removed (the pre-exit lines missing, then the
   post-exit ones).
2. **`LOG_READ` and `/proc/log`.** The syscall, gated to `fsd`, and the
   file, 0400. **Check:** the rig reads `/proc/log` as root and is refused
   as a user, and reads it over the 9P export with `np9p_client.py`, the
   foreign observer, against the transcript; `make test-mount-gate`'s shape
   for the gate (another task's call refused).
3. **The card's copy.** `/var/log/boot.<N>` appended on a timer, the oldest
   removed past a count (D3, D6). **Check:** on QEMU, a boot with a hang
   planted after `fsd` is up (the restart rigs' way: a ping to a
   TEST-NET address), the image read on the host after: the file holds
   the lines up to the hang, less at most one interval; and a run of more
   boots than the count leaves the count.
4. **The board.** A Pi 4 with no serial cable: `/proc/log` read from the
   Mac through the export, then the same boot with the cable, compared line
   for line. The planted hang, the card read on the Mac.
5. **The records.** `testing-pi4.md` on reading a board with no cable,
   `make-targets.md` for the rigs, `gap-analysis.md`'s logging row,
   `ROADMAP.md`'s `dmesg` item closed with a pointer here,
   `docs/manual.md` for `/proc/log`.

## Out of scope

- Log levels, a syslog protocol, sending the log over the network as it is
  written (the reading is pulled, never pushed).
- A board that hangs before `fsd` serves: serial or the framebuffer.
- Userland's text on a framebuffer console (D5, if deferred).
- Rotation by size or age beyond a count of boot files.

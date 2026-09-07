# Moving the console into userland: a microkernel postmortem

A write-up of the milestone that moved the console out of the EL1 kernel
of [Ouroboros](../README.md), a from-scratch ARM64 microkernel, into an
unprivileged, protected userland server — the second component isolated
this way, after the filesystem. It's a companion to
[`isolation-and-dataflow-postmortem.md`](isolation-and-dataflow-postmortem.md),
which covers the earlier arc that made isolation MMU-enforced in the
first place; this one is the next step: *proving the pattern
generalizes* to a hardware-adjacent component.

Like the others, it's kept separate from the project's own history
(`CLAUDE.md`, `CHANGELOG.md`) because most of it isn't Ouroboros-specific.
If you're building a microkernel and wondering how to move a service the
kernel itself still needs, or why routing output through IPC exposed a
scheduler bug that batch tests hid, some of this may save you time.

## The one idea: a driver the kernel depends on is a *split*, not a move

The filesystem was a clean move: the kernel never needed FAT32 for
itself, so all of it could leave, with the kernel keeping only a raw
block-device syscall underneath. The console is different. The kernel
prints through it constantly, with no userland available: every boot
message before the first process exists, and — the load-bearing case —
every fault report, which can fire when the scheduler isn't even running.
You cannot move the console wholesale into a userland server, because the
thing that reports "the userland server crashed" *is* the console.

So the milestone is a **split**, not a move:

- The **steady-state** console (everything userland prints during normal
  operation) moves to a server, `cond`.
- The kernel keeps a **minimal emergency console** for its own boot and
  fault output.

That framing — "keep an early/panic console, move the rich one" — is how
real kernels are structured (printk vs. a display server), and naming it
up front kept the rest of the design honest. The interesting engineering
is all in the seams it creates.

## The framebuffer-access decision: gated primitives, not mapped memory

For the server to render text on a framebuffer, it needs to reach the
framebuffer. Two ways:

1. **Map the framebuffer into the server's own address space.** The
   purest "driver owns its hardware" — the server writes pixels directly,
   zero-copy.
2. **Keep the framebuffer in the kernel behind dumb, gated primitives**
   (blit a run of glyphs, scroll, clear); the server owns the *logic*
   (font, cursor, wrap, scroll decisions, ANSI) and calls the primitives.

Option 1 needed a capability the kernel's MMU didn't have: per-address-
space access to a *device* region (only ordinary task RAM was ever made
user-accessible). Building that is exactly the class of MMU change that
had already bitten this project once — a per-task-ASID optimization that
passed every emulator test and then faulted real hardware, and had to be
reverted. Taking that risk to move a *text console* was a bad trade.

Option 2 won, and it's a defensible microkernel split on its own terms:
the kernel keeps a dumb 2D blitter (put these pixels here; move these
rows up), and everything that makes it a *text console* — the font, the
grid, the cursor, line wrap, when to scroll, escape-sequence parsing —
lives in the server. The gated-primitive path also reused machinery that
already existed and was trusted (the same slot-gating the filesystem
server's block syscalls use), where the mapping path was net-new. **When
the "pure" version needs a new privileged mechanism and the pragmatic
version reuses a proven one, the pragmatic version is usually right —
especially when the new mechanism is the kind your hardware has already
punished you for.**

## The real bug: routing echo through IPC exposed a scheduler lie

The server takes output over IPC. The shell now sends each printed line —
and, because a line editor echoes as you type, *each typed character* —
as a synchronous call to the server. The first real-hardware-shaped test
dropped characters: `help` came back as `hl`.

The cause was not in the new code. It was a latency that had always been
there and never mattered until output ran through it. A synchronous IPC
call blocks the caller and switches to "the next runnable task." With an
always-runnable idle task in the schedule, the next runnable task is
*idle*, not the server that was just handed the request — so the reply
didn't come back until the round-robin wrapped around a full tick later.
At roughly a tick per echoed character, input arriving faster than that
overflowed the hardware receive buffer and dropped.

The sharp part: **the IPC primitive's own documentation already claimed
this couldn't happen.** Its contract said a call to a server waiting to
receive "round-trips without waiting for a tick." That was aspirational —
the direct *message delivery* was implemented, but the *scheduling* still
went through the idle task. A property you documented but never enforced
is a latent bug with a note attached; the first workload that leaned on
it found it. The fix made reality match the doc: a call now switches
straight to the destination server, which runs, replies, and blocks
again, all before the next tick. Every filesystem call got faster too.

**Lesson: test on the real hot path.** A batch-output test (print a whole
line, check it appears) would never have found this — it only shows up
when the round trip happens per character, fast, against real input
timing. And when a fix makes a *documented* promise finally true, that's
a sign the promise was load-bearing all along.

## The oversight: sweep *all* the clients, or one strands

Moving output to the server means every program that prints has to be
switched from "write to the kernel" to "send to the server." The shell,
the demo programs, the pipe filter — all updated. One was missed: the
*filesystem* server, which also prints a couple of startup lines.

It compiled and ran fine, and on the byte-stream (developer) console it
was invisible — kernel output and server output both go to the same
serial stream, so interleaving is just interleaving. It only became
visible on the real target, on a screenshot from actual hardware: the
filesystem server's two lines rendered *stranded in the middle of the
screen*, at the kernel console's cursor, while everything else rendered
at the server's cursor near the top. A missed client doesn't error; it
draws in the wrong place, and only on the platform where two cursors
exist. **A "convert every caller" change needs a checklist of callers,
and the platform where a miss is invisible is not the platform to sign
off on.**

## The handoff: the kernel has to learn to shut up

Even with every userland client routed through the server, the *kernel*
still prints its own operational lines during steady state — a task
exited, a device was mounted, a diagnostic. On the developer console
those are useful and harmless. On a framebuffer-only platform, where the
kernel's emergency console and the userland server draw to the same
screen at independent cursors, they corrupt the server's output.

The fix is a real handoff: once the server owns the screen, the kernel
goes quiet on its console — a flag, armed only when the kernel's own
console is a framebuffer and a server exists to take it over, so the
developer console (a byte stream) keeps its logs. **Fault reports bypass
the flag**: a fault is worth showing even if it overwrites the server's
screen — that's the whole reason the kernel kept an emergency console.
The distinction that matters is *operational noise* (suppress) versus *a
fault* (always show), and it's worth drawing explicitly rather than
suppressing everything or nothing.

## Verifying pixels: screendumps, not logs

A text console's output can be checked by reading the bytes. A rendered
framebuffer can't — "the driver didn't crash" says nothing about whether
the right pixels landed. So this milestone leaned on capturing the actual
framebuffer to an image and looking at it: three captures on the emulator
(text with correct line-wrap; a screen that a `clear` actually blanked —
which the kernel's old renderer never managed, having no escape parser;
and many screens' worth of output scrolling cleanly), then the real
hardware. The staging was emulator-first throughout — the byte-stream
backend and all the server plumbing proven with zero framebuffer risk
before any of the pixel path existed. **When your output is visual,
verify it visually; a returned success code is not a rendered glyph.**

## Takeaways

- **A component the kernel itself depends on splits; it doesn't move.**
  Keep the minimal in-kernel version for the kernel's own use, move the
  rich version to userland.
- **Prefer reusing a proven privileged mechanism over building a new
  one** — especially a new MMU capability, on hardware that has already
  shown it will fault on MMU changes the emulator accepts.
- **A documented invariant you never enforced is a latent bug.** The
  first workload that relies on it will find it; here it was per-character
  echo relying on sub-tick IPC that wasn't actually sub-tick.
- **Test on the real hot path.** The scheduler latency was invisible to a
  batch test and obvious to per-character echo against real input timing.
- **"Convert every caller" needs a caller checklist**, and don't sign off
  on the platform where a missed one is invisible.
- **Separate operational noise from faults** when deciding what a shared
  emergency console suppresses.
- **Verify visual output visually.** Screendumps caught what no log could.

None of these are specific to ARM or to this hypervisor; they're the kind
of thing that's obvious afterward and expensive to learn by shipping. If
any of it saves you a bad afternoon, it did its job.

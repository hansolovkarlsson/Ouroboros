# Enforce the isolation, then rebuild the data flow: a one-day microkernel postmortem

This is a write-up of one day's work on
[Ouroboros](../README.md), a from-scratch ARM64 microkernel, that took
it from *isolation as a polite convention* — tasks that could read each
other's memory, a userland crash that halted the whole kernel — to
**MMU-enforced isolation with fault containment**, and then rebuilt the
data paths that enforcement broke. Five milestones landed: EL0 fault
isolation, pipelines, per-task page tables, a grant/safecopy IPC
primitive, and a FAT32 offset-write.

It's a companion to this project's earlier debugging postmortems, which
cover an entirely different era — getting the kernel *running* on real
hardware at all:
[`boot-bringup-postmortem.md`](boot-bringup-postmortem.md) (exception
vectors, the MMU, console discovery),
[`shell-and-filesystem-postmortem.md`](shell-and-filesystem-postmortem.md)
(the road to a disk-backed shell),
[`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md) (a USB
keyboard driver), and
[`usb-storage-postmortem.md`](usb-storage-postmortem.md) (a real disk on
Parallels). Those are bug-hunt narratives — five real-hardware faults,
decoded register by register. This one is different in character: the
day was **design-led and largely smooth**, so what's worth sharing here
is a *design* story, with a handful of "caught by reasoning, not by a
crash" moments and one genuine works-on-the-emulator-fails-on-silicon
reversal.

Like the others, it's kept separate from the project's own historical
record (`CLAUDE.md`, `CHANGELOG.md`) because most of it isn't
Ouroboros-specific. If you're building a microkernel and about to move a
driver into userland, or wondering how processes move bulk data across a
hard isolation boundary without a shared heap, some of this may save you
time.

## The one idea the whole day turns on

A microkernel's pitch is isolation: put the filesystem, the drivers, the
network stack in ordinary user processes, so a bug in one can't corrupt
the kernel or its neighbours. By the start of this day Ouroboros had
already done the structural part — the FAT32 filesystem ran as a
userland server (`fsd`, task 2), reached over IPC, and the kernel held
only raw block-device access.

But the isolation was **make-believe**. Every task's memory was
accessible to every other task; nothing but good behaviour stopped the
shell from scribbling on the filesystem server's memory. And a wild
pointer in *any* user process didn't fault that process — it halted the
entire kernel. A microkernel whose whole point is containment was
converting every userland fault into a total-system halt, and calling
memory isolation "enforced" when it was enforced by nobody.

The day's arc is the consequence of fixing that honestly:

> **Enforcing isolation is the easy half. The hard half is that real
> isolation breaks every cheap way processes were moving data — shared
> memory, passed pointers, "just read it from over there" — and you have
> to rebuild each one as an explicit, authorized, copied operation.**

Fault isolation and per-task page tables are the enforcement. FSOP v2,
grant/safecopy, and the FAT32 offset-write are the rebuilt data paths.
Pipelines are data flowing between processes on top of all of it.

## Part 1 — Containing a crash: EL0 fault isolation

The first finding was worse than the feature it motivated. The plan was
modest — restart a filesystem server if it wedges — but scoping it
surfaced that **any EL0 fault halted the whole kernel.** The AArch64
exception vector for "synchronous exception from a lower EL" had exactly
one resumable path: the `svc` (syscall) trap. Every *other* lower-EL
synchronous exception — a null dereference, a bad pointer, a stack
overflow in any user program — fell through to the same
report-the-registers-and-halt path the kernel used for its own fatal
faults.

So the fix is two layers:

**Contain the fault.** A fourth exception trampoline: the syscall vector's
"not an `svc`" fall-through now builds the same register frame the IRQ
and syscall paths build and calls a Rust handler that tears down *only*
the faulting task (free its region, revert keyboard ownership if it held
it, reap its slot) and switches to the next runnable task — the exact
teardown a `kill` does, driven from an exception instead of a syscall.
Tasks 0 and 1 (the boot shell and the idle task) still halt honestly on a
fault: nothing meaningful survives the loss of the keyboard owner or the
idle task.

**Supervise the server.** The filesystem server is special — if it dies,
so does every disk operation, *including the one that would reload it.*
So the kernel keeps `fsd`'s raw program image in a static buffer, filled
while boot services can still read the ESP. On a task-2 fault, the kernel
reparses that image into a fresh region and re-runs the server's own
startup (probe the device, remount from disk — its state was always
disk-derivable, which is what makes this real recovery rather than
wishful restart). A per-boot cap of three restarts guards a crash loop;
past it, disk access degrades gracefully, exactly as if `fsd` had never
loaded.

**A latent bug this surfaced, worth stealing:** runtime-spawned code
never got the cache maintenance (clean D-cache to point of unification,
invalidate I-cache) that boot-loaded code did. Invisible on QEMU (TCG
models no cache incoherency) and never *observed* to bite on real
hardware — but a self-modifying-by-construction operation (writing code
bytes, then executing them) is simply wrong without it. If you load or
relocate code at runtime on ARM, do the cache dance; don't wait for the
hardware that notices.

**How it was verified:** direct fault injection, with the `-d int`
abort-count discipline this project uses everywhere adapted to *expect*
exactly the injected aborts and no others. A deliberately-crashing test
program (null write) was killed alone with the shell surviving; a
deliberately-crashing `fsd` woke its blocked caller with a clean error,
restarted 1/3 → 2/3 → 3/3 with a working server each time, then degraded
cleanly at the cap.

## Part 2 — Data between processes: pipelines

A short one, included because it's the "flow" half of the day's theme and
because it contains a lesson about testing on real hardware.

`builtin | program` now pipes a shell builtin's output into a freshly
spawned program over IPC: the shell captures the left side, spawns the
right, streams the bytes as messages, and marks end-of-stream with a
single **empty message** (a zero-length message became legal for exactly
this convention). A filter program's shape is stdin = receive, stdout =
putc, EOF = the empty message, done = exit.

The robustness piece worth copying: a right-hand program that never reads
its input (say, piping into something that only reads the keyboard) fills
the receiver's bounded mailbox, and the shell would spin forever trying
to send — and *Ctrl+C can't rescue a process that's running, only one
that's blocked.* So the send loop carries a real wall-clock deadline
(via the generic timer's free-running counter, no interrupts needed) and
kills the stuck child after a few seconds.

**The real-hardware lesson:** the scripted test typed a `|`, and nothing
happened. The HID keycode for backslash/pipe was missing from the
driver's keymap entirely — which meant not just that the test failed, but
that **no physical keyboard could type a pipeline on the actual target
platform.** A feature can be "done" on the emulator and unreachable by a
real user for a reason that has nothing to do with the feature. The fix
was one keymap entry; the lesson is that your input path is part of your
feature's surface.

## Part 3 — Enforcing memory isolation: per-task page tables

This is the load-bearing milestone, and the one with the day's only
genuine hardware reversal.

Until now every task's EL0 region was accessible to every task — one
shared translation-table set, isolation by convention. The fix: **each
scheduler slot gets its own translation-table view.** The kernel and
device mappings are identical in every view; only the EL0-access overlay
differs, granting a task EL0 reach into its own region alone. A context
switch swaps `TTBR0` and flushes the TLB. Touch another task's memory and
you take a permission fault — which, thanks to Part 1, kills only the
toucher.

**Proven, not asserted, with an A/B test.** A temporary probe that reads
one byte of the shell's region from a spawned program *succeeded* under
the old shared map and *faulted* (permission fault, faulter killed alone,
shell alive) under per-task views. Isolation you can't demonstrate
breaking is isolation you're only hoping for.

**The cascade — and the reason the rest of the day existed.** Per-task
views break exactly one thing: the filesystem server dereferencing a
client's pointer. Under the old shared map, a client passed a pointer
into its own buffer and the server just read it. Under enforced
isolation, that pointer points at memory the server's own tables deny it.
Every "just read it from the client" shortcut in the IPC protocol was now
a fault waiting to happen.

Two ways out were weighed with the project owner, explicitly, because the
choice is a security decision:

- **Map-based grants** (as MINIX is often *described*): the client grants
  the server a mapping of its buffer. Rejected — sub-page grants leak the
  *neighbouring* bytes in the same page into the server, which is the
  opposite of the milestone's point.
- **Inline payloads + kernel copies** (MINIX's *actual* production
  design, `sys_safecopy`): the request carries its data inside the
  message; the kernel copies it task-to-task; no pointer crosses a
  boundary.

Inline payloads won. The filesystem protocol became **FSOP v2** — fully
self-contained requests and replies, kernel-copied — and messages grew
from 64 to 768 bytes to carry a path plus a data buffer. This is the
*first half* of MINIX's real IPC design; Part 4 is the second half.

**The hardware reversal: per-task ASIDs.** Flushing the whole TLB on
every context switch is correct but wasteful; the standard optimization
is to tag each view's translation with an Address Space ID and mark the
EL0 pages non-global, so a switch is a cheap tagged-`TTBR0` write with no
flush. It was implemented as a **separately-committed, separately-
revertible** stage, precisely because ASIDs and stale TLBs are classic
silicon-only trouble and the emulator models TLBs loosely.

Good instinct: it passed every QEMU test *and* the cross-task isolation
probe — and then faulted the idle task on real Parallels hardware, on its
own instruction fetch, with an instruction-permission fault at its own
address. Per the plan's own contingency it was reverted to the
flush-on-switch design (correct on both platforms, re-confirmed on real
hardware), and recorded as a future optimization with the fault evidence
rather than chased at the end of a long day. Root cause is unproven — the
leading suspects are real hardware not honouring the non-global bit the
way TCG does, or a break-before-make gap when a rebuild changes a view
whose ASID has live TLB entries. **The lesson is structural: put the
optimization you're least sure of on its own commit, so "revert it and
ship the correct-everywhere version" is one command, not a bisect.**

## Part 4 — Moving bulk data under enforcement: grant/safecopy

FSOP v2's inline payloads solved correctness but imposed a cap: a request
is one 768-byte message, so no operation could move more than ~512 bytes.
`cat` truncated at 512; `cp` of anything larger refused. Growing the
message is a dead end (you can't put a megabyte in a mailbox). The right
move is MINIX's second half: let the kernel copy bulk data *directly
between two isolated regions*, under an explicit capability.

**The design, and why enforced beat simple.** Two syscalls:

- `grant(grantee, ptr, len, dir)` — a client records, in its own single
  per-task grant slot, that one server may copy an *exact* buffer in the
  client's own region, in a given direction.
- `safecopy(client, offset, local, len, dir)` — the server, while
  serving that client, copies within the grant.

The tempting cheaper design is "the server names a pointer, the kernel
checks it's inside the calling client's region." It's less code and can
never reach a *third* task. But it lets a buggy or compromised server
read or write *anywhere* in its caller's region — a trust assumption,
reintroduced one milestone after a whole milestone spent removing trust
assumptions. The project owner chose the enforced grant deliberately: the
server can touch only the bytes the client designated.

**The authorization is a conjunction, and the interesting term is
temporal.** A `safecopy` is allowed only if the grant names this server
*and* permits the direction *and the client is currently blocked in a
call to this server* *and* the ranges are in bounds. That third clause
means you need **no persistent grant table and no revocation**: a stale
grant is inert, because the instant the call returns the client is
runnable, not blocked-calling-me. In a synchronous request/response model
this is exactly enough — a client makes one call at a time, so it has at
most one grant in flight, so one slot per task suffices. Capability plus
temporal binding replaces a whole data structure.

**Two mechanical notes worth stealing.** First, the copy itself works
*across* per-task views because all RAM stays identity-mapped
read/write for the kernel (EL1) in *every* view — only the EL0-access
overlay is per-view. So a `safecopy` running in the kernel reaches both
regions regardless of which view's `TTBR0` is active; you don't switch
address spaces to copy between them. Second, `safecopy` needs five
arguments and the syscall ABI passes four in registers — the fifth
(direction) is read out of the saved trap frame's `x4`, which the
exception trampoline already spills. When you're one argument over the
ABI, the frame is already holding it.

**A correctness catch made before testing, not by a crash:** the bulk
read op carries a `want` (max-bytes) parameter. Without it, a server
reading a full chunk into its own buffer would try to `safecopy` that
whole chunk into a *client* buffer that might be smaller — overrunning
the grant. The client passing its buffer length as `want` is what keeps
the server inside the grant when the client's buffer is small. Reasoned
through from "what are the buffer sizes on each side," not discovered by
a corruption.

The payoff: `cat` streams a file of any size (loop the bulk read one
chunk at a time, never holding the whole file); `cp` and redirection
handle larger files. **Confirmed on real Parallels hardware**: `cat` of a
5.7 KB binary off a real USB stick rendered the ELF header, an embedded
`.rodata` string, and the section-name tail ~5.7 KB in — the old `cat`
would have stopped at 512 bytes and never reached it.

## Part 5 — Files that actually flow: the FAT32 offset-write

Grant/safecopy lifted the *IPC* cap but not the *filesystem* one. Every
write at the FAT32 layer was a full replace: allocate a fresh cluster
chain, write it, free the old, repoint the directory entry. It never
mutated an existing cluster and never wrote a partial sector. So `cp` and
`>>` were still bounded by a single in-memory buffer.

`write_at(path, offset, data)` writes at a byte offset and *extends* the
file without rewriting the bytes before it. Most of it reuses machinery
that existed — the cluster-chain walk, cluster allocation, the
directory-entry patch, the read side's offset-traversal template. **The
one genuinely new primitive is a partial-sector read-modify-write of file
data.** Every prior write built fresh whole clusters and had nothing to
preserve; RMW existed only for metadata (directory and FAT entries). The
rule, per sector: a full-sector write goes straight down (nothing to
preserve); a partial write into a sector that overlaps existing content
is read-modify-written (preserve the bytes outside the window); a partial
write into a sector entirely past the old end of file is zero-padded
(freshly allocated, nothing real there).

**Two things worth carrying to any streaming-write rewrite:**

*The self-destruct trap.* The old `cp` read the whole source before
writing anything, so `cp x x` was safe by construction. Streaming `cp`
truncates the destination *first* and then reads the source chunk by
chunk — so `cp x x` (a file onto itself) would truncate the source to
empty before reading a byte of it. Guarded with a same-path check
(byte-equality of the two resolved paths) before any write. When you
convert a read-all-then-write operation to a streaming one, re-audit
every safe-by-construction property: streaming changed the order of
operations, and order was doing load-bearing work.

*The coverage blind spot.* Here's the subtle one. Streaming `cp`
truncates the destination to empty first, so its size grows monotonically
and *every* write lands sector-aligned at the current end of file — the
full-sector and zero-pad branches. **Streaming `cp` never exercises the
read-modify-write branch at all.** The RMW path — the one genuinely new,
genuinely risky piece of code — only fires when appending into an
*existing, non-sector-aligned* file, which is what `>>` does. If we'd
tested `write_at` only through `cp`, the scariest code would have shipped
unexercised. A streaming rewrite of an operation can silently route
*around* the code path you most need to test; know which caller hits
which branch.

**Confirmed on real Parallels hardware** (with the owner's explicit
go-ahead to write a scratch file — the standing policy is reads-only on
the real stick): a streaming `cp` of a 5.7 KB binary read back complete;
a file created with `>` persisted across a full VM reboot; and `>>` then
appended to it, `cat` showing both the original and appended lines — the
partial-sector RMW preserving content on real silicon. Scratch files
removed afterward.

## Real-hardware findings worth their own note

Two things surfaced on real Parallels that aren't specific to any one
milestone:

**A stick present at boot displaces the keyboard.** On roughly half the
boots with a USB stick connected, the one-shot xHCI device scan enumerated
the storage device and reported *no boot-protocol keyboard* — the known
4-device-pool / enumeration-timing limitation, but far more consistent
with a real stick in the mix than "occasionally." The working path is the
platform's own designed workflow: boot with the keyboard enumerated
first, then trigger a rescan (the shell's `mount` command) for the stick
that attaches a few seconds later. If your one-shot device scan races the
hypervisor's device attachment, a runtime rescan is not a nicety, it's
the only reliable path.

**The reads-only-then-scratch-file discipline.** Real-hardware *write*
testing means writing to someone's physical device. The rule this project
follows: reads-only by default, an explicit go-ahead before any write,
and then only a *new scratch file* (existing files read, never mutated),
removed afterward. It costs a couple of extra boots and it's worth it.

## Takeaways

- **Enforcing isolation is the cheap half; rebuilding data flow under it
  is the real work.** Per-task page tables were a bounded change. The
  three milestones that followed (FSOP v2, grant/safecopy, `write_at`)
  all exist because enforcement broke a data path that used to be free.
- **A capability plus a temporal binding can replace a data structure.**
  Tying a grant's validity to "the client is currently blocked in a call
  to me" eliminated the grant table and revocation entirely, for a
  synchronous IPC model.
- **The kernel is above the isolation boundary — use that.** Copying
  between two isolated regions needs no address-space switch when all RAM
  stays kernel-mapped in every view; only the EL0 overlay is per-task.
- **Put your least-certain optimization on its own revertible commit.**
  Per-task ASIDs passed the emulator and the isolation probe and still
  faulted real silicon; reverting was one command because it was one
  commit.
- **A streaming rewrite can route around the code you most need to test.**
  `cp` never exercised `write_at`'s read-modify-write; only `>>` did.
  Map callers to branches before you trust your test matrix.
- **Re-audit safe-by-construction properties when you change operation
  order.** Read-all-then-write made `cp x x` safe for free; streaming
  took that away silently.

None of these are ARM-specific or Parallels-specific; all of them are
the kind of thing that's obvious in hindsight and expensive to learn by
crash. If any of it saves you a bad afternoon, it did its job.

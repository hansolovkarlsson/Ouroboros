# Scope it down before you build it: a one-day microkernel postmortem

This is a write-up of one day's work on
[Ouroboros](../README.md), a from-scratch ARM64 microkernel, that took the
isolation/capability model and the userland runtime from *"the mechanisms
are real"* to *"the mechanisms are general and hardened."* Five milestones
landed in a single day: an **active health-ping** for the server
supervisor, a **capability model** for who-may-call-whom, **program-to-
program pipes** (and `exec > file`), a **stack guard page**, and a
**userland heap**.

It's a companion to this project's earlier postmortems. Four of those
([`boot-bringup`](boot-bringup-postmortem.md),
[`shell-and-filesystem`](shell-and-filesystem-postmortem.md),
[`xhci-keyboard`](xhci-keyboard-postmortem.md),
[`usb-storage`](usb-storage-postmortem.md)) are bug-hunt narratives —
real-hardware faults decoded register by register. Two
([`isolation-and-dataflow`](isolation-and-dataflow-postmortem.md),
[`console-server`](console-server-postmortem.md)) are design retrospectives
of the microkernel era that followed. This one is the third design
retrospective, and its lesson is not about a bug at all. It's about
**scoping**: on this day, most of the value was in deciding what *not* to
build.

Like the others, it's kept out of the project's own historical record
(`CLAUDE.md`, `CHANGELOG.md`) because most of it isn't Ouroboros-specific.
If you build systems and have ever committed a week to a feature whose hard
part turned out to be unnecessary — or whose blocking unknown could have
been settled on the first afternoon — some of this may save you time.

## The one idea the whole day turns on

Every one of the five milestones started with an obvious, heavyweight
design — and three of the five got *smaller* the moment someone asked a
blunt question before writing code:

> Does the clean design actually need the hard part?

Twice the answer was no, and the milestone shrank. Once the hard part was a
genuine unknown that gated the entire approach, and the right move was to
**retire that unknown with the cheapest possible experiment before building
anything else** — a go/no-go gate. It failed, and failing in one build
instead of one week is the whole point.

The remaining two milestones were straightforward to build — and each
handed back a *finding for free*, something true about the system that the
work exposed without anyone going looking for it.

Here is each, in the order it happened.

## 1. The active ping: the kernel isn't a task, so it borrows a reply

The server supervisor already restarted a *crashed* server and caught a
*wedged* (infinite-loop, `Runnable`) one via a passive heartbeat. The one
gap: a server stuck **`Blocked` forever** — deadlocked on a reply that
never comes — is indistinguishable from a healthy idle server from the
outside. The only way to tell them apart is to poke it.

The obvious design is heavy: a `*_PING` request op every server implements,
a `PING_ACK` syscall, changes to every server's main loop. And it runs into
a real obstacle — **the kernel isn't a task.** It has no mailbox, it can't
`msg_call` a server and block for a reply the way a client does.

The blunt question: does the ping need any of that? It doesn't. A server
already replies to *any* message it receives — an unknown op just yields a
harmless status reply. So the kernel injects a ping as an ordinary message
(sender = a reserved `KERNEL_SENDER` sentinel), and the server's *ordinary
reply*, addressed back to that sentinel, is intercepted by the `MSG_SEND`
syscall arm as the ack. **No new syscall. No change to any server.** A
server idle in its main `recv` is woken and replies within a tick; a server
stuck mid-request never sees the queued ping and never acks — which is
exactly the signal.

The honest part, documented rather than hidden: in today's small, acyclic
server topology (`clients → fsd → cond`, and `cond` calls no one) a true
blocked-deadlock *can't actually form* — `fail_calls_to` already rescues
callers of a dying server, and the passive heartbeat restarts a
runnable-wedged one. So the active ping is a **forward-looking** guard with
no failing case to demonstrate today. That's worth saying out loud when you
ship a mechanism: *this is insurance against a future the code doesn't have
yet*, not a fix for a bug you can point at.

## 2. Capabilities as a pure function of slot

Isolation was MMU-enforced at the *memory* level but flat at the *IPC*
level: any task could message any task, and the privileged gates (disk,
console) were ad-hoc `if current == FSD_TASK` checks scattered around. The
capability model closes that — a per-task send-mask, checked at the
`msg_send`/`msg_call` boundary.

The design instinct is a per-task capability *table*: mutable state,
populated at task creation, threaded through every spawn and restart path.
But task-slot roles in this kernel are **static** — slot 0 is always the
shell, 2 always the filesystem server, 3 always the console server, 4–5
always spawnable. So a task's capabilities are a **pure function of its
slot**. No table, no mutable state, no plumbing: the entire policy is one
`caps_for_slot(slot)` function, and a restarted server or a spawned child
gets the right capabilities automatically because its *slot* determines
them.

The one subtlety worth keeping is the **reply exemption**. In a
request/response system the server replies by sending *to* the client — so
a naive "who-may-send-to-whom" mask would forbid every reply. The fix
reuses a condition the bulk-transfer primitive (`safecopy`) already needs:
a send is always allowed if the destination is *currently blocked in a call
to the sender*. That completes an authorized round trip rather than
initiating a new one. Only *unsolicited* sends are mask-checked.

The flow that shaped the policy — and the one the "does every existing path
still work?" test had to catch — was the `pong` echo server. It replies to
a `send`/`recv` client with an **unsolicited** `msg_send` (the client did a
`send` and then a *separate* `recv`, so it isn't blocked in a call), which
means the reply exemption does *not* apply and the child send-mask must
include the shell. It's easy to miss on paper and obvious the moment you
enumerate real flows. Enumerate the real flows.

## 3. Pipes: the feature I expected to "absorb" delegation didn't need it

Program-to-program pipes (`a | b` where both are programs) were queued as
the consumer for **runtime capability delegation** — the idea being that
the producer would stream directly to the consumer, which would need the
producer to be *granted* the capability to reach it.

Scoping the clean design killed that assumption. The cleanest pipe has the
shell **relay**: `producer → shell → consumer`. And `producer → shell` is
*already permitted* by the capability send-mask from milestone 2 (a spawned
task's mask includes the shell). So no capability is granted to anyone —
**the relay model needs no delegation at all.** Better still, the *same*
mechanism — "a program's stdout can be routed to the shell instead of the
console" — delivers `exec prog > file` for free (the shell captures the
output and writes it), which the direct-streaming design would not.

This is the second time in the day the heavyweight sub-feature (here,
delegation) evaporated under the question. It's also a reminder that a
roadmap item's *stated dependency* is a hypothesis, not a fact: "pipes need
delegation" was written down long before anyone scoped the pipe, and it was
wrong. The delegation milestone survives — but now honestly labelled as
*premature*, a mechanism still without a hard consumer.

**Addendum, the next day: delegation got its consumer, and it was
self-securing.** Runtime capability delegation shipped afterward as the
relay-free upgrade to program-to-program pipes: the shell hands the producer
a capability to send *straight* to the consumer, taking itself out of the
byte path. What made it small was a property that falls out of milestone 2's
static send-mask: **you may only delegate a send-capability you statically
hold**, and the check reads the static policy, not the dynamically-delegated
one — so nothing can be laundered onward (no transitive re-delegation). Only
the shell statically holds "send to a spawnable slot," so *only the shell can
authorize producer→consumer streaming* — no new capability bit, no
delegation gate; the existing static policy secures delegation by
construction. The scoping lesson held on the way in, too: I picked the
milestone expecting it to "absorb" delegation as the hard part, and the clean
relay design still didn't *need* it — delegation is an optimization (shell
out of the hot path), not a requirement, which is exactly how it was shipped.

The mechanism that did ship is a per-task **stdout target** (a task index,
`CON_TASK` by default, set at spawn), plus a tiny `SELF` syscall so the
shell can name itself as a producer's target (a foreground-spawned shell
isn't task 0, so it can't hardcode it).

## 4. The guard page that paid for itself on the first test

Every userland task ran on a fixed 8KB stack with **no guard** — the
region was `[code][stack]`, tight, so an overflow ran straight into the
program's own code, still EL0-accessible: silent corruption, no fault. The
fix is textbook: one inaccessible page below the stack, so an overflow
faults cleanly and the existing fault-isolation handler contains it (kills
just that task).

This one built smoothly and then, on the *very first* fault-injection test,
found a real bug that had nothing to do with the injection. `exec
/EFI/ORBS/HELLO.BIN` faulted the **shell itself** — `EL0 FAULT task=0
esr_el1=0x9200004f far_el1=0x5c60cfe0`, a permission fault 32 bytes below
the shell's 8KB stack bottom, inside its brand-new guard page. The shell's
own `exec` path (`spawn_path` → the filesystem server call, with its
768+768-byte request/reply buffers, plus staging and the call chain) uses
just *over* 8KB of stack, and had been quietly overflowing by ~32 bytes
into the top of its own code region *on every `exec`* — undetected because
nothing was mapped there to fault on. The guard page turned an invisible,
years-latent corruption into a loud, decoded fault the first time it ran.

The fix was to grow the stack 8KB → 16KB. But the lesson is the mechanism's
whole justification, demonstrated rather than argued: **a guard page's
value isn't hypothetical overflow protection — it's that it makes existing,
silent overflows visible.** The kind of bug that "never caused a problem"
because its corruption happened to land on bytes nobody read yet.

(One honest caveat, documented: a single stack frame larger than the
one-page guard could jump over it. Incremental overflows — recursion,
normal frame growth — hit it; a giant single local array might not. Still
strictly better than silent corruption.)

## 5. The heap, and the one-build gate that saved a week

The last milestone wanted a real userland heap: `Vec`, `String`, `Box` —
dynamic allocation for programs, and concretely a way to lift the shell's
512-byte capture cap so `cat big > file` stops refusing.

This one had a genuine unknown at its center, and it gated the *entire*
approach: **can `alloc`'s collections even link under this project's
hand-rolled PIE loader?** The loader already couldn't tolerate certain
libcore relocations (documented cases: `str` range-slicing, `memrchr`),
because prebuilt libraries on stable Rust carry `R_AARCH64_ABS64`
relocations that a `-pie` link rejects. Whether that wall extended to
`liballoc` was unknown — and everything downstream (a `GlobalAlloc`, a
free-list allocator, lifting the linker's no-static-state assert) depended
on the answer.

So the milestone opened with a **go/no-go gate**: the smallest possible
experiment that answers the blocking question and nothing else. A minimal
bump `GlobalAlloc` over a static array, `extern crate alloc`, and a
`selftest` that builds a `Vec` and a `String`. It failed at *link*, in one
build:

```
rust-lld: error: relocation R_AARCH64_ABS64 cannot be used against
local symbol; recompile with -fPIC
>>> defined in ... liballoc-....rlib(alloc-....rcgu.o)
>>> referenced by ... .rodata..Lanon...
```

Prebuilt `liballoc`'s own `.rodata` (anonymous const data that `Vec`/
`String` pull in unavoidably) carries the exact relocation the loader can't
accept. The only fix is `-Z build-std` to rebuild `liballoc` with PIE
flags — **nightly-only**, off-limits on a project that has held the
stable-only line since the relocating-loader milestone declined `-Z
build-std` for this very reason.

That is a **useful negative result**, and getting it on the first afternoon
— before writing a free-list allocator, before touching the linker script,
before any of the plumbing that assumed `Vec` — is the entire argument for
gating a risky unknown up front. The milestone pivoted (with the user) to a
**raw buffer**: a 256KB kernel-provided heap area per program, reported by
a `heap_info` syscall, used via a plain `&mut [u8]`. No allocator, no
collections, no static state — it sidesteps every obstacle the `alloc`
version would have had to solve, and it still delivers the concrete win
(the shell backs its redirect/pipe capture with it, and `cat big > file`
now captures the full 72KB and writes it in chunks). The negative result is
now recorded in `processes.md`'s relocation-gotcha list so it never has to
be re-discovered.

## What generalizes

Nothing here is Ouroboros-specific. The reusable parts:

- **Before building the heavyweight version, ask if the clean design needs
  it.** Three of five milestones shrank on this question: the ping needed
  no new syscall (a reply is an ack), the capability model needed no mutable
  table (roles are static), the pipes needed no delegation (the shell can
  relay). A stated dependency is a hypothesis.
- **When an unknown gates the whole approach, retire it first, with the
  cheapest experiment that answers only that question.** The heap's `alloc`
  gate failed in one build. A week of allocator and linker work would have
  hit the same wall at the end instead of the start.
- **A negative result is a deliverable.** "`alloc` can't be PIE-linked on
  stable" is now permanent, cited knowledge — as valuable as a feature,
  because it stops the next attempt.
- **Some mechanisms pay for themselves by making the invisible visible.**
  The guard page's first test found a real, silent, pre-existing overflow.
  You don't add a guard page to catch a *future* bug; you add it and it
  shows you the bug you already had.
- **Ship forward-looking guards honestly.** The active ping guards a
  deadlock the current topology can't produce. Saying so — "insurance, not
  a fix" — is the difference between documentation and marketing.

The day shipped five milestones and one negative result. The negative
result may be the most reused of the six.

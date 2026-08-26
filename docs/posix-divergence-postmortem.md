# The POSIX divergence: how "POSIX-ish syscalls" became a message-passing microkernel ABI

*A design retrospective (the thirteenth), and an unusual one: it covers no
single day and no bug. It's the retrospective of a **drift** — the slow,
unplanned divergence between a one-line project goal ("POSIX-ish system
calls") and what the architecture actually forced into being — noticed not
because anything broke, but because someone asked a plain question:* **"are
the syscalls POSIX, or something else?"** *Written for other microkernel /
bare-metal-OS developers, because the shape of this divergence is one many
microkernels walk into, and it's worth seeing named.*

A companion to the [isolation-and-dataflow](isolation-and-dataflow-postmortem.md)
and [console-server](console-server-postmortem.md) postmortems (the arcs that
did the actual forcing) and to the [cluster](cluster-phase0-postmortem.md)
[postmortems](cluster-distributed-postmortem.md) (where the result got its
name). The forward half — how portability comes back — lives in
[`roadmap.md`](roadmap.md); the honest user-facing trade is in
[`comparison.md`](comparison.md); the crystallized statement of what the ABI
*is* is the "Philosophy — not POSIX, not Linux" subsection of
[`architecture.md`](architecture.md).

## The starting point: a goal phrased as a feeling

`notes.txt` listed the design goals, and among them: *microkernel
architecture*, *POSIX-ish system calls*, and *draws ideas from Linux, Minix,
and Plan 9*. Read them again with hindsight and the tension is already there
on the page — but it doesn't read as tension, because "POSIX-ish" is phrased
as a *feeling*, not a specification. A shell, familiar command names, byte
exit codes, files and directories: POSIX-ish. Nobody writing that line was
promising `fork(2)` semantics or a conformant `unistd.h`. So the goal never
felt like a constraint the microkernel goal could collide with — and that is
exactly why the collision, when it came, was invisible while it happened.

And it genuinely started POSIX-shaped. The early kernel had real `fs_*`
syscalls — `list_dir`, `read_file`, `mkdir`, `rmdir`, `touch`, `rm`,
`write_file`, `mv` — trapped straight into the kernel at numbers 7–14, a
POSIX programmer's mental model rendered directly as kernel entry points. If
the project had stayed there, "POSIX-ish syscalls" would have been simply
true. The divergence is the story of why it couldn't stay there.

## Lesson 1: a goal phrased as a *feel* doesn't constrain the architecture — the load-bearing requirement does

The two goals that actually shaped the code were "microkernel" and (later)
"Plan 9." "POSIX-ish" shaped *vocabulary* — what commands are called, what
`exit` codes look like — and nothing structural. This is the first
generalizable lesson, and it's a warning: **when one of your stated goals is
an aesthetic and another is a load-bearing architectural commitment, the
aesthetic loses every conflict, silently, and you won't notice until someone
asks a direct question.** The fix is not to demote the aesthetic — a
familiar feel is a real, worthy goal — but to *notice the moment it stops
describing your kernel and starts describing only your userland*, and say so
out loud before a user has to ask.

Here, nobody said so for months. The kernel stopped being POSIX-ish around
the isolation arc, but the goal line in `notes.txt` didn't move, and no doc
reconciled them until the question forced it.

## Lesson 2: enforcing isolation *deletes* the kernel-centric syscall model — the file calls have to become messages

This is the mechanical heart of the divergence, and it is not a choice you
get to make separately from "microkernel with enforced isolation." It is a
consequence.

The [isolation-and-dataflow](isolation-and-dataflow-postmortem.md) arc took
isolation from a convention to something the MMU enforces: per-task page
tables, EL0 fault containment, a filesystem that is a *supervised userland
server* (`fsd`) rather than kernel code. The [console-server](console-server-postmortem.md)
arc did the same to the console; the network stack did it to the NIC. The
spine of the isolation postmortem was *"enforcing isolation breaks every
cheap data path, so you rebuild them as explicit authorized operations."*
The POSIX divergence is the same spine, applied to the **syscall surface
itself**:

> Once the filesystem is not in the kernel, a "file operation" cannot be a
> kernel syscall. There is nothing in the kernel to serve it. It has to be a
> *message to the server that now owns the disk.*

So the `fs_*` syscalls at 7–14 didn't get redesigned — they got **deleted**,
their exact contracts moving out verbatim as the `fsd` server's `FSOP_*`
request protocol, their old numbers left as permanent gaps (gravestones, for
ABI stability). `open`/`read`/`write`/`stat`/`mkdir`/`unlink` — the POSIX
file surface — *cannot exist as syscalls in this design*, not because
they're unimplemented but because the thing they would trap into is no longer
in the kernel. The same is true of `socket`/`connect` (that's `netd`) and of
console writes (that's `cond`).

The generalizable form: **a microkernel that actually enforces driver
isolation cannot keep POSIX's kernel-centric syscall model, because POSIX
assumes the kernel implements files, sockets, and terminals — and you just
moved all three out.** You don't decide to diverge from POSIX; you decide to
enforce isolation, and the POSIX syscall surface is collateral. The only
question left is what the message protocol looks like — which is where Plan 9
walks in.

## Lesson 3: `fork` is the POSIX primitive a microkernel can't cheaply honor — recognize which primitive your memory model actually wants

The other half of "not POSIX" is process creation. Ouroboros has `spawn`, not
`fork`+`exec`: `spawn` starts a *new* task alongside the caller, leaving the
caller untouched — `posix_spawn`/`CreateProcess`, not Unix.

This, too, wasn't a rejection of `fork` so much as a discovery that the
memory model never wanted it. The isolation work gave each task its own
translation tables and a dedicated EL0 region. In that world, the natural
unit of "make a new process" is *allocate a fresh isolated region and load a
program into it* — which is `spawn`. `fork`'s defining trick, duplicating the
caller's entire address space (cheaply, via copy-on-write), is a large piece
of machinery that serves a model — shared-then-diverging address spaces —
this kernel deliberately doesn't have. Building COW `fork` over per-task
isolation would be real work in service of a primitive nothing here needs.

The lesson: **your process-creation primitive is decided by your memory
model, not your ABI aspiration.** An isolation-first microkernel wants
`spawn`; a shared-address-space Unix wants `fork`. Trying to bolt `fork` onto
the former is swimming upstream. (Fuchsia reached the identical conclusion
and simply has no `fork`.) Name the primitive your architecture wants and
build *that* one well.

## Lesson 4: portability is a userland *personality*, not a kernel property

The reflexive worry, once you admit "not POSIX," is *"then I can never run
existing C programs."* That conflates two things that are actually separate:
having POSIX **syscalls** and being able to run POSIX **programs**. C
programs don't call syscalls; they call `libc`. So the portability target is
the *bottom edge of a libc*, whose ~20 porting stubs (`_open`, `_read`,
`_write`, `_lseek`, `_fstat`, `_sbrk`, `_exit`, a process-creation call)
translate into whatever the host actually offers — here, messages to `fsd` /
`cond` / `netd`.

This is not speculation; it's the standard microkernel answer.
**Fuchsia** — Zircon microkernel, *zero* POSIX syscalls, pure message-passing
channels, as un-POSIX a kernel as Ouroboros — runs POSIX C programs via a
userland compat layer (musl + `fdio`). MINIX3 and Plan 9's APE do the same.
A message-passing microkernel running unmodified C is *normal*.

The lesson: **don't confuse "run C programs" with "have POSIX syscalls." The
first is a userland library you port; the second is a kernel shape you
(correctly) chose not to have.** Keeping this distinction clear is what lets
you accept the divergence without panic — the portability you seemed to lose
was never in the layer you changed.

## Lesson 5: the deferral came back as the key — a POSIX fd *is* a Plan 9 fid

The nicest turn in the whole story. In [cluster Phase 0](cluster-phase0-postmortem.md)
we **deferred fids** — the uniform verbs stayed path-based (each op carries a
path, no server-side open-file handle), because a stateless path-per-op
protocol was simpler and, as Phase 1 immediately showed, *better over TCP*
(one self-contained round-trip, no session state to replicate). That deferral
was recorded as a known simplification to revisit.

The POSIX portability plan is where it comes back, because a **POSIX file
descriptor** — an integer naming an open-file handle with a cursor — is
almost exactly a **9P fid**. So the single mechanism you'd add to make the
namespace protocol "properly" Plan 9 (fids) is the *same* mechanism a libc's
fd table wants underneath. Adding it someday pays off twice, in two arcs that
looked unrelated.

The lesson generalizes past this instance: **when you defer a mechanism,
notice whether the thing you deferred is the same *shape* as something a
later goal will need.** A deferral of "the same shape as a thing you'll want
anyway" is nearly free — you'll build it once and collect two payoffs. Here,
"we skipped fids" and "we're not POSIX yet" turned out to be one gap wearing
two faces, and the fix for both is one feature.

## Lesson 6: the most important architectural clarifications sometimes have no failing test

Every other postmortem in this directory was triggered by something breaking
— a fault loop, a stalled endpoint, a packet that didn't arrive, a checker
that reported corruption. This one was triggered by a **question**: *are the
syscalls POSIX, or something else?* Nothing was wrong. The code did exactly
what it should. And yet the answer exposed that a stated project goal had
quietly stopped being true months earlier, and that the reason *why* — a
forced consequence of isolation, later rationalized by Plan 9 — was a genuine
piece of the system's design story that lived nowhere in the docs.

The lesson, and the reason this document exists: **an architecture can drift
away from its stated goals with no test ever going red. Correctness won't
catch it; only attention will.** When a plain question about your own system
is momentarily hard to answer crisply — "well, the *calling convention* is
Linux-shaped, but…" — that hesitation is the signal. It means the map and the
territory have diverged, and the divergence is usually not a mistake to fix
but a truth to write down. The output of this postmortem isn't a code change;
it's the reconciliation itself: the philosophy subsection in
`architecture.md`, the portability plan in `roadmap.md`, the honest trade in
`comparison.md`, and this account of how the drift happened — so the next
person who asks doesn't have to reverse-engineer the answer from the gaps at
syscall numbers 7–14.

## What this cost, and what it bought

It cost the POSIX syscall surface and `fork`, and — until now — a documented
account of why. It cost the ability to say "POSIX-ish" about the *kernel*
without a footnote.

It bought a microkernel that actually enforces the isolation it claims; a
syscall surface small enough to hold in your head; a uniform file protocol
that made the entire distributed cluster fall out *for free* (the same verbs
over TCP — remote mounts, remote `/proc`/`/dev/cons`/`/net`, remote `cpu`);
and a clean, twice-paying path back to C-program portability whenever that
becomes a goal. The trade the project would make again — the only correction
needed was to *admit it out loud*.

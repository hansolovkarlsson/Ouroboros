# Research: comparative directions — MINIX, Linux, Plan 9, Helix, and where they point Ouroboros next

A synthesis note across this project's four stated design influences
(the original brief: "draw ideas from Linux, Minix, and Plan 9", plus Helix as a
later fault-tolerance reference). Unlike the per-influence notes
([`research-minix-boot.md`](research-minix-boot.md),
[`research-helix-os.md`](research-helix-os.md)), which each look at one
system, this one asks a single forward-looking question: **given everything
Ouroboros has actually built, which architectural ideas from these systems
are still genuinely interesting to implement — and which one is the
standout?** It also supplies the Plan 9 material the influence notes were
missing. Sourced where it draws on outside systems (linked at the end);
Ouroboros's own state is drawn from `CLAUDE.md` / `CHANGELOG.md`.

The short answer, up front: **Plan 9's per-process namespace + uniform
file-server protocol** is the standout — it's the one idea that would
*unify* mechanisms Ouroboros has already built piecemeal (the `fsd`/`cond`
server protocols, the capability send-mask, per-task memory isolation,
delegation) into a single coherent design, rather than piling on a new
feature. Everything else is either already absorbed or better sequenced
behind it.

## First, recalibrate: what Ouroboros has already absorbed

This has to come first, because the older influence notes were written when
Ouroboros had *"no fault isolation of any kind"* and *"one program,
hardcoded"* — both long since false, which changes what's worth borrowing.
The classic microkernel checklist is largely done:

- **MMU-enforced per-task isolation** (per-task page-table views) plus
  **`grant`/`safecopy`** bulk transfer — this *is* MINIX's `sys_safecopy`
  capability-copy model, enforced, not by convention.
- **Synchronous `sendrec`-shaped IPC** (`MSG_CALL`, sender-filtered replies)
  — MINIX's IPC shape.
- **Server supervision**: a registry that restarts a crashed server from a
  kept boot image, a passive **heartbeat** that catches a *runnable* wedge,
  and an active **ping** that catches a *blocked* wedge — this is MINIX's
  reincarnation server *and* Helix's watchdog/health-monitor/recovery
  pipeline, in miniature.
- **A capability model**: a per-slot IPC **send-mask** enforced at the
  `MSG_SEND`/`MSG_CALL` boundary, extensible at runtime by **delegation**.
- **Two real userland servers** reached over IPC: the FAT32 filesystem
  (`fsd`) and the console (`cond`).

So the interesting question is no longer "what basic microkernel feature is
missing." It's "what *different* architectural idea would take this
somewhere new." That reframing points hard at Plan 9.

## The standout: Plan 9's namespaces + a uniform file protocol

Plan 9's whole design rests on two ideas: a **per-process name space** and
**one message-oriented file protocol (9P)** that *every* service speaks. All
I/O — files, network connections, process control, the window system, the
console — is expressed as file operations on named objects, and each process
composes its own private view of those objects by mounting service file
trees into its namespace (the `bind`/`mount`/`attach` operators, with union
directories joining several sources under one name). Crucially, **a mount
affects only the namespace of the process that made it** — no global state,
no special permission needed ([The Use of Name Spaces in Plan 9](https://9p.io/sys/doc/names.html),
[Plan 9 overview](https://9p.io/wiki/plan9/Overview/index.html)).

**Why this maps onto Ouroboros unusually well: Ouroboros has independently
reinvented the pieces Plan 9 unifies, but in bespoke form.**

| Plan 9 unifies… | …what Ouroboros built by hand |
|---|---|
| One 9P protocol every server speaks | `fsd`'s `FSOP_*` protocol *and* `cond`'s separate `DSPOP_*` protocol |
| Namespace = the set of services you can reach | The per-slot capability **send-mask** (who a task may message) |
| Per-process namespace, mounts affect no one else | Per-task **page-table views** (each task's own memory view) |
| `bind` / passing a mount to a child | Runtime capability **delegation** (the current frontier item) |

Adopting the Plan 9 shape would deliver three things at once:

1. **`FSOP_*` and `DSPOP_*` collapse into one protocol.** Every server —
   and every *future* server (a network stack, a synthetic `/proc`) —
   becomes reachable through the same `walk`/`open`/`read`/`write`/`stat`
   verbs, with no new ABI per server. The console becomes `/dev/cons`, the
   filesystem `/`, the process table `/proc`. *"Everything is a file"*
   stops being a slogan and becomes less bespoke code.

2. **The namespace *becomes* the capability mechanism** — the elegant part.
   Plan 9's sandboxing *is* namespace restriction: a process can only reach
   what's mounted in its namespace. That is a cleaner, more general
   statement of the capability model Ouroboros just built by hand —
   "reaching a server = having it in your namespace." And the
   **transitive-delegation gap** currently at the top of the roadmap
   frontier becomes Plan 9's `bind` (a namespace entry handed to a child).
   The two designs converge: the hard capability problem and the naming
   problem are the *same* problem in Plan 9.

3. **It subsumes several separate roadmap items** — general delegation, a
   VFS-like indirection, multi-filesystem support, program-to-program
   composition — into one mechanism instead of four features. In Plan 9 the
   namespace *is* the VFS.

**Honest scoping.** This is a large milestone, not a cheap win: a namespace
table per task, a 9P-ish protocol definition, `mount`/`bind` syscalls, and
rewriting `fsd`/`cond` as 9P servers. And the *distributed* half of Plan 9
(9P is network-transparent — mount a remote machine's file tree over the
wire) needs a network stack Ouroboros doesn't have. **Take the local half
now, defer the distributed half honestly.** But the local half is exactly
the coherent next architecture given everything that just landed — it makes
the system feel *designed* rather than accreted.

## The rest, ranked by fit

**Worth holding for the right moment:**

- **MINIX's VFS layer / a real `init` + boot image.** MINIX packs
  kernel+PM+VFS+RS+drivers into one boot image started in dependency order,
  and `/etc/rc` starts the rest. Ouroboros loads *one* configured program.
  A boot image (an `INIT.CFG` naming a *list* of programs) plus an `init`
  task that starts and supervises them is a small, natural step now that
  `spawn`/`wait`/supervision all exist. (Note: Plan 9 namespaces would make
  the VFS part moot — the namespace *is* the VFS.)

- **Helix's hot-reload with state migration.** Ouroboros restarts a crashed
  server *statelessly* — reload the kept image, re-derive state from disk.
  Helix's pause → snapshot → swap → restore → **rollback** is *live*
  replacement *with* state hand-off. It doesn't matter for `fsd` (its state
  is disk-derived), but it would matter for the first *stateful* server —
  a future network stack with live connections that can't just be
  reconstructed. Worth remembering the *shape* before building a stateful
  server, not before.

- **A unified `poll`/`select` event model (Linux and Plan 9 both).**
  Ouroboros's blocking is single-reason today (wait on the keyboard, *or*
  one message, *or* one task-exit). A task can't wait on several sources at
  once. Plan 9 does this by reading files; Linux by `poll`/`epoll`/
  `io_uring`. This becomes real the moment something needs to multiplex — a
  shell waiting on the keyboard *and* a pipe, a server waiting on requests
  *and* a timer. Premature until a multiplexing consumer exists.

**Worth adopting now as vocabulary, even without code:** Helix's explicit
**mechanism-vs-policy split** (`core/` = mechanism, `subsystems/` +
`modules_impl/` = policy). Ouroboros's scheduler and MMU are still
undifferentiated EL1 code; naming the boundary is a useful phrase to hold
future EL1-vs-EL0 decisions to, and to resist the split happening by drift.

## What to skip (cautionary)

- **Plan 9's network transparency / distributed 9P.** The *local*
  namespace+protocol idea is the gold; the "mount a remote machine over the
  network" half needs a network stack that doesn't exist. Take the local
  half, defer the distributed half — don't let the elegance of the full
  vision pull in a networking dependency prematurely.
- **Linux's monolithic model.** cgroups, the in-kernel driver model, the
  sheer syscall surface — all antithetical to the microkernel direction
  Ouroboros has deliberately walked. The *one* Linux idea worth stealing is
  the fd / VFS abstraction, and Plan 9 does that more cleanly anyway.
- **Full POSIX `fork`.** `fork` needs copy-on-write address spaces; Ouroboros's
  `spawn` (load a fresh image, no shared parent state) is a deliberately
  simpler and, for this system, better-fitting primitive. Don't retrofit
  `fork` semantics onto it.

## Side-by-side: each system's signature idea vs. Ouroboros

| System | Signature idea | Ouroboros status |
|---|---|---|
| **MINIX** | OS components as supervised, restartable user-space processes; `safecopy` capability IPC | **Largely absorbed** — userland `fsd`/`cond`, supervision + heartbeat + ping, `grant`/`safecopy`. Missing: a full server *fleet* (net, more drivers), a real VFS/PM, process trees |
| **Linux** | Monolithic breadth; the fd / VFS abstraction; unified event multiplexing | **Deliberately not pursued** as a whole; the fd/VFS + `poll` ideas are worth borrowing (via Plan 9's cleaner form) |
| **Plan 9** | Per-process namespaces + one uniform file protocol (9P); "everything is a file" | **The standout, not yet started** — and the natural unifier for `fsd`/`cond`/capabilities/delegation. See above |
| **Helix** | Hot-reload with state migration; self-heal; mechanism/policy split | Self-heal **absorbed** (supervision). Hot-reload-with-state-migration and the mechanism/policy vocabulary remain |

## Recommendation

The Plan 9 **namespace + uniform-protocol** direction is the one genuinely
interesting architectural move left: it unifies `fsd`, `cond`, the
capability model, and delegation into a single coherent mechanism, and it's
the natural next architecture given everything that has already landed.
Everything else on the list is either already built, or better sequenced
behind it. If a single next *big* milestone is wanted (as opposed to the
smaller roadmap frontier items — general delegation, FAT32 fine points,
per-task ASIDs), this is it. General capability delegation in particular
should probably be designed *as* the namespace/`bind` mechanism rather than
as a standalone send-mask extension, since Plan 9 shows they are the same
problem.

## Sources

- [The Use of Name Spaces in Plan 9](https://9p.io/sys/doc/names.html) — per-process namespaces, `bind`/`mount`/`attach`, union directories, mounts as process-local state.
- [Plan 9 wiki: Overview](https://9p.io/wiki/plan9/Overview/index.html) — the two foundational ideas (namespaces + 9P), "everything is a file."
- [9P (protocol) — Wikipedia](https://en.wikipedia.org/wiki/9P_(protocol)) — the 9P message set and its role as the universal connector.
- [In praise of Plan 9 — Drew DeVault](https://drewdevault.com/blog/In-praise-of-Plan-9/) — a practitioner's summary of what the design buys.
- This project's own [`research-minix-boot.md`](research-minix-boot.md), [`research-helix-os.md`](research-helix-os.md), and [`microkernel-comparison.md`](microkernel-comparison.md) for the MINIX/Helix material and Ouroboros's current self-assessment.

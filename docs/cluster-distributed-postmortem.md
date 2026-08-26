# Cluster Phases 1–3: the day Ouroboros went distributed

A design-and-bugs retrospective on the arc that took the cluster from a *local*
foundation to a real two-machine system: **9P-over-TCP** (Phase 1, a machine reads
another's disk), **two-node read+write** (Phase 2, a machine writes another's
disk), and **resources as files** (Phase 3, `/proc`, `/dev/cons`, `/net` — a
machine reads another's processes, writes its screen, reads its network identity).
Three arcs, three releases (v0.6.0/v0.7.0/v0.8.0), one day, one story: the pivot to
distributed and its full first payoff.

This continues [`cluster-phase0-postmortem.md`](cluster-phase0-postmortem.md) (the
*local* half — one uniform protocol + per-task namespaces + multi-mount) and rides
the transport built in [`network-stack-postmortem.md`](network-stack-postmortem.md).
Written for other bare-metal / distributed-systems builders: the lessons are meant
to travel.

## The starting point

Phase 0 had already done the hard conceptual work: every server spoke one uniform
verb set (`ninep-abi`), and each task composed a per-task namespace where a `bind`
mapped a path prefix to a mount. The whole thesis of the cluster was one sentence:

> **"Remote" is just "the same protocol, over TCP instead of local IPC."**

Phases 1–3 are the test of that sentence. If it was true, distribution would be a
*transport swap*, not new operations — and the consumers (`ls`, `cat`, the fs
helpers) would not change. They didn't. That is the first and largest lesson, and
the rest are the traps met making it real.

## Lesson 1: for a network protocol, the wire is the source of truth — not your logs

Every real bug in this arc was found in a **packet trace**, and every one of them
was *invisible* in the kernel's own output. Three examples, all from Phase 1c/2:

- The first remote `ls` returned "no filesystem mounted." The kernel log said
  exactly that — a dead end. The `tcpdump` said everything: guest SYN → host
  SYN-ACK → **silence**. The guest never ACKed. `parse_tcp` (the client-side TCP
  parser, shared with the HTTP fetch path) hard-wired the *peer's* source port to
  80, so it dropped a SYN-ACK arriving from port 564. One line. Invisible without
  the trace.
- Then reads worked *intermittently* — a readdir would succeed, the next one
  wouldn't. The pcap: some SYNs got no reply at all. A remote mount opens a fresh
  connection *per verb*, back to back, and the fixed ephemeral source port meant
  the second SYN reused a 4-tuple the peer still held in `TIME_WAIT` — silently
  dropped until it expired.
- And a first-op flake on the two-VM link: a single SYN with no retransmit failed
  the whole operation if that one packet dropped on a freshly-connected socket hub.

**The rule:** when the bug is in a protocol, instrument the *protocol*, not the
program. Verify against a **foreign observer** — `tcpdump` on the wire, a host
python 9P peer speaking the frame format, macOS mounting the guest's disk image to
`cmp` a written file byte-for-byte. Your own logs tell you what your code *thinks*
happened; the trace tells you what actually crossed the boundary, and the gap
between them is the bug.

## Lesson 2: the local IPC optimization and the network reality meet at the gateway, mirror-imaged

Locally, bulk data never rides in a message — it moves by `grant`/`safecopy`, the
kernel copying between task regions while the sender is blocked in the call. Over
TCP there is no grant; data must ride **inline** in the stream. The export gateway
is exactly where these two worlds meet, and the meeting is symmetric:

- A remote **read**: `fsd` delivers bytes into a `GRANT_WRITE` buffer (local grant)
  → the gateway frames them inline on the wire.
- A remote **write**: the gateway receives inline bytes off the wire → copies them
  into a local buffer, `GRANT_READ`s it to `fsd`, and issues the write.

The write bridge is the *exact mirror* of the read bridge, the other direction.
Naming that symmetry made Phase 2 small: once the read gateway existed, the write
gateway was "the same, inverted." **The lesson:** when a system has a local
fast-path (shared memory, grants, zero-copy) and a remote slow-path (serialize,
inline, copy), the boundary component translates between them — and if you built
one direction, the other is usually its reflection. Look for the reflection before
writing new code.

## Lesson 3: scope the mechanism to the consumer count — a special-case is cheaper than a general mechanism until the third consumer

Phase 3 added three "resource as a file" servers, and each one routed through the
export by an **explicit path prefix** (`/proc` → the proc tree, `/dev/cons` → the
console, `/net` → netd's own state) rather than the "correct" Plan 9 mechanism: a
fully **namespace-aware export** that resolves incoming paths through a composed
per-export namespace. Each prefix special-case was a handful of lines.

I deferred the general mechanism at every step, and *recorded the deferral each
time* — after `/dev/cons` (the second consumer) the note read "getting closer to
worth building; still deferred," and only after `/net` (the third) did it read "has
earned its place as the next structural step." That was not indecision; it was the
discipline working. Two special-cases genuinely *are* cheaper than the general
resolver. Three is where the special-cases start to outweigh it.

**The rule:** don't build the general mechanism on the first consumer, or even the
second — build the special-case, and *count*. Write down, each time, whether the
general version has earned its place yet. The threshold is when the Nth special-case
costs more than the one-time generalization would have. Guessing that threshold up
front (at N=1) is how you build machinery no one uses; the [Phase 0
postmortem](cluster-phase0-postmortem.md)'s Lesson 1 ("a mechanism with no visible
consumer gets merged with its consumer") is the same rule from the other side.

## Lesson 4: a new "resource as a file" is usually a new arm on an existing server, not a new server

None of Phase 3's three file servers is actually a new server task:

- `/proc` is a **fourth arm of `fsd`'s `Filesystem` enum** — and the *first
  non-disk* one, which is what turned that enum from a format multiplexer (FAT32 /
  exFAT / ext2) into a genuine VFS. No new task, no new protected scheduler slot,
  no new capability. It generates its listings from the `TASK_STATE` syscall on
  demand.
- `/dev/cons` is `con_write` — an `NP_WRITE_FILE` to the console server that
  *every task already sends to print*. `/dev/cons` didn't add an operation; it
  added a **destination** (a namespace route to `CON_TASK`).
- `/net` is served by `netd` out of its own state (`our_ip()` / `NET_MAC`), reusing
  the same `net_op` for the local client path and the export.

**The lesson:** "everything is a file" is cheap to extend when your servers already
speak a file protocol. Before spinning up a new server for a new resource, ask
which existing server *owns* the resource and whether the resource is a new *arm*
or a new *route* on it. The expensive version (a new supervised task + a
server-selector in the namespace + export routing to arbitrary tasks) is real work
you can usually avoid.

## Lesson 5: when two bindings resolve to the same server, you need a discriminator — pick a robust one and document it

`/net` created a genuine ambiguity: a **local `/net`** binding and a **remote
mount** both resolve to `server = NET_TASK` (the network server owns both the local
`/net` fs and the remote-mount TCP path). The dispatcher couldn't tell them apart
by server alone.

The discriminator is the **endpoint**: a remote mount always carries a real
`ip:port`; a local `/net` binding carries zeros. `is_local_net()` checks it, and a
local read goes to a direct `np_netlocal` NP call while a remote one goes to the
`NETOP_RMOUNT` TCP wrap. This is a load-bearing invariant (a remote mount must
*never* have a zero endpoint, which is safe because `0.0.0.0` is never a mount
target), so it is stated in the code and the ABI, not left implicit.

**The rule:** as routing generalizes, target collisions appear — two logically
different destinations that map to the same handle. Don't disambiguate by accident
(an incidental field that happens to differ); choose the discriminator
deliberately, prove it can't alias, and write the invariant down where the next
reader will see it.

## Lesson 6: choose the distributed-consistency contract, document its boundary, and don't fake the rest

Phase 2 let two machines write one disk. The temptation is to reach for
coordination — a distributed lock, multi-writer coherence. Instead the contract was
picked deliberately and stated loudly: **single-writer** (one machine writes a tree
at a time, by convention, *no lock*), **clean-disconnect** (a dropped peer fails the
next op cleanly — a distinct error, the mount goes stale, nothing half-applied — no
hang, no corruption). A single writer never tears, because `fsd` serializes all
disk access through one task. Concurrent multi-writer coherence is **explicitly out
of scope**, said so in the design, the notes, and the release.

Then the boundary was *tested*, not assumed: `SIGKILL` machine A mid-session, and
B's next remote op returns a clean error while B stays responsive locally — and A's
disk fsck-clean. Clean-fail is a claim you verify by killing a node, not by hoping.

**The lesson:** distributed consistency is a menu, not a default. The honest move is
to pick the weakest contract that is still *clearly stated* — say exactly what holds
(single-writer never tears) and exactly what doesn't (concurrent writers aren't
coherent) — and then prove the failure mode you promised. Never let "it usually
works" stand in for a named contract.

## Lesson 7: the old traps don't retire — the PIE relocation trap and the `.bss` ceiling both bit again

Two constraints this project has lived under for its whole life resurfaced,
milestones later, in brand-new code:

- **The PIE relocation trap** (from
  [`shell-and-filesystem-postmortem.md`](shell-and-filesystem-postmortem.md)):
  `mount -r`'s `host:port` split first used `&hostport[..c]` — ordinary `str`
  range-indexing. That inserts a UTF-8 char-boundary check whose panic path drags
  in `core`'s formatting tables, which emit an `R_AARCH64_ABS64` against a local
  symbol, which the PIE link rejects outright. The fix is the same as it always
  was: work on **byte slices** (`&hb[..c]` + `from_utf8`), never `str` range-index.
  Bisected the same way as years ago — stub functions until it links.
- **The `.bss` ceiling:** the rotating TCP source port wanted a counter, i.e. a
  mutable `static`. But a zero-init `static` needs `.bss`, which the userland loader
  doesn't support. So the port is **derived from the microsecond clock** instead —
  and since successive `tcp_get`s are a full round trip apart, the clock has always
  advanced. A constraint turned into a stateless design.

**The lesson:** a platform's hard constraints are not a phase you pass through —
they are permanent, and they will ambush *new* code written by someone (even you)
who has half-forgotten them. Keep the traps documented where the compiler error will
send the next person, and treat "the obvious construct" (a `str` slice, a `static`
counter) as guilty until proven innocent on this target.

## Lesson 8: the build reorders the plan's last mile — record it, don't paper over it

Phase 1's design staged the outbound remote-client as 1a and the export gateway as
1b. The build shipped the **export first** (easier to verify with a host python
client) and *labeled it* 1a — so the later "1c" step was really the design's 1a+1c
together. Small, but recorded honestly in the CHANGELOG and the design doc rather
than silently renumbered, the same tradition as the Phase 0 postmortem's "the build
revises the plan's last mile." A reader comparing the design doc to the commits
should never have to wonder whether they're looking at the same thing.

## What it added up to

Three phases, three releases, in a day:

- **v0.6.0 — Phase 1:** a machine reads another's disk over a 9P-over-TCP protocol
  written from scratch, proven between two QEMU VMs on a shared virtual link.
- **v0.7.0 — Phase 2:** a machine *writes* another's disk, byte-exact (verified by
  macOS mounting the far disk image), single-writer + clean-disconnect.
- **v0.8.0 — Phase 3:** three resources become files — another machine's processes
  (`/proc`), console (`/dev/cons`), and network identity (`/net`) — each local and
  remote.

The years-long question — *can several computers share resources as one system?* —
has a demonstrated **yes** across storage, processes, console, and network
identity. And the thesis held the whole way: none of it changed `ls`, `cat`, or the
fs-helper contract. Distribution was routing, a transport swap, and a handful of
sentinels — exactly what "remote is the same protocol over TCP" promised.

What's deliberately *not* done, and named so it isn't mistaken for finished: the
namespace-aware export (three prefix special-cases now say it's time); Plan 9's full
`/net/tcp` connection files (dialing *out* through another machine's NIC);
authentication (trusted-LAN, loudly documented); concurrent-writer coherence; and
remote execution (the `cpu` model — Phase 4). Each is a known edge, not a surprise.

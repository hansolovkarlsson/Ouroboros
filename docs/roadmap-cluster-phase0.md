# Phase 0 design — local namespace + one uniform file protocol

The detailed design for **Phase 0** of [`roadmap-cluster.md`](roadmap-cluster.md):
the foundation of the distributed-cluster direction, built now for a concrete
*local* payoff — **two filesystems mounted at different paths at once** — that
today's single-filesystem-at-`/` model physically cannot express. This is step 1
of the cluster ("remote is just this same protocol over TCP instead of local
IPC"), not a detour from it.

This document commits to a *design and a sequence*. It is grounded in the code as
it stands (constants and `file` refs are current); where it makes a judgment
call, it states the decision, the rationale, and the alternative it rejected.

## Goal, and the one consumer that justifies it

Replace the three bespoke server protocols (`FSOP_*` in `fsd`, `DSPOP_*` in
`cond`, `NETOP_*` in `netd`) with **one uniform, server-agnostic verb set**, and
give each task a **per-process namespace** that maps path prefixes to servers —
so reaching any server is "walk your namespace to the right server, speak the one
protocol."

The consumer that makes this a feature and not a refactor-for-vanity:
**simultaneous multi-mount.** `fsd` today holds exactly one mounted filesystem
(`let mut fs: Option<vfs::Filesystem>` — `programs/servers/fsd/src/main.rs:50`),
so a USB stick *and* a second disk cannot both be mounted. Phase 0 makes
`/mnt/a` and `/mnt/b` two different live filesystems at once.

### Done looks like

- One verb set, defined in a new `ninep-abi` crate, spoken by `fsd` **and**
  `cond`; `FSOP_*` and `DSPOP_*` are gone from the client path.
- A per-task namespace with `mount`/`bind` that affect **only the calling task's
  view** and are inherited by its children (Plan 9's crucial property).
- **Two filesystems mounted at different paths simultaneously** — `ls /mnt/a`
  and `ls /mnt/b` list two different on-disk filesystems in one boot — verified
  on QEMU against the host's own mount of the same partitions, zero `-d int`
  aborts. Real-hardware confirmation at the phase boundary, as usual.

`netd`'s file-ification (`/net`) is **out of Phase 0 scope** — it belongs to
Phase 3, and its client protocol (`NETOP_*`) is left untouched here.

## The three design decisions

### Decision 1 — Fused path-based transactions now; fids deferred to Phase 1

**Decision.** The Phase 0 verb set is **stateless and path-addressed**, like
today's `FSOP_*`: every request carries `(tree-id, path, op-params)` and is
self-contained. It does **not** introduce 9P's `fid` handles (`attach`/`walk`/
`open`/`clunk`) yet. A Phase 0 "read" is best understood as a *fused* 9P
transaction — attach+walk+open+read+clunk collapsed into one message.

**Rationale.** Fids buy nothing *locally*: kernel IPC is cheap, and re-sending a
≤128-byte path per op costs nothing across a `MSG_CALL`. What fids buy is
**network** efficiency — "walk the path once, then read by handle" instead of a
round trip per path component — and that is precisely the pain
[`roadmap-cluster.md`](roadmap-cluster.md) names for Phase 1 ("9P walks a path
one component per round trip; over a network that's painful"). Introducing fids
locally would add a bounded per-client fid table to `fsd` **and** a
dead-client fid-reclamation lifecycle (today `fsd` holds *no* per-client
state — every request is self-contained), for zero local benefit. The project's
loudest discipline is "scope it down; let testing find the boundaries," and
Phase 1 is where a packet trace will *justify* fids rather than presuppose them.
The roadmap explicitly licenses a reduced set: *"Keep it small — enough to
express the filesystem and console; not the full 9P2000 spec on day one."*

**What we build so Phase 1 is an extension, not a rewrite.** The wire header
reserves a **session/tree field** and a spare param slot from day one (below), so
Phase 1 adds a `fid` field + `open`/`clunk` verbs *alongside* the fused ops — it
does not reshape the protocol. Phase 0 ships the real **data model and
namespace**; fids are a transport optimization layered on in Phase 1.

**Alternative rejected.** Full fid-based 9P in Phase 0. Honest to the letter of
"attach/walk/clunk," but it front-loads the network phase's complexity (fid
tables, per-client open-handle lifecycle, dead-owner reclamation) with no local
consumer — the opposite of how every prior arc was scoped.

### Decision 2 — The namespace is per-task kernel-stored *opaque* bytes; resolution lives in ulib

**Decision.** Each task gets a **namespace table** stored in the kernel as
opaque bytes — a fixed `[Namespace; NUM_TASKS]` array, exactly like the existing
per-task CWD store (`CWDS: [_; NUM_TASKS]`, `kernel/src/tasks.rs:657`). New
`mount`/`bind` syscalls append a binding to the *calling* task's table; a
`get_namespace` syscall returns it. A child inherits its parent's namespace at
spawn, the same way it inherits the CWD (staged by the parent). The kernel
**stores** the bytes and **interprets nothing** — path resolution (longest-prefix
match → `(server-task, tree-id, subpath)`) is a pure function in **ulib**.

**Rationale.** This is the CWD pattern, which already proves the shape: the
kernel holds a per-task path (`CWD_STAGE` = 34 / `GET_CWD`) without knowing what
it means; userland resolves. It gives the roadmap's exact requirement —
"`mount`/`bind` syscalls that add to the calling task's namespace only … a mount
affects no other process's view" — with clean child inheritance, while keeping
filesystem/path logic **out** of the microkernel (the whole reason `fsd` exists).
Userland cannot hold this table itself as a `static`: the PIE linker asserts
`.data`/`.bss` are empty (`programs/linker.ld:98-99`), so per-process mutable
state must be either stack-threaded (works only for a long-lived program like the
shell, not a short-lived `/bin` command) or kernel-stored. Kernel-stored, opaque,
is the idiomatic answer here.

**Alternative rejected.** A pure-userland namespace threaded on the stack (like
the shell's `env`). Fine for the shell; but a `/bin` command is spawned fresh and
would have to receive its whole namespace at spawn and rebuild it on its stack,
and "mount as a syscall that mutates the caller's view" no longer has a home. The
kernel already crossed this exact bridge for CWD; reuse it.

### Decision 3 — Capabilities are unchanged in Phase 0; the delegation gap is named for Phase 1

**Decision.** No capability-model change. A spawnable task statically holds
`TO_FSD | TO_CON` (`caps_for_slot`, `kernel/src/tasks.rs:137`), and everything
Phase 0's namespace points at is a **boot server with a static capability**
(`fsd` at slot 2, `cond` at slot 3). Multi-mount lives *inside the single `fsd`
task* (one existing `TO_FSD`), not as separate server tasks — so no new "may
reach server N" grant is needed.

**Rationale.** The runtime-delegation primitive (`DELEGATE` = 41) is
**one-target-per-task**: `DELEGATED_SEND: [AtomicU64; NUM_TASKS]` holds a single
target per grantee (`kernel/src/tasks.rs:726`). That is enough for a linear
pipeline (one downstream per stage) and for `TO_NET` at spawn, but it could *not*
express a task that must reach several dynamically-mounted server tasks at once.
Phase 0 sidesteps this entirely by keeping the mount count inside one server.

**The gap, named for Phase 1.** The moment a namespace binds a *remote* subtree
(Phase 1) or a *spawned* filesystem server, "bind = hand a child the capability
to reach server task N" needs **multi-target delegation** — the single-slot
`DELEGATED_SEND` must generalize to a small per-task set. This is the roadmap's
"delegation becomes `bind`" convergence; Phase 0 builds the namespace mechanism,
Phase 1 generalizes the capability that backs a *cross-task* binding. Flagged
here so it is a planned step, not a surprise.

## The verb set (the `ninep-abi` crate)

A new shared crate `ninep-abi` (sibling of `syscall-abi`), `#![no_std]`, consts
only — same discipline as `syscall-abi` (every value a scalar inlined at the use
site, safe under either target). It defines **one** request/reply encoding used
by every server.

**Wire format** (mirrors `FSOP_*`'s proven layout, +1 field). Request:

| offset | field | notes |
|--------|-------|-------|
| 0 | `verb: u64` | `NP_STAT`, `NP_READ`, `NP_WRITE`, `NP_READDIR`, `NP_CREATE`, `NP_REMOVE`, `NP_MKDIR`, `NP_MV`, … |
| 8 | `tree: u64` | **which mount** (the session/tree selector — the multi-mount key, and Phase 1's remote-tree handle) |
| 16 | `a0..a3: u64×4` | op params (offset, length, mode, second-path-len …) |
| 48 | payload | path bytes, then any inline data — starts at `NP_REQ_PAYLOAD = 48` |

Reply: `status: u64` at 0, result payload from `NP_REPLY_PAYLOAD = 8` —
unchanged from `FSOP_*`. Bulk data still moves by **grant/safecopy**, untouched:
`NP_READ`/`NP_WRITE` use the existing `GRANT_WRITE`/`GRANT_READ` +
`SAFECOPY` path (cap `SAFECOPY_MAX = 2048`), and the inline payload cap stays
`FS_DATA_MAX = 512`, all inside `MSG_MAX_LEN = 768`. The only structural change
from `FSOP_*` is the **`tree` field** (offset 8) and a server-agnostic verb
namespace; `fsd`'s existing decoder (`handle()`,
`programs/servers/fsd/src/main.rs:247`) shifts params by one slot and reads
`tree`.

The admin/control ops (`mount`/`unmount`/`erase`/`partition`/`format`/
`mount_info`) stay `fsd`-specific control messages — they are not part of the
uniform *file* verbs (a console has no `format`). They keep working; only their
constants move into `ninep-abi` for tidiness.

## fsd: from one mount to several

`Option<vfs::Filesystem>` → a bounded **mount table**:

```rust
struct Mount { tree: u32, fs: vfs::Filesystem, partition_lba: u32 }
// fsd main frame, no heap — the netd `[Option<TcpConn>; MAX_CONNS]` pattern
let mut mounts: [Option<Mount>; MAX_MOUNTS] = core::array::from_fn(|_| None);
```

`MAX_MOUNTS = 4` (bounded, like `MAX_CONNS = 4`). A file request selects its
mount by `tree` (linear scan, as `netd`'s `find_conn` does); the `vfs::Filesystem`
enum engine underneath is **entirely unchanged** — it already multiplexes
*format* (FAT32/exFAT/ext2), and now `fsd` multiplexes *mount instances* above
it. `mount` grows a `tree`/target-path argument; `mount_info` lists all mounts.
`partition::discover` already enumerates up to `MAX_PARTITIONS = 16` partitions
on a disk — Phase 0 mounts two of them into two trees instead of picking the
first.

## cond on the uniform verbs

`cond` today decodes `DSPOP_WRITE`. Under the verb set, console output is
`NP_WRITE` on the `/dev/cons` tree — `cond`'s request loop keeps its exact shape
(recv → decode → render → reply), it just decodes `NP_WRITE` instead. The
namespace binds `/dev/cons → (CON_TASK, cons-tree)`; `ulib::con_write` resolves
and emits an `NP_WRITE`. This is the proof the verb set is **not
filesystem-specific** — a second, structurally different server reached by the
same verbs. **Watch item:** the console-server postmortem documents that routing
per-character echo through IPC once exposed a sub-tick-IPC scheduler assumption;
Phase 0 must keep `cond`'s batched-write shape (not a verb per character) so it
doesn't reintroduce that.

## Staging — five independently shippable steps

Each step boots, passes a full regression, and shows zero `-d int` aborts before
the next begins — the cadence that carried the filesystem/network/xHCI arcs.

- **0a+0b — `ninep-abi` defined; `fsd` speaks it; `ulib` uses it. — DONE
  (2026-08-25).** Merged into one step because 0a alone is untestable without an
  NP client (the minimal client *is* the `ulib` re-point) — see the CHANGELOG's
  "Cluster Phase 0, step 0a+0b" entry. The `ninep-abi` crate defines the verbs
  (`tree` selector at offset 8; payload at `NP_REQ_PAYLOAD` 48; `NP_BASE` 0x100);
  `fsd` gained `handle_ninep` (mirroring each `FSOP_*` file-op arm onto the same
  `vfs` calls) and dual-speaks; `ulib`'s fs client emits the verbs with
  `tree = 0`, so **every `/bin` filesystem command reaches `fsd` over NP** with no
  `/bin` source change. Verified byte-identical to a pre-change baseline (`ls`/
  `cat`/64 KiB read/`mkdir`/`touch`/`writeat`/`cp`/`mv`/`rm`/`rmdir`), zero aborts.
  One scope correction found in build: the **shell has its own `FSOP_*` helpers**
  (separate from `ulib`), so `FSOP_*` is *not yet* dead client-side — the shell
  migration + retiring `fsd`'s `FSOP_*` file-op arms is a small **next sub-step**,
  after which `FSOP_*` file ops are deleted. No kernel change.
- **0c — kernel per-task namespace + `bind` — DONE (2026-08-25).** The
  `NAMESPACES` per-task store (CWD-shaped), `NS_SET` (52) / `GET_NS` (53), and a
  `bind <new> <old>` shell builtin; `resolve_ns` (longest component-aligned
  prefix) in both `ulib` and the shell's fs helpers. Design revised while
  building: instead of staging the namespace for the next spawn (the CWD model),
  a child **inherits the spawning task's namespace automatically** at
  `spawn_staged`, and `bind` sets its own via `NS_SET` — simpler, and it made the
  shell's own `cd`/`write` (which must resolve too) fall out for free. `fsd`
  untouched (every binding is tree 0). *Shipped:* `bind /mnt /EFI` then `ls /mnt`
  == `ls /EFI`, per-task, inherited by spawned `/bin` commands; regression
  byte-identical (empty namespace = identity), zero aborts. See the CHANGELOG's
  "step 0c" entry.
- **0d — `fsd` multi-mount + THE payoff.** The `[Option<Mount>; MAX_MOUNTS]`
  table; `mount` a second filesystem into a second tree; namespace binds
  `/mnt/a`, `/mnt/b`. *Ship:* **two filesystems mounted at once** — `ls /mnt/a`
  (say FAT32) and `ls /mnt/b` (say ext2) in one boot.
- **0e — `cond` → `/dev/cons` on the verbs.** `cond` adopts `NP_WRITE`; console
  output becomes a namespace-resolved write. *Ship:* two different servers, one
  verb set; `DSPOP_*` deleted. **Phase 0 done.**

## The multi-mount test disk

Phase 0's payoff needs a disk with **two mountable data partitions** plus the
FAT32 ESP. Extend the existing `scripts/mk*.py` + `make run-image-*` pattern
(`mkexfat.py`/`mkext2.py` already build two-partition MBR disks): a new
`scripts/mkmulti.py` + `make run-image-multi` building a **three-partition MBR** —
partition 1 FAT32 data, partition 2 ext2 data, partition 3 FAT32 ESP (UEFI boots
it). `fsd` mounts partitions 1 and 2 into `/mnt/a` and `/mnt/b`.

**Verify against a foreign observer** (the project's rule): the host's own mount
of the *same two partitions* (macOS FAT32 + `debugfs` on the ext2 image, exactly
as the exFAT/ext2 arcs validated). The guest's simultaneous two-tree `ls`/`cat`
must match each partition's real contents. A two-VM network is **not** needed for
Phase 0 — that's Phase 1.

## Risks / explicitly deferred

- **Fid deferral (Decision 1).** Mitigation: the `tree`/session field and a spare
  param are in the wire format from 0a, so Phase 1 adds fids as fields+verbs, not
  a reshape. If a Phase 0 workload ever shows path-per-op as a *local* cost
  (it shouldn't — IPC is cheap), revisit early.
- **Single-target delegation (Decision 3).** Fine for Phase 0 (static caps, one
  `fsd`); `DELEGATED_SEND` must grow to a small per-task set before Phase 1's
  cross-task/remote binds. Named, not solved, here.
- **Namespace sizing.** Bounded like everything else: `MAX_BINDINGS ≈ 8` per
  task, prefix ≤ 64 bytes — a fixed `[Binding; 8]` in the per-task store. Pick the
  numbers when 0c lands; err small.
- **Header budget.** `NP_REQ_PAYLOAD = 48` (verb + tree + 4 params) + path
  (≤ path-max) sits well under `MSG_MAX_LEN = 768`; bulk still rides
  grant/safecopy, so the message never carries file data.
- **cond hot path.** Keep batched writes (above) — do not let the verb layer turn
  console output into a per-character `MSG_CALL`.
- **Netd untouched.** `NETOP_*` stays; `/net`-as-files is Phase 3. Phase 0 does
  not claim "all three bespoke protocols gone" — it retires two (`FSOP_*`,
  `DSPOP_*`) and unifies the servers that own *files today*.

## Effort

Comparable to a mid-size arc slice — larger than the disk-management milestones,
smaller than the whole network stack. 0a–0b (the ABI + ulib re-point) is the bulk
of the careful work; 0c is a small, CWD-shaped kernel addition; 0d is the payoff
and is mostly `fsd`-internal; 0e is a light `cond` adaptation. Each step is a
plausible single session, and each is independently shippable — so this can land
incrementally, and a natural release boundary (a completed **arc** →
[`RELEASING.md`](RELEASING.md)'s minor bump) is Phase 0 complete = **v0.5.0**.

## Sources

- [`roadmap-cluster.md`](roadmap-cluster.md) — the phased cluster plan this
  details Phase 0 of.
- [`research-directions.md`](research-directions.md) — the Plan 9 namespace +
  uniform-protocol analysis this rests on.
- [The Use of Name Spaces in Plan 9](https://9p.io/sys/doc/names.html) and
  [9P (protocol)](https://en.wikipedia.org/wiki/9P_(protocol)) — the model and
  the message set to base the verbs on (a minimal subset first).

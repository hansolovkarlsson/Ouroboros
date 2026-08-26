# Phase 3 design — resources as files: `/proc`, remotely mountable

The detailed design for the **first step of Phase 3** of
[`roadmap-cluster.md`](roadmap-cluster.md): *all resources as files, remotely
mountable.* Phase 3 is broad (the roadmap names `/dev/cons`, `/net`, and `/proc`
as file servers); this step takes the cleanest, most self-contained of them —
**`/proc`, a synthetic process-table filesystem** — and makes it both a local
file tree and, over the Phase-2 transport, **visible from another machine**. Done
looks like: `mount -p /proc; ls /proc; cat /proc/0/state` locally, and `ls
/mnt/a/proc` on machine B showing **machine A's live tasks**.

Grounded in the code as it stands after Phase 2. Scoped hard, per the project's
"pick the smallest version with a real consumer" discipline.

## Why `/proc` first

Of Phase 3's three file servers, `/proc` is the one that adds the least new
surface for the most demonstrable win:

- It is **read-only and synthetic** — generated from the kernel's task table via
  the ungated `TASK_STATE` syscall — so it needs no disk, no write path, no new
  ABI. `/dev/cons` (writes, and the deliberately-namespace-bypassed console echo
  hot path from the console-server postmortem) and `/net` (a whole connection/ctl
  file surface) are both larger and messier.
- It **reuses `fsd`'s existing machinery whole**: the `Filesystem` enum (a fourth,
  synthetic arm), the `tree`-selected mount table (multi-mount, Phase 0), and —
  crucially — the export gateway, so the *remote* payoff comes almost for free.
- The remote payoff is the genuinely cluster-y thing: **see another machine's
  processes as files**, the Plan 9 "everything, everywhere, is a file" idea made
  concrete across the network.

## The two design decisions

### Decision 1 — `/proc` is a synthetic `Filesystem` arm at a reserved fsd tree

**Decision.** `fsd` grows a `Filesystem::Proc` arm (a new `proc.rs`) implementing
the read methods (`list_dir`/`read_file`/`read_at`) synthetically from
`TASK_STATE`, and rejecting the write methods. It is **auto-mounted at boot into a
reserved mount-table index** (`MAX_MOUNTS` grows by one; the new top slot,
`PROC_TREE`, is proc), so the proc tree always exists alongside the boot disk at
tree 0. The tree layout it serves:

```
/               ->  0/  1/  2/ ...        (one dir per scheduler slot)
/<n>            ->  state                 (the slot's files)
/<n>/state      ->  "runnable\n" | "blocked\n" | "zombie\n" | "unused\n"
```

**Rationale.** `fsd`'s `Filesystem` enum already dispatches every verb per-arm and
its mount table is already `tree`-indexed; a synthetic arm and a reserved tree
drop straight in, and the whole verb/reply path (local *and* exported) works
unchanged above it. Linux's procfs is a filesystem too — this is well-precedented,
not a layering novelty.

**Alternative rejected.** A separate `/proc` **server task** (its own protected
slot) with the namespace/export generalized to route to any server. That is the
"true" Plan 9 shape and Phase 3 may grow into it, but it is a much larger change
(a new supervised slot, a server-selector in the namespace binding, export
routing to arbitrary tasks) for no more visible behavior today — build it when a
*second* non-fsd server (a writable `/dev`, `/net`) actually needs it.

### Decision 2 — the export gateway prefix-routes `/proc`, no namespace refactor

**Decision.** `netd`'s export (`handle_9p`) today sends every incoming path to
`fsd` tree 0 (the disk). It now computes the tree from a **path prefix**: a path
under `/proc` routes to `PROC_TREE` (with the `/proc` prefix stripped), everything
else to tree 0. So a remote `ls /mnt/a/proc` — which arrives at A as the wire path
`/proc/0/state` — is served from A's proc tree; a remote `cat /mnt/a/EFI/…` still
hits A's disk.

**Rationale.** This is the *smallest* change that makes `/proc` remotely visible:
the export exposes exactly two things, A's disk and A's `/proc`, by an explicit
prefix. Making the export fully namespace-aware (resolving incoming paths through
a composed per-export namespace, the general Plan 9 "export a namespace") is the
right long-term shape but is Decision 1's rejected-alternative in another guise —
deferred until more than two synthetic trees exist to expose.

**Local access** is the existing namespace mechanism: a new shell `mount -p
/proc` binds `/proc → (PROC_TREE, "/")` in the caller's namespace, exactly like
`mount <n> <path>` binds a disk partition's tree. A remote viewer needs *no* bind
— it just reads `/mnt/a/proc`, and A's export does the routing.

## Staging

- **3a — `/proc`, local + remote. ✅ DONE.** The `Proc` arm (`proc.rs`) + reserved
  tree (`NS_PROC_TREE`, `MAX_MOUNTS + 1`) + boot auto-mount in `fsd`; the export
  prefix-routing (`route_export`) + `tree` threaded through the export's
  `fsd_call`/`read_file_chunk`/`stat_size`/`list_dir` in `netd`; the `mount -p`
  builtin in the shell. *Shipped:* locally `mount -p /proc; ls /proc` → `0/`…`9/`,
  states read true (fsd `runnable`, netd `blocked`, slot 9 `unused`); between two
  VMs, from B `ls /mnt/a/proc` lists A's ten slots and `cat /mnt/a/proc/2/state`
  reads A's fsd state — B reading A's live process table as files. Disk access
  alongside, zero `-d int` aborts on both nodes.

- **3b — `/dev/cons`, local + remote. ✅ DONE.** The console as a writable file —
  the first route to a **non-fsd** server. A console sentinel (`NS_CON_TREE`,
  mirroring the remote sentinel): locally the shell's `mount -c /dev/cons` binds
  it and `resolve_ns` returns `server = CON_TASK`, so the fs write helpers (shell
  + `ulib`) `con_write` the bytes instead of calling fsd; reads are refused
  (write-only). Remotely, `netd`'s `route_export` recognizes `/dev/cons` and
  `handle_9p` emits the write's inline bytes to `CON_TASK`. *Shipped:* locally
  `mount -c /dev/cons; echo hi > /dev/cons` renders and `cat /dev/cons` errors;
  two VMs, `write /mnt/a/dev/cons …` prints on **machine A's** screen. Zero
  `-d int` aborts. (`/dev` as a directory listing isn't served — `/dev/cons` is
  the writable file.)

Later Phase 3 (not yet): `/net` (use another machine's NIC), and — now that
`/dev/cons` is the *second* non-disk consumer — a fully namespace-aware export
(resolve incoming paths through a composed per-export namespace) to retire the
per-server prefix special-cases, plus union directories when a consumer appears.

## Risks / deferred

- **Only `state` per task.** `GET_ARG*` returns the *caller's* argv, not another
  task's, so there is no cross-task command/name to expose yet — `/proc/<n>` has
  just `state`. A per-task name would need a new kernel accessor; deferred until
  wanted.
- **The prefix hack.** `/proc` routing is an explicit special-case, not general
  namespace-aware export (Decision 2). Named loudly so it isn't mistaken for the
  finished shape.
- **No auth.** Inherited from Phase 1 — trusted-LAN, and `/proc` is read-only, so
  the exposure is a process list, not a write vector.

## Effort

Small and contained: a synthetic read-only FS (no disk, no write path), a
one-line-ish tree bump + boot mount, an export prefix branch with a `tree`
threaded through three fsd-client calls, and a shell builtin. A completed step →
folds into the Phase 3 arc; the arc's first shippable, demonstrable "a resource is
a file, and a *remote* resource is a *remote* file" result.

## Sources

- [`roadmap-cluster.md`](roadmap-cluster.md) Phase 3; the Phase 0 multi-mount
  (`tree` selector) and Phase 1/2 transport this rides on.
- Plan 9's `/proc` and the "everything is a file" model; Linux procfs as the
  synthetic-FS precedent.

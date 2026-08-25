# Cluster Phase 0: one protocol, per-task namespaces, and multi-mount

*A design retrospective (the eleventh), covering the day three bespoke server
protocols became one uniform Plan 9-style verb set, each task grew its own
namespace, and — the payoff — two filesystems could be mounted at once. The
local foundation of the distributed-cluster direction. Written for other
microkernel / bare-metal-OS developers.*

A companion to the [network-stack](network-stack-postmortem.md) and
[console-server](console-server-postmortem.md) postmortems (which built the
servers this arc unifies) and the forward-looking
[`roadmap-cluster.md`](roadmap-cluster.md) and
[`roadmap-cluster-phase0.md`](roadmap-cluster-phase0.md) (the vision and the
detailed design). This is the arc where the insight *"remote is just this
protocol over TCP instead of local IPC"* stopped being a slogan and became the
actual shape of the code.

## The starting point

Three userland servers, three hand-rolled request protocols: `fsd`'s `FSOP_*`
(filesystem), `cond`'s `DSPOP_*` (console), `netd`'s `NETOP_*` (network). Each a
different set of op numbers over the same `MSG_CALL` IPC. Plan 9's whole design
rests on the opposite: *one* file-protocol every server speaks, and per-process
namespaces that compose those servers into a private view. Phase 0's job was to
take that locally — one verb set, a per-task namespace, and a concrete consumer
(multi-mount) to prove it was a feature and not a refactor-for-vanity — as the
foundation the network phases build on.

It landed in six shippable sub-steps, each verified byte-identical to the prior
behavior and each merged before the next began. The lessons below are the ones
that generalize.

## Lesson 1: a mechanism with no visible consumer gets merged with its consumer

Twice in this arc a "clean" sub-step turned out to be **untestable on its own**,
and the right response both times was to merge it with the step that consumes it.

The design doc staged **0a** as "`fsd` speaks the new verbs" and **0b** as
"`ulib` switches to them." But 0a alone changes nothing observable — a server
answering a protocol no client speaks. The *only* client that could test it was
0b's `ulib` re-point. So they became one step: define the ABI, `fsd` speaks it,
`ulib` uses it, and the test is the **real regression** — the whole shell +
`/bin` surface, byte-identical over the new protocol — with no throwaway client.

The same shape appeared at **0c** (the per-task namespace). A namespace resolves
paths to a mount; with a single mount, it resolves everything to the same place,
so wiring it in changes nothing you can see. What made 0c testable was giving it
a real *feature* as its consumer: a `bind` command (map a prefix onto any
subtree), so `bind /mnt /EFI` then `ls /mnt` is an observable proof of the whole
machinery — store, stage, inherit, resolve.

**The rule:** if a step builds a mechanism whose only exercise is a later step's
code, don't ship it blind behind scaffolding — merge it with the consumer and let
the real regression be the test. "Scope it down" doesn't mean "ship untestable
slices."

## Lesson 2: building the step tells you the design; the plan was more complex than needed

0c's plan followed the existing per-task **CWD** store faithfully: the parent
*stages* its namespace before `SPAWN` and the child reads it — the same
`CWD_STAGE`/`GET_CWD` mechanism. It even worked out a pending-length scheme to
carry the staged blob without a free `SPAWN` argument.

Then building it surfaced a fact the plan had missed: the shell's own `cd`
*validates* a directory by listing it (`fs_list_dir`), so the shell must resolve
the namespace **for itself**, not just deliver it to children. That one
observation collapsed the design: if the shell reads its own namespace anyway,
then a plain `NS_SET` (set the caller's own namespace) plus **automatic
parent→child inheritance at spawn** is strictly simpler — no staging buffer, no
pending length, no `SPAWN`-argument gymnastics — and it makes `cd`/`write` resolve
for free. The shipped 0c is smaller and cleaner than the approved plan.

**The lesson isn't "plans are wrong."** The plan resolved the load-bearing
decisions correctly (kernel-stored opaque bytes, resolution in userland). It's
that a design has a *last mile* only the implementation walks, and the discipline
is to let the build revise the plan when it learns something — and to write down
that it did (the CHANGELOG and design doc both record the revision), so the next
reader sees the real reasoning, not the superseded sketch.

## Lesson 3: retiring a protocol is a client census, not a server edit

Deleting `fsd`'s `FSOP_*` file-op handlers *looked* like a one-file change. It
was not, because you cannot delete a server's handler while any client still
sends that op. Retiring `FSOP_*` was a **census of every client**, and the census
had a surprise: the **shell keeps its own filesystem-client layer**, separate
from `ulib`'s (it predates `ulib`). So the migration was: `ulib` (every `/bin`
command), then the shell's own helpers, then `netd` (which is an `fsd` client
too — it reads files to serve them over HTTP) — and only after all three could
`fsd`'s twelve file-op arms (~210 lines) actually be deleted.

**The generalizable trap:** in a message-passing system, a protocol constant is
an implicit contract with an unknown set of senders. Before you delete the
receiver, `grep` for the senders — *all* of them, including the ones that don't
look like clients (a server calling another server). The compiler will not catch
a stale wire op; only the census will.

## Lesson 4: make the default the identity, and the whole arc verifies byte-for-byte

Every sub-step in this arc was verified the same way: capture the full shell +
`/bin` session before the change, apply it, capture again, and **diff** — the
only allowed differences being non-functional boot artifacts (a binary grew a
few KB; an async startup line interleaved differently). That was possible
because each change was designed so that **the default path is the old
behavior**:

- The uniform verbs carry a `tree` selector, but it's `0` until multi-mount, and
  tree 0 is the one existing mount — so single-mount results are identical.
- An **empty namespace is the identity** (resolve returns the path unchanged to
  tree 0), so an unbound task behaves exactly as before namespaces existed.
- Multi-mount is a table whose slot 0 is the boot mount, so the single-mount code
  path is unchanged.

This is a deliberate scope-down lever: if the *new* capability is opt-in and its
absence reproduces the old behavior bit-for-bit, then "did I regress anything?"
has a mechanical answer (a byte diff), and the risky new code only runs when you
ask for it. It turned a large, cross-cutting protocol change into a sequence of
provably-neutral steps.

## Lesson 5: the payoff often needs no new test infrastructure — look for the rig you already have

The multi-mount payoff (0d) seemed to need a new disk: two data partitions plus
an ESP, a new `mk*.py` script, a new Makefile target, an ext2 payload. Then a
second look at what already existed: the `run-image-ext2` disk is *already* two
partitions — ext2 at partition 0 (auto-mounted at tree 0) and the FAT32 ESP at
partition 1. Mounting partition 1 at `/mnt/f` gives `ls /` (ext2) and `ls /mnt/f`
(FAT32) — **two different on-disk filesystems, live at once** — on a disk the
build already knew how to make. The headline feature shipped with zero new test
infra.

**The habit worth keeping:** before building a test rig for a new feature, check
whether an existing rig already produces the conditions you need. The most
convincing demonstration reused the oldest asset.

## Lesson 6: deferring the hard part is a bet that can pay off in a later phase

Phase 0's protocol deliberately did *not* adopt 9P's stateful `fid` handles — a
call made early, on the grounds that fids buy nothing when IPC is cheap and a
128-byte path per op is free. That left the verbs **path-based**: each op carries
its whole path and is one self-contained round trip.

The bet paid off one phase later. When the same verbs went over TCP (Phase 1),
real 9P's chattiness — a `walk` per path component, the thing the roadmap flagged
as needing a client-side cache — simply *did not arise*, because a path-based
verb is one network round trip regardless of path depth. The deferral wasn't just
"less to build now"; it happened to produce the shape a network transport wants.

**The nuance:** this is not a license to defer everything. It worked because the
deferred thing (fids) had *no local consumer* and its absence had a *concrete
benefit* (self-contained ops). Defer the hard part when it's genuinely not needed
yet **and** you can name why the simpler thing is also better — not merely to
avoid work.

## Lesson 7: the capability model didn't have to grow — the shape of the feature avoided the limit

Ouroboros enforces "who may call whom" with a per-task send-mask, and its runtime
**delegation** is one-target-per-task (a single slot). That limit *looked* like a
Phase 0 blocker: multi-mount and remote mounts sound like "a task reaching several
servers." It never bit, because the features were shaped to avoid it:

- Multi-mount lives **inside one `fsd`** (a mount table indexed by `tree`), so a
  client still reaches exactly one server with one existing static capability.
  No new grant, no kernel change.
- Reaching both `fsd` (local paths) and `netd` (remote paths, in Phase 1) works
  because `TO_FSD` is **static** and `TO_NET` is the one **delegated** slot — they
  don't contend.

**The lesson:** when an enforcement limit seems to block a feature, check whether
the *feature* can be shaped to fit the limit before you grow the *mechanism*.
Growing the capability model is real work with real security surface; keeping the
mount count inside one server was free. (The genuine gap — a task reaching several
*dynamically-spawned* server tasks — remains named for when a phase actually needs
it, not pre-built.)

## What it added up to

Six steps, each byte-identical and shippable, turned three bespoke protocols into
one uniform verb set, gave every task a per-process namespace with a real `bind`,
and made two filesystems mountable at once — the Plan 9 local foundation, cut as
**v0.5.0**. None of it needed a kernel-capability change or a new test rig for the
payoff, and all of it was provably neutral until you opted into the new
capability. The distributed half (9P-over-TCP) started immediately after and
found the groundwork already the right shape — which was the whole point of doing
the local half first.

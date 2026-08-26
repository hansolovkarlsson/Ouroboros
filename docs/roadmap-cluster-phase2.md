# Phase 2 design — two-node disk-sharing: read *and* write

The detailed design for **Phase 2** of [`roadmap-cluster.md`](roadmap-cluster.md):
a machine now **writes** a remote disk over the 9P/TCP transport built in
[Phase 1](roadmap-cluster-phase1.md), not just reads it. **Done looks like:**
machine B creates and edits files on machine A's storage, and a clean disconnect
fails cleanly rather than corrupting. This is the milestone that turns "read
another machine's disk" into a genuine two-node disk-sharing cluster — the
years-long question answered with a yes you can *act on*, not just look at.

Grounded in the code as it stands after Phase 1. Where it makes a call, it states
the decision, the rationale, and the alternative rejected.

## What already exists (and what's missing)

Phase 1 left the client write path **mostly wired but dead-ended**: the fs
helpers already route a remote `fs_write_*`/`fs_op_path`/`fs_mv` through
`np_remote` (data inline, no grant across a machine), but the export gateway
(`handle_9p`/`build_9p_reply`) is **read-only** — every mutate verb falls to
`_ => FS_ERROR`. So the whole of Phase 2's core is: **teach the export gateway
the write verbs**, and make the client **chunk** a large write to the inline cap.
netd is already an `fsd` client (`fsd_call`, `read_file_chunk`'s grant pattern),
so the gateway has every primitive it needs.

## The three design decisions

### Decision 1 — the export gateway relays writes to fsd; wire-inline bridges to fsd's local ABI

**Decision.** `handle_9p` gains the mutate verbs, each decoded from the wire frame
and run against the local `fsd`, reusing netd's existing fsd-client calls:

- **Path-only** (`NP_TOUCH`/`NP_MKDIR`/`NP_RMDIR`/`NP_RM`): `fsd_call(verb,
  pathlen, 0, path)` — one call, no data.
- **`NP_MV`** (two paths inline): `fsd_call(NP_MV, srclen, dstlen, src++dst)` —
  `fsd_call` already copies its payload argument verbatim, so the combined
  `src++dst` buffer and the two length params are all it needs.
- **`NP_WRITE`** (full create/overwrite, data inline ≤ `NP_REMOTE_CHUNK`):
  relayed as fsd's **inline** `NP_WRITE_FILE` (`fsd_call(NP_WRITE_FILE, pathlen,
  datalen, path++data)`). `NP_WRITE_FILE`'s create-or-overwrite-from-inline
  semantics are exactly `NP_WRITE`'s for a ≤512 chunk, and it needs **no grant**
  — the cleanest bridge. (A 0-byte `NP_WRITE` = truncate-to-empty, which
  `NP_WRITE_FILE` with 0 data also does — the create step `cp` issues first.)
- **`NP_WRITE_FILE`** (inline): relayed directly, same shape.
- **`NP_WRITE_AT`** (offset write): fsd's `NP_WRITE_AT` is **grant-only** (there
  is no inline offset-write), so netd copies the wire data into a local buffer,
  `GRANT_READ`s it to fsd, and issues `NP_WRITE_AT` — the exact mirror of
  `read_file_chunk`'s `GRANT_WRITE` read bridge, in the other direction.

The reply is a bare status (0 or an `FS_ERR_*`), framed back like any verb.

**Rationale.** netd is the DMA/transport owner and already the one task that may
call fsd for the HTTP file server; relaying is a handful of lines on primitives
it already has. Mapping wire `NP_WRITE` → fsd `NP_WRITE_FILE` avoids a needless
grant for the common small write; only offset-writes take the grant path.

**Alternative rejected.** Adding an *inline* `NP_WRITE_AT` to fsd to avoid the
grant. It would change fsd's ABI for one caller's convenience; the grant bridge
is already the established pattern (`read_file_chunk`) and keeps fsd untouched.

### Decision 2 — the client chunks a large write to the inline cap; callers unchanged

**Decision.** A remote write's data rides **inline** in the `NETOP_RMOUNT`
request, bounded by `NP_REMOTE_CHUNK` (512, to fit `MSG_MAX_LEN` both ways). So
the client write helpers **loop internally**, splitting a larger buffer into
≤512-byte `NP_WRITE_AT` sub-requests at rising offsets — `fs_write_at` becomes a
chunking loop for the remote case, and `fs_write_bulk` truncates then streams. A
caller that hands `fs_write_at` a 2048-byte `SAFECOPY_MAX` chunk (as `cp` does
when the *source* is local) still works, one round trip per 512 bytes.

**Rationale.** Keeps every `/bin` writer (`cp`, `writeat`, the shell's `write`
builtin, `>>` redirection) unchanged — the chunking is invisible above the fs
helper, exactly as the inline-read fallback was in Phase 1. `cp` from a *remote*
source already reads ≤512 per call (Phase 1's read cap), so its writes were
already ≤512; this decision covers the local-source→remote-dest direction too.

**Alternative rejected.** Raising `NP_REMOTE_CHUNK` toward a full segment. It
buys a little throughput but the reply/message caps still bound it, and the loop
is needed anyway for anything larger — better one honest chunking path.

### Decision 3 — single-writer, clean-disconnect: documented, not coordinated

**Decision.** Phase 2 is **single-writer**: one machine writes a given remote
tree at a time, by convention, with **no distributed lock**. A remote op whose
TCP round trip fails returns a clean error (`NO_FS` — the mount goes stale, the
op fails, nothing is half-applied at the client), which Phase 1's `handle_rmount`
already delivers. Concurrent multi-writer coherence is **explicitly out of scope**
and called out loudly (CAP realities — you can't have it all; we pick clean
single-writer and say so).

**Rationale.** This is the roadmap's stated posture: "start single-writer, define
what happens on disconnect, be explicit that concurrent multi-writer coherence is
a later, harder problem." Building a lock protocol now, before single-writer
read/write is even proven, is premature. Each op is a self-contained request/
reply against fsd (which serializes all disk access through one task anyway), so a
*single* writer never tears — the only unshipped guarantee is *concurrent*
writers, which we don't claim.

**Alternative rejected.** A remote advisory lock verb now. No consumer yet, and
it presumes a coordination model Phase 2 deliberately doesn't have.

## Staging

- **2a — write-side export + client chunking (the core). ✅ DONE.** The mutate
  verbs in `handle_9p` (path-only + `mv` via `fsd_call`; `NP_WRITE`→inline
  `NP_WRITE_FILE`; `NP_WRITE_AT` via a new `fsd_write_at` grant bridge) and the
  client chunking loop in `fs_write_at`/`fs_write_bulk` (ulib + shell). *Shipped:*
  two VMs — from B, `mkdir`/`write`/`cat`-back and `cp /BIN/LS /mnt/a/LSCOPY` (17
  KB → 34 chunked `NP_WRITE_AT` round trips) onto A's disk; A reading its own disk
  sees it all. **Byte-exactness confirmed by a foreign observer:** mounting A's
  disk image on macOS, `LSCOPY` is `cmp`-identical to `/BIN/LS`. Zero `-d int`
  aborts on both VMs.
- **2b — clean-disconnect semantics. ✅ DONE.** `SIGKILL`ing A mid-session makes
  B's next remote op fail cleanly (a distinct error via ARP/round-trip timeout →
  `NO_FS`, no hang) and B stays responsive locally — Decision 3 as designed, no
  new code. **One robustness gap the test surfaced and fixed:** the client
  `tcp_get` sent a single SYN with no retransmit, so a dropped first packet on a
  freshly-connected socket link failed the whole op (an intermittent first-`ls`);
  now the SYN is retransmitted a few times within the op (helps HTTP fetch and
  remote reads too).

## Testing

Reuse the discipline: verify against a **foreign observer** and the **wire**, not
our own logs. 2a between two Ouroboros VMs (`make run-image-2vm-a`/`-b`) watched
with `tcpdump`, and — for byte-exactness of what landed — read the written file
back through the *host* python 9P client against A's export, or mount A's disk
image on the host and diff. 2b by killing a node mid-write.

## Risks / deferred

- **Torn writes across chunks.** A multi-chunk remote write that fails partway
  leaves the file partially written (the early chunks landed). Same as a local
  streamed `cp` interrupted mid-way — not corruption of the *filesystem*, just an
  incomplete *file*. Single-writer + clean-fail is the contract; atomic
  multi-chunk writes (temp-file + rename) are a later nicety if a consumer wants them.
- **No auth.** Inherited from Phase 1, Decision 4 — trusted-LAN, loud, deferred.
- **Concurrent writers.** Decision 3 — out of scope, said loudly.
- **Latency.** One round trip per ≤512-byte chunk, so a big `cp` over the network
  is chatty. Measure before optimizing (client-side write-back caching is a later
  option); the path-based verbs already avoid per-component walks.

## Effort

Small next to Phase 1: the transport, framing, remote binding, and client routing
all exist. 2a is the mutate verbs in `handle_9p` (a bounded addition) plus a
chunking loop in two fs helpers; 2b is a test. A completed Phase 2 → **v0.7.0**,
and it is **the milestone that answers "is a shared-disk cluster doable?" with a
demonstrated yes** — two Ouroboros machines, one reading *and writing* the
other's disk over a protocol written from scratch.

## Sources

- [`roadmap-cluster.md`](roadmap-cluster.md) Phase 2; the Phase 1 design
  ([`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md)) whose transport,
  `NETOP_RMOUNT`, remote binding, and inline-chunk model this builds on.
- [`network-stack-postmortem.md`](network-stack-postmortem.md) — the
  trace-based, foreign-observer testing discipline to reuse.

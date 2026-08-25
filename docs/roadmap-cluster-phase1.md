# Phase 1 design — 9P-over-TCP: the pivot to distributed

The detailed design for **Phase 1** of [`roadmap-cluster.md`](roadmap-cluster.md):
carry the uniform verb set ([`ninep-abi`](../ninep-abi/src/lib.rs)) over a TCP
connection, so one machine **exports** its filesystem server and another
**remote-mounts** it. This is where Phase 0's local groundwork pays off — the
whole thesis was *"remote is just this same protocol over TCP instead of local
IPC."* **Done looks like:** machine B runs `ls`/`cat` against machine A's disk.

Grounded in the code as it stands. Where it makes a call, it states the
decision, the rationale, and the alternative rejected.

## The core reframing: grant/safecopy → inline stream frames

Locally, a verb request is a kernel-copied `MSG_CALL` (≤ `MSG_MAX_LEN` 768) and
**bulk data moves by grant/safecopy** (`NP_READ`/`NP_WRITE`/`NP_WRITE_AT`, cap
`SAFECOPY_MAX` 2048) — the kernel copies between task regions, no data in the
message. Over TCP there is no grant. So the wire form of a verb is the **same
verb and params, with data inline in the stream**, length-delimited so the
receiver knows where a message ends:

```
request:  [u32 len][verb:u64][tree:u64][a0..a3:u64×4][payload…]   (path, then any data)
reply:    [u32 len][status:u64][result…]                          (dir bytes / file data)
```

The header is exactly the existing NP wire (`NP_REQ_PAYLOAD` 48 / `NP_REPLY_PAYLOAD`
8) with a `u32` length prefix for stream framing. A whole message is small — the
header plus an inline chunk ≤ 2048 fits in ~2 TCP segments (MSS 1400) — and the
receiver reassembles by the length prefix (netd's client path already does
in-order reassembly into a buffer). **The grant/safecopy bulk ops become inline
bulk on the wire**; that is the only structural change to the protocol.

**A property worth naming: our verbs are already the right shape for a network.**
Real 9P `walk`s a path one component per round trip — the chattiness the roadmap
flagged as needing a cache. Our verbs are **path-based** (Phase 0 Decision 1
deferred fids), so **each op is one round trip regardless of path depth**. The
fid deferral pays off exactly here: no per-component walk to amortize.

## The four design decisions

### Decision 1 — netd is the gateway, both directions

**Decision.** Both halves live in **netd**: it already owns TCP *and* is already
an fsd client (`fsd_call`/`read_file_chunk`, reading files to serve over HTTP).
It gains (a) an **export listener** — accept a TCP connection, decode an inbound
NP frame, run it against local fsd, frame the reply back; and (b) a **remote
client** — a local op that takes `(endpoint, NP-request)`, opens/reuses a TCP
connection to a remote export listener, and returns the reply.

**Rationale.** netd is the transport fabric by design; a client can't do TCP
itself (only `NET_TASK` holds `CAP_NET`). Reuse netd's TCP primitives — the
connection state machine (`handle_tcp`/`TcpConn`/`ConnState`), `build_tcp_srv`,
`send_seg`, and the client's SYN/ACK/reassemble path (`tcp_get`). Only the *what
to do with the bytes* is new (a framed-NP handler instead of the HTTP parser).

**Alternative rejected.** A new dedicated 9P server task. It would duplicate
netd's entire TCP stack; netd already has every primitive.

### Decision 2 — length-prefixed inline frames on a dedicated port

**Decision.** The export listener runs on its **own TCP port** (9P's registered
564), separate from the HTTP server (port 80). netd's inbound filter
(`parse_tcp_in`, which today hard-drops anything not destined for `SERVER_PORT`)
learns a second port and dispatches by local port: 80 → the HTTP handler, 564 →
the framed-NP handler. `TcpConn` gains a one-byte "service" tag so a connection
remembers which it is.

**Rationale.** A dedicated port keeps the two protocols cleanly separable (no
content-sniffing), matches how 9P is deployed, and reuses the existing accept
path unchanged. The bytes are framed (Decision's length prefix), so a partial
segment is handled by "wait for `len` bytes" — netd already reassembles on the
client side; the server side gains the same for the (small) request frame.

**Alternative rejected.** Multiplexing 9P onto port 80 with protocol detection —
fragile, and conflates two services.

### Decision 3 — a *remote* namespace binding; resolution returns the destination

**Decision.** The namespace binding grows a **remote flavor**. Today a binding is
`[tree:u8][prefix_len][target_len][prefix][target]` and `tree` selects a mount
*within fsd*; `resolve_ns` returns `(tree, fs_path)` and every `np_call` targets
`FSD_TASK` unconditionally. Phase 1:

- A **remote binding** encodes `prefix → (host:port, remote-root)` — e.g. reserve
  a `tree` sentinel (`0xFF` = "remote") and store `[ip:4][port:2]` at the head of
  the target, remote-root following. It fits the same opaque per-task blob
  (`NS_MAX` 256).
- `resolve_ns` returns a small **`Resolved { server_task, tree_or_endpoint,
  fs_path }`** instead of `(tree, fs_path)`: a local match → `(FSD_TASK, tree,
  path)`; a remote match → `(NET_TASK, endpoint, remote_path)`.
- The fs helpers route to `Resolved.server_task`. For a remote resolution they
  wrap the NP request in a netd op (`NETOP_RMOUNT`) carrying the endpoint; netd
  does the TCP round trip and returns the NP reply verbatim.

**Capabilities need no change.** A `/bin` command already holds `TO_FSD`
statically *and* is delegated `TO_NET` by the shell at every spawn
(`delegate_net`), and `TO_FSD` being static means the two don't contend for the
single `DELEGATED_SEND` slot. So a command can reach fsd (local paths) and netd
(remote paths) in the same breath, today.

**Rationale.** The namespace is already the mount table and already per-task —
"a remote mount is a binding" is the Plan 9 model stated in the roadmap, and it
drops into the existing resolver. Threading a `server_task` through resolution is
the one real client-side change (in **both** `ulib` and the shell's duplicate fs
layer), but the surface is contained (the resolver + the `np_call` target).

**Alternative rejected.** A separate "remote mount table" outside the namespace —
it would duplicate the resolver and lose the per-task, inherited-at-spawn
property the namespace already gives for free.

### Decision 4 — trusted-LAN first, loudly; auth is a later phase

**Decision.** The first cut assumes a **trusted LAN with no authentication** —
the export listener serves any peer that connects — and **says so at every
layer** (the shell command, the docs, a boot log line). Authentication (who may
mount what, capabilities carried over the wire) is a dedicated hardening phase,
**not faked**.

**Rationale.** This is the roadmap's stated posture. The namespace-as-capability
model (a remote mount *is* a capability a task holds) is what extends cleanly to
"who may reach this remote tree" later; building auth now, before the transport
is proven, is premature. Never pretend the network is trusted — document it.

## Staging — three shippable steps, then the integration

Each boots, is verified, and shows zero `-d int` aborts before the next. The
first two are testable with **one VM plus the host as the network peer** (SLIRP
`hostfwd`, exactly the `run-image-server`/`curl` pattern) — the two-VM rig is
only needed for the final integration.

- **1a — the `np-net` frame + netd's remote-client primitive.** Define the
  length-prefixed frame (in `ninep-abi`: a `NP_NET_MAX` cap and the framing
  convention — no new verbs). Add netd's outbound "send this NP frame to an
  endpoint, reassemble the reply" (a `tcp_get`-shaped helper generalized off the
  HTTP specifics) and a client op `NETOP_RMOUNT(endpoint, np-request) → np-reply`.
  *Ship:* a self-test — netd sends a framed NP request to a **host-run mock 9P
  server** (a ~40-line python script over SLIRP) and gets a correct reply.
- **1b — the export gateway.** netd's inbound gains the port-564 framed-NP
  handler: decode → local `fsd_call`/`read_file_chunk` → frame the reply.
  *Ship:* **a machine exports its filesystem** — a host python 9P client (over a
  `hostfwd tcp::5640-:564`) runs `readdir`/`read` against the guest's disk and
  gets its real contents. One VM, host as client.
- **1c — the remote-mount client.** The remote namespace binding + `resolve_ns`
  returning `server_task` + fs helpers routing remote paths through
  `NETOP_RMOUNT`; a shell **`mount -r <host:port> <path>`** builtin
  (`ns_add` gains a remote entry). *Ship:* a guest **remote-mounts a host-run 9P
  server** and `ls`/`cat`s it — proving the client routing end to end. One VM,
  host as server.
- **1d — two-VM integration (the "aha").** A new Makefile target giving two
  guests a shared L2 link (QEMU `-netdev socket,listen=`/`connect=`; today only
  SLIRP exists, guest↔host only) and a **configurable guest IP** (netd hardcodes
  `10.0.2.15` — two guests need distinct addresses; derive it from the MAC or a
  boot arg). *Ship:* **machine A exports, machine B `mount -r A:564 /mnt/a`,
  `ls /mnt/a` lists A's disk.** Verified against a `tcpdump` on the shared link.

## Testing

Reuse the network stack's discipline — verify against a **foreign observer**, not
our own logs:
- 1a/1b/1c against a **host-run python 9P peer** (client or server) speaking the
  frame format, over SLIRP `hostfwd` — one VM, fast loop.
- 1d against a second VM over a QEMU socket link, watched with `tcpdump`.
The mock peer *is* the foreign checker (as macOS's `fsck`/`curl` were for the
filesystem/network arcs).

## Risks / deferred

- **Latency & chattiness.** Mitigated by design (path-based verbs = one round
  trip per op, not per component). If a workload still bites, client-side caching
  is a Phase 2 measure — *measure first* (a trace), never guess.
- **Disconnect & failure.** A remote mount whose peer drops must fail its ops
  **cleanly** (a distinct error, the mount goes stale) — never hang or corrupt.
  Single-writer, clean-failure semantics are Phase 2's remit; Phase 1 just needs
  a remote op to time out and return an error (netd's TCP already has RTO).
- **Inline-bulk cap.** A network read/write chunk travels inline (no grant), so
  it's bounded per frame (`NP_NET_MAX`, ~`SAFECOPY_MAX`); large files stream chunk
  per round trip, as `cat` already does locally.
- **netd size.** netd is already ~2200 lines; the gateway + client add
  meaningfully. If it strains the guard page (it has, twice before — 16→24→32 KB),
  grow the stack, or split the export gateway into its own server (deferred; netd
  first for the TCP reuse).
- **Two-guest networking & IP config.** Net-new infra (Decision-free, just work):
  the socket netdev target and a per-guest IP. Isolated to 1d.
- **No auth.** Decision 4 — trusted-LAN, loud, deferred.

## Effort

The largest arc yet — a real network protocol in *both* directions, on top of a
TCP stack that was HTTP-shaped. But it decomposes: 1a/1b are additive to netd and
host-testable; 1c is the client-routing thread through the resolver; 1d is test
infra. A completed Phase 1 → **v0.6.0**, and it sets up **Phase 2** (two-node
read/write disk sharing — the milestone that answers "is it doable?" with a yes).

## Sources

- [`roadmap-cluster.md`](roadmap-cluster.md) Phase 1; the Phase 0 design
  ([`roadmap-cluster-phase0.md`](roadmap-cluster-phase0.md)) whose verb set,
  `tree` selector, and namespace this rides on.
- [`network-stack-postmortem.md`](network-stack-postmortem.md) — netd's TCP and
  the trace-based, foreign-observer testing discipline to reuse.
- [9P (protocol)](https://en.wikipedia.org/wiki/9P_(protocol)) — the model; we
  carry our own verb set, not 9P2000 on the wire (a minimal, honest subset).

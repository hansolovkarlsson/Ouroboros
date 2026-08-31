# Distributed Ouroboros — a Plan 9-style resource-sharing cluster (roadmap)

The long-term direction for Ouroboros: **several machines, each exporting its
resources as file trees, each composing a private namespace view of the whole
cluster's storage, devices, and services.** A machine reads another's disk as if
it were local; uses another's network; runs a program where the CPU or the data
lives. This is Plan 9's distributed model — the system it was designed to be —
and it is the thing Ouroboros is ultimately aiming at.

This document is the honest, phased plan from where the code is today to a
working two-node cluster and beyond. It's a companion to
[`research-directions.md`](research-directions.md) (which argued Plan 9's
namespace model is the standout next architecture) — this one takes that
conclusion and follows it all the way to the distributed goal. It commits us to
a *direction and a sequence*, not to writing all of it now.

## The vision, stated honestly (the dream minus the mirage)

There are two very different things people mean by "distributed OS," and the
difference decides whether the dream is engineering or fantasy:

- **Sharing resources across machines** — storage, devices, services, and
  "run this *there*" execution. **This is doable.** It's exactly what Plan 9
  delivered, it needs no magic, and Ouroboros already has the two hardest
  prerequisites (a microkernel with servers-over-IPC, and a working TCP stack).
  This is the whole of Phases 0–4 below.

- **A single system image: shared memory across machines, and one computation
  transparently split across many CPUs.** **This is the wall.** Network latency
  is ~10,000× RAM latency, so "the RAM of N machines as one pool" is either
  unusably slow or forces the program to be written for it anyway. Every system
  that chased transparent single-system-image (distributed shared memory,
  MOSIX-style process migration) found the transparency leaks. The version that
  *works* makes distribution **explicit**: ship the computation to where the
  data or the CPU is (Plan 9's `cpu`), or partition the work and message-pass
  (HPC's MPI). We deliberately target the explicit version and treat transparent
  shared memory as out of scope — not because it's forbidden to think about, but
  because chasing the transparent version is how you spend years for little
  return.

**North star, in one line:** *a cluster of Ouroboros machines that share
storage, devices, and services through per-machine namespaces over a uniform
file protocol — resource-sharing and explicit remote execution, not shared
memory.*

## Why Ouroboros is well-positioned (the prerequisites already exist)

The two hardest pieces of a distributed OS are already built and
**hardware-confirmed on real Parallels hardware**:

- **A microkernel with userland servers reached over IPC** — `fsd` (filesystem),
  `cond` (console), `netd` (network). The server-request shape (a client sends a
  request, blocks, gets a reply) is *already* the shape of a file-protocol
  transaction.
- **A working TCP/IP stack** (`netd`, the network-stack arc — see
  [`network-stack-postmortem.md`](network-stack-postmortem.md)): ARP, IPv4,
  ICMP, UDP, DNS, and TCP with congestion control, loss recovery, and a
  concurrent HTTP server. This is the transport a distributed protocol rides on.

And it has already *reinvented, by hand,* the pieces Plan 9 unifies — three
bespoke server protocols (`FSOP_*`, `DSPOP_*`, `NETOP_*`), a per-task capability
send-mask, runtime delegation, and a multi-filesystem VFS enum inside `fsd`. The
whole insight of this roadmap is:

> **"Remote" is just "the same protocol, over TCP instead of local IPC."**

Once every server speaks *one* file protocol, mounting a remote machine's
service is the same operation as mounting a local one — the transport changes,
the model doesn't. That is why the local Plan 9 work (Phase 0) is not a detour
from the cluster dream; it is step 1 of it.

## The dependency spine

```
Phase 0  Local namespace + one uniform (9P-ish) protocol
            │  (makes fsd/cond/netd uniform; multi-mount is the consumer)
            ▼
Phase 1  9P-over-TCP transport
            │  (netd carries the file protocol; export/mount over the network)
            ▼
Phase 2  Two-node disk-sharing cluster   ◀── the milestone that answers
            │                                 "is it doable?" — YES
            ▼
Phase 3  All resources as files, remotely mountable (/dev, /net, /proc)
            ▼
Phase 4  Remote execution (Plan 9 `cpu` model — ship work to the resource)
            ▼
Phase 5  Explicit distributed compute (frontier / research)
```

Each phase is independently shippable and useful. You could stop after Phase 2
and have a real, novel thing: two hobby-OS machines sharing a disk over a
protocol you wrote from scratch.

## Phases

### Phase 0 — Local namespace + a uniform file protocol (the foundation)

> **Detailed design:** [`roadmap-cluster-phase0.md`](roadmap-cluster-phase0.md) —
> the resolved design decisions (fused verbs now / fids deferred to Phase 1;
> namespace as per-task kernel-stored opaque bytes; capabilities unchanged), the
> `ninep-abi` verb/wire format, `fsd` multi-mount, and the staged 0a–0e plan.

*This is the Plan 9 "local half" from [`research-directions.md`](research-directions.md),
now understood as the foundation of the whole distributed vision rather than a
standalone elegance play. It has a concrete local consumer — **multiple
simultaneous mounts** — so it's a feature, not a refactor-for-vanity.*

- **Define a minimal 9P-ish verb set** as a new shared ABI (in `syscall-abi`, or
  a new `ninep-abi` crate): `attach` / `walk` / `open` / `create` / `read` /
  `write` / `clunk` / `remove` / `stat`. Keep it *small* — enough to express the
  filesystem and console; not the full 9P2000 spec on day one.
- **A per-task namespace table** — a fixed-size, no-heap array of
  `(path-prefix → server endpoint)` bindings, exactly the spirit of the existing
  per-slot capability send-mask. Path resolution walks this table. (`no_std`,
  no-`alloc` discipline holds: bounded mounts per task, like everything else.)
- **`mount` / `bind` syscalls** that add to the calling task's namespace only —
  Plan 9's crucial property that a mount affects *no other process's* view and
  needs no global permission.
- **Migrate `fsd` first**, with multi-mount as the payoff: mount the USB stick at
  `/mnt/usb` *and* a second disk at `/mnt/disk` at once — something the current
  single-filesystem-at-`/` model physically cannot express. Then migrate `cond`
  (→ `/dev/cons`) and `netd`.
- **Delegation becomes `bind`.** The transitive-delegation gap on the current
  roadmap frontier *is* Plan 9's "hand a child a namespace entry." Build general
  delegation as the namespace mechanism, not as a separate send-mask extension —
  they are the same problem.

**Done looks like:** two filesystems mounted at different paths simultaneously,
every server reached through the same verbs, all on QEMU + real hardware, zero
aborts. The three bespoke protocols are gone.

### Phase 1 — 9P-over-TCP: the pivot to distributed ✅ DONE

> **Status:** complete. Export gateway (1a), remote-mount client (`mount -r`, 1c),
> and two-node integration (1d — two QEMU guests on a shared L2 link, one mounting
> and reading the other's disk, MAC-derived per-guest IPs) all shipped and verified.
> Read-only sharing; read+write is Phase 2. Ready to cut **v0.6.0**.

> **Detailed design:** [`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md) —
> the grant/safecopy→inline-frame reframing, netd as the gateway both ways, the
> remote namespace binding, trusted-LAN-first, and the staged 1a–1d plan (the
> first two host-testable with one VM; two-VM socket networking only for 1d).

- **`netd` carries the file protocol over a TCP connection.** A 9P transaction is
  a request/reply message pair — the same shape `netd` already services locally,
  now framed over a TCP stream instead of kernel IPC.
- **Server side (export):** a machine runs a 9P listener on a TCP port that
  re-exposes one of its local servers (start with `fsd`) to the network.
- **Client side (remote mount):** the namespace table gains the ability to bind
  a *remote* endpoint (`host:port`) as a subtree. A `walk`/`read` on that subtree
  becomes a 9P message over TCP instead of a local IPC call.
- **Trust, stated explicitly:** the first cut assumes a **trusted LAN** — no
  authentication — and *says so loudly*. Auth (who may mount what, capabilities
  carried over the wire) is real work and is deferred to a hardening phase, not
  faked. A remote mount is a capability; the namespace-as-capability model
  (Phase 0) is what extends cleanly to "who may reach this remote tree."

**Done looks like:** machine A exports its filesystem; machine B mounts it over
TCP and runs `ls` / `cat` against A's disk. **Testable entirely in QEMU with two
VMs on a virtual network — no second physical machine required** (see Testing
below). This is the "aha."

### Phase 2 — Two-node disk-sharing cluster (the answer to "is it doable?") ✅ DONE

> **Status:** complete. Machine B creates/edits files on machine A's disk over
> 9P/TCP (write verbs relayed to fsd; large writes chunked to the inline cap);
> single-writer, clean-disconnect (a killed peer fails the next op cleanly, no
> hang, no corruption). Byte-exactness confirmed against A's disk image on macOS.
> **The years-long question answered with a demonstrated yes.** Read+write,
> trusted-LAN, single-writer; auth and concurrent-writer coherence remain. Cut
> **v0.7.0**. Detailed design: [`roadmap-cluster-phase2.md`](roadmap-cluster-phase2.md).

- **Read *and* write** a remote disk over 9P/TCP — machine B creates/edits files
  on machine A's storage.
- **Failure and consistency semantics, chosen and documented, not hand-waved:**
  start **single-writer** (one machine owns write access at a time), define what
  happens on disconnect (the mount goes stale, operations fail cleanly rather
  than corrupt), and be explicit that concurrent multi-writer coherence is a
  later, harder problem (CAP realities — you can't have it all; pick, and say
  which). The project's "scope it down, let testing find the boundaries"
  discipline applies hard here.

**Done looks like:** two Ouroboros machines, one mounts the other's disk over the
network and reads/writes it, surviving a clean disconnect. **This is the
milestone that answers the years-long question with a yes** — everything after is
expansion of a proven idea.

### Phase 3 — All resources as files, remotely mountable ✅ DONE

> **Detailed design:** [`roadmap-cluster-phase3.md`](roadmap-cluster-phase3.md).
> Three resources, each a file server, each remotely readable:
> **`/proc`** (a synthetic process-table fs in fsd — `ls /mnt/a/proc` shows another
> machine's live tasks); **`/dev/cons`** (the console as a writable file routed to
> `cond` — `write /mnt/a/dev/cons …` prints on another machine's screen); and
> **`/net`** (the network identity as read-only files, served by netd itself —
> `cat /mnt/a/net/ip` reads another machine's address). "Everything is a file,
> everything is mountable" pays off. Cut **v0.8.0**.

**Follow-up ✅ DONE (2026-08-26):** the namespace-aware export — the export now
resolves incoming paths through its own composed namespace with the shared
`ninep_abi::resolve_ns` (one implementation, used by `ulib`, the shell, and
`netd`), retiring the three per-server prefix special-cases. A fourth exported
resource is a binding, not a code branch.

**Follow-up ✅ DONE (2026-08-26): `/net/tcp` dial-out** — Plan 9's connection
files, so a machine dials TCP **out of another's NIC** (`dial /mnt/a/net <ip>
<port> …` connects from A's network). The connection handle lives in the path
(`/net/tcp/N/…`), so no fids were needed (the Phase-0 path-based-verbs design
extended); `net_op` only mutates state while the event loop (`pump_dials` /
`dial_on_segment`) does the TCP; `/net` became read-write. Stop-and-wait, TCP
client only. The "use another machine's network" half of the north star. See
[`dial-out-postmortem.md`](dial-out-postmortem.md).

**Follow-up ✅ DONE (2026-08-26): `/net/tcp` dial-in** — the mirror of dial-out:
`announce <port>` + `listen` make a machine **accept inbound** connections on
another's NIC (`serve /mnt/a/net 9000 …` answers clients that connect to A's
address). Passive open on A, relay to B, B owns the service. An accepted
connection is just another `DialConn` (handle in the path, still no fids), so it
was almost all reuse; small fan-out (a listener + two accepts). Completes the
`/net/tcp` model symmetrically. See
[`dial-in-postmortem.md`](dial-in-postmortem.md).

Deferred to later phases: a persistent multi-accept server loop; UDP; union
directories when a consumer appears; and transitive/remote re-export (the export
namespace can now bind a remote subtree — a natural Phase 4 hook).
- Union directories (Plan 9's join-several-sources-under-one-name) if/when a real
  consumer appears — not before. A fully namespace-aware export (vs. `/proc`'s
  prefix hack) lands when a second synthetic tree needs it.

### Phase 4 — Remote execution (the honest "distributed processing") ✅ DONE

> **Detailed design:** [`roadmap-cluster-phase4.md`](roadmap-cluster-phase4.md).
> The full Plan 9 `cpu` model: `cpu <host:port> <command>` runs `<command>` on
> another machine's CPU while reading **your** files through your namespace
> imported at `/host`. **4a (remote spawn + output stream):** netd spawns the
> `/bin` program with its stdout piped back (a spawner→child reply capability),
> captures it non-blocking in its event loop, streams it to the caller's `cpu`
> builtin. **4b (namespace import):** the remote binds `/host → remote(caller)` on
> the child; the caller's netd stays responsive with `tcp_run` (pumping the event
> loop while the run is in flight) so it serves the child's `/host` callbacks —
> `cpu A cat /host/F` runs on A and reads B's file. Proven two-VM, zero aborts.

- The Plan 9 `cpu` model: **run a program on machine B while its namespace —
  files, console, devices — is imported from machine A over 9P.** The program
  just does file I/O against imported trees, so **no shared memory is needed.**
- This is distributed processing done right: move the computation to where the
  CPU or the data is, explicitly. Useful (offload to a beefier node; run near the
  data), and reachable, precisely because it sidesteps the shared-memory wall.

**Output delivery ✅ improved (v0.14.0): chunked pull.** `cpu` output was capped
at one IPC message (768 bytes); the shell now pulls it in chunks (`NETOP_RUN_MORE`,
netd holding the run's output in a `PendingRun` buffer), lifting the cap to ~2 KB
— realistic command output comes back whole.

> **Future refinement — truly unbounded `cpu` output streaming (build if the need
> arises).** The chunked pull above is still **bounded**: netd collects the whole
> run into a ~2 KB buffer before the shell pulls it, and the remote itself
> accumulates the child's output into a fixed buffer before sending. To stream
> *unbounded* output, three things change: (1) the **remote** sends the child's
> stdout as it's produced (a sliding send buffer, drained as it's ACK'd, instead
> of accumulate-then-send) with TCP/mailbox backpressure throttling the child; (2)
> the **caller** advances a *resumable* run connection one chunk per shell pull
> (rather than running to completion first), so remote→caller→shell interleave;
> and (3) either netd is granted `TO_SHELL` to *push* chunks, or the shell keeps
> pulling as it does now. None of it is hard, but it's a real arc touching the
> remote streaming path, a resumable-run restructure in netd, and the shell loop —
> deferred until a command's multi-kilobyte output is actually wanted (the small
> commands `cpu` is really for fit the ~2 KB bound today). The capability
> constraint that netd can't push (no `TO_SHELL`) is why even the bounded version
> is a pull loop.

### Phase 5 — Explicit distributed compute (frontier / research)

- A deliberate work-distribution model — partition work + inputs, ship them,
  collect results (message-passing in spirit, not shared memory). This is where
  it becomes genuine research; scope it only once Phases 0–4 are real, and only
  against a concrete workload. **Not** transparent compute-splitting — that's the
  mirage; this is the explicit, tractable cousin.

## The hard parts, named up front (so they don't ambush us)

- **The shared-memory wall.** Covered above: transparent shared memory / SSI is
  out of scope by design. The achievable substitute is explicit "ship work to
  data" (Phase 4). If this roadmap ever seems to drift toward "make the RAM one
  pool," that's the drift to catch.
- **Security and trust across the network.** Local IPC is protected by the MMU
  and the capability send-mask; a TCP socket is not. Distributed 9P needs
  authentication and authorization eventually. The plan: **explicit trusted-LAN
  first, loudly documented; auth as a dedicated hardening phase.** The good news:
  the namespace-as-capability model means "a remote mount is a capability" fits
  the existing design rather than fighting it.
  - **First cut ✅ DONE (v0.10.0, 2026-08-26) — the export-hardening phase.**
    Trusted-LAN is over: every 9P-export request is authenticated with a
    **shared cluster secret** via a client-nonce MAC (`mac = HMAC-SHA256(key,
    nonce ‖ np)`, the secret never on the wire, no extra round trip), gating fs
    verbs *and* `NP_RUN` at `netd`'s one chokepoint (`handle_9p`). Fail-closed
    (no `\CLUSTER.KEY` = export refuses all remote clients); a `\NOEXEC` flag
    shares the disk while refusing remote-exec. The symmetric key makes the
    bidirectional `cpu` `/host` callback authenticate for free. See
    [`cluster-auth-postmortem.md`](cluster-auth-postmortem.md).
  - **Tier 2 — reply-direction (mutual) auth ✅ DONE (v0.13.0, 2026-08-26).** The
    export MACs its *reply* too (`mac = HMAC(key, request_nonce ‖ [status]
    [result])`, prepended to the framed reply), so an active injector can't feed a
    client forged data; the client rejects any reply whose MAC doesn't verify.
    Bound to the request nonce (no new wire field). No round trip, no state.
    **Integrity, not confidentiality** — bytes still cross in cleartext. Scoped to
    the framed fs / `/net/tcp` replies; the `cpu`-run *output stream* (not a framed
    reply) is left with the untrusted-network work below. Banked as defense-in
    -depth under trusted-LAN because the symmetric key made it nearly free. See the
    tier-2 addendum in [`cluster-auth-postmortem.md`](cluster-auth-postmortem.md).
  - **Tier 3 — per-user identity ✅ DONE (2026-08-31).** The tiers above
    authenticate a *machine*; this says which of that machine's **users** is
    asking. The auth header carries the caller's **name** (32 bytes, NUL-padded),
    inside the MAC (`mac = HMAC(key, nonce ‖ name ‖ message)`) so it cannot be
    altered without the key; the exporter resolves it through **its own**
    `/etc/passwd` and refuses a name it does not know. A *name*, not a uid: two
    nodes number their users independently, and NFS's `AUTH_SYS` shows what
    sending the number does. The identity reaches `fsd` as a **required
    parameter** carried in the request (`a3`), never an opt-in wrapper or a latch
    — see [`unspellable-postmortem.md`](unspellable-postmortem.md) for the
    attempt that did it the other way and what that cost. `cpu` is covered by a
    second mechanism: `netd` assumes the mapped user for the length of the spawn
    so the child inherits it. **Still machine-keyed**, which is the point of the
    per-user-*key* item below: a peer holding the cluster key can claim any name,
    so this protects against the users of a trusted node, not a compromised one.
  - **Tier 2+, TRIGGER-GATED — activate only when Ouroboros leaves a trusted
    network.** These are real hardening but **deliberately not built while the
    deployment is a trusted LAN** (two QEMU VMs, or Hans's own Raspberry Pi
    boards on his own network — same trusted posture). The **trigger** is a
    concrete "expose the cluster across a semi-trusted or hostile segment"
    scenario; until then, building these is a mechanism ahead of its threat model.
    When the trigger fires:
    - **Replay protection** — a captured request can be replayed verbatim today
      (forgery of a *new* one cannot). Fix: a server nonce (costs a per-op round
      trip) or a bounded seen-nonce cache (needs bounded state under no-heap).
    - **Per-peer and per-user keys** — one shared secret = interchangeable
      members; no per-machine access control or key rotation, and (since tier 3)
      a peer that holds the key can claim any *user* name. Fix: per-machine — and
      ultimately per-user — identities, properly wanting **asymmetric crypto**
      (Ed25519-class) — the heavy lift. **A designated auth server (Plan 9's
      `authsrv` + tickets) is evaluated in full under item 1 of
      [`roadmap.md`](roadmap.md)**: it is the right long-term shape, it is what
      Plan 9 does, and the note records both the detail that decides whether it
      closes the hole at all (the ticket must be verifiable by the *exporter*
      without trusting the peer) and why per-machine keypairs should come first.
    - **Confidentiality (encryption)** — the important honest one: authentication
      (tiers 1–2) protects *integrity and who-may-act*, **not secrecy**. Every
      byte still crosses the wire in **cleartext** — a sniffer reads your files
      even with perfect MACs. An untrusted-network move needs transport
      encryption, a separate axis from all the authentication above.
    - **`cpu`-stream reply-auth** — MAC the remote-run output stream (per-chunk
      or streaming), the framed-reply auth's harder cousin.
- **Consistency and failure.** Partitions, disconnects, concurrent writers — the
  CAP realities. Plan: single-writer first, clean-failure semantics, explicit
  about what's *not* coherent. Never pretend the network is reliable.
- **Latency and chattiness.** 9P walks a path one component per round trip; over
  a network that's painful. Plan 9 needed a caching layer (`cfs`) for exactly
  this. Expect to add client-side caching once Phase 2 works and the round trips
  bite — but *measure first* (the project's "verify against a trace, not a
  guess" discipline).
- **`no_std` / no-heap under a dynamic, network-driven namespace.** Mounts and
  connections are now driven by the network, not just boot config, but the
  fixed-size, bounded-buffer discipline still holds: bounded namespace entries
  per task, a bounded pool of remote connections, no unbounded allocation. Same
  constraint the whole kernel already lives under.
- **Machine identity and discovery.** How machines find and name each other
  (static config first; discovery later). Small, but real.

## How this reframes the existing roadmap

- The Plan 9 **local namespace** milestone graduates from "nice architectural
  unification" to **Phase 0 of the cluster** — with a hard local consumer
  (multi-mount) *and* a hard long-term consumer (the whole distributed vision).
  That resolves the "mechanism without a consumer" worry that (rightly) haunted
  the standalone version.
- **General / transitive delegation** = `bind` = the capability model for remote
  mounts. Three roadmap items converge into one mechanism.
- **`netd` grows a second role:** not just the guest answering the network, but
  the transport fabric of the cluster.
- The small frontier items (the large-read `fsd` restart, GPT read-validation,
  pipeline redirect) are still worth doing and are **independent** of this — they
  can land opportunistically without blocking or being blocked by the cluster
  arc.

## How we'll build it (the project's own discipline, applied)

- **Each phase independently shippable and testable**, newest capability proven
  before the next is started — the same cadence that carried the filesystem,
  network, and xHCI arcs.
- **Two-VM QEMU testing before real hardware.** A two-node cluster is fully
  testable with **two QEMU instances on a shared virtual network** (QEMU socket
  networking / a virtual switch) — no second physical machine needed for most of
  the work. This keeps the fast dev loop the project relies on, and mirrors how
  the network stack was built and validated against `tcpdump`/`curl` before it
  ever touched hardware. Real-hardware confirmation (two Parallels VMs, or a
  Parallels VM talking to another machine) comes at phase boundaries, the way it
  has all along.
- **Scope it down; let testing find the boundaries.** Every phase picks the
  smallest version with a real consumer. Security starts explicit-trusted;
  consistency starts single-writer; the protocol starts with a minimal verb set.
- **Verify against a foreign observer, not our own logs** — a real 9P client or a
  packet trace, the way exFAT/ext2 were validated against macOS's own `fsck` and
  the network stack against `tcpdump`.
- **A postmortem per arc**, continuing the project's tradition — the design
  retrospectives are part of the deliverable, not an afterthought.

## What "done" ultimately means

The years-long question — *can several computers share resources as one system?*
— gets its first real **yes at Phase 2**: two Ouroboros machines, one mounting
and using the other's disk over a from-scratch protocol. Phases 3–4 turn that one
proof into a genuine resource-sharing cluster: any machine's storage, devices,
and services usable from any other, and programs run where the resource lives.
That is what Ouroboros is meant to become — and, unlike the shared-memory
mirage, it's a straight line of engineering from what already boots today.

## Sources / prior art to draw on

- [The Use of Name Spaces in Plan 9](https://9p.io/sys/doc/names.html) — the
  namespace model this whole plan rests on.
- [9P (protocol) — Wikipedia](https://en.wikipedia.org/wiki/9P_(protocol)) — the
  message set to base the uniform protocol on (a minimal subset first).
- Plan 9's `cpu` command and CPU-server model — the reference for Phase 4
  (execution with an imported namespace).
- This project's own [`research-directions.md`](research-directions.md) (the
  Plan 9 local-half analysis) and
  [`network-stack-postmortem.md`](network-stack-postmortem.md) (the transport
  this rides on, and the two-VM/trace-based testing discipline to reuse).

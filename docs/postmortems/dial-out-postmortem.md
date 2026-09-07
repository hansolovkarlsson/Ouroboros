# Dial-out — the `/net/tcp` connection-files postmortem (the fifteenth)

*A design retrospective, 2026-08-26 — the day a machine learned to dial TCP out
of **another** machine's NIC. The cluster could already share a disk, `/proc`,
the console, network *identity*, and remote execution; the one resource it
couldn't share was the active side of the network — "use A's connection to reach
the outside." This is that, built as Plan 9's `/net/tcp` connection files.
Companion to the [distributed-cluster](cluster-distributed-postmortem.md) and
[cluster-auth](cluster-auth-postmortem.md) postmortems.*

## The consumer question, asked before the mechanism

The arc opened with a check the project keeps relearning: *does this mechanism
have a consumer that something cheaper doesn't already serve?* Because
`cpu A fetch example.com` **already dials out through A's network** — it runs the
fetch program on A, which uses A's NIC. So "can B use A's network at all" was
already answered by remote execution. Building `/net/tcp` only earns its place if
it does something `cpu` doesn't:

1. a **raw** connection the caller drives byte-by-byte, with **no matching
   program needed on A**; and
2. the **file interface** — `write /mnt/a/net/tcp/N/data …` — composing with the
   namespace instead of being a bespoke verb.

Naming that up front kept the scope honest: this is the raw-socket-through-A
primitive, not a second way to do what `cpu` does. The request/response case
stays `cpu`'s; `/net/tcp` is for when B owns the protocol.

## The insight that avoided the feature the project had deferred: the handle is the path

A live TCP connection has state that must persist across many operations —
open, connect, several read/writes, close. That is exactly what **fids** are for
in 9P, and fids were *deliberately deferred* back in Phase 0 (path-based verbs,
no per-client session state). A naïve "connection as a file" would have dragged
fids in as a prerequisite.

Plan 9 itself shows the way out: `/net/tcp/**N**/data` puts the connection
number **in the path**. So the handle needs no protocol machinery — `netd` keeps
a bounded `[Option<DialConn>; MAX_DIAL]` table, and every op is an ordinary
path-based NP verb that names slot `N`. The Phase-0 decision to keep verbs
path-based (which looked like a simplification at the time) turned out to be the
thing that let a stateful, long-lived resource be modeled without new mechanism.
*A constraint accepted early paid off two arcs later.*

## The architectural rule that made it fit: `net_op` never blocks; the event loop does the TCP

The dangerous way to build this would have been to make `connect` (or a data
read) block inside the request handler until the network responded. That handler
runs **inside** an event-loop pass (a client message or an export request being
serviced), so blocking it is the same self-deadlock the `cpu` Phase-4b work hit:
the loop can't process the very frames the blocked call is waiting for.

So the split is strict: **`net_op` only mutates `DialConn` state** — allocate on
`clone`, set `Connecting` on `connect`, append to the send buffer on a `data`
write, drain the receive buffer on a `data` read. **All actual TCP happens in the
event loop**: `pump_dials` sends the SYN and retransmits and sends the FIN;
`dial_on_segment` completes the handshake, ACKs and buffers inbound data, and
handles the peer's FIN. `connect` is therefore **asynchronous** — it returns
immediately and the client polls `status` until `Established`. This is the exact
shape the server-side connections already use, and it's the same "netd must never
block its own loop" lesson from the network-stack and remote-exec arcs, applied a
third time. The rule generalizes: *in a single-threaded event-loop server, a
request handler sets intent; the loop does the work.*

## Routing: the elegance was that there was almost none

Because the resolver already returns a task-neutral `NsTarget`, `/net/tcp` needed
no new routing. Locally, `mount -n /net` resolves it to `NsTarget::NetLocal` — a
direct NP verb to `NET_TASK`. Through a remote mount it resolves to
`NsTarget::Remote` — the export gateway — so dialing out of A's NIC is *literally
just the path* `/mnt/a/net/tcp/…`, authenticated by the same v0.10.0 cluster key
as any other export access. The only real client-side gap was that `/net` had
been **read-only**: `ulib` refused writes to it. Opening that (a new
`fs_write_inline` that routes an `NP_WRITE_FILE` through the namespace, and
teaching `np_netlocal` to carry the write payload) was the whole client change.
"Everything is a file, everything is mountable" kept paying: a brand-new active
capability rode in on plumbing built for reading `/net/ip`.

## The trap that came back (it always does): the stack

First boot after wiring it up: `netd` faulted immediately and the supervisor
killed and restarted it into the give-up cap. The fault decoded to a permission
fault at translation level 3 near the top of `netd`'s region — a **guard-page
stack overflow**. The cause was mundane and entirely self-inflicted: each
`DialConn` carried a 2 KB send buffer and a 4 KB receive buffer, and
`[Option<DialConn>; 4]` — ~24 KB — sits on `serve`'s stack frame alongside the
already-large server-connection array. The network-stack postmortem bumped this
same stack 16→24→32 KB three times; the lesson is apparently not learned once. The
fix was to right-size the buffers to the actual target (small transactions):
512 B send, 1 KB receive, two connections. *A `no_std`, no-heap server pays for
every buffer in stack it can't grow — size buffers to the workload, not to a
comfortable-looking round number.*

## Verified against a foreign observer, again

The test that matters didn't read `netd`'s own logs. A host TCP server (the
foreign observer) listened; the host 9P client drove the **guest's** `/net/tcp`
over the export to dial back out to that server. The server logged the
connection arriving **from the guest's NIC** and the guest-forwarded request
bytes verbatim; the reply streamed back through the guest to the client. That the
bytes made a full round trip *through an independent server* — not that netd
printed "connected" — is the proof. Same discipline as the exFAT/ext2 `fsck`
checks and the auth arc's Python-vs-Rust HMAC: the observer has to be something
that doesn't share your code's assumptions.

## Scope held (named, not faked)

Deferred out loud: inbound `listen`/`accept` through A (dialing *in* to the
cluster's network); UDP; flow/congestion control beyond a single-segment
stop-and-wait window; and the full Plan 9 ctl command set. Each is a real
follow-on with no consumer yet.

## The one-line lesson

*A constraint you accept early (path-based verbs, no fids) can be the exact thing
that lets a hard feature fit later; and in an event-loop server the handler's job
is to record intent, never to wait — the loop is the only thing allowed to block
on the world.*

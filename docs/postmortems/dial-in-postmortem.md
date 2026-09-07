# Dial-in — the `/net/tcp` accept postmortem (the sixteenth)

*A design retrospective, 2026-08-26 — the mirror of [dial-out](dial-out-postmortem.md).
Dial-out let a machine open a connection *out of* another's NIC; dial-in lets a
machine **accept** connections *on* another's NIC. A client that connects to
machine A's address is answered by a program running on B — A is pure ingress,
B owns the service. This closes the `/net/tcp` model symmetrically: active open
(dial-out) and passive open (dial-in).*

## The consumer question, answered honestly again

I opened this arc the same way I opened dial-out: *is there a consumer something
cheaper doesn't already serve?* And the honest answer this time was weaker.
Dial-out had a concrete win — fetch the web through A. Dial-in's value is
NAT-traversal-shaped (expose a B-hosted service at A's reachable address), which
is real but **no existing cluster feature demands it**. `cpu A <server>` runs a
server *on A*; dial-in is specifically for when the server's *state* must live on
B while A only lends its ingress. I built it anyway — to *complete the model*
symmetrically, with the consumer question flagged, not hidden. That's the honest
posture: "this closes a mechanism" is a legitimate reason, as long as you say
that's what it is and don't pretend a consumer is clamoring.

## Why it was mostly reuse: an accepted connection is just a DialConn

The satisfying part: dial-in added a *capability* (passive open + accept
fan-out) with almost no new *machinery*. An accepted inbound connection is a
`DialConn` like any other — same send/receive buffers, same retransmit
(`pump_dials`), same inbound handling (`dial_on_segment`). The relay of bytes
between the external client and the far machine needs **no relay code at all**:
the client writes/reads `/net/tcp/M/data` over the export, which moves bytes
in/out of the same `DialConn` buffers netd's TCP already services. The handle is
still path-encoded (`/net/tcp/M/…`), so — as in dial-out — no fids.

What was genuinely new was small and local: a `Listening` state (announced port,
no peer); an `Accepting` state (passive open — SYN received, SYN-ACK sent,
awaiting the final ACK); `dial_accept` (a fresh SYN to an announced port
allocates an accepted conn and replies SYN-ACK); and a `listen` read that hands
out accepted conns via a `pending` flag. *The lesson repeats from dial-out: when a
new capability is a new **arm** on an existing state machine rather than a new
machine, it's a day, not a month.*

## The routing subtlety that could have double-accepted

Inbound TCP now has three tries in order: `dial_on_segment` (existing conns, by
4-tuple), then `dial_accept` (a fresh SYN to an announced port), then the server
path (HTTP/export on 80/564). The **order matters for correctness, not just
tidiness**: a *retransmitted* SYN — the client resending because our SYN-ACK was
lost — must match the already-accepted `DialConn` in `dial_on_segment` (its peer
is now known) and be ignored there, so it never reaches `dial_accept` to spawn a
*second* accepted connection for the same client. Getting this backwards would
have leaked a slot per dropped SYN-ACK. The fix was free once seen — put
`dial_accept` after `dial_on_segment` — but it's the kind of ordering bug that
only shows up under loss, which a lossless emulator wouldn't provoke.

## The real bug: `close` didn't flush the response

The one genuine defect, and a good one because it's the kind that passes a naive
test. `serve` writes the response, then immediately writes `close`. In the
original `pump_dials`, the `Established` state flushed queued send data but the
`Closing` state *only sent the FIN* — so a response queued microseconds before
`close` sat unsent in the buffer while netd cheerfully FIN'd the connection. The
external client would get a clean close and **no data**.

The fix names the invariant the split had violated: *data must flush before the
FIN, regardless of which state we're in*. `pump_dials` now handles send data
identically in `Established` and `Closing`, and sends the FIN only once the send
buffer is drained (`slen == 0 && inflight == 0`). The general lesson: a
teardown state that forgets it may still owe the peer buffered bytes is a
classic half-close bug — TCP's `FIN` means "no *more* data," not "abandon what's
queued."

## The trap that came back, for the fifth time: the stack

First boot after wiring dial-in: netd faulted immediately, decoded to a
guard-page permission fault — a **stack overflow**, again. Bumping `MAX_DIAL`
from 2 to 4 doubled the `[Option<DialConn>; MAX_DIAL]` array (each ~1.6 KB) on
`serve`'s 32 KB stack, and the ~3 KB of margin wasn't there. This is the *fifth*
time this exact class has bitten (the network stack bumped netd's stack
16→24→32 KB three times; dial-out hit it once). Each time the fix is the same
shape: right-size to the actual workload. Here that meant `MAX_DIAL=3` (a
listener + two concurrent accepts is a fair "small fan-out") and trimming the
receive buffer to 768 B (reads are capped at 512 anyway). *A no-heap server pays
for every buffer in stack it can't grow; the recurring mistake is reaching for a
comfortable round number instead of the number the workload needs.* At this point
the honest note is that the reflex should be inverted: on a fixed 32 KB stack,
assume any per-connection array growth is a stack question first.

## Verified against a foreign observer, again

The test kept both roles on the host but neither in netd's own logs. A host
socket connected to the guest's announced port (through a hostfwd) as the
*external client*; a separate host driver ran the *server side* over the export
(announce, `listen`, read the request, write the response, close). The external
client received the served response byte-for-byte, having reached it purely
through the guest's passive-open accept and relay. That an independent socket got
the right bytes — not that netd printed "accepted" — is the proof.

## Scope held

Small fan-out (a listener + two accepts), TCP only, stop-and-wait, one
accept-then-exit in the `/bin/serve` demo. Deferred, named: a persistent
multi-accept server loop, UDP, and richer listener semantics. And the consumer
question stays open — this completes `/net/tcp`, and waits for a workload that
actually wants inbound-through-another-machine to justify going further.

## The one-line lesson

*Symmetry is a real reason to build — as long as you say so instead of
manufacturing a consumer; and on a fixed stack, every per-connection buffer is a
stack-budget decision before it is anything else.*

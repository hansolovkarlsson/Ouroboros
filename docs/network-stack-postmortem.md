# A network stack in a day: the driver split, the async gap, and testing loss on a lossless wire

This is a write-up of one day's work on
[Ouroboros](../README.md), a from-scratch ARM64 microkernel, that took it
from *no networking at all* to a **concurrent HTTP server you can point a
browser at** — plus `ping`, `resolve` (DNS), and `fetch` (client HTTP) at
the shell. Everything is hand-rolled, `no_std`, no crates: a virtio-net
driver, ARP, IPv4, ICMP, UDP, DNS, and a TCP that does flow control, fast
retransmit, a retransmit timeout, and multiple simultaneous connections.

It's a companion to this project's earlier postmortems. Four of those
([`boot-bringup`](boot-bringup-postmortem.md),
[`shell-and-filesystem`](shell-and-filesystem-postmortem.md),
[`xhci-keyboard`](xhci-keyboard-postmortem.md),
[`usb-storage`](usb-storage-postmortem.md)) are bug-hunt narratives; three
([`isolation-and-dataflow`](isolation-and-dataflow-postmortem.md),
[`console-server`](console-server-postmortem.md),
[`capability-and-hardening`](capability-and-hardening-postmortem.md)) are
design retrospectives. This one is both: a design story (the driver/protocol
split, the async-receive primitive) wrapped around three real bugs that only
surfaced under test.

Like the others, it's kept out of the project's own record
(`CLAUDE.md`, `CHANGELOG.md`) because most of it isn't Ouroboros-specific. If
you've ever written a TCP from scratch, or tried to test loss recovery on a
network that never loses a packet, some of this may save you time.

## The shape of the day

Networking landed as one long staged arc, each stage built and tested before
the next depended on it:

- **Stage 1** — the virtio-net driver: raw Ethernet frames in and out,
  proven by a hand-built ARP round-trip.
- **Stage 2** — the driver/protocol split: the protocol stack moved to a
  userland server (`netd`), reached through gated syscalls; ARP + IPv4 + ICMP
  and a `ping` command.
- **Stage 3** — UDP, proven by real DNS resolution (`resolve`).
- **Stage 4a** — a client TCP (`fetch` — a real HTTP GET to a real site).
- **Stage 4b** — the async-receive primitive, and `netd` becomes a *server*
  (answers the network) rather than only initiating.
- **Stage 4c–4g** — a real static-file HTTP server: it serves files from the
  filesystem server, with a directory listing, correct `Content-Type` /
  `Content-Length`, and `HEAD`.
- **Stage 4h–4i** — TCP loss recovery: fast retransmit, then a
  timer-based retransmit timeout (RTO).
- **Stage 4j** — concurrent connections.

Three of those had a bug or a design turn worth writing down. The rest were
"read the spec, hand-roll it, cross-check the bytes."

## Thread 1: the driver/protocol split is forced, not chosen

The very first architectural decision was where the code lives. This project
is a microkernel: it had already moved its filesystem and console *out* of
the kernel into supervised, MMU-isolated userland servers. Networking wants
the same — but there's a hard constraint that decides *which* half stays in
the kernel.

**There is no IOMMU.** A device programmed to DMA can read or write
*anywhere* in physical memory, not just the buffers you meant to give it.
So the code that owns the device's DMA rings and buffers cannot be an
untrusted EL0 task — a bug (or a compromise) there is a write primitive over
all of RAM. The DMA owner has to stay in the trusted kernel. That's not a
preference; it's the same rule that already kept the virtio-blk driver in the
kernel.

So the split writes itself: the **DMA-owning virtio-net driver stays in the
kernel** (rings, buffers, the 12-byte virtio-net header), exposed through
three gated syscalls — `NET_SEND`/`NET_RECV`/`NET_MAC` — that move *opaque
frames* and are accepted only from one task. The **entire protocol stack**
(ARP/IP/ICMP/UDP/DNS/TCP/HTTP) lives in `netd`, a new userland server, which
holds the only capability to call those syscalls. The kernel never learns
what a TCP segment is.

The payoff is concrete: a whole new transport (UDP in Stage 3, TCP in Stage
4) is *pure userland code* — no kernel change at all, because the kernel only
ever moved bytes. The lesson generalizes past "no IOMMU": **let the trust
boundary, not convenience, decide the split.** The narrowest thing that must
be trusted (here, the DMA owner) is the only thing that should be privileged;
everything above it is a normal program.

## Thread 2: a server that can only initiate isn't a server

Stages 1–4a all *initiate*: the guest sends a request and waits for the
reply. `ping`, `resolve`, `fetch` are all "send, then poll for the answer."
That's easy — the server's loop is `send(); loop { recv() }`.

A *server* is the opposite: it must react to input that arrives
**unsolicited**, with no outstanding request. And that exposed a gap that had
been invisible until now. `netd`'s loop blocked in one of two ways — waiting
on an IPC message (a client's `ping` request) *or* polling the NIC for a
frame — **but a task can only block on one thing.** Block on IPC and you're
deaf to the wire; poll the wire and you spin (and the supervisor's
heartbeat, seeing a task that never returns to a blocked state, restarts you
as wedged).

This is the classic poll/select problem, and it needed a kernel primitive.
The fix was a new blocking reason — a task can wait for **"a frame arrives on
the NIC *or* a message lands in my mailbox,"** whichever comes first (a
`NET_WAIT` syscall, backed by a `WaitReason::NetInput` the scheduler's
per-tick wake-check evaluates by peeking both sources). It consumes neither;
the woken server drains both itself. That single primitive turned `netd` from
a request/reply box into an event loop, and the guest started *answering* the
network: replying to ARP for its own address, and serving TCP.

The primitive grew once more, in Stage 4i, for a reason worth noting: **RTO
needs to fire when nothing is arriving.** A retransmit timeout exists
precisely for a silent peer — no frames, no messages — so a wait that only
wakes on input can never trigger it. So `NET_WAIT` gained an optional
timeout (`WaitReason::NetInput { deadline }`): wake on a frame, a message,
*or* a deadline. It's the only `WaitReason` in the system with a timer, and
it's general — any future timed wait can reuse it. The broader point:
**a blocking primitive that can't also time out can't drive a protocol
timer.** If you're building an event loop, build the timeout in from the
start.

## Thread 3: testing loss recovery on a wire that can't lose a packet

The two hardest stages — fast retransmit (4h) and RTO (4i) — recover from
*lost segments*. The dev environment is QEMU's user-mode networking (SLIRP),
which is a reliable local path: **it never drops a packet.** So the exact
condition these features exist for cannot occur in testing.

The answer was to **inject the loss in the sender itself**: a temporary
one-line drop in the send path — skip putting one segment on the wire, but
still advance the send cursor as if it had gone out, so the peer sees a gap.
That's a genuine lost segment from the peer's point of view, and it forced
the recovery path to run. Both temporary hooks were reverted after
confirming recovery; the acceptance test was the file arriving
**byte-identical** despite the injected drop.

Isolating *which* recovery mechanism fired took one more step. A mid-stream
drop makes the peer send duplicate ACKs, which triggers **fast** retransmit —
so to test **RTO** specifically, fast retransmit had to be temporarily
disabled, leaving the timer as the only path back. With it off, the injected
drop stalled the transfer for ~1 second (the RTO), then recovered — and the
transfer took measurably longer (~6.7 s vs ~5 s), which is the RTO wait made
visible. That combination — inject a drop, disable the faster path — is the
general recipe for testing a fallback mechanism: **you can't test the
fallback until you disable the thing that would rescue you first.**

Two real bugs came out of this testing that no amount of reading would have
found:

**The go-back-N wrap-to-huge stall.** After a fast retransmit rewinds the
send cursor back to the gap and resends, a *buffering* receiver (real
Linux/macOS TCP buffers out-of-order segments) can acknowledge *past* the
rewound point in a single jump once the gap is filled. That left the
sender with `snd_una` (highest acked) *ahead of* `snd_nxt` (next to send).
The in-flight calculation `snd_nxt - snd_una`, done in unsigned arithmetic,
wrapped to ~4 billion — so the congestion/flow-control check "is there room
in the window?" always answered *no*, and the transfer stalled forever at
~86 KB. The fix is a one-line invariant — **never let `snd_nxt` fall below
`snd_una`**; when an ACK moves past it, fast-forward the cursor — but the
*symptom* (a clean stall at a specific offset, no crash, no fault) is the
kind you only diagnose by watching the sequence numbers in a packet trace.

**The supervisor restarting the server mid-transfer.** The server is
supervised: a userland server that stops responding to a periodic health-
ping (~160 ms budget) is judged wedged and restarted. Streaming a large file
is *slow* here — every 1400-byte chunk is an IPC round-trip to the filesystem
server plus an emulated disk read — so an early version that sent a whole
window in one uninterrupted burst blocked the server long enough to miss the
ping and get **restarted mid-transfer** (losing the connection state, which
lives on its stack). The fix wasn't "make it faster"; it was **yield often
enough to stay responsive**: send in small bursts, draining the mailbox (and
so acking the health-ping) between each. The lesson: *supervision and
long-running work are in tension, and the long work has to cooperate.* A
server that can't be preempted must voluntarily check in.

## Thread 4: the guard page as a running stack budget

An earlier milestone gave every userland program a **stack guard page** — an
unmapped page just below the stack, so an overflow faults cleanly instead of
silently corrupting the code below it. Across the network arc, that guard
earned its keep as a *budget check that reports when you've exceeded it*.
`netd` overflowed its stack **twice more** as it grew, each caught as a clean
fault at the guard page rather than a mystery corruption:

- The client-op path (`fetch` → the TCP client → DNS) nests several
  1600/2048-byte frame buffers down one call chain, and tipped over the
  16 KB stack → grown to 24 KB.
- Concurrent connections (Stage 4j) put an **array** of connection structs
  (each carrying a ~2 KB response buffer) permanently on the server's stack
  frame, and a client op nesting *on top of that* tipped over 24 KB → grown
  to 32 KB.

The second one has a diagnostic detail worth keeping: **which half overflowed
told us where to look.** The concurrent *streaming* path was fine — four
simultaneous transfers ran with zero faults — because pumping data doesn't
nest deeply. It was the *client* path, nesting its buffers on top of the now-
larger permanent frame, that overflowed. A guard-page fault isn't just "you
used too much stack"; combined with *which operation* triggered it, it points
straight at the deep call chain. `netd` is now by far the most stack-hungry
program in the system (an array of connections *and* a full TCP client),
which is itself a useful thing to have learned by measurement rather than
guessing.

## Thread 5: verify against something that isn't your own code

A recurring discipline, not unique to this arc but sharpest here: **never
confirm a network feature from the kernel's own log alone.** Every stage was
cross-checked against a `tcpdump` decode of a packet capture
(`-object filter-dump` writes every frame QEMU sees to a pcap), and the
server side against a real host `curl` through a forwarded port. When the
kernel prints "sent an ARP reply" and the pcap shows the reply actually on
the wire — with checksums `tcpdump` accepts — those are two independent
witnesses. The bugs above (the wrap-to-huge stall especially) were *found* in
the trace: the kernel's own output showed nothing wrong; the sequence numbers
in the pcap showed the transfer jumping backward and then flat-lining. A
from-scratch stack lies to you in its own logs by construction — it only
knows what you told it to print. The wire, and a mature peer's reaction to
it, don't.

## What it adds up to, and what's honestly still missing

By the end of the day the guest is a real, if small, networked machine: it
resolves DNS, fetches web pages, and serves its own filesystem over HTTP to
a browser opening several concurrent connections — over a TCP that paces
itself to the peer's window and recovers lost segments two ways. All of it
hand-rolled, all of it in a userland server the kernel can restart if it
crashes.

What's deliberately not there, and why it didn't block the arc:
**IRQ-driven receive** (the NIC is still polled — the first place polling is
a real latency cost, and the biggest remaining item); **congestion control**
and **RTT estimation** (only the peer's flow-control window is honored; the
RTO is a fixed 1 s); **SACK** (loss recovery is go-back-N, which re-sends
data a buffering receiver already has); and it's **QEMU-only** (the real
Parallels target exposes virtio-net over PCI, a transport this project's
virtio path doesn't drive). None of these are hard to *see* the shape of;
they're just the next milestones.

The meta-lesson of the day is the staging itself. A network stack sounds like
one enormous thing. Split into "driver, then one protocol at a time, each
proven end-to-end before the next," it became ten small milestones, each of
which either worked on the first real test or failed in a way a packet trace
explained in minutes. The hard parts — the async gap, testing loss, the stack
— announced themselves exactly when the milestone that needed them arrived,
and not before.

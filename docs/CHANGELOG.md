# Ouroboros changelog

Historical record of completed milestones, newest first. For
forward-looking plans, see [`roadmap.md`](roadmap.md); for the
debugging history and lessons behind each decision (what was tried,
what broke, how it was diagnosed), see the debugging postmortems under `docs/`; for *how* something
here actually works today, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md).

## Network stack, Stage 4n: TCP congestion control (Reno)

The send window was bounded only by the peer's advertised flow-control
window. This adds a congestion window (`cwnd`) — TCP Reno — so the send rate
is `min(cwnd, peer window)`: it ramps up gently on a fresh connection and
backs off on loss.

Per-connection `cwnd`/`ssthresh` (bytes): `cwnd` starts at `INIT_CWND`
(4·MSS) and grows on each new ACK — by up to a segment per ACK in slow start
(cwnd < ssthresh, ~doubling per RTT), by ~MSS²/cwnd per ACK in congestion
avoidance (~one segment per RTT) — capped at the 16-bit window ceiling (no
window scaling negotiated). Loss cuts it: a fast retransmit (3 dup-ACKs)
halves cwnd to ssthresh (multiplicative decrease); an RTO drops cwnd to one
segment (back to slow start), the stronger response to the stronger loss
signal. All wired into the existing send-window computation and the
already-proven fast-retransmit / RTO branches — no new state machine.

Verified on QEMU with temporary instrumentation (reverted): a 60 KB download
showed textbook Reno in the cwnd log — slow-start growth 5601 → 26604 (+MSS
per ACK), then an injected drop (fast retransmit enabled) halved it to
ssthresh = 13302, after which congestion avoidance grew it ~+146/ACK
(MSS²/cwnd, the additive-increase phase, distinct from slow-start's +1400) —
the file downloading byte-complete throughout. The pcap showed the initial
burst metered by cwnd (≈4 segments) rather than the whole peer window. A
clean run streamed byte-complete with client `ping`/`fetch` unregressed;
zero `-d int` aborts.

On lossless SLIRP the reduction paths need injected loss to exercise (cwnd
otherwise just ramps to the peer window and stays); the visible everyday
effect is the slow-start ramp at the start of a transfer. Still QEMU-only;
this is Reno (no fast-recovery inflation, and go-back-N retransmit is
unchanged — SACK stays the one open TCP item).

## Network stack, Stage 4m: HTTP 405 for unsupported methods

The server treated every request like a GET (it only distinguished HEAD). A
request with any other method (POST, DELETE, …) now gets a proper `405
Method Not Allowed` with the `Allow: GET, HEAD` header RFC 7231 requires.
Pure `netd`, one check: `start_response` rejects anything but GET/HEAD up
front — before touching the path, so an unsupported method never reaches
`fsd` — reusing the same fixed-response prefix mechanism as 404/503.

Verified on QEMU: `curl -X POST /` and `curl -X DELETE /file` → `405 Method
Not Allowed` + `Allow: GET, HEAD` + a plain-text body; `GET` (200, correct
Content-Type/Content-Length) and `HEAD` (200 headers, no body) unregressed;
zero `-d int` aborts.

## Network stack, Stage 4l: RTT-estimated RTO

The retransmit timeout (Stage 4i) used a fixed 1 s base. This makes it
adaptive: an RFC 6298 estimator (`TcpConn`/`rtt_update`) measures each
connection's round-trip time and derives the RTO from a smoothed SRTT and
its variation (RTTVAR), replacing the fixed base — a fast peer gets a fast
RTO, a slow one a patient one.

Meaningful RTT estimation needs a finer clock than netd's `now()` (the 20 ms
preemption tick — a fetch RTT of tens of ms is 1–2 of those). A new kernel
syscall, `MONOTONIC_US` (46), exposes the ARM generic timer's free-running
counter as microseconds since boot (overflow-safe: whole seconds plus the
sub-second remainder, not a naive `ticks * 1_000_000` that overflows a u64
in days). Pure system-register reads, not gated — a monotonic clock is a
harmless read for any task.

The estimator, per RFC 6298: the first sample R sets SRTT = R, RTTVAR = R/2;
later samples fold in RTTVAR = ¾·RTTVAR + ¼·|SRTT−R|, SRTT = ⅞·SRTT + ⅛·R;
then RTO = SRTT + max(G, 4·RTTVAR), converted to `now()` ticks and clamped
to [200 ms, 2 s] (the floor matches the `NET_WAIT` poll so a minimum-RTO
timer fires at the next poll; the ceiling stays under the supervisor's
~2.5 s wedge threshold). `RTO_INIT_TICKS` (~1 s, the RFC's initial value) is
used until the first sample. `service_rto` arms the timer with this
per-connection estimate instead of a fixed base.

One RTT sample is timed at a time (Karn's algorithm): a sample starts when
genuinely new data is sent (tracked via a send high-water mark `snd_max`, so
a retransmit is never timed), completes when its end sequence is acked, and
is invalidated by any retransmit (`rewind_to` clears it — the ACK would be
ambiguous). The exponential backoff on repeated firings is unchanged.

Verified on QEMU with temporary instrumentation (reverted): the server's RTT
samples logged real values with correct RFC-6298 first-sample maths (srtt =
rtt, rttvar = rtt/2) and an adaptive RTO (10 and 47–49 ticks, not the fixed
50); loss recovery via the adaptive RTO was confirmed by injecting one
dropped segment with fast retransmit disabled — the file still downloaded
complete (59968 bytes), so the RTO fired, resent from `snd_una`, and
recovered. A clean run (no injection) streamed the file byte-complete with
no spurious retransmits, and client `ping`/`fetch` worked; zero `-d int`
aborts throughout.

A documented characteristic, not a bug: netd is single-threaded and
processes a segment's ACK only after pumping a window, so a busy transfer's
measured RTT includes netd's own send latency (~300 ms for a 60 KB file over
SLIRP, vs ~9 ms for a tiny one). This *over*-estimates the RTO, which is the
safe direction (a too-short RTO would retransmit spuriously) — it is not
pure wire RTT. Still QEMU-only (the whole stack is); the client `fetch` path
uses its own fixed deadlines, not the estimator (only the server has a
persistent per-connection RTO).

## Network stack, Stage 4k: IRQ-driven NIC receive

The NIC was the last driver still polled — `netd` woke on the timer tick's
wake-check (`net_has_frame()`), so a delivered frame waited up to one tick
(20 ms) to be noticed. This wires the virtio-net receive queue's interrupt
through the GIC so a frame wakes `netd` immediately. The tick-poll stays as
a fallback, so the interrupt is a latency optimization, not a correctness
dependency: a missed IRQ degrades to ≤1 tick of delay, never a lost frame.

The NIC's interrupt is a GIC **SPI**, and neither GIC backend routed SPIs
before — both only ever enabled the timer PPI. `gicv2::enable_interrupt`
now also sets `GICD_ITARGETSR` for an SPI (route it to CPU 0; an untargeted
SPI is delivered nowhere); `gicv3::enable_interrupt` gains a distributor
path for SPIs (`GICD_ISENABLER` + `GICD_IROUTER` affinity routing + Group 1
in `GICD_IGROUPR`, versus the redistributor path a PPI uses). The
slot→INTID mapping (QEMU `virt`: virtio-mmio slot *i* → SPI 16+*i* → GIC
INTID 48+*i*) was confirmed via a devicetree dump, like the transport
addresses themselves.

`virtio_net::init` suppresses *transmit* interrupts
(`VIRTQ_AVAIL_F_NO_INTERRUPT` on the TX ring — `send_frame` polls), so the
device's single interrupt line only ever means "a receive frame arrived"
and the handler needn't disambiguate queues. `rust_irq_handler` acks the
device (`InterruptStatus` → `InterruptACK`, required or the device won't
raise the next one) and calls a new `tasks::on_net_irq`, which wakes the
`NetInput`-blocked server and switches to it immediately — the same
frame-overwrite contract as `on_tick`. `netd` drains all frames and
re-posts buffers itself, as before. Enabled only when a NIC was actually
installed (QEMU only, behind `virtio_mmio_probe_safe`); the no-NIC path is
untouched.

Verified on QEMU, both GIC versions, with temporary IRQ instrumentation
(reverted): under default **GICv2**, the receive IRQ fired for every
inbound frame — 2 per `ping` (ARP + ICMP reply), 2 per `resolve`, 7 for a
`fetch` — with `ping`/`resolve`/`fetch` all working, `uptime` advancing
(the timer PPI coexists with the NIC SPI), and zero `-d int` aborts; under
forced **GICv3** (`-machine virt,gic-version=3`), the same, via the
distributor SPI path. A no-NIC boot (`selftest`/`ls`/`uptime`) was
byte-unchanged with no interrupt enabled. Still QEMU-only (Parallels'
virtio-net is PCI, an unsupported transport); receive is now
interrupt-driven, transmit still polls.

## Network stack, Stage 4j: concurrent TCP connections

The server handled one connection at a time — a second peer's SYN *replaced*
the first, so a browser (several connections per page) or multiple clients
couldn't be served together. This multiplexes up to `MAX_CONNS` (4)
connections, keyed by peer IP+port.

The single `Option<TcpConn>` became `[Option<TcpConn>; MAX_CONNS]`;
`handle_tcp` is now a router (a SYN opens a connection in a free slot —
dropped if all busy, the peer retransmits — every other segment dispatched by
`find_conn` to its connection's state machine, extracted into
`handle_conn_segment`). `serve()` services the RTO and pumps *each* active
connection per wake. Each connection's flow control / fast retransmit / RTO
were already per-`TcpConn`, so this was purely routing + a per-connection
loop.

A guard-page overflow surfaced — and which half overflowed is the point: the
concurrent *streaming* path was fine (4 concurrent 60 KB streams, zero
faults), but a *client* op (`fetch`'s ~5 KB of TCP/DNS buffers) nesting on
top of 4 `TcpConn`s (~2.3 KB each) on `serve()`'s frame overflowed 24 KB.
`STACK_PAGES` grew 6→8 (24→32 KB/region); netd is by far the most
stack-hungry program (4 connections + a full TCP client).

Verified on QEMU: 4 concurrent small fetches all correct; 4 concurrent 60 KB
`SH.BIN` streams all byte-identical (~1 s); a single connection still works;
client net ops, disk, a pipe, `exec`, selftest unregressed; zero aborts, zero
restarts, zero EL0 faults. Still coarse: 4 connections max, a SYN with no
free slot is dropped, round-robin servicing, QEMU-only.

## FAT32 long filename (LFN) read support

The oldest FAT32 limitation, closed on the read side — surfaced by real use:
serving `index.html`. A 4-char extension can't be an 8.3 short name, so a
formatter writes a long filename (LFN) entry plus a mangled alias
(`INDEX~1.HTM`), and `fat32.rs` (in `fsd`) only saw the alias. Now long names
are **read, listed, and matched** by their true names: the web server serves
`/index.html` (correct `text/html` type) and directory listings show real
names.

`walk_dir_with_location` accumulates the run of LFN entries preceding each
short entry — 13 UTF-16 chars each, in reverse order, placed by sequence
number (order-independent), reconstructed to ASCII. The correctness piece:
the long name is used only if its checksum (byte 13, over the short 8.3 name)
matches the short entry, so an orphaned LFN run (deleted file, reused slot)
can't attach the wrong name. `DirEntry.name` grew 12→255 bytes; `name()`
returns the effective name, so `find`/`list_dir` get long names for free.

Read-only: the guest still can't *create* a long-named file (`make_short_name`
is 8.3-only), and deleting one leaves its LFN entries orphaned (harmless —
the checksum guard prevents mis-association). Both are documented follow-ups.

Verified on QEMU against `index.html`, a 21-char name (2 LFN entries), and a
long name in a subdirectory (all written by macOS): served by the web server
and read via the shell (`ls`/`cat`/`cd`, nested). 8.3 files unaffected; the
full write path unregressed; selftest, zero `-d int` aborts.

## Network stack, Stage 4i: TCP retransmit timeout (RTO)

Fast retransmit (4h) recovers a loss only while data keeps flowing (the
peer's dup-ACKs drive it). When the peer goes **silent** — the last segments
of a burst lost, or all its ACKs lost — no frames arrive and it never fires.
RTO is the timer-based fallback.

**Kernel:** `NET_WAIT` now takes a timeout. It used to wake only on a frame
or a message — useless for a timer that must fire when nothing arrives.
`WaitReason::NetInput` gained a `deadline`; the tick wake-check wakes the task
once it passes even with no input. The syscall reads `arg0` as a ms timeout.

**netd:** a per-connection RTO timer (`service_rto`, once per wake) — armed
while data is unacked, restarted (backoff reset) on ACK progress, and on
expiry (only reached when the peer is silent, since dup-ACKs would fire fast
retransmit first) resends from `snd_una` (go-back-N via `rewind_to`) with
exponential backoff (1 s base, capped) and a give-up (RST + close) after 5
tries. `serve()` uses a 200 ms `NET_WAIT` timeout while data is unacked so
netd wakes to check the timer during silence.

Verified on QEMU by temporarily disabling fast retransmit and injecting one
drop (reverted), so only the RTO could recover: a 256 KB transfer took ~6.7 s
(vs ~5 s — the RTO wait) and completed byte-identical, the timer firing once —
exercising the `NET_WAIT` timeout (netd woke during silence), `service_rto`,
and the resend. The normal lossless path is unaffected (the timer never fires
spuriously — `snd_una` advances every check): 256 KB byte-identical at normal
speed. Client net ops, disk, a pipe, selftest unregressed; zero aborts, zero
restarts. Still coarse: fixed 1 s RTO (no RTT estimation), go-back-N (not
SACK), give-up path unexercised, one connection.

## Network stack, Stage 4h: TCP fast retransmit

The server's first loss recovery. A lost segment used to stall the transfer
forever (`snd_una` never advanced past the gap) — fine on SLIRP's lossless
loopback, broken on a real network. This adds **fast retransmit**: three
duplicate ACKs at `snd_una` (the peer re-acking the last in-order byte for
each out-of-order segment past a gap) trigger a **go-back-N** resend from
`snd_una`. Driven by incoming frames — no timer, no kernel change.

`TcpConn` tracks `dup_acks` and `last_rexmit_una` (so leftover dup-ACKs don't
double-retransmit the same gap); `rewind_to(seq)` repositions the send cursor
(the response is a fixed `[prefix][file body][FIN]` stream from `SERVER_ISN+1`,
so a seq maps back to a prefix/file offset), and `pump_send` resends.

A real bug found by testing (an injected drop, since SLIRP can't lose
packets): the first cut recovered the gap but stalled at ~86 KB — a buffering
receiver acks *past* the rewound `snd_nxt` once the gap is filled, leaving
`snd_una > snd_nxt` so the window wraps to look permanently full. Fixed by
keeping `snd_nxt >= snd_una` (fast-forward the cursor when an ACK moves past
it).

Confirmed on QEMU with the drop injection: a 256 KB file that would stall now
streams byte-identical at normal speed; the pcap shows textbook go-back-N
(segments climb to the window edge, jump back to the gap sequence, re-climb).
The normal lossless path is unaffected (retransmit code dormant without
dup-ACKs); shell + client net ops + selftest unregressed; zero aborts, zero
restarts. Still coarse: fast retransmit only (no timer-based RTO for a silent
peer — needs a `NET_WAIT` timeout), go-back-N (not selective/SACK), one
connection.

## Network stack, Stage 4g: HTTP HEAD

A `HEAD` request now returns exactly the headers a `GET` would (including
`Content-Type`/`Content-Length` for a file) with no body. Small, self-
contained, and correct for every response kind: `start_response` builds the
response as usual, then for a HEAD trims the prefix to the header block
(through the first `\r\n\r\n` — present in the file 200, the directory
listing, and the 404/503) and streams no file body. `is_head()` matches
`"HEAD "` (a non-GET/HEAD method is still served like a GET — no 405 yet).

Confirmed on QEMU with a raw client: `HEAD /EFI/ORBS/INIT.CFG` → 200,
Content-Length 16, 0-byte body (headers byte-identical to the GET, which
still returns its 16-byte body); `HEAD /EFI/ORBS` → text/html, 0-byte body;
`HEAD /nope` → 404, 0-byte body. Shell + client net ops + selftest
unregressed; zero aborts, zero restarts.

## Network stack, Stage 4f: a browsable directory listing

A `GET` whose path resolves to a directory (including `/`) now returns a
generated HTML index of its entries, each a link — so the guest's filesystem
is browsable in a browser: `/` → `EFI/` → `ORBS/` → `INIT.CFG`, click through
and open files. Pure `netd`, no kernel/capability change.

`start_response` distinguishes file / directory / neither: stat the path
(`FSOP_READ_FILE`) → a file is served with headers as before; else (not a
no-fs/no-server case) try `list_dir` (`FSOP_LIST_DIR`) → a directory is
rendered as an index; else 404 (no-fs/no-server → 503). `/` lists the root
now (the old fixed landing page is gone). The two fsd calls were unified into
one `fsd_call` helper; `build_listing` turns fsd's newline-separated entries
(dirs suffixed `/`) into `<li><a href=…>` links resolved against the request
path (`.` filtered, `..` kept for parent nav). `PREFIX_MAX` 512→2048 to hold
a listing.

Confirmed on QEMU: `GET /` → `EFI/`; `GET /EFI` → `../ ORBS/ BOOT/`;
`GET /EFI/ORBS` → all the `.BIN` files + `INIT.CFG`, each linked;
`GET /EFI/BOOT` → `BOOTAA64.EFI`; files still serve with Content-Type/Length;
404 for a missing path; hrefs correct at every level. Shell + client net ops
+ selftest unregressed; zero aborts, zero restarts. Still coarse: capped by
fsd's ~512-byte inline listing (big dirs truncate), names+links only (no
sizes/timestamps/sorting), no Content-Length on the listing.

## Network stack, Stage 4e: proper HTTP response headers (Content-Type + Content-Length)

A small polish over the file server, which sent every file as
`application/octet-stream` with no length (so browsers downloaded instead of
rendering, and clients relied on connection-close to find the end). Pure
`netd` — no kernel/capability change.

`start_response` now *stats* the file (one `FSOP_READ_FILE`, whose status is
the real size — `want=1`, the smallest fsd accepts) instead of a probe read:
that both checks existence and gives the size for `Content-Length`.
`content_type()` maps the path extension (case-insensitive) to a MIME type
(html/htm, txt/cfg/md/log, css, js, json, png, jpg/jpeg, gif; else
octet-stream). `build_200_header()` formats the 200 with both headers
(hand-rolled `u64_decimal`); `TcpConn.prefix` became an owned buffer so the
header can be built per-request.

Confirmed on QEMU (`curl -i`): `.cfg`→text/plain len 16, `.htm`→text/html
len 63 (renders), `.bin`→octet-stream len 59968, a 256KB file→Content-Length
262144 with a byte-identical streamed body, 404 for a missing path. (A
`.html` name — 4-char extension — 404s because it isn't a valid FAT 8.3 name
and fsd has no LFN support; not a bug, the documented limitation. `.htm`
works.) Shell + client net ops unregressed; zero aborts, zero restarts.

## Network stack, Stage 4d: TCP send-side flow control (stream files of any size)

Stage 4c capped a served file at ~16 KB because it blasted the whole body at
once — fine only if it fits the peer's window. This adds real **send-side
flow control**: the server never lets unacknowledged data (`snd_nxt -
snd_una`) exceed the peer's advertised window, and streams the body *paced by
the peer's ACKs*, so a file of any size flows. The cap is gone.

The TCP server is stateful across event-loop wakes now: `TcpConn` tracks
`snd_una` (advanced by ACKs), the peer's `window`, and the in-progress
response (a fixed prefix, then a file body streamed from `fsd` by rising
offset). `handle_tcp` updates `snd_una`/`window` per segment but no longer
sends inline; `pump_send` sends one window-bounded segment and `serve()`
loops pump + a mailbox drain, flushing a full window per wake then blocking
in `NET_WAIT` for the next ACK.

**The per-segment mailbox drain is the subtle, load-bearing part.** A large
transfer is many slow `fsd` reads (each chunk = an IPC round trip + a
virtio-blk read, tens of ms under TCG), so netd is busy for seconds — and
the supervisor's ~160 ms health-ping would judge it wedged and **restart it
mid-transfer** (losing the connection state; observed directly as a
truncated download). Draining the mailbox — acking the ping — after *every
segment* keeps the worst ack latency to one segment. Bursts of 16 and 48
each occasionally overran the timeout under TCG; per-segment draining removes
the race at negligible cost.

A real stack overflow surfaced too: the extra frame tipped netd's client-op
path (already nesting 1600/2048-byte buffers near the 16 KB edge) into its
**guard page** — a clean contained fault, exactly what the guard is for.
`STACK_PAGES` grew 4→6 (16→24 KB/region); netd is the most stack-hungry
program.

Confirmed on QEMU: a 256 KB file streams byte-identical three times in a
row, zero restarts/faults, plus 60 KB `SH.BIN`, the small responses, and a
full shell+disk+client-net regression — zero `-d int` aborts. Throughput is
disk-bound (~55 KB/s under TCG), not a TCP-layer limit. Still coarse: no
retransmission, no congestion control, one connection, QEMU-only.

## Network stack, Stage 4c: a static-file HTTP server (netd serves files from fsd)

Stage 4b's server answered every request with one fixed page. This makes it
real: it parses the request-target path and streams that file from the
filesystem server over TCP — `curl localhost:5555/EFI/ORBS/INIT.CFG` returns
the file's actual bytes. The guest serves its own filesystem.

**The interesting part is the cross-server flow, not the parsing:** `netd`
becomes `fsd`'s **first non-shell client**. A request arrives at `netd` over
TCP → `netd` `MSG_CALL`s `fsd` to read the file → `netd` streams it back —
two userland servers cooperating on one external request, the driver-isolation
design paying off. One visible capability change makes it legal: `netd`'s
send-mask (`caps_for_slot`) gains `TO_FSD`. The read reuses the shell's exact
grant/safecopy `FSOP_READ_BULK` path (`cat`'s), ported into `netd`.

`serve_http` serves a landing page for `/`, else probes the file (so a 404
replaces the 200 header rather than following it) and streams a 200 header
plus the body in `SERVE_CHUNK` (1400)-byte segments (one `FSOP_READ_BULK`
each), advancing `snd_nxt`; 404 for a missing file, 503 for no filesystem.
Bounded at `MAX_SERVE` (~16 KB) — no flow control yet, so the body must fit
the peer's window; larger files truncate.

Confirmed on QEMU (`make run-image-server`, zero `-d int` aborts): `curl /`
→ the page; `curl /EFI/ORBS/INIT.CFG` → HTTP 200 + the real file content;
`curl /nope` → 404; `curl /EFI/ORBS/SH.BIN` (72 KB) → the first 16800 bytes,
**byte-identical** to the real file, over 12 body segments + a header (the
multi-segment path). Shell disk commands, `selftest`, and client net ops
unregressed. Still coarse: the ~16 KB cap, no `Content-Length`, one
connection, no directory listing, QEMU-only.

## Network stack, Stage 4b: an async-receive event loop, and a TCP HTTP server

The guest **answers** the network now, not just initiates. Stage 4a's gap
was that `netd` was client-only — it blocked on `MSG_RECV`, so nothing
watched the wire between requests, and a task can't block on IPC *and* poll
for frames at once. This adds the missing async-receive primitive (the
poll/select gap flagged since Stage 2) and a TCP HTTP server you can `curl`
from the host.

**Kernel (a minimal poll/select):** `virtio_net::has_frame()` peeks the RX
used-ring without consuming; `NET_WAIT` (syscall 45, gated to `CAP_NET`)
blocks the caller until either a frame or a message is pending, then returns
(the caller drains both itself). `tasks.rs` gained `WaitReason::NetInput`
(wakes on frame-or-message, consuming neither) and a `send_message` wake so
client calls to `netd` still land sub-tick.

**`netd`:** `serve()` is an event loop now — block in `NET_WAIT`, drain
client requests (`ping`/`resolve`/`fetch`, unchanged) *and* incoming frames.
Frames feed an ARP responder (answers requests for our IP) and a minimal
TCP server on port 80 (SYN → SYN-ACK, request → a fixed HTML page + FIN,
then acks the peer's FIN and closes). One connection at a time, state on
`serve()`'s stack (no static mutable state in userland). `build_tcp` split
into `build_tcp_generic` + client/server port wrappers.

Confirmed on QEMU (`make run-image-server`, SLIRP hostfwd tcp::5555→:80):
host `curl http://localhost:5555/` → `HTTP/1.0 200 OK` + the page, three
connections in a row; the pcap showed a textbook SYN/SYN-ACK/GET/200/
FIN/FIN-ACK exchange. Client ops (`ping`→reply, `resolve`→real DNS,
`fetch`→Example Domain) and the full disk/selftest/pipe surface unregressed;
zero `-d int` aborts. Still coarse: one fixed page, one connection at a
time, no retransmission/congestion control, QEMU-only.

## Network stack, Stage 4a: a client TCP, and a `fetch` command (real HTTP)

The stack's first **TCP**: `fetch <hostname>` opens a client TCP connection
on port 80, sends a minimal HTTP GET, and prints the response — a from-scratch
microkernel fetching a real web page over hand-rolled TCP. Two calls confirmed
with the user first (TCP was the roadmap's "separately scoped" milestone):
**hand-roll** (not `smoltcp`) and **minimal client scope** (active-open only).
**No kernel changes** — `netd` uses the Stage 2 `NET_*` syscalls.

`fetch` chains the whole stack: `netd`'s `NETOP_FETCH` resolves the hostname
(reusing `resolve_ip`), picks the next hop (a minimal default route —
on-subnet → target, else the gateway; the first time off-subnet routing
mattered), ARP-resolves it, and runs a hand-rolled client TCP: `build_tcp`/
`parse_tcp` (with the **TCP checksum over the IPv4 pseudo-header**), and
`tcp_get` — the SYN(+MSS)/SYN-ACK/ACK handshake, the HTTP request, in-order
response reassembly (ACKing each segment), and a clean four-way FIN teardown.
The request carries a Host header (name-based vhosts) and `Connection: close`.
Tight timeouts keep the busy-polling fetch under the supervisor wedge
threshold. The shell's `fetch <hostname>` prints the response (truncated to
one IPC reply, with the full length reported).

Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts, cross-checked
with `tcpdump`: `fetch example.com` → `HTTP/1.1 200 OK` + the real Example
Domain HTML. The pcap showed a complete, correct TCP conversation (SYN /
SYN-ACK `ack=ISN+1` / ACK / GET / 200 / FINs — a clean four-way close), all
checksums accepted. `ping`/`resolve` still work, no supervisor restart. Still
coarse: client active-open only (no `listen`), one connection, no
retransmission, response capped at one reply.

## Network stack, Stage 3: UDP, and a `resolve` command (real DNS)

The stack's first UDP application: `resolve <hostname>` does a **DNS A-record
query over UDP** and prints the resolved IPv4. Builds entirely on Stage 2's
`netd`/`NETOP` plumbing — **no kernel changes** (the `NET_*` syscalls already
move opaque frames; UDP is just a different payload), the payoff of the
driver/protocol split.

`netd` gained a `NETOP_RESOLVE` handler (request = op + hostname; reply =
status + packed IPv4): ARP-resolve the user-net DNS server (`10.0.2.3`),
encode a DNS query (header + hand-rolled QNAME labels), wrap it in
UDP → IPv4 → Ethernet (IP checksum computed, UDP checksum 0 — optional on
IPv4), send, and poll for the response; a hand-rolled DNS parser walks the
answer records (**handling name-compression pointers**) and returns the first
A record, mapping no-answer to `NXDOMAIN` and no-response to `TIMEOUT`. The
shell's `resolve <hostname>` command packs the name into a `MSG_CALL` and
prints `<host> is a.b.c.d`.

Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts, cross-checked
with `tcpdump` — and it resolves **real hostnames** via SLIRP's DNS proxy:
`resolve example.com` → `172.66.147.243`, `resolve one.one.one.one` →
`1.0.0.1`, `resolve nope.invalidtld` → `could not resolve` (`NXDomain`). The
pcap decoded our query as `10.0.2.15.32768 > 10.0.2.3.53: … A? example.com.`;
multi-A responses parsed correctly. `ping` still works, no supervisor
restart. Still coarse: A records only, fixed DNS server, guest-initiated
only. TCP is Stage 4.

## Network stack, Stage 2: the protocol stack moves to userland (`netd`), and a `ping` command

Networking moves out of the EL1 kernel the same way `fsd` and `cond` did: the
kernel keeps only the DMA-owning virtio-net driver (no IOMMU), reached by one
task through gated syscalls, and the whole protocol stack (ARP/IPv4/ICMP)
lives in a new userland server, `netd`. It ends with a real `ping <ip>` at
the shell. Two staged commits.

**2a - a fourth protected task slot + gated `NET_*` syscalls.** `netd` is
`NET_TASK` (slot 4), the fourth boot-loaded, supervised, protected server -
inserting it shifted the spawnable slots to {5,6} (`NUM_TASKS` 6→7, the
`exit`/`kill`/`wait` protections to `<= 4`, a `CAP_NET` arm in
`caps_for_slot`, seven static arrays each +1), the same shape as when `cond`
was inserted at slot 3, regression-tested hard afterward. `NET_SEND` (42) /
`NET_RECV` (43) / `NET_MAC` (44) operate on a kernel-held `NetCell` gated to
`CAP_NET` (`netd` alone), the `BLOCK_*` → fsd pattern; `init_net` installs the
NIC instead of probing it. `netd` proved the userland NIC path with an ARP
round-trip from EL0. A subtlety: a supervised server must *reply* to every
message or the health-ping restarts it, so `netd`'s loop replies with a
status u64 - which acks the ping.

**2b - real ARP + IPv4 + ICMP + `ping`.** `netd` gained a `NETOP_PING` handler
(the `FSOP_*` shape): ARP-resolve the target, build an ICMP echo request in an
IPv4 packet in an Ethernet frame with correct IP/ICMP checksums, send it, poll
for the matching reply. Hand-rolled fixed-buffer (the checksum, the frame
builders, the reply matcher), no crates. Short timeouts (ARP ~500ms, ICMP
~1s) keep an unreachable ping under the supervisor's wedge threshold. The
shell's `ping <a.b.c.d>` parses the quad byte-by-byte (no `str::split`, PIE
relocation safety), `MSG_CALL`s `netd`, maps the status to a message.
**Scope: guest-initiated ping only** - answering a host's ping needs an async
receive loop (the poll/select gap), deliberately deferred.

Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts, cross-checked
with `tcpdump` of the pcap: `ping 10.0.2.2`/`10.0.2.3` → `reply from ...`,
`ping 10.0.2.99` → `unreachable (no ARP reply)`; the pcap showed valid ARP
and ICMP echo request/reply (`id 0x4f42`, checksums validated) for the
reachable hosts. The renumber left everything working (`selftest`, disk
surface, `exec` → slot 5, pipe → slot 6, `ps` shows netd in slot 4), no
supervisor restart. See `docs/roadmap.md`. UDP is Stage 3, TCP Stage 4.

## Network stack, Stage 1: a virtio-net driver, and the first frames this kernel has sent

The first networking this project has ever had. Stage 1 of the network-stack
arc (`docs/roadmap.md`): the **kernel-side virtio-net driver only** - the
DMA-owning half that, per the no-IOMMU DMA constraint, must stay in the
trusted EL1 kernel (a device can DMA anywhere without an IOMMU, so the
ring/buffer owner can't be an untrusted EL0 task - the same rule that keeps
virtio-blk in the kernel). The protocol stack (ARP/IP/ICMP/UDP/TCP) is a
later stage's userland `netd` server; this driver just moves opaque frames.

`kernel/src/virtio_net.rs` reuses `virtio_mmio.rs`'s transport and the block
driver's static-virtqueue + poll-the-used-ring shape (no IRQ). Two real
differences from virtio-blk, both handled: **two virtqueues** (a receiveq
with pre-posted buffers drained incrementally by `poll_frame`, a transmitq
used one-at-a-time by `send_frame`), and a **12-byte `virtio_net_hdr`** on
every frame (`VIRTIO_F_VERSION_1`; `MRG_RXBUF` deliberately not negotiated,
so one buffer per frame). Negotiation spans both feature words
(`VIRTIO_NET_F_MAC` low, `VIRTIO_F_VERSION_1` high); the MAC is read from
config space; TX completion polls a **real wall-clock deadline** off the
generic timer (the xHCI iteration-count lesson).

`main.rs::init_net` is the end-to-end proof (modeled on `init_storage`):
discover + init, log the MAC, then send a **broadcast ARP request** for the
QEMU user-net gateway and decode the reply - proving transmit *and* receive.
Gated behind `virtio_mmio_probe_safe` like storage, so **Stage 1 is QEMU-only
by design** (the scan crashes real Parallels, which exposes virtio-net over
PCI anyway - a transport this project doesn't have). Silent when no NIC is
attached, so plain `make run`/`run-image` are unperturbed.

Confirmed on QEMU **two ways** (a new `make run-net` target attaches the NIC
over virtio-mmio with QEMU user-net + an `-object filter-dump` pcap): the
kernel logs `virtio-net ARP reply - 10.0.2.2 is at 52:55:0a:00:02:02`, and
`net.pcap` decoded independently with `tcpdump` shows the ARP request out and
the reply in - TX and RX confirmed by a source outside the kernel's own
output. Regression: plain `make run` boots normally, `init_net` silent, zero
`-d int` aborts. Still just the driver: no protocol stack, no `NET_*`
syscalls yet (those land with `netd`, Stage 2), polled not interrupt-driven,
QEMU-only. See `docs/roadmap.md`.

## FAT32 interior / random-access writes (`write_at` past EOF) - and a `writeat` builtin

`fat32::write_at` refused any offset past the current end of file
(`Error::InvalidOffset`) - a sparse gap FAT32 can't represent - so it only
did append and sequential-overwrite, which is all `cp` streaming and `>>`
append ever needed. This lifts that: a true **random-access write** at any
offset, zero-filling the gap when the offset is past EOF. The frontier item
the roadmap named next (concrete, unblocked, self-contained - entirely in the
fsd server's `fat32.rs` plus its `FSOP_*` layer, no kernel/scheduler/MMU
risk).

**A finding that shaped the scope: most of "interior writes" already
existed.** `write_at`'s per-sector loop already RMW'd a partial sector
overlapping existing content, so an *interior* overwrite (offset ≤ old_size)
was already coded - it just had **no caller** (`cp`/`>>` are sequential/append
only). And `FSOP_WRITE_AT` (op 13) + the shell's `fs_write_at` wrapper already
existed. The real gaps were exactly two: the `offset > old_size` refusal, and
no user-facing way to invoke an arbitrary-offset write. No new syscall or FSOP
op needed.

**The zero-fill, by generalizing the existing loop.** When `offset > old_size`
the gap `[old_size, offset)` must become **real zero bytes on disk** (FAT32
has no sparse representation, and `extend_chain`'s fresh clusters aren't
zeroed - so without this the gap is garbage, the correctness crux). The
per-sector loop now runs one unified pass over
`[min(old_size, offset), offset + len)`, building each sector from zeros
(positions before `offset`) and data (from `offset`). The boundary sector
straddling `old_size` and the same-sector-gap case both fall out of the
existing RMW branch with no special-casing, and it's byte-identical for the
offset-within-file paths (so `cp`/`>>` don't regress). A `MAX_GAP_FILL`
(1 MiB) cap keeps a fat-fingered offset from zero-filling the volume
(`Error::InvalidOffset`, mapped to `FS_ERR_IO`).

**The consumer:** a `writeat <file> <offset> <text...>` shell builtin - a
random-access write, in place, exercising both the previously-unreachable
interior-overwrite path and the new past-EOF zero-fill. Unlike `write` (full
replace) it leaves the bytes outside the window intact and does not create
the file (it must already exist).

**Confirmed on QEMU (real FAT32), zero `-d int` aborts, with the gap bytes
verified as real `0x00` by hex-inspecting the raw serial log on the host:**
interior overwrite (`AAAXYAAAAA`), append, past-EOF single-sector gap (5
bytes all zero) and **multi-sector gap** (a 1195-byte gap read back all
`0x00` - exercising freshly `extend_chain`'d clusters), the error cases
(nonexistent file, 1 MiB cap), **reboot persistence** (gap zeros + interior
content survive a fresh boot), and FAT16 degradation (shared no-filesystem
message; `help` lists `writeat`). Real-Parallels confirmation pending (the
fat32 path is only reachable there via a mounted USB stick, reads-only by
policy). See `docs/shell-commands.md`.

## Runtime capability delegation - relay-free program-to-program pipes

The IPC send-mask (who-may-call-whom) was a **pure function of task slot**:
a spawned program could reach `{shell, fsd, cond}` and nothing else. So a
program-to-program pipe (`/prog_a | /prog_b`) couldn't stream directly - the
shell relayed every chunk (`producer → shell → consumer`), sitting in the
byte hot path. This adds **runtime delegation**: the shell hands the
producer a capability to send straight to the consumer, taking itself out of
the loop.

**The primitive** (`DELEGATE`, syscall 41): grant one task the runtime right
to send to another - a dynamic addition to the grantee's static send-mask,
consulted by `may_send` after the static check, stored in one per-task slot
(`tasks::DELEGATED_SEND`, the single-slot `grant`/`stdout_target`
precedent), and cleared on task death (both a dying task's delegated-out
capability and any delegation aimed *at* it, so a reused slot can't inherit
a stale one). The rule that makes it safe: **a task may only delegate a
send-capability it *statically* holds** (`may_delegate` checks
`caps_for_slot`, never the delegated slot - no transitive re-delegation).
This self-secures the feature: only the shell statically holds the spawnable
slots' send-caps, so **only the shell can authorize one spawned program to
reach another** - a spawned program cannot, because it doesn't hold that
reach.

**The consumer** (`cmd_pipeline_prog`): the shell spawns the consumer
(stdout → console) and the producer with its stdout aimed straight at the
consumer's slot, `DELEGATE(producer, consumer)`, then only waits for both to
exit - no relay loop. The producer program needs no change: it already
routes output to its stdout target (a raw `msg_send` stream + empty
end-of-stream message when the target isn't the console). The spawn/delegate
race (a tick could let the producer run before the shell delegates) is
closed in the producer, not by pre-arranging slots: `hello`'s send retry now
tolerates `MSG_ERR_DENIED` as well as `MSG_ERR_FULL`, a denied-then-allowed
send being exactly the transient the existing bounded (~3s) retry is for.
The lost auto-kill of a non-reading consumer is the documented tradeoff -
the producer's bounded retry gives up and exits, then the shell's
`WAIT(consumer)` blocks until Ctrl+C, leaving the consumer alive in the
background (`kill` it via `ps`).

Landed in two staged commits (the standing discipline). Stage 1 the inert
kernel primitive - verified byte-identical existing behavior plus a
temporary race-free kernel self-test (reverted) confirming `may_send(4,5)`
went false → true → false across delegate/clear and `may_delegate` was true
for the shell, false for a child. Stage 2 the shell/`hello` wiring.
**Confirmed on QEMU, zero `-d int` aborts:** `HELLO.BIN | UPPER.BIN` streams
both lines uppercased with both tasks reaped (both lines intact proves the
race is closed); the builtin-left pipe (`echo | UPPER.BIN`) and `exec >
file` are unregressed; and the non-reading case (`HELLO.BIN | SH.BIN`)
recovers cleanly on Ctrl+C with the shell responsive afterward. See `docs/architecture.md`. Real-Parallels confirmation of the success path is
pending (it needs the userland binaries on a USB stick + `mount`, per the
standing reads-only policy); the delegation primitive is inert until a pipe
uses it, and a clean boot with it present is already confirmed.

## Userland heap (raw buffer) - and a useful negative result about `alloc`

Programs were fixed-buffer only (16KB stack, no heap, no static state), so
the shell's redirect/pipe capture was a 1024-byte stack array and `cat big
> file` *refused* anything larger. This gives each program a **256KB raw
heap area** in its region (`[code][heap][guard][stack]`), reported by a new
`heap_info` syscall (40), and backs the shell's capture with it - so a large
capture is held whole and written to disk in chunks.

**A go/no-go gate produced a useful negative result first:** a real
`alloc`-backed heap (`Vec`/`String`/`Box`) **can't be built under this
loader on stable.** Prebuilt lib`alloc` carries `R_AARCH64_ABS64`
relocations in its `.rodata` that a `-pie` link rejects (the same
`recompile with -fPIC` wall documented for `slice_error_fail`/`memrchr`),
and the fix - `-Z build-std` to rebuild lib`alloc` with PIE flags - is
nightly-only, off-limits on this stable-only project. One build proved it,
before any real investment. So this is a **raw buffer** a program uses via a
`&mut [u8]`, not a `GlobalAlloc` heap.

`loader.rs` sizes the heap into the region (the guard page is unchanged -
still just below the stack, the heap sits below the guard). The shell's
`Output::Capture` holds a heap-backed slice now, and `finish_redirect`
writes a large capture in `SAFECOPY_MAX` chunks (`write_all`), since one
`fs_write_*` is capped there.

Verified on QEMU, zero `-d int` aborts: **`cat /EFI/ORBS/SH.BIN > /big.bin`
(the full 72KB) captures completely and writes it**, where it used to refuse
at 1024 bytes - proven by round-tripping (`cat /big.bin | UPPER.BIN` shows
the uppercased ELF section names, so the whole file came back). Small `>`,
`>>` append, and the builtin-left pipe all still work; the region grows
~256KB per program (fits the 2MB slot), the idle region gets no heap. Still coarse: a raw buffer, not
dynamic collections; one fixed-size area per program; the shell is the only
consumer so far.

## Stack guard page - and the silent overflow it immediately found

Every userland task ran on a fixed 8KB stack with **no guard**: the layout
was tight (`[code][2 stack pages]`, no gap), so a stack overflow descended
straight into the program's own code - still EL0-accessible - as silent
corruption, no fault. This adds one inaccessible **guard page** immediately
below each stack, so an overflow takes a clean EL0 permission fault that
the existing fault-isolation handler already contains (kills just that
task; halts only for task 0/1).

`loader.rs` grows each region by a page (`[code][1 guard page][stack]`);
`mmu.rs::build_view` derives the guard's address from the region's
`(base, size)` and maps that one L3 page EL1-only - a hole inside the EL0
region, size-gated so the single-page idle region gets none. No ABI
change, no userland change.

**The guard immediately earned its keep by catching a real, pre-existing
bug:** the shell's own `exec` path (`cmd_exec` -> `spawn_path` ->
`fs_call`'s 768+768-byte request/reply buffers + staging + the call chain)
was overflowing its 8KB stack by ~32 bytes into the top of its code region,
every time - silently, because nothing was mapped to fault on it. Fixed by
growing the stack 8KB -> **16KB** (`STACK_PAGES` 2 -> 4, kept in sync
between `loader.rs` and `mmu.rs`).

Verified on QEMU, both directions, zero unexpected aborts: **no false
faults** with the 16KB stack (`selftest`, the previously-faulting `exec`,
streaming `cp` of the 72KB binary, cat-stream+redirect, and the
`hello | upper` program-pipe all run with zero EL0 faults); and a
temp-injected recursion overflow in `hello` (reverted) took a clean EL0
fault in its guard page -> the task killed alone -> the shell survived.
Known limitation (documented): a single >4KB stack frame could skip the
one-page guard - the standard single-guard limitation, still strictly
better than today's silent corruption.

## Program-to-program pipes and `exec … > file`: stdout-over-IPC

Pipes were **builtin-left only** (`builtin | /path/program`, the shell
capturing the builtin's output and streaming it) because a *task's own*
output wasn't capturable - a spawned program's output went straight to the
console. This milestone makes it capturable, delivering both
`programA | programB` (both spawned programs) and `exec prog > file`.

**A scoping finding shaped the design:** the clean pipe has the shell
**relay** (`producer → shell → consumer`), and `producer → shell` is
already permitted by the capability send-mask - so this needed **no
capability delegation** (which the item had been expected to require). The
same "route a program's stdout to the shell" mechanism delivers `exec >
file` too (the shell captures and writes the file).

The mechanism is a per-task **stdout target** (a task index, `CON_TASK` by
default), set at spawn (`SPAWN` gained an arg; new `STDOUT_TARGET` and
`SELF` syscalls, 38/39). A producer program routes output there:
`CON_TASK` → the console server (`DSPOP_WRITE`, unchanged); otherwise a raw
byte stream (chunked data messages + an empty end-of-stream marker) to the
shell. For a pipe the shell relays each chunk on to the consumer (reusing
the existing `pipe_send` + timeout-kill) and forwards EOF; for a redirect
it captures into the buffer `finish_redirect` writes to the file. The
consumer side of a pipe is an ordinary stdin→console filter (`upper`,
unchanged).

Four staged commits (stdout_target plumbing, `hello`'s producer routing,
the `program | program` orchestration, then `exec > file`). Verified on
QEMU, zero `-d int` aborts: `HELLO.BIN | UPPER.BIN` renders both of hello's
lines uppercased through the relay; `exec HELLO.BIN > /h.txt` then `cat`
shows the captured output; the existing `builtin | program` pipe, builtin
`>`/`>>` redirects, and plain `exec` are all unchanged. Still 2-stage (`a | b | c` needs
chaining), producer-`hello`-only for now (any future generator follows the
same pattern), and capture-bounded for the `> file` case (512 bytes, same
cap as existing redirects; pipes stream unbounded).

## A capability model for who-may-call-whom: IPC isolation goes topological

Isolation was MMU-enforced at the *memory* level (per-task page tables,
grant/safecopy) but the IPC *topology* was still flat: any task could
`MSG_SEND`/`MSG_CALL` any task, and the privileged kernel gates were ad-hoc
hardcoded slot checks. This milestone - the roadmap's last big structural
gap vs MINIX - makes isolation topological: a per-slot capability set
enforced at the IPC boundary, so a task can only reach the endpoints its
capabilities allow.

The change is small because **capabilities are a pure function of task
slot** (roles are static: 0 shell, 1 idle, 2 fsd, 3 cond, 4-5 spawnable) -
no stored table, no mutable state, the whole policy in one
`caps_for_slot(slot)`. It packs a **send-mask** (which slots this one may
*initiate* IPC to) plus resource bits (`CAP_BLOCK` for `BLOCK_*`, `CAP_CON`
for the console device). The policy: the shell reaches the two servers and
its children; each program reaches the servers and the shell; `fsd` reaches
`cond` (its logs); `idle`/`cond` initiate nothing. A **reply exemption**
makes request/response work - a server's reply to a caller blocked in a
`MSG_CALL` to it is always allowed (the same "client blocked in a call to
me" condition `SAFECOPY` uses), so only *unsolicited* sends are
mask-checked. The one flow that shaped the policy: `pong`'s echo to a
`send`/`recv` client is an unsolicited server->client send (not a reply),
which is why the child mask includes the shell.

Two staged commits: stage 1 folded the two hardcoded resource gates
(`== FSD_TASK`/`== CON_TASK`) into `cap_has` - a pure refactor, verified
byte-identical; stage 2 added the send-mask and the `may_send` check in the
`MSG_SEND`/`MSG_CALL` arms (returning a new `MSG_ERR_DENIED`). Verified on
QEMU, zero `-d int` aborts: **every existing flow intact** (selftest, the
disk surface over IPC, console output, the pipeline, the pong echo,
exec/ps - no false denials), and **denials fire** (A/B, temp probe
reverted: the shell denied a send to idle, and a spawned `hello` denied a
send outside its `{shell,fsd,cond}` mask - enforcement against untrusted
spawned code - while permitted sends succeed alongside). The supervisor
ping is unaffected (kernel-origin, bypasses the boundary). Static policy only (no
runtime delegation yet - the natural follow-up).

## Active health-ping: catching a server wedged while blocked

The refinement the supervision milestone (below) named as its one gap: the
passive heartbeat catches a server stuck *`Runnable`* (an infinite loop),
but a server stuck *`Blocked`* forever - deadlocked mid-request, waiting on
a reply that never comes - is invisible to it, since a healthy idle server
and a deadlocked one look identical from the outside. The active ping tells
them apart by *poking* the server.

The mechanism needs **no new syscall and no change to any server**, because
it rides the machinery already there. When a supervised server has sat
`Blocked` for a poke interval (~1.3s), `supervisor::poll_ping` (driven by
`on_tick` alongside the passive `heartbeat`) has the caller inject a
`SYSOP_PING` message under a reserved sender sentinel (`KERNEL_SENDER`). A
server idle in its main `msg_recv` is woken by direct delivery and replies
to whatever "sender" the message carried; that reply, addressed back to
`KERNEL_SENDER`, is intercepted by the `MSG_SEND` syscall arm as the ack
(`supervisor::note_ack`). A server stuck mid-sub-call does *not* get woken -
the ping just queues, unseen - so an outstanding ping older than the timeout
(~160ms, far above a healthy ack's tick-or-two) means wedged, and it
restarts on the *same* teardown path the crash and runnable-wedge paths
already use. One ping outstanding at a time, so a server's 4-deep mailbox
can never fill with pings; a `Runnable` server is never pinged (that's the
passive heartbeat's job).

Landed in two staged commits (the project's standing discipline for any
scheduler/SVC-path change): stage 1 the inert plumbing (the `KERNEL_SENDER`/
`SYSOP_PING` ABI constants, the `MSG_SEND` ack interception, the per-`Entry`
ping state + `poll_ping`/`note_ack`), regression-verified byte-identical to
prove the ABI change and the new `MSG_SEND` branch don't perturb the
heavily-exercised `fsd`/`cond` IPC; stage 2 the `on_tick` wiring. Verified
on QEMU, zero `-d int` aborts throughout: the **false-positive gate** (15s
and 10s idle + render-heavy bursts + the full disk surface -> zero spurious
restarts - a healthy server always acks in time); the **ack path live**
(temp instrumentation, reverted: clean inject/ack pairs for both servers);
and a **caught wedge** (temp `fsd` deadlock via an `MSG_CALL` to idle that
never replies, reverted: the ping to `fsd` went unacked -> "wedged -
unresponsive (ping timeout) - restarting" -> "restarted (attempt 1/3)" ->
`fsd` remounted -> disk commands worked again, with `cond` acking normally
throughout and the blocked shell call rescued by `fail_calls_to`). Not
re-run on real Parallels hardware this round - the ping machinery is inert
on a healthy system (a server always acks), the same no-regression posture
the supervision milestone's non-crash paths already have.

## Server supervision + heartbeat: uniform crash recovery, and the first wedge detection

Generalized fault tolerance from a bespoke fsd-only mechanism to a real
supervision registry (MINIX's reincarnation server / Helix's self-heal,
in miniature). New module `kernel/src/supervisor.rs` keeps each
supervised server's boot ELF image and restarts it on either failure
mode: a **crash** (the EL0-fault handler now restarts *any* supervised
slot generically, not just fsd - so the console server `cond` recovers
from a fault too, which it previously couldn't) or a **wedge** (a server
stuck in an infinite loop, which never faults). Wedge detection is a
passive **heartbeat** in `tasks::on_tick`: a healthy server keeps
returning to a `Blocked` state (idle in `msg_recv`, or briefly busy), so
staying continuously `Runnable` for ~2.5s (`WEDGE_TICKS = 128`) is the
wedge signal - no server changes, no new ABI. Both failure modes restart
on the same teardown path (`free_runtime_region` + `revert_input_owner_if`
+ `fail_calls_to` + reload from the kept image + mmu rebuild), with a
shared per-boot restart cap (3) that degrades gracefully past it (a dead
`cond` falls back to the kernel's `PUTC` console; a dead `fsd` degrades
like a missing FSD.BIN).

The fsd-only singletons in `syscall.rs` (`FSD_IMAGE`/`stash_fsd_image`/
`restart_fsd`/`MAX_FSD_RESTARTS`/`FSD_RESTARTS`) were deleted and
replaced by the registry; `loader.rs` registers both servers at boot.
Verified on QEMU by temp fault/wedge injection (reverted after): the
critical false-positive check (idle + bursts never trip a restart), cond
crash+restart, cond wedge+restart, cond restart-cap "giving up" with
graceful `PUTC` fallback, and fsd crash+wedge both recovering - then a
clean full regression (selftest + disk surface, zero aborts, no
supervisor events). The passive heartbeat can't catch a server
*deadlocked while blocked* - an active health ping does, added as the
follow-up milestone above.
**Confirmed on real Parallels hardware too:** with temporary
instrumentation (reverted, never committed), a live `cond` crash recovered
on the framebuffer - the screen cleared to a fresh `cond` banner and the
next `uptime` rendered a real tick count through the restarted server, the
console server recovering on the platform where it's the only console.


## Console server: the console moves out of the kernel (rendering in userland)

The second component moved out of the EL1 kernel (after the filesystem
server): the *steady-state* console. `cond/` (the seventh userland
program) is boot-loaded into a new protected task slot 3
(`syscall_abi::CON_TASK`), exactly like `fsd` in slot 2. Userland text
output no longer goes straight to the kernel - the shell, `hello`,
`pong`, and `upper` now send it to `cond` as batched `DSPOP_WRITE`
messages (`MSG_CALL`), and `cond` forwards it through a new **gated
`CON_WRITE` syscall (33)**, the console analogue of `BLOCK_*` gated to
`FSD_TASK`. A `PUTC` fallback in every client keeps output working if
there's no console server this boot.

Done in two stages. **Stage 1** was the byte-stream backend (the second
protected server, the `DSPOP_*` protocol, userland output as an IPC
stream - the stdout-over-IPC substrate the pipe/redirect items wanted),
zero framebuffer/MMU changes. **Stage 2 moved the framebuffer text-
rendering logic out of the kernel** - the real "rendering driver in
userland." `cond` gained a `Framebuffer` backend (chosen from a new
`CON_INFO` syscall) that owns the cursor, line wrap, scroll decisions,
ANSI parsing, and its **own** copy of the 8x8 font; the kernel keeps only
dumb gated pixel primitives in `fbdev.rs` - `FB_BLIT` (plot glyph
bitmaps), `FB_SCROLL` (`ptr::copy` the screen up), `FB_CLEAR`. Gated
primitives were chosen over mapping the framebuffer into the server's
EL0 view (which would need the per-view device-mapping MMU work that
faulted real Parallels once). A payoff beyond parity: the kernel's old
`fbconsole` never parsed ANSI, so `clear` did nothing on a framebuffer;
`cond`'s parser handles `\x1b[2J`/`\x1b[H`, so `clear` clears. The kernel
keeps a minimal emergency console (fbconsole/UART) for boot and faults.

**A scheduler fix landed alongside it, needed but not originally scoped:
`MSG_CALL` now switches directly to the destination server.** Per-
character echo through IPC exposed that a plain `MSG_CALL` reply waited
up to a full tick - the round-robin picked the always-runnable idle task
before the just-woken server - which dropped burst input outright. The
ABI's `MSG_CALL` doc already promised sub-tick round trips, so
`tasks::block_current_and_switch_to` now takes a `prefer` task and
`MSG_CALL` passes the destination; the server runs, replies (direct-
delivered back), and blocks again before the next tick. Every `fsd` call
got faster too.

**Confirmed on QEMU (`make run` and `make run-image`), zero `-d int`
aborts:** shell banner/`help`/`uptime`/`echo` render through `cond`;
`selftest`'s relocation checks pass (the shell binary changed); `ls`/
`cat` via `fsd` (two servers coexisting); a pipe (`echo ... |
UPPER.BIN`) and `exec HELLO.BIN` route spawned output through `cond`;
`ps` shows all six slots. Stage 2's framebuffer rendering was confirmed
on QEMU `ramfb` by QMP screendump (the pixel-level check): the wrapped
`help` output, `clear` actually clearing, and 32 `help`s scrolling
cleanly. Not yet on real Parallels hardware - the framebuffer backend is
exactly what matters there (Parallels has no UART console), and that
confirmation is on the user, same as every framebuffer milestone.

## FAT32 offset-write (`write_at`): streaming `cp`, unbounded `>>`

The follow-on the grant/safecopy milestone recorded. Every write at the
FAT32 layer was a **full replace** - `write_file` allocated a fresh
cluster chain, freed the old, repointed the entry, and never touched
existing clusters or wrote a partial sector - so `cp`/`>>` were each
bounded by one in-memory buffer and `cp` of a file over `SAFECOPY_MAX`
(2048) refused outright.

`fat32::write_at(path, offset, data)` writes at a byte offset and
**extends** the file without rewriting the bytes before it. The one
genuinely new primitive is a partial-sector **read-modify-write for file
data** (RMW previously existed only for metadata - directory and FAT
entries); a sector overlapping existing content is read, spliced, and
written back, a sector past the old end of file is zero-padded, a full
sector skips the read. It reuses the chain walk, cluster allocation, and
size-field patching that were already there, adds a grow-only size
update, and refuses a write past EOF (no sparse gaps). A new
`FSOP_WRITE_AT` carries the data via grant/safecopy exactly like
`FSOP_WRITE_BULK`.

`cp` streams now - probe the source, truncate the destination, then loop
read-a-chunk/`write_at`-at-the-offset - so it copies a file of **any
size** (bounded by disk space, not a shell buffer). `cp x x` (self-copy)
is refused, because streaming truncates the destination first, which
would destroy the source (the old read-whole-then-write cp was safe by
construction; this isn't). `>>` appends at the file's end via `write_at`
(no read-back), so it works on a target of any existing size - the old
"file too large to append to" refusal is gone.

Verified on QEMU against the real FAT32 image: streaming `cp` of a
5564-byte (non-sector-aligned, multi-chunk) text file is byte-exact
(chunk-boundary markers and all 120 lines correct); `cp` of the 72KB
shell binary persists complete across a reboot (the `.shstrtab` section
name, at the very end of the ELF ~72KB in, reads back - all 36 streamed
chunks landed); `>>` appends correctly and preserves existing content on
both a tiny file (RMW of sector 0) and a 5564-byte file (RMW of the last
partial sector, previously refused); `cp x x` refused; a missing source
leaves the destination untouched; inline `write` unaffected; FAT16
degrades to the shared no-filesystem message. Zero aborts throughout.
**Confirmed on real Parallels hardware** (with the user's go-ahead to
write a scratch file): a streaming `cp` of the 5.7KB `hello.bin` on the
real Lexar stick read back complete, and after a VM reboot a persisted
file was appended to with `>>` - `cat` showing both the original and
appended lines, the partial-sector RMW preserving content on real
silicon; the scratch files were removed afterward.

## Grant/safecopy IPC: enforced capability-based bulk transfer

The second half of MINIX's IPC design, and the fix for the last
user-visible limitation per-task page tables left behind. FSOP v2 made
filesystem requests self-contained (payloads inline, kernel-copied)
because a server can no longer dereference a client pointer - which
capped every operation at what fits one 768-byte message, ~512 bytes.
This milestone moves bulk data directly between two isolated regions
instead, without the message.

Two new syscalls, **enforced, not trust-based** (the user's explicit
call, consistent with the per-task-tables milestone): `grant` (31) - a
client records, in its own single per-task grant slot, that one server
task may bulk-copy an *exact* buffer in the client's own region, in a
given direction; `safecopy` (32) - the server, while handling the
client's `msg_call`, copies within that grant. The kernel authorizes a
copy only when the grant names this server and permits the direction,
the client is *currently* blocked in a call to it (a stale grant is
inert), and both ranges are in bounds. The copy runs at EL1, where all
RAM is identity-mapped read/write in every per-task view - so it reaches
both regions regardless of the active `TTBR0`, while the server can
touch only the designated bytes, only during a call the client
initiated, and never a third task. `safecopy` takes five arguments; the
fifth (direction) is read from the saved trap frame's `x4`, the
4-argument dispatch signature being full.

The FSOP protocol gained `FSOP_READ_BULK`/`FSOP_WRITE_BULK`. `cat` now
**streams a file of any size** (loops the bulk read, one `SAFECOPY_MAX`
= 2048-byte chunk at a time, never holding the whole file - the old
512-byte truncation and its notice are gone); `cp` copies files up to
2048 (was refused past 512); `>`/`>>` redirection captures and appends
past 512 too. Buffer sizes were chosen against the guard-page-less 8KB
userland stack: cp uses a full 2048; the redirect capture/append stay
at 1024 because `cat ... > file` nests the capture, cat's 2048 chunk,
and `fs_call`'s request/reply on one frame. `write`'s content is
bounded by the 128-byte input line, so it stays on the cheap inline
path. Larger transfers still refuse-not-truncate; the ultimate ceiling
is userland memory (no heap), with a FAT32 offset-write primitive and a
userland heap/guard page recorded as the follow-ons.

A real correctness detail, caught by design: a client with a buffer
smaller than `SAFECOPY_MAX` would have the server overrun its grant, so
`FSOP_READ_BULK` carries a `want` parameter bounding the read. Verified
on QEMU against the real FAT32 image: a 5564-byte file streams complete
(chunk-boundary markers all present, all 120 lines); cp of a 1500-byte
file round-trips where it used to refuse, cp of 5564 refuses; a
>512-byte redirect capture and a correctly-ordered `>>` append (not the
historical overwrite bug); reboot persistence; FAT16 graceful
degradation; zero aborts throughout. **Confirmed on real Parallels
hardware end to end**: `selftest` passes (the heavily-modified shell
binary relocates correctly), then `mount` finds the passthrough Lexar
stick, `ls` lists it, and `cat /hello.bin` streams the whole 5784-byte
binary via grant/safecopy - the `ELF` magic, the `.rodata` string, and
the section-name tail (~5KB in) all rendered with no truncation, where
the old `cat` would have stopped at 512 bytes. (A stick present *at
boot* consistently displaced the keyboard in the one-shot xHCI scan -
the known multi-device limitation - so the run used the designed
workflow: boot, then `mount` to rescan for the late-attaching stick.)

## Per-task page tables: MMU-enforced isolation, not trust

The last of the three post-part-2 candidates, and the one that makes
"microkernel" mean something enforceable. Before this, every EL0
region was accessible to every task - isolation was a convention. Now
each scheduler slot runs under its own translation-table view
(`mmu.rs`: per-view L0/L1/L2/L3) with identical kernel/device mappings
but EL0 access to its own region alone; `mmu::activate_task` switches
`TTBR0` and flushes the TLB at every context switch. Touching another
task's memory faults, and the fault-isolation work kills only the
toucher - proven by an A/B probe (a byte-read of the shell's region
from a spawned program succeeded under the old shared map, faulted
under views). The syscall boundary closed its matching gap the same
day: `in_caller_region` validates every `(pointer, length)` against the
calling task's own region, so access can't be laundered through a
kernel copy.

The one thing per-task views broke - the filesystem server
dereferencing pointers into client memory - drove a protocol rework
(chosen with the user over mapping-based grants, which leak neighboring
client memory at page granularity): **FSOP v2**, fully self-contained,
payloads inline in the message (kernel-copied task-to-task), over
768-byte messages (`MSG_MAX_LEN` 64 -> 768). This is the first half of
MINIX's real design; a grant/safecopy primitive can lift the 512-byte
per-op cap later without touching the framing. Per-task ASIDs (a
per-switch-TLBI optimization) were implemented and passed on QEMU but
faulted the idle task on real Parallels hardware - reverted in favor of
the flush-on-switch design, confirmed correct on both platforms;
recorded as a future optimization with the fault evidence. Confirmed on
QEMU (full regression, the isolation A/B proof, fsd crash/restart under
views) and real Parallels (selftest, disk, echo, ps, advancing uptime -
no fault).

## Pipelines: `builtin | program`, data flowing between processes over IPC

The composition layer on top of part-1 IPC and the two-step spawn:
`left | /path/to/program` captures a builtin's output (the redirection
machinery's 512-byte capture, refuse-not-truncate), spawns the program
(`spawn` now returns the new task's slot index - the shell needs it to
stream to, wait on, and kill), streams the capture to it as IPC
messages, and marks end-of-stream with one *empty* message - a
zero-length `msg_send` is legal now, the one ABI-behavior change. The
filter putc's its output as it runs and exits; the shell waits
(Ctrl+C-interruptible), and a program that stops reading its input is
killed after a ~3-second real-tick timeout rather than hanging the
shell on a full mailbox. `upper/` (the sixth userland program, ~80
lines) is the reference filter: uppercases its stream, exits on the
empty message. A real keymap gap found by the scripted real-hardware
smoke: HID keycode 0x31 (backslash/pipe) was missing from
`xhci.rs::keycode_to_ascii` entirely - no physical keyboard could type
`|` on Parallels; fixed, and `test-parallels.sh` gained the
held-Shift `|` chord. Verified on QEMU (echo/ls/cat piped through
UPPER.BIN, every parse error, early-exiting and never-reading right
sides including the timeout kill, FAT16 degradation, full regression,
zero aborts) and on real Parallels hardware (the pipeline typed
through the real keymap, correctly degrading to the no-filesystem
message). v1 limits, documented: one pipe per line, builtin left /
program right, no combining with `>`.

## EL0 fault isolation, and the filesystem server survives its own crashes

The containment payoff of process isolation, plus MINIX-style
supervision in minimal form. Before this, any userland fault - a wild
pointer in any program - took the diverging report-and-halt exception
path and stopped the whole system. Now: a fourth, resumable trampoline
in `exceptions.rs` handles every non-SVC synchronous EL0 exception by
killing just the faulting task (same teardown as `kill`, slot reaped
immediately) and resuming the next runnable one; tasks blocked
mid-`msg_call` to any dying task are woken with a cleanly failed call
on all three death paths (`tasks::fail_calls_to` - previously they
waited forever); and a faulted filesystem server is restarted from its
raw image, kept in a kernel static at boot precisely because the
crashed server *was* the filesystem that would otherwise be needed to
reload it. The fresh server remounts from disk (its state was always
disk-derivable); a 3-restart per-boot cap guards against crash loops,
degrading to the same no-filesystem behavior as a missing FSD.BIN. A
latent gap fixed along the way: runtime-spawned code never got the
dcache-clean/icache-invalidate sequence boot-loaded programs always
had (`tasks::flush_new_code`, now shared by spawn and the restart
path). Verified by direct fault injection on QEMU: a crashing spawned
program killed alone (shell running on, region reclaimed and reused);
fsd crashed mid-call four times - three restarts each followed by a
working remount, with the blocked caller failing cleanly each time,
then the capped give-up; a task-0 fault still halting cleanly; abort
counts in the -d int traces exactly matching the injections. Boot and
typing regression confirmed on real Parallels hardware. Not covered,
deliberately: wedged (non-faulting) servers - no watchdog - and disk
state corrupted mid-write - no journaling.

## Driver isolation part 2: the filesystem moves to userland

The first real component out of the EL1 kernel: `fsd/`, the fifth
userland program, owns the FAT32 engine (the kernel's old `fat32.rs`,
moved essentially verbatim onto a `BLOCK_*`-syscall disk shim) in
protected, boot-loaded task slot 2. The kernel keeps only raw sector
access - `BLOCK_INFO`/`BLOCK_READ`/`BLOCK_WRITE` (26-28), accepted
from the server's slot alone - and syscalls 7-14 (the old `fs_*`
family) are numbering gaps now; their contracts survive unchanged as
the `FSOP_*` request protocol clients speak to the server over IPC.
Enabling work in the same milestone: `MSG_CALL` (29), a synchronous
MINIX-sendrec-shaped call primitive built from direct delivery in
`send_message` (a matching blocked receiver gets the bytes, its saved
`x0`, and a wake immediately - no tick wait) plus a reply-sender
filter on the message wait (an unrelated task's message can no longer
be mistaken for a reply); a two-step staged `spawn` (`SPAWN_STAGE`,
30 - `exec` reads the program via the server in 512-byte chunks, since
the kernel can't read a path anymore); and a server-first two-phase
`mount` with device replacement (preserving first-MOUNTED-wins: an
unmountable boot disk never blocks a later USB stick). Scheduler/MMU
grew to 5 slots/EL0 regions. Two new prebuilt-libcore PIE constraints
found and documented (`str` range slicing, and `rfind`'s `memrchr`).
Confirmed end to end on QEMU (full disk surface over IPC, chunked
exec, reboot persistence, FAT16/missing-FSD.BIN degradation, USB
replace flow, zero aborts throughout) and real Parallels hardware
(mount -> INQUIRY -> server-side FAT32 mount -> ls of the real stick;
reads only by policy).

## Driver isolation part 1: real IPC

The prerequisite the roadmap's driver-isolation item named: fixed-size
(≤64-byte) messages copied through the kernel into bounded per-task
mailboxes (4 pending), `msg_send`/`msg_recv`/`msg_try_recv` (syscalls
23-25), with blocking receive as the third user of the
`WaitReason`/wake-check machinery (Ctrl+C-interruptible, same as
`wait`) and mailboxes cleared on task death so a slot's next occupant
can never receive a predecessor's mail. Proven by `pong/` - the fourth
userland program and first long-lived *server* (the process shape the
part-2 FAT32 filesystem server will have): recv -> echo to sender ->
repeat, `quit` to exit. Confirmed on QEMU (echo round trips,
queue-full at exactly the fifth pending message, the
cleared-mailbox-on-kill proof, blocked-recv Ctrl+C, quit/wait
lifecycle) and real Parallels - where, with the binaries copied onto
the USB stick, the full success path landed a stack of firsts in one
screen: the first runtime program load ever on real Parallels
hardware (`exec /pong.bin` off the mounted stick), a complete IPC
round trip through it, and `exec /hello.bin` + a blocking `wait`
collecting its status.

## USB mass storage - a real disk on real Parallels hardware

The milestone the whole diagnostic arc pointed at: `usb_msd.rs`
(Bulk-Only Transport + SCSI INQUIRY/READ CAPACITY/READ(10)/WRITE(10))
over new bulk endpoints in `xhci.rs`, a `BlockDevice` enum decoupling
`fat32.rs` from virtio-blk, boot-time auto-mount (QEMU) and a `mount`
command with a runtime port rescan (Parallels passthrough attaches
seconds after boot). Two real findings along the way: keyboard events
arriving mid-bulk-transfer must be routed, not dropped (dropping also
kills the keyboard permanently - the interrupt buffer never reposts),
and a freshly attached SCSI device reports Unit Attention (INQUIRY is
exempt; the first READ CAPACITY failed until the standard TEST UNIT
READY/REQUEST SENSE bring-up loop was added - found by the hot-plug
test, not spec-reading). Confirmed on QEMU (auto-mount, reads, writes
with reboot persistence, hot-plug + mount, typing during disk I/O
losing zero keystrokes) and on real Parallels hardware: `mount` on a
real Lexar USB 3.x stick produced its real INQUIRY strings, real
capacity (243,404,800 sectors), a mounted FAT32, and an `ls` of the
stick's actual contents - the first working disk this kernel has ever
had on its primary hardware target.

## `wait()` and reaping - exit statuses become real

The last job-control gap: exit codes went nowhere but a log line.
`exit` now leaves a `Zombie(status)` (code masked to `0..=255`,
POSIX-style) holding its slot until a `wait <n>` collects it - the
collection is the reap; `kill` still reaps immediately (the killer
knows the outcome). `wait` on a live task blocks on the same
machinery as `read_char` (`WaitReason::TaskExit`, the wake-check
generalizing exactly as designed), waking with the status - or
`TASK_KILLED_STATUS` if the target was killed out from under it, or
`WAIT_INTERRUPTED` on Ctrl+C (drained by the wake-check for a waiting
keyboard owner, so one bad `wait` can't brick the session; other
typing during a wait is discarded). `hello/` gained a deliberate
~2-second tick-delay so the blocking path is organically testable.
Honest behavior change: un-waited exited tasks hold their slots -
`ps` shows them, `wait` clears them. Confirmed on QEMU (block → child
exits → status collected, with ticks provably advancing during the
block; zombie-then-collect; slots-exhausted-by-zombies; interrupted
wait; fg/exit revert still firing at death not reap; all error paths)
and real Parallels (error paths, typing unregressed).

## Ctrl+C: the `fg` escape hatch

Job control's documented strand (fg a task that never reads input and
the keyboard was unreachable until it exited) is closed: Ctrl+C typed
while a non-boot-shell task owns the keyboard is intercepted at the
single keyboard-poll choke point, reverting ownership to task 0 and
swallowing the byte - reclamation, not a signal; the foregrounded task
keeps running in the background. Supporting pieces: the xHCI keymap
gained the Ctrl modifier (Ctrl+A..Z -> C0 control bytes; it only
handled Shift before), the shell's line editor now ignores all
unhandled control bytes, and test-parallels.sh gained a CTRL-C chord
pseudo-command. Confirmed on QEMU (fg -> strand -> Ctrl+C -> keyboard
back, nested task still alive) and real Parallels (the chord produced
a genuine 0x03 through the real xHCI path - no stray 'c' - and the
shell ignored it cleanly).

## Job control: `kill <n>` and `fg <n>`

The task-lifecycle arc completes: `kill` (syscall 19) destroys another
task (exit's teardown minus the context switch - a non-current task
isn't executing, its saved context is simply discarded), and `fg` (20)
hands the keyboard to a spawned task - the input owner, hardcoded to
task 0 since the keyboard-routing fix, is runtime state now, with an
automatic revert to task 0 whenever the owner dies (task 0 itself can
never die, so the revert target is always valid). `exec
/EFI/ORBS/SH.BIN` + `fg 2` is a real nested interactive shell session;
its `exit` hands the terminal back. Confirmed on QEMU (separate cwd
state proving input genuinely moved, roles visibly flipped in `ps`,
revert on exit, kill of a background task, all error paths) and real
Parallels (the reachable error-path subset). Documented deliberate
limitation: no interrupt key exists, so `fg` to a non-interactive task
strands the keyboard until it exits - `fg` is for interactive
programs; `kill` is the escape hatch for everything else.

## `ps`, and shell buffers at the syscall cap

Two more small same-day items: a `ps` builtin backed by a new
read-only `task_state` syscall (18) - one line per scheduler slot
(`unused`/`runnable`/`blocked`), the first way userland can see the
spawn/exit lifecycle it now controls (a spawned shell visibly sits
"blocked (waiting for input)"; an exited task's slot visibly returns
to "unused"), with the slot count discovered by probing rather than
leaking into the ABI as a constant. And the shell's `cat`/`ls`/`cp`
buffers doubled from 256 bytes to 512 - the kernel's per-syscall cap
(`MAX_USER_LEN`) and therefore the ceiling, not a choice - confirmed
by cat'ing and cp'ing a 408-byte file untruncated that the old
buffers would have cut off or refused.

## `mv` into directories, and `SPAWN_ERROR` split

Two small follow-ups the same day: `mv <src> <dir>` now moves *into* an
existing directory keeping the basename (shell-side, probing the
destination with the same `fs_list_dir` trick `cd` uses; the trivial
self-move `mv x x` is refused, guarding the degenerate
move-into-itself case). And `spawn`'s collapsed sentinel is split like
`FS_ERROR` was: read failures return the ordinary `FS_ERR_*` code,
plus `SPAWN_ERR_BAD_ELF`/`SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_NO_FREE_SLOT`
- and a failed spawn now returns its allocated region to the runtime
allocator (always the LIFO case). Confirmed on QEMU with organic
triggers for every new path, zero aborts.

## The collapsed `FS_ERROR` sentinel split into specific error codes

The gap flagged since phase 4: every `fs_*` failure collapsed to one
sentinel, so the shell printed guess-lists. `syscall-abi` now reserves
a top band of named codes (`FS_ERR_NOT_FOUND` through `FS_ERR_IO`,
with `FS_ERR_MIN` as the is-any-error floor since read/list return
arbitrary byte counts on success); the kernel maps `fat32::Error`
variants via one `fs_error_code` function; the shell prints one
accurate reason per failure via a shared `print_fs_error`. Backward
compatible - `FS_ERROR`/`NO_FS` keep their values. The specific
messages immediately surfaced a real mis-mapping the collapse had
hidden (`rmdir /` reported "invalid name" - `split_parent` failed
before the root check ran; fixed at the source), and the `>>` append
path now only treats "not found" as create-the-file instead of every
read failure. Confirmed on QEMU with one organic trigger per message,
full happy-path regression, FAT16 degradation unchanged, and a real
Parallels smoke. `SPAWN_ERROR` stays collapsed, deliberately.

## Task destruction (`exit`) - and `hello/`, the second real userland program

The `EXIT` syscall (17) destroys the calling task: slot freed for a
future `spawn`, EL0 mapping dropped (the same masked-IRQ
`mmu::rebuild_with_el0_regions` spawn proved safe), RAM returned to the
runtime bump allocator when LIFO order allows (most-recent allocation
only - anything else leaks, deliberately). Tasks 0 (boot shell, sole
keyboard owner) and 1 (idle) are refused with `EXIT_DENIED`.
`tasks::exit_current_and_switch` mirrors `block_current_and_switch`'s
proven frame-overwrite shape, discarding the context instead of saving
it. The test vehicle is a real deliverable: `hello/`, a ~70-line
second userland program (banner + exit) proving both the exit path and
"the shell is just a program" - zero new toolchain work, the
`aarch64-unknown-none` PIE flags and shared linker script are
workspace-wide. Confirmed on QEMU with the reclaim directly observable
(three exec/exit cycles landing at the same region base, a long-lived
spawned shell interleaved with slot-3 cycles) and on real Parallels
hardware (the typed `exit` refusal - the one reachable path there).


## Directory extension - full directories grow by a cluster instead of failing

`fat32.rs::insert_dir_entry` (the single choke point every
entry-creating operation goes through - `mkdir`/`touch`/`write`/`cp`/
`mv`) now extends a directory's cluster chain when every existing slot
is taken, claim-then-zero-then-link ordering so a partial failure never
corrupts the chain; `Error::DirectoryFull` is deleted (unconstructable).
The real correctness piece: `rmdir` freed exactly one cluster - correct
only while directories were single-cluster by construction - and would
have silently leaked extension clusters; fixed with a shared
`free_chain` helper that also deduplicated `rm`'s and `write_file`'s
identical existing loops. Confirmed organically on QEMU (the test
image's 512-byte clusters fill after 14 entries): 20 files in one
subdirectory, root-directory extension, content round-trip on an
extended-cluster entry, reboot persistence, `rmdir` of the two-cluster
directory, and freed-cluster reuse - zero aborts.

## ESP directory renamed to `\EFI\ORBS\`, and a project logo

The ESP directory became `\EFI\ORBS\` (was `\EFI\OUROBORO\` - both
exist because the full 9-character project name exceeds FAT's 8.3
short-name limit and `fat32.rs` doesn't parse LFN entries; `ORBS` is
the tidier abbreviation). `loader.rs`'s `CONFIG_PATH`, the Makefile's
`esp` target, and current-state docs updated together; confirmed
booting on QEMU (including a real `exec /EFI/ORBS/SH.BIN`) and real
Parallels hardware. The project also gained a logo (`logo1.png`
source at the repo root; resized copies on the website and README,
plus a real favicon replacing the per-page emoji ones).

## xHCI multi-device support - every connected device enumerated, classified, and kept addressed

The keyboard driver's one-port/one-device/one-slot scope is lifted -
the named prerequisite for the USB mass-storage milestone. The
all-in-one `Device` struct split into controller-global state
(`Xhci`), per-device slots with their own EP0 rings and Output Device
Contexts from 4-entry pools (the two statics that were only safe
while the old scan abandoned every non-keyboard), and `KeyboardState`.
The scan enumerates every connected port, logs each device's
interface class/subclass/protocol (with a mass-storage class-`0x08`
callout), keeps non-keyboards addressed, activates the keyboard only
after the scan completes, and routes transfer events by slot ID +
endpoint DCI. Confirmed on a new QEMU three-device rig
(`make run-usb-multi`: keyboard + tablet + storage - the storage
stick classified exactly `0x08`/`0x06`/`0x50`, Bulk-Only Transport)
and on real Parallels hardware (virtual mouse + keyboard concurrently
addressed, typing clean).

## Parallels disk diagnostic - no documented storage controller exists on this platform

A diagnostic round (no driver written - that was the point) settled
the Parallels disk question: with the VM's `sata:0` boot disk attached
and bootable, the PCI bus shows no storage controller of any kind, and
scratch disks deliberately attached as `scsi`/`lsi-sas` then
`lsi-spi` are equally invisible (`buslogic` is rejected by Parallels
itself as EFI-incompatible; `ide`/`scsi` are the only interfaces
prlctl offers ARM64 EFI VMs). All storage flows through a proprietary
non-PCI path - "implement a documented spec" is not available for any
attached-image disk there. The one documented lead left: USB mass
storage over the existing xHCI driver (a first check with a USB 2.0
stick found Parallels routes USB 2.0 passthrough to the EHCI
controller instead; a USB 3.x stick is the pending retry). A permanent
diagnostic improvement fell out: `pci::log_all_devices` returns its
inventory and `main.rs` re-prints it through the post-exit console,
since boot reaches the shell ~2 seconds after power-on - far too fast
to read the UEFI-console rendering. See the roadmap's "Disk on real Parallels hardware" entry.

## Output redirection (`>`/`>>`) - pure shell-side, zero kernel changes

`cmd > file` (create/overwrite) and `cmd >> file` (append) work for
every builtin, composed entirely from the existing `fs_read_file`/
`fs_write_file` syscalls the same way `cp` was - no new syscall, no
kernel changes. `run_line` (`shell/src/main.rs`) peels a trailing
redirect off the line before dispatch; command output flows through an
explicit `Output` sink passed down to handlers (error messages
deliberately stay on the console - the POSIX stdout/stderr split);
`>>` is shell-side read-concatenate-rewrite, bounded by the kernel's
512-byte per-syscall buffer cap. That cap found a real bug in testing:
a 1024-byte append buffer failed `valid_user_range` in a way
indistinguishable from "no such file", silently turning append into
overwrite - fixed by sizing the buffer at the cap. Confirmed on QEMU
(overwrite/append/create-empty, reboot persistence, both overflow
refusals - one organically, via six consecutive appends - and every
error case, zero aborts) and on real Parallels hardware (the `NO_FS`
path; typing `>` there needed a real `test-parallels.sh` extension - a
held-Shift scancode chord via `prlctl`'s `--event press/release`).
See `shell-commands.md`'s user-facing reference.

## Keyboard input routing: one designated owner task, not first-blocked-wins

Found live by the user immediately after `exec` shipped: with two
concurrent shells, keystrokes split unpredictably between them
(`uptime` arriving as `ptime` + a stray `u`), because `on_tick`'s
wake-check polled every `Blocked(Keyboard)` task in index order and
the underlying poll destructively consumes a byte for whoever asks
first. Fixed with `INPUT_OWNER_TASK` (hardcoded to task 0, the
boot-loaded shell - always valid since no task destruction exists):
the wake-check now skips keyboard polling for every other task, which
simply stays blocked, a genuine background task rather than a second
terminal racing the first. Confirmed by re-running the exact live QEMU
scenario that exposed it, plus a real-Parallels regression pass (this
wake-check is the only keyboard path there).

## Dynamic task creation and `exec` - a real `spawn` syscall

A new `spawn` syscall (16) loads a program from disk at runtime and
starts it as an independent task alongside the caller (deliberately
spawn semantics, not POSIX exec-replaces-current-process, though the
shell command is named `exec`). Needed: a runtime physical-page bump
allocator (nothing could hand out RAM after boot services exited), a
re-callable `mmu::install_identity_map` (stashed memory map +
extra-device list, `rebuild_with_el0_regions`), the scheduler grown
from 2 fixed slots to 4 (`TaskState::Unused`), and `loader.rs`'s ELF
core split (`elf_region_size`/`populate_region`) so the same parsing
serves boot-time and runtime loads. A real bug found by testing:
`Vec`-based program-header parsing *hangs silently* when first reached
from the runtime path - the global allocator is boot-services-backed
and invalid after exit, and misuse doesn't fault, it just hangs -
fixed with a fixed-capacity array. Confirmed on QEMU end to end (a
second shell instance genuinely running concurrently); on real
Parallels hardware only the error path is reachable (no disk driver
exists there at all - pre-existing gap), confirmed clean. See `architecture.md`'s process-model section.

## Blocking primitives - tasks can really wait, and a second SVC-frame bug

`READ_CHAR` (15) is the first genuinely blocking syscall: instead of
the shell busy-polling `try_read_char` every scheduler slice, the task
is suspended (`TaskState::Blocked(WaitReason::Keyboard)`) and
`on_tick`'s wake-check resumes it with the byte already in `x0` once
one arrives. Deliberately never executes `wfe` anywhere (real
Parallels hardware has a confirmed unresolved hang for EL0 `wfe`).
Testing this immediately exposed a second real, pre-existing
`exceptions.rs` bug (after the relocating-loader milestone's `x9`
clobber): the SVC trampoline's saved frame didn't match `Context`'s
real field layout and never saved `SP_EL0` at all - harmless while
every syscall resumed its own caller, fatal the moment a blocking
syscall loaded a *different* task's context through it. Fixed to match
the IRQ trampoline's proven layout. Confirmed on QEMU (a real
4-second block with zero wake events, then instant wake on input) and
on real Parallels hardware.
section.

## A real relocating loader - and a pre-existing SVC-trampoline bug it surfaced along the way

**The goal:** replace the flat, position-*dependent* userland-program
loader with a real ELF64 loader that parses `PT_LOAD` segments and
processes `R_AARCH64_RELATIVE` self-relocations against wherever a
program actually loads - the documented fix (`roadmap.md`) for this
project's single most-repeated bug class: `core::fmt`'s argument-
dispatch table and slice/literal comparisons both crashing for the
identical reason (an absolute data pointer baked in for a link-time
base of `0x0` that never matches the real runtime load address).

**Delivered:** `kernel/src/loader.rs` now hand-rolls a real ELF64
parser (header, program headers, section headers, `Elf64_Rela`
entries) and applies every `R_AARCH64_RELATIVE` relocation it finds in
`.rela.dyn` against the real load address; `LoadedProgram` gained a
real `entry` field, used by `tasks.rs` instead of assuming it equals
`base`. `shell`'s toolchain switched to `relocation-model=pic` + `-pie`
+ `--no-dynamic-linker` (`.cargo/config.toml`), `shell/linker.ld` gained
`.rela.dyn`/`.dynsym`/`.dynamic`/`.data.rel.ro` output sections, and the
Makefile's `shell-bin` target now hardcodes `--release` for userland
program builds - a real, confirmed toolchain constraint, not a style
choice (a debug build fails to *link* at all, due to an
`R_AARCH64_ABS64` relocation inside prebuilt `libcore`'s own object
code). The shell gained a permanent `selftest` builtin proving both
previously-crashing patterns - `write!`/`core::fmt` and a slice-vs-
literal comparison - now work correctly.

**A second, genuinely important finding along the way:** a real,
pre-existing bug in `exceptions.rs`'s SVC trampoline, latent since the
syscall boundary was first built and never triggered before this
milestone's different register allocation happened to expose it - the
EC check at the top of the SVC vector slot clobbers `x9` *before* the
trampoline's own save sequence gets a chance to preserve it, silently
discarding whatever value userland had live in `x9` at the moment of
`svc`. Root-caused via a direct register-survival probe (raw inline
`asm!`, not guessed) and fixed by saving `x9` to a scratch stack slot
before the EC check runs.

**Confirmed working end to end** via the same piped-stdin QEMU
technique as every prior milestone, against both `make run`'s FAT16
vvfat and the real FAT32 `esp.img`: `selftest`'s three checks all pass;
`help`/`echo`/`uptime` (a genuinely multi-digit tick count, the exact
previously-crashing shape) all produce correct output; the full disk-
command surface round-trips correctly. Zero aborts in `-d int`
cross-checks. **Confirmed on real Parallels hardware too**, via `make
test-parallels`: `selftest`'s three checks all passed identically, and
`uptime` printed a genuine 4-digit tick count with no crash - direct
real-hardware confirmation of the `x9` fix specifically.

## xHCI busy-waits switched from iteration-bounded to time-bounded - a real bug the user found, not this project's own testing

**The goal:** none going in - this was a live bug report. Booting the
real `esp.hdd` normally in Parallels (a manually-launched, live VM
window - not `make test-parallels`, which drives Parallels' own
synthetic keyboard headlessly) produced `xhci: keyboard not available
(Command ring: timed out waiting for a completion event)`, something
none of this project's own real-hardware testing had ever hit.

**Delivered:** every busy-wait in `kernel/src/xhci.rs`
(`wait_command_completion`, `wait_transfer_event`, the port-scan loop,
`poll_until`) switched from a fixed iteration count (`POLL_ITERS`) to a
genuine wall-clock deadline, using the ARM generic timer's free-running
counter (`CNTPCT_EL0`/`CNTFRQ_EL0` - pure system-register reads,
needing no GIC or interrupts, the same property `timer.rs` already
relied on for its own timer setup). `timer.rs` gained a new
`pub(crate) fn now_ticks()` for this reuse.

**Root cause:** a fixed iteration count is only a valid stand-in for
real elapsed time if the host never stalls the guest's vCPU for any
real duration while it spins - untrue under a real hypervisor. The
strongest evidence: reproducing the bug a second time showed a
*different* xHCI command timing out than the first attempt (very early
port setup, then much later at what's almost certainly the interrupt
endpoint's `Configure Endpoint` command) - a pattern that indicts the
wait mechanism itself, not any one command's logic. The most likely
trigger: a live-rendered VM window competing for real host CPU/GPU
time, something none of this project's own headless scripted testing
ever does.

**Confirmed fixed by the user directly**, on the exact real-world
scenario that originally failed - not a scripted reproduction. Also
regression-tested on QEMU and via this project's own `make
test-parallels`, neither of which had ever reproduced the original bug
but both confirm no regression to the already-working path.

## MADT/GICv3: real interrupt-controller discovery for Parallels - full preemptive multitasking confirmed working end to end

**The goal:** replace `gic.rs`'s old QEMU-devicetree-derived GICv2
addresses (already confirmed unsafe on real Parallels hardware) with real ACPI MADT discovery, and add a
GICv3 driver so Parallels - which almost certainly runs GICv3, not
GICv2 - can actually reach it.

**Delivered:** new `kernel/src/madt.rs` (MADT parsing: GICD/GICC/GICR
structures, cross-checked against Linux's `actbl2.h`), `kernel/src/gicv3.rs`
(a real GICv3 backend - system-register CPU interface, per-CPU
redistributor discovery and wake-up), and `kernel/src/gic.rs` turned
into a version-dispatch facade over `gicv2.rs`/`gicv3.rs` so `main.rs`/
`exceptions.rs`'s call sites barely changed. Confirmed on QEMU two
independent ways (a devicetree dump and the real MADT parse agreeing
exactly, both for default GICv2 and a newly forced `-machine
virt,gic-version=3`, `make run-gicv3`) before ever risking real
hardware. Two real GICv3 bugs found and fixed on QEMU first - a PPI
defaulting to Group 0 (FIQ) instead of Group 1 (IRQ) without an
explicit `GICR_IGROUPR0` write, and `GICD_CTLR` needing more than
GICv2's single enable bit - both cross-checked against Linux's own
`gic_cpu_init`/`gic_dist_init`.

**On real Parallels hardware:** MADT discovery confirmed clean and safe
(`GIC V3, GICD @ 0x2410000, GICC/GICR @ 0x2500000`, genuinely different
addresses from QEMU's - resolving whether Parallels' MADT describes an
interrupt controller at all, previously an open question given its
absent SPCR). GIC/timer IRQ delivery itself is conclusively confirmed
working there too - a real, correctly-incrementing `uptime`
(`533` -> `752` ticks in one observed run), isolated via a
single-variable diagnostic (temporarily skipping just the task-switch
call while leaving GIC/timer otherwise fully active).

**A second, separate, real bug found in the process - root-caused and
fixed the same session.** The actual task switch (`tasks::on_tick`)
hung the system outright the first time it ran on real hardware - this
exact interrupt-delivery-plus-context-swap combination had never
executed on real hardware before (Parallels had no working GIC/timer at
all until this milestone). A single-variable diagnostic isolated it to
the task switch specifically, not GIC/timer delivery (confirmed solid
on its own). Leading suspect: task 1's idle loop, `wfe` - real
hardware's `wfe` may be trapped/emulated by the host hypervisor in a
way QEMU/TCG's never is. Swapping the idle loop for a plain busy-spin
and re-testing confirmed it: the hang was completely gone, verified by
a sustained interactive test with a correctly, continuously
incrementing tick count throughout. Task switching is now
unconditionally enabled on every platform - preemptive multitasking
works on real Parallels hardware for the first time ever, with zero
change to QEMU's behavior (retested end to end there too, both GIC
versions, no regressions). A real, secondary, minor finding along the
way - also root-caused and fixed the same day: an occasional dropped
keystroke under active task switching, traced to a genuine logic bug in
`xhci.rs::Device::poll_key` (not a hypervisor timing quirk) - a single
polled report can legitimately carry more than one newly-pressed
keycode at once, and the original code only ever translated and
returned the first one, silently discarding any second forever. Fixed
with a small `pending` buffer draining every qualifying keycode from a
report. Confirmed fixed on real Parallels hardware: ten consecutive
`uptime` invocations back to back, zero drops.

**Made practical by a new discovery the same day:** Parallels Desktop's
own CLI, `prlctl`, can script an entire real-hardware test round trip
(boot, type via `send-key-event`, screenshot via `capture`) with no
human watching the VM live - now `make test-parallels`
(`scripts/test-parallels.sh`). Every real-hardware round trip in this
milestone went through it. See `roadmap.md`'s "Testing infrastructure"
section.

## USB HID keyboard driver: confirmed - real, physical keyboard input on real Parallels hardware, first time ever

**The goal:** a real input path for Parallels, closing the gap the GOP
framebuffer console milestone left open (write-only, no keyboard driver
at all). A from-scratch xHCI driver (`kernel/src/xhci.rs`): capability/
operational register bring-up, command ring, event ring, device slot
enable/address over control transfers, and - the mechanism that turned
out to actually be required - a real interrupt IN transfer ring.

**Five independently-confirmed real-hardware bugs, none visible on
QEMU**, each found by direct evidence rather than guessing:

1. A PCI Command register bit-position error - `CMD_MEMORY_SPACE` was
   `1 << 0` (I/O Space Enable) instead of `1 << 1` (Memory Space Enable),
   found by decoding an observed before/after register dump
   (`0x0010 -> 0x0015`) that showed I/O Space + Bus Master set, never
   Memory Space. Explained every earlier "nothing responds" symptom on
   both QEMU and real hardware at once.
2. A genuine firmware panic on real hardware -
   `PANIC@11.28 UEFI-exception-ArmPciCpuIo2Dxe.dll`, decoded from
   Parallels' own hypervisor crash log - from a PCI config-space
   BAR-reassignment write (a standard technique, needed to work around
   QEMU's own OVMF build leaving this device's BAR completely
   unassigned) that real PCIe firmware doesn't tolerate the way QEMU's
   software model does. Fixed by never writing to PCI config space
   beyond the one narrow Command-register enable.
3. The discovered BAR (`0x8000004000` on QEMU) landed outside the
   identity map's original single-L0-table-entry span (a real
   simplifying assumption from early in this project, never revisited
   until a real PCI BAR needed an address outside the first 512GB).
   Fixed by generalizing `mmu.rs` to allocate further top-level table
   entries on demand.
4. The deepest finding: Parallels' USB passthrough doesn't forward HID
   *class* requests (`SET_PROTOCOL`, `GET_REPORT`) to the real device at
   all - confirmed by a live, correct `GET_DESCRIPTOR` *standard*
   request returning Parallels' own real registered USB vendor ID
   (`0x203a`) right next to a `GET_REPORT` that kept echoing this
   driver's own Setup packet back (byte-for-byte, tracked exactly across
   a changed request length - not a coincidence). Fixed by switching to
   a real interrupt endpoint, armed via the standard `Configure Endpoint`
   xHCI *command* (not a class request), the same mechanism every
   production USB HID driver actually uses at runtime.
5. Once the interrupt endpoint was delivering real live data, it turned
   out to be reading Parallels' virtual *mouse*, not the keyboard - this
   driver's port scan had just grabbed the first connected device.
   Fixed by scanning every connected port and checking each device's
   actual HID interface protocol (`bInterfaceProtocol=1`, Keyboard)
   before configuring it.

**Confirmed working end to end on real Parallels hardware, not just
QEMU:** typed a full command line (`abc`), used backspace, pressed
Enter, got the shell's real `unknown command` response - the complete
keyboard-to-shell round trip, on a real physical USB keyboard.

Full technical write-up - including the debugging techniques that found
each bug (poisoned DMA buffers, widening a request to break a suspicious
size coincidence, using a known-good standard request as a control,
decoding raw exception registers by hand) - in
[`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md), written to
be useful to other bare-metal-OS developers hitting the same class of
problem, not just as this project's own history.

**Still coarse, worth knowing before building on this:** one port, one
device, one slot, no hot-plug, no hubs, no real HID report-descriptor
parsing (boot-protocol's fixed 8-byte layout assumed directly, and
`SET_PROTOCOL` almost certainly still isn't reaching the device either -
this only works because the real keyboard happens to use the same
simple layout regardless), no stall recovery on the interrupt endpoint
specifically (EP0's setup-time control transfers do recover from a
Stall), only the first matching interrupt IN endpoint is ever
configured. Preemptive multitasking is still unavailable on Parallels
(a separate, already-tracked gap - see `roadmap.md`).

## GOP framebuffer console: confirmed - a real, working shell prompt on real Parallels hardware, first time ever

**The goal:** with the broadened `qemu_device_region_safe` gate in
place, the user tested a fifth time. Success - the complete predicted
sequence reached on real hardware with no further issues:
`framebuffer console live` → `skipping virtio-blk` → `skipping
GIC/timer init` → `shell ready` → the userland shell's own banner and a
live `$` prompt.

This is the first time this project has ever reached a running,
prompt-displaying shell on real Parallels hardware - the actual goal
the whole console-discovery effort (devicetree/ACPI/PCI, then
virtio-console, then five rounds of GOP-framebuffer-console hardware
testing) was for. Confirmed working on real hardware, not just QEMU:
GOP discovery/mapping, direct-write display visibility with no flush
needed, `fbconsole.rs`'s actual text rendering, the exception handler
reporting through it, the MMU identity map, and EL0 task entry.

**What's not working: keyboard input, a separate, already-known gap.**
The framebuffer console is write-only by design - this kernel has no
keyboard driver at all. The shell is genuinely running, but nothing
typed reaches it yet. A real input path needs a keyboard driver (USB
HID over UEFI, most likely - Parallels' own PCI inventory already shows
real USB controllers present). Preemptive multitasking also isn't
available on Parallels yet (the safety gate disables GIC/timer setup
there), needing real interrupt-controller discovery (ACPI MADT, likely
GICv3) to fix - a separate, substantial follow-up.

## GOP framebuffer console, round four: the GIC crashes too - broadened the safety gate beyond virtio-mmio

**The goal:** with the `virtio_mmio_probe_safe` gate in place, the user
tested a fourth time. `skipping virtio-blk` printed cleanly - that fix
worked - but the boot halted again with a second exception.

- Decoded the same way as the previous crash: `esr_el1=0x96000050`
  gives EC `0x25` (Data Abort, same EL) and DFSC `0x10` (Synchronous
  External Abort - a real bus fault), same signature as before, but
  `far_el1=0x8000000` this time - exactly `gic.rs`'s `GICD_BASE`,
  specifically `gic::init()`'s very first write.
- **A structural finding, not a second instance of the same bug:**
  `gic.rs`'s addresses are the identical kind of QEMU-shaped convention
  as `virtio_mmio.rs`'s - confirmed only via a QEMU devicetree dump,
  never discovered on Parallels. Nothing in this project's fixed
  low-1GB device-region convention has ever been confirmed safe on
  Parallels, only on QEMU.
- One real exception: `timer.rs` is pure system-register access
  (`cntfrq_el0` etc.), architecturally safe on any ARMv8 CPU regardless
  of platform device layout - only the GIC, needed to forward the
  timer's interrupt, depends on the unconfirmed address.
- **Fix:** renamed the gate `qemu_device_region_safe` (from
  `virtio_mmio_probe_safe`) and broadened it to also cover
  `gic::init()`/`gic::enable_interrupt()`, skipped together with
  `timer::arm()` as one block when unsafe. Verified `tasks::start()`
  has no dependency on GIC/timer having run (a straight `eret` into
  task 0's saved context) before shipping this, not assumed - skipping
  this block still reaches a working interactive shell, just without
  preemption (`uptime` would report a static count in this mode, a
  real, minor, documented limitation).
- Re-verified on QEMU, now checking ticks genuinely increase (88 → 214
  a few seconds later) to confirm the normal path still works, not just
  "didn't crash." A forced `discovery=None` + `ramfb` test (the actual
  Parallels shape) showed the complete intended sequence end to end:
  framebuffer console live → clean virtio-blk skip → clean GIC/timer
  skip → shell ready → a real working userland shell prompt. Zero
  aborts. Not yet re-confirmed on real Parallels hardware.

## GOP framebuffer console, round three: it renders real text on Parallels, and directly confirmed the virtio_mmio crash

**The goal:** with the console-fallback reorder in place, the user
tested a third time. Real progress - genuinely readable kernel text
rendered live on real Parallels hardware after `exit_boot_services`, a
first for this project - but the boot still halted, this time with an
exception report as the last thing printed.

- The rendered exception (`EXCEPTION vector=4 esr_el1=0x96000010
  far_el1=0xa000000 elr_el1=0xbba94c68`) decodes to EC `0x25` (Data
  Abort, same EL) with DFSC `0x10` (Synchronous External abort - a real
  bus fault) at `FAR_EL1 = virtio_mmio::SLOT_BASE` exactly - direct,
  decoded proof (not just a strong suspicion) that the virtio-mmio
  magic-value scan crashes real Parallels hardware on its very first
  read.
- The freeze this time was total, not just a lost console: `init_storage()`
  runs unconditionally after the console fallback chain, and
  `virtio_blk::Device::discover()` goes through the identical
  `virtio_mmio::find_device` scan - so the console reorder alone wasn't
  enough, the disk driver's own use of the same unsafe scan was always
  going to hit the same wall moments later.
- **Fix:** a new `virtio_mmio_probe_safe` flag (`true` only when a
  byte-stream console was found via devicetree/ACPI/PCI - the one
  platform this scan has ever been confirmed safe on) now gates *every*
  caller of `virtio_mmio::find_device`, both `try_virtio_console` and
  virtio-blk discovery. `virtio_mmio.rs`'s doc comments were corrected -
  the old claim that unbacked reads "read as 0" on real hardware was
  never confirmed, and is now confirmed wrong.
- Re-verified on QEMU across three scenarios: the normal ACPI-console
  regression pass (virtio-blk still initializes normally, confirmed
  against both FAT16 vvfat and a real FAT32 image with working `ls`/
  `cat`), and a forced `discovery=None` + `ramfb` test simulating the
  actual Parallels shape end to end - framebuffer console live, a clean
  "skipping virtio-blk" message, and a fully working interactive shell
  prompt. Zero aborts across all three. Not yet re-confirmed on real
  Parallels hardware.

## GOP framebuffer console, round two: display-write visibility confirmed, virtio-console MMIO scan reordered out of the way

**The goal:** with the `open_protocol_exclusive` fix in place, the user
tested again on real Parallels hardware. Progress - GOP discovery now
succeeds (`GOP framebuffer @ 0x20000000, 1024x768, stride=1024,
format=Bgr`) and boot reaches further than before - but the screen
still froze, with no framebuffer console output ever appearing.

- **A temporary raw full-framebuffer fill diagnostic** (`0xff` to every
  byte, placed right after the MMU switch, bypassing `fbconsole.rs`/
  `font.rs` entirely) isolated the question that mattered most: does a
  direct write to this physical address actually reach the display at
  all, given that a real GPU might need an explicit flush/present step
  unlike QEMU's `ramfb`. Verified visible on `ramfb` first (solid white
  via QMP screendump), then handed to the user for real hardware.
- **Result: solid white screen on real Parallels hardware** - confirming
  the MMU mapping and direct-write display visibility both work, no
  flush needed. But the screen stayed solid white, frozen - meaning
  execution never reached the actual framebuffer-console rendering.
- **Root cause: `try_virtio_console()` ran immediately afterward, and
  its MMIO scan carries an unconfirmed assumption** -
  `virtio_mmio.rs`'s `find_device` reads a magic-value register at 32
  fixed QEMU-specific addresses, with a comment asserting (never
  actually confirmed) that an unpopulated slot "reads as 0" on real
  hardware. A real bus fabric commonly raises an external abort for a
  read to genuinely unbacked device-memory space instead. With no
  console installed yet at that point on Parallels, a fault there is
  completely silent.
- **Fix: reordered the fallback chain** so the now-hardware-validated
  framebuffer console is tried before `try_virtio_console`, not after -
  justified by independent evidence (`pci.rs`'s device inventory) that
  virtio-console doesn't exist on Parallels at all, so demoting it costs
  nothing there. Re-verified on QEMU: the forced-fallback `ramfb` test
  and the normal ACPI-console regression pass are both unchanged. Zero
  aborts. Not yet re-confirmed on real Parallels hardware.

## GOP framebuffer console fix: `open_protocol_exclusive` was disconnecting Parallels' own boot console

**The goal:** the user tested the framebuffer console (below) on real
Parallels hardware and it still didn't work - a screenshot showed
devicetree/ACPI/PCI discovery and the PCI device dump rendering
normally, then nothing further, not even `framebuffer::discover()`'s own
unconditional success/failure log line.

- Root cause, found by reading the `uefi` crate's own doc comment rather
  than guessing: `framebuffer.rs` opened `GraphicsOutput` with
  `open_protocol_exclusive`, which is specified to forcibly disconnect
  any driver holding the protocol `ByDriver` (the crate's own example:
  "opening the SERIAL_IO_PROTOCOL exclusively will disconnect the
  console driver from it"). On real Parallels hardware, firmware's own
  text console holds GOP `ByDriver` to render the boot screen - so the
  very first call into `discover()` silently killed the visible console
  before anything else could print. QEMU's `ramfb` test never caught
  this because it always ran with `-display none`, so there was no
  console driver attached to GOP to disconnect.
- **Fix:** switched to `OpenProtocolAttributes::GetProtocol` via the
  `unsafe` generic `uefi::boot::open_protocol` - a read-only, non-owning
  open, safe here since `discover()` only reads `ModeInfo`/`FrameBuffer`
  once and never touches the `GraphicsOutput` object again.
- Re-verified end to end on QEMU after the fix: GOP discovery still
  succeeds identically, the forced-fallback screendump still renders
  boot messages and the shell's banner/prompt correctly, and a full
  regression pass on the normal ACPI-console dev loop is unchanged. Zero
  aborts. **Not yet re-confirmed on real Parallels hardware** - that
  remains the next step.

## GOP framebuffer console: a fifth, better-grounded lead for Parallels output

**The goal:** find a real answer for Parallels console output after
virtio-console (below) was confirmed a dead end there. Prompted by the
user asking for research into how other OSes (Linux/FreeBSD) handle a
console on Parallels ARM64, given that Linux is known to run there.

- Deep research (a forked subagent, independently re-verified rather
  than trusted wholesale) turned up two claims. One - a specific PCI
  device ID mapping for Parallels' proprietary "ToolGate" mechanism -
  turned out to be unsourced by its own citation and was discarded. The
  other held up under direct verification: a FreeBSD forum thread
  (https://forums.freebsd.org/threads/parallels-on-macos-apple-silicon-freebsd-14-stuck-on-virtio_gpu.96762/)
  shows a real FreeBSD boot log on Parallels ARM64 with `VT: Replacing
  driver 'efifb' with new 'virtio_gpu'` - direct confirmation that a
  generic UEFI GOP framebuffer (`efifb`) drives early console output on
  that exact platform, and that Parallels' own VM config only offers two
  "video type" options (a proprietary GPU or VirtIO GPU) with no serial
  option at all. This matches this project's own Parallels boot
  screenshots, which already show UEFI graphics output working.
- **New module: `kernel/src/framebuffer.rs`** - discovers
  `EFI_GRAPHICS_OUTPUT_PROTOCOL` (a standard, fully-specified UEFI
  protocol, unlike every previous console mechanism this project has had
  to guess an address or convention for) during boot services, returning
  the framebuffer's physical base/size, resolution, stride, and pixel
  format. Only `Rgb`/`Bgr` (4 bytes/pixel, fixed known layout) are
  supported - `Bitmask`/`BltOnly` are rejected outright, since `BltOnly`
  specifically has no direct-memory-access path usable after
  `exit_boot_services` at all.
- **New module: `kernel/src/font.rs`** - a public-domain 8x8 bitmap font
  (`dhepper/font8x8` on GitHub, itself based on Marcel Sondaar/IBM's
  public-domain VGA fonts), downloaded verbatim via `curl` rather than a
  summarizing fetch specifically to avoid silent transcription errors in
  hex glyph data, then mechanically sliced down to the 95 printable-ASCII
  glyphs (0x20-0x7E) and embedded as a `const` array.
- **New module: `kernel/src/fbconsole.rs`** - a `Write`-implementing text
  console over the raw framebuffer: draws glyphs on a fixed character-cell
  grid, tracks a cursor, and scrolls via a raw `ptr::copy` pixel-row
  memmove directly in the framebuffer (deliberately no text buffer -
  matches this kernel's zero-heap discipline). Write-only: no keyboard
  driver exists in this kernel at all yet, a real gap independent of this
  console.
- **`console.rs` gained a `Framebuffer` variant** and an `is_installed()`
  accessor, so `main.rs` can gate the framebuffer console as a genuine
  last resort - tried only once devicetree/ACPI/PCI *and* virtio-console
  have all failed, since a real byte-stream console (which also gets
  input) is strictly more capable whenever one exists.
- **`mmu.rs::install_identity_map` gained an optional `framebuffer`
  argument.** Most of the time this needs no new mapping at all - on
  QEMU's `ramfb` device, the framebuffer address already falls inside the
  discovered RAM span, so the existing RAM loop covers it for free. Only
  if a framebuffer's containing 1GB block is *still* unmapped after that
  loop does this add one more Device-nGnRnE block for it (same
  convention as the existing fixed low-1GB device block, just at
  whatever address the framebuffer actually reports) - a real
  possibility on hardware this has never been tested against, not yet
  confirmed either way.
- **QEMU testing needed a real display device, and `-nographic` doesn't
  provide one at all** (confirmed by direct testing: `NoGop`). Verified
  instead with `-device ramfb -display none -serial file:...` (a
  RAM-backed, direct-access framebuffer QEMU builds specifically for
  headless use) plus a QMP `screendump` HMP command to capture the
  rendered output as a `.ppm` image for visual inspection - a new
  verification technique for this project, since every prior console
  driver could be checked through its own text output alone.
  `virtio-gpu-pci` was also tried and confirmed to report `BltOnly` under
  this QEMU/OVMF combination - a real, confirmed case of `discover()`'s
  pixel-format rejection actually triggering, not just a theoretical
  branch.
- **Confirmed working, not just "renders something":** a boot-time
  screendump (with a temporary `if true` override, matching this
  project's established technique for testing a fallback that QEMU's own
  ACPI console would otherwise always win over first) showed correctly
  rendered kernel boot messages and the loaded shell program's own
  banner/prompt - proving both `console::println!` and the shell's
  userland `putc` syscall reach the framebuffer, not just kernel-side
  text. A second run added a temporary 90-line print loop to force the
  scroll path: the screendump showed a clean, correctly-ordered scrolled
  view with no corruption or ghosting. Both temporary overrides were
  reverted before committing. Zero aborts in `-d int` cross-checks across
  both runs. A full regression pass on the normal QEMU dev-loop config
  (`-nographic`, real ACPI/PL011 console, piped-stdin `help`/`uptime`)
  confirmed no change in behavior when a byte-stream console exists -
  GOP is still discovered and logged, but the framebuffer console never
  installs, exactly as designed.
- **Still coarse, worth knowing before building on this:** no colour, no
  ANSI escape parsing (the shell's `clear` command's escape sequence just
  draws junk glyphs here instead of clearing the screen); no keyboard
  input at all (this console is write-only, and this kernel has no
  keyboard driver of any kind yet - a gap independent of this milestone);
  the MMU's device-block fallback path for a framebuffer outside the
  discovered RAM span has never been exercised against real hardware,
  only reasoned about; and confirmation against actual Parallels hardware
  is still pending - this was built and verified against QEMU's `ramfb`
  only, the same "confirmed on QEMU, awaiting real-hardware confirmation"
  posture every other console mechanism in this project has gone through
  first.

## Parallels console: virtio-console confirmed not applicable, with real hardware evidence

**The goal:** resolve the one open question the virtio-console milestone
(below) left for the user - does `try_virtio_console` actually find and
drive a device on real Parallels hardware. The user booted the real
`esp.hdd` on real Parallels-on-Apple-Silicon hardware and reported back;
this entry covers what that testing found and the diagnostic built to
make sense of it.

- Real Parallels boot confirmed devicetree/ACPI/PCI 16550 all fail
  exactly as already documented, and showed no visible output at all
  afterward - inherently ambiguous on its own, since the UEFI graphics
  console only renders during boot services, so "the driver ran and
  found nothing" and "the driver ran and hung" look identical on that
  screen.
- The user's separately configured Parallels "Serial Port" device
  (output to a file) received nothing at all, not even boot-firmware
  noise - unlike QEMU, where EDK2 opportunistically mirrors its own
  debug output onto any attached virtio-serial chardev. Inconclusive on
  its own (this firmware build might just not do that), so it ruled
  nothing in or out.
- **New permanent diagnostic: `pci::log_all_devices`
  (`kernel/src/pci.rs`)**, reusing the same boot-services
  `PciRootBridgeIo` walk `discover_uart16550` already proved safe on
  this hardware (reaching `NoSerialDevice` there, not `NoRootBridge`,
  was already proof the walk itself works). Logs every PCI device's
  vendor:device and class:subclass through the still-working pre-exit
  UEFI console, whenever all three normal console-discovery mechanisms
  fail - cheap, read-only, and adds no noise to the normal QEMU boot
  path.
- **The real Parallels PCI inventory this produced**: an Intel HD Audio
  controller, an Intel USB2 controller, an NEC USB3 controller, a
  virtio device (vendor `0x1af4`) with device ID `0x1000` -
  **virtio-net, not virtio-console** (which would be device ID `0x1003`
  or `0x1043`, neither present) - and one unclassified device under
  Parallels' own PCI vendor ID (`0x1ab8`, device `0x4000`, class
  `0xff`).
- **Conclusion: Parallels' serial port is very likely a proprietary
  Parallels device, not virtio-console at all** - no public
  specification exists for it. Reverse-engineering it (blind
  register/BAR probing against an undocumented protocol) was considered
  and explicitly declined - an open-ended task with no guaranteed
  payoff, categorically different from implementing a documented spec.
  A deliberate stopping point, not an assumption.
- A second, smaller finding from the same round of testing: the
  `esp.hdd` "opens as a folder" attachment concern that prompted this
  investigation was never a real problem - the user's Parallels version
  just labels the file-open dialog's button "Open" instead of "Choose."
  Confirmed by the same boot log successfully reading `INIT.CFG`/the
  shell binary off the real disk.

## virtio-console — a real transmit-only driver, confirmed on QEMU

**The goal:** the real lead flagged (and deliberately deferred) for
Parallels console output — `kernel/src/virtio_console.rs`, a fourth
console-discovery mechanism reusing `virtio_mmio.rs`'s existing
transport, modeled directly on `virtio_blk.rs`'s discover/init/one-
virtqueue/poll-based-completion shape.

- Discovery, feature negotiation (`VIRTIO_F_VERSION_1` only), and
  transmitq0 (queue 1) setup, mirroring `virtio_blk.rs` closely.
  Receiveq0 (queue 0) deliberately left unconfigured - transmit-only,
  matching the plan this milestone started from.
- New `Console::Virtio` variant: `write_str` batches through a local
  buffer with `\n`->`\r\n` translation and sends chunked virtqueue
  transfers (a full virtqueue round trip per `Device::write` call makes
  per-byte sends too expensive for whole log lines); `write_byte` sends
  one byte per call (used for the userland shell's character-at-a-time
  output); `read_byte` always returns `None` - no receive path exists.
- **A real placement constraint found while building this, not assumed
  going in:** unlike devicetree/ACPI/PCI (which install their console
  immediately after `exit_boot_services`), virtio-mmio discovery needs
  the device region mapped under this kernel's *own* translation tables,
  which only happens after `mmu::install_identity_map`. `try_virtio_console`
  in `main.rs` therefore runs later than the other three - a real,
  accepted consequence: boot messages between `exit_boot_services` and
  there are lost on a virtio-console-only platform.
- **Verified end to end on QEMU**, using the same kind of temporary,
  documented source-level force this project used to originally verify
  `exceptions.rs` (QEMU's default ACPI-first boot never organically
  reaches this fallback): discovery, feature negotiation, virtqueue
  setup, and real bytes reaching the host chardev all confirmed, `\r\n`
  bytes verified in the raw output via `xxd`, and the normal (unforced)
  boot path re-confirmed unaffected afterward. New `make
  run-virtio-console` Makefile target for future testing. Zero aborts in
  `-d int` cross-checks.
- **A genuine surprise along the way:** `-machine virt,acpi=off`, tried
  first as a way to organically force all three existing mechanisms to
  fail, instead made *devicetree* discovery succeed (this OVMF build
  apparently advertises a DTB when ACPI is disabled) - a real, useful
  finding about this specific firmware configuration, not the forcing
  mechanism actually needed here.
- **Parallels itself remains unconfirmed** - this environment has no way
  to boot real Parallels hardware. Whether Parallels exposes its
  console via virtio-mmio at all (vs. virtio-pci), and at the same
  address range this QEMU-confirmed driver assumes, is a real open
  question only a real Parallels boot can answer.
- **Still transmit-only, no RX**, deliberately - a receive virtqueue
  (symmetrically simpler than transmit) is the natural next step once
  Parallels itself is confirmed reachable this way.

## Phase 8 — `mv`, and the last real correctness risk in the write-support arc

**The goal:** `mv <src> <dst>`, the last cheap command-level win left in
the "close up the easy write-support commands" arc (phases 4-7) before
moving on to bigger work.

- `Fs::mv` reuses the file/directory's *existing* cluster chain rather
  than reading and rewriting content: locate `src`'s entry, insert a new
  entry for `dst` with the same cluster/size/kind, then free `src`'s old
  entry - same "write the new thing before touching the old one"
  ordering as `write_file`'s overwrite path, so a failure partway
  through (most likely `Error::DirectoryFull` on `dst`'s parent) never
  leaves `src` half-deleted. `dst` must not already exist - `mv` refuses
  rather than overwriting it or moving `src` inside it if `dst` happens
  to be an existing directory, narrower than a real `mv`'s full
  semantics.
- **The one real correctness risk, caught by design rather than by a
  crash:** when a *directory* moves to a *different* parent, its own
  `..` entry has to be patched to point at the new parent (or cluster
  `0`, root's convention, if the new parent is root) - otherwise a
  moved directory's `cd ..` would keep resolving to its *old* parent
  forever, silently. This is the identical cluster-`0`-means-root
  convention from phase 3c's `Fs::find`, reapplied on a write path
  rather than reintroduced as a new bug. Reused `patch_entry_cluster_size`
  (phase 6) to make the fix, rather than writing a new low-level sector
  patcher for it.
- One new syscall, `fs_mv` (14, `(src ptr, src len, dst ptr, dst len)`),
  and a new `mv <src> <dst>` shell builtin - the cheapest of the write
  commands to add on the shell side, since it's a single syscall call
  with no buffer/content handling at all, unlike `cp`.
- **Confirmed working end to end, with the `..`-fixup specifically
  exercised, not just reasoned about:** a same-directory rename; a
  cross-directory move of a plain file; a cross-directory move of a
  *directory* containing its own subdirectory, followed immediately by
  `cd`-ing into the moved directory and back out with `cd ..` - which
  correctly landed at the *new* parent, not the old one; renaming a
  directory in place (same parent) and confirming a doubly-moved nested
  directory's own `..` still resolved correctly afterward; destination-
  already-exists, missing-source, and missing-destination-parent all
  correctly rejected with the source left untouched. Persistence
  confirmed by an actual reboot - all of the above state, including the
  moved/renamed directory tree, was still correct on a fresh mount.
  `make run` (FAT16, no mount) still degrades gracefully. Zero aborts in
  `-d int` cross-checks across every session.
- **Still coarse:** no move-into-an-existing-directory-keeping-basename
  semantics (real `mv`'s most common shortcut); no cycle detection (`mv`
  a directory into its own descendant isn't guarded against); same
  8.3-short-name constraints as `mkdir`/`touch` apply to `dst`'s name.

## Phase 7 — `cp`, pure shell-side plumbing over existing syscalls

**The goal:** `cp <src> <dst>`, the next item phase 6's own changelog
entry and the roadmap parking lot both flagged as newly buildable now
that files can hold real content.

- **No new syscall, no kernel changes at all.** `cmd_cp` reads `src`'s
  entire content into a local stack buffer via the existing
  `fs_read_file` (the same syscall `cat` already uses), then writes
  that buffer to `dst` via the existing `fs_write_file` (phase 6) -
  creating `dst` if it doesn't exist, replacing it if it does, same
  semantics as `write`. Pure shell-side composition of two primitives
  that were already there.
- **Copying a file onto itself is safe by construction, not a special
  case.** The read completes in full, into the shell's own buffer,
  before the write ever starts - there's no window where a partial
  write could clobber a read still in progress.
- **Refuses rather than silently truncates when a source file is too
  large for the shell's read buffer** — a genuinely different judgment
  call than `cat`'s, which prints a truncated prefix with a notice.
  `cat` displaying an incomplete file is merely incomplete; `cp`
  producing an incomplete copy would be a *wrong* copy, so it errors
  outright instead.
- Confirmed working end to end against the real `esp.img`: copy to a
  new destination and to an existing one (overwrite), copy a file onto
  itself (content unchanged), copy a real pre-existing file
  (`INIT.CFG`) to a new name, copy into a subdirectory, and the usual
  error set (missing source, destination is a directory, missing
  destination parent) all correctly handled. `make run` (FAT16, no
  mount) still degrades gracefully. Zero aborts in `-d int`
  cross-checks. No new persistence check needed beyond what phase 6
  already established for `fs_read_file`/`fs_write_file` themselves -
  `cp` adds no new on-disk code path, only a new caller of two already
  reboot-tested ones.
- **Still coarse:** copy size is bounded by the shell's read buffer
  (256 bytes, same as `cat`'s); no recursive directory copy, no `-r`
  flag or any flags at all; no `mv`.

## Phase 6 — `write`, the first way to put real content into a file

**The goal:** close the gap phase 5 left open — every file this kernel
could create was permanently zero bytes, since `touch` only ever
produces empty files. `write_file` is the "actual blocker for `cp`,
output redirection, and anything else that needs a file to hold more
than zero bytes" that phase 5's changelog entry and the roadmap parking
lot both flagged.

- `Fs::write_file` creates a file with exactly the given content, or
  fully replaces (not appends to) an existing file's content. Ordering
  matters: the new cluster chain is allocated and written *before*
  anything about an existing file is touched (freeing its old chain,
  patching its directory entry), so a failure partway through never
  frees or unlinks a file that was already there.
- Two new private helpers: `write_chain` (allocates and writes a fresh
  cluster chain for arbitrary-length data, linking each cluster to the
  next via `write_fat_entry` as it goes) and
  `patch_entry_cluster_size` (rewrites just an existing entry's cluster
  and size fields in place, leaving name/attribute/timestamps alone —
  what makes overwriting different from creating).
- One new syscall, `fs_write_file` (13, `(path ptr, path len, data ptr,
  data len)`), and a new `write <file> <words...>` shell builtin that
  joins the remaining words with spaces (same style as `echo`) and
  writes the result as the file's entire content.
- **A real bug found immediately by testing, not by inspection:**
  `write <file>` with no content words (a legitimate "truncate to
  empty" case, exactly matching `touch`'s own empty-file semantics)
  failed with a generic disk error instead of succeeding. Root cause:
  the syscall's argument-sanity check (`valid_user_range`) rejected any
  zero-length buffer, correct for `fs_list_dir`/`fs_read_file`'s output
  buffers (a zero-length destination is pointless there) but wrong for
  `fs_write_file`'s *input* data, where empty is a real, meaningful
  value. Fixed with a second check, `valid_user_range_allow_empty`,
  used only for the data argument (the path argument still can't be
  empty).
- Confirmed working end to end against the real `esp.img`: create a
  file with content, `cat` shows it exactly; overwrite with shorter
  content, `cat` shows only the new content (no stale trailing bytes
  from the longer original); the empty-write fix specifically
  re-tested and confirmed fixed; writing to an existing directory or a
  missing parent both correctly rejected; a genuine reboot-and-remount
  persistence check (write, reboot, `cat` still shows the content); no
  corruption of pre-existing files; `make run` (FAT16, no mount) still
  degrades gracefully. Zero aborts in `-d int` cross-checks across
  every session.
- **Still coarse:** every write is a full replace, no append and no
  partial/offset writes; content is bounded by whatever the caller can
  fit in one buffer (the shell's `write` command specifically is capped
  by its 128-byte input line, so it can only ever produce a single
  FAT32 cluster's worth of content on this test image — `rm`'s
  multi-cluster cluster-chain-freeing loop, added in phase 5, remains
  logically implemented and reasoned-through but still not exercised by
  an end-to-end test, since nothing in this kernel can yet create a
  file spanning more than one cluster). No `cp`, no output redirection
  yet — those need shell-side plumbing this syscall alone doesn't
  provide (see `roadmap.md`'s parking lot).

## Phase 5 — file lifecycle: `touch`/`rm`

**The goal:** round out phase 4's directory-only write support with the
file equivalent — create and remove files, not just directories.

- `Fs::touch` turned out simpler than `Fs::mkdir`: a real FAT32 empty
  file needs no allocated cluster at all (a directory entry with
  starting cluster `0` and size `0` is a valid empty file per spec), so
  it's just one `insert_dir_entry` call, no `find_free_cluster`/
  `zero_cluster`/`.`/`..` writes. Succeeds as a no-op if the file already
  exists (no RTC to update a modification time with, so "no-op" is the
  honest approximation of real `touch`'s behavior there).
- `Fs::rm` mirrors `Fs::rmdir`'s shape, plus one thing `rmdir` never
  needed: freeing the target's entire cluster chain (a loop over
  `next_cluster`/`write_fat_entry`) before freeing its directory entry -
  a no-op for an empty file, but real for anything with content once a
  future "write file contents" syscall exists.
- Two new syscalls, `fs_touch`/`fs_rm` (11/12, path pointer/length only),
  and matching `touch`/`rm` shell builtins.
- **A latent bug caught by reasoning, not by testing:** `Fs::find`'s
  cluster-`0`-means-root substitution (from phase 3c) applied to *every*
  resolved entry, not just directories - harmless before this milestone
  (nothing but a `..` entry ever had cluster `0`), but `touch` was about
  to make "cluster `0`, and it's a file" common. Fixed by gating the
  substitution on `is_dir` before writing any test that could have hit
  it.
- **Still no way to write content into a file** - `touch` only ever
  produces zero-byte files. `cp`/output redirection need a real
  "write file contents" syscall this project doesn't have yet.
- Confirmed working end to end against the real `esp.img`, including the
  same reboot-and-remount persistence check as phase 4, `rm`/`rmdir`
  correctly refusing each other's target (a directory vs. a file), and
  no corruption of pre-existing files.

## A shared syscall-ABI crate

Every "known rough edges"/"next milestone" note since phase 3c had
flagged the same thing: syscall numbers and sentinel values were
hand-duplicated between `kernel/src/syscall.rs`'s dispatch table and
`shell/src/main.rs`'s caller, kept in sync only by convention. Fixed
with a third workspace member, `syscall-abi/` — a plain `#![no_std]`
library crate holding nothing but `pub const` syscall numbers and
sentinel values, no logic. Both `kernel` and `shell` now depend on it
via a path dependency and reference constants directly
(`syscall_abi::FS_MKDIR`, etc.) instead of local, independently-numbered
consts — a future syscall added on one side with the wrong number
literally fails to compile on the other rather than silently
misbehaving at runtime. Confirmed safe against this project's specific
relocation risk (every value is a scalar `u64` const, inlined as an
immediate at the use site under both targets — fundamentally different
from the `core::fmt`/slice-literal-comparison bug, which is specifically
about pointers to literal `.rodata` data).

Also folded in during the same stretch of work: a real UX bug where
every `fs_*` syscall collapsed "no filesystem is mounted this boot" and
"the filesystem is mounted but this operation failed" into the same
`u64::MAX` sentinel, making every disk command on `make run`'s FAT16
disk look identical to a genuinely broken path. Fixed by splitting the
sentinel (`NO_FS`, `u64::MAX - 1`, distinct from `FS_ERROR`) so the
shell can print an explicit "no filesystem mounted" message instead.

## Phase 4 — first filesystem write support: `mkdir`/`rmdir`

**The goal:** cross the write-support line phase 3 deliberately drew, for
the narrowest useful case — creating and removing empty directories — not
the full write surface (`rm`/`touch`/`cp`/`mv`/redirection) at once.

- `virtio_blk::Device` gained `write_sector`, sharing a `submit_request`
  helper with `read_sector` (the only real difference between the two
  requests is the data descriptor's write-flag direction and the
  request-type field).
- `fat32.rs` gained `write_fat_entry` (keeps every FAT copy in sync, not
  just the first — reads only ever needed the first), `find_free_cluster`,
  `zero_cluster`, and `write_raw_entry`, plus `Fs::mkdir`/`Fs::rmdir`
  themselves.
- Two new syscalls, `fs_mkdir`/`fs_rmdir` (9/10, path pointer/length
  only), and matching `mkdir`/`rmdir` shell builtins.
- **Deliberately narrow, not a full write implementation:** no
  directory-extension (a full parent directory makes `mkdir` fail rather
  than growing it), no file creation/deletion, no `cp`/`mv`, and a
  conservative 8.3 short-name character set (ASCII alphanumerics plus
  `_`/`-` only) for names this kernel creates.
- Confirmed working end to end against the real `esp.img`, including a
  genuine reboot-and-remount persistence check (not just a live
  in-memory one), root-removal and already-exists rejection, and
  no corruption of pre-existing files. The on-disk write ordering each operation follows is deliberate
  (claim-before-use for `mkdir`'s cluster allocation,
  check-everything-before-writing-anything for `rmdir`).

## Phase 3 — a fully functional shell with disk commands

**The goal:** `ls`, `cat`, `cd`, `pwd` — a shell that can actually browse
and read the filesystem it's running from, not just talk to the console.

**Why this is bigger than phase 2, and can't be shortcut:** every disk
read so far happens during the UEFI boot-services window, before
`exit_boot_services` — a one-shot, one-way door. There is no way to reach
back into UEFI's filesystem protocol once the shell is actually running;
"disk commands" by definition means reading files *after* boot, which
means finally building the runtime storage stack that's been deliberately
deferred twice already (once in the phase-1 shell milestone, again in the
disk-loading milestone) rather than a shortcut on top of what exists. This
phase is really three dependent stages:

### 3a. A real block device driver

- **Transport: virtio-mmio, confirmed and deliberately chosen over
  QEMU's own default.** Addresses confirmed via the same devicetree-dump
  technique as GICv2/the timer (32 slots, `0xa000000`, `0x200` apart).
  Worth recording since it wasn't the obvious path: a plain
  `-drive ...,media=disk` with no `if=`/`-device` actually auto-attaches
  as **virtio-blk-pci**, not virtio-mmio — reaching that at runtime would
  need this kernel's own PCI/ECAM config-space walk (a real subsystem on
  its own, comparable to writing PCI enumeration a second time, since the
  existing `pci.rs` is boot-services-only). The Makefile now attaches the
  drive as virtio-mmio explicitly instead, sidestepping that entirely.
  Modern (non-legacy) register interface, also deliberately chosen and
  verified via direct QEMU-monitor memory peeks before any driver code
  was written — including a real bug in the diagnostic process itself (a truncated
  monitor read that briefly looked like "no block device exists at all").
  Parallels' own virtio-mmio behavior is still unconfirmed — same open
  question already on record for virtio-console.
- **Driver scope, as built:** device discovery, feature negotiation (just
  `VIRTIO_F_VERSION_1`), one virtqueue, synchronous polling block reads.
  Write support stayed out of scope, as planned at the time (see phase
  4, above).
- Confirmed working end to end: reads sector 0 back and checks the real
  MBR boot signature, not just "no error returned." See
  `kernel/src/virtio_mmio.rs`/`kernel/src/virtio_blk.rs`.
- **Kernel-resident, not a user-space driver process — a deliberate,
  explicit choice, not an oversight.** `docs/research-minix-boot.md`
  raised a real fork here: writing virtio-blk (and eventually
  virtio-console) as an isolated EL0 driver process would be a concrete
  step toward this project's stated microkernel goal, using the driver as
  the forcing function for dynamic task creation and real IPC. Decided
  against, for now — that would pull the process-model/IPC work (see
  `roadmap.md`'s parking lot) into phase 3's critical path, and phase 3's
  actual goal is disk commands working at all. Revisit once there's more
  than one reason to want driver isolation; virtio itself doesn't
  require it.

### 3b. A filesystem reader

- **Target format: FAT32, as planned** — matches `make image`'s
  `hdiutil -fs FAT32` output, what Parallels ultimately boots from too.
  Real surprise along the way: `make run`'s fast dev-loop disk
  (`fat:rw:esp`, QEMU's `vvfat`) turned out to be **FAT16**, confirmed by
  decoding its BPB by hand before writing any parser code (`BS_FilSysType`
  literally reads `"FAT16   "`) — `make run-image` (Makefile target)
  boots the real `esp.img` instead, since `run`'s disk can never satisfy
  a FAT32 mount.
- **Decided: hand-rolled, not a crate** — turned out to be more than a
  style preference: this reader runs after `exit_boot_services`, where
  the global allocator is no longer valid, and every `no_std` FAT crate
  surveyed assumes an allocator is reachable somewhere in its stack. A
  hard constraint, not just precedent.
- **A second real surprise, since resolved:** this project's own
  `\EFI\OUROBOROS\` directory name didn't fit FAT's 8.3 short-name limit
  (9 characters) — real FAT32 formatters handle that with a long-filename
  (LFN) entry this reader doesn't parse. Resolved at the start of 3c by
  renaming the directory to `\EFI\OUROBORO\` (8 characters) rather than
  implementing LFN parsing — this project controls the name, so once 3c's
  actual need (the shell navigating there) was concrete, renaming was the
  cheaper, more honest fix. `fat32.rs` still has no LFN support in
  general; any *other* 9+ character name is still unreachable.
- Confirmed working end to end: lists `\EFI\BOOT`, reads `BOOTAA64.EFI`
  back (a real multi-cluster file, not a single-block special case) and
  checks its exact size and PE header magic against the real built
  binary. See `kernel/src/fat32.rs`.

### 3c. New syscalls + the commands themselves

- **Syscall shape, as built:** `fs_list_dir`/`fs_read_file` (7/8), each
  taking `(path ptr, path len, buf ptr, buf len)` and writing into the
  caller's buffer directly, rather than the originally-sketched
  `open_dir`/`read_dir`/`open_file`/`read_file`/`close` handle-based
  shape — simpler, and sufficient since nothing here needs a persistent
  open handle across multiple calls. This is also what pushed the
  syscall ABI itself from 1 argument to 4 (`x0`-`x3`).
- **All four commands built, all read-only, matching the original
  target:** `ls [path]` (`fs_list_dir`), `cat <file>` (`fs_read_file`),
  `pwd` (shell-local `cwd` state, no syscall), `cd <path>` (shell-local
  state + `fs_list_dir` for validation, no dedicated "exists" syscall).
- **Two real bugs found by testing the actual commands, not by
  inspection** - a slice/string-vs-literal comparison (`cwd_bytes != b"/"`)
  crashed for the same underlying reason `core::fmt` does (a data
  reference computed for link-time base `0x0`), and a FAT32 `..`-entry
  convention (cluster `0` means "root," not root's real cluster number)
  that wasn't handled hung the *entire system* - masked-IRQ syscall
  context, nothing left to preempt a runaway computation. Both fixed;
  `shell/src/main.rs` also gained path normalization (`normalize_path`)
  as a direct result, collapsing `.`/`..` instead of letting `cwd`
  accumulate them literally.

**Deliberately out of scope at the time** (later picked up as phases
4/5, or still open — see `roadmap.md`): any write support (`mkdir`,
`rm`, `touch`, `cp`, `mv`, output redirection — writing to a real
filesystem is a meaningfully bigger risk than reading, and doesn't block
anything read-only commands need); `stat`/`file`/`find`/`tree`; wiring
`loader.rs` itself to use the new runtime driver instead of
boot-services file I/O (boot-time loading already works and has no
reason to change).

## Phase 2 — real commands

`help`, `echo`, `uptime`, `clear`, with `uptime` backed by a real new
syscall (`get_ticks`) rather than being another echo demo.

## Phase 1.5 — the shell becomes a real process

Moved from a kernel-compiled EL0 blob to a genuine separate program,
loaded from disk at boot and selected by a config file — see
[`processes.md`](processes.md). Unplanned at the start of phase 1, but a
prerequisite for phase 2 meaning anything more than kernel demos.

## Phase 1 — interactive echo shell

Real UART input, a line editor, live character echo with backspace/DEL
handling.

## Phase 0 — boot infrastructure

UEFI entry, console discovery (devicetree/ACPI/PCI), exception vectors,
a real MMU identity map, GICv2 + timer-driven preemption, the syscall
boundary, and two-task preemptive round-robin scheduling. See
[`architecture.md`](architecture.md).

# Async `NETOP_RMOUNT`: the plan

**Step 0 of the standup item "session-scoped authentication and an async
`NETOP_RMOUNT`", and its deliverable: an ordered plan with a named check per
step, written before the code.** The two halves of that item are two arcs, and
`roadmap-fid-verbs.md` said so three times (Decision 3, Decision 4, the
boundary): changing the concurrency model and the auth model in one change is
two risky things at once. This plan is the concurrency half. Session-scoped
authentication is its own plan, after this one lands, and nothing here
forecloses it.

Grounded in the code at `4f9b8e4` (2026-09-21), by reading the paths rather
than the comments above them.

## The symptom, stated precisely

`netd` blocks its whole event loop for every remote round trip. A local
program's `NETOP_RMOUNT` is a synchronous `MSG_CALL`, and `netd` answers it
from inside the handler: `handle_rmount` builds the frame, `tcp_get` (or the
held session's `client_exchange`) runs its own receive loop until the reply is
complete, and only then does `reply` go out and the event loop resume. While it
waits, the export serves nothing, a `cpu` run makes no progress, and the health
ping is acked only by `beat_if_new_tick`.

That is why #147 had to *refuse*. A `cpu` run is the one place the loop is
pumped re-entrantly (`tcp_run` drains the mailbox while it waits), and every
handler a client could reach from that drain sits on top of `handle_run` and
`tcp_run`'s frames; the remote-mount relay overflowed the guard page there,
measured, so an identified client's request mid-run answers `FS_ERR_BUSY`. The
cpu child's own Phase 4b remote-fs (its `/host` reads) takes the same deep path
and is not refused, because it is the run; that overflow is latent. The ledger
in `roadmap-fid-verbs.md` names "async `NETOP_RMOUNT`" as the fix for both.

## What is actually there

Four ways `netd` drives a TCP client connection, three of them blocking:

| | shape | blocks the loop | used by |
|---|---|---|---|
| `tcp_get` | connect, send, receive until a framed reply or FIN, close | yes | `oneshot_rmount` (path verbs), `handle_fetch` (HTTP) |
| `client_connect` / `client_exchange` | the same, split so a `ClientSession` keeps the connection between verbs | yes | `session_rmount` (fid verbs) |
| `tcp_run` | connect, send, receive until FIN, pumping the export and the mailbox meanwhile | yes, re-entrantly | `handle_run` (`cpu`) |
| `DialConn` + `pump_dials` + `dial_on_segment` | an event-loop state machine: SYN and data with retransmit, a send and a receive buffer, FIN, idle reap | **no** | the `/net/tcp` connection files |

The fourth is the shape the other three should have had. It already exists
because the `/net/tcp` files could not block: a client polls `status` while
`netd` connects. Nothing about it is specific to `/net/tcp` except its buffer
sizes (512 out, 768 in, chosen for stack) and the listener/accept states.

**One kernel fact shapes the design.** A reply to a `MSG_CALL` is a `MSG_SEND`
addressed to a *slot*, allowed by the reply exemption while that slot's task is
blocked calling us. Today the reply goes out before the handler returns, so the
caller is the task that asked. A parked request answers later, and in between
the caller can die (Ctrl+C), its slot be recycled, and the next occupant call
`netd` itself: the parked reply would then complete the wrong call, with the
wrong bytes. `SENDER_TASK` gives the caller's full identity (generation and
slot) for exactly this class of bug, and `tasks::live_occupant` is the kernel's
own "is the task I remembered still there"; but `MSG_SEND` takes a slot. A
deferred reply needs a send that names an identity.

## Decisions

**Decision 1: park the call, reply to an identity.** `handle_rmount` stops
waiting. It resolves the caller's name and uid, the expected reply key and the
next hop as today (all before any network work, for the reasons written there),
frames and signs the request into a remote connection's send buffer, records
the caller's `SENDER_TASK` identity, the nonce and the key beside it, and
returns without replying. The event loop drives the connection; when the framed
reply is complete a service pass verifies it and replies to the *identity*.
`MSG_SEND` gains that spelling: a destination at or above `1 <<
TASK_ID_SLOT_BITS` is a packed identity (every generation is at least 1, so no
slot number can be mistaken for one), resolved through `live_occupant`, and
refused with `TASK_ERR_NO_SUCH_TASK` when the occupant has changed. The reply
exemption then applies to the resolved slot as before. Alternative rejected: a
new syscall. The destination word already has room, and one arm with one check
is smaller than a second entry point that must repeat the buffer and capability
checks.

**Decision 2: one engine, not a fourth.** `DialConn` becomes generic over its
two buffer sizes, `pump_dials` and `dial_on_segment` take slices, and the
remote-mount client is a second table of the same type on `serve`'s frame,
sized for a framed NP request and a sealed NP reply (1 KB each: a client
message is at most `MSG_MAX_LEN` 768, so a signed frame is under 910 bytes, and
a sealed reply to a `NP_REMOTE_CHUNK` read is under 600). A `Parked` record
rides each remote connection. The `/net/tcp` table keeps its sizes and its
budget, so a remote mount never competes with a dial-out for a slot, and the
listener/accept states are simply never entered by a remote slot. Alternative
rejected: a `RemoteConn` with its own SYN/data/FIN machine, modelled on
`DialConn`. Two copies of a TCP state machine in one file is the copies-drift
class, and the buffer sizes are the only thing that differs.

**Decision 3: three steps in this order, one-shot first.** The path verbs
(`oneshot_rmount`) go async first: they carry no state between calls, so the
parked record is the whole story, and the stack win is immediate. The fid
verbs' held session is next: a `ClientSession` becomes a remote slot that stays
`Established` between verbs, with its fid refcount and idle reap, and
`client_connect`/`client_exchange`/`session_exchange`/`TcpScratch` are deleted.
The `cpu` run is last: `tcp_run` becomes a remote slot whose reply completes on
FIN rather than on a framed length, the output lands in `PendingRun` as now,
and the re-entrant drain, the by-sender refusal and `handle_run`'s pumping all
go, because nothing blocks. Each step deletes a blocking path and is verified
on the rigs that path has.

**Decision 4: what stays synchronous, on purpose.** ARP (`arp_resolve` polls
for up to half a second), DNS (`handle_resolve`), ICMP (`handle_ping`) and the
HTTP fetch (`handle_fetch` over `tcp_get`) keep blocking. They are bounded,
they carry no fid, and none of them is on the re-entrant path once step 3
removes it. Making them async is the same recipe applied later, with the
exhaustion as the evidence.

## The steps

**Step 0: the kernel arm.** `MSG_SEND` to a packed identity. Its check rides
step 1 (there is no client for it before then): a parked request whose caller
was killed and whose slot a second caller now holds must be refused, and the
second caller must get its own reply. The rig is a stalling host peer
(`np9p_server.py --delay`), `exec` a `cat` against it, `kill` it, then `cat` a
different file from the same shell: the second `cat` prints its own file. On
the tree with the arm sending to the slot instead, it prints the first file's
bytes, which is the misdelivery the arm exists to refuse.

**Step 1: the top-level one-shot path.** `oneshot_rmount` and its use of
`tcp_get` are replaced by a parked remote slot, for a path verb arriving at the
TOP-LEVEL drain. The re-entrant drain is left exactly as #147 left it: it
refuses an identified client by sender. Parking there was tried and reverted,
because the park still signs the request (a deep Ed25519 frame build,
`frame_signed`), and the run path that reaches the re-entrant drain is already
at its guard page. So the re-entrant drain, and the cpu child's own Phase 4b
`/host` read that shares its depth, stay on the synchronous path until step 3
removes the drain outright; the concurrency win of step 1 is at the top level,
where a mount no longer blocks the loop. Checks: the driven single-VM rig
`scripts/test-async-rmount.sh` (served, identity, concurrent, each with a
measured control); the fid gate and path gate unchanged (the session path is
untouched); the two-node `test-reentrant-session.sh` still refuses `cat` and
`resolve` mid-run with no fault, exactly as before.

**Step 2: the session path.** `ClientSession` becomes a remote slot held
between verbs. Checks: the two-VM ext2 rig's `cbig`/`cwrite` witnesses, the fid
gate, and a re-entrant `cbig` recipe, which the by-sender refusal made a flaky
driver and step 3 makes unnecessary.

**Step 2 landed 2026-09-22.** The held session is a `SessionConn`, the same
`Wire` engine as the path table with a third const buffer (`held`) that carries
the first verb while its `NP_SESSION` opens. `handle_rmount`'s fid branch calls
`park_session` and returns without waiting; `service_remotes`, run once per
table, does the two-phase open (sign the held verb only after the far side
answers the `NP_SESSION` with status 0), delivers replies to the caller's
identity, keeps the fid refcount, and closes a session at zero fids or on a
fail-dead reply. The whole blocking path is deleted: `ClientSession`,
`TcpScratch`, `SESSION_BUF`, `client_connect`, `client_exchange`,
`client_close`, `session_exchange`, `find_or_open_session`, `session_rmount`,
`reap_client_sessions`. The source-port window is split across both tables by
slot index (`remote_src_port`, `REMOTE_SLOTS`) rather than the retired
`MAX_REMOTE == 2` assert, and `pump_dials` takes its table's idle threshold so a
held session lives `CLIENT_SESSION_IDLE_TICKS` (40 s) where a dial or a parked
path verb lives `DIAL_IDLE_TICKS` (12 s). Net: netd `main.rs` 462 lines deleted,
339 added. Measured: `make test-async-rmount` 6 of 6 (the four step-0/1 checks
plus the two step-2 checks below), the two-node ext2 `cbig`/`cwrite`/`cwrite`-as-user
witnesses (0 faults both nodes), and `make test-reentrant-session` served (both
recipes refused with 0 faults).

Two new async-rig checks, each with a measured control. **Check 5 (session):**
a C program's `open`/`fstat`/`read`/`close` (`/bin/cremote`) is served from a
parked session slot held `Established` between the verbs; its control is the
close-after-every-reply mutation (force `c.fids = 0` before the keep-open test
in `service_remotes`), which makes cremote's `fstat` reach a clunked fid and
fail, measured. **Check 6 (boundary):** a fid verb parked on a peer whose
`NP_SESSION` reply never comes (the `--delay 30` host peer) does not stall a
concurrent path-verb `cat` of a live peer, the cat's bytes landing BEFORE the
open fails at the 5 s deadline. This is the step-1/step-2 boundary the third
review of #149 named. Its control is `main`'s netd, where the session path
still blocks the loop for its round trip and the cat is served AFTER the
failure, measured.

**The stack moved, exactly as finding 5 said, and was measured not reasoned.**
A `SessionConn` is 2896 bytes (its send/recv buffers plus the 752-byte held
verb); the old `ClientSession` held no buffers at all, a round trip borrowing
the caller's. Three session slots resident on `serve`'s frame is ~8.5 KB more
under the deep `cpu`-run chain (`handle_run` -> `tcp_run`) already at the guard
page. The two-node rig faulted `tcp_run`'s own prologue at 48 KB, overflowing
704 bytes into the guard page (`far_el1` inside it, a clean EL0 fault). Grew
`STACK_PAGES` 12 -> 14, restoring step 1's ~7 KB of headroom. Step 3 removes the
re-entrant run drain and can hand the pages back. This is not the plan's "should
fit 48 KB" being wrong so much as the reason it was measured: the compiler put
the resident table where it tipped a chain that was already at the edge.

The overlap refusal is now a stated, exercised limit: two fid verbs from one uid
to one endpoint can overlap (the blocking path made that unspellable), and
`park_session` refuses the second `FS_ERR_BUSY`, which libc's `read()` loop does
not retry, so two C programs sharing one remote mount under one user is a
documented boundary, not a bug.

*Sized 2026-09-21, from the code at `78352df`, before any of it was written.*
Eight things the paragraph above does not say, found by reading the paths step 2
replaces (`client_connect`, `client_exchange`, `session_exchange`,
`find_or_open_session`, `session_rmount`, `reap_client_sessions`, `TcpScratch`:
about 330 lines, all of which go). (1) A fid verb's park is TWO-PHASE when no
session exists: connect, send `NP_SESSION`, verify its reply is status 0, and
only then frame and send the verb. The raw message (up to 752 bytes) has to be
held on the parked record and signed later from the service pass, because the
far export serves one framed request per session reset and two frames
pipelined on the stream is not a case it handles. Signing from `service_remotes`
is affordable: it runs at the top level, not the re-entrant drain. (2) A
session slot outlives its reply: it stays `Established`, carries the fid
refcount, closes on the last successful clunk, and needs its own idle
threshold, because the engine reaps an idle slot at `DIAL_IDLE_TICKS` (12 s)
while a held session must live `CLIENT_SESSION_IDLE_TICKS` (40 s), above the
far side's 30 s. A per-slot exemption, as the listener has. (3) Two fid verbs
from one uid to one endpoint can now OVERLAP, which the blocking path made
impossible. Refuse the second `FS_ERR_BUSY` (the plan's answer to every budget),
and record that libc's `read()` loop does not retry on it, so two C programs
sharing one remote mount under one user is a stated limit, not a bug to chase.
(4) The per-slot source-port window is pinned by a compile-time assert to
`MAX_REMOTE == 2`; session slots need the split re-derived. (5) The stack moves
rather than shrinks: each session slot is a `RemoteConn` (~2 KB) plus the
held raw message from (1) (752 bytes), ~2.7 KB RESIDENT on `serve`'s frame,
three of them ~8 KB, against the ~5.7 KB `session_rmount` frame that
disappears. It should fit 48 KB, and step 1 showed the cost is where
the compiler puts it, so it is measured on the two-node rig, not reasoned. (6)
The fid refcount and last-clunk close move into the service pass, so the verb
is recorded on the parked record. (7) The deadline arm needs a stated rule for
a session slot: `service_remotes` fails a parked request at
`REMOTE_DEADLINE_TICKS` (5 s) and closes the slot. On a session that is
fail-dead, the verb answers `FS_ERROR`, the session closes and its fids go,
and the client re-opens on its next call: exactly what `client_exchange` does
today after one second of silence (its `deadline = now() + 50`), so the rule
is inherited, not new, and five seconds is the more lenient of the two. It
also settles the late reply: it lands on a closed slot, never ahead of a next
verb's reply. (8) The blast radius is wider than the seven functions: the
`sessions` table is threaded through the signatures of
`drain_client_messages`, `handle_client`, `tcp_run` and `handle_run`, so
whether sessions get their own slot table or join `remotes`, four signatures
on the run path that step 1 left exactly as #147 left it are edited in step
2 and again when step 3 deletes the drain. One check the paragraph above lacks: a fid
verb parked on a slow peer must not stall a concurrent path-verb mount, the
boundary the third review of #149 named and the reason step 2 exists; it rides
the async rig with a delayed host peer and a C program. Estimate: a day, the
shape of step 1 (#149: ~430 netd lines plus a 16-line kernel arm, three
review rounds). Step 2 itself needs no kernel change and deletes more than it
adds.

**Step 3: the run.** `tcp_run` and the re-entrant drain go. Checks: every
`test-reentrant-session.sh` recipe is *served*; `cpu` output past one message
still pulls through `NETOP_RUN_MORE`; the supervisor's ping is acked during a
run with no `beat_if_new_tick` in the way.

**Step 3 landed 2026-09-22.** `handle_run` became `park_run`: it builds and
signs the `NP_RUN` as before, then parks it and returns. `service_remotes`
drains the slot's receive buffer into `PendingRun` as output arrives, so
`RUN_OUT_MAX` of output never has to fit `REMOTE_RBUF`, and answers the caller
on the peer's FIN. A run slot is identified by `verb == NP_RUN`, which also
says its reply is a stream rather than a framed NP reply and carries no
signature, so no new field was needed. Deleted: `tcp_run` (139 lines),
`pump_conns` (32, it existed only so `tcp_run` could keep the export flowing),
the re-entrant drain, the by-sender refusal of #147, the chained-cpu refusal,
the `Option<&mut PendingRun>` that only existed to say "re-entrant", and
`handle_client`'s `conns` parameter. 266 lines out, 144 in.

**The latent overflow was not latent, and it is closed.** Measured with a
control: on `main`, `cpu <A> ls /host` and `cpu <A> cat /host/HELLO.TXT` BOTH
fault netd at the guard page (`esr_el1=0x9200004f`, `elr` in `handle_tcp`'s
prologue, reached from `tcp_run`'s `on_frame`) and the supervisor restarts the
server; on this branch both return the right bytes with zero fault lines on
both nodes. Phase 4b remote-fs was **broken** on `main`, not merely fragile,
and nothing had reported it because no rig ran a `/host` read.

**Three findings the paragraph above did not say.**

1. *The run's timeout changes shape.* `tcp_run` used 300 ticks of silence but
   reset it on ANY frame it served, the child's `/host` callbacks included. A
   parked run never sees those, so reproducing that coupling would mean
   threading export activity into the slot. `RUN_DEADLINE_TICKS` is 30 s of
   silence on the run connection instead: strictly more permissive than the old
   rule for the case that changed, a child working silently on `/host` reads.

2. *Step 3 does NOT hand back step 2's two pages, which this plan said it
   would.* Measured at 48 KB: still faults, 1936 bytes into the guard page, in
   `caller_name` beneath `park_run`. Deleting the deepest function did not make
   the deepest CHAIN shallower, because what dominates is no longer a transient
   frame at all: it is step 2's RESIDENT tables on `serve`'s frame, which every
   chain stands on. Two attempts to buy a page back were measured and both
   refused: `#[inline(never)]` on `park_run` made it WORSE (3072 bytes over,
   because inlining had been overlapping its buffers with `handle_client`'s
   fetch/resolve ones, whose lifetimes are disjoint), and sharing one reply
   buffer in `service_remotes` moved this chain not at all. The pages come back
   when the resident tables shrink, not when a function is deleted. **The
   step-1 lesson is about SIBLING branches, not about depth in general**, and
   this is the case that draws the line.

3. *Inverting the re-entrant rig cost it a distinction, and the header says
   so.* Before, "refused" and "succeeded" were different strings, so the
   harness could see whether the condition had been created and retry when the
   run had returned first. Now a client served DURING a run and one served
   AFTER it returned produce the same output. The rig is a regression check
   against the refusal and the overflow, not a proof of overlap; the async
   rig's ordering checks are where overlap is proven. It also gained the
   session recipe (`cbig`) the by-sender refusal had made too flaky to grade,
   and its `resolve` recipe expects "no response" because the two-node link has
   no SLIRP and so no DNS - netd running the resolver at all is the thing being
   checked.

## Deliberately not in scope

- **Session-scoped authentication.** Its own plan, after this. The held
  session step 2 produces is where it will live.
- **Async ARP, DNS, ICMP, HTTP fetch.** Decision 4.
- **Raising any budget.** `MAX_REMOTE` starts at 2, the smallest number that
  lets a bystander's read proceed while a child's is parked; raise it when a
  rig exhausts it.

## Ledger

Kept as the steps land, newest first.

- **Step 3 landed 2026-09-22**, the same day as step 2, and with it the arc's
  concurrency half is done: nothing in `netd` blocks its event loop for a
  remote round trip any more. The detail is in the step 3 note above. The two
  things worth carrying out of it: the overflow #147 called latent was
  reproducible on demand and had broken Phase 4b outright, and a step that was
  planned to RETURN stack instead proved the stack is now dominated by resident
  tables rather than call depth.
- **Step 2 landed 2026-09-22**, the day after it was sized, and the sizing
  held: a day, the shape of step 1, deleting more than it adds (462 lines out,
  339 in), no kernel change to the ABI but ONE to the loader (`STACK_PAGES` 12
  -> 14, the resident-table growth finding 5 predicted and the two-node rig
  measured: `tcp_run`'s prologue over the guard page at 48 KB). All eight
  sized findings landed as written: the two-phase park with the held verb, the
  40 s vs 12 s idle split per table, the overlap refusal (`FS_ERR_BUSY`, libc
  does not retry), the re-derived per-slot port window, the moved-not-shrunk
  stack, the refcount and last-clunk close in the service pass, the inherited
  five-second deadline, and the four run-path signatures re-threaded. The
  latency-boundary check finding also landed (async-rig check 6). Nothing in
  the sizing turned out wrong on contact with the code; the one thing it did
  not name was that the stack would need +2 pages rather than fitting 48 KB,
  which is why it said measure, not reason. Full detail in the step 2 note
  above.
- **Step 2 sized 2026-09-21**, before any of it was written: the eight
  findings and the estimate are in the dated note under step 2 above, the one
  place they live.
- **Steps 0 and 1 landed 2026-09-21** (the kernel arm and the parked one-shot
  path). Measured: `make test-async-rmount` 3 of 3 (served, identity,
  concurrent), and both controls fail as they must: on a kernel that resolves
  the identity to its SLOT the second `cat` prints the first file's bytes
  (`line 000: hello from the host`), and on `main`'s image the silent peer's
  `NO_FS` comes BEFORE the live read (the loop was blocked for the wait). The
  identity check needed its window measured too: at a 1.5 s hold the reply
  landed before the second caller existed and the check passed on the
  slot-resolving kernel; the hold is 3.5 s and the deadline 5 s now. What
  they found: **the stack cost is where the compiler puts it, not
  where the code reads.** The first build overflowed netd's guard page on the
  very first parked request, at the top level, in `handle_rmount`'s own
  prologue: with `oneshot_rmount` gone, `session_rmount` had exactly one
  caller and was inlined, so `handle_rmount`'s frame carried the session
  path's ~5 KB of buffers on every remote mount, the parked one included.
  Measured, not reasoned: the fault's `elr` symbolized to `handle_rmount +
  0x2c`, the stack probe after a `sub sp, sp, #0x1660`. `#[inline(never)]` on
  both branches put each set of buffers back in its own frame (`handle_rmount`
  336 bytes, `park_rmount` 272, `session_rmount` 5712). The lesson the
  `oneshot_rmount` comment had already stated ("each branch holds its own big
  buffers in its OWN frame") was true because of a property the comment did
  not name - two callers - and deleting one of them silently revoked it; the
  attribute names it now. Also found: my first "smoke test" of the new code
  booted a stale `build/esp.img` (`make esp` stages the tree, `make image`
  builds the image) and passed on the OLD netd; the rig's own freshness check
  is what caught the real fault. A passing run of an image you did not just
  build is a claim about the previous build.
- **The re-entrant park was over-reach, reverted the same day.** The first cut
  of step 1 also parked a path verb arriving at the RE-ENTRANT drain (inside a
  `cpu` run), to close the latent overflow #147 named. The single-VM rig passed
  (its mounts are top-level), but the two-node rig faulted netd at `tcp_run`'s
  own prologue on the run path: the park still calls `frame_signed` (a deep
  Ed25519 build), and servicing the remote table from inside `tcp_run` added
  ~2.8 KB, both on a frame already at the guard page. Reverted to #147's
  by-sender refusal for the re-entrant drain. The finding is the plan's own
  premise, now measured rather than argued: **the signature is the deep cost,
  and it is exactly what makes the re-entrant drain unaffordable** - so the
  latent overflow closes at step 3 (remove the drain), not at step 1. Step 1's
  win is real and top-level: a mount no longer blocks the loop while its reply
  is in flight, which the concurrent check proves.
- **The review of #149 found a real hang and a chained-cpu stall, both fixed.**
  (1) A parked mount to an UNREACHABLE peer was reaped by `pump_dials` the
  moment its SYN retries exhausted, in the same pass that set it Closed, so the
  `Parked` record was destroyed before `service_remotes` could send `NO_FS` -
  the caller hung in its `MSG_CALL` forever. `REMOTE_DEADLINE_TICKS` above the
  retransmit budget is exactly what let the reap win. Fixed by guarding the
  reap with `parked.is_none()`, so a parked slot is `service_remotes`'s to free
  (correct by construction: it removes the sole place a parked slot is freed
  without answering its caller). The async rig gained a fourth check, a
  connection REFUSED by the authorized host on a dead port: it parks, the host
  RSTs, and the caller gets `NO_FS` with the prompt back. That check does not
  reproduce the reap race itself, which needs a peer answering ARP but DROPPING
  the SYN so `Connecting` exhausts inside `pump_dials`; SLIRP RSTs a dead port,
  and an in-subnet down IP fails ARP synchronously (a correct `NO_FS` by another
  path). The SYN-drop control is a live L2 node refusing a dead port plus a
  mutation to confirm the hang without the guard; described here, not automated,
  the same honesty the `tcp_get` coalesced-FIN arm carries. (2) The chained-cpu
  stall: a cpu child's Phase 4b `/host` request reaching the re-entrant drain
  on a node both running a cpu AND hosting a child took the child arm, bypassed
  the by-sender refusal, and parked on a slot `tcp_run` does not drive -
  undriven, hanging the child. Now refused `FS_ERR_BUSY` when re-entrant, like a
  bystander; the normal Phase 4b child (top-level, on a node not itself in a
  run) still parks and is driven. Two comments that claimed the re-entrant drain
  parks a path verb were corrected: it refuses. (3) A low-probability src-port
  collision was noted and, that round, accepted; the second review below showed
  it real and fixed it.
- **The second review of #149 found four more, all fixed.** (1) `next_src_port`
  was only unique across calls a round trip apart, an invariant the async park
  breaks: two mounts parked back-to-back to the same peer could draw the same
  ephemeral port and cross each other's replies (~1/12288 per pair, but real).
  Fixed by giving each of the two remote slots a disjoint half of the ephemeral
  window by slot index, so two live remote slots never share a 4-tuple; a
  compile-time assert pins `MAX_REMOTE == 2`, which the split assumes.
  (2) A top-level park could be starved by a re-entrant cpu run: its SYN was
  never sent (tcp_run does not pump remotes) yet its deadline clock ran, so a
  reachable peer was reported `NO_FS` when the run returned past 5 s. Fixed by
  starting the deadline at first SYN SEND, not at park; a starved park does not
  age. A park whose SYN was already sent and is then starved by a long run can
  still age; that residual closes at step 3. (3) `--delay` with no value crashed
  the host peer with IndexError; it errors cleanly now. (4) A spent remote slot
  (reply delivered, parked cleared) was not in the NET_WAIT wakeup set, so it
  could linger until an unrelated event; Closed remotes are in the poll set now.
- **A third review of #149 found no bug**, and named the step-1/step-2
  boundary precisely: a fid-verb `NETOP_RMOUNT` still runs `session_rmount`
  synchronously on the serve frame, so while it does its round trip a
  concurrently parked path-verb mount's SYN, retransmits and reply are not
  serviced. A cross-client latency, not a correctness break, and exactly what
  step 2 removes by moving the held session onto the same parked engine.
  Recorded here so it is a known boundary, not a surprise. Two small things
  also fixed: a stale in-function comment (a leftover `0x3000` where the code
  uses `REMOTE_PORT_SPAN` 0x1800), and `Parked.since`'s zero sentinel made
  sound against a `GET_TICKS` value of 0 (clamped to >= 1 at first SYN send).

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

**Step 3: the run.** `tcp_run` and the re-entrant drain go. Checks: every
`test-reentrant-session.sh` recipe is *served*; `cpu` output past one message
still pulls through `NETOP_RUN_MORE`; the supervisor's ping is acked during a
run with no `beat_if_new_tick` in the way.

## Deliberately not in scope

- **Session-scoped authentication.** Its own plan, after this. The held
  session step 2 produces is where it will live.
- **Async ARP, DNS, ICMP, HTTP fetch.** Decision 4.
- **Raising any budget.** `MAX_REMOTE` starts at 2, the smallest number that
  lets a bystander's read proceed while a child's is parked; raise it when a
  rig exhausts it.

## Ledger

Kept as the steps land, newest first.

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

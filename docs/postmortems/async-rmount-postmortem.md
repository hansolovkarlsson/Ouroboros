# What blocking was doing

*A design and bug retrospective, the thirtieth, covering 2026-09-21 and
2026-09-22: the async remote mount, planned in
[`roadmap-async-rmount.md`](../roadmap/roadmap-async-rmount.md) and carried
through all four of its steps in two days (#149, #152, #154). Before it, `netd`
blocked its whole event loop for every remote round trip. After it, no remote
mount, fid verb or `cpu` run does. Three PRs, five review rounds, and a stack
that grew four pages and would not give any back.*

The plan was good, and the sizing of step 2 held finding for finding. What the
plan could not see was the other half of every deletion. A blocking round trip
is slow, and it is also a lock: while `tcp_run` or `session_rmount` held the
loop, nothing else could start, overlap, age or reap. Code all over `netd` had
come to rely on that without saying so, and each step that removed a blocking
path removed those guarantees with it.

> **A BLOCKING CALL IS A LOCK NOBODY WROTE DOWN.** Taking it out is a
> concurrency change, and every invariant that rested on "nothing else runs
> meanwhile" is now false, silently, in code the diff never touched. Before
> deleting a wait, list what the wait was serializing, and give each item an
> explicit guard or a reason it no longer matters.

Most of the arc's review findings are that sentence. So is the one limit the
plan did anticipate (two fid verbs overlapping on one session, refused
`FS_ERR_BUSY` since step 2), which is the proof that the question can be asked
ahead of time: the sizing asked it once, for one resource, and got it right.

---

## The arc

| step | PR | what moved off the blocking path | lines |
|---|---|---|---|
| 0 | #149 | the kernel arm: `MSG_SEND` to a packed task identity, refused once the slot is recycled, so a late reply cannot complete a stranger's call | |
| 1 | #149 | a top-level path verb parks on the `/net/tcp` engine, made generic | |
| 2 | #152 | the held fid session becomes a parked `SessionConn`, with a two-phase open | 462 out, 339 in |
| 3 | #154 | the `cpu` run parks; `tcp_run`, the re-entrant drain and #147's refusal are deleted | 266 out, 144 in |

ARP, DNS, ICMP and the HTTP fetch still block, on purpose (the plan's Decision
4). The arc shipped in v0.20.0.

## 1. The guards nobody wrote down

Each row is a review finding, the job the blocking path had been doing, and
the explicit guard that now does it.

| found by | the defect | what blocking had provided | the guard now |
|---|---|---|---|
| review 1 of #149 | a parked mount to an unreachable peer was reaped by `pump_dials` in the same pass that marked it dead, so its caller hung in `MSG_CALL` forever | there was no parked state for a reaper to destroy | the reap skips a slot with `parked` set; `service_remotes` is the one place a parked slot is freed |
| review 2 of #149 | two mounts to one peer could draw the same source port and cross replies (about 1 in 12,288 per pair) | `next_src_port` was unique across calls a full round trip apart, because calls could not overlap | each remote slot owns a disjoint slice of the ephemeral window |
| review 2 of #149 | a park starved by a `cpu` run aged to its deadline before its SYN went out, and a reachable peer was reported `NO_FS` | parking and sending were one step, so the clock and the wire started together | the deadline starts at the first SYN send |
| review of #152 | the same defect on the held session's `Established` branch: a verb aged its full five seconds unsent, and the failure destroyed a live session's fids | as above | "the clock starts when bytes go out", now one rule for both tables and both phases |
| review of #152 | the idle backstop dropped a session without a FIN, leaving the far export holding its fids | the deleted `reap_client_sessions` closed politely, synchronously | an idle slot goes `Closing`, sends its FIN, then reaps |
| review of #154 | two concurrent runs shared the one `PendingRun` buffer: A was answered empty and B got A's bytes in front of its own | `tcp_run` held the loop for the whole first run, and #147 turned a second client away | a run is refused while one is in flight, before `pending` is touched |
| review of #154 | a run whose peer was never reached answered with an empty success, not a failure | `tcp_run`'s own completion rule was `got > 0 \|\| fin` | completion tests `peer_fin`, not `finished` |
| review of #154 | a run plus one path verb filled `MAX_REMOTE` and the next verb was refused busy, indistinguishable from the refusal step 3 had deleted | a blocking run held no slot at all | `MAX_REMOTE` 2 to 3 |

Eight findings, one shape. None was careless code: each guard that was
missing had never existed as code, because the blocking call made it
unnecessary. That is also why neither the compiler nor the rigs found them.
The rigs drive one caller at a time unless a check is written to overlap two,
and a check for overlap is only written by someone who already suspects it.

**The deadline defect came back one step later.** Review 2 of #149 fixed it
for the SYN branch of the path table. Step 2 wrote a new send branch for a new
table and stamped the deadline at park time again, and review found it again.
The first fix was the size of the bug, which is right
([`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md)),
but the bug was an instance of a rule, and the fix recorded the instance.
When a fix is really a rule, the rule is what has to be written down, next to
every branch it governs, or the next branch is written without it. It is now:
every park site says the clock starts at the send, and both send branches in
`pump_dials` stamp it.

**The question for next time is cheap.** Before a step deletes a wait, list
what can now happen during it that could not before: a second caller, a
reaper, a timer, a recycled slot, a reused port. Step 0 is the arc's one
design decision that did ask this at plan time (a parked reply addressed by
slot could reach whoever holds the slot later, so the kernel arm refuses a
recycled identity), and it is the one class of defect the reviews never
found.

## 2. The "latent" overflow was live

#147 refused a remote mount arriving at the re-entrant drain inside a `cpu`
run, because that path overflowed `netd`'s guard page. It noted that the cpu
child's own Phase 4b `/host` reads take the same deep path and are not
refused, since they are the run, and called that overflow **latent**. The
async plan repeated the word and scheduled the fix for step 3.

At step 3 I went to verify the fix and ran the control on `main` first.
`cpu <A> ls /host` and `cpu <A> cat /host/HELLO.TXT` both faulted `netd` at
the guard page and the supervisor restarted it, every time. Phase 4b remote
file access, the headline of the cpu model, was broken on `main`, and nothing
had reported it because no rig ran a `/host` read. On the step-3 branch both
return the right bytes.

"Latent" is a claim of the kind
[`true-when-written-postmortem.md`](true-when-written-postmortem.md) added on
the same day: a claim about reachability is a claim about what does not need
testing, and it seals itself, because it is the reason the check is never
written. Whether it was ever latent nobody can say now, because nobody ran
it. The single most valuable result of the arc came from refusing to assert
that step 3 closed the overflow and measuring the before instead.

## 3. The stack went from depth to residency

`STACK_PAGES` went 10 to 12 at step 1 and 12 to 14 at step 2, and step 3 was
planned to hand two pages back. Every one of these was settled by a boot, and
twice the boot contradicted the reasoning.

- **Step 1: the compiler moved the buffers.** With `oneshot_rmount` gone,
  `session_rmount` had one caller and was inlined, so `handle_rmount`'s frame
  carried the session path's buffers on every mount (`sub sp, sp, #0x1660`,
  faulting at `handle_rmount + 0x2c`). `#[inline(never)]` on both branches put
  them back. The comment that said each branch holds its buffers in its own
  frame had been true because there were two callers, and deleting one revoked
  it.
- **Step 1: the re-entrant park was over-reach.** Parking at the re-entrant
  drain too passed the single-VM rig, whose mounts are top level, and faulted
  the two-node rig at `tcp_run`'s prologue. The park still signs, and an
  Ed25519 frame build is exactly the deep cost the re-entrant drain could not
  afford. Reverted the same day. The plan's premise, that the signature is the
  deep cost, became a measurement.
- **Step 2: the cost moved from a frame to a table.** A `SessionConn` is 2,896
  bytes of resident buffers where `ClientSession` held none. Three of them on
  `serve`'s frame sit under every chain, and the two-node rig faulted 704 bytes
  into the guard page at 48 KB.
- **Step 3: deleting the deepest function did not make the deepest chain
  shallower.** `tcp_run`'s frame left the tree and `netd` still faulted at
  48 KB, 1,936 bytes into the guard, beneath `park_run`. The stack is now
  dominated by resident tables, which every chain stands on, not by call depth.
  `#[inline(never)]` on `park_run` made it worse (3,072 bytes), because
  inlining had let the compiler overlap its buffers with sibling branches whose
  lifetimes are disjoint. So the attribute is about sibling branches that would
  otherwise share one frame, not about depth, and the reason for fourteen pages
  is recorded beside the constant.

The pattern: this is the same lock-shaped change seen in memory. A blocking
path's state lived in a transient frame because it only existed while the loop
was held. A parked request outlives the handler, so its state has to be
resident, and resident state is paid by every chain for the whole life of the
server.

## 4. Where the defects were found

The rigs were good, and every check in them had a measured control. They
caught the stack overflows, a `resolve` recipe that could not succeed on a
link with no DNS, and, through the async rig's freshness guard, a first "smoke
test" that had booted a stale `build/esp.img` and passed on the old `netd`.

But every defect my own changes introduced came back through review, not a
rig: the panic on a 16-byte message in `park_rmount`, the two runs sharing a
buffer, the deadline started before the send, and a guard whose comment
described a refusal the code did not perform. Three of the four were in code
written that same hour. Section 1 explains the ratio: a rig proves what it was
built to exercise, and the new defects lived in interleavings nobody had built
a rig for yet. Review reads for interleavings; a rig has to be told them.

The one structural lesson about rigs: **a single-node pass is not evidence
about the two-node path.** The single-VM async rig passed the re-entrant park
because its mounts are all top level. Only the two-node rig reaches the run
path, so a change to anything the run path stands on needs the two-node rig,
whatever the single-node one says.

## What to carry forward

- **Before deleting a wait, list what it serialized** (callers, reapers,
  timers, recycled slots, reused ports) and guard each explicitly. Section 1 is
  eight findings of not doing this.
- **When a fix is an instance of a rule, write the rule** where every branch
  it governs can see it.
- **"Latent" and "unreachable" are proposals not to test.** Run the control
  on the tree that is supposed to have the problem.
- **Parked state is resident state.** Budget it against every chain, and
  measure the stack by booting, on the rig that reaches the deepest path.

The next arc is session-scoped authentication
([`roadmap-session-auth.md`](../roadmap/roadmap-session-auth.md)), and it is
the mirror image of this one. Per-request signing is stateless: every message
stands alone, so nothing can go stale, replay out of order or outlive a
reboot. A session key and a strict `seq` add exactly that state. The question
this arc learned to ask about blocking applies unchanged to statelessness:
list what it was providing for free before replacing it.

## The deepest thing standing on the tables (2026-09-23)

*Added the morning after this was written, from step 1 of
[`roadmap-session-auth.md`](../roadmap/roadmap-session-auth.md).*

Section 3 ended on resident state being paid by every chain. The next day's
stack gate measured where that bill had landed. Every park (`park_session`,
`park_rmount`, `park_run`) is reached from `drain_client_messages`, which the
export's connection pump calls from inside its own loop, about 10 KB below
`service_remotes`. The parks signed their requests there, and on the two-node
rig the sign in `park_session` ran within **80 bytes** of the guard page on every
fid verb; a 512-byte pad at that depth faulted `netd`. The rigs passed only
because nothing had yet added those bytes. Nothing in this arc's reviews or
rigs had looked, because no check measured headroom at a call site: the
overflows this postmortem records were all found by a fault, and this one had
not faulted yet.

The fix was this arc's own pattern made a rule rather than an instance: step 2's
two-phase park, where the held verb is signed later in the service pass, became
"a park never signs" for every park (#159), and the signing site moved to
`sign_parked`, with 10 KB of room. Section 1's lesson in a different currency: a
blocking design had kept the signing shallow for free, because the call and the
sign happened in the handler, and parking moved the handler under the pump.


## The mirror of step 1's inlining, and the bill for sessions (2026-09-26)

*Added from steps 6 and 7 of
[`roadmap-session-auth.md`](../roadmap/roadmap-session-auth.md), the keyed
export and client.*

Section 3's first entry was a caller deleted: `session_rmount` became the only
caller of its path, was inlined, and carried the other path's buffers on every
mount. Step 6 was the same fact the other way round. The keyed dispatch added a
second caller of `build_9p_reply`, which had been inlined into `handle_9p`'s host
frame with its locals sharing that frame's slots; with two callers it got a
frame of its own, and **every export verb, signed ones included, peaked 1,760
bytes deeper than on `main`**, on a path the change was not meant to touch. And
inside that total, 1,320 bytes were `log`'s console buffers inlined into the
keyed dispatch for refusal lines that almost never print, which two reasoned
guesses (the HMAC state, the call site) missed and the disassembly showed at
once. The fix restored one call site for both formats and moved the refusals to
a cold function. The rule this adds to section 3's: the number of callers is
part of every frame's size, in both directions, and a change that adds one is a
stack change even when it adds no code to the deep path.

Section 3's other lesson, residency, came back as the bill it predicted. Step 7
measured the client's deepest point, which is still the park path, at 3,840
bytes of headroom on `main`; the keyed client costs exactly 3 × 112 bytes of
it, the resident key state of three session slots, and nothing else, since its
crypto runs from the service pass ~10 KB higher. The state was 144 bytes a slot
until a review made the handshake's secret and the session's keys one enum,
since they are never live together. Resident bytes are paid by every chain, so
they were counted, then shrunk, then counted again.

# Ouroboros development journal

A chronological dev-log — what was worked on each day and why, in narrative
form. For the condensed milestone record see [`CHANGELOG.md`](CHANGELOG.md); for
the deeper design-and-bugs retrospectives see the postmortems under `docs/`;
for the forward plan see [`ROADMAP.md`](ROADMAP.md).

---

## 2026-09-06 (cont. 3) — a grant that was only ever made once

*(The second ledger finding: nothing re-grants `TO_NET` after a supervised
`netd` restart.)*

**The shape, before touching anything.** A spawned program holds no static
right to reach `netd`; the shell delegates it once, in `spawn_path`, and that
is the only `DELEGATE` aimed at `netd` in the tree. When `netd` faults or
wedges, the teardown calls `clear_delegate` on its slot, which strips that bit
from every live task's set — the right thing for a spawnable slot, whose next
occupant may be anything, and then the supervisor reinstalls `netd` into the
same slot. So every program alive across the restart is netless for life, and
its next request fails the way #107's missing grant did: a 150-tick spin, then
`request failed`. Three fixes were on offer — re-grant from the supervisor
(the kernel would have to know the shell's policy), re-grant from the shell
(which cannot learn a restart happened), or stop stripping grants aimed at a
protected slot, since `SPAWN` can never fill one and the only thing that ever
comes back there is the same server. Hans chose the third.

**Building the check first, because nothing in the tree can force a restart.**
Two temporary mutations: a resolve of the name `wedgeme` parks `netd` in a
spin, so the passive heartbeat restarts it about 2.6 s later; and `ping` makes
eight requests 1.5 s apart instead of one, so it is alive across the restart.
`exec` the ping, trigger the wedge on its first reply. Against the unmodified
kernel: *"server slot 4 restarted"*, then six failures out of six. That is
the check that can fail. The fix is four lines — `clear_delegate` returns
before the strip loop when the dead slot is below `FIRST_SPAWNABLE` — and the
same run then shows one in-flight failure and replies resuming. Mutations
reverted, the network controls (`ping`, `ping | wc`, a now-harmless `resolve
wedgeme`) unchanged, suite green. The recipe, mutations included, is in
`testing-qemu.md`, since a future reader of the ledger line "measured" should
be able to measure it again.

**One rig lesson, small and reusable.** The first attempt never typed the
trigger: `exec` prints the prompt *before* the child's first line, so a step
waiting for `# ` after it sees nothing new and times out. Key the next step on
the child's output instead. And a doc-comment wart fixed in passing: yesterday's
insertion of `clear_delegations_of` had landed between `clear_delegate`'s doc
and `clear_delegate`, so the doc described the wrong function.

---

## 2026-09-06 (cont. 2) — the wrong slot, and a bug you have to build a window to reach

*(The first of the five pre-existing ledger findings from the delegation
review: the pipeline's "stay quiet" heuristic reads the consumer's state when
the kernel may have denied on the producer's.)*

**First move: run it, before reading it.** The ledger said a producer that
exited early gets `pipe: could not authorize the stream` printed on top of its
own message, and the standup called it the most user-visible of the five. On
the unmodified tree, `ls /nope | wc` and `cat /nope | wc` printed the
producer's line, `0 0 0`, and `pipe: ls exited with code 1` — quiet, and
correct. The heuristic *does* read the wrong slot; the case the reading
described simply never arrives that way.

**Why not, from the code.** The first stage is spawned last, so the shell
delegates it before it has run an instruction. A producer that *has* run and
failed does not exit either: `ulib::end_of_stream` treats a denial as
transient and retries it for 150 ticks, so it sits alive until the grant lands.
What is left is a producer that exits with **no** end-of-stream (`-?` does
exactly that: print usage, `exit(0)`), running to its exit in the gap between
two adjacent shell syscalls — a tick landing in a window of a few instructions.
Real, and rare enough that no recipe would ever see it.

**So the window was built.** A temporary loop parked the shell after the spawn
loop until stage 0 was a zombie. Under it, *every* pipeline printed the line —
`ls -? | wc`, `ls /nope | wc`, even `ls / | wc` once its producer gave up on
the stream after 150 ticks. That is the check that can fail
(`cluster-keys-postmortem.md`): a fix verified only on the unmodified tree
would have been verified against runs that never reached the branch. The fix
reads both ends of the link; same window, same three commands, no line. Window
removed, controls byte-identical to the morning's run, 137 tests, 62 binaries
with no `ABS64`. Nineteen lines in, four out, most of them the comment saying
how narrow the path is and how it was reached.

**The finding beside it.** Using `-?` as the exit-without-EOF producer showed
that `ls -? | wc` on the *unmodified* shell never returns a prompt: `wc` waits
in `pipe_recv` for an end-of-stream nobody sends, and the shell waits on `wc`.
Ctrl+C then holds the slots, which is the deliberate hold from yesterday. On
the ledger, not fixed — the instance is one call in `usage_if_requested`, but
the class is "a task exited with a non-console stdout target and no one ended
the stream", which the kernel could close for every program at once. That is a
design choice, so it is written down for Hans rather than made.

**Then the review moved the fix a layer down.** `/code-review high` on the
diff: the kernel folds "that slot is dead" and "you may not" into one
`MSG_ERR_DENIED`, so any `TASK_STATE` read after the fact is a guess — a policy
refusal on a stage that died between the two syscalls reads as "already
exited", and a nested shell (whose static mask holds no spawnable slot) is
refused on every link, so under the window the shell-side fix would have
silenced the one message that was true. Every sibling syscall — `KILL`, `FG`,
`WAIT`, `MSG_SEND`, `MSG_CALL` — already answers `TASK_ERR_NO_SUCH_TASK` for a
dead slot. So `DELEGATE` does too now, the shell reads the reason from the
answer, and the closure, the second and third syscalls, the TOCTOU and the
"NOT merely not-runnable" caveat are all gone. The reason is atomic with the
denial only if the kernel gives it — the same shape as the `ls` error table
that rendered every `fsd` code as "no such file": the caller had flattened a
reason the layer below knew. Same window, same commands: quiet. The nested
shell under the window is quiet too, and that is also right: there the exit
*precedes* the syscall, so the kernel answers "no such task"; the case the
review named — refused first, then the stage dies before the shell's state
read — is the one no read after the fact can get right, and the one the
atomic answer removes. And without the window, the nested shell answers
`pipe: could not authorize the stream` to `ls / | wc` — it cannot run a
pipeline at all, which the ledger had recorded only as "cannot delegate
`TO_NET`". Widened there, measured.

**And it found what I had written beside the finding.** "Ctrl+C then holds
the slots" was inferred from yesterday's hold, not run — the rig cannot send
Ctrl+C, and for a console-sink pipeline the consumer owns the keyboard, so
Ctrl+C kills it and the shell recovers. Struck. And `-?` was "the instance"
only because it was the one I ran: twenty-six of fifty-three programs never
call `end_of_stream`, and the ones that do skip it on their early error exits.
The class fix I had written down in one line has three open questions the
review listed, all now on the ledger. Six wrong-or-narrow claims in the prose
around a fix whose code was right twice — yesterday's ratio, again.

**A second pass, `medium`, on the kernel version — because the code on the
branch was no longer the code the first review had read.** Six findings, one
of them a gap the fix had opened: the "already told the user" premise is false
for a program that exits with *no* output, so under the window `touch f | wc`
went from a wrong line to a bare prompt, and a non-zero exit from a silent
producer would have vanished with it — `kill_and_reap` discards the `WAIT`
status. The teardown on that path now `KILL`s each stage and, where the kill
is refused because the stage already exited, reaps it through
`wait_pipe_stage`: silent for exit 0, and `pipe: ls exited with code 1` where
the first version printed nothing. Forced window again: `touch /T1 | wc` gives
a bare prompt, `ls /nope | wc` its message and its exit code. Beside it: the
`architecture.md` syscall table and `delegate_net`'s doc both still described
`DELEGATE` as answering only `MSG_ERR_DENIED` — the falsifying edit was in the
kernel, the claim in two other files, the `true-when-written` shape exactly —
and the arm answered out-of-range by *argument position*, `DENIED` for the
grantee and `NO_SUCH_TASK` for the target. One rule now, stated in the ABI doc
with its precedence. Left alone from that pass: `kill_and_reap`'s unconditional
`WAIT` after a successful `KILL` is a wasted trap and reopens the slot-reuse
window its own doc records; behaviour-preserving to fix, but the helper landed
yesterday with a review of its own, and this branch is about the link.

**Measured later the same day: the narrowing was half wrong.** Running the
review's suggested `touch f | wc` on the unmodified shell reached the dead-link
path on the first attempt — `touch` exited before the link was authorized — and
wedged the shell on the second, when it exited after. A producer that neither
prints nor ends its stream does not hand the shell its turn back: `SPAWN` is
the shell's longest syscall, a tick is pending at its `eret`, and the child
gets the slice. The morning's "a window of a few instructions" was true for
`ls` and `cat`, which block on `cond` before they exit, and a reading for
everything else. So the original line was reachable all along by any silent
producer, the ledger now says so, and the next change is written down beside
it: when the producer is the dead end, the shell can end the stream on its
behalf — it holds the send right and nothing was delivered — so `touch f | wc`
prints `0 0 0`. The site's architecture page was re-read against the changed
`architecture.md` and does not abridge the syscall table's return codes, so it
was re-stamped, not rewritten.

**What this one says.** A finding made by reading code is a hypothesis about a
run, and the consequence attached to it is the part most likely to be wrong —
the code was read correctly, and the sentence after it ("a producer that
exited early gets…") described a run that does not happen. Yesterday's lesson
from the other direction: a stored measurement is a hypothesis for the next
run; a stored *reading* was never a measurement at all. The work today was not
the five-line fix but building the window that let the check fail, and the
window found a second bug the reading could not have — and the review then
found that half of what I wrote about that second bug was itself a reading.

---

## 2026-09-06 (cont.) — a second review, and the reason beside a right decision was wrong

*(The slot-leak fix went through `/code-review high` before merging. It found no
defect in the code and four in what the code said about itself.)*

The review traced the kernel side independently and confirmed the two claims
`kill_and_reap` rests on: `KILL` refuses anything not `Runnable`/`Blocked`
while `kill_task` frees a live slot outright, and `WAIT` never blocks on an
`Unused`, out-of-range or protected slot. So the helper cannot deadlock at any
of its eight sites, and the `WAIT` removed from the redirect-overflow block was
safely covered on both routes out of `capture_program_output`. The fix was
right. Every finding was about the prose beside it.

**The justification I had written for a deliberate choice was backwards.** The
redirect-overflow block kills its producers with a bare `KILL` rather than
`kill_and_reap`, and my comment said this was so the `wait_pipe_stage` loop
below could report how each ended — that reaping first "would turn every report
into *did not exit cleanly*". But a *successful* bare `KILL` frees the slot to
`Unused`, so that loop gets `TASK_ERR_NO_SUCH_TASK` — which sits above
`FS_ERR_MIN` — and prints *did not exit cleanly* regardless. The bare `KILL` is
still the right call, for the case I had not named: a producer that already
exited on its own is a zombie, the kill fails, and the loop's `WAIT` reaps its
**real** exit code. And that is the common case, because `ulib::pipe_out`
fast-fails the moment its consumer is gone, so producers here mostly die by
themselves. Right decision, wrong reason written down — and a wrong reason
attached to a right decision is worse than no reason, because the next reader
who checks it finds it false and reasonably concludes the decision is too.

**The claim I had flagged as argued-not-tested was, in fact, too strong.**
"`WAIT` cannot block here" is unconditional; the two syscalls are separate trips
through EL0 with the shell preemptible between them, and `netd` spawns its own
remote-exec child into the lowest free slot — so a `cpu` request arriving in
that gap takes the slot a successful `KILL` just freed, and the `WAIT` blocks on
a stranger. Narrow enough to accept, and now written as "does not block on the
task it was called for", with the window named. The same window was a
*guaranteed* occurrence on one path, because `pipe_send` killed and reaped
`slots[0]` and then its caller did it again; `pipe_send` now leaves teardown to
its one caller.

**And "the completion paths never leaked" holds only absent Ctrl+C.**
`wait_pipe_stage` treats `WAIT_INTERRUPTED` as something to report — *it may
still be running, see `ps`* — and moves on without reaping. Left deliberately:
the task may be alive, and reaping a live task behind the user's back is worse
than a held slot that names the tool to release it. Recorded on the roadmap's
ledger as a design question — what should Ctrl+C *mean* for a pipeline — rather
than as a repair waiting to happen.

Two reviews in one day, then, and the shape of what they found moved the same
way it did across the three rounds on 09-05: the first found a real security
regression in my own diff; the second found none in the code and four in the
claims. Not exercised by either, and said so in the commit: `pipe_send`'s
timeout branch, which needs a `/bin` program that stays alive and never reads
its input, and none exists.

---

## 2026-09-06 — the remote-read bug was never the wedge timer, and the citation was the problem

*(One fix, two files, and a correction to a record that had the wrong cause
written down with a measured table beside it.)*

The day's task was "the `WEDGE_TICKS` remote-read bug": remote `cat` of
anything over ~2 KB was recorded as broken because `netd` stays continuously
`Runnable` across a multi-chunk read, and any read past 2.56 s gets the server
restarted mid-transfer. The record was confident and it was wrong.

**The first run disproved it.** Reproducing it against the host peer showed
`cat: failed` exactly as documented — and **no `server slot 4 wedged` line
anywhere in the boot**, which is the signature that failure announces itself
with. Zero aborts in QEMU's own trace. The 09-03 heartbeat fix was doing its
job; nothing had been restarted.

**Then the discriminator turned out to be the pipe, not the size.** With the
host peer instrumented to log every accept and every request:

| command | round trips | before | after |
| --- | --- | --- | --- |
| `cat /mnt/a/BIG.TXT` (13,600 B) | 28, ~17 s | **works** | works |
| `cat /mnt/a/HELLO.TXT \| wc` (1,960 B) | 5, ~3 s | `cat: failed` | `40 400 1960` |
| `ping 10.0.2.2` | — | works | works |
| `ping 10.0.2.2 \| wc` | — | `ping: request failed` | `1 3 21` |

A 17-second read succeeding while a 3-second piped one fails is not something
a threshold can produce. And the host logged **not one request** on a failing
run — the request never left the guest.

**The cause, and why it needed a kernel change to fix.** The shell delegates
`TO_NET` to a spawned command in `run_found_command`; `run_head_pipeline` never
did, and neither did `cmd_exec`. So `cat` in a pipeline was denied by the
capability check, `MSG_ERR_DENIED` surfaced through the generic arm as
`FS_ERROR`, and `cat` printed "cat: failed". That is the *same* chain
`ulib::net_msg_call` was built to absorb in September — but its retry is for a
denial that is transient by construction, and here the grant simply never came.
**Every network-using program was broken inside a pipeline**, not just remote
mounts.

It could not be fixed in the shell alone. `tasks.rs`'s `DELEGATED_SEND` held
**one** delegated target per task, so a pipeline stage could hold the pipe
delegation *or* `TO_NET` and the second grant silently revoked the first. It is
a bitmask now, in the same `1 << slot` shape `caps_for_slot` already uses.
Nothing is widened: every bit still needs a delegator that statically holds it.
What is gone is the accidental revocation — and `clear_delegate` now clears one
bit rather than a whole set, or a stage exiting would strip its siblings' grant.

### What the review then found, which was more than the fix

`/code-review max` returned fifteen findings against a thirty-line diff. Five
were acted on here; the rest are pre-existing and are now recorded rather than
quietly carried.

**The one that mattered was a security regression I introduced.** `DELEGATE`
validated the *target* slot but never the **grantee**, so any `/bin` program
could grant a *server* a send right — `DELEGATE(grantee=cond, target=shell)`
passed every check, and `cond`'s mask is deliberately empty because it only
ever replies. That was self-limiting purely by accident: with one slot, each
injection overwrote the last. Making it a set removed the accident and nothing
replaced it, and servers never exit so `clear_delegate` never ran. The grantee
must now be a spawnable slot; both real delegators only ever grant to a slot
they just spawned, so it costs nothing legitimate. **A widening in one place
turned an accidental bound into a real hole somewhere else** — and the diff
that opened it did not touch the file it opened it in.

The rest, briefly: `delegate_net` moved into `spawn_path`, the one function all
five spawn sites already go through, so the sixth cannot omit it (the fix
otherwise repeated the same line five times and trusted the next author);
`DELEGATED_SEND` is zeroed where a slot becomes live, so freshness stops
depending on every teardown path remembering; the `NUM_TASKS <= 16` ceiling
that `caps_for_slot`'s `u32` has always needed is now a `const` assert instead
of a comment, which matters more once two arms shift the same bit index in two
widths.

And it caught four wrong claims in the prose I had just written to correct
wrong claims: the ROADMAP contradicted itself two hundred lines down (item 4
still described the retired single-slot model), I wrote "the non-pipeline spawn
path only" when `cmd_exec` is a non-pipeline path that *also* lacked the grant,
I cited the wrong frontier item, and my before/after table recorded `cat:
failed` while dropping the `0 0 0` — the exact half of the signature the 09-05
entry had kept and I had faulted it for reading alone. The slot map inside
`tasks.rs`, which `CLAUDE.md` names as **the authority** on slot numbers, was
itself wrong in two places: `5..10 spawnable` when `FIRST_SPAWNABLE` is 6 and
slot 5 is `accountd`.

**Nine documents still asserted the single-slot model**, including
`architecture.md`, the reference `CLAUDE.md` names for the privilege model.
`make check-site` went red the moment `architecture.md` changed — which is the
09-04 site-freshness work doing exactly its job, one day after it landed.

### What this was really about

Every hard part of the day was in the record, not the code. The fix is thirty
lines and the diagnosis took four runs.

**Matching a symptom to a stored cause is a guess wearing a citation.** The
09-05 entry did what this project recommends — checked a "new" bug against a
documented, measured analysis rather than guessing fresh, and explicitly noted
that it had nearly overwritten the analysis with a guess. It still landed on
the wrong cause, because it matched on the *symptom* (a remote read fails) and
never re-checked the analysis's **signature**. `WEDGE_TICKS` prints
`server slot 4 wedged`; that line was absent from every failing run, and
nobody looked. A stored analysis is evidence for the run it was measured on,
and a hypothesis for every run after.

**The counter-example was in hand and read as confirmation.** The 09-05 entry
records `cat /mnt/a/BIG.TXT` — the *unpiped* form — as failing. It does not,
and did not. The one observation that would have collapsed the timing theory
in a minute was written down as supporting it.

**A fixture chosen to be far past a threshold is only a better probe if the
threshold is the cause.** `BIG.TXT` was added on 09-05 specifically because
`HELLO.TXT` sat right *on* the 2.56 s line — sound reasoning that made the
fixture strictly worse here, since the bigger file exercises the working path
harder while the smaller piped one was the actual bug.

**And one gap the fix did not have to find: testing one spawn path is not
testing the others.** The delegation race fixed on 09-03 was proved with a
forced-race scaffold and a three-row table — all of it running unpiped
commands, on the one spawn path that had the grant. Every recipe in
`testing-qemu.md` did the same, which is why months of green said nothing about
this. There is a piped-and-`exec` recipe there now, with `BIG.TXT` as the
control that separates a capability denial (instant, no packets) from a
supervisor restart (`server slot 4 wedged`) — the two failures this arc spent
two days confusing for each other.

**One of those was then fixed the same day** — the slot leak, because it was
the one a user hits by ordinary accident. `KILL` frees a *live* task's slot
outright, but a stage that already exited is a `Zombie`, `task_exists` counts
only `Runnable`/`Blocked`, so the kernel refuses the kill and only `WAIT` ever
reaps. Every "abandon this pipeline" path leaked one of five slots. Five
`cat /etc/passwd | grep` with no pattern — a typo, not a stress test — and the
shell could no longer run **any** `/bin` program: `echo still-alive` answered
`echo: no free task slot`. Eight paths now go through one `kill_and_reap`.

Two things this one taught. **The completion paths never leaked**, because they
already waited on every stage; only the error paths did — so the bug lived
exactly where nothing routine goes, which is why it survived every green run.
And **the fix falsified a comment two hundred lines away**: a block that read
"the capture killed the last stage (but didn't reap it)" and carried an
explicit `WAIT` to compensate. True when written; false the moment the capture
path started reaping. That comment was found by looking, not by the compiler —
the same class `true-when-written-postmortem.md` collects, hit while fixing a
bug found by a review, on a branch about correcting claims.

**Still pre-existing, recorded not fixed** (each is its own change, and a repair
is a change — `repairing-the-repairs-postmortem.md`): the "consumer already exited, stay quiet" heuristic reads the *target*'s state
when the kernel denies on a dead *grantee* first, so it prints the noise it
exists to suppress; nothing re-grants `TO_NET` after a supervised `netd`
restart, so a program spawned before the restart is netless for the rest of its
life; `netd` demuxes remote-exec by raw slot number, which was safe while one
task at a time held `TO_NET` and is not now; a nested shell cannot delegate
`TO_NET` at all (`may_delegate` reads the *static* mask, and spawnable slots
have none), so its whole subtree is silently netless; and `delegate_net`
discards its result, making the one grant this arc is about the only `DELEGATE`
in the shell with no failure signal.

---

## 2026-09-05 (cont.) — the fid verbs, two gates, and three reviews of twenty lines

*(Seven PRs after the site work. The frontier item's reported symptom is
closed, and the two most valuable hours were spent on things that did not get
built.)*

**The item named the wrong subsystem, and the scoping is what found it.** "The
fid verbs reach no export" says the export. But `libc/src/file.c` sent every
request to `FSD_TASK` and resolved no namespace at all, so a C
`open("/mnt/a/F")` asked `fsd` about a path only `netd` knows and never left
the machine. Teaching the export those five verbs is real work — it is now
steps 5–7 — and it would not have moved the reported symptom by one byte. The
plan was written first, with a check and a negative control per step, and that
alone paid for itself before any code.

**Step 1 was making the failure legible**, and it went first because every
later step is debugged through that message. Both peers answered an
unimplemented verb with the generic `FS_ERROR`, which clients render as "no
such file or directory" — a message about a path for a request whose path was
fine. Reserving `FS_ERR_NO_SUCH_VERB` turned out to move `FS_ERR_MIN`, because
the error band was **full** from `MAX-1` to `MAX-38`; `libc/include/sys.h`
hand-mirrors that floor, and a C program compiled against a stale one reads new
error codes as *successful returns*.

**Step 2 built the foreign observer before the client it measures**, and it
earned that ordering twice over. Writing the fid arms into
`scripts/np9p_server.py` exposed that **`NP_OPEN` does not use the parameter
layout every other path verb uses** — its `a0` is the flags and `a1` the path
length, the reverse of all its siblings — and it exposed it in the least
deniable way: the first self-test sent a generic frame, so a ten-character path
arrived as flags `OPEN_WRITE|OPEN_TRUNC`. The harness written to check the trap
reproduced it. Later the same observer found an `fsd` bug **by disagreeing with
it**: `NP_OPEN` never checked the file existed unless the caller was creating
it, so a read-only open of an absent path handed back a valid fid and the truth
arrived at the first read.

**Two gates ran, and both failed. That is the day's real result.** Step 3a
asked whether a Rust `staticlib` can link into these C programs at all. The
trivial version — `x + 1` — linked and proved nothing; the *representative*
version, calling the real resolver, failed outright on `R_AARCH64_ABS64` out of
prebuilt `core`. `--gc-sections` fixed it properly (the offending `.rodata` is
unreferenced, so collecting it removes the relocation rather than hiding it).
Step 4 asked whether the export connection can be held open for a fid's
lifetime. It built cleanly, and then the blocking fact turned up on the *client*
side: **both clients use the server's FIN as the end-of-reply marker**, so an
export that stops sending one hangs the Python peer and stalls every
guest-to-guest request into a timeout. That prototype was reverted rather than
parked behind a flag, because known-breaking code waiting to be switched on is
how a flag day happens by accident.

**The cheapest option was recommended first, and it was wrong.** For remote file
handles, translating fid ops to the path verbs the export already serves needs
no server state and would have closed remote write nearly for free. It was
rejected once the question was asked properly — stable, safe, room to grow —
because path-translation *structurally cannot* express a handle that survives
`unlink`, file locking, `O_APPEND` atomicity or directory streams, and because
a path re-resolved per operation is a TOCTOU: between a read at offset 0 and one
at 512 the far side can replace that path and the client splices two files
together with no error anywhere. A fid names the file; a path names a name.

**Then three reviews of the same twenty lines, and the progression is the
lesson.** Round one found a real bug: the early break left the export's FIN in
flight, stranding a slot out of a pool of four. Round two found a second real
bug — the break skipped a *coalesced* FIN — and, worse, arithmetic invented out
of nothing: a comment asserting `1960 = 4 × 512` (it is not) supporting a story
about a fixture stopping one chunk short (it does not). Round three found no
code defects at all, only claims: "make **every** client length-aware" marked
DONE when only the framed path had changed, with `cpu`'s `NP_RUN` stream
carrying no length prefix at all and therefore genuinely unable to follow.

Two things are worth carrying out of that. The first is that **rounds two and
three found things the day's own testing had passed** — including a "verified
both directions" run that proved less than it looked, because the host fixture
sat *exactly* on the threshold that decides the outcome. The second is sharper:
by round three the defects were no longer in the code, they were in what had
been *said about* the code. The reviews stopped finding bugs and kept finding
claims. A day's honest output includes a correction to a bug record that
contradicted a measured analysis already sitting in the same file, twelve
hundred lines further down.

---

## 2026-09-05 — clearing the site by deleting four of it

*(Documentation and one Makefile line. The check built yesterday went green,
and then into `make test`.)*

Yesterday's detector reported nine public pages behind their sources. Today
cleared them, by **fixing fewer pages than were broken**.

**Four pages were deleted rather than re-abridged.** `changelog`, `roadmap`,
`shell-commands` and `processes` are reference material: they are worth more
*current* than abridged, and they move on days when nothing else does.
`roadmap.html` was 11,050 words against a 14,286-word source — barely an
abridgement, so it drifted every time the roadmap was touched. `docs.html` now
links the markdown on GitHub for all four, which removes four drift surfaces
permanently instead of promising to re-abridge them forever. The site is
public, so this was a decision to take rather than a maintenance call: the
alternative — re-abridge all nine and accept the recurring cost — was
legitimate and was declined.

**The remaining five were re-abridged by hand**, and that is where the day's
real finding turned up: **three of the markdown sources were themselves behind
the code.** `architecture.md` described six task slots and stopped its
capability table at `netd`; `tasks.rs` says eleven, with `accountd` at slot 5
and 6–10 spawnable, and its `EXIT`/`KILL` guard derives from one constant
rather than the list of slots the document named. `manual.md` said the
workspace had four crates (it has 61) and that the filesystem server was
FAT32-only (it reads FAT32, exFAT and ext2). Each was corrected at the source
before its page was written from it, because a page cannot be honestly
abridged from a document that is wrong, and the only other option was
publishing the error onto a live site.

That is [`asking-the-right-question-postmortem.md`](asking-the-right-question-postmortem.md)'s
shape arriving from a new direction. `tasks.rs` is the authority on the slot
map; every restatement of it drifts, and this time the drift had propagated two
copies deep — code → `architecture.md` → `architecture-overview.html` — with
the public copy the furthest behind and the only one a stranger reads.

The two research pages needed less than the diff suggested. Both already
carried the *substance* of an "this section's premise is now out of date"
correction; what they carried stale was a **count** — "the FS is a userland
server", written when there was one, against four today. A number is the part
of a page that ages first and reads most confidently.

**One page needed no edit at all.** `research-directions.html` was reported
because its source's only change was `notes.txt` → "the original brief", a
rewording the page never quoted. That is the over-reporting direction the check
was specified to err in: diffed, confirmed in sync, re-stamped alone. Saying
which way a check errs is part of specifying it, and this is the second time
that clause has been the one that mattered.

**Then the one-line change the whole thing was for:** `check-site` now runs
inside `make test`. It was outside only while nine failures would have made the
suite permanently red. Verified by mutation first, not by watching it pass — a
line appended to `manual.md` turned it red at exactly one page, which is the
only evidence that a green run means anything. `make test` is 137 tests plus
the wire constants plus the site, still under three seconds.

---

## 2026-09-04 — housekeeping, and measuring before restructuring

*(No kernel or server code. Three PRs, all documentation and tooling — and one
scoping pass whose main product was refusing two plausible ideas.)*

The day started as tidying and turned into an argument with an instinct.

**The standup was in the wrong place.** `/day-closeout` had been writing
`docs/daily-standup.md`, kept out of history by a `.gitignore` rule. That works,
but it puts a personal note that is overwritten daily inside the directory
holding the durable record — the one place a reader is told to trust — with a
rule as the only thing separating them, and a rule is invisible when you are
looking at a file listing. Moved to `scratch/`, which is already ignored and is
not part of the repository. The exact-path ignore rule was then dead, so it went
too: a guard over a location nobody writes to would only ever fire on a mistake
it also hides.

**`docs/roadmap.md` became `docs/ROADMAP.md`**, matching `CHANGELOG.md` and
`README.md`; it had been the one lowercase name in the set. Renamed through a
temporary name deliberately — macOS is case-insensitive, so a direct
`git mv docs/roadmap.md docs/ROADMAP.md` is a no-op that leaves the file
lowercase in the index while every local check reports success, and the rename
surfaces only in a case-sensitive checkout, where the links break. Two moves via
`ROADMAP.tmp.md` forces git to record it, and it is stored as `R100`. All 120
references across 46 files followed, reaching well past markdown: comments in
four kernel sources, `libc/hello.c`, the Makefile, and the committed
`docs/site/*.html`. Also an MIT `LICENSE` — the repo has been public without one,
which means all rights reserved — and a `make all` target.

**A wrong turn worth recording**, because it is the kind that looks like it
worked. The three changes were meant to be three reviewable commits. The first,
`git add LICENSE` then `git commit`, silently swallowed the roadmap rename as
well: the rename had been staged in the index earlier, and `git commit` commits
the *index*, not the paths you just added. Caught before pushing by checking
`--name-status` rather than trusting the commit message. Rebuilding it needed
care in a second direction — the fix must never *unstage* the rename, because an
index holding `docs/roadmap.md` while the worktree holds `docs/ROADMAP.md` is
precisely the case-insensitive phantom state the temp-name move exists to avoid.
Three commits, each one concern, verified by reading `--name-status` back.

**Then the real question: the project feels big, so what should be split out or
parked?** Two specific proposals — break some programs into a separate
repository, and bench FAT32 to focus on ext2. Both measured as wrong targets,
and the measuring was the day's real work.

*It is not big, and not slow.* Tracked source is **9.8 MB**; the 2.1 GB working
tree is 1.95 GB of `target/` and `build/`. An incremental kernel build is
**1.5 s**, the full ESP stage of all ~50 userland binaries **12 s**, `make test`
**2.6 s**. `default-members = ["kernel"]`, so a bare `cargo build` never touches
the 50 userland crates or the 5 pure ones — splitting them would save
approximately zero, because the time is not there to save. Only `ed25519` and
`regex` are genuinely standalone libraries anyway; splitting buys coordination
overhead (path deps become git deps, a change across the boundary becomes two
PRs) against no measured gain.

*FAT32 cannot be benched.* Three findings, each verified rather than recalled.
UEFI can only boot FAT, and `mkext2.py` says so in its own docstring —
`image-ext2` *depends on* `image`, because the ext2 rig still needs a FAT32 ESP
as partition 2. But that ESP is read by firmware, not by `fsd`: `loader.rs` uses
`uefi::fs::FileSystem` during the boot-services window, so `fsd`'s 2,239-line
`fat32.rs` is only for runtime mounting — which made the question a fair one.
What settles it is that **on real hardware there is only one partition and it is
FAT32**: Parallels' `esp.hdd` and the Pi 4 boot card both. Parking `fsd`'s FAT32
arm would leave `fsd` able to mount nothing on either real-hardware target,
while 2× Pi 4 is the declared next step. The invertible version does work —
`exfat.rs` (1,713 lines, one rig, no real hardware) is the only arm with no
structural role — but parking it saves attention, not time, and that was worth
saying rather than dressing up.

**The weight is in the documents, and one of them is public.** 31,430 lines of
markdown, 29 postmortems, a 6,242-line changelog, and 23 hand-maintained HTML
pages under `docs/site/` with no generator. That last one is not a size
complaint but a live failure: `docs/` is served by GitHub Pages, and the site
**froze on 2026-08-23** while its sources kept moving. Two releases and a closed
frontier item are absent from the public site, with nothing anywhere reporting
it. See [`ROADMAP.md`](ROADMAP.md) and
[`true-when-written-postmortem.md`](true-when-written-postmortem.md).

**The first framing of the fix was wrong too, and the numbers refused it.** A
generator was the obvious answer and would have destroyed the site: the pages
are curated *abridgements*, not renderings. `changelog.html` is half its
source's words; `architecture-overview.html` a sixth, and a different document
with a different job. The site carries 10 of 29 postmortems, `glossary.html` has
no markdown source at all, and the slugs are not derivable. Rendering every
`.md` would double the changelog page and delete the glossary. What was needed
was not generation but detection — so `scripts/check-site-freshness.py` makes
the drift loud and fixes nothing, and the nine pages stay a human job.

It is deliberately **not** in `make test` yet. Nine pages are already behind, so
wiring it in today would make `test` permanently red and train everyone to
ignore it — the failure mode that would waste the check entirely. It sits beside
`check-relocs` as `make check-site` until the backlog clears, and the Makefile
comment says that moving it in is the one-line change that finishes the job.

---

## 2026-09-03 (cont.) — the supervisor was killing netd mid-read

*(The fix for what the packet trace found, plus the `ls` exit code that made it
hard to see. Two changes, one kernel and one server.)*

The trace showed every TCP connection healthy and `netd` being **restarted by
its own supervisor** during a multi-chunk remote read. The mechanism, once
found, is a clean case of an expired premise — the third today.

`supervisor.rs` has two liveness regimes: a *Blocked* server gets an active
ping and must ack it; a *Runnable* server is judged by the passive
`WEDGE_TICKS` counter, 128 ticks = **2.56 s** of continuous runnability. That
split is sound, and its doc states the assumption it rests on:

> *"safely above any real request (servers return to `Blocked(recv)` in far
> less than one tick)"*

True when a request meant a local disk read. **`netd` busy-polls a non-blocking
`recv` for the whole of a TCP round trip**, so it is `Runnable` throughout — and
against a peer that signs in Python, five sequential round trips is 3.0 s. Over
the line, every time.

And the active ping could not save it: `poll_ping` returns early for a
non-blocked server, on the reasoning that "a Runnable *wedge* is the passive
heartbeat's job". So a busy server is **never asked** whether it is alive, and
is killed for taking too long.

### The fix, and why it is this one

The tempting fix is to raise `WEDGE_TICKS`. That is not a fix — any threshold
is exceeded by a slow enough peer, and the number would have to encode
assumptions about a machine at the other end of a network.

Instead: **a supervised server may say it is alive, unprompted.** The mechanism
already existed — the kernel intercepts a `MSG_SEND` to `KERNEL_SENDER` as an
ack before any argument validation, so it needs no buffer and cannot fail.
`note_ack` now also clears the passive counter, which promotes the ack from
"the ping was answered" to "this server is making progress", and `netd` beats
once per tick inside each of its seven wait loops.

What it deliberately keeps: a genuinely wedged server never reaches the line
that sends the beat, so the passive arm still catches exactly the case it was
written for. What it stops doing is punishing a server for being busy.

### Two things the edit itself got wrong first

**A substring replace double-patched two loops.** The pattern for the deeply
indented sites was a superstring of the pattern for the shallow ones, so the
deep loops got the beat line twice, at the wrong indentation. Caught by reading
the result rather than trusting the replacement count — which was 7 where the
grep had said 5, and that discrepancy was the tell.

**The first pass missed the loops that mattered.** Five sites matched
`if now() > deadline {`; the two **reply** waits read
`if got >= resp.len() || now() > deadline {`, a different shape. Those are the
loops that span the peer's think time — the handshake completes in
milliseconds. Fixing only the five would have left the bug and produced a
convincing-looking patch. Now asserted mechanically: every line containing
`now() > deadline` must have a beat on the line above, 7 of 7.

### Result

The workload that failed **11 of 42** now fails **2 of 42**, with the peer
serving 105 requests where it had served 59 — and *zero* wedge or restart lines
in the kernel log, down from three restarts and an exhausted budget. The
residual is a different fault: it reports `cat: failed`, not the `NO_FS` of an
absent server, and it is the older "intermittent first-op" the roadmap recorded
on the two-node rig. About 3 in 46 across both runs, small sample, stated as
one.

### And the `ls` exit code

`ls` had exactly one `exit` call — `exit(0)` — so a missing file, a permission
denial and an unreachable cluster peer all reported success. Every other command
under `programs/fileutils/` exits 1. It now accumulates a failure flag and exits
1 if any operand failed, while still listing the operands that worked, which is
what every Unix `ls` does.

A local, not a `static mut`: all three failure sites are in one function, so the
mutable-statics question does not arise. `ls /mnt/a/NOPE` now exits 1, verified
in the same run as the wedge fix.

---

## 2026-09-03 (cont.) — the twenty-ninth postmortem: true when written

*(No code. Reading back the day's four fixes and finding they were one fix.)*

Five PRs, and four of them corrected a **statement** rather than logic. In every
case the code was doing exactly what it had been written to do; what had rotted
was the description. Written up as
[`true-when-written-postmortem.md`](true-when-written-postmortem.md), the
twenty-ninth.

It is the sequel to `repairing-the-repairs`' *a claim in a comment is not a
check*, moved one step:

> **THE COMMENT WAS TRUE WHEN IT WAS WRITTEN. THAT IS THE PROBLEM.**

A claim that is wrong on arrival can be caught by a careful reader. A claim that
is *right* on arrival passes every review it will ever get, and is then
falsified by an unrelated change in a different file.

The number that made the shape unarguable: `fs_mv`'s *"a cross-tree move can't
arise yet (a later phase concern)"* was written at **12:06** on 2026-08-25 and
falsified at **16:37 the same day** by Phase 1c's remote mounts. Its
consequence — a cross-mount `mv` that renamed the file locally and exited 0 —
survived nine days, hidden behind a second defect that stopped `mv`'s guard
before it could reach the first.

The mechanism the four share: **the falsifying edit is always in another file.**
Which is exactly why the three defences all miss it — the compiler sees only a
string, no test asserts that a comment is true, and a reviewer reads a diff that
does not contain the claim it invalidates.

Two things I had not seen before writing it down. **Duplication acted as the
only detector** — the stale 8.3 filename message surfaced because the shell's
duplicate error table was laid beside `ulib`'s and they disagreed; wire
*constants* have a comparer that grew 12 → 27 names that day and caught every
mutation, while wire *prose* has none. And **two of the four claims were mine
and never true at all**, the second written immediately after being corrected
for the first — which is a different species from the other two, preventable at
writing time, and worth not conflating with them, since "read more carefully"
does nothing for a comment that was accurate when read.

---

## 2026-09-03 (cont.) — widening the parser instead of describing the gap

*(The last of the three `NP_STAT`-review follow-ups, and the shortest. Its one
lesson is about where I had put the blame.)*

`STAT_FLAG_DIR` — the bit saying a remote entry is a directory — was pinned by
nothing. `check-wire-constants.py`'s Rust integer pattern wanted `usize` (it is
`u32`) and both languages' patterns wanted a literal (it is `1 << 0`), so it
was invisible to **both** sides and adding it to `CHECKED` failed with "not
found in ninep-abi".

I had written that up, the day before, as *"genuinely unpinnable as this script
stands"*. That sentence is the finding. **The script's reach is a property of
the script**, and I had recorded it as a property of the constant — a limit of
my own tool, described as a fact about the thing it was failing to see. Two
regex lines per language.

25 → 27 constants: the dir bit, plus `FS_ERROR`, which the server and Rust both
spelled and the list had simply never mentioned. That second one was found by
**asking the question mechanically** — which names does a peer spell that Rust
also declares, and that `CHECKED` omits — rather than by remembering. The
answer was one name, and nothing would have prompted me to look for it.

### The damage, forged rather than asserted

The roadmap entry claimed a drifted bit would make "every remote directory
render as a file". Rather than ship that as a prediction, I set the peer to
write the bit at `1 << 1` and ran the guest:

```
# ls -l /mnt/a
-rw-r--r--    -    -         0        -         /mnt/a
```

The mount **root** is classified as a file, so `ls` prints one zero-byte entry
and never enumerates it — `HELLO.TXT` and `SUB/` do not appear at all. Exit
code 0, no error at any layer. Worse than predicted, and the kind of thing
worth knowing precisely before writing a comment about it.

Both mutation directions are caught, including the one the review named (move
it in `ninep-abi`, peer keeps writing bit 0). The `FS_ERROR` mutation was
instructive too: setting it to `(1 << 64) - 2` made it match **no** pattern, so
instead of a value disagreement it tripped the *dead-entry* guard and the
per-peer baseline — the two checks that exist for a constant that has stopped
being looked at, rather than one that is wrong. I expected the third guard and
got the other two, which is the better outcome.

---

## 2026-09-03 (cont.) — a status code nobody compared, and the `mv` it was hiding

*(The third follow-up from the `NP_STAT` review. The intended fix was three
lines. What it uncovered was a cross-mount `mv` that renamed the file locally
and reported success.)*

### The intended fix

`scripts/np9p_server.py` answered a bare `FS_ERROR` for every failure, where
`fsd` answers `FS_ERR_NOT_FOUND` for a path that is definitively absent. That
is not merely a vaguer message: `ulib::fs_presence` **branches on that exact
value** to answer `Absent` rather than `Unknown`, and `Unknown` is what
`mv`/`cp`'s destructive-overwrite guard treats as "could not tell". So the peer
knew a path was absent and the guest could not find out.

| | before | after |
|---|---|---|
| `ls /mnt/a/NOPE` | `failed` | `no such file or directory` |
| `cp /F.TXT /mnt/a/NEW.TXT` | `cannot tell whether … exists` | `read-only filesystem` |

A verb the peer *knows* but refuses on policy now answers `FS_ERR_READ_ONLY`
instead of sharing one value with "verb I do not recognise" — three different
conditions that had all rendered as one.

`check-wire-constants.py` grew to read **syscall-abi as well as ninep-abi** and
to parse the `u64::MAX - N` idiom that both Python peers hand-transcribe as
`(1 << 64) - 1 - N`. That arithmetic was being done twice by hand, in two
languages, for values whose drift is silent — 12 → 25 constants checked, both
mutations caught. It also now reports *which crate* a disagreement is against,
because saying "ninep-abi has …" for a syscall-abi constant sends the reader to
a file that does not contain it.

### What it uncovered

With the guard no longer stuck on `Unknown`, `mv /F.TXT /mnt/a/NEW.TXT` got
past it — and **exited 0 with no output**. The peer's log showed no `NP_MV` had
ever arrived.

`ulib::fs_mv` resolved both paths through the namespace and then dispatched on
the **source's** target, handing that server the destination's *string*. The
server read it as its own path. So the file was renamed to a **local**
`/NEW.TXT`, the remote mount was untouched, and the exit code said success.
Confirmed against `main`: `mv -f` reached the same path there, so this was
pre-existing and `-f` was all it took.

Above that dispatch sat:

> `// Both paths resolve through the namespace; in Phase 0 every binding is`
> `// tree 0, so a cross-tree move can't arise yet (a later phase concern).`

True when written. False since remote mounts, `/proc` and multi-mount arrived.
**The later phase came and nobody came back** — and because the assumption
lived in a comment rather than a check, nothing failed when it expired. That is
the same shape as the `8.3 short-name` message fixed hours earlier: a true
statement that quietly stopped being one, with no mechanism watching.

Refused now with a reserved `FS_ERR_CROSS_DEVICE` — POSIX's `EXDEV` — comparing
**all three** fields of the resolution, because a local `/net` and a remote
mount both resolve to `NET_TASK` with tree 0 and differ only in the endpoint.
`-f` does not bypass it: the guard is in `fs_mv`, not in `mv`'s presence check,
and `-f` was precisely the arm that reached the silent rename. A same-tree `mv`
is unchanged, verified in the same run.

### Two notes on method

**My PR made a pre-existing bug more reachable, so it was mine to fix.** The
silent rename needed `-f` before and would have needed nothing after. Shipping
the status-code fix alone and filing the `mv` bug would have been the tidier
PR and the wrong call.

**The error-message tables I aligned earlier today got their first test.**
Adding `FS_ERR_CROSS_DEVICE` meant updating both `ulib::fs_error_msg` and the
shell's `print_fs_error` — the two copies whose drift was fixed that morning.
The discipline held because the drift was fresh in mind, which is not a
mechanism. Unifying them is still open.

Also: an earlier run of this sequence showed `cat: failed` on a chunked remote
read. Re-run, it succeeded, as did the final verification — the recorded
intermittent, not this change. Logged under frontier item 3 with its sample
size rather than explained away.

---

## 2026-09-03 (cont.) — `ls` stops claiming every failure is a missing file

*(A guest bug, three lines, armed on every filesystem that enforces
permissions.)*

`ls_err` hardcoded `"no such file or directory"` and printed it for **every**
`fsd` status. `ls` was the only command under `programs/fileutils/` that never
called `ulib::fs_error`, which already renders fifteen codes distinctly — cat 1,
cp 4, mv 3, chmod 3, `ls` **0**.

So on any ext2 mount, a file you are not allowed to read reported as a file that
does not exist. Demonstrated both ways on the ext2 rig, the only image where
`fsd` enforces modes:

```
  before:  $ ls /PRIV      ls: /PRIV: no such file or directory
           $ ls /NOPE      ls: /NOPE: no such file or directory
  after:   $ ls /PRIV      ls: /PRIV: permission denied
           $ ls /NOPE      ls: /NOPE: no such file or directory
```

The "before" arm is the point: a directory that **is there** and one that **is
not** were indistinguishable. Misleading in ordinary use and actively so in a
security context — and the same string covered `FS_ERR_AUTH` (a cluster peer
refusing a signature) and a transient `NO_FS` while `fsd` restarts.

`ulib::fs_error_msg(code)` splits the message table out of `fs_error` so `ls`
keeps its richer `ls: <operand>: <msg>` prefix — with several operands the
message alone does not say which one failed — without carrying a second copy of
the table. Both failing call sites now pass the code they already had and were
discarding. The third, `resolve` returning `None`, is a *client-side* path
problem no `FS_ERR_*` value describes, so it gets its own message rather than a
code the server never sent.

### A second table, found by looking for the first

Checking whether anything else hardcoded the string turned up the shell's
`print_fs_error` — not the same defect (it does dispatch on the code) but a
**second copy** of `ulib`'s table, which the shell keeps because it has its own
fs layer and cannot share one. The two had already drifted, both ways:

- The shell was missing `NO_FS` and `FS_ERR_READ_ONLY`, so both fell to its `_`
  default and printed "failed".
- Its `FS_ERR_INVALID_NAME` read *"invalid name (must fit this kernel's 8.3
  short-name subset)"* — **false since 2026-08-27**, when FAT32 long-filename
  *write* support landed and began generating a short alias beside the LFN
  entries for exactly the names that message says are refused. Verified rather
  than assumed: `touch /AVERYLONGFILENAME.TXT` on the FAT32 image succeeds and
  lists under its full name. The message outlived the restriction it described
  by a week.

Both fixed. Unifying the two tables is a larger change and is not attempted
here; the copies at least now mention each other.

### One self-inflicted scare

A verification run printed `ls: /PRIV: no such file or directory` *after* the
fix, which read as a regression. It was the test: `make image-ext2` regenerates
the ext2 partition from `mke2fs -d`, so the `/PRIV` an earlier boot created was
gone and `ls` was correctly reporting a missing directory. **On a rig whose disk
is rebuilt under it, the setup steps must be in the same run as the assertion.**
Worth recording because the failure looks exactly like the bug returning.

---

## 2026-09-03 (cont.) — the `ls`-of-a-remote-mount bug, and a review that inverted my account of it

*(Roadmap frontier item 2, open since 2026-08-31. Fixed in the host test peer.
Then reviewed at `max`, which produced fifteen findings — and the three that
mattered were all corrections to what I had just written about the fix.)*

The symptom, exactly as recorded: on the `run-image-9p-client` rig,
`mount -r 10.0.2.2:5641 /mnt/a` succeeds, `cat /mnt/a/HELLO.TXT` works, and
`ls /mnt/a` says **"no such file or directory"**. The roadmap's note read: *"So
it is the guest's resolution of the mount root — probably an empty path where
the server expects `/`."*

`resolve_ns` has an explicit guard for exactly that case, so either the guard
was broken or the hypothesis was. **Run it, don't read it.** I stood the host
peer up behind a logging wrapper printing the decoded verb and path of every
request:

```
  REQ 0x10c (UNKNOWN) path=b'/'      pathlen=1  served=NO
  REQ 0x10c (UNKNOWN) path=b'/SUB'   pathlen=4  served=NO
  REQ READ            path=b'/SUB/NOTE.TXT'     served=yes
```

The guest sends **`/`**, correctly. `/SUB` fails identically, so it was never
about the mount root — the original note had simply not tested that. `0x10c` is
`NP_BASE + 12` = **`NP_STAT`**, and the peer implemented no such verb. `ls`
stats a named operand before listing it; the peer returned `FS_ERROR`; `ls`
renders `FS_ERROR` as "no such file or directory". **A message about a path,
for a request whose path was fine** — which is why it stayed filed as a path
bug. Fixed by implementing the verb.

That much held up. What follows is what the review found, and it is the more
useful half of the day.

### The lesson I drew was the wrong lesson

I wrote this up as *repairing one half of a pair is not repairing the pair*:
[`blind-instruments-postmortem.md`](blind-instruments-postmortem.md), written
the day before, opens with `np9p_client.py`'s fake `stat`; the client was
repaired 2026-09-02; nobody checked the server. Neat, and it fit the week.

**`git log -S` says it is not what happened.** The peer was created 2026-08-25
(`a9e7342`) and `ls` did not call `fs_stat` **at all** until 2026-08-27
(`3cf79d1` added `ls -l`, `54a9b01` file operands). So the peer was *adequate
for its documented recipe when it was written*, and
`roadmap-cluster-phase1.md` and `CHANGELOG.md` were accurate when written too —
my note implied both had been wrong for days. Acting on it would have put an
error into the milestone record while correcting a different one.

The real mechanism: **a guest client grew a verb dependency, and nothing re-ran
the recipe that depended on it.** Seven days, 08-27 to 09-03. That points
somewhere else entirely — not at the peers being unchecked mirrors, but at a
documented recipe with no test behind it. `chmod`'s symbolic form already calls
`fs_stat`; `mv` and `cp` call `fs_presence`, which is `fs_stat`. The next one to
grow a requirement breaks this rig identically, and the lesson I had written
would not have caught it.

**A neat lesson that fits the week is the one to check hardest.** I had three
postmortems in a row about unchecked observers and reached for a fourth
instance of the same shape. The shape was available; the history did not
support it.

### The control I called proof could not fail

I recorded `ls /mnt/a/NOPE` still failing as "what proves the new arm can say
no". It proves nothing. An unserved verb and an absent path both reach
`sealed(FS_ERROR)`, so the reply is byte-identical — the check passes against
the *unfixed* peer. A check that cannot fail, inside the verification record of
a PR whose subject is checks that cannot fail.

The control that does discriminate was in the same list and unlabelled:
`ls /mnt/a/SUB`, which fails before and lists `NOTE.TXT` after. **I wrote the
check from the shape of the fix, not from the shape of the failure** — which is
[`repairing-the-repairs`](repairing-the-repairs-postmortem.md)'s finding, and I
had read it that morning.

### And a cheaper instrument existed

`ls` with no operand does not stat — it lists the cwd. So `cd /mnt/a; ls`
worked the whole time, and two commands would have isolated the fault to the
operand path before any wrapper was written. The wrapper was still what named
`0x10c`, so it earned its keep; but I built the precise instrument before the
cheap bisect, in that order.

### Three more of mine, all restatements that drifted

- **`cat` sends `NP_READ`, not `NP_READ_AT`** as I wrote — and the transcript
  pasted two lines above the claim reads `REQ READ`. Contradicted by my own
  evidence, in the same entry.
- **FAT32 *does* decode an mtime** (`fat32.rs:788`). My docstring said `fsd`
  reports no time for "FAT32, exFAT, `/proc`"; the `time: None` arms are exFAT,
  ext2 and `/proc`, while the *mode* triple is FAT32/exFAT/`/proc`. I collapsed
  two different lists into one.
- **`cp` does call `fs_stat`.** I ran `grep -c fs_stat` over `cp`, got 0, and
  published the reason. It calls `ulib::fs_presence`, which is `fs_stat` one
  level down. The conclusion (`cp` worked) was right; the reason was wrong — it
  worked because that command's *destination* was local, so the stat never
  crossed the mount. Reverse the operands and it fails. **My instrument was a
  grep that could not see through an indirection, in the entry about
  instruments.**

Also mine, in the wire-constant check: I excluded the four `STAT_*` offsets
with a note claiming "only `np9p_server.py` spells them, so there is no second
declaration to compare, same as `NP_MAC_LEN`". Both halves wrong. The script
compares each peer to **`ninep-abi`**, not the peers to each other, so one peer
is enough — the offsets were checkable all along (adding them: 14 → 18
constants, and a mutated `STAT_FLAGS_OFF` is caught). And `NP_MAC_LEN` is
excluded for a different reason: Rust does not declare it at all. I had also
left the rationale three lines below stating two counts that were both wrong,
one of them already false before I touched it.

### What the review did not find

The fix itself. The peer genuinely lacked the verb, `0x10c` is `NP_BASE + 12`,
`40 × 49 = 1960`, and `ls` works. Every finding of substance was about the
*account* of the fix rather than the fix — which is its own data point about
where the defects are when the code change is twelve lines and the prose around
it is a hundred and thirty.

### The one genuinely valuable find, and it is a guest bug

`ls_err` hardcodes "no such file or directory" for **every** error code, and
`ls` is the only command under `programs/fileutils/` that never calls
`ulib::fs_error` — which already renders fifteen codes distinctly (cat 1, cp 4,
mv 3, chmod 3; `ls` 0). The misleading message that cost this session is three
lines in the guest, and it stays armed where no Python peer exists: over a real
ext2 mount an `FS_ERR_PERM` prints "no such file or directory", which is
actively misleading in a security context, and so do `FS_ERR_AUTH` and a
transient `NO_FS` during an `fsd` restart. Its own change, next.

---

## 2026-09-03 — shrinking `CLAUDE.md`, and what the move exposed

*(No code. `CLAUDE.md` had reached 197,771 bytes — read in full at the start of
every session, whether or not any of it was relevant. It is now 50,003, and
nothing was deleted from the repository.)*

The file's bulk was concentrated: `## Structure` alone was 147KB of it, 75%.
But length was not really the problem. **The same prose was in three places.**
Take the cluster-auth arc: a 1.6KB abstract in `Structure`'s `docs/` block, a
second description of the same thing inside a 13.9KB bullet 700 lines earlier,
and the real one at the top of `cluster-auth-postmortem.md` — which, like every
postmortem here, already opens with a title, an italic abstract and its spine in
a blockquote. Two copies of an abstract the file itself writes better.

Two moves, both verbatim:

- The annotated `docs/` index (55KB) became [`README.md`](README.md). Moving it
  next to the documents it indexes should keep it current, since it can now be
  edited in the same commit as the doc it describes. It immediately gained the
  nine files it had drifted away from: `RELEASING.md`,
  `microkernel-comparison.md`, `roadmap-cluster-phase{0..4}.md`,
  `release-notes/`, and the generated site.
- The annotated source tree (91KB) became [`source-map.md`](source-map.md).
  `CLAUDE.md` keeps a skeleton: one line per file, enough to find the right one
  and to know what else a change will touch.

The 13.9KB postmortem bullet was deleted outright rather than moved, replaced by
a pointer plus the five whose lessons generalize past their own subsystem.

Both moves were reconciled the way
[`review-and-split-postmortem.md`](review-and-split-postmortem.md) says to:
diff the original against the *union* of the pieces before closing the source.
Every one of the original's 808 unique lines is accounted for — in the new
`CLAUDE.md`, in one of the two new files, or in the one bullet deliberately
removed. Five lines came back unmatched on the final pass and all five were
edits made on purpose. The moved blocks were also `cmp`-checked byte-for-byte
against what came out the other side, because "the lines are all present" and
"the block is intact" are different claims.

### The instrument was wrong again

Two of my own verification counts disagreed: 27 postmortems indexed, or 28. The
index was right. The count regex was `^  [a-z-]+-postmortem\.md `, and
`cluster-phase0-postmortem.md` has a digit in it. A trivial slip, but the shape
is the one [`blind-instruments-postmortem.md`](blind-instruments-postmortem.md)
is about — the check reported a plausible number about a question it had not
asked, and it was only caught because a second check disagreed with it.

### What the move exposed: the `//!` headers have drifted

The argument for `source-map.md` was partly that the long-form annotation
belongs in each file's own `//!` module doc, which is read only when that file
is open. That argument assumed the `//!` headers were current. They are not, and
on exactly the files that have changed most:

| file | `//!` says | reality |
|---|---|---|
| `programs/servers/netd/src/main.rs` | ends at "Stage 2b: real ARP + IPv4 + ICMP … guest-initiated ping only" | the TCP server, the HTTP static-file server, all of Stages 4a–4o, the 9P export gateway, cluster auth and signed frames, `cpu` remote execution, and the `/net/tcp` dial-out/dial-in files — none mentioned |
| `programs/servers/fsd/src/main.rs` | no mention of permission enforcement or fids | both shipped (users arc step 3; libc arc step 5) |
| `kernel/src/tasks.rs` | no mention of `SENDER_CREDS` | the credential-bound-at-send fix, the whole point of [`asking-the-right-question-postmortem.md`](asking-the-right-question-postmortem.md) |
| `ed25519/src/lib.rs` | no mention of small-order key rejection | added after the review that found the hand-written table with three wrong entries |

Seven checks for a latest-documented capability, seven absent. None of this is
visible to the compiler or to any test — the category, not bad luck, and the
same category the last two retrospectives are about.

It also meant I had written a sentence into both new files claiming the `//!`
was "the closest thing to authority", which I disproved twenty minutes later
while looking for exactly this. Both now say the code is the authority and every
annotation is a claim to check. **Refreshing those headers is the follow-up this
day leaves open**, and it is a source change, not a docs one.

---

## 2026-09-02 — emptying the small-gaps parking lot, and four blind instruments

*(A day of small items. Five roadmap ledger entries struck, one feature
finished, two review rounds — and a theme nobody set out to find.)*

Started by trimming the assistant's memory directory, which had grown to 26
notes and 62 KB, most of it restating what this repository already records. It
came down to 20 notes and ~35 KB, with the pre-trim state snapshotted into
`docs/archive/` beside the two build logs moved out on the 1st. Six notes were
retired outright because a postmortem or a reference doc already covers them.

Then the roadmap's small items, chosen by reading the code rather than trusting
the roadmap's own sizing — which turned out to matter, because the first one's
description was wrong.

**The `fsd` mode-check.** `NP_STAT`, `NP_CHMOD` and `NP_CHOWN` skipped the
"does this filesystem model modes?" short-circuit that every other verb gets.
The roadmap said the symptom was `cat ../f` succeeding while `ls -l ../f` is
refused. Booted `main` to get a baseline and `ls -l /BIN/../ETC/PASSWD` worked
fine: `ulib::normalize_path` collapses `..` client-side, so no `/bin` program
can send `fsd` a path containing one. The divergence is real but reachable only
from the 9P export, which sends raw paths.

**So the export was the observer — and the observer was blind.** Three probes
through `np9p_client.py` came back green against a `main` guest that definitely
had the bug. They were green because the tool's `stat` op sends `NP_READ_FILE`,
not `NP_STAT`, and prints the returned byte count as a "size" — a plausible
answer produced by a completely different verb. `NP_STAT` is the *only* verb
that reaches `ancestors_searchable` without going through `path_allows`, so the
one arm most in need of a foreign observer was precisely the one this tool could
not address. Fixed it; the divergence reproduced immediately.

The same shape appeared twice more before the day ended. `drive-qemu.py` could
not type an empty line at all — `if text:` skipped the step — which made every
"refuse an empty answer" rule untestable from the harness, including the
`useradd` empty-password refusal added an hour later. And `cargo doc`, which
caught an absorbed doc comment during the cluster-keys arc, did not catch one
this time: the userland crates emit 39 unresolved links where the kernel is held
at 0, and nobody reads a noisy output.

**The fail-open mount warning** was the nicest fix of the day, because it turned
out not to need the thing I had planned. I had said "add the bounded `NO_FS`
retry"; the actual defect was that two functions three lines apart disagreed
about whether a race exists, and adding a second retry budget would have made
that worse while looking like a fix. Reordering `login` to read the account file
first and warn second makes the race *unreachable* instead of merely unlikely,
with one budget rather than two to keep in step. Three of the four branches
can't occur on a healthy QEMU boot, so they were reached by breaking the server
on purpose — the useful one being that clearing the flag on ext2 raises the
warning *while `mount` still prints the name `ext2`*, which is what proves the
shell reads the flag and not the name.

**POSIX character classes** were the one item with no boot at all. The
definitions are computed from `core`'s `is_ascii_*` predicates rather than
transcribed as twelve bit tables — 384 hex bytes is a transcription task, and
the same reasoning as the small-order key table in the keys arc. That surfaced
two real disagreements between `core` and POSIX. The tests assert cardinalities,
since spot-checking `[[:alpha:]]` against `"a"` passes for five of the twelve
classes.

**`mv` replacing an existing file** took the rest of the day and two review
rounds. The feature itself was small once scoped to file-replaces-file; ext2 got
the near-atomic version the roadmap predicted, one write of the destination's
directory entry. Then Hans asked whether replacing should require asking first —
a good question, because adding replace had *removed* a safety that was already
there. The answer was `-f` on both `mv` and `cp` (which has always clobbered
silently), and a refusal rather than a prompt: interactivity here is keyboard
ownership, and neither command has a keyboard in a pipeline, under `cpu`, or
when the request arrives from a 9P peer.

Round 1 found ten things, two of which could destroy a file. Both FAT arms freed
the *destination* before writing the new entry — fine against a crash, wrong
against an ordinary error. And ext2's replace path freed an inode two names
shared. Neither this OS nor its tools can produce a hard link or a cross-linked
cluster, so both were forged: `debugfs` for ext2, a hand-patched first-cluster
field for FAT. With the ext2 guard removed, `cat` returned empty and `mv` still
exited 0.

Round 2, scoped to the repairs as this project has learned to do, found eight —
and four were prose that the round-1 reorder had invalidated, sitting in the
commit whose message was partly about fixing stale doc comments. The other
notable one: round 1 had argued for the same-inode guard from "this OS cannot
create that, but these images are host-built," and then applied it to one arm of
three. The same argument covers `vvfat`.

And the fourth blind instrument, from testing that: `cat` still printed the right
content with the cross-link guard removed, because freeing a chain marks the FAT
and never touches the data. Only `fsck_msdos` saw it. I would have called that
guard verified on a passing `cat`.

Cut **v0.17.0** at the end of the day — and the release produced the fifth.
`scripts/release.sh publish` printed its "published" line having created the
tag, pushed the tag, and cut the GitHub Release, but never pushed `main`. So the
release pointed at a commit that existed on the remote only via the tag, and the
repository still said `VERSION 0.16.0`. It has been wrong since the tooling was
written, and it never showed because the next merged PR carried the release
commit up afterwards: the state repaired itself, just never while anyone was
looking. `RELEASING.md` has listed "push main" as the first publish step all
along, which is exactly what made it invisible — the prose was right, so nobody
read the script against it.

Caught by running `git rev-parse HEAD origin/main` after the success line rather
than trusting it. That is the whole of today in one sentence: every one of these
five was a message that was accurate about what had happened and silent about
what had not.

Written up as [`blind-instruments-postmortem.md`](blind-instruments-postmortem.md),
the twenty-eighth — the third in a row about verification rather than code, after
*a step is only verifiable if the check can fail* and *a repair is a change*.

---

## 2026-09-01 — reviewing the review's repairs, and shipping v0.16.0

*(The day after. No new features: one open PR reviewed four times, eight small
follow-ups, a release, and a postmortem about the reviewing itself.)*

The flag day sat open as #60 overnight. Reviewing it found fifteen things, one
of which mattered a great deal: deleting the retried `CLUSTER.KEY` read had
promoted the `\NOEXEC` probe to `netd`'s first `fsd` call and left it bare, so a
machine configured to share its disk while refusing `cpu` would enable remote
execution for the whole boot — silently, because the line that says otherwise
prints only inside the true branch. Plus an unsealed reply path, a stale
`CLUSTER.KEY` surviving in every built image because `make esp` only ever added
files, and `ninep-abi` never actually running in `make test`.

Fixed those. Reviewed again — and the second round was mostly about **the
fixes**. So was the third. So was the fourth.

That is the day's subject, and it has its own postmortem
([`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md)).
The `\NOEXEC` classification took **four** corrections, each a reasonable
response to the evidence and each incomplete: `NO_FS` only; then
`TASK_ERR_NO_SUCH_TASK`; then the discovery that the restart window produces
neither, because `read_file_chunk` grants *before* it calls and a refused grant
is `FS_ERROR`; then a bound, because the inverted predicate that fixed *that*
retried permanent errors forever. A `rm -rf $(ESP_DIR)` I added carried a comment
of mine asserting it could never expand to `rm -rf $HOME`, which it could — a
claim in a comment is not a check, and it is a `test` in the recipe now.

The thread through all of it: **every defect came from building more than the bug
needed** — a staging tree where a guarded delete would do, an inverted predicate
where a list would do, a catch-all flag scanner where three cases would do. The
last commit of the sequence was a *reduction*, and nothing has needed fixing
since.

I also wrote three checks that could not fail while fixing checks that could not
fail, including one the Makefile comment cited by name as the reason for the
change. Mutation caught all three; reading caught none. The honest reading is
that I write the check from the shape of the fix rather than the shape of the
bug, and a condition derived from the repair cannot detect the defect that
motivated it.

Then #60 merged and **v0.16.0** went out — the second deliberate wire flag day
(`AUTHNP02` → `03`, the retired format refused outright), so both ends of a
cluster upgrade together. The remaining findings became eight small PRs, one
subject each, rather than another round on a branch that had already grown too
big to review: the pre-auth fingerprinting leak, transient-vs-permanent `fsd`
failures, three transcribed constants, the dev-label tables, the
duplicate-address lookup asymmetry, sealing the post-authentication refusal, the
wire-spec docs, and a trigger. None needed a second round.

Two smaller things worth keeping. A `LineScan` split *looked* done and produced
the old message on a real boot, because `map_user` flattened the value one level
above the code that distinguished it — the same "fixed one layer away from where
the failure enters" as the `\NOEXEC` bug, and running found both where reading
found neither. And a remote `cat` failed during verification; rather than blame
my change or wave it off, I baselined it — 2 in 6 on the branch, 1 in 4 on
`main`, against a roadmap recording that flake at "2 of 6 on main" — and said so
including the sample size.

The one open item, `NET_WAIT` not being a sleep, is parked on purpose:
instrumented, the retry loop runs **zero** times on QEMU, so that rig can
observe neither the bug nor a fix. It is queued against the first Raspberry Pi
bench session, written into `testing-pi4.md` as Risk 4b and as a numbered step
of "when the boards arrive".

## 2026-08-31 (evening) — the shared cluster key is gone

*(The third and largest thread of one long day. Fourteen merged PRs, plus the
flag day open as #60.)*

Hans's answer to "what next" was per-machine keypairs, with an instruction that
shaped the whole arc: lay out steps that are each verifiable, because the last
time we did too much before reviewing, the review found too many issues to fix
cleanly. No time pressure — the only pressure is quality.

So the plan doc came first, with no code: eleven steps, each with a stated check
and a negative control, where steps 1–4 touch nothing that exists and step 4 is
a go/no-go gate. Then a hand-rolled Ed25519 built in that order — SHA-512, field
arithmetic mod 2²⁵⁵−19, curve points, scalar arithmetic, sign and verify — with
every layer checked against a Python reference that is itself checked against
RFC 8032. The gate was "does this reproduce the RFC's *published signatures*
byte-for-byte", which is a stronger question than "does it verify what it
signs", and is only askable because Ed25519 is deterministic. It passed.

Then the on-disk format (`/etc/cluster/{id,id.pub,authorized}`), a generator
that **refuses without real entropy** rather than producing a guessable machine
key, an exporter that verifies signatures, a client that makes them, and signed
replies — each proven against a Python peer holding the other half, because a
format both ends of which are mine proves only that I am consistent.

The day's real subject turned out not to be cryptography. It was **checks that
could not fail**. The relocation check used since step 1 had been reporting `0`
because `llvm-readelf` prints nothing at all for these binaries — five steps of
worthless evidence, in the one arc where everything else was mutation-tested.
Two test tables spliced into the wrong place and iterated empty lists. A bound
test named for `u64` overflow did 256 subtractions where ~4,096 are needed.
Seven mutations stacked silently because I never restored the file between runs.

And the one that mattered: the audit added a guard refusing small-order public
keys — a real universal-forgery credential — as a hand-written table of eight
hex values. Three were wrong. It accepted three genuinely small-order keys and
blocked one ordinary valid point. Its test exercised the two that happened to be
right. The fix is not a better table: `[8]P == identity` is the definition,
costs three doublings, and cannot be transcribed wrongly. Then the *replacement*
test turned out to be vacuous too — it passed with the guard removed — and the
version that survives mutation builds two different forgery families, because
the obvious one is accidentally caught by a guard on the wrong point.

Step 10 was one line in the plan: "flag day, drop the shared key." Asking what
could go wrong split it in two, and the split is what found a hole. `enabled()`
was `key_len > 0` and gated the export, inbound auth *and* outbound signing —
so deleting the key file without moving it takes the cluster dark, which I
confirmed on two guests before writing anything. 10a moved the gate and deleted
nothing; its acceptance test is a two-node cluster running with no shared key
anywhere, which is only possible while the deletion has not happened. It also
caught what the obvious refactor would have shipped: **HMAC with a zero-length
key is a valid HMAC**, so removing the gate would not make an unconfigured
export refuse MAC'd frames — it would make it accept the ones computed under the
empty key. Proven by removing that one guard and reading a guest's disk from the
host holding no secret at all.

10b was then subtraction: −412 lines, `hmac.rs` deleted entirely. What it had to
*add* was a way to send a retired frame, so that the guest refusing one is
demonstrable rather than assumed.

The counter-practice worth keeping: after a two-node run, read the **pcap**, not
the transcript. It had been captured for exactly that question since the rig was
built and nothing had ever read it — which is how the shell came to print
"remote-mounted (cluster-key auth)" on a run whose wire carried five signed
frames and zero MAC'd ones. It now reports the format each node actually used,
beside the fault count.

Full write-up: [`cluster-keys-postmortem.md`](cluster-keys-postmortem.md).

## 2026-08-31 — a remote request now says who is asking

*(Later the same day: v0.15.0 cut, and a design question answered on paper.)*

Hans asked whether a designated master auth server would close the remaining
gap — one machine as the cluster's identity authority, the others obtaining
authentication from it. The answer is yes, and it is precisely what Plan 9 does
(`authsrv`, `factotum`, `secstore` — the Kerberos family), which makes it a fit
for this project rather than a departure.

The part worth writing down is the detail that decides whether it works: a master
whose answer is relayed *by the peer* fixes nothing, because the exporter is still
trusting the peer's word, which is the entire current gap. It closes only when the
ticket is verifiable by the exporter against a key **it** shares with the master.
Everything else — N keys instead of N², one place to revoke a user, and (in the
strong form, where the user authenticates to the master at login) a defence
against a compromised node rather than only its users — follows from that one
property.

What stopped it being built today is cost that is specific to this system rather
than to the idea: tickets want lifetimes and this OS has no wall clock or time
sync; the export is one TCP connection per request, which is exactly why v0.10.0
avoided challenge-response, so a ticket wants a cache in the one task with no heap
and a stack that has hit its guard page five times; and it makes identity
cluster-wide rather than node-autonomous, which is a change to what the cluster
*is*, not just to how it authenticates. Per-machine keypairs come first: no
server, no clock, no single point of failure, and they kill the interchangeable-
members problem that is the real weakness of what shipped this morning.

Recorded under roadmap item 1 rather than built. The trigger is unchanged — this
is for when Ouroboros leaves a trusted network.

The rejected design came back, built the other way round.

Yesterday's attempt at per-user cluster identity was turned down by review with
fifteen findings, five of them independent routes by which a remote request still
reached `fsd` with root authority. The wire design was right and survived intact:
send the caller's **name** (not a uid — two nodes number their users
independently), put it **inside the MAC** (free, the MAC was already there), and
resolve it on the far side through that machine's own `/etc/passwd`, refusing a
name it does not know.

What changed is how the identity gets from `netd` to `fsd`. It was an *opt-in*
wrapper function plus a *latch* held between two messages; it is now a
**required parameter** carried **in the request**. That is the whole difference,
and it is the difference between a convention and a rule: a new export verb that
calls `fsd` does not compile until it says who it is for, and nothing can
interleave between a request and a field of that request. `fsd` refuses a
`NET_TASK` request that states nothing rather than falling back to `netd`'s own
credential — the fallback being root is what made a forgotten call site an
escalation rather than a failure.

`cpu` needed a second mechanism, because a spawned program is not `netd`'s
request: it makes its own, with its own task identity. So `netd` becomes the
mapped user for the length of the spawn and the child inherits that, restored by
a `Drop` guard rather than a line at the end. The kernel's saved-uid rule makes
it safe in both directions — `netd` may return to root because root is its saved
id, and the child cannot, because its saved id is the user's.

The verification was the interesting part of the day. The two-node ext2 rig now
shows a four-way matrix: `user` on B is refused A's `/etc/shadow` but served
`HELLO.TXT`, and `root` on B gets both — so the mechanism *discriminates* rather
than merely denying. The same script against `main` prints the hashes, which is
the negative control the last attempt's evidence lacked in the places that
mattered. And reading the output rather than the exit status caught a bug of my
own: `cpu A id` printed `uid=1000(user) gid=0(root)`, because `SET_ID` takes uid
and gid as separate arguments and I had handed it the packed word that
`GET_ID` returns. The uid was right by luck — it is the low half.

## 2026-08-30 — the question a server is actually asking

Picked up the review findings recorded against the account server. The one that
needed a kernel change turned out not to be about the account server at all.

`GET_ID(sender)` answers *"who occupies slot N now."* A server authorizing a
request is asking *"who occupied it when the request was made."* Those come
apart exactly when a caller sends and exits — `MSG_SEND` does not block — and
its slot is reaped and re-spawned before the server drains its mailbox. An
earlier fix had made `GET_ID` refuse a *dead* slot, which felt like closing
this and wasn't: a recycled slot is alive, and the message carries a bare `u8`
with no generation, so nothing distinguishes the two. And the recycled case is
not exotic — slots 5+ are the pool the shell reuses for every command. The hole
was raised against the unmerged server, but `fsd` had it too, in shipped code,
on every permission check and every fid op.

So the kernel captures the credential at send. That is a small change, and it
made two further things obvious only once it existed. Groups have to travel
*with* the identity, because `SET_ID`'s group half exists precisely so the two
can't be set out of step — capturing them separately would rebuild the hole one
field at a time. And a reply must not overwrite the captured value: `fsd` logs
to `cond` through a blocking `MSG_CALL`, so a single print mid-request would
have had it authorize against `cond`. Restricting the capture to *unfiltered*
receives turns "read it immediately, before you call anyone" from an invariant
every future server has to remember into a property of the mechanism.

The first boot hung one second in. The supervisor's health ping sends as
`KERNEL_SENDER` (0xFE), which indexes no task slot — so the credential snapshot
ran off the end of the array and panicked in the tick handler. Worth recording
*how* that was found: the same script had been run against `main` first, and
passed. A green run means nothing without a red one, and this is the second day
running that the practice has earned its keep.

The other four findings were the account server's own. Three were contained:
`/etc/shadow`'s mode was only asserted when *creating* the file, so an existing
world-readable one was never repaired and every later write landed secrets in
it while reporting success; the resolved target name was silently truncated
into a 64-byte buffer and then used as the rewrite key; and `/etc/passwd` was
read with a helper that folds an I/O error into zero bytes, which then reads as
"no such user" and sends the operator after a typo in a name that is present.

The fourth was the one where the review's suggested fix did not exist. Writing
the credential database as one whole-file write is truncate-then-write, so an
`fsd` restart or a power loss in that window leaves `/etc/shadow` empty and
locks everyone out, root included. The review said to use a temp file and
`NP_MV` — except `mv` refuses an existing destination on all three filesystems,
so replace-on-rename would be a filesystem arc of its own.

It turned out not to be needed, and the reason is a property of the *data*
rather than of the filesystem: a shadow line's salt and hash are fixed-width
hex, so changing a password leaves the file **exactly as long, with every other
byte identical**. The update can therefore be written as just the bytes that
differ, at their offset — no truncation anywhere, and the worst an interruption
can do is damage the one entry being changed while every other account still
logs in. That is a smaller guarantee than atomic rename and a much cheaper one,
and it is the right trade here: the property that matters is not "the change is
all-or-nothing", it is "a failed change never locks anyone out".

The ultra review came back with two findings, both nits — against fifteen on
the combined branch yesterday. Encouraging, and the shape of what it found is
the interesting part: both were **drift between a fact and its restatement
elsewhere**, not logic errors.

The four protected-slot guards in `syscall.rs` were correctly generalized to
`FIRST_SPAWNABLE`, but all four comments beside them still enumerated "tasks
0-4". They had been rewritten once already when netd became the fourth server;
they would need it again for a sixth. So they now state the *bound* rather than
the list, which is the only version that stops going stale.

The second was better still. `libc/include/sys.h` hand-mirrors the reserved
error band's floor, and this branch moved that floor from `MAX-33` to `MAX-38`
to fit the `ACCT_ERR_*` codes — so a C program compiled against the stale
header would read `ACCT_ERR_IO` as a *successful* return value. No live
consumer today (no C program calls `accountd`), which is exactly why nothing
caught it. Fixed both sides, and put a note at the Rust definition pointing
back at the mirror, since the definition is what someone edits next.

And a small "splitting isn't free" artifact fell out of checking it: the
roadmap already carried a note saying this header had drifted — on `main`,
where the two values actually still *matched*. The note had been written during
the combined branch's review and went to `main` in the docs split while the
code change it described stayed here. A deferral note that arrives before the
thing it defers is just a false statement with a good alibi. Removed, now that
it is genuinely fixed.

One of the tests written for it turned out to be worthless, and only mutating
the code showed it — a test named for a bound that guards nothing, because the
backward scan is already stopped by the byte the forward scan stopped on.
Renamed and re-commented to say so. A test that cannot fail is worse than no
test, because it gets counted.

Both branches then went through their own ultra review, one at a time rather
than bundled — the combined-branch mistake was still fresh enough not to
repeat. The account server came back with two findings; the kernel change with
one. All three were nits, against fifteen on the combined branch the day
before, which is about as direct a measurement of the split's value as we are
going to get.

What is worth recording is not the count but the *shape*. Not one of the three
was a logic error. Every one was a fact correctly changed in one place and left
standing in its restatement somewhere else: four comments that still enumerated
"tasks 0-4" beside a guard that now covers 0-5; a C header still pinning the
reserved-error floor at `MAX-33` while the Rust constant had moved to `MAX-38`,
so a C caller would read `ACCT_ERR_IO` as a *successful* return value; and a
doc link pointing at `SENDER_IDS`, which does not exist.

Then the same failure turned up twice more, found two other ways. Reconciling
the old combined branch before closing it — 207 identifiers introduced, 203
present on `main`, the four absent all verified renames — surfaced a manpage
still telling users the group list reaches the kernel via `SET_GROUPS`, a
syscall that was designed and never shipped. And chasing the broken doc link
meant running `cargo doc`, which reported **nine** unresolved links, eight
older than this week. Nine had accumulated for the obvious reason: nothing
reads that output, so the signal was already useless by the time the ninth
arrived. Fixed all of them, so the next one is visible rather than buried.
Finally `CLAUDE.md` — the file that exists to be read before touching the code
— still described ten task slots, four servers, and two different, both wrong,
crate counts.

Five instances in one day, found five different ways, and **none** were visible
to the compiler or to any test. That is not a coincidence, it is the category:
a comment, a manpage, a doc link, an unused C constant and a prose count are
precisely what neither checks. Yesterday's lesson was that a green signal is a
claim rather than evidence. Today's is narrower and, I think, more useful: the
code has one copy of each fact and the prose has several, nothing keeps them in
step, and the drift is invisible until someone goes looking. `cargo doc`
returning zero is now one small piece of machinery that will notice — for one
of the five kinds.

Written up as `docs/asking-the-right-question-postmortem.md`, the
twenty-fourth.

A sixth instance turned up during the documentation pass that followed, and
this one was mine from the same day: I had been calling `accountd` "the fifth
server" everywhere, because it sits in protected slot 5. It is the **fourth**
server — `fsd`, `cond`, `netd`, `accountd` — and slot 5 is its task number.
Protected slots 0 and 1 are the boot shell and idle, which are not servers. The
phrase had reached ten places, including merged code comments and the
postmortem written an hour earlier, before counting the servers instead of
trusting the phrase caught it. Exactly the failure the postmortem describes,
committed while describing it: a number restated from memory rather than
derived, in prose that nothing checks. The arc that started with "the OS has no idea who you are" is
finished; the only thing left in it is per-user *cluster* identity, which is
the one place a remote request still arrives as root.


---

The afternoon was a second day's worth, and it went differently.

`ls -a` was reported as broken by the one mechanism that has caught nothing else
all week: someone using the system. It turned out never to have worked — the
manpage promised `.` and `..` from the day it was written and no filesystem arm
ever returned them. Worth keeping the promise rather than deleting it, because
with enforcement live `ls -la` is the only way to see a directory's *own* mode.
Synthesized in `ls`, not `fsd`, since every arm's dot-filter is load-bearing for
`tree`, globbing, and any future recursive `cp`.

Then per-user cluster identity, the users arc's last item, promoted that morning
because `accountd` had put a privileged writer on the far end of a hole that
until then only exposed a readable file. The design questions were real and I
think the answers were right: send the **name** rather than the number, because
two nodes number their users independently; put it inside the MAC, which cost
nothing because the MAC was already there; and let the far side resolve it
through its own `/etc/passwd` and refuse a stranger.

The implementation was wrong, and a review found fifteen things.

Five of them were independent ways a remote request still reached `fsd` with
root authority. The demo I had led the PR with — an unprivileged user refused
another node's `/etc/shadow` — passed with **all five present**, because `chown`,
`writeat` and `cpu` each defeated it by a different route. My verification had
been a live two-node run with a negative control, which is the strongest
evidence I know how to produce, and it certified a branch whose central claim
was false.

The lesson is not "test more". It is that **four of the five traced to one
design decision**: opt-in `_as` twins plus a mutable latch. Opt-in means a
missed call site is silent. A latch means anything else touching `fsd` destroys
it. I had even written down the invariant — "is every export-path call
proxied? is one grep" — and then run that grep against the four helper names I
knew about rather than against the property, missing a fifth path I had
forgotten existed. A safety property you have to remember at each call site is
not a safety property. The rebuild makes the wrong thing *unspellable*: identity
as a required parameter, carried in the request rather than latched.

Three things in the review's list were about the test infrastructure, and one of
those was worse than the feature bugs. My own refactor of the console harness
had dropped `SError` from the health bar — so every "0 aborts" I had reported
after that point, including on the feature under review, was a weaker claim than
the ones before it. The number had not changed; what it meant had. That is the
same failure as the day's other six, in the one place I was leaning hardest.

So the rig was split out and landed first: an ext2 two-node pair (the FAT32 one
records no mode, so a permission test on it passes before a fix and after it),
`CLUSTER.KEY` at 0600 on the disk where modes are enforced, the health bar
restored, and a harness that can no longer report success having typed nothing.

Last, a live crash the review had noticed in passing: one malformed export frame
panicked `netd`, and three of them take the network down for the boot. Two more
of the same class turned up in the sweep, by a different mechanism — a wrapping
add rather than an unclamped start. Fixed as a class, and proven both ways with
the host-side Python peer, which is the only thing that can reach that code path
at all. Worth noticing that the foreign observer earned its keep on the same day
I argued for keeping it alive.

Two things I want to remember from the afternoon rather than the morning. The
first: I nearly merged the crash fix on a verification that never touched the
code I had changed — I altered write paths and tested reads. Going back to
exercise writes properly is what surfaced an intermittent remote `cp` failure,
now recorded rather than dismissed. The second: a green test suite, a live
two-node run, and a negative control all agreed the cluster branch was good, and
a foreign reader disagreed fifteen times. The reviews are not a formality at the
end of the work. On a security boundary they are part of the work.


## 2026-08-29 — finishing the security tier, and learning to distrust green

A day of *finishing* rather than building: the follow-ups the users arc had
deferred, plus the small parking-lot items left over from the day before. It
started as one branch and ended as six pull requests, which is the whole story.

The morning was features. A real **`regex` crate** (pure, `no_std`, no heap,
host-unit-tested) behind `grep`'s patterns — with an explicit backtracking stack
rather than host recursion, because a recursive matcher's depth grows with the
*input*, and `a*` over a 256-byte line is 256 frames against a 32 KB guarded
stack. Then **symbolic `chmod`** (`u+x`, `go-w`, `a=rx`, copy-form `g=u`,
conditional `X`), **`chown` by name**, **`/etc/skel`**, an **atomic `useradd`**
(ordered so the `/etc/passwd` write is the single commit point, with rollback
before it and warn-only convenience after), one shared account-file reader
replacing three near-identical copies, the **picolibc stdout** follow-up, and a
**virtio-rng** driver behind a `RANDOM` syscall so password salts stop being
clock-derived. That half merged cleanly as #26.

Then the security tier — `/etc/shadow`, supplementary groups, ancestor-`x`
traversal, an `accountd` server so a user can change their own password — and
this is where the day turned. A local code review found 10 findings. Fixing them
and re-running found 15. Fixing *those* found 15 again, and two separate
`max`-effort passes over the same diff returned **largely disjoint sets**. Worse,
my own fixing was producing roughly **one new bug per round**: a group escalation
introduced while closing a group finding, a truncation window opened while fixing
an ordering bug, and one "fix" that was a silent no-op the kernel's own range
check rejected — caught only by booting it.

The user asked whether to roll the branch back entirely. Checking the history
said no: the *worst* findings pre-dated the branch. But the non-convergence was
real, and the diagnosis was that **a diff too big to review is also too big to
fix** — every fix lands in a context nobody is holding any more. So we split it,
one module per PR, and merged them in order.

The split paid immediately. Reviewing `/etc/shadow` alone surfaced a **lockout**
three passes over the combined diff had missed: the shell reads an account file
into a 2 KB buffer and returns `0` on overflow, which for `/etc/passwd` correctly
means "no accounts, start a root session" — and for `/etc/shadow` means "this
user has no secret", refusing every password, root's included, with no way back
in. Shadow lines are ~90 bytes to passwd's ~30, so shadow crosses first, at ~23
accounts. The function's comment justifying the behaviour was written for its
first caller and was false for its second, word for word. Fixed by streaming the
lookup one line at a time, so the size of the database stops deciding whether
anyone can log in — and verified in *both* directions, reverting only that change
to watch root get locked out with a correct password.

The split also **dropped** things, which is the other half of the lesson: nine
documentation files and a one-constant fix raising `IMG_CAP`, without which
`FSD.BIN` (137 KB against a 128 KB cap) meant **`fsd` had not been restartable at
all** — announced by one boot warning among forty, scrolling past before the login
prompt. I found it by accident, reading unrelated output. Nothing else would
have. Reconciling the original branch against the union of the pieces, before
closing it, is now the rule.

Two more green signals lied before the day was out. `usermod -G ""` cleared the
group list *by accident* — only because no group is named `""` — while printing
an error-shaped message and exiting 0. And GitHub called #30 `MERGEABLE` when the
merged tree did not compile: #32 changed a function's return type, #30 added a
fourth call site on lines #32 never touched, so there was no textual conflict and
no meaning either. Ninety seconds of merging-and-building locally caught it.

Also learned, expensively: squash-merging a PR whose branch is the *base* of
another does not retarget the stacked PR — it **closes** it, and GitHub then
refuses to reopen it. #29 lost its number that way (its work is #31). The fix is
ordering: merge without deleting the branch, rebase the stacked branch, retarget
it, *then* delete. Applied to #31 → #30 it worked with nothing lost.

Ended with `main` carrying #26/#27/#28/#31/#32 and the docs caught up to what
the system actually is. `accountd` (#30) is rebased, builds and boots, with five
review findings deliberately left for a fresh session — it has the largest new
surface, and the day had already demonstrated what fixing under pressure
produces. Full write-up in
[`review-and-split-postmortem.md`](review-and-split-postmortem.md).

---

## 2026-08-28 (cont.) — closing the security arc: account management

After the libc arc, one more the same day: **closing out the users/permissions
work** with the tools to actually manage accounts on-device. The earlier arc
built identity + login + enforcement; this one built `passwd`, `useradd`,
`groupadd`, `usermod`, name resolution (`id`/`su` by name), `/etc/group`, and
per-user home directories.

I started by scoping the gaps and putting the real design forks to the user:
**who may mutate `/etc/passwd`** (a non-root user changing their own password
needs a privileged path we don't have), **how deep groups should go**, and
**where salt entropy comes from**. The decisions: root-only tools built on a
*shared helper* (self-service `passwd` deferred to a later `accountd`/setuid tier
the helper is built to slot into — option 1 is a strict subset of that server);
**primary-gid** groups (the kernel identity is one packed word, so supplementary
membership is a later tier); and a **clock-derived salt** (weak, documented, with
a virtio-entropy RNG as the noted upgrade).

Everything shared lives in a new pure `accounts` crate (no I/O, no syscalls),
which made it **host-unit-testable** — and the tests immediately caught that my
own fixtures used truncated hashes (the parser correctly rejects a non-32-byte
hash). The tools are all root-only, PIE-safe, over `ulib` + `accounts`.

The interesting part was what the arc *exposed*. Before this, everything ran as
root; the moment a non-root `user` had a home under `/Users` it was meant to
write in, `echo hi > ~/note.txt` was **denied** — ext2 created every new inode
owned by root, so the file was born root-owned and the follow-up write correctly
refused. Fixed by having `fsd` stamp the caller's identity (`set_creator`) onto
new inodes. Then the code-review agent caught a second-order bug I'd introduced:
`write_file`'s *overwrite* branch preserved the old file's mode/links but not its
owner, so overwriting a file now chowned it to the writer — POSIX overwrite never
chowns; fixed to preserve the old uid/gid.

Testing was scripted end-to-end on `run-image-ext2` (a paced fifo feeding the
guest shell after the login prompt — the PL011 has no RX FIFO, so input dropped
at boot bit hard until I paced it). Login `user` → `id` names, `/Users/user`
home, `~`, home-write allowed, `/`-write denied; login `root` →
`useradd`/`groupadd`/`usermod`/`su carol` → `uid=1001(carol) gid=1002(staff)`.
Shipped as PR #22 (merged), including the `/User` → `/Users` rename (a home-base
spec call the user made). Full retrospective in
[`account-management-postmortem.md`](account-management-postmortem.md).

## 2026-08-28 (cont.) — the libc arc, ending at a real C library (picolibc)

After the users/permissions arc, a second arc the same day: making Ouroboros run
C. It went in six steps — (1) a first C program (clang → our loader, no kernel
change), (2) `.data`/`.bss` loader support so C globals/statics work, (3) a
hand-rolled minimal libc (`printf`/`malloc`/`string.h`), (4) file I/O + a
stdout-target-aware `write` so a C program works in a pipeline, (5) fids
(server-side open-file handles in `fsd` — a POSIX fd *is* a 9P fid), and (6) the
one I'll write up here: **porting picolibc**, the real C library.

The whole port turned out to be a build-and-link exercise with **no kernel or
loader change** — which is the interesting part. The reason is that the porting
layer was already built. picolibc's stdio, compiled with `-Dposix-console=true`,
bottoms out at `write`/`read`/`open`/`sbrk`/`_exit` on fd 0/1/2 — exactly the
syscall stubs steps 3–4 already wrote in `libc/src/os.c` and `file.c`. So
picolibc's `libc.a` links against *our* `crt0` + stubs (recompiled against
picolibc's own headers so `struct stat`/flag layouts match), we drop our own
`stdio.c`/`stdlib.c`/`string.c`, and picolibc supplies `printf`/`malloc`/
`memcpy`/`qsort`/…

Two things had to be right. First, **the relocations**: picolibc had to be built
`-fPIC` so it self-relocates the way every Ouroboros program does — the linked
binary came out `static-pie` with 22–23 `R_AARCH64_RELATIVE` and **zero
`ABS64`** (`llvm-objdump -R`), so the loader eats it unchanged. That the recurring
ABS64 trap *didn't* bite here is precisely because `-fPIC` is the whole contract
our loader depends on. Second, **two compiler-rt builtins**: the float-printf
(ryu) path calls `__lshrti3`/`__ashlti3` (128-bit shifts), and macOS ships
compiler-rt only as Mach-O — unusable for our ELF link. clang lowers a variable
128-bit shift *to those very symbols*, so I couldn't implement them with `>>`
(infinite recursion); they split the value into two 64-bit halves and shift
those. Thirty lines in `libc/pico/builtins.c` and the link closed.

The regeneration friction is worth recording: modern clang makes
implicit-function-declaration a hard error, tripping a couple of picolibc 1.8.9
tinystdio files, and Apple clang defaults to the Mach-O linker, which rejects
meson's GNU-linker probe. Both are handled in `libc/picolibc-cross.txt` (demote
the warning; `--ld-path=…/ld.lld` to force LLD). The built lib is committed
stripped (`third_party/picolibc-prebuilt`, ~2.1 MB) so a checkout needs no
meson; `scripts/build-picolibc.sh` regenerates it and the 39 MB source is
gitignored.

The proof: booted, logged in as root, ran `/bin/CPICO` (`libc/picodemo.c`) —
`pi=3.14159  e=2.718e+00  g=1.23457e+06`, `qsort: 1 2 3 5 8 9`, `snprintf`,
`malloc`, `strtol`, exit 0. Full float formatting the hand-rolled printf never
had. What's left in the arc is no longer "invent the mechanism" — it's "port one
more program" (SQLite, a small C compiler). One honest follow-up: picolibc's
posix-console stdout is unbuffered, so console output is one IPC round trip per
character — correct but chatty; line-buffering it is the natural next tidy-up.

## 2026-08-28 — the users/permissions arc: from stored metadata to an enforced login

A full arc in one span: the step from "single implicit user, no permissions" to
a real Unix-style identity-and-permission model. Five pieces, each built and
merged in turn, plus a couple of asides.

**The mode/owner surface (read side).** The `stat` op (`NP_STAT`) already
carried size/dir-flag/mtime; it grew a POSIX `mode`/`uid`/`gid` triple guarded
by a `mode_valid` byte (20→27-byte record, appended so the old fields decode
unchanged). ext2 fills it from the inode (`read_inode` now parses `i_uid`/
`i_gid`); FAT32/exFAT/`/proc` return `None`. `ls -l` renders a real permission
string (`drwxr-xr-x`, setuid/setgid/sticky and all) plus owner columns — real on
ext2, synthesized-with-a-dash owner elsewhere. This was the keystone the whole
arc waited on — the roadmap had ranked it #1 precisely because everything else
hangs off it.

**chmod/chown (write side).** The write twins. Two verbs (`NP_CHMOD`/`NP_CHOWN`),
a new `FS_ERR_NOT_SUPPORTED` for the filesystems that can't model it, and — the
one subtlety — ext2 patches the inode field **in place** with a surgical
read-modify-write, *not* the existing `write_inode` (which zeroes the whole slot
for a fresh inode and would wipe a pre-existing file's timestamps). `chmod`
preserves the type nibble. Validated the way this project validates
disk-format work: not against our own reader but against **macOS's `e2fsck`**
(clean after the edits) and `debugfs` (read back `Mode: 0700 User: 7 Group: 8`
exactly). `/bin/chmod` (octal) and `/bin/chown` (numeric `uid:gid`).

*An aside that fell out of testing:* manual pages had only ever been staged onto
the FAT image; the ext2 and exFAT images had `/bin` but no `/man`, so `man`
failed there. One Makefile line each. Also a small `docs/resources.md` +
"develop on QEMU first" note for the eventual Pi work — QEMU has `raspi3b`/
`raspi4b` machines, so Pi bring-up needn't wait for boards.

**Identity (step 1).** A uid/gid per task. The load-bearing decision — and a
deliberate reversal of the roadmap's tentative "identity is probably a userland
construct" note — was to put the binding **in the kernel** (`IDS`, one packed
`(gid<<32)|uid` per slot, `SET_ID`/`GET_ID`). The reason is unforgeability: the
kernel is the only thing that knows an IPC message's real sender, so it's the
only trustworthy place to bind identity to a task. Names/passwords/policy stay
100% userland. Root-gated (only root may change identity), inherited across
spawn, with `/bin/id` and a `su` builtin, and the prompt showing `#`/`$`.

**Login (step 2).** This is where the arc got interesting. I started building
`login`-as-init (a separate boot process spawning a per-session shell, the
classic getty model the user had picked) and hit a wall: the capability model
bakes in "slot 0 = the shell" (`caps_for_slot`, `TO_SHELL = 1<<0`), so moving
the shell to a spawned slot would mean rewiring the whole security policy and
re-validating every IPC flow. I surfaced that and we switched to a **saved-uid**
model instead: the shell stays task 0, `login` drops it to the user, and logout
restores root (POSIX saved-set-uid: a non-root task may `SET_ID` back only to
its saved identity) to re-prompt. Same login→session→logout→re-login experience,
zero capability surgery. Passwords: `/etc/passwd` = `name:uid:gid:home:salt:hash`
with `SHA-256(salt‖password)`, salts precomputed at build time so login only
verifies. Two real bugs on the way: an **escalation hole** (the shell's saved id
is root, so a logged-in user could `SET_ID(0)` — closed by making `su` root-only
at the shell, since a user's children can't restore root and logout re-prompts),
and a **`want=1024 > FS_DATA_MAX`** bug where login couldn't even read
`/etc/passwd` (the shell's inline read caps at 512) — which I first misread as a
mount race until the actual error code showed it was the buffer size.

**Enforcement (step 3).** The payoff: `fsd`'s `check_access` finally *checks* the
caller (via `GET_ID(sender)`) against the target's owner+mode and refuses with
`FS_ERR_PERM`. Classic Unix — root bypasses, then owner→group→other; reads need
`r`, writes `w` (on the file or the parent when creating), namespace changes `w`
on the parent, `chmod` owner-only, `chown` root-only. ext2-only; the FAT boot
disk stays open. A deliberate first cut: the object of the op and its parent are
checked, but not the search (`x`) bit on every ancestor directory — kept out so
the logic lives centrally in `fsd`'s dispatch rather than woven into each
filesystem's path walk. No kernel change needed — `GET_ID` already existed.
Verified on ext2: as root, set up `/secret` (600) and a user-owned `/u`, drop to
the user — `cat /secret` and `touch` in root-owned `/` both denied, `touch /u/mine`
succeeds, `/bin` programs still run.

The arc is complete: mode/owner → chmod/chown → identity → login → enforcement.
The roadmap's #1 gap — a single implicit user — is closed. Design retrospective:
[the users-and-permissions postmortem](users-and-permissions-postmortem.md).

## 2026-08-27 (cont.) — exporting the environment to child programs

The shell's env was a dead end: `set FOO=bar` and `$VAR` expansion worked, but
a spawned program had no way to *read* the variables. Fixed by exporting the
env at spawn — deliberately built as a near-exact copy of the argv ABI, since
it's the same shape (a blob of length-prefixed entries delivered kernel-side
and fetched by the child). Three syscalls (`ENV_STAGE`/`GET_ENVC`/`GET_ENV`),
the same `[count][len][…]` encoding, a per-task store cleared on death, ulib
helpers, and a `/bin/printenv` consumer to prove it.

The one wrinkle: `SPAWN`'s four argument slots are already full (region offset,
stdout target, argv length, cwd length), so there was no room for an env
length. Rather than widen the ABI, `ENV_STAGE` *latches* the blob and the next
`SPAWN` consumes the latch — clean, and the single-core sequential shell always
stages immediately before spawning.

Then a genuinely instructive bug. It all wired up, the kernel debug showed the
env attached (`env_len=17`, then `28` after two `set`s), the child's `GET_ENVC`
returned the right count — but `printenv` printed nothing. `GET_ENV` was
returning "no such entry". The cause: I'd sized the read buffer at `ENV_MAX`
(2048, the *whole-blob* size), but `GET_ENV`'s out-pointer is range-checked like
every user pointer, and the boundary caps a user range at `MAX_USER_LEN` = 512.
So a 2048-byte capacity failed the check and the syscall bailed before copying.
The fix is to read one `NAME=VALUE` entry at a time into a small (256-byte)
buffer — an entry is at most ~153 bytes. Worth a doc note on `GET_ENV`, because
it's a non-obvious asymmetry (the *store* is big, each *read* is small).

`printenv` (and `printenv | grep PATH`) now show the exported env; closes the
last of the small roadmap follow-ups.

## 2026-08-27 (cont.) — `sort`, the filter that can't stream

Wrote the one standard filter that had been left for last, because it doesn't
fit the shape of the others. Every existing filter (`grep`, `head`, `nl`, …)
streams: fixed line buffer, emit as you go. `sort` fundamentally can't — it
has to see the whole input before the first output line — so it needs to
buffer everything, against this project's no-heap-allocator, fixed-buffer
constraint.

The fit was `ulib::heap` (the 256KB per-program region already used by the
pager). I split it: input bytes in the front, and a line index (start+len per
line) reinterpreted from the 4-aligned tail via `align_to_mut::<u32>()` — so
the index costs no stack (the spawn stack is only 32KB with a guard page, so a
big on-stack index was out). Sorting is an in-place **heapsort** over that
index (iterative, no recursion, no scratch), which keeps the *working* memory
O(1) on top of the input it has to hold anyway. Flags `-r/-n/-u/-f` are cheap
additions on top. The "buffer everything" limit is a documented size cap:
truncate past the heap and warn on the console (stderr-style, not into the
sorted stream).

All variants check out on QEMU — lexical, reverse, unique, and numeric
(`100 30 9` → `9 30 100`) — no faults. Nice that the interesting constraint
(can't stream) had a clean answer (heap + index + heapsort) rather than forcing
an allocator. Closes the last of the small filter follow-ups.

## 2026-08-27 (cont.) — a builtin anywhere in a pipeline

The last of the pipeline open-gaps: a non-first stage couldn't be a builtin, so
`cat f | ps` was refused. The roadmap had this filed as "reasonable, not worth
changing," and it's true there's no *useful* builtin that transforms a stream —
but the reason is the interesting part, and it's what made the fix clean. A
builtin runs in the shell (not a task) and none of them read stdin, so a builtin
can only ever be a pipeline *source*. A non-first builtin therefore reduces to:
run everything upstream for side effects, throw its output away, and let the
builtin source the rest. `ls | ps | grep runnable` = (ls drained) then
(ps → grep).

So I refactored `cmd_pipeline` into a classifier that finds the single builtin
and a `run_head_pipeline(stages, …, sink)` core taking a `PipeSink` of
`Console | Redirect | Drain`. A builtin at position k>0 runs `stages[..k]` with
the Drain sink (a new no-limit discard loop — draining `cat bigfile | ps`
shouldn't hit the 256KB redirect cap) then `stages[k..]` with the real sink.
Nicely, the `> file` redirect from the previous change just became the Redirect
sink, so console/file/drain are now one unified path. Two builtins are refused
(only one source per pipeline).

All the positions check out on QEMU — builtin last (`cat tf.txt | ps`), middle
(`ls | ps | grep runnable`), first (unchanged), last+redirect
(`cat tf.txt | ps > psout.txt` → the table in a file), and `env | ps` gets the
"only one builtin" message. A case where the honest reduction ("a builtin
discards its upstream") turned a "not worth it" into a small, uniform change.

## 2026-08-27 (cont.) — grep flags, and a YIELD syscall for prompt early-exit

Two more open-gap filter follow-ups. `grep` gained `-i`/`-v`/`-n` (case-fold,
invert, line-number) — the substantive "no longer case-sensitive" fix;
substring matching stays (regex is a bigger arc). Straightforward, and all three
plus the combined `-in` form check out on QEMU.

The `head` half was more interesting than it looked. The gap said head "relies
on the producer's send-timeout" when it exits early. Digging in, that turned out
to be half-untrue: a send to an *exited* consumer already fails non-transiently
(`task_exists` is false for a zombie), so `pipe_out` returns immediately, not on
the 150-tick deadline — the old comment overstated it. The *real* residual cost
was a busy-spin: while head is still draining its final buffer, a producer that
fills head's mailbox gets `MSG_ERR_FULL` and re-sends in a tight loop until the
next tick preempts it and lets head run. Under QEMU's ~37 ms ticks that's
invisible, but at a 1-second hardware tick it's a real ~1 s stall — and a
busy-spin burning the CPU the consumer needs.

There was no way to hand the CPU to the consumer cooperatively — no yield
primitive existed. So I added one: a `YIELD` syscall (57) that saves the caller
as still-runnable and switches to another runnable task, *skipping idle unless
it's the only thing runnable* (the subtlety — a naive yield could park on the
always-runnable idle task and waste exactly the tick it was trying to save).
`pipe_out` now yields on `MSG_ERR_FULL` instead of spinning, so the consumer
runs, drains or exits, and the producer's retry then succeeds or fails fast. It
benefits every pipe, not just head. `tree / | head 3` — a producer that would
generate the whole filesystem tree feeding a consumer that wants three lines —
returns promptly, no hang, no fault.

Nice case of the gap note pointing at the symptom (head) when the fix belonged
one layer down (the shared producer path + a missing scheduler primitive).

## 2026-08-27 (cont.) — `a | b > file`: pipelines compose with redirection

Small, satisfying one: the shell refused `a | b > file` (pipeline plus
redirect) because the last stage wrote straight to the console. The fix was
mostly a reordering — `run_line` now parses the trailing `>`/`>>` redirect
*first* (leaving the `|` in the command part), and when a redirect is present
the pipeline's last stage is spawned with its stdout pointed at the shell
instead of the console. From there it's the *exact* path a single `cmd > file`
already uses: `capture_program_output` folds the stream into the 256KB heap
capture, `finish_redirect` writes it to the file. So almost no new
mechanism — the redirect capture and the pipeline machinery already existed;
they just needed to meet. The only genuinely new handling is the error path
(kill+reap the producers if the capture fails so nothing hangs) and *not*
handing the last stage the keyboard when its output is going to a file.

Verified on `make run-image`: `ls /bin | grep PW > pwout.txt` then a `>>`
gave a two-line file (confirmed by the guest and by mounting the image on the
Mac), and `env | grep PATH > envout.txt` proved a builtin-head pipeline
redirects too (`PATH=/bin`). Plain pipelines still go to the console; no
faults. Closes another of the small open-gaps.

## 2026-08-27 (cont.) — GPT CRC validation + backup fallback

Closed the "GPT parsed but CRCs not validated on read" open gap. `fsd`'s
`partition::discover` trusted the "EFI PART" signature and read the entry
array without checking either CRC32. Now `try_gpt` validates the header CRC
(over `HeaderSize` bytes, CRC field zeroed) and the entry-array CRC (a
table-free bitwise CRC-32, reflected `0xEDB88320`, folded incrementally so
the array is CRC'd sector-by-sector), and a corrupt *primary* falls back to
the *backup* GPT at the last LBA; both corrupt → no partitions. Also detect a
GPT by a `0xEE` protective-MBR entry, not just the header signature, so a
disk whose primary signature is itself smashed is still recovered from the
backup.

The interesting part was *testing* it, because two layers of firmware get in
the way. I flipped a bit in the primary header of `espgpt.img` and booted it:
`fsd` mounted — but when I checked the on-disk image afterward, the primary
header was *valid again*. EDK2/OVMF **auto-repairs a corrupt primary GPT from
the backup during boot**, so by the time `fsd` reads the disk there's nothing
wrong with the primary. And corrupting *both* copies just makes the firmware
refuse to boot at all (`CheckCrc32: Crc check failed` → it drops to the EFI
shell), so the kernel never runs. Either way a QEMU boot can't actually
exercise `fsd`'s fallback. So I wrote a small host harness that `#[path]`-includes
the **real `partition.rs`** and drives it through a mock `Disk` over the raw
image bytes — no firmware in the loop. Five cases pass: clean → mounts on
primary; corrupt-primary-header and corrupt-primary-array → both fall back to
the backup; corrupt-both → zero; plain-MBR → still one partition (no
regression). I also cross-checked the bitwise CRC against `zlib.crc32` and the
values `mkgpt.py` writes — byte-identical for both the primary and backup
headers. The clean `run-image-gpt` boot still mounts, so nothing regressed at
the real-boot level either.

Nice reminder that "test it on real hardware/firmware" isn't automatically the
strongest test — here the firmware's own robustness (repair + reject) actively
*hides* the code path under test, and a host harness driving the real module
was the honest way to see `fsd`'s fallback actually fire.

## 2026-08-27 (cont.) — FAT32 long-filename *write*

Closed the "FAT32 long-filename write" follow-up from the roadmap. LFN was
readable but not creatable: `fsd`'s FAT32 arm only ever wrote 8.3 short
entries, so `touch archive.tar.gz` or `write LongFileName.txt …` failed with
"invalid name". Now a name that doesn't fit 8.3 gets a generated `NAME~N`
short alias plus a contiguous run of LFN entries carrying the real name;
8.3-fitting names still take the old plain-short-entry path byte-for-byte, so
nothing existing changed.

The pieces, all self-contained in `fat32.rs`: `insert_named_entry` (the one
funnel `mkdir`/`touch`/`write_file`/`mv` route through, deciding 8.3-vs-LFN),
`generate_short_alias` (`~N` incremented until unique — the base trimmed so
even `~999999` fits 8 chars), `build_name_entries`/`put_lfn_chars` (the
on-disk entry run, checksum-stamped, written high-sequence-first), and
`write_entry_run`/`place_entries` (find a physically-contiguous run of free
slots, extend the directory by a zeroed cluster if needed). I also did the
bonus the roadmap flagged: `free_entry_with_lfn` frees a deleted file's LFN
run, so `rm`/`rmdir`/`mv` no longer strand orphaned LFN entries — it matches
the target by exact on-disk location (so an `mv` dst inserted into the same
directory isn't mistaken for it) and only frees a preceding run whose
checksum matches.

Testing leaned on the foreign-observer discipline this project keeps: drove
the guest shell unattended over `make run-image` (the FIFO recipe), creating
`longfilename1.txt`/`longfilename2.txt` (aliases collide →
`LONGFI~1.TXT`/`LONGFI~2.TXT`), a long-named directory, a nested long-named
file, then `rm` and `mv` of long names. The guest read every name back; then
**macOS's own FAT driver** mounted the guest-written image and showed all the
long names and contents; and **`fsck_msdos`** passed its directory phase with
no orphaned-LFN/checksum/duplicate-short-name complaints — the real proof, an
implementation that shares none of my code. Only wrinkle was a pre-existing,
advisory FSInfo free-count warning (`fsd` never maintains that hint on
writes), unrelated to LFN. No faults, no guard-page overflow. Left
case-preservation for lowercase-8.3 names (`File.txt` → `FILE.TXT`) alone on
purpose — the gap was `>8.3` names, and that's closed.

## 2026-08-27 (cont.) — reading the neighbours: Redox OS and the Pi-4 tutorials

Not code — a research pass Hans asked for, on two outside resources: Redox OS
(the mature Rust microkernel) and the `rust-raspberrypi-OS-tutorials` repo. I
ran two parallel research agents and wrote up `docs/research-redox-and-pi.md`.

The Redox half had a genuinely useful conclusion I didn't expect going in:
**Ouroboros already structurally *is* a small Redox.** Both arrived
independently at userspace fs/net/console daemons, supervised driver restart, a
uniform resource protocol (their URL "schemes" ≈ our 9P verbs + per-task
namespaces), and a non-POSIX kernel with POSIX pushed to a userland libc —
which is exactly the conclusion our own posix-divergence postmortem reached. So
the "what to adopt" list is short and specific rather than sprawling: `relibc`
(the existence proof for our libc-portability plan — and it targets *both* Redox
and Linux, so it's host-testable), making the namespace *itself* the capability
boundary (Redox's null-namespace sandbox unifies two things we built separately
— namespaces and the send-mask), and RedoxFS's CoW+checksums shape for the
cluster-redundancy direction. Schemes vs. 9P turned out to be the *same*
architecture in different spelling, so there's nothing to copy there except the
namespace-as-security-boundary idea. I threaded those three into the roadmap's
portability/security/redundancy north-stars where they'll actually be worked.

The Pi half is a bring-up cookbook for when the boards arrive. The load-bearing
call: we boot via UEFI, the tutorials boot a raw `kernel8.img` — two different
worlds — so the plan is to try the pftf/RPi4 EDK2 UEFI+ACPI firmware *first*,
because a Pi 4 under it exposes UEFI + ACPI + a GOP framebuffer, and our
existing stack (UEFI loader, ACPI MADT → `gicv2.rs` for the Pi 4's GIC-400,
GOP `fbconsole`) should carry over largely unchanged. The tutorials stay the
reference for the raw BCM2711 facts (peripheral base `0xFE00_0000`, the GIC
addresses, PL011-not-mini-UART, the serial rig) if UEFI proves unworkable.

## 2026-08-27 (cont.) — keeping the roadmap forward-looking

A cleanup Hans asked for: the roadmap had grown to 1700 lines, half of it
finished work. Split it — `ROADMAP.md` back down to ~530 forward-looking lines
(open frontier, remaining follow-ups, north-stars, open gaps), and the finished
arcs (microkernel bring-up, network stack, filesystems, disk management,
standalone binaries, pipelines, and the done parking-lot entries) moved verbatim
into a new `roadmap-completed.md` — the plan-shaped companion to CHANGELOG.md's
condensed milestone log. The moved text keeps its original "deferred" tails for
context, with a note that those open items now live back in the roadmap.

## 2026-08-27 (cont.) — shell wildcards and tab completion

Two classic interactive-shell features. Globbing was the straightforward one:
expand `*`/`?` tokens against the filesystem in `run_line` before dispatch, with
the standard iterative backtracking matcher and bash's "no match stays literal"
rule. Tab completion was a bit more fiddly — find the current word, list the
directory, and either complete a lone match (replacing the typed prefix with the
real filename case), extend to the common prefix of several, or list them and
redraw the line.

The interesting part was a link failure that cost a proper bisection: the shell
suddenly wouldn't link, `R_AARCH64_ABS64 ... referenced by core`. The project has
long known that `core::fmt` can't be PIE-linked, but I hadn't hit it in a while.
Stubbing pieces one by one showed it wasn't `fs_list_dir` or `resolve_path`
(already live) — it was the `&str` slicing itself. Slicing a `&str` by a runtime
index emits `str`'s char-boundary panic, which *formats* the offending string,
which drags in the unlinkable `core::fmt`. Byte slicing doesn't. So the glob and
completion code got rewritten to work entirely in `&[u8]`, converting back with
`from_utf8` only where a `&str` was actually needed. Worth a memory: never slice
a `&str` by a runtime index in a `/bin` program. After that, both features came
up clean — `echo *.txt` matched, `cat ba<Tab>` filled in `BANANA.TXT`.

## 2026-08-27 (cont.) — man pages

`man <command>`. The first question was how much formatting the console can do,
and the honest answer settled the design: `cond`'s ANSI parser handles clear and
home and *silently drops everything else*, including SGR — so bold and colour
would render as nothing on the framebuffer (they'd work over the QEMU serial
line, but that's not the real target). So: plain text, with UPPERCASE section
headers doing the visual structure the way real man pages always have. The
implementation is deliberately boring — pages are plain files under `/man/`, and
`man` is basically `cat` with a `/man/` prefix, a friendly "no manual entry"
message, and `\n`→`\r\n` translation for the console. Keeping the pages as files
(not baked into the binary) means adding one is just dropping a file in
`manpages/`. A nice incidental check: `man partition` exercises FAT long
filenames (9 chars, past 8.3), and it read back fine. Wrote pages for the whole
command set — the file commands, the filters, the net commands, the builtins —
so there's a real reference now, not just terse `-?` lines.

## 2026-08-27 (cont.) — `-?` usage help everywhere

A small usability sweep: every command that takes arguments now answers `-?` with
a one-line usage. The trick to keeping it from being tedious was a single ulib
helper, `usage_if_requested(b"usage: ...")`, that each `/bin` program calls as its
first line — one line per program — and a small `builtin_usage` table the shell
consults before dispatching a builtin. The judgment calls were about what to
*leave alone*: `echo` and `args` print their arguments, so intercepting `-?` would
be wrong, and the pure filters (`wc`/`rev`/`uniq`/…) have no options to describe.
Everything else — the file commands, the net commands, the filters that take an
argument, the arg-taking builtins — got it. Nice to type `mount -?` or `dial -?`
and get a reminder instead of guessing.

## 2026-08-27 (cont.) — shrinking the shell: more/less/send/recv/selftest to /bin

With the keyboard arc in place, the obvious follow-up: pull everything out of the
shell that doesn't need to be there, and leave only the parts that genuinely are
the shell — cwd, namespace, environment, job control, the disk/power commands
that must run with no disk, and remote exec. The flagship extraction was the
pager. It had been a builtin purely because it reads the keyboard; now a `/bin`
program can, so out it went. `more <file>` was trivial. `cmd | more` took one
insight: the pager, as a pipeline's *last* stage, needs the keyboard too, so the
shell now fg's the last stage of every pipeline (harmless for `grep`/`wc`, which
own it and never read). Two small bugs surfaced doing it — a piped `more` handed
`MSG_RECV` its whole 256 KB heap as the receive buffer (rejected, since messages
cap at 768), and I'd forgotten the last-stage fg at first — both quick fixes.
`send`/`recv`/`selftest` came along for the ride; the nice moment was `selftest`
running as a `/bin` binary and proving `core::fmt` links under PIE from a spawned
program, not just the shell. The shell's builtin list is now short and every
entry earns its place.

## 2026-08-27 (cont.) — the keyboard arc: interactive programs can be `/bin` now

The big one, and it came straight out of a question: does a program that reads the
keyboard (a BASIC interpreter, an editor) have to be built into the shell? The
answer was "only because of one missing piece," so I built the piece. Keyboard
input goes to a single owner, and nothing ever handed that ownership to a
foreground program — so the shell now `FG`s a command before it `WAIT`s on it, and
the kernel already reverts ownership when the command dies. That's the whole
unlock: `readkey`, a throwaway echo program, ran as an ordinary `/bin` binary and
printed the keys I typed. First time a spawned program in this OS has read the
keyboard.

The subtle half was Ctrl+C. It used to just steal the keyboard back and leave the
program running (fine when nothing owned the keyboard but the shell; a deadlock
once a foreground program reads input). So I made Ctrl+C *terminate* the
foreground program — the escape hatch marks it, and `on_tick` does the kill using
the exact teardown the supervisor's crash-restart already uses (current-task vs
not). The one genuinely hard case was a *compute-bound* program that never reads:
nobody polls the keyboard for it, so I added a once-per-tick poll of a running
foreground owner. That can eat type-ahead, but only while a program is actively
running rather than blocked on a read — and a program waiting for input is
blocked, so in practice it's a non-issue. Terminate, not a signal, is the honest
limitation; an editor that wants to catch Ctrl+C needs real signals later. But the
door's open: the next editor or REPL is just a program in `/bin`.

## 2026-08-27 (cont.) — moving `pwd` and `write` out to `/bin`

A cleanup prompted by a good question: which builtins are builtins by *necessity*
and which are just leftovers? Going through them honestly, most have a real
reason — `cd`/`bind` change the shell's own cwd/namespace, `more`/`less` need the
keyboard only the shell owns, `erase`/`partition`/`format` have to run when
nothing is mounted (so `/bin` can't be loaded). But two didn't: `pwd` and
`write`. `pwd` just prints the cwd, which the shell already hands every spawned
program; `write` just writes a file, like every other `/bin` file command. The
nice discovery was that the old `write` builtin *already* word-split and rejoined
with single spaces — the "raw command line" reason in its doc comment was never
actually true of the code — so an argv-based `/bin/write` is a byte-for-byte
behavioural match, not an approximation. Both moved out cleanly and verified
(`cd /d; pwd` → `/d`, `write` + `cat` round-trip). Notably I *didn't* move the
two that were suggested first, `more`/`less` and `partition`/`format` — they're
the two with the strongest reasons to stay (keyboard ownership; no-disk
bootstrap), which was worth saying out loud rather than mechanically obliging.

## 2026-08-27 (cont.) — a pager: `more` (and `less`)

Hans asked for `more` or `less` — "pick one". They're forward-vs-backward
cousins; I made `more` the pager and `less` an alias for it (backward scrolling
would need a lot more, and our content is small). The design was decided for me
by one fact: a pager has to read the keyboard *while it's running*, and this
kernel gives keyboard input to exactly one owner — the boot shell, task 0. A
spawned `/bin` program never becomes that owner (there's a whole doc comment in
`tasks.rs` about why, born from a bug where two programs split keystrokes letter
by letter). So a pager simply cannot be a `/bin` program; it has to be a builtin
that runs inside the shell, which already holds the keyboard. That settled it.

The rest fell out cleanly by reusing what was there: the shell's 256 KB heap
buffer and its `Output::Capture` sink already exist for `>` redirects and
pipeline heads, so `<cmd> | more` just intercepts the trailing `| more`,
dispatches the producer with a capturing sink, and pages the buffer; `more <file>`
reads the file into the same buffer. The only fiddly bit was erasing the
`--More--` prompt, and `cond` turned out to already handle `\r`, so a
carriage-return-and-spaces wipe did it. Space pages, Enter line-steps, q quits —
all confirmed against a QEMU guest driven through piped stdin.

## 2026-08-27 (cont.) — `ls` grows up: columns, sort, and `-l`

The `ls` that just dumped names one per line always felt like a placeholder. This
turned it into a real one: sorted by name, multi-column by default (column-major,
like a terminal), dotfiles hidden unless `-a`, and a proper `-l` long form. The
columns and sort are pure client formatting (the `tree` playbook — collect
offset/length records, insertion-sort, lay out). The interesting part was `-l`,
because size and date/time are *metadata the filesystem protocol didn't expose*.

That's the keystone the gap-analysis kept pointing at: there was no `stat`. So I
added one — `NP_STAT`, a fixed 20-byte record (size, dir flag, modified time).
The design choice that made it clean was returning a **broken-down calendar**
rather than an epoch: FAT stores calendar fields, ext2 stores unix seconds, and
if the wire carried epochs I'd have to pick one and convert on both sides. A
calendar means each filesystem fills what it natively has and the client just
formats digits — no date math anywhere. I scoped the actual timestamp decode to
FAT32 (the tested disk) and left exFAT/ext2/proc returning size+type with the
time marked absent, shown as `-`. And crucially I made `ls -l` do readdir + a
stat per entry rather than fattening the readdir format — so tree, cd, the HTTP
directory index, none of the existing readdir callers had to change. `ls -l /bin`
lighting up with real sizes and `2026-08-27 08:54` timestamps was a good moment.

## 2026-08-27 (cont.) — `shutdown` and `halt`

The machine could boot but never stop itself — you killed QEMU from the host or
pulled the Parallels plug. So: `shutdown`/`halt`. The interesting part is that
powering off is genuinely privileged: it's a PSCI firmware call (`hvc`/`smc`),
which only EL1 can make, so it needs a real syscall (`POWER`, 56) rather than
anything a userland program could do alone. And PSCI has one gotcha — is the
conduit `hvc` or `smc`? Guessing wrong faults. The honest answer lives in ACPI's
FADT (`ARM_BOOT_ARCH` flags), which the project already knows how to walk (it
reads SPCR and MADT the same way), so `power.rs` reads the conduit at boot next to
the MADT parse and stashes it. QEMU's ACPI advertises `hvc`, and `SYSTEM_OFF` made
the VM exit cleanly on the first try. `halt` is the humble sibling — mask
interrupts, `wfi` forever, and confirmed the machine truly stops by watching a
follow-up `echo` produce nothing. Both are builtins, not `/bin`: you have to be
able to power off with no disk mounted, the same logic that keeps
`erase`/`partition`/`format` builtin.

## 2026-08-27 (cont.) — `tree` joins `/bin`

A satisfying little program to write because the constraints did the designing.
`tree` is inherently recursive, and two things immediately bound the shape: the
framebuffer font is ASCII-only (so the pretty Unicode box-drawing is out — ASCII
`|-- `/`` `-- `` it is), and a spawned program has a ~32 KB stack (so unbounded
recursion is a guard-page fault waiting to happen — depth-capped at 16, with each
frame's buffers kept small since the child borrows the parent's path/prefix
across the recursive call). The one real correctness trap was `.`/`..`: FAT hands
them back in every subdirectory listing, and descending `..` would loop forever —
so they're skipped explicitly. Came up clean on the first real boot: a built
`/t/sub/deep.txt` tree rendered with the right branches and `1 directory, 3
files`, and `tree /efi` drew the boot layout with correct `|   ` continuation
lines.

Then the obvious polish, added right after: **sorting**. Raw on-disk order is
whatever the directory happens to hold; alphabetical is what you want to read. No
heap means no `Vec` to sort, so each directory's entries become fixed
offset+length records into the listing buffer and get insertion-sorted in place
before emitting — the name bytes never move. Capped at 64 entries per directory,
since that array stays live across the recursion into each child (a stack cost).
`zebra`/`apple`/`mango` came out `APPLE BERRY MANGO ZEBRA`, and `tree /` rendered
the whole disk sorted top to bottom without tripping the guard page.

## 2026-08-27 (cont.) — a path-command bug: `bin/echo` from `/`

Hans hit a real one: `bin/echo hello` from `/` said `unknown command: bin/echo`,
but `../bin/echo` from a subfolder worked. The "subfolder fixes it" framing was a
red herring — the actual cause was that the top-level command dispatch fed
*anything* non-builtin through the `$PATH` search, prepending each PATH dir. So
`bin/echo` became `/bin/bin/echo` (gone), while `../bin/echo` became
`/bin/../bin/echo` which normalizes back to `/bin/echo` by pure luck, no matter
what the cwd was. The pipeline-stage resolver already got this right (a
`/`-containing token is a pathname, resolved against the cwd, PATH untouched) —
the top-level path just never learned the same rule. One `command.contains('/')`
branch brought them into line. Nice to see the fix confirmed across all five
shapes (bare, absolute, relative-from-root, relative-from-subdir, and a bogus
path still landing on `unknown command`).

## 2026-08-27 — `ps` shows process names

A small, satisfying observability win. `ps` had always printed a bare slot
number and state (`task 5: runnable`) — you could see *that* something ran, but
not *what*. The interesting part was that the data was already there: the kernel
keeps every spawned task's argv, and `argv[0]` is the command name. It just
couldn't get out — `GET_ARG` only reads the *calling* task's own argv, and the
boot-loaded servers (`fsd`/`cond`/`netd`, idle, the init shell) were never
`SPAWN`ed with an argv at all, so they had no name to read.

So two pieces: a `TASK_NAME` syscall (54) — the read-another-task's-argv[0]
mirror of `GET_ARG`, same bounds-checked copy-to-caller shape, the read-only
partner to the `TASK_STATE` that `ps` already probes — and naming the
boot-loaded tasks by synthesizing a one-argument argv blob for them (`set_name`),
with a `server_name` helper as the single source of truth so a supervised
*restart* re-applies the name after a crash wipes it. `ps` now appends the name;
`exec /bin/PONG` then `ps` confirmed a spawned task shows the path it was
launched with, and the servers show `fsd`/`cond`/`netd`. Zombies stay nameless
(their argv is cleared on exit — a zombie isn't running), which felt like the
right call rather than reaching to preserve it.

Then a natural follow-on the same day: if `ps` is going to list zombies, it
should say *why* they exited. The status was already there (it's what `wait`
returns), it just wasn't readable without reaping. A `TASK_EXIT_CODE` syscall
(55) that peeks a zombie's status without consuming it — the read-only sibling of
`wait` — and `ps` now prints `exited (code N)`. `exec /bin/CAT` with no argument
(exits 1) and `exec /bin/ARGS` (exits 0) both showed the right code, matching the
kernel's own exit log. Small, but it turns a zombie line from "something died
here" into "this exited cleanly / this failed", which is the actual question you
have when you see one.

## 2026-08-26 (cont.) — four more `/bin` filters (`tail`/`nl`/`rev`/`uniq`)

A quieter, additive session after the cluster work: filling out `/bin` with the
classic Unix line filters the pipeline machinery deserves. `head`/`grep`/`wc`
already proved the shape (stdin = `pipe_recv`, stdout = `write_out`, EOF out =
`end_of_stream`, finish = `exit`), so this was mostly writing four more programs
to that template — the interesting part was each filter's own bounded, no-heap
trick.

`tail` is the one with a twist: unlike `head` it can't stop early, because it
doesn't know which lines are the *last* ones until stdin ends. So it keeps the
newest lines in a fixed ring (cap 64, a larger N clamped down) and flushes at EOF.
`nl` numbers every line — I streamed it with a line-piece flush and a "mid-line"
flag so a long line split into pieces still gets its number exactly once, rather
than buffering whole lines. `rev` is an in-place two-pointer swap keeping the
newline last. `uniq` compares each completed line against the previously emitted
one (adjacent-only, real Unix semantics — it'll pair with a future `sort`).

No new mechanism, no kernel change — just four crates, the workspace membership,
and the Makefile's var/build/stage triples. I checked the binaries for stray
`R_AARCH64_ABS64` relocations out of habit (the PIE trap that keeps biting new
code) and they came back with *zero* relocations at all — fully position
independent, everything inlined. Verified the lot in one QEMU boot against a
multi-line `/TEST.TXT` fixture: `uniq` collapsed 6 fruit lines to 4, `rev` turned
`hi mom` into `mom ih`, `nl` numbered the listing, and `cat TEST | uniq | wc`
came back `4 4 25`. Workspace is 36 crates now.

## 2026-08-26 (cont.) — `cpu` output past one message (closing out the cluster)

The one real functional gap left in the cluster: `cpu`'s output was capped at one
IPC message (768 bytes) — `cat` a file or `ls` a big dir over `cpu` truncated. Now
the shell pulls the output in chunks, so a command's full output (up to ~2 KB)
comes back. The design was *forced* by the capability model: netd doesn't hold
`TO_SHELL`, so it can't push a stream — only reply once via the reply exemption.
So it's a pull loop: netd collects the run's output into a `PendingRun` buffer and
the shell pulls chunks with a new `NETOP_RUN_MORE` op (empty reply = end).

Two things worth noting. **The stack held** — I braced for the recurring guard-page
overflow (a ~2 KB buffer on netd's already-tight frame), but writing tcp_run's
output *into* the pending buffer instead of a local resp kept the peak neutral, and
it booted clean. **The test apparatus levelled up**: I confirmed QEMU `-nographic`
delivers piped stdin to the guest shell, then drove `cpu 10.0.2.2:5641 …` against a
host "fake export" that streamed 1500 bytes back — the guest printed all 33 lines,
past the old 768-byte cut. A single-guest end-to-end `cpu` test, no second VM.

Scoped honestly as *bounded* (~2 KB), which covers the small commands `cpu` is
really for; truly unbounded streaming (remote sends as it produces, resumable
caller) is documented in the roadmap as a later arc if the need arises. This was
the pragmatic close-out — the cluster's last obvious gap, filled without opening a
big new mechanism. **The cluster feels done.** Written up as
[`cpu-streaming-postmortem.md`](cpu-streaming-postmortem.md) (the 17th).

## 2026-08-26 (cont.) — reply authentication (auth tier 2, the cheap half)

v0.10.0 authenticated the export *request*; this makes the *reply* authenticated
too, so the exchange is mutually authenticated — an active injector can't feed a
client forged data. The framed reply gains a 32-byte MAC over `request_nonce ‖
[status][result]`; binding to the request nonce (which both sides already hold)
means no new wire field *and* ties each reply to its request. `netd`'s
`seal_reply` wraps every framed export reply; `handle_rmount` verifies before
trusting a byte. No round trip, no state — the existing `hmac.rs` applied to the
other direction.

Built as deliberate **defense-in-depth**, not to solve a current threat — Hans
isn't planning a non-trusted LAN, and even reply-auth only matters against an
active on-wire attacker. The reason to bank it now: the symmetric key + the
request-nonce trick made it nearly free. The rest of the hardening (replay
protection, per-peer identity, transport **encryption**, and reply-auth for the
`cpu`-run output stream) is now on the roadmap behind an explicit **"leaving a
trusted network" trigger**. The honest boundary I made sure to state everywhere:
this is *integrity, not confidentiality* — bytes still cross in cleartext; a
sniffer still reads every file. Verified cross-implementation (the Python
observer verifies the guest's sealed reply; a tampered reply is rejected), zero
faults. Written up as a tier-2 addendum to
[`cluster-auth-postmortem.md`](cluster-auth-postmortem.md), not a fresh
postmortem — it's a clean, small follow-on, not a debugging saga.

## 2026-08-26 (cont.) — dialing *in* through another machine (`/net/tcp` accept)

The mirror of dial-out landed the same day: a machine now **accepts inbound** TCP
connections on another's NIC (`serve /mnt/a/net 9000 …` announces on A's network;
a client that connects to A:9000 is answered by a program on B). Passive open on
A, relay to B, B owns the service. Verified end-to-end: the guest announced 9000,
a host socket connected to it, the guest accepted (passive open), and the
request/response relayed through the export were byte-exact; zero faults.

Mostly reuse — an accepted connection *is* a `DialConn`, so buffers/retransmit/
inbound handling were unchanged; the new bits (a `Listening` + `Accepting` state,
`dial_accept`, a `listen` read) were a new arm on an existing state machine, a day
not a month. Three things worth remembering. **Ordering is correctness**:
`dial_accept` runs *after* `dial_on_segment` so a retransmitted SYN matches the
already-accepted conn and never double-accepts. **The close-flush bug**: `Closing`
originally sent only the FIN, stranding a response written just before `close` —
fixed by flushing send data in both `Established` and `Closing`, FIN only once
drained (TCP's FIN means "no *more* data," not "drop what's queued"). And **the
stack overflow returned a fifth time**: `MAX_DIAL=4` blew netd's 32 KB stack;
capped to 3 + a smaller rbuf. On a fixed stack, per-connection array growth is a
stack question first. Full account in
[`dial-in-postmortem.md`](dial-in-postmortem.md). Honest caveat carried from the
design: dial-in is more speculative on consumers than dial-out — it completes the
`/net/tcp` model symmetrically rather than unblocking a waiting consumer.

## 2026-08-26 (cont.) — dialing out of another machine's NIC (`/net/tcp`)

The last unshared cluster resource fell today: a machine can now open a TCP
connection **out of another machine's network card**, exposed as Plan 9's
`/net/tcp` connection files. `dial /mnt/a/net <ip> <port> …` connects from A's
network — read `/net/tcp/clone` for a connection number, write `connect ip!port`
to its `ctl`, read/write its `data`. Verified end-to-end: from the host I drove
the guest's `/net/tcp` over the export to dial back out to a host TCP server,
which saw the connection arrive from the guest's NIC and got the guest-forwarded
request; the reply streamed back. Zero faults.

Three things worth remembering. **The handle is the path** (`/net/tcp/N/…`), so a
stateful, long-lived connection needed *no fids* — the Phase-0 decision to keep
verbs path-based, which looked like a mere simplification then, is what let this
fit now. **`net_op` never blocks; the event loop does the TCP** — `connect` is
async and the client polls `status`, the same "netd must never block its own
loop" rule from the network and remote-exec arcs, third time applied. And **the
stack trap came back**: 2 KB+4 KB buffers per connection × 4 blew netd's
guard-paged stack on first boot (the network arc bumped that stack three times;
still not learned) — fixed by right-sizing to the actual small-transaction
workload. Scoped honestly: `cpu A fetch` already dials out for the
run-a-program case, so `/net/tcp`'s real value is a *raw* connection B drives
with no program on A. Full account in
[`dial-out-postmortem.md`](dial-out-postmortem.md).

## 2026-08-26 (cont.) — locking the cluster door: export authentication

Phases 1–4 all shipped the same asterisk: *trusted-LAN, no auth*. Any host that
could reach a machine's 9P export (TCP 564) could read/write its disk and run
arbitrary `/bin` programs on it. Today that got a lock — the first real
hardening phase, cut as **v0.10.0**.

**The design fork, settled first.** Three ways to carry a credential over the
wire; all need a keyed hash. Picked the **client-nonce MAC** (`mac = HMAC-SHA256(
key, nonce ‖ np)`) over a server-nonce challenge-response, specifically because
the export uses one connection per request — a challenge-response would tax every
`ls`/`cat` with an extra round trip, while the client-nonce version folds into the
existing single request. Weaker on paper (a sniffer can replay an observed
request), stronger in *this* system. The secret is a **shared cluster key** —
which made the scary case (the `cpu` `/host` reverse callback, where the remote
becomes a client of the caller) authenticate in both directions for free.

**Built in one gate.** SHA-256 + HMAC hand-rolled (`hmac.rs`, checked against
NIST/RFC 4231 vectors); an auth header prepended to each framed request;
`netd` reads the key from `\CLUSTER.KEY` at boot (fail-closed if absent); the key
threads as `&Auth` through the whole event loop (no mutable statics — the `.bss`
ceiling again). Inbound verify in `handle_9p`; outbound sign in `handle_rmount`/
`handle_run`. A `\NOEXEC` flag shares the disk but refuses remote `cpu`.

**Two bugs, both from foreign observers.** The host-only Python↔Python test
passed while *both* scripts had the same transposed magic-byte constant — a
mirror confirms your bug as happily as your correctness. Only pointing the Python
client at the real Rust guest caught it. And `rustc`'s `unreachable_patterns`
caught an `FS_ERR_AUTH` sentinel colliding with `SPAWN_ERR_BAD_ELF`. Verified
end-to-end against the guest export (correct key serves the real disk, wrong key
refused, zero faults). Full account in
[`cluster-auth-postmortem.md`](cluster-auth-postmortem.md).

## 2026-08-26 (cont.) — v0.9.0, and admitting the syscalls aren't POSIX

A release-and-documentation day, no code. Two things.

**Cut v0.9.0.** Phase 4 (the full cpu model) plus the namespace-aware export
refactor and the new user docs, folded to `main` in one fast-forward — the two
stacked branches (`cluster-ns-aware-export` → `cluster-phase4-cpu`) were linear,
so merging the child brought the parent. VERSION → 0.9.0, release notes, artifacts
built, tagged, pushed, GitHub release live, both branches deleted. Phases 0–4 of
the cluster are all released now — the achievable Plan 9 vision (resource-sharing
+ explicit remote execution), with only the shared-memory mirage out of scope.

**Then a good question from Hans: are the syscalls POSIX, or something else?**
The honest answer is *neither*, and pulling on that thread was the more valuable
half of the day. The original `notes.txt` goal said "POSIX-ish system calls," but
what actually got built is a message-passing microkernel ABI: a tiny trap surface
(spawn/exit/kill/wait, the IPC primitives, raw console, and the three block_*
calls gated to fsd) and *everything else* — files, console, network — as messages
to userland servers. Only the register-shape calling convention is borrowed from
Linux; the numbers match nothing, there's no `fork` (spawn instead), and none of
`open`/`read`/`stat`/`socket` exist as syscalls (the fs_* calls that once did are
the gravestone gaps at 7–14).

The realization worth recording: this divergence wasn't a decision so much as a
*consequence*. The microkernel + enforced-isolation work forced the filesystem out
of the kernel (a driver the kernel depends on is a split, not a driver), which made
"a file operation" necessarily a message to a server; Plan 9 then arrived and
*rationalized* that into one uniform protocol + namespaces + the cluster. Hans's
call: keep the design, don't force POSIX back in — but plan for eventual C-program
portability. Which has a clean answer, because POSIX is a libc, not a kernel: port
newlib/picolibc whose ~20 stubs translate to the existing server messages (the
Fuchsia/MINIX3/APE way), implement `posix_spawn` natively for the fork gap, and
build the fd table in userland. The nice twist: a POSIX fd is essentially a Plan 9
fid, and we *deferred* fids in Phase 0 — so adding them someday pays off twice.

Wrote it all down: a "Philosophy — not POSIX, not Linux" subsection in
`architecture.md`, a parked "POSIX portability via a userland libc personality"
entry in `ROADMAP.md`, and a fresh `comparison.md` — a user-facing "what you gain,
what you give up" pro/con vs MINIX/Linux/Unix/Plan 9/Helix (the older
`research-directions.md` had gone stale as a user-facing view).

**No bug to postmortem today** — the release was mechanical and clean, the docs
were prose. The only "problem" was conceptual (intended POSIX, built something
else). At first I left it documented across those three files without a
standalone retrospective, reasoning there was no debugging story to tell; Hans
then asked for the postmortem anyway, and writing it proved the instinct wrong —
the drift *is* the story, and it's a genuinely different postmortem shape from the
twelve before it (no bug, no single day, triggered by a question, not a crash).
It's now [`posix-divergence-postmortem.md`](posix-divergence-postmortem.md), the
thirteenth: how "POSIX-ish syscalls" stopped being true, why isolation forced it
and Plan 9 rationalized it, and how portability returns as a userland libc — with
the through-line that an architecture can drift from its stated goals with no test
ever going red, and only attention catches it.

## 2026-08-26 (cont.) — Phase 4b: the full cpu model, importing the caller's namespace

The completion of remote execution: a command runs on the remote's CPU but reads
*your* files. `cpu 10.0.2.10:564 cat /host/BONLY.TXT` runs cat on machine A and
prints `hello-from-B-imported` — a file that only exists on B, the caller. On the
same command, `ls /` shows A's disk and `ls /host` shows B's. Data on B, compute
on A. Plan 9's cpu, whole.

I'd stopped here last round because wiring it hit a deadlock, and this time I
built the fix. The import itself is small: the cpu frame carries the caller's
endpoint, and the remote netd binds /host -> remote(caller) on its own namespace
before SPAWN, which the child inherits. The two hard parts were both foreseen by
the last session's design finding. First, the child now talks to netd for two
things — stdout (a send) and its /host reads (a NETOP_RMOUNT call) — so netd
demuxes by op field; a fs call always carries NETOP_RMOUNT, so it's never mistaken
for output, which would deadlock the child. Second, the real blocker: the caller's
netd serviced the run with a blocking tcp_get, so it couldn't serve the child's
/host callbacks arriving at its own export — it dropped their frames. That's a
mutual deadlock (A waits for output; A must serve the reads that produce it).

The fix is tcp_run: a client connection that pumps the event loop while it waits.
Each pass it accumulates the run's output, feeds every non-run frame to on_frame
(so the child's /host reads get served during the run), pumps the server
connections so those replies go out, and acks the health-ping. It's the exact same
"netd must never block" lesson that shaped 4a's capture — I just hadn't seen it
would bite the caller too until I traced the deadlock. Once tcp_run was in, it
worked on the first two-VM boot: A ran the command, saw both its own / and the
imported /host, and read B's file through it. Zero aborts. Phase 4 — the honest
distributed processing — is done.

## 2026-08-26 — Phase 4a: running a program on another machine

The compute half of the cluster: `cpu <host> <command>` runs a program on another
machine and streams its output back. The demo that makes it undeniable: I create a
`/RANHERE` directory on machine A, then on machine B run `cpu 10.0.2.10:564 ls /` —
and B prints `RANHERE/ BIN/ EFI/`. B's own disk has no RANHERE, so the `ls`
*ran on A*. B's CPU never touched A's disk; A's did.

I designed this carefully first (the Phase 4 doc), and the design paid off twice.
First, it settled that netd is the spawner — because a spawned child's output
arrives as ordinary messages to NET_TASK, which netd's event loop already drains
every wake, so the capture is *non-blocking* (netd is supervised and can't block
in a recv the way the shell's capture does). The child runs on its own slot; netd
just relays its messages to the run connection, and its end-of-stream reaps it and
releases the accumulated output to stream out. Second, the design flagged a
capability gap ahead of time: the child's output pipe needs it to send to netd,
but DELEGATE only hands out a cap the caller statically holds, and a /bin slot
doesn't hold TO_NET. Fixed minimally — NET_TASK holds a self-send TO_NET bit *only*
to delegate the reply cap to a child it spawns.

Two bugs, both quick because the design had mapped the terrain. The serve loop
held a `&mut conns[i]` across the per-segment mailbox drain, which now needs
`&mut conns` to route cpu output — so I re-shaped it to iterate by index and end
the borrow before draining. And the first run failed with "stage fail": I'd read
and staged the ELF in 2048-byte chunks, but the kernel's per-syscall pointer cap
(MAX_USER_LEN) is 512, which the shell's spawn_path already respects — dropped to
512-byte chunks and it spawned. A console diagnostic in netd found it in one run.

Scope kept tight: 4a runs with the *remote's* namespace (B's ls lists B's disk),
so it's "run there with the remote's resources" — the ssh-like core. Importing the
caller's namespace (so the remote command reads *my* files, the true Plan 9 cpu
model) is 4b, and the namespace-aware export left exactly the hook it needs. The
serve-loop restructure got a full export-matrix regression pass, zero aborts.
Ouroboros can now run a computation on another machine — a compute cluster, not
just a storage one.

## 2026-08-26 — the namespace-aware export: paying down the three prefix hacks

Not a feature day — a cleanup I'd been deferring on purpose. Each Phase 3 file
server (/proc, /dev/cons, /net) rode the export by an explicit path-prefix
special-case, and I kept writing down "the general namespace-aware export is
getting closer to worth building." Three consumers is the threshold I'd named, so
today I built it.

The move: the export gateway should serve *its own namespace* through the same
resolver a local client uses — Plan 9's "a server exports a namespace." So the
resolution logic (which had been duplicated in ulib and the shell) became one
`resolve_ns` in ninep-abi, returning a task-neutral `NsTarget` the callers map to
their own server ids. ulib and the shell deleted their copies and delegate; netd's
export resolves incoming paths against a tiny `EXPORT_NS` binding blob and
dispatches on the `NsTarget`. `route_export` and its three prefix checks: gone. A
fourth exported resource is now a fourth binding, not a branch.

The satisfying part is that there's nothing to demo — every path resolves exactly
as before. The whole value is that three implementations that had to agree became
one. That's the kind of change that's invisible until the day someone adds a
resource and it Just Works without touching netd.

Two process notes worth keeping. First: I verified `resolve_ns` in isolation (a
throwaway `rustc` of the pure function against the export blob) the moment a remote
`/net` read flaked in the big matrix test — the isolated test said the resolver was
correct, which saved me from "fixing" the resolver, and a focused re-run confirmed
remote /net works fine; the flake was connection churn (the 8th rapid back-to-back
op), a pre-existing chattiness of one-connection-per-op, not a refactor bug.
Second: deferring this until the third consumer was right. Built at the first
consumer it would have been machinery guessing at a shape; built now, the shape was
obvious because three real callers had already drawn it.

## 2026-08-25 (cont.) — Phase 3, step 3: /net, and Phase 3 complete

The last Phase 3 file server: `/net`, the machine's network identity as read-only
files (`/net/ip`, `/net/mac`). Locally `cat /net/ip`; remotely `cat /mnt/a/net/ip`
reads *another* machine's address. That completes the trio — `/proc`,
`/dev/cons`, `/net` — three resources, three file servers, each remotely readable.

`/net` is the first served by netd itself (netd owns the NIC, so it knows the
IP/MAC). The remote half was easy — the export already routes prefixes, and netd
is the export, so it just serves `/net` from its own state. The interesting part
was the *local* half: a local `cat /net/ip` has to reach netd, but netd's client
handler only spoke NETOP_*. So it now also answers NP read verbs addressed to
NET_TASK. And the routing had a wrinkle: a `/net` binding and a *remote* mount
both resolve to `server = NET_TASK`, so I needed to tell them apart — the
discriminator is the endpoint (a remote mount always carries a real ip:port; local
`/net` carries zeros). `is_local_net` checks it, and a local read goes to a direct
`np_netlocal` NP call instead of the NETOP_RMOUNT remote wrap. Writes to `/net`
are refused.

Worked on the first boot of both: local ip/mac read true (10.0.2.15 + the default
MAC), write refused; two VMs, B read A's 10.0.2.10 and its ...:0a MAC. Zero aborts.

That's three prefix special-cases in the export now (/proc, /dev/cons, /net) —
which is finally the honest signal that the namespace-aware export I've been
deferring has earned its place. Each of these was a few lines because it rode the
export's existing routing; a fourth resource is when I'd stop special-casing and
make the export resolve through a real composed namespace. But Phase 3's stated
scope — resources as files, remotely mountable — is done. The /net slice is
network *identity*; using another machine's NIC to actually dial out (Plan 9's
/net/tcp connection files) is a bigger surface for later. Cut v0.8.0.

## 2026-08-25 (cont.) — Phase 3, step 2: /dev/cons, writing another machine's screen

Second Phase 3 file server: `/dev/cons`, the console as a writable file. The demo
that makes it click: `write /mnt/a/dev/cons hi` on one machine, and "hi" appears
on *another* machine's screen. Remote console.

Where `/proc` was easy because it lived inside fsd (reusing the disk path whole),
the console is the interesting case — it's a *different server* (`cond`,
CON_TASK), so this is the first time the namespace and the export route somewhere
other than fsd. I did it with a console sentinel (NS_CON_TREE), the same shape as
the remote sentinel: locally `mount -c /dev/cons` binds the path, `resolve_ns`
returns server = CON_TASK, and the write helpers just `con_write` the bytes (an
NP_WRITE_FILE to the console) — which every task already does to print, so there
was no new plumbing, just a new *destination*. Reads are refused (write-only).
Remotely, the export recognizes `/dev/cons` and emits the inline bytes to
CON_TASK — and netd already logs to the console, so it could reach it.

Worked on the first boot of both halves: locally `write`/`echo >` both render and
`cat /dev/cons` fails as it should; between two VMs, B's `write /mnt/a/dev/cons
>>>HELLO-ON-A-FROM-B<<<` printed exactly that on A's console. Zero aborts. It's
another explicit prefix/sentinel special-case rather than a general
namespace-aware export — but /dev/cons is now the *second* such consumer, which is
the signal that the generalization (an export that resolves incoming paths through
a composed namespace) is getting closer to earning its keep. Still deferred; two
special-cases is cheaper than the general mechanism, and honest about it. /net is
the remaining big Phase 3 piece.

## 2026-08-25 (cont.) — Phase 3 begins: /proc, and reading another machine's processes

With the shared disk working, the next Plan 9 idea: *everything* is a file, not
just the disk. Phase 3's first step is `/proc` — the kernel's task table as a file
tree — and, because it rides the same export, **a remote machine's `/proc` too**.
The payoff line: `ls /mnt/a/proc` on one machine lists *another* machine's live
tasks, and `cat /mnt/a/proc/2/state` reads its filesystem server's state.

The satisfying part was how little it took. `fsd`'s `Filesystem` enum was already
a per-op dispatch over a `tree`-indexed mount table; `/proc` is just a fourth arm
— the *first non-disk* one, which is what turns the enum from a format
multiplexer into a real VFS. It holds nothing; every listing and file is generated
from the `TASK_STATE` syscall on demand (`/` → a dir per slot, `/<n>/state` →
runnable/blocked/zombie/unused). Auto-mounted at a reserved tree at boot, so it
always exists. Local access is a one-line namespace bind (`mount -p /proc`), the
same mechanism `mount <n>` and `mount -r` already use.

The remote half was the interesting design call. The export gateway sent every
path to fsd tree 0 (the disk); to serve `/proc` it has to pick a different tree. I
took the scoped route — the export *prefix-routes*: a `/proc/…` wire path goes to
the proc tree (prefix stripped), everything else to the disk. That's a deliberate
two-things-exposed hack, not the fully namespace-aware export Plan 9 ultimately
wants (resolve the incoming path through a composed per-export namespace) — but
that generalization has no second consumer yet, so it waits. Threading a `tree`
through the export's four fsd-client calls (HTTP passes 0) was the only real
plumbing.

It worked first try on both halves. Locally the states read true — fsd runnable,
netd blocked in NET_WAIT, slot 9 unused, the shell itself blocked waiting on the
`cat` it spawned. Remotely, B lists and reads A's slots over TCP, disk access
still working alongside, zero aborts on both nodes. One honest limit: only per-slot
*state* is exposed — `GET_ARG*` returns the caller's own argv, so there's no
cross-task name/cmd to show without a new kernel accessor. Enough for a real,
demonstrable "a remote resource is a remote file." More of Phase 3 (`/dev/cons`,
`/net`) later.

## 2026-08-25 (cont.) — Phase 2: a machine writes another's disk (shared-disk cluster)

Phase 1 ended with B *reading* A's disk. Phase 2 makes it a genuine shared disk:
B *writes* A's disk. And it turned out small — the transport, framing, remote
binding, and client routing all existed; Phase 2 was really just teaching the
export gateway the write verbs and chunking a large write client-side.

The export side relays each mutate verb to the local fsd, reusing netd's
fsd-client calls: path-only ops and `mv` go straight through, a full `NP_WRITE`
maps to fsd's inline `NP_WRITE_FILE` (no grant), and `NP_WRITE_AT` bridges the
wire's inline bytes to fsd's grant-based offset write — the exact mirror of the
Phase-1 read bridge, the other way. The client loops a large write into ≤512-byte
`NP_WRITE_AT` chunks, so `cp`/`writeat`/`>>` are all unchanged above the fs
helper. Semantics: single-writer, clean-disconnect — documented, not coordinated
(fsd already serializes disk access through one task, so a single writer never
tears; the only thing we don't claim is *concurrent* writers).

Two VMs, and it worked: from B, `mkdir /mnt/a/CL`, `write /mnt/a/CL/NOTE.TXT …`,
`cat` it back through A, then `cp /BIN/LS /mnt/a/LSCOPY` — a 17 KB file over 34
chunked round trips. Then A, reading its *own* disk, sees `CL/`, `LSCOPY`, and the
note's exact text: B genuinely wrote A's disk. The clincher was the foreign
observer — mounting A's disk image on macOS, `LSCOPY` is `cmp`-identical to
`/BIN/LS`, byte for byte. That's the years-long question answered with a yes you
can act on: two machines, one reading *and writing* the other's disk over a
protocol written from scratch.

The two-VM link also surfaced a real robustness gap the single-VM tests never
could: the client's `tcp_get` sent a *single* SYN with no retransmit, so a dropped
first packet on a freshly-connected QEMU socket hub failed the whole op (an
intermittent first-`ls`). Added a SYN retransmit (a few tries within the op) — it
helps HTTP fetch and remote reads too. And the disconnect test did its job:
`SIGKILL` A mid-session, and B's next remote op fails cleanly (a distinct error,
no hang) and B stays responsive locally — clean-disconnect, as designed. Zero
aborts on both nodes throughout. Phase 2 done → v0.7.0.

## 2026-08-25 (cont.) — Phase 1d: two Ouroboros machines, one reads the other's disk

The "aha," and it landed the same day. 1c proved the remote-mount client against a
host python server; 1d makes the peer a *second Ouroboros VM* on a shared L2 link
(QEMU `-netdev socket,listen=`/`connect=` — a virtual hub, no SLIRP, no gateway).
Machine A exports (its port-564 listener is always on); machine B runs `mount -r
10.0.2.10:564 /mnt/a` and reads A's actual disk — `ls /mnt/a` → `BIN/ EFI/`, `cat
/mnt/a/EFI/ORBS/INIT.CFG` → A's file, `ls /mnt/a/BIN` → A's whole `/bin`.

The one real design piece was the per-guest IP. netd's `OUR_IP` was the hardcoded
SLIRP lease `10.0.2.15`; two guests need distinct addresses. I made it `our_ip()`,
deriving the last octet from the NIC's MAC — the cleanest config channel that
already exists (no boot arg, no new syscall). The trick that kept it zero-risk:
map the QEMU-default MAC `…:56` back to `.15`, so every existing SLIRP path
(ping/resolve/fetch, and the export gateway's hostfwd, which all target `.15`) is
untouched, while the two-VM target hands out `…:0a`/`…:0b` → `.10`/`.11`. And no
mutable global was needed — userland has no `.bss`, so `our_ip()` just reads
`NET_MAC` each call. `next_hop` was already subnet-based, so two on-link guests
route directly; the existing ARP responder answers for the derived IP. Only eight
`OUR_IP` sites to touch.

It worked on the first boot of the pair. The shared-link pcap tells the whole
story: B ARPs for 10.0.2.10, A answers with `…:0a`, B opens TCP to `:564`, sends
the 53-byte framed readdir, A replies 22 bytes and FINs — rotating source ports
per connection (the TIME_WAIT fix from 1c earning its keep), SACK-permitted on A's
SYN-ACK, no SLIRP anywhere. Zero Data/Prefetch aborts on *both* VMs under `-d int`.

That's **Phase 1 complete** — the years-long question ("can several computers
share resources as one system?") has its first real yes: two Ouroboros machines,
one mounting and reading the other's disk over a protocol written from scratch.
Read-only for now; read+write is Phase 2. Ready to cut v0.6.0.

## 2026-08-25 (cont.) — Phase 1c: a machine reads another's disk over TCP

Picked up where the cluster day left off: the export gateway (1a) let the host
read the *guest's* disk; today's work is the mirror — the **remote-mount
client**, so a guest reads someone else's disk. The shape is the whole thesis
made real: `mount -r 10.0.2.2:5641 /mnt/a`, then `ls /mnt/a` and `cat
/mnt/a/HELLO.TXT` — and `ls`/`cat` are the *unchanged* `/bin` programs. Only the
namespace resolver decides local-vs-remote; everything above it is untouched.

The pieces: a `NETOP_RMOUNT(endpoint, NP-request)` op in netd that frames the
verb onto a TCP round trip (reusing `tcp_get`) and returns the reply; a remote
namespace binding (`tree` sentinel `0xFF`, target = `[ip][port][root]`); a
resolver that now returns a `Resolved { server, tree, endpoint }` instead of a
bare `(tree, path)`; the fs helpers routing a remote resolution through netd
instead of fsd — in both `ulib` and the shell's duplicate layer; and the `mount
-r` builtin. Bulk reads, which use grant/safecopy locally, fall back to inline
512-byte chunks over the wire, so `cat` streams a remote file the same way it
streams a local one.

It compiled clean and then the trace did its job — twice. First, `mount -r`
succeeded but every remote op returned "no filesystem": the pcap showed the
guest sending SYN, the host replying SYN-ACK, and the guest never ACKing.
`parse_tcp` — the client-side parser, shared with HTTP fetch — hardwired the
peer's source port to **80**, so it dropped the SYN-ACK coming from 5641. One
line. Then a subtler one: readdir worked but the *next* read failed, then a
readdir worked again — intermittent. The pcap: some SYNs got no reply at all.
The remote-mount client opens a fresh connection per verb, back to back, and the
fixed ephemeral source port `0xc000` meant each new SYN reused a 4-tuple the
peer still held in TIME_WAIT — silently dropped until it expired. Fixed with a
rotating `next_src_port` (and a derived ISN). A `.bss` snag along the way: the
obvious counter is a `static`, but a zero-init static needs `.bss` the userland
loader doesn't support, so the port comes from the microsecond clock instead —
successive connections are a round trip apart, so it's always advanced.

And one more for the relocation-trap collection: `mount -r`'s `host:port` split
first used `&hostport[..c]`, and str range-indexing pulls in a UTF-8
char-boundary panic path whose formatting tables break the PIE link
(`R_AARCH64_ABS64`). Byte slices + `from_utf8` fixed it — the same class the
shell-and-filesystem postmortem is about, still lurking.

Verified against a foreign observer, a ~120-line host python 9P *server*
(mirroring the 1a client): the guest lists and cats its tree, including a
multi-chunk `cat` over four round trips. Local ls/cat unchanged, the export
gateway still serves, zero Data/Prefetch aborts. A small honest note on staging:
the export shipped first (labeled 1a), so this is the design's 1a+1c together —
the outbound half is no longer ahead of us. Next is 1d, the two-VM integration
(a shared QEMU socket link + per-guest IPs), which turns "host as server" into
"machine A ↔ machine B" and cuts **v0.6.0**.

## 2026-08-25 — the cluster day: Phase 0 done (v0.5.0), Phase 1 begun

The biggest single day of the project. It started with a small fix and ended
with the filesystem servers speaking a single protocol over the network.

**First, a patch: the large-read `fsd` restart (v0.4.1).** The real-hardware pass
had left one open bug — a multi-MB `cat` got `fsd` supervisor-restarted mid-read.
The roadmap had guessed the fix was "ack the health-ping during long reads," but
reading the code told a different story: `fat32::read_at` re-walked the cluster
chain *from the file's start on every call*, with no FAT cache, so a chunked
read was O(n²) and a single late-offset request issued enough uncached FAT reads
to blow past the *runnable*-wedge threshold (2.56 s) — which no ping touches. The
fix was a sequential-read cursor that resumes the walk instead of restarting it:
each request O(chunk), the read O(n). Proven on QEMU with a decisive A/B (a 1 MiB
read: 0.99 s with the cursor vs. did-not-finish-in-120 s without). Shipped as
v0.4.1 — the first *patch* release, exercising that half of the version scheme.

**Then the whole of cluster Phase 0 — the arc.** The goal from
[`roadmap-cluster.md`](roadmap-cluster.md): stop having three bespoke server
protocols (`FSOP_*`, `DSPOP_*`, `NETOP_*`) and give every server *one* uniform,
Plan 9-style verb set, with each task composing its own namespace. Built in
sub-steps, each verified byte-identical and shippable:

- **0a+0b — the `ninep-abi` verb set, in use end to end.** Merged into one step
  because "fsd speaks the verbs" is untestable without a client — the minimal
  client *is* the `ulib` re-point, so they went together. Every `/bin` filesystem
  command reached `fsd` over the new protocol, byte-identical, with no `/bin`
  source change (ulib absorbed it). The load-bearing addition was the `tree`
  selector in the wire header — the future multi-mount key.
- **The FSOP retirement, client by client.** The shell keeps its *own* fs helpers
  (separate from ulib — a surprise found in the build), and `netd` is an fsd
  client too, so retiring `FSOP_*` was a client census, not a server edit: migrate
  the shell, then `netd`, and only then delete `fsd`'s twelve FSOP file-op arms
  (~210 lines). `FSOP_*` is now admin-only.
- **0c — per-task namespaces + `bind` (the first kernel change).** Modeled on the
  existing per-task CWD store. The plan was CWD-style spawn-time *staging*; while
  building it, the shell's own `cd` (which validates via `fs_list_dir`) showed it
  must resolve too — which made **direct `NS_SET` + automatic parent→child
  inheritance** the simpler design. An empty namespace is the identity, so
  unbound behavior stayed byte-identical. `bind /mnt /EFI` → `ls /mnt` == `ls
  /EFI`, per-task, inherited by spawned commands.
- **0d — multi-mount, the payoff.** `fsd`'s single mount became a table indexed
  by `tree`; `mount <partition> <path>` mounts a second filesystem and binds it.
  A nice simplification: the existing two-partition `run-image-ext2` disk was
  already the test rig — `ls /` (ext2) and `ls /mnt/f` (FAT32) show two different
  on-disk filesystems at once, which the single-mount model physically couldn't
  do.
- **0e — `cond` on the verbs.** Console writes became `NP_WRITE_FILE` to the
  console "file"; `DSPOP_*` deleted. The last bespoke protocol gone.

Cut **v0.5.0** — the first *per-arc minor* release (Phase 0).

**Then Phase 1 began — 9P-over-TCP.** Wrote the design
([`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md)) and built **step 1a:
the export gateway.** The reframing: locally, bulk data moves by grant/safecopy;
over TCP there is no grant, so a verb travels as a length-delimited frame with
data inline. `netd` grew a second inbound listener on port 564 (alongside HTTP's
80) — the connection remembers its local port so replies leave the right source
port, and the first-data handler dispatches by port to a new `handle_9p` that
decodes the frame, runs the verb against local `fsd`, and frames the reply. A
host-side python 9P client read the guest's disk over TCP (`readdir /` →
`BIN/ EFI/`; `read /EFI/ORBS/INIT.CFG` → its contents), HTTP unregressed, zero
aborts. A property worth naming fell out: because Phase 0 deferred fids, the
verbs are *path-based* — one round trip per op regardless of path depth — so real
9P's per-component-walk chattiness simply doesn't arise. The client half (1b/1c)
and the two-VM "aha" (1d) are next.

Design retrospective:
[the cluster Phase 0 postmortem](cluster-phase0-postmortem.md).

## 2026-08-24 — real-hardware xHCI, the north star, and the first releases

Three threads: closed the last real-hardware bug, wrote down where the project is
ultimately going, and started actually cutting releases.

**The xHCI keyboard↔USB-storage contention, fixed on real Parallels hardware.**
The bug wore one symptom over two boot configs, and it was *two* bugs. **Mode A**
(keyboard works, storage degrades to I/O errors) was a missing BOT error
recovery in `usb_msd.rs` — a single contention-induced bulk-endpoint stall
stayed halted forever; fixed with `xhci::reset_storage_endpoint` (controller
commands Parallels forwards, not the class request it doesn't) + a bounded
retry. **Mode B** (storage works, keyboard never addressed) was a port-scan race
— the scan broke on the first connected port, so the fast SuperSpeed stick won
and the slower synthetic keyboard was missed; fixed with a minimum-settle +
debounce scan. QEMU hid both (its keyboard is synthetic, its storage virtio-blk
— they never share the xHCI bus). Both confirmed on hardware.

**The north star, written down.** [`roadmap-cluster.md`](roadmap-cluster.md): a
Plan 9-style distributed resource-sharing cluster — machines exporting resources
as file trees, each composing a private namespace of the whole. Stated honestly:
sharing resources is doable (Phases 0–4); transparent shared memory / single
system image is the mirage, out of scope by design. The key insight the whole
plan rests on: *"remote" is just "the same protocol over TCP instead of local
IPC"* — which is why the local Plan 9 work (Phase 0) is step 1 of the cluster,
not a detour.

**Releases began.** A `VERSION` file, `scripts/release.sh` (a deliberate
two-phase `build`/`publish` split), and `docs/RELEASING.md` with the version
scheme: `0.MINOR.PATCH`, a completed *arc* → minor bump, an isolated *fix* →
patch. First cut was **v0.4.0** — bundling everything built to date (four arcs).
A real quirk surfaced: `prl_disk_tool`'s `.hdd` bundle references its `.dmg` by
absolute path, so a zipped `.hdd` is useless off the build machine; the portable
Parallels artifact is the self-contained `.dmg`, wrapped into a `.hdd` locally.

## 2026-08-23 — the filesystems day: exFAT read+write, a cleanup, then ext2

Yesterday's filesystem arc left a `Filesystem` enum inside `fsd` with exactly
one arm (FAT32) — a refactor that *claimed* to make a second format cheap.
Today made good on it: `fsd/src/exfat.rs`, a read-only exFAT driver, the first
real second arm. Nothing above `fsd` changed — clients speak `FSOP_*` and never
learn the on-disk format — which is the whole point the refactor was for.

**Read-only first, deliberately.** The roadmap's own scoping lever: FAT32's
write support was phases 4–8, where every corruption bug lived. So a new format
lands read-only as one milestone; read-write is a separate one. Every write op
in `exfat.rs` returns a new shared `Error::ReadOnly` → a new `FS_ERR_READ_ONLY`
ABI code → `ulib`'s "read-only filesystem" message.

**What was actually new versus FAT32.** exFAT is still a cluster filesystem, so
the read machinery (cluster→LBA, chain walking, windowed `read_at`) is the same
shape as `fat32.rs`. The genuinely different parts: a boot sector described by
`log2` shifts; **contiguous files that skip the FAT entirely** (a `NoFatChain`
flag — `advance()` either walks the FAT or just returns cluster+1); directory
**entry sets** (a File entry + a Stream-Extension entry + File-Name entries)
reassembled into one `DirEntry`, structurally the same job FAT32's LFN
reconstruction does; and UTF-16 names. Two structures a read-only driver gets to
*ignore*: the allocation bitmap (that's how you find free clusters — a write
concern) and the up-case table (ASCII case-fold suffices for the names this
system uses, same shortcut `fat32.rs` already takes).

**The testing wrinkle was the interesting part.** To prove the reader, `fsd`
has to *mount* exFAT — but UEFI can only boot from FAT, and `fsd` mounts the
first partition it can. So the test disk (`scripts/mkexfat.py`) is a
two-partition MBR: **exFAT first** (so `fsd` mounts it — the FAT32 probe fails,
the exFAT probe succeeds, exercising the real enum fallthrough) and the **FAT32
ESP second** (UEFI ignores the exFAT partition it can't read and boots from the
FAT32 one). The exFAT filesystem itself is built with macOS's `newfs_exfat`
(`hdiutil` can't make exFAT) and carries `/bin` plus test files — so the shell
runs its commands *off* exFAT. One block device, no slot ambiguity. It worked
first boot: `ls`/`cat`/a `cat | grep | wc` pipeline all reading from exFAT,
long names and subdirs resolved, writes refused read-only, zero aborts. FAT32
via `run-image` unregressed.

The recurring lesson, again: a parser is only tested if you can *build* it a
valid input — the harness that produces the artifact is part of the feature,
same as `mkgpt.py` was for the GPT parser yesterday.

**Then, read-write — the harder half, in four staged commits.** With the reader
proven, the write surface followed the same discipline the FAT32 write arc used:
narrowest-useful-first, one tested-and-committed stage at a time. Stage A built
the machinery (allocation bitmap + entry-set construction with both checksums)
and exercised it with `touch`, which allocates nothing — the lowest-risk first
cut. Stage B added `write_file`/`write_at` (cluster allocation + data). Stage C
added `mkdir`/`rm`/`rmdir`. Stage D added `mv`.

What made exFAT writes genuinely harder than FAT32's: free space is an
allocation *bitmap*, not a scan-the-FAT-for-zeros; and creating a file means
building a whole *entry set* (a File entry + a Stream-Extension entry + N
File-Name entries) with two checksums the format requires — a `SetChecksum` over
the entire set and a `NameHash` over the up-cased name. Get either wrong and a
real driver rejects the file. What made it *easier* than expected: exFAT has no
8.3/LFN mangling (names are just UTF-16, so no `make_short_name`), and no
`.`/`..` directory entries (an empty dir is a zeroed cluster, and moving a
directory needs no `..` fixup — the one hard step FAT32's `mv` couldn't skip).

The decision that kept it simple: created files are always FAT-*chained*
(`NoFatChain = 0`), never pure-contiguous, so allocation is the direct parallel
of FAT32's `write_chain` (set the bitmap bit, link the FAT) and the reader's
existing `advance()` walks them. A macOS-created *contiguous* file being appended
to is the one case needing a convert-to-chain step first.

The validation this time was better than any log line: macOS's own exFAT driver
mounts the volume `fsd` wrote, a binary copied through `cp` reads back
byte-identical, and `fsck_exfat` — a real filesystem checker — pronounces the
bitmap and directory hierarchy clean after a full churn of creates, writes,
deletes, and renames. When another vendor's checker signs off on your on-disk
structures, the checksums are right.

**Then two housekeeping passes, before the tree got unwieldy.** First, the ~26
userland crates (which had piled up at the repo root) moved under a `programs/`
tree grouped by role (`fileutils`, `textutils`, `netutils`, `shellutils`,
`servers`, `demos`, plus `shell`) — a pure `git mv` (history preserved), and
because crates build by package name, the Makefile needed *zero* edits; only the
`path = "../..."` deps deepened. Second, every generated artifact (the `esp/`
staging tree, the disk images, `net.pcap`, logs) moved under a single `build/`
dir driven by one Makefile variable, so the repo root is source/docs/config
only. We considered moving `kernel/` and the shared libs too, but left them —
three top-level dirs isn't crowding, and `kernel/` at the root is the
conventional, legible shape.

**Then ext2, read-only — the arm that actually tests the abstraction.** FAT32
and exFAT are the same shape (clusters, a FAT, a flat directory-entry list), so
the `Filesystem` enum barely had to stretch. ext2 is a different world: inodes
own the metadata and a directory entry is just `name -> inode number`, so path
resolution bounces between directory blocks and the inode table; files are found
through block-group descriptors; data blocks are reached through 12 direct
pointers then single/double indirect pointer-blocks; and names are
*case-sensitive*. Driving all of that through the **unchanged** `FSOP_*`
protocol — nothing above `fsd` touched — is the proof the abstraction was real
and not FAT-shaped in disguise.

Two nice moments in bring-up. First, the reader worked on the first boot for
`exec` and `cd`, but bare commands (`ls`) failed — which turned out *not* to be
a reader bug: the shell probes `/bin/<command>` as typed (lowercase), and it had
always relied on FAT/exFAT matching case-insensitively. ext2, correctly
case-sensitive, wouldn't match lowercase `ls` against the uppercase `LS` I'd
copied in. The fix wasn't in the reader at all — it was to give the ext2 image a
*lowercase* `/bin`, which is the Unix convention anyway. The abstraction was
faithfully surfacing ext2's semantics; my test image was the thing being
un-Unix. Second, forcing the image's block size to 1024 meant the >12 KiB `/bin`
binaries spilled past the 12 direct pointers into single-indirect blocks — so
`exec /bin/CAT` was quietly exercising the indirection path from the first run.

**And finally ext2 read-write — the finale of the whole filesystems arc.** Four
staged commits again (allocation + `touch`; `write_file`/`write_at`;
`mkdir`/`rm`/`rmdir`; `mv`). ext2 writing is the fiddliest of the three formats,
because its consistency is spread across more structures than FAT's single
table: allocation flips a block/inode *bitmap* bit **and** decrements free
counts in both the group descriptor and the superblock; directories carry link
counts that `mkdir`/`rmdir` must bump and drop; a cross-directory directory move
has to repoint the moved dir's `..` and shift its parent-link contribution. The
independent checker this time was `e2fsck` (macOS can't mount ext2), and it kept
me honest.

The best bug of the arc surfaced here. After `rm`/`rmdir`, `e2fsck` reported a
"corrupted orphan linked list" naming exactly the inodes I'd deleted. Everything
*looked* right — links 0, bitmap freed, deletion time set — and `s_last_orphan`
was 0, so there shouldn't have been an orphan list at all. The cause was subtle:
e2fsck treats a links-0 inode whose `i_dtime` is *less than the inode count* as
sitting on the orphan list, using `i_dtime` as the next-orphan inode pointer. I'd
written a sentinel deletion time of `1`, which e2fsck read as "next orphan =
inode 1", fabricating a bogus chain. The fix was to write a plausible *timestamp*
(a fixed large constant, since there's no RTC) instead of a small sentinel —
after which `e2fsck -fn` passed all five passes completely clean, including the
directory-connectivity and reference-count passes that validate `..` and link
counts after directory moves. A copied binary also came back byte-identical
through `debugfs`.

That closes the "more filesystems" arc: GPT/MBR discovery, a VFS refactor, and
FAT32 + exFAT + ext2 — `fsd` reads and writes all three through one unchanged
`FSOP_*` protocol. The whole point of the VFS refactor, proven: a genuinely
different (inode-based, case-sensitive) filesystem slotted in as a third enum arm
with zero changes above `fsd`.

**And then the real-hardware pass — which did exactly what it's for: found a bug
QEMU can't show.** After a session-long pile of QEMU-only work (exFAT, ext2,
`/bin`, pipelines, the housekeeping), we took it to a real Parallels VM via
`prlctl` (start / send-key-event / capture, reading the screenshots back).
**Part 1 was clean:** boots to a shell, all servers supervised, `ps` correct,
`selftest` (the relocation self-test) passes — none of the churn regressed the
hardware boot path.

**Part 2 got interesting.** Parallels exposes no disk to the kernel except USB
mass storage, so testing the filesystem work meant a real USB stick (an
`espexfat.img` written to it). First attempt — boot from the `.hdd`, stick
passed through — showed `fsd: exFAT mounted` but then every read failed with
"device I/O error", even reads of the *same sectors* the mount had just read
successfully. Then Hans ran his own experiment: detach the `.hdd`/CD entirely so
the VM boots *from the stick's* FAT32 ESP — and exFAT auto-mounted and read/wrote
fine. But when I tried to drive that config, the keyboard was completely dead —
not even a builtin registered.

The two configs form an inverse correlation: `.hdd`-boot → keyboard works,
storage reads degrade; USB-boot → storage works, keyboard dead. That's the
diagnosis: the xHCI/USB stack can't reliably keep a USB keyboard *and* a USB
mass-storage device live at the same time on this hardware (the "up to 4
concurrently addressed devices" limit + enumeration order in `xhci.rs`, and/or
mass-storage endpoint recovery in `usb_msd.rs`). It's a USB-subsystem robustness
bug, not a filesystem bug — the FS drivers read and wrote correctly whenever the
block layer actually served them. Recorded in the roadmap; it's the next real
debugging target, and postmortem-worthy. The satisfying part: the whole reason
to run on real hardware is to find precisely this kind of thing, and it did.

## 2026-08-22 — the userland day: /bin, pipelines, and the filesystem arc begins

A long single day that took the shell from "every command is a builtin compiled
into the shell binary" to a real `/bin` of standalone programs, gave it genuine
multi-stage pipelines with a filter set, raised the task-slot ceiling to make
that concurrency possible, and then started the "more filesystems" arc with a
VFS refactor and GPT support. Roughly in order:

**Finished the network stack's maturity work (Stages 4k–4o).** IRQ-driven NIC
receive (the RX queue's GIC SPI wakes `netd` instead of polling), an
RTT-estimated RTO (RFC 6298 SRTT/RTTVAR over a new microsecond clock), an HTTP
405 for unsupported methods, TCP congestion control (Reno cwnd/ssthresh), and
sender-side SACK. These closed out the network arc — the stack now has flow
control, loss recovery, congestion control, and selective retransmit.

**Trimmed `CLAUDE.md`** from a 6500-line milestone narrative to ~600 lines of
durable "read this before touching the code" guidance, moving the history into
`CHANGELOG.md` and the postmortems, and stripping the now-dangling back-pointers.

**The standalone-binaries arc — the day's spine.** The goal: commands become
real programs in `/bin`, found via `$PATH`, with arguments and a shell
environment. Built in stages: an argv ABI (`ARGS_STAGE`/`GET_ARGC`/`GET_ARG`); a
`/bin` + PATH lookup for unknown commands; a shell environment (`set`/`env`/
`unset`, `$VAR`, `PATH` a real variable); then a shared `ulib` support crate and
the externalization itself — `echo`/`uptime`/`clear`, then a cwd-delivery ABI
(`CWD_STAGE`/`GET_CWD`) so `ls`/`cat` resolve relative paths, then the path-only
write commands (`mkdir`/`rmdir`/`touch`/`rm`), then the bulk/multi-arg ones
(`cp`/`mv`/`writeat`). The **whole filesystem command surface** left the shell.

Then the network commands (`ping`/`resolve`/`fetch`) — the first `/bin` programs
to reach a server a spawnable slot can't statically talk to. Rather than widen
the capability policy, the shell **delegates** its `TO_NET` capability to the
child at spawn (the same `DELEGATE` mechanism the program-to-program pipe uses).

One batch was **attempted and reverted**: `ps`/`kill`/`wait`. Testing showed an
externalized job-control command runs in a spawnable slot, so it lists itself
and a task number goes stale between commands — which is exactly why bash makes
them builtins. That's a design finding, recorded, not a failure.

**Stage 0: raised `NUM_TASKS` 7 → 10** (five spawnable slots, up from two) — the
headroom real pipelines need. Mechanical, except the capability `u32` packed
resource caps at bits 8/9/10 and the send-mask now reached bit 9: a collision
caught before it shipped, fixed by moving the caps to bit 16+.

**The multi-stage-pipeline arc.** Turned the two-stage pipe into a real
N-stage `a | b | c`: a chainable filter shape (`upper` rewritten to write to its
stdout target, not a hardcoded console), N-stage parsing, argv and PATH on every
stage, and the spawn/delegate/wait plumbing. A satisfying confirmation: a linear
chain needs only the *existing* one-target-per-task delegation — the roadmap had
parked general delegation for want of a consumer, and this was that consumer,
without needing the general version. Then the payoff: real `/bin` filters
`grep`/`wc`/`head`, so `cat FILE | grep x | wc` works.

**The filesystems arc began.** A pure VFS refactor extracted `fsd`'s hardcoded
FAT32 into a `Filesystem` enum (FAT32 the only arm, proven byte-identical), then
GPT + multi-partition discovery: a new `partition.rs` enumerates partitions on
GPT *or* MBR disks, and `fsd` mounts a FAT32 partition wherever it sits. Testing
the GPT path meant hand-building a bootable GPT disk (macOS has no GPT tooling) —
`scripts/mkgpt.py` with correct CRC32s so UEFI boots it.

Everything QEMU-verified with zero `-d int` aborts throughout; a real-hardware
pass over the whole new surface is still outstanding. Design retrospective:
[the userland & pipelines postmortem](userland-and-pipelines-postmortem.md).

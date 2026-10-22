# Repairing the repairs

*A process retrospective, 2026-09-01. The day after the per-machine-keypair arc
was built, spent reviewing it — and then reviewing the review's fixes, three
more times.*

The arc itself is [`cluster-keys-postmortem.md`](cluster-keys-postmortem.md),
whose spine was *a step is only verifiable if the check can fail*. This is the
sequel, and its spine is narrower and less comfortable:

> **A REPAIR IS A CHANGE, AND CHANGES HAVE THE SAME DEFECT RATE AS THE CODE THEY
> FIX.** Four review rounds ran against one branch. The first found the arc's
> bugs. The next three mostly found the previous round's *repairs*.

That is not an argument against reviewing. Every round found real defects,
including two that would have shipped a security lever silently disabled and one
that could have deleted a home directory. It is an argument about **where the
risk moves once a review starts**, and about a habit of mine that the rounds
made impossible to miss.

---

## The shape of it

| round | findings | mostly about |
| --- | --- | --- |
| 1 | 15 | the flag day itself |
| 2 | 15 | round 1's fixes |
| 3 | 23 (15 + an 8-item sweep) | round 2's fixes |
| 4 | 15 | round 3's fixes |

Round 4 was scoped deliberately to **only my four fix commits** rather than the
whole PR — a few hundred lines against a diff already swept three times. That
scoping is what caught the worst thing in the whole sequence.

---

## The fix that took four attempts

`netd`'s `\NOEXEC` lever — the flag that lets a machine share its disk while
refusing remote code execution — is read once at boot. The flag day deleted the
retried `CLUSTER.KEY` read that had sat in front of it, promoting the `\NOEXEC`
probe to `netd`'s **first** `fsd` call and leaving it with no retry of its own.
`fsd` answers `NO_FS` while the disk is unmounted, `NO_FS` is above `FS_ERR_MIN`,
and the presence test is `m < FS_ERR_MIN` — so the lever came out **false** with
the flag present on disk. It fails *open*, and the boot line that would say so
prints only inside the true branch.

Then:

1. **Attempt one** wrapped it in a retry loop on `NO_FS`.
2. **Round two** found that an `fsd` restart yields `TASK_ERR_NO_SUCH_TASK`,
   which is also above `FS_ERR_MIN` and was not retried. Added it.
3. **Round three** found that the restart window does not produce that value at
   all: `read_file_chunk` issues its `GRANT` *before* the `MSG_CALL`, the kernel
   refuses a grant to a non-existent task, and `netd` turns that into
   `FS_ERROR`. The fix had been aimed at a path the failure does not take.
   Inverted the predicate: retry anything that is not a *definitive* answer.
4. **Round four** found that this now retries **permanent** errors forever —
   `mkdir /NOEXEC` yields `FS_ERR_NOT_A_FILE`, and since this is the first read,
   the whole shared budget goes on sleeping for a condition waiting cannot
   change, starving the two reads that configure the cluster. Bounded it to the
   four codes that mean *the server could not answer*.

Four corrections. Each was a reasonable response to the evidence available, and
each was incomplete in a way the next round's independent look exposed.

**The lesson is not "think harder".** It is that *enumerating* the failures you
expect is the wrong shape when the interesting ones are the failures nobody
thought to list. Two of the four attempts enumerated. The surviving version
names what a definitive answer looks like and treats everything else as *ask
again*, which is a rule about the boundary rather than a list of what crosses it.

---

## `rm -rf $HOME`, with a comment saying it could not happen

Round one found that `make esp` only ever *added* files, so a `\CLUSTER.KEY`
staged before the flag day survived it and kept being baked into every image —
while the PR's headline claim was "an image with no CLUSTER.KEY anywhere on it".
The fix was `rm -rf $(ESP_DIR)`, with a comment I wrote:

> `ESP_DIR` is a literal (build/esp), never empty or user-supplied.

It is not. `ESP_DIR` derives from `BUILD_DIR`, both are ordinary
simply-expanded variables, and a command-line assignment overrides them — an
idiom this very Makefile documents for `CLUSTER_NODE`. `make esp
ESP_DIR=$HOME` would have expanded to `rm -rf $HOME`. Tested, confirmed, and
the decoy directory survived only because the guard now exists.

**A claim in a comment is not a check.** The assertion was load-bearing for
safety and was never executed by anything. It is now a `test` in the recipe,
which refuses to delete a directory that does not look like an ESP tree.

---

## Every defect came from building more than the bug needed

This is the finding that ties the sequence together, and it took until round four
to see it.

- The `rm -rf` was a **wipe** where a *guarded* wipe was needed. Round three's
  repair then replaced it with a **staging tree plus a rename plus a guard on
  both paths** — which bought a permanent build wedge (an interrupted run left
  `build/esp.new`, which the guard then refused to delete, aborting every target
  with a message arguing against the only recovery), a word-splitting hole
  (the guard quoted its paths; the three destructive commands it protected did
  not), and the silent deletion of anything a guest had written through QEMU's
  `fat:rw:` mapping.
- The error predicate was **inverted** where a *list* was needed.
- The flag parser gained a **catch-all scanner** where three specific cases were
  needed — which then rejected `--help`, ate `dial`/`serve` payloads, and still
  missed `--sign nobody` (the orphaned value is not a flag, so the scanner
  cannot see it) — the exact control it was added to protect.

The final commit of the sequence was therefore a **reduction**: it removed the
staging tree, the inverted predicate and the catch-all scanner, keeping only
what the original bugs required. Net 130 lines removed against 138 added, and
the removals were the machinery. **Nothing has needed fixing since.**

> **The repair should be the size of the bug.** Machinery added while fixing is
> not free: it is new surface, written under the impression that one is being
> careful, and reviewed less carefully than the original because it is "just a
> fix".

---

## Three checks that could not fail, written while fixing checks that could not fail

The parent arc's whole subject was checks that cannot fail. In the course of
fixing them I wrote three more.

1. **`a_frame_buffer_fits_the_whole_header`** asserted `NP_FRAME_MAX ==
   NP_NET_LEN_PREFIX + NP_AUTH_HDR_SIGNED + NP_NET_MAX` — that constant's own
   definition, token for token. Setting `NP_NET_MAX = 7` left all thirteen tests
   green. And the Makefile comment I wrote, justifying adding the crate to `make
   test`, **cited that test by name** as the payoff.
2. **The bulk-chunk bound** then asserted `NP_NET_MAX >= NP_MSG_HDR +
   SAFECOPY_MAX` using a *local copy* of `syscall_abi::SAFECOPY_MAX`. Raising the
   real constant left the assertion comparing against the stale copy and passing.
   Its doc claimed it pinned the value "by MEANING"; the meaning was a hand-copy.
3. **The stray-label check** flagged dev seed labels *not in* the peer table —
   excluding precisely the case it exists for, a label that is correctly in the
   table **and also copied elsewhere**. Found by reverting the known-bad line and
   watching the check pass.

Each was caught by mutation, none by reading. Three occurrences is no longer a
run of bad luck:

> **I reach for the condition that describes the fix, not the one that describes
> the bug.** The fix's shape is what is in mind while writing the check, so the
> check inherits it — and a condition derived from the repair cannot detect the
> defect that motivated it. Write the check from the *failure*, then mutate the
> code to make it fail.

---

## The edit that deleted fourteen tests and left the suite green

Rewriting one test in `clusterkeys` with a script whose start index walked too
far back removed **fourteen** of twenty-two tests. `cargo test` reported *8
passed, 0 failed* and exited 0.

Caught only because the number looked wrong. The edit script now asserts the
`#[test]` count moves by exactly the expected amount before writing — which is
the parent arc's own lesson arriving in my tooling rather than in the code.

> **A green suite is a claim about the tests that ran, not about the tests that
> exist.** Any mechanical edit near a test module needs a count.

---

## Found by running, not by reading

Splitting `LineScan::Unreadable` into transient and permanent looked done: the
enum had a new state, the classifier set it, `deny_unknown_user` matched on it.
Booting a node with `/etc/passwd` removed produced the **old** message.

`map_user` flattened the value one level *above* the code that distinguished it,
with the same compare-instead-of-match pattern — `None if scan ==
LineScan::Unreadable`, so a third state took the "no such account" branch by
default. The leaf fix was correct and unreachable.

This is the same shape as the `\NOEXEC` fail-open: **fixed one layer away from
where the failure enters.** Reading found neither; running found both.

---

## Measure before blaming your own change

`cat` across the two-node link failed while verifying the sealed-refusal change.
The temptation was to wave it off as the known intermittent — and the roadmap
does record one. Instead: six runs on the branch, four on `main`.

- branch: 2 failures in 6
- `main`: 1 failure in 4

The roadmap records that flake at *"2 of 6 on main"*. So it is the pre-existing
one at its documented rate, and the PR says exactly that — including that n=6
cannot rule out a small effect. **Naming the size of the sample is part of the
claim.**

---

## What changed in how the work is organised

**A review's findings belong in a new PR against the same base, not as commits
on the branch under review.** Every fixing round made #60 larger, which made the
next round's target larger, which is the loop
[`review-and-split-postmortem.md`](review-and-split-postmortem.md) already
warned about — *a diff too big to review is too big to fix* — and which I had
available and did not apply.

What worked instead, once the sequence was already long:

- **Scope the review to the repairs.** Round four targeted only my four fix
  commits. Small target, high defect density, and it found the `rm -rf` hazard.
- **Then freeze and split.** The remaining findings became eight small PRs, one
  subject each. None of those has needed a second round.

---

## One more, about the instrument

Round three reported "15 findings in the main pass plus 8 from the gap sweep",
and **only the 8 reached me**. I noticed because the completion notice mentioned
a number I had not seen, went looking in the transcript on disk, found the main
pass was not there (the reviewer had delegated to sub-agents, so those findings
lived in nested transcripts), and asked for them to be re-emitted.

A recommendation had already been given on the strength of a third of the data.
It happened to survive contact with the other two thirds, which is luck rather
than method.

> **Reconcile what you received against what the instrument says it produced.**
> Same discipline as the twenty-fourth postmortem's identifier-by-identifier
> reconciliation before closing an unrecoverable source — a partial result does
> not announce itself as partial.

---

## What shipped

**v0.16.0**, and nine merged PRs: the flag day and its documentation (#60), then
eight follow-ups — pre-auth config fingerprinting (#61), transient-vs-permanent
`fsd` failures (#62), transcribed constants (#63), the dev-label tables and a
checker covering both peers (#64), the duplicate-address lookup asymmetry (#65),
sealing the post-authentication refusal (#66), the wire-spec documentation
(#67), and the `NET_WAIT` trigger (#68).

One item is open and deliberately so: `NET_WAIT` is not a sleep, and **QEMU
cannot test the fix** — instrumented, the retry loop runs zero times there,
because virtio-blk has `fsd` ready before `netd` asks. It is queued against the
first Raspberry Pi bench session, written up as
[`testing-pi4.md`](../testing/testing-pi4.md)'s Risk 4b and as a numbered step of "when the
boards arrive". Writing it blind against a rig that cannot exercise it is, after
this day, a recognisable mistake rather than a hypothetical one.

---

## The sequel's sequel: fourteen rounds, and the round that said stop (2026-09-07)

*Added six days later, from the `netd` session-gate arc. Same spine, three times
the scale, and one thing this file did not previously have: a stopping rule.*

| round | findings | mostly about |
| --- | --- | --- |
| 1 | 4 | the session gate itself |
| 2 | 9 | round 1's fixes |
| 3 | 2 (cut short by a rate limit) | round 2's fixes |
| 4 | 9 | round 3's fixes, including a regression it introduced |
| 5 | 5 | round 4's fixes |
| 6 | 7 | round 5's fixes, including a leak round 5 added |
| 7 | 15 (`max`) | rounds 1 to 6, eight of them in the checks |
| 8 | 5 | round 7's fixes |
| 9 | 5 | round 8's fix, withdrawn |
| split | | the branch cut in two |
| 10 | 15 (`max`, TCP half alone) | six regressions the arc introduced |
| 11 | 9 | the split's own collateral |
| 12 | 6 | the harness, no code finding |
| 13 | 4 | the harness and prose, no code finding |

**Three of the regressions were severe**, and each was correct about the bug it
named while wrong about something beside it. An incoming-ACK bound was placed on
`snd_nxt`, a cursor `rewind_to` moves *backwards*, so it discarded the recovery
ACK of a transfer that was healing and let the RTO reset it. A persist exemption
meant to protect a paused peer made a *vanished* one immortal, because the
reaper declines any connection with data in flight. A handshake retransmit went
out at `snd_nxt` rather than the sequence the original used, so a lost SYN-ACK
stopped being recoverable at all. Every one of those was a repair.

**What is new here is not the ratio, it is what the ratio was allowed to
decide.** The 2026-09-01 arc took four rounds and shipped. This one took nine
and was still finding severe regressions, so the question stopped being "what
does the next round find" and became "what does this rate *mean*". It means the
work had no check that could fail: `netd`'s TCP paths cannot be reached by
anything in the tree. SLIRP loses no segments and forges none, the hardware
targets have no networking at all, and the two-VM link rotates its source ports,
so it cannot even collide a 4-tuple deliberately. Every one of those fixes was
reasoning, and reasoning has no error bar.

**The split was the first move that changed the picture**, and it is the
transferable one. The branch was 950 lines of two different things. Cut apart,
the session half was 99 lines that a gate exercises end to end, and the TCP half
was 404 lines that nothing exercises. Reviewed alone against a clean baseline,
the TCP half gave up fifteen findings including **six regressions of its own**,
two of which broke shipped behaviour: an `in_window` gate that rejected `cpu`'s
own connection close, and an ACK guard that let a FIN with the ACK bit clear be
honoured anywhere in a 64 KB window, a one-packet blind teardown strictly worse
than the stale-FIN case the guard was written for. A diff that size hides its
own regressions inside its own volume; the same reviewers found them
immediately once it stood alone.

**So it was abandoned**, and that is the entry this file was missing. The five
real gaps in the receive path are on the ledger with their analysis, the branch
stays on origin as the reference the ledger points at, and the precondition for
the next attempt is written down as **a rig, not more care**: a host-side peer
that can forge a segment onto a guest's link, or the two-Pi network already on
the hardware roadmap. Meanwhile the session half merged after two consecutive
rounds that found nothing wrong with its code.

**The stopping rule, stated so it can be reused:**

> When consecutive review rounds keep finding defects **in the previous round's
> repairs**, that is not a reason to run another round. It is evidence that the
> work has no check that can fail, and the next move is a rig or a retreat, not
> a fix. Splitting the change is how you find out which half is which: the half
> a test can reach converges, and the half it cannot does not.

**One measurement survived the retreat, and it is the exception that proves
the rule.** Loss recovery *can* be reached, by swallowing one segment in
`pump_send` and fetching a 140 KB file over HTTP. That rig caught the ACK-bound
regression above and pinned it to a number: 140,088 bytes hash-identical with
the fix, truncated at 11,303 without it. One rig, one measured finding, out of
an arc of roughly seventy. The recipe is in
[`testing-qemu.md`](../testing/testing-qemu.md) and it says plainly that the fix
it measured is not in the tree.

## A plan reviewed four times, and what a plan's rounds converge on (2026-09-22)

*Added from the session-scoped authentication plan,
[`roadmap-session-auth.md`](../roadmap/roadmap-session-auth.md), #157. A
document, not code: no rig can reach it, so review is its only instrument,
which is the condition the 2026-09-07 stopping rule above says to treat with
suspicion.*

| round | findings | the one that mattered | in the previous round's repair? |
| --- | --- | --- | --- |
| 1 | 10 | the ephemerals need entropy, and Parallels and the Pi have none | no: a missing premise |
| 2 | 10 | round 1's derivation could not be built (three ways) | **yes, all three** |
| 3 | 10 | a same-boot read-back cannot prove a counter persisted | **yes** |
| 4 (medium) | 4 | two counter stores could repeat a value | **yes** |

**The spine held again: every round after the first found a defect in the
previous round's repair**, and each one was in the *mechanism the repair had
added* (a derivation, then a persistence check, then a second store), never in
the parts of the design that had not moved. That is the useful observation. The
X25519 exchange inside `NP_SESSION`, the per-direction keys and the sequenced
HMAC frame came through all four rounds untouched; the boot counter was new in
round 1's repair and drew a defect in every round after.

**Why this one merged when 2026-09-07's did not.** The rule above says rounds
that keep finding defects in the repairs mean the work has no check that can
fail. A plan never has one, so the rule cannot be applied to it as written. What
decided it here was two things the 09-07 arc did not have. The severity fell
every round (a premise, then an unbuildable mechanism, then an undetectable
failure, then a value that could repeat under a store switch), and **the plan
itself is a list of checks**: every step names a check and a control that shows
it can fail, and the boot counter has its own step with a two-boot check and
three controls. The remaining risk in a plan moves into those checks, which
will be real code with real rigs, and that is where it should be caught.

**Two of the findings were the cluster-keys spine, caught before any code
existed.** A persistence check that reads back in the same boot passes on a
store that forgets at every reboot; a control listed under the step whose
honest export can never produce the input it tests. Both are checks that
cannot fail, and both were found in prose. A plan's controls deserve the
question "what would make this pass when it should not" as much as a test's.

**The rewrite beat the patch once.** Round 2's three defects came from
patching Decision 6 finding by finding. After it, the decision was rewritten
whole around the new choice (a kernel boot ID through UEFI), and the next two
rounds found smaller things in it. A repair that touches one mechanism three
ways is a redesign of that mechanism, and it is cheaper to review as one.

## A second plan reviewed four times, and what the rounds cannot find (2026-09-26)

*Added from the per-user keys plan,
[`roadmap-user-keys.md`](../roadmap/roadmap-user-keys.md), #169. A document
again, so review was again its only instrument.*

| round | findings | the one that mattered | in a previous round's repair? |
| --- | --- | --- | --- |
| 1 (high) | 10 | logging out left the user's keyed session attested | no: the original design |
| 2 (high) | 10 | the lock notice from the key holder could be lost, spoofed or never sent | **yes, 8 of 10**, six in one mechanism |
| 3 (medium) | 6 | a derivation inside `accountd` trips the supervisor's `WEDGE_TICKS`; an upgrade that changes a line's length can empty `/etc/shadow` | **yes, 5 of 6** |
| 4 (medium) | 7 | every credential lets a wire observer guess the password offline | **no: the original design**, read past three times |

**The spine held, and the 09-22 section's remedy worked a second time.** Round
1's repairs piled into one mechanism (`accountd` as the key holder, which grew
login handles, a cross-server lock notice and capabilities in both
directions), and round 2 found six defects there. The mechanism was rewritten
whole instead of patched again: keys moved to `netd`, where they are used,
which deleted the notice, the capabilities and a blocking call rather than
guarding them. Round 3's count fell to six. A repair that keeps drawing
findings is a mechanism asking to be redesigned, and deleting parts beats
guarding them.

**Round 3 found what no reading of the plan's own logic could.** Both serious
findings were constraints of the system that the plan never mentioned: the
supervisor restarts a server that stays `Runnable` for 2.56 s, and `accountd`'s
safe shadow write depends on the file keeping its length. The round-2 rewrite
had put a 1.5 s derivation inside a supervised server, and a length-changing
rewrite inside the shadow file, without checking either against the code that
governs them. A repair that adds work to an existing component inherits that
component's unwritten limits, and those limits live in other files.

**The lesson this section adds: the rounds are a sample, and the repairs
attract it.** The most important finding of the four, that the design adds an
offline-guessing exposure to anyone on the wire, was in the original design,
and three rounds had read past it. Two of them were spent mostly on the
previous round's repairs. That is
[`review-and-split-postmortem.md`](review-and-split-postmortem.md)'s "past a
size, findings are a sample, not an inventory" meeting this file's spine: once
repairs start, they draw the reviewer's attention, because they are new and
visibly fragile, and the unchanged core gets less of it. The practical form is
cheap. After a round whose findings are mostly in repairs, the next review
should be told to read the parts that have **not** moved as well; they are no
safer for having survived, only less looked at.

## Three branches in a day, and the round that handed back a choice (2026-10-08)

*Added from the Ctrl-C plan's steps 1 and 3 and DevTools's `poll`
([`roadmap-ctrl-c.md`](../roadmap/roadmap-ctrl-c.md)), #235, #238 and #240,
each reviewed high until its findings were older than the branch or about
completeness rather than defects. Code this time, so the rigs and their
mutations stood beside the review.*

| branch | rounds | findings by round | the repair a later round found wanting |
| --- | --- | --- | --- |
| #235, raw mode | 5 | 8, 6, 8, 7, 8 | the boot shell's interrupt: a queue scan, then a per-byte mark (lost with its byte in a full queue), then a flush at the prompt (lost keys typed just before it, which `test-kbd-mode` caught before the review did) |
| #238, cut keys | 5 | 7, 7, 9, 9, 9 | "every byte through the queue" first drained the devices at each read, handing a Ctrl-C typed after Enter to the next command as data; its one-byte replacement could not keep that promise either, and was named an inherent race |
| #240, `poll` | 4 | 10, 9, 10, 10 | the timed wait: a spin on `YIELD`, then a kernel wait that took the key, then a wait that left the key but spun the last stretch, then tick precision |

**The spine held again, and so did both remedies already in this file.** On
all three branches the middle rounds were mostly about the previous round's
repairs, and the rounds converged only when a mechanism was replaced rather
than patched once more: the boot shell's interrupt became a return value of
the read, not a queued byte or a mark; `poll`'s key stayed in the kernel, not
in the program. The 09-22 section's advice, redesign a repair that keeps
drawing findings, was the thing that worked, each time a round later than it
could have been.

**The lesson this section adds: a repair cycle that keeps returning to the
same trade-off is a decision, not a defect.** `poll`'s timed wait went round
three times between precision and cost: the note asked for 10 ms, the kernel
looks at a blocked task once a 20 ms tick, and every repair that met the
10 ms spun the core for the last stretch while every review said the spin was
wrong. No fix could end it, because both sides were requirements. The fourth
round ended it by asking Hans, who chose tick precision; the reply to DevTools
says so, with what a finer timer would take. Two more rounds would have found
the same spin from the other side. When two successive rounds find opposite
faults in the same line, the next step is to name the trade and hand it to
whoever owns the requirement.

**And one the rigs added: a repair can be caught by an instrument the round
never asked about.** The flush-at-the-prompt repair on #235 was not found by
the next review but by `test-kbd-mode`'s `ef` check, a check written two
rounds earlier for something else, which failed on the next run. It makes a
cheap habit to set beside the 09-26 one (tell the next review to read what
has not moved): after a round whose findings are mostly in repairs, run every
rig the branch touches, not just the ones the repair seems to concern.

## One fix built three times, and the round that named the level (2026-10-09)

*Added from #244, the stale reply after an interrupted call: four high
reviews and a low one over three designs, each design chosen by Hans
between rounds. Code, with a rig and its mutation controls beside every
round.*

| design | rounds | findings | what the review found it cost |
| --- | --- | --- | --- |
| a call's wait ignores Ctrl+C | 1 | 8 | a `cpu` that never stops printing held the console for good; a Ctrl+C typed during a program load reached the program as a key |
| the abandoned answer drained before the next call | 1 | 10 | a stuck `cpu` held every later netd call, and a Ctrl+C at logout's key drop left the user's key held |
| a reply names its call (`MSG_REPLY`, `SENDER_CALL`) | 2 high, 1 low | 9, 10, 0 | nothing in the design: the call number checked at the wrong level, a fid and a connection nobody heard of, stale comments |

**The spine held in a new shape.** The repairs that drew findings here were
not patches to a line but whole designs, and each review found the same
kind of fault in the next one: the cheaper designs each left a caller
waiting somewhere the session could not reach. Both were the roadmap's own
candidates or a variation of them, and both looked sound on paper; only the
built thing, reviewed, showed the cost. The 10-08 section's lesson applied at
once: when the first review's findings were a trade (liveness against the
stale answer), the trade went to Hans rather than into the next patch.

**The lesson this section adds: a review's altitude finding is worth acting
on the second time it points the same way.** The first review's
counter-proposal and the second's "altitude" finding both said the kernel
could not tell which call a reply answered; every design that left that
question unanswered paid for it somewhere else. The design that answered it
drew findings only at the level below (where the check sat, what a refused
answer left behind), and by the fourth round only comments and one leak.
The rounds converged when the fix moved to the level the bug was at, which
this file's 09-22 remedy says in other words: redesign a repair that keeps
drawing findings. What this day adds is the signal for when: two reviews
whose deepest finding is the same missing fact.

**And a smaller one: a review's counter-proposal is a claim like any other.**
The first review suggested counting abandoned calls and dropping the next
reply. It assumed a server answers a caller's requests in order, and netd's
parked requests do not; checked against the code before it was offered on,
it was set aside.

## Three branches, thirteen rounds, and the question that ended them (2026-10-12)

*Added from the end of the multi-core arc: #278 (the stranded task found
unreachable), #279 (`SPAWN` grants a child its stdout target) and #280 (a
round of its own for each core's tick), with #281 the follow-up to a review
of #278's last commit. Code, each round answered with rigs and, where a
check was new, a mutation control.*

| branch | rounds | findings by round | the repair a later round found wanting |
| --- | --- | --- | --- |
| #278 + #281 | 4 | 5, 8, 6, 7 | the boot core's identity: by index, then by index and affinity in one caller, then affinity for every caller (which rested on `VMPIDR_EL2` no rig could check), then "yes until the first `CPU_ON`", one rule and no register |
| #279 | 5 | 10, 10, 6, 7, 8 | a dead stdout target: refused, then accepted (a producer took the keyboard's foreground to a dead stage), then refused again, then a guard on `FG` (which killed `cp` mid-copy when `echo` had finished), then the guard taken out |
| #280 | 4 | 8, 7, 6, 8 | `PING_TIMEOUT`: 8, raised to 22, back to 8, raised to 22, back to 8, each move argued from one case |

**The spine held on all three, and the 10-08 lesson came back word for
word on #280.** The ping timeout went round between two opposite arguments,
the idle server the ping wakes at once and the server mid-call that it does
not, and each round found the other side. It ended when the constant was
seen not to be the branch's to change at all: the round did not create the
exposure, so the timeout went back to `main`'s value and into a follow-up
whose answer is a measurement.

**The lesson this section adds: ask of every finding whether it is true on
`main`.** #279's middle rounds were not about #279. After its first repair,
almost every finding was about how the shell decides a pipeline's fate (from
a moment's check), which the `DELEGATE` loop the branch removed had decided
the same way; each guard added to that model drew the next finding,
including one that was worse than what it guarded against. The series
converged the moment the findings were sorted by that one question: what
the branch made worse was fixed (end-of-stream at exit, the console exempt,
a dead consumer refused as `main` refused it), and the rest, true before the
branch, went into one roadmap entry, "A pipeline's lifetime is managed by
snapshots", whose fix is a design (the pipeline as one job) rather than a
fifth guard. A review reads the code around a change, and old behaviour
around a change reads as the change's; only the comparison with the base
tells them apart, and it is the reviewer's question as much as the author's.

**And one this file has said before, twice in one day: a check written into
a repair can be one that cannot fail.** #279's rig check for a dead last
stage was mutated, and passed with the guard it was written for disabled,
on four cores and on one, because the case reached a different refusal
first; it was kept anyway, with a comment saying so, and the next review
asked why a check that cannot fail was there. #280's static assert on the
ping timeout restated the line above it and was not mutated at all. The
habit already in this file, mutate the new check before trusting it, was
half kept: the mutation that shows a check cannot fail is an instruction to
delete the check or find the case that reaches it, not to annotate it.

## Five branches, twelve rounds, and a decision changed in a round (2026-10-13)

*Added from the one-disk arc's first day: #285 (the boot partition named),
#286 (every image signed), #287 (the disk booted from installed), #288 (the
root beside the ESP) and #291 (D5 and D6, a root that does not mount and a
boot disk's failed read). Code, each round answered with rigs and, for each
new check, a mutation control.*

| branch | rounds | findings by round | the repair a later round found wanting |
| --- | --- | --- | --- |
| #285 | 2 | 9, 0 | none; the second round was one read of the diff |
| #286 | 2 | 9, 7 | the signature checked as the hash of the image's bytes, which every boot changes; then the GPT GUIDs derived from the same bytes |
| #287 | 3 | 9, 10, 0 | the fallback's log line, which stood for three reasons |
| #288 | 3 | 10, 10, 10 | `format`: partition 1 (the ESP), then `mount`'s order with the boot partition in it, which reached the ESP when the root was damaged |
| #291 | 2 | 10, 8 | D6, reversed in the first round and put back in the second |

**The spine held: the later rounds found the earlier rounds' repairs.**
#288's third round found the second round's `format` rule, built to take
the root by `mount`'s order, still able to reach the ESP; #286's second
round found the first round's own error made again in a second file. The
10-12 question settled #288: sorted by whether each finding was the
branch's or `main`'s, the third round's own regression was fixed and the
rest went to Hans as two questions.

**The lesson this section adds: a review round is not where the user's
decision is changed.** D6 was Hans's, written into the plan an hour
before: a disk whose table cannot be read twice is taken as not the boot
disk, and the line says so. The first review of #291 argued the opposite,
that such a disk should be taken as the boot disk, with a real failure
behind it (a one-disk stick mounting its ESP writable). I made the change.
The second review found what the change cost, as the opposite side of any
trade will: an unrelated disk with one bad sector winning over the real
boot stick, a dead stick going in over a working disk. Each round was right
about the side it looked at, and the code could only take one side, so the
rounds would have gone on swinging it. D6 went back as written and the
trade went to Hans with three answers and their costs; he kept D6. When a
finding argues against a decision someone else made, the answer is a
question to them, not a commit: the round has found a cost, not a defect,
and choosing which cost to pay is the decision itself.

**And one smaller: when a finding names a mistake of reasoning, look for
the mistake, not the instance.** #286's first round found the disk
signature checked as a hash of the image's current bytes, an identity
treated as a checksum. The fix corrected the check and left `mkgpt.py`
deriving its GUIDs the same way, which the next round found. A finding
about a line is fixed at the line; a finding about a way of thinking is
fixed by searching the branch for that thought.

## Four branches, one round each, and the question asked first (2026-10-14)

*Added from the one-disk arc's second day: #293 (`make bootstick`), #294
(the Pi's stick), #295 (the root by its type) and #296 (`parttable`, one
copy of the partition tables). Each round answered with rigs and, for each
new check, a mutation control.*

| branch | rounds | findings | taken | to Hans |
| --- | --- | --- | --- | --- |
| #293 | 1 | 10 | 10 | 0 |
| #294 | 1 | 10 | 10 | 0 |
| #295 | 1 | 8 | 7 | 1 (MBR 0x83 as a root type) |
| #296 | 1 | 9 | 9 | 0 |

**The spine was not tested today, and that is the honest reading of the
table.** No branch had a second round: Hans merged each after its fixes,
their rigs and their mutations. The spine says the repairs carry the same
defect rate as the code; today's 36 repairs were checked by rigs and
mutation and by nothing that reads them, so whatever they carry is still
in `main`. A later round on the one-disk code should be scoped to the four
review commits (`97a7239`; `81c4c98` and `532843a`; `7587e03`; `76dbeb7`), as 09-01's
fourth round was.

**Yesterday's lesson, applied: one question instead of two rounds.** The
review of #295 found that 0x83 is the generic Linux type, so a disk made
elsewhere with an ext4 data partition typed 0x83 would hand that partition
the root. That is a cost, not a defect: counting 0x83 only when it mounts
would remove it and give a damaged ext2 root's protection back to the
guess. On 10-13 a round's finding against D6 was committed and the next
round swung it back. Today the trade went to Hans as three answers with
their costs before any change, he kept 0x83, and it went into the plan as
settled. It took one message.

**And the findings that were not the reviewer's to find.** Three of
today's 37 were blind spots in my own checks rather than in the code's
logic: a self-test written in the same wrong passwd format as the code, a
check claiming "file for file" over four of fifteen files, and a rig that
tested only the case with the boot partition known. They are written up
where instruments are, in
[`blind-instruments-postmortem.md`](blind-instruments-postmortem.md).

## One branch, three rounds, and the spine tested after all (2026-10-14, evening)

*Added the same evening, from DevTools's note: `make CLINK=devtools`, which
links `/bin/edit` and `/bin/cpp` with DevTools's `ld`
(`handoff/triage-devtools-ld`, unpushed at the day's end). The second and
third rounds were Hans's, run on the repairs of the round before.*

| round | on | findings | taken | declined |
| --- | --- | --- | --- | --- |
| 1 | the change (`5249e7e`) | 8 | 6 | 2 |
| 2 | round 1's repair (`cf7cc5f`) | 6 | 5 | 1 |
| 3 | round 2's repair (`1995bee`) | 8 | 6 | 2 |

**The morning's section said the spine went untested; the evening tested
it on one branch, and it held.** Round 1 found the change's real gap: the
image's source stamp did not cover `ld`, so a rig would grade an image
linked by a changed `ld` as current. Its repair recorded the linker choice
in a file made at parse time. Round 2 found three defects in that repair
and none in the original change: nothing could remake the record, so `make
clean image` stopped on it; every goal rewrote it, so an unrelated target
relinked both programs; and `ld` was not rebuilt for a deleted source. The
repair was smaller than the change and carried more defects than it.

**A declined finding was the one that mattered.** Round 1 also said a
linker switch made within the second of the last link would leave the
record no newer than the binaries. I declined it in the commit message:
APFS keeps sub-second times. That was a belief about the filesystem, not a
measurement of the tool, and the tool is GNU Make 3.81, which compares
whole seconds; a backdated `touch` showed it while I was fixing round 2's
findings. The first fix for it, deleting the dependents when a record
changed, failed too, because make stats a target before remaking its
prerequisites. The cure was to remove the timestamp from the question:
each linker's outputs and each `ld` build have names of their own.
**A reason for declining is a claim like any other, and this one had a
one-line test nobody ran.**

**Round 3 found no correctness defect.** Its eight were fragilities (the
per-linker names right only for a `CLINK` given on the command line,
`--gc-sections` a second copy of a flag) and costs (a hash and a recursive
make on every run). That is the shape 09-01's fourth round had: when a
round finds only what would break later, the branch is near its floor.
Of the two it declined, one is the note's own recipe for building `ld`,
which could only be cured by running DevTools's build, which the handoff
rule forbids.

## The round the morning owed (2026-10-14, late)

*The second round on the one-disk arc's repairs, scoped to the five repair
commits of #293 to #296, as 09-01's fourth round was scoped to its fixes.
The fixes are #300.*

| on | findings | taken | to Hans | declined | already handled |
| --- | --- | --- | --- | --- | --- |
| `97a7239`, `81c4c98`, `532843a`, `7587e03`, `76dbeb7` | 10 | 5, and part of a sixth | 2, answered by one choice | 2 | 1 |

**The repairs carried defects, as the spine says, and the worst was a
repair's own.** The review of #293 had added a kept image, the only copy of
the old disk's `/etc` and `/Users` after a failed write; nothing stopped
the next run from rebuilding it out of the half-written disk. That defect
did not exist before the repair that introduced the copy. **And the repair
of it had one too, which the rig rather than a reviewer found:** the new
refusal treated an image left by an earlier refusal, made before any write,
as a failed write's. Two rounds of the same shape in one evening (this and
the linker's above) suggest the rule this file keeps arriving at: a repair
that adds state (a kept file, a recorded choice) adds the question of who
cleans it up, and that question is where its defects are.

**Two findings were costs, and went to Hans as one question.** Both came
from #294's settled rule that a Pi stick carries only the firmware's own
files: telling those files by the pinned zip's names needs the network
after a clean and forgets an older release's files after a pin bump.
Carrying everything not ours would cure both by undoing the rule; a
manifest written on the stick cures both and keeps it. He chose the
manifest in one answer, as 0x83 was settled in one this morning.

## Two branches, nine rounds, and a refusal past the commit point (2026-10-20)

*#330 bounded every volume by its partition; #331 bounded every ext2 block
number by the volume. #329 before them had one round and is left out.*

| on | round | findings | taken | to the roadmap | declined |
| --- | --- | --- | --- | --- | --- |
| #330 | high | 10 | 10 | 0 | 0 |
| #330 | high | 10 | 10 | 0 | 0 |
| #330 | medium | 10 | 8 | 1 | 1 |
| #330, round 3's commits | medium | 7 | 6 | 0 | 1 |
| #330, round 4's commit | low | 0 | 0 | 0 | 0 |
| #331 | high | 9 | 7 | 1 | 1 |
| #331 | high | 8 | 5 | 1 | 2 |
| #331 | high | 10 | 8 | 0 | 2 |
| #331, from round 3 on | medium | 9 | 3 | 4 | 2 |

**#330 converged; #331 did not, and the reason is the spine's.** #330's
rounds found values its mount had never bounded, one after another, until
the low round on the last commit found none (it read only that diff, and the
question it should have asked I checked by hand). #331's bound drew findings
in its first round only. Every later round found the same defect in a new
place, and each place was made by a repair: a bound that refuses turns a
silent wrong write into an error, and ext2's free path runs after its commit
point, where an error leaves the file half removed. Round one found it on a
pointer block the new bound refused; the repair skipped pointer blocks, and
round two found it on a bitmap. **A repair that adds a refusal adds a
question for every caller past its commit point: what happens there now.**

**A repair's own defect, found by the next round.** Round three's fix for
fast symlinks, "an inode with `i_blocks` 0 frees nothing", was wrong both
ways, and round four said so: an attribute block raises `i_blocks` on a fast
symlink, so its text was freed again, and damage can zero it on a regular
file, which then leaked every block. The repair had taken Linux's
fast-symlink test without the half of it that subtracts the attribute block.
It is told by type and length now, with a control for each direction.

**The 2026-09-07 stopping rule, met and answered as it says.** Rounds two
and four found defects in the previous round's repairs, the condition that
rule names as evidence of a missing check. Each time the answer was a rig
case with a control, not only a fix: DUP and C's bitmaps for the free counts
and the bitmap skip, L and Z for both directions of the symlink test, BIGD for
a double-indirect walk no rig had run. The rule's last sentence held: the
bound, which a rig reached from its first commit, converged after one round;
the free path, which the rig reached only as each case was added, converged
last. Round four's remaining findings were damage inside the volume and code
older than the branch, and went to the roadmap as one item instead of into a
fifth round.

## Three branches, one round each, and each first version's own defect (2026-10-21)

#337, #338 and #339 each had one high review, and each review found a
defect that the branch's first version had brought in itself, not one it
had failed to fix.

**#337, a repair that read again what it had just changed.** The fix for
`ensure_block`'s leak took the missing blocks first and linked them after,
and on the double-indirect path it read the second-level pointer again
after the allocation. When the second-level block was among those just
taken, that read could answer 0, and the write would go to block 0. Walking
the path once, into a value the linking steps use and do not re-read, closed
it.

**#338, a repair that rebuilt what it should have kept.** The first rename
kept the vendor entries but built the Stream afresh, which raised a
preallocated file's ValidDataLength to its DataLength, so the bytes past the
valid length, which the spec says read as zeros, became readable. The fix
for one field dropped another. The old Stream is now kept, and only its
name fields are rewritten.

**#339, a repair that widened a value past the bound everyone met.** Before,
a listed exFAT name was at most 255 bytes, because each unit became one
byte. Rendering UTF-8 let a name grow to 765 bytes, past the 512-byte
listing page, and a directory's listing failed at such a name. The control
showed the root's listing failing, `/bin` with it, and the shell unable to
find `cat`. The same widening made names with an unpaired surrogate
unreachable where they had been reachable as `?`. **A repair that widens a
value must be checked against every bound the old value met without anyone
writing the bound down.** Here that bound was the listing page and the path
buffer, and nothing in the diff named either.

Each fix came with a rig case and a control that failed without it: CP and
ONEB for the first, STRAY and the Stream fields for the second, and the
overlong and lone-surrogate names for the third. Each round's findings were
taken or declined with reasons, and no second round was run on any of the
three, so whether those repairs carry defects of their own is not known.

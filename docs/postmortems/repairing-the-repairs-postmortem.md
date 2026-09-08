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

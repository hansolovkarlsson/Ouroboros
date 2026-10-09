# Blind instruments

*A process retrospective, 2026-09-02. A day of small roadmap items — five PRs,
one release, no arc — during which five separate tools reported success while
proving nothing.*

The previous day's retrospective
([`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md))
had the spine *a repair is a change*, and before it
([`cluster-keys-postmortem.md`](cluster-keys-postmortem.md)) *a step is only
verifiable if the check can fail*. This is the third in that family and it moves
the question one level out:

> **THE OBSERVER IS A CHECK TOO, AND IT IS THE ONE NOBODY MUTATES.** A test gets
> broken on purpose to prove it can fail. The tool used to *watch* the test —
> the client, the harness, the linter, the `cat`, the release script — is
> trusted on the strength of having worked before.

None of the five was found by reading. Each was found the same way: by breaking
the thing underneath and checking whether the instrument noticed.

---

## The five

| instrument | what it reported | what it was actually doing | wrong for |
| --- | --- | --- | --- |
| `np9p_client.py stat` | `size: 38` | sending `NP_READ_FILE`, not `NP_STAT` | since it was written |
| `drive-qemu.py` | steps passed | silently skipping any step with empty input | since it was written |
| `cargo doc` | 39 warnings | drowning a real one in pre-existing noise | since the userland crates existed |
| `cat` | the correct file contents | reading data a freed FAT chain still holds | always — it is not that kind of tool |
| `release.sh publish` | `published: <url>` | tagging and releasing without pushing the branch | **fourteen releases** |

## `stat` that was not a stat

The day's first item was an `fsd` permission divergence: `NP_STAT`, `NP_CHMOD`
and `NP_CHOWN` skipped a short-circuit every other verb had. The roadmap said
the symptom was `cat ../f` succeeding while `ls -l ../f` was refused, from the
shell.

Booting `main` to get a baseline, `ls -l /BIN/../ETC/PASSWD` worked fine. It
cannot fail: `ulib::normalize_path` collapses `..` client-side, so no `/bin`
program can send `fsd` a path containing one. **The stated symptom could not
happen.** The divergence was real but reachable only from the 9P export, which
sends raw paths — so the export became the observer.

Three probes through `np9p_client.py` came back green against a guest that
definitely had the bug. They were green because the tool's `stat` op sends
`NP_READ_FILE` with `want=1` and prints the returned byte count as a "size".
That is a plausible-looking answer produced by an entirely different verb, and
`NP_STAT` is the *only* verb that reaches `ancestors_searchable` without going
through `path_allows`. **The one arm most in need of a foreign observer was
precisely the one this tool could not address.**

Fixed the client; the divergence reproduced on the first attempt —
`FS_ERR_PERM` against `main`, served against the branch.

## A harness that could not press Enter

`useradd` accepted an empty password where `passwd` refused one, so an account
created by pressing Enter twice was loginable by pressing Enter. A three-line
fix.

Proving it needed the harness to type an empty line, and `drive-qemu.py`'s step
loop is `if text: self.type_line(text)` — an empty TYPE waits and types nothing.
So **every "refuse an empty answer" rule in the system was untestable from the
rig**, including the one being added. Not a wrong answer; no answer, reported as
a passing step.

A `<ENTER>` sentinel now types a bare Enter, which is what let the bug be
demonstrated on `main` first: `useradd bob`, Enter, Enter, `useradd: created
bob`, then `login: bob` + Enter reaching a shell as `uid=1001(bob)`.

## A linter nobody reads

Fixing the `fsd` divergence meant inserting a function above `remove_dirent`.
Its doc comment was **absorbed**: `set_dirent_inode` opened its rustdoc with
"Unlink `name` from directory `dir`…", and `remove_dirent` was left undocumented.

This is not a new failure mode here. `cargo doc` caught exactly this defect
during the cluster-keys arc, which is why the kernel is held at **zero**
unresolved intra-doc links — a baseline created deliberately so the next one
would be visible. It did not catch this one, because the userland crates emit
**39**:

```
cargo doc --no-deps -p ouroboros-kernel        ->  0
cargo doc --no-deps -p fsd -p ulib -p mv -p cp -> 39
```

The lesson from the earlier arc was *make a clean baseline*. It was applied to
the kernel and not to userland, and the half without a baseline is the half
where the defect shipped. Recorded as a roadmap item: the value of zero is
entirely in what it makes visible.

## `cat` is not an integrity checker

A code review pointed out that the FAT arms lacked a guard the ext2 arm had
just gained: two directory entries cross-linked to one first cluster, where the
replace path would free the chain the *surviving* name points at.

Neither this OS nor its tools can produce that, so it was forged — a script
patched `B.TXT`'s first-cluster field to `A.TXT`'s, the FAT analogue of the
`debugfs` hard link used for the ext2 case an hour earlier. Then the guard was
removed and the operation run.

`cat /B.TXT` printed the right contents.

Freeing a chain marks clusters free in the FAT and never touches the data, so
the file reads correctly until something reuses the space. **The damage was
latent, and the instrument was incapable of seeing latent damage.**
`fsck_msdos`, three ways, could:

| | result |
| --- | --- |
| forged, no `mv` | `/B.TXT starts with cross-linked cluster (3)` |
| forged + `mv`, guard in | cross-link **resolved**; only the orphan the forging left |
| forged + `mv`, guard out | `/B.TXT starts with free cluster` |

The guard turned out to do better than avoid harm — it collapses the two names
and leaves the volume cleaner than it found it. That is the part a passing `cat`
would also have hidden.

## "published"

The day ended by cutting v0.17.0. `scripts/release.sh publish` printed its
success line having created the annotated tag, pushed the tag, and created the
GitHub Release. It never pushed `main`.

So the release pointed at a commit that existed on the remote **only via the
tag**: `origin/main` was one commit behind and the repository still advertised
`VERSION 0.16.0`. Caught by running `git rev-parse HEAD origin/main` after the
success line rather than trusting it.

It has been wrong since the script was written — **fourteen releases** — and
never showed, because the next merged PR carried the release commit up
afterwards. The state repaired itself, just never while anyone was looking.

And the reason nobody read the script against the docs is that **the docs were
right**. `RELEASING.md` has listed "push `main`" as the first publish step all
along. A correct description of an incorrect implementation is harder to catch
than a wrong description, because the thing you would check against already
says what you expect.

---

## What they have in common

**Every one produced a plausible result, not an error.** A green step, a
`size: 38`, correct file contents, a URL. Nothing crashed, nothing timed out,
nothing printed a warning. The failure mode of an instrument is not noise — it
is *confident, well-formed output about a question it did not ask*.

**Three of the five were self-repairing, which is why they lasted.** The release
gap was fixed by the next merged PR. The `cargo doc` warning was buried by other
warnings rather than absent. The freed FAT chain read correctly until reuse. A
wrong result that corrects itself before anyone looks is indistinguishable from
a right one, and it accumulates a long lifetime for exactly that reason.

**The bug's age tracks the tool's familiarity, inversely to attention.**

| tool | age of defect | how much I questioned it |
| --- | --- | --- |
| `np9p_client.py`, `drive-qemu.py` | since written | extended them the same day, still missed it |
| `cargo doc`, `cat`, `release.sh` | since written / always / 14 releases | not at all — load-bearing, long-standing, assumed |

The two I was actively editing I still got wrong. The three I had never thought
about were wrong for far longer. **Familiarity is not evidence**, and a tool
that has "always worked" has usually only ever been run against cases where its
blind spot did not matter.

## Two adjacent cases, same disease

**A mutation that did not apply.** While mutation-testing the new POSIX
character classes, one of six reported `test result: ok` — which would mean the
tests could not detect that mutation. It had not applied: a shell-quoting slip
meant the anchor never matched. **A mutation that fails to apply looks exactly
like a test that cannot fail.** The scripts now assert the anchor is unique
before reading the result.

**A roadmap claim that could not happen.** The `ls -l ../f` symptom above. Not
an instrument, but the same shape: a confident, specific, plausible statement
that nothing had ever checked. Corrected in place rather than quietly dropped,
because a reader who tries it deserves to know why it does not reproduce.

## The counter-practice

Everything that worked today was a variation on one move: **break the thing
underneath and watch whether the instrument notices.**

- **Forge the condition when the system cannot produce it.** A `debugfs` hard
  link (two names, link count 1) and a hand-patched FAT first-cluster field.
  Both guards were then demonstrated *both ways* — with the ext2 guard removed,
  `cat` returned empty and `mv` still exited 0.
- **Mutate the code the check guards, and confirm the mutation applied.**
  Thirteen across the day: six against the POSIX character classes, three
  against the mount-info flag (whose other three branches are unreachable on a
  healthy boot), and four against the `mv` guards — the cleanup, the self-move
  check, the same-inode clause and the cross-link check. Restore from a backup
  copy and `cmp` or `git diff` afterwards; do not repair by hand.
- **Pick an instrument that can see the damage you are causing.** `fsck_msdos`
  and `e2fsck` for allocator and directory damage; `cat` for contents and
  nothing else. `e2fsck` reporting `Unattached inode 153` when the cleanup was
  mutated away is what made its clean run mean anything.
- **Check the state, not the success message.** `git rev-parse HEAD origin/main`
  after "published". Unzipping the packaged artifact to confirm a smoke test
  that wrote files had not contaminated it.
- **Baseline on `main` first.** Every fix today was preceded by demonstrating
  the defect against `main` — which is how the roadmap's impossible symptom was
  caught within minutes rather than after the fix was written.

## What it cost, and what it bought

Two review rounds on one PR produced eighteen findings. Round 1 found ten, two
of which could destroy a file — both FAT arms freeing the destination before
writing the replacement (fine against a crash, wrong against an ordinary error),
and ext2's replace path freeing an inode two names shared. Round 2, scoped to
the repairs as the previous postmortem recommends, found eight — **four of them
prose that round 1's own reorder had invalidated**, sitting in the commit whose
message was partly about fixing stale doc comments.

That last one is the previous retrospective's lesson arriving on schedule, and
it is worth stating without softening: fixing a class of defect in a commit is
not protection against committing that defect in the same commit.

## What shipped

v0.17.0. `mv` replaces an existing file on all three filesystems — near-atomically
on ext2, one write of the directory entry — and `mv`/`cp` require `-f` to do it,
a deliberate departure from POSIX on a system with no undo. POSIX character
classes in `grep`, computed rather than transcribed. Two permission fixes.
`useradd`'s empty-password refusal. Seven roadmap ledger entries struck, two new
ones recorded.

And five instruments that can now fail.

## Two more, earned four days later (2026-09-06)

Both are the spine again, and both were found the same way as the five: not by
reading, but by a result that did not fit any version of the code.

**An instrument that could not tell two shells apart.** The nested-shell check
types `exec /EFI/ORBS/SH.BIN`, `fg 6`, logs in, and runs a pipeline. The first
run against the parent-tracking fix showed the first two pipelines refused and
the next two working, which no version of the rule predicts. The edit script
had stopped on its first assertion and written nothing, so that was the *old*
kernel; and the old kernel was showing something else. After any command in the
nested shell, even a builtin, the keyboard goes back to the boot shell, and both
shells print the same `# `. The "working" pipelines ran in the other shell. The
transcript was confident, well-formed, and about a question it had not asked:
*which task answered*. The recipe now types `fg 6` before every nested command,
keys each step on the previous command's output rather than the prompt, and
ends with a `ps` whose `task 6: runnable` is the only line that says who ran
the test. The keyboard revert itself is a pre-existing bug, on the ledger.

**A fix that fails to apply looks exactly like a partial fix.** Twice in one
afternoon an edit script asserted an anchor, missed it, exited, and wrote
nothing, and the "after" run was the "before" tree. The adjacent case above
("a mutation that did not apply") is this one's dual: there, an unapplied
mutation looked like a test that could not fail; here, an unapplied fix looked
like a fix that half worked, and the half that "worked" was the other bug. A
third time the script applied and the build reported success with a warning
the log did not surface: a fold over teardown sites had matched the new
helper's own body and turned it into a call to itself, and the guest never
reached the login prompt. The counter-practice is the same as before with one
line added: **confirm the change applied before reading the result** (print
per step, write per file, `grep` the symbol you expect), and treat a build
that succeeds with a new warning as a build that has not been read.

**Eight review rounds, and the delta every one found.** Four PRs today went
through eight rounds of `/code-review`. Every round found at least one real
defect, and in seven of the eight it was in code that had changed since the
previous round: the fix for the previous round's finding. The one regression
that would have shipped (a sentinel equal to "no identity", so the kernel's
health ping matched every idle connection and the supervisor restarted `netd`)
was in a round-two repair. That is
[`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md)'s
number arriving on schedule, and it is what makes "the code that was reviewed
is not the code on the branch" a reason to run the next round rather than a
reason to skip it.


## Two more, from a day of moving files (2026-09-07)

Both from the reorganization of `docs/` (#114 to #117), and both are the
spine again: an instrument that answered a question adjacent to the one asked,
and a result read before confirming what the change had actually done.

**A count of the wrong population.** Before moving the postmortems, the audit
estimated how many site pages would need re-stamping by counting, in each
abridged postmortem, the links that would change. The grep counted outward
links across all 29 postmortems and the estimate said eight; the move showed
two. Nineteen of the 29 are not abridged by the site at all, so most of what
was counted could not affect any page. The number was exact, computed, and
about a different question. The check that actually settled it was the site
checker itself after the rewrite, which reports pages behind their source by
blob hash and cannot count a file the site does not carry. An estimate is an
instrument too, and the population it counts is the first thing to state.

**A stash that carried more than its pathspec.** The roadmap's seven new
open-gap lines were set aside with `git stash push -- docs/ROADMAP.md` so the
journal PR could go first. `journal.md`'s deletion was staged on the same
branch at that moment, and the stash took the staged deletion along. Popped
onto a fresh branch and committed with `-a`, the "roadmap only" commit held
`docs/ROADMAP.md` plus 3489 deleted lines of `docs/journal.md`, and the PR
would have removed the journal with nothing in its place. It was caught by
reading `git show --stat` on the commit before reporting the PR open, which is
the one-line version of "confirm the change applied before reading the
result" from the 09-06 section above: here, confirm the change is *only* the
change. Amended, pushed with lease, and the PR's file list checked against the
remote rather than the local branch, since the first `gh pr view` after the
force push still showed the stale two files.

## The rig that measured a dead guest (2026-09-07, later the same day)

The worst instance in this file so far, because the instrument reported
**PASS** for the property under test while the subject was not running.

`np9p_client.py session-gate` checks the export's session handling against a
booted guest: eleven checks, ending with an idle session being reaped after
30 s of silence and its slot coming back. The guest was booted by
`drive-qemu.py`, which drives the *shell*: it types its steps, lingers four
seconds and kills QEMU. The gate needs a minute or more. So the guest was
being killed part-way through, and every socket error after that read to the
client as **the export refusing it** - which is exactly what a reaped session
looks like from the host. The reap check reported `3/3 reaped`. Nothing had
been reaped; the process was gone.

**What it looked like from outside.** The gate flaked: 11/11, then 10/11 with
`0/3 reaped`, then 11/11 again. The obvious reading is a timing-sensitive
guest, and the fix that suggests itself is a longer wait - which would have
made the false pass more reliable rather than less. Two hypotheses were tried
against it and both were wrong: that the guest tick under TCG is not
wall-clock (true, and not the cause), and that a change in this branch had
made a connection immortal (false, and the tightening it produced was kept
because it is correct on its own terms).

**What settled it** was refusing to keep guessing and instrumenting the guest:
a temporary log of the connection table whenever a SYN was refused, then of
every slot creation and free with the peer's port. The table was full of
*recently created* connections with ages of 22 to 73 ticks - nothing like the
1500-tick idle limit - which is impossible if the table were merely
un-reaped, and pointed straight at the client's own connections. From there
the driver's `finally: g.stop()` was three lines of reading away.

**The lesson is the file's, sharpened.** Every earlier section here is about
an instrument answering a different question than the one asked. This one is
about an instrument answering a question about *nothing at all*: the subject
had stopped existing, and the harness had no way to say so, because nothing
in it ever asked whether the guest was still there. A test rig needs a
liveness assertion about its subject for the same reason
`trace-remote-flake.py` refuses to report unless the peer's request count
rose - and that precedent was in the tree, in a sibling script, and was not
carried across.

**What shipped:** `scripts/run-guest.sh`, which boots a guest for exactly as
long as a host-side client needs, waits for the export to announce itself
rather than sleeping a fixed time, and **asserts the guest is still alive
before believing the run**, exiting 99 when it is not. Under it the same gate
is 11/11 three times with the guest alive at the end, zero restarts and zero
aborts. The earlier "8 of 8" and "11 of 11" runs recorded for step 4 of the
fid-verbs plan were re-measured; the early checks in those runs were real (the
guest was alive for them), and the late ones were not evidence either way.

## Five more, from one pull request (2026-09-19)

The newtype day added five to the catalogue, and three of them were mine in
the sense that I built the instrument and read its answer.

**A mutation that fails for another reason.** A `const` assert tying two
literals was "shown to fail" by setting one literal to twelve. The review
deleted the assert and ran the same mutation: the build failed on four type
mismatches, because the equality was already enforced by array-typed
parameters. The mutation had never asked the assert anything. The honest
control is the mutation with the check removed, and the difference between
the two runs is the evidence.

**A mutation that is not in situ.** A runtime refusal in the view switch was
exercised by calling the switch directly from `main.rs` with a bad slot, and
the refusal printed. Every real caller indexes the task table before it
switches, so a real bad slot panics there and the refusal never runs. The
guard had been proved to work in a position no caller reaches. A mutation has
to enter where a caller would.

**A drive against a stale image.** A `cargo build` failed in the middle of a
chained command, `make image` then staged the previous kernel without
complaint, and the driven boot that followed passed. Only the build line in
the output said otherwise. The image timestamp is checked against the edit
before a transcript is read as evidence, and chained commands stop at the
first failure.

**A boot with stdin closed.** A one-off QEMU runner passed a closed stdin;
the firmware read the end-of-file as a keypress, dropped to its boot menu,
and the capture showed only firmware lines and two minutes of firmware timer
interrupts. The kernel had never run. `drive-qemu.py` holds a pipe open even
when it types nothing, and the reason is now written down.

**A push that pushed nothing.** After one commit a review run had checked out
the remote ref in the working tree. Two later commits landed on a detached
HEAD, and each `git push origin <branch>` pushed the unchanged branch ref and
printed success; the pull request was two commits behind while every push
"worked". The reviewer's own remark that the remote was behind was the only
signal. Now the branch name is checked before every commit and the PR's head
is read back after every push.

The common line, same as the one this retrospective opened with: each of these
returned a well-formed answer to a question it had not been asked, and each was
caught by reading a second instrument that could disagree with the first.

## The check that reported four failures (2026-09-20)

The day's new instrument was `scripts/test-keyboard-chain.sh`: four driven
boots through the nested-shell keyboard recipes, each graded on the lines its
negative control lacked. Three things it did wrong before it did anything
right, all found the same day.

**Four failures, two of them false.** Its first run against the push-only
mutant reported every recipe failing. Two of the four should have passed on
that mutant, and by hand they did. The script kept no transcript, so it could
say only "expected /EFI after pwd" and not why. Once it kept them, the answer
was that three of the four boots had hung in the firmware before its own boot
entry, before any kernel code ran, and only the fourth had run and failed as
the mutant should. A check that cannot show its transcript cannot be
distinguished from the failure it is looking for. It keeps them now, prints
where the driver gave up and the tail, and retries a boot whose transcript
never shows `BdsDxe: starting`, saying so; a boot past that line is never
retried, because from there a hang is the kernel's.

**A hang I read as evidence three times.** Before the script existed the same
firmware hang had cost an hour: three boots in a row ended at the firmware's
clear-screen with no kernel output, each right after a rebuild, and I stashed
and rebuilt the committed kernel to find out whether my change had broken
boot. It had not; the committed kernel hung the same way once. Measured
directly afterwards: one hang in six bare boots, pauses between boots making
no difference, the same image booting on every retry. The guide now carries
the measurement, and the rule that a boot without the firmware's boot-entry
line is not evidence about the kernel.

**A target that would grade a stale image.** `make test-keyboard-chain` did
not depend on `image`, and the script tested only that the image file
existed; an edit to `revert_input_owner_if` followed by the target alone
would have booted the previous kernel four times and printed four `ok`
lines. Found by review before it happened. The target depends on `image`
now, and the script refuses an image older than the kernel of its profile
or the staged copy in the ESP tree.

And one more, smaller, from the same day: the `SyncCell` ledger entry quoted
a grep and a number, and the grep did not produce the number. The count is
stated once now, with the anchored command that reproduces it.

## A rig that would have failed for the wrong reason (2026-09-20, later)

The afternoon's new instrument was `scripts/test-reentrant-session.sh`, a
two-node rig that drives a remote request into `netd` while `netd` is inside a
`cpu` run and grades whether the request is refused rather than faulting the
guard page. Its `check()` helper took a fourth parameter, the regex that says
"the client SUCCEEDED, so the run returned before it asked and the re-entrant
condition was never created", a case that should be retried, since it proves
nothing, rather than failed. The parameter was assigned and never read. A run that
never created the condition would have burned its retries and then reported a
hard failure, the instrument turning red for a reason that has nothing to do
with the kernel. The condition happened to be created reliably (the unreachable
ping's ARP wait holds the run open), so it never misled in practice, but that
is luck, not a working check. The third review found the dead parameter; it is
wired into both the retry note and the final message now, so a persistent
"client kept succeeding" points at the ping timing and a persistent silence
points at the shared socket link, and neither is graded as a fault.

The same rig carries a witness it deliberately does not grade. The session-path
client `cbig` reaches `netd` only intermittently: a pre-existing race delegates
a spawned task's `netd` send right just after spawn, so its first request is
sometimes refused by capability before any remote request, unrelated to what
the rig tests. Grading `cbig` would have made the rig fail on that race, the
blind-instrument inverse: a red that is not the defect. The rig grades `cat`
and `resolve`, which reach `netd` deterministically, and reports the `cbig`
race rather than failing on it. A check that can turn red for a cause outside
its subject is as broken as one that cannot turn red at all.

The line is the one this retrospective always ends on. An instrument that
cannot fail for the reason it was built for, or cannot show why it failed, is
not measuring; and the first thing to mutate is the instrument.

## Four instruments in one day of building (2026-09-23)

*Added from the day session-scoped authentication went from plan to step 5,
seven PRs, each with a review.* Four of the day's defects were in the
instruments, not the code they measured, and each had passed.

**The probe that measured its neighbours.** `/bin/edtest` gained an HMAC arm in
the same `work` function as sign+verify and X25519, and its timing buffer sat in
`_start`. The stack probe reads depth from the top of the stack, so both 2 KB
buffers were counted in every reading: sign+verify read 8,960 bytes, then 6,320
after the first fix, where it has read 3,584 since 2026-08-31. The calibration
passed throughout, because a 4 KB pad still moved the reading by 4 KB; the
calibration proves the probe responds, not that it measures only its subject.
What caught it was a historical figure to compare against. Each operation has
its own frame now.

**The restore that ran the mutation.** A Python control flipped a bit in the
X25519 reference's own vector and was reverted; the reverted file then failed
the same assertion. Python had reused the mutated bytecode, because the
mutation and the restore were the same size and landed in the same second. The
control's result was right and its cleanup lied, which is worse than either
alone, since it points at the code under test. Python controls run with `-B`.

**The check that passed on a crash.** The keyed self-test's "the export closes
on a skipped seq" check passed when the client saw the connection end. A server
that crashed on that frame also ends it, so the check could not tell the
refusal it was named for from a defect that would kill the server's accept loop
in production. It was found by review (#165), and its control is exactly that
crash: the check now requires the server to say it refused.

**The read-back that proved less than its comment.** The boot counter's
read-back after writing was documented as going "back to the volume". edk2's
FAT driver caches, so it can be answered from the cache; it proves the firmware
took the write, and only the next boot proves the medium did. Found by the
high-effort review of #163; the comment says so now, and the two-boot check is
what carries the durability claim.

And one entry from this file's last section closed. The 2026-09-20 rig declined
to grade `cbig` because of a capability race that refused its first request, and
said so rather than failing on it. That race was the 2026-09-03 delegation fix
never carried into libc's NP call; a control on `main` reproduced it one boot in
three, and #161 fixed it (twenty clean boots against three failures in sixteen).
The ungraded witness was the right call, and naming why it was ungraded is what
kept it findable.


## Two more, from the arc's last day (2026-09-26)

*Added from the day session-scoped authentication closed, steps 6 to 8 and
v0.21.0.* Both instruments were right about what they were built to measure and
wrong about something next to it.

**The timing probe that measured its own dump.** Step 7 re-ran step 0's
counters around the four keyed operations. The per-operation figures were
sound (169 µs against 2,926, each count matching the packet capture), but the
round-trip counter read an 18 ms median verb, against step 0's 6.65 ms: taken
at its word, keying had made every verb slower. The capture of the same kind of
run showed replies in about 1.4 ms. The probe's dump is a blocking console
write at the top of `serve`'s loop and fires on every counted event, which puts
it between framing a request and sending it, inside the interval it timed. Step
0's probe, re-run the same day, still read 5.9 ms, so the host was not the
cause. What caught it was an observer that shares no code with `netd`; the verb
figures in the plan come from the capture, on non-probed builds of both trees,
and the probe's round trip is recorded as the one figure it got wrong.

**The gate whose hint blamed the wrong thing.** `keyed-gate`'s user check
fails when a keyed session as a non-root user reads `/etc/shadow`, and when both
reads are served it added "a FAT32 image enforces no modes". Under the mutation
that ran every keyed verb as root, on the ext2 image, it printed exactly that
hint. The check failed correctly and then sent the reader to the image. It names
both causes now. A diagnostic written for the likely failure is read during the
unlikely one, which is when it is needed.

## Six more, from one evening of the per-user keys arc (2026-09-26, later)

*Added from steps 0 to 4 of
[`roadmap-user-keys.md`](../roadmap/roadmap-user-keys.md), built the same
evening on stacked branches.* Every one of these was an instrument I had just
written, which is the case this document keeps finding.

**A timing that moved with the code.** `edtest`'s PBKDF2 figure read 4.2 µs an
iteration on one build and 6.2 µs, steadily, on the next. The next build had a
review's fixes in it, so the obvious reading was a regression. An A/B on one
host put the new crate under the old `edtest` at 3.9, and the disassembly
showed `pbkdf2_hmac_sha512` was the same 1,598 instructions in both binaries,
at `0x6cac` and `0x6d48`. Under TCG the address alone moved the figure by half.
The instrument's own scaling check had refused two earlier versions of the
measurement for real causes, which made its later readings easy to trust. What
settled this one was the disassembly, an observer the timing could not
influence.

**A diagnostic that changed what it diagnosed.** A `netd` wedge came about one
run in four. I added a kernel line naming what a wedged server was blocked on,
and it caught one: `netd` in a `MSG_CALL` to `fsd`, no reply. Extending it to
print `fsd`'s state stopped the wedge: 42 runs, none. The observer is part of
the timing it observes, on an emulator where placement changes timing.

**The gate's hint, again.** `impersonate-gate`'s check that the intruder key is
authorized failed on FAT32 because `/HELLO.TXT` is not on that image, and its
hint said the image predated the `intruder` line. It is the same fault
`keyed-gate` had this morning (a hint written for the likely cause, read during
another), made again by the same hand the same day. The hint now fires only on
the status it describes.

**A check that passed over its own failures.** The held-key rig's `keyprobe`
check asked whether the probe reached its last line. Under two mutations it
printed `[FAIL]` lines and reached its last line, and the scenario still failed,
but only through a separate check of the table. Found by reading the transcript
of a mutation run. It checks the probe's exit code now, which is its failure
count.

**Scenarios that shared a disk.** The same rig booted one image for every
scenario, and a guest writes to the image it boots. `no-realm` deletes
`/etc/cluster/realm`, so `full-table`, running after it, held no keys and
failed for a reason that had nothing to do with full tables. Each scenario now
boots its own copy. The first full run looked like a regression in the table.

**A batch of gates that ran nothing.** Four host gates were run in one loop that
word-split a variable, and zsh does not word-split. Three gates received one
argument and printed their usage. My filter matched the usage line's own
`PASS/FAIL` and the guest's "alive" line, so the batch read as three quiet
passes. The fourth, a single word, ran, and failed because it stats a FAT32
path on the ext2 image. Each gate re-run on its documented image, with its
arguments as separate words, passed. A loop that runs checks is a check.

## Six more, from the day the wedge was found (2026-09-27)

*Added from the hunt for step 4's `netd` wedge and its fix (#175), recorded in
[`2026-09-27.md`](../work-journal/2026-09-27.md).* The wedge itself turned out
to be the supervisor's instrument misreading a busy server as a dead one, which
is this document's subject in the kernel's own voice. The rest were mine.

**A saved context read as a live one.** To see what `fsd` was doing when
`netd` wedged, I printed its PC from `TASKS[f]`, and two wedges running put it
right after the `svc` of `MSG_RECV`: "woken and never scheduled", a
starvation. But `TASKS[f]` is the context saved when the task was last
switched out, and when the task is the one running at the tick it is stale: a
task that blocked in `MSG_RECV` and has run since always shows exactly that PC.
The instrument could only ever print one answer for a running server. The
version that read the live frame and the runnable set put `fsd` in
`fat32::Fs::find` ten times out of ten.

**A batch whose rig left with the branch.** A hundred-run batch on a scratch
branch ran `scripts/test-held-keys.py` from the working tree. I switched the
tree to the fix branch to edit a document, the script did not exist there, and
99 runs "failed" in no time. The wrapper counted a missing file as a failing
test. It stops now when the script is gone, and the rule is to stay on a
batch's branch until it ends. The same wrapper's first "answered" check would
have counted a Python traceback as an answer; it was tightened to the node's
64-hex key before it was used.

**A forcing mutation that forced one time in ten.** To show the wedge on
demand I set `PING_INTERVAL` to 2, so a ping lands in every spawn, and got one
wedge in ten. The mutation was right about the ping and wrong about the spawn,
which usually finishes inside the 8-tick timeout; only `PING_TIMEOUT` at 3 made
it four in four. A control that fires one time in ten is not a control, and it
was measured before it was trusted, which is the only reason that is known.

**A control chosen by the fix's author.** The kernel rule paused a server's
ping while its partner ran, and my control for "a busy partner that is really
stuck" was an `fsd` that spins forever. It passed. The review found the case it
could not show: a partner that never replies but blocks briefly now and then
resets its own heartbeat, so neither detector fires. A spin never blocks. The
control tested the shape the fix handled, not the one it broke, and the fix was
replaced.

**A page name the checker prints and refuses.** `check-site-freshness.py`
reports a stale page as `docs/site/<page>` and tells you to re-stamp it with
`--update <page>`, but its manifest keys pages as `site/<page>`, and the name
it printed was refused as "not currently reported stale". Found by following
its own instructions. Not fixed yet.

**A description that said every one.** #175's description said all twelve
wedges showed `fsd` in `find`. Ten had been measured; the first printed no PC
and the second printed the stale one above. Corrected after merge, and marked
as corrected. The count was right and the claim about it was wider than the
instrument that produced it.

## Seven more, from porting to a board that was not on the desk (2026-09-27, later)

*Added from the afternoon's Raspberry Pi 400 work (#176 to #179), recorded in
[`2026-09-27.md`](../work-journal/2026-09-27.md).* Every check of the day ran
on QEMU, standing in for a machine it differs from in exactly the places the
work was about. Most of these are that difference, seen late.

**A rig broken for a month that nothing depended on.** `make run-usb-kbd` had
found no xHCI controller since 2026-08-29: the discovery code's exclusive
open of the PCI root bridge was refused once `virtio-rng` joined every disk
target, and the loop skipped the refusal without a log line. No automated
check noticed, because every driven test types over the serial console, and
the boot still reached a prompt. It was found by running the target by hand
for a different reason. A rig that nothing fails without is not checked by
anything.

**A platform that cannot see the property under test.** QEMU and Parallels
both model coherent DMA and no caches worth the name, and the Pi's PCIe is not
coherent. Every USB check in the project, and every framebuffer check, was
therefore blind to the one thing that decides whether USB and HDMI work on the
board; the difference was found by reading the firmware's ACPI source, not by
any run. The compensating instrument is the kernel's own table-walker
self-check, which proves the mapping, and says in its log what it cannot prove.

**A translation QEMU sets to zero.** On QEMU the PCI address translation is 0,
so no run could tell a right sign from a wrong one in my first fix's
arithmetic. The fix that landed stopped doing arithmetic: it takes firmware's
own host address and refuses it unless it gives back the raw BAR, a check that
can fail on QEMU too (it did, under a mutation).

**A control that passed both ways.** Leaving one hub request unrung produced a
real timeout, and the kernel with EP0 recovery passed; so did the kernel
without it, because QEMU completes a ring the moment it is rung and the
off-by-one pairing of requests with answers never shows. The only tell was one
stray completion in the log. And the review then found that the line I used as
the old bug's signature, `unexpected event type=32`, is also what a correct
recovery prints on real hardware, so on the board it would not have told a
fixed kernel from a broken one. The line now names a stopped request's event
as such.

**A check that failed for a different reason.** Checking that `mount -a`
retries a device that failed at boot, typed on the USB keyboard, the second
`mount -a` never arrived. The retry was fine; the keyboard had died, because a
keystroke that lands while the kernel waits on a controller command was
dropped (a real bug, already on `main`). Typed over serial, the check passed.
A failing check says something failed, not what.

**A self-check that looked at two bytes in one view.** The first version of
the non-cacheable mapping's self-check walked the two ends of each range under
the current task's tables. A wrong 2 MB slot in the middle, or a wrong entry
in the shared tables that only an empty task's view uses, would have passed.
The review found it; the check now walks every page in every view, plus the
page either side and the kernel's own data, and a mutation of each path was
shown to trip it.

**A boundary check on offsets that cannot fail.** The compile-time assertion
that no xHCI ring crosses 64 KB tests offsets inside the DMA pool, and the
pool is aligned to 4 KB, not 64 KB, so the assertion says nothing about the
real addresses. Found by the last review of the day: the right check is the
4 KB page, which the pool's alignment makes exact. Its sibling, the page check
on the contexts, is sound, and was shown to fail with `MAX_DEVICES` set to 7.
(Fixed the next morning on #179's branch: the rings are checked against the
page, and an event ring made larger than a page now fails the build.)

## Three more, from the first boots on the boards (2026-09-28)

The Pi 400 and a 1 GB Pi 4 booted for the first time, watched over HDMI with no
serial cable, through a phone camera. Every observation came through at least
two instruments that had never been checked: the screen and the photo of it.

**A screen that stopped updating, read as a machine that stopped.** The first
boot ended on the firmware's text, partway through the line after the xHCI
command-register write, and I said it had frozen at that write. Two boots with
flags later I said the opposite: the firmware's ACPI names a serial port, so
the kernel's own lines after the exit go to serial only, and the Pi might be at
the shell with nothing reaching HDMI. Both were readings of the screen, not of
the machine. What settled it was booting QEMU with `-device ramfb` and looking:
the console server draws on the framebuffer whenever one exists (`CON_INFO`
asks `fbdev`, not the kernel's console), so an HDMI screen that never changes
means the boot never reached it. The fix to the instrument was to make the
screen report: `\FBCON` puts the kernel's framebuffer console up right after
the exit, and progress squares cover the stretch before any console exists.
Each was shown to fail on QEMU with a planted fault or hang.

**A photo read wrong, twice, each time to an answer that looked like one.** The
firmware printed only `Synchronous Exception at 0x…` on HDMI, and the kernel
now logs where it was loaded. From a 480×360 photo I read the image base
`0x376d0000`: the address fell in `.rdata`, on a `core::panic::Location`.
Correcting one digit by the image's size gave `0x376d7000`: the address fell on
a stack store between two stack stores that had succeeded. Both were readings
of blurred hex digits, and both produced a symbol name. The check that caught
them was a property the answer had to have: the loaded image is exactly
`SizeOfImage` long (0x1e9000, seen on QEMU), so a base and end read off the
screen must differ by that. A sharp photo gave `0x376df000..0x378c8000`, which
does, and an instruction that can fault. I had asked for the sharper photo
only because the answers were impossible; a plausible wrong one would have
gone through.

**A disk image that was not the card.** `make sdcard` was tested against
`hdiutil` FAT32 images, every guard shown to fail. On the first real card it
stopped with `rm: .Spotlight-V100: Operation not permitted`: Spotlight indexes
a mounted card and macOS will not let its folder go, and the images were
never indexed in time. Rerunning the old script on a fresh image passed, so the
image could not reproduce it at all. It reproduced every time with an `rm`
shim that refuses that one path: the old script failed, the fix passed. The
stand-in has to be made to do what the real thing does, not assumed to.

## Two more, from the review of the bench tools (2026-10-01)

The diagnostics built for the first boots went through a review before their
pull request (#181). Neither of its two real findings had been seen on QEMU,
because every QEMU run had been set up so that it could not show them.

**A tool built after the lesson, without the lesson in it.** The section above
ends on the property that caught two misread photos: a base and end read off
the screen must differ by the image's `SizeOfImage`. `scripts/efi-symbol.py`
was written the same day, to turn exactly those readings into a function name,
and it did not check that property. Given a base with one blurred digit it
printed a confident `symbol + offset`, and the same for a map relinked from a
different profile than the card's binary. The check had been applied by hand
and written down as the lesson, but never put into the tool. The script now
takes the relinked `.efi` as a required argument and refuses before naming
anything unless the map and the `.efi` share a link timestamp and `<end>-<base>`
is that `.efi`'s `SizeOfImage`. Both refusals were run: a base off by one digit,
and an `.efi` whose timestamp was patched.

**A flag that made the screen report could make nothing report.** `\FBCON`
puts the framebuffer console up instead of the serial one. It dropped the
serial console whenever the flag was set, whether or not a framebuffer existed,
so on a board with a UART and no usable GOP the flag removed the only console,
and every line after the exit went nowhere, the kernel's own `EXCEPTION`
report included. Every QEMU check of `\FBCON` had run with `-device ramfb`,
because the point was to see the screen, so the case without a framebuffer
never ran. The review found it by reading. The fix drops the serial console
only when there is a framebuffer to replace it with, and the control is the old
filter put back: with no `ramfb`, that boot's serial log ends at
`exiting boot services`, where the fixed one reaches the shell.

## Three more, from the storage recovery (2026-10-01, later)

The rest of the day turned two storage defects found by reading into defects
observed on QEMU, and built a committed test for each (#184 to #186). Three of
the instruments built along the way passed while blind to the thing they were
named for, and one more was blind by design and is recorded as such.

**A harness whose every tree had one answer.** The fix for the devicetree's
missing `/chosen` (#182) was checked by a host harness running the old and the
new console lookup side by side on hand-built trees. Each tree had one PL011,
so "`stdout-path` resolved" and "`stdout-path` failed, fell back to the first
PL011" returned the same node, and a lookup that dropped the last byte of a
path agreed with one that did not. The review's fix for exactly that byte
passed the harness before the harness could see it. A decoy PL011 placed
ahead of the real one in every tree made the two outcomes different; then the
old code's truncation showed (it fell back to the decoy), and putting either
old behaviour back was caught.

**A "recovered" check that saw a retry begin.** The first version of
`test-usb-hub.py --stall` (#185) passed `recovered` on the kernel's `retry 1/3`
line, which is printed before the reset and the retry run, and passed `no
transfer timed out` on the absence of a line the kernel stops printing after
sixteen recoveries, in a boot that has thirty. A recovery that never worked
would have passed both. The review found it by reading the log limit against
the measured count. The check became exact: one `retry 1/` line per Stall by
QEMU's own count, no `retry 2/`, no `giving up`, with the cap lifted while the
fault is armed. The rewind from before #184 put back then fails it with 56
first retries and 18 further for 19 Stalls.

**A Stall test that only ever stalled one direction.** The same test
corrupted CBWs, so only Bulk-OUT ever halted, and at every recovery the IN
endpoint was Running and its reset was refused and discarded. The max review
deleted the IN half of the recovery outright and every check stayed green.
It then moved one Stall from the CBW to the CSW read, and found the driver bug
the test could not reach: after a Bulk-IN Stall the device still owes its CSW,
and the driver's fresh CBW is stalled in turn, so the command gave up and
nothing mounted (fixed in #186). The test now also reads some CSWs short,
which QEMU stalls with the CSW owed, and the same deletion fails it.

**Blind by design, and said so.** The device-side CLEAR_FEATURE(ENDPOINT_HALT)
added in #186 is something QEMU does not need: the Stall test passes with it
removed, and with it sent to the wrong endpoint, since QEMU accepts any
endpoint address. The one thing QEMU can see is a refusal, and a class-type
request in its place fails the check with sixty. That reach is written into
the roadmap entry, so a green `--stall` is not read as evidence that a real
stick's halt was cleared. It is not; the board decides.

It is this document's spine again: each was found by breaking the thing
underneath (a decoy, a deletion, a moved Stall) and watching whether the
instrument noticed, and none by the instrument's own green.

## Three more, from the Pi 4's first boots with serial (2026-10-01, evening)

The early fault reporter (#187) was built so a Pi could say what the
firmware's one-line exception report does not. Its first dump on the board
said the kernel was running at EL2, and in doing so named the oldest blind
instrument in the tree.

**A log line that reported the registers, not the machine.** `identity map
installed, MMU running on our own tables` has printed on every post-exit boot
since the MMU milestone, and `mmu.rs` backs it with a check: after the
switch it asks the hardware walker (`AT S1E1R`, `PAR_EL1`) whether the serial
console is Device memory. On the Pi both passed, and neither was true of the
machine, because `TTBR0_EL1`, `TCR_EL1` and `MAIR_EL1` had been written at
EL2, where they govern nothing, and `AT S1E1R` walks exactly those tables.
The check confirmed that the tables the kernel wrote describe what the kernel
wrote. `exceptions.rs` had said since its first day that the kernel "assumes
EL1, not verified at any other EL", which is the honest form of the claim;
nothing turned it into a check, and the instrument that finally did was built
for a different question. The one-line fix that should have existed all
along, log `CurrentEL` after the exit, is step 0 of the plan.

**A round trip spent on the previous build.** The first boot after the merge
was staged from the tree before the merge had reached it, and ran the old
kernel. Nothing on the bench checks which build a card carries. What caught
it was incidental: the kernel's log lines carry their source line numbers,
and those did not match the file; the `armed` line the new kernel prints was
absent. A boot should say what it is: the image range is already logged, and
a build identity beside it (the commit, or the binary's link timestamp) would
have made this a one-second check instead of a round trip read backwards.

**A dump that ordered its fragile part before its robust part.** The
reporter prints the backtrace before the register rows, and on the Pi the
backtrace's tenth frame led the image-naming walk into a translation fault of
its own. The reentrancy guard did its job and halted, and the register rows,
which need nothing but the context the firmware had already saved, were never
printed. The instrument lost its most reliable output to its least reliable
one. Rows first, then the walk, and the walk bounded, is on the roadmap.

## Three more, and the one that worked (2026-10-02)

The day the Pi 4's firmware faults were finally read. Four of them in two
days, at four addresses, in four kernel steps, and the dumps from the
reporter built the day before were in hand for all but the first.

**A rig blind by platform.** Every check this kernel has ran on QEMU, whose
firmware gives the loader a 128 KB stack with its page tables somewhere else.
The Pi firmware gives it 16 KB with the tables directly beneath. The kernel's
deepest pre-exit calls, a debug build with the firmware's timer interrupt
landing on top, pushed past that bottom and wrote frames over the tables,
and the TLB kept the firmware running until a cold page was walked. No rig
could have seen it, and none did: the two-day search went through the xHCI,
the exit, the console and the variable store because those were where the
cold walks happened. The 2026-09-27 section of this document says QEMU
differs from the Pi exactly where the work lives; this is the sharpest case
yet, since the difference was in the one thing the kernel never measured
because it never owned it, the stack it was entered on. The fix is a stack of
its own; the lesson for the instruments is that a kernel's log should say
what it was given (`PcdCPUCorePrimaryStackSize` is a number in a public
file) before it runs on it.

**A bound narrower than it claimed, and honest about it.** The reporter's
image walk was bounded to the memory map's RAM ranges, capped at 64, and the
cap was applied before contiguous descriptors were merged. The Pi's map has
more than 64, so the walk kept the lowest ones and dropped the top of RAM,
where every firmware image lives, and a dump placed `elr` and three
firmware frames in no image. What caught it was the instrument's own line:
`bounded to 3 RAM range(s) ... (the map had more; the rest are outside the
walk)`. An instrument that states its own narrowing is caught on its first
bad reading; one that does not is caught on its first wrong one. The cap
now counts merged ranges, with the smallest giving way at it, and the line
lists the ranges.

**A diagnosis written six times and measured never.** The fault at address
0 that the EL1 drop closed was written down as "the firmware's leftover EL2
timer" in the plan, the rig's docstring, two testing guides, a Makefile
comment and the module doc, then hedged to "most likely the kernel's own
tick" in all six, before a reviewer read the dump's fields: `spsr=0x800003c9`,
EL2h with interrupts masked, `elr=0`, an instruction abort at the same EL.
No interrupt can produce that. It was the first `eret` into task 0, made at
EL2, restoring the firmware's stale `ELR_EL2`. The fields had been on the
screen all along; what was missing was anyone matching them to the
mechanism rather than reasoning from what the kernel had done last. This is
[`true-when-written`](true-when-written-postmortem.md)'s class as much as
this one's, and it is recorded there too.

**And the one that worked.** After the third firmware fault the question was
whether the firmware's table entry was zero (cleared) or garbage (overwritten),
and the instrument for it was a line in the dump: walk the firmware's live
tables for `far` and print each level's entry. It was built and checked on
QEMU in an afternoon, with the planted test fault moved to the first byte
past RAM so the walk had entries to read and the rig could require the
walk's level to agree with the ESR's. Its first boot on the board printed
`L0[0x0] @ 0x3b3fa000 = 0x3b3fa038 (invalid), neighbours: [0x1]=0x388
[0x2]=0x0`, a saved frame pointer and a count where descriptors should be,
and the two-day search ended. The spine of this document is that the
observer is a check too. Its converse held today: an observer that reads the
machine, rather than the registers the kernel wrote or the story the kernel
told, answers in one boot what six written diagnoses could not.

## Three more, from the Pi's USB controller (2026-10-03)

The day the xHCI driver met its first real controller since Parallels: the
Pi 4's VL805, behind the BCM2711's PCIe bridge. Four board boots, four pull
requests (#192 to #195), and by the end a full session over a USB keyboard
and a USB stick. Each stop on the way was a limit or an assumption that
QEMU had made invisible, and each was found by a different kind of
instrument.

**A limit sized by the only controller it had met.** The driver reserved
eight scratchpad pages, and QEMU's controller asks for none, so the number
was never exercised. The VL805 asks for 31. What caught it was the driver's
own refusal, `controller wants 31 scratchpad buffers, only 8 are
supported`: a limit that names itself when it is hit is found on the first
boot that hits it. The rigs could not have; QEMU reports `scratchpads=0`,
which the controller line now logs, so the fact is on every boot rather
than in a comment about QEMU that nothing checked. The compile-time layout
assertions caught the next mistake, a 256-byte array pushing a ring across
a page, before anything ran.

**A store that every working driver splits, and this one did not.** With
the scratchpads in place the first command timed out. The symptom was the
one predicted for non-coherent DMA, whose fix was in and whose self-check
passed, so the reading went elsewhere: the driver against the three that
work on this controller (edk2's, Linux's, U-Boot's), which all write a
64-bit xHCI register as two 32-bit halves where this one used one 64-bit
store across the bridge. The first build of the fix also added a barrier
and a probe that logged what a single 64-bit store read back as. The review
caught that the probe was the suspect access itself, able to damage the
register beside it while reading back intact, and that the round now
changed three things at once. The board round shipped with the split write
alone and passive logging, and its answer was unambiguous: Enable Slot
completed. **An instrument built to observe a suspect operation must not
perform it**; this one would have been the observer that broke the thing.

**A field read at the one instant it is not valid.** A USB 3 stick on a
SuperSpeed root port came out of every reset with `speed=0`, at boot and on
rescan, and the log said nothing more. The next build logged the port's
state before and after the reset and watched it for a second. The first
boot with it read: enabled at SuperSpeed before the reset, in Polling with
no speed at the moment the reset-complete bit was set, back at SuperSpeed
within the watch. The old code had read the speed in exactly that window,
on every boot. The warm-reset fallback built beside the watch was never
needed. The logging, not the reasoning, found it: three plausible causes had
been written down, and the right one was the least interesting.

What the three share is the 2026-10-02 lesson again from a new direction:
QEMU's controller differs from the board in its scratchpad count, its bus
and its link timing, and none of those differences can be seen from QEMU.
What found them was, in order, a limit that says when it is hit, a
comparison with code that already works on the hardware, and a log of the
state rather than the conclusion. The one instrument the review stopped,
the probe, is the one that would have taken the round's answer with it.


## Two more, from the ordering fixes (2026-10-03, afternoon)

Two barrier fixes in the xHCI driver after the board was up, #196 (a `dsb`
before the registers that hand the controller its rings) and
`pi4/trb-cycle-order` (a TD published whole, its first TRB's cycle bit
flipped last behind `dmb oshst`). Neither changes anything QEMU can see.

**Rigs that pass either way.** `test-usb-hub` and `test-el1-drop` were green
on both branches and would have been green without either fix: QEMU's
controller is emulated in the same process and sees guest memory as the
CPU last wrote it, with no write buffer between them. A green rig here proves the change broke nothing; it says
nothing about the change. Both branches said so in their own text, and
both found a second instrument. For #196 it was the image, the count of
`dsb sy` up by four. For the TD it was a mutation that can fail: with the
final flip removed, the rig went red on every device and the timeout dump
showed the Enable Slot command held in the ring with its cycle bit 0. That
proves the hold, not the barrier; nothing on QEMU can prove the barrier,
and the image is the only place it is visible at all.

**An image check written up from a sample.** A count of barriers in the
image proves they exist, not where. The first roadmap note on the
cycle-bit fix said each of the 17 `dmb oshst` sat between the `+0x8` and
`+0xc` stores, written after looking at three. Checking all 17 found five
with other instructions between the barrier and the store; the property
that held for all of them, and the one that matters, is that the first
store after each barrier is the cycle word. The note was narrowed to that.
And nothing in the tree re-runs either check: the counts were run by hand
once, and a later edit can drop a barrier with every rig still green. The
structure narrows that (one function is now the only way a TRB reaches a
live ring, so there is one barrier to lose rather than one per caller) but
does not close it: the branch's second review said so, and a scripted
disassembly check, shown to fail by deleting a barrier, is on the roadmap.

## Two more, from the evening (2026-10-03)

The same day, after the barriers were on the board: the committed check the
afternoon's section asked for, and the build line the 10-01 section asked
for. Each turned up an instrument that would have read the wrong thing.

**An image check run on the build the card does not carry.** Every
disassembly check of the afternoon (the barrier counts, the flip as the
first store after each `dmb oshst`) was run on the release image, because
release is where the pattern was tidy. `make sdcard` stages the debug
build. Writing the committed check showed the difference: in debug, the
flip after `dmb oshst` is a call into `write_volatile`, not an adjacent
store, so the afternoon's property was never true of the kernel on the
card; it was true of a kernel nobody boots. The barrier was there in both,
and the debug build is still correct, but the check was an observation of
a different artifact. The fix moved the property out of the compiler's
hands: each barrier and its store became a naked function of fixed
instructions (#198), the same in every profile, and
`scripts/check-xhci-barriers.py` checks the profile `make sdcard` stages.
Check the artifact that ships, not the one that is easiest to read.

**A build line that would have named the previous commit.** The line that
says which build is running (#199) is baked in by a build script, and the
idiomatic `rerun-if-changed=build.rs` was measured before it went in: after
a commit that changed no source (a new HEAD, the same files), the image
kept the old commit and cargo did not even recompile. That is the one case
the line exists for, a card staged right after a commit. So the script
reruns on every build, at about a second a build, and the QEMU rig reads the
identity from the staged image and requires the boot to log it and its
commit to be HEAD's; a stale ESP fails it. An instrument that says which
build ran has to be the one thing that cannot be stale, so it was mutated
before it was trusted: the idiom was the mutation.

## Four more, from the register rounds (2026-10-04)

A day of four small xHCI changes, each a board round (#200 to #203), and one
rig fix (#204). The controller changes were all spec form: on the Pi 4
every one of them turned out to write what the code before it wrote. That
is what made the instruments the day's real subject, since the only way to
know a change was a no-op was to see it.

**A claim with no instrument under it.** The first version of the RsvdP
change (#201) said in its commit and on the roadmap that the RsvdP bits
"read 0 after HCRST on every controller seen". Nothing in the tree had ever
printed them before the write; the only reads were in the command-timeout
dump, which runs after the writes and only on a timeout. The review asked
where they had been seen. One line at init printing the kept bits made the
claim an observation, and the board then confirmed it (`RsvdP kept: ERSTSZ
0x0, ERSTBA 0x0`). The sentence was true; it had simply never been checked,
and a sentence like that reads the same either way.

**A check that could not fail, said so by its own author.** The follow-up
(#202) put five registers behind one merge helper, and its roadmap note
admitted that no rig could fail on it: QEMU reads 0 in every RsvdP field,
so a helper that dropped the kept bits passed `test-usb-hub`,
`test-el1-drop` and `make test` alike. Saying so was honest and not
enough. The review moved the merge into a pure `const fn` under const
asserts, and two mutations of it then broke the build. Where the system
under test cannot produce the condition, move the property to where it can
be checked: here, the compiler. The same review moved the log line from
after the writes it reports to before them, since a write that hung the
board would otherwise have left the capture without the one value the
round was for.

**An observer shown wrong before it was trusted.** The HCRST question (is
the controller still running when it is reset?) was answered with a
read-only line (#203), and QEMU only ever shows a halted controller, so the
line had only ever printed one answer. It was mutated: with the controller
started just before the read, both lines said `R/S true ... HCH false`. The
same mutation showed something no reading could: QEMU accepted the reset
of a running controller and the rig stayed green, so only the board could
say whether this mattered. It did not; the Pi's firmware hands the
controller over halted. The review also caught that the first version read
HCH and then printed before the write, and a serial line takes long enough
for a controller that was still halting to finish.

**A rig that failed a kernel that never ran, and then lost the evidence.**
One `test-usb-hub` run reported five FAILs. The transcript, 667 bytes, had
no kernel line in it: QEMU's own firmware had stalled in its USB boot. An
instrument that grades a run it never observed reads exactly like a
regression. The fix (#204) calls that case INCONCLUSIVE, and only that
case, because the loose version, "no kernel line", would also have hidden
a kernel that hangs before printing; a saved passing transcript showed
such a kernel ends on `BdsDxe: starting`, and the self-test now holds that
case graded. Its review found two more blind spots in the fix itself: a
count of stalls has no denominator, so it grows the same at any stall rate
(every run's verdict is logged now, and the stall prints as a rate); and
the rerun the rig asks for overwrites the transcript, which is how the
first stall was lost and why its fixture had to be transcribed from the
session that saw it. A verdict that says "run it again" has to keep what
it saw first.

## Four more, from the night (2026-10-04, after the closeout)

The evening closeout ended on *build the observation into the change, and
review the observation as hard as the change*. Three PRs followed the same
night (#205 to #207), and each found a blind instrument, two of them built
that night by the session that had just written that sentence.

**A log field that said what the code meant to do.** The halt before HCRST
(#205) added a field to the reset line, `halted here first`, set from the
condition that decided whether to halt. QEMU hands the controller over
halted, so the halt path was run by mutation: the controller started before
the handover read. A second mutation then kept it running and skipped the
halt. The line printed `HCH false` at the reset write, which caught it, and
beside it `halted here first true`, which lied. The field reported the
branch, not the act. Renamed `found running`, what was read, so a halt shows
as `found running true` beside `HCH true` and a missing one as `HCH false`.
Only the mutation that removed the thing the field described could show the
field wrong.

**One verdict for three failures.** On the Pi, `root` was refused for the
first seconds after the prompt and accepted a few seconds later. The login
says `Login incorrect` for a wrong password, for no `/etc/shadow` entry, and
for any read error from `fsd` other than "no filesystem yet". Nothing on the
screen could say which, and the password is not echoed, so the capture
could not even rule out a typo. #206 gives each lookup failure its own line
and leaves the verdict alone, each line forced once on QEMU by a mutation.
Four boots since have not refused, so the instrument is armed and has
reported nothing yet. That is still better than the old state, where a
recurrence would have taught us nothing.

**The instrument a refactor quietly blinded.** `check-xhci-barriers.py`
requires each barrier store in the image exactly once *and at least one call
to it*. Before #207, `write32` was the store's only caller, so routing a
write around the barrier left the store uncalled, dead, and the check red.
#207 split the writes into handles, and `Kept32::write` called the store
directly beside `Whole32::write`: two callers, so rerouting either left "at
least one call" true. Nobody had asked. It turned up because the review
said a docstring example was vague, and the corrected example was tested
before it was written: callers went from 2 to 1 and the check exited 0. The
reserved-bit writes now go through the whole writes, one caller per store,
and a reroute fails (`found 0 times`). The next review then found the claim
made of that fix still too wide. A reserved-bit write routed around the
whole write is not seen either, since the doorbells keep calling it. That
was shown by mutation too, and the comments now say so. A change that alters
a function's callers can blind a check that counts calls, and nothing in the
check will say so.

**A mutation read through a stale image.** The first run of that mutation
called the script directly on the kernel image already in `target/`, built
before the mutation, while the rebuilding `make` run beside it had its
output hidden. The result, green with two callers, was about the old code.
It was caught because the count had not moved. A mutation test has an
observer of its own, here the image the script reads, and it has to be shown
to be looking at the mutated build: the rerun through `make`, output shown,
said 1 caller.

## Six more, from the storage and C-library day (2026-10-05)

Eight PRs (#209 to #216), each reviewed, and the reviews kept finding the
same thing: a check written alongside the change that could not have failed.
One instrument was the system's own.

**The supervisor's heartbeat saw only what its sampling let it see.** `cp`
of a 758 KB file had `fsd` restarted as wedged near 170 KB. The first account,
written into the roadmap, was one request too slow for the 2.5 s limit.
Counters put into `fsd` said each 2 KiB write finished inside two ticks. The
heartbeat samples task state at the tick, and the tick reaches the CPU only
at EL0, so a tick fired during a disk-read syscall is taken as `fsd` returns
to EL0, when it is runnable by definition. Every sample of a server busy in
the kernel reads runnable, so a long enough stream of requests looks like a
wedge: 128 samples in a row (`WEDGE_TICKS`), which the 758 KB `cp` gave it. Found by measuring the thing
the observer was judging, not the observer's verdict (#211).

**A program that printed the number and exited 0 whatever it was.** The
first `cmem` mallocd the heap and printed its size, `heap 1048576`, and
returned success on any size. With 64 pages it printed `heap 262144` and
still exited 0, so only a person reading the line would see the regression.
Found by the review of #210; `cmem` now fails under 1 MiB, and `make
test-heap` reads its exit.

**A summary that every passing line also contained.** `test-crename` graded
`"crename: ok" in out`, and every passing check line begins `crename: ok`.
A run cut short after its first check would have passed, and the rig typed
`ls /` on that first line instead of waiting for the end. Found by the
review of #215; the rig now waits for and grades the summary line exactly.

**A value compared with itself.** `cfstat` built the struct it expected from
the zero struct plus the four fields `fstat` fills, copying `st_uid` and
`st_gid` from the result itself, and `/etc/passwd` happened to be 0:0. Had
`fstat` never filled them, or filled them from the wrong offset, the check
still matched. Found by the review of #216; the rig now compares them with
`ls -l /etc/passwd`, which reads them through the shell's own path, and a
mutation reading uid from the gid offset fails it.

**A path that the layer below fixed anyway.** The `..` collapse in libc was
tested with `rename("/etc/../x", ...)`. With the collapse removed by
mutation, the test still passed: `fsd` follows `..` entries on disk itself.
The library's part only shows where `..` climbs out of a MOUNT, which no
disk can resolve, so the check now uses `/mnt/f/../..`, and the mutation
fails it on both formats. The mutation was run because the check was new;
had it not been, the check would have been believed.

**A test image that could not show the bug.** The review of #212 found the
free-cluster scan bounded by the FAT's capacity, not the volume's clusters,
so the zeroed entries a rounded-up FAT holds past its last cluster read as
free. The test image's FAT is exactly the size of its volume, so no rig on
it could ever reach them. A copy with 1,000 sectors cut from its recorded
size, filled from the Mac, made the bug reachable: with the old bound the same `cp` wrote about
500 KB past the volume, and `fsck_msdos` named the chain out of range; with
the new one `cp` says `disk full` and `fsck_msdos` finds nothing.

And the one that worked the other way: `cfstat`, written to check that
`fstat` zeroes what it does not fill, failed before the zeroing was even
in, and the reason was larger than its subject. The picolibc port had been
built against the hand-rolled `struct stat`, so `fstat` wrote a program's
file size into `st_dev`/`st_ino`, and Proem's file identity had been the
file size. A check that compares the whole struct, not the fields under
test, saw what a field check would have passed.

## Four more, from the C runtime (2026-10-05, evening)

Steps 1 to 3 of the C-hosting plan (#219 to #221), each with a rig that
passed on its first run. Three of the four were found by the review, one
while writing the check.

**An exit check that read every exit line.** `test-cargs` first graded "every
CARGS exited 0" by counting `exited (code 0)` lines in the whole transcript.
The fence's `echo` and the Rust `ARGS` each supply one, so a C program that
exited 1 would still have been counted among enough zeros. Found while
writing the next change to the rig, before any boot relied on it; it now
reads the exit line that follows the C program's own last output.

**A remote check given a local path.** `cremote` and `cbig` gained their path
as an argument, defaulting to the old remote one. Given a local path, the
remote read became a local read, the "remote write is refused" check was
refused for the read-only fd rather than for being remote, and `cbig`
compared a local file with itself. Every line said ok. Found by the review
of #219; both now refuse a path that does not resolve to a remote mount,
and a mutation making that test always say "remote" fails both refusal
checks.

**A check whose setup failed into its own expected answer.** `cerrno`'s two
mode checks opened a file and then wrote to the read-only fd or read the
write-only one, expecting `EBADF`. The `open` was never checked, and a failed
`open` returns -1, for which the library answers `EBADF` itself, without
asking the server. So a broken `open` would have passed the two checks meant
to reach fsd. Found by the review of #220; each `open` is a check of its own
now.

**A bound named as tested that the rig cannot reach.** `test-cargs` called
its fifteen-argument case "the longest vector a command line can stage". It
is the most arguments, but 163 bytes of the 512 `crt0` sizes its buffer by,
so the code at that bound never ran. Found by the review of #219; the
docstring and the plan now say the byte bound is not reached, and why: the
shell's 128-byte line keeps every blob far below it.

## Nine more, from the C-hosting finish (2026-10-06)

Steps 3 to 8 of the C-hosting plan (#221 to #226), each reviewed until a
round found nothing. Seven of the nine were found by a review, two by a
control run on purpose. Four of the nine were made by the previous round's
own fix, which is
[`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md)'s
spine meeting this one's: the repair added an instrument, and nobody mutated
the instrument.

**A wire check that never saw a syscall number.** `check-wire-constants.py`
matches names exactly, and the C header spells the syscall numbers
`SYS_GET_ENV` where `syscall-abi` says `GET_ENV`. So none of the fifteen was
ever compared, and a typo in one would have made every C program call
another syscall at start-up with `make test` green. Found by the review of
#221; the script now pairs every `SYS_*` with its Rust name, with a floor,
and a mutated number, a renamed one and a deleted one each fail it.

**An expectation copied from the program under comparison.** `test-cenv`
checked `getenv`'s answers against the values `printenv` printed. Had
`printenv` lost `PATH`, the check would have expected "unset", and a C side
broken the same way would have passed. Found by the review of #221; the
expected values are the rig's own, and a control with the shell's default
`PATH` changed fails exactly the two `PATH` checks.

**A comparison of two outputs from one decoder.** #223's `stat` check
compared its record with `fstat`'s, both decoded by the new shared
`stat_from_record`, so a decoding mistake would have agreed with itself.
Found by its review; `cerrno` now checks the mode and owner of two files as
`debugfs` reads them off the ext2 image. A control swapping uid and gid in
the decoder passed the old comparison and failed the new one. The new check
found something on its first run: `/etc/shadow` on that image is owned by
the build host's user (501:20), the carried "ext2 uid 501" item.

**A routing check that two servers answer alike.** With `stat` of a `/net`
path sent to fsd instead of `netd` (a control), "`/net` is a directory" and
"a missing `/net` file is `ENOENT`" both still passed: fsd also has a `/`,
and also says a missing file is missing. Only `/net/ip` told the servers
apart. Found by running the control, not by a review; the `/net` routing was
later taken back out for another reason.

**A tolerance wider than the error it was for.** `test-cclock` allowed the
guest's clock 2 s across a 9 s gap, so a clock 20% wrong passed. Found by the
review of #224. The first tightening, half a second plus 3%, still passed a
control clock made 5% fast (9.55 s against 9.09 s); the gap is 20 s now and
the slack 0.3 s plus 1%, and the 5% clock fails (22.11 s against 21.08 s)
while a correct one passes with 0.01 s to spare. A clock 1000 times slow
passed all nine of `CCLOCK`'s own checks: only the host's clock could see it.

**A by-path check that echoes what it was given.** `test-include` proved
every staged header there by `ls -l` with the file names as operands. `ls`
prints an operand as typed, and FAT32 and exFAT look names up regardless of
case, so the check proved existence and size and nothing about a name's
case, which is what the day's FAT32 fix was about. Found by the review of
#225; the report says what it proves, and the listings carry the case check.

**An observer that quietly finished the job.** `test-cpp` had clang compile
the guest's preprocessed output as `-x c`, with picolibc's include path. A
guest `cpp` that left `#include <stdio.h>` unexpanded would have passed,
clang resolving it from the host. Found by the review of #226; it is
`-x cpp-output` now, and a file with an `#include` left in is a fatal error
there, where `-x c` passed it.

**An exit read for a command never typed, then one borrowed from the next.**
`test-cpp`'s first run reported "cpp picodemo.c exited 0" for a command the
harness never typed: with no echo of it in the transcript, the search read
the whole transcript and found `hello`'s exit. Fixed on that run to read
after the command's own echo; the review of #226 then found it unbounded
the other way, so a run that faulted (no exit line) would borrow the next
command's. It stops at the next command's echo now.

**The fault count, read from a buffer that a kill throws away.** Every QEMU
rig counts fault lines in QEMU's `-d int` trace. Most read it while QEMU ran,
though `drive-qemu.py`'s own `main` warned that QEMU buffers it; the second
review of #226 moved four rigs' reads after `stop()`, and the third found
that `stop()` sent SIGKILL, which discards the buffer anyway. So a fault in a
rig's last command could have been missed in every rig the project has.
Fixed once, in the harness: `stop()` sends SIGTERM, whose exit flushes the
log, and `aborts()` stops the guest itself before reading, so no rig can read
it early.

One more from the same day belongs to this family though it is no observer:
a high-byte input added to catch a lexer leaning on `char`'s sign, with the
reference forced to unsigned `char` like the target, so the one difference it
was for could not show. The third review of #226 found it; the reference is
built both ways now, and the guest must match both. Built both ways, the two
give the same output for those bytes: `cpp` does not lean on the sign there,
and the input proves the bytes survive, not that a sign bug would be caught.

## Ten more, from the editor's console (2026-10-07)

Three of Edit's four items and the first steps of the fourth (#227 to #233),
each reviewed high and then low. Six of the ten were found by a review, four
by a control run on purpose. The day's best instrument went the other way: a
rig that reads the framebuffer back by pixel, decoding each cell with `cond`'s
own font, found a kernel bug no other rig could have seen (#227, the FP/SIMD
registers never saved), because it was the first observer that looked at what
a program had actually drawn.

**A control whose two halves never met.** `fpprobe | fpprobe -` was meant to
switch between two tasks that both held live vector state, and the FPCR
control (the kernel's FPCR restore removed) passed. The first probe wrote its
first line into the pipe and blocked until the second read it, which was after
the second had finished, so the two spins never ran side by side. Found by
running the control; the probe reports once, at the end, and the control fails
with `first: fpcr`.

**A register saved and never checked.** The same probe compared all 32 vector
registers and FPCR, and not FPSR, so removing FPSR's restore left `make
test-fpsimd` green. Found by the high review of #227; each probe sets its own
FPSR flags now, and that control fails with `first: fpsr`.

**A fill that looks the same scrolled.** `test-cond-vt`'s first step filled
every row with `#` and claimed that the bottom row's last glyph must not
scroll the screen. A scroll there shifts identical rows. Found by the high
review of #228; each row is its own letter, and a mutation scrolling only on
the bottom-right cell turns 873 cells red.

**A refusal folded into "unknown".** `ioctl(TIOCGWINSZ)` and
`ulib::screen_size` turned a kernel refusal into "size unknown", which on a
serial console is also the right answer, so `test-cwinsz`'s serial boot could
not see the `CON_INFO` gate closed again. Found by the high review of #230; a
refusal is `EIO` now, and the gate control fails both serial checks.

**A step counted done whatever its wait said.** `test-nav-keys` appended each
step to its done list after the wait, not on the wait's success, so the login
check passed with login's filter removed and `Login incorrect` on the screen.
Found by running that control; a step counts only when its wait matched.

**A check that an undelivered key passes.** The same rig typed `echo a`, six
navigation keys on USB, then `b`, and looked for `ab`. If no USB key reached
the shell at all, `ab` printed just the same. Found by the high review of
#229; a USB `x` among them makes it `axb`.

**A leak check on a program that never exits.** "Nothing of the Down left for
the shell" could only fail if `more` exited right after the key, and with a
3147-line file it never did. Found by running the `more` control (it passed
that check while failing the real one); the check was removed.

**A wait the probe was never in.** `test-kbd-queue`'s message-wait check had
the probe write to the console during its spin, so it would be blocked in
`con_write`'s call to `cond`. `cond` answers within the tick, so the probe
was almost never in the wait when a tick landed, and the check passed with
the waits discarding bytes again. Found by running that control; the probe is
now a pipeline's last stage blocked in `MSG_RECV` for three seconds.

**A probe that drained its own input.** `readkey spin` read every byte
waiting before it exited, so the bytes the kernel's exit and message waits
threw away were never missed, and #232's rig could not see that the PR's
headline case, an editor redrawing, was still losing keys. Found by the high
review of #232, by reading; `readkey spin keep` exits without reading, and
`echo kept` typed meanwhile must run whole in the shell.

**A full queue the check never filled.** #233's whole-key check sent 62
letters then Up: ESC and `[` fit in the two places left and only `A` met a
full queue, so the branch that drops the rest of a key whose front did not fit
was never reached, and could be broken with the check green. Found by the high
review of #233; 63 letters reach it, and both the no-take-back and the
never-drop mutations fail.

## Nine more, from the Ctrl-C plan and `poll` (2026-10-08)

The rest of Edit's request and the `poll` that followed it (#235 to #240),
sixteen high reviews. Six of the nine were found by a review, three by a
control run on purpose or by its output looking wrong. The day's best answer
to an instrument that could not see went furthest: the keyboard queue moved
out of the kernel into the `keyseq` crate, where `make test` can push bytes
at chosen ticks, because no QEMU rig could time the case that mattered.

**A rule masked by its neighbour.** `test-kbd-cut`'s check that nothing typed
after a kill is eaten (decision E1) passed with E1 reverted, because at typing
pace E2's 40 ms bare-Escape interval had already reset the parser before the
next key arrived. Found by running the control: only E1 and E2 removed
together failed it. The rule is now a host test, `nothing_typed_after_an_interrupt_is_eaten`,
which pushes the next byte at the same tick.

**A slot assumed.** `test-kbd-mode`'s "the next task in a raw task's slot
starts cooked" looked for `mode cooked` and never confirmed that `readkey
mode` ran in the slot the raw run had used. Found by the first high review of
#235; both modes print their slot, and the check compares them.

**A wait arm with no check.** Ctrl+\ interrupting the boot shell's stuck
`wait` was a new line in `keyboard_interrupts_wait` that no step exercised.
Found by the same review; the rig waits on `recv` and presses Ctrl+\, later
behind 70 queued letters.

**A no-zombie check that never queued anything.** "A raw owner's queued
Ctrl-C left no zombie" passed whether or not the 0x03 had reached the queue
when the program exited. Found by the fourth high review of #235; `echo qk`
typed behind the Ctrl-C must run as the shell's next line, which proves the
queue held both.

**A readback the recipe could not tell apart.** `test-ctermios` checked that
`tcgetattr` gave back what Edit's raw recipe set, and the mutation "settings
not stored" survived: the recipe only clears flags the console already
lacks, so the fixed settings read back the same. Found by running the
control; a check now sets flags the console never has.

**A reset both runs made themselves.** "The next program starts with `ISIG`
set" could not fail, because both earlier `ctermios` runs turned cooked
before ending, so the kernel's reset at death was never what made the third
run cooked. Found by the first high review of #236; a run now ends by Ctrl+\
while still raw.

**A pass that an error message could give.** `test-edit` counted the editor
open when `hello.txt` appeared after the command, which an error naming the
file, or the shell's "not found", would also satisfy. Found by the first high
review of #240; it waits for Edit's status line, `Line 1  Col 1`.

**A check that a skipped fd passes.** `test-cpoll`'s "with fd 1 ready, fd 0 is
answered too" ran with nothing typed, so a `poll` that skipped fd 0 also
answered `key=0` at once. Found by the second high review of #240; a key is
typed before the poll, and the answer must be `POLLIN`.

**A mutation harness that ran nothing.** The mutation run for #240's third
round wrapped each rig in `timeout 300`, which macOS does not have. Every run
failed to start, the `grep` for `FAIL` printed nothing, and three mutations
read as survivors until the emptiness itself looked wrong. Found by that;
the rerun prints each rig's "driven to the end" line beside the failures, so
a run that did not happen cannot read as a pass. The family's spine in one
line: the harness that runs the controls is a check too.

## Six more, from the stale reply (2026-10-09)

One fix, #244, built three times over four high reviews, with a new rig,
`test-call-interrupt`, rebuilt for each design. Three of the six were found
by running a control and finding it passed, two by review, one by a hang
that turned out to be the rig's image and not the kernel.

**A control the serial line could not fail.** The first design's read-ahead
during a call was guarded by keys typed while the call was held, and the
control (the read-ahead removed) passed: QEMU holds a serial byte until the
guest reads it, so nothing typed on the serial line is ever lost to a guest
that is not looking. Found by running the control; the keys moved to QEMU's
USB keyboard.

**And a control six keys could not fail.** On the USB keyboard the same
control passed again: QEMU's keyboard queues sixteen events, and six keys,
pressed and released, fit. Found by running it; seventeen keys overflow the
queue, and the control then lost eight of twelve `k`s.

**A timing check the script's own sleeps satisfied.** "The call ran to the
peer's answer" was `waited >= hold - 1 s`, and the script slept and typed
for longer than that before it began to wait, so a call cut short at 0.6 s
still read as 3.6 s. Found by the first high review of #244. The checks now
read a snapshot of the transcript 0.8 s after the key, far inside the hold.

**A control whose wrong answer was the right one.** The drain design's "the
abandoned call not recorded" control passed: in the rig's first order, the
late answer the next call took belonged to a second interrupted `cd` to the
same missing directory, so it said "no such file", exactly what the next
call's own answer would. Found by running the control; the steps were
reordered so the late answer ("exists") and the right one differ.

**A guard that compared times, not contents.** One run of the drain design
hung, and it looked like a kernel bug. It was the rig: run directly instead
of through `make`, it booted the image the last mutation control had built,
the "answer not dropped" kernel, whose predicted symptom is a hang. `git
checkout` had restored the source; nothing rebuilt the image, and the rig's
staleness guard, which compares modification times, saw nothing newer than
it. Found by six reruns with debug lines that all passed. Every run since
goes through `make test-call-interrupt`, which rebuilds the image first.

**A prompt check that searched the wrong text.** "The prompt is back" took
the text after the echoed command, and when the echo was not found it took
the whole snapshot, where any earlier `# ` counted. Found by the second high
review of #244; the echo must now be present and the prompt must follow it.

## Four more, from the guard that replaced it (2026-10-09, afternoon)

The last case above was fixed the same afternoon (#247): `scripts/srcid.py`
stamps each ESP with the git tree id of the source it was staged from, and
`drive-qemu.py`'s `Guest` refuses an image whose stamp is not the tree's. Its
control was the morning's trap itself, reproduced (a mutant built, `git
checkout`, the old guard passing and the new one refusing). That control
proved the case the guard was built for and nothing else, and the guard,
being an instrument, had blind spots of its own. All four were found by
Hans's high review of #247, none by a run.

**A guard that broke the rigs it guards.** `test-early-fault` and
`test-el1-drop` boot an ESP directory through vvfat and hand `Guest` a
placeholder path that does not exist, so the guard's `open()` raised before
QEMU started. I had confirmed the guard on `test-heap`, a rig that boots a
disk image like most of them. Now `Guest` takes `stamp_from`, and the check
reads `\EFI\ORBS\SRCID.TXT` from a directory; both rigs pass and refuse a
changed tree.

**A stamp taken at the wrong end.** The tree was hashed after every program
had been built, so an edit made during the build would be stamped as the
source of binaries compiled before it: the exact staleness the guard exists
to refuse, written in by the guard. Now the tree is hashed before the build
and again at the stamp, and a difference fails the build.

**A boot that never asked.** `run-guest.sh` starts QEMU itself, so it never
went through `Guest`, while the docs written the same hour said every boot
did. Its `NO_BUILD=1` mode, kept for mutation runs, was the stale-image case
left open. Now it asks `srcid.py require` and exits 95.

**A refusal described for a stamp that was not there.** The module doc said
an image whose data partition came from an older ESP holds two stamps and is
refused. The ext2 and exFAT payloads copy `/bin` and `/include` from the ESP,
never the stamp, so such an image held one stamp and passed. Now both
payloads carry it as `/etc/srcid`.

The four share a shape: each was a rig or a build step of a different shape
from the one in mind when the guard was written (a directory, not an image;
an edit during the build, not after it; a script that launches QEMU itself;
a partition built separately). The control could not see them because it was
chosen for the case the guard was built for. **A new instrument needs its
controls chosen against its own blind spots, not against the bug it was
built to catch**, and for a guard that claims "every boot", the first control
is to list every boot.

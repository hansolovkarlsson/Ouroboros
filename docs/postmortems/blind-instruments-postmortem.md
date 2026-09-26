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

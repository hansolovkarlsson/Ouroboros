# The day every green signal was wrong (postmortem)

*Process/bug retrospective, a twenty-third piece, 2026-08-29. Not an arc — a
**day of finishing**: the security follow-ups the users arc had deferred
(`/etc/shadow`, supplementary groups, ancestor-`x` traversal, an `accountd`
server) plus a pile of small gaps, taken from one branch too big to review,
split into modules, reviewed, fixed, and merged. The code it produced is
ordinary. What it taught is not about any of that code: it is about the
**signals** we trust to say work is finished, and how many of them were green
while something was wrong.*

## What got built

Two halves of an over-large branch, landed as six PRs:

- **#26 (the feature half)** — `grep` regexes over a new pure `regex` crate,
  libc output buffering, symbolic-mode `chmod`, `chown` by name, a
  virtio-entropy RNG behind a `RANDOM` syscall, `/etc/skel`, an atomic
  `useradd`, one shared account-file reader.
- **#27** — ancestor-directory `x`-traversal in `fsd`'s enforcement.
- **#28** — `/etc/shadow`: secrets out of the world-readable `/etc/passwd`.
- **#31** — supplementary groups carried with the kernel identity.
- **#32** — the documentation, plus a supervisor fix the split had dropped.
- **#30** — `accountd`, still open at day's end, deliberately.

## The spine: a green signal is a claim, not evidence

Six things reported success today while being wrong, and they were not
obscure — they were the *primary* signals for their respective questions:

| Signal | Said | Was |
|---|---|---|
| `read_account_file` returning `0` + a warning | "no accounts, fall back" | **total lockout**, root included |
| The boot log | (a warning, scrolled past) | `fsd` **unsupervised** for weeks |
| GitHub's `MERGEABLE` | "#30 merges cleanly" | merged tree **did not compile** |
| Three code-review passes | 10 → 15 → 15 findings | **not converging**; two `max` runs found *disjoint* sets |
| `usermod -G ""` exit 0 | (printed what looked like an error) | succeeded — **by accident** |
| `e2fsck -fn` clean | filesystem consistent | it was; consistency was never the question |

None of these was a bug in the signal. Each was a signal answering a slightly
different question than the one being asked. That is the whole lesson, and the
counter-practice that actually worked is at the end.

## Part 1: the review that would not converge

The combined branch collected the entire security tier. A local `code-review`
pass found 10 findings; fixing them and re-running found 15; fixing those and
re-running found 15 again. Two separate `max`-effort passes over the same diff
produced **largely disjoint** finding sets — which is not a reviewer failing but
a statement about the diff: past a certain size, what a reviewer finds is a
sample, not an inventory.

Worse, the fixing was itself producing defects at roughly **one self-inflicted
escalation per round**: a non-root group escalation introduced while closing a
group finding, a shadow truncation window opened while fixing shadow ordering, a
shared regex step budget. One "fix" was a **silent no-op** — a namespace reset
that the kernel's own range check rejected, caught only by running it in QEMU
rather than reading it.

The conclusion was not "review harder". It was that **a diff too large to review
is also too large to fix**, because every fix lands in a context nobody is
holding in their head any more.

## Part 2: what the split found

Splitting into one module per PR was not a cleanup gesture; it immediately found
bugs that three passes over the combined diff had missed. The `/etc/shadow`
lockout is the one worth keeping.

The shell reads an account file into a fixed 2 KB buffer and, on overflow,
prints a warning and returns `0`. That behaviour is **deliberate, documented,
and correct** — for `/etc/passwd`, where `0` means "no accounts" and `login`
starts a root session, exactly as it does on a fresh disk.

`/etc/shadow` came through the same reader. There, `0` means "this user has no
secret" — so every password is refused, **root's included**, with no fallback
and no way to repair it from the machine. The two halves of one database had
**opposite failure modes for the same overflow**, and the shadow half crosses
first: its lines are ~90 bytes against a four-field passwd line's ~30, so it
passes 2 KB at ~23 accounts while `/etc/passwd` is still under 700.

The reasoning in that function's comment was written for `/etc/passwd` and was
sound. It was inherited by a second caller for whom every word of it was false.
**A safety argument does not travel with the code that carries it.**

The fix removed the class rather than raising the number: the secret lookup
streams one line at a time, so the size of the credential database no longer
decides whether anyone can log in.

## Part 3: what the split dropped

Splitting is not free, and the cost lands in exactly the places no test covers.

Reconstructing modules onto fresh branches left behind everything that was not
obviously part of a module: **nine documentation files**, and a one-constant
kernel fix raising `supervisor::IMG_CAP` from 128 KB to 192 KB. That second one
mattered: `FSD.BIN` is 137 KB, so **`fsd` — the server the entire supervision
machinery exists for — had not been restartable at all**, announced by a single
boot warning among forty that says "too large to keep for crash recovery" and
scrolls past before the login prompt.

I found it by accident, reading unrelated boot output. Nothing else would have:
there is no test for "the recovery path you never exercise is still armed."

The lesson is not "don't split." It is that a split needs an explicit
**reconciliation step** — diff the original against the union of the pieces —
and that this must happen *before* the source branch is closed. Doing it caught
the supervisor fix and the docs; skipping it would have lost both silently, and
the docs would have been lost in a *squash*, where they are genuinely
unrecoverable.

## Part 4: MERGEABLE is a claim about text

After #32 changed `supervisor::register` from returning `bool` to returning a
`Registered` enum, GitHub reported #30 as `MERGEABLE`. It was: no textual
conflict, because #30's contribution is a *fourth call site* on lines #32 never
touched.

```
error[E0600]: cannot apply unary operator `!` to type `Registered`
  --> kernel/src/loader.rs:456:8
```

A merge tool answers "do these two texts overlap". It cannot answer "does the
result mean anything". For any change to a shared signature, the only real check
is to **perform the merge locally and build it** — which took ninety seconds and
would otherwise have broken `main`.

## Part 5: the practice that worked — prove the test can fail

Against six false-green signals, one habit did the actual work: **making the
failure reproducible before trusting the fix.**

- **The lockout.** After the streaming fix passed on a 3814-byte `/etc/shadow`
  with the real accounts past byte 3640, I reverted *only* that change and ran
  the identical test. It reproduced exactly: `warning: the account file is
  larger than this shell can read - ignoring it`, then `Login incorrect` for a
  correct root password. Only then was the passing run evidence rather than
  decoration.
- **The registry message.** Rather than read the new `Registered::RegistryFull`
  path and believe it, I set `MAX_SUPERVISED` to 2, booted, and read the actual
  string off the console — then restored it and confirmed the warning was gone.
- **The merge.** Built it instead of trusting the flag.

All three are the same move: *construct the failure*. It is cheap — minutes —
and it converts "the code looks right" into "the test discriminates."

The counter-example from the same day: `usermod -G ""` **did** clear the group
list, while printing `no such group (skipped): ""` and exiting 0. It worked only
because no group happens to be named `""`. A passing outcome, an
error-shaped message, and a success exit code, all simultaneously — which is
what "it worked when I tried it" is worth on its own.

## Part 6: the git mechanics, which are not a footnote

Squash-merging a PR whose branch is the **base of another PR** does not
retarget the stacked PR. It **closes** it, and GitHub then refuses to reopen it
once the head has been force-pushed. That cost #29 its number (its work survives
as #31).

The fix is ordering, not vigilance:

1. merge the base PR **without** `--delete-branch`
2. rebase the stacked branch onto the new `main`
3. retarget the stacked PR (`gh pr edit N --base main`)
4. *then* delete the old base branch

Applied to #31 → #30, that worked with nothing lost.

## Lessons

1. **A green signal answers the question it was built for, not the one you are
   asking.** Six of them were green today while something was wrong.
2. **Prove the test can fail.** Revert only the fix; force the branch; build the
   merge. Minutes, and it is the difference between a test and a decoration.
3. **A safety argument does not travel with its code.** `read_account_file`'s
   overflow reasoning was correct for its first caller and catastrophic for its
   second, with the comment still there asserting the original case.
4. **A diff too large to review is too large to fix.** Non-converging reviews and
   ~1 self-inflicted bug per fixing round were both symptoms of one cause.
5. **Splitting is not free — reconcile explicitly.** Diff the original against
   the union of the pieces *before* closing the source, or lose whatever did not
   belong to any module. Here: nine doc files and an unsupervised `fsd`.
6. **`MERGEABLE` is a claim about text.** For a shared signature, merge and build.
7. **Merge a stack in the order that keeps it alive**, and record the branch tips
   first — the recovery from any bad rebase is a SHA you wrote down.

## What this day did not finish

`accountd` (#30) is rebased, builds, and boots, with **five review findings
unfixed** — including the `/etc/shadow` mode predicate that #28's fix could not
reach, because on that branch `passwd` becomes a pure IPC client, and a
recycled-slot TOCTOU that needs a kernel generation counter to fix properly.
Stopping there was deliberate: it is the module with the largest new surface,
and the day had already demonstrated what fixing under pressure produces.

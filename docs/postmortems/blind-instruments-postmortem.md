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

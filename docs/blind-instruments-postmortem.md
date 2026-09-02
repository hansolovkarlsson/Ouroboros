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

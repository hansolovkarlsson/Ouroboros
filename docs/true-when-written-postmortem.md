# True when written

*A process retrospective — the twenty-ninth — covering 2026-09-03. Five PRs, no
new subsystem, and four defects that were all the same defect: a statement that
was correct on the day someone typed it, and stopped being correct without
anyone touching the file it lived in.*

The previous day's retrospective
([`blind-instruments-postmortem.md`](blind-instruments-postmortem.md)) had the
spine *the observer is a check too, and it is the one nobody mutates*. The one
before it ([`repairing-the-repairs-postmortem.md`](repairing-the-repairs-postmortem.md))
found, among other things, a `rm -rf` guarded by a comment asserting the path
was safe, and drew the rule **a claim in a comment is not a check**.

This is the sequel to that rule, and it moves one step in a direction that is
harder to defend against:

> **THE COMMENT WAS TRUE WHEN IT WAS WRITTEN. THAT IS THE PROBLEM.** A claim
> that is wrong on arrival can be caught by anyone who reads it carefully. A
> claim that is *right* on arrival cannot — it passes every review it will ever
> get, and then some unrelated change in some other file makes it false, and
> nothing anywhere fails.

The `rm -rf` comment was wrong the moment it was typed. Every claim below was
accurate, defensible, and worth writing. One of them was falsified **four and a
half hours later**.

---

## The four

### 1. "A later phase concern" — which arrived the same afternoon

`ulib::fs_mv` resolved both of its paths through the namespace and then
dispatched on the *source's* target, handing that server the destination's
string. Above it:

```rust
// Both paths resolve through the namespace; in Phase 0 every binding is
// tree 0, so a cross-tree move can't arise yet (a later phase concern).
```

Correct, carefully reasoned, and explicit about its own scope — the good kind of
comment. It was written in `fce52d1`, *cluster: Phase 0 step 0c — per-task
namespaces + bind*, at **12:06 on 2026-08-25**.

`a9e7342`, *cluster: Phase 1 step 1c — the remote-mount client*, landed at
**16:37 the same day**. Four and a half hours. The "later phase" the comment
deferred to was the next commit series of the same afternoon, and `/proc` and
multi-mount followed within days.

The consequence sat there for nine days. `mv /F.TXT /mnt/a/NEW.TXT` renamed the
file to a **local** `/NEW.TXT`, left the remote mount untouched, and **exited
0**. No `NP_MV` ever reached the peer. The file was not where the user asked it
to go and nothing said so.

It was found on 2026-09-03 only because an unrelated fix — making a test peer
answer `FS_ERR_NOT_FOUND` instead of a generic error — unblocked the path that
had been reaching it. Before that, `mv`'s destructive-overwrite guard stopped on
"cannot tell whether the destination exists" and never got as far as the bug.
**A second defect had been standing in front of the first one, hiding it.**

### 2. A filename rule that a feature removed

The shell's error table rendered `FS_ERR_INVALID_NAME` as:

```
invalid name (must fit this kernel's 8.3 short-name subset)
```

True when written on **2026-08-17**, when the collapsed `FS_ERROR` sentinel was
split into specific codes. FAT32 could only create 8.3 short names, so telling
the user that is exactly right.

**2026-08-27**: `959a984`, *fsd/fat32: create long filenames (LFN write), not
just read them*. From that commit on, a name that does not fit 8.3 gets a
generated short alias beside its LFN entries. The restriction was gone; the
message describing it was not.

Ten days to falsification, seventeen to discovery — and it would still be there
if it had not been sitting next to something else being fixed.

### 3. A documented recipe that a client outgrew

`scripts/np9p_server.py` is a host-side 9P server whose docstring shows the
guest commands it supports:

```
mount -r 10.0.2.2:5641 /mnt/a
ls /mnt/a
cat /mnt/a/HELLO.TXT
```

Correct on **2026-08-25**, when the peer was written. `ls` at that point called
only `fs_list_dir`.

**2026-08-27**: `3cf79d1` gave `ls` its `-l` form and `54a9b01` gave it file
operands — and with them a call to `fs_stat`, so `ls` began stating a named
operand before listing it. The peer implemented no `NP_STAT`. From that day
`ls /mnt/a` failed, while `cat` under the same mount kept working.

Nothing in that story is a mistake. `ls` was right to grow the feature; the peer
was adequate for the recipe it documented on the day it was written; the recipe
was accurate. **The falsifying change was in `programs/fileutils/ls/`, and the
claim it falsified was in `scripts/`.**

It cost a full debugging session on 2026-09-03, and worse: it was recorded in
the roadmap for three days as a *guest-side path-resolution bug* — "probably an
empty path where the server expects `/`" — a diagnosis that was wrong in both
of its parts and would have sent the next reader into `resolve_ns`.

### 4. Mine, and it was never true at all

Two of the day's own claims, written by me while fixing the three above.

The wire-constant checker excludes some names deliberately. I added:

```
# NOT checked either: STAT_INFO_LEN's field OFFSETS. Only np9p_server.py
# spells them ... so there is no second declaration to compare, same as
# NP_MAC_LEN above.
```

Both halves false, and not by expiry — **false on arrival**. The script compares
each peer to the Rust crate, not the peers to each other, so one peer spelling a
name is sufficient; and `NP_MAC_LEN` is excluded for an unrelated reason (Rust
does not declare it at all). Adding the four offsets took one line and caught a
mutated offset immediately.

Then, having been corrected on that, I wrote in its place:

```
# NOT checked, and genuinely unpinnable as this script stands: STAT_FLAG_DIR
```

Also false. The bit was invisible because the script's patterns matched `usize`
and a literal, while the constant is `u32` and `1 << 0`. **The script's reach is
a property of the script.** I had recorded a limitation of my own tool as a fact
about the thing it was failing to see, in the sentence immediately following a
correction for doing that. Two regex lines per language fixed it.

There is also a smaller one of the same shape: the checker's `PEER_BASELINE`
carried prose saying "today the client spells 6 of these names and the server
4". The server had gone to 6 on **2026-09-01**, two days earlier. I then raised
both numbers again on 09-03 **without reading the sentence eight lines below the
value I was editing**.

---

## The mechanism: the falsifying edit is in a different file

Every one of these was falsified by a change somewhere else:

| the claim | lives in | falsified by a change in |
|---|---|---|
| "a cross-tree move can't arise yet" | `ulib/src/lib.rs` | `ninep-abi`, `netd`, the shell (remote mounts) |
| "must fit 8.3" | `programs/shell/src/main.rs` | `programs/servers/fsd/src/fat32.rs` |
| "`ls /mnt/a`" in the usage block | `scripts/np9p_server.py` | `programs/fileutils/ls/src/main.rs` |
| "the server spells 4" | `scripts/check-wire-constants.py` | `scripts/np9p_server.py` |

That single fact explains why none of the three defences this project relies on
caught any of them:

- **The compiler cannot see it.** Every one is a string or a comment. The code
  around them kept compiling perfectly, because the code was never the problem.
- **Tests cannot see it.** There is no test that a comment is true. The `mv` case
  had a *behavioural* consequence and still no test failed, because no test
  moved a file across a mount — the feature the comment said could not exist.
- **Review cannot see it.** A reviewer reads a diff. The diff that falsifies the
  claim **does not contain the claim**. The Phase 1c commit that broke `fs_mv`'s
  comment does not touch `fs_mv`; a perfect review of it would not have looked
  at the line it invalidated.

This is what makes the class different from the ones the last three
retrospectives covered. A check that cannot fail is at least *present* at the
place where it matters. An expired claim is present at a place that nobody has
any reason to visit.

---

## Two species, needing different answers

They are not all the same problem.

**Expired truths** (cases 1–3, and the baseline prose). Correct when written,
falsified later, by someone else, elsewhere. The author did nothing wrong. No
amount of care at writing time prevents these, because at writing time there is
nothing to catch.

**Never-true claims** (case 4). Wrong on arrival, and plausible enough to
survive. These *are* preventable at writing time, by the ordinary discipline of
checking a claim before making it — the same discipline as writing a test that
can fail. Both of mine took under a minute to disprove once anyone tried; nobody
tried, including me, and I wrote the second one immediately after being
corrected on the first.

Worth separating because the second species is the embarrassing one and the
first is the dangerous one, and conflating them leads to the wrong remedy —
"read more carefully" does nothing for a comment that was accurate when read.

---

## Duplication as an accidental detector

The one case found by anything resembling a system was the 8.3 message, and the
detector was **duplication**.

The shell keeps its own copy of the error-message table because it has its own
filesystem layer and cannot share `ulib`'s. That is normally a hazard, and this
project has written about it as one. But when the two copies were laid side by
side — for an unrelated reason, while fixing `ls` — they disagreed, and the
disagreement is what exposed the stale message. The shell was also missing two
codes entirely.

**Two copies of a claim are a poor cross-check, but they are not zero, provided
something ever compares them.** Nothing did; a human comparison happened by
accident.

The contrast with constants is exact and uncomfortable. Wire *constants* have a
comparer: `scripts/check-wire-constants.py` reads three implementations and
fails when they disagree — and over the day it grew from 12 to 27 names,
catching every mutation aimed at it. Wire *prose* has nothing. The same day's
constant drift was caught mechanically in under a second; the same day's prose
drift was caught by luck, days late, three separate times.

---

## A related shape, earned two days later: the guard that stopped at one sibling

*Added 2026-09-03, after three more instances turned up in the same window.*

Case 4 above records that I wrote a lesson — *repairing one half of a pair is
not repairing the pair* — and that a review showed the history did not support
it for the case I had attached it to. That correction stands: the peer was
adequate when written, and a client grew a dependency.

But the lesson itself turned out to be real. I had simply claimed it before
earning it, on the wrong evidence. Three genuine instances appeared within two
days:

- **`ls` alone never called `ulib::fs_error`.** Every other command under
  `programs/fileutils/` rendered its errors through the shared helper, which
  distinguishes every `FS_ERR_*` the servers can return; `ls` hardcoded
  `"no such file or directory"` for all of them. A helper existed and one
  caller did not use it. (Deliberately no counts here: both the number of
  commands and the number of codes changed within a day of this being written,
  and a rotting number in *this* document would be a poor joke.)
- **`ls` alone never exited non-zero.** One `exit` call in the whole file,
  `exit(0)`, where every sibling exits 1 on failure. Same command, second
  instance, found separately.
- **`net_call` retried `MSG_ERR_DENIED`; `np_remote` and `np_netlocal` did
  not.** The retry carried a comment naming the hazard — "the brief
  delegation-not-yet-applied window" — and that comment was *true*, and scoped
  to the one function it sat in, while the window itself is a property of
  **every** call to `netd` from a spawned program. Two of the three callers
  raced. That was the residual half of the flake this project had been
  recording as a TCP problem for weeks.

The shell's duplicate error table, in the section above, is a fourth of the
same family — a copy that drifted rather than a caller that abstained, but the
same failure to travel.

**What connects it to the rest of this document** is scope. `net_call`'s
comment is a correct statement about a hazard, written in one of the three
places the hazard applies. Nothing is false; nothing expires. The *claim* is
local and the *condition* is global, and they diverge in silence exactly the
way a true-when-written comment does — because the thing that would reveal the
mismatch is, again, an edit in a different file: the sibling that was written
later, or the caller that was never revisited.

**The practical form.** A fix lands where the bug was *observed*, not
everywhere the bug is *possible*, and nothing marks the difference. So when a
guard, a retry or a shared renderer is added, the question is not "does this
fix the failure" but **"how many places could have this failure, and how would
I enumerate them?"** — which is answerable mechanically, and was in
every case here. `grep -c 'ulib::fs_error' programs/fileutils/*/src/main.rs`
returned a zero for exactly one command (it returns none now). Listing every
`MSG_CALL` to `NET_TASK` and asking which of them retried `DENIED` returned two
of three. Neither took a minute, and neither is prompted by the diff in front
of you — which is the whole difficulty: the question has to be asked
deliberately, because nothing in the change you are making raises it.

The fix for the third instance is the one to imitate: the retry now lives in a
single `net_msg_call` that all three callers share. A fourth caller cannot
reintroduce it, because there is no longer a version of the code that omits
it — [`unspellable-postmortem.md`](unspellable-postmortem.md)'s *make the wrong
thing unspellable, not un-grepped*, applied to a hazard rather than to a
privilege.

## The shape at its largest, earned a day later: a public copy, twelve days stale

*Added 2026-09-04, from a housekeeping pass that went looking for something
else entirely.*

Every case above is a claim inside the repository, falsified by an edit to
another file in the same repository. The largest instance of the shape is not
in the repository at all — it is **served to the public**.

`docs/` is a GitHub Pages site, and `docs/site/` is 23 hand-written HTML pages
abridging documents under `docs/`. Nothing generated them, so nothing noticed
when a source moved on and its page did not. The site **froze on 2026-08-23**.
Twelve days later it was still presenting v0.17.0 as current, with two releases,
twelve merged PRs, a closed frontier item and this postmortem absent from it —
and the pages themselves read as confident and finished, because they were true
when written.

That is the same mechanism as the four cases above, with two aggravations. The
distance between the claim and the edit that falsifies it is now a **file
format** as well as a file: no compiler, test, grep or review crosses from
`manual.md` to `manual.html`. And the audience is external, so the cost is not a
developer misled for an afternoon but a stranger given a wrong answer for as
long as nobody looks.

**The detector had to be built carefully, and the obvious version would have
been born green.** The natural check is "is the source's last commit newer than
the page's?" — and dates fail here twice over:

- **A repo-wide mechanical edit levels them.** The same day, a
  `roadmap.md` → `ROADMAP.md` rename rewrote all 23 pages and all their sources
  *in one commit*. Anchored on the current history, a date-based check would
  have reported every page fresh on the day it was written — a green signal
  produced by the rename, not by the site being current.
- **Dates are too coarse to see a same-day edit.** Comparing last-commit dates
  found 7 stale pages. Comparing the source **blob hash** each page was written
  against found **9**. The two dates missed had a matching date and 19–20
  changed lines each, one of them an entire *"this section's premise is now out
  of date"* block that had never reached the public page — a correction that had
  itself gone stale in copy.

So the check records the exact blob each page was abridged from. The cost is the
opposite error: a mechanical edit applied to *both* sides still changes the
blob, and reports a page that is genuinely in sync. That happened at exactly one
page (two lines, both the rename). It was diffed rather than assumed, confirmed
in sync, and re-stamped alone. Over-reporting in that direction is the safe one,
and saying which direction a check errs in is part of specifying it.

Two things this adds to the rules above rather than restating them:

- **A copy in another format is the same claim, with every detector removed.**
  The `duplication as an accidental detector` section notes that two copies
  disagreeing is what exposed the 8.3 message. That only works when something
  *lays them side by side*. Across a format boundary nothing ever does, so the
  duplication stops being an accidental detector and becomes a pure liability.
- **A detector for staleness can itself be stale-by-construction.** The
  date-based version would have passed on the day it was written, for a reason
  that had nothing to do with the property it claimed to check — the exact
  failure [`blind-instruments-postmortem.md`](blind-instruments-postmortem.md)
  is about, reached by a different road. It was caught only by asking what a
  known-stale page *should* produce and finding the answer was "green".

The check is `scripts/check-site-freshness.py` (`make check-site`), verified by
mutation: a changed source flags its page, an unregistered page under
`docs/site/` fails the manifest, and a manifest entry whose file is gone fails
too. The nine stale pages are not fixed by it and were not fixed that day — see
[`ROADMAP.md`](ROADMAP.md). It is deliberately outside `make test` until they
are, because a suite that is permanently red teaches people to ignore it, which
would end with the detector as stale as the thing it detects.

## What actually worked

Three things, none of them "be more careful".

**Compute the property instead of stating it.** The `fs_mv` fix does not say a
cross-target move cannot arise; it compares the two resolutions and refuses when
they differ. This is the lesson
[`cluster-keys-postmortem.md`](cluster-keys-postmortem.md) reached from the
other direction — a hand-transcribed table of small-order keys replaced by
`[8]P == identity`, the definition, which cannot be transcribed wrongly. A
statement decays; a computation of the same property does not, because the thing
that would falsify it now breaks the computation instead, loudly.

Note what this buys in case 1 specifically: the comparison lives in the same
function as the dispatch it constrains, so a future change to the dispatch
cannot get past it.

**Ask the question mechanically.** `FS_ERROR` was added to the checked
constants not because anyone remembered it, but because the question "which
names does a peer spell that Rust also declares and this list omits?" was asked
as a script rather than from memory. It returned exactly one name — one nobody
would have thought to look for. The generalisation is cheap: when maintaining a
list of things that must agree, derive the *candidates* rather than curating
them.

**Forge the condition and watch the damage.** The roadmap claimed a drifted
`STAT_FLAG_DIR` would make "every remote directory render as a file". Rather
than ship that as a prediction, the bit was set to `1 << 1` and the guest run:

```
# ls -l /mnt/a
-rw-r--r--    -    -         0        -         /mnt/a
```

The mount **root** is classified as a file, so `ls` prints one zero-byte entry
and never enumerates the directory — the contents do not appear at all, exit
code 0, no error at any layer. Worse than the prediction, and now the roadmap
says what actually happens rather than what seemed likely.

---

## The honest limit

Most claims cannot be converted into checks. "In Phase 0 every binding is tree
0" is a statement about a design era, not a computable property, and demanding a
check for every such sentence would mean writing far fewer of them — which would
be worse, because the comments in this codebase are load-bearing and several of
today's fixes depended on reading old ones.

So the realistic position is narrower:

- **A claim that guards behaviour must be a check.** `fs_mv`'s comment was not
  documentation, it was a *precondition* — the code was only correct while it
  held. That is the category to convert, and it is recognisable: if the code
  would be wrong when the claim is false, the claim is a check that has not been
  written yet.
- **A claim that merely explains may stay prose**, and should say when it was
  true. "In Phase 0…" was doing this honestly; it was simply attached to code
  that depended on it.
- **A user-facing string is a claim with the widest blast radius**, because it is
  read by people who cannot check it. The 8.3 message told users a rule about
  their own filenames that had not been true for ten days.

And one observation to close on, stated carefully because the tidier version of
it is the kind of claim this whole retrospective is about. **All five of the
day's PRs corrected a statement** — an error message, a comment, a docstring, a
roadmap diagnosis, or an index that had drifted from the directory it described.
Three of them also fixed real logic: a missing verb, a status code, a cross-mount
rename. So the honest form is not "the day was all prose"; it is that *every*
change, whatever else it did, turned out to be carrying a correction to something
the code had said about itself.

In each of the four cases above, the code was doing exactly what it had been
written to do. What had rotted was the description — and the description is what
the next person reads first, which is why a wrong one cost three days of a
roadmap entry pointing at the wrong subsystem.

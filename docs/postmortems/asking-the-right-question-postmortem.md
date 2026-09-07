# The question a server is actually asking (postmortem)

*Design/bug retrospective, a twenty-fourth piece, 2026-08-30. The day the
users/permissions arc closed for good: the five outstanding review findings on
the account server fixed, `accountd` merged as the fourth server, and
a kernel-level hole in **shipped** code closed underneath it. Two threads run
through it, and neither is about accounts. The first is that three of the five
findings were mechanisms answering a question **adjacent to** the one being
asked. The second is that five separate facts were correctly changed in one
place and left standing in their restatements elsewhere — and not one of those
five was visible to the compiler or to any test.*

## What got built

- **#36** — the kernel binds a message's credential at **send**
  (`SENDER_ID` 65 / `SENDER_GROUPS` 66); `fsd` authorizes on that.
- **#30** — `accountd`, the fourth server (protected slot 5), so a user can
  change their own password. All five prior review findings fixed.
- **#37 / #38** — the documentation drift the above exposed.
- **#24** — the original over-large branch, reconciled and closed.

## The spine: the mechanism answered an adjacent question

Not one of the interesting findings was a mechanism that *malfunctioned*. Each
worked exactly as specified, and the specification answered a question one step
to the side of the one the caller had.

| The caller asked | The mechanism answered |
|---|---|
| "Who **sent** this request?" | `GET_ID(sender)` — who occupies slot N **now** |
| "Is this file **protected**?" | `read_file_all` — bytes read, with *failure* folded into `0` |
| "Did the update **land**?" | a whole-file write — which truncates **first** |

An adjacent answer is far more dangerous than a wrong one, because it is
correct almost always. `GET_ID(sender)` is right whenever the sender is still
alive — which is nearly every request, and every request anyone ever tested.

## Part 1: the recycled slot

`GET_ID(sender)` answers *"who occupies slot N now."* A server authorizing a
request is asking *"who occupied it when the request was made."* Those come
apart in a specific window: a caller does `MSG_SEND` — which does **not**
block — then `EXIT`, and its slot is reaped and re-spawned before the server
drains its mailbox. `GET_ID` then reports the *new* occupant. If that is root,
the request is authorized as root.

Three things made this worse than it first looked.

**The earlier fix felt like the fix.** `GET_ID` had already been made to refuse
a slot with no live task, precisely so a caller could not send-and-exit into a
root-by-default identity. That closed the *dead*-slot half and read, at the
time, as closing the problem. A **recycled** slot is perfectly alive; nothing
in the message distinguishes it, because a message carries a bare `u8` slot
number and no generation.

**The window is the ordinary path, not an exotic one.** Slots 5+ (now 6+) are
exactly the pool the shell recycles for every single command. This was not a
race requiring adversarial timing to reach; it is the normal lifecycle.

**It was in shipped code.** The finding was raised against the unmerged account
server, but `fsd` had the identical hole — on every permission check and every
fid op — and had had it since enforcement landed. That is why the fix was split
out and merged on its own rather than waiting behind a feature branch.

### The fix, and the two things that only became visible once it existed

Capture the credential at `send_message`, where the sender is unambiguously
itself: into the queued `Message` when it queues, straight into the receiver's
cell on the direct-delivery fast path where no `Message` ever exists.

That much was obvious. Two consequences were not:

**Identity and groups must be captured together.** `SET_ID`'s group half exists
*precisely* so identity and membership cannot be set out of step. Capturing a
bound identity and then reading a live group list would have rebuilt the same
hole one field at a time — a caller could keep the uid it sent with and inherit
the recycled occupant's groups.

**A reply must not overwrite the capture.** `fsd` writes to the console through
`cond`, via a blocking `MSG_CALL`. The reply to that call is a message, and a
naive "credential of the last message received" would have been replaced by
`cond`'s the moment `fsd` logged anything mid-request. The rule that fixes it —
**only an unfiltered receive updates the cell** — is precise rather than
defensive: a request always arrives on an unfiltered receive, and a reply to
your own `MSG_CALL` never does. The value of stating it that way is that it
turns "read the credential immediately, before you call anyone" from an
invariant every future server must *remember* into a property of the mechanism.

### The boot hang, and why it was diagnosable in one step

First boot after the change hung one second in. The supervisor's health ping
sends as `KERNEL_SENDER` (0xFE) — not a task slot at all — so the credential
snapshot indexed off the end of the array and panicked inside the tick handler.

The fix is a bounds check, and the right *semantics* were the interesting part:
such a message now carries **no** credential rather than a synthesized root
one, so anything attempting to authorize it fails closed.

What made this a ten-minute problem instead of an afternoon is that **the same
script had been run against `main` first, and passed**. Not as ceremony —
because the "does it still work" question is worthless without knowing the test
can say no. That practice came from yesterday's postmortem and paid for itself
on its first use here.

## Part 2: the fix the review recommended did not exist

Finding 4 was that the credential database was rewritten with a single
whole-file write. On `ext2` that is truncate-then-write — the overwrite branch
frees the old blocks *before* the new ones land — so an `fsd` restart
(documented to happen) or a power loss in that window leaves `/etc/shadow`
**empty**. An empty shadow file locks out every account, root included.

The review's suggested fix was the standard one: write a temp file, then
rename over the target. **That is not available here.** `mv` refuses an
existing destination on all three filesystem arms, so replace-on-rename would
be a filesystem arc of its own. (Recorded in `ROADMAP.md` as a real gap, with
the note that `ext2` can very nearly make it atomic — replacing a name means
overwriting one directory entry's inode number in place, a single block write —
while FAT32/exFAT have no such indirection and are unavoidably
delete-then-rename.)

It turned out not to be needed, and the reason is a property of the **data**
rather than of the filesystem: a shadow line's salt and hash are fixed-width
hex, so changing a password leaves the file **exactly as long with every other
byte identical**. The update can therefore be written as just the bytes that
differ, at their offset. Nothing is ever truncated, and the worst an
interruption can do is damage the one entry being changed while every other
account still logs in.

That is a *smaller* guarantee than atomic rename, and the right one. The
property that matters here was never "the change is all-or-nothing" — it is
"**a failed change never locks anyone out**." Naming the actual requirement
made an unavailable fix unnecessary rather than blocking.

## Part 3: the test that could not fail

`accounts::changed_span` computes the differing range, and got six host tests.
One of them was worthless, and only a deliberate mutation showed it: a test
named for a loop bound that guards nothing, because the backward scan is
already stopped by the very byte the forward scan stopped on. Mutating the
bound away left every test green.

The test was renamed to describe what it actually checks, and the code comment
now says the bound is belt-and-braces rather than load-bearing. A test that
cannot fail is worse than no test, because it gets **counted** — six passing
assertions read as more coverage than five.

The other direction was checked too: mutating `changed_span` to return the
whole buffer fails three tests, so the suite does discriminate. Both halves are
needed. "The tests pass" and "the tests would notice" are different claims.

## Part 4: five restatements, none checkable

Both ultra reviews came back nearly empty — two nits on the account server, one
on the kernel change, against fifteen findings on the combined branch the day
before. That is about as direct a measurement of the previous day's split as
this project is going to get.

The **shape** of what they found is the lesson. Not one was a logic error.
Every one was a fact correctly changed in one place and left standing in its
restatement somewhere else. Three more turned up the same day, found three
other ways:

| What drifted | Found by | Would have bitten |
|---|---|---|
| Four comments still enumerating "tasks 0-4" beside a guard covering 0-5 | ultra review | whoever adds the sixth server |
| `libc/include/sys.h` pinning the error floor at `MAX-33` after Rust moved to `MAX-38` | ultra review | a C caller reading `ACCT_ERR_IO` as **success** |
| A doc link to `SENDER_IDS`, which never existed | ultra review | a reader chasing the security rationale |
| A manpage naming `SET_GROUPS`, a syscall designed and never shipped | reconciling #24 | a user, directly |
| `CLAUDE.md`: ten slots, four servers, two disagreeing crate counts | reading it | the next session, before it writes a line |
| "the fifth server" — `accountd` is the **fourth**; 5 is its slot number | counting them during the doc pass | anyone reasoning about the server fleet |

The last row was written *into this postmortem* an hour before it was caught,
and had reached ten places including merged code comments. Restating a number
from memory instead of deriving it is the same failure the rest of this section
describes, and knowing about the failure is evidently not protection from it.

**None were visible to the compiler or to any test.** That is the category, not
bad luck: a comment, a manpage, a doc link, an unused C constant and a prose
count are precisely what neither checks. The code holds one copy of each fact;
the prose holds several; nothing keeps them in step.

Three responses, in increasing order of durability:

1. **Fix the instance.** Necessary, insufficient.
2. **State the invariant instead of enumerating.** "Every slot below
   `FIRST_SPAWNABLE`" cannot go stale; "tasks 0-4" already had, once, when netd
   became the fourth server.
3. **Give the drift a reader.** `cargo doc` had been reporting nine unresolved
   intra-doc links, eight older than this week. Nine accumulated for the
   obvious reason — nothing read the output, so the signal was useless by the
   time the ninth arrived. Fixing all nine means the tenth is *visible*. That,
   not the eight fixes, was the point.

## Part 5: reconciling before closing the source

PR #24 — the over-large branch the previous day had split — was closed today.
A squash-merged branch is unrecoverable, and the previous day's split had
already silently dropped nine doc files and an `IMG_CAP` fix, so it was
reconciled first rather than closed on memory:

- Of the **207 identifiers** it introduced, **203 are present on `main`**.
- The four absent were checked individually and are deliberate renames
  (`SET_GROUPS`/`SET_GROUPS_DENIED`/`SET_ID_GROUPS` folded into `SET_ID`'s
  group half; `restrict_shadow` → `ulib::write_private_file`).
- Every file it added exists on `main`.

The reconciliation paid for itself: it is what found the manpage naming a
syscall that does not exist. Worth noting that the *first* attempt at this
check reported all 207 identifiers missing — a broken `grep` invocation, where
`--` had ended option parsing so `--include` was read as a filename. A check
that reports catastrophe is at least loud; a check that reports success while
broken is the failure mode worth fearing, and it is the same one as Part 3's
un-failable test.

## Lessons

1. **Ask what question the mechanism actually answers.** `GET_ID(sender)` was
   never broken. It answered "who is in slot N now" — correct, and adjacent to
   "who sent this". Adjacent answers are dangerous *because* they are right
   almost always.
2. **A guard that closes half a window can read as closing it.** Refusing a
   dead slot felt like the fix for send-and-exit. The recycled slot is alive.
   When you close a hole, state precisely which half you closed.
3. **Capture credentials at the moment of the act, not the moment of the
   check.** And capture the whole credential — identity and membership move
   together, or the hole comes back one field at a time.
4. **Make the safe usage a property of the mechanism, not a rule to remember.**
   "Only an unfiltered receive updates the credential" beats documenting "read
   it before you call anyone", because the second is an invariant every future
   server can violate.
5. **Name the requirement before accepting that a fix is unavailable.** The
   requirement was "a failed change never locks anyone out", not "the change is
   atomic" — and the weaker, available guarantee satisfied it.
6. **Prove the test can fail, in both directions.** Run it against the tree
   without the fix; mutate the code and confirm it goes red. This session's
   `KERNEL_SENDER` hang was diagnosed in minutes because of the first, and a
   useless test was caught by the second.
7. **The prose drifts and nothing watches it.** Prefer invariants to
   enumerations; put a mirroring note at the *definition*, not only at the
   mirror; grep `manpages/` and `docs/` when renaming anything user-visible;
   and keep at least one machine-checkable signal (`cargo doc` unresolved
   links at zero) so the next drift is visible instead of buried.
8. **Reconcile before closing an unrecoverable source.** Identifier-level, not
   impression-level — and sanity-check the checker, because a broken check
   reporting success is the same failure as a test that cannot fail.

## What this day did not finish

**Per-user cluster identity** — the last item in the arc, and the only one
left. The 9P export relays every remote request under `netd`'s own root
identity, so `check_access`'s root bypass short-circuits before any mode is
consulted: an unprivileged user on node B can `mount -r <A> /mnt/a` and read
every hash on node A, and `cpu A passwd root` now reaches the account server
with root authority. Not a regression — the export has always been
machine-authenticated rather than user-authenticated — but `accountd` existing
gives it a reachable payload it did not have this morning.

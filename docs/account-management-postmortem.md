# Account management — the day the OS could make its own users (postmortem)

*Design/bug retrospective, a twenty-second piece, 2026-08-28. The
continuation of the users/permissions arc
([`users-and-permissions-postmortem.md`](users-and-permissions-postmortem.md),
the 20th): that arc built the identity + login + enforcement machinery; this
one built the **tools to manage accounts** on top of it — `passwd`, `useradd`,
`groupadd`, `usermod`, name resolution, groups, and per-user home directories —
and, in doing so, exercised the enforcement path with a genuinely new caller
(a non-root user actually **using** its home) for the first time, which is where
the real lessons came from.*

## What got built

A shared **`accounts`** crate (repo-root, pure `no_std`, no I/O, no syscalls,
**host-unit-tested**) holding all the account-database logic — `/etc/passwd` +
`/etc/group` parse/format, SHA-256 hashing, salt derivation, name↔id lookups,
and the file-rewrite helpers — and, over it, six user-facing pieces:

- `/bin/passwd`, `/bin/useradd`, `/bin/groupadd`, `/bin/usermod` (all root-only);
- `su <username>` and `id` with **name resolution**;
- `/etc/group` (`name:gid:members`) with a **primary-gid** group model;
- per-user homes under **`/Users`**, `~`→`$HOME` shell expansion.

The whole thing changed the kernel **zero** times. It's a userland arc — a crate
plus tools plus a shell change plus one fix in `fsd`. But the spine isn't "it was
easy"; it's what the arc *exposed*.

## The spine: closing an arc means running old code with a new caller

The earlier arc's postmortem spine was "the feature was a JOIN, not a build."
This arc's is its sequel: **the tools were mostly a join over existing
machinery, but the value was in what a new caller revealed.** Before this arc,
*everything ran as root* — the `user` account existed and login could drop to
it, but home was `/`, so a logged-in user could never actually create a file
anywhere. The moment `/Users/user` became a user-owned directory the user was
expected to write in, an entire latent bug surfaced.

## Bug 1 (the keystone): new files were born owned by root

`echo hi > ~/note.txt` as `user` was **denied**, and `ls -l` showed the file
created but owned `0 0`. ext2's `touch`/`write_file`/`mkdir` hardcoded
`uid: 0, gid: 0` on every new inode. It had never mattered — everything that
ever created a file ran as root, so root-owned was correct by accident. Now a
non-root user's `> ~/file` created a root-owned file (allowed: it has write on
the *parent*), then the follow-up data write to that now-root-owned file was
correctly **denied** by the very enforcement the previous arc shipped.

**The fix** is small and the lesson is not: `fsd` stamps the caller's identity
onto the filesystem (`set_creator`, from `GET_ID(sender)`) before each op, and
ext2 creates with it (root → `(0,0)`, unchanged for boot/format). The lesson —
**a new privileged path exercises code that only ever ran as one caller** — is
the same shape as the previous arc's "root-bypass hides enforcement bugs," seen
from the other side: here the *absence* of any non-root creator had hidden a
correctness gap for the entire life of the write path.

## Bug 2 (the foreign reviewer's catch): overwrite silently chowned

Fixing bug 1 introduced a subtler one, and I didn't see it — a **code-review
agent did**. `write_file`'s overwrite branch restored the existing file's
`mode` and `links` from the old inode but **not** its `uid`/`gid`, so with
creator ownership now live, *overwriting* an existing file re-owned it to the
writer. POSIX overwrite never chowns; this did. The fix is one line each for
uid/gid, mirroring the mode/links restore that was already there.

Two things worth keeping:
- **The reviewer found the branch I didn't think to guard.** Same lesson as the
  interactive-shell arc's ultrareview catch (FG+Ctrl+C killing a protected
  server): when you add a new lever (here, "creator owns new inodes"), a foreign
  reviewer finds the *existing* code path where the new lever misbehaves. The
  create path was obviously right; the overwrite path was the one that regressed.
- **The blast radius was bounded by the very check that exposed bug 1.** An
  overwrite-chown only fires when the caller *can* write the existing file — so
  it needs a non-owner-writable file (mode `666`, say). Not catastrophic, but a
  real ownership-integrity break, and exactly the kind of thing that's invisible
  until someone reasons about the overwrite-of-a-shared-file case. Fixed before
  merge; `touch` (no-ops on an existing file) and `mkdir` (errors) were confirmed
  *not* affected, so `write_file` was the only site.

## The decision that mattered: option 1 on a shared helper

The one real design fork was **who may write `/etc/passwd`**. Self-service
`passwd` means a non-root user modifies a root-owned file, and there's no setuid
mechanism and no privileged account service. Three options: (1) root-only tools;
(2) a dedicated `accountd` server (self-service works, microkernel-clean, but a
whole new server); (3) a setuid bit (Unix-authentic, adds an escalation surface).

Chosen: **option 1, built on a shared helper** — because it's a *strict subset*
of option 2. The on-disk format, the hashing, the lookup/rewrite logic all live
in the pure `accounts` crate; the only thing options 2/3 add is a privileged
*write path* on top. So picking option 1 now defers the "which privileged
mechanism" decision without a rewrite: a future `accountd` reuses the crate
verbatim and just repoints the tools at it. This is the "build the consumer when
it exists" discipline (the capability-and-hardening postmortem's warning against
premature mechanism) applied to a security feature: don't build the account
*server* until self-service is actually wanted; build the account *logic* now,
factored so the server slots in.

The corollary shaped the code: **factor the shared logic into a pure crate, not
into the first tool.** Inlining parse/hash/rewrite into a root-only `useradd`
would have made the later upgrade a rewrite; the `accounts` crate makes it a
repoint.

## The pure crate paid off immediately (host tests caught my own fixtures)

`accounts` is deliberately I/O-free and syscall-free — callers pass byte buffers
in, and the salt entropy is a parameter, not a read. That purity made it
**host-unit-testable** (`cargo test -p accounts --target aarch64-apple-darwin`,
`#![cfg_attr(not(test), no_std)]`), and the tests earned their place on the
first run: three failed instantly — not because the code was wrong, but because
my **test fixtures used truncated password hashes**, and the parser correctly
rejects a line whose hash isn't exactly 32 bytes. The foreign observer here was
the crate's own contract: writing the test forced me to produce *valid* fixtures,
which confirmed the parser's strictness was real. A pure crate is the cheapest
foreign observer you can build — no QEMU, no boot, just `cargo test`.

## Groups: primary-gid, because the kernel identity is one packed word

"Assign a user to a group" wants group *membership*, which is plural. But the
kernel carries a single packed `(gid<<32)|uid` per task, and `fsd` checks one
gid. Full supplementary membership would mean widening the kernel identity to a
group *list* and threading it through enforcement. So the scoped answer:
**primary-gid** — `usermod -g` / `useradd -g` set the passwd `gid` field, and
`/etc/group` gives groups names; the members list is recorded but informational.
`useradd` with no `-g` creates a **user-private group** (`gid == uid`, the Linux
default) so `id` shows a name. Supplementary groups are named as the next tier,
not faked. Same discipline as the previous arc's "deliberate first cut."

## Salts: clock-derived, and said so loudly

Runtime password hashing needs a fresh salt, and the only entropy source in
userland is `MONOTONIC_US` (predictable). Rather than pretend, `make_salt` hashes
the clock sample (spreading it across 8 bytes so two registrations a microsecond
apart differ) and the doc comment states plainly that this is **weak** — an
attacker who can bound the registration time can bound the salt — with a
virtio-entropy `RANDOM` syscall named as the real fix. It keeps salts
*per-account-unique* (defeating a shared rainbow table across our own accounts),
which is what an 8-byte salt is *for*; it just isn't unpredictable yet. The
principle (from cluster-auth): **state the security tier you're at out loud**,
don't let a weak primitive masquerade as a strong one.

## A testing gotcha worth remembering: PL011 has no RX FIFO

Scripted-boot testing under QEMU `-nographic` bit hard. Piping the whole input
script at boot **dropped almost all of it**: QEMU pushes stdin bytes into the
guest's PL011 receive register, and with the guest not reading until the shell's
login prompt (~70 s later under TCG), the bytes overflow the tiny RX buffer and
vanish. The fixes that worked: **wait for the `login:` prompt to appear in the
log, then feed input one line at a time, paced** (a few seconds each), holding
the fifo open on a spare fd. Even then a plain `\r\n` line ending left a stray
`\n` that the password field read as an empty password — send `\n` only. And the
two-session harness proved reliable where a single-session variant raced, for
timing reasons not worth chasing. The durable lesson: **a real UART's RX is not
a pipe buffer** — scripted serial input must be paced against the guest actually
reading, or it's lost. (See [[reference-qemu-stdin-guest-shell]].)

## The spec correction: `/Users`, not `/User`

The home base shipped as `/User` (singular) and the user corrected it to
`/Users` (plural) — a spec call that's theirs to make. The rename was a clean
global replace anchored on `/User\b` (word boundary), which is **idempotent**:
`\b` doesn't match inside `/Users` (the `s` is a word char), so re-running it
can't double-apply, and it never touches an absolute `/Users/hans` host path.
A one-line spec change, but a reminder that home-directory layout is a product
decision, not an implementation detail — surface it, don't bury it.

## Recurring traps that showed up again

- **Store-size ≠ read-size, a third time.** `id` read `/etc/passwd` with the
  512-byte inline read while `login`/`su`/`useradd` read the full ~2 KB, so `id`
  couldn't resolve names past ~5 accounts (the review caught this too). The
  account files are now read chunked into a 2 KB buffer everywhere. This exact
  bug (a read cap narrower than the store) bit the login step and the env-export
  step before it.
- **The PIE str-slice trap stayed retired.** Every new tool and the crate work
  in `&[u8]` (byte-only parse/format, a hand-rolled `Writer`), so none of them
  pulled in `core::fmt`'s panic formatter — verified with `llvm-readobj -r`
  showing zero `R_AARCH64_ABS64` across all of them. See
  [[reference-str-slice-pie-trap]].

## What's left (named, not hidden)

Self-service `passwd` (the `accountd`/setuid tier the crate is built for), a
**virtio-entropy RNG** for strong salts, **supplementary group membership**
(needs a kernel group list), `/etc/shadow`, the ancestor-directory `x`-traversal
check, and per-user *cluster* identity. Each is a deliberate deferral with its
reason attached, in [`roadmap.md`](roadmap.md).

## The shape of the lesson

An arc that "just adds tools" over a finished mechanism is exactly where latent
bugs in that mechanism surface, because the tools are the first code to use it
the way it was meant to be used. The keystone bug (root-owned new files) had
lived in the write path since day one and was invisible until a non-root user
had a home to write in; the second bug (overwrite-chown) was *created* by fixing
the first and caught only by a foreign reviewer. Build the shared logic as a pure,
host-testable crate; scope the group model and the salt honestly; and when you
give files a creator, remember that overwriting one must preserve its owner, not
claim it.

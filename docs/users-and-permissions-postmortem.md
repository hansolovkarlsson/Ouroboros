# The users & permissions arc — giving the OS a real "who"

*A design retrospective (a twentieth piece, 2026-08-28). The arc that took
Ouroboros from a single implicit user with no access control to an authenticated
login, a per-task identity, and enforced file permissions: five merges —
mode/owner `stat` surface, `chmod`/`chown`, kernel identity, `login`, and `fsd`
enforcement. The roadmap's #1-ranked gap, closed.*

For the milestone facts see `CHANGELOG.md`; for the day's narrative see
`journal.md`. This is the retrospective — the threads that ran through the arc,
and the two places the design changed under contact with reality.

## The spine: the feature was a *join*, not a build

By the time this arc started, all the parts of a permission system existed
separately and did nothing together. ext2 had stored owners and mode bits since
the filesystems arc — read but ignored. The IPC layer had always told `fsd`
which task sent a request — used only for `SAFECOPY`, never for *who*. What was
missing was never one big mechanism; it was the **wiring between three things
that already worked**. That shape recurs late in a system's life: the
high-leverage move stops being "add a subsystem" and becomes "make the
subsystems you have finally talk." The whole arc is four small surfaces
(`stat` fields, two verbs, two syscalls, a login prompt) plus one `if` in
`fsd`'s dispatch.

Which is why the *keystone* was the least glamorous piece: a mode/owner triple
on the `stat` record. Nothing about it is hard. But `ls -l`, `chmod`/`chown`,
login policy, and enforcement all hang off it, so it was correctly ranked #1 —
the smallest change that unblocks the most. **Rank by what a thing unblocks, not
by how much code it is.**

## Reversing a roadmap note on purpose: identity belongs in the kernel

The roadmap had a tentative line: identity is *probably* a userland construct
(a login server, an identity carried in the capability set), *not* a uid field
in the task struct — reasoning from the "personality lives in userland" stance
the POSIX-divergence work established. Building it, I reversed that, and the
reason is worth stating because it's a good test for where any security
primitive belongs.

The question is: **what is the unforgeable part?** A permission check is only as
trustworthy as the binding between "this request" and "this user." The kernel is
the one component that authoritatively knows an IPC message's real sender — it
stamps it. So the kernel is the only place a task→identity binding can't be
forged. Everything *else* about users — names, passwords, `/etc/passwd`, home
directories, login policy — genuinely is userland, and stayed there. The kernel
learned exactly one new thing: a task has a uid/gid, which are numbers. That's
the whole reconciliation with "personality in userland": the kernel owns the
minimal unforgeable *mechanism*, userland owns the entire *model*.

The lesson generalizes the posix-divergence one. There, a goal phrased as a
*feel* ("POSIX-ish") didn't constrain the architecture and lost silently. Here,
the roadmap's guess was a *hypothesis*, not a commitment — and the load-bearing
requirement (an unforgeable binding for a security feature) is what actually
decides, overriding the aesthetic lean. Roadmap notes written before the design
exists are hypotheses to test at build time; reverse them out loud, with the
reason, when the build teaches you better.

## The obstacle that changed the design — and re-asking when the tradeoff moved

Login is where the arc bent. The user had chosen the classic model: a `login`
process as init (task 0) that authenticates and spawns a *per-session shell*.
I started building it and hit a wall that wasn't visible from the plan: the
capability model bakes in **"slot 0 is the shell."** `caps_for_slot` hands slot
0 the shell's powers (`TO_NET` to delegate to network commands, `TO_SPAWNABLE`
to reach its children), and `TO_SHELL` is literally `1 << 0`. Moving the shell
off slot 0 means a reserved session-shell slot, retargeting `TO_SHELL`, one
fewer spawnable slot, and **re-validating every IPC flow** — exactly the
security-sensitive rewiring the capability-and-hardening postmortem warns a
moved lever demands.

The right response to "the chosen path costs far more than it looked" is not to
power through it silently, nor to quietly substitute a different design. It's to
**surface the obstacle and re-choose** — the tradeoff the user decided on had
materially changed, so the decision was theirs to revisit. The alternative was a
POSIX **saved-set-uid**: keep the shell on slot 0, let `login` drop it to the
user, and let logout restore root (a non-root task may `SET_ID` back only to its
*saved* identity). Same login → session → logout → re-login experience, and
**zero capability surgery**. The only thing given up is that a "session" is the
same shell process cycling identity rather than a fresh process — a difference
the user never sees. That's the general lesson: when the expensive design and
the cheap one deliver the same *observable* behaviour, the observable behaviour
is the spec, and the cheap one wins — but the person who picked the expensive
one gets to make that call once you can show them the real cost.

## The hole the cheap model opened, and closing it at the right layer

Saved-uid has a trap the getty model doesn't: the shell's saved identity *is*
root (that's what makes logout work), so a logged-in user's shell could just
`SET_ID(0)` and escalate. The kernel can't distinguish "logout restoring root"
from "a user restoring root" — both are the same task calling the same syscall.

The fix is a layered argument, and it's why it holds:

- **The kernel guarantees the containment that can't be bypassed.** A child's
  *saved* identity is set to its own (inherited) uid, not the parent's — so a
  user's spawned programs can never restore root, full stop. The only task that
  *can* restore root is the one shell that dropped from it.
- **The trusted component enforces the policy.** That one shell is the login
  TCB; it's trusted not to hand root back arbitrarily. So `su` is made
  **root-only at the shell** — a non-root session's `su 0` is refused before it
  reaches the kernel. The kernel's saved-restore then has exactly one caller:
  logout, which immediately re-prompts login. A user can momentarily *be* root
  only during a non-interactive logout transition, with no way to run anything.

The generalizable point: a dual-use mechanism (here, `SET_ID`-to-saved serves
both logout and, dangerously, `su`) is safe when the *unbypassable* half lives
in the kernel and the *policy* half lives in the trusted userland component —
and you can state, in one breath, why no other path reaches the capability.

## The foreign observer, and diagnosing by signal not story

Two testing habits earned their keep again. `chmod`/`chown` edit an on-disk
inode; the validation wasn't our own reader agreeing with itself but **macOS's
`e2fsck`** calling the filesystem clean afterward and `debugfs` reading back the
exact `Mode`/`User`/`Group` — a genuinely foreign checker, the discipline the
filesystems arc established.

The counter-example was more instructive. Login couldn't read `/etc/passwd` and
fell back to a root session; the console showed `fsd` mounted *before* the
failure, so my first story was a boot mount-race. It wasn't. The actual return
code was `FS_ERROR`, not `NO_FS` — and the cause was that the shell's inline
`fs_read_file` passes `want = buf.len()`, which `fsd` rejects above
`FS_DATA_MAX` (512); my passwd buffer was 1024. A plausible story (the race) had
to yield to the actual signal (the error code). And it was the *second* time in
two days the same asymmetry bit: the day before, env-export's `GET_ENV` failed
because the read buffer was sized to the whole-blob store (2048) rather than the
per-read cap (512). **Store size is not read size** — worth internalizing as its
own rule.

## The deliberate first cut, and the old traps that didn't retire

Enforcement ships as an honest partial: it checks the *object* of an operation
(and the parent directory for create/delete/rename), but not the search (`x`)
bit on every ancestor directory the full POSIX model walks. That was a choice,
not an oversight — the ancestor-`x` check would have to live inside each
filesystem's path walk, whereas the object check lives as one function in
`fsd`'s dispatch. A bounded, centralized, honestly-documented first cut delivers
the feature and stays reviewable; the traversal check is a clean follow-up. Same
for remote requests: they arrive through `netd` (a root server) and stay
machine-authenticated by the cluster key — per-user *cluster* identity is a named
next tier, not a silent gap.

And the recurring traps stayed true to form:

- **The PIE `&str`-slice wall.** `fsd` needed a `parent_of(path)` helper;
  slicing a `&str` by a runtime index pulls in `core::fmt`'s panic formatter
  (`R_AARCH64_ABS64`, unlinkable under `-pie`). Worked in `&[u8]` and re-wrapped
  with `from_utf8`, exactly as `[[reference-str-slice-pie-trap]]` prescribes.
  The trap didn't fire because the code was written to not invite it.
- **The error band shifted again.** A new `FS_ERR_PERM` meant moving
  `FS_ERR_MIN` down one more slot — the third code (`FS_ERR_NOT_SUPPORTED`,
  `FS_ERR_PERM`) to ride that mechanism this month, and it stays safe because
  every caller imports the symbol, never the literal.
- **Root bypass as the safety valve.** The one design choice that makes an
  enforcement bug survivable: since uid 0 skips the check entirely, a mistake in
  `check_access` can only ever *over*-restrict a non-root user — it can never
  lock root out of its own machine. Build the escape hatch into the security
  mechanism itself.

The arc closes the oldest gap in the gap-analysis. What's left are refinements
with their own follow-up notes: `/etc/shadow` (the hashes currently sit in a
world-readable `passwd`), `passwd`/`useradd`, per-user `/home`, groups, the
ancestor-`x` traversal, and per-user cluster identity. See
[[project-users-permissions-arc]] and `docs/ROADMAP.md`.

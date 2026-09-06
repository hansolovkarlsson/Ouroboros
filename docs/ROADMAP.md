# Ouroboros roadmap

Forward-looking plan — what's next and why, in plan form rather than
chronological narrative. **Completed arcs and milestones have been moved
out** to [`roadmap-completed.md`](roadmap-completed.md) (the plan-shaped
record — how each arc was sequenced and what was learned) and
[`CHANGELOG.md`](CHANGELOG.md) (the condensed milestone log), so this
document stays about what's *still open*. For *how* something already built
actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see the postmortems under `docs/` and `CLAUDE.md`.
This document is the one to update first when direction changes.

> **The long-term direction** — a Plan 9-style **resource-sharing cluster**
> (distributed Ouroboros: machines sharing storage/devices/services over
> per-machine namespaces and a uniform file protocol) now has its own phased
> plan in [`roadmap-cluster.md`](roadmap-cluster.md). The Plan 9 "local
> namespace + uniform protocol" work below is **Phase 0** of that arc — the
> foundation the whole distributed vision builds on, not a standalone item.

## What's next (the current frontier)

The microkernel arc is largely built — the FAT32 **filesystem** (`fsd`),
the **console** (`cond`), and the **network** server (`netd`) all run as
supervised, MMU-isolated userland servers, with a capability model, crash
recovery, and grant/safecopy bulk IPC (all in
[`roadmap-completed.md`](roadmap-completed.md) / [`CHANGELOG.md`](CHANGELOG.md)).
So is the users/permissions arc, whose last item — per-user *cluster* identity —
shipped 2026-08-31, and the per-machine-keypair arc that followed it
([`roadmap-cluster-keys.md`](roadmap-cluster-keys.md)), which retired the shared
cluster key entirely. What is left is the tier below: keys are per-*machine*,
not per-*user*, which is item 1. In rough order of value (2 and 3 are what
the microkernel arc itself still leaves open):

1. **Per-user keys for the cluster.** Per-user cluster *identity* shipped
   2026-08-31 (see [`CHANGELOG.md`](CHANGELOG.md) and
   [`unspellable-postmortem.md`](unspellable-postmortem.md)): a remote request
   carries the requesting user's **name** inside the signature, the far side resolves
   it through its own `/etc/passwd` and refuses a stranger, and the identity
   reaches `fsd` as a **required parameter** carried in the request rather than
   an opt-in wrapper and a latch. `cpu` is covered too — `netd` assumes the
   mapped user's identity for the spawn, so a remote command inherits it.

   **What is left is the tier below it: keys are per-machine, not per-user.**
   The shared secret is gone — each machine has its own Ed25519 keypair and
   authorizes peers by public key, so a member can be revoked by deleting a line
   — but an authorized *machine* can still claim any of its own users' names. So
   the model defends against the users of a trusted node — the real exposure —
   but not against a compromised node. Per-user keys would close that, and the design forks are
   real: whether each user gets a key or the machine key signs a per-user
   credential; where those live (`/etc/cluster/keys/<name>`? a factotum-style
   agent, as Plan 9 does it?); how a node learns a peer user's key without a
   distribution mechanism this project does not have; and whether any of it is
   worth building before Ouroboros leaves a trusted network, which is the
   trigger the rest of the security tier already sits behind.

   **A designated auth server — the Plan 9 answer, evaluated 2026-08-31.** Hans
   asked whether one machine could be the cluster's identity master, with the
   others obtaining authentication from it. Recorded here in full because it is
   the natural next question, the answer is "yes, and it is what Plan 9 does",
   and the reasoning for *not building it yet* is the part that will be needed
   again.

   Plan 9 has exactly this: `authsrv` issues tickets, `factotum` holds a user's
   keys, `secstore` keeps the shared secrets. It is the Kerberos family
   (Needham-Schroeder). So the shape is well-trodden, and it fits this project's
   lineage rather than fighting it.

   **The detail that decides whether it works at all.** If the master's role is
   "B asks the master, the master says yes, B tells A it was approved", it fixes
   **nothing** — A is still trusting B's word, which is the whole of the current
   gap. It works only when the master's answer is a **ticket A can verify without
   trusting B**: the master shares a key with *each* machine and issues B a
   ticket MAC'd under **A's** key, naming the user. B cannot forge it, A verifies
   it against a key it already shares with the master, and the username becomes
   *attested* rather than *asserted*. Any design discussion that skips this
   distinction is discussing something that does not close the hole.

   **What it would buy.** N keys instead of N² — every machine shares one key
   with the master and none with its peers, which is the real argument at three
   or more nodes. One place to add or revoke a user, against today's model where
   `map_user` resolves through *each* node's `/etc/passwd`, so a user must exist
   everywhere and revocation means visiting every machine. And, in its strongest
   form — the *user* authenticating to the master at `login` and receiving a
   ticket — the user's secret stops living on the asking machine, which is the
   one thing that would defend against a **compromised node**. Neither the
   shipped design nor per-user keys stored on each node manage that.

   **What it would cost here specifically**, which is where it stops being cheap:

   - **A clock.** Tickets need lifetimes; this OS has `MONOTONIC_US` (per boot),
     no wall clock and no time sync, so two machines cannot agree on "expires
     at". Either build time sync, or replace expiry with a challenge nonce — an
     extra round trip.
   - **Which collides with the export's shape.** The export is **one TCP
     connection per request**, which is precisely why v0.10.0 chose a
     client-nonce MAC over challenge-response. Per-op ticket handshakes would be
     brutal, so it wants a ticket cache in `netd` — the task with no heap, a
     32 KB stack that has hit the guard page five times, and no mutable statics
     (the auth config already threads as `&Auth` for that reason).
   - **A single point of failure that is also the highest-value target.** Master
     down = no new sessions; master compromised = the whole cluster. Plan 9 lives
     with this; it is a real cost, not a footnote.
   - **A new machine role** in a design that is currently peer-symmetric —
     though Plan 9 was itself role-split (cpu / file / auth servers), so this is
     consistent with the model rather than against it.

   **The fork to settle before writing any code**, and it is bigger than the
   crypto: a master changes **where identity lives**. Today each node is
   autonomous — it resolves a name through its own `/etc/passwd` and may refuse a
   stranger. With a master, identity becomes cluster-wide and node-local accounts
   become secondary. That is a philosophical change to the cluster, not just an
   authentication mechanism, and it should be decided deliberately rather than
   arrived at.

   **The cheaper step that came first: per-machine keypairs — ✅ BUILT
   2026-08-31.** Each node holds its own Ed25519 keypair and lists the peer
   *public* keys it accepts (SSH's `authorized_keys` model); the shared secret
   is deleted. No new server, no clock, no ticket cache, no single point of
   failure. See [`roadmap-cluster-keys.md`](roadmap-cluster-keys.md) for the
   step log (and for why the *symmetric* version was rejected: with a symmetric
   key, the ability to verify is the ability to forge),
   [`roadmap-completed.md`](roadmap-completed.md) for the plan-shaped summary,
   and [`cluster-keys-postmortem.md`](cluster-keys-postmortem.md) for what it
   cost to learn.

   It killed "one shared secret = interchangeable members" and gave per-peer
   revocation — the largest single weakness of what shipped in v0.15.0. It
   deliberately left **"B can claim any of its own users"** open, which is
   exactly the residual a master exists to close: that is now a *measured*
   remainder rather than an assumed one, which was the point of building this
   first. Two costs it introduced, worth weighing against a master: a peer list
   caps a cluster at about a dozen nodes (`AUTHORIZED_MAX`), where one secret
   scaled without limit, and key generation **refuses without real entropy**, so
   platforms with no RNG (Parallels, the Pi) need keys staged at build time.

   All of it stays behind the **"leaving a trusted network" trigger**. Today's
   deployment is two QEMU VMs and, soon, two Raspberry Pi 4s on a home network,
   where the shipped machine-key model is proportionate. The master earns its
   cost when there is a node that is not fully trusted, or enough nodes that N²
   key distribution genuinely hurts.

   Two smaller follow-ups from the same arc, both deliberate scope calls rather
   than oversights:

   - **Supplementary groups do not cross the cluster.** The identity word is one
     `u64` (uid + primary gid), so a remote caller is authorized on its primary
     group alone. This can only ever *deny* access a local session would grant,
     never grant one it would deny. Carrying the list needs either a second word
     or a payload extension, and the thing to preserve is that the groups can
     never arrive out of step with the identity they belong to.
   - **Both ends now require an `/etc/passwd`.** A machine that cannot name its
     own caller refuses to send; one that cannot resolve the name refuses to
     serve. Fail-closed and consistent with the key being required, but it does
     mean a disk without an account database cannot join a cluster.

   **Considered and not taken: a per-user `~/.shadow`.** Recorded here because per-user credential records are exactly what the
   per-user-key tier above will reach for, and the reasoning below is the thing to re-read when it does.

   The question: `/etc/shadow` is mode 0600 root, so a user cannot write their
   own password — which is the entire reason `accountd` exists. What if each
   user's secret lived in `~/.shadow` instead, owned by them at 0600? Then a
   user can write it with no privilege at all, root still reads it through the
   root bypass, and the server is unnecessary.

   **It works.** `login` already learns the home directory from the
   world-readable `/etc/passwd` before it knows who you are, so it can find the
   file; `passwd` becomes an ordinary program; a task slot, an IPC protocol and
   ~270 lines disappear. This is not a bad idea, and it is worth understanding
   why it was not taken rather than assuming it was never thought of.

   **What it costs is the property that makes a credential store worth having:
   the record stops being outside the control of the principal it
   authenticates.** Three consequences, ascending:

   - **`passwd`'s policy becomes advisory.** Its empty-password rejection — and
     any future length or complexity rule — is enforced in a program the user
     need not run. They can write the file directly with `writeat`, or compute
     a hash with their own program (there is a C toolchain). With a server, the
     server is the *only* writer and policy sits at a choke point.
   - **The old-password proof becomes unenforceable**, and that check's whole
     point is lost with it. It never protected against the user — they are
     already authenticated as themselves. It protects against *someone at their
     unattended terminal*, for whom overwriting a user-writable file is a
     one-liner.
   - **Disabling, expiry and lockout become impossible.** Root disables an
     account; the account's owner edits it back. Ouroboros has none of these
     today, so the cost is entirely future — but it forecloses the category
     rather than deferring it.

   Smaller structural warts: root's home is `/`, so root's record would be
   `/.shadow`; a service account with no home has nowhere to put one; and
   `useradd` grows more fragile, since the home would have to exist and be
   chowned *before* the password commits, undoing the ordering that makes
   `/etc/passwd` the single commit point.

   **The good idea inside it is separable, and worth keeping** — see the
   `/etc/shadow.d/` follow-up below. The *split* (one record per user) is sound
   on its own; it is putting the split somewhere the user **owns** that gives
   away the guarantee. Split and ownership are independent choices, and only
   the second one is the problem.

2. ~~**`ls` of a remote mount fails against the host Python peer.**~~ —
   **fixed 2026-09-03.** The cause was `scripts/np9p_server.py` implementing no
   `NP_STAT` verb: `ls` stats a named operand before listing it, the peer
   returned `FS_ERROR` for the unknown verb, and `ls` renders every error as
   "no such file or directory". A message about a path, for a request whose
   path was fine — which is why it sat filed as a path bug.

   **Three things this entry previously got wrong, kept because each one cost
   something.**

   - **The diagnosis was wrong.** It read "the guest's resolution of the mount
     *root* — probably an empty path where the server expects `/`". The guest
     sends `/`, correctly; `ninep-abi`'s `resolve_ns` has an explicit guard for
     that case. And it was never about the root — `ls /mnt/a/SUB` failed
     identically, which the note had not tested.
   - **The mechanism was the mirror image of the one first recorded.** The
     first write-up called this a neglected half of a mirrored pair: the
     client's `stat` was repaired 2026-09-02 and nobody checked the server.
     **`git log -S` says otherwise.** The peer was created 2026-08-25
     (`a9e7342`) and `ls` did not call `fs_stat` at all until **2026-08-27**
     (`3cf79d1` added `-l`, `54a9b01` file operands). So the peer was adequate
     for its documented recipe when it was written, and
     [`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md) and
     [`CHANGELOG.md`](CHANGELOG.md) were correct then too. The real mechanism:
     **a guest client grew a verb dependency, and nothing re-ran the recipe
     that depended on it.** Which matters, because the lesson points somewhere
     different — the next `/bin` command to grow one breaks this rig the same
     way, and `chmod`'s symbolic form already calls `fs_stat`.
   - **A cheaper discriminator existed than the one used.** `ls` with **no
     operand** does not stat (it lists the cwd), so `cd /mnt/a; ls` worked
     throughout. Two commands would have isolated the fault to the operand
     path; an ad-hoc logging wrapper round the peer was built instead. The
     wrapper did name the verb, which is what the fix needed — but not before
     a cheap bisect could have narrowed where to point it.

   **The `ls /mnt/a/NOPE` control the first write-up leaned on does not
   discriminate.** It was recorded as "what proves the new arm can say no", and
   it proves nothing: an unserved verb and an absent path both reach
   `sealed(FS_ERROR)`, so the reply is byte-identical and the check passes
   against the *unfixed* peer. The control that does discriminate is
   `ls /mnt/a/SUB` — it fails before the fix and lists `NOTE.TXT` after.
   Making the absent case honestly distinguishable is the `FS_ERR_NOT_FOUND`
   follow-up below, and it would make the `NOPE` control real as a side effect.

   Scope, checked rather than assumed: `tree /mnt/a` worked against the
   unfixed peer (it takes the directory flag from the readdir trailing `/` and
   stats nothing), and so did
   `cp /mnt/a/SUB/NOTE.TXT /COPY.TXT` — but **not** for the reason first
   recorded. `cp` *does* stat, via `ulib::fs_presence`; a `grep` for `fs_stat`
   in `cp` returns zero because the call is one level of indirection away. It
   worked because that command's **destination was local**, so the stat never
   crossed the mount. Reverse the operands — `cp /F.TXT /mnt/a/NEW.TXT` — and
   it stats the remote path directly, as `mv` does for a remote source.

   Follow-ups this opened, each its own change:

   - ~~**`ls` renders every `fsd` error as "no such file or directory".**~~ —
     **fixed 2026-09-03.** `ls` was the only command under
     `programs/fileutils/` that never called `ulib::fs_error`, so over an ext2
     mount a file you may not read reported as a file that does not exist —
     and `FS_ERR_AUTH` and a transient `NO_FS` said the same. `fs_error_msg`
     now exposes the message table so `ls` keeps its `ls: <operand>: <msg>`
     prefix without a second copy of it. Verified with a negative control (only
     that change reverted: the denied directory and the missing one print the
     same string). Fixing it turned up a **second copy** of the table in the
     shell's `print_fs_error`, already drifted both ways — missing `NO_FS` and
     `FS_ERR_READ_ONLY`, and an `FS_ERR_INVALID_NAME` message still naming an
     8.3 restriction that FAT32 long-filename *write* support removed on
     2026-08-27 (checked: `touch /AVERYLONGFILENAME.TXT` succeeds). Both fixed;
     **unifying the two tables is still open** — the shell keeps its own fs
     layer and cannot share `ulib`'s.
   - ~~**The peer answers an absent path with `FS_ERROR`, where `fsd` answers
     `FS_ERR_NOT_FOUND`.**~~ — **fixed 2026-09-03.** All four absent-path arms
     now answer `FS_ERR_NOT_FOUND`, and a verb the peer knows but refuses on
     policy answers `FS_ERR_READ_ONLY` rather than sharing one value with "no
     idea". Measured on the SLIRP rig:

     | | before | after |
     |---|---|---|
     | `ls /mnt/a/NOPE` | `failed` | `no such file or directory` |
     | `cp /F.TXT /mnt/a/NEW.TXT` | `cannot tell whether … exists` | `read-only filesystem` |

     The status codes are now covered by `scripts/check-wire-constants.py`,
     which grew to read **syscall-abi as well as ninep-abi** and to parse the
     `u64::MAX - N` idiom both peers hand-transcribe as `(1 << 64) - 1 - N`
     (12 → 25 constants). `FS_ERR_NOT_FOUND` is the load-bearing one: it is
     branched on, not displayed.

   - **A cross-mount `mv` silently renamed the file locally and reported
     success** — found by this work, fixed with it, and the more serious half.
     `ulib::fs_mv` resolved both paths but dispatched on the **source's**
     target, handing that server the destination's *string*, which it then
     read as its own. So `mv /F.TXT /mnt/a/NEW.TXT` produced a local
     `/NEW.TXT` and exit 0 — the file was not where it was asked to go, and
     nothing said so. No `NP_MV` ever reached the peer.

     The code carried a comment saying a cross-tree move *"can't arise yet (a
     later phase concern)"*. True when every binding was tree 0; false since
     remote mounts, `/proc` and multi-mount landed. **The later phase arrived
     and nobody came back** — and the assumption was recorded as a comment
     rather than a check, so nothing failed when it expired.

     Now refused with a reserved `FS_ERR_CROSS_DEVICE` (POSIX's `EXDEV`),
     compared across **all three** fields of the resolution — a local `/net`
     and a remote mount both resolve to `NET_TASK`/tree 0 and differ only in
     the endpoint. `-f` does **not** bypass it, since the guard is in `fs_mv`
     rather than in `mv`'s presence check, and `-f` was exactly the arm that
     reached the silent rename before. A same-tree `mv` is unchanged. Doing
     the copy-then-delete that Unix `mv` does across filesystems is left open;
     refusing is the honest floor, and `cp` already works across a mount.

   - ~~**`STAT_FLAG_DIR` is pinned by nothing.**~~ — **fixed 2026-09-03** by
     widening the patterns rather than continuing to describe the gap: the
     parser's reach had been treated as a property of the constants. Rust
     `u32` and both languages' `1 << n` shift form are now parsed, so the dir
     bit is compared (25 → 27 constants, `FS_ERROR` picked up alongside it by
     asking which names a peer and Rust *both* spell that the list did not
     mention).

     The damage it now guards was **forged and observed**, not asserted: with
     the peer writing the bit at `1 << 1`, `ls -l /mnt/a` classifies the mount
     **root** as a file and prints one zero-byte entry — `HELLO.TXT` and
     `SUB/` never appear, exit code 0, no error anywhere. Worse than the
     "directories list as files" this was predicted to cause: the listing
     silently becomes a one-line file listing.
   - **The fid verbs reach no export at all.** `NP_OPEN`/`NP_PREAD`/
     `NP_PWRITE`/`NP_FSTAT`/`NP_CLUNK` appear in neither Python peer *and* in
     no arm of `netd`'s export, which falls through to `FS_ERROR` — so a C
     program's `open`/`fstat` over a remote mount fails on a real guest-to-guest
     mount too. `ls -l` works remotely now; `fstat` of the same file does not.

     **IN PROGRESS — [`roadmap-fid-verbs.md`](roadmap-fid-verbs.md) is the
     plan: seven ordered steps, each with a check and a negative control.
     STEPS 1–3 ARE DONE AND THE REPORTED SYMPTOM IS CLOSED** — a C program
     opens and reads a file on a remote mount.

     **This heading names the wrong subsystem**, which is what the scoping
     found. `libc/src/file.c` sent every fid verb to `FSD_TASK` with no
     namespace resolution anywhere, so a C `open("/mnt/a/F")` asked `fsd`
     about a path only `netd` knows and never left the machine. Teaching the
     export the five verbs is real work — it is now steps 5–7 — but it would
     not have moved the reported symptom by one byte.

     Three decisions are recorded there, all confirmed: namespace resolution
     for C's fd path lives in a **Rust staticlib shim** (the C arc's first Rust
     link, which needed `--gc-sections` to link at all, since prebuilt `core`
     carries `ABS64`); **`netd` owns remote fids**; and the export connection
     becomes a **session**, held for a fid's lifetime — because step 4 found
     there is **no connection to key a fid table on**, every remote request
     opening its own TCP connection with a fresh source port. The smallest
     option (translate fid ops to the path verbs the export already serves) was
     rejected for foreclosing unlink-then-read, locking and `O_APPEND`
     permanently, and for making every multi-op read a TOCTOU on the path: a
     fid names the *file*, a path names a *name*.

     **Step 4's gate has since RUN, and it failed usefully**: a persistent
     connection is viable but not unconditionally, because both clients used
     the export's FIN as the end-of-reply marker — so switching the export
     would have been a flag day. The prototype was reverted rather than parked
     behind a flag. **Prerequisite 1 (length-aware clients) is DONE** for the
     framed path (`#105`); the remaining blocker is prerequisite 2, the wire
     signal by which a client opts into a session — and it must also answer
     what happens to `cpu`, whose `NP_RUN` reply carries no length prefix at
     all, so EOF is genuinely its terminator there. Detail, with the checks
     and controls, in [`roadmap-fid-verbs.md`](roadmap-fid-verbs.md).

   - ~~**Re-confirmed 2026-09-05, and it is the WEDGE-TIMER failure already
     analysed below, not a new one.**~~ — **THAT ATTRIBUTION WAS WRONG, and
     the real bug is FOUND AND FIXED 2026-09-06.** It was never the wedge
     timer. The 09-03 heartbeat fix works exactly as documented: a remote
     `cat` of `BIG.TXT` is **28 round trips over ~17 s** — nearly seven times
     `WEDGE_TICKS` — and it now completes with **zero** `slot 4 wedged`
     messages and zero faults. Timing was never the discriminator.

     **The discriminator was the PIPE.** Measured on `main`, same boot, same
     peer:

     | command | before | after |
     | --- | --- | --- |
     | `cat /mnt/a/BIG.TXT` (28 RTs, ~17 s) | **works** | works |
     | `cat /mnt/a/HELLO.TXT \| wc` (5 RTs, ~3 s) | `cat: failed` + `0 0 0` | `40 400 1960` |
     | `cat /mnt/a/BIG.TXT \| wc` | `cat: failed` + `0 0 0` | `200 2600 13600` |
     | `ping 10.0.2.2` | works | works |
     | `ping 10.0.2.2 \| wc` | `ping: request failed` + `0 0 0` | `1 3 21` |

     Both halves of each `before` cell were always printed together: `cat`'s
     error path calls `end_of_stream` before exiting, so the consumer sees a
     clean empty stream and reports `0 0 0`. The 09-05 entry recorded only the
     `0 0 0`; recording only the `cat: failed` would drop the same signature
     from the other end.

     The longer read succeeded and the shorter piped one failed, which no
     threshold can explain. **The host peer logged not one request** for a
     failing run — the same "zero packets left the guest" signature as cause B
     below, and the same symptom text, because it is the same denial: of the
     shell's five spawn sites, only `run_found_command`'s two delegated
     `TO_NET`. `run_head_pipeline` and **both** `cmd_exec` arms did not — so
     *every network-using program was broken inside a pipeline*, and under
     `exec`, not just remote mounts. The grant now lives in `spawn_path`, the
     one function all five go through, so a sixth site cannot omit it.

     **And it could not be fixed in the shell alone**, which is why it lasted:
     `tasks.rs::DELEGATED_SEND` held **one** delegated target per task, so a
     stage could hold the pipe delegation *or* `TO_NET`, and the second grant
     silently revoked the first. It is now a **set** (a bitmask in the same
     `1 << slot` shape `caps_for_slot` already uses). Not a widening — every
     bit still needs a delegator that statically holds it; what is gone is the
     accidental revocation.

     **The lesson, and it is about the record rather than the code.** The
     09-05 entry did the thing this file elsewhere recommends — it checked a
     "new" bug against a documented, measured analysis instead of guessing —
     and it landed on the wrong cause anyway, because it matched on the
     *symptom* (a remote read fails) and never re-checked the old analysis's
     **signature**: `WEDGE_TICKS` announces itself with `server slot 4 wedged`
     on the console, and that line was absent from every failing run. Reusing
     a measured analysis is only safe if its signature is re-observed; matching
     a symptom to a stored cause is a guess wearing a citation. The 09-05 entry
     even recorded `cat /mnt/a/BIG.TXT` as failing, when the unpiped form
     works — the counter-example was in hand and read as confirmation.

   The docs needed no correction: `CLAUDE.md` and
   [`testing-qemu.md`](testing-qemu.md) both show `ls /mnt/a` in that recipe,
   and it now does what they say.

3. ~~**The remote-read flake, on both transports.**~~ — **BOTH CAUSES FOUND
   AND FIXED 2026-09-03.** It was two faults filed as one for weeks, and
   neither was TCP.

   **Cause A: the supervisor was restarting `netd` mid-read** (the dominant
   one). Fixed by letting a supervised server report progress unprompted — see
   the trace below.

   **Cause B: a capability-delegation race, and the request never left the
   guest.** The shell `SPAWN`s a program and only *then* `DELEGATE`s it
   `TO_NET` — it cannot delegate to a slot that does not exist yet — so a
   child reaching `netd` inside that window gets `MSG_ERR_DENIED`.
   `ulib::net_call` had absorbed that for years, with a comment naming it "the
   brief delegation-not-yet-applied window". **`np_netlocal` and `np_remote`
   did not**, and `MSG_ERR_DENIED` sits above `FS_ERR_MIN`, so it fell into
   the generic arm and surfaced as `FS_ERROR` — which `cat` prints as
   "cat: failed", giving no hint that the cause was a capability not yet
   granted. One sibling had the guard and another did not; the retry now lives
   in a single `net_msg_call` all three share, so a fourth caller cannot
   reintroduce it.

   **Proved by forcing the race rather than by counting boots.** Six boots
   after Cause A was fixed gave one failure — the historic 1-in-6 — and its
   capture contained **zero ARP and zero TCP**, which is what pointed inside
   the guest. But six samples at 1-in-6 have a 33% chance of showing nothing,
   so the fix was demonstrated deterministically instead: a temporary 4-tick
   delay inserted between `SPAWN` and `DELEGATE` (test scaffold, not
   committed) makes every spawn race.

   | build | result | peer requests |
   | --- | --- | --- |
   | wide window, **no** fix | **3 of 3 failed** | **0** |
   | wide window, with fix | 3 of 3 ok | 15 |
   | clean, with fix | 4 of 4 ok (+6 of 6 earlier) | — |

   Zero requests reaching a peer that was verified alive is the same signature
   as the original failing capture, which is what ties the forced case to the
   real one.

   **Cause B had a THIRD sibling, found 2026-09-06 — see the corrected
   09-06 bullet above.** The retry `net_msg_call` added absorbs a denial that
   is *transient by construction*, and its doc says so. It cannot help where
   the grant never arrives at all, and there the very same `MSG_ERR_DENIED` →
   `FS_ERROR` → "cat: failed" chain plays out permanently: the shell delegated
   `TO_NET` on its non-pipeline spawn path only. Every measurement in the
   table above ran an unpiped command, so the surviving half of the bug was
   invisible to the check that proved the fix. A denial-absorbing retry is not
   the same thing as a grant, and testing one spawn path does not test the
   others.

   The original entry, and the trace that split the two faults, follow.

   ~~Previously: dominant cause fixed, residual open.~~ The packet trace below found it was never TCP: the
   supervisor was restarting `netd` mid-read. That is fixed (a supervised
   server may now report progress unprompted, so it is not killed for being
   busy), and the workload that failed **11 of 42** now fails **2 of 42** with
   zero restarts.

   **What remains open is a genuinely different fault**, and the two were
   filed as one item for weeks: the residual reports `cat: failed` — a generic
   error — not the `NO_FS` of an absent server, it survives with the
   supervisor quiet, and it is the older *"intermittent first-ls on two-VM"*
   the Phase 2 notes recorded. About **3 in 46** across two runs after the fix
   (small sample, stated as one) against ~1 in 4 before.

   **It is POSITIONAL, not a flat rate — measured 2026-09-03 after the fix
   landed, and this is the useful part for whoever chases it.** Six identical
   `cat /mnt/a/HELLO.TXT` in one boot immediately after `mount -r`: the
   **first fails, the next five succeed**, with zero wedge lines. A separate
   run of two cats as the first two ops failed **both**. So a rate quoted from
   a mixed workload ("2 of 42") is real but misleading as a search target — it
   invites hunting a random ~5% fault, when the signal is concentrated in the
   **first remote op(s) after a mount**. Which is exactly what the Phase 2
   notes named it: *intermittent FIRST-ls*. Start there, with one boot and one
   op, rather than a long mixed run.

   The trace harness (`scripts/trace-remote-flake.py`) applies to it
   unchanged, and the two-node rig is where it was first seen. The original entry, and the trace
   that split it, follow.

   Originally: roughly one remote op in six
   fails, reported to the caller as a generic failure (`cat: failed`). Originally
   measured on the two-VM socket link; observed again 2026-08-31 on the
   **SLIRP** path of `run-image-9p-client`, one run in two, so it is not specific
   to the socket netdev — which makes a QEMU-link explanation less likely and a
   guest-side one more so. Measured
   2026-08-31 across scripted runs — **2 of 6 ops on `main`, 1 of 6 on a branch**
   — so it is not new, and it is the same intermittent the Phase 2 notes called
   "intermittent first-ls on two-VM", which the 4-try SYN retransmit reduced but
   did not remove. Suspects, in order: the SYN retransmit budget still being too
   small for a cold link; source-port/ISN reuse landing in the peer's `TIME_WAIT`
   (fixed once for back-to-back connections, but every op opens a new connection);
   and no retransmit at all on the *request* segment after the handshake. It
   matters more than a flake usually would, because it is the rig the cluster's
   permission tests run on — see the message table in
   [`testing-qemu.md`](testing-qemu.md) for telling it apart from a real refusal.
   The fix wants a packet trace first, not a guess.

   **TRACED 2026-09-03, and it is not TCP.** The packet capture this entry
   asked for was finally taken (SLIRP rig, `run-image-9p-client` shape, against
   `np9p_server.py`, 42 mixed ops with `-object filter-dump` attached). **Every
   one of the 58 TCP connections in the capture is healthy** — SYN, SYN-ACK,
   request, reply, FIN — with a single SYN retransmit across the whole run and
   no RST, no unanswered SYN, no missing reply. So all three suspects above are
   **disproven for this rig**: the failures are not a TCP problem, and 42 ops
   produced only 58 of the ~112 connections they should have, because the guest
   **stopped issuing requests** rather than losing them.

   The cause is in the guest's own console output, which nothing had been
   reading:

   ```
   Ouroboros kernel: server slot 4 wedged - no progress (runnable) - restarting
   Ouroboros kernel: server slot 4 restarted (attempt 1/3)
   ```

   Slot 4 is `netd`. **The supervisor's wedge detector restarts the network
   server mid-read**, and the in-flight command dies with it —
   `supervisor.rs`'s `WEDGE_TICKS = 128` at a 20 ms tick is **2.56 s**
   continuously `Runnable`, and `netd` is `Runnable` (not `Blocked`) for the
   whole of a multi-chunk remote read. Measured round trip against this peer is
   **0.602 s** (its Ed25519 signing is Python), so:

   | op | round trips | time | vs 2.56 s | observed |
   | --- | --- | --- | --- | --- |
   | `cat HELLO.TXT` (1960 B) | 5 | **3.01 s** | **over** | failed **7 of 7** |
   | `ls -l /mnt/a` | 4 | 2.41 s | under by 0.15 s | 0 of 7 |
   | `cat NOTE.TXT` (29 B) | 2 | 1.20 s | under | 3 of 7 |
   | `ls /mnt/a` | 2 | 1.20 s | under | 1 of 7 |

   The only op over the threshold is the only one that failed every time.
   `netd` then exhausts `MAX_RESTARTS = 3`, after which **every** remote op
   fails — which is what the scattered late failures of the short ops are, not
   a per-op probability at all. That also explains why the rate looked like
   "roughly one in six": it is not a rate, it is one deterministic failure plus
   the collateral of a dead server.

   This is the class `network-stack-postmortem.md` already recorded once — the
   supervisor restarting `netd` mid-transfer when a burst ran too long — back in
   a new path.

   **What this does NOT explain, stated because the entry conflated two
   observations.** This is the *SLIRP + Python-peer* rig, where a round trip
   costs 0.6 s because the peer signs in Python. On the **two-node** rig both
   ends sign in Rust (~2 ms), so a five-chunk read is ~10 ms and cannot approach
   2.56 s. The "intermittent first-ls on two-VM" recorded earlier is therefore
   **probably a different fault**, still open, and the two should not have been
   filed as one item. The harness that found this (attach `filter-dump`, run a
   mixed cycle, classify every connection) applies there unchanged.

   **A second, smaller finding — FIXED 2026-09-03 — and it is what made the
   first one hard to see:** `ls` called `ulib::exit(0)` on **every** path — it has exactly one
   `exit` in the file — so it reports a missing file, a permission denial or an
   unreachable cluster peer with **exit status 0**. `cat` exits 1 correctly. A
   test harness written for this investigation scored a whole run of failures as
   passes because of it, and no script can detect an `ls` failure today.

   **One process note.** The first "reproduction" was an artifact: the host peer
   had been killed by a tool timeout, so every SYN got an RST, `cat` exited 1,
   `ls` printed an error and exited 0, and the harness reported a beautifully
   regular "only `cat` fails" pattern that was entirely the dead instrument. It
   was caught by the capture showing 42 connections all RST. The harness now
   refuses to report results unless the peer's request count went **up** during
   the run — [[reference-a-check-that-cannot-fail]], applied to the rig rather
   than the code.

   **Earlier data point, 2026-09-03** (SLIRP rig, `run-image-9p-client` shape,
   against the Python peer): `cat /mnt/a/SUB/NOTE.TXT` failed **once in five**
   observations — `cat: failed`, exit 1 — then succeeded 4/4 on immediate
   re-runs with no code change in between, each showing the expected two
   chunked `NP_READ`s arriving at the peer. Small sample, stated as one:
   consistent with the rate already recorded, and useful mainly as a *negative*
   result — it appeared during a run verifying an unrelated `NP_STAT` fix, and
   the re-runs are what established it was not that change. Worth knowing when
   the trace is finally taken: the failing op was a **chunked** read (offset
   29, the second segment), not the first request of a connection.

4. **General / transitive capability delegation.** The delegation shipped
   2026-08-21 is deliberately coarse: **non-transitive, irrevocable short of
   task death, and in practice shell-only.** (It was also *one target per
   task* until 2026-09-06, when that turned out to be a bug rather than a
   scope cut — see the 09-06 entry under item 2. It is a per-task set now,
   which is what `a | b | c` with a network stage needed; the rest of this
   item is untouched by that.) Making it general (any task hands
   any held capability onward, revocably — MINIX's full grant model) would
   unlock true relay-free `a | b | c` and a spawned program running its
   *own* server. The catch: **neither consumer exists yet**, so building
   this first would repeat the "premature, a mechanism without a hard
   consumer" trap the capability-and-hardening postmortem flagged for
   delegation itself. Build the consumer first, or wait until one is
   actually wanted.

5. **Per-task ASIDs, revisited** — a pure TLB-flush-per-switch optimization
   that passed on QEMU but faulted the idle task on real Parallels and was
   reverted (see the isolation postmortem for the decoded fault evidence);
   needs a proven break-before-make sequence. Low value — a context switch
   already does far heavier work than the per-switch flush it would save.

The stack **guard page** (16KB guarded stack, which immediately caught a
real silent 8KB overflow in the shell's own `exec` path) and the 256KB raw
**userland heap** (`heap_info` — a real `alloc`-backed heap stays blocked on
stable: prebuilt lib`alloc` has `R_AARCH64_ABS64` relocations a `-pie` link
rejects, and `-Z build-std` is nightly-only), formerly tracked here, both
shipped 2026-08-20. See `CHANGELOG.md`.

**Deferred / blocked** (recorded, not chased): moving a *third* driver
out is limited by the no-IOMMU DMA constraint (the block transport can't
safely leave the kernel); reverse-engineering Parallels' proprietary
serial/storage device (vendor `0x1ab8`, no public spec); and an EHCI
driver for USB 2.0 sticks (a whole second host-controller bring-up for
poor value).

## Completed arcs (moved out)

These arcs are **done**; their full plan-shaped write-ups moved to
[`roadmap-completed.md`](roadmap-completed.md), and the condensed milestone
record is in [`CHANGELOG.md`](CHANGELOG.md):

- **The microkernel arc** — `fsd`/`cond`/`netd` as supervised MMU-isolated
  servers, EL0 fault isolation + supervision + heartbeat, the capability
  model + runtime delegation, per-task page tables, grant/safecopy IPC.
- **The network stack** — virtio-net driver + `netd` (ARP/IPv4/ICMP/UDP/DNS
  and a full TCP with flow control, RTO, congestion control, SACK), an HTTP
  static-file server, `ping`/`resolve`/`fetch`.
- **More filesystems** — GPT/MBR discovery, the VFS refactor, FAT32 + exFAT +
  ext2 read *and* write, plus the `stat` op.
- **Disk management** — `mount`-info/`unmount`, `erase`/`partition`, and
  `format` (mkfs) for all three filesystems.
- **Standalone binaries** — `/bin`, PATH, argv/cwd ABI, `ulib`, and the whole
  fs+net command surface externalized; then a *minimal* shell (only genuinely
  shell-coupled commands stay builtin).
- **Multi-stage pipelines** — N-stage `a | b | c` of standalone filters.
- **Shell interactive features** — output redirection, filename wildcards,
  tab completion, `-?` usage help, `man` pages, and the keyboard-ownership arc
  that lets interactive programs be `/bin` binaries.
- **Users, permissions & account management** (2026-08-28 → 2026-08-30) — a
  kernel-owned identity per task, a login gate, `fsd` permission enforcement
  with ancestor-`x` traversal, `/etc/shadow`, supplementary groups, the
  on-device account tools over a shared pure `accounts` crate, and finally
  `accountd` — a fourth server (protected slot 5) so a user can change their *own*
  password — with the message credential bound at **send** underneath it.
  **One item remains and it is the next arc, promoted to the frontier below:
  per-user cluster identity.**

## Remaining follow-ups from completed arcs (small, unsequenced)

The small open tails those arcs deliberately left:

- **ext4.** Much larger (extents, journaling, htree, checksums, 64-bit) and
  the no-alloc fixed-buffer constraint makes a big FS genuinely harder — a
  separate large arc, not a near-term ext2 follow-on.
- **A `/dev` namespace.** Only if multi-disk/partition addressing arrives (the
  Plan 9 devfs direction); nothing to name yet with one block device.
- **`/etc/shadow.d/<name>` — one credential record per user, in a *root-owned*
  directory** (dir 0755 root, files 0600 root). The salvageable half of the
  `~/.shadow` idea above: it keeps the per-user split and drops the per-user
  ownership, so `accountd` remains the only writer and every policy check stays
  at its choke point. Three concrete wins, none of them speculative:
  - **It bounds the read by construction.** A whole-file read of `/etc/shadow`
    reporting `0` on overflow is what locked out every account *including root*
    at ~23 entries (see the ledger below). That was fixed by streaming one line;
    a per-user file makes the bug unrepresentable instead of handled.
  - **It removes the whole-file rewrite**, and with it the reason
    `accounts::changed_span` and the write-only-the-differing-bytes path had to
    exist — those were written because truncating the shared file would lock
    everyone out mid-update.
  - **It is probably the shape per-user cluster identity wants**, since a
    credential that must be named per user across machines is already a
    per-user record.

  Not urgent: the streaming read and the non-destructive write already close the
  failure modes it would prevent. It is a simplification with a security
  argument, not a fix.

## Testing infrastructure: scripted real-hardware round trips

> **Direction update (2026-08-26): Parallels real-hardware testing is PARKED.**
> QEMU (single machine *and* the two-node cluster on a shared socket link — see
> [`testing-qemu.md`](testing-qemu.md)) is the working dev/test loop and is
> **good enough for now**. Parallels was never going to prove the cluster anyway
> — it has no working NIC transport (virtio-PCI, unsupported), so networking and
> the whole Plan 9 cluster are unreachable there (see
> [`testing-parallels.md`](testing-parallels.md) for the full analysis, kept as a
> "perhaps later" reference, not an active plan). **The intended physical target
> is now 2× Raspberry Pi 4** (real ARM hardware, ordered 2026-08-26): the Plan 9
> resource-sharing mechanics are a better fit for genuine physical machines than a
> VM, so a real two-node cluster on the Pis is the eventual real-hardware proof.
> A concrete Pi test plan is now written -- [`testing-pi4.md`](testing-pi4.md), 2026-08-28, ahead of the boards, with every claim labelled (predicted) or (confirmed) so the first bench session turns it into a log. Note its headline finding: **the Pi's GENET NIC is not virtio either**, so 2x Pi 4 does not by itself deliver the two-node cluster proof -- that needs USB-Ethernet over the existing xHCI stack, or a GENET driver, first. The
> `prlctl`/`make test-parallels` tooling below stays available but is no longer a
> priority.
>
> **Pi-4 bring-up reference (pre-read, for when the boards arrive):**
> `docs/research-redox-and-pi.md` (Part 2) maps the
> `rust-raspberrypi-OS-tutorials` repo onto our situation. The key call: **try
> the [pftf/RPi4](https://github.com/pftf/RPi4) EDK2 UEFI+ACPI firmware first** —
> a Pi 4 under it exposes UEFI + ACPI + a GOP framebuffer, so our existing boot
> path (UEFI loader, ACPI MADT → `gicv2.rs` for the Pi 4's GIC-400/GICv2, GOP
> `fbconsole`) should carry over largely unchanged, rather than rewriting for raw
> `kernel8.img` boot. The tutorials stay the fallback reference for the raw
> BCM2711 facts (peripheral base `0xFE00_0000`, GIC-400 at GICD `0xFF84_1000`/
> GICC `0xFF84_2000`, PL011-not-mini-UART, GPIO14/15 = ALT0, the serial rig:
> USB-serial to TX/RX/GND, **not** VCC). See [[project-physical-hardware-target]].

Every real-hardware bug in `xhci-keyboard-postmortem.md` and
`boot-bringup-postmortem.md` cost a manual round trip: rebuild, re-image,
boot Parallels, watch the screen, type on a physical keyboard, report
back. `make test-parallels` (`scripts/test-parallels.sh`) closes that gap
using `prlctl`, Parallels Desktop's own CLI (`man prlctl`) — discovered
2026-08-16, not something this project had used before. It rebuilds
`esp.hdd`, boots the registered VM headlessly, types a `;`-separated list
of shell commands via `prlctl send-key-event` (real decimal PS/2 Set-1
scancodes — `prlctl` rejects hex), and saves a screenshot
(`prlctl capture`) after each one, all with no human watching the VM
live. Confirmed working end to end: `help`/`echo hi`/`uptime` all
produced correct, readable output in the captured screenshots, including
the `xhci::report` debug lines showing genuine HID reports reaching the
same interrupt-endpoint code path the physical-keyboard postmortem is
about (`send-key-event` drives Parallels' own synthetic keyboard device,
not that specific physical one — a real distinction, though the code
path it exercises is the same one).

This doesn't replace real-physical-hardware confirmation for anything
USB-passthrough-specific (the xHCI postmortem's bugs 1-5 needed the real
device), but for everything else — does a shell command still work after
a change, did a fix regress the boot sequence — this turns what used to
be a human-paced manual check into something that can run unattended and
be reviewed after the fact from the saved screenshots.

## POSIX / C-program portability: a userland libc personality (STARTED 2026-08-28)

> **Progress: the foundation is proven.** A C program now runs on Ouroboros
> (`libc/hello.c`, `make chello-bin`): clang → `aarch64-unknown-none` ELF →
> Rust's LLD against `programs/linker.ld` → the existing loader → the syscall
> boundary, spawned like any `/bin` program (`# chello` → `hello from C on
> Ouroboros`). No loader or kernel change was needed. That closes the one real
> uncertainty — the toolchain path — so the rest is *growing a libc*, not
> inventing the mechanism. **`.data`/`.bss` support landed next** (the second
> step): userland programs may now have mutable statics/globals — the loader
> already loaded initialized data and zeroed `.bss` per PT_LOAD segment, so this
> was removing the linker-script ASSERTs and verifying (fresh-per-spawn,
> `data=7 bss=0` → `data=8 bss=5`, RELATIVE relocs only). That was the real
> blocker for non-trivial C. **A minimal libc landed next** (third step):
> `libc/` now has standard headers + sources (`crt0`, syscall stubs, `printf`,
> `malloc`/`free` over `sbrk`, `string.h`) — a C program `#include`s `<stdio.h>`
> and calls `printf`/`malloc` (`make cdemo-bin`, `/bin/CDEMO`: formatted output,
> heap allocation, `sum(1..100)=5050`). **File I/O + pipe-aware output landed
> next** (fourth step): `open`/`read`/`write`/`close`/`lseek`/`fstat` over `fsd`
> with an fd table, and a stdout-target-aware `write` so a C program works in a
> pipeline (`make cfile-bin`, `/bin/CFILE`: writes a file, reads it back;
> `cfile | grep hello` filters its output). **Fids landed next** (fifth step): fsd
> gained real server-side open-file handles (`NP_OPEN`/`NP_PREAD`/`NP_PWRITE`/
> `NP_FSTAT`/`NP_CLUNK`, a per-client fid table, permission checked once at open),
> the C libc uses them, and they coexist with the path verbs — the
> deferred-since-Phase-0 "a POSIX fd ≈ a 9P fid" feature, paying off for both C
> portability and the 9P model. **picolibc landed next** (sixth step, the real C
> library): `picolibc` 1.8.9 is built `-fPIC` (so it self-relocates under our
> loader — `R_AARCH64_RELATIVE` only, zero `ABS64`) and linked against OUR
> porting layer — the same `crt0`/syscall stubs (`write`/`read`/`open`/`sbrk`/
> `_exit`), which is exactly what picolibc's `posix-console` stdio bottoms out
> at, plus two 128-bit-shift builtins its float printf needs (`libc/pico/
> builtins.c`). `make cpico-bin`, `/bin/CPICO`: **full `%f`/`%e`/`%g` float
> formatting** (ryu), `snprintf`, `qsort`, `malloc`, `strtol` — unmodified
> standard C the hand-rolled libc couldn't run. The prebuilt static lib + headers
> are committed under `third_party/picolibc-prebuilt` (regenerate with
> `scripts/build-picolibc.sh`), so `make` needs no meson/ninja. **The arc's one
> open follow-up — picolibc's unbuffered console stdout — closed 2026-08-29**:
> stdout is line-buffered at the `write` boundary (in `file.c`, so it serves
> whichever C library is linked), stderr and a read-from-stdin stay unbuffered,
> and exit flushes from `_exit` — which also fixed a real hang, since a picolibc
> program links picolibc's `exit()`, not our `stdlib.c`'s, so it had never been
> sending a pipe consumer its end-of-stream marker (`cpico | wc` hung). See
> `CHANGELOG.md`. **Remaining:**
> port a real application on top (SQLite, a small C compiler) — now "port one
> more program," not "invent the mechanism." See `docs/processes.md`'s "Writing a
> program in C." The reasoning below is the original parked plan, still accurate.

**The goal, restated honestly.** The original `notes.txt` intent was
"POSIX-ish system calls." What actually got built is *not* POSIX and not
Linux — it's a message-passing microkernel ABI (see
`docs/architecture.md`'s "Philosophy — not POSIX, not Linux" subsection):
a tiny syscall trap surface plus a set of userland servers reached by IPC,
and — via the cluster arc — the same verbs over TCP. That divergence was
*forced* by the microkernel/isolation work (a filesystem the kernel
depends on is a split, not a driver) and then *rationalized* by the Plan 9
direction (one uniform file protocol, per-task namespaces). **The decision
here is to keep that design, not to force POSIX back into the kernel** —
and to recover C-program portability the way real microkernels do: as a
**userland POSIX personality**, not a kernel ABI.

**The key realization: POSIX is a libc, not a kernel.** Existing C
programs call `libc` (`open`/`read`/`printf`/`malloc`), never raw
syscalls. So the port target is the *bottom edge of a libc*, whose stubs
translate into this project's existing server messages — `read(fd)` →
`FSOP_READ`/`NP_READ` to `fsd`, `write(1,…)` → `cond`, `socket`/`connect`
→ `netd`. The kernel and servers stay exactly as they are. This is a
solved shape, not a contradiction: **Fuchsia** (Zircon microkernel, *zero*
POSIX syscalls, pure message-passing channels) runs POSIX C programs via a
userland compat layer (musl + `fdio`); MINIX3 and Plan 9's APE do the
same. A message-passing microkernel running unmodified C programs is
normal.

**Shape of the work:**

- **~~Port a small libc~~ — DONE (picolibc, 2026-08-28).** `picolibc` is
  ported and running (`/bin/CPICO`: `%f`/`%e`/`%g` float printf, `snprintf`,
  `qsort`, `malloc`, `strtol`), built `-fPIC` so it self-relocates under the
  existing loader with zero `ABS64`, linked against the same syscall stubs the
  hand-rolled libc used (`write`/`read`/`open`/`sbrk`/`_exit` — picolibc's
  `posix-console` stdio bottoms out at exactly those). No kernel/loader change.
  The full six-step arc (first C program → `.data`/`.bss` → minimal libc → file
  I/O + pipes → fids → picolibc) is recorded in `roadmap-completed.md` and
  `docs/libc-arc-postmortem.md`. **The mechanism is done; the remaining bullets
  below are the still-forward parts.**

- **The architectural mismatches** (not just missing functions — think
  about these before they can bite):
  - **`fork()` — the big one.** There is no `fork`, only `spawn` (a new
    task alongside the caller, no address-space copy). The honest answer
    (Fuchsia's answer): implement **`posix_spawn` natively** — it maps
    almost directly onto `SPAWN`/`SPAWN_STAGE`/`ARGS_STAGE` + the
    stdout-target flow — and accept that programs which `fork()` and keep
    running in *both* halves (not fork-then-exec) need porting. Most
    well-behaved programs are fork-then-exec, which `posix_spawn` covers.
  - **File descriptors.** POSIX wants integer fds with a stable open-file
    handle + cursor; the current protocol is **path-per-op** (each verb
    carries a path, no server-side handle — the Phase 0 fid deferral). An
    fd table mapping `fd → (server, handle, offset)` is a *userland*
    construct (libc/`fdio`), buildable entirely on top of today's servers.
  - **`select`/`poll`, signals, `mmap`.** The blocking primitives
    (`msg_recv`/`read_char`/`NET_WAIT`) are the substrate for poll; signals
    mostly get stubbed in a first port; anonymous `mmap` maps to region
    allocation, file-backed is harder.

- **~~The one connection worth remembering: a POSIX fd ≈ a Plan 9 fid~~ —
  DONE (fids, libc arc step 5).** Phase 0 *deferred* fids (verbs stayed
  path-based, which paid off over TCP in Phase 1). The libc arc cashed the
  deferral: `fsd` now has server-side open-file handles (`NP_OPEN`/`NP_PREAD`/
  `NP_PWRITE`/`NP_FSTAT`/`NP_CLUNK`, a per-client fid table, permission checked
  once at open), a fid is directly usable as a C fd, and they coexist with the
  path verbs — one feature serving the 9P model *and* POSIX portability, exactly
  as predicted.

- **The existence proof to read first: Redox OS's `relibc`.** Redox is a
  Rust microkernel with exactly this architecture (non-POSIX kernel, POSIX
  in a userland libc) and it *ships* — real C/C++ programs and Rust `std`
  both run on it via `relibc`. Two transferable tricks from it:
  `relibc` **targets both Redox and Linux** (thin syscall wrapper on Linux,
  `libredox` on Redox), so the libc is host-testable before the OS backend
  exists; and Redox pushed **`fork`/`execve` into userspace** (`redox-rt`),
  synthesizing `fork` as `clone` without `CLONE_VM` — the answer to "but C
  calls `fork()`" without putting `fork` back in the kernel. See
  `docs/research-redox-and-pi.md`.

**Status (2026-08-28): the mechanism is built, the arc's remainder is
forward-looking.** The six-step libc arc is complete through a running picolibc
(see `roadmap-completed.md` for the sequenced record and
`docs/libc-arc-postmortem.md` for the retrospective). What remains is genuinely
different in kind — "port one more program": a real application (SQLite, a small
C compiler), plus the still-open architectural mismatches above (`posix_spawn`
native / `fork` in userspace à la Redox's `redox-rt`, `select`/`poll`/signals/
`mmap`). Those matter only once running third-party C code is an active goal.

## North-star directions ("Polaris" planning pass, 2026-08-26, not sequenced)

A batch of longer-horizon directions captured together — what would move
Ouroboros from "a microkernel that boots, runs a shell, and clusters" toward
a system you could actually *live in*: a richer terminal, richer commands
with real argument handling, more of the standard command set, a security /
identity model, on-device compilation, and an honest map of what mainstream
Unixes still have that this doesn't. **None are designed or sequenced yet;**
each is recorded so the reasoning and the starting points aren't lost.
Several build directly on things that already exist (cond's small ANSI
parser, ext2's on-disk permission bits, the per-task capability model, the
cluster-auth crypto, `ulib`, and the POSIX-libc plan above), which is the point
of writing them down now rather than from scratch later.

### 1. Terminal escape codes / VT100 (scoped-ish, the nearest of these)

cond already renders a **small ANSI parser** in the framebuffer backend
(cursor, wrap, scroll — see `CLAUDE.md`'s "Driver isolation, part 3"), so
this is *extending an existing subsystem*, not a new one. The goal is a
usefully-complete VT100/VT220-ish terminal: SGR colors + bold/underline/
reverse, cursor positioning (`ESC[H`, `ESC[<n>;<m>H`), line/screen erase
(`ESC[K`, `ESC[2J`), save/restore cursor, and scroll regions — the subset a
full-screen program (an editor, `less`, a `top`) needs to paint a screen.

**What exists to build on / the hard parts.** The rendering primitives
(`FB_BLIT`/`FB_SCROLL`/`FB_CLEAR`) are already gated to cond and already do
glyph runs + scroll, so *color* is mostly a per-glyph attribute added to the
blit path, and *positioning* is arithmetic cond already does for wrap. The
genuinely new pieces: a color-capable font blit (foreground/background per
cell), a real parser state machine (parameter accumulation, intermediate
bytes) rather than the current minimal one, and — the awkward one — an
**input** path for the responses some sequences require (cursor-position
report, device attributes), which today's one-way `NP_WRITE_FILE` output
model doesn't carry back. Reading the byte-stream UART backend (QEMU) is
straightforward; the framebuffer backend (Parallels) is the one that matters
and has no return channel yet. **Consumer question:** the first real
consumer is a full-screen program that paints and repaints a screen. Note the
**pager already shipped** (2026-08-27, `more`/`less`) *without* this — it
scrolls line-by-line and clears with the minimal `ESC[2J`/`ESC[H` cond already
has, so it isn't the consumer that forces the full escape set. The true
consumer is an **editor** (or a `top`), so build the terminal and its first
editor close together, driven by real need rather than guessed — the
keyboard-ownership arc (below/shipped) already cleared the "an interactive
`/bin` program can read keys" prerequisite both need.

### 2. Richer commands: flags, arguments, real option parsing (scoped, incremental)

Today's `/bin` commands take mostly positional arguments, and several are
deliberately minimal (the open-gaps list notes `grep` is still substring-only;
`ls -l`/`-a`, `grep -i/-v/-n`, `sort` (with `-r/-n/-u/-f`), and a `-?` usage
flag have since shipped).
The direction: give the existing commands the flags that make them actually
usable — `ls -l`/`-a`, `grep -i`/`-r`/`-n` (and eventually real patterns),
`rm -r`/`-f`, `cp -r`, `cat -n`, `head -n`/`tail`, `wc -l`/`-w`/`-c`
selection — plus a shared **option-parsing helper in `ulib`** so every
command parses `-x`/`--long`/`--` the same way instead of hand-rolling it.

**What exists to build on / the hard parts.** `ls -l` is the tell: it needs
a **richer stat surface** than the protocol exposes today — mode bits, size,
mtime, link count, uid/gid. ext2 *already stores* all of that (a guest-
written file showed up as `inode 12, 0644, 42 bytes`), so `-l` is partly a
matter of surfacing metadata the on-disk driver already reads through a
`FSOP_STAT`-shaped op — but FAT/exFAT have no Unix mode/owner, so the stat
surface has to degrade honestly per filesystem (the same "present what the
FS can model" discipline ext2 read-only already used). Recursive flags
(`-r`) want directory-tree walking in the client, which is new but small.
This is a broad-but-shallow arc — many small, independently-shippable
increments, each one command's flags — and it's the natural companion to
item 4 (the two are "make the command set real"). It also feeds item 5:
`chmod`/`chown` are exactly "a write path for the stat surface `ls -l`
reads."

### 3. More `/bin` commands (scoped, incremental)

The standard toolset still missing, roughly in cheapness order (`tail`, `nl`,
`rev`, `uniq`, and now `sort` already shipped): `tee`, `tr`, `cut`,
`find`, `du`, `df`,
`date`/`sleep` (both want a wall-clock the kernel already has via the timer
counter and `MONOTONIC_US`), `env`-as-a-program, `true`/`false`/`yes`, and a
`kill`-by-name. The **pager** (`more`/`less`) **shipped 2026-08-27** — the
keyboard-ownership arc (a foreground `/bin` program can read the keyboard)
was its enabler, so it's a `/bin` program now, not a builtin. The remaining
hard one is an **editor** — it needs item 1's cursor addressing plus item 4's
richer input, and it's the real consumer that would justify item 1's terminal
work.

**What exists to build on.** Every one of these is "a new crate under
`programs/<category>/` over `ulib`, found by PATH" — the externalization arc
is complete and the pattern is turnkey (a filter reads `pipe_recv`, writes
`write_out`; a fs command resolves against the delivered cwd and calls the
`fs_*` helpers), and the keyboard-ownership arc proved even an *interactive*
program (one that reads keys while running) can be a `/bin` binary — the pager
is the existence proof. So most of these are genuinely small. The one that
isn't: an **editor** (needs item 1's cursor addressing + item 4's richer
input). (`sort` — the one filter that can't stream — shipped by buffering the
whole input in its heap and sorting an in-place line index, with a documented
size cap.) Cheap wins first; the editor last, gated on item 1.

### 4. Login, users, security, file permissions — DONE (2026-08-28)

**Complete.** The full arc — identity, login, enforcement, and account
management — is finished; the sequenced plan-shaped record moved to
[`roadmap-completed.md`](roadmap-completed.md) and the milestone log is in
[`CHANGELOG.md`](CHANGELOG.md). Retrospectives:
[`users-and-permissions-postmortem.md`](users-and-permissions-postmortem.md)
(steps 1–3) and
[`account-management-postmortem.md`](account-management-postmortem.md) (step 4).
What shipped: a kernel-owned uid/gid per task; a `login:` gate over
`/etc/passwd`; `fsd` permission enforcement (ext2); and the account tools
(`passwd`/`useradd`/`groupadd`/`usermod`, `su`/`id` by name, `/etc/group`
primary-gid groups, `/Users` homes + `~`) on a shared host-tested `accounts`
crate, plus creator-owned new inodes.

**Still open (deferred refinements, unsequenced):**

- **Self-service `passwd`** — a non-root user changing their own password needs
  a privileged path: a dedicated **`accountd`** server (the `accounts` crate is
  built to slot into it) or a setuid mechanism. Root-only tools ship today.
  **In flight** as PR #30: the server exists, builds and boots, and `passwd`
  becomes a pure IPC client of it — held back with five code-review findings
  outstanding, including a `/etc/shadow` mode predicate and a recycled-slot
  TOCTOU (the kernel's message carries a bare slot number with no generation
  counter, so a sender that exits and is replaced between send and dequeue is
  authorised as its successor).
- ~~**A virtio-entropy RNG**~~ — **shipped 2026-08-29.** A `virtio_rng.rs` driver
  (one virtqueue, device-writable descriptor, polled) behind a `RANDOM` syscall;
  `accounts::salt_from` takes the bytes and reports whether the salt is strong,
  so `passwd`/`useradd` use real entropy where a device exists and say "no
  hardware RNG - using a weaker clock-derived salt" where it doesn't. `make esp`
  targets `run-image`/`run-image-ext2` now attach `-device virtio-rng-device`;
  the other targets deliberately don't, so the degradation path stays exercised.
  Verified by creating the same account on three boots: the two with the device
  produced different salts, the one without printed the warning.
- ~~**Supplementary group membership**~~ — **shipped 2026-08-29.** `SET_ID`'s
  `arg2`/`arg3` carry a supplementary gid list (`MAX_SUPP_GROUPS` 8) alongside
  the packed identity word, so identity and membership change in ONE call and a
  session can never keep the previous user's groups; `GET_GROUPS` reads it back,
  a child inherits it at spawn, and `fsd` grants the group triad on a primary OR
  supplementary match. `usermod -G` sets the list, `id` prints it. Setting a
  non-empty list is root-only — membership is a privilege grant, so it is gated
  separately from the identity change it travels with.
- ~~**`/etc/shadow`**~~ — **shipped 2026-08-29.** The salts and hashes moved out
  of the world-readable `/etc/passwd` (now four fields) into `/etc/shadow`, mode
  0600 root-owned, which `fsd`'s enforcement makes genuinely unreadable to a
  non-root user on ext2. Legacy 6-field lines still verify and `usermod`
  migrates them. The lookup STREAMS one line rather than reading the whole file:
  a whole-file read reports 0 on overflow, which for `/etc/passwd` safely means
  "no accounts, start a root session" but for `/etc/shadow` means "no secret"
  and locked out every account, root included, at ~23 accounts.
- ~~**Ancestor-directory `x`-traversal**~~ — **shipped 2026-08-29.** Enforcement
  walks every ancestor's search bit, not just the object and its parent.
- **Per-user cluster identity** — **the only item of this arc still open, and
  promoted to "What's next" above on 2026-08-30** once `accountd` gave the hole
  a privileged writer on the far end. **Shipped 2026-08-31**: the export now
  carries the requesting user's name inside the signature and resolves it
  through the far side's own `/etc/passwd`. What remains is the tier below —
  the export authenticates the *machine* (its keypair), so an authorized
  machine can still claim any of its own users' names; see item 1 above.
- ~~**Symbolic-mode `chmod`** (`u+x`)~~ — **shipped 2026-08-29** (`u+x`, `go-w`,
  `a=rx`, `u+rw,go+r`, copy-source `g=u`, conditional `X`, `s`/`t`; octal still
  works and stays absolute). A real `/etc/skel` for `useradd` **also shipped
  2026-08-29** (top-level files copied into a newly created home, owner + mode
  carried across; absent by default, subdirectories skipped). Its twin, **`chown` by name**
  (`chown alice:staff`, resolved via the `accounts` crate like `su`/`id`), also
  **shipped 2026-08-29** - numeric ids still work, and an all-digits field stays
  an id.

**A mechanism to borrow from Redox: the namespace *is* the sandbox.** Redox
sandboxes a process by restricting which schemes its namespace can name (down to
a "null namespace"). Ouroboros has both halves — per-task namespaces (`bind`/
`NS_SET`) and the capability send-mask — but hasn't joined them (an empty
namespace means "unchanged," not "no access"). Making the namespace the
enforcement boundary is the reconciliation the self-service/privilege work wants;
Redox is the working model (and RedoxFS's encrypted partition is the reference
for at-rest security). See `docs/research-redox-and-pi.md`.

### 5. An on-device compiler: C and/or Rust (north-star, very large)

The self-hosting dream — compile a program *on* Ouroboros rather than
cross-compiling from the Mac. Recorded honestly because the scale is very
different for the two languages, and because it's tightly coupled to the
POSIX-libc plan above.

**C is the realistic target; Rust almost certainly isn't (near-term).** A
Rust compiler self-hosting is effectively out of reach — `rustc` is enormous,
assumes a hosted std/LLVM, and this project can't even PIE-link prebuilt
`liballoc` on stable (the recurring `-Z build-std` wall). A **small C
compiler** (`tcc`, `chibicc`, `cproc`+`qbe`) is a real possibility, but *only
on top of the userland libc personality* — a C compiler is a C program: it
needs `fopen`/`malloc`/`fork`-or-`posix_spawn`/`_exit`, i.e. it's a *consumer
of item "POSIX / C-program portability" above*, not independent of it. So the
honest sequence is: libc personality first, then a C compiler is "port one
more (large, self-contained) C program." Below even that, the realistic
*first* step toward on-device code generation is much smaller — an
**assembler** (text → the ELF the loader already parses) or a tiny toy
language — which needs no libc and would prove the write-a-program-then-run-it
loop end to end. **Consumer question, stated plainly:** on-device compilation
is a *want*, not a *need* — nothing here requires it, cross-compilation works
fine — so this is a "because it's the Ouroboros thing to do" goal (the name
is a snake eating its tail; a system that can build itself is the literal
endgame), sequenced behind everything with an actual consumer.

### 6. Document what MINIX / Linux / Unix have that Ouroboros doesn't (a doc task — the organizing exercise) — DONE (2026-08-26)

**Done — see [`gap-analysis.md`](gap-analysis.md).** A factual, per-subsystem
*have / partial / don't* inventory of the current boundary (process model,
syscall surface, VFS/fds, terminal, libc, users/permissions, networking,
devices, memory, scheduling, cluster, time, the utility set, init, and
observability), each row noting what it would take and which arc it maps to,
capped by a ranked "biggest gaps" synthesis. It confirmed the sequencing hunch
above: a per-file `FSOP_STAT` surface is the keystone (it gates `ls -l`, richer
flags, *and* permissions), and a POSIX libc + fds is the second. Original
framing kept below.

Not a feature — a **gap-analysis document**, and the meta-item that helps
sequence the other five. `docs/comparison.md` already frames Ouroboros
against MINIX/Linux/Unix/Plan 9/Helix as a "what you gain, what you give up"
table; this extends that from *philosophy* to a *concrete checklist*: the
syscalls, the libc functions, the `/bin` utilities, the subsystems (signals,
job control depth, pipes-to-files, TTY line discipline, `/dev`, users/groups,
mmap, dynamic linking, a real VFS with per-FS servers, sockets-as-fds, cron/
init/service management, swap/paging) — each marked *have / partial / don't*,
with a one-line "why not / what it would take" pointing at the relevant
roadmap arc.

**Why it's worth doing early.** It's cheap (a doc, no code), it's the
natural *input* to prioritizing items 1–5 (it surfaces which gaps are one
small program vs. a multi-milestone arc), and it's the kind of honest
self-accounting this project already values — the postmortems and the
POSIX-divergence reflection are the same instinct. The risk to avoid is
turning it into an aspirational feature list; keep it a *factual* inventory
of the current boundary, the way the open-gaps list tracks
specific known gaps, just organized as a coherent map rather than a running
list. This is the one to do *first* of the six, precisely because it tells
you the order for the rest.

### Additional directions (2026-08-27 batch, not sequenced)

A second batch, captured the same way. Several **extend items 1–6 above** rather
than being new — flagged as such so the roadmap doesn't fork — and the genuinely
new ones (links, a GPU, cluster data redundancy, SQLite) get the same "what
exists to build on / hard parts / consumer question" treatment.

**a. Users, login, passwords, permissions — and per-user home directories (extends item 4).**
Item 4 already scopes the identity/permission arc: a login prompt, an
`/etc/passwd`-shaped file with hashed passwords (reusing the cluster-auth
SHA-256, now the `accounts` crate's), and ext2 mode/uid/gid actually *enforced* at the `FSOP_*`
dispatch — ext2-only, because FAT/exFAT can't store owners. The addition here is
**per-user home directories**: a `/home/<user>` the login sets as the shell's
initial cwd — a small convention layered on the permission work, not a separate
arc. Still sequenced after item 2's stat surface (nothing to check against until
then).

**b. Links: hard links + symbolic links (new, ext2-only).**
The Unix link model, which ext2 already half-supports: an inode owns the data and
a directory entry is just `name → inode`, `fsd`'s ext2 arm already keeps
`i_links_count` consistent for `mkdir`/`rmdir`, and it already *reports*
(doesn't follow) symlinks. So a **hard link** is "a second directory entry
pointing at an existing inode, `i_links_count` bumped," and a **symlink** is "an
inode whose data is a target path." The work: `ln`/`ln -s` commands,
`FSOP_LINK`/`FSOP_SYMLINK` ops, and **symlink-following in path resolution**
(with loop detection / a depth cap) — the last is the only genuinely new
mechanism, and it's shared with item 4 (a `/home` symlink) and the stat surface
(link count + type in `ls -l`). **ext2-only** (FAT/exFAT have no link concept),
the same honest per-FS degradation as permissions. Small given the ext2
foundation; pairs with items 2/4.

**c. A text editor + full-screen terminal control (extends items 1 and 3).**
Item 1 (VT100/cursor addressing in `cond`) plus the editor already noted under
items 3/4 *are* this. The specific question raised — **"graphics mode only?"** —
is worth recording an answer to: the **framebuffer** backend (Parallels, the
real target) needs `cond` to grow real cursor positioning / erase / scroll
regions (item 1's core) *and* an input return-channel for the sequences that
need one (the awkward part item 1 flags); but the **byte-stream UART** backend
(QEMU serial) already passes ANSI straight through to a host terminal, so an
editor can be *developed and tested there first* and the framebuffer terminal
caught up to it. So: not graphics-mode-only, but the framebuffer is where the
real work is. Build the terminal and its first editor together (item 1's
consumer question).

**d. On-device compilers, C and Rust (extends item 5).**
Item 5 already covers this in full: a small **C** compiler (`tcc`/`chibicc`/
`cproc`+`qbe`) is realistic *on top of the userland libc personality* (a C
compiler is a C program); **Rust** self-hosting is effectively out of reach
(`rustc`'s size + the recurring `-Z build-std` PIE wall); and an **assembler** or
a tiny toy language is the small first step that needs no libc. No change —
recorded here as a pointer.

**e. Download and run a Rust toolchain — "GnuRust" / gccrs (new, the far end of item 5).**
The ambitious flip side of item 5: rather than *writing* a compiler, *acquire* a
prebuilt one — **gccrs** (the GCC Rust front end) or a ported Rust toolchain —
and run it on-device. The reality check makes it the furthest-out item here: it
needs (1) the POSIX libc personality mature enough to run a very large C++
program (gccrs is C++), (2) a filesystem with real capacity and enough RAM, and
(3) a **download/fetch flow** (the network stack + a `fetch`-to-file path exist;
a real package step doesn't). So it's a *consumer of the libc + fetch
capabilities*, even further out than a small C compiler — and, like item 5, a
"because it's the Ouroboros thing to do" goal, not a need. Recorded as the
north-star tip of the compiler direction.

**f. Graphics card / GPU support (new, large, QEMU-shaped start).**
Today the only "graphics" is the boot-discovered **GOP linear framebuffer** that
`cond` blits glyphs into — no acceleration, no mode-setting, no display-controller
driver. Real GPU support is a large hardware arc; the realistic starting point
(matching every other device here) is **virtio-gpu on QEMU** — a virtio device
over the existing `virtio_mmio` transport, like virtio-net/blk, giving
mode-setting and a 2D blitter under the same DMA-in-the-kernel /
protocol-in-userland split the whole system already uses. A real discrete GPU is
out of scope. **Consumer question, stated plainly:** nothing needs it yet — the
framebuffer console suffices, and the terminal/editor work (items 1/c) lives
happily on the plain framebuffer — so this is for an eventual windowing system /
graphical apps, sequenced behind everything with a nearer consumer. Note
virtio-gpu as the entry point when the time comes. **The whole GUI stack above
this** — how far up toward SDL/GTK it could go, which layer actually blocks, and
why a Plan 9 `/dev/draw`-shaped `drawd` server (not an `SDL_Surface` pixel-ship
model) is the fit for a 768-byte inline ABI with no shared memory — is worked out
in [`research-gui-stack.md`](research-gui-stack.md). Its finding: the mouse
driver and a `drawd` draw server are the two steps that unlock everything else,
and the pixel-transfer model is the day-one decision to get right.

**g. Cluster data redundancy — documents failsafed across nodes (new, a later cluster phase).**
The cluster (see [`roadmap-cluster.md`](roadmap-cluster.md)) shares disk and
resources today, but a document lives on exactly **one** node — lose that node,
lose the file. The direction: **automatic replication** so data is mirrored
across cluster nodes and survives a node failure (a write on one node propagated
to others, with failure detection and recovery). This is a genuine
distributed-systems arc — the cluster-distributed postmortem deliberately scoped
**single-writer + clean-disconnect** and put concurrent-writer/replication *out
of scope*, so this is exactly where that boundary would be revisited: a
replication protocol, a consistency contract (quorum? primary-backup?), conflict
handling, and failure detection. Large, and gated on the consistency model being
worked out; it belongs as a later phase in `roadmap-cluster.md`, not a near-term
item. It's the strongest "why" the project has for going distributed *beyond*
resource-sharing. **The substrate to borrow from Redox: RedoxFS's shape** — a
small Rust filesystem (a daemon, exactly `fsd`'s model) with copy-on-write plus
**data *and* metadata checksums**, written from scratch rather than porting ZFS
(Redox tried the ZFS port and abandoned it as microkernel-hostile). Checksums +
CoW are the integrity substrate a replication scheme needs; RedoxFS is the "write
it small, don't port a giant" precedent. See `docs/research-redox-and-pi.md`.

**h. SQLite — an on-device database (new, the canonical first libc port).**
SQLite is a single-file, dependency-light **C library** — the textbook "port one
self-contained C program" target — so it's a direct **consumer of the POSIX libc
personality** (the section above): it needs `open`/`read`/`write`/`fsync`/
`lseek`, optionally a little `mmap`, and file locking. Once the libc runs C
programs, SQLite is a high-value, self-contained first real port (a real database
on the device) *and* an excellent libc **test case** — it exercises a large slice
of the file API and its own test suite is exhaustive. Recorded as a concrete,
motivating milestone for the libc arc: "the libc is real when SQLite runs on it."

## Review findings against shipped code (2026-08-29 →)

Raised by code review of the security tier and **verified**, but left unfixed
at the time because they concern code already on `main` rather than the branch
under review. Recorded here so they are not lost with the review transcript.

Kept as a **ledger**: an item that gets fixed is struck through with the date
and left in place, rather than deleted. Two reasons. A reader wants to know a
hazard was *considered*, not just that it is absent today; and the section
would otherwise silently shrink into looking like nothing was ever found.

- **Six findings from the 2026-09-06 delegation review, all PRE-EXISTING**
  (raised against the `TO_NET`-in-pipelines fix; the five findings that were
  *about* that diff were fixed in it). Each is its own change, deliberately not
  bundled — a repair is a change, and changes have the defect rate of the code
  they fix.
  - ~~**The shell's pipeline error paths `KILL` without `WAIT`.**~~ **Fixed
    2026-09-06.** `KILL` is refused on a zombie (`task_exists` matches only
    `Runnable | Blocked`) and only `WAIT` reaps, so a stage that exited on its
    own — a bad `grep` pattern, an unknown flag — leaked one of just five
    spawnable slots. **Measured before the fix:** five `cat /etc/passwd | grep`
    (no pattern) left all five slots held, after which the shell could not run
    *any* `/bin` program — `echo still-alive` answered `echo: no free task
    slot` — and only five `wait <n>` calls recovered it. Eight abandon paths
    went through one `kill_and_reap` (seven since later on 2026-09-06: the
    link-authorization path now `KILL`s and, for a stage that had already
    exited, reaps it through `wait_pipe_stage` so its exit code is reported
    rather than discarded); the *completion* paths never leaked,
    because they already `wait_pipe_stage` every stage. Seven iterations now
    leave the pool untouched.

    **One leak of the same shape survives, deliberately: Ctrl+C during a
    pipeline's wait loop.** `wait_pipe_stage` treats `WAIT_INTERRUPTED` as
    something to report — *"it may still be running - see ps"* — and moves to
    the next stage without reaping, so the slots stay held. It is left as-is
    because the task genuinely may still be running and reaping a live task
    behind the user's back is worse, and because unlike the fixed bug it
    announces itself and names the tool that resolves it. Closing it properly
    means deciding what Ctrl+C should *mean* for a pipeline (detach? kill the
    whole group?), which is a design question, not a repair.
  - ~~**The "consumer already exited, stay quiet" heuristic inspects the wrong
    slot.**~~ **Fixed 2026-09-06.** The kernel denies `DELEGATE` on a dead
    *grantee* before it looks at the target, but the shell explained the denial
    from the *target*'s state — so a producer that exited early got `pipe:
    could not authorize the stream` printed on top of its own message, which
    is exactly the noise the check exists to suppress. **Fixed in the kernel,
    not the shell:** `DELEGATE` now answers `TASK_ERR_NO_SUCH_TASK` for a dead
    slot at either end and `MSG_ERR_DENIED` only for a refusal, as `KILL`,
    `FG`, `WAIT`, `MSG_SEND` and `MSG_CALL` already did, and the shell reads
    the reason from the answer. The first version read `TASK_STATE` for both
    ends *after* the denial; the `high` review showed that a later state read
    cannot tell a refusal from an exit that happened in between, so it would
    have silenced a real denial — and a nested shell, whose static mask holds
    no spawnable slot, is refused on every link.

    **Narrower than recorded for a producer that PRINTS, routine for one
    that does not — measured, both.** `ls /nope | wc` and `cat /nope | wc`
    were already quiet before the fix: a producer that prints blocks on
    `cond` and hands the shell its turn back before the link is authorized,
    and one that reaches `ulib::end_of_stream` stays alive in its 150-tick
    retry of the denial. But a producer that neither prints nor ends its
    stream runs to exit *first*: `SPAWN` is the shell's longest syscall, a
    tick is pending at its `eret`, and the child gets the slice.
    `touch /T3 | wc` on the unmodified shell reached the dead-link path on
    the first attempt — where the original code would have printed the line —
    and the next attempt, `touch /T4 | wc`, went the other way and wedged the
    shell (next item). So the path was reachable all along by any silent
    producer, and the first write-up's "a tick landing in a window of a few
    instructions" was a reading. Forced for verification by parking the
    shell until stage 0 was a zombie: under that window every pipeline
    printed the line before the fix and none after; with the window removed
    the controls are byte-identical. The dead stage's exit code is reported
    (`pipe: ls exited with code 1`), silent for exit 0.

    **The next change, not this one:** when the *producer* is the dead end
    and the consumer is alive, the shell could end the stream on its behalf.
    It holds the send right, and a producer that died before its link was
    authorized delivered nothing, so one empty message is the whole correct
    stream — `touch f | wc` would print `0 0 0`, and the racy and non-racy
    paths would converge. Not done here: it is a behaviour, not a repair.
  - **A producer that exits without `end_of_stream` wedges the pipeline.**
    `ls -? | wc` leaves `wc` blocked in `pipe_recv` forever and the shell
    waiting on it: no prompt came back within the rig's 90 s step timeout.
    `touch /T4 | wc` the same — `touch` exits 0 after the link is authorized
    and sends nothing, so a silent producer either wedges the shell or (when
    it exits before the link) is torn down with its consumer; it never works.
    Found 2026-09-06 while using `-?` as the exit-without-EOF producer the
    previous item's reproduction needed — and `usage_if_requested` is only the
    instance that was run. **26 of the 53 programs under `programs/` never
    call `end_of_stream` at all** (counted by grep: every `admin/*` tool,
    `cp mv rm mkdir rmdir touch chmod chown write writeat more`, `send recv
    readkey args selftest`, both demos, the four servers), and the ones that
    do skip it on their early error exits (`cat` with no operand). What the
    rig did *not* measure is Ctrl+C: `drive-qemu.py` cannot send it, and the
    review traced that for a console-sink pipeline the consumer owns the
    keyboard, so Ctrl+C kills it and the shell's `WAIT` returns — the slot
    hold above is the redirect/drain sinks' problem, not this one. Untested
    either way. **`netd`'s `cpu` path has the same wedge with a leak on top:**
    `cpu_child_msg` reaps the child only on the empty message and `pump_send`
    holds the connection until then, so a remote command that exits without
    one never streams, never FINs, and its slot is never reaped by anyone.
    Own change, two shapes: every exit path ends the stream (the instances,
    half the tree), or the kernel ends it for any task whose stdout target is
    not the console (the class). The class shape is not free, and the review
    listed why: there are three death paths (`EXIT`, `KILL`, the EL0-fault
    teardown) and `fail_calls_to` covers `MSG_CALL` waiters, not the plain
    `MSG_RECV` that `pipe_recv` is; a well-behaved producer already sends its
    own empty message, and a second one lands in a mailbox nothing drains —
    the shell's next capture would read it as an immediate EOF; and a full
    mailbox at teardown cannot be retried by a task being torn down. Decide
    the shape before writing either.
  - **The shell's pipeline teardown uses slot numbers as identity.** A stage
    that faults at EL0 goes straight to `Unused` (`kill_current_and_switch`
    stores it, no zombie), and any spawner — `netd`'s `cpu` handler, a nested
    shell — may take that slot before the shell's `kill_and_reap` loop reaches
    it, which then kills or reaps a stranger; `KILL` and `WAIT` have no owner
    check. The same class as the `netd` raw-slot item below, seen from the
    other side, and pre-existing: the 2026-09-06 fix did not widen it, but it
    is the moment "unused means already exited" was written down as a rule.
    Raised by the `high` review, traced not run.
  - **`cmd_pipeline` runs a builtin-headed second segment after the first
    segment failed.** `run_head_pipeline` returns `()`, so when the drained
    upstream segment aborts — a stage that will not spawn, a link that cannot
    be authorized — the builtin and everything after it still run and print
    success-shaped output under the error line, where an all-program pipeline
    aborts whole. A `bool` return gating the second call is the whole fix.
    Raised by the `high` review, traced not run.
  - **Nothing re-grants `TO_NET` after a supervised `netd` restart.**
    `clear_delegate` strips bit 4 from every live task (correctly — the slot
    could be reused by something else), but the grant is only ever made at
    spawn. A program spawned before the restart is netless for the rest of its
    life, and `net_msg_call` busy-spins its 150-tick deadline with no yield
    before reporting the same generic failure this arc was about.
  - **`netd` demuxes remote-exec by raw slot number** (`PendingRun.owner`,
    `TcpConn.cpu_child`). Slots are recycled the moment a task is reaped, and
    `handle_client`'s comment states an invariant — that only one task can hold
    `TO_NET` — which the shell's blanket grant has now made false.
  - **A nested shell cannot delegate `TO_NET` at all.** `may_delegate` reads
    the *static* mask and spawnable slots have none, so every `delegate_net`
    from a spawned `SH.BIN` is denied, discarded by its `let _ =`, and its
    whole subtree is silently netless — bit-for-bit the symptom this arc spent
    two days tracing. **Wider than `TO_NET`, measured 2026-09-06:** the same
    refusal hits every producer→consumer link, so a nested shell cannot run
    *any* pipeline — `exec /EFI/ORBS/SH.BIN`, `fg 6`, log in, `ls / | wc`
    answers `pipe: could not authorize the stream` and kills both stages. It
    is at least honest now: since `DELEGATE` answers `TASK_ERR_NO_SUCH_TASK`
    for a dead slot, that line is only ever printed for a real refusal.
  - **`delegate_net` discards its result**, so the one grant this arc is about
    is the only `DELEGATE` in the shell with no failure signal. A future
    `caps_for_slot` edit dropping `TO_NET` from slot 0 would return the tree to
    the pre-fix behaviour with no diagnostic anywhere. A check that cannot
    fail.

- ~~**The 9P export bypasses permissions entirely.**~~ **Closed 2026-08-31** by
  per-user cluster identity (v0.15.0), which this finding specified. `fsd`'s
  `effective_caller` now REFUSES a `NET_TASK` request that states no identity
  rather than falling back to netd's root, and `netd`'s `AsUser::enter` makes a
  `cpu` child inherit the mapped user, so both doors below are shut. The
  residual — an authorized *machine* may still claim any of its own users'
  names — is frontier item 1 above, not this entry. The finding as originally
  recorded, left in present tense rather than rewritten:

  `netd` relays a remote
  request to `fsd` under its *own* root identity, so `check_access`'s
  `if uid == 0 { return true }` short-circuits before any mode is consulted —
  and `mount -r` is not root-gated (the shell's only `shell_uid() != 0` check is
  `cmd_su`). On the two-node rig, an unprivileged user on node B can
  `mount -r <A> /mnt/a; cat /mnt/a/etc/shadow` and read every hash on node A;
  `cpu A passwd root` is a second door, since a spawned remote child inherits
  netd's root. **Not a regression** — the export has always been
  machine-authenticated rather than user-authenticated — but `/etc/shadow` gives
  it a payload it did not have before — and `accountd` (2026-08-30) sharpened
  that considerably: `cpu A passwd root` now reaches a *privileged writer* on
  the far end, not just a readable file. This is the concrete argument for
  **per-user cluster identity**, which was promoted out of the north-star
  section to the top of "What's next" on 2026-08-30 precisely because of it.
  **This finding was the specification for that arc**, and closed with it.
- **`fsd`'s per-request cost multiplied** when ancestor-`x` traversal landed:
  `path_allows` now costs 2 + (ancestors + 1) + 1 path resolutions where it cost
  1, `NP_OPEN` with `O_RDWR` does three ancestor walks, and `caller_id` issues
  both credential syscalls (`SENDER_ID`/`SENDER_GROUPS` since 2026-08-30, when
  the recycled-slot escalation below was closed; `GET_ID`/`GET_GROUPS` before
  that) two or three times per request — including fetching an 8-gid list for a
  root caller that returns immediately. (2026-08-30: those two calls are now
  `SENDER_ID`/`SENDER_GROUPS`, which read a captured cell rather than
  validating a live slot — marginally cheaper, but the *count* is unchanged, so
  this finding stands.) Same shape
  as the v0.4.1 FAT32 O(n²) read that ran past the supervisor's runnable-wedge
  and got `fsd` restarted mid-read; the cost is milliseconds per sector on real
  USB-MSD, not QEMU virtio-blk. **Unmeasured on hardware.**
- ~~**`NP_STAT`/`NP_CHMOD`/`NP_CHOWN` skip the "does this filesystem model
  modes?" short-circuit**~~ — **fixed 2026-09-02.** They called
  `ancestors_searchable` directly instead of going through `path_allows`, so on
  FAT32/exFAT — which record no mode, so every other verb short-circuits to
  allow — a non-root stat of a path containing `..` was refused while a read of
  the same path succeeded, and every `ls -l` entry paid a guaranteed-useless
  ancestor walk. The question is now asked once at the top of `check_access`,
  where it covers every verb.

  **One correction to the finding as originally written**, since it named a
  symptom that cannot occur: it said `cat ../f` succeeds while `ls -l ../f` is
  refused *from the shell*. It does not — `ulib::normalize_path` collapses `..`
  client-side, so no `/bin` program can send `fsd` such a path. The divergence
  was reachable only from a client that sends raw paths, which means the 9P
  export. Verified there in both directions against an unpatched guest
  (`np9p_client.py stat /BIN/../ETC/PASSWD --user user` → `FS_ERR_PERM`, `read`
  of the same path → served) — and *that* took fixing the observer first, whose
  `stat` op was sending `NP_READ_FILE`. The cost half was reachable all along:
  `ls -l` sends one `NP_STAT` per entry.

- ~~**`check_access` is default-allow** (`_ => true`), so a future `NP_` verb
  added without an arm here ships unauthenticated.~~ — **fixed 2026-09-03.**
  The default arm now REFUSES. Every verb that can reach `check_access` has an
  arm (`handle_ninep` only receives `[NP_BASE, NP_LIMIT)`, and the four fid ops
  return before it), so the arm was **unreachable** — which is precisely why it
  had to change: nothing exercised it, nothing would have warned, and the
  change that adds a verb touches `ninep-abi`, not `fsd`.

  Refusing is the safe direction for the same reason the root bypass exists: an
  enforcement mistake can then only over-restrict a non-root caller, never hand
  out access. But a verb *silently refused* is still a bug, so a
  `const _: () = assert!(NP_LIMIT == NP_BASE + 20, ...)` makes the compiler say
  so — adding a verb to `ninep-abi` now fails the `fsd` build with a message
  naming `check_access`, rather than being discovered as a mysterious
  permission denial. Verified by mutation: bumping `NP_LIMIT` produces
  `error[E0080]: evaluation panicked: a NP_* verb was added or removed…`.

  Enforcement behaviour is unchanged, checked on the ext2 rig (the only
  filesystem that models modes): a non-root user is refused `ls`, `cat` and
  `chmod` on a 0700 directory it does not own, allowed to write and read back
  inside a 0777 one, and the C fid path (`NP_OPEN`/`PREAD`/`PWRITE`/`FSTAT`/
  `CLUNK`, which bypasses this check by design) still round-trips a file.
- ~~**A server authorized on the *current* occupant of the sender's slot.**~~
  **Fixed 2026-08-30.** `GET_ID(sender)` answered "who occupies slot N now",
  not "who sent this": a non-root task could `MSG_SEND` (non-blocking), `EXIT`,
  and have its slot reaped and re-spawned before `fsd` drained its mailbox, at
  which point the request was authorized as whatever landed there — root, if a
  root command did. Slots 5+ are the pool the shell recycles for every command,
  so this was the ordinary path, not an exotic one. The earlier `is_live` guard
  closed only the *dead*-slot half; a recycled slot is alive and
  indistinguishable, because a message carries a bare `u8` slot number with no
  generation. The kernel now binds the sender's credential at send
  (`SENDER_ID`/`SENDER_GROUPS`) — see `docs/architecture.md`'s syscall table.
  Raised against the unmerged account server, but it was `fsd`, in shipped
  code, that had it on every permission check and every fid op. Written up in
  [`asking-the-right-question-postmortem.md`](asking-the-right-question-postmortem.md).
- ~~**One malformed export frame could kill the network for the boot.**~~
  **Fixed 2026-08-30** (#44). `NP_WRITE_AT` sliced `&payload[p0..p0 + dlen]`
  with the range *start* unclamped, and two sibling arms had a wrapping add that
  put `end` below a clamped `start` — both panic, and a panic in `netd` parks it
  and burns a supervisor restart. Fixed as a class with one clamping helper.
  Raised by the review of #42 as pre-existing; proven both directions with the
  host-side Python peer. Note the `-d int` health bar reads `0` either way: a
  userland panic parks a task rather than raising a CPU exception, so the signal
  is the supervisor's restart line.
- ~~**`warn_if_unprotected` fails open**~~ — **fixed 2026-09-02**, and the
  structure that was the real finding is gone rather than patched.

  `mounted_fs_unprotected` returned `false` — "this filesystem enforces
  permissions" — for **any** non-zero `FSOP_MOUNT_INFO` status, including the
  `NO_FS` that means "`fsd` has not finished mounting yet". It was the first
  statement of `login()` and had **no retry**, while `read_account_file` three
  lines away carries a bounded 200-try `NO_FS` retry *precisely because login
  can beat the mount* — two functions in one file disagreeing about whether a
  race exists. It passed on QEMU because virtio-blk mounts first; the device
  that loses is USB-MSD on real hardware, where the whole symptom is a warning
  that silently does **not** print.

  Three changes, and the first is the one that matters:

  - **`login` now reads the account file FIRST and warns SECOND.** That makes
    the race unreachable instead of merely unlikely: `read_account_file`
    returns only once `fsd` has answered, so the warning asks a server that is
    up. No second retry budget to keep in step with the first. The printed
    order is unchanged.
  - **`FSOP_MOUNT_INFO` carries a flags word** with
    `MOUNT_FLAG_ENFORCES_MODES`, derived by `fsd` from the root's `stat` — the
    same question `check_access` asks, via one tri-state helper whose two
    callers resolve "cannot tell" in opposite directions (deny more / warn
    more) and say so. The shell no longer string-matches `"ext2"`: a security
    decision by string comparison would raise a false alarm for the next
    filesystem that models modes.
  - **An unknown status now warns**, where it used to reassure. `NO_FS`
    deliberately does not: nothing is mounted, so there is no filesystem to
    make a claim about, and `login` says "no /etc/passwd" for itself.

  All four branches were exercised by mutation, since three of them cannot be
  reached on a healthy QEMU boot: clearing the flag on ext2 raised the warning
  *while `mount` still printed the name `ext2`* (which is what proves the shell
  reads the flag and not the name), and forcing `FS_ERROR` and `NO_FS` produced
  the warn and no-warn branches respectively.

  **Still open, deliberately scoped out**: it inspects tree 0 only, so a
  multi-mount with `/etc` on a different tree misreports. That needs the
  warning to know which tree `/etc/passwd` resolved through, which is a
  namespace question rather than this one.

- ~~**`libc/include/sys.h`'s `FS_ERR_MIN` had drifted from the Rust
  constant.**~~ **Fixed 2026-08-30** (#37). The C header hand-mirrored the
  reserved-error floor at `MAX-33` while `accountd`'s codes moved it to
  `MAX-38`, so a C caller would have read `ACCT_ERR_IO` as a *successful*
  return value. No live consumer (no C program calls `accountd`), which is
  exactly why nothing caught it. The Rust definition now carries a note back to
  the mirror, since the definition is what gets edited next. *Recorded as a
  strike-through rather than deleted, per the ledger note above — it was
  removed outright when fixed, which was the wrong call and is corrected here.*
- ~~**`useradd` accepts an empty password**~~ — **fixed 2026-09-02.** `passwd`
  rejected one and `useradd` did not, so an account created by pressing Enter
  twice was loginable by pressing Enter — confirmed on `main` before the fix
  (`useradd bob`, Enter, Enter → `useradd: created bob`, then `login: bob` with
  an empty password → `uid=1001(bob)`). It is the only writer of an *initial*
  secret, so it was the one that most needed the check. Both manpages now state
  the rule; neither did.

## Open gaps (small, from the old parking lot)

Known small gaps, not yet sequenced (the *completed* parking-lot entries — USB
keyboard, GOP console, preemption, task destruction, driver isolation, etc. — are
in [`roadmap-completed.md`](roadmap-completed.md)):

- **`NET_WAIT` is not a sleep — TRIGGERED ON PI HARDWARE, deliberately not fixed
  yet.** `load_auth`'s retry loops treat `NET_WAIT(40)` as a 40 ms timer, but
  `tasks.rs` wakes a `NetInput` waiter on `has_queued_message` *without consuming
  it*, and `load_auth`'s reads are sender-filtered `MSG_CALL`s that drain nothing
  else. Once the supervisor's health ping is queued (~1.28 s in) every subsequent
  wait returns instantly, so the documented "~2 s at 40 ms a try" becomes a
  busy-spin that spends the budget at once — and the `\NOEXEC` probe, the read
  that fails *open*, is first in line.

  **The trigger is hardware, not a decision.** Instrumented on QEMU the loop
  retries **0 times**, because virtio-blk has `fsd` ready before `netd` asks: the
  path never runs, so neither the bug nor a fix is observable there. The fix —
  draining the mailbox while waiting instead of ignoring it — touches
  supervision, and writing it blind against a rig that cannot exercise it is how
  the fixes in this arc's own review kept needing fixes. Queued as step 4 of
  [`testing-pi4.md`](testing-pi4.md) §8 and written up as its Risk 4b, so the
  first bench session picks it up rather than rediscovering it.

- **An intermittent failure of `cp` across a remote mount, observed once.**
  2026-08-30, on the two-node ext2 rig: `cp /mnt/a/README.TXT /mnt/a/COPY.TXT`
  returned `cp: failed` in one run and succeeded in the two that followed,
  including a re-run of the byte-identical script. Recorded rather than
  dismissed, because an intermittent failure that is not written down is
  indistinguishable from one nobody has hit yet.

  **Not the wire-clamp change** (`wire_slice`, the same day): that rewrite is
  provably a no-op on every input the old expression did not panic on — for
  `off <= len` the two produce the same range, since `len - start` *is* the
  old `saturating_sub`, and they diverge only where the old form's range start
  was out of bounds. So the cause is older than that fix and still unknown.
  Suspicion, untested: the export is stop-and-wait, and a remote `cp` is the
  longest chain of round trips any command makes — a dropped segment plus the
  RTO is the obvious candidate, and `net-ext2-*.pcap` from a failing run would
  settle it. Reproducing it is the first step, and may take a loop.
- ~~**`mv` cannot replace an existing destination.**~~ — **fixed 2026-09-02.**
  All three arms now replace an existing destination when both it and the
  source are ordinary files, which is what POSIX `rename` does and what every
  Unix `mv` does.

  **ext2 gets the near-atomic version the note predicted**: the whole change is
  one write of the destination's directory entry, re-pointing it at the
  source's inode. The name never resolves to nothing — a reader sees either the
  old file or the new one — and everything after that write is cleanup (unlink
  the source name, drop the replaced inode). A crash inside the cleanup leaks a
  link count or some blocks, both of which `e2fsck` repairs, rather than losing
  either file.

  **FAT32 and exFAT cannot**, and the note was right about why: their directory
  entries hold the file's own location rather than an inode number, so the
  change takes two writes rather than one. What they cost is *atomicity*, not
  the name — the new entry is written before either old one is freed, so a
  reader in between finds two entries and gets one of the two files, never
  nothing. That ordering was got wrong first and caught by review: freeing the
  destination first survives a crash no worse but destroys `dst` on an ordinary
  *error*, such as a directory that cannot be extended. Data chains are freed
  last, so a crash leaks clusters (which `fsck_msdos`/`fsck_exfat` reclaim)
  rather than dropping live data out from under a name that still resolves.

  **Deliberately still refused**: a directory as the destination, and a
  directory moved onto an existing name. POSIX also replaces an empty directory
  with a directory; that needs an emptiness check and the parent link counts
  moved, and nothing has asked for it.

  **The commands ask for the intent; the server does not.** `fsd`'s `NP_MV`
  replaces, which is POSIX `rename` and right for a protocol verb with nobody
  to consult, while `/bin/mv` refuses an existing destination unless `-f` is
  given — and `/bin/cp`, which has always clobbered silently, gained the same
  flag so the two most destructive commands agree. A **refusal, not a prompt**:
  prompting needs the keyboard, and neither command has one as a pipeline
  stage, under `cpu` on another machine, or when the request arrives from a 9P
  peer, so a prompt would guard the interactive case and nothing else. (There
  is no `isatty` equivalent to branch on, which is why "prompt when we can" was
  not built.) `> file` redirection still truncates silently — a third case, not
  addressed here.

  **The self-move guard now exists in `fsd` as well as `/bin/mv`.** `mv f f`
  must be a no-op, because the replace path would otherwise free the entry it
  is about to rebuild from — the `cp x x` self-destruct one layer down. The
  `/bin/mv` guard cannot cover the 9P export, which sends raw paths; removing
  the `fsd` guard and driving a self-`mv` from the host client destroyed a
  directory entry (the volume went from 150 files to 149) and returned an
  error. `np9p_client.py` gained an `mv` op to make that demonstrable, for the
  same reason its `stat` was fixed the day before.

  Verified on all three rigs against the foreign checkers: `e2fsck` clean
  (and it reports `Unattached inode` when the cleanup is mutated away, so the
  clean result means something), `fsck_exfat` "appears to be OK" including the
  active bitmap, and `fsck_msdos` clean but for a pre-existing FSInfo drift —
  see the next item.

- **`cargo doc` is noisy for the userland crates, and that hid a real defect.**
  The kernel is held at **zero** unresolved intra-doc links (the cluster-keys
  arc did that deliberately, precisely so the next one would be visible);
  `fsd`, `ulib`, `mv` and `cp` together emit **39**. Because nobody reads that
  output, a doc comment ABSORBED by a function inserted above it — `set_dirent_inode`
  opening its rustdoc with `remove_dirent`'s description — shipped in the `mv`
  work and was caught by a code review rather than by the tool that exists to
  catch exactly this, and had caught it once before. Bringing the userland
  crates to zero is a small, purely mechanical job whose value is entirely in
  the baseline it creates.

- **`fsd` never maintains the FAT32 `FSInfo` free-cluster count.** It is
  written once at `format` time and never updated by an allocation or a free,
  so `fsck_msdos` reports "Free space in FSInfo block (N) not correct (N-1)"
  after any write. Found 2026-09-02 while checking the `mv` work, and
  **confirmed pre-existing**: a single `echo one > /F` on `main`, with no `mv`
  involved at all, produces the identical warning. Harmless today — the count
  is a hint and every real driver recomputes when it does not trust it — but it
  is a false positive that will keep showing up in exactly the check most
  likely to catch a genuine allocator bug, which is the argument for fixing it.
  The fix is small and local: adjust the stored count in `alloc_cluster` and
  `free_chain`, and write the sector back.

- ~~**`grep` has no regex**~~ — **shipped 2026-08-29.** Patterns are POSIX
  **extended** regular expressions (`.` `*` `+` `?` `[...]` `^` `$` `|` `(...)`),
  via a new pure, host-tested **`regex` crate** at the repo root; `-F` keeps the
  old literal-substring behaviour. Bounded by design (an explicit backtracking
  stack, not recursion; empty-body repeats refused so every accepted pattern
  terminates; a step budget whose exhaustion reports `Limit`, never a silent
  "no"). Still open, each a real addition rather than a tweak: back-references,
  `{n,m}` counted repetition and submatch capture (`[[:alpha:]]` class names
  shipped 2026-09-02: all twelve, computed from `core`'s `is_ascii_*`
  predicates rather than transcribed as bit tables, with an unknown name an
  error rather than a fall back to the literal letters) —
  plus the shared `ulib` option parser of North-star item 2, still unbuilt. The
  `regex` crate is deliberately reusable: an editor's search and a `find` are
  the next consumers.
- ~~**`useradd` is not atomic**~~ — **fixed 2026-08-29.** The `/etc/passwd` write
  is now the single commit point: the group entry and home directory are prepared
  first, a failed prep commits nothing and exits non-zero, and a failed commit
  rolls the prep back (`accounts::remove_line`, `rmdir`). See `CHANGELOG.md`.
- ~~**Three near-identical small-file readers**~~ — **the two shell copies merged
  2026-08-29** into one `read_account_file` (carrying login's boot-time `NO_FS`
  retry), used by `login`, `su`, and `id`'s name lookups. `ulib::read_file_all`
  stays separate by design — it lives in the `/bin` programs, and the shell has
  its own fs layer. Likewise `ulib::read_line` still duplicates
  `login::read_field` (the same split); consolidate if the shell ever gains a
  `ulib` dependency.

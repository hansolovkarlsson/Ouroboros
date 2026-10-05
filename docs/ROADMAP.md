# Ouroboros roadmap

Forward-looking plan — what's next and why, in plan form rather than
chronological narrative. **Completed arcs and milestones have been moved
out** to [`roadmap-completed.md`](roadmap-completed.md) (the plan-shaped
record — how each arc was sequenced and what was learned) and
[`CHANGELOG.md`](CHANGELOG.md) (the condensed milestone log), so this
document stays about what's *still open*. For *how* something already built
actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see the postmortems under `docs/postmortems/` and `CLAUDE.md`.
This document is the one to update first when direction changes.

> **The long-term direction** — a Plan 9-style **resource-sharing cluster**
> (distributed Ouroboros: machines sharing storage/devices/services over
> per-machine namespaces and a uniform file protocol) now has its own phased
> plan in [`roadmap-cluster.md`](roadmap/roadmap-cluster.md). The Plan 9 "local
> namespace + uniform protocol" work below is **Phase 0** of that arc — the
> foundation the whole distributed vision builds on, not a standalone item.

## What's next (the current frontier)

> **Released 2026-10-04: v0.22.0, the Raspberry Pi 4 runs a full session**
> (#176 to #207, recorded in [`CHANGELOG.md`](CHANGELOG.md) and
> [`roadmap-completed.md`](roadmap-completed.md); the board's record is
> `testing/testing-pi4.md` section 6). The Pi's open directions are under "The
> Raspberry Pi, after the first full session" below; per-user keys steps 0 to
> 4 went out in the same release, with step 5 next (item 1).

> **Done 2026-09-26: session-scoped authentication**
> ([`roadmap-session-auth.md`](roadmap/roadmap-session-auth.md), moved to
> [`roadmap-completed.md`](roadmap-completed.md)). A held session is keyed: an
> X25519 exchange inside the signed `NP_SESSION`, then an HMAC and a strict
> sequence number per message. The four crypto operations per fid verb fell
> from 2,926 µs to 169 µs, and the median verb cycle from 7.50 ms to 4.53 ms.
> **Still owed from it:** the boot identity measured on Parallels (A6 of
> `testing-parallels.md`): which store serves the counter there, and whether
> the firmware offers `EFI_RNG_PROTOCOL`. The Pi 4 answered on its first
> serial boot (2026-10-01, `testing-pi4.md` section 6): 32 bytes of entropy,
> and the counter from `\EFI\ORBS\BOOTID.TXT` with the UEFI variable absent. A node without entropy keys its sessions with no forward
> secrecy, which the plan states rather than claims.

The microkernel arc is largely built — the FAT32 **filesystem** (`fsd`),
the **console** (`cond`), and the **network** server (`netd`) all run as
supervised, MMU-isolated userland servers, with a capability model, crash
recovery, and grant/safecopy bulk IPC (all in
[`roadmap-completed.md`](roadmap-completed.md) / [`CHANGELOG.md`](CHANGELOG.md)).
So is the users/permissions arc, whose last item — per-user *cluster* identity —
shipped 2026-08-31, and the per-machine-keypair arc that followed it
([`roadmap-cluster-keys.md`](roadmap/roadmap-cluster-keys.md)), which retired the shared
cluster key entirely. What is left is the tier below: keys are per-*machine*,
not per-*user*, which is item 1. In rough order of value (2 and 3 are what
the microkernel arc itself still leaves open):

1. **Per-user keys for the cluster.** **Planned 2026-09-26:
   [`roadmap-user-keys.md`](roadmap/roadmap-user-keys.md)** (password-derived
   keys held by `netd` while the user is logged in, root squash first, shaped as the foundation for the
   auth server below). **Steps 0 to 4 on `main` 2026-09-27** (#170 to #174:
   the impersonation gate, root squash, PBKDF2, the user key and realm, and
   keys held by `netd` from login to logout); step 5, the wire, is next. Per-user cluster *identity* shipped
   2026-08-31 (see [`CHANGELOG.md`](CHANGELOG.md) and
   [`unspellable-postmortem.md`](postmortems/unspellable-postmortem.md)): a remote request
   carries the requesting user's **name** inside the signature, the far side resolves
   it through its own `/etc/passwd` and refuses a stranger, and the identity
   reaches `fsd` as a **required parameter** carried in the request rather than
   an opt-in wrapper and a latch. `cpu` is covered too — `netd` assumes the
   mapped user's identity for the spawn, so a remote command inherits it.

   **What is left is the tier below it: keys are per-machine, not per-user.**
   The shared secret is gone: each machine has its own Ed25519 keypair and
   authorizes peers by public key, so a member can be revoked by deleting a
   line. But an authorized *machine* can still claim any user who has an account
   on the export, **root included** (the export checks only that the name exists
   there; `map_user` has no uid-0 case, and `fsd`'s root bypass applies). So the
   model defends against the users of a trusted node, which is the real
   exposure, but not against a compromised node: one is root on every export
   that lists it. Per-user keys would close that, and the design forks are
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
     fixed stack (the loader's `STACK_PAGES`) that has hit the guard page five
     times, and no mutable statics
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
   failure. See [`roadmap-cluster-keys.md`](roadmap/roadmap-cluster-keys.md) for the
   step log (and for why the *symmetric* version was rejected: with a symmetric
   key, the ability to verify is the ability to forge),
   [`roadmap-completed.md`](roadmap-completed.md) for the plan-shaped summary,
   and [`cluster-keys-postmortem.md`](postmortems/cluster-keys-postmortem.md) for what it
   cost to learn.

   It killed "one shared secret = interchangeable members" and gave per-peer
   revocation, the largest single weakness of what shipped in v0.15.0. It
   deliberately left **"B can claim any user"** open (recorded then as "any of
   its own users"; the export checks only that the name exists there, root
   included, see above), which is
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
     [`roadmap-cluster-phase1.md`](roadmap/roadmap-cluster-phase1.md) and
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

     **✅ DONE 2026-09-12. [`roadmap-fid-verbs.md`](roadmap/roadmap-fid-verbs.md)
     is the plan: seven ordered steps, each with a check and a negative
     control, ALL COMPLETE.** A C program's `open`/`read`/`write`/`close` over
     a remote mount works guest-to-guest, and permissions are enforced across
     it. Steps 1-3 made a C read work; step 4 (the session) landed 2026-09-07;
     steps 5 (the export serves the fid verbs on a session), 6 (`NP_PREAD` plus
     the dispatch fold), the client half of 5 (Decision 4: `netd` holds a
     `ClientSession` per (endpoint, uid)), and 7 (`NP_PWRITE`, and `libc`'s
     remote `write()`) all landed 2026-09-12. Each measured by a gate under
     `run-guest.sh` or a C witness on the two-VM ext2 rig (`cbig` reads a
     nine-chunk file and byte-matches; `cwrite` writes two chunks and reads
     them back identical, refused `FS_ERR_PERM` for a user without `w`), each
     shown failing by mutation.

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
     and controls, in [`roadmap-fid-verbs.md`](roadmap/roadmap-fid-verbs.md).

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
   [`testing-qemu.md`](testing/testing-qemu.md) both show `ls /mnt/a` in that recipe,
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
   [`testing-qemu.md`](testing/testing-qemu.md) for telling it apart from a real refusal.
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
   2026-08-21 was deliberately coarse: **non-transitive, irrevocable short of
   task death, and in practice shell-only**; since 2026-09-06 it is one-step
   (a task may pass what it holds to its own direct children) and still
   irrevocable. (It was also *one target per
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
   actually wanted. **Update 2026-09-06:** the consumer arrived, and the
   one-step half shipped with it: the kernel records each task's
   parent, and a task may pass a right it holds to its own children and
   authorize links between them (see the review ledger's nested-shell item).
   Still open here: revocation, and passing a right to anything that is not
   your own child.

5. **Per-task ASIDs, revisited** — a pure TLB-flush-per-switch optimization
   that passed on QEMU but faulted the idle task on real Parallels and was
   reverted (see the isolation postmortem for the decoded fault evidence);
   needs a proven break-before-make sequence. Low value — a context switch
   already does far heavier work than the per-switch flush it would save.

The stack **guard page** (a guarded stack, which on the day it arrived caught
a real silent overflow in the shell's own `exec` path; the size today is the
loader's `STACK_PAGES`) and the 256KB raw
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

## The Raspberry Pi, after the first full session (asked by Hans, 2026-10-03)

The first full session on the Pi 4 ran on 2026-10-03 (`testing-pi4.md` §6):
the kernel and boot programs from the SD card, then a USB keyboard and a USB
stick that `fsd` mounted and ran `/bin` from. Hans listed six directions to
keep in the pipeline before the current tests go on. Not sequenced against
the rest of this roadmap; the dependencies between them are stated. Claims
not yet checked on this tree are marked (predicted).

- [ ] **1. More than one partition in use, on the card or a stick.** `fsd`
      already discovers MBR and GPT partitions (`partition::discover`) but
      mounts only the first one that probes as FAT32, exFAT or ext2 (`vfs.rs`).
      Wanted: the card's FAT32 boot partition and, beside it, a data
      partition (ext2, the one where `fsd` enforces permissions), each
      mountable, `mount` naming which. This is where the `/dev` namespace
      item under "Remaining follow-ups" stops being speculative. **Depends on
      item 2 for the card** (the kernel cannot read the card at all after
      the exit); on a stick it can be built and tested now, and on QEMU first
      (`make run-image-ext2` already builds a two-partition disk).
- [ ] **2. The SD card as a runtime disk, so no stick is needed.** A driver
      for the BCM2711's EMMC2 controller (SDHCI-style, the one the card sits
      on), as a third `block.rs` arm beside `virtio_blk` and `usb_msd`, taken
      over at the exit the way the xHCI is. What to settle first, each
      (predicted) until read in the firmware's tables or sources: where the
      ACPI tables describe it and whether that agrees with Linux's devicetree
      (`emmc2` at `0xfe340000` CPU side); whether PIO is enough for a first
      version, since EMMC2's DMA has its own address limits; and **the
      firmware keeps its variable store inside `RPI_EFI.fd` on this same
      card**, so the kernel must not write the card while anything could
      still write variables through the firmware (today nothing does after
      the exit; the boot counter uses the variable before it). Not testable on
      QEMU's `virt` machine (no such controller); QEMU's `raspi4b` machine
      has one but no UEFI path (see "Develop on QEMU first" in
      `testing-pi4.md`). With it, a card and nothing else is a whole system,
      and item 1 applies to the card.
- [ ] **3. Booting from a stick, with no card.** Most of it exists: on QEMU
      the kernel already boots from a USB stick behind a hub and mounts that
      same stick as its runtime disk (`test-usb-hub.py --usb-boot`, and
      `testing-pi4.md` §6, "the takeover now happens last"). What is left is
      the board's side: the Pi 4's bootloader EEPROM must try USB (its
      `BOOT_ORDER`; recent EEPROMs try SD then USB, (predicted) for this
      board until read with `rpi-eeprom-config`), and the stick must carry
      the firmware and the whole ESP, FAT32 on MBR partition 1. That is a
      card staged on a stick, which `make stick` deliberately does not do
      (it leaves out `EFI` so a stick never boots a stale kernel in a card's
      place), so this needs its own mode, say `make bootstick`, and a rule
      for which of card and stick wins when both are present. Also the Pi
      400 (item 6).
- [ ] **4. Multi-core, on QEMU first.** Today one core runs everything and
      the others are never started; the single-core argument is load-bearing
      (`synccell.rs` states it once for every mutable static, and the kernel
      never runs at EL1 with IRQs unmasked). What it needs: the secondary
      cores started through PSCI `CPU_ON` (the conduit is already found,
      `power.rs`), a stack and an exception level setup per core (the EL2
      drop too, on the Pi), the GIC's per-core interface and the timer per
      core, the cores found from the MADT's GICC entries, and then the real
      work: locking or per-core ownership wherever `SyncCell` stands today,
      and a scheduler that places tasks on cores. A plan document before any
      code, the way `roadmap-el1-drop.md` was. QEMU `-smp 4` is the loop; the
      Pi 4 (four A72s) and Parallels follow.
- [ ] **5. SSH, to reach a Pi remotely.** **Needs networking on the Pi
      first**, which it has none of: the Pi 4's on-board Ethernet is GENET,
      not virtio (see the Pi test plan note above), so either a GENET driver
      or a USB-Ethernet adapter through the existing xHCI stack (a CDC-ECM or
      CDC-NCM class driver; the adapter chip decides which). Then an SSH
      server in userland beside `netd`. Pieces that exist: Ed25519 for the
      host key, X25519 for `curve25519-sha256` key exchange and HMAC (the
      `ed25519` crate, from session auth), SHA-256 (`accounts`), users and
      passwords (`/etc/passwd`, `login`). Missing: a cipher
      (`chacha20-poly1305@openssh.com` needs ChaCha20 and Poly1305), the SSH
      transport, user-auth and connection protocols, and a channel that
      carries a shell session the way `cond` carries the console. A nearer
      step, once there is networking: the cluster's own remote execution
      (`cpu`, over 9P with signed requests) from the Mac with the host peer
      `scripts/np9p_client.py`, which already exists.
- [ ] **6. More hardware: the Pi 400 again, and whether a Pi 3 can run it.**
      **The Pi 400** is the same BCM2711 as the Pi 4: its faults of
      2026-10-01 were the stack overflow fixed in #191, so it should now
      reach the same session; its built-in keyboard is behind the same VIA
      hub. Re-boot it with the same card and stick. **A Pi 3**
      (predicted, from the hardware, not tried): it is AArch64 (Cortex-A53)
      and has UEFI firmware from the same project (`pftf/RPi3`), so the
      kernel may well boot to the exit, but almost nothing after it carries
      over. It has **no GIC** (the BCM2837 has Broadcom's own interrupt
      controllers), so there is no timer tick and no scheduling without a new
      interrupt backend beside `gicv2.rs`/`gicv3.rs`; and **no xHCI**: its
      USB host is a DWC2 (Synopsys OTG), a different and notoriously hard
      controller, which is also where its Ethernet sits (on USB). So a Pi 3
      is a serial-console port with two new drivers before it has a
      keyboard, a disk or a tick. Worth a boot to see how far it gets; not
      worth the drivers before items 2 to 5.

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

- ~~**`netd` blocked on an `fsd` reply that never came (found 2026-09-26).**~~
  **FIXED 2026-09-27 on `main` (#175).** Not a lost reply: `fsd` was busy on
  it, and the supervisor restarted a healthy `netd` because a `cpu` spawn's 98
  chunk reads keep it out of the loop that acks a ping. `cpu_spawn` now beats
  after each chunk. Full account in `docs/roadmap/roadmap-user-keys.md`, step
  4's results; the follow-ups it left (the quadratic spawn read, pings piling
  up behind a long loop's acks, a kernel-side rule) are below.

- **A login waits on `netd` (found 2026-09-27, review of user-keys step 4).**
  `login`'s `NETOP_KEY_DROP_MINE` before every prompt and its `HOLD` after a
  good password are blocking `MSG_CALL`s. A busy `netd` delays the prompt; a
  wedged one delays it until the supervisor restarts it or, past its cap,
  tears it down and the call fails. Bounded, never a hang, but local login
  should not wait on the network server at all: a timed call, or skipping the
  drop when this task never held a key.

- **`netd` reaps held keys on every wake (found 2026-09-27, review of
  user-keys step 4).** `held.reap_dead()` runs once per pass of the serve loop,
  two syscalls per held key, so under TCP or 9P load a full table adds eight
  syscalls to every wake. `HOLD` and `LIST` already reap on demand; the loop's
  reap only needs the timeout or ping path.

- **Three copies of the version-2 salt code (found 2026-09-27, review of
  user-keys step 4).** `passwd`, `useradd` and `login` each draw random bytes,
  fall back to the clock and truncate to `SALT_V2`, and each restates the
  40-byte request header. One had already drifted (`login` dropped the
  weak-salt warning; fixed). An `accounts::salt_v2_from` and a shared request
  header width in `syscall-abi` would leave one of each.

- **Files `mke2fs -d` stages on the ext2 images belong to the building host's
  uid, not root (found 2026-09-26).** `ls -l /etc/cluster` on the ext2 image
  shows owner 501 (the macOS uid that ran `make`) for `id`, `authorized`,
  `realm` and `users`, and the rest of the staged tree is the same apart from
  the two directories the Makefile `chown`s with `debugfs`. Root reads them
  through its bypass, so nothing fails today, but the modes mean "owner and
  nobody else" and the owner is a uid no dev account has; an account created
  with uid 501 would own the machine's private key. The fix is to stage as root
  (`mke2fs -d` takes ownership from the source tree, so a root-owned copy, or a
  `debugfs` pass over what was staged); its check is `ls -l /etc/cluster` and
  `/etc/shadow` showing uid 0 on a fresh image.

- **A `cpu` spawn reads its binary quadratically (found 2026-09-27).**
  `netd`'s `cpu_spawn` fetches `/bin/<cmd>` as one path-based `NP_READ` per
  512-byte chunk (98 for the 49 KB `clusterkey`), and each one re-walks `/`
  and `/bin` and then the file's FAT chain from its first cluster. With the
  images' 512-byte clusters and no FAT-sector cache in `fsd`, chunk *k* costs
  about *k* disk reads: some 4,800 for one spawn, while `fsd` shares the CPU
  half and half with the idle task. It is what made the supervisor's ping
  restart `netd` about one `cpu` run in ten (fixed the same day by a
  `heartbeat()` per chunk in `cpu_spawn`), and it is still slow. Two
  independent cures: read by fid (`NP_OPEN` once, then `NP_PREAD`), and a
  small FAT-sector cache in `fat32.rs`. Neither is needed for correctness
  now. The write side, measured 2026-10-05 with counters in `fsd`: `cp` of
  a 758 KB file made 23 reads for its first 2 KiB `write_at` and 353 at
  160 KB (the walk, plus `find_free_cluster` scanning from cluster 2 for
  each new cluster), each request still inside two ticks on QEMU. On the
  Pi, where a read is a USB transfer, it is the same count at a higher
  price.

- ~~**A server busy with a stream of requests is restarted as wedged
  (found 2026-10-05).**~~ **FIXED 2026-10-05** on `fsd/large-write`: `cp`
  of a 758 KB file had `fsd` restarted at about 170 KB (`server slot 2
  wedged - no progress (runnable)`), the copy left short, with or without
  #210's 1 MiB heap. No request was slow: counters in `fsd` showed each
  2 KiB write inside two ticks. The heartbeat samples state at the tick,
  and the tick reaches the CPU only at EL0, so a tick fired during a disk
  read is taken when `fsd` returns to EL0, `Runnable`, and 128 such ticks
  in a row restarted it. Now the `MSG_RECV` arm calls
  `supervisor::note_progress` on every call (not `MSG_TRY_RECV`, which
  `netd` polls from inside its long loops), which clears the passive
  count: a server back at its
  receive has finished its last request, and one wedged in a loop never
  gets there. Shown: the copy completes, 758,272 bytes, `cmp`-identical to
  the source from the host; a temporary mutation that spins `fsd` on a
  path holding `WEDGEME` is still restarted after the wedge time, in the
  same boot; without the change the copy is cut short. This is a partial
  answer to the open question below for the passive arm only; the ping
  arm is unchanged. Its review (`/code-review high`, seven findings) took
  `MSG_TRY_RECV` out, and left one: a single request longer than the
  wedge time is still a wedge, and `write_at`'s cost grows with the
  offset (the item above), so a large enough file, or a slow enough
  disk such as the Pi's USB stick, can still be restarted mid-write.

- **A long loop's acks let pings pile up in the mailbox (found 2026-09-27,
  in review).** A server clears `ping_outstanding` when it is seen
  `Runnable` or sends an unprompted ack (`note_ack`), while the ping itself
  is still unread in its mailbox. So a server that stays inside one long
  loop (`netd`'s `cpu_spawn`, or its multi-round-trip remote reads, both of
  which beat) is pinged again every `PING_INTERVAL`, and after about five
  seconds its 4-deep mailbox is full of pings and refuses clients
  (`MSG_ERR_FULL`) until the loop ends. Not seen in a run. Keeping the flag
  set until the ping itself is answered would close it.

- **Should the kernel, not each server, know a waiting server is alive?
  (open question, 2026-09-27).** The per-loop `heartbeat()` is opt-in: this
  is the second time a long `netd` loop was killed for being busy (the
  first was remote reads, 2026-09-03). A kernel rule was built and reviewed
  the same day and set aside: pausing a server's ping while its call chain
  ends in a running supervised server. It had a hole the opt-in does not.
  A callee that never replies but blocks briefly now and then (short calls
  to `cond`, say) resets its own heartbeat and keeps the caller's pause on,
  so neither detector fires. A kernel rule needs a bound on the pause
  before it is safe.

- **A call's reply wait accepts the partner's own request (found
  2026-09-27, latent).** `send_message` direct-delivers any message from the
  task a caller is blocked on as that caller's reply, including a new
  `MSG_CALL` request from it. Two servers calling each other at the same moment
  would turn one's request into the other's reply, and the first would then
  wait forever. No pair of SERVERS can do it today (by the send masks, `fsd`
  calls only `cond`, `cond` nobody, `netd` and `accountd` only `fsd` and
  `cond`); the first server-to-server cycle makes it possible. The shell and
  its children may message each other both ways (`TO_SPAWNABLE`, `TO_SHELL`),
  and whether two simultaneous calls there can meet has not been examined. A
  reply needs to be told apart from a request at delivery.

- **`check-site-freshness.py` prints a page name its `--update` refuses
  (found 2026-09-27).** A stale page is reported as `docs/site/<page>`, the
  manifest keys it as `site/<page>`, and `--update docs/site/<page>` answers
  "not currently reported stale". Print the key, or accept both spellings.

- **A server past its restart cap draws a second wedge line (seen
  2026-09-27).** Forcing `netd` through four ping-timeout wedges in one boot
  printed `failed more than 3 times this boot - giving up` and then
  `slot 4 wedged - no progress (runnable) - restarting` and a second
  `giving up`. Read from the code, not yet confirmed by a run: the
  heartbeat's `blocked` is "the state is `Blocked`", so a torn-down (`Unused`)
  slot counts as runnable, and the ping's `Wedged` path zeroes
  `runnable_ticks` (`reset_liveness`), which `heartbeat`'s own comment relies
  on NOT happening ("a give-up leaves it climbing past the threshold so it
  never re-fires"). After a ping-timeout give-up it climbs from zero and
  fires once more, calling `kill_task` on an `Unused` slot, which has not
  been checked for harm. The cap holds either way.

- ~~**libc's `MSG_CALL` does not ride out the delegation window (found
  2026-09-23).**~~ **FIXED 2026-09-23**: `np_request` now retries a
  `MSG_ERR_DENIED` from `netd` for the same 150 ticks as
  `ulib::net_msg_call`. Twenty boots, first `cbig` after a mount, all clean;
  the same script on the unfixed libc failed one boot in ten (three in sixteen
  across the day). The original entry: A C program's first remote op after a mount fails
  `not allowed to reach that server (capability)` about one boot in three on
  the two-node ext2 rig (`cbig` right after `mount -r`; the second `cbig` in
  the same boot always passes), measured on `main` and on a branch alike. It is
  **Cause B of the 2026-09-03 remote-read flake** (the shell `SPAWN`s, then
  `DELEGATE`s `TO_NET`, so a child reaching `netd` in between is refused
  `MSG_ERR_DENIED`), which was fixed in `ulib`'s NP paths (a bounded retry,
  `ulib/src/lib.rs` around `np_remote`) and in libc's `MSG_SEND` output path,
  but never in libc's NP call (`libc/src/file.c`, the `SYS_MSG_CALL` in the
  request builder), whose own comment names the race and returns. The fix is
  the same bounded retry; its check is the recipe above run until the first
  `cbig` has failed on the tree without it and passed ten boots with it.

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
> [`testing-qemu.md`](testing/testing-qemu.md)) is the working dev/test loop and is
> **good enough for now**. Parallels was never going to prove the cluster anyway
> — it has no working NIC transport (virtio-PCI, unsupported), so networking and
> the whole Plan 9 cluster are unreachable there (see
> [`testing-parallels.md`](testing/testing-parallels.md) for the full analysis, kept as a
> "perhaps later" reference, not an active plan). **The intended physical target
> is now 2× Raspberry Pi 4** (real ARM hardware, ordered 2026-08-26): the Plan 9
> resource-sharing mechanics are a better fit for genuine physical machines than a
> VM, so a real two-node cluster on the Pis is the eventual real-hardware proof.
> A concrete Pi test plan is now written -- [`testing-pi4.md`](testing/testing-pi4.md), 2026-08-28, ahead of the boards, with every claim labelled (predicted) or (confirmed) so the first bench session turns it into a log. Note its headline finding: **the Pi's GENET NIC is not virtio either**, so 2x Pi 4 does not by itself deliver the two-node cluster proof -- that needs USB-Ethernet over the existing xHCI stack, or a GENET driver, first. The
> `prlctl`/`make test-parallels` tooling below stays available but is no longer a
> priority.
>
> **Pi-4 bring-up reference (pre-read, for when the boards arrive):**
> `docs/research/research-redox-and-pi.md` (Part 2) maps the
> `rust-raspberrypi-OS-tutorials` repo onto our situation. The key call: **try
> the [pftf/RPi4](https://github.com/pftf/RPi4) EDK2 UEFI+ACPI firmware first** —
> a Pi 4 under it exposes UEFI + ACPI + a GOP framebuffer, so our existing boot
> path (UEFI loader, ACPI MADT → `gicv2.rs` for the Pi 4's GIC-400/GICv2, GOP
> `fbconsole`) should carry over largely unchanged, rather than rewriting for raw
> `kernel8.img` boot. The tutorials stay the fallback reference for the raw
> BCM2711 facts (peripheral base `0xFE00_0000`, GIC-400 at GICD `0xFF84_1000`/
> GICC `0xFF84_2000`, PL011-not-mini-UART, GPIO14/15 = ALT0, the serial rig:
> USB-serial to TX/RX/GND, **not** VCC). See [[project-physical-hardware-target]].
>
> **Pi 400 desk check (2026-09-27), three findings made without a board**,
> written up in [`testing-pi4.md`](testing/testing-pi4.md) §1b, checkpoint 4 and
> Risk 6:
>
> - [ ] **BLOCKS ALL USB ON THE PI: map xHCI DMA memory non-cacheable.**
>       *Built 2026-09-27 (branch `pi400/noncacheable-dma`); open until USB
>       works on the board.* All xHCI/USB DMA memory in one page-aligned pool,
>       mapped Normal Non-cacheable by `mmu.rs`, checked at every boot through
>       the hardware walker in every view, every page and the neighbours on
>       either side (mutations that plan nothing, break either table path,
>       bleed past a range or reach kernel data are caught). Only where the
>       firmware's ACPI declares DMA non-coherent (`_CCA 0`, found by a byte
>       scan of the DSDT/SSDTs): Parallels emulates xHCI and a guest-side
>       non-cacheable mapping could disagree with the host's cacheable one.
>       QEMU cannot show the bug itself.
>       Why: the BCM2711's PCIe DMA is not cache-coherent (`testing-pi4.md`
>       Risk 8: the pftf firmware's ACPI `_CCA 0` for the PCIe root, its
>       `NonCoherentDmaLib`, no `dma-coherent` in Linux's devicetree), and
>       every ring, context and buffer in `xhci.rs` and `usb_msd.rs` was
>       Normal write-back cacheable with no cache maintenance. QEMU and
>       Parallels are coherent, so nothing had shown it. It needed a way in
>       `mmu.rs` to map chosen physical ranges Normal Non-cacheable at 4 KB
>       granularity, done before the first boot. (The framebuffer item below,
>       Risk 7, was first planned on the same mechanism and ended up fixed by
>       cleaning each write instead.)
>
> - [x] **xHCI hub support.** *Built on QEMU; open until the Pi's keyboard
>       comes up. Closed 2026-10-03: on the Pi 4 the VIA hub on root port 1
>       was configured and a Full Speed keyboard behind it (the transaction
>       translator's first test) typed a whole session, beside a High Speed
>       stick on the same hub (`testing-pi4.md` §6).* Every USB 2.0 device on a BCM2711 board, the Pi
>       400's built-in keyboard included, sits behind the on-board VIA hub, and
>       `xhci.rs` reaches only root-port devices: **no keyboard on the Pi at
>       all** until this lands. Buildable on QEMU with `usb-hub`; the
>       transaction-translator fields (a full-speed keyboard below a
>       high-speed hub) get their first test on the board. The first new
>       driver work the Pi needs, ahead of any NIC. **Built 2026-09-27**
>       (branch `pi400/usb-hub`): `make test-usb-hub` passes both boots (a
>       keyboard and a stick behind a hub; and a boot FROM a stick behind the
>       hub, mounted through it), which failed before the driver work (3 of 4,
>       and 4 of 5), and fail again with the route string forced to 0. Also
>       in: the Full-speed EP0 packet-size correction, 64-byte bulk
>       packets at Full speed, a Full-speed interrupt Interval rounded down
>       and clamped to 3-10 (it rounded up, unclamped), Max ESIT Payload
>       set, each device's own configuration value, and a failed setup
>       that releases its slot (Disable Slot), so `mount -a` can retry a
>       device on a root port. Left for the board: the translator fields.
>
>       Follow-ups from the reviews of that branch, not done there:
>       - [x] **EP0 recovers only from a Stall.** After a timeout or any
>             other transfer error, `control_transfer` leaves EP0 halted or
>             with TRBs outstanding, and a late completion can be taken for
>             the next request's. The hub path makes many control transfers
>             and promises that one bad port costs only that port, so it
>             needs the standard recovery (Stop Endpoint, then Set TR
>             Dequeue) on every failure. **Done 2026-09-27 (branch
>             `pi400/fb-and-ep0`):** `recover_ep0` runs after every failed
>             control transfer: Reset Endpoint if Halted, Stop Endpoint if
>             Running, then Set TR Dequeue to the *current* enqueue position.
>             Checked by leaving one hub `GET_STATUS` unrung (a real timeout
>             with its TRBs queued): recovery ran, the rig passed, and no
>             stray completion was left; the kernel before it left one
>             (`unexpected event type=32`), which on real hardware pairs
>             each later request with the previous one's answer. QEMU hides
>             that, since it completes a ring as soon as it is rung. A
>             forced Stall recovers too (endpoint state 2, Reset Endpoint).
>       - [x] **The storage endpoint's recovery rewinds to the ring's
>             start.** `reset_storage_endpoint` sets the dequeue pointer to
>             the ring's start with DCS=1. The slots after it still hold
>             earlier TRBs carrying that cycle bit, so the controller can
>             run on into a stale TRB once the new ones are done: an old
>             Normal TRB, and a DMA into an old buffer. The EP0 version had
>             the same flaw and now dequeues at the current enqueue
>             position instead; the storage one is the path confirmed on
>             Parallels ("Mode A"), so it is left for its own change and
>             check. Found reading the code, not observed. *Fixed 2026-10-01
>             (#184, merged 2026-10-01): it dequeues at the current
>             enqueue position with the current cycle, as EP0 does. Observed
>             on QEMU by corrupting every seventh CBW's signature, which
>             `usb-storage` answers with a Stall, under `test-usb-hub.py
>             --usb-boot`: the old code timed out 12 transfers after its
>             rewinds and failed the typed check; the fix recovered 30
>             Stalls with no timeout and passed. `make test-usb-hub` passes
>             without the injection.*
      - [ ] **Re-confirm the storage recovery on the board.** The fix above
>             replaces the path confirmed on Parallels ("Mode A") and has
>             been observed only on QEMU. Recovery lines
>             (`usb-msd: ... resetting bulk endpoints`) during the Pi's stick
>             reads, followed by reads that succeed, confirm it; repeated
>             retries that all fail point at the next item first. From the
>             review of #184.
>       - [x] **Bulk recovery never clears the halt on the device.** It
>             resets the host side only (Reset Endpoint, Set TR Dequeue) and
>             never sends CLEAR_FEATURE(ENDPOINT_HALT), which the BOT spec's
>             reset recovery includes. A real stick that stalled keeps its
>             own endpoint halted or its data toggle out of step, so every
>             retry can stall again. QEMU does not need it, which is why the
>             injected Stalls recovered. A standard request, not a class one,
>             so Parallels' passthrough limit does not rule it out. For the
>             Pi's stick. From the same review. *Done 2026-10-01 (#186, merged 2026-10-01): `reset_storage_endpoint` sends it after
>             a Reset Endpoint that took (the endpoint was halted), and only
>             then, since on a healthy endpoint it would reset the device's
>             data toggle and not the host's. Best-effort. QEMU checks only
>             that it is not refused (`--stall` fails on a class-type
>             request, 60 refusals): the test passes with the request
>             removed, and with it sent to the wrong endpoint, since QEMU
>             accepts any endpoint address. Open on a real stick with the
>             board item above.*
>       - [ ] **A storage transfer timeout is still not recovered.** With
>             the endpoint Running, Reset Endpoint is refused and the timed
>             out TRB stays queued ahead of the retry's, so the retry can
>             take the wrong packet. `recover_ep0`'s state dispatch (Halted:
>             Reset Endpoint; Running: Stop Endpoint; then Set TR Dequeue at
>             the enqueue position) is the shape. From the same review.
>       - [x] **The storage Stall test is not committed.** The injection
>             that observed the fix (every seventh CBW's signature
>             corrupted) was temporary, so no committed test runs
>             `reset_storage_endpoint` at all, and going back to the
>             rewind would pass `make test-usb-hub`. A test-only boot flag
>             that corrupts CBWs, and a `test-usb-hub.py` mode that sets it
>             and requires recovery lines and the typed check, would make it
>             a check that can fail. From the same review. *Done 2026-10-01
>             (#185, merged 2026-10-01): the `\MSDSTALL` boot flag,
>             honoured only for a stick whose INQUIRY vendor is `QEMU`, `make
>             image-stall`, and `test-usb-hub.py --stall`, run third by `make
>             test-usb-hub`. It requires the fault armed, the Stalls by
>             QEMU's own count, and exactly one first retry per Stall with no
>             second retry and no giving up (every recovery is logged while
>             the fault is armed). Passes with 30 Stalls and 30 first
>             retries. Fails with the rewind from before #184 put back (19
>             Stalls, 56 first retries and 18 further, `typed` too), with
>             the corruption disabled (no Stalls), and with the vendor test
>             pointed elsewhere (not armed, no Stalls).*
>       - [ ] **The Stall test never stalls a retry.** `\MSDSTALL`
>             corrupts a CBW whose tag is 3 mod 7, and every attempt takes a
>             tag, so an injected Stall's own retry (4 mod 7) is never
>             corrupted: a second Stall in a row on one command and running
>             out of attempts are not exercised, and a recovery that works
>             once but leaves the endpoint wrong for the next one passes.
>             (A retry after some other failure can land on 3 mod 7; the
>             fault does not single out first attempts.) Corrupt some
>             retries on purpose, and grade the second and third attempts.
>             From the review of #185.
>       - [x] **A Bulk-IN Stall in the data or status phase is not
>             recovered.** `bot_command` answers every transfer failure the
>             same way: reset both host endpoints, send a fresh CBW. After
>             a Bulk-IN Stall the device still owes its CSW (BOT 6.7.2 and
>             6.7.3: clear the halt, then read the CSW), so each new CBW is
>             stalled in turn and the command gives up. Reproduced on QEMU
>             by the max review of #185, stalling one CSW read (an IN
>             shorter than 13 bytes): READ CAPACITY gave up after three
>             attempts, `usb-msd init failed`, nothing mounted. A real stick
>             that stalls a failed READ(10) is in the same state. The fix is
>             recovery by phase: after a data- or status-phase Stall, clear
>             the halt and read the CSW before any new CBW. Ahead of the
>             board's storage, and the next branch after #185. *Done 2026-10-01 (#186, merged 2026-10-01): a stalled data stage clears that
>             endpoint and reads the CSW, and the command then fails with
>             the CSW's status, or `ShortData` when that says success (not
>             retried, see below); a stalled CSW read clears the IN halt and
>             reads it once more. Stall only, as Linux's usb-storage does:
>             after a Babble or a Transaction Error the CSW may already be
>             lost, and the full reset handles that. The status phase is observed: `\MSDSTALL` reads the CSW
>             of every command with tag 5 mod 7 as 12 bytes, QEMU stalls it
>             with the CSW owed, and `--stall` recovers 30 in place; with
>             the CSW recovery disabled, or the IN endpoint reset removed,
>             the boot does not mount. The data phase is not observed: QEMU
>             pads a short data phase rather than stalling it.*
>       - [ ] **`--stall` exercised the Bulk-OUT recovery only.** The fault
>             corrupted CBWs, so only Bulk-OUT halted, and deleting the IN
>             recovery outright still passed (the max review of #185).
>             *Mostly done 2026-10-01 (#186, merged 2026-10-01): the
>             short CSW read halts Bulk-IN, and the same deletion now fails.*
>             Still open: the capped recovery log the board uses is run by
>             no test, since arming the fault lifts the cap.
>       - [ ] **A short data stage is an error, not partial data.** When
>             the data stage stalls and the CSW then reports success with a
>             residue (BOT 6.7.2, the host asking more than the device
>             has), `bot_command_once` returns `ShortData` and hands no
>             data back, since a sector read with a residue is not a
>             sector. A command that may legally come back short (INQUIRY,
>             MODE SENSE, REQUEST SENSE) would need the bytes that did
>             arrive: per-command rules, and the transfer event's own
>             residual length, which `bulk_transfer` does not report. From
>             the review of #186.
      - [ ] **An invalid CSW is not recovered.** A CSW whose signature or
>             tag is wrong returns `CswMismatch`, which `bot_command` does
>             not retry, so the read fails up to `fsd` as a disk I/O error;
>             BOT 6.7 calls for reset recovery. Seen while looking for a way
>             to stall the data phase on QEMU (a CBW promising more data
>             than the command returns makes QEMU pad, and the CSW read
>             then gets padding). Reset recovery's class request is not
>             forwarded by Parallels, so the shape needs thought.
>       - [x] **Make the Pi's in-RAM framebuffer visible to the display** (the Pi's
>             HDMI console, `testing-pi4.md` Risk 7). The mapping is
>             confirmed cacheable on QEMU `ramfb`; the stale-text symptom is
>             predicted for the Pi. Fix in `mmu.rs` at 4 KB granularity at
>             its edges, merged with the per-task EL0 splits, or clean the
>             written range to the point of coherency after each write.
>             Same `mmu.rs` mechanism as the xHCI DMA item at the top.
>             **Done (2026-09-27), by cleaning:** the framebuffer stays
>             cacheable and every write in `fbdev.rs`/`fbconsole.rs` is
>             cleaned out to memory (`mmu::clean_to_poc`). A non-cacheable
>             mapping was built first and replaced after review: a hypervisor
>             host reading guest memory cacheable could show it stale, and
>             every scroll read 8 MB of uncached memory. Nothing becomes
>             non-cacheable implicitly; only the ranges `main.rs` passes.
>       - [ ] **A kernel log buffer and `dmesg`.** Every kernel boot line,
>             the non-cacheable mapping's self-check included, reaches a
>             framebuffer-only screen only until the console server clears
>             it, and the kernel keeps nothing to read back. On the Pi,
>             watched over HDMI alone, the lines `testing-pi4.md` says to
>             read are gone within seconds; a ring buffer of kernel output
>             and a command to print it would keep them.
>       - [ ] **CLEAR_TT_BUFFER after a halted transfer behind a
>             High-speed hub.** When EP0 (or a bulk endpoint) of a Full- or
>             Low-speed device behind a High-speed hub halts, the hub's
>             transaction translator can keep the failed split transaction
>             buffered; Linux sends the hub CLEAR_TT_BUFFER (USB 2.0
>             11.24.2.3). The Pi 400's keyboard is exactly that case. Not
>             written: QEMU models no translator, so the code would run in no
>             check we have. For the bench session.
>       - [ ] **Full-speed bulk packet size is assumed to be 64.** Full-speed
>             bulk endpoints may be 8, 16, 32 or 64; read `wMaxPacketSize`
>             from the endpoint descriptor, for every speed.
>       - [ ] **`mount -a` does not look behind hubs.** A device behind a
>             hub that fails at boot, or is plugged in after it, is not
>             found by the rescan, which walks root ports only.
>       - [x] **A keystroke during a controller command kills the USB
>             keyboard.** `wait_command_completion` drops any transfer event
>             it sees, where `wait_transfer_event` routes the keyboard's to
>             `process_keyboard_report`. A dropped report is never reposted,
>             so the interrupt endpoint stops and the keyboard is dead for the
>             boot. Reached by `mount -a` typed on the USB keyboard: the
>             Enter key-up arrives during the rescan's Enable Slot (seen on
>             QEMU 2026-09-27 as `unexpected event type=32 while waiting for
>             command completion`, after which the keyboard typed nothing).
>             Already on `main`, not introduced by the hub work. **Fixed on
>             the same branch:** both waits route the keyboard's reports
>             through one helper, `route_keyboard_event`; checked by typing
>             `mount -a` twice on the USB keyboard, which ran only once
>             before the fix and twice after.
>       - [ ] **`mount -a` retries a broken root-port device every time.**
>             Releasing a failed device's slot is what lets `mount -a` retry
>             a stick that was merely slow at boot; the price is that a device
>             that always fails is set up again on every `mount -a`, about a
>             second of timeouts each. Accepted for now (`mount -a` is typed
>             by a person); a per-port retry limit is the fix if it grates.
>       - [ ] Tidying: one helper for the input-context command TRB (five
>             hand-written sites), and a named struct for `Scan`'s storage
>             tuple (the keyboard's became `KeyboardEndpoint`).
> - [x] **Open the xHCI's own `PciIo` exclusively, not the whole root
>       bridge.** The root-bridge open was refused (`ACCESS_DENIED`) when any
>       firmware driver on the bus would not stop. On QEMU that had been true
>       since `virtio-rng` joined every disk target on 2026-08-29: with the RNG
>       and the default NIC, `make run-usb-kbd` found no controller. Found
>       by running it, one variable at a time; fixed on branch
>       `pi400/bar-translation` and checked on QEMU with both present.
>       Parallels not rechecked.
> - [x] **The xHCI BAR is a bus address; use firmware's CPU address.** On
>       the Pi the CPU sees it at `0x6_0000_0000`. Branch
>       `pi400/bar-translation` takes it from `PciIo.GetBarAttributes` and
>       refuses the controller unless it reproduces the BAR (a refusal checked
>       on QEMU by mutation). Open until the board shows a nonzero
>       translation working: QEMU's is 0.
>       *2026-10-03: the no-flag Pi 4 boot used translation
>       `0xfffffffaf8000000` (BAR `0xf8000000` to CPU `0x600000000`), and
>       the controller reset and read `max_slots=32 max_ports=5` there, so
>       a nonzero translation works on the board. Merged as #176; this
>       was the check it waited on.*
>
> **First boots on the boards (2026-09-28), over HDMI alone**, a Pi 400 and a
> 1 GB Pi 4, with no serial cable yet. Written up as a log in
> [`testing-pi4.md`](testing/testing-pi4.md) §6 ("Bisecting a hang with boot
> flag files" and what follows it):
>
> - [x] **The Pi 400's xHCI takeover damages memory before the exit.** The
>       firmware's own handler reported `Synchronous Exception at …` at a
>       point that moved between boots; placed once (`scripts/efi-symbol.py`)
>       at a Relaxed `atomic_load` in `log` through a pointer the debug build
>       had spilled to the stack four instructions earlier, a pointer that
>       cannot be wrong unless memory changed under it. With `\NOXHCI` the
>       boot gets through `exiting boot services`. Two suspects, both
>       Pi 400-shaped: its keyboard is always attached, so the controller is
>       always doing DMA for the firmware's driver; and its VL805 has no
>       firmware EEPROM (loaded by the bootloader, and reloaded by Linux after
>       a reset). The serial dump (ESR, FAR, a backtrace) decides. *Update
>       2026-10-01: the Pi 4's first serial boot faulted in the same step,
>       at a firmware address (`0x39F2D1A0`, outside the image, before the
>       command-register line), and the firmware printed its one line and
>       no dump: a RELEASE build compiles the register dump out, and the
>       pftf zip ships only RELEASE. So the dump is now the kernel's: the
>       next item.* *Revised 2026-10-02: a `NOXHCI` boot died at the same
>       firmware instruction (`DxeCore + 0xae14`, `0x39F36E14`) INSIDE
>       `ExitBootServices`, with the controller untouched: a write to
>       `0x38670810`, a RAM page the firmware's own tables do not map
>       (translation fault at level 3), one of 0x1100 bytes from
>       `0x38660708`; the next boot of the same card went through. So the
>       suspect is no longer the takeover's DMA but a DxeCore routine
>       reached from both the PCI protocol and the exit, writing where its
>       tables have a hole, intermittently. The dump is in `testing-pi4.md`
>       section 6; a second one at the same address, to compare `far` and
>       the registers, is the next data point. The pftf `RPi4.dsc` sets no
>       heap guard, so the hole is not EDK2's freed-memory guard.* *Third
>       sighting the same afternoon, one boot in four: a firmware write
>       into the GOP framebuffer at `0x3e98fe00` (the console drawing a
>       glyph row), a level-2 translation fault, before the exit and before
>       the boot-identity line. Three faults, three addresses, one shape:
>       the firmware's tables with an invalid entry where a valid one was.
>       The next instrument is the reporter walking the firmware's live
>       tables for `far` and printing each level's entry, zero or garbage;
>       `testing-pi4.md` section 6 has the reading and the suspects.*
>       ***Answered the same evening (#190's walk, first boot): the
>       firmware's root table reads as stack frames. The Pi firmware's
>       stack is 16 KB (`PcdCPUCorePrimaryStackSize 0x4000`) at the top of
>       RAM with its page tables directly below, and the kernel, running on
>       it, overflows into them; the TLB hides it until a cold page is
>       walked. Every firmware fault since 2026-10-01 is this. The fix is a
>       kernel-owned stack from the first instruction: the next item.***
>       *Closed 2026-10-03: four `NOXHCI` boots of the fix (#191), four
>       shells, no fault; the card before it faulted once more with the
>       identical table garbage. The takeover itself is still to be booted
>       with the fix (step 4 of the drop plan), but its fault was this.*
> - [x] **fix** **The kernel runs on the firmware's stack, which on the Pi
>       is 16 KB with the page tables beneath it.** Switch to a stack the
>       kernel owns (a static in the image, 256 KB) as the entry's first
>       act, before any call; keep it through the exit and the drop
>       (`SP_EL1` is set from `SP`). `earlyfault::arm`'s stack bound then
>       comes from the kernel's own stack. Check: the rigs on QEMU
>       unchanged, and `NOXHCI` boots on the Pi 4 that no longer fault in
>       firmware code, with the walk's line never again showing garbage in
>       the tables; a mutation (the switch disabled) must bring the fault
>       back on the board. *Built 2026-10-02 on `pi4/own-stack`: the
>       switch as the entry's first act, the banner line naming the stack
>       (and saying `the FIRMWARE'S stack` with the switch disabled, which
>       the rig's control then fails on), the frame walk crossing once into
>       the firmware's frames. QEMU: both rigs and the image's spawns pass.
>       Merged as #191; on the board 2026-10-03, four `NOXHCI` boots, four
>       shells, no fault, where the day before one boot in two to four
>       faulted in firmware code. Done.*
> - [x] **fix** **The xHCI DMA pool has 8 scratchpad pages; the Pi's VL805
>       wants 31.** The no-flag boot of 2026-10-03 took the controller and
>       crossed the exit and the drop with no fault, then `xhci.rs`
>       declined it: `controller wants 31 scratchpad buffers, only 8 are
>       supported` (`HCSPARAMS2` `0xfc000031`). Raise
>       `MAX_SCRATCHPAD_BUFFERS` to 32 (128 KB of the pool, still mapped
>       non-cacheable with the rest). QEMU cannot check the number, since
>       its controller asks for fewer than 8; the check is the board: the
>       driver goes past `DCBAAP` and the port scan reports the VL805's
>       ports. `testing-pi4.md` §6. *Built 2026-10-03 on
>       `pi4/scratchpad-32`: 32 pages; the array moved to the pool's end
>       and the ERST into its slot (the layout assertions refused the first
>       order); the count logged on the controller line; a controller whose
>       `PAGESIZE` lacks 4 KB refused rather than handed 4 KB buffers. QEMU:
>       `test-usb-hub` and `test-el1-drop` pass. Merged as #192; on the
>       board 2026-10-03, two boots: `scratchpads=31`, the controller
>       accepted, port 1 connected and reset (High Speed). Done; the first
>       command then timed out, the next item.*
> - [x] **fix** **The first xHCI command times out on the Pi 4.** Two
>       boots of #192: `port 1 setup failed (command ring: timed out waiting
>       for a completion event)`, while the non-coherent pool's self-check
>       passed. The driver differed from every one that works on the VL805
>       (edk2 `XhciDxe`, Linux, U-Boot) in two places: it wrote the 64-bit
>       registers (DCBAAP, CRCR, ERSTBA, ERDP) as one 64-bit store across
>       the PCIe bridge, and it put no barrier between the ring setup in
>       memory and the register writes that hand it over (the next item).
>       One variable per board round, so this one is the store. *Built
>       2026-10-03 on `pi4/xhci-mmio-order`: `write64` as low then high
>       32-bit halves; passively, `USBSTS.HSE` logged right after Run, and
>       a once-per-boot dump on a command timeout (USBSTS, CRCR.CRR, IMAN,
>       the register readbacks against the pool's addresses, the event
>       ring's slot, ERST[0], the command that timed out). A probe of one
>       64-bit store was built and taken out at review: it was the suspect
>       access itself, and could damage CONFIG beside DCBAAP. QEMU:
>       `test-usb-hub` and `test-el1-drop` pass, and the dump read
>       correctly with the command doorbell removed. Merged as #193; on
>       the board 2026-10-03: slot 1 enabled and addressed, the VIA hub on
>       port 1 (`2109:3431`, 4 ports) configured, and a boot-protocol
>       keyboard on port 1.4 brought to `keyboard ready`, the first USB
>       device on this board. So the 64-bit store was the cause. Done.*
> - [ ] **new** **A Pi boot from a card and a stick of different builds.**
>       Since `make stick` (2026-10-03) a Pi boot has two staged halves: the
>       card holds the kernel and the boot servers, the stick `/bin`,
>       `/etc` and `/man`. Nothing checks they came from the same build, so
>       a card restaged after an ABI or protocol change runs old programs
>       from the stick, which looks like a kernel bug. Stamp a build
>       identity (a hash of `build/esp`) on both and compare: at boot, once
>       the stick mounts, and in the log line that says which build is
>       running, which a boot also still lacks (owed since the wrong-tree
>       card of 2026-10-01). Until then, restage both together. Found by
>       the review of `pi4/stick-target`. *The kernel's half built
>       2026-10-03 on `pi4/build-identity`: `kernel/build.rs` bakes in the
>       commit (12 digits), `+dirty` and the profile, and the boot logs it
>       in its first line and in whichever line announces the console after
>       the exit. The script reruns every build (the kernel crate then
>       recompiles, about a second): the usual `rerun-if-changed=build.rs`
>       was shown to keep the old commit after a commit that changed no
>       source. The control boot of `test-early-fault`/`test-el1-drop`
>       requires both lines to name the staged image's identity and that
>       identity to be HEAD's commit, and fails on a stale ESP (shown). Open: the
>       stick's stamp and the comparison once it mounts.*
> - [x] **fix** **A USB3 stick on the Pi 4's SuperSpeed root port comes out
>       of its reset with speed 0.** Same boot: `port 3 reset, speed=0`,
>       `unsupported port speed 0`, at boot and again on `mount -a`'s
>       rescan, so no disk. Nothing in the log said why. *Built
>       2026-10-03 on `pi4/usb3-port-state`: a port in SS.Inactive or
>       Compliance before its reset is Warm Reset at once (Linux's rule);
>       a port not enabled after its reset gets PORTSC before and after,
>       decoded (CCS, PED, PLS, speed), a watch of up to a second for it
>       to enable (each change logged, at most 16; stops on disconnect),
>       then, from those two states, a Warm Reset and the same watch, and
>       is refused (`root port not enabled after reset`, with PORTSC)
>       rather than addressed if none of that worked. Each step logs its
>       outcome. QEMU cannot produce the case; forced onto every port the
>       lines decode and every device still enumerates, and with no port
>       counted as enabled every port is refused and the rig fails. Merged
>       as #194. The first board boot after it (2026-10-03) had the stick,
>       a Lexar, on hub port 1.2 at High Speed, the USB 2 path, so it never
>       reached a SuperSpeed root port and this path did not run. Closed
>       the same day: the stick on SuperSpeed root port 2 was enabled at
>       speed 4 before the reset, the Hot Reset left it in Polling (`PLS
>       8`, speed 0) when PRC was set, and the watch saw it back in U0 at
>       speed 4 within the second, then storage came up and mounted. The
>       old code read the speed at PRC; the wait was the fix, no Warm Reset
>       needed.*
> - [x] **fix** **No barrier between the xHCI ring setup and the registers
>       that hand it over.** The DCBAA, the scratchpad array, the command
>       ring's Link TRB and the ERST are stores to the DMA pool; DCBAAP,
>       CRCR, ERSTBA and Run are Device stores, and nothing orders the two
>       kinds without a barrier. The controller fetches the ERST when
>       ERSTBA is written and reads the scratchpad array at Run. Linux's
>       `writel` puts a barrier before every MMIO write; QEMU has no write
>       buffer to show the miss. A `dsb` before the register block (and
>       before Run). Held back from the 64-bit-store round so the board
>       answers one question; take it after that round whatever it shows.
>       *(That round answered: the split write alone brought the
>       controller up. This stays a correctness fix, not a board fix.)*
>       *Built 2026-10-03 on `pi4/xhci-handover-barrier`: a `dsb sy`
>       before DCBAAP, CRCR, the ERST block and Run in `init_inner`. No
>       QEMU boot can tell it from the old code; the check is the image,
>       four more `dsb sy` in debug and release alike. Merged as #196,
>       with its review's three findings on the roadmap below. Done; on the
>       board 2026-10-03 with #197, a full session, no fault (`testing-pi4.md`
>       section 6).*
> - [x] **fix** **A TRB's cycle bit is not ordered after its other words.**
>       `ring_push` writes all four dwords of a TRB in one `write_volatile`,
>       cycle bit included, with no barrier before dword 3. On the Pi the
>       pool is non-cacheable and the controller may re-read the dequeue
>       TRB without a new doorbell, so it can see a fresh cycle bit over a
>       stale buffer pointer or length. Write dwords 0 to 2, a write
>       barrier, then dword 3 (Linux's `queue_trb` does `wmb()` before
>       `field[3]`). The write-side twin of `event_ring_pop`'s read order.
>       Found by the review of `pi4/xhci-handover-barrier`; take it next.
>       *Built 2026-10-03 on `pi4/trb-cycle-order`, wider than asked: the
>       review of the first build found the same gap ACROSS the TRBs of one
>       transfer (a control request's Setup could be taken before its Data
>       and Status TRBs were written). `ring_publish` now writes a batch of
>       TRBs (one TD, or a control transfer's three stage TDs together, as
>       Linux's `xhci_queue_ctrl_tx` does) the way `giveback_first_trb`
>       publishes: every word written with the first TRB's cycle bit still
>       software's, then `dmb oshst`, then that bit flipped. `ring_push` is
>       a batch of one, and `control_transfer` publishes its TRBs together.
>       It is the one way a TRB reaches a ring the controller may be
>       reading; ring setup (zeroing, the Link TRB) is the only other TRB
>       store, made before the ring is handed over. The Link TRB's wrap
>       writes its cycle word only and needs no barrier of its own: the
>       controller stops at the held first TRB. A second review found no
>       correctness bug in the path and seven smaller things, taken: the
>       addresses returned by value in a `[u64; MAX_PUBLISH]` rather than
>       through a caller's slice (two slices of unequal length could panic
>       halfway through a held batch), the count asserted before anything
>       is written, the stage-TD wording, the history note, a stale doc
>       line, and `INT_RING` zeroed (the item below). Checked on the final
>       code: `test-usb-hub` (19 ok) and `test-el1-drop` green; with the
>       flip removed the rig fails (17 FAIL), the Enable Slot command left
>       in the ring with its cycle bit 0 and timed out, so the held TRB is
>       not the controller's until the flip; in the release image all 19
>       `dmb oshst` have the flip at `+0xc` as the first store after them.
>       Merged as #197. Done; on the board 2026-10-03, a full session over
>       the USB keyboard and a SuperSpeed stick, no fault (`testing-pi4.md`
>       section 6).*
> - [x] **fix** **`INT_RING` is not zeroed when the keyboard is set up.**
>       `activate_keyboard` writes only the ring's Link TRB, where the EP0
>       and bulk ring setups zero the whole ring first. A second activation
>       would start at enqueue 0 with cycle 1 over TRBs left from the first,
>       and any of them carrying cycle 1 would look owned to the
>       controller. Latent: `activate_keyboard` runs once per boot today.
>       Zero it like the others. Found by the review of
>       `pi4/trb-cycle-order`. *Done on the same branch, after its second
>       review pointed out that `ring_publish`'s contract says ring setup
>       zeroes the ring: one line, no change on today's single activation.*
> - [x] **fix** **No committed check guards the xHCI barriers.** Every QEMU
>       rig passes with or without the `dmb oshst` in `ring_publish` and the
>       `dsb sy` before the register writes, and the image checks (19
>       barriers, the flip the first store after each; `dsb sy` 18 to 22)
>       were run by hand. A refactor that drops a barrier, or puts a store
>       between it and the flip, stays green everywhere. A scripted
>       disassembly check over the built image (the PE carries no symbols,
>       so it matches the instruction pattern, not a function), shown to
>       fail by deleting one barrier. Found by the second review of
>       `pi4/trb-cycle-order`. *Built 2026-10-03 on
>       `pi4/xhci-write-barrier`, with the item below:
>       `scripts/check-xhci-barriers.py`, run by `make test` and on its own
>       as `make check-xhci-barriers`. A pattern over compiled Rust could not
>       work: in the debug build `make sdcard` stages, the flip after
>       `dmb oshst` is a call into `write_volatile`, not an adjacent store
>       (the earlier hand checks were all on release), and other modules'
>       `dsb sy` look like the xHCI one. So each barrier and its store became
>       a naked function of fixed instructions (`mmio_write32`: `dsb sy;
>       str w1, [x0]; ret`; `mmio_write64`, the same with both halves after
>       one barrier; `publish_cycle_word`: `dmb oshst; str w1, [x0]; ret`),
>       and the script requires each exactly once in the image and called
>       (`bl`, or `b` for a tail call) at least once. Shown to fail three ways (the `dsb`
>       deleted, a `nop` before the store, `write32` routed around the
>       function), and `make test` itself red with the `dsb` deleted. Its
>       limit: a new raw `write_volatile` to a register elsewhere in
>       `xhci.rs` would pass; `write32` being the only register writer is
>       what that rests on. Merged as #198. Done.*
> - [x] **fix** **The barrier before an xHCI register write is placed by
>       hand.** Every doorbell and, since `pi4/xhci-handover-barrier`, the
>       four handover writes in `init_inner` carry their own `dsb sy`; the
>       next register that hands the controller memory can be written
>       without one, and QEMU will not show it. Put the barrier in the
>       write path (a `write32`/`write64` that orders, as Linux's `writel`
>       does, or a required handover variant), so the barrier-free form
>       cannot be spelled. Settle the one odd site with it: `event_ring_pop`
>       writes ERDP after `dmb sy`, every other site `dsb sy`, and no
>       comment says why. Then the redundant barrier before Run goes, and
>       the manual `dsb` count stops being the only check. Found by the
>       same review. *Built 2026-10-03 on `pi4/xhci-write-barrier`: every
>       register write goes through `mmio_write32` or `mmio_write64` (`dsb
>       sy` then the store, one barrier for both halves of a 64-bit
>       register), so a register cannot be written without the barrier; the
>       nine hand-placed barriers are gone, the one before Run with them;
>       ERDP's `dmb sy` became the same `dsb sy`, which also completes the
>       event's loads before the controller is told the slot is free.
>       `dsb sy`, not Linux's lighter `dmb oshst`, because it is the barrier
>       the Pi 4 has run every doorbell behind; a register write is now a
>       call and a `dsb` more than before, on every write. QEMU:
>       `test-usb-hub` (19 ok) and `test-el1-drop` green. Merged as #198.
>       Done; on the board 2026-10-03 (`build 08d3b4e27601 debug`), a full
>       session, no fault (`testing-pi4.md` section 6).*
> - [x] **fix** **ERDP is programmed after ERSTBA.** `init_inner` writes
>       ERSTSZ, ERSTBA, ERDP; the xHCI spec's initialization order (4.2)
>       is ERSTSZ, ERDP, ERSTBA, so a controller that starts on the
>       segment at the ERSTBA write could sample a stale dequeue pointer.
>       Check edk2's and Linux's order before changing it, and change it in
>       a round of its own: it changes what the board sees. Found by the
>       same review. *Checked 2026-10-03 in the sources: edk2's
>       `XhcInitSched` writes ERSTSZ, ERDP, ERSTBA (each commented with its
>       spec section); U-Boot's `xhci_mem_init` ERDP, ERSTSZ, ERSTBA;
>       Linux's `xhci_add_interrupter` ERSTSZ, ERSTBA, ERDP, this driver's
>       old order (Linux also carries `XHCI_WRITE_64_HI_LO` for controllers
>       that act on ERSTBA's low-half write, which is the ERSTBA write as a
>       trigger). Built on `pi4/erdp-before-erstba`: ERSTSZ, ERDP, ERSTBA,
>       edk2's order, since edk2 drives this VL805 before the kernel.
>       Hardening, not a fix for anything seen: the board works in Linux's
>       order. The spec PDF itself was not read; its order is taken from
>       edk2's comments. QEMU: `test-usb-hub` (19 ok) and `test-el1-drop`
>       green. Merged as #200. Done; on the board 2026-10-04 (`build
>       d89908f1d0ab debug`, #200's kernel), two boots and a full session
>       over the serial console, no fault or timeout (`testing-pi4.md`
>       section 6).*
> - [x] **fix** **ERSTSZ and ERSTBA are written bare, zeroing their RsvdP
>       bits.** `init_inner` writes ERSTSZ as `1` and ERSTBA as the address,
>       where the spec's RsvdP fields (ERSTSZ 31:16, ERSTBA 5:0) are to be
>       preserved: Linux (`xhci_add_interrupter`) and U-Boot read, modify
>       and write both. edk2 writes them bare too, on this very controller,
>       so the Pi shows no harm; spec-correctness, not a fix for anything
>       seen. Read-modify-write both (ERDP's DESI/EHB bits are another
>       question: EHB is RW1C and is written 0 deliberately). Its own
>       change, kept out of the ERDP-order round so that round has one
>       variable. Found by the review of `pi4/erdp-before-erstba`. *Built
>       2026-10-04 on `pi4/erst-rsvdp`: ERSTSZ and ERSTBA read, their RsvdP
>       bits kept (`ERSTSZ_RSVDP`, `ERSTBA_RSVDP`), the size and the address
>       each masked to its field and ORed in; ERDP still written whole. Its
>       review: the address masked as Linux masks it, a const assert that
>       ERST is 64-byte aligned (shown to fail by mutation), the timeout
>       dump's ERSTSZ in hex, and one init line printing the bits kept, since
>       "they read 0 after HCRST" had never been observed. On QEMU it says
>       `RsvdP kept: ERSTSZ 0x0, ERSTBA 0x0`, so there this writes what the
>       old code wrote; the board's line is the round's evidence. QEMU:
>       `test-usb-hub` 19 ok in three captured runs and 18 in one run whose
>       output was not kept (the missing check unknown; watch for it),
>       `test-el1-drop` and `make test` green. Merged as #201. Done; on the
>       board 2026-10-04 (`build c07804a040e7 debug`): `RsvdP kept: ERSTSZ
>       0x0, ERSTBA 0x0`, so on the VL805 it writes what the old code
>       wrote, and a full session over serial, no fault or timeout
>       (`testing-pi4.md` section 6).*
> - [x] **fix** **CRCR, CONFIG and USBCMD are written whole, zeroing their
>       RsvdP bits** (CRCR 5:4, CONFIG 31:10, USBCMD's reserved ranges), the
>       same defect as ERSTSZ/ERSTBA in the same function. Linux keeps them
>       (`CMD_RING_RSVD_BITS`; CONFIG read and `HCS_SLOTS_MASK` replaced).
>       Fix with one helper that reads, keeps a mask and masks the new value
>       to its field (`portsc_preserve` is the existing shape; it was replaced
>       by `reg::PortStatus` on 2026-10-05), then move
>       ERSTSZ/ERSTBA onto it. Spec form, nothing seen; a board round of
>       its own. Found by the review of `pi4/erst-rsvdp`. *Built 2026-10-04
>       on `pi4/rsvdp-preserve`: `write32_rsvdp`/`write64_rsvdp` (read, keep
>       the mask, the new value masked to the rest, write, return the bits
>       kept) for USBCMD (both writes), CONFIG, CRCR, ERSTSZ and ERSTBA.
>       Masks: CONFIG 31:10 and CRCR 5:4 from Linux's `xhci.h`; USBCMD only
>       6:4 and 31:17, where Linux's comments (4:6, 15:31) and xHCI 1.2
>       (13:16 defined) agree, so 12:16 stay written 0 and no enable the
>       firmware left set is kept (the spec PDF was not read). The boot's
>       one line now names all six writes; on QEMU all are 0, so no rig can
>       fail on this change (a helper that dropped the kept bits passes them
>       all), and the board's line is the only evidence. Its review: the
>       merge made a pure `const fn` (`rsvdp_merge`) under const asserts,
>       so a broken merge now fails the build (two mutations shown); the
>       bits printed before the writes, not after (USBCMD's before the
>       reset, all five after it), so a write that hangs the board still
>       leaves them; a `debug_assert` against a value with RsvdP bits and a
>       const assert that the command ring is 64-byte aligned; the ERST
>       writes as three plain statements. QEMU: `test-usb-hub` 19 ok in four
>       runs, `test-el1-drop` and `make test` green. Merged as #202. Done;
>       on the board 2026-10-04 (`build 9ff7e4a9831b debug`): USBCMD 0x0
>       before the reset, and USBCMD, CONFIG, CRCR, ERSTSZ and ERSTBA all
>       0x0 after it, so on the VL805 it writes what the old code wrote;
>       booted to the shell, no fault or timeout (`testing-pi4.md` section
>       6).*
> - [x] **fix** **HCRST is set without halting the controller first.**
>       `init_inner` waits for CNR and sets HCRST; xHCI 5.4.1 says HCRST
>       shall not be set while HCHalted is 0, and Linux's `xhci_reset`
>       calls `xhci_halt` first. Clear R/S (through `write32_rsvdp`), poll
>       for HCH, then reset. On the Pi the firmware has handed the
>       controller over before this, so whether it is still running here is
>       a question for the capture: log USBSTS.HCH before the reset first,
>       then change it. A board round of its own. Found by the review of
>       `pi4/rsvdp-preserve`. *Step 1, the log, built 2026-10-04 on
>       `pi4/hcrst-halt-log`, read only: `xhci: as handed over: USBCMD …
>       (R/S …, RsvdP …), USBSTS … (HCH …, CNR …)` before the CNR wait,
>       and `xhci: at the reset write: USBSTS … (HCH …)`, read with nothing
>       between it and the write (the review: a serial print in between
>       could outlast a halt in progress, and a CNR timeout would have lost
>       a line taken after it). QEMU: `R/S false`, `HCH true` on both,
>       USBSTS 0x9 (EINT pending from the firmware). Mutated before it was
>       trusted: the controller started just before the read gave `R/S
>       true`, `HCH false` on both lines, and QEMU accepted that HCRST of a
>       running controller, so only the board can say what the VL805 does.
>       The Pi's lines decide step 2: HCH true at the write makes the halt
>       a no-op on the board, false makes it a real change. QEMU:
>       `test-usb-hub` 19 ok in four runs after one lost to the flake
>       below, `test-el1-drop` and `make test` green. Merged as #203. On
>       the board 2026-10-04 (`build 7f39b6ea226c debug`): `as handed
>       over: USBCMD 0x8 (R/S false, …), USBSTS 0x19 (HCH true, …)` and `at
>       the reset write: … (HCH true)`. **The Pi's firmware hands the
>       controller over halted**, so step 2 (clear R/S, wait for HCH, then
>       HCRST) is a spec-form no-op on this board: worth building for
>       other firmware and for any later path that re-initializes the
>       controller (a supervisor restart), not urgent, and its board round
>       can show only that it changed nothing.*
>       *Step 2 built 2026-10-04 on `pi4/halt-before-hcrst`: after the CNR
>       wait, a controller with HCH 0 has R/S cleared through
>       `write32_rsvdp` and HCH waited for (`Error::HaltTimeout` with
>       USBSTS if it never comes); a halted one is not written, so the Pi
>       sees no change. The reset line gains `found running …`, what was
>       read, not a claim the halt ran: the first wording, `halted here
>       first`, said `true` under a mutation that skipped the halt, and was
>       renamed for it. Mutated on QEMU: the controller started before the
>       handover read gave `found running true`, `HCH true` at the reset
>       write and a working keyboard; the same with the halt skipped gave
>       `HCH false`, so the line catches a missing halt. Unmutated:
>       `found running false`, `HCH true`. `make test`, `test-el1-drop`
>       and `test-usb-hub` (19 ok) green. The board round should print
>       `found running false` and change nothing else. Merged as #205. Done; on the board
>       2026-10-04 (`build 676797f83e01 debug`), two boots: `at the reset
>       write: USBSTS 0x19 (HCH true, found running false)`, every line to
>       the first `login:` as #203's, no fault or timeout (`testing-pi4.md`
>       section 6).*
> - [ ] **fix** **A login refusal does not say why, and a shadow read
>       error reads as a wrong password.** On the Pi 2026-10-04 (#205's
>       round), `root` was refused on its first attempts in two boots and
>       logged in after `user` had; every earlier capture logs `root` in
>       first time. `find_account_line` (`programs/shell/src/main.rs`)
>       retries only `NO_FS`, so any other `fsd` error on `/etc/shadow`
>       (a transient stick read, say) returns `None` and `verify_password`
>       falls through to a passwd line with no secret: `Login incorrect`.
>       First observe: say on the console when the shadow file could not
>       be read and with which error (no word about the account or the
>       password), and try `root` first on the next boots. Only then
>       decide whether a retry is right; retrying an error nobody has seen
>       would hide it. Found on the board, unconfirmed. Narrowed the same night:
>       `root` again after a logout logged in; after a reboot, refused 5 s
>       after the prompt and accepted 5 s later, nothing printed between.
>       A window early in the boot, not a state a login changes. *The
>       observation built 2026-10-04 on `login/say-why`: `find_account_line`
>       returns the read error instead of `None`, and `verify_password`
>       says which way the lookup failed (`login: /etc/shadow could not be
>       read (u64::MAX-N): <reason>`, `no /etc/shadow entry for this
>       account`, `this account's /etc/shadow entry does not parse`), the
>       verdict unchanged; a bare `Login incorrect` is now a wrong password.
>       Each line shown by a mutation on QEMU (the path misspelled, the name
>       swapped, the line cut), unmutated a wrong password still bare.
>       `make test`, `check-relocs`, `test-held-keys` green. Merged as #206.*
>       *Not reproduced since: on the board 2026-10-04 (`build 0af55b5c3b81
>       debug`, the stick and card re-staged), four boots, `root` typed at
>       once after the prompt, logged in every time, no line. The failing
>       and the passing sessions print the same boot lines (sorted, unique),
>       so nothing on screen tells them apart. Open, with the line armed: a
>       line before `Login incorrect` names a read error; a bare one says the
>       file was read and the password did not match, a typo or a lost byte
>       (the password is not echoed, and the kernel never programs the
>       PL011's LCR_H, so the receive FIFO is whatever the firmware left,
>       and the firmware's settings had just been reset). Leave it until it
>       recurs; no retry on a guess.*
> - [x] **fix** **`test-usb-hub`'s usb-boot layout sometimes never reaches
>       the kernel.** 2026-10-04, one run in three: QEMU's firmware stopped
>       in its own USB boot (`UsbBootExecCmd: Success to Exec 0x0 Cmd
>       (Result = 1)` the last line, the transcript 667 bytes, no `UEFI stage
>       alive`), and the rig reported five FAILs for a kernel that never
>       ran. The rig should report that case as INCONCLUSIVE ("the
>       firmware never loaded the kernel", no `UEFI stage alive`) rather
>       than fail each check, and count such runs; not retry until green,
>       which would hide a change that makes the stall likelier (the review
>       of `pi4/hcrst-halt-log`). Whether the unexplained 18-of-19 run earlier the same day was
>       this is unknown (one FAIL, not five, so probably not). *Built
>       2026-10-04 on `rig/usb-hub-inconclusive`: `firmware_stalled` calls
>       it only when the kernel printed nothing, the fault trace was read
>       and is clean, and the transcript ends in `UsbBootExecCmd`;
>       anything else, a kernel hanging before its first line included,
>       is still FAIL. INCONCLUSIVE exits 100; the make target still fails,
>       and says to run it again only when no layout really failed. Its
>       review: every run's verdict goes to `scratch/usb-hub-runs.log`
>       (outside `build/`, which `make clean` empties) and an INCONCLUSIVE
>       run prints its layout's stalls out of all its runs, a rate rather
>       than a count that only grows; each stalled transcript is kept in
>       `scratch/usb-hub-inconclusive/`, since the rerun overwrites the
>       one in `build/` (as it did the first one, which the fixture had to
>       be transcribed from); the fixture keeps the real line's escapes,
>       and a "firmware started the kernel, which printed nothing" case
>       must stay graded. `--self-test` (nine cases) runs in `make test`.
>       Mutated: dropping either condition, or anchoring the match, fails
>       the self-test; a forced verdict and a forced verdict beside a real
>       FAIL drove both endings of the make target. Merged as #204. Done.*
> - [ ] **new** **Is the usb-boot stall the rig's own doing?** The
>       `--usb-boot` stick is `build/esp.img`, which the hub boot just
>       before it mounted read-write as its virtio disk and then killed
>       (`g.stop()` is a SIGKILL). If the stall comes from that state, it
>       is a rig defect that INCONCLUSIVE would hide. Once
>       `scratch/usb-hub-runs.log` has a base rate, boot `--usb-boot` from
>       a fresh copy, as `--stall` already does with its own image, and
>       compare. Found by the review of `rig/usb-hub-inconclusive`.
> - [x] **fix** **Nothing stops a plain `write32` to a register with RsvdP
>       bits.** USBCMD, CONFIG, CRCR, ERSTSZ and ERSTBA go through
>       `write32_rsvdp`/`write64_rsvdp` today, by convention. Make the
>       wrong write unspellable: typed register handles whose only writer
>       takes the mask, or failing that a check in `make test` that rejects
>       a bare write at those offsets (`unspellable-postmortem.md`). Found
>       by the same review.
>       *Built 2026-10-04 on `xhci/typed-registers`: a private `mod reg`
>       in `xhci.rs` holds the two barrier stores and four handle types,
>       `Whole32`/`Whole64` (doorbells, PORTSC, DCBAAP, ERDP) and
>       `Kept32`/`Kept64` (USBCMD, CONFIG, CRCR, ERSTSZ, ERSTBA), their
>       fields private, so the only handle for a RsvdP register is the one
>       its constructor gives, carrying its mask. The free `write32`,
>       `write64`, `write32_rsvdp` and `write64_rsvdp` are gone. Shown unspellable by mutation: a bare `mmio_write32`, a
>       hand-built `Whole32` and a hand-built `Kept32` at USBCMD each fail
>       to compile (E0603, E0603, E0451), and a legal write at the same spot
>       compiles. Behaviour unchanged: the barrier check finds both
>       sequences, `test-usb-hub` 19 ok with the same reset and RsvdP lines,
>       `test-el1-drop` and `make test` green. Its review (`/code-review
>       high`, eight findings, no bug) found the claim wider than the code:
>       every constructor took a bare `u64` base, so `reg::doorbell(op_base,
>       0)` was a whole write to USBCMD. Now the blocks are types
>       (`OpRegs`, `Ir0Regs`, `Doorbells`) made only by `reg::locate` from
>       the capability registers, and that spelling fails (E0308, a
>       hand-built `Doorbells` E0603). It also found that the split had
>       weakened the image check: `mmio_write32` had two callers, so
>       rerouting `Whole32::write` left it green (shown so); the RsvdP
>       writes now go through `Whole32::write`/`Whole64::write`, one caller
>       per store, and rerouting either of those fails the check (`found 0
>       times`); a RsvdP write routed around them is still not seen, the
>       whole writes keeping their other callers.
>       Also: reads through the handles (`read`, `kept_bits`, so the RsvdP
>       diagnostic no longer restates address and mask), `debug_assert`s on
>       the port and doorbell index, and the holes named in the module doc
>       and the source map: a raw pointer store, `publish_cycle_word` (any
>       pointer), a block's `at` used for a read. Suites green again. A second
>       review found no bug and narrowed the record: the image check's
>       reach above, an index bound only by `debug_assert`, and reads half
>       through handles; the comments now say so. Not done, judged cleanup:
>       one generic block type for the three, and the timeout dump's reads
>       through the handles. Merged as #207. Done; no board round, the same
>       instructions through the same two stores.*
> - [x] **fix** **PORTSC's RW1C bits and PED are masked by convention.**
>       `reg::portsc` is a `Whole32`, and every write must go through
>       `portsc_preserve` or it writes back PED (disabling the port) and
>       clears pending change bits; nothing makes the bare write
>       unspellable, the same opt-in shape #207 removed for RsvdP. A PORTSC
>       handle whose write applies the preserve itself, taking the RW1C
>       bits to clear as an explicit argument. Found by the review of #207;
>       its second review adds that PORTSC and the doorbells share
>       `Whole32` (DCBAAP and ERDP share `Whole64`), so a port function
>       takes a doorbell. A PORTSC type of its own closes both.
>       *Built 2026-10-05 on `xhci/portsc-type`: `reg::PortStatus`, made
>       only by `reg::portsc`, with `read` and `write(current, set,
>       clear)`, which writes `portsc_merge`: by inclusion, as Linux's
>       `xhci_port_state_to_neutral`, only `current`'s read-only and
>       setting bits (`PORTSC_KEPT`), every other bit 0, so PED, the change
>       bits, PR and RsvdZ are never written back as read; then `set`, held
>       to PR and WPR (`PORTSC_SETTABLE`), and a 1 in each change bit in
>       `clear`. `current` is the caller's own read, so the write and the
>       decision see one state and the reads are as before. Out-of-range
>       bits stop a debug build and are dropped in a release one;
>       `portsc_merge` is a `const fn` with build-time asserts. It goes
>       through `Whole32::write`, so the barrier check is unchanged.
>       `portsc_preserve` and `Whole32::read` are gone. Shown unspellable
>       from outside `mod reg` by mutation: a one-value write (E0061), a
>       doorbell passed for a port (E0308) and a hand-built handle (E0603)
>       fail to compile, a legal write compiles, and a merge that writes
>       back PR or the change bits fails the asserts (E0080). Its review
>       (`/code-review high`, seven findings) moved it from a blacklist to
>       the inclusion mask and a settable list, and put the caller's read
>       back. At today's two writes PR reads 0, so the merge differs from
>       the old one only where a RsvdZ bit reads 1, now written 0; no
>       capture shows one, and no board round has run it. `make test` green, `test-usb-hub` 19 ok
>       with the same reset lines, `test-el1-drop` PASS. Left: DCBAAP and
>       ERDP still share `Whole64`; WPR on a USB2 port is refused only by
>       the caller's link-state check. Merged as #209. Done; the next board
>       round runs it, with the RsvdZ bits as its only difference on the
>       wire.*
> - [ ] **fix** **DCBAAP and ERDP share `Whole64`**, so a function taking
>       one takes the other, as PORTSC took a doorbell until #209. A type
>       each, or a type for ERDP whose write takes the dequeue pointer and
>       the EHB clear apart (EHB is RW1C, written 0 today). Left by #209.
> - [ ] **fix** **The xHCI rings stay off page boundaries by field order.**
>       The EP0 rings sit on a 256-byte boundary only because the 64-byte
>       ERST precedes them; the compile-time assertions catch a bad order,
>       but each layout change means reshuffling. A ring type aligned to its
>       own size (`#[repr(align(256))]` for the 16-TRB rings) makes every
>       order safe. Found by the review of `pi4/scratchpad-32`.
> - [ ] **new** **Scratchpad pages sized to what the controller asks.** The
>       pool keeps 32 pages (128 KB) in `.bss` on every platform, used only
>       by the VL805 so far, and caps the count at 32 where the spec allows
>       1023. Taking them from boot-services `AllocatePages` before the
>       exit, sized from `HCSPARAMS2`, removes both. Lower priority than the
>       board check. Found by the same review.
> - [ ] **new** **A guard page under the kernel's stack.** `KERNEL_STACK`
>       has a canary word at its base that both fault reporters check and
>       name, which turns an overflow into a line in the next dump rather
>       than a mystery; a guard page would fault at the moment of the
>       overflow instead. It needs the lowest 4 KB page of the stack left
>       unmapped in the kernel's own tables, a page split inside the
>       kernel's 2 MB block in `mmu.rs`, which today splits only the EL0
>       regions. Check: a deliberate overflow halts with a fault at the
>       guard's address, under both handoffs.
> - [x] **The early fault reporter: the kernel's own dump for a fault taken
>       under the firmware's vectors.** `earlyfault.rs` registers a handler
>       for synchronous exceptions through `EFI_CPU_ARCH_PROTOCOL` right
>       after console discovery (VBAR_EL1 untouched) and prints ESR decoded,
>       FAR, ELR, every register, a frame-pointer backtrace and the loaded
>       image holding each address (the kernel by offset, a firmware driver
>       by PE debug name and firmware-file GUID). *Built 2026-10-01 (branch
>       `pi4/early-fault-reporter`): checked on QEMU by `make
>       test-early-fault` (the `\EARLYFAULT` flag plants a fault in the DXE
>       core's `CopyMem`; the dump names `DxeCore.dll` and the kernel's
>       frames; with the registration removed the test fails). The kernel
>       is built with frame records now (`.cargo/config.toml`). Reviewed
>       and merged as #187 (2026-10-01). Open until a Pi boot shows the
>       dump where the firmware's line was: the first boot after the merge
>       ran a card staged from the pre-merge tree, and showed the one line
>       again at a third address (`0x39F36E14`); the boot after it, with the
>       reporter armed, went silent at the same step: no firmware line, no
>       dump (`testing-pi4.md` section 6 has the three readings). The
>       `NOXHCI` boot after that showed the dump for a post-exit fault, so
>       the reporter and the kernel's PL011 driver both work on the Pi;
>       what it showed is the next item. Done: the dump on a board was the
>       condition, and the `NOXHCI` boot gave it.*
> - [x] **The Pi hands the kernel off at EL2, and the kernel assumes EL1.**
>       The `NOXHCI` dump (2026-10-01, `testing-pi4.md` section 6): SPSR mode
>       EL2h, the fault delivered through the firmware's vectors after
>       `exceptions::install()`, a translation fault at `0xa000000` after
>       the identity map "installed": VBAR_EL1, TTBR0_EL1, TCR_EL1, MAIR_EL1
>       and the EL1 timer are all written to registers EL2 does not use. Drop
>       to EL1 right after `exit_boot_services` (HCR_EL2.RW, no traps, no
>       stage 2, CNTHCTL_EL2 for the EL1 timer, SP_EL1, `eret`). Explains
>       "the Pi 4 stops after the early console's clear". The xHCI takeover
>       fault is before the exit and stays its own item. **The plan:
>       [`roadmap-el1-drop.md`](roadmap/roadmap-el1-drop.md)** (2026-10-01,
>       scoped before code): prepare EL1's tables and vectors at EL2, then
>       `eret` into a running MMU; four steps; the dev loop is QEMU with
>       `-machine virt,virtualization=on`, whose firmware also hands off at
>       EL2 and which reproduced the Pi's signature the same evening.
>       *Steps 0 to 2 built 2026-10-02, merged as #188: `el2.rs` drops
>       from `mmu::switch_to_identity_map`, `make test-el1-drop` is the
>       rig, and a mutation that disabled the drop fails it. Open for step
>       3, one Pi 4 boot under `NOXHCI` from a card staged off `main`,
>       which is the first boot whose post-exit log can mean what it
>       says; `testing-pi4.md` checkpoint 5b has the three lines to read.
>       Done the same day: the second of two `NOXHCI` boots printed the
>       three lines in order and reached the shell, the GIC-400 and the
>       tick up. The first boot is the revised item above.*
> - [ ] **fix** `virtio_mmio_probe_safe`'s premise ("a serial console means
>       QEMU") is false on the Pi 4, which has a serial console and no
>       virtio transport. Under the kernel's own tables the scan at
>       `0xa000000` did not fault there (2026-10-02; the low 1GB is a Device
>       block) and reported no device, so the premise is false but harmless.
>       Retire it for honesty, the way the devicetree translation fails
>       closed, not because it is dangerous.
> - [x] **fix** The reporter's register rows come after the backtrace, so a
>       fault in the image-naming walk (seen on the Pi, frame 10, a return
>       address in the firmware volume) loses them; and the walk reads
>       images with no bound beyond their declared size. Rows first, the
>       nested-fault line with FAR, the walk bounded. *Done 2026-10-02: the
>       rows print before anything reads firmware memory; the nested line
>       carries `far` and the address of whatever firmware memory was under
>       read (a table entry, a record, an image header, a device path, a
>       frame record), or says there was none; the walk believes each of
>       those only inside one RAM descriptor of the memory map at arm time
>       (`mmu::is_general_ram`, up to 64 of them), and the `armed` log line
>       states the bound. Whether the Pi's entry claiming to hold `0x26e28`
>       falls outside it is for that line to say on the next boot: the
>       kernel's own RAM span there starts at `0x3b0000`, but the reporter
>       admits more types. `\WALKFAULT` makes the report fault inside its
>       walk, and the rig's third boot grades the rows and the nested line;
>       mutations showed the order check and the refusal path can fail.*
> - [ ] **The Pi 4 stops after the early console's clear.** Under `\FBCON`
>       the whole-screen clear reaches the display and the first line after it
>       never does. On an earlier build the same board went black the same
>       way; on another boot it did not clear at all. For the serial cable.
>       *Explained 2026-10-01: the kernel runs at EL2 on the Pi, so the
>       early console's first write after `exceptions::install()` and the
>       MMU switch faulted into the firmware's EL2 vectors with the
>       firmware's text console gone. Closes with the EL1 drop below.*
> - [x] **The devicetree console's address is a bus address; translate it.**
>       The Pi 4 logged `console @ 0x7e201000 (via devicetree)`, the PL011's
>       VideoCore bus address (`0xfe201000` for the ARM), so without `\FBCON`
>       the kernel's first write after the exit went to no device (1 GB Pi 4)
>       or to RAM (Pi 400). *Built 2026-09-28 (branch `pi400/boot-flags`,
>       not pushed): `dtranges.rs` applies every ancestor's `ranges`, fails
>       closed, and the console is mapped as its own device (it was covered
>       only by the GIC's 1 GB block). Checked on the host against the pftf
>       zip's Pi 4 and Pi 400 trees, hand-built trees and a mutation; on
>       `main` since #181 (2026-10-01). Serial showed `console @ 0xfe201000
>       (via devicetree)` on the Pi 4 the same day: closed.*
> - [x] **Boot flag files and `\FBCON` for bench diagnostics.** `\NOXHCI`,
>       `\XHCINOWR`, `\FBCON` (the framebuffer console right after the exit,
>       with progress squares before it), the image range and an
>       `exiting boot services` marker in the log, and
>       `scripts/efi-symbol.py` to place a firmware-reported address. *Built
>       on the same branch; each checked on QEMU (`-device ramfb`, planted
>       faults and hangs). Reviewed and merged as #181 (2026-10-01); the
>       review found `\FBCON` with no framebuffer left no console at all,
>       and `efi-symbol.py` now refuses a range whose length is not the
>       relinked `.efi`'s `SizeOfImage`. The one small item it left,
>       `bootflags::present()` opening the volume once per flag, became
>       `bootflags::read()`, one open for all three (#183,
>       merged 2026-10-01).*
> - [x] **`make sdcard`.** *Built 2026-09-28, merged as #180 (2026-10-01):* the pinned pftf
>       firmware plus `build/esp` on an already-formatted card, installed
>       once since the firmware keeps its settings in `RPI_EFI.fd`; never
>       formats.
> - [x] **Small:** the `fdt` crate panics on a tree with no `/chosen`
>       (`chosen()` expects one; found building test trees, real firmware
>       trees have it). *Fixed 2026-10-01 (#182, merged the same day):*
>       `devicetree.rs` reads `/chosen/stdout-path` itself. That also removes
>       the crate's second panic, on a zero-length `stdout-path`; skips a path
>       that is only a NUL (`find_node("")` is the root, which ended the
>       search as an unsupported console); and trims a trailing NUL instead
>       of dropping the last byte. Checked by a host harness, not committed,
>       on the pftf Pi 4 and Pi 400 trees and seven hand-built ones with a
>       decoy PL011 ahead of the real one: the two that panicked fall
>       through, the NUL-only and NUL-less paths now resolve as intended,
>       the rest are unchanged, and putting back either old behaviour is
>       caught.
> - [ ] **The devicetree console lookup has no test that can fail.** QEMU
>       never runs it (its firmware publishes no DTB) and the harness above
>       lives outside the tree, so going back to `chosen().stdout()` would
>       pass `make test` and every QEMU run and show only as a halted Pi.
>       Move the lookup into a pure helper with host tests over those trees.
>       From the review of #182.
> - [ ] **Other `fdt` panics reachable from firmware data.** In the same
>       function: `reg()`'s cell sizes on a short `#address-cells`, a
>       malformed unit name in `find_node`, and alias resolution that
>       recurses without limit on an alias naming itself. Wrap the crate
>       in non-panicking helpers as `dtranges.rs` already does, or
>       validate the tree first. From the same review.
> - [ ] **Which UART the devicetree console picks: three open choices, for
>       the serial session.** `stdout-path` may carry options
>       (`serial0:115200n8`) that are not stripped, so it never resolves; the
>       PL011 fallback takes the first in tree order, including one with
>       `status = "disabled"`; and a `stdout-path` naming a non-PL011 UART
>       (the Pi's mini UART) ends as an unsupported console without trying
>       the PL011 search. Each changes which UART a board gets, so none is
>       made before the serial dump shows what the Pi's tree says. From the
>       same review.
> - [ ] **Small:** the UART's and GIC's device block depends on
>       "Limit RAM to 3 GB" staying on: with RAM to 4 GB, block 3 is mapped as
>       RAM and the device mapping is skipped. Since #181 the kernel at least
>       says so: after the MMU switch it asks the hardware walker whether the
>       serial console is Device memory and warns if not (shown on QEMU by
>       pointing the check at RAM). The GIC has no such check.

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
>
> **The small C compiler has a plan since 2026-09-29.** The workspace chose
> Ouroboros as the destination of its C toolchain arc: hello.c edited,
> compiled, linked and run here, by Proem (the preprocessor), a compiler that
> Phoenix generates and an assembler Futamura describes. All three are C11
> with no dependencies, so the path above carries them. The order and the gaps
> are in `~/Projects/docs/c-compiler-toolchain.md`, outside this repository.
> Two things it asks of Ouroboros itself: **a static ELF linker that runs
> here**, since picolibc is a `.a` and LLD is on the host, probably a project
> of its own; and **an editor**, whose catch is that the console offers no
> termios for one in the kilo style, so a line editor comes first.
>
> **CPP is now Proem, since 2026-10-01.** The workspace's C preprocessor was
> renamed: the folder is `~/Projects/Proem` and the repository
> `github.com/hansolovkarlsson/Proem`. `~/Projects/CPP` no longer exists. A
> mention of CPP in older records means Proem.
>
> **What Proem asks of Ouroboros, accepted 2026-10-01 and 2026-10-02, for
> later.** Five handoff notes, triaged and accepted; none is started. A sixth,
> a notice with Proem's smaller heap numbers, was taken on 2026-10-02 and is
> in `handoffs/closed/`. Each note carries its evidence, its **Done when**
> and the reply below in full.
>
> - [ ] **new** **A user heap of at least 1 MiB, ideally sized per program.**
>       `HEAP_PAGES` is 64 (256 KiB, `kernel/src/loader.rs`) for every
>       program; an empty file with Proem's `target.h` already takes 152 KB
>       of live `malloc`. Proem's figures, remeasured on 2026-10-01 after it
>       freed each file's text and shrank its tables: 342 KB for
>       `libc/picodemo.c`, 667 KB for its C11-header test, 2,031 KB for its
>       112-header picolibc test (down from 697, 1,459 and 3,560). The bound
>       is the one 2 MB region slot that holds code, heap, guard and stack:
>       1 MiB now holds everything but the picolibc test, which needs regions
>       larger than one slot, `mmu.rs` and `tasks.rs` work. Within one slot a
>       larger `HEAP_PAGES` costs no RAM (`allocate_runtime_region` rounds
>       every region to a whole slot already), only code room in every
>       program, the largest of which loads at 135 KiB today (`netd`, static
>       data included): 256 pages is the cheap first step and meets the
>       request. A size carried per program in the ELF is what goes further,
>       with multi-slot regions past 2 MB, wanted for that one test and for
>       the compiler later. Blocks step 8 of `roadmap-c-hosting.md` (branch
>       `docs/c-hosting`). Notes:
>       [`handoffs/2026-10-01-from-proem-heap-growth.md`](handoffs/2026-10-01-from-proem-heap-growth.md)
>       and, for the numbers,
>       [`handoffs/closed/2026-10-01-from-proem-heap-numbers.md`](handoffs/closed/2026-10-01-from-proem-heap-numbers.md).
> - [ ] **new** **Stage the headers in two directories, and build `proem`
>       with them built in.** Steps 6 and 8 of the C-hosting plan, decided:
>       `/include` holds picolibc's 136 headers and a `target.h` generated at
>       build time with exactly `$(CFLAGS_OS)`; `/include/clang` holds
>       clang's eleven freestanding headers plus their `__float_*.h`,
>       `__stddef_*.h` and `__stdarg_*.h` helpers, copied from `$(clang
>       -print-resource-dir)/include` by glob, not by a kept list. Two
>       directories because `inttypes.h`, `limits.h`, `stdint.h` and
>       `stdnoreturn.h` exist on both sides and picolibc's `limits.h` reaches
>       clang's by `#include_next` (its line 143). `driver/proem.c` is then
>       compiled with `-DPROEM_SYSTEM_DIRS='"/include:/include/clang"'`, so
>       `proem hello.c` needs no options. To check on the way: the FAT32 ESP
>       takes the lowercase, nested and `__`-prefixed names through `fsd`'s
>       long-name path, and the ext2 and exFAT images stage the tree too.
>       The stage has its own check (`cat /include/stdio.h`, `ls
>       /include/sys`, `ls /include/clang` on a booted image); the build
>       waits on steps 1 to 5 and the heap above, and its **Done when** is
>       the arc's finish line. Note:
>       [`handoffs/2026-10-01-from-proem-system-dirs.md`](handoffs/2026-10-01-from-proem-system-dirs.md).
> - [ ] **fix** **`fstat` leaves every field but two as it found them.**
>       `libc/src/file.c`'s `fstat` sets `st_size` and `st_mode` only, so a
>       caller's `struct stat` keeps stack garbage in `st_dev` and `st_ino`,
>       and Proem's `#pragma once` and include-guard skipping can take two
>       headers for one. First, and small: zero every field `fstat` does not
>       fill, which Proem reads as "no identity". Then a real identity: the
>       `NP_FSTAT` record (27 bytes, `ninep-abi`) has no inode or qid, so it
>       is a wire change across `ninep-abi`, both C headers and the Python
>       peers, plus an identity per filesystem (ext2's inode; FAT32 and
>       exFAT have none, so the directory entry's location; `/proc`'s own)
>       and an `st_dev` per server and per remote mount. Note:
>       [`handoffs/2026-10-01-from-proem-fstat-identity.md`](handoffs/2026-10-01-from-proem-fstat-identity.md).
> - [x] **Rename CPP to Proem in the C-hosting plan** on the branch that
>       carries it (`docs/c-hosting`, not on `main`): `/bin/proem`,
>       `proem-bin`, `PROEM_DIR` defaulting to `../Proem`, `driver/proem.c`,
>       the finish line's command, and the plan's index line. **Done
>       2026-10-04, `69653a8`**, before the plan merges, so `main` never
>       carries the old names. Note:
>       [`handoffs/closed/2026-10-01-from-workspace-proem-rename.md`](handoffs/closed/2026-10-01-from-workspace-proem-rename.md).
> - [ ] **new** **`unlink` in the C port.** picolibc's `remove` calls
>       `unlink`, which nothing in `libc/src` defines, so linking Proem (which
>       removes its `-o` output after a failed run, as Clang does) fails on
>       the undefined symbol. One more `np_request` in `libc/src/file.c`,
>       `NP_RM` on the resolved path beside `open`'s `NP_OPEN`; 0 on success,
>       -1 with `errno` (`ENOENT` for a missing file) from step 2 of the
>       C-hosting plan, so it lands with or after that step. Done when a C
>       program on a booted image removes a file it made, `ls` no longer
>       shows it, and a second `remove` gives -1 and `ENOENT`. Note:
>       [`handoffs/2026-10-01-from-proem-unlink.md`](handoffs/2026-10-01-from-proem-unlink.md).
>
> **What Edit asks of Ouroboros, accepted 2026-10-05, for later.** One
> handoff note, a full-screen editor's needs, which is item c below (a text
> editor and full-screen terminal control) made concrete. Not started.
>
> - [ ] **new** **A console and keyboard a full-screen editor can use.**
>       Five parts, in Edit's order: (1) `cond`'s framebuffer backend
>       interprets `ESC [ r ; c H` (it ignores the parameters today and
>       goes home), `ESC [ K`, `ESC [ 2 J`, `ESC [ 7 m`/`ESC [ 0 m`/`ESC [
>       m`, and swallows `ESC [ ? 25 l/h` without drawing; (2) the USB
>       keyboard sends the VT100/xterm sequences for the arrows, Home, End,
>       Page Up, Page Down, Delete, F2 and F3, which `xhci.rs`'s
>       `keycode_to_ascii` drops; (3) the screen size readable by an
>       ordinary program on both backends (`CON_INFO` is gated to `cond`,
>       and `more` assumes 24 rows); (4) a per-program opt-out of the
>       Ctrl-C kill, so 0x03 reaches it as a key (WordStar's page down);
>       (5) `main(argc, argv)` in `crt0.c`, which calls `main(void)`. Done
>       when a program in this tree, on the framebuffer console, writes
>       each sequence of (1) and prints the bytes each key of (2) sends,
>       and a C program's `main` receives its arguments. (4) touches the
>       one choke point every keyboard path funnels through
>       (`syscall.rs`'s Ctrl-C interception), so it needs its own design
>       note first. Note:
>       [`handoffs/2026-10-05-from-edit-editor-console.md`](handoffs/2026-10-05-from-edit-editor-console.md).

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
  `docs/postmortems/libc-arc-postmortem.md`. **The mechanism is done; the remaining bullets
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
  `docs/research/research-redox-and-pi.md`.

**Status (2026-08-28): the mechanism is built, the arc's remainder is
forward-looking.** The six-step libc arc is complete through a running picolibc
(see `roadmap-completed.md` for the sequenced record and
`docs/postmortems/libc-arc-postmortem.md` for the retrospective). What remains is genuinely
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
[`users-and-permissions-postmortem.md`](postmortems/users-and-permissions-postmortem.md)
(steps 1–3) and
[`account-management-postmortem.md`](postmortems/account-management-postmortem.md) (step 4).
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
  through the far side's own `/etc/passwd`. What remains is the tier below:
  the export authenticates the *machine* (its keypair), so an authorized
  machine can still claim any user with an account on the export, root
  included; see item 1 above.
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
for at-rest security). See `docs/research/research-redox-and-pi.md`.

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
in [`research-gui-stack.md`](research/research-gui-stack.md). Its finding: the mouse
driver and a `drawd` draw server are the two steps that unlock everything else,
and the pixel-transfer model is the day-one decision to get right.

**g. Cluster data redundancy — documents failsafed across nodes (new, a later cluster phase).**
The cluster (see [`roadmap-cluster.md`](roadmap/roadmap-cluster.md)) shares disk and
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
it small, don't port a giant" precedent. See `docs/research/research-redox-and-pi.md`.

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
    from the *target*'s state, so a producer that exited early got `pipe:
    could not authorize the stream` printed on top of its own message, which
    is exactly the noise the check exists to suppress. **Fixed in the kernel,
    not the shell:** `DELEGATE` now answers `TASK_ERR_NO_SUCH_TASK` for a dead
    slot at either end and `MSG_ERR_DENIED` only for a refusal, as `KILL`,
    `FG`, `WAIT`, `MSG_SEND` and `MSG_CALL` already did, and the shell reads
    the reason from the answer. The first version read `TASK_STATE` for both
    ends *after* the denial; the `high` review showed that a later state read
    cannot tell a refusal from an exit that happened in between, so it would
    have silenced a real denial, and a nested shell, whose static mask holds
    no spawnable slot, is refused on every link.

    **Narrower than recorded for a producer that PRINTS, routine for one
    that does not. Measured, both.** `ls /nope | wc` and `cat /nope | wc`
    were already quiet before the fix: a producer that prints blocks on
    `cond` and hands the shell its turn back before the link is authorized,
    and one that reaches `ulib::end_of_stream` stays alive in its 150-tick
    retry of the denial. But a producer that neither prints nor ends its
    stream runs to exit *first*: `SPAWN` is the shell's longest syscall, a
    tick is pending at its `eret`, and the child gets the slice.
    `touch /T3 | wc` on the unmodified shell reached the dead-link path on
    the first attempt, where the original code would have printed the line,
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
    stream: `touch f | wc` would print `0 0 0`, and the racy and non-racy
    paths would converge. Not done here: it is a behaviour, not a repair.
  - **A producer that exits without `end_of_stream` wedges the pipeline.**
    `ls -? | wc` leaves `wc` blocked in `pipe_recv` forever and the shell
    waiting on it: no prompt came back within the rig's 90 s step timeout.
    `touch /T4 | wc` the same: `touch` exits 0 after the link is authorized
    and sends nothing, so a silent producer either wedges the shell or (when
    it exits before the link) is torn down with its consumer; it never works.
    Found 2026-09-06 while using `-?` as the exit-without-EOF producer the
    previous item's reproduction needed, and `usage_if_requested` is only the
    instance that was run. **26 of the 53 programs under `programs/` never
    call `end_of_stream` at all** (counted by grep: every `admin/*` tool,
    `cp mv rm mkdir rmdir touch chmod chown write writeat more`, `send recv
    readkey args selftest`, both demos, the four servers), and the ones that
    do skip it on their early error exits (`cat` with no operand). What the
    rig did *not* measure is Ctrl+C: `drive-qemu.py` cannot send it, and the
    review traced that for a console-sink pipeline the consumer owns the
    keyboard, so Ctrl+C kills it and the shell's `WAIT` returns: the slot
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
    own empty message, and a second one lands in a mailbox nothing drains:
    the shell's next capture would read it as an immediate EOF; and a full
    mailbox at teardown cannot be retried by a task being torn down. Decide
    the shape before writing either.
  - **The shell's pipeline teardown uses slot numbers as identity.** A stage
    that faults at EL0 goes straight to `Unused` (`kill_current_and_switch`
    stores it, no zombie), and any spawner, `netd`'s `cpu` handler, a nested
    shell, may take that slot before the shell's `kill_and_reap` loop reaches
    it, which then kills or reaps a stranger; `KILL` and `WAIT` have no owner
    check. The same class as the `netd` raw-slot item below, seen from the
    other side, and pre-existing: the 2026-09-06 fix did not widen it, but it
    is the moment "unused means already exited" was written down as a rule.
    Raised by the `high` review, traced not run.
  - **`cmd_pipeline` runs a builtin-headed second segment after the first
    segment failed.** `run_head_pipeline` returns `()`, so when the drained
    upstream segment aborts (a stage that will not spawn, a link that cannot
    be authorized), the builtin and everything after it still run and print
    success-shaped output under the error line, where an all-program pipeline
    aborts whole. A `bool` return gating the second call is the whole fix.
    Raised by the `high` review, traced not run.
  - ~~**Nothing re-grants `TO_NET` after a supervised `netd` restart.**~~
    **Fixed 2026-09-06, the other way round: nothing strips it any more.**
    `clear_delegate` stripped bit 4 from every live task on `netd`'s
    teardown, right for a spawnable slot, which may be reused by a stranger,
    and wrong for a protected one, which `SPAWN` can never fill and which only
    ever holds the same supervised server again. The grant is only ever made
    at spawn, so a program alive across the restart was netless for the rest
    of its life, and `net_msg_call` busy-spun its 150-tick deadline before
    reporting the same generic failure #107 was about. The strip moved from
    teardown to `spawn`, the one place a slot is actually taken by a
    stranger, so a grant aimed at a protected slot now survives its
    occupant's restart; a grant aimed at a dead slot is inert in between,
    since every send tests liveness before the capability. **Measured
    before and after** with a temporary ping loop (eight requests, 1.5 s
    apart) `exec`'d and then `netd` parked by a temporary trigger so the
    supervisor restarted it: before, one in-flight failure and then six
    failures out of six after *"server slot 4 restarted"*; after, the one
    in-flight failure and replies resuming. The two rig mutations are not in
    the tree; the recipe is in `testing-qemu.md`. Still true and untouched:
    the client's 150-tick spin on a denial has no yield. The `medium` review
    found the grant this keeps that nobody had named: `netd`'s own, to its
    remote-exec child. An orphaned child now reaches the *fresh* `netd`,
    which has no record of it, and its zero-length end-of-stream marker
    would have been decoded from the never-cleared request buffer as the
    previous client's request. Closed with a length guard at the top of
    `handle_client` that answers as the unknown-op arm does rather than
    dropping, so a blocking caller is never left waiting (the `low` review's
    one finding; the supervisor ping is eight bytes, unaffected); the
    child's data messages dispatch as unknown ops and are answered and
    ignored. Demuxing the child by its stdout target instead of a remembered
    slot is the fuller shape, and belongs with the raw-slot item below.
  - **A client alive across a `netd` restart keeps a bare `/net/tcp/N` index
    the fresh `netd` may hand to someone else.** `DialConn` records no owner,
    `dial_file_op` checks only `dials[n].is_some()`, and `alloc_dial_slot`
    reuses the lowest free index; `dial`'s polling loop treats the
    restart-window errors as "nothing buffered" and keeps reading, so it can
    drain bytes meant for a `serve` that got index 0 after the restart, and
    its `close` tears down that connection. The mechanism is pre-existing
    (the idle GC recycles indices too), but until 2026-09-06 a stale client
    was denied at the send boundary and could not reach it; keeping the
    grant across the restart is what makes it reachable. Own change: record
    the opening sender in `DialConn` and check it in `dial_file_op`. Raised
    by the `medium` review, traced not run.
  - ~~**`netd` demuxes remote-exec by raw slot number** (`PendingRun.owner`,
    `TcpConn.cpu_child`).~~ **Fixed 2026-09-06 with a kernel-issued task
    identity.** Slots are recycled the moment a task is reaped, and until
    #107 the code could lean on only one spawned task at a time holding
    `TO_NET`; the shell's blanket grant made that false. The kernel now issues
    every task a **generation** when its slot becomes live (a boot-wide
    counter; a supervised restart is a new occupant too), captures the
    sender's packed identity `(generation << 8) | slot` with its credential at
    send time, and exposes it as `SENDER_TASK`, with `TASK_IDENTITY(slot)`
    for the other direction. `netd` records both its remote-exec child and
    its run owner by that identity and matches every message against it.
    **Measured before and after** (recipe in `testing-qemu.md`): a remote
    `cpu` run of `touch`, which exits without ending its stream, left the
    child a zombie with the connection still naming its slot; `wait 6` in
    the guest reaped it, and the next `ping` landed in slot 6. Before: the
    ping's request was captured as the child's output and the prompt never
    returned. After: `reply from 10.0.2.2`. Chosen over "reap promptly" and
    "demux by stdout target" on all three criteria (stable: captured by the
    kernel, not inferred; safe: closes the class; not blocking: a generation
    is what a process id is). **Same disease, next to convert, each its own
    change:** ~~`fsd`'s fids (`Fid.owner` is a slot, and its own doc says so)~~
    **converted 2026-09-12**: `Fid.owner` is the `SENDER_TASK` identity, the
    reaper frees by it (a recycled shell slot's leaks and a restarted `netd`'s
    fids alike), and a uid mismatch is refused without dropping the fid;
    `libc/cleak.c` is the check, and the fid gate's co-tenant check passes
    with `netd`'s own per-user check removed. Still to convert:
    the `/net/tcp` connection table (the item above). `ps`, `wait`,
    `kill` and `fg` still speak in slots on purpose; widen when something
    needs it. **The review's catch, fixed in the same branch:** a message with
    no identity (the kernel's health ping, a boot task before the boot slots
    got their generations) answered all-ones from `SENDER_TASK`, which was
    also `netd`'s "no child" sentinel, so an idle open connection captured
    every ping and the supervisor restarted `netd` (measured by holding a
    bare TCP connection open for a minute). The sentinel is zero now, which
    no identity can be; a message with no identity is never demuxed as a
    child; a run from a caller with none fails closed.
  - ~~**A nested shell cannot delegate `TO_NET` at all.**~~ **Fixed
    2026-09-06 with parent tracking and one-step delegation.**
    `may_delegate` read the *static* mask and spawnable slots have none, so
    every `delegate_net` from a spawned `SH.BIN` was denied, discarded by its
    `let _ =`, and its whole subtree silently netless; wider than `TO_NET`,
    measured: the same refusal hit every producer→consumer link, so a nested
    shell could run *no* pipeline (`ls / | wc` answered `pipe: could not
    authorize the stream`). The kernel now records every task's parent at
    `spawn`, by task identity, and `may_delegate` has a second way to pass: a
    task may hand a right it holds, statically or by delegation, to its own
    direct children (a grandchild does not qualify). Rights flow one step
    down and nowhere else; no task can name a stranger. (`SPAWN` is ungated, so
    any task may spawn a child and pass it what it holds. Stated as accepted:
    the parent could relay every byte itself, so the child gains nothing the
    parent could not do on its behalf. Gating `SPAWN` is its own decision,
    below.) Scored against the alternatives
    (consult runtime grants in `may_delegate`: laundering; a "delegable" flag:
    the shell cannot tell a subshell from any program without trusting a
    binary's name) it is the one that wins on all three criteria, and parent
    tracking is the primitive the owner-less `WAIT`/`KILL` and the pipeline
    teardown are waiting on. **Measured after:** the same nested shell
    answers `1 5 40` to `ls / | wc`, `reply from 10.0.2.2` to `ping`, and
    `1 3 21` to `ping | wc`. The nested shell needed no change: the calls it
    already made stopped being refused. The `medium` review then found the
    half the measurement missed: a nested shell could still not relay a
    builtin's output into a child or capture a child's output, so
    `echo hi | wc` and `ls > f` failed under it, because the parent-child
    channel was the boot shell's *static* privilege. A parent and its child
    are now granted each other at spawn. The review also removed the
    "statically held" rule as dead: every task the boot shell wires is one it
    spawned. This is the one-step form of
    north-star item 4 ("transitive delegation"), built now because the nested
    shell is the consumer that item said to wait for; the general, revocable
    form is still open there.
  - **`SPAWN` is ungated.** Any task may spawn, which with one-step
    delegation means any task may pass what it holds to its own children.
    Accepted on the proxy argument (above), but a `CAP_SPAWN` bit in
    `caps_for_slot` would make "which tasks may create tasks" a stated policy
    rather than an accident of the dispatch table. Raised by the `medium`
    review of the one-step delegation change, 2026-09-06. **Decided
    2026-09-07, not yet built: a STATIC bit**, held by slot 0, `netd` and every
    spawnable slot, denied to `fsd`, `cond`, `accountd` and the idle task. The
    delegable form collapses on contact (the boot shell cannot tell a subshell
    from any program without trusting a binary's name, the argument already
    rejected once), so the bit buys exactly one stated sentence, *servers
    cannot create tasks*, which today is an absence rather than a check.
    Verify by temporary mutation (no server can be made to spawn on cue);
    pairs with the misbehaving-program rig item below.
  - **The session gate's stated limits (2026-09-07, step 4 of
    `roadmap/roadmap-fid-verbs.md`).** Sessions are capped in total
    (`SESSION_MAX = MAX_CONNS - 1`), not per peer, so with three or more nodes
    one peer can hold every session slot; a per-peer share is the fix, and a
    two-node cluster cannot exercise it. `scripts/np9p_server.py` serves a
    session single-threaded (a held session blocks the next accept until it
    closes or idles out at 60 s), fine for the gate and not for the step that
    makes the guest hold one. A session client that pipelines a request before
    the previous reply is acked has it retransmitted, not lost (the export's
    request state resets only once the reply is acked, since the reply buffer
    feeds retransmits). And the export still takes a request from one segment,
    **which on a session changes the failure's shape** (review of #123): a
    request split across two segments used to be one refused request on a
    connection that then closed; on a session the first segment earns an auth
    refusal, the second is dropped and retransmitted, and after the reset it
    arrives as a fresh "request" and earns a second one, so the client reads
    two replies for one request and every reply after is off by one. Not
    reachable from a shipped caller (the guest caps inline data at 512 bytes),
    but step 7's `NP_PWRITE` frames are up to `NP_FRAME_MAX` = 2252 bytes
    against an MSS of 1460, so **a per-connection request buffer filled to
    `4 + len` is a prerequisite of step 7**, not a nicety.
  - **A SYN against a transiently full table is now refused, and the guest's
    own clients take a RST as final** (review of #123). `tcp_get`/`tcp_run`
    answer `NET_FETCH_REFUSED` at once on a RST with no SYN retry, so a remote
    read that lands while the export node holds all four connections (three
    idle sessions and one one-shot, or four HTTP transfers) fails immediately where
    a dropped SYN used to be absorbed by a one-second retransmit into a freed
    slot. Rare with two nodes, real under load. Two shapes, a decision: drop
    the SYN when the table's non-session slot will free by itself (a plain
    one-shot: bounded by the RTO give-up and the 30 s reap) and RST only when
    it is held by something unbounded (a `cpu` run, which the reap exempts
    while its child runs); or the client retries a SYN once or twice after a
    RST (a genuinely closed port then takes longer to report). The first
    version of this item said "RST only when every busy slot is a session",
    which cannot happen: `SESSION_MAX` is `MAX_CONNS - 1`, so a full table
    always holds one non-session (review of #124).
  - **`netd`'s TCP receive path does not validate what it accepts, and an
    attempt to fix that was ABANDONED on 2026-09-07 after it proved worse than
    the gaps.** The gaps are real and are listed here so the analysis outlives
    the branch (`netd/tcp-hardening`, cc1a190, kept on origin, PR #126
    closed unmerged):

    - **Nothing bounds an incoming ACK.** `handle_conn_segment` applies any
      `ack` past `snd_una`, so a stale ACK from a previous incarnation of a
      4-tuple (`SERVER_ISN` is the constant 0x0002_0000, so every incarnation
      shares one sequence space) advances `snd_una` arbitrarily, grows `cwnd`,
      and through the go-back-N fast-forward moves the file cursor - a hole in
      the served body rather than an error.
    - **No segment is sequence-checked.** A RST frees a slot on the 4-tuple
      alone; a FIN closes on the 4-tuple alone; `find_conn` ignores the local
      port, so one peer's two connections to 80 and 564 are one slot to netd.
      RFC 5961's three protections (challenge on an unacceptable ACK, on an
      out-of-window RST, on a SYN into an established connection) are all
      absent.
    - **A duplicate SYN rebuilds a live slot**, discarding whatever it held.
      **Since 2026-09-12 "whatever it held" includes open files**: a session's
      fids are clunked on `fsd` when its `TcpConn` drops, so a blind SYN or
      RST on a guessed 4-tuple closes the real peer's files, whose next
      `NP_FSTAT` answers a bare `FS_ERROR` indistinguishable from a bad fid.
    - **A peer that RSTs or FINs mid-`cpu` run orphans the child.** The slot
      is freed without looking at `cpu_child` (the idle reap and the RTO
      give-up exempt a live child, so only those two paths are reachable);
      the child's end-of-stream then finds no connection to route to and
      lands in `handle_client`'s length guard, and nothing `WAIT`s it, so it
      holds a task slot until someone types `wait N`. Pre-existing, found by
      the review of step 5 (2026-09-12) because `TcpConn`'s new `Drop` reads
      as the connection's teardown and is not: a `WAIT` there would block the
      event loop, so the release is a `KILL` or a deferred reap, its own
      change.
    - **No zero-window persist timer** (RFC 1122 4.2.2.17). Nothing probes a
      receiver that shuts its window, so nothing elicits the update that would
      reopen it: with the window shut the first RTO expiry's `rewind_to` drives
      `in_flight` to 0, the next call takes the early return and resets
      `rto_retries`, so the abort arm is unreachable and `reap_idle` RSTs the
      transfer at 30 s instead - a live but paused reader, dropped mid-file.
      (An earlier draft of this bullet said the RTO retransmits into the shut
      window and aborts after `RTO_MAX_RETRIES`; `service_rto`'s own comment is
      the correct half, and this is the record the next attempt builds on.)
    - **The dial table repeats all of it**, and its ISN is a pure function of
      its source port.

    **Why the fix was abandoned rather than repaired.** Ten review rounds over
    one day found roughly fifty defects in it; six were REGRESSIONS the fix
    itself introduced, and two broke shipped behaviour: an `in_window` gate on
    the FIN arm rejected `cpu`'s own close (`tcp_run` binds `snd_nxt` once and
    never advances it, unlike `tcp_get`), leaking an exporter slot per remote
    run; and the ACK guard, gated on the ACK bit, let a FIN with that bit clear
    be honoured anywhere in a 64 KB window - a one-packet blind teardown, worse
    than the stale-FIN case it was written for. A third removed an existing
    bound: with the persist timer, a conformant peer advertising window 0 holds
    a slot forever, where the pre-change code reaped it in 30 s. The pattern
    across rounds was a repair rate matching the defect rate
    ([`repairing-the-repairs-postmortem.md`](postmortems/repairing-the-repairs-postmortem.md)),
    which is what happens when every change rests on reading.

    **The precondition for trying again is a rig, not more care.** Nothing in
    the tree can reach these paths: SLIRP loses no segments and forges none,
    Parallels and the Pi have no networking at all, and the two-VM socket link
    rotates its source ports so it cannot even collide a 4-tuple on purpose.
    What would reach them: the two-Pi network already on the hardware roadmap,
    or a host-side injector that can forge a segment onto a guest's link (a raw
    socket peer on the QEMU socket netdev, which the two-VM rig already
    proves is possible). The one path that DOES have a rig today - loss
    recovery, via a temporary one-segment drop in `pump_send` plus an HTTP
    fetch, see `testing/testing-qemu.md` - is exactly where the one measured
    finding of the whole arc came from. Build the rig first; it is the part
    that decides whether the next attempt converges.
    **The stack cost is a separate warning**: the abandoned branch's own
    `syn_sent_reset` added a 1600-byte frame that LLVM inlined into `tcp_get`,
    taking the `fetch` path to 32,272 bytes of netd's 32,768-byte stack. Any
    future work here must measure the frame, not just the logic; `netd` has
    overflowed its stack twice already (see `loader.rs`).

  - **The session gate's own harness was blind until 2026-09-07, and its late
    checks proved nothing.** `drive-qemu.py` kills QEMU seconds after its last
    typed step; the gate runs for a minute or more, so its reap and
    slots-come-back checks were measured against a guest that had already been
    killed, and a refusal by a dead process is indistinguishable from the
    export refusing. It surfaced as flake (11/11, then 0/3 reaped) and was
    settled by logging netd's connection table rather than by another guess.
    `scripts/run-guest.sh` now boots the guest for the client's whole run and
    asserts it is alive at the end. Re-measured under it: 11/11 three times,
    0 restarts, 0 aborts. Full write-up in
    [`blind-instruments-postmortem.md`](postmortems/blind-instruments-postmortem.md).
  - **What the session gate still does not prove (2026-09-07).** Its last
    check, "the reaped slots are back", closes the held sessions before opening
    fresh ones, so it passes even against a netd that never reaps (measured:
    with `CONN_IDLE_TICKS` multiplied by 1000 the reap check fails 0/3 and this
    one still passes). It verifies slots are usable again, not that the reap
    returned them; the reap check beside it is what can fail for that. Worth
    tightening if the two ever need to be independent.
  - **The refusal side of delegation has no check that can fail.** Every
    `DELEGATE` in the tree is a parent granting to its own child, so nothing
    ever exercises "a task cannot delegate to or for a stranger"; the
    one-clause rule rests on reading. Rig item: a deliberately misbehaving
    `/bin` program that issues `DELEGATE(grantee=<not its child>, ...)` and
    `DELEGATE(<its child>, <a task it cannot reach>)` and prints the two
    answers, expected `MSG_ERR_DENIED` both times. Promised in the journal on
    2026-09-06 and not built.
  - ~~**A foregrounded nested shell loses the keyboard after every command.**~~
    **Fixed 2026-09-20**, as the entry "a child shell loses the keyboard
    after its first command" further down: the same defect. `exec
    /EFI/ORBS/SH.BIN`, `fg 6`, log in, `echo hi`: `ps` before showed task 0
    blocked and task 6 runnable, `ps` after showed task 0 runnable and task
    6 blocked, and the next line typed ran in the boot shell. Measured
    2026-09-06 while testing subtree delegation, where it first looked like
    the nested shell's later pipelines had started working: they had, in the
    other shell. The cause was not traced then; "a builtin, no child" was
    wrong, since `echo` is a `/bin` program and its exit is what reverted
    the keyboard to slot 0.
  - **`delegate_net` discards its result**, so the one grant this arc is about
    is the only `DELEGATE` in the shell with no failure signal. **Worse since
    subtree delegation (2026-09-06):** a nested shell that never received
    `TO_NET` itself (netd absent or dead when it was spawned) is refused on
    every grant it makes, so one dropped result silences a whole subtree for
    the boot, with no line anywhere. A future
    `caps_for_slot` edit dropping `TO_NET` from slot 0 would return the tree to
    the pre-fix behaviour with no diagnostic anywhere. A check that cannot
    fail.

- ~~**The 9P export bypasses permissions entirely.**~~ **Closed 2026-08-31** by
  per-user cluster identity (v0.15.0), which this finding specified. `fsd`'s
  `effective_caller` now REFUSES a `NET_TASK` request that states no identity
  rather than falling back to netd's root, and `netd`'s `AsUser::enter` makes a
  `cpu` child inherit the mapped user, so both doors below are shut. The
  residual, that an authorized *machine* may still claim any user with an
  account on the export, root included, is frontier item 1 above, not this
  entry. The finding as originally
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
  [`asking-the-right-question-postmortem.md`](postmortems/asking-the-right-question-postmortem.md).
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
- **Three findings from the 2026-09-13 review of the shared `STACK_PAGES`
  fix (#134), all PRE-EXISTING.** The fix itself, the `GUARD_PAGES` pin and
  the dead `edtest` probe were done in the PR; these are the wider copies of
  the same layout the review found around it.
  - ~~**Nothing checks that a loaded region fits one 2 MB slot.**~~ **Fixed
    2026-09-13** (the PR after #134): `elf_region_size` refuses a region over
    `SLOT_ALIGN` with `LoaderError::RegionTooLarge`, mapped to
    `SPAWN_ERR_TOO_LARGE`, and a `const` assert pins the tail below the slot.
    Witnessed first: a C program with a 1.9 MB `.bss` faulted at its own
    region base with an unknown-instruction exception (its code fetched
    through the aliased table as zeros); with the check it is refused by
    name. The original finding, kept:
    `elf_region_size` sums code pages and the fixed tail with no bound, while
    `build_view` fills one L3 table per view on the strength of a "fits one
    slot by construction" comment. A `.bss` past ~1.7 MB passes the staging
    size check (it is memory size, not file bytes) and the two 2 MB sub-slots
    then share one L3, so the first slot's pages resolve to the second's.
    Wants a `const` assert on the tail against `SLOT_ALIGN` and a runtime
    size check mapped to the existing too-large spawn error.
  - ~~**Twelve prose copies of the stack size, disagreeing with each other and
    with the constant**~~ **Fixed 2026-09-20** (#146): the nine that
    stood after #135, and the nine more the review of #146 found (`useradd`
    twice, `chown`, `edtest`, `netd` four times, the cluster-keys roadmap,
    and `SAFECOPY_MAX`'s doc still describing 8 KB stacks with no guard
    page), now state the relationship (the loader's `STACK_PAGES`, reported
    by `heap_info`) and carry no value. `tree`'s depth cap, the one place
    the value was load-bearing, is computed at start from the stack the
    loader reports instead of from a number in a comment. The two growth
    histories gained their missing last step (32 KB to 40 KB, the
    remote-mount session path). The original finding:
    `processes.md` (8 pages/32 KB and 4 pages/16 KB in
    the same file), `architecture.md` (32 KB, three places), `gap-analysis.md`
    (16 KB), `ROADMAP.md` (32 KB), `syscall-abi`'s `HEAP_INFO` doc and the
    shell's `main.rs` (16 KB), `tree`'s `main.rs` (~32 KB). The number is
    40 KB. State the relationship ("the loader's `STACK_PAGES`") where a
    value is not load-bearing, and the value in one place where it is.
    *Three of the twelve went in #135 (2026-09-13): `processes.md`'s
    memory-model paragraph, `architecture.md`'s layout line, and the ABI's
    `HEAP_INFO` doc. The rest stand.*
  - ~~**`activate_task` clamps an out-of-range view index silently**~~
    **Fixed 2026-09-19** (#136). It was `L0_TABLES[view.min(MAX_EL0_REGIONS
    - 1)]` in `mmu.rs` (twice: `activate_task` and `switch_full`), so a task
    past the last view would have run under another task's translation
    tables, the opposite of a fail-safe, and nothing documented it. Noticed
    in passing by the fourth review of #135 (2026-09-13). Unreachable while
    `NUM_TASKS == MAX_EL0_REGIONS`, which the array-typed parameters of
    `install_identity_map` and `rebuild_with_el0_regions` already enforced
    (a "must stay equal" comment on each side undersold that). Now
    `MAX_EL0_REGIONS` is *defined as* `tasks::NUM_TASKS`, one definition
    rather than two literals, and both lookups are plain indexes. The PR's
    first round added a `const` assert instead and claimed a mutation showed
    it could fail; the review found the same mutation fails the build with
    the assert deleted, so it proved nothing about the assert. The
    definitional constant needs no such proof. The fifth review then held
    that with the clamp gone a bad slot would panic, and a panic is silent
    (the entry below), so the refusal the original finding offered as the
    alternative landed too: `l0_table` reports the slot and the view count
    through the console and halts instead of indexing. A mutation (a
    temporary `activate_task(NUM_TASKS)` after the install) printed the
    line and halted with no aborts in QEMU's trace. **The sixth review
    then showed that mutation was not in situ**: every runtime caller
    indexes `tasks::TASKS[next]` before it switches, so a bad slot panics
    there first and the refusal never prints; and at the boot-time
    `switch_full` call there is no console yet on framebuffer-only
    machines. The refusal guarded the lookup only. The check that can fail
    where the slot is made is the `TaskIndex` newtype, the next entry but
    one, landed as the follow-up.
  - **The kernel has no panic handler of its own, and a post-exit panic
    is silent.** `kernel/Cargo.toml` takes the `uefi` crate's
    `panic_handler` feature. **Measured on QEMU (2026-09-19)**, with a
    temporary `panic!()` placed right after the identity-map install and
    QEMU's own exception trace on: the last console line was the install
    message, nothing further printed, the trace showed no abort of any
    kind, and QEMU exited about a minute into the boot on a PSCI call,
    which is the firmware's shutdown. That matches the crate source
    (`println!` degrades to `log::debug!` once boot services are gone, a
    long spin, then `ResetSystem` with `SHUTDOWN`), but the run is the
    evidence. On Parallels, where the framebuffer is the only console,
    that is indistinguishable from a hardware crash. Found by the review of
    #136 (2026-09-18) while checking what "panics here" would actually do.
    It is not hypothetical: `mmu.rs` has two post-exit `expect` sites
    guarding "install_identity_map ran first" (`rebuild_with_el0_regions`
    and `ram_span`), plus an `unwrap` inside `install_identity_map` itself
    that re-reads the map it just stored. Wants a kernel
    `#[panic_handler]` that reports the message and location through
    `console::println!` and halts, the way `exceptions.rs` already reports
    a fault. **First drop the `panic_handler` feature from the `uefi`
    dependency in `kernel/Cargo.toml`**, or the build fails on a duplicate
    `panic_impl` lang item.
  - ~~**The task slot passed to `activate_task`/`switch_full` is a bare
    `usize`, in range only by the discipline of its callers.**~~ **Fixed
    2026-09-19** (#137). Every caller derived it in `tasks.rs`
    from the slot count or from a value `syscall.rs` had already
    range-checked, so the index held; but nothing tied those checks to the
    index, and #136's reviews found every prose inventory of the callers
    written to document the discipline wrong within a round. Now
    `tasks::TaskIndex`, a `usize` below `NUM_TASKS` by construction, with
    its field private to a nested module so that even `tasks.rs` cannot
    spell `TaskIndex(x)`: the constructors are `new` (checked, `None` past
    the end; used wherever a caller-supplied slot becomes one, which is
    a grep and not a list here), the constants `FIRST` and `IDLE`, `all`, `current_index` (which
    rebuilds the one `set_current` stored), and `succ` (the
    round-robin step, from a slot that already exists). There is no total
    `usize -> TaskIndex` constructor: the first round had a `wrapping`
    modulo, and its review found it used as a clamp twice (the
    `current_index()` read, and `next_runnable`'s fallback), so it is
    gone. `CURRENT` is atomic storage inside the type's own module with
    `set_current(TaskIndex)` as its only store (a third review caught the
    second commit swapping the atomic for an `UnsafeCell`, which had
    turned race-freedom into a claim); `next_runnable` takes and returns
    the type. **Scope**: the type covers the view switch and the
    scheduler's own slot. `task_exists`, `may_send`, `send_message` and the
    per-task arrays still take a `usize`, so an arm that hands a typed slot
    to one of them unwraps it, harmlessly; `is_live` remains an alias of
    `task_exists` for the two identity arms, which are also the last
    guard-then-index pairs (three names for one predicate; the next step
    of this scope, with a `switch_to` helper for the five-site switch tail
    and one predicate-driven scan for the two runnable loops). Converting
    those is a separate change: **done 2026-09-20 in #141, merged as #144**
    (its base was deleted under it and GitHub closed it), no behaviour
    change (`switch_to`, `scan_from` over `is_runnable`, `is_live` and
    `task_exists` gone with the identity arms on `live_index`, and
    `live_occupant(id)` for the two identity-to-live-slot lookups).
    `may_send`, `send_message` and the per-task arrays still take a
    `usize`. `activate_task`,
    `switch_full`, `build_view` and `block_current_and_switch_to`'s
    `prefer` take it; every table index in `mmu.rs` is typed, and
    `l0_table`'s runtime refusal is gone because the case is unspellable.
    Named `TaskIndex` because `TaskSlot` was already the saved-context
    cell. Shown to fail at compile time: `activate_task(11)`,
    `set_current(3usize)` and `next_runnable(3usize)` (type mismatch),
    `TaskIndex(11)` in `mmu.rs` and in `tasks.rs` outside the
    `task_index` module (private field), each
    restored and the files confirmed identical afterwards. Boot-tested on
    QEMU after each commit with the guest driven through login, `echo`,
    `uptime`, `ls` and `cat` (spawn, the `MSG_CALL` handoff, the exit
    paths), zero fault lines. Found by the second review of #136
    (2026-09-18). Pairs with the `TaskIdentity` newtype still on the small
    list; #137's review also counted the hand-rolled `UnsafeCell` +
    `unsafe impl Sync` wrappers, each restating the single-core argument:
    eleven in `tasks.rs` (`StateSlot`, `RegionSlot`, ...) and three in
    `mmu.rs` (`Table`, ...), by `grep -c 'unsafe impl Sync'`. A generic
    `SyncCell<T>` holding that argument once was a do-when-touched item:
    **done 2026-09-20 in #142** (`kernel/src/synccell.rs`). The wrappers
    across `tasks`, `mmu`, `syscall`, `console`, `supervisor` and `xhci`'s
    controller handle are `SyncCell<T>` now, no behaviour change (the
    measured count is in the module doc, and only there: a review found
    this entry, the source map and the module giving three different
    numbers for one property); the bound is `T: Send`, so
    the two cells that hold a raw pointer (`fbdev`'s `FbCell`, `mmu`'s
    `StoredMapCell` around the uefi crate's `MemoryMapOwned`) keep their
    own wrapper and say why, the two aligned ones (`Table`, `IdleRegion`)
    keep a `#[repr(align)]` newtype around a `SyncCell`, and the DMA rings
    in the virtio and xHCI drivers keep theirs, their SAFETY being about
    the device writing memory. `gic.rs`'s `GicCell` stays a `Cell` for its
    by-value `get`/`set`.
  - ~~**A task could `KILL` itself.**~~ **Fixed 2026-09-19** (#137). The
    `KILL` arm refused the protected slots and empty slots, and nothing
    else; `kill_task`'s doc said the syscall layer guaranteed the victim
    was never the running task because "only tasks >= 2 can be killed, and
    the caller is always whichever task is running", true when slot 0 was
    the only task issuing syscalls and false since spawned programs run.
    Witnessed first: a child shell in slot 6 ran `kill 6`; the kernel
    printed "task 6 killed", the `eret` landed in the freed region (EL0
    instruction permission fault), and the fault handler tore the slot
    down again. Now refused like WAIT and MSG_CALL, and all three return a
    code of its own, `TASK_ERR_SELF` (`MAX-42`, the floor's old value; floor to `MAX-43`), because
    `TASK_ERR_PROTECTED`'s one explanation names the permanent slots, the
    wrong one for the slot the caller just named (the shell's error
    printer distinguishes it today; `libc`'s `sys.h` mirrored no task code
    until 2026-09-20, when it gained all three, pinned by the wire check,
    and `cremote`/`cwrite` name the one their requests can meet, a server
    that is not running, instead of "failed");
    `kill_task` checks it again as the mechanism (a reported
    halt), and the two paths that tear the running task down go through
    `switch_away_from_dead`, which halts rather than resume a torn-down
    context if nothing else is runnable. Found by the fifth review of
    #137; the shell names the self case in its own words, and the ABI
    doc, `shell-commands.md` and `manual.md` state the protected set.
  - ~~**Ctrl+C could tear down a task that had already exited.**~~ **Fixed
    2026-09-19** (#137). `interrupt_key_check` marks the keyboard owner in
    `PENDING_KILL` and the next tick tears it down; the tick's guard
    checked only that the slot was spawnable, not that it was live. A
    foreground program that exited between the mark and the tick was a
    Zombie whose status the parent's `WAIT` may already have collected;
    the second teardown turned that into `TASK_KILLED_STATUS` and cleared
    tables the slot no longer owned. The guard now goes through `live_index`
    too. Found by the ninth review of #137, while the guard was being
    rewritten to go through `TaskIndex::new`; not driven on the guest,
    since `drive-qemu.py` types characters and a Ctrl+C that lands in the
    window between an exit and the next tick is not something the rig can
    place. Read, not run.
  - ~~**The Ctrl+C mark is a bare slot, so a slot re-spawned inside the
    window is the one killed.**~~ **Fixed 2026-09-19** (#138).
    `interrupt_key_check` stored the keyboard owner's slot in
    `PENDING_KILL`; a task that exited, was reaped and whose slot was
    spawned into again before the next tick would have had its new
    occupant terminated as "the foreground task" (#137's liveness check
    narrowed that window and said so). The mark now holds the owner's
    packed identity (generation and slot, the value `SENDER_TASK`
    captures) and the tick kills only if `occupant_is` that identity.
    Shown to fail in situ by mutation: with the mark storing a stale
    generation, Ctrl+C at a foreground `readkey` did nothing and the
    program ran on to a normal exit; restored, the same Ctrl+C terminated
    it. The re-spawn window itself is not driven (the rig cannot place a
    keystroke between an exit and the next tick); the identity check is
    what the mutation exercises. Found by the eleventh review of #137.
  - ~~**A background task can steal the keyboard owner's keystrokes.**~~
    **Fixed 2026-09-19.** `TRY_READ_CHAR` and `READ_CHAR` polled for
    whoever called; only the tick's wake-check gated on `INPUT_OWNER`.
    Witnessed first with `readkey`'s new `poll` mode (a spinner on
    `try_read_char`, added as the observer): `exec /bin/readkey poll`, then
    `echo hi` at the shell arrived as `ehoi`, the poller having echoed the
    stolen `c`, ` ` and `h`. Both arms now gate on the owner: a non-owner's
    `try_read_char` answers `NO_CHAR` without consuming and its `read_char`
    blocks until ownership reaches it. After: the same `echo hi` prints
    `hi` with the poller running. `architecture.md`'s claim that only the
    owner receives keystrokes is true at the syscalls now, not only at
    the wake-check. Found by the second review of #138.
  - ~~**A child shell loses the keyboard after its first command.**~~
    **Fixed 2026-09-20.** Spawn a shell from a shell (`/EFI/ORBS/SH.BIN`,
    slot 6), run any command in it (`echo hi`, slot 7): when slot 7 exited,
    keyboard ownership reverted to task 0, the boot shell, so the child
    shell's next prompt never received input. The revert-on-death rule in
    `revert_input_owner_if` was "to task 0". Seen 2026-09-19 while driving
    the self-`KILL` witness for #137. Now `FG` records who held the keyboard
    when it handed it over (`PREVIOUS_OWNERS`, a packed identity per task,
    not a slot), and the owner's death returns it to that task if it is
    still the same occupant and live, else to task 0. The previous owner
    rather than the parent because every previous owner was a valid `FG`
    target when recorded, so the revert can never land on a protected task;
    a parent can be a server (netd's remote-exec children). Witnessed with
    `drive-qemu.py` on `esp.img`: `exec /EFI/ORBS/SH.BIN`, `fg 6`, log in,
    `cd /EFI`, `echo hi`, `pwd`. Before: `/` (the boot shell answered) and
    `ps` showed task 0 runnable, task 6 blocked. After: `/EFI`, task 6
    runnable, task 0 blocked. The chain too: with the nested shell run as a
    foreground command, Ctrl+C at a grandchild `readkey` returned the
    keyboard to the nested shell (`pwd` gave `/EFI`), and Ctrl+C at the
    nested prompt killed it and returned the keyboard to the boot shell
    (`pwd` gave `/`). Zero aborts in QEMU's trace for both runs. The
    older entry of 2026-09-06 below ("a foregrounded nested shell loses
    the keyboard after every command") was the same defect seen from the
    `exec` then `fg` flow. The second review of #140 found the chain could
    still fall to task 0 past a live shell: a link that dies while NOT the
    owner (a nested shell killed from a shell below it) took its entry
    with it. Every entry naming a dying task is now re-pointed at that
    task's own previous owner; witnessed three shells deep (`kill 7` from
    task 8, Ctrl+C at 8's prompt, `pwd` answered by task 6). The third
    review found the splice could make a task its own previous owner
    (`fg 6` from shell 7 makes a cycle, `kill 7` from 6 splices 6 onto
    itself) and the next revert then stranded the keyboard on the empty
    slot: witnessed, no prompt ever came back. An entry naming its own
    task counted as none from then until #143 made the cycle unspellable
    (the next entry). The recipes are `make test-keyboard-chain`.
  - ~~**`FG` pushes onto the keyboard chain unconditionally**~~ **Fixed
    2026-09-20 (#143).** `fg` to a task already in the chain recorded a
    cycle rather than returning along it, and this entry first claimed
    "nothing strands": the review of #142 found the case that does. Four
    deep, `fg 7` from shell 8 overwrote 7's link to 6; `kill 8` then made
    7's entry name itself, the normalisation read that as none, and Ctrl+C
    at 7 sent the keyboard to task 0, blocked in `WAIT` on 6, while 6 sat
    at a live prompt nothing could reach (measured: the prompt printed,
    `pwd` was never answered). `set_input_owner` keeps the table a stack
    now: a walk down from the holder, and if it reaches the target every
    link walked is cleared (a pop), else the target's entry becomes the
    holder (a push). No cycle is spellable, the self-naming normalisation
    is gone, and the four recipes are `make test-keyboard-chain`
    (`scripts/test-keyboard-chain.sh`), each graded on the lines its
    negative control lacked. Found by the third review of #140 and the
    review of #142.
  - ~~**Ctrl+C at a nested shell's own prompt kills the shell**~~ **Resolved
    2026-09-21: it DETACHES now, it does not kill.** `interrupt_key_check`
    tells the two cases apart exactly as this item proposed: if the owner's
    previous owner is blocked in `WAIT` (`WaitReason::TaskExit`) on it, it is
    a foreground command and Ctrl+C terminates it; if that previous owner is
    at its own prompt (not waiting), it is a session handed over with `fg` and
    Ctrl+C reverts the keyboard to it WITHOUT killing the session, which `fg`
    resumes. Not the literal "pass the byte through, as for task 0" first
    filed here: that would have trapped the keyboard, since Ctrl+C was the only
    way back from a `fg`-handed shell and its `exit` only logs out. Detach
    preserves the session AND stays escapable. Driven by
    `scripts/test-keyboard-chain.sh` recipes 5 (a handed-over shell survives
    Ctrl+C, `ps` shows it blocked not unused) and 6 (a foreground shell is
    still terminated), each with a measured always-kill mutation control. The
    remaining type-ahead sub-point (a nested shell can lose the first
    characters of a line typed while it is still printing its prompt) is
    UNCHANGED and still filed. Found by the second review of #140.
  - **`FG` is caller-unchecked.** A task that does not hold the keyboard can
    foreground any spawnable task, and the recorded chain then names the
    holder, not the caller, so the revert can route the keyboard to a task
    that never asked for it. Not new (it was always unchecked), but the
    shell's "reverts to this shell" now rests on caller == holder, which is
    true because only the owner can read a command line and nothing else
    calls `FG`. ~~Refusing `FG` from a non-owner would make the chain
    trustworthy by construction.~~ **Done 2026-09-21:** `FG` from a task that
    does not currently own the keyboard is refused `TASK_ERR_PROTECTED`, so the
    property holds by construction, not by that argument. Found by the second
    review of #140.
  - ~~**The stack top is hand-derived as `base + size` at seven sites across
    three files**~~ **Fixed 2026-09-20** (#146). It was spelled inside
    an identical `Context` literal in `tasks.rs` five times, `supervisor.rs`
    and `syscall.rs`, so anything ever placed above the stack had to be
    found at all seven with the compiler flagging none. Now one private
    `region_end` in the loader is read by `tail` (the stack area) and by
    `LoadedProgram::end()`/`stack_top()`, `initial_context()` beside them
    builds the context every loaded program starts in, and the seven sites
    plus `main.rs`'s five boot-log lines call those. The idle task's
    context stays hand-built: it is not a loaded program. The first cut put
    the constructor in `exceptions.rs` and claimed `stack_top` was the one
    derivation while `tail` and the log lines still spelled their own; the
    review of #146 caught both.

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
  [`testing-pi4.md`](testing/testing-pi4.md) §8 and written up as its Risk 4b, so the
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
- **The shell has no quoting** (carried in the standup since 2026-09-03, recorded
  here 2026-09-07). The word splitter is whitespace-only, so an argument
  containing a space cannot be spelled: `echo "a  b"` prints the quotes and
  splits on the run of spaces.
- **`resolve` blames the boot for a mid-call death.** Its only failure text is
  "no network server this boot" (`programs/netutils/resolve/src/main.rs`),
  printed also when `netd` died during the call and the supervisor will have it
  back within a tick.
- **`fsd` answers a request naming a fid it does not hold with bare
  `FS_ERROR`** ("bad or not-yours", `programs/servers/fsd/src/main.rs`), which
  every client renders as "no such file or directory" for a request that named
  no path. The same shape `FS_ERR_NO_SUCH_VERB` was reserved to fix for verbs.
- **C programs receive no `argv`.** `libc/src/crt0.c` calls `main(void)`; a C
  program cannot read its own command line while a Rust one can, so no ported
  tool that takes a filename argument works yet.
- **Two error tables name the same codes.** The shell's `print_fs_error` and
  `ulib::fs_error_msg` are hand-kept copies, and they have already disagreed
  once (the stale 8.3 filename message, journal 2026-09-05). The split exists
  because the shell has no `ulib` dependency, the same reason its file readers
  are separate.
- **`netd`'s module doc ends at "Stage 2b"** (real ARP, IPv4, ICMP, guest-side
  ping). TCP, the HTTP server, the 9P export gateway, cluster auth, `cpu` remote
  execution and the `/net/tcp` files are undescribed at the top of the file, so
  the first thing a reader sees is a claim four arcs behind the code.
- **`make clean` is the only pruning of `target/`**, which is 1.0 GB on
  2026-09-07 on a tree that builds 62 binaries; there is no partial clean, so
  recovering the space means rebuilding everything.
- ~~**A C `open()` silently truncates a path longer than 95 bytes, and opens
  whatever the first 95 bytes name.**~~ **Fixed 2026-09-07, the same day:**
  `resolve_path` reports overflow, `open()` records `FS_ERR_CLIENT` and returns
  -1, and `cremote` asserts the refusal with that status; the check fails
  against the truncating version (it answered "no such file") and passes with
  the fix, from `/` and from a subdirectory. A cwd the kernel reports as too
  long now fails the same way instead of being replaced by `/`. As found:
  `resolve_path` in `libc/src/file.c` copied
  the caller's path into a 96-byte buffer (`PATH_MAX_C`) and cuts it there
  without an error, before namespace resolution. **Measured 2026-09-07** on the
  FAT32 image with a file whose absolute path is exactly 95 bytes and a C
  program opening that path with three characters appended: `open(O_RDONLY)`
  returned fd 3 and `read` gave the 95-byte file's 26 bytes; `open(O_WRONLY |
  O_TRUNC)` returned fd 3 and `ls -l` then showed the 95-byte file at size 0.
  Exit code 0 throughout, no message anywhere. The comment on the *other*
  buffer in the same file (`FSPATH_MAX_C`, raised to 256 on 2026-09-05) warns of
  exactly this, "a truncated path with O_TRUNC truncates the WRONG FILE with no
  error anywhere", and the buffer before it still does it. Read, not measured:
  a relative path is capped the same way after `cwd` is prepended, so it is
  reached sooner. The 09-07 audit's "a path over 255 bytes" as a client-side
  failure was a reading of that comment; a long path never reaches
  `FS_ERR_CLIENT`, it reaches `fsd` shortened. Fix shape, its own change:
  `resolve_path` reports overflow instead of truncating, `open()` records
  `FS_ERR_CLIENT` and returns -1, and `cremote`'s `must_leave` asserts it, the
  check that would have failed against today's code. Found while fixing #119,
  in the same function.
- **The shell drops input past 128 bytes without saying so.** `BUFFER_SIZE` in
  `programs/shell/src/main.rs`; `on_byte`'s own comment reads "Buffer full:
  silently drop further bytes". Met by accident on 2026-09-07 while measuring
  the item above: a 129-byte `write <95-byte path> hello-from-the-95-byte-file`
  wrote `hello-from-the-95-byte-fil`. The dropped byte is not echoed, so a
  human sees the line stop, but a driven or pasted line loses its tail and the
  command runs on the rest, which for `write` and `cp` means the wrong content
  or the wrong destination. A 95-byte path plus a second one already exceeds
  the line. Same disease as the item above, one layer up.

# Archive — assistant memory notes, as they stood 2026-09-02

A verbatim snapshot of the assistant's memory directory for this project taken
just before it was consolidated from 26 files (62 KB) down to 20 (~22 KB).

**This is not the reference**, for the same reason as the build logs beside it:
almost everything below is a restatement of something the repo already records
properly — `CHANGELOG.md`, `journal.md`, `roadmap*.md`, the postmortems, and
`CLAUDE.md`. That duplication is exactly why it was trimmed. Memory keeps only
what those cannot: who Hans is, how he wants the work done, live direction, and
the traps worth reaching for again.

Kept for archaeology, and so that nothing removed from memory was removed
without a copy. Six notes were retired outright and survive only here:

| Retired note | Now covered by |
| --- | --- |
| `project-cluster-keys-arc` | `../cluster-keys-postmortem.md`, `../roadmap-cluster-keys.md` — the arc shipped as v0.16.0 |
| `reference-write-the-check-from-the-bug` | folded into `reference-a-check-that-cannot-fail` |
| `reference-bind-credential-at-send` | shipped and documented: `CLAUDE.md`'s `tasks.rs` `SENDER_CREDS` entry |
| `reference-redox-os-cousin` | `../research-redox-and-pi.md` Part 1 |
| `reference-osdev-wiki-and-qemu-pi` | `../resources.md`, `../testing-pi4.md` |
| `reference-prlctl-parallels-testing` | `../testing-parallels.md`, `CLAUDE.md`'s `make test-parallels` |

The `[[double-bracket]]` links below point at memory notes, not at files in this
repository, and will not resolve here.

---


## `MEMORY.md`

```markdown
# Memory index

One line per memory; the memory file itself holds the detail. Keep hooks short —
if a line starts growing a status report, that belongs in the file it points at.

**The repo is the record.** `docs/CHANGELOG.md`, `docs/journal.md`,
`docs/roadmap*.md` and the postmortems (all indexed from `CLAUDE.md`) hold what
was built and why. Memory holds only what those don't: who Hans is, how he wants
the work done, live direction, and traps worth reaching for again.

## Who Hans is, and how he wants me to work
- [User: Ouroboros role](user-ouroboros-role.md) — builds Ouroboros himself; values shareable postmortems
- [Feedback: real-hardware debugging discipline](feedback-realhw-debugging.md) — one variable per round trip
- [Feedback: don't stop short](feedback-dont-stop-short.md) — Hans decides when the day ends

## Project state and direction
- [Project: Ouroboros status](project-ouroboros-status.md) — where real status lives + the lasting platform limits
- [Project: cluster vision](project-cluster-vision.md) — the north star; Phases 0–4 done, SSI out of scope
- [Project: completed arcs](project-completed-arcs.md) — what's done, and only the constraints they left live
- [Project: cluster-keys arc](project-cluster-keys-arc.md) — per-machine keys, released v0.16.0; per-*user* keys left
- [Project: GUI-stack direction](project-gui-stack-direction.md) — parked want; needs a mouse + drawd
- [Project: physical-hardware target](project-physical-hardware-target.md) — Parallels parked, 2× Pi 4 next (+ a queued NET_WAIT task)
- [Project: release process](project-release-process.md) — per-arc minor bumps; ship the .dmg

## How to work: traps and techniques to reach for again
- [Reference: a repair is a change](reference-a-repair-is-a-change.md) — fixes carry the same defect rate; keep them bug-sized
- [Reference: a check that cannot fail](reference-a-check-that-cannot-fail.md) — mutate it, or it proves nothing
- [Reference: write the check from the bug](reference-write-the-check-from-the-bug.md) — not from the shape of the fix
- [Reference: fixed one layer away](reference-fixed-one-layer-away.md) — a caller flattened it; run it, don't read it
- [Reference: compute, don't transcribe](reference-compute-dont-transcribe.md) — evaluate the definition, don't table it
- [Reference: unspellable, not un-grepped](reference-unspellable-not-ungrepped.md) — required param beats opt-in
- [Reference: bind the credential at send](reference-bind-credential-at-send.md) — SENDER_ID, not GET_ID
- [Reference: restatements drift](reference-restatements-drift.md) — prose copies rot invisibly
- [Reference: &str-slice PIE trap](reference-str-slice-pie-trap.md) — slice bytes, not &str, in /bin code
- [Reference: firmware hides robustness bugs](reference-firmware-hides-robustness.md) — host-harness the module
- [Reference: stacked-PR squash trap](reference-stacked-pr-squash-trap.md) — don't squash a base branch

## Testing rigs
- [Reference: QEMU stdin drives the guest shell](reference-qemu-stdin-guest-shell.md) — unattended shell tests
- [Reference: prlctl Parallels testing](reference-prlctl-parallels-testing.md) — scripted real-hardware runs (parked)

## Outside reading
- [Reference: Redox OS cousin](reference-redox-os-cousin.md) — the closest shipped relative; what to steal
- [Reference: OSDev wiki + QEMU-Pi](reference-osdev-wiki-and-qemu-pi.md) — Pi bring-up reference
```

## `user-ouroboros-role.md`

```markdown
---
name: user-ouroboros-role
description: "Hans is building Ouroboros, a from-scratch ARM64 microkernel OS in Rust, and personally runs real-hardware tests on Parallels"
metadata: 
  node_type: memory
  type: user
  originSessionId: ec4d3ecd-0891-4b97-ac94-ddf166d56b24
  modified: 2026-08-26T19:51:23.683Z
---

Hans is building [[project-ouroboros-status]], a from-scratch ARM64 (aarch64) microkernel OS in Rust, developed primarily against QEMU. He does his own real-hardware testing loop. (Historically via Parallels Desktop on Apple Silicon — but **as of 2026-08-26 Parallels is PARKED**: QEMU incl. the two-node cluster is good enough for now, and the physical target going forward is 2× Raspberry Pi 4. See [[project-physical-hardware-target]]. Don't push Parallels testing unprompted.)

The historical Parallels loop (kept for context): boots a rebuilt `esp.hdd` in Parallels, reports back with a screenshot (often via the framebuffer console, since there's no working serial log on Parallels — see [[project-ouroboros-status]]) plus a one-line description of what happened. He's willing to do many real-hardware round trips in one session when a bug needs iterative narrowing (a single day's session did 10+ real-hardware boots chasing a chain of five distinct hardware bugs).

He explicitly values documenting dead ends and debugging methodology, not just final working code — he asked, unprompted, for a debugging postmortem of a hard bug-hunting session to be written up as a standalone document "useful for other people making their own OS," not just kept in the project's own internal history files.

**How to apply:** when work involves a real-hardware round trip, make each one count — target one falsifiable hypothesis per test rather than "let's see what changes," and say so explicitly before asking him to test. When a session produces a hard-won debugging story, proactively suggest (or just do it) writing it up as a shareable artifact, not only updating the project's own internal docs.
```

## `feedback-dont-stop-short.md`

```markdown
---
name: feedback-dont-stop-short
description: "Hans pushes back when I wrap up a session prematurely - offer the next step rather than framing work as done for the day; he'll say when to stop"
metadata:
  type: feedback
---

When I framed a long session as finished ("that's a good place to stop for the
day"), Hans corrected it directly: *"Well, where I'm at, it's still only
afternoon. I can work a couple more hours."*

**Why:** I had been inferring a stopping point from session length and the
volume of work done, not from anything he said. He decides when the day ends;
repeatedly proposing to stop reads as me deciding for him, and it cost real
working time.

**How to apply:** finish the current piece, report it, and name the next
concrete step as available work — not as something to defer. If a genuine
reason to stop exists (a risky merge better done fresh, an unreviewed surface),
say *that specific reason* rather than a generic wrap-up. He asks "what's the
safest next step?" when he wants a recommendation, and that question means
"which one", not "should we continue". He'll say when to stop.
```

## `feedback-realhw-debugging.md`

```markdown
---
name: feedback-realhw-debugging
description: "Discipline for real-hardware (Parallels) debugging rounds on Ouroboros — one variable per round trip, diagnostics must survive a later crash"
metadata: 
  node_type: memory
  type: feedback
  originSessionId: ec4d3ecd-0891-4b97-ac94-ddf166d56b24
  modified: 2026-08-16T17:35:59.873Z
---

When debugging a hardware/driver issue on Ouroboros that requires a real Parallels-hardware round trip (the user boots a rebuilt image and reports back a screenshot), change and test exactly one variable per round trip, and say up front what result would confirm vs. refute the hypothesis. This was validated repeatedly across a real session that found five independent hardware bugs in one day by doing this — each round trip was aimed at a specific falsifiable question, not "let's see what's different now."

**Why:** each real-hardware round trip costs the user real wall-clock time (rebuild, re-image, reboot, screenshot) and is much more expensive than the QEMU dev loop. Bundling multiple speculative changes into one test makes a confirming or refuting result ambiguous about *which* change mattered.

**A load-bearing constraint that shapes what diagnostics are worth adding:** Parallels has no working byte-stream console for this guest (see [[project-ouroboros-status]]) — the only console once boot services exit is a write-only GOP framebuffer text console that *clears the screen* when it installs. Any `log::info!` printed during UEFI boot services is permanently lost the instant that happens. This means:
- Never rely on a boot-services-only log line to still be visible if something later in boot crashes — re-print anything important through the post-exit console instead (see how `pci::discover_xhci`'s command-register diagnostic was restructured to return a value `main.rs` re-prints post-exit, specifically so it would survive a later crash screenshot).
- A diagnostic print that fires on *every* iteration of an unthrottled polling loop (e.g. every USB transfer poll) will flood the small on-screen buffer and scroll the actually-useful lines away within seconds — gate diagnostics to fire only on a state *transition* (error↔ok, value changed), not unconditionally, or the fix for the bug you're chasing will be undiagnosable through screen-flood alone.

**How to apply:** before asking for a real-hardware test, check that (a) the test isolates one hypothesis, (b) any new diagnostic will still be visible if the boot crashes partway through, and (c) it won't flood the screen if it runs in a hot loop.
```

## `project-cluster-keys-arc.md`

```markdown
---
name: project-cluster-keys-arc
description: "Per-machine Ed25519 keypairs replaced the shared cluster key - built 2026-08-31, reviewed and released as v0.16.0 on 2026-09-01. COMPLETE: PRs #47-#68 all merged, nothing open"
metadata:
  type: project
---

The per-machine-keypair arc (roadmap item 1's "cheaper step first") was built in
ONE DAY, 2026-08-31, as fourteen merged PRs plus one open.

**STATE: COMPLETE.** Released as **v0.16.0** on 2026-09-01
(github.com/hansolovkarlsson/Ouroboros/releases/tag/v0.16.0). PRs #47-#68 all
merged; nothing open. The second deliberate wire flag day (AUTHNP02 -> 03, the
retired format refused outright), so both ends of a cluster upgrade together.

**The review cost as much as the build.** #60 took FOUR review rounds, then
eight one-subject follow-up PRs (#61-#68). Round 1 found the arc's bugs; rounds
2-4 mostly found the previous round's REPAIRS - see
[[reference-a-repair-is-a-change]] and `docs/repairing-the-repairs-postmortem.md`
(the 27th). One item is deliberately open and parked on Pi hardware: NET_WAIT is
not a sleep, and QEMU cannot test the fix (see
[[project-physical-hardware-target]]).

**State as originally recorded, end of 2026-08-31:**
- **#47–#59 merged to `main`.** Plan doc, `ed25519` crate (steps 1–5),
  `clusterkeys` + `clusterkey` (6a–6c), verify (7), sign-out (8), reply
  signing (9), whole-arc audit + 10a (#59).
- **#60 OPEN, unreviewed** — 10b, the flag day (−412 lines; `hmac.rs` deleted).
  Branch `cluster-keys-10b`. Hans reviews PRs before merge; #59's review found a
  HIGH, so #60 should get the same treatment.
- **Step 11 partly done**: postmortem, journal, CHANGELOG, roadmap-completed and
  the roadmap all written (committed on the 10b branch). **The RELEASE is not
  cut** — v0.16.0 is the obvious number. See [[project-release-process]] for the
  lockfile sequence that worked for v0.15.0.

**What shipped:** each machine holds `/etc/cluster/id` (0600 private key),
`id.pub`, and `authorized` (one line per peer: `name ipv4 pubkey-hex`). Every
request AND reply is Ed25519-signed, domain-separated. `\CLUSTER.KEY` is gone
entirely. Fail-closed both directions, and the boot line says WHICH half is
missing.

**The asymmetry to remember**: an exporter looks a peer up BY KEY (it is offered
one; the only question is whether it is allowed), a client looks up BY ADDRESS
(it is offered nothing and must know whose signature will do). That is why an
authorized line carries an address as well as a key.

**Why:** with a symmetric key, the ability to VERIFY is the ability to FORGE —
sound at two nodes, false at three, with no revocation short of re-keying
everyone. **How to apply:** revocation is now "delete a line". Two costs it
introduced: `AUTHORIZED_MAX` (1 KB) caps a cluster at ~12 peers, and
`clusterkey new` REFUSES without real entropy, so Parallels and the Pi (no
virtio-rng) need keys staged at build time.

**Still open, and named honestly:** keys are per-MACHINE, not per-USER — an
authorized machine can claim any of its own users' names. That is the residual a
master auth server would close; the roadmap evaluates it.

`docs/cluster-keys-postmortem.md` (the 26th) is the retrospective;
`docs/roadmap-cluster-keys.md` is the step log. See
[[reference-a-check-that-cannot-fail]], [[reference-compute-dont-transcribe]],
[[project-cluster-vision]], [[project-completed-arcs]].
```

## `project-cluster-vision.md`

```markdown
---
name: project-cluster-vision
description: "Ouroboros's long-term north star - a Plan 9-style distributed resource-sharing cluster; the phase spine, what is explicitly out of scope, and where the build log now lives"
metadata:
  node_type: memory
  type: project
---

**The direction Ouroboros is ultimately aiming at** (Hans's years-long personal
goal, committed 2026-08-24): a **Plan 9-style resource-sharing cluster** -
several Ouroboros machines, each exporting resources as file trees, each
composing a private namespace view of the whole cluster's storage/devices/
services. A machine reads another's disk as if local, uses another's network,
runs a program where the CPU/data lives.

Phase spine (full plan `docs/roadmap-cluster.md`, per-phase design docs
`docs/roadmap-cluster-phase0..4.md`):
- **Phase 0** - local namespace + one uniform 9P-ish verb set (retires fsd/cond/
  netd's three bespoke protocols; consumer = multi-mount; delegation becomes
  `bind`).
- **Phase 1** - 9P-over-TCP (export + remote-mount).
- **Phase 2** - two-node disk sharing (THE "is it doable?" milestone - yes).
- **Phase 3** - all resources as files, remotely mountable (/proc, /dev/cons, /net).
- **Phase 4** - remote execution (Plan 9 `cpu`: run there, namespace imported).
- **Phase 5** - explicit distributed compute (frontier/research, not started).

**Explicitly OUT of scope (the mirage):** shared memory across machines /
transparent single-system-image / transparently splitting one computation across
CPUs. Network latency ~10,000x RAM makes it unusably slow or non-transparent;
every SSI/DSM attempt found the transparency leaks. The achievable substitute is
**explicit** "ship work to the data/CPU" (Phase 4). If a future session sees
drift toward "make the RAM one pool," that's the drift to catch.

Why it was doable: the two hardest prerequisites already existed and were
hardware-confirmed - a microkernel with servers-over-IPC (fsd/cond/netd) and a
working TCP stack (netd). Key insight: "remote" is just "the same protocol over
TCP instead of local IPC." Trust started explicit-trusted-LAN (auth added later);
consistency is single-writer + clean-disconnect, documented not coordinated.

**Status (as of 2026-08-31, v0.15.0): Phases 0-4 are COMPLETE and shipped**, plus the
export-hardening arcs on top - cluster auth (v0.10.0), /net/tcp dial-out
(v0.11.0), dial-in (v0.12.0), reply-auth/mutual auth (v0.13.0), and chunked cpu
output streaming (v0.14.0), and PER-USER IDENTITY (v0.15.0, the current tag - a
remote request carries the caller's name inside the MAC, resolved through the far
side's own /etc/passwd, so the cluster no longer serves everything as root). In Hans's words after that one, "the
cluster feels done" - it was the last obvious gap.

**Then PER-MACHINE KEYPAIRS, the same day** (2026-08-31, unreleased at
end-of-day - see [[project-cluster-keys-arc]]): the shared cluster key is DELETED
and every machine holds its own Ed25519 keypair, authorizing peers by public key.
Which buys REVOCATION - one shared secret made every member interchangeable.

Remaining named candidates:
Phase 5 (needs a concrete workload first), concurrent-writer semantics, truly
unbounded cpu streaming, and the trigger-gated tier-2 security work (replay
protection, PER-USER keys - keys are per-MACHINE now, so an authorized machine
can claim any of its OWN users' names - transport encryption, cpu-stream
reply-auth), built only if Ouroboros leaves a trusted network. Also newly recorded: a measured ~1-in-6
remote-op flake on the two-VM socket link, present on main before this arc (see
docs/roadmap.md item 2 and testing-qemu.md's message table - `cat: failed` is
the flake, `cat: permission denied` is a real refusal).

**Testing rig:** two QEMU VMs on a shared L2 socket link (`make run-image-2vm-a`
then `-b`), driven unattended by `scripts/drive-2vm.py`; use the **ext2** images
for anything permission-related (FAT32 records no mode, so such a test passes
before and after a fix). See `docs/testing-qemu.md`.

Day-by-day build notes: `docs/archive/cluster-build-log-2026-08.md` (kept for archaeology; the
repo's own `docs/CHANGELOG.md` + cluster postmortems are the better source).
Releases: [[project-release-process]].
```

## `project-completed-arcs.md`

```markdown
---
name: project-completed-arcs
description: "The finished Ouroboros arcs in one line each, with only the LIVE constraints they left - full record is in the repo's CHANGELOG, roadmap-completed and postmortems"
metadata:
  type: project
---

**Consolidated 2026-09-01** from seven per-arc memories, because the repo records
each arc properly and memory was duplicating it. For any arc's actual story read
`docs/CHANGELOG.md`, `docs/roadmap-completed.md`, and its postmortem under
`docs/` — all indexed from `CLAUDE.md`. What survives here is only the part that
**changes what to do next** and is not stated as an instruction anywhere in the
repo.

## Arcs done (don't re-propose them)

Standalone binaries + PATH/argv/cwd; multi-stage pipelines; the interactive
shell and a bigger `/bin`; the filesystems arc (FAT32/exFAT/ext2, read-write);
disk management; the network stack; cluster Phases 0–4; the small-gaps parking
lot; users/permissions + account management; the libc arc (C runs, picolibc);
per-machine cluster keypairs (v0.16.0, see [[project-cluster-keys-arc]]).

## Live constraints these left

- **Job control stays a builtin.** Don't propose externalizing `ps`/`kill`/
  `wait`/`fg` to `/bin`: an externalized version runs in a *spawnable slot*, so
  it lists itself and its task numbers are racy — the slot a `ps` reports is
  reused by the very next command. Same reason bash makes them builtins. Nor
  `mount`/`erase`/`partition`/`format` (they must run when nothing is mounted —
  they cannot live on the disk they manage), nor `shutdown`/`halt`.
- **`/bin` needs a mounted filesystem** (`make run-image`; real hardware needs a
  USB stick), and the net commands additionally need a NIC + `netd`
  (`make run-image-net`).
- **The C portability waist is the syscall stubs** in `libc/src` —
  `write`/`read`/`open`/`close`/`lseek`/`fstat`/`sbrk`/`_exit`. picolibc needed
  ZERO new porting code because its stdio bottoms out there. Anything else ported
  should aim at the same waist rather than widening the kernel. What is left of
  that arc is "port one more real program" (SQLite, a small C compiler).
- **Home base is `/Users`**, Hans's stated preference — not `/home`.
- **Relocation safety applies to all userland code**: see
  [[reference-str-slice-pie-trap]].

## Resolved, kept only so they are not re-investigated

- **The fsd large-read restart** (v0.4.1) — `read_at` re-walked the FAT chain
  from the start on every call, so a long `cat` ran past the supervisor's wedge
  timer. Fixed with a forward-only chain *position* cache (never data,
  invalidated on write).
- **The xHCI keyboard ↔ USB-storage contention** — both modes fixed; see
  `docs/xhci-keyboard-postmortem.md` and the real-hardware pass in
  `docs/filesystems-arc-postmortem.md`.

Related: [[project-ouroboros-status]], [[project-cluster-vision]],
[[project-release-process]].
```

## `project-gui-stack-direction.md`

```markdown
---
name: project-gui-stack-direction
description: Parked future direction — a GUI stack (SDL/GTK-shaped) on Ouroboros; the design note and its key finding
metadata: 
  node_type: memory
  type: project
  originSessionId: 949ae0fd-032c-469a-9c0a-b451ccbe88d2
  modified: 2026-08-28T21:49:27.648Z
---

A GUI stack on Ouroboros is a parked future direction the user wants to look
into eventually (flagged 2026-08-28). The design reasoning is written up in
`docs/research-gui-stack.md` (expands roadmap item **f**, Graphics/GPU).

**Why:** graphical apps for their own sake — nothing needs it yet (the
framebuffer console + terminal/editor work live fine on the plain framebuffer),
so it's sequenced behind everything with a nearer consumer. The user framed it
as a someday-want, not a next task.

**How to apply (the note's key findings, when it comes):**
- The decisive blocker is the **bulk-pixel path**, NOT toolkit size: `MSG_MAX_LEN`
  is 768B inline with no shared memory, so an 8MB frame ≈ 11,000 messages — SDL's
  "ship a pixel buffer, present it" model is the wrong shape for this ABI.
- The fit is **Plan 9's `/dev/draw`**: compact drawing *verbs* referencing
  *server-side* images (pixels stay in the server, the wire carries verbs) — which
  `cond` already half-is (it sends 8-byte glyph bitmaps to a dumb kernel blitter).
  Build a **`drawd`** server in the `cond`/`fsd`/`netd` mould; bulk image upload
  chunks like the `cpu` `NETOP_RUN_MORE` pull. SDL/GTK become pure-userland client
  libs — no kernel change after `drawd`.
- **The two unlocking steps:** a USB boot-protocol **mouse** driver (a small
  reuse of the xHCI HID path — same interrupt-endpoint lesson as the keyboard) and
  the **`drawd`** draw server. The pixel-transfer model is the day-one decision.
- Don't widen the `FB_*` gate (it's `CON_TASK`-only by design — the isolation
  property); add a draw server instead. Porting REAL SDL/GTK is harder than
  native-shaped equivalents (mmap/pthreads/dlopen; GLib/Cairo/Pango/HarfBuzz).

Depends on roadmap item f's **virtio-gpu** for real mode-setting/acceleration.
Same "write to the ABI's grain, don't emulate someone else's" conclusion as the
POSIX-divergence arc. Related: [[project-cluster-vision]] (the other north-star
direction), [[reference-redox-os-cousin]].
```

## `project-ouroboros-status.md`

```markdown
---
name: project-ouroboros-status
description: "Where the authoritative Ouroboros status lives (CLAUDE.md / docs), plus the few platform constraints that have stayed true across the whole project"
metadata:
  node_type: memory
  type: project
---

**Don't keep a status narrative in memory - the repo carries it, and a memory
copy goes stale within days.** Current truth lives in `CLAUDE.md` (durable
boot/MMU/exception/syscall/task guidance), `docs/CHANGELOG.md` (milestone
record, newest first), `docs/journal.md` (day-by-day narrative),
`docs/roadmap.md` (what's next) and the twenty-plus postmortems under `docs/`.
Read those before describing what the OS can do today.

The constraints below are here because they are *platform facts* that outlived
every milestone, and each one has repeatedly changed what was worth attempting:

- **Parallels has no byte-stream console for this guest.** After
  `exit_boot_services` the only console is the write-only GOP framebuffer, and
  installing it clears the screen - so a boot-services `log::info!` is gone the
  moment anything later crashes. Re-print anything diagnostic through the
  post-exit console. See [[feedback-realhw-debugging]].
- **Parallels has no usable storage controller except USB mass storage** - no
  PCI/SATA/NVMe path, and its virtio devices are virtio-PCI (unsupported here),
  which is also why **the whole cluster is untestable on Parallels** (no NIC ->
  `init_net` is skipped). Cluster work is validated on two QEMU VMs. See
  `docs/testing-parallels.md`.
- **Every user pointer a syscall copies is bounded by MAX_USER_LEN = 512 bytes**,
  and inline IPC messages by MSG_MAX_LEN = 768. Store size is not read size -
  conflating a buffer's capacity with those caps has caused the same silent-fail
  bug at least four separate times (env read, login's passwd read, `id`'s account
  read, the pager's piped read). Chunk the read; don't size the buffer and hope.
- **Real hardware is now parked**: Parallels testing is on hold and 2x Raspberry
  Pi 4 are the intended real-ARM target - see
  [[project-physical-hardware-target]] and `docs/testing-pi4.md`.

The 2026-08-16..18 milestone log that used to live here is
`docs/archive/early-milestones-log-2026-08.md`.
```

## `project-physical-hardware-target.md`

```markdown
---
name: project-physical-hardware-target
description: "Physical-hardware direction: Parallels parked, QEMU sufficient for now, 2x Raspberry Pi 4 as the eventual real ARM cluster target - and one deferred netd task queued against the first bench session"
metadata: 
  node_type: memory
  type: project
  originSessionId: 0206d3f8-87e3-4a6c-858b-986ed40be360
  modified: 2026-08-27T22:13:56.211Z
---

**Decision (2026-08-26): Parallels real-hardware testing is PARKED; the physical
target is now 2× Raspberry Pi 4.**

- **Parallels is de-prioritized** ("perhaps do at some point later"). Don't push
  Parallels testing unprompted. It was never going to prove the cluster anyway:
  no working NIC transport there (Parallels' NIC is virtio-PCI, which the project
  doesn't support - `virtio_mmio_probe_safe=false` skips `init_net` on Parallels),
  so all networking + the whole Plan 9 cluster (mount -r/cpu/dial/export/auth) is
  unreachable on Parallels. See `docs/testing-parallels.md` (kept as a reference,
  banner marks it PARKED) and the roadmap "Testing infrastructure" section.

- **QEMU is good enough for now** - single machine AND the two-node cluster on a
  shared socket link (`make run-image-2vm-a`/`-b`, see `docs/testing-qemu.md`).
  That's the working dev/test loop; keep using it. [[reference-prlctl-parallels-testing]]
  (the `make test-parallels` tooling) still works but is parked.

- **2× Raspberry Pi 4 ordered** (Hans, 2026-08-26; recommended in another session
  as a physical ARM platform). When they arrive we'll write a concrete
  real-hardware test plan FOR THE PIS - a genuine two-node physical cluster - NOT
  before. Rationale Hans gave: the Plan 9 resource-sharing mechanics are a better
  fit for actual physical hardware than a VM, so a real two-node Pi cluster is the
  eventual real-hardware proof of the [[project-cluster-vision]].

- **Follow-on implication:** the virtio-PCI NIC transport (needed for Parallels
  networking) is no longer motivated by Parallels. Whether the Pis need new
  driver work (their NICs, USB, etc. vs. QEMU virtio-mmio) is an open question to
  assess when the boards arrive - that shapes whatever bring-up the Pi target
  needs.

- **A TASK IS QUEUED AGAINST THE FIRST BENCH SESSION (2026-09-01):** the
  `NET_WAIT`-is-not-a-sleep defect in netd's `load_auth`. It is deferred on
  purpose and the trigger is HARDWARE, not a decision - instrumented on QEMU the
  retry loop runs **0 times** (virtio-blk has `fsd` ready before `netd` asks), so
  that rig can observe neither the bug nor a fix. USB-MSD on a Pi is the first
  rig where the loop executes. Written up as `docs/testing-pi4.md` Risk 4b and
  as step 4 of its "When the boards arrive", plus `roadmap.md`'s open gaps.
  **How to apply:** when Pi bring-up starts, that step is the one open item
  waiting on hardware - instrument the retry count first, and only then decide
  whether the fix (drain the mailbox while waiting, which touches supervision)
  is needed.

- **Pi-4 bring-up reference already written** (2026-08-27): `docs/research-redox-and-pi.md`
  Part 2 maps the `rust-raspberrypi-OS-tutorials` repo onto our situation. KEY
  CALL: **try the pftf/RPi4 EDK2 UEFI+ACPI firmware FIRST** (flash `RPI_EFI.fd` to
  FAT32) - a Pi 4 under it gives UEFI + ACPI + GOP framebuffer, so our existing
  boot path should carry over largely unchanged: UEFI loader, ACPI MADT →
  `gicv2.rs` for the Pi 4's GIC-400 (= GICv2), GOP `fbconsole`. Raw `kernel8.img`
  boot (peripheral base `0xFE00_0000`, GIC-400 GICD `0xFF84_1000`/GICC
  `0xFF84_2000`, PL011-not-mini-UART, GPIO14/15=ALT0, serial rig = USB-serial to
  TX/RX/GND NOT VCC) is the fallback. Also: Redox OS hit ARM64 bugs worth
  searching their commit log for (FP reg corruption, PTE shareability) if we see
  similar. See [[reference-redox-os-cousin]].
```

## `project-release-process.md`

```markdown
---
name: project-release-process
description: "How Ouroboros cuts releases - the process lives in docs/RELEASING.md; this holds only what that file cannot say"
metadata:
  type: project
---

**The process is `docs/RELEASING.md`** — versioning scheme, the two-phase
build/publish split, the release table, and the three gotchas that have bitten
(the `Cargo.lock` trap, the `.dmg`-not-`.hdd` quirk, smoke-testing the image
before publishing). That file was thin until 2026-09-01; the detail that used to
live in this memory was moved there, where Hans can read it too.

What stays here, because it is about *how to behave* rather than how the tooling
works:

- **Publishing is gated on explicit go-ahead, every time.** A public GitHub
  release is hard to reverse. `scripts/release.sh build` is local and safe;
  `publish` is not, and there is deliberately no `make publish`. Do the
  reversible half freely, stop before the outward half.
- **One completed arc = one minor bump.** An arc is the unit `CHANGELOG.md` and
  the postmortems already think in. A patch is an isolated fix on a released
  minor.
- **A wire flag day belongs in the notes' first screen.** v0.15.0 and v0.16.0
  each changed the auth magic and refuse the older format outright, so both ends
  of a cluster must upgrade together. Say so before the feature list, not after.
- The publish half can be done with `scripts/release.sh publish` or the direct
  steps (push `main` → `git tag -a -F notes` → push tag → `gh release create`
  with the `.img.zip`, `.dmg` and `SHA256SUMS`). Both have worked; the direct
  steps are easier to inspect mid-flight. `gh` is authed as hansolovkarlsson.

Latest: **v0.16.0**, 2026-09-01. Related: [[project-cluster-keys-arc]],
[[project-ouroboros-status]].
```

## `reference-a-check-that-cannot-fail.md`

```markdown
---
name: reference-a-check-that-cannot-fail
description: "A green check is a claim about the CHECKER, not the code - the recurring failure mode in this project; the only cure is to break the code on purpose and watch the check notice"
metadata:
  type: reference
---

The single most productive question in this codebase: **could this check have
failed?** It has caught more real bugs than any other habit, because the
alternative failure mode is invisible — a passing test and a passing tool look
identical to a correct one.

**Instances, all real:**
- `llvm-readelf -r <bin> | grep -c ABS64` returned `0` for five straight steps.
  `llvm-readelf` prints NOTHING for these binaries. Now `make check-relocs`
  (`scripts/check-relocs.sh`) uses `llvm-readobj` AND fails if the total
  `RELATIVE` count across all binaries is zero — a **canary**, because a tool
  that has stopped reporting looks exactly like a clean result.
- Two test tables were spliced into anchors that had moved; the tests iterated
  EMPTY LISTS and passed. Fix: a `vectors_are_not_empty` assertion.
- A test named for `u64` limb overflow did 256 subtractions where ~4,096 are
  needed. It could not reach the bound it was named for.
- Mutation-testing without restoring between runs let SEVEN mutations stack;
  repairing by hand fixed `add`'s carry, missed `sub`'s, and every test still
  passed. Back up the file, restore from the backup, `git diff` afterwards.
- A truncation probe passed its offset in the `tree` slot, so the server refused
  every probe and the warning could never fire.
- A security test asserted `!verify(...)` on a forgery that would never have
  verified anyway — so it PASSED with the guard removed.
- `clippy` without `--all-targets` never lints tests. "Clippy clean" was claimed
  too broadly twice before it went into `make test`.
- A pcap census read a STALE capture from a previous run and reported it
  confidently. Delete the artifact before the run.

**Why:** every one of these reported success while proving nothing, and none was
visible without deliberately breaking something. **How to apply:** after writing
any check that matters, mutate the thing it guards and watch it fail. If it does
not fail, the check is decoration. Prefer checks that fail loudly on their own
absence (a canary, a non-empty assertion, a per-peer baseline rather than a
total).

Related: [[reference-restatements-drift]] (the prose version of the same
disease), [[project-cluster-keys-arc]].
```

## `reference-a-repair-is-a-change.md`

```markdown
---
name: reference-a-repair-is-a-change
description: "A fix has the same defect rate as the code it fixes - so reviews start auditing the repairs, and the repair should be the size of the bug"
metadata:
  type: feedback
---

**A REPAIR IS A CHANGE, AND CHANGES HAVE THE SAME DEFECT RATE AS THE CODE THEY
FIX.** Measured on Ouroboros PR #60 (2026-09-01): four review rounds, 15 / 15 /
23 / 15 findings. Round 1 found the arc's bugs. **Rounds 2-4 mostly found the
previous round's repairs.** One fix took FOUR attempts (netd's `\NOEXEC`
classification), and a `rm -rf` introduced *while fixing* could have expanded to
`rm -rf $HOME`.

**Why:** every defect in that sequence came from **building more than the bug
needed** - a staging tree where a guarded delete would do, an inverted predicate
where a list would do, a catch-all flag scanner where three cases would do. New
machinery added while fixing is new surface, written under the impression of
being careful, and reviewed less carefully than the original because it is "just
a fix". The sequence's last commit was a REDUCTION (-130/+138, the removals being
the machinery) and nothing needed fixing after it.

**How to apply:**

- **Make the repair the size of the bug.** If a fix introduces a mechanism, that
  mechanism needs its own review, and usually it is not warranted.
- **A review's findings go in a NEW PR against the same base**, never as commits
  on the branch under review - each fixing round otherwise grows the next round's
  target. See [[reference-stacked-pr-squash-trap]] for the merge mechanics.
- **Scope a later round to the repairs themselves.** Round 4 targeted only the
  four fix commits and found the worst item of the whole sequence.
- **Then freeze and split** into one-subject PRs. Eight of those followed and
  none needed a second round.

Related: [[reference-a-check-that-cannot-fail]], [[reference-write-the-check-from-the-bug]],
[[project-cluster-keys-arc]]. Full account: `docs/repairing-the-repairs-postmortem.md` (the 27th).
```

## `reference-bind-credential-at-send.md`

```markdown
---
name: reference-bind-credential-at-send
description: A server must authorize on the credential the kernel bound at SEND; reading the sender slot's identity at dequeue reads whoever recycled into it
metadata:
  type: reference
---

In a message-passing kernel, "who sent this?" and "who is in slot N now?" are
different questions, and only the first one authorizes anything. A caller can
send (non-blocking), exit, and have its slot reaped and re-spawned before the
server drains its mailbox — so a late `GET_ID(sender)` reports the **new**
occupant, root included. Refusing a *dead* slot closes only half of it: a
**recycled** slot is alive and, with a bare slot number on the wire, identical.

Ouroboros fixed this 2026-08-30 by capturing the sender's credential at
`send_message` (into the queued message, or straight into the receiver's cell on
the direct-delivery path) and exposing it as `SENDER_ID` (65) /
`SENDER_GROUPS` (66). Three things that generalize:

- **Capture identity and groups together.** They are one credential; capturing
  them separately rebuilds the hole one field at a time.
- **Only an unfiltered receive updates it.** A reply to the server's own
  `MSG_CALL` must not overwrite it, or one log line mid-request silently
  re-authorizes against the server you just called. This turns "read it
  immediately" from an invariant every future server must remember into a
  property of the mechanism.
- **Not every sender is a task.** The supervisor's health ping sends as
  `KERNEL_SENDER` (0xFE) and indexes no slot — give it *no* credential rather
  than a synthesized root one, so anything authorizing it fails closed. Missing
  that guard panicked the tick handler and hung the boot.

Applies to any new server that authorizes requests. See
[[project-completed-arcs]] and [[feedback-realhw-debugging]].
```

## `reference-compute-dont-transcribe.md`

```markdown
---
name: reference-compute-dont-transcribe
description: "Where a definition is cheap to evaluate, evaluate it - a hand-written table of eight Ed25519 small-order keys had three wrong entries and was a security hole in both directions"
metadata:
  type: reference
---

**Enumerating a mathematical property is a transcription task; computing it is
not.**

The case: a guard refusing small-order Ed25519 public keys (against such a key
the cofactorless verification equation is satisfiable with NO secret, so an
authorized line carrying one is a universal forgery). It was written as a table
of the eight small-order encodings, 32 bytes of hex each. **Three entries were
wrong** — it ACCEPTED three genuinely small-order keys and REFUSED one ordinary
valid point plus one string that is not a curve point at all. Nobody reads 32
bytes of hex and notices `26e8…` became `13e8…`, and the accompanying test
exercised only the two entries that happened to be right.

The fix is not a better table: `[8]P == identity` **is** the definition, costs
three point doublings, and cannot be transcribed wrongly. It lives in
`ed25519::verify` — the security boundary — not in the parser that happened to
think of it.

**Two corollaries worth keeping:**
- **Put the check at the boundary, not at a call site.** A guard in the parser
  protects only callers who go through the parser.
- **Make test DATA self-checking.** The replacement test asserts each candidate
  IS small order (via the curve arithmetic) before asserting it is refused, so a
  mistyped constant fails loudly instead of quietly shrinking the test. Its
  candidates are now derived from one checked generator by repeated addition,
  with the subgroup asserted to close at eight distinct points.

**Why:** a wrong constant in a security check fails in BOTH directions — it lets
the dangerous thing through and blocks a legitimate one — and neither is visible
without an independent evaluation of the property. **How to apply:** when a
guard is a list of magic values, ask what property the list is standing in for
and whether computing it is affordable. Usually it is.

Related: [[reference-a-check-that-cannot-fail]], [[project-cluster-keys-arc]].
```

## `reference-firmware-hides-robustness.md`

```markdown
---
name: reference-firmware-hides-robustness
description: "Testing lesson: a robust surrounding layer (UEFI/OVMF) can HIDE the robustness path you're testing — use a host harness against the real module, not a boot"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 5e785f1e-43e6-4a4b-af77-9c70cdd70edb
  modified: 2026-08-28T03:58:12.087Z
---

**When testing a *robustness/recovery* path, the robustness of the layer around
it can mask the very behavior you want to observe — so a QEMU/real boot is NOT
the strongest test there.** Found 2026-08-27 adding GPT CRC-validation + backup
fallback to `fsd`'s `partition.rs`:

- Corrupt the **primary** GPT and boot → `fsd` mounted, but the on-disk primary
  was **valid again afterward**: EDK2/OVMF **auto-repairs a corrupt primary GPT
  from the backup during boot**, so `fsd` never saw the corruption.
- Corrupt **both** copies → the firmware refuses to boot at all
  (`CheckCrc32: Crc check failed` → EFI shell); the kernel never runs.

Either way `fsd`'s own fallback logic is invisible behind the firmware.

**The honest test was a host harness** that `#[path]`-includes the *real*
`partition.rs` and drives it through a mock `Disk` over raw image bytes (clean /
corrupt-primary-header / corrupt-array / corrupt-both / plain-MBR), plus a
cross-check that the hand-rolled bitwise CRC-32 matched `zlib.crc32` and the
image builder's stored values. Generalize: for a recovery path, host-harness the
real module directly; reserve the boot for the clean/no-regression case. Related:
the silent-sentinel debugging reflex — a bug that returns a shared error code
(e.g. `GET_ENV` conflating "range invalid" with "no entry") is invisible until a
kernel-side `println!` prints the branch taken. See [[project-completed-arcs]].
```

## `reference-fixed-one-layer-away.md`

```markdown
---
name: reference-fixed-one-layer-away
description: "A correct fix at the leaf can be unreachable because a caller flattens the value first - run it, don't read it"
metadata:
  type: feedback
---

**A FIX CAN BE CORRECT AND UNREACHABLE**, because something above it already
collapsed the distinction. Twice in the Ouroboros cluster-keys review
(2026-09-01), and both times **running it found what reading it had not**:

- Splitting `LineScan::Unreadable` into transient vs permanent looked complete -
  new state, classifier sets it, `deny_unknown_user` matches it. Booting a node
  with `/etc/passwd` removed produced the OLD message: `map_user` flattened the
  value one level ABOVE, with `None if scan == LineScan::Unreadable`, so a third
  state took the "no such account" branch by default.
- netd's `\NOEXEC` fail-open was "fixed" twice against `MSG_CALL` failure codes
  before anyone noticed `read_file_chunk` issues its `GRANT` **first**, and a
  refused grant reports `FS_ERROR` - so the restart window never produced either
  value the fix handled.

**The tell in both:** a `==` comparison where a `match` belongs. A comparison
silently routes any new variant to the default branch; a match makes it a compile
error.

**How to apply:** after fixing a leaf, **run the failing scenario end to end**
before believing it - and when a value is a state rather than a boolean, match on
it at every hop so a later state cannot be absorbed. Then check what the caller
does with the value, and what the caller's caller does.

Related: [[reference-a-repair-is-a-change]], [[reference-unspellable-not-ungrepped]].
```

## `reference-osdev-wiki-and-qemu-pi.md`

```markdown
---
name: reference-osdev-wiki-and-qemu-pi
description: "OSDev wiki as the bare-metal reference, and QEMU can emulate the Pi (raspi3b/raspi4b) — start Pi bring-up on QEMU before hardware"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 949ae0fd-032c-469a-9c0a-b451ccbe88d2
  modified: 2026-08-28T17:56:11.708Z
---

For the upcoming Raspberry Pi 3/4 hardware work:

- **OSDev Wiki** (<https://wiki.osdev.org/>) is the go-to bare-metal reference —
  esp. its `Raspberry_Pi_Bare_Bones` / `ARM_RaspberryPi` / `PL011` / `GIC` pages
  (register-level companion to the rust-raspberrypi-OS-tutorials). Cross-check
  its values against authoritative sources, same discipline as mmu.rs/gic.rs.
- **QEMU emulates the Pi**: `qemu-system-aarch64 -machine help` lists `raspi3b`
  and `raspi4b` (confirmed present). So Pi peripheral bring-up can start on QEMU
  before the boards arrive. Nuance: our kernel is UEFI-native, and the `raspi*`
  machines boot the RAW kernel8.img path (would need a raw-boot build variant);
  the preferred UEFI route is already covered by QEMU `virt`+OVMF. Peripheral
  coverage on `raspi*` is partial and version-dependent.

Written up in `docs/resources.md` (new external-references doc) and
`docs/testing-pi4.md` ("Develop on QEMU first"). See
[[project-physical-hardware-target]].
```

## `reference-prlctl-parallels-testing.md`

```markdown
---
name: reference-prlctl-parallels-testing
description: "Parallels Desktop's CLI (prlctl) can script real-hardware VM testing (start/stop/screenshot/send-key-event) — used to build Ouroboros's `make test-parallels`"
metadata: 
  node_type: memory
  type: reference
  originSessionId: ec4d3ecd-0891-4b97-ac94-ddf166d56b24
  modified: 2026-08-16T20:08:52.004Z
---

Parallels Desktop ships a CLI tool, `prlctl` (`man prlctl` for the full
reference — installed at `/usr/local/bin/prlctl` on Hans's machine),
capable of scripting a full VM test round trip with no human watching
the screen or typing on a physical keyboard:

- `prlctl list -a` — list registered VMs and their state.
- `prlctl start <name>` / `prlctl stop <name> [--kill]` / `prlctl status <name>`.
- `prlctl capture <name> --file <path.png>` — screenshot the VM's display to a file.
- `prlctl send-key-event <name> --scancode <n> [--event press|release]` — inject a key event. **Scancodes must be decimal, not hex** (`--scancode 0x23` is rejected; `--scancode 35` works). Standard PC AT Set-1 make codes apply (e.g. h=35, i=23, Enter=28). With no `--event`, a single call sends press+release sequentially.

**How this was found:** Hans mentioned on 2026-08-16 that he'd only
learned that morning that `prlctl` existed and had a key-send/capture
capability at all — this was not previously known to the
[[project-ouroboros-status]] project or used in it before that day.

**Why it matters for Ouroboros specifically:** every real-hardware bug
in the project's postmortems (see [[project-ouroboros-status]]) cost a
manual round trip — rebuild, re-image, boot Parallels, watch the
screen, type on a keyboard, report back. `prlctl` closes that gap.
Confirmed end to end: booting the registered "Ouroboros" VM, sending
scancodes for `hi` + Enter via `send-key-event`, and capturing a
screenshot showed the kernel's own `xhci::report` debug log receiving
genuine HID reports and the shell correctly printing
`unknown command: hi` — a full scripted round trip through the same
xHCI interrupt-endpoint code path documented in
`docs/xhci-keyboard-postmortem.md`. One caveat: `send-key-event` drives
Parallels' own synthetic keyboard device, not the specific physical USB
keyboard from that postmortem's bugs 1-5 — a legitimate stand-in for
scripted regression checks, not a substitute for real-USB-passthrough
hardware confirmation.

This is now wired up as `make test-parallels`
(`scripts/test-parallels.sh` in the Ouroboros repo) — rebuilds
`esp.hdd`, boots the VM, types a `;`-separated list of shell commands,
and screenshots after each one. See `docs/roadmap.md`'s "Testing
infrastructure" section and `CLAUDE.md`'s "## Commands" section for the
full writeup.

**How to apply:** for any future bare-metal/VM project (Ouroboros or
otherwise) where Parallels is the real-hardware test target and manual
round trips are a bottleneck, reach for `prlctl` first rather than
assuming Parallels testing must be manual — it wasn't obvious this
existed, but it does, and it's capable enough to fully script a test
loop analogous to QEMU's monitor/HMP `sendkey` + screendump techniques.

**PARKED (2026-08-26):** Parallels real-hardware testing is no longer a
priority - Hans decided QEMU (incl. the two-node cluster) is good enough for
now, and Parallels can't run the cluster anyway (no virtio-PCI NIC transport).
The physical ARM target going forward is 2× Raspberry Pi 4 (see
[[project-physical-hardware-target]]). This `prlctl`/`make test-parallels`
tooling still works and the write-up (`docs/testing-parallels.md`) is kept as a
reference, but treat Parallels as "perhaps later," not an active plan. Don't
push Parallels testing unprompted.
```

## `reference-qemu-stdin-guest-shell.md`

```markdown
---
name: reference-qemu-stdin-guest-shell
description: "Reusable test technique: QEMU -nographic delivers piped stdin to the Ouroboros guest shell, so shell commands (cpu/etc.) can be driven unattended"
metadata: 
  node_type: memory
  type: reference
  originSessionId: 0206d3f8-87e3-4a6c-858b-986ed40be360
  modified: 2026-08-28T23:38:58.718Z
---

**Confirmed 2026-08-26: `qemu-system-aarch64 ... -nographic` delivers piped
stdin to the Ouroboros guest SHELL**, so shell commands can be driven unattended
from the host (no keystroke injection needed). The guest's console read
(`read_char`) reads from the PL011 UART, which `-nographic` wires to the process
stdin. Verified: feeding `help\n` made the guest shell print its command list.

**How (the working recipe):** boot qemu with stdin from a FIFO, hold the FIFO's
write end open with a long background `sleep > fifo`, `echo "cmd" > fifo` to type,
capture the console to a logfile:

```bash
FIFO=/tmp/qin; rm -f "$FIFO"; mkfifo "$FIFO"
sleep 600 > "$FIFO" &            # holds the write end open
qemu-system-aarch64 -machine virt -cpu cortex-a72 -m 512M -bios "$OVMF" \
  -drive file=build/esp.img,format=raw,if=none,id=hd0 -device virtio-blk-device,drive=hd0 \
  -netdev user,id=net0 -device virtio-net-device,netdev=net0 \
  -global virtio-mmio.force-legacy=false -nographic < "$FIFO" > /tmp/vm.log 2>&1 &
# wait for a boot marker in /tmp/vm.log, then:
echo "cpu 10.0.2.2:5641 bigtest" > "$FIFO"
```

Run the orchestration as a BACKGROUND bash job (the sandbox blocks foreground
`sleep`; background jobs allow it). Use `pkill ... || true` (set -e + pkill
returns 1 when nothing matches, which aborts the script).

**CRITICAL pacing refinement (learned 2026-08-28, account-management arc): a
PL011 has NO RX FIFO worth relying on, so DON'T pipe the whole script at boot —
it DROPS.** QEMU pushes stdin bytes into the guest's UART receive register; the
guest doesn't read until the shell/login prompt (~70s later under TCG), so an
up-front burst overflows and vanishes (symptom: boot reaches `login:` but no
input is ever echoed/consumed). The reliable recipe: (1) hold the fifo open on a
spare fd (`exec 3<>"$fifo"`), (2) POLL the logfile until the prompt appears
(`until grep -q "login:" log; do sleep 1; done`), (3) THEN feed lines ONE AT A
TIME with a few seconds between (`for l in ...; do printf '%s\n' "$l" >&3; sleep
5; done`). Two more gotchas: send `\n` only, NOT `\r\n` (a stray `\n` after a
CR-submitted username is read as an EMPTY password → "Login incorrect"); and a
two-session harness proved reliable where a single-session variant raced (timing,
not worth chasing). For a login-gated shell the first two fed lines are the
username + password.

**Why it matters:** previously, testing anything that runs IN the guest shell
(vs. the host driving the export over TCP) seemed to need two interactive
terminals or `prlctl send-key-event` on real Parallels. This lets a single
QEMU guest run shell commands unattended - e.g. the `cpu` output-streaming test
drove `cpu <host> <cmd>` against a host "fake export" that streamed 1500 bytes
back and confirmed the guest printed all of it. Pair with a host TCP server
(the guest reaches the host at 10.0.2.2 over SLIRP) to close the loop for
network/cluster commands. The `[[reference-prlctl-parallels-testing]]` approach
is still the only real-hardware path (now [[project-physical-hardware-target]]
PARKED); this is the QEMU unattended-shell complement.
```

## `reference-redox-os-cousin.md`

```markdown
---
name: reference-redox-os-cousin
description: "Redox OS is Ouroboros's closest cousin (shipped Rust microkernel); what to adopt (relibc, namespace-as-capability, RedoxFS) vs already-have"
metadata: 
  node_type: memory
  type: reference
  originSessionId: fb4afbc0-7023-4929-b0bd-c9797ea0f13b
  modified: 2026-08-27T22:14:14.351Z
---

Analyzed Redox OS (https://www.redox-os.org/) against Ouroboros 2026-08-27; full
write-up in `docs/research-redox-and-pi.md` Part 1.

**Headline finding: Ouroboros already structurally matches Redox** (both arrived
independently at: Rust microkernel; userspace fs/net/console daemons — Redox's
redoxfs/smolnetd/vesad ≈ our fsd/cond/netd; supervised restart of a crashed
driver; a uniform resource protocol — Redox *schemes* `scheme:path` ≈ our 9P
verbs + per-task namespaces; non-POSIX kernel with POSIX pushed to a userland
libc). Schemes vs 9P are the SAME architecture, different spelling — don't switch
syntax. The gap to Redox is maturity/userland breadth, not structure.

**The 3 real adoption candidates (each maps to an existing roadmap north-star):**
1. **relibc** — Redox's Rust-written C library; the EXISTENCE PROOF for our
   already-written "POSIX = userland libc, not kernel" plan. Real C/C++ AND Rust
   `std` run on Redox via it. Steal: relibc targets BOTH Redox and Linux (thin
   syscall wrapper on Linux) so it's host-testable; and Redox pushed fork/execve
   into userspace (redox-rt, fork = clone w/o CLONE_VM) — the answer to "but C
   calls fork()" without kernel fork. → POSIX/portability north-star.
2. **Namespace AS the capability boundary** — Redox sandboxes by restricting which
   schemes a process can NAME (down to a "null namespace" = only pre-opened fds).
   We have per-task namespaces AND a capability send-mask but haven't JOINED them
   (empty namespace = "unchanged" today, not "no access"). Joining them is the
   near-term clean idea. → security/login/permissions north-star (item 4).
3. **RedoxFS shape** — small Rust FS daemon w/ CoW + data+metadata checksums +
   transparent encryption; written from scratch (ZFS port abandoned as
   microkernel-hostile). Boots kernel off an encrypted partition. → cluster-data-
   redundancy north-star (checksums+CoW = the integrity substrate) + at-rest
   security.

**NOT adopting:** smoltcp (our hand-rolled netd is a learning goal + big-crate PIE
link wall, [[reference-str-slice-pie-trap]]); scheme URL syntax (already 9P);
Orbital/COSMIC GUI (out of scope).

Redox also treats **aarch64 as first-class** with active 2026 ARM64 work (Pi 3B+
target) — its commit log is a reference for ARM-specific kernel bugs (FP save,
PTE shareability). Companion: the Pi-4 bring-up half of the same note, see
[[project-physical-hardware-target]].

Sourcing caveat: Redox Book + release notes 403 automated fetch; findings lean on
DeepWiki mirror + search snippets — verify exact API names (setrens, scheme
packet formats) against the live book before building.
```

## `reference-restatements-drift.md`

```markdown
---
name: reference-restatements-drift
description: The code holds one copy of each fact; the prose holds several, and nothing keeps them in step - comments, manpages, doc links, mirrored C constants and prose counts drift invisibly
metadata:
  type: reference
---

2026-08-30, five instances in one day, found five different ways (two cloud
reviews, a branch reconciliation, a `cargo doc` run, and a manual read):
protected-slot comments still enumerating "tasks 0-4" beside a guard covering
0-5; `libc/include/sys.h` pinning the reserved-error floor at `MAX-33` after
the Rust constant moved to `MAX-38` (a C caller would read `ACCT_ERR_IO` as a
*success* value); a doc link to a `SENDER_IDS` that never existed; a manpage
naming `SET_GROUPS`, a syscall designed and never shipped; and `CLAUDE.md`
describing ten slots, four servers, and two mutually contradictory crate counts.

**None were visible to the compiler or to any test** - a comment, a manpage, a
doc link, an unused C constant and a prose count are exactly what neither
checks. That is the category, not bad luck.

Practices that actually caught them, worth reusing:

- `cargo doc --no-deps -p ouroboros-kernel` must report **zero** unresolved
  intra-doc links. Nine had accumulated precisely because nothing read that
  output; a clean baseline is what makes the next one visible.
- When a constant is **hand-mirrored across languages**, put the note at the
  *definition* pointing at the mirror - the definition is what gets edited.
- Prefer stating an **invariant** over enumerating a list ("every slot below
  `FIRST_SPAWNABLE`", not "tasks 0-4"). The enumeration is the part that goes
  stale, and it had already gone stale once before.
- Grep for a removed identifier's name across `manpages/` and `docs/` when
  renaming anything user-visible.

See [[reference-bind-credential-at-send]], [[reference-stacked-pr-squash-trap]].
```

## `reference-stacked-pr-squash-trap.md`

```markdown
---
name: reference-stacked-pr-squash-trap
description: "Squash-merging a PR whose branch is the BASE of another CLOSES the stacked PR and GitHub won't reopen it - merge without --delete-branch, rebase, retarget, then delete"
metadata:
  type: reference
---

When merging a stack of PRs (B based on A, C based on B), squash-merging A with
`--delete-branch` **closes** PR B rather than retargeting it. GitHub then
**refuses to reopen** it once B's head has been force-pushed — `gh pr reopen`
fails with "Could not open the pull request", and restoring A's branch ref does
not help. B's work survives, but it needs a new PR number.

The order that works:

1. `gh pr merge A --squash` — **without** `--delete-branch`
2. `git rebase --onto main <A-old-tip> B` — git drops commits already upstream
   by patch-id, so a squashed A's commits vanish cleanly
3. `gh pr edit B --base main`
4. *then* `git push origin --delete A`

**Record every branch tip before starting** (`git rev-parse` each) — the recovery
from any bad rebase is a SHA you wrote down.

Also: `MERGEABLE` is a claim about **text**, not meaning. A changed function
signature in one PR plus a new call site in another produces no textual conflict
and a tree that does not compile. For any shared-signature change, merge locally
and build before trusting the flag.

Cost this once: PR #29 (Ouroboros, 2026-08-29) had to be reopened as #31.
See [[project-completed-arcs]].
```

## `reference-str-slice-pie-trap.md`

```markdown
---
name: reference-str-slice-pie-trap
description: "Ouroboros PIE link trap: slicing a &str with a runtime index pulls in unlinkable core::fmt - use byte slices"
metadata: 
  node_type: memory
  type: reference
  originSessionId: fb4afbc0-7023-4929-b0bd-c9797ea0f13b
  modified: 2026-08-27T21:16:09.351Z
---

**Symptom:** a userland (`aarch64-unknown-none`, PIE) program or the shell fails
to LINK with `rust-lld: error: relocation R_AARCH64_ABS64 cannot be used against
local symbol` / `referenced by core.<hash>-cgu.0`, even in the release profile
that normally strips the poisoned `core::fmt` code.

**Cause:** slicing a **`&str`** with a *runtime* index (`&s[a..b]`, `&s[..=i]`,
`&s[i..]`, `s.strip_suffix(c)`, `s.rfind(c)`) emits `str`'s char-boundary /
range-fail panic path, which **formats** the error message (prints the string
around the bad boundary) → drags in `core::fmt::builders::PadAdapter` etc. → the
`R_AARCH64_ABS64` the `-pie` link rejects. Plain **byte** slicing (`&[u8]`,
`&data[a..b]`) uses a lighter panic path that is already linked and fine.

**Fix:** work in bytes. Take `s.as_bytes()`, index/slice the `&[u8]`, and convert
back with `core::str::from_utf8(...)` only where a `&str` is actually needed
(that's a validation returning `Result`, no panic-format). Hand-roll the little
helpers instead of the `str` methods: `rfind`→reverse byte scan,
`strip_suffix('/')`→`if b.last()==Some(&b'/') {...}`, case-insensitive compare→a
manual ASCII fold (`ci_eq`). This is the same family as the documented
liballoc/`core::fmt` PIE limits ([[project-ouroboros-status]]); the sharp new
rule is **"never slice a &str by a runtime index in a /bin program - slice
bytes."**

Hit and fixed 2026-08-27 building shell filename globbing + tab completion
(`glob_split`/`expand_one_glob`/`complete_word` all rewritten to bytes). The
bisection that found it: comment out the new call → links; stub each inner call →
still fails; the constant was the `&str` slices, not `fs_list_dir`/`resolve_path`
(both already live). Relates to [[project-completed-arcs]].
```

## `reference-unspellable-not-ungrepped.md`

```markdown
---
name: reference-unspellable-not-ungrepped
description: A safety property you must remember at each call site is not a safety property - make the wrong thing unspellable (required parameter) rather than un-grepped (opt-in wrapper)
metadata:
  type: reference
---

2026-08-30: a per-user-cluster-identity branch was rejected by review with
fifteen findings, **five of them independent ways a remote request still
reached `fsd` with root authority**. Four of the five were **one design
decision**, not four mistakes:

- **Opt-in wrappers** (`fsd_call_as` vs `fsd_call`) — a missed call site is
  *silent*. One path (`fsd_write_at`) was simply forgotten. I had even written
  the invariant down ("is every export-path call proxied? one grep") and then
  grepped the four helper **names** I knew rather than the **property**.
- **A latch** holding identity between two messages — every other actor is a way
  for its lifetime to end early. Any task's request to `fsd` cleared it, and the
  fallback was root.

**The rebuild shape, and the general rule:** make it a **required parameter**
with an explicit `FirstParty` value for the server's own calls, and carry state
**with the request** rather than beside it. Then the dangerous thing does not
compile, and a new verb cannot be added wrong.

Two verification lessons from the same day, both expensive:

- **Evidence for the wrong proposition.** A green build, clippy clean, host
  tests, a live two-node run showing the exploit refused *and* a negative
  control — all correct, and all establishing "that path is fixed", which I read
  as "the file half is fixed". Enumerate the paths before generalizing.
- **`-d int` cannot see a userland panic.** A parked task raises no CPU
  exception, so the health bar reads 0 while a server dies and the supervisor
  restarts it. The signal is the supervisor's restart line. Separately, a
  refactor once dropped `SError` from that bar without changing its output.

See [[reference-bind-credential-at-send]], [[reference-restatements-drift]],
docs/unspellable-postmortem.md.
```

## `reference-write-the-check-from-the-bug.md`

```markdown
---
name: reference-write-the-check-from-the-bug
description: "A check written from the shape of the fix cannot detect the defect that motivated it - derive it from the failure, then mutate to prove it fails"
metadata:
  type: feedback
---

**I WRITE THE CHECK FROM THE SHAPE OF THE FIX, NOT THE SHAPE OF THE BUG.** Three
occurrences in one day (2026-09-01), all while fixing *other* checks that could
not fail:

1. A test asserting `NP_FRAME_MAX == prefix + hdr + net_max` - the constant's own
   definition, token for token. `NP_NET_MAX = 7` left all 13 tests green. **The
   Makefile comment justifying the change cited that test by name** as the payoff.
2. A bound asserted against a LOCAL COPY of `syscall_abi::SAFECOPY_MAX`; raising
   the real constant left it comparing the stale copy and passing. Its doc claimed
   it pinned the value "by MEANING".
3. A stray-label check that flagged labels **not in** the table - excluding
   precisely the case it existed for, a label correctly in the table and *also*
   copied elsewhere.

**Why it happens:** the fix's shape is what is in mind while writing the check,
so the check inherits it - and a condition derived from the repair cannot detect
the defect that motivated it. All three were caught by mutation; none by reading.

**How to apply:** state the FAILURE first ("a 33-byte stored hash is accepted by
prefix"), write the assertion from that sentence, then **mutate the code to make
it fail before believing it**. If the mutation passes, the check is describing
the fix. Also: a mechanical edit near a test module needs a `#[test]` COUNT - one
bad index deleted fourteen tests and left the suite green at 8.

Related: [[reference-a-check-that-cannot-fail]] (the parent lesson),
[[reference-compute-dont-transcribe]], [[reference-a-repair-is-a-change]].
```

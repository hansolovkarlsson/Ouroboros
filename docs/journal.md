# Ouroboros development journal

A chronological dev-log — what was worked on each day and why, in narrative
form. For the condensed milestone record see [`CHANGELOG.md`](CHANGELOG.md); for
the deeper design-and-bugs retrospectives see the postmortems under `docs/`;
for the forward plan see [`roadmap.md`](roadmap.md).

---

## 2026-08-22 — the userland day: /bin, pipelines, and the filesystem arc begins

A long single day that took the shell from "every command is a builtin compiled
into the shell binary" to a real `/bin` of standalone programs, gave it genuine
multi-stage pipelines with a filter set, raised the task-slot ceiling to make
that concurrency possible, and then started the "more filesystems" arc with a
VFS refactor and GPT support. Roughly in order:

**Finished the network stack's maturity work (Stages 4k–4o).** IRQ-driven NIC
receive (the RX queue's GIC SPI wakes `netd` instead of polling), an
RTT-estimated RTO (RFC 6298 SRTT/RTTVAR over a new microsecond clock), an HTTP
405 for unsupported methods, TCP congestion control (Reno cwnd/ssthresh), and
sender-side SACK. These closed out the network arc — the stack now has flow
control, loss recovery, congestion control, and selective retransmit.

**Trimmed `CLAUDE.md`** from a 6500-line milestone narrative to ~600 lines of
durable "read this before touching the code" guidance, moving the history into
`CHANGELOG.md` and the postmortems, and stripping the now-dangling back-pointers.

**The standalone-binaries arc — the day's spine.** The goal: commands become
real programs in `/bin`, found via `$PATH`, with arguments and a shell
environment. Built in stages: an argv ABI (`ARGS_STAGE`/`GET_ARGC`/`GET_ARG`); a
`/bin` + PATH lookup for unknown commands; a shell environment (`set`/`env`/
`unset`, `$VAR`, `PATH` a real variable); then a shared `ulib` support crate and
the externalization itself — `echo`/`uptime`/`clear`, then a cwd-delivery ABI
(`CWD_STAGE`/`GET_CWD`) so `ls`/`cat` resolve relative paths, then the path-only
write commands (`mkdir`/`rmdir`/`touch`/`rm`), then the bulk/multi-arg ones
(`cp`/`mv`/`writeat`). The **whole filesystem command surface** left the shell.

Then the network commands (`ping`/`resolve`/`fetch`) — the first `/bin` programs
to reach a server a spawnable slot can't statically talk to. Rather than widen
the capability policy, the shell **delegates** its `TO_NET` capability to the
child at spawn (the same `DELEGATE` mechanism the program-to-program pipe uses).

One batch was **attempted and reverted**: `ps`/`kill`/`wait`. Testing showed an
externalized job-control command runs in a spawnable slot, so it lists itself
and a task number goes stale between commands — which is exactly why bash makes
them builtins. That's a design finding, recorded, not a failure.

**Stage 0: raised `NUM_TASKS` 7 → 10** (five spawnable slots, up from two) — the
headroom real pipelines need. Mechanical, except the capability `u32` packed
resource caps at bits 8/9/10 and the send-mask now reached bit 9: a collision
caught before it shipped, fixed by moving the caps to bit 16+.

**The multi-stage-pipeline arc.** Turned the two-stage pipe into a real
N-stage `a | b | c`: a chainable filter shape (`upper` rewritten to write to its
stdout target, not a hardcoded console), N-stage parsing, argv and PATH on every
stage, and the spawn/delegate/wait plumbing. A satisfying confirmation: a linear
chain needs only the *existing* one-target-per-task delegation — the roadmap had
parked general delegation for want of a consumer, and this was that consumer,
without needing the general version. Then the payoff: real `/bin` filters
`grep`/`wc`/`head`, so `cat FILE | grep x | wc` works.

**The filesystems arc began.** A pure VFS refactor extracted `fsd`'s hardcoded
FAT32 into a `Filesystem` enum (FAT32 the only arm, proven byte-identical), then
GPT + multi-partition discovery: a new `partition.rs` enumerates partitions on
GPT *or* MBR disks, and `fsd` mounts a FAT32 partition wherever it sits. Testing
the GPT path meant hand-building a bootable GPT disk (macOS has no GPT tooling) —
`scripts/mkgpt.py` with correct CRC32s so UEFI boots it.

Everything QEMU-verified with zero `-d int` aborts throughout; a real-hardware
pass over the whole new surface is still outstanding. Design retrospective:
[the userland & pipelines postmortem](userland-and-pipelines-postmortem.md).

# Ouroboros development journal

A chronological dev-log — what was worked on each day and why, in narrative
form. For the condensed milestone record see [`CHANGELOG.md`](CHANGELOG.md); for
the deeper design-and-bugs retrospectives see the postmortems under `docs/`;
for the forward plan see [`roadmap.md`](roadmap.md).

---

## 2026-08-23 — the filesystems day: exFAT read+write, a cleanup, then ext2

Yesterday's filesystem arc left a `Filesystem` enum inside `fsd` with exactly
one arm (FAT32) — a refactor that *claimed* to make a second format cheap.
Today made good on it: `fsd/src/exfat.rs`, a read-only exFAT driver, the first
real second arm. Nothing above `fsd` changed — clients speak `FSOP_*` and never
learn the on-disk format — which is the whole point the refactor was for.

**Read-only first, deliberately.** The roadmap's own scoping lever: FAT32's
write support was phases 4–8, where every corruption bug lived. So a new format
lands read-only as one milestone; read-write is a separate one. Every write op
in `exfat.rs` returns a new shared `Error::ReadOnly` → a new `FS_ERR_READ_ONLY`
ABI code → `ulib`'s "read-only filesystem" message.

**What was actually new versus FAT32.** exFAT is still a cluster filesystem, so
the read machinery (cluster→LBA, chain walking, windowed `read_at`) is the same
shape as `fat32.rs`. The genuinely different parts: a boot sector described by
`log2` shifts; **contiguous files that skip the FAT entirely** (a `NoFatChain`
flag — `advance()` either walks the FAT or just returns cluster+1); directory
**entry sets** (a File entry + a Stream-Extension entry + File-Name entries)
reassembled into one `DirEntry`, structurally the same job FAT32's LFN
reconstruction does; and UTF-16 names. Two structures a read-only driver gets to
*ignore*: the allocation bitmap (that's how you find free clusters — a write
concern) and the up-case table (ASCII case-fold suffices for the names this
system uses, same shortcut `fat32.rs` already takes).

**The testing wrinkle was the interesting part.** To prove the reader, `fsd`
has to *mount* exFAT — but UEFI can only boot from FAT, and `fsd` mounts the
first partition it can. So the test disk (`scripts/mkexfat.py`) is a
two-partition MBR: **exFAT first** (so `fsd` mounts it — the FAT32 probe fails,
the exFAT probe succeeds, exercising the real enum fallthrough) and the **FAT32
ESP second** (UEFI ignores the exFAT partition it can't read and boots from the
FAT32 one). The exFAT filesystem itself is built with macOS's `newfs_exfat`
(`hdiutil` can't make exFAT) and carries `/bin` plus test files — so the shell
runs its commands *off* exFAT. One block device, no slot ambiguity. It worked
first boot: `ls`/`cat`/a `cat | grep | wc` pipeline all reading from exFAT,
long names and subdirs resolved, writes refused read-only, zero aborts. FAT32
via `run-image` unregressed.

The recurring lesson, again: a parser is only tested if you can *build* it a
valid input — the harness that produces the artifact is part of the feature,
same as `mkgpt.py` was for the GPT parser yesterday.

**Then, read-write — the harder half, in four staged commits.** With the reader
proven, the write surface followed the same discipline the FAT32 write arc used:
narrowest-useful-first, one tested-and-committed stage at a time. Stage A built
the machinery (allocation bitmap + entry-set construction with both checksums)
and exercised it with `touch`, which allocates nothing — the lowest-risk first
cut. Stage B added `write_file`/`write_at` (cluster allocation + data). Stage C
added `mkdir`/`rm`/`rmdir`. Stage D added `mv`.

What made exFAT writes genuinely harder than FAT32's: free space is an
allocation *bitmap*, not a scan-the-FAT-for-zeros; and creating a file means
building a whole *entry set* (a File entry + a Stream-Extension entry + N
File-Name entries) with two checksums the format requires — a `SetChecksum` over
the entire set and a `NameHash` over the up-cased name. Get either wrong and a
real driver rejects the file. What made it *easier* than expected: exFAT has no
8.3/LFN mangling (names are just UTF-16, so no `make_short_name`), and no
`.`/`..` directory entries (an empty dir is a zeroed cluster, and moving a
directory needs no `..` fixup — the one hard step FAT32's `mv` couldn't skip).

The decision that kept it simple: created files are always FAT-*chained*
(`NoFatChain = 0`), never pure-contiguous, so allocation is the direct parallel
of FAT32's `write_chain` (set the bitmap bit, link the FAT) and the reader's
existing `advance()` walks them. A macOS-created *contiguous* file being appended
to is the one case needing a convert-to-chain step first.

The validation this time was better than any log line: macOS's own exFAT driver
mounts the volume `fsd` wrote, a binary copied through `cp` reads back
byte-identical, and `fsck_exfat` — a real filesystem checker — pronounces the
bitmap and directory hierarchy clean after a full churn of creates, writes,
deletes, and renames. When another vendor's checker signs off on your on-disk
structures, the checksums are right.

**Then two housekeeping passes, before the tree got unwieldy.** First, the ~26
userland crates (which had piled up at the repo root) moved under a `programs/`
tree grouped by role (`fileutils`, `textutils`, `netutils`, `shellutils`,
`servers`, `demos`, plus `shell`) — a pure `git mv` (history preserved), and
because crates build by package name, the Makefile needed *zero* edits; only the
`path = "../..."` deps deepened. Second, every generated artifact (the `esp/`
staging tree, the disk images, `net.pcap`, logs) moved under a single `build/`
dir driven by one Makefile variable, so the repo root is source/docs/config
only. We considered moving `kernel/` and the shared libs too, but left them —
three top-level dirs isn't crowding, and `kernel/` at the root is the
conventional, legible shape.

**Then ext2, read-only — the arm that actually tests the abstraction.** FAT32
and exFAT are the same shape (clusters, a FAT, a flat directory-entry list), so
the `Filesystem` enum barely had to stretch. ext2 is a different world: inodes
own the metadata and a directory entry is just `name -> inode number`, so path
resolution bounces between directory blocks and the inode table; files are found
through block-group descriptors; data blocks are reached through 12 direct
pointers then single/double indirect pointer-blocks; and names are
*case-sensitive*. Driving all of that through the **unchanged** `FSOP_*`
protocol — nothing above `fsd` touched — is the proof the abstraction was real
and not FAT-shaped in disguise.

Two nice moments in bring-up. First, the reader worked on the first boot for
`exec` and `cd`, but bare commands (`ls`) failed — which turned out *not* to be
a reader bug: the shell probes `/bin/<command>` as typed (lowercase), and it had
always relied on FAT/exFAT matching case-insensitively. ext2, correctly
case-sensitive, wouldn't match lowercase `ls` against the uppercase `LS` I'd
copied in. The fix wasn't in the reader at all — it was to give the ext2 image a
*lowercase* `/bin`, which is the Unix convention anyway. The abstraction was
faithfully surfacing ext2's semantics; my test image was the thing being
un-Unix. Second, forcing the image's block size to 1024 meant the >12 KiB `/bin`
binaries spilled past the 12 direct pointers into single-indirect blocks — so
`exec /bin/CAT` was quietly exercising the indirection path from the first run.

**And finally ext2 read-write — the finale of the whole filesystems arc.** Four
staged commits again (allocation + `touch`; `write_file`/`write_at`;
`mkdir`/`rm`/`rmdir`; `mv`). ext2 writing is the fiddliest of the three formats,
because its consistency is spread across more structures than FAT's single
table: allocation flips a block/inode *bitmap* bit **and** decrements free
counts in both the group descriptor and the superblock; directories carry link
counts that `mkdir`/`rmdir` must bump and drop; a cross-directory directory move
has to repoint the moved dir's `..` and shift its parent-link contribution. The
independent checker this time was `e2fsck` (macOS can't mount ext2), and it kept
me honest.

The best bug of the arc surfaced here. After `rm`/`rmdir`, `e2fsck` reported a
"corrupted orphan linked list" naming exactly the inodes I'd deleted. Everything
*looked* right — links 0, bitmap freed, deletion time set — and `s_last_orphan`
was 0, so there shouldn't have been an orphan list at all. The cause was subtle:
e2fsck treats a links-0 inode whose `i_dtime` is *less than the inode count* as
sitting on the orphan list, using `i_dtime` as the next-orphan inode pointer. I'd
written a sentinel deletion time of `1`, which e2fsck read as "next orphan =
inode 1", fabricating a bogus chain. The fix was to write a plausible *timestamp*
(a fixed large constant, since there's no RTC) instead of a small sentinel —
after which `e2fsck -fn` passed all five passes completely clean, including the
directory-connectivity and reference-count passes that validate `..` and link
counts after directory moves. A copied binary also came back byte-identical
through `debugfs`.

That closes the "more filesystems" arc: GPT/MBR discovery, a VFS refactor, and
FAT32 + exFAT + ext2 — `fsd` reads and writes all three through one unchanged
`FSOP_*` protocol. The whole point of the VFS refactor, proven: a genuinely
different (inode-based, case-sensitive) filesystem slotted in as a third enum arm
with zero changes above `fsd`.

**And then the real-hardware pass — which did exactly what it's for: found a bug
QEMU can't show.** After a session-long pile of QEMU-only work (exFAT, ext2,
`/bin`, pipelines, the housekeeping), we took it to a real Parallels VM via
`prlctl` (start / send-key-event / capture, reading the screenshots back).
**Part 1 was clean:** boots to a shell, all servers supervised, `ps` correct,
`selftest` (the relocation self-test) passes — none of the churn regressed the
hardware boot path.

**Part 2 got interesting.** Parallels exposes no disk to the kernel except USB
mass storage, so testing the filesystem work meant a real USB stick (an
`espexfat.img` written to it). First attempt — boot from the `.hdd`, stick
passed through — showed `fsd: exFAT mounted` but then every read failed with
"device I/O error", even reads of the *same sectors* the mount had just read
successfully. Then Hans ran his own experiment: detach the `.hdd`/CD entirely so
the VM boots *from the stick's* FAT32 ESP — and exFAT auto-mounted and read/wrote
fine. But when I tried to drive that config, the keyboard was completely dead —
not even a builtin registered.

The two configs form an inverse correlation: `.hdd`-boot → keyboard works,
storage reads degrade; USB-boot → storage works, keyboard dead. That's the
diagnosis: the xHCI/USB stack can't reliably keep a USB keyboard *and* a USB
mass-storage device live at the same time on this hardware (the "up to 4
concurrently addressed devices" limit + enumeration order in `xhci.rs`, and/or
mass-storage endpoint recovery in `usb_msd.rs`). It's a USB-subsystem robustness
bug, not a filesystem bug — the FS drivers read and wrote correctly whenever the
block layer actually served them. Recorded in the roadmap; it's the next real
debugging target, and postmortem-worthy. The satisfying part: the whole reason
to run on real hardware is to find precisely this kind of thing, and it did.

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

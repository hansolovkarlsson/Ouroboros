# Ouroboros roadmap

Forward-looking plan — what's next and why, in plan form rather than
chronological narrative. For completed milestones, see
[`CHANGELOG.md`](CHANGELOG.md); for *how* something already built
actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see `CLAUDE.md`. This document is the one to
update first when direction changes; the others describe what exists,
this one describes where it's going.

## What's next (the current frontier)

The microkernel arc is the through-line now. Two components have moved
out of the EL1 kernel into supervised, protected userland servers — the
FAT32 **filesystem** (`fsd`, part 2) and the **console** (`cond`, part 3,
both stages done and confirmed on real Parallels hardware). What each
newly proved: the pattern generalizes (a second server, on a
hardware-adjacent component), and userland output is now an IPC stream.
The prioritized next steps, roughly in order of value (details in the
parking lot below and in `microkernel-comparison.md`):

**Recently completed (2026-08-19/20): general supervision + heartbeat,
now with an active health ping** (`kernel/src/supervisor.rs`). Crash
recovery is no longer bespoke to `fsd` — a registry supervises both `fsd`
and `cond` (and up to 4 servers), restarting either on a fault; a passive
**heartbeat** in `on_tick` catches a *runnable* wedge (a looping,
non-faulting server) by observing that a healthy server keeps returning to
a `Blocked` state while a wedged one stays `Runnable`; and an **active
health ping** (2026-08-20) now also catches a server *deadlocked while
blocked* — the one gap the passive heartbeat couldn't see — by poking a
long-`Blocked` server with a `SYSOP_PING` message (sender `KERNEL_SENDER`)
and restarting it if the reply/ack doesn't come back inside a timeout. It
needs no new syscall and no server changes (a server's ordinary reply,
addressed to the sentinel, is the ack). A shared per-boot restart cap
degrades gracefully past it. See `CHANGELOG.md`. The prioritized next
steps:

**Recently completed (2026-08-20): a capability model for
who-may-call-whom.** The IPC topology is no longer flat — a per-slot
capability set (a **send-mask** of which slots each task may initiate IPC
to, plus resource caps for the disk/console gates) is enforced at the
`MSG_SEND`/`MSG_CALL` boundary, so a task can only reach the endpoints its
capabilities allow. Isolation is now *topological*, not just memory-level —
the last big structural gap against MINIX. Because task-slot roles are
static, capabilities are a pure function of slot (no stored state); a
reply exemption keeps request/response working (a server replies to a
caller blocked in a call to it regardless of the mask). Static policy only
for now — runtime delegation (granting a spawned program a capability it
doesn't get by default) is the natural follow-up. See `CHANGELOG.md`.

**Recently completed (2026-08-20): the stdout-over-IPC payoff — program-to
-program pipes and `exec … > file`.** A task's own output is capturable now
via a per-slot **stdout target** (a task index, `CON_TASK` by default, set
at spawn): a producer routes its output to the shell instead of the
console, and the shell relays it on to a consumer program (`a | b` where
both are programs) or captures it to a file (`exec prog > file`). The clean
relay design (`producer → shell → consumer`) needed **no capability
delegation** — `producer → shell` is already permitted by the send-mask.
Still 2-stage, producer-`hello`-only for now. See `CHANGELOG.md`.
**(Update, 2026-08-21: program-to-program pipes are relay-free now — see the
delegation note immediately below; `exec … > file` still routes through the
shell for capture.)**

**Recently completed (2026-08-21): runtime capability delegation — its first
consumer, relay-free program-to-program pipes.** The static per-slot
send-mask is now a *baseline* extensible at runtime: `DELEGATE` (syscall 41)
hands one task the right to send to another, confined by a "may only
delegate a send-cap you *statically* hold" rule (no transitive
re-delegation). This self-secures it — only the shell holds the spawnable
slots' send-caps, so only it can authorize one spawned program to reach
another. Its first consumer: `/prog_a | /prog_b` now streams the producer's
output **directly** to the consumer (the shell delegates producer→consumer
and steps out of the byte path, instead of relaying every chunk). Confirmed
on QEMU (both lines of `HELLO.BIN | UPPER.BIN` uppercased and reaped, the
builtin-left pipe and `exec > file` unregressed, the non-reading consumer
recovering on Ctrl+C), zero aborts. Still coarse (one delegated target per
task, non-transitive, in practice shell-only); a general, transitive
capability-passing mechanism is the remaining gap. See `CHANGELOG.md`.

**Recently completed (2026-08-21): FAT32 interior / random-access writes.**
`fat32::write_at` used to refuse any offset past the current end of file (no
sparse gap), so it only did append and sequential-overwrite. It now supports
a true random-access write at any offset, **zero-filling the gap** on disk
when the offset is past EOF (FAT32 has no sparse representation, so real zero
bytes). A finding that shaped the scope: the *interior*-overwrite path
(offset ≤ old_size) was already coded — it just had no caller, since `cp`/`>>`
are sequential/append only; and `FSOP_WRITE_AT` already existed, so no new
syscall/FSOP was needed. A `writeat <file> <offset> <text...>` shell builtin
is the reachable consumer (in place, file must exist). Verified on QEMU with
the gap bytes confirmed real `0x00` by hex-inspecting the raw serial log
(a 1195-byte multi-sector gap all zero — the `extend_chain` fresh-cluster
case), reboot persistence, and FAT16 degradation; zero aborts. Still coarse:
1 MiB gap cap, no create-on-`writeat`, no truncate-to-length. See
`CHANGELOG.md`.

The frontier, in rough order of value:

1. **General / transitive capability delegation.** The delegation shipped
   2026-08-21 is deliberately coarse: one delegated target per task,
   non-transitive, in practice shell-only. Making it general (any task hands
   any held capability onward, revocably — MINIX's full grant model) would
   unlock true relay-free `a | b | c` and a spawned program running its
   *own* server. The catch: **neither consumer exists yet**, so building
   this first would repeat the "premature, a mechanism without a hard
   consumer" trap the capability-and-hardening postmortem flagged for
   delegation itself. Build the consumer first, or wait until one is
   actually wanted.

2. **Per-task ASIDs, revisited** — a pure TLB-flush-per-switch optimization
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

## A network stack (scoped)

The one large subsystem Ouroboros has never had. It's a good fit for the
current architecture rather than a stretch: the "DMA driver in the kernel,
protocol logic in a userland server" split maps onto the `fsd`/`cond`
precedent exactly, and it would be the first genuinely *stateful* server —
which is what would finally motivate Helix-style hot-reload-with-state-
migration and, eventually, the distributed half of the Plan 9 `/net`
direction (see [`research-directions.md`](research-directions.md)).

**The architecture, dictated by the no-IOMMU DMA constraint.** Same rule
that keeps the block transport in the kernel: a NIC does DMA (RX/TX
descriptor rings plus packet buffers the device reads and writes), and with
no IOMMU a device can DMA anywhere, so the *driver* must stay in the trusted
EL1 kernel. So the stack splits:

- **Kernel (EL1): a virtio-net driver** — RX/TX virtqueues over the existing
  `virtio_mmio.rs` transport (the same one `virtio_blk.rs` uses), exposed as
  a gated device through a small syscall pair (send a frame / poll-receive a
  frame), accepted from one network-server task alone — exactly the
  `BLOCK_*` → `fsd` gating pattern.
- **Userland (EL0): `netd`, the protocol stack** — Ethernet / ARP / IPv4 /
  ICMP / UDP (/ TCP), a boot-loaded, supervised, MMU-isolated EL0 server (the
  `fsd`/`cond` precedent), reached over IPC by clients. The MINIX `inet`-
  server / Plan 9 `/net` model.

**Platform reality (the storage story again).** QEMU exposes virtio-net over
virtio-mmio (`-device virtio-net-device`), reachable with the existing
transport — the dev loop. **Parallels exposes virtio-net over PCI** (the
device inventory already found it: vendor `0x1af4`, device `0x1000`, class
`0x02:0x00`), which needs a **virtio-pci transport this project doesn't
have** (it deliberately uses virtio-mmio only). So real-hardware networking
is gated behind a virtio-pci transport sub-project — the same shape as
storage (virtio-blk-mmio on QEMU, a separate USB-MSD path for Parallels).
QEMU-first throughout; Parallels networking is a later, separately-scoped
step, not a Stage 1 concern.

**Staging (each independently verifiable, the standing discipline):**

1. ~~**virtio-net driver (kernel), raw frames.**~~ **DONE (2026-08-21).**
   `kernel/src/virtio_net.rs`: discovery + feature negotiation
   (`VIRTIO_F_VERSION_1`/`VIRTIO_NET_F_MAC`), receiveq (pre-posted buffers) +
   transmitq, the 12-byte virtio-net header, `send_frame`/`poll_frame`.
   Polled, matching every other driver. Proven on QEMU by `main.rs::init_net`
   sending a broadcast ARP request and decoding the reply, cross-checked
   against a `tcpdump` of the `make run-net` pcap (request out, reply in) -
   not "no error returned." Gated behind `virtio_mmio_probe_safe` (QEMU-only;
   Parallels' virtio-net is PCI). The gated `NET_SEND`/`NET_RECV` syscalls
   deferred to Stage 2 with `netd`, their first consumer, rather than dead
   code now. See `CHANGELOG.md` / `CLAUDE.md`'s "Network stack, Stage 1."
2. ~~**ARP + IPv4 + ICMP echo (`netd`).**~~ **DONE (2026-08-21).** The
   protocol stack moved to a userland server (`netd/`, the eighth userland
   program, a fourth protected task slot — `NET_TASK`, 4 — reached via the
   gated `NET_SEND`/`NET_RECV`/`NET_MAC` syscalls). ARP resolution + IPv4 +
   ICMP echo, all hand-rolled fixed-buffer with correct Internet checksums,
   exposed as a `NETOP_PING` request; a `ping <a.b.c.d>` shell command is the
   first client. Verified on QEMU (`ping 10.0.2.2`/`.3` → reply, `.99` →
   unreachable), cross-checked against a `tcpdump` of the `run-image-net`
   pcap. **Guest-initiated ping only** at the time — answering unsolicited
   input needed an async receive loop (the poll/select gap), since closed by
   Stage 4b's `NET_WAIT`/`WaitReason::NetInput` (`netd` now answers ARP
   requests and serves TCP; a host-ping responder would be a trivial add on
   the same event loop, though SLIRP can't route an inbound host→guest ping
   to test it). See `CHANGELOG.md` / `CLAUDE.md`'s "Network stack, Stage
   2/4b."
3. ~~**UDP.**~~ **DONE (2026-08-21).** UDP send/receive, proven by real
   **DNS resolution**: a `NETOP_RESOLVE` op in `netd` (the `NETOP_*`/`FSOP_*`
   shape) does a DNS A-query over UDP to the user-net DNS server, parses the
   response (name-compression-aware), and returns the first A record; the
   shell's `resolve <hostname>` command is the first client. Verified on QEMU
   resolving real hostnames via SLIRP's DNS proxy (`resolve example.com` →
   `172.66.147.243`), cross-checked against a `tcpdump` of the pcap. No kernel
   changes — a whole new transport landed purely in userland, the payoff of
   the driver/protocol split. See `CHANGELOG.md` / `CLAUDE.md`'s "Network
   stack, Stage 3."
4. **TCP — client active-open DONE (2026-08-21, Stage 4a); server side
   DONE (2026-08-21, Stage 4b).** Decided hand-roll (not `smoltcp`) and
   minimal scope with the user. Stage 4a: `netd` gained a hand-rolled client
   TCP (`build_tcp`/`parse_tcp` with the IPv4 pseudo-header checksum,
   `tcp_get`: SYN handshake, in-order reassembly, clean FIN teardown) plus a
   minimal default route (on-subnet → target, else the gateway), and a
   `fetch <hostname>` shell command chains resolve → route → TCP → HTTP GET.
   Stage 4b closed the async-receive gap `ping`/`resolve`/`listen` all named:
   a new `NET_WAIT` syscall + `WaitReason::NetInput` (a minimal poll/select
   over frames-or-messages) makes `netd` event-driven, and it now runs a TCP
   HTTP server on port 80 (SYN → SYN-ACK → request → a fixed page + FIN →
   ack the peer's FIN) plus an ARP responder — the guest *answers* the
   network. Verified both directions on QEMU: `fetch example.com` →
   `HTTP/1.1 200 OK` + Example Domain HTML (client), and host
   `curl localhost:5555` → `HTTP/1.0 200 OK` + the guest's page via SLIRP
   hostfwd (server), each cross-checked against a `tcpdump` of the pcap (clean
   four-way conversations, checksums accepted). Stage 4c made the server a
   real **static-file server**: it parses the request path and streams the
   file from `fsd` over TCP (`netd` becomes `fsd`'s first non-shell client,
   via a new `netd`→`fsd` capability), verified serving a real file
   byte-identically over multiple segments. Stage 4d added real **send-side
   flow control** (track the peer's window, keep `snd_nxt - snd_una` under it,
   stream ACK-paced) so a file of *any* size streams — verified with a 256 KB
   file, byte-identical, three times over. Stage 4e added proper HTTP
   response headers (`Content-Type` by file extension + `Content-Length` via
   an `fsd` stat), so a browser renders served files. **The TCP feature set
   is complete** (handshake, flow control, fast retransmit, RTO, congestion
   control, concurrent connections, and SACK on the sender). (Stage 4f added a browsable HTML
   directory listing; Stage 4g added `HEAD`; Stage 4h added fast retransmit;
   Stage 4i added a timer-based RTO — via a new `NET_WAIT` timeout — for a
   silent peer, so loss recovery is complete; Stage 4j added concurrent
   connections (up to 4), so a browser loading a page and multiple clients
   are served at once; Stage 4k made receive interrupt-driven — the NIC's
   GIC SPI wakes `netd` directly instead of waiting for the tick poll, which
   stays as a fallback; Stage 4l made the RTO adaptive — RFC 6298 SRTT/RTTVAR
   estimated from measured round-trip time via a new `MONOTONIC_US` clock
   syscall, replacing the fixed 1 s base; Stage 4m returns a proper 405 for
   an unsupported HTTP method; Stage 4n adds TCP congestion control — a Reno
   `cwnd` (slow-start / congestion avoidance, halve on fast retransmit, reset
   on RTO), so the send rate is `min(cwnd, peer window)`; Stage 4o adds
   sender-side SACK — negotiate SACK-permitted, parse the peer's SACK blocks,
   selectively retransmit only the hole instead of go-back-N (SLIRP doesn't
   speak SACK, so the go-back-N fallback is the exercised path there).) See
   `CHANGELOG.md`'s "Network stack, Stage 4a–4o."

**Decisions to settle before starting (not now):**

- **Hand-roll vs. a `no_std` stack for the protocol logic**, TCP especially.
  The project has hand-rolled every parser so far and avoided
  allocator-assuming crates; `smoltcp` *can* run `no_std`/no-alloc with
  fixed buffers, so it's a genuine option for TCP specifically — the same
  hand-roll-vs-crate call FAT32 faced. Note it, don't pre-decide.
- **Client API shape**: a bespoke socket-op IPC protocol now, or wait for the
  Plan 9 `/net` file interface. The two connect — a network server is a
  strong first consumer for the namespace direction.
- **Polled vs. interrupt-driven RX**: *settled — receive is interrupt-driven
  now (Stage 4k).* Polled was the first cut (every driver is), but a NIC is
  the strongest case for real IRQ-driven RX, since packets arrive
  unsolicited — the first driver where polling is a real latency cost, not
  just a simplicity choice. The NIC's GIC SPI now wakes `netd` directly (the
  tick poll stays as a fallback). Transmit still polls (`send_frame`).

**Scale, honestly:** Stages 1–3 are each roughly a milestone the size of the
storage or console work; Stage 4 (TCP) is larger than any single milestone
this project has done. This is a multi-milestone arc, not one task — but
Stages 1–2 alone (frames + `ping`) are a satisfying, self-contained first
target that proves the whole architecture end to end.

## More filesystems: a VFS layer, then exFAT / ext2 (scoped)

**Smaller FAT32 follow-up first: LFN *write*.** Long filenames are *read*
now (`fsd` reconstructs them from a real formatter's LFN entries — see
`CHANGELOG.md`'s "FAT32 long filename (LFN) read support"), but the guest
still can't *create* one: `make_short_name` only makes 8.3 names. Writing LFN
needs generating a unique `~N` short alias (scan the directory for
collisions), the alias checksum, and laying down N+1 contiguous directory
entries (growing the directory if needed) — a self-contained addition to
`fat32.rs`, much smaller than a whole new filesystem. It would also let a
clean delete free the orphaned LFN entries `rm` currently leaves behind.

Today the filesystem server `fsd` *is* FAT32 — the type is hardcoded
(`fsd/src/fat32.rs`), and it mounts only the first FAT32-typed MBR
partition. Supporting more filesystems is really two things: an internal
abstraction so `fsd` can drive several, and the filesystem drivers
themselves.

**The key insight: the client-facing VFS already exists.** Clients call
`FSOP_LIST_DIR`/`READ_FILE`/`WRITE_AT`/… over IPC — they never know it's
FAT32. So the *interface* is already filesystem-agnostic; what's missing is
**internal multiplexing inside `fsd`**: detect the filesystem type at mount
time and dispatch each `FSOP_*` op to the right driver. That's a much
smaller change than "write a VFS from scratch."

**Architecture, matching the project's idioms.** A `Filesystem` **enum**
(`Fat32 | ExFat | Ext2 | …`) with a shared method surface — the same
enum-over-`dyn` pattern `console::Console` and `block::BlockDevice` already
use, chosen because `fsd` is `no_std` with no heap (fixed buffers, no
`alloc` — the real `alloc` heap stays blocked on stable, see above), so
`dyn Trait` isn't available. FAT32 becomes the first arm; `FSOP_*` is
unchanged. Every driver is **hand-rolled, fixed-buffer**, no crates — the
same reasoning FAT32 was hand-rolled (no-alloc post-`exit_boot_services`,
and filesystem crates assume an allocator and would hit the PIE/libcore
wall anyway).

**Read-only first, read-write later — the big scoping lever.** For each new
filesystem, read-only support is dramatically simpler and safer (no
allocation, no bitmap/inode/FAT updates, no directory insertion, no
corruption risk). FAT32's own history is the evidence — write support was
phases 4–8, where all the corruption risk lived. So each new FS lands
read-only as one milestone, read-write as a separate one.

**Staging:**

0. **GPT + multi-partition (prerequisite) — DONE.** New `fsd/src/partition.rs`
   `discover`s a disk's partition start LBAs — GPT (via the "EFI PART" header +
   entry array) or MBR — and `vfs::mount` tries mounting a filesystem at each
   (first FAT32 wins), so `fsd` mounts a FAT32 partition wherever it sits, MBR
   *or* GPT. `fat32::Fs::mount` became `mount_at(disk, lba)` (no partition
   scan). Tested both paths on QEMU (a new `scripts/mkgpt.py` / `make
   run-image-gpt` builds a bootable GPT disk, since macOS has no GPT tooling);
   the GPT disk has no real MBR table, so `fsd` mounting it proves the GPT
   parser. See `CHANGELOG.md`'s "More filesystems, step 0."
1. **The VFS refactor (a pure refactor first) — DONE.** `fsd/src/vfs.rs`'s
   `Filesystem` enum (FAT32 the only arm) now wraps the hardcoded `Fs`; its
   per-op methods forward to the arm, and `mount` is the type-detection point.
   `main.rs` holds an `Option<vfs::Filesystem>` and calls it exactly as before.
   Proven byte-identical on QEMU (the whole FS surface + a file-reading
   pipeline), zero aborts. A second filesystem is a new arm plus a `mount`
   branch.
2. **exFAT, read-only — DONE.** The second filesystem arm (`fsd/src/exfat.rs`),
   and the first real exercise of step 1's `Filesystem` enum. Structurally the
   *same shape* as FAT (clusters, a partition, directory entries), so the read
   machinery mirrors `fat32.rs`; the genuinely new parts a read-only driver
   handles: the `log2`-shift boot sector; **contiguous files that skip the FAT**
   (the `NoFatChain` flag → `advance()`); directory **entry sets** (`0x85` File
   + `0xC0` Stream-Ext + `0xC1` File-Name, reassembled by `walk_dir` — the
   analogue of FAT32 LFN); **UTF-16 names** rendered ASCII. The allocation
   *bitmap* (`0x81`) is ignored (a write concern — read-only never allocates)
   and the up-case table (`0x82`) approximated by ASCII case-fold (correct for
   our names). Writes return the new `Error::ReadOnly` → `FS_ERR_READ_ONLY`.
   `vfs::mount` probes each partition FAT32-then-exFAT. Tested on QEMU via a new
   two-partition disk (`scripts/mkexfat.py` + `make run-image-exfat`: exFAT
   partition first so `fsd` mounts it, FAT32 ESP second so UEFI boots it),
   exFAT built with `newfs_exfat`; `ls`/`cat`/a `grep | wc` pipeline all read
   from exFAT, `/bin` runs off it, writes refused read-only, zero aborts. It
   lifts the **real limitation** the USB-mass-storage milestone noted ("exFAT
   sticks won't mount — reformat FAT32"), at least for reads. See
   `CHANGELOG.md`'s "More filesystems, step 2."
3. **exFAT, read-write — DONE.** Built in four staged commits mirroring FAT32's
   write arc (allocation + `touch`; `write_file`/`write_at`; `mkdir`/`rm`/
   `rmdir`; `mv`). Free clusters tracked by the allocation *bitmap*
   (`alloc_cluster`/`bitmap_set`, located at mount); created files/dirs are
   FAT-chained (`NoFatChain = 0`), so allocation parallels FAT32's `write_chain`.
   Creating an entry (`create_entry`/`build_entry_set`) computes both required
   checksums (whole-set `SetChecksum` + up-cased `NameHash`); deleting one
   (`delete_set`) clears each entry's in-use bit. Same claim-before-use /
   write-new-before-free discipline as the FAT32 arc. exFAT dirs have no `.`/`..`
   (so an empty dir is a zeroed cluster, and `mv` of a directory needs no
   fixup). Verified on QEMU per stage (zero aborts) and **against a real driver**:
   macOS mounts the volume, a copied binary reads back byte-identical, and
   `fsck_exfat` passes (bitmap + hierarchy clean) after create/write/rm/rmdir/mv
   churn. See `CHANGELOG.md`'s "More filesystems, step 3."
4. **ext2, read-only — DONE.** The third filesystem arm (`fsd/src/ext2.rs`),
   and the real test of the abstraction: a genuinely *different* (inode-based)
   model driven through the unchanged `FSOP_*` protocol. Inodes own metadata (a
   directory entry is just `name -> inode number`); block group descriptors
   locate each group's inode table; `block_for` follows 12 direct + single/
   double indirect block pointers (triple = EOF; a `0` pointer is a sparse
   hole); names are **case-sensitive** (Unix), unlike FAT/exFAT. Read-only
   (writes return `Error::ReadOnly`); the FAT-shaped `FSOP_*` presents
   files/dirs and ignores Unix metadata it can't model (symlinks reported, not
   followed). Tested on QEMU (`make run-image-ext2`, a two-partition disk from
   `scripts/mkext2.py`: ext2 first so `fsd` mounts it, FAT32 ESP second so UEFI
   boots; ext2 built with `e2fsprogs`' `mke2fs -d`, block size 1024 to exercise
   single-indirect blocks, lowercase `/bin` since the shell probes as typed).
   Confirmed: `ls`/`cat`, subdirs, `/bin` off ext2, a `grep | wc` pipeline,
   case-sensitivity, and writes refused; zero aborts. See `CHANGELOG.md`'s
   "More filesystems, step 4."
5. **ext2, read-write — DONE.** Built in four staged commits (allocation +
   `touch`; `write_file`/`write_at`; `mkdir`/`rm`/`rmdir`; `mv`). Bitmap-based
   block + inode allocation, keeping the free counts consistent across the group
   descriptor *and* the superblock (plus `bg_used_dirs_count`); files use direct
   + single/double indirect pointers (`ensure_block`); directories track link
   counts (`mkdir`/`rmdir`) and a cross-dir `mv` fixes the moved dir's `..`. A
   real e2fsck finding fixed along the way: a freed inode's `i_dtime` must be a
   plausible timestamp, not a small sentinel (e2fsck reads a links-0 inode's
   small `i_dtime` as a next-orphan pointer). Verified per stage on QEMU (zero
   aborts) and **against the reference tools**: `e2fsck -fn` passes completely
   clean after each stage's churn, and `debugfs` reads a copied binary back
   byte-identical. See `CHANGELOG.md`'s "More filesystems, step 5."

**The "more filesystems" arc is complete** (steps 0–5): GPT/MBR discovery, the
VFS refactor, and FAT32 + exFAT + ext2 — `fsd` reads and writes all three
through the unchanged `FSOP_*` protocol, the abstraction validated by a
genuinely different (inode-based, case-sensitive) filesystem needing zero
changes above `fsd`.

**Deferred, noted:** **ext4** is much larger (extents, journaling, htree
directories, checksums, 64-bit features) and the no-alloc fixed-buffer
constraint makes a big filesystem genuinely harder — a separate, large arc,
not a near-term follow-on to ext2. FAT12/16 read support would be a cheap
add to the exFAT/FAT32 family if ever wanted, but has little real use here.

**Relationship to the Plan 9 direction** (see
[`research-directions.md`](research-directions.md)): the `Filesystem`-enum-
in-`fsd` is the pragmatic near-term shape. The Plan 9 end state is instead
*one server per filesystem* mounted into per-task namespaces — the enum is a
stepping stone to that, not a dead end, and the `FSOP_*` protocol growth
ext2 wants (permissions, symlinks) is the same richer file interface the
namespace direction points at. Sequence the enum now; generalize to
per-FS servers if and when namespaces land.

## Disk management tools: mount/unmount, partition, format (scoped)

With `fsd` now reading and writing FAT32/exFAT/ext2, the natural next capability
is *managing* disks from the running system: listing what's mounted, unmounting,
partitioning a raw disk, and formatting (mkfs) a partition. Today the guest can
only use a disk that some *other* tool already partitioned and formatted (the
`scripts/mk*.py` builders); these tools would let Ouroboros prepare a blank disk
itself — e.g. erase, partition, and format a USB stick from within the VM.

**Architecture — `fsd` grows from "filesystem server" into "storage server."**
It's already the only task with `BLOCK_*` access and already contains the code
that lays out FAT/exFAT/ext2 on-disk structures (that *is* the write arcs), so
these become new `FSOP_*` ops with thin `/bin` clients — the same pattern as
every other command. No new privileged task. Partitioning lives here too even
though it's *below* any one filesystem, because `fsd` owns the raw-disk write
path.

**`/dev`? Deferred, deliberately.** There is exactly one block device (the
kernel's single `BlockDevice` cell) and `fsd` mounts one filesystem, so a
`/dev` namespace (devices/partitions addressed by name) has nothing to name yet.
Tools target "the disk" implicitly and a partition by index. A real `/dev` is
the Plan 9 direction (a device namespace served by a devfs) — do it *if/when*
multiple disks/partitions need addressing, not before. YAGNI now.

**Testing.** Fully developable and verifiable on **QEMU with a blank virtio-blk
disk**: point the block device at an empty image, boot (`fsd` finds no FS →
`NO_FS`, device present), then `partition` → `format` → `mount` → `ls`/`cat`,
cross-checked with host tools (`fdisk`/`fsck_msdos`/`fsck_exfat`/`e2fsck`) — the
standing discipline. The appealing real-hardware workflow ("boot from the
`.hdd`, then erase/partition/format the passed-through USB stick from the VM")
is **blocked by the xHCI keyboard↔storage contention bug** (see the parking
lot): in `.hdd`-boot the keyboard works but USB storage reads/writes degrade. So
that bug is a prerequisite for the real-hardware *demo* of this arc, though not
for building it.

**Staging (each independently shippable + testable, risk increasing):**

1. **`mount` (no arg) lists what's mounted, and `unmount`.** A new
   `FSOP_MOUNT_INFO` returning the mounted format + partition (LBA/index) +
   capacity (`fsd` already has `Filesystem::name()`), so `mount` with no
   argument prints e.g. "exFAT on partition 1"; and `FSOP_UNMOUNT` drops the
   mounted FS (`fs = None`) so the disk can be reformatted. Small, low-risk, and
   foundational — you unmount before formatting, and mount-info is how you
   confirm a format worked. **Do first.**
2. **`partition` + `erase`.** `erase` zeroes the leading sectors (a `BLOCK_WRITE`
   loop via `fsd`). `partition` writes a partition table — **MBR first**
   (trivial: 4 entries + `0x55AA`), **GPT later** (the `scripts/mkgpt.py` logic
   — protective MBR + primary/backup headers with CRC32s + entry array — in
   Rust). New `FSOP_PARTITION`/`FSOP_ERASE`. Moderate.
3. **`format` (mkfs) — FAT32 first.** The big one: create a filesystem from
   scratch on a partition (BPB/FAT/root-dir for FAT32). It's essentially the
   *inverse* of the read/write arcs, so each format is its own milestone the
   size of a slice of the write work: **FAT32** (moderate), then **exFAT** (VBR
   checksum + up-case table + allocation bitmap), then **ext2** (superblock +
   group descriptors + block/inode bitmaps + inode table + root inode +
   `lost+found`). New `FSOP_FORMAT(partition, fstype)`. High-risk (writes fresh
   filesystem metadata), but onto a fresh partition, so no existing-data risk.
   Validate each against the matching host `fsck`.
4. **Deferred: a `/dev` namespace** — only if multi-disk addressing arrives; the
   Plan 9 devfs direction.

**Scale, honestly:** milestones 1–2 are small-to-moderate and land quickly;
milestone 3 is a multi-step arc (one mkfs per filesystem), the mirror image of
the read/write work already done. Start with mount-info/unmount for a fast,
useful, foundational win.

## Standalone command binaries: `/bin`, PATH, and a shell environment (scoped)

Today every command is a **shell builtin** compiled into `shell/src/main.rs`
(`dispatch_line`): the shell parses the line and calls a `cmd_*` handler
directly. The only way to run a real program is `exec /path` — and even that
passes **no arguments** (`SPAWN`/`SPAWN_STAGE` carry only the program bytes
plus a stdout target; a spawned task's GPRs are all zeroed; `_start()` takes
nothing). The goal: commands become **standalone programs** found via a
**PATH** in a **`/bin` directory**, with a real **environment** in the shell.

**The key insight: the exec substrate already exists — what's missing is
argv.** The shell already resolves a path, reads a program from `fsd` in
chunks, stages it, spawns it, and can route its output to the console, a
capture buffer, or a pipe consumer (`spawn_path`/`cmd_exec`). The one
foundational gap is that **nothing passes arguments to a spawned program**,
so `ls /foo`, `cat x`, `echo hi` can't work as external programs until an
argv mechanism exists. Everything else is plumbing on top of that.

**Decisions (recommended; alternatives noted):**

- **argv delivery: kernel-stored + getter syscalls.** The shell stages the
  argv bytes; the kernel keeps them per-task; the child reads argc/args via
  new syscalls — exactly how it already reads `stdout_target`/`heap_info`.
  `_start` stays argument-less and the spawn `Context` is untouched. *Alt:
  Unix-style argv on the child's stack — more conventional, but needs an asm
  `_start` in every program.*
- **Packaging: a shared userland support crate + thin per-command crates.**
  Factor the `fs_*`/`con_write`/argv/formatting helpers (currently inside the
  shell) into one `ulib`-style crate; each command is a small crate depending
  on it. *Alt: one self-contained crate per command (heavy duplication); or a
  busybox-style multi-call binary (FAT has no symlinks, so `/bin` needs copies
  per name).*
- **Environment: full, but shell-local.** A PATH plus a general env-var table
  with `env`/`set`/`unset` and `$VAR` expansion — modeled on how `cwd` is
  already threaded (stack-local, no statics; `linker.ld` asserts `.data`/
  `.bss` empty). **Not** exported into child programs yet (that's a second,
  argv-like ABI — deferred). *Alt: PATH-only.*
- **`/bin` + naming:** a top-level `\bin` on the FAT root, programs staged
  **uppercase, no extension** (`LS`, `CAT`, `ECHO`) — 8.3-legal, since FAT
  writes are 8.3-only and lowercase-on-disk is impossible; the shell
  uppercases the typed command for lookup.

**Necessarily still builtin:** `cd`/`pwd`/`exit`/`exec` (they mutate or read
the shell's own cwd/lifecycle) and the redirection/pipe **syntax**
(`>`/`>>`/`|`). So it's *most* commands (~20 of 29), not all.

**Staging:**

0. **More spawnable task slots (prerequisite) — DONE.** Was `NUM_TASKS=7`
   (slots 5–6, two concurrent spawned tasks); now `NUM_TASKS=10`
   (`FIRST_SPAWNABLE=5`, so slots 5–9 — **five** spawnable), the headroom a
   foreground command + a background task + a pipeline need. The per-task arrays
   in `tasks.rs` and the EL0 table pool in `mmu.rs` (`MAX_EL0_REGIONS`, which
   must stay equal) were converted to `[const { … }; N]` so they now auto-scale
   from the one constant; the boot EL0-regions array in `main.rs` is built
   programmatically for the same reason. One real gotcha caught: the caps `u32`
   packs the send-mask in the low `NUM_TASKS` bits and the resource caps at bits
   8/9/10 — at `NUM_TASKS=10` the send-mask reached bit 9 and would have
   collided with `CAP_CON`, so the resource caps moved to bits 16+. Verified on
   QEMU: five `pong` instances spawned concurrently (slots 5–9), the sixth
   refused with "no free task slot", the pipeline and `ps` (now ten slots) still
   correct, zero `-d int` aborts.
1. **argv ABI (foundational) — DONE.** New syscalls stage an argv blob and let
   the child read `argc`/args (`ARGS_STAGE`/`GET_ARGC`/`GET_ARG`, mirroring
   `SPAWN_STAGE` + the `stdout_target`/`heap_info` getters); a per-slot kernel
   argv store, cleared on task death; `spawn_path` stages the token list before
   `SPAWN`. Proven by the new `args/` program (`exec …/ARGS.BIN a b c` prints
   `argc=4` + each arg). See `CHANGELOG.md`'s "Standalone binaries, Stage 1."
2. **`/bin` + PATH lookup — DONE.** Makefile stages command binaries into
   `esp/bin/` (uppercase, extension-less); the shell's unknown-command arm
   (`run_path_command`) searches `DEFAULT_PATH` (`/bin`), probes existence
   (the one-byte `fs_read_file` trick, case-insensitive via fsd's `find`), and
   spawns the first hit with the whole line as argv — foreground (waited +
   reaped, so no slot exhaustion), branching on console vs. capture like
   `cmd_exec`. See `CHANGELOG.md`'s "Standalone binaries, Stage 2."
3. **Shell environment — DONE.** A stack-local env store (`Env`: PATH + user
   vars) threaded like `cwd`; `env`/`set`/`unset`; `$VAR` expansion
   (`expand_vars`) — all relocation-safe (scalar comparisons, hand-rolled
   formatting). `run_path_command` reads `PATH` from it, so `set PATH=…`
   changes command lookup. Shell-local (not exported to children yet). See
   `CHANGELOG.md`'s "Standalone binaries, Stage 3."
4. **Externalize the commands — IN PROGRESS.** A shared `ulib` crate, then a
   thin `/bin` program per externalizable command, each reading argv and using
   `ulib`, dropping the shell builtin once its `/bin` version works. *First
   increment done:* `ulib` (syscalls, argv, output routing, decimal, `exit`,
   the panic handler) plus `echo`/`uptime`/`clear` — the commands needing
   neither the filesystem nor the cwd. *Filesystem increment done:* a
   **cwd-delivery ABI** (`CWD_STAGE`/`GET_CWD`, a per-task cwd mirroring argv,
   staged at `SPAWN`) plus `ls`/`cat` externalized — `ulib` grew the fs client
   layer (`cwd`, `resolve`, `fs_list_dir`/`fs_read_bulk`) so a spawned command
   resolves relative paths and defaults a bare `ls` to the current directory.
   See `CHANGELOG.md`'s "Standalone binaries, Stage 4 (first increment)" and
   "(filesystem increment)." *Write-command increment done:*
   `mkdir`/`rmdir`/`touch`/`rm` externalized — the path-only write ops, each a
   single `FSOP_*` via a shared `ulib::fs_op_path` helper. *Bulk-data increment
   done:* `cp`/`mv`/`writeat` externalized (`ulib` gained `fs_read_file`/
   `fs_write_bulk`/`fs_write_at`/`fs_mv`/`parse_u64`) — **the whole filesystem
   command surface now lives in `/bin`**.

   *Netd increment done:* `ping`/`resolve`/`fetch` externalized via capability
   delegation. A spawnable slot gets `TO_SHELL | TO_FSD | TO_CON` — not
   `TO_NET` — so the shell (which holds `TO_NET` statically) `DELEGATE`s it to
   the child at spawn (`delegate_net`), the same mechanism the program-to-
   program pipe uses; `ulib::net_call` retries briefly on `MSG_ERR_DENIED` to
   ride out the delegation race. Verified on QEMU with a NIC. **With the network
   commands out, the externalization arc is effectively complete.**

   *The commands that stay builtin, by design (established by trying `ps`/`kill`/
   `wait` and reverting):*
   - **`ps`/`kill`/`wait`/`fg` — job control.** They act on the kernel's task
     table, which the shell (task 0) sees directly. An externalized version runs
     in a *spawnable slot*, so it (a) lists itself and (b) makes a task number
     racy: the slot a `ps` reports is reused by the very next command, so
     `wait <n>` typed after `ps` waited on itself (observed: "that task is
     protected"). This is exactly why `kill`/`wait`/`jobs`/`fg`/`bg` are shell
     builtins in bash. `ps` is external in Unix only because Unix PIDs are
     stable; Ouroboros reuses slots immediately.
   - **`mount`/`selftest`/`help` — shell-coupled / low-value externalized.**
     `mount` drives the kernel USB rescan; `selftest` is the shell's own
     relocation proof; `help` is worth keeping as an always-available builtin
     (it's the fallback when `/bin` isn't mounted).

   (`cd`/`pwd`/`exit`/`exec`, plus `write` and the redirect/pipe syntax, always
   stay builtin.)

**Scale, honestly:** a multi-milestone arc (breadth comparable to a slice of
the network stack), but Stages 1–3 alone already deliver "type a bare program
name from `/bin`, with args and an environment" before most commands are
externalized — a satisfying, self-contained first target.

**Deferred, noted:** exporting the environment into child programs (a second,
argv-like ABI); and a minimal built-in fallback set for booting without `/bin`.
`/bin` needs real FAT32, so this is a `make run-image` / QEMU-verified feature
primarily (like the rest of the disk surface); on Parallels it needs a disk
(USB stick, reads-only posture).

## Multi-stage program pipelines (`a | b | c`) (scoped, IN PROGRESS)

The next arc, chosen after the standalone-binaries arc completed: turn the
two-stage pipe into a real N-stage pipeline of standalone programs. Stage 0
(five spawnable slots) and the netd-command increment (which proved runtime
`TO_NET` delegation) already landed the prerequisites; each pipeline link is
one producer→consumer `DELEGATE`, and a linear chain only needs the existing
*one-target-per-task* delegation (task *a* delegates to *b*, *b* to *c* — each
task has exactly one downstream target, so the general/transitive delegation in
"What's next" item 1 is **not** required for a linear pipe).

**Where it stands today (the patchwork to replace):** `parse_pipe` splits on
the *first* `|` only; the right side must be a single token with **no
arguments** and an explicit path (`cmd_pipeline`/`cmd_pipeline_prog` use
`spawn_path`, cwd-relative, **not** PATH); and a filter that isn't the last
stage couldn't route its output onward. The last limitation is the one already
fixed:

1. **Chainable filter shape — DONE.** `upper` was rewritten over `ulib` to read
   stdin (`MSG_RECV`) and write to its **stdout target** (`write_out`) instead
   of a hardcoded console, propagating end-of-stream (`end_of_stream`) so the
   next stage finishes. No regression to `X | upper` (target is the console
   there); it can now be a *middle* stage. The reference shape every future
   filter copies.
2. **N-stage parsing — DONE.** `split_pipeline` splits on every standalone `|`
   token into up to `MAX_STAGES` (8) trimmed stages (a fixed array, no `Vec`).
3. **argv on pipeline stages — DONE.** `spawn_stage` tokenizes each stage into
   its own argv; the "programs take no arguments" pipe restriction is gone, so
   `cat FILE | …` carries its file.
4. **PATH resolution in pipes — DONE.** `resolve_command` resolves a stage's
   command via `$PATH` for a bare name (the `run_path_command` probe, factored
   out) or as-is for a `/`-path, so `echo … | upper` works with bare names.
5. **N-stage plumbing — DONE.** `cmd_pipeline` (rewritten) spawns program stages
   right-to-left, `DELEGATE`s each adjacent link (a linear chain needs only the
   existing one-target-per-task delegation), and waits/reaps all; the first
   stage may be a builtin (captured and streamed to stage 2). The old
   `parse_pipe`/`PipeParse`/`cmd_pipeline_prog` patchwork was removed.
6. **More filters (payoff) — DONE.** `wc` (line/word/byte counts), `grep
   <pattern>` (substring line filter), and `head [N]` (first N lines) shipped as
   `/bin` programs over `ulib` (a new `ulib::pipe_recv` factors out the filter
   stdin read). `cat FILE | grep x | wc` and `ls /bin | grep C | wc` work.
   (`sort` deferred - it needs to buffer all input before emitting, unlike the
   streaming/line-buffered three.)

**The multi-stage-pipeline arc is complete.** Only `sort` (full-input
buffering) is left as an optional future filter.

**Verification (steps 2–5, on `make run-image`, zero `-d int` aborts):**
`echo hello world | upper` → "HELLO WORLD"; `echo chained pipe | upper | upper`
→ "CHAINED PIPE" (three tasks, a real middle stage); `cat /pt.txt | upper` →
the file uppercased (argv producer); `echo x | nosuchprog`/`echo x | pwd` → the
not-a-program error; `pwd | upper` → "/" (builtin head). Combining `|` with
`>`/`>>` stays refused.

## Testing infrastructure: scripted real-hardware round trips

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

## Parking lot (known future work, not yet sequenced)

Pulled from `docs/processes.md`'s "known rough edges" and `CLAUDE.md`'s
running "next milestone" notes — real gaps, not yet a committed next
phase:

**New gaps from the /bin + pipelines + filesystem work (2026-08-22):**

- **Real-hardware regression pass — DONE (2026-08-23), and it found a real
  USB bug.** Ran on real Parallels hardware via `prlctl`. **Part 1 (boot /
  kernel / shell): green** — boots to a working shell, `cond`/`netd`/`fsd` all
  supervised, `ps` shows the correct task table incl. the raised slot count,
  and `selftest` (the PIE/relocation self-test) passes; so the housekeeping
  (moving `fsd`'s source, the `build/` reorg), the +1700 lines of exFAT/ext2 in
  the `fsd` binary, and the slot bump did **not** regress the hardware boot
  path. **Part 2 (filesystem on a real USB stick):** the exFAT read-write
  driver is confirmed good on hardware — booting from a USB stick (an
  `espexfat.img` written to it) auto-mounts the exFAT partition and reads/writes
  work. **But the pass surfaced a genuine USB-subsystem bug** (see next item),
  invisible on QEMU, where the keyboard is synthetic and storage is virtio-blk.
- **USB keyboard ↔ mass-storage contention on xHCI (real-hardware bug, found
  by the pass).** On Parallels the xHCI/USB stack can't reliably serve the USB
  keyboard *and* USB mass storage at once — an inverse correlation observed
  across two boot configs: booting from the `.hdd` with the stick passed through
  late, the **keyboard works but storage reads degrade to device-I/O errors
  shortly after mount**; booting from the USB stick itself, **storage works end
  to end but the keyboard is never addressed** (no shell input). Points at
  `xhci.rs`'s device management — the "up to 4 concurrently addressed devices"
  limit and enumeration/addressing *order* — and/or `usb_msd.rs` read
  recovery (SCSI Unit Attention / endpoint stall). A USB-subsystem robustness
  issue, not a filesystem-driver bug (the FS drivers read/wrote fine whenever
  the block layer served them). The scoped follow-up: make the xHCI driver keep
  a HID keyboard *and* a mass-storage device concurrently live, and recover a
  mass-storage endpoint that errors mid-session. Postmortem-worthy. See the
  [userland & pipelines postmortem](userland-and-pipelines-postmortem.md) for
  the QEMU-only body of work this pass was checking.
- **GPT is parsed but not validated on read.** `fsd`'s `partition::discover`
  trusts the "EFI PART" signature; it doesn't check the GPT header/entry-array
  CRC32s or fall back to the backup GPT on a corrupt primary. Fine for the
  clean images we make, a robustness gap for a real damaged disk. (The
  *builder*, `scripts/mkgpt.py`, does write correct CRCs — UEFI requires them.)
- **Pipelines can't combine with `>`/`>>`.** `a | b > file` is refused (the last
  stage writes straight to the console; there's no capture of its output). A
  real feature would route the last stage's stdout into a file capture.
- **`grep` is substring-only and case-sensitive** (no regex, no `-i`); **`head`
  relies on the producer's send-timeout** when it exits early rather than
  actively signalling upstream; **`sort` isn't written** (it needs to buffer all
  input before emitting, unlike the streaming/line-buffered filters). All small,
  optional filter follow-ups.
- **A pipeline stage other than the first can't be a builtin** (a later stage
  must be a program that reads stdin). Reasonable, but means e.g. `cat x | ps` is
  not meaningful; documented, not likely worth changing.

- ~~xHCI keyboard failing outright on a real, manually-launched
  Parallels VM~~ — **done, found and fixed the same day, by the user
  directly** (not by any of this project's own scripted testing, which
  never reproduced it — see `docs/roadmap.md`'s "Testing infrastructure"
  section for why: `make test-parallels` drives Parallels' own synthetic
  keyboard headlessly, with no live-rendered VM window competing for
  host CPU/GPU time). Root cause: every busy-wait in `xhci.rs` was
  bounded by a fixed iteration count, not real elapsed time — a real
  hypervisor can stall the guest's vCPU for a genuine, unpredictable
  duration (e.g. while actually rendering a live VM window on screen)
  without an iteration count reflecting that at all. Fixed by switching
  every wait to a genuine wall-clock deadline using the ARM generic
  timer's free-running counter (`CNTPCT_EL0`/`CNTFRQ_EL0`, pure
  system-register reads, no GIC or interrupts needed — the same
  property `timer.rs` already relies on). Confirmed fixed by the user
  on the exact real-world scenario that originally failed. See
  `CLAUDE.md`'s "xHCI's busy-waits were iteration-bounded" section for
  the full writeup.
- ~~Diagnose the real-Parallels task-switch hang~~ — **done, same day it
  was found.** Root cause traced (well enough to fix) to task 1's idle
  loop using `wfe` — real hardware's `wfe` semantics under Apple's
  virtualization layer are the leading (still not fully proven)
  explanation. Confirmed fixed by swapping the idle loop for a plain
  busy-spin (`nop; b 1b`, see `tasks.rs`'s `el0_idle_template` doc
  comment) and re-testing: a sustained real-hardware interactive session
  showed a correctly, continuously incrementing tick count (`644` ->
  `1210` in one observed run) with no hang. Task switching is now
  unconditionally enabled again (the temporary `TASK_SWITCH_ENABLED`
  gate was removed entirely) — preemptive multitasking works on real
  Parallels hardware for the first time ever. A real, secondary, minor
  finding along the way, also root-caused and fixed the same day: an
  occasional dropped keystroke under active task switching, traced to a
  genuine logic bug in `xhci.rs::Device::poll_key` (not a hypervisor
  timing quirk) - a single polled report can legitimately carry more
  than one newly-pressed keycode at once, and the original code only
  ever translated and returned the first one, silently discarding any
  second one forever. Fixed with a small `pending` buffer draining every
  qualifying keycode from a report instead of just the first. Confirmed
  fixed on real Parallels hardware: ten consecutive `uptime` invocations
  back to back, zero drops.
- ~~Confirm virtio-console on real Parallels hardware~~ — done, and the
  answer is no. Tested on real hardware: a full PCI device inventory
  (`pci::log_all_devices`, kept as a permanent diagnostic) shows no
  virtio-console device over PCI, and no direct evidence of one over
  MMIO either. Parallels' actual serial port is very likely a
  proprietary device (PCI vendor `0x1ab8`, no public spec) - see
  `CLAUDE.md`'s "virtio-console" section. Reverse-engineering that
  device was considered and explicitly declined (open-ended, no
  guaranteed payoff); revisit only if real documentation or driver
  source for it ever surfaces. The virtio-console *driver* itself stays
  in the tree, confirmed working on QEMU - just not the answer for
  Parallels.
- ~~Build a GOP framebuffer console (the real lead after virtio-console)~~
  — done, **and confirmed reaching a real, working shell prompt live on
  real Parallels hardware** ("take six" - `framebuffer.rs`/`font.rs`/
  `fbconsole.rs`, see `CLAUDE.md`/`CHANGELOG.md`). Five real-Parallels
  test rounds, each finding and fixing one real bug: `open_protocol_exclusive`
  disconnecting firmware's own boot console from GOP; `try_virtio_console`'s
  MMIO scan freezing the boot with no console yet installed to report
  through (a reorder got a console installed, and that console then
  rendered the exception that led to the real fix); a genuine bus fault
  in `virtio_mmio::find_device`'s scan, decoded directly from that
  rendered exception, fixed with a safety gate covering every caller of
  that scan; and a *second*, differently-addressed instance of the
  identical fault at `gic.rs`'s `GICD_BASE` - proof the entire fixed
  low-1GB QEMU-shaped device-region convention (not just virtio-mmio) is
  unsafe on Parallels, fixed by broadening the same gate
  (`qemu_device_region_safe`) to cover GIC/timer setup too. **This is
  now done** - and the console's write-only limitation is also now done
  (see the USB HID keyboard driver item below). What was left from this
  era - no preemption on Parallels - is also now done, see the
  "Preemption on Parallels" item below.
- ~~A USB HID keyboard driver~~ — **done, and confirmed with a real,
  physical keyboard typing a full command line (with backspace) into the
  shell on real Parallels-on-Apple-Silicon hardware.** A genuinely large
  addition, as flagged when this was scoped: a from-scratch xHCI driver
  (`kernel/src/xhci.rs`) covering capability/operational register
  programming, the command ring, the event ring, device slot enable/
  address, control transfers, and - the mechanism that turned out to
  actually be required, see below - a real interrupt IN transfer ring.
  Five independently-confirmed real-hardware bugs along the way, none
  visible on QEMU: a PCI Command register bit-position error (Memory
  Space Enable is bit 1, not bit 0 - explained both the QEMU dev-loop
  quirk below and the real-hardware failure); a firmware panic
  (`PANIC@11.28 UEFI-exception-ArmPciCpuIo2Dxe.dll`, decoded from
  Parallels' own hypervisor crash log) from a PCI config-space
  BAR-reassignment write that real firmware doesn't tolerate the way
  QEMU's does; the discovered BAR landing outside the identity map's
  original single-L0-table-entry span, fixed by generalizing
  `mmu.rs` to allocate further top-level table entries on demand; the
  deepest finding, that Parallels' USB passthrough doesn't forward HID
  *class* requests (`SET_PROTOCOL`, `GET_REPORT`) to the real device at
  all (confirmed via a live, correct `GET_DESCRIPTOR` *standard* request
  returning Parallels' own real registered USB vendor ID, `0x203a`,
  right next to a `GET_REPORT` that kept echoing this driver's own Setup
  packet back) - fixed by using a real interrupt endpoint (armed via the
  standard `Configure Endpoint` xHCI *command*, not a class request) the
  way every production USB HID driver actually works at runtime, instead
  of polling `GET_REPORT`; and, once that interrupt endpoint was
  delivering real live data, discovering it was reading Parallels'
  virtual *mouse*, not the keyboard - fixed by scanning every connected
  port and checking each device's actual HID interface protocol
  (`bInterfaceProtocol=1`, Keyboard) before configuring it. Full
  technical write-up, including the debugging techniques that found each
  bug, in [`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md) -
  written to be useful to other bare-metal-OS developers hitting the
  same class of problem, not just this project's own history.
  **Still coarse, worth knowing before building on this:** one port, one
  device, one slot (**update: lifted** - see the multi-device entry
  further below; every connected device is now enumerated, classified,
  and kept concurrently addressed), no hot-plug, no hubs, no real HID
  report-descriptor parsing (boot-protocol's fixed 8-byte layout assumed
  directly), no stall recovery on the interrupt endpoint specifically,
  only the first interrupt IN endpoint on a matching interface is ever
  configured.
- ~~Preemption on Parallels~~ — **fully done.** Real ACPI MADT discovery
  (`kernel/src/madt.rs`) replaced the old heuristic for GIC/timer setup,
  a GICv3 driver (`kernel/src/gicv3.rs`) is confirmed working on real
  Parallels hardware, and the task-switch hang found in the process
  (see the parking-lot item above) is fixed too - real, preemptive,
  two-task round-robin multitasking now works end to end on real
  Parallels hardware, confirmed by a sustained interactive test with a
  genuinely, correctly incrementing `uptime` throughout. See
  `CLAUDE.md`'s "MADT/GICv3" section for the full writeup, including the
  two real GICv3 bugs found and fixed on QEMU first (`GICR_IGROUPR0`
  Group-1 assignment, `GICD_CTLR`'s multi-bit enable) before ever
  risking a Parallels round trip on them, and the task-switch fix
  itself (idle task `wfe` -> busy-spin) - root-caused well enough to
  fix, not fully proven why real hardware's `wfe` didn't work as
  expected.
- ~~**Output redirection (`>`/`>>`)** — needs shell-level parsing this
  project doesn't have yet (splitting `cmd > file` into a command and a
  target). `cp` is done (see below); redirection is the one piece of
  the original "output redirection (`>`/`>>`), and anything else that
  needs a file to hold more than zero bytes" item still open.~~
  **Done** — `> file` (create/overwrite) and `>> file` (append) work
  for every builtin, entirely shell-side over the existing
  `fs_read_file`/`fs_write_file` syscalls (zero kernel changes, same
  compose-what-exists approach as `cp`): `run_line` peels a trailing
  redirect off the line before dispatch, command output goes through an
  explicit `Output` sink passed down to the handlers (a capture buffer
  when redirecting; error messages deliberately stay on the console -
  the POSIX stdout/stderr split), and `>>` is read-concatenate-rewrite
  bounded by the kernel's 512-byte per-syscall cap. That cap found a
  real bug during testing: a 1024-byte append buffer failed the
  kernel's `valid_user_range` check in a way indistinguishable from
  "no such file", silently turning append into overwrite. Confirmed on
  QEMU (overwrite/append/create-empty/persistence-across-reboot/both
  overflow refusals/error cases, zero aborts) and on real Parallels
  hardware (the `NO_FS` path - no disk driver exists there - plus a
  `test-parallels.sh` extension typing `>` as a real held-Shift
  scancode chord). See `CLAUDE.md`'s "Output redirection" section and
  `docs/shell-commands.md`.
- ~~**Lifting `mkdir`'s no-directory-extension limitation** - a full
  parent directory currently makes `mkdir` fail rather than growing it.~~
  **Done** — `insert_dir_entry` (the single choke point every
  entry-creating operation goes through: `mkdir`/`touch`/`write`/`cp`/
  `mv`) now grows a full directory by one cluster, claim-then-zero-then-
  link ordering so a partial failure never corrupts the chain; the
  `DirectoryFull` error is gone (unconstructable). The real correctness
  piece this exposed: `rmdir` freed exactly one cluster — correct only
  while directories were single-cluster by construction — and would have
  leaked extension clusters; fixed with a shared `free_chain` helper
  that also deduplicated `rm`'s and `write_file`'s identical existing
  loops. Confirmed organically on QEMU (fill a subdirectory past its
  512-byte cluster with 20 entries, write/cat on an extended-cluster
  file, root-directory extension too, reboot persistence, rmdir on the
  two-cluster directory, freed-cluster reuse, sibling integrity, zero
  aborts) — see `CLAUDE.md`'s "Directory extension" section.
- ~~A real relocating loader (ELF + relocation processing) — would also
  lift the current `core::fmt`/`write!` restriction in userland
  programs.~~ **Done** — real ELF64 parsing and `R_AARCH64_RELATIVE`
  relocation processing, confirmed on both QEMU and real Parallels
  hardware (`core::fmt`/`write!` and slice/literal comparisons both work
  correctly now, via the shell's `selftest` command, on both platforms),
  plus a real pre-existing kernel bug this work surfaced and fixed (the
  SVC trampoline losing `x9` across every syscall — also confirmed fixed
  on real hardware via a genuinely multi-digit `uptime` value). See
  `CLAUDE.md`'s "A real relocating loader" section for the full writeup.
- ~~Blocking/waiting primitives, so tasks aren't limited to unconditional
  round-robin `wfe` polling.~~ **Done** — `tasks.rs` gained real
  `Runnable`/`Blocked(reason)` task state, a `block_current_and_switch`
  that suspends the calling task and switches to another runnable one
  mid-syscall (not via `wfe` - real Parallels hardware has a confirmed,
  unresolved hang when an EL0 task executes it, so this mechanism
  deliberately never does), and a per-tick wake-check. The shell's main
  loop now blocks on a real `read_char` syscall instead of busy-polling.
  Confirmed on QEMU and real Parallels hardware. A real, if incidental,
  bug surfaced and fixed along the way: the SVC trampoline's saved-frame
  layout never matched `Context`'s real field order (no `SP_EL0` slot at
  all, `ELR`/`SPSR` at the wrong offset) - harmless for every syscall
  before this one, since none needed the frame to be a fully
  interchangeable `Context`, but it directly corrupted the resumed
  task's `ELR_EL1` the first time one did. See `CLAUDE.md`'s "Blocking
  primitives" section for the full writeup.
- ~~Dynamic task creation and `exec()` — running more than one loaded
  program, or reloading one without a reboot.~~ **Done** — a new `spawn`
  syscall (16) loads a program from disk at runtime and starts it as a
  genuinely new, independent task alongside whatever's already running
  (`tasks::spawn`, not exec-replaces-current-process — the shell command
  is named `exec` to match this item's original wording, but nothing
  about the calling task is replaced). Needed a runtime physical-page
  bump allocator (nothing before this handed out RAM after boot
  services exited), `mmu::install_identity_map` made callable a second
  time (reusing the exact "swap the whole table set while code keeps
  running" mechanism already proven at boot, not a new incremental-remap
  primitive), and the scheduler grown from a fixed 2 slots to 4 with an
  `Unused` state for the two new ones. A real bug surfaced and fixed
  along the way: the ELF parser's `Vec`-based program-header parsing
  hung completely (no exception, no output) when called from this new
  runtime path, since the global allocator is boot-services-backed and
  invalid post-`exit_boot_services` — fixed with a fixed-capacity
  `[ProgramHeader; 16]` instead. Confirmed on QEMU (two shell instances
  alive concurrently, ticks still advancing, zero aborts) and on real
  Parallels hardware for every piece except the actual disk-load success
  path, which real hardware can't reach yet — a pre-existing,
  already-tracked gap (no working virtio-blk on Parallels at all, see
  below), not something this feature introduces. See `CLAUDE.md`'s
  "Dynamic task creation and `exec()`" section and
  `docs/architecture.md`'s "Dynamic task creation" section for the full
  writeup.
- **Disk on real Parallels hardware — diagnosed (2026-08-17), and the
  answer rules out every documented PCI storage controller.** A
  dedicated diagnostic round (see `CLAUDE.md`'s "Parallels disk
  diagnostic" section) confirmed with fresh evidence, not the old
  inventory alone: the VM's own boot disk is attached as `sata:0`, yet
  the PCI bus shows **no storage controller of any kind** — the same
  five devices as ever (audio, EHCI, xHCI, virtio-net, and Parallels'
  proprietary vendor-`0x1ab8` device). Deliberately attaching a scratch
  disk as `scsi` with subtype `lsi-sas`, then `lsi-spi` (`prlctl set
  --device-add hdd --iface scsi --subtype ...` — the only interfaces
  prlctl offers an ARM64 EFI VM are `ide` and `scsi`; `buslogic` is
  rejected by Parallels itself as EFI-incompatible) changed nothing:
  the inventory is byte-identical, the emulated controllers simply
  don't exist on Apple Silicon. Conclusion: *all* storage on this
  platform flows through a non-PCI/proprietary path (the `0x1ab8`
  device is the only candidate), so "implement a documented spec"
  is not available for any *attached-image* disk. **The one real
  documented-spec lead left: USB mass storage.** The xHCI controller is
  on the bus and this kernel already drives it end to end (the
  keyboard); a USB storage device passed through to the VM would be
  reachable via USB Mass Storage Bulk-Only Transport + SCSI commands
  over that same driver — a genuinely documented protocol stack,
  building on the project's own working xHCI code, at the cost of disk
  content living on a real USB stick rather than `esp.hdd`. Untested;
  needs a real passed-through USB storage device to even scope
  properly.
- ~~**USB mass storage over xHCI**~~ — **DONE (2026-08-17, the same
  day it was scoped): real Parallels hardware has a working disk for
  the first time.** `usb_msd.rs` (Bulk-Only Transport + SCSI
  INQUIRY/READ CAPACITY/READ(10)/WRITE(10)) over new bulk endpoints in
  `xhci.rs`, a `BlockDevice` abstraction decoupling `fat32.rs` from
  virtio, boot-time auto-mount (QEMU) plus a `mount` command with a
  runtime port rescan (Parallels, where passthrough attaches seconds
  after boot). Confirmed end to end on real hardware: `mount` on the
  real Lexar USB 3.x stick → INQUIRY with its real vendor strings,
  capacity 243,404,800 sectors, FAT32 mounted, `ls` of the stick's
  actual contents. Reads and writes confirmed on QEMU (including
  reboot persistence and typing-during-disk-I/O keyboard survival);
  real-stick testing kept read-only by policy. **Historical scoping
  notes below kept as written.**
  **GO - confirmed with a real USB 3.x stick (2026-08-17, second
  enumeration check):** a SuperSpeed flash drive (`idVendor=0x21c4`,
  `bcdUSB` 3.2) passed through to the VM landed on the xHCI
  controller's port 3 at `speed=4`, enumerated through this kernel's
  own multi-device scan, and presented **exactly the target
  interface - `class=0x08 subclass=0x06 protocol=0x50`
  (SCSI-transparent, Bulk-Only Transport)** - with the mass-storage
  callout printed and the device left addressed, keyboard working
  alongside it. The driver plan below applies as written. **One real
  design input found by the same diagnostic:** Parallels attaches
  passthrough USB devices *a few seconds after* VM start - the
  temporary port dump showed only the mouse/keyboard 6 seconds in,
  and the stick appeared moments later mid-scan (the first, bare run
  missed it entirely) - so the storage driver needs a delayed or
  repeated port scan (or minimal hot-plug via Port Status Change
  events) rather than trusting the current boot-time one-shot.
  **The first enumeration check (a USB 2.0 stick) had already found
  the complementary negative:** A
  SanDisk Cruzer Glide (high-speed/USB 2.0, per Parallels' own device
  listing) was passed through via `prlsrvctl usb set` and confirmed
  `Connected-To-Vm: YES` while the VM ran — yet a temporary in-kernel
  diagnostic (dump every connected xHCI port at scan time, wait 6
  wall-clock seconds, dump again; removed after the answer) showed
  only the same two ports as always (virtual mouse, keyboard), before
  *and* after the wait. Conclusion: Parallels routes USB 2.0
  passthrough devices to the **EHCI (USB2) controller** — also on the
  PCI bus, but a whole separate host-controller driver this kernel
  doesn't have — not to xHCI. **Next concrete step: repeat the same
  zero-code check with a USB 3.x stick**, which should land on the
  xHCI root ports; only if that works does the driver plan below
  apply as written (the alternative — an EHCI driver just to reach
  USB 2.0 devices — is a second full HC bring-up, much worse value).
  If a 3.x stick enumerates,
  the driver work is: recognize a Mass Storage interface
  (`bInterfaceClass=0x08`, subclass `0x06` SCSI-transparent, protocol
  `0x50` Bulk-Only Transport) during the port scan the same way the
  keyboard's HID interface is recognized today, configure its bulk
  IN/OUT endpoint pair (the first non-interrupt endpoint type this
  driver would ever drive), and implement BOT's CBW/CSW framing around
  a minimal SCSI command set (`INQUIRY`, `READ CAPACITY(10)`,
  `READ(10)`, `WRITE(10)`) — then adapt `fat32.rs` to sit on a second
  block-device backend besides `virtio_blk::Device`.
  **Update (2026-08-17): the multi-device groundwork is done** — the
  known constraint this item named (one-port/one-device, keyboard and
  stick must coexist) is lifted: `xhci.rs`'s scan now enumerates every
  connected device into its own slot with per-device EP0 rings/output
  contexts, logs every interface's class/subclass/protocol (a
  passed-through stick's boot log now directly shows the mass-storage
  classification this item's scoping check needs, with an explicit
  class-`0x08` callout - already proven against QEMU's `usb-storage`
  via the new `make run-usb-multi` three-device rig, which showed
  exactly `0x08`/`0x06`/`0x50`), and keeps non-keyboard devices
  addressed, ready for a driver. Confirmed on real Parallels hardware
  (mouse + keyboard concurrently addressed, typing unregressed). See
  `CLAUDE.md`'s "xHCI multi-device support" section. Remaining
  constraints going in: everything stays polled, matching the rest of
  the kernel; no hot-plug (the stick must be attached before boot);
  no hubs.
- ~~Task destruction~~ — **done (2026-08-17).** The `EXIT` syscall (17)
  lets a task end itself: slot freed for a future `spawn`, EL0 mapping
  dropped, RAM reclaimed by the runtime allocator in the common LIFO
  case (leak otherwise — a bump cursor, not a free list, deliberately).
  Tasks 0 (boot shell/keyboard owner) and 1 (idle) are refused. Came
  with `hello/`, the second real userland program (banner + exit), the
  natural test vehicle since a spawned shell can never exit on its own.
  Confirmed on QEMU (exec/exit cycles visibly reusing the same slot
  *and* region base) and real Parallels (the typed-`exit` refusal
  path). What was deliberately left for a job-control milestone is
  now done too - `kill <n>` and `fg <n>` (keyboard handoff with
  automatic revert on the owner's death; a spawned shell is a real
  nested interactive session now) - and `wait()`/reaping landed the
  same day too: exit statuses are kept (`Zombie(status)` holds the
  slot until a `wait <n>` collects it - `kill` reaps immediately),
  `wait` blocks on the same machinery as `read_char`
  (`WaitReason::TaskExit`), and Ctrl+C interrupts a wait so the
  session can't be bricked. The task-lifecycle arc
  (spawn/ps/exit/kill/fg/Ctrl+C/wait) is complete; nothing is left in
  this area short of real signals/parent-child process trees. See `CLAUDE.md`'s "Task destruction" section.
- **Actual microkernel-style driver isolation** — moving components
  out of the EL1 kernel and into supervised EL0 processes, per
  `docs/research-minix-boot.md`'s comparison (process-boundary
  isolation, MINIX's answer) and `docs/research-helix-os.md`'s.
  **Part 1 — real IPC — is done (2026-08-17):** fixed-size (≤64-byte)
  copied messages, bounded per-task mailboxes, blocking `msg_recv` on
  the proven `WaitReason` machinery (Ctrl+C-interruptible), mailboxes
  cleared on task death, proven end to end by `pong/` (the fourth
  userland program, a real long-lived echo server) round-tripping
  messages with the shell on QEMU, and the error/interrupt paths on
  real Parallels hardware. **Part 2 — the first component actually
  moved — is done too (2026-08-18): the FAT32 filesystem lives in
  userland** (MINIX-style: pure logic, no MMIO/DMA — hardware drivers
  can't be meaningfully isolated without an IOMMU anyway). `fsd/` (the
  fifth userland program) owns the kernel's old `fat32.rs` essentially
  verbatim, boot-loaded into protected task slot 2; clients reach it
  via a synchronous `MSG_CALL` round trip (new syscall 29: direct
  delivery plus a reply-sender filter, MINIX-sendrec-shaped) carrying
  `FSOP_*` requests whose contracts are the old `fs_*` syscalls'
  verbatim; the kernel keeps only raw sector access
  (`BLOCK_INFO`/`BLOCK_READ`/`BLOCK_WRITE`, syscalls 26-28, accepted
  from task 2 alone — the "supervised" part), and syscalls 7-14 are
  numbering gaps now. `spawn` became a two-step staged flow (the
  kernel can't read a path anymore; `exec` reads via the server and
  feeds `SPAWN_STAGE`), and `mount` a server-first two-phase flow with
  device replacement. Confirmed end to end on QEMU (full disk surface
  over IPC, chunked exec, reboot persistence, FAT16 and
  missing-FSD.BIN degradation, the USB replace flow) and on real
  Parallels hardware (the whole mount → INQUIRY → server-side FAT32
  mount → `ls` of the real stick chain; reads only by policy). See
  `CLAUDE.md`'s userland-filesystem section. **The follow-up - EL0
  fault isolation + server supervision - is done too (2026-08-18):**
  an EL0 fault now kills just the faulting task and the system keeps
  running (a new resumable fault trampoline in `exceptions.rs`; a
  userland wild pointer used to halt the whole kernel), tasks blocked
  mid-`msg_call` to a dying task get a cleanly failed call on every
  death path (`tasks::fail_calls_to`), and a crashed filesystem server
  is restarted from an image kept at boot (remounting from disk,
  3-restart crash-loop cap, then graceful give-up). Confirmed on QEMU
  by direct fault injection: a crashing spawned program killed alone
  with the shell running on; fsd crashed mid-call four times - three
  restarts each followed by a working remount, then the capped
  give-up; a task-0 fault still halting cleanly. Not covered,
  deliberately: a wedged (looping, non-faulting) server - no watchdog
  **(update: a wedged server IS caught now, by the 2026-08-19 heartbeat
  in `supervisor.rs`; the crash-recovery here was also generalized past
  fsd to cover `cond` - see the "Recently completed" note near the top)**;
  and no journaling for disk state corrupted mid-write. Pipelines
  (`builtin | program`, data streaming between processes over IPC with
  an empty-message EOF convention and a real-tick timeout kill for
  non-reading programs - see docs/shell-commands.md) landed the same
  day, closing the last small item queued behind IPC. **Per-task memory protection is
  done too (2026-08-18): isolation is MMU-enforced now, not
  trust-based.** Each scheduler slot runs under its own
  translation-table view where only its own region is EL0-accessible;
  touching another task's memory faults and (via the fault-isolation
  work) kills only the toucher, proven by A/B fault injection on QEMU
  and confirmed on real Parallels hardware. The filesystem protocol
  became fully self-contained to survive this (payloads inline, no
  cross-task pointers - FSOP v2 over 768-byte messages), and the
  syscall boundary now validates every pointer against the caller's
  own region. Per-task ASIDs (a per-switch-TLBI optimization) were
  implemented, passed on QEMU, but faulted the idle task on real
  Parallels - reverted in favor of flush-on-switch, correct on both;
  ASIDs stay a recorded future optimization.
  **Part 3 - a second driver out of the kernel - is in progress
  (2026-08-19): the console server, `cond/`.** Per the
  microkernel-comparison assessment, moving a *second* component out is
  the highest-value proof the `fsd` pattern generalizes (the block
  transport can't move without an IOMMU, so the console's text-rendering
  logic is the candidate). **Stage 1 (byte-stream) is done and verified
  on QEMU:** `cond` is boot-loaded into a new protected slot 3
  (`CON_TASK`); userland output routes to it as batched `DSPOP_WRITE`
  `MSG_CALL`s and it forwards via the gated `CON_WRITE` syscall (33),
  with a `PUTC` fallback - the stdout-over-IPC substrate the pipe/redirect
  items wanted. A needed scheduler fix landed with it
  (`block_current_and_switch_to`: `MSG_CALL` switches straight to the
  destination server, making round trips sub-tick as the ABI doc already
  promised - without it, per-character echo dropped burst input).
  **Stage 2 (framebuffer) is done and verified on QEMU too:** the
  text-rendering logic (cursor, wrap, scroll, ANSI, the font) moved into
  `cond`, driving new *dumb* gated pixel primitives in `fbdev.rs` -
  `FB_BLIT`/`FB_SCROLL`/`FB_CLEAR` (chosen over mapping the framebuffer
  into EL0, which would need the MMU surgery that faulted real Parallels
  once). Confirmed on QEMU `ramfb` by QMP screendump (wrapped `help`,
  `clear` actually clearing - which never worked on the kernel's old
  fbconsole - and 32 `help`s scrolling cleanly); the kernel keeps a
  minimal emergency console (fbconsole/UART) for boot/faults.
  **Real-Parallels confirmation is pending** - the framebuffer backend is
  what matters most there (no UART console), on the user to test like
  every framebuffer milestone. See `CLAUDE.md`'s "Driver isolation, part
  3". **(Update: the general supervision/heartbeat mechanism named here
  as the next step is now done - 2026-08-19, `supervisor.rs`; and the
  capability model for who-may-call-whom that followed it is done too -
  2026-08-20, IPC send-mask enforced at the `MSG_SEND`/`MSG_CALL`
  boundary. See the "Recently completed" notes near the top.)**
- **Grant/safecopy IPC - the enforced bulk-transfer primitive - is
  done** (confirmed end-to-end on both QEMU and real Parallels hardware:
  `cat /hello.bin` streamed a full 5784-byte binary off the USB stick,
  no truncation, where the old `cat` stopped at 512 bytes). Two syscalls
  (`grant`/`safecopy`) move bulk file data directly between two isolated
  regions under a kernel-enforced capability (an explicit grant + an
  active call), lifting the 512-byte per-op cap for file reads/writes to
  `SAFECOPY_MAX` (2048): `cat` streams any size, `cp`/redirect handle
  larger files. This is the second half of MINIX's IPC design (small
  messages + kernel-mediated copies).
- **The FAT32 offset-write primitive (`fat32::write_at`) is done** -
  write at a byte offset, extending the file, without rewriting the
  bytes before it (a partial-sector read-modify-write, the one piece no
  prior write path had). `cp` streams through it and copies a file of
  **any size** now (proven on QEMU with the 72KB shell binary,
  reboot-persisted, and confirmed on real Parallels hardware - a
  streaming `cp` and a `>>` append both verified on the real USB stick);
  `>>` appends at the end of a file of any existing size (the old "too
  large to append" refusal is gone). ~~**Still next**:
  *interior/random-access* writes (`write_at` refuses
  an offset past EOF - no sparse files) for a future editor/log~~ **done
  (2026-08-21): `write_at` now supports a random-access write at any offset,
  zero-filling a past-EOF gap; the `writeat` builtin is the consumer - see
  `CHANGELOG.md`**; the
  **userland heap and guard page from this note are both done** (see
  `CHANGELOG.md`) - the stack is a 16KB *guarded* stack now, and each
  program has a 256KB raw heap area (`heap_info`) the shell uses to lift
  its capture cap; a real `alloc`-backed heap (`Vec`/`String`) stays
  blocked on stable (prebuilt lib`alloc` isn't PIE, `-Z build-std` is
  nightly); moving
  further components out of the kernel; and ~~program-to-program pipes (a
  task's own output still isn't capturable, so pipelines are builtin-left
  only - a stdout-over-IPC model is the real fix)~~ **done — a per-task
  stdout target made a program's output capturable (`exec > file`, pipes
  relayed through the shell), and runtime capability delegation (2026-08-21)
  then made `programA | programB` stream directly, shell out of the byte
  path. See the "Recently completed" notes above.**

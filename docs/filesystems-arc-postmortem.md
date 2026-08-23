# The filesystems arc: exFAT, ext2, and what real hardware revealed

*A design-and-debugging retrospective (the tenth), covering one long day:
adding exFAT and ext2 to `fsd` — each read-only then read-write — validating
every step against foreign checkers, a tidy-up of the tree along the way, and
then taking the whole body of work to real Parallels hardware, which found a bug
the emulator can never show. Written for other bare-metal-OS developers.*

A companion to the [shell & filesystem](shell-and-filesystem-postmortem.md)
postmortem (which brought up FAT32) and the [USB storage](usb-storage-postmortem.md)
one (which gave Parallels its first disk). This is the arc where the single
hardcoded FAT32 engine became a real multi-filesystem layer — and where the
value of validating against something other than your own code paid off
repeatedly.

## The starting point

`fsd`, the userland filesystem server, *was* FAT32: one hardcoded engine, one
mounted filesystem. A prior refactor had already wrapped it in a `Filesystem`
enum with a single arm and a `mount()` that probed partitions — a deliberate
"prove it's byte-identical first" step that did nothing but set up for this. The
goal now: a second filesystem (exFAT), then a third and structurally different
one (ext2), each read-only first and read-write second, all behind the
unchanged `FSOP_*` IPC protocol clients already spoke.

## Lesson 1: an abstraction is only tested by a *different* implementation

exFAT went in easily, and that was almost a trap. Structurally exFAT is the same
shape as FAT — a partition, a FAT, a heap of fixed-size clusters, a directory of
entries — so the read machinery mirrored `fat32.rs` closely and the `Filesystem`
enum barely had to stretch. If the arc had stopped there, the "abstraction"
would have been validated only against a near-twin of what it already held. That
proves very little.

**ext2 is what actually tested it.** ext2 is a genuinely different model: inodes
own a file's metadata and a directory entry is just `name -> inode number`, so
resolving a path bounces between directory blocks and a separate inode table;
files are reached through block-group descriptors and direct/indirect block
pointers; names are case-*sensitive*. Making ext2 work through the *unchanged*
`FSOP_*` protocol — nothing above `fsd` touched — is the real proof the
abstraction was an abstraction and not FAT with extra coats of paint.

The generalizable point: if every implementation behind an interface is similar,
the interface is untested. Reach for the most *different* second implementation
you can, early, or you're just writing the same thing twice and calling it a
layer.

## Lesson 2: validate against a foreign checker, never your own output

The single most valuable habit of the arc was never trusting the driver's own
reads to prove its own writes. Every write stage was checked with a tool written
by someone else:

- **exFAT**: after the driver created files/dirs, macOS's *own* exFAT driver
  mounted the volume and listed them; `fsck_exfat` pronounced the bitmap and
  directory hierarchy clean after a churn of create/write/delete/rename; and a
  binary copied *by the OS* read back byte-identical when dumped on the host.
- **ext2**: `e2fsck -fn` ran all five passes clean after each stage, and
  `debugfs` dumped a copied binary that matched the original byte-for-byte.

When a different vendor's `fsck` signs off on your on-disk structures — checking
invariants your own code never looks at (free-count agreement across three
places, link counts, orphan lists) — you have evidence your own reader can't
give you, because your reader shares your bugs. This caught real problems (next
lesson) and, just as importantly, gave confidence that the clean stages were
actually clean.

## Lesson 3: a field's meaning can depend on state — the e2fsck "orphan list" bug

The best bug of the arc. After `rm`/`rmdir`, ext2 files were being freed
correctly by every measure I could see: link count zero, inode bitmap bit
cleared, a deletion time set, and the superblock's `s_last_orphan` was `0` — no
orphan list at all. Yet `e2fsck` reported a **"corrupted orphan linked list"**
naming exactly the inodes just deleted.

The cause: e2fsck treats a links-0 inode whose `i_dtime` is *less than the inode
count* as an entry on the orphan list, using `i_dtime` itself as the
next-orphan-inode pointer. The driver had written a sentinel deletion time of
`1` — which e2fsck read as "next orphan = inode 1", fabricating a bogus chain.
The fix was to write a plausible *timestamp* (a fixed large constant, since there
is no RTC) instead of a small sentinel.

The lesson generalizes past ext2: **a field can be overloaded to mean different
things depending on the record's state**, and a value that's obviously harmless
in one reading (a nonzero "deleted" flag) can be actively wrong in another (a
next-pointer). "Set it to 1, that's clearly fine" is exactly the kind of
assumption a foreign checker exists to puncture.

## Lesson 4: read-first, write-later, and stage the write

The corruption risk in a filesystem lives entirely in the writes, so the whole
arc leaned on the same discipline FAT32's own history taught: each filesystem
landed **read-only as one milestone**, then read-write as a **sequence of small,
independently-validated stages** — allocation + `touch` (no data blocks); then
data writes (`write_file`/`write_at`); then directories (`mkdir`/`rm`/`rmdir`);
then `mv`. Each stage was booted, exercised, and `fsck`-checked before the next
began. exFAT's checksums (the whole-set `SetChecksum` and the up-cased
`NameHash`) came out right on the first real test precisely because the
entry-set construction was the *only* new thing being checked that stage — a
narrow surface is a debuggable surface.

## Lesson 5: the emulator hides an entire class of bug

With all of it QEMU-green, the arc ended by taking the work to a real Parallels
VM. Booting, the shell, the servers, the task table, the relocation self-test —
all clean, confirming the churn (including a mid-arc reorganization of the source
tree and disk-image outputs) hadn't regressed the hardware path.

Then the filesystem-on-real-hardware test found something QEMU structurally
cannot show. Parallels exposes no disk to the kernel except USB mass storage, so
the test needed a real USB stick. Two boot configurations produced an *inverse
correlation*:

- Boot from the virtual hard disk with the stick passed through late: the **USB
  keyboard works, but USB storage reads degrade to device-I/O errors** shortly
  after mount — the *same* sectors that read fine during mount.
- Boot from the USB stick itself: **storage works end to end, but the keyboard is
  never addressed** — no shell input at all.

Neither is a filesystem bug; the FS drivers read and wrote correctly whenever the
block layer served them. It's the xHCI/USB stack failing to keep a HID keyboard
*and* a mass-storage device concurrently live — a device-addressing/enumeration
contention (the driver's "up to 4 concurrently addressed devices" limit and
ordering) plus mass-storage endpoint recovery.

The reason QEMU never hinted at this: there the keyboard is a *synthetic* device
driven by the host's `send-key-event` and storage is *virtio-blk* — they never
share an xHCI controller, so the contention that defines the bug doesn't exist to
be found. **Some bugs only exist where real hardware forces a sharing the
emulator abstracts away.** An emulator is a hypothesis about the hardware;
real-hardware passes are how you find where the hypothesis is wrong. This one did
exactly its job — it turned an unknown into a specific, recorded, next-to-fix
bug — even though (fittingly) the same bug is what made the bug awkward to
demonstrate.

## The shape of the arc

Two filesystems, each read then write, all behind one unchanged protocol; every
stage checked by a tool that didn't share the driver's assumptions; and a
real-hardware pass that both confirmed the work and surfaced the one thing the
emulator could never reveal. The recurring disciplines were the old ones —
*prove the abstraction with a different implementation, stage the risky writes,
and verify against something outside your own code* — plus a new one worth
keeping: **run on real hardware not to confirm success but to find the failures
your emulator is structurally incapable of producing.**

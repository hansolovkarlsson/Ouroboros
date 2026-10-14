# One disk for boot and root: the plan

**Set by Hans on 2026-10-12 as the direction after multi-core** (`ROADMAP.md`,
"The Raspberry Pi, after the first full session", item 3, which this plan
merges with item 1). One disk carries both the boot partition (FAT32, the
ESP) and a root partition (ext2 or exFAT). It may be a stick, a card or a
virtual disk, and the system needs nothing else to boot and run.

**Status 2026-10-14: steps 1 to 5b built, D1, D2, D5 and D6 settled, D3 and
D4 deferred** (`kernel/src/bootdisk.rs`,
`scripts/disksig.py`, `make test-boot-partition`, `scripts/bootstick.py`,
`make test-bootstick`); the rest planned. Grounded in the code at
`ebdebc1` (v0.23.0). Six decisions are Hans's, listed under "Decisions"
below: D1 ext2, D2 the image written, D5 the ESP read-only when the root
does not mount, D6 a failed read retried once and named; D3 and D4
deferred. Claims not yet checked on this tree are marked
(predicted).

## Why

Today each platform splits the system across two places, and each split has
a cost:

- **The Raspberry Pi 4** boots from the SD card (the pftf firmware and the
  ESP) and runs from a USB stick, because the kernel has no SD driver and
  the card is invisible after `exit_boot_services` (`testing-pi4.md`
  section 6, "The storage surprise"). Card and stick are staged separately
  from the same build, and nothing checks that they match (`ROADMAP.md`,
  "Stage card and stick from the same build").
- **Parallels** boots from `build/esp.hdd` and runs from a passed-through
  stick, because the kernel can read no storage on Parallels but USB: there
  is no storage controller on its PCI bus (`usb-storage-postmortem.md`,
  Finding 1). The `.hdd` is only a way to reach the firmware.
- **QEMU** already boots two-partition images (`make run-image-ext2`,
  `run-image-exfat`), but only with the root partition **first** and the
  ESP second. The reason is that `fsd` mounts the first partition it can
  (below), so ESP-first would make the ESP the root.

One disk ends the matching problem, puts accounts and `/bin` on the same
medium as the kernel that expects them, and lets the root be ext2, the one
filesystem where `fsd` enforces permissions.

## What the code does today

How the system decides which disk and which partition it uses, as read at
`ebdebc1`:

1. **The boot programs come from the ESP, through the firmware, before the
   exit.** `loader.rs` reads `\EFI\ORBS\INIT.CFG`, the shell it names, and
   `FSD.BIN`, `COND.BIN`, `NETD.BIN` and `ACCOUNTD.BIN` through
   `get_image_file_system(image_handle())`, the volume the kernel itself was
   loaded from. Nothing is read from the ESP after the exit. The supervisor
   restarts servers from in-RAM copies, and every `/bin` program is read by
   userland through `fsd`.
2. **There is exactly one block device.** It is the `BLOCK` cell in
   `syscall.rs`, and the first one installed wins. virtio-blk is probed first
   (`main.rs`, `init_storage`), then USB storage. Two sticks: the xHCI scan
   drives the first and logs the second as "left addressed, not driven".
   **Nothing knows which disk the system booted from.** `LoadedImage` is
   opened only to log the image's address. No device path is read anywhere
   in the kernel.
3. **`fsd` mounts as `/` the first partition that validates.** `vfs.rs`'s
   `mount` walks the partitions in table order and tries FAT32, exFAT and
   ext2 on each. So partition order decides, and an ESP that comes first is
   the root. `mkext2.py` and `mkexfat.py` put the root first for exactly
   this reason.
4. **A second partition is reached by hand.** `mount <n> <path>` (root only)
   binds partition `n` in the calling shell's namespace. There is no boot-time
   mount configuration: no fstab, and `INIT.CFG` is one program path.
5. **`/bin` and `/etc` are tree 0.** `login`, `accountd` and `netd` read
   `/etc` from whatever `fsd` mounted as `/`. On a FAT32-root image that is
   the ESP, so `passwd` rewrites files on the boot partition.
6. **The kernel writes the ESP once**, `\EFI\ORBS\BOOTID.TXT`, before the
   exit (`bootid.rs`).
7. **The disk images carry no MBR disk signature.** Bytes 440 to 443 are zero
   in both `build/esp.img` (hdiutil) and `build/espext2.img` (`mkext2.py`),
   read 2026-10-13. A firmware device path that names an MBR disk by its
   signature names all of ours as 0.

**What the record already says about Parallels and a stick.** In 2026-08
Parallels booted from the passed-through USB stick itself: storage worked,
and the keyboard was never addressed until the boot scan learned to wait
for slower devices (`CHANGELOG.md`, the xHCI contention fix, "Mode B":
"stick present at boot, or booting from the stick";
`filesystems-arc-postmortem.md`). So the question `ROADMAP.md` item 3 asks
first, whether Parallels' firmware boots from the stick, was answered yes
on the tree of the time. Step 0 re-asks it on this tree.

## The shape of it

- **The ESP is partition 1, the root partition 2.** Partition 1 is where
  the Pi's boot ROM looks for its firmware files, so on the Pi this order is
  forced. It is also the conventional order everywhere else.
- **The kernel names its boot partition.** Before the exit it reads the
  device path of `LoadedImage`'s device handle and takes the `HardDrive`
  node: the partition's number, start LBA, size, and the disk's signature
  (MBR) or GUID (GPT). `uefi` 0.39 has the node (`HardDrive`,
  `partition_start`, `partition_signature`).
- **The block device is the disk the system booted from.** At install, a
  candidate is the boot disk if its partition table has an entry with the
  boot partition's start LBA and size, and its signature matches when both
  sides have one. Any other disk is left as today's fallback, with a log
  line. "Which of card and stick wins" (`ROADMAP.md` item 3) then has one
  answer: the firmware decides, by its boot order, and the kernel follows
  it.
- **`fsd` mounts as `/` the first mountable partition on the boot disk that
  is not the boot partition. It mounts the boot partition itself only when
  there is no other.** Order stops mattering, so today's images keep
  working: `esp.img` (one partition) mounts its FAT32, and `espext2.img`
  (root first) mounts its ext2.

## Steps

Each step ends with a check that can fail. A mutation is named for each new
check.

**0. Re-confirm the boots (no code).**
- **Parallels:** boot the VM with a stick carrying a whole `build/esp` tree,
  EFI included. The `.hdd` stays attached but is dropped from the boot order.
  Record which entry the firmware boots and whether the keyboard and the
  stick both come up. If it boots, the `.hdd` is no longer needed to run on
  Parallels.
- **The Pi 4:** read its EEPROM's `BOOT_ORDER`, which needs Raspberry Pi
  OS's `rpi-eeprom-config`, or the boot ROM's diagnostic screen on HDMI
  (predicted). Then try a stick staged as a card (pftf plus `build/esp`) with
  no card in the slot.

Both answers go into `testing-parallels.md` and `testing-pi4.md`. Only these
two boots can show it. QEMU already boots from a stick
(`test-usb-hub.py --usb-boot`).

**1. The kernel names its boot partition. Built 2026-10-13.** On QEMU the
path ends `HD(1,MBR,0x00000000,0x1,0x1FFFF)/\EFI\BOOT\BOOTAA64.EFI` from
virtio and runs through `USB(0x0,0x0)` from a stick, so the `HardDrive` node
is there in both. `make test-boot-partition` compares five boots with the
host's reading of each table, and fails both with the kernel naming
partition 1 and with the host reader taking the first partition. The
review of #285 added a null device path (an image loaded from memory), a
signature of a type the specification does not list, and a nested
partition, each logged rather than taken as something else. The step as
built: read the `HardDrive` node before the exit and log it as `boot
partition: N of MBR|GPT at LBA s, b blocks`, then `disk signature none` or
`0x...`, or `partition GUID ...`. Keep it in a static, class A in the
multi-core inventory
(written once before the exit, read-only afterwards), and give it a row in
`roadmap-smp.md` so `check-statics` passes. Check on QEMU: `run-image`
names partition 1, and `run-image-ext2` names partition 2 at the ESP's
start.
- Mutation: log the first partition instead. The ext2 check goes red.
- If a firmware gives no `HardDrive` node, log so, and every later step
  falls back to today's behaviour. A vvfat ESP (`make run`) was predicted to
  have none and has one: `HD(1,MBR,0xBE1AFDFA,0x3F,0xFBFC1)`, read
  2026-10-13, the one boot so far with a nonzero MBR signature.

**2. Images get a disk signature. Built 2026-10-13.** `scripts/disksig.py
set` writes each MBR image's signature at byte 440 as its recipe's last step
(`image`, `image-stall`, `image-ext2`, `image-exfat`, and so the 2vm pairs
built through them; step 4's image will do the same): the first four bytes
of SHA-256 over the image with those bytes counted as zero, 1 if that is 0.
`mkgpt.py`'s disk and partition GUIDs come from `esp.img`'s bytes the same
way, where they were random. The kernel reports what was written, on every
boot of `test-boot-partition`. Before any boot the rig refuses an MBR image
with no signature (it stopped there with `make image` left unsigned) and two
MBR images sharing one (it stopped there with `espext2.img` given
`esp.img`'s). `make test` runs `disksig.py --self-test`, eleven rules on
synthetic images.

The signature is the disk's name, not a checksum. The first version also
required it to equal the hash of the image's current bytes, and the review of
#286 found every boot breaks that: the boot counter's file is rewritten on
each one, so an image booted once was refused. The rule that step 3 needs is
the one kept: nonzero, and different between disks. A copy of an image
carries its source's signature, as a disk copied with `dd` does, so a rig
that attaches a copy beside its source must sign the copy again (step 3's).

Planned as "the same tree gives the same image", which does not hold:
hdiutil's FAT32 differs byte for byte between two builds of one tree. What
holds is per image: two images built different get different signatures
(the node-a and node-b pair, a stall image and its source). Planned too as
`make test` checking every image the Makefile builds; it does not build
them, so the rig checks the images it boots instead.

**3. The block device is the boot disk. Built 2026-10-13.**
`bootdisk::is_boot_disk` reads a disk's own table through a read-sector
function and answers whether it holds the boot partition: on MBR, entry
`number` with the same start and size and the disk signature the firmware
read; on GPT, entry `number` with the same start, size and unique GUID.
`init_storage` installs the virtio disk only when it matches, or when the
boot partition is unknown, and otherwise holds it back; `install_boot_disk`
then installs the USB stick if it matches, or the held virtio disk as the
fallback, logged. With the boot partition unknown the order is the old one.
`make test-boot-partition` gained two boots with `usbstick.img` beside the
boot disk. With the boot stick on USB and `usbstick.img` on virtio, virtio
is held back, the stick installed, and `ls /` shows the ESP's tree. With
the roles swapped, virtio is installed as before. Mutation: `is_boot_disk`
answering unknown, the old order, and the first boot failed four checks,
login among them, since `usbstick.img` has no `/etc`. A vvfat ESP still
installs its virtio disk (its sector 0 carries the signature the firmware
reported). Planned as:
- the rule applied in `init_storage` and in `try_install_usb_block_device`,
  with a rig booting from a stick beside an unrelated virtio disk: built so.
- `mount -a`'s rescan keeps its fallback, so a stick passed through late on
  Parallels is still taken: unchanged, and it does not ask.

The review of #287 changed four things. A disk is read by the table the
firmware named, so a hybrid MBR is read as the GPT it booted, and a GPT
whose primary header is gone is read from its backup, as `fsd` does. Every
value read from a disk is bounded with checked arithmetic, since a header
built to overflow had panicked a debug kernel. A node with no signature
matches on the table alone. And the fallback's line says why no other disk
went in: the stick refused, failed, or absent. A rig boot from NVMe, which
the kernel cannot drive, reaches that fallback (mutation: the fallback
dropped, and it failed), and every boot alone on its bus now checks that
its own disk was installed directly (mutation: the GPT match refusing, and
the GPT boot failed). Left as found: the choice spread over `init_storage`
and `install_boot_disk` rather than one chooser, and sector 0 read twice.
The medium review of #287 changed three more. A GPT header counts only if
its CRC and its entry array's CRC hold, as in `fsd`, so a stale "EFI PART"
or a corrupt primary falls to the backup (mutation: the header CRC test
inverted, and the gpt boot failed). Two overflows are no longer a match.
And a stick whose init failed leaves the device empty rather than mounting
the fallback, so `mount -a`, which replaces the device only when `fsd`
mounted nothing, can still reach it; a stick that attaches after the boot
scan is still not reached when a virtio fallback was installed. The fallback
boot now pins the firmware to its NVMe disk with `bootindex=0`: once the
rig's arguments were reordered, the firmware booted the stick beside it,
which carries an ESP of its own. Declined: two unsigned disks of the same
layout both match an unsigned boot partition, since signature 0 names
nothing; every image is signed since step 2. Deferred to `ROADMAP.md`: the
matcher in a pure crate with host tests, shared with `fsd`'s partition.rs.

Not covered, as before step 3: only the first virtio-blk slot is probed,
and the xHCI drives only the first storage device it finds, so two sticks
are still "first found" whichever was booted.

**4. `fsd` takes the root by the rule, and a combined image exists. Built
2026-10-13**, with D1 settled as ext2. A new system call,
`BLOCK_BOOT_START` (77), gated to `fsd` like the other `BLOCK_*` calls,
answers the boot partition's start LBA on the installed device, or 0 when
that device is not known to be the boot disk. The kernel keeps it beside the
device and every install must pass it (`install_block_device`'s second
argument), so the fallback and `MOUNT`'s late stick answer 0. The USB stick
is now always asked whether it is the boot disk, even when it is installed
regardless, so a stick booted from tells `fsd` so. `vfs::mount` tries the
boot partition last. `scripts/mkdisk.py` (`make disk`, `make run-disk`)
builds `build/disk.img`: the FAT32 ESP as partition 1 (type 0x0c,
bootable), the ext2 root as partition 2, signed. `make test-boot-partition`
boots it on virtio and as a USB stick, and every boot now checks the
filesystem `fsd` mounted: ext2 for `disk.img` and `espext2.img`, FAT32 for
`esp.img`, exFAT for `espexfat.img`. Mutations: `fsd` ignoring the boot
partition, and both `disk.img` boots mounted the ESP; the kernel installing
the stick with no start, and the USB boot did. The ESP still carries the
whole tree, `/bin` and `/etc` included, as every image's does; what the ESP
needs once a root sits beside it is step 5's question.

The high review of #288 found two ways the rule was not applied
everywhere. `format` took the first MBR partition, which on `disk.img` is
the ESP: it wrote over the boot partition and left the root `/` showed
untouched. It now takes the root by `mount`'s rule (rig: `disk-format`
formats a copy and finds `/` empty and the ESP whole; mutation: the old
rule, and it failed). And `MOUNT`'s rescan installed every stick with no
start, so the `mount -a` retry after a failed boot-time init mounted the
ESP; the rescan now asks the stick too (`bootdisk::boot_start`). That case
needs a stick whose init fails at boot, which no rig sets up. A root that
does not mount now says so (`fsd: no partition beside the boot partition
mounted`; rig: `disk-badroot`, a copy with its superblock zeroed; mutation:
the line removed, and it failed). `fsd`'s mount line is now one write: in
three it was split by `accountd`'s, which the rig's new check on what
mounted caught. The three image scripts share `scripts/mbrdisk.py`, and
write the same bytes as before.

The medium review of #288 tightened `format` again. It reads the root by
`mount`'s own order and probe (`vfs::root_order`, `Filesystem::probe`), so
a partition before the root holding no filesystem is not formatted in its
place, and it refuses a GPT disk, over which it would have laid a
filesystem from LBA 1, true on `main` too (rig: `gpt-format`; mutation: the
refusal dropped, and it failed). The fallback warning and the mount line
are one write. Kept: with the boot partition unknown, `format` formats
partition 1, which is then what `/` shows. On the one-disk layout that is
the ESP, and only naming the root by type closes it. A firmware counting in
blocks other than 512 bytes cannot skip the wrong partition: its LBAs would
not match the disk's table, the disk would not be the boot disk, and `fsd`
would be told 0.

A third review of #288 found the last fix's own regression: with the root
beside the ESP damaged, the probe reached the ESP, the only partition there
that still validated, and `format` would have written over it. `format`
now looks only beside the boot partition while another is there (rig:
`disk-badroot` formats its damaged root and finds the ESP whole). It also
takes its partitions from `partition::discover` and refuses what
`partition::is_gpt` calls GPT, the tests `mount` uses, so it cannot pick an
entry `mount` would not list. And after `erase` or `partition` rewrites
the table, `fsd` stops using the boot partition's start for the rest of
the boot, since it could name a new partition.

Two questions from the reviews of #288 (the series stopped there, the rest
being design rather than defect) went to Hans and are settled as D5 and D6
under "Decisions": a root that does not mount leaves the ESP mounted
read-only, and a boot stick's failed read is retried once and then logged
as a failed read. Both are step 4b.

Open for step 5, from that review: the root is the first partition beside
the boot partition that mounts, which is right for two partitions and a
guess for more (an exFAT data partition before the root would be `/`).
Naming the root, by MBR type 0x83 or a GPT root type GUID, would hold for
any layout. **Built 2026-10-14:** `partition::discover` records each
partition's type, and `vfs::root_order` puts the partitions typed as a root
first: MBR 0x83, or on GPT the Discoverable Partitions Specification's
AArch64 root GUID only, since the generic "Linux filesystem data" GUID
marks a data partition as much as a root. With a typed root on the disk,
the untyped partitions beside the boot one are not candidates at all, so
a typed root that does not mount leaves the ESP read-only (D5) rather than
a data partition as `/`; with none, the old rule stands, which
`espexfat.img` needs (its exFAT root's type, 0x07, is a data partition's
too). `format` takes the same order, so it formats a typed root even when
the root no longer probes, and with the boot partition unknown it now
formats the typed root, not partition 1, which closes the case kept open
above. `make test-boot-partition` gained `disk-data.img`, the ESP, an exFAT
data partition and the ext2 root: the root mounts, `format` takes it and
leaves the data partition whole, and with the root's superblock zeroed the
ESP comes up read-only. Mutations: the type ignored (all three failed), and
the untyped partitions kept as candidates beside a typed root (the damaged
root's boot failed).

The plan for step 4, as it was written:
- `fsd` asks the kernel for the boot partition: a new syscall, or a field of
  an existing one. That is settled at the step, and the number goes in
  `syscall-abi`.
- `vfs::mount` skips the boot partition while another partition on the disk
  mounts.
- `make disk` builds `build/disk.img`: MBR, the FAT32 ESP as partition 1,
  the root as partition 2 (decision D1), with `/bin`, `/etc`, `/man`,
  `/include` and `/Users` on the root. `make run-disk` boots it.
- Check: `run-disk` mounts the root and not the ESP. `esp.img` and
  `espext2.img` still mount what they do today.
- Mutation: today's first-partition rule. `run-disk` mounts the FAT32 and
  the rig fails.

**4b. Failing closed, D5 and D6. Built 2026-10-13.** D5: when `fsd` falls
back to the boot partition beside a root that did not mount, tree 0 is
read-only (`ROOT_READ_ONLY`): `handle_ninep` refuses every verb that
changes the disk with `FS_ERR_READ_ONLY` before `check_access`, so root is
refused too, and an `NP_OPEN` for writing, creating or truncating is
refused, which leaves no fid that could write. Any other mount of tree 0,
and `unmount`, clear it, so `unmount; format ext2; mount -a` still repairs
the root. The line now ends "read-only". Rig: `disk-badroot` has root's
`touch /rotest` refused as a read-only filesystem; mutation: the refusal
removed, and that check failed. D6: `bootdisk::is_boot_disk_retrying`
asks again once when a read failed on the way, and after a second failure
says the read failed, "though it may be" the boot disk; virtio, the boot
stick and `MOUNT`'s rescan all go through it. That needs a disk whose
reads fail at boot, which no QEMU rig sets up.

The review of #291 tightened D5 and changed D6's answer, and the second
review of #291 changed it back. The first review argued that a disk whose
table could not be read twice should be taken as the boot disk; the code
did that, and the second review found it putting an unrelated disk with an
unreadable sector, or a dead stick, in place of the real boot disk or a
working one. That is a design question, and it was being decided in review
rounds, so D6 is as Hans wrote it: the disk is taken as not the boot disk,
and the line says the read failed. The matcher answers yes, no or
unreadable (`bootdisk::Boot`), and a GPT copy that validates decides even
when the other copy could not be read, so a readable backup's no is a no.
The question of what an unreadable disk should be went back to Hans, with
its costs set out (leave it out unless nothing else is there; take it as the
boot disk), and **he kept D6 as built on 2026-10-13**: an unreadable disk is
not the boot disk, accepting that a one-disk stick failing its read mounts
its ESP as `/` read-write, and that a dead stick goes in over a working
virtio disk. Not to be re-litigated in a review.
A failed read of the backup header beside a primary that was read
and did not validate is not an unreadable disk, and `init_storage`'s first
read of sector 0 is tried twice (a disk that fails it twice is still dropped,
as on `main`).
`mutates()` is default-deny: it lists the verbs that only read, so a verb
added later is refused on a read-only tree until it is listed as reading
(no verb today is unlisted, so no rig can show that). The fid path checks
the read-only tree too, through one gate (`refused_read_only`), and the
open-for-write mask is one function, `ninep_abi::open_writes`, where `fsd`
had three copies. Kept: the flag is a
static for tree 0 rather than a property of the mount, and `FSOP_MOUNT_INFO`
does not report it; only tree 0 can be the fallback, and the two places
that can change it clear or set it.

**5. Staging a physical disk** (decision D2, settled: the image written). `make bootstick` puts the
combined layout on a stick, for the Pi with pftf on partition 1. `make
stick` and `make sdcard` stay until the two-disk layouts are retired.

**5a. Built 2026-10-14, for QEMU and Parallels** (`scripts/bootstick.py`,
`make bootstick BOOTSTICK=/dev/diskN`, `make test-bootstick`). It writes
`disk.img`'s layout to a whole disk, built from `esp.img` and
`ext2part.img` with the old disk's state laid over it: `/etc` file by file
(a file a newer tree introduced is added), `/Users` home by home (a home
the old disk has replaces the image's whole, so a file deleted on the
stick stays deleted; `make stick` keeps homes the same way), each with its
mode, owner and links. Hans added `/Users` to D2 on 2026-10-14, and kept
the root at the image's 24 MiB (growing it is a `ROADMAP.md` follow-up).
The disk keeps its signature across a re-stage, since it is the same disk,
and a disk that had none of ours gets a random one, never the image's: two
sticks written from one build must differ (step 2's rule). The boot
counter's file is carried too, the larger of the old and the new, since
rolling it back would let a machine whose variable store does not persist
hand out a counter twice (`bootid.rs`). `CARRY=0` resets `/etc` and
`/Users` only.

The guard: a whole disk's node, not internal fixed media, not the Mac's
own, large enough, and empty or ours. Ours is a disk this wrote (an ext2
partition 2 holding `/etc/srcid`), a `make stick` stick, or a written
`esp.img`; from the last two, which record no modes, files take the
image's mode for the same path and the owner of the home they are in. A
Pi card is refused. A file in place of a device is attached with `hdiutil`
and written through the same `/dev/diskN` path, which is how the rig
reaches the device code without `sudo`; a device the user cannot open goes
through `sudo dd`, which no rig reaches. The write is read back and
compared, and the disk's last MiB zeroed so an old backup GPT cannot be
found behind the new MBR.

`make test-bootstick`, 27 checks as first built: four refusals each leave their disk's
bytes unchanged, a fresh file holds the ESP and root as built, then three
boots. A user is added on the first; after a re-stage she logs in, reads
her file, and the counter goes on; after `CARRY=0` she is refused and the
counter still goes on. Mutations, each failing the rig: the root's carry
dropped, the counter's carry dropped, a new signature on every write, and
the guard taking any volume as empty. They run with `OUROBOROS_STALE_OK=1`:
`bootstick.py` is in the tree's source hash but not in the image, so a
mutated writer looks to the stale-image guard like a stale image, when the
image booted is the one built from the clean tree.

The high review of #293 found ten things, all taken:
- A `make stick` stick's homes went to root, since the passwd reader took
  Unix's seven fields and this project's are `name:uid:gid:home` (its own
  self-test fed it the wrong format).
- The old root's `/etc/srcid` was carried, so a stick re-staged from a newer
  build held two build stamps and the stale-image guard refused it. It and
  `/etc/disksig` are never carried.
- A Linux stick (FAT then ext4) had its whole root copied before it was
  refused. `/etc/srcid` is now looked for in place, on the device.
- A written `esp.img`'s boot counter was not carried; it is now, from the
  volume.
- The zeroed tail ended short of the last LBA on a disk that is not a whole
  number of MiB. It now runs to the end.
- On APFS the Mac's own disk was named by its synthesized container, never
  its physical store. The stores are looked up too.
- After a failed write, the only copy of the old `/etc` and `/Users` was
  deleted with the temporary directory. The disk as written is kept at
  `build/bootstick-written.img` until it reads back, and the error names it.
- A tree that failed to list was taken as absent and carried nothing; only
  debugfs's "not found" now means absent.
- A dd copy of `disk.img` kept the image's signature, which every such copy
  shares. The signature is kept only when `/etc/disksig` in the root says
  this script gave it.
- The layout, the MBR reader and the build's dependency were copies. The
  layout is `mbrdisk.one_disk` and the reader `mbrdisk.entries`, both used by
  `mkdisk.py` and `bootstick.py`, and `make bootstick` builds only the two
  payloads. `LITTER` stays a copy of `sdcard.sh`'s list, which is shell.

The rig grew to 32 checks: a `make stick` stick's home owned by its user, a
written `esp.img`'s counter carried, a dd copy given a signature of its
own, an odd-sized disk's old backup GPT header zeroed, and an old build
stamp left behind on a re-stage. Each failed with its fix reverted. No rig
reaches the APFS lookup, the kept image or the listing failure.

Left open: the ESP still carries the whole tree, `/bin` and `/etc`
included, as every image's does. Its `/etc` is what D5's read-only
fallback shows, with the image's dev accounts. Not carried: hard links
(each name gets its own copy) and timestamps.

**5b. The Pi's stick. Built 2026-10-14** (`PI=1`). The same disk with the
pftf firmware at the root of partition 1, where the Pi's boot ROM reads it,
beside the ESP tree: the pinned release on a fresh stick, put into a copy
of `esp.img` through `hdiutil` (the 64 MiB ESP holds the tree's 5 MB and
the firmware's 7 MB). The pin moved to `scripts/pftf.env`, fetched and
checked by `scripts/pftf.sh` for both `sdcard.sh` and `bootstick.py`, so a
card and a stick cannot get different releases. On a re-stage the stick's own firmware files are
carried whole, the release's own files and `config.txt` (after the review,
below), `RPI_EFI.fd` among them: it is the firmware and the store of its
settings and every UEFI variable, so this is `make sdcard`'s "installed
once". A Pi stick stays one by default; `PI=0` drops the firmware and says
so; `FIRMWARE=1` reinstalls the release, resetting the settings and keeping
the old `config.txt` (the UART overlay is set there by hand), and is
refused on a stick that is not a Pi's. A card as the target is still
refused, now saying why: the kernel cannot read it once it runs.

`make test-bootstick`, 39 checks, gained seven: the pinned files on a fresh
stick, file for file, without the readme or macOS's sidecars; a boot on
QEMU with the firmware beside the ESP; a changed `RPI_EFI.fd` and
`config.txt` carried by a re-stage; `FIRMWARE=1`; `PI=0`; the refusal.
QEMU cannot run pftf, so whether the Pi boots the stick is step 8.
Mutations, each failing the rig: the old firmware never carried,
`config.txt` not kept by `FIRMWARE=1`, a Pi stick not staying one, and
the refusal removed.

The high review of #294 found ten things, all taken:
- What was carried as firmware was everything at the old ESP's root that
  `build/esp` did not own, so a boot flag set to bisect a hang (`\NOXHCI`)
  stayed on every later kernel, and stray entries came back. Only the
  release's own names (the zip's top level, matched without case) and
  `config.txt` are carried now, and what was left behind is named.
- A failed `hdiutil detach` was ignored, and the copy then read from its
  backing file could miss FAT writes. A detach is retried and then refused
  (`bootstick.detach`), in the rig too, whose mounts each get their own
  mountpoint.
- An ESP that would not mount made a one-disk stick impossible to
  re-stage, `PI=0` included. It is now refused only when its firmware
  could matter: `PI=0` writes the disk without firmware, and `PI=1
  FIRMWARE=1` with the release's.
- A failed download said the ESP might be full. The fetch is
  `scripts/pftf.sh` now, the one fetch and check `sdcard.sh` and
  `bootstick.py` both run, with its own error.
- `PI=true` fell back to the default; every option is now 1, 0 or unset.
- The rig compared four of the release's files; it compares all fifteen.
- The `make` target took `PI` and `FIRMWARE` from the shell's environment
  too, so a `FIRMWARE=1` left exported would reset a Pi stick's settings.
  It passes each option from the command line only, and its usage lists
  them. CLAUDE.md's and the Makefile's rig descriptions are current.

The rig is at 43 checks: a flag and a directory not carried, an
unmountable ESP refused and then repaired by `PI=0`, and `PI=true`
refused. Each new check failed with its fix reverted, as did installing
one file short. No rig reaches a busy volume's detach.

**6. The boot partition at a path** (decision D3). Optional for the arc:
mount the ESP at `/boot` for every task, so a new kernel can be put on the
disk from inside Ouroboros.

**7. Parallels on the stick alone.** Run the multi-core boot checks
(`testing-parallels.md`, "Multi-core") with the stick as the only disk.
`make parallels-hdd` stays for the release `.dmg` unless decision D4 changes
what a release ships.

**8. The Pi 4 on the stick alone.** A full session with no card, then the
same with the card in the slot, to see which the firmware takes (step 3
makes the kernel follow it).

**Not in this arc:** the SD card as one disk needs the EMMC2 driver
(`ROADMAP.md` Pi item 2). This plan's rule applies to the card unchanged
once that driver exists. The firmware's variable store lives in
`RPI_EFI.fd` on whatever disk carries pftf, so on a boot stick it moves
there.

## Decisions (Hans's)

- **D1. The root filesystem: ext2 or exFAT.** ext2 is the only filesystem
  where `fsd` enforces permissions, so `/etc/shadow` is protected. But macOS
  cannot write ext2 without e2fsprogs and raw-device access, so a stick
  re-staged from the Mac needs `sudo`. exFAT is writable by Finder and by
  `sdcard.sh` as today, but it warns that accounts are not secure.
  **Recommended: ext2**, staged by writing a built image (D2).
  **Settled by Hans on 2026-10-13: ext2.**
- **D2. How a physical stick is staged.**
  - (a) Write the whole `disk.img` to the device: simple and exact, but it
    replaces accounts made on the device. `KEEP_ETC` would then have to
    become "copy `/etc` off the old root first".
  - (b) Stage each partition in place, as `sdcard.sh` does: possible for
    FAT32 and exFAT, awkward for ext2.

  **Recommended: (a) with an `/etc` carry-over.** It is a destructive write
  to a raw device, so it is guarded like `make sdcard` is: the marker file,
  and the device named, never guessed.
  **Settled by Hans on 2026-10-13: (a), the image written, `/etc` carried
  over from the old root.**
- **D3. Whether the ESP is mounted at `/boot`** (step 6), or reached only
  by `mount 0 <path>` as today.
- **D4. Whether a release ships the combined disk** in place of today's
  one-partition image. That would change the downloadable artifacts and
  `RELEASING.md`.

  **D3 and D4 deferred by Hans on 2026-10-13:** releases stay on today's
  one-partition image until step 0 shows Parallels and the Pi boot from a
  stick, and `/boot` is left out of this arc.
- **D5. A root that does not mount** (from the reviews of #288). **Settled
  by Hans on 2026-10-13: mount the ESP read-only.** When the disk has a
  partition beside the boot one and none of them mounts, `fsd` mounts the
  boot partition as `/` read-only, with the line it prints now: the system
  comes up to be looked at and repaired, nothing is written to the ESP, and
  accounts cannot be changed there. FAT32 still enforces no modes, so its
  `/etc/shadow` stays readable while it is `/`. Built as step 4b.
- **D6. A boot stick's read failing at boot** (from the reviews of #288).
  **Settled by Hans on 2026-10-13: retry once, then log.** The boot-disk
  check is retried once when a read fails; if it fails again, the stick is
  installed as not the boot disk and the line says the read failed, not
  that it did not match, so a one-disk stick's wrong root is visible.
  Built as step 4b. **Confirmed by Hans on 2026-10-13**, after the reviews
  of #291, with its costs: a one-disk stick failing its read mounts its ESP
  as `/` read-write, and a dead stick goes in over a working virtio disk.

## Risks

- **A firmware's device path may not end in a `HardDrive` node** for USB
  boot media, or may describe the partition differently (predicted).
  Step 1's log is the check, on QEMU, Parallels and the Pi.
- **The Pi's EEPROM may not try USB** without being reflashed (predicted).
  Step 0 reads it before anything is built for the Pi.
- **Parallels' keyboard with a stick at boot.** The 2026-10-12 boot lost
  keys with the stick passed through (`ROADMAP.md` item 4's sub-item). A
  stick-only Parallels inherits that, and this arc does not fix it.

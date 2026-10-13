# One disk for boot and root: the plan

**Set by Hans on 2026-10-12 as the direction after multi-core** (`ROADMAP.md`,
"The Raspberry Pi, after the first full session", item 3, which this plan
merges with item 1). One disk carries both the boot partition (FAT32, the
ESP) and a root partition (ext2 or exFAT). It may be a stick, a card or a
virtual disk, and the system needs nothing else to boot and run.

**Status 2026-10-13: steps 1 to 4 built** (`kernel/src/bootdisk.rs`,
`scripts/disksig.py`, `make test-boot-partition`); the rest planned. Grounded in the code at
`ebdebc1` (v0.23.0). Four decisions are Hans's, D1 settled (ext2), listed
under "Decisions" below. Claims not yet checked on this tree are marked
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
needs once a root sits beside it is step 5's question. Planned as:
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

**5. Staging a physical disk** (decision D2). `make bootstick` puts the
combined layout on a stick, for the Pi with pftf on partition 1. `make
stick` and `make sdcard` stay until the two-disk layouts are retired.

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
- **D3. Whether the ESP is mounted at `/boot`** (step 6), or reached only
  by `mount 0 <path>` as today.
- **D4. Whether a release ships the combined disk** in place of today's
  one-partition image. That would change the downloadable artifacts and
  `RELEASING.md`.

## Risks

- **A firmware's device path may not end in a `HardDrive` node** for USB
  boot media, or may describe the partition differently (predicted).
  Step 1's log is the check, on QEMU, Parallels and the Pi.
- **The Pi's EEPROM may not try USB** without being reflashed (predicted).
  Step 0 reads it before anything is built for the Pi.
- **Parallels' keyboard with a stick at boot.** The 2026-10-12 boot lost
  keys with the stick passed through (`ROADMAP.md` item 4's sub-item). A
  stick-only Parallels inherits that, and this arc does not fix it.

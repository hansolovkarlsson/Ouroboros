# The SD card as the only disk: the plan

*Asked by Hans on 2026-11-05: "put all that's needed to run on the SD card
only". This is `ROADMAP.md`'s Raspberry Pi item 2, made an arc.*

## Why

Today a Pi 4 needs two pieces of media: the card, which the firmware boots
the kernel and the boot programs from, and a USB stick, the only disk the
kernel can read once boot services end (`testing-pi4.md` §6, "The storage
surprise"). That costs a USB port that a keyboard, a mouse or other
equipment wants, and a second medium per board, which does not scale to
several Pis. The one-disk arc (`roadmap-one-disk.md`) already put the ESP
and an ext2 root on one disk and taught the kernel to take the disk it
booted from; its closing note says the rule applies to the card unchanged
once the card has a driver. That driver is this arc.

**Done when** a Pi 4 with a card and no stick boots to a login, mounts the
card's ext2 root as `/`, runs `/bin` from it, writes a file, and reads it
back after a reboot; and the same card with a stick also in a port still
takes the card, the disk it booted from.

## What is known (read 2026-11-05, from source, not yet from the board)

From edk2-platforms, the source of the Pi's UEFI firmware (pftf):

- **The card sits on EMMC2**, the BCM2711's SDHCI-style controller, when
  the firmware's `SdIsArasan` setting is 0 (`RPi4.dsc`'s default). The
  setup form marks Arasan as its default, so which value a fresh card has
  is not settled by the sources. Step 0 reads it.
- **The firmware's own driver reads the card by PIO** through EMMC2
  (`ArasanMmcHostDxe.c`, pointed at `MMCHS2_BASE` with the EMMC2 clock when
  not Arasan): no DMA. So a PIO driver does what the firmware already does
  on this board.
- **ACPI declares it** as `SDC3`, `_HID "BRCME88C"`, in an SSDT of its own
  (`Emmc.asl`, table `RPI4EMMC`, installed only when `SdIsArasan` is 0),
  `_STA` always present, one MMIO range and one interrupt. Its `_CRS` sets
  its base through a `MEMORY32SETBASE` macro (predicted: a `_CRS` method
  rather than a static buffer, which a byte scan like `amlscan`'s may not
  read). Its DMA window, `_DMA`, depends on the chip revision: 1 GB at bus
  `0xC0000000` mapping to CPU 0 on older ones, untranslated on newer.
- **Linux drives it** with `sdhci-iproc` (`brcm,bcm2711-emmc2` in a
  devicetree, `BRCME88C` in ACPI): one quirk, `MULTIBLOCK_READ_ACMD12`, and
  a minimum clock of 200 kHz, because the controller hangs probing at
  100 kHz when the core clock is 500 MHz or more.
- **The firmware's variable store is a file on this card**, `RPI_EFI.fd` in
  partition 1. The firmware writes it only while boot services run (the
  boot counter's variable is written before the exit), so after the exit
  the kernel is the only writer, and it must never write that file's
  sectors.

And from this tree: **the Pi publishes no devicetree.** Its console is
found through ACPI's SPCR, and the devicetree search ends `NoDtb`
(`screenlog.0`, 2026-11-05: `devicetree console discovery failed (NoDtb)`,
then `console @ 0xfe201000 (via ACPI SPCR)`). The plan's first draft said
the devicetree found the console there; it does not, and D1 changed with
it.

**QEMU can stand in for most of it.** QEMU 11.1's `virt` machine takes
`-device sdhci-pci -device sd-card,drive=...`: a standard SDHCI controller
on PCI, which `pci.rs`'s enumeration can find by class (0x08, 0x05). The
command set, the card's state machine and PIO are the same, so the driver
is written and its rig built on QEMU, and the Pi adds only discovery,
the clock and the takeover. QEMU's `raspi4b` machine has EMMC2 but no UEFI
path, so it does not help (`testing-pi4.md`, "Develop on QEMU first").

## The design

1. **One SDHCI driver, `sdhci.rs`, a third `block.rs` arm** beside
   `virtio_blk` and `usb_msd`. It resets the controller and brings the card
   up from scratch by the SD spec (CMD0, CMD8, ACMD41 to ready, CMD2, CMD3
   for the address, CMD9 for the size, CMD7 to select, ACMD6 for 4-bit),
   rather than trusting the state the firmware left, so QEMU and the Pi run
   the same code from the same starting point. 3.3 V, default speed
   (25 MHz) first, identification at 400 kHz and never under 200 kHz.
   Sectors move by PIO through the buffer data port: CMD17 and CMD24 for
   one block, CMD18 and CMD25 with an auto-CMD12 for several, which is the
   case Linux's one quirk covers.
2. **Found, never assumed.** On QEMU by PCI class. On the Pi by the
   devicetree node `brcm,bcm2711-emmc2` through `dtranges.rs`, or the ACPI
   device `BRCME88C` (D1). No fallback address, as for the UART.
3. **Taken over at the exit,** as the xHCI is: the firmware's driver stops
   with boot services, and the first thing the kernel does is a full
   controller reset.
4. **The block device is chosen by the rule that exists.** `bootdisk.rs`
   names the partition the firmware booted from, and `is_boot_disk` makes
   that disk the block device; the card becomes one more candidate. With a
   card and a stick both present, the disk booted from wins, by the rule
   one-disk step 3 built.
5. **The firmware's file is out of reach.** The kernel does not write the
   ESP on the card (D3), so `RPI_EFI.fd` and the FAT that describes it are
   never written after the exit.
6. **The card is staged as the stick is now.** `make bootstick PI=1`
   already writes the ESP-plus-ext2 layout with the Pi's firmware to a whole
   disk; a card is a whole disk. Whether `make sdcard` stays, for a card
   with only the boot partition, is D5.

## Decisions (settled by Hans, 2026-11-05, each as recommended)

- **D1. How the Pi's controller is found.** The devicetree node, or ACPI's
  `BRCME88C`, whose base may sit in a `_CRS` method. Recommended and
  settled: the devicetree first, with step 0 reading both so the choice is
  made on what the board shows. **The board had already shown it:** the Pi
  in its ACPI mode publishes no devicetree (above), so the source is ACPI,
  and `amlscan` learns to read the `_CRS` step 0 dumps. The devicetree stays
  a second source for a firmware set to publish one.
- **D2. PIO or DMA first.** PIO is what the firmware does, needs no DMA
  window and has no cache-coherence question (the Pi's PCIe DMA is
  not coherent, risk 8 in `testing-pi4.md`; EMMC2's is not yet read).
  DMA (SDMA or ADMA2) is faster.
  **Recommended and settled: PIO**, DMA as a follow-up once a measurement says the card
  is slow enough to matter.
- **D3. Writing the card's ESP.** Never (the driver refuses a write inside
  the boot partition, so it cannot be spelled), or allowed except for
  `RPI_EFI.fd`. Never costs installing a new kernel from the Pi itself,
  which nothing does today. **Recommended and settled: never, enforced in the driver
  by the partition's range,** a rule that can be relaxed later by a
  deliberate change, not by a slip in `fsd`.
- **D4. The firmware's `SdIsArasan` setting.** If a card comes up with
  Arasan selected, EMMC2 does not reach the card and `SDC3` is not
  declared. Either the plan requires EMMC2 (staging writes the setting, or
  the guide says to set it once in the firmware's menu), or the driver
  also handles Arasan, which needs Arasan's quirks (its capability register
  reads 0, per `Sdhc.asl`). **Recommended and settled: require EMMC2, the
  default in `RPi4.dsc`,** and have the kernel say plainly on serial when
  the card is routed elsewhere. Step 0 shows which a card has.
- **D5. `make sdcard`'s future.** Keep it for a card that boots and leaves
  the root on a stick, or retire it once a card carries the whole system.
  **Recommended and settled: keep it until step 4 passes on the board,** then decide.

## Steps, each with its check

0. **Read the board.** One boot that logs, before the exit, what the
   devicetree says of `emmc2`, which ACPI tables declare `BRCME88C` or
   `BCM2847`, and the `SdIsArasan` variable; with `\SDDUMP` at the card's
   root, those tables in hex. No controller register is touched. Check:
   the lines appear on serial, and a card with `SdIsArasan` set (if one can
   be made) shows no EMMC2 table, so the absence is told apart from a
   search that cannot find. **Built 2026-11-05** (`kernel/src/sdscan.rs`,
   the search `amlscan::hid_string_at` with host tests that two mutations
   fail; QEMU, which declares neither, logs all three lines and boots on):
   the Pi boot is owed. The register dump this step first named (version,
   capabilities, present state) waits for the base: it is read in step 0b,
   once the dumped `_CRS` is parsed by `amlscan`, host-tested on those
   bytes, since reading an address the parser has not been shown to find
   is a guess.
1. **The driver on QEMU, reading.** `sdhci.rs` on `sdhci-pci`, a
   `make run-sd` that boots the ESP as today and puts `build/disk.img` on
   the SD card, and a `make test-sd` that mounts the root from it and reads
   files. Check: the rig passes with the card as the only root, and fails
   under three mutations: the card's address (CMD3's answer) ignored, the
   bus left 1-bit while the controller is set 4-bit, and a multi-block read
   without its CMD12.
2. **Writing, on QEMU.** CMD24 and CMD25, and the ESP refusal (D3). Check:
   a file written, the VM restarted, the file read back; a write into the
   ESP's range refused with a line; mutations: the refusal removed (the
   rig's ESP check fails), a write's completion not waited for.
3. **The Pi, reading.** Discovery (D1), the clock from the capability
   register's base clock and the firmware's rate, the takeover. A card
   staged by `make bootstick PI=1`, a stick still in a port as the root.
   Check: the kernel names the card, lists its partitions, and reads its
   ext2 root's superblock; the stick stays the root this round, so a fault
   leaves a working system.
4. **The Pi, the card alone.** The card as the block device by the boot
   rule, no stick. Check: this plan's Done when, both halves.
5. **The records.** `testing-pi4.md`'s storage surprise rewritten,
   `gap-analysis.md`'s block-storage row, `make-targets.md` for the new
   rigs, and Pi item 2 on `ROADMAP.md` closed with a pointer here.

## Out of scope

- DMA (D2), high speed and UHS modes, and SDXC's faster bus modes.
- The Arasan controller (D4), and eMMC parts on the Compute Module.
- Card hot-plug: the card is present at boot or not at all.
- Parallels and the Pi 400's own needs: Parallels has no SD controller, and
  the Pi 400 is a Pi 4 here, predicted to need nothing more.

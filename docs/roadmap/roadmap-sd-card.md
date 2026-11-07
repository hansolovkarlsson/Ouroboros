# The SD card as the only disk: the plan

*Asked by Hans on 2026-11-05: "put all that's needed to run on the SD card
only". This is `ROADMAP.md`'s Raspberry Pi item 2, made an arc.*

## Why

Today a Pi 4 needs two pieces of media: the card, which the firmware boots
the kernel and the boot programs from, and a USB stick, the only disk the
kernel can read once boot services end (`testing-pi4.md` §6, "The storage
surprise", since retitled "The storage surprise, and its end"). That costs a USB port that a keyboard, a mouse or other
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
   Sectors move by PIO through the buffer data port, one block at a time:
   CMD17 to read, CMD24 to write. *(Built 2026-11-06 without multi-block
   commands: every `BLOCK_READ` and `BLOCK_WRITE` is one sector, so CMD18
   and CMD25, and the auto-CMD12 Linux's one quirk is about, would be code
   no caller reaches.)*
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
   **Read on the Pi 2026-11-05** (build `a8b92f41`, pftf v1.53, a card and
   the stick; the first try, `bb7141c3`, halted in `sdscan.rs` itself: the
   DSDT's OEM table ID holds bytes the UEFI console refuses, and
   `uefi::println!` panics on the refusal, fixed in `a8b92f41` and the Pi's
   output reproduced on QEMU first). No devicetree. The DSDT (`RPI`, 8,921
   bytes) declares Arasan, `_HID "BCM2847"`. The SSDT `RPI4EMMC` (631
   bytes, kept as `amlscan/testdata/pi4-ssdt-rpi4emmc.aml`, its checksum
   whole) declares EMMC2 as `\_SB.GDV1.SDC3`: `_HID "BRCME88C"`, `_UID 1`,
   `_CCA 0` (DMA not coherent), `Method (_STA) { Return (0x0F) }`, and
   `Name (RBUF, ResourceTemplate () { Memory32Fixed (ReadWrite, 0, 0x100);
   Interrupt (ResourceConsumer, Level, ActiveHigh, Shared) { 158 } })` with
   `Method (_CRS, 0, Serialized) { CreateDWordField (^RBUF, 4, RBAS);
   Store (0xFE340000, RBAS); Return (^RBUF) }`. So the registers are 256
   bytes at `0xFE340000` and the interrupt is INTID 158, level, which
   agrees with Linux's devicetree. `SdIsArasan` is 0, 4 bytes, vendor
   `cd7cc258-31db-22e6-9f22-63b0b8eed6b5`: the card is routed to EMMC2, as
   D4 needs. Its `_DSD`, with `SDMA` 1, gives `sdhci-caps-mask`
   `0x0000000500080000`.

0b. **Read the controller's address, then its registers.** `amlscan` takes
   a device by its `_HID` string and reads two method shapes exactly, no
   more general: a `_STA` that is only `Return (<constant>)`, and a `_CRS`
   that is only `CreateDWordField (^<buffer>, <offset>, <field>);
   Store (<constant>, <field>); Return (^<buffer>)` over a `Name`d
   resource buffer, the base patched in at that offset. Anything else in
   either method leaves the device not taken, as today. Check: host tests on
   the kept SSDT give `0xFE340000`, 0x100 and INTID 158, and mutations fail
   them (the store's offset ignored, a second store in the method
   accepted, a `_STA` returning 0 taken). Then a Pi boot that, before the
   exit and read only, logs the controller's host version, capabilities and
   present state at that address.
   **Built 2026-11-05:** `amlscan::declared` (virtio-mmio now one caller of
   it) and `MethodShape`; the buffer a patching `_CRS` names is found by a
   third pass, defined exactly once in its device. On the kept SSDT, and
   on it beside the kept DSDT as the kernel scans them, EMMC2 comes out
   `0xFE340000`, 0x100, INTID 158, and the Pi declares no virtio-mmio.
   Seven mutations each fail a test: the offset ignored, a statement after
   the Return accepted, the `_STA` constant ignored, a second definition of
   the buffer ignored, the stored-to field or the returned buffer
   unchecked, a method with arguments taken. `sdscan.rs` logs the declared
   count and, for exactly one, six registers (present state, host control,
   clock control, both capability words, host version), none whose read
   changes anything. QEMU declares none and boots as before.
   **Read on the Pi 2026-11-05** (build `c8bf046a`, to the login, no
   fault): `EMMC2 declared present: 1`, `0xFE340000`, 0x100, INTID 158, as
   the host tests had it. Present state `0x1fff0000` (card inserted and
   stable, write protect off, DAT and CMD lines high, no command in
   flight); host control `0x00800fc0` (bus power on at 3.3 V, the data
   width bit clear, so 1-bit as the firmware left it); clock control
   `0x000e0207` (internal clock stable, SD clock on, divider 2: 25 MHz from
   the base); capabilities `0x45ee6432` (base clock 100 MHz, timeout clock
   50 kHz, 2048-byte blocks, 8-bit, ADMA2, SDMA, high speed, 3.3 V and
   1.8 V, slot type embedded) and `0x0000a525` (SDR50, DDR50); host version
   `0x10020000`, SDHCI 3.00, vendor 0x10. So, unlike Arasan, whose
   capability registers read 0 (`Sdhc.asl`), EMMC2's are real: the driver
   takes its base clock from them. Step 0 is done; step 1 is next.
1. **The driver on QEMU, reading.** `sdhci.rs` on `sdhci-pci`, a
   `make run-sd` that boots the ESP as today and puts `build/disk.img` on
   the SD card, and a `make test-sd` that mounts the root from it and reads
   files. Check: the rig passes with the card as the only root, and fails
   under three mutations: the card's address (CMD3's answer) ignored, the
   bus left 1-bit while the controller is set 4-bit, and a multi-block read
   without its CMD12.
   **Built 2026-11-06** (`kernel/src/sdhci.rs`, `pci::discover_sdhci`,
   `main.rs`'s `init_sd`, `make test-sd`, `make run-sd`). QEMU's firmware
   does not boot from an SD card (it falls to its shell), so the rig boots
   the ESP from `virtio-blk-pci`, which the firmware reads and the kernel
   cannot, and puts `disk.img` on the card, as the Pi's card and stick are
   today; the card is installed only when no other disk went in, so no
   configuration that boots now changes. Two boots: a 128 MiB card, which
   QEMU makes standard-capacity and byte-addressed, and a sparse 4 GiB one,
   high-capacity and block-addressed; each mounts the ext2 root from the
   card and reads `/etc/srcid` back as staged. QEMU's controller is SDHCI
   2.00 with a 52 MHz base, so the card was identified at 203 kHz and read
   at 13 MHz, the most a power-of-two divisor gives under 25. Two of the
   three planned mutations could not fail and were replaced: QEMU models no
   data lines, so dropping ACMD6 passes (run, and it does: the bus width is
   the Pi's to show, in step 3), and there is no multi-block read to cut
   the CMD12 from. The mutations that fail: CMD7 with no RCA (both boots,
   `CMD7 failed, error status 0x0001`), byte addressing always (the SDHC
   boot, `disk I/O error`), block addressing always (the SDSC boot, no
   mountable partition). `make test`, `test-early-fault`, `test-el1-drop`,
   `test-usb-hub`, `test-boot-partition` and `test-blank-disk` pass.
   Its high review (2026-11-06, `d384a536`): a timed-out read now resets the
   command and data lines; voltage before bus power, 3.3 V checked, 10 ms
   to settle; the card stays out when the USB stick's init failed, the cell
   `install_boot_disk` leaves empty on purpose; prog-if 0x00 taken too; one
   `take_from_firmware` for the xHCI and SD paths. The Pi booted that
   build with its card and stick: the xHCI taken through the shared path
   with the same command register as before (`0x0140` to `0x0146`), the
   stick's ext2 root mounted, a login, no SD line (the Pi has no PCI SD
   controller), EMMC2 still declared at `0xFE340000`.
2. **Writing, on QEMU.** CMD24, one sector a write, and the ESP refusal (D3). Check:
   a file written, the VM restarted, the file read back; a write into the
   ESP's range refused with a line; mutations: the refusal removed (the
   rig's ESP check fails), a write's completion not waited for.
   **Built 2026-11-06** (`b470a84d`). CMD24 shares the read's address rule.
   The refused range is fixed at bring-up by `parttable::firmware_partition`
   (host-tested): the boot partition when the card is the boot disk, which
   is the Pi's case, else the card's first partition unless it is a root,
   the whole card when its table cannot be read; a write inside it is
   refused before the controller is touched, the first one said. Fixed at
   bring-up so that `partition` rewriting the table cannot move it off
   `RPI_EFI.fd`'s sectors. `make test-sd` now boots each card twice: the
   first writes `/sdnote` and powers off, the second reads it back and then
   runs `unmount`, `partition fat32` (LBA 0, allowed) and `format fat32`,
   whose first write, at sector 2048, the kernel refuses; the host then
   finds the 131,071 sectors of partition 1 in the card image byte for byte
   as in `disk.img`. Twenty checks across the two cards. The refusal
   removed fails the format and byte checks on both; the data written
   big-endian fails the read-back on both. A write's end not waited for
   passes: QEMU completes a write at once, so like ACMD6 it is the Pi's to
   show.
   Its high review (`125647ec`): the table that finds the firmware
   partition is kept with it, since `partition fat32` on a Pi card would
   have left `RPI_EFI.fd` whole and the card unbootable. So
   `parttable::firmware_sectors` protects sector 0 to the partition's end
   and a GPT's backup copy, and a card that is not blank but whose table
   lists nothing (a GPT whose two copies fail) is kept whole. The driver
   checks the card's R1 status from CMD7 on (ACMD6 before the host goes
   4-bit; not during identification, where an illegal-command bit can be
   the CMD8 an older card does not know), honours the write-protect switch,
   waits for both lines before writing the block registers, waits 20 us
   after each register write at the identification clock as Linux's
   `sdhci-iproc` does on the BCM2711, and clamps a byte-addressed card to
   4 GiB. `timer::deadline_us` and `delay_us` replace the driver's own
   tick arithmetic; `xhci.rs`, `smp.rs` and `syscall.rs` keep theirs for
   now. `sdscan` runs only under `\SDDUMP`. `make test-sd` now refuses
   `partition` at sector 0 as well as `format`, checks sectors 0 to the
   partition's end, and adds a fifth boot, the no-table card; 27 checks.
   The refusal removed, the table left out of the range, the no-table card
   left writable and big-endian data each fail their checks. (The
   no-table control's SDSC boot also hit a firmware stall, before the
   kernel ran, a data point for the stall study and not a result.)
   `make test`, `test-usb-hub`, `test-early-fault`, `test-el1-drop` and
   `test-boot-partition` pass. QEMU cannot show the write delay, the R1
   checks on a real error, or the power sequence; step 3 is the board.
3. **The Pi, reading.** Discovery (D1), the clock from the capability
   register's base clock and the firmware's rate, the takeover. A card
   staged by `make bootstick PI=1`, a stick still in a port as the root.
   Check: the kernel names the card, lists its partitions, and reads its
   ext2 root's superblock; the stick stays the root this round, so a fault
   leaves a working system.
   **Built 2026-11-06** (`e86fa5a6` and the commit after it): with no SD
   controller on PCI the kernel takes the one EMMC2 its ACPI tables declare
   present (`amlscan::declared`, at least 0x100 bytes; two would be a
   guess, so neither), maps it when a slot is left, and after the exit
   brings the card up and reads it whatever else is installed: sector 0,
   whether it is the boot disk, each partition with a FAT32 boot sector or
   an ext2 root's superblock read, its protected sectors. It is installed
   only when no other disk was, so the stick stays the root. The first Pi
   boot uses the card as `make sdcard` stages it, one FAT32 partition, so
   its boot sector is the data read past the table; the ext2 superblock
   waits for a card staged by `make bootstick PI=1` (step 4). On QEMU,
   `make test-sd`'s sixth boot puts `esp.img` on virtio-mmio beside the
   card: 34 checks, and the superblock read a sector off and the card
   installed over the other disk each fail. The ACPI path itself only the
   board runs.
   **Read on the Pi 2026-11-06** (build `385f6d9d`, the card as `make
   sdcard` stages it and the stick, first try): `sd: ACPI declares EMMC2 @
   0xfe340000` before the exit; after it, SDHCI 3.00 with a 100 MHz base, a
   high-capacity card, RCA `0x0001`, 244,277,248 sectors (119 GiB),
   identified at 400 kHz (the 10-bit divider at 125), then 4-bit at 25 MHz,
   so the power sequence, the waits between register writes, ACMD6 and its
   card status held on the board; sector 0 read (`0x55aa`), the card named
   the boot disk with its boot partition at LBA 32768 as the firmware's
   device path says, partition 1's FAT32 boot sector read at 4-bit, the
   whole card protected (its one partition runs to its last sector), not
   installed; the stick's ext2 root mounted, four cores, a login, no
   fault. Step 3 is done but for the ext2 superblock, which a card staged
   by `make bootstick PI=1` gives in step 4.
4. **The Pi, the card alone.** The card as the block device by the boot
   rule, no stick. Check: this plan's Done when, both halves.
   **Begun 2026-11-06.** `make bootstick PI=1` refused a Pi card, because
   the kernel could not read one; since step 3 it does, so `bootstick.py`
   takes a Pi card as ours (its `/etc` and boot counter carried) and as a
   Pi disk, its firmware files, `RPI_EFI.fd` with the settings that route
   the card to EMMC2 (`SdIsArasan` 0), carried whole rather than the
   pinned release's, which could route it to Arasan (D4). `make
   test-bootstick` grades it on a card-shaped image: 52 checks, and the
   firmware not carried or the card refused each fail. The card is then
   staged by `sudo`'s raw writes, Hans's step on the Mac. Two boots: the
   card alone (the existing rule installs it, no other disk going in),
   then the card with the stick, which needs a ranking rule this plan has
   not built: an SD card that is the boot disk ahead of a stick that is
   not.
   **The card alone, read on the Pi 2026-11-06** (build `0f6f471a`, the
   card restaged by `make bootstick PI=1`, no stick): the card brought up
   as in step 3, the boot disk with its boot partition at LBA 2048,
   partition 1's FAT32 boot sector and partition 2's ext2 superblock read
   (LBA 133120), sectors 0 to 133118 protected, and the card installed as
   the block device, no other disk being in. fsd mounted the ext2 root off
   it (`mount` says 244,277,248 sectors, the card's count), named the boot
   partition on this disk, and the login, `bootid` (boot 2, the counter
   carried from the old card) and `/proc` worked.
   **Written on the Pi 2026-11-06**, the same build, the next round trip:
   `echo hello-from-the-card > /card` (the name Hans typed), listed and
   read back, `shutdown` (PSCI off), the board power-cycled with the
   card alone, and boot 3 read `/card` back as written. The driver's
   first writes on hardware went through ext2 and the PSCI power-off and
   stood; the protected range refused nothing it should have taken. The
   first half of step 4 is done.
   **The ranking rule, built 2026-11-06** (`cf3d18a6`): `init_sd` brings
   the card up and reads it before the USB step and hands it back, and a
   card that is the boot disk is installed there, ahead of any stick and
   of a virtio disk held back as not the boot disk; any other card goes in
   after the USB step only when no disk did (`install_sd_fallback`), as in
   step 3. A virtio disk that is the boot disk still goes in first.
   `make test-sd`'s seventh boot boots disk.img itself from
   virtio-blk-pci, so the card's copy matches the firmware's device path,
   with esp.img on a USB stick: 42 checks, and with the early install
   switched off the card ahead of the stick, the stick left out, the root
   on the card's disk and fsd's boot partition each fail.
   **The card and the stick, read on the Pi 2026-11-06** (build
   `b6b78bee`, its `BOOTAA64.EFI` and stamp copied onto the card's boot
   partition, nothing else on the card changed, the old stick in a
   port): the xHCI scan activated the stick on port 3, the card came up,
   was named the boot disk (LBA 2048) and installed ahead of it, and no
   `usb-msd block device installed` line followed; fsd mounted the ext2
   root from the card (`mount`: 244,277,248 sectors) and found the boot
   partition on its disk, `cat /card` read back the file written two boots
   before, boot 4, no fault. **Step 4 is done:** the Pi boots and runs
   from its card alone, and a stick in a port no longer takes the root.
   **The high review of steps 3 and 4** (`62304e02`): `bootstick` had
   taken any card holding `RPI_EFI.fd`, another OS's too, and now wants
   `EFI/ORBS/INIT.CFG` beside it; the card's boot-disk question is asked
   again on a failed read; a GPT card's backup is kept where its headers
   put it (`parttable::gpt_backup_extent`), and a failed read there keeps
   the whole card; a card installed first says what it went in ahead of.
   Declined: one function ranking every disk, a restructure of every disk
   path, for when a fourth arrives.
   **Then a gap the review did not name** (`0aa8c177`): a card as `make
   sdcard` stages it is the boot disk too, so the rule put its firmware
   partition, every sector refused, ahead of the stick staged as its root.
   A boot card now goes in ahead only with a root partition past its
   protected sectors, and says so when it has none; `make test-sd`'s
   eighth boot is that card beside a stick holding a root, 48 checks, and
   every boot card ranked ahead fails it. Not booted on the board, whose
   card has its own root; the card-and-stick boot above holds for it.
   **Its high review** (`acf3bc63`): the kernel's root test was a second
   copy of fsd's rule and already disagreed with it (typed roots only,
   where fsd takes an untyped partition when none is typed), and took a
   typed root with no filesystem as a root. `parttable::root_candidates`
   is now the one rule, `vfs::root_order` built on it and the card ranked
   by it, a candidate counting only with an ext2, FAT32 or exFAT mark;
   `make test-sd`'s ninth boot zeroes the card root's superblock, 55
   checks, and with the filesystem mark always true its ranking checks
   fail; `make test-boot-partition` unchanged. Declined: a stick that
   attaches after the boot scan, beside a rootless boot card installed as
   the fallback, which is step 3's rule and `mount -a`'s for every disk.
   **Read on the Pi 2026-11-07** (build `e89c972e`, `main` after #412 and
   #413, the card re-staged by `make bootstick PI=1`, no stick): `acf3bc63`'s
   one root rule on the board, first try. Partition 2 at LBA 133120 was named
   a root with its ext2 superblock read, the card was installed as the boot
   disk ahead of any stick, fsd mounted the root (244,277,248 sectors) and
   found the boot partition on the card, and the login, `bootid` (boot 5)
   and `/proc/cpus` (4) worked, no fault. Neither a write's end waited for
   nor an R1 error on a real card has been shown: a healthy card shows
   neither.
5. **The records.** `testing-pi4.md`'s storage surprise rewritten,
   `gap-analysis.md`'s block-storage row, `make-targets.md` for the new
   rigs, and Pi item 2 on `ROADMAP.md` closed with a pointer here.
   **Done 2026-11-06:** `testing-pi4.md` §6's "The storage surprise, and
   its end", §4's new "One disk: the card alone" (staging, which disk is
   the root, what was observed, a kernel alone copied on), §5's routing
   setting and the USB stick's section; `gap-analysis.md`'s block-storage
   row; `ROADMAP.md`'s Pi item 2 closed and item 1's dependency on it;
   the stick's lines in the Makefile, `sdcard.sh`, `make-targets.md` and
   `CLAUDE.md`. **The arc is done.** D5, whether `make sdcard` is
   retired now that a card can carry the whole system, is Hans's to take:
   it still stages a card that boots beside a stick, and the eighth boot
   keeps that layout working.

## Out of scope

- DMA (D2), high speed and UHS modes, and SDXC's faster bus modes.
- The Arasan controller (D4), and eMMC parts on the Compute Module.
- Card hot-plug: the card is present at boot or not at all.
- Parallels and the Pi 400's own needs: Parallels has no SD controller, and
  the Pi 400 is a Pi 4 here, predicted to need nothing more.

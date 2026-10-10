# Running and testing Ouroboros on real Raspberry Pi 4 hardware

> **Status: test plan, not a test log (2026-08-28).** The boards were ordered
> 2026-08-26 ([`ROADMAP.md`](../ROADMAP.md)) and nothing here has been booted yet.
> Claims are marked **(confirmed)** when they come from this repository's own
> source or from a vendor document, and **(predicted)** when they are reasoning
> from those two. The point of writing it before the boards arrive is that the
> predictions are falsifiable: each one below names the log line that settles
> it, so the first bench session turns this document into a log instead of
> starting from a blank page. When a prediction is wrong in an interesting way,
> it graduates to a postmortem — that is how every other hardware surprise in
> this project got written up.

> **Update 2026-09-27: the Pi 400, two wrong predictions and a live
> regression.** A desk session checking this plan against a Raspberry Pi 400
> found three things without a board. Two are predictions that were wrong on
> paper. **The keyboard cannot work yet**: every USB 2.0 device on a BCM2711
> board, the Pi 400's built-in keyboard included, sits behind an on-board hub,
> and `xhci.rs` has no hub support (§1b). **The xHCI BAR is a bus address, not
> a CPU address**, on this board (§6 checkpoint 4). The third is observed, not
> predicted: **the xHCI discovery's exclusive root-bridge open was refused by
> firmware**, and `make run-usb-kbd` had found no controller on QEMU since
> 2026-08-29 (Risk 6). The second and third are fixed on branch
> `pi400/bar-translation`; the table in §2, §3's shopping list, checkpoint 4
> and §6's storage note are corrected in place.
>
> **Later the same day, the largest finding: the Pi's PCIe DMA is not
> cache-coherent** (Risk 8, confirmed from the firmware's own ACPI tables and
> from Linux's and FreeBSD's drivers), and `xhci.rs` assumes it is. Until
> that is fixed, expect no USB at all on the board, so no keyboard and no
> disk: plan the first boot around the serial console.
>
> **Update 2026-10-01: the two notes above are superseded, and the boards
> have booted.** Everything they say is missing is now on `main`: the BAR
> translation and the exclusive open (#176), hub support (#177) and the
> non-cacheable DMA pool under ACPI `_CCA 0` (#179). So the keyboard and USB
> storage are expected to work, and are not yet seen to. On 2026-09-28 a Pi 400
> and a Pi 4 booted over HDMI alone, with no serial cable, and neither reached
> the shell: the Pi 400 faults in firmware once the xHCI is taken, and the
> Pi 4 stops after the early console's clear (§6, "Bisecting a hang with boot
> flag files", and the roadmap's "First boots on the boards"). The next session
> is serial, through a Raspberry Pi Debug Probe (§3), and it is the one that
> starts turning this plan into a log.

The practical guide to booting Ouroboros on a **real Raspberry Pi 4** under UEFI
firmware. Companion to [`testing-qemu.md`](testing-qemu.md) (the fast dev loop,
single machine and two-node cluster) and [`testing-parallels.md`](testing-parallels.md)
(the parked VM target, whose single-machine matrix and `netd` boot-race analysis
both transfer here). [`manual.md`](../manual.md) covers *using* the OS once booted;
[`research-redox-and-pi.md`](../research/research-redox-and-pi.md) Part 2 is the BCM2711
register-level fallback reference.

---

## 1. Why a Pi 4 — the choice is really about UEFI

The usual advice for "cheap board to test my own ARM OS on" points at a Pi and
then at the bare-metal tutorials: build a `kernel8.img`, let the VideoCore
bootloader drop it at `0x80000`, talk to the BCM2711 registers directly. **That
advice does not apply to this kernel**, and knowing why is what narrows the
hardware list to one board.

Ouroboros boots as a **UEFI application** — `aarch64-unknown-uefi`,
`EFI/BOOT/BOOTAA64.EFI` (confirmed: `Makefile`'s `esp` target, `README.md`'s
boot strategy). Everything the kernel knows about the machine it is on, it
learns through firmware: the devicetree or ACPI tables for the console
(`devicetree.rs`, `acpi.rs`), `PciRootBridgeIo` for the xHCI controller
(`pci.rs`), GOP for the framebuffer (`framebuffer.rs`), MADT for the interrupt
controller (`madt.rs`), and the UEFI filesystem for loading every userland
binary (`loader.rs`). Take UEFI away and there is no boot path at all — not a
degraded one.

So the requirement is not "an ARM board." It is **an ARM board with usable UEFI
firmware**, which is a far shorter list.

| Candidate | ~Cost | UEFI story | Verdict |
| --- | --- | --- | --- |
| **Raspberry Pi 4 (4 GB)** | **$55** | [pftf/RPi4](https://github.com/pftf/RPi4) EDK2 — mature, ships real ACPI (FADT, SPCR, DBG2, XHCI, dummy MCFG) *and* devicetree | **chosen** |
| Raspberry Pi 5 | $60–80 | [worproject/rpi5-uefi](https://github.com/worproject/rpi5-uefi) **archived Feb 2025**; active forks exist but report trouble on D0 boards and newer EEPROMs | avoid for now |
| Rockchip RK3588 (Orange Pi 5, Rock 5B) | $60–120 | [edk2-rk3588](https://github.com/edk2-porting/edk2-rk3588) — genuine UEFI+ACPI, but a much less trodden path and a second unknown stacked on the first | later, if ever |
| Windows-on-ARM laptop (used ThinkPad X13s) | $250–400 | Genuine SystemReady firmware, the most "real" target of the four | too expensive, no serial header, slow recovery loop |
| Bare-metal SBCs / Cortex-M boards | $4–25 | None — would mean abandoning the UEFI boot path entirely (and Cortex-M has no MMU at all) | wrong architecture for this kernel |

Beyond the firmware, four things make the Pi 4 fit *this* kernel unusually well:

- **GIC-400 is a GICv2.** `gicv2.rs` already exists and is selected from a real
  MADT parse (`madt.rs`), which is exactly the mechanism that fixed the second
  Parallels External Abort. The Pi 4 exercises that machinery on a third
  platform rather than needing new code. (predicted)
- **The xHCI path is the one storage path already confirmed on real hardware.**
  The Pi 4's VL805 sits behind the Broadcom PCIe root complex, so
  `pci::discover_xhci` → `xhci.rs` → `usb_msd.rs` should carry over — the same
  chain the [xHCI keyboard postmortem](../postmortems/xhci-keyboard-postmortem.md) is about.
- **It boots from a removable SD card**, so a bad build is a card swap, not a
  recovery procedure. Keep a second card flashed and known-good.
- **It has a serial header**, which the laptop option does not.

---

## 1b. The Raspberry Pi 400

The Pi 400 is a Pi 4 built into a keyboard: the same BCM2711 SoC, the same
VL805 xHCI controller behind the same PCIe block, 4 GB of RAM. The pftf/EDK2
firmware lists it as supported next to the Pi 4 B (confirmed: edk2-platforms
`Platform/RaspberryPi/RPi4/Readme.md`), so everything in this document applies
to it. What differs:

- **The keyboard is a USB device behind a hub, and `xhci.rs` cannot reach
  it.** On a Pi 400 `lsusb` shows the built-in keyboard (Holtek, `04d9:0007`)
  behind the VIA Labs hub `2109:3431` (confirmed: user reports, see Sources).
  That hub is soldered to the VL805's USB 2.0 root port on every BCM2711
  board, so on a plain Pi 4 **every USB 2.0 device** sits behind it too:
  keyboards, and USB sticks that are not SuperSpeed. `xhci.rs` addresses only
  devices attached directly to a root port (its module doc: *"no hubs (route
  string is always 0)"*). Predicted result: the port scan finds the hub,
  reads interface class `0x09`, skips it, and the boot has **no keyboard at
  all**. On the Pi 400 there is no workaround short of an external keyboard,
  and an external USB 2.0 keyboard goes through the same hub. (predicted)

  **Update 2026-09-27: hub support is built** (branch `pi400/usb-hub`, see
  below), so the prediction now is that the keyboard works, with one open
  question: the transaction-translator fields, which only this board tests.
  If the keyboard fails at checkpoint 4 with the hub found and its ports
  brought up, those fields are the first suspect. (predicted)
- **What never needed hubs:** a SuperSpeed (USB 3) stick in one of the
  two blue ports is attached directly to a USB 3 root port, so `usb_msd.rs`
  should reach it. (predicted)
- **Serial:** the 40-pin GPIO header is on the back edge, same pinout, so §3's
  wiring applies unchanged.
- **RAM:** 4 GB, so the "Limit RAM to 3 GB" setting in §5 is the one that
  matters. Leave it on.

**Hub support is therefore the first piece of new driver work the Pi 400
needs**, ahead of any NIC. It can be built and tested on QEMU before any
board is on the bench: `-device usb-hub,bus=xhci0.0,port=1 -device
usb-kbd,bus=xhci0.0,port=1.1` puts a keyboard behind a hub on the existing
`qemu-xhci`. The work: recognise a hub (class `0x09`), read its hub
descriptor, power and reset its ports with hub class requests, and address
each downstream device with a nonzero route string. **One part QEMU cannot
exercise:** QEMU's `usb-hub` is full-speed, while the VIA hub is high-speed
with a full- or low-speed keyboard below it. That case needs the
transaction-translator fields in the slot contexts (the hub's own slot marked
as a hub with its port count and TT think time; the keyboard's slot naming its
parent hub slot and port). Those fields get their first test on the board.

**Built and checked on QEMU (2026-09-27, branch `pi400/usb-hub`).**
`configure_hub` in `xhci.rs` sets the hub's configuration, reads its
descriptor, marks its slot as a hub, and powers its ports; then each port is
reset and its device addressed with a Route String before the next port is
reset (a real hub with two devices on it would otherwise have both answering
at address 0, which QEMU cannot show - so a second device behind the Pi's hub
is worth plugging in on purpose), all Slot
Contexts coming from one builder (`slot_context`). `make test-usb-hub` runs
two boots and both pass: the keyboard and a stick behind the hub, with a line
typed through the USB keyboard; and `--usb-boot`, which boots `build/esp.img`
from a stick behind the hub with no other disk, the kernel then mounting that
stick through the hub. Both failed on the kernel before (the controls are in
the script's docstring), and forcing the route string to 0 makes them fail
again. What to read on the board, at checkpoint 4 and after:
`xhci: port N: hub with M ports (speed=3, TT think time T)`, then
`port N.P: device connected, reset, speed=1` (or `2`) for the keyboard, then
`keyboard ready`. The keyboard's line is the translator test: speed 1 or 2
below a speed-3 hub is exactly the case QEMU cannot model. Just before
`keyboard ready`, `interrupt endpoint Interval field N (every … us)` shows the
polling rate the controller was given; for a Full-speed keyboard N is 3 to 10.
If the keyboard fails there with the hub up, the endpoint's bandwidth fields
(Interval, and Max ESIT Payload, both set to the spec since 2026-09-27) are the
second suspect after the translator fields.

## 2. The one caveat that shapes everything: still no networking

**A Pi 4 has no virtio device of any kind.** Its NIC is Broadcom GENET
(`bcmgenet`, MMIO at `0xFD58_0000`) — not virtio-mmio, not virtio-PCI.
`init_net()` calls `virtio_net::Device::discover()` (confirmed:
`kernel/src/main.rs:757`), so the outcome is identical to Parallels for an
entirely different reason: `NET_MAC` returns `NET_ERROR`, `netd` reports *"no
NIC this boot,"* and **every network and cluster feature is unreachable** —
`ping`, `resolve`, `fetch`, `mount -r`, `cpu`, `dial`, the 9P export, cluster
auth, anything two-node.

This has a consequence worth stating plainly, because it contradicts the
optimistic reading of the roadmap entry:

> **Buying two Pi 4s does not, on its own, deliver the two-node cluster proof.**
> The boards are the right physical substrate for it, but a NIC driver has to
> exist first. Until then the second board is a spare, not a peer.

Three ways to close that gap, ranked by how much new ground each breaks:

1. **USB Ethernet over the existing xHCI stack** (CDC-ECM, or an ASIX
   AX88179 dongle). A bulk-endpoint driver alongside `usb_msd.rs`, on the one
   bus already confirmed working on real hardware. Cheapest real path, and it
   would work on Parallels too.
2. **A GENET driver.** Plain MMIO, discoverable from devicetree, and
   well-documented by Linux's `bcmgenet` and the U-Boot driver. Native and
   gigabit, but it is a genuinely new device family for this kernel.
3. **UEFI `SimpleNetworkProtocol` before `exit_boot_services`.** Tempting and
   fast, but boot-services-backed — it cannot survive the exit, so it proves
   packets move and nothing else. A demo, not a path. Noted here so it doesn't
   get rediscovered as a shortcut later.

### What each platform can actually validate

| Capability | Real Pi 4 | Real Parallels | Two QEMU VMs |
| --- | --- | --- | --- |
| Boot, console (serial + HDMI/GOP) | ✅ (predicted) | ✅ | ✅ |
| USB keyboard (xHCI HID) | ✅ (predicted) **behind a hub**, the translator fields untested, see §1b | ✅ | ✅ |
| USB storage + disk/FS commands | ✅ **USB stick only** (predicted: any port since hub support; SuperSpeed in a blue port is the safer bet), see §1b and §6 | ✅ (USB-MSD) | ✅ (virtio-blk) |
| Shell, `/bin`, pipelines, env | ✅ (predicted) | ✅ | ✅ |
| Networking (`ping`/`resolve`/`fetch`) | ❌ GENET is not virtio | ❌ no NIC transport | ✅ |
| Cluster (`mount -r`/`cpu`/`dial`/export/auth) | ❌ same | ❌ same | ✅ |
| Graceful no-NIC degradation | ✅ (worth testing) | ✅ | n/a |
| **Genuinely new coverage vs. Parallels** | **GICv2 via MADT, a third firmware's ACPI, real PCIe DMA** | — | — |

That last row is the honest answer to "what is this hardware *for*, given it
can't run the cluster." Three things that have never been exercised anywhere
else, each one in the exact area that has already produced two hardware crashes.

---

## 3. The bench rig

Per board:

- **Raspberry Pi 4, 4 GB.** 2 GB is enough; 8 GB is actively unhelpful (see the
  3 GB DMA limit in §5).
- **The official 15 W USB-C supply.** Undervoltage on a Pi 4 presents as
  intermittent, unreproducible weirdness — the worst possible failure mode when
  you are also debugging a kernel.
- **2× microSD cards** (A2, 32 GB) plus a reader. Two per board so there is
  always a known-good card to fall back to.
- **A 3.3 V USB serial adapter.** This is the single most important item in
  the list. The one on order is the **Raspberry Pi Debug Probe**; a CP2102 or
  FT232 USB-TTL adapter does the same job.
- **micro-HDMI cable** — the GOP framebuffer console (`fbconsole.rs`) is a
  separate output path from serial and needs its own verification.
- **A FAT32-formatted USB 3 (SuperSpeed) stick**, for a blue port: the
  `usb_msd` block path, and the only runtime filesystem the Pi has (§6). A USB
  2.0 stick, or any stick in the black port, goes through the on-board hub:
  reachable since hub support (§1b), but through the untested translator path
  if it is not High-speed, so the SuperSpeed stick is the first one to try.
- **The keyboard is the one to test, not to rely on.** Every USB keyboard
  goes through the on-board hub (§1b), the Pi 400's own included, and it is
  the translator fields' first test. Keep the serial cable as the fallback:
  the PL011 console reads input as well as writing it (`console.rs`'s
  `read_byte`), so the Mac's terminal is the keyboard if the USB one is not.

### Wiring the serial console

Pi 4 GPIO header, with the USB adapter **unplugged** while you wire it:

```
Pi pin  6  (GND)          ->  adapter GND
Pi pin  8  (GPIO14, TXD)  ->  adapter RX
Pi pin 10  (GPIO15, RXD)  ->  adapter TX
Pi pin  2/4 (5V)          ->  NOTHING
```

TX and RX cross. **Do not connect the adapter's VCC to the Pi** — the Pi is
powered by its own supply, and back-feeding it through the header while the
USB-C supply is also connected is how boards die.

With the Debug Probe, the wires are its UART cable on the port marked **U**:
orange is the probe's TX (to Pi pin 10), yellow its RX (to Pi pin 8), black
GND (to Pi pin 6). The cable carries no VCC, so there is nothing to leave
off. (confirmed: Raspberry Pi's Debug Probe documentation, see Sources)

The device name depends on the adapter. The Debug Probe is a USB CDC device
and appears as `/dev/tty.usbmodem*` (its documentation names the
`/dev/cu.usbmodem*` twin, and either works for `screen`); a CP2102 or FT232
appears as
`/dev/tty.usbserial-*`. `ls` both after plugging it in, then capture the whole
session to a file (`-L` writes `screenlog.0` in the current directory), since
the log is the evidence §6 and §8 work from:

```sh
screen -L /dev/tty.usbmodemXXXX 115200          # Debug Probe
screen -L /dev/tty.usbserial-XXXXXXXX 115200    # CP2102 / FT232
```

115200 8N1 is the pftf firmware default (confirmed: pftf/RPi4 readme). Exit
`screen` with `Ctrl-a k`.

---

## 4. Building the card

The Pi's boot medium holds two things that do not collide: the firmware at the
root of the FAT partition, and Ouroboros's ESP tree underneath `EFI/`. The
firmware boots `RPI_EFI.fd` via `armstub`, which then boots
`\EFI\BOOT\BOOTAA64.EFI` from that same partition, the well-known
removable-media path `make esp` already writes to (confirmed: `Makefile`'s `esp`
target).

**Do not `dd` `build/esp.img` onto the card.** It is a fixed 64 MB `hdiutil`
image (confirmed: `Makefile`'s `image` target) with no room for the firmware,
and writing it would leave the rest of the card unusable. `make sdcard` copies
the tree instead.

```sh
# 1. Once per card, BY HAND: format it as MS-DOS (FAT32) with an MBR map.
#    Disk Utility: "MS-DOS (FAT)" + "Master Boot Record". Or:
#    diskutil eraseDisk MS-DOS OUROBOROS MBRFormat /dev/diskN     # CHECK diskN FIRST

# 2. Every time after that:
make sdcard SDCARD=/Volumes/OUROBOROS            # add EJECT=1 to eject when done
```

**The target never formats.** Erasing takes a `/dev/diskN`, and a wrong N
erases some other disk, so step 1 stays manual. `scripts/sdcard.sh` refuses a
path that is not a volume's mount point under `/Volumes`, a volume that is not
FAT, one on fixed internal media, and one that is neither empty nor already a
Pi card (no `RPI_EFI.fd`). Each refusal was seen once: against a disk image
built to trip it, except fixed internal media, which an attached image never
reports, so that one was tripped through a `diskutil` shim that reports it.
`make sdcard` checks that `SDCARD` names a directory before it builds anything.

**The firmware is pinned, checksummed and installed once.** The release
(pftf v1.53) and its SHA-256 are constants in `scripts/sdcard.sh`; the zip is
cached in `build/cache/` and checked on every use. It is unzipped onto a card
only when the card has no `RPI_EFI.fd`, because this firmware has no NVRAM: the
§5 settings and every UEFI variable, the boot counter's included, are stored
inside `RPI_EFI.fd` itself (edk2-platforms' `Platform/RaspberryPi/RPi4/Readme.md`,
"NVRAM"; the readme in the pftf zip does not say). Rewriting it on each update
would silently reset them. `FIRMWARE=1` reinstalls it deliberately, and even
then keeps an existing `config.txt`, since Risk 3's UART overlay is set there
by hand. The
firmware's files keep the names they ship with; the readme is explicit that
renaming them breaks boot. To move to a newer release, bump the version and the
checksum together (`gh release view --repo pftf/RPi4` prints the asset's).

**Ouroboros's entries are replaced whole on every run.** Which entries those
are is computed from `build/esp`: each top-level entry and each entry under
`EFI/`. A program dropped from the tree therefore leaves the card too. Two of
them are state rather than build output:

- `etc` is re-staged by default (the dev passwd, shadow, group and cluster
  files, as every other image target stages them; the cluster keys are
  deterministic, so the identity does not change). That resets accounts and
  passwords made on the Pi, and the script says so. `KEEP_ETC=1` keeps the
  card's files and adds only the ones a newer tree introduced.
- `Users` (the home directories) only gains the directories it is missing.
  What was made on the Pi is never replaced.

Each is copied beside the old one and swapped in afterwards, so a copy that
fails (a full card, a pulled reader) leaves the old `EFI/BOOT` bootable.
Anything else on the card, the firmware's files or a file of your own at the
root, is left alone. Re-staging `EFI/ORBS/BOOTID.TXT` rolls the boot counter's
file store back, as every image does; the counter's preferred store is the
UEFI variable, which lives in `RPI_EFI.fd` and so survives.

### The USB stick

The card stops being readable once the kernel runs (see "The storage
surprise" in §6), so the stick is the only disk the system has: `mount -a`
mounts it, and `/bin`, `/etc` and `/man` come from it.

```sh
make stick STICK=/Volumes/<stick>          # KEEP_ETC=0 re-stages its /etc, EJECT=1 ejects
```

Format it once by hand: in Disk Utility, **MS-DOS (FAT) or ExFAT with the
Master Boot Record scheme**. MS-DOS (FAT) gives FAT32 on a stick over 2 GB.
The default GUID scheme puts a hidden EFI partition first, and `fsd` mounts
the first partition it can, so `make stick` refuses a volume that is not
partition 1 of an MBR disk. FAT16 is refused too: `fsd` mounts FAT32 and
exFAT.

`make stick` is `make sdcard` in stick mode (`scripts/sdcard.sh` with
`STICK=1`): the same guards (a mounted volume under `/Volumes` on removable
or external media, never formatted, empty or already ours) and the same
copy-and-swap. What differs:

- **No firmware and no `EFI` tree.** The kernel and the boot programs are
  the card's, and nothing at runtime reads `EFI` from the disk. A stick
  carrying its own `EFI/BOOT/BOOTAA64.EFI` could be booted by the firmware
  in the card's place, running whatever kernel was last staged on it.
- **"Ours" is a `.ouroboros-stick` file at the root**, written before
  anything is copied, so a run that stops partway leaves a stick the next
  run accepts. A volume with `RPI_EFI.fd` is a card and is refused.
- **`etc` is kept by default**, since the stick is where accounts and
  passwords made on the Pi live; files a newer tree adds are still copied.
  `KEEP_ETC=0` re-stages it and says so.

**Stage the card and the stick from the same build.** Nothing checks that
they match yet (roadmap: "A Pi boot from a card and a stick of different
builds"), and old programs on a stick under a new kernel look like a kernel
bug.

**Which kernel the card carries is in the capture** since 2026-10-03: the
first line, `UEFI stage alive, build <commit>[+dirty] <profile>, ...`, and
again in the line that announces the console after the exit (`boot services
exited, console live, build ...` on serial; the `\FBCON` and fallback lines
carry it too).
Compare it with `git rev-parse --short=12 HEAD` of the tree you staged from;
`+dirty` means the tree differed from that commit when it was built (a
tracked file changed, or an untracked one not ignored). It says nothing about the stick, which has no kernel.

*Built 2026-10-03, checked only on `hdiutil` images:* FAT32 and exFAT staged
(no `EFI`, the marker present); a restage keeping `etc`; `KEEP_ETC=0`
re-staging it; a stick with its marker but half its tree accepted; refused
were a GUID-scheme volume (partition 2), a FAT16 volume, a volume holding a
foreign file, `make stick` on a staged card and `make sdcard` on a staged
stick. `make sdcard` on a fresh image still installs the firmware once. A
real stick may differ in what macOS does to a mounted volume, as the first
real card did (`blind-instruments-postmortem.md`, "A disk image that was not
the card").

---

## 5. Firmware settings to check before the first boot

Press **Esc** during the firmware splash → `Device Manager` → `Raspberry Pi
Configuration` → `Advanced Configuration` (confirmed: pftf/RPi4 readme).

**Limit RAM to 3 GB — leave it ENABLED.** It is on by default, and the reason
is not conservatism: the Pi 4's PCIe block cannot address above 3 GB, and every
OS that lifts the limit does so by *patching its own DMA paths* to compensate.
Ouroboros does no such patching — `xhci.rs`'s rings and transfer buffers come
from UEFI page allocations with no address constraint. Disabling this setting is
the shortest route to silent DMA corruption on the one storage path that works,
and it would present as data errors rather than a clean fault. This is also why
the 4 GB board is the right buy and the 8 GB board is not.

**ACPI vs. devicetree.** `discover_console` tries devicetree, then ACPI/SPCR,
then PCI, in that order (confirmed: `main.rs:100`, `devicetree.rs`, `acpi.rs`,
`pci.rs`). The pftf firmware can present either or both. Set it to **both** for
the first boot — it is the most informative setting, because the
`console @ {base:#x} (via {source})` log line then tells you which mechanism
actually won on this platform. Once that is known, pin it deliberately.

---

## 6. The boot sequence — ordered checkpoints

These follow `main.rs`'s real order, so the first line that does *not* appear
localises the failure without any guessing.

1. **Firmware splash, then the Ouroboros banner** over the UEFI boot-services
   console — which the firmware mirrors to both serial and HDMI. Reaching here
   proves the card layout and the firmware handoff, nothing else. *(Since
   2026-10-02 the banner line says `on its own stack (sp ..., the kernel's
   stack ..., the firmware's sp was ...)`: the kernel left the firmware's
   16 KB stack, whose page tables lie beneath it, as its first act. A line
   saying `the FIRMWARE'S stack` means the switch did not happen.)*

2. **`console @ {base:#x} (via {source})`.** *(predicted: `via acpi`; observed
   2026-09-28 on a Pi 4 with the firmware set to ACPI + Devicetree:
   `console @ 0x7e201000 (via devicetree)`. The devicetree is tried first and
   its `reg` is a VideoCore bus address; the PL011 is at `0xfe201000` for the
   ARM. `dtranges.rs` now translates through the ancestors' `ranges`, so the
   line should read `0xfe201000`. Checked on the host against
   `bcm2711-rpi-4-b.dtb` and `bcm2711-rpi-400.dtb` from the pinned firmware,
   not yet on a board.)* The
   RPi4 EDK2 firmware ships a real SPCR table, so the ACPI branch should
   resolve a PL011 base. **If this line is missing**, `pci::log_all_devices`
   runs instead as a diagnostic, and after `exit_boot_services` there is no
   byte-stream console at all — you are down to whatever `fbconsole` gives you
   on HDMI, and the serial cable will be silent for the rest of the boot. That
   is a working state, not a dead one, but check HDMI before concluding the
   board is hung.

3. **`GOP framebuffer @ {base}, {w}x{h}, stride=…`.** *(predicted: found)* The
   HDMI console. Needs a display connected at power-on.

4. **`xHCI controller @ {base}, PCI command register 0x…→0x…`.** *(predicted:
   found)* The VL805 behind the Broadcom PCIe root complex. Watch the before →
   after command-register values: the Memory-Space-vs-I/O-Space bug documented
   in `pci.rs` made every prior platform read `0xffffffff` or take an External
   Abort, and the fix is only confirmed on Parallels so far.

   **Order changed 2026-09-27:** this line now prints later than its number
   says, after checkpoint 5's MADT line and the loader's program lines, as the
   last thing before boot services exit (Risk 6 has why). Its reprint after the
   exit is still the first `xhci:` line of the post-exit console.

   **Corrected 2026-09-27: `{base}` must be the CPU address, and on this board
   the BAR does not hold it.** The BCM2711's PCIe window is translated: under
   pftf the BAR holds a bus address near `0xF800_0000` while the CPU reaches
   the controller at `0x6_0000_0000` (confirmed: edk2-platforms `RPi4.dsc`,
   `PcdBcm27xxPciBusMmioAdr` and `PcdBcm27xxPciCpuMmioAdr`). Before the fix,
   `pci.rs` used the raw BAR, and `xhci.rs`'s first register read would have
   gone to an address with nothing behind it. Branch `pi400/bar-translation`
   takes the CPU address from firmware (`PciIo.GetBarAttributes`, which per
   UEFI 2.7 reports the host address and does the conversion itself) and
   refuses the controller unless that address plus firmware's translation
   gives back the BAR read from config space. The line to read, reprinted after
   `exit_boot_services` so it survives on an HDMI-only boot:
   `xhci: CPU {cpu} (BAR {bus}, translation {t})`. Expected here: CPU
   `0x6_0000_0000` or just above, BAR near `0xF800_0000`, translation
   `0xffff_fffa_f800_0000` (`0xF800_0000 - 0x6_0000_0000`, wrapped). On QEMU
   the translation is 0, so this checkpoint is the first test of a nonzero
   one. A refusal reads `xHCI discovery failed (xHCI controller: BAR0 …
   does not equal firmware's CPU … + translation …)` and the boot goes on
   without USB; that is the fail-closed path, checked on QEMU by corrupting
   the CPU address on purpose. (predicted)

5. **MADT parse → GICv2.** *(predicted)* The Pi 4's GIC-400 is a GICv2, with
   GICD at `0xFF84_1000` and GICC at `0xFF84_2000`. **This is the single most
   valuable checkpoint on the board**, because a hardcoded `GICD_BASE` is
   precisely what took the second External Abort on Parallels, and `madt.rs` +
   `gicv2.rs` were written to make that impossible. A third platform with a
   genuinely different GIC address is the first real test of that claim.

5b. **After the exit, three lines in this order** *(predicted 2026-10-02,
   from `pi4/el1-drop`; the first line's fact is confirmed, the other two
   are the drop's first run on a board)*:
   - `running at EL2 after the exit`. The 2026-10-01 `NOXHCI` dump showed
     SPSR in EL2h, so EL2 is what this board hands off at; QEMU says EL1,
     and EL2 only under `make run-el2`.
   - `dropped from EL2 to EL1, on our own tables and vectors (GIC system
     registers: not implemented)`: `el2.rs` set EL1's regime to the built
     identity map, wrote `HCR_EL2` to `el2.rs`'s `HCR_EL2_VALUE` (`RW`,
     `HCD`, no traps, interrupts to EL1), switched the firmware's EL2
     timer off, and `eret`ed. The GIC-400 is a GICv2, so
     `ID_AA64PFR0_EL1.GIC` is 0 and `ICC_SRE_EL2` is left alone; QEMU
     under `gic-version=3` prints `enabled for EL1` here instead.
   - `identity map installed, MMU running on our own tables`, which before
     the drop was true of the registers and false of the machine
     (`blind-instruments-postmortem.md`, 2026-10-01).

   **If the second line is missing** the `eret` did not land: the first
   suspect is the L0 start level (`mmu.rs`'s module doc; the EL1 regime
   starts from nothing here, with no firmware configuration to match), and
   the reporter's dump, still registered through the firmware's EL2
   vectors at that point, is what to read. After the second line a fault
   reports through the kernel's own `EXCEPTION vector=...` line, the first
   time that has been possible on this board; the virtio-mmio scan at
   `0xa000000` (Risk 1) is expected to be the first. Also read, before the
   exit: `PSCI conduit: smc` or `hvc` (the FADT's flag, logged since
   2026-10-02). TF-A answers `smc`. An `hvc` conduit would be unusable: the
   drop sets `HCR_EL2.HCD`, so an `hvc` at EL1 is an undefined instruction
   reported by the kernel's own vectors, and on a boot that drops
   `power.rs` treats an `hvc` conduit as none and `shutdown` halts, saying
   so in this line. Neither branch has run on a board; both were run on
   QEMU by forcing the conduit.

6. **Timer, preemption, task start** — `cond`, `fsd`, `netd`, the supervisor.

7. **`netd`: "no NIC this boot."** Expected (§2). It is a pass, not a failure —
   and confirming it *degrades cleanly here too*, on firmware that is neither
   QEMU's nor Parallels', is one of the things this board is for.

8. **Shell prompt**, on serial and on HDMI.

9. **The boot identity, over two boots (owed by step 3 of
   [`roadmap-session-auth.md`](../roadmap/roadmap-session-auth.md)).** Run
   `bootid`, reboot the same card, run it again. Record: the two counters
   (they must differ, the second one higher); which store held the counter
   from before the boot (the UEFI variable is the preferred store, and the Pi
   4's firmware keeps its variables in a file on the SD card, so it rolls back
   with the card); and the boot entropy's length, the kernel log's
   `boot identity:` line. **No entropy means no forward secrecy for any session
   this node takes part in**, so that number decides a property of the cluster.

### The storage surprise, stated plainly

**Ouroboros has no SD-card driver, and will not acquire one by booting from an
SD card.** `loader.rs` reads every userland binary through UEFI's filesystem
protocol during the boot-services window (confirmed: `loader.rs`'s module doc —
"there is no runtime disk driver yet"), and `block.rs` dispatches over exactly
two runtime drivers, `virtio_blk` and `usb_msd` (confirmed: `block.rs`). The Pi
has neither on its SD slot.

So: the card boots the kernel and the boot programs, and then becomes
invisible the moment `exit_boot_services` runs. The `/bin` and `/man` copies on
it are unreachable from then on: running a program goes through `fsd`
(`spawn_stage` in the shell reports `NO_FS` without a disk), so with no stick
the shell has its builtins and nothing else. Every runtime filesystem test:
`ls`, `cat`, `write`, `mount`, `erase disk`, `partition`, `format` — needs the
**USB 3 stick in a blue port**, as on Parallels, staged with `make stick`
(§4) so that `/bin` is there to run. This is not a Pi limitation; it is the
same architecture that made USB-MSD necessary there in the first place.

### Bisecting a hang with boot flag files

An empty file at the card's root switches off one boot step
(`kernel/src/bootflags.rs`), so a hang can be narrowed without a rebuild or a
serial cable: `touch /Volumes/OUROBOROS/NOXHCI`, eject, boot; delete it for the
next round. `make sdcard` leaves files at the root alone. The boot log names a
flag when it is set.

| Flag | What it switches off | A boot that reaches `shell ready` means |
|---|---|---|
| `NOXHCI` | xHCI discovery and bring-up: no USB at all | the hang is inside `pci::discover_xhci` |
| `XHCINOWR` | only the PCI command-register write in discovery (the takeover from firmware still happens), and the bring-up after the exit | the hang is the write or the bring-up after the exit, not the takeover; `NOXHCI` cannot split those two either, so a boot that reaches the shell under both flags leaves both open |
| `FBCON` | installing the SPCR serial console after the exit; the kernel's framebuffer console is installed instead, right after `exceptions::install()`, on the firmware's page tables | (not a bisection) every post-exit line, the MMU switch and the kernel's own `EXCEPTION …` report included, appears on HDMI, so the last one shows where the boot stops |

One flag per bisection boot; `FBCON` combines with either of the others.
They exist for the first Pi 400 boots (2026-09-28), which all ended on HDMI
with the firmware's text console: first partway through
`xhci: PCI command register was`, then under `NOXHCI` at the
`xHCI discovery failed (skipped…)` line, and under `XHCINOWR` at the earlier
MADT line.

Three more flags are test faults for QEMU and not diagnostics (`WALKFAULT`,
2026-10-02, makes the reporter's own report fault inside its image walk,
with `EARLYFAULT`; the rows must survive it). `EARLYFAULT`
(2026-10-01) asks the firmware's own `CopyMem` to write at an address nothing
maps, just before the xHCI takeover, so the fault is taken in the firmware's
code with the firmware's vectors installed, the shape of the Pi 4's first
serial boot below; what follows on serial is the early fault reporter's dump
(`kernel/src/earlyfault.rs`, checked by `make test-early-fault`). On a card it
only ends the boot in that dump. `MSDSTALL` (2026-10-01) is the other: it corrupts every seventh CBW so QEMU's stick stalls
(`test-usb-hub.py --stall`). The kernel honours it only for a stick whose
INQUIRY vendor is `QEMU`, but it has no place on a card.

**Why HDMI shows nothing after the firmware's text, without `FBCON`.** The
firmware describes a serial port in ACPI SPCR, so the kernel installs the
PL011 console after the exit (checkpoint 2) and every kernel line from there
on goes to serial only. The framebuffer is not idle, though: `cond` draws on
it whenever a framebuffer was discovered (`CON_INFO` asks `fbdev`, not the
kernel's console), which QEMU with `-device ramfb` confirms. So HDMI that
never changes after the firmware's text means the boot did not reach `cond`,
or the framebuffer does not show what is written to it. `NOXHCI` did not
change that, so it is not the xHCI step alone.

**`FBCON`'s first boot ended in the firmware's own exception handler**:
`xhci: PCI command register was 0x0140, wrote+read back 0x0146`, blank lines,
then `Synchronous Exception at 0x0000000039F31A40`. That message is EDK2's, not
the kernel's (the kernel reports `EXCEPTION core=N vector=… esr_el1=…`, the core since multi-core step 2), so the fault
came while the firmware's vectors were still installed: after that line and
before `exceptions::install()` just past `exit_boot_services`. The
`xHCI controller @ …` line that normally follows did not appear.

**Placing a firmware-reported address.** The kernel logs where the firmware
loaded it (`image @ <base>..<end>`, printed at startup and again, so that it
is still on screen, just before the xHCI step), and
`exiting boot services` right before the exit. With the address from HDMI:

```sh
cargo rustc -p ouroboros-kernel --target aarch64-unknown-uefi -- \
    -C link-arg=/MAP:build/BOOTAA64.map          # the same tree as the card
scripts/efi-symbol.py build/BOOTAA64.map \
    target/aarch64-unknown-uefi/debug/BOOTAA64.efi <base>..<end> <pc>
```

The build is deterministic apart from the PE timestamp and debug record, so
the relinked map fits the card's binary. The script refuses before naming
anything unless `<end>-<base>` is the relinked `.efi`'s `SizeOfImage` (a
misread digit, or a card built with another profile or tree) and the map and
`.efi` come from the same link. Checked on QEMU with a planted
pre-exit fault: its firmware printed the same `Synchronous Exception at`
line, and the script named the planted write. An address outside the image
is firmware code, and only the serial dump names that module.

**The second `FBCON` boot placed it** (build `b5bf5b3`): `image @
0x376df000..0x378c8000` (0x1e9000 bytes, matching the binary, which is the
check that the digits were read right: an earlier misreading of a low-resolution
photo landed in `.rdata`, and another on a stack store between two stack stores
that had succeeded), and `Synchronous Exception at 0x37758628`, offset
0x79628: `core::sync::atomic::atomic_load` (instantiated in `log`) + 0x5c, the
`ldr x8, [x8]` of a Relaxed load through the pointer the debug build had
spilled to the stack four instructions earlier. Its three callers are the
`AtomicUsize::load` wrappers of the kernel, `uefi` and `log`, and every
`AtomicUsize` among them is a static in the image, so the pointer should always
be valid. This time the fault came before the `was …` line, right after
`boot flag \FBCON is set`; the first `FBCON` boot faulted after it. A fault
that moves, on a load whose address cannot be wrong, points at memory changing
underneath the kernel (the stack slot overwritten, or the image's pages made
unreadable) rather than at one bad instruction, and nothing in the kernel
changes memory attributes before the exit. Unresolved: the firmware's serial
dump (ESR, FAR and a backtrace naming the caller) is what settles it.
**Update 2026-10-01: the firmware has no such dump.** EDK2's default handler
prints the one line through the serial port and everything after it with
`DEBUG()`, which a RELEASE build compiles out, and pftf's release zip ships
only the RELEASE firmware (its workflow builds DEBUG too and does not package
it). The Pi 4's first serial boot, below, showed exactly the one line, twice.
The kernel now prints the dump itself: see "The early fault reporter" below.

**`NOXHCI` + `FBCON` got through the exit** (`exiting boot services`, no
firmware exception), so the firmware-phase fault needs the xHCI takeover. Then
HDMI showed nothing more, not even `framebuffer console live`: something
between the exit and the MMU switch, with no console yet for an exception
report. `FBCON` now installs the framebuffer console right after
`exceptions::install()` and the kernel prints `installing our own identity
map` just before the switch, so that stretch is on screen. Checked on QEMU
(`-device ramfb`): a fault planted just before the switch shows
`EXCEPTION vector=4 esr_el1=0x96000047 far_el1=0x10` on the framebuffer, and
without the plant the boot reaches the shell.

**The Pi 4 then stopped at `exiting boot services` without the clear** (build
`0602ca3`, `NOXHCI` + `FBCON`), where an earlier build's boot of the same board
had cleared the screen: the same step behaving differently between boots, as
on the Pi 400. Nothing prints between the exit and that clear, so under
`FBCON` the kernel now draws solid white squares at the top-right, with plain
stores and no console: **square 1** when `exit_boot_services` has returned,
**square 2** when the exception vectors are written (on this board's EL2
handoff they are live only from the drop to EL1, checkpoint 5b). The early console's
clear wipes them, so squares still on screen mean the boot stopped before it,
and none at all means inside the firmware's `ExitBootServices`. Checked on
QEMU with a hang planted after each: one square, then two.

**Where the Pi 4 stops, as far as HDMI can say** (build `3da3cad`,
`NOXHCI` + `FBCON`): the screen cleared, with no squares and no text. The
clear is the early console's, and it wipes the squares, so the exit returned,
the vectors went in, and the whole-screen clear (one `write_bytes` and one
cache clean) reached the display; the first line after it
(`framebuffer console live early …`, drawn a pixel at a time with a clean per
glyph) never appeared. The stop is between the clear and that line, or the
line was drawn and never reached the display. Testing paused here on
2026-09-28 until the serial cable arrives: boot without flags first, since
the console is now at the translated `0xfe201000` and every line after the
exit goes to serial.

**The Pi 4's first serial boot (2026-10-01, build `b8aaf33`, no flag, the
Debug Probe on the PL011, `screen -L`).** Three things it settled:

- `console @ 0xfe201000 (via devicetree)`: the translation through `ranges`
  (#181) holds on the board. The item is closed.
- The boot got further than any HDMI session had shown: the GOP framebuffer,
  `_CCA 0`, the MADT (GIC V2, GICD `0xff841000`), all five programs loaded,
  the boot identity (`boot 1`, the UEFI variable absent, the file present, 32
  bytes of entropy), and `image @ 0x376e1000..0x378d0000, taking the xHCI
  controller next`. Then, with none of `discover_xhci`'s own lines (no
  `skipping`, no `PCI command register`, no `xHCI controller @`), the firmware
  printed `Synchronous Exception at 0x0000000039F2D1A0` twice and nothing
  else: no ESR, no FAR, no registers, no module. The address is above the
  image, so firmware code, during the takeover, which is where the Pi 400's
  first `FBCON` boot faulted too (`0x39F31A40`, 18 KB away, after its
  command-register write; the Pi 4's came before that line).
- The firmware's one line is all a RELEASE build prints (see the update
  above), so the "serial dump" the Pi 400 item waited for was never going to
  arrive from the firmware.

**The early fault reporter** (`kernel/src/earlyfault.rs`, the same day) is the
answer: right after console discovery the kernel registers its own handler for
synchronous exceptions through the firmware's `EFI_CPU_ARCH_PROTOCOL`, the
mechanism the firmware's GIC driver uses for IRQs, so VBAR_EL1 and the
firmware's interrupt handling are untouched. From then until
`exceptions::install()` a fault prints, on the serial console with raw writes
(or, with no serial console, on a framebuffer console made at that moment,
which clears the screen first: the HDMI-only case the Pi 400's first boots
were), the ESR decoded (class, fault status and level, read or write), FAR, ELR, SP,
LR, FP, every x register, a frame-pointer backtrace (the kernel is now built
with frame records, `.cargo/config.toml`), and for each code address the
loaded image holding it, from the firmware's debug image info table: the
kernel by offset, a firmware driver by the module name in its PE debug entry
(`XhciDxe.dll`, say) and the firmware-file GUID of its device path. Checked on
QEMU by `make test-early-fault`: a fault planted in the DXE core's `CopyMem`
(`EARLYFAULT`) prints `elr ... = DxeCore.dll ... + 0x1dd00`, then the kernel's
own frames, then the firmware's frames that called the kernel; and with the
registration deleted the test fails and the firmware's bare line comes back.
The frame walk believes a record only between the faulting SP and the end of
the stack's memory-map descriptor, so a clobbered frame pointer cannot make
the report fault and cut itself short. The framebuffer fallback is not
exercised by the test: QEMU's firmware always describes a serial console, so
the serial path wins there.
A second Pi 4 boot the same evening (17:40), meant to carry the reporter but
staged from the tree before #187 merged (the kernel's line numbers in the log
say so, and no `armed` line), showed the same shape at a third address:
`Synchronous Exception at 0x0000000039F36E14`, twice, after `taking the xHCI
controller next`, with the image at `0x376c4000..0x378b3000` this time. Three
boots, three firmware addresses, one step: the fault moves.

**The first boot with the reporter (17:44, build `b51b022`, no flag) went
silent instead.** `early fault reporter armed: a fault before the kernel's own
vectors reports on the serial console` printed where expected, every line up
to `taking the xHCI controller next` matched the earlier boots (the image at
`0x376ab000..0x378a700`, the console at `0xfe201000`), and then nothing at
all: no firmware line, no dump, the capture ends on that line. The one change
from the boot before was the registration, and with it the firmware's one line
is gone too, so the firmware's dispatcher took the registered path and what
came of it never reached the UART. Three readings, not yet told apart: the
handler ran and its console was empty or its writes went nowhere (the kernel's
own PL011 driver has never printed on a Pi, since every boot died before the
exit); the handler was never reached, the fault having wrecked the
dispatcher's data or the stack before the call (the memory-damage suspect,
which the moving address already favours); or there was no fault this time
and the takeover hangs, the memory-map read `arm` adds having moved the
firmware's allocations. The next boot asks the first question alone: `NOXHCI`
on the same card, which skips the takeover and should print the kernel's
post-exit lines through that driver.

**`NOXHCI` with the reporter (17:55): the kernel's PL011 driver works, the
reporter works, and the kernel is running at EL2.** The boot left boot
services and printed, through the kernel's own driver at `0xfe201000`,
`boot services exited, console live`, `installing our own identity map`, the
map's ranges (RAM `0x3b0000-0x3b400000`, device `0x0-0x3fffffff`, the six EL0
regions) and `identity map installed, MMU running on our own tables`. The
next thing on serial was the reporter's dump:

```
EARLY EXCEPTION (firmware vectors) type=0 esr=0x96000006 far=0xa000000 elr=0x376c923c sp=0x3b3fceb0 lr=0x376c9234 fp=0x3b3fced0 spsr=0x400003c9
  esr: ec=0x25 (data abort, same EL), fsc=0x06 (translation fault at level 2), read
```

Three facts in it. The fault was delivered through the FIRMWARE's vectors,
after `exceptions::install()` had written VBAR_EL1: so VBAR_EL1 is not the
vector base in use. `SPSR` mode bits are `0b1001`, EL2h: the firmware hands
the kernel off at EL2 (TF-A's BL31 enters UEFI at EL2 on this board), and
`exceptions.rs`'s "assumes EL1, not verified at any other EL" is the case that
was never tested. And the faulting read is at `0xa000000`, the first
virtio-mmio slot, a translation fault: the identity map that "installed" is
in TTBR0_EL1/TCR_EL1/MAIR_EL1, which do not govern translation at EL2, so the
MMU is still on the firmware's EL2 tables, where nothing maps that address.
Every EL1 system register the kernel writes after the exit (VBAR, TTBR0, TCR,
MAIR, SCTLR, the EL1 timer) has been written to a register the running
exception level does not use. That is also the shape of "the Pi 4 stops after
the early console's clear": a fault to the firmware's EL2 vectors with the
firmware's text console gone.

The dump itself was cut short: at frame 10 (the records below the kernel's
own, a return address inside the firmware volume at `0x26e28`) the image
naming walk took a translation fault of its own (`esr=0x96000007` in
`pe_codeview_name`), and the reentrancy guard halted before the register
rows. The register rows belong before the walk, and the walk needs a bound
on what it reads.

What follows is a kernel decision, not a boot: drop to EL1 right after
`exit_boot_services`, as Linux's entry does (`HCR_EL2.RW` for AArch64 EL1,
no traps, no stage 2, the EL1 physical timer allowed through `CNTHCTL_EL2`,
`SP_EL1` set, `eret`), and let `mmu.rs` enable the EL1 MMU from off rather
than swap tables under one that is on. Until then nothing after the exit on
a Pi means what its log line says. If it still shows only the line, the fault corrupted the firmware's
dispatcher or stack before the handler ran, which is itself the answer the
memory-damage suspect predicts.

**2026-10-02, the drop on the board: two `NOXHCI` boots from a card staged
off `main` at `367ceda` (#188 merged).** The first died inside
`ExitBootServices`; the second ran the drop and reached the shell.

*Boot 1 (boot identity 1).* Every line to `exiting boot services` as before,
then the reporter's dump, with the controller untouched:

```
EARLY EXCEPTION (firmware vectors) type=0 esr=0x96000047 far=0x38670810 elr=0x39f36e14 sp=0x3b3fcd90 lr=0x39f2ebb4 fp=0x3b3fcd90 spsr=0x60000309
  esr: ec=0x25 (data abort, same EL), fsc=0x07 (translation fault at level 3), write
  elr 0x39f36e14 = DxeCore.dll @ 0x39f2c000 + 0xae14
  lr  0x39f2ebb4 = DxeCore.dll @ 0x39f2c000 + 0x2bb4
  frames 1-9: DxeCore + 0x2d44, 0x3c94, 0xda20, 0xda48, 0xdc88, 0x5b00, 0x608c, 0x66b0, 0x6898
  frames 10-12: kernel + 0x7007c, 0x6f9b8, 0xd4ac
  frames 13-16: DxeCore + 0x943c, BdsDxe + 0x8c08, 0xa100, DxeCore + 0x918c
```

Read: a firmware write, nine frames below the kernel's call into
`ExitBootServices` (the `uefi` crate's `exit_boot_services`, which allocates
a pool buffer for the memory map and calls the service twice at most), to a
RAM address (`0x38670810`, in the firmware's own region below the kernel's
image) that the firmware's tables do not map at level 3. `x2` is
`0x38670808`, `x3` is `0x1100`, `x0` is `0x38660708`: a copy or fill of
0x1100 bytes, one write past `0x38670808`. **The same instruction,
`0x39F36E14`, is where the two 17:40 boots of 2026-10-01 died, inside the
xHCI takeover, before any `exiting boot services` line.** So the faulting
firmware routine is reached both from the PCI protocol and from
`ExitBootServices`, and it faults without the takeover: the memory-damage
suspect that named the xHCI is weakened, and the fault is now "a DxeCore
routine at `+0xae14`, writing into a page its own tables do not map,
intermittently". The pftf build's `RPi4.dsc` sets no heap guard, so the
unmapped page is not EDK2's freed-memory guard. The register rows printed
in full this time, and the walk reached the firmware's BDS frames: the
rows-first fix is still owed but was not needed here.

*Boot 2 (boot identity 2), the same card, power-cycled.* Checkpoint 5b's
three lines, in order, through the kernel's PL011 driver:

```
boot services exited, console live
running at EL2 after the exit
installing our own identity map
identity map RAM 0x3b0000-0x3b400000, device 0x0-0x3fffffff, per-task EL0 regions [...]
dropped from EL2 to EL1, on our own tables and vectors (GIC system registers: not implemented)
identity map installed, MMU running on our own tables
virtio-blk discovery failed (no virtio-mmio block device found)
MADT: GIC V2, GICD @ 0xff841000, GICC/GICR @ 0xff842000 (size 0x0)
mmu: 0x377b4000-0x377c2000 mapped Normal Non-cacheable
mmu: 0x377b4000-0x377c2000 walks as Normal Non-cacheable (attr 0x44) on every page in all 11 views, its neighbours and kernel data do not
shell ready - type and press Enter
```

**(confirmed)** The drop lands on the Pi 4: the `eret` into the kernel's
own EL1 regime from the L0-start tables worked first time, the GIC-400
came up at the MADT's addresses, the tick runs (the shell was served), and
every post-exit line now describes the machine. **Risk 1 did not fire the
way it was predicted to:** the virtio-mmio scan at `0xa000000` ran under the
kernel's own tables, where the low 1GB is a Device block, and reported no
device instead of faulting; under the firmware's EL2 tables on 2026-10-01
the same read faulted. So the scan is harmless on this board and the
premise of `virtio_mmio_probe_safe` is false but not dangerous; the item
to retire it stands on honesty, not safety. The shell prompt appeared on
HDMI: with a GOP framebuffer the console server draws there and the
kernel's serial console goes quiet after `shell ready`, so typing on the
serial terminal reaches the shell (the UART is the keyboard under
`NOXHCI`) and its echo lands on the monitor. The capture ends on the shell
line for that reason, not because the board stopped.

Next on this board: the `+0xae14` fault wants a second dump at the same
address, to compare `far` and the registers; and the no-flag boot, now
with the kernel's own vectors live after the exit, to see the takeover's
fault reported in full.

**2026-10-02, afternoon: four `NOXHCI` boots of a card from `main` at
`a9672d1` (#189, the reporter's rows first and its walk bounded).** Boots 1
to 3 went through the drop to the shell, as in the morning. Boot 4 died
BEFORE the exit, between `loaded account server` and the boot-identity
line, with the rows intact:

```
EARLY EXCEPTION (firmware vectors) type=0 esr=0x96000046 far=0x3e98fe00 elr=0x39f43b60 sp=0x3b3fbf40 lr=0x385b2fe0 fp=0x3b3fbf40 spsr=0x80000309
  esr: ec=0x25 (data abort, same EL), fsc=0x06 (translation fault at level 2), write
  x0 =0x3e98fe00 x1 =0x37fee018 x2 =0x20 ...
  elr 0x39f43b60 (in no loaded image)        <- DxeCore + 0x17b60, see below
  lr  0x385b2fe0 (in no loaded image)
  frames 1-3 in no loaded image; frames 4-16 in the kernel
```

A firmware `CopyMem` of 32 bytes from `0x37fee018` to `0x3e98fe00`, which
is inside the GOP framebuffer (`0x3e402000`, size `0x7e9000`): the
firmware's text console drawing a glyph row onto HDMI, as it had for every
line of this boot and the three before. The 2 MB block holding that part
of the framebuffer took a translation fault at level 2 in the firmware's
own tables. The images are unnamed because the reporter's walk was
bounded to three ranges, `0x3b0000..0x3e0000`, `0x400000..0x380b0000`,
`0x38120000..0x387b0000`, with "the map had more": the Pi's map has more
than 64 descriptors and the cap was applied before the merge, dropping
the top of RAM where every image lives (fixed on `pi4/reporter-tables`;
`elr` is `DxeCore + 0x17b60` by the morning's base). One fact the bound
gave anyway: the lowest RAM range starts at `0x3b0000`, so the 2026-10-01
entry claiming to hold `0x26e28` lies outside the walk, as the fix
intended.

**Three firmware faults today, read together.** Each is a write by the
firmware's DXE core to memory the firmware had mapped and was using,
taken as a translation fault in its own tables: at `0x38670810` (level 3,
in `ExitBootServices`), at `0x3e98fe00` (level 2, the framebuffer, while
printing), and on 2026-10-01 at `0x39F36E14`'s target inside the xHCI
takeover. Different kernel steps, different addresses, one of two to one
of four boots. What they share is the firmware's translation tables having
an invalid entry where a valid one was: a cleared or clobbered table page,
not a missing mapping. Whether the entry is zero (cleared) or garbage
(overwritten) is what the next instrument reads: the reporter walking the
firmware's live tables for `far` at the moment of the fault, printing each
level's entry (built the same afternoon on `pi4/reporter-tables`: a
`tables for far:` line after the rows, each level's entry down to the
invalid one and its four neighbours, and whether the level agrees with the
ESR's; on QEMU the planted fault reads `L0[0x0] ... (table), L1[0x1] ...
(table), L2[0x100] @ 0x47ffd800 = 0x0 (invalid), neighbours: [0xfe]=...003
[0xff]=...003 [0x101]=0x0 [0x102]=0x0; the ESR's level 2 agrees`, which is
the Pi's framebuffer fault's shape, with the answer the Pi's line will
give in the entry's value; the test faults land on the lowest page past
RAM that no descriptor of any type covers, chosen from the map at arm
time, not on an address assumed free). Suspects, in order: the kernel's own pre-exit writes landing
outside what it owns (its allocations move a little per boot, which fits
the intermittence); the firmware's own break-before-make on a split block;
the VideoCore writing into ARM memory. The dump's frames 4 to 16 are in the
kernel: with the linker map (`scripts/efi-symbol.py`) they say which
kernel call the firmware was printing for.

**2026-10-02, evening: the answer, from a card at `185a137` (#190, the
tables walked at the fault).** First `NOXHCI` boot, a fault inside the
boot-identity variable read, with the walk's line:

```
EARLY EXCEPTION (firmware vectors) type=0 esr=0x96000007 far=0x3b0038 elr=0x39be0384 sp=0x3b3fc930 ...
  esr: ec=0x25 (data abort, same EL), fsc=0x07 (translation fault at level 3), read
  tables for far: TTBR0 0x3b3fa000, T0SZ 20, 4K, from L0: L0[0x0] @ 0x3b3fa000 = 0x3b3fa038 (invalid), neighbours: [0x1]=0x388 [0x2]=0x0; the ESR says level 3, which DISAGREES
  elr 0x39be0384 = VarBlockServiceDxe.dll @ 0x39bd0000 + 0x10384
  frames 1-5 VariableRuntimeDxe.dll, 6-9 kernel, 10-13 DxeCore/BdsDxe, 14-15 at 0x26e28/0x26f88 (in no loaded image)
```

Read: the firmware's ROOT translation table, at `0x3b3fa000`, does not hold
page-table entries. Its first entry is `0x3b3fa038`, a pointer into the
same page 0x38 further on, the second is `0x388`, the third 0: the shape of
stack frames (a saved frame pointer, a small count), not descriptors. The
firmware's stack is 16 KB (`PcdCPUCorePrimaryStackSize` is `0x4000` in
pftf's `RPi4.dsc`) at the top of RAM, `0x3b3fc000..0x3b400000` (`sp` is
`0x3b3fc930` here, the DXE core's frames at `0x3b3ff7xx`), and its page
tables lie directly below it, the root two pages down. The kernel runs on
that stack, a debug build with large frames, and its deepest calls (plus
the firmware's timer interrupt landing on top, which is the intermittence)
push below `0x3b3fc000` and write stack frames over the tables. The TLB
keeps the firmware running on cached translations until some cold page is
walked, and that walk fails at whatever level's page the overflow reached:
level 3 on 10-01 and this morning, level 2 this afternoon (the
framebuffer), level 0 now. The ESR's "level 3" against the walk's "level
0" is the TLB too: the hardware walk that faulted used the still-cached
upper levels and failed at a clobbered L3 page; by the time the reporter
walked from memory, the root itself read as garbage.

So the suspect is the kernel, as the morning's list had first: not a write
outside what it owns, but its stack, which was never its own. QEMU's
firmware gives it 128 KB with the tables elsewhere, so no rig could see
this. Frames 14 and 15, at `0x26e28` and `0x26f88` below RAM, are the same
unplaceable addresses as 10-01's: the firmware's own entry stack frames in
TF-A or the FD at `0x0..0x3b0000`, outside the walk's bound and correctly
refused now.

**The fix is a stack the kernel owns**, switched to as its first act,
before anything the firmware or the kernel pushes can reach the tables;
until then every boot of a debug kernel on this board is a dice roll, and
even a release build only shrinks the frames. Everything after the exit
runs on the same stack today too (the drop sets `SP_EL1` to it), so the
switch matters past the exit as well. *Built the same evening on
`pi4/own-stack`: `KERNEL_STACK`, 256 KB in the image, entered before any
call; the banner line names the stack; the reporter's frame walk crosses
from it into the firmware's frames that called the kernel. The check on
this board: `NOXHCI` boots that never again fault in firmware code, and a
`tables for far:` line, if any fault comes, that shows entries and not
frames.*

**2026-10-03: the fix on the board. Four `NOXHCI` boots of a card at
`c8492ad` (#191), four shells, no fault.** The banner line each time:

```
UEFI stage alive, on its own stack (sp 0x3787c340, the kernel's stack 0x3783e870..0x3787e870, the entry's sp on the firmware's was 0x3b3ff730)
```

So the kernel's stack is in its image (`0x37788000..0x379cb000`), and the
firmware's SP at entry was `0x3b3ff730`, 0x8d0 below the top of its 16 KB
stack, with the root table at `0x3b3fa000` a further 0x5730 down: the
margin the whole boot used to run in. Every boot then went `running at EL2
after the exit`, `dropped from EL2 to EL1`, `identity map installed`, and
reached the shell, where the day before a boot faulted in firmware code one
time in two to one time in four. Two more facts from the same capture. The
card before the fix faulted once more before it was re-staged, in the
variable store's write this time (`far=0x3b7f38`, VarBlockServiceDxe +
0x11468), with the identical root-table garbage (`0x3b3fa038`, `0x388`,
`0`): the same overflow, same signature. And `shutdown` from the shell
printed `powering off` and the board went down: the first PSCI power-off on
this board, through the `smc` conduit the FADT names, as `power.rs` was
written to do.

These boots had no HDMI display (`GOP framebuffer discovery failed`), so
the console server took the serial backend and the shell's own lines came
over the cable: `login: no /etc/passwd - starting a root session`, and
`unknown command: ls`. Both are the known limit of `NOXHCI` on this board,
not defects: after the exit the kernel has no block device (the SD card is
the firmware's, and USB storage is what the flag skips), so `fsd` has no
disk, no `/etc` and no `/bin`. The disk arrives with the takeover.

**Next: the no-flag boot**, the xHCI takeover with the kernel's own stack
under it and the reporter still armed, which is the one pre-exit step
whose fault the overflow may or may not have been. The 2026-10-01 takeover
faults showed the same stack-frame shape in the firmware's one line
(`0x39F36E14` is the same DxeCore instruction the exit faulted at), so the
expectation is a clean takeover and the first USB keyboard and stick on
the board; if it faults, the dump now carries the tables and the canary.

**2026-10-03, later: the no-flag boot. The takeover is clean, and the
driver stops at a limit of its own.** Same card, `NOXHCI` removed, serial
capture in `screenlog.0`. Before the exit, in order: `taking the xHCI
controller next`, the PCI command register `0x0140 -> 0x0146`, the
controller at `0x600000000` (BAR `0xf8000000` plus the translation read from
`PciIo.GetBarAttributes`, here `0xfffffffaf8000000`), `exiting boot
services`. After it: `running at EL2`,
`dropped from EL2 to EL1`, `identity map installed`, and then the driver
itself, under the kernel's tables:

```
xhci: DMA pool @ 0x378e8000, 0xe000 bytes
xhci: controller @ 0x600000000, max_slots=32 max_ports=5
xhci: keyboard not available (controller wants 31 scratchpad buffers, only 8 are supported)
```

No fault anywhere in the boot, which closes the question this step was
for: the 2026-10-01 takeover faults were the stack overflow too. The
controller came out of its reset and its registers read sanely
(`max_slots=32`, `max_ports=5`) at the translated address, the first
nonzero translation seen working, and the driver
then refused it on purpose: `HCSPARAMS2` is `0xfc000031`, whose Max
Scratchpad Buffers field (bits 31:27, high bits 25:21) is 31, and
`xhci.rs` reserves `MAX_SCRATCHPAD_BUFFERS = 8` pages in its DMA pool.
QEMU's controller asks for few enough that the limit was never reached
before. The rest of the boot is what no xHCI means on this board: the
non-cacheable plan and its self-check pass on the pool (`walks as Normal
Non-cacheable ... in all 11 views`), the shell comes up over the serial
backend as root with no `/etc/passwd`, and `mount -a` finds no USB storage.

**Next: give the pool 32 scratchpad pages** (128 KB; the spec allows up to
1023, the VL805 asks for 31), and boot this card again. That is the first
time the board's controller is run past `DCBAAP`, so the port scan, the
keyboard and the stick are all new on this hardware from there.
*(Built on `pi4/scratchpad-32`, which also logs the count as
`scratchpads=` on the controller line and refuses a controller whose
`PAGESIZE` lacks 4 KB. Merged as #192.)*

**2026-10-03, later still: two boots of #192. The controller is accepted,
and the first command times out.** Both captures, the same lines:

```
xhci: DMA pool @ 0x378d0000, 0x26000 bytes
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: device connected on port 1
xhci: port 1 reset, speed=3
xhci: port 1 setup failed (command ring: timed out waiting for a completion event), continuing with other ports
xhci: keyboard not available (command ring: timed out waiting for a completion event)
```

Then, as before, `mmu: 0x378d0000-0x378f6000 walks as Normal Non-cacheable
(attr 0x44) on every page in all 11 views`, and a shell with no disk. The
scratchpad fix did its part: the driver programmed the controller, started
it, and reset a High Speed device on port 1. The timeout is the signature
Risk 8 predicted for non-coherent DMA, but that fix is in and its self-check
passes, so the cause is elsewhere. Two places where `xhci.rs` differed from
every driver that works on this controller (edk2 `XhciDxe`, Linux's
`xhci_write_64`, U-Boot):

- **64-bit registers were written as one 64-bit store** (DCBAAP, CRCR,
  ERSTBA, ERDP), across a PCIe bridge not shown to carry one intact. The
  others write the low half, then the high half, which xHCI section 5.1
  allows.
- **No barrier between the ring setup and the registers that hand it
  over.** The DCBAA, the command ring's Link TRB and the ERST are stores to
  the pool (Normal Non-cacheable); the register writes are Device stores,
  and nothing orders the two kinds without a barrier. Writing ERSTBA makes
  the controller fetch the ERST at once. Linux's `writel` has a barrier
  before every MMIO write; QEMU has no write buffer to show the miss.

**Built on `pi4/xhci-mmio-order`, one variable: the 64-bit store.**
`write64` writes the low half, then the high half; the barrier is held for
the round after, so the board answers one question. Two passive lines to
read: `xhci: WARNING: Host System Error after Run` if the controller
failed a DMA fetch when it started; and, on the first command timeout of a
boot, a dump of `USBSTS` (HCH, HSE), `CRCR.CRR` (whether the controller
took the command ring), `IMAN`, the DCBAAP/ERSTSZ/ERSTBA/ERDP readbacks
beside the pool's addresses, the event ring's slot at the dequeue pointer,
`ERST[0]` and the command that timed out. Checked on QEMU with the command
doorbell removed: `CRCR.CRR false`, readbacks equal to the pool's
addresses, an empty event slot, the Enable Slot TRB. (A probe that logged
what one 64-bit store to DCBAAP reads back was built and taken out at
review: it was the suspect access itself, and a mangled store could damage
CONFIG beside DCBAAP while the readback looked intact.)

**Read on the board:** success is the port scan getting past Enable Slot on
port 1, which names the 64-bit store as the cause. A timeout again refutes
it, and the dump says where to look: readbacks that differ from the pool's
addresses mean the registers still do not hold the rings; `CRCR.CRR false`
with correct readbacks means the controller never took the ring; `CRR true`
and an empty event slot means it ran the command and its event did not
reach the ring the CPU reads (the barrier is next); `HSE true` is a DMA
fault.

**2026-10-03: the board boot of #193. The controller runs commands, and
the keyboard comes up.** One boot, no `NOXHCI`:

```
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: device connected on port 1
xhci: port 1 reset, speed=3
xhci: slot 1 enabled
xhci: slot 1 addressed (port 1)
xhci: GET_DESCRIPTOR(Device) -> [12, 01, 10, 02, 09, 00, 01, 40, 09, 21, 31, 34, 21, 04, 00, 01, 00, 01]
xhci: port 1: hub - its ports are brought up after the root ports
xhci: device connected on port 3
xhci: port 3 reset, speed=0
xhci: port 3 setup failed (unsupported port speed 0 (only Low/Full/High/SuperSpeed/SuperSpeedPlus are implemented)), continuing with other ports
xhci: port 1: hub with 4 ports (speed=3, TT think time 3)
xhci: port 1.4: device connected, reset, speed=1
xhci: slot 2 enabled
...
xhci: port 1.4: boot-protocol keyboard - activating after the scan
...
xhci: keyboard ready
```

No dump, no HSE line: Enable Slot completed, so **the 64-bit store was the
cause**, the one variable of the round. Root port 1 is the VL805's USB 2
port, wired to a VIA hub (`2109:3431`, 4 ports) that carries the USB 2
lines of all four sockets; the keyboard, a Full Speed device, is on its
port 4 and reached `keyboard ready`, the first USB device brought up on
this board. The stick did not: it showed up on root port 3, one of the
VL805's SuperSpeed ports (a USB 2 device would have appeared behind the
hub), and came out of its reset with speed 0, at boot and again when
`mount -a` rescanned, so there was still no disk and `ls` was unknown.

**Built on `pi4/usb3-port-state`.** A port already in SS.Inactive (`PLS
6`) or Compliance (`PLS 10`) before its reset gets a Warm Reset instead of
a Hot one, as Linux does. A port that is not enabled after its reset gets
its PORTSC before and after, decoded (`CCS`, `PED`, `PLS`, `speed`); a
watch of up to a second for it to enable, logging each change; then, from
those two link states, a Warm Reset and the same watch; and it is refused
with its PORTSC rather than addressed if none of that brought it up. **Read
on the board:** `link state … before the reset, warm reset` means the port
was waiting for a Warm Reset from the start; `enabled after the reset`
means the link was still training and only needed time; `link state 6`
or `10, warm reset` then `enabled after the warm reset` means the Warm
Reset after the Hot one was the answer; `root port not enabled after
reset` means none of these, and the decoded PORTSC values say what the
port did instead.

**2026-10-03: a whole session on the board.** Card and stick staged from
`main` at #195 (`make sdcard`, `make stick`, the stick exFAT), no
`NOXHCI`, the keyboard and the stick plugged in:

```
xhci: port 1: hub with 4 ports (speed=3, TT think time 3)
xhci: port 1.2: device connected, reset, speed=3
xhci: port 1.2: USB mass storage - activating after the scan
xhci: port 1.4: device connected, reset, speed=1
xhci: port 1.4: boot-protocol keyboard - activating after the scan
xhci: storage bulk endpoints configured (IN 0x81 DCI 3, OUT 0x02 DCI 4)
xhci: keyboard ready
usb-msd: INQUIRY -> vendor='Lexar' product='USB Flash Drive'
usb-msd: capacity 243404800 sectors (512-byte blocks)
usb-msd block device installed
```

Then `fsd` mounted the stick (`exFAT mounted, disk commands available`, the
line interleaved with the other servers' on the serial console) and warned,
correctly, that exFAT cannot enforce permissions. `login:` asked for a user
from the stick's `/etc/passwd`; a mistyped first attempt said `Login
incorrect`, then `root` logged in. Typed on the USB keyboard, Hans
confirmed: `ls` showed `bin/ etc/ man/ Users/` (the `.ouroboros-stick`
marker hidden as a dot file), `ls bin` the programs, `man rev` its page,
`uptime` its ticks, Ctrl+C ended a waiting `rev`, and `halt` halted.
**The first full session on the Pi 4: USB keyboard in, USB disk mounted,
programs run from it.**

The stick came up behind the hub at High Speed (port 1.2), the USB 2 path,
not on a SuperSpeed root port as the stick of the earlier boots did, so the
root-port recovery built for that (#194) did not run, and the transaction
translator path (a Full or Low Speed device behind the High Speed hub) has
still only carried the keyboard's interrupt endpoint, not bulk storage. Open:
a stick on a SuperSpeed root port.

**2026-10-03: the stick on a SuperSpeed root port, and the reason for
`speed=0`.** The same Lexar stick (reformatted, re-staged), this time on
root port 2:

```
xhci: port 2: not enabled after the hot reset; before it 0x00281203 (CCS 1, PED 1, PLS 0, speed 4), after it 0x00200311 (CCS 1, PED 0, PLS 8, speed 0)
xhci: port 2: enabled after the reset: 0x00201203 (CCS 1, PED 1, PLS 0, speed 4)
xhci: port 2 reset, speed=4
...
xhci: port 2: USB mass storage - activating after the scan
usb-msd block device installed
```

The port was already enabled in U0 at SuperSpeed before the driver touched
it; the Hot Reset sent the link back through Polling (`PLS 8`), and PRC was
set while it was still there, with `PED 0` and speed 0. The old code read
the speed at that moment, which is every `port 3 reset, speed=0` of the
earlier boots. Waiting for the port to enable was the whole fix; the Warm
Reset path was not needed. The descriptor now says `bcdUSB 0x0320` (it said
`0x0210` behind the hub, the same stick at High Speed). Then the session as
before: `exFAT mounted`, `login: root`, `ls` from the stick, `halt`.

Which path a USB 3 stick takes is decided by the link, not by what is on
it: in a blue socket it trains SuperSpeed and appears on a root port (2 or
3, one per blue socket); in a black socket, or a blue one whose SuperSpeed
pins did not make contact or whose link did not train, it appears at High
Speed behind the hub. The reformat between the boots cannot have changed
that, since enumeration reads no sector.

**2026-10-03, evening: the ordering fixes on the board.** Card and stick
staged from `main` at `e738302`, which carries #196 (a `dsb sy` before the
registers that hand the controller its rings) and #197 (TRBs published as
a batch, the first one's cycle bit flipped last behind `dmb oshst`). The
stick in a blue socket, the keyboard behind the hub. That the card carried
this build is read from the image, since a boot does not yet say which
build it is: `image @ 0x37760000..0x379bd000` is `0x25d000` bytes, the
`SizeOfImage` of the `BOOTAA64.EFI` built for the staging, after both
merges.

```
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: port 3: not enabled after the hot reset; before it 0x00281203 (CCS 1, PED 1, PLS 0, speed 4), after it 0x00200311 (CCS 1, PED 0, PLS 8, speed 0)
xhci: port 3: enabled after the reset: 0x00201203 (CCS 1, PED 1, PLS 0, speed 4)
xhci: port 3 reset, speed=4
xhci: port 1: hub with 4 ports (speed=3, TT think time 3)
xhci: port 1.4: device connected, reset, speed=1
xhci: storage bulk endpoints configured (IN 0x81 DCI 3, OUT 0x02 DCI 4)
xhci: keyboard ready
usb-msd: INQUIRY -> vendor='Lexar' product='USB Flash Drive'
usb-msd block device installed
```

The same session as the two boots before it: the stick on SuperSpeed root
port 3, through the same Polling window #194 waits out; the keyboard on
port 1.4; `exFAT mounted at partition LBA 2048`; `login: root`; `ls`,
`mount`, `man ls`, `ls Users/` from the stick; `shutdown` powered the board
off. No fault, no timeout, no `WARNING`. Every request on the way went
through the batch publish: Enable Slot and Address Device for three slots,
each device's descriptors and configuration as control transfers (Setup,
Data and Status published together), the hub's, the keyboard's and the
storage endpoints' commands, and every bulk transfer `fsd` made. **#196 and
#197 break nothing on the board.** That is all a boot can say about them:
the board ran without them too, and what they fix is a window this boot did
not have to hit.

**2026-10-03, later: #198 on the board, and the first boot that names its
build.** Card and stick staged from `main` at `08d3b4e` (#198, every xHCI
register write through `mmio_write32`/`mmio_write64`, a call and a `dsb sy`
each; #199, the build line). The capture says which build it is, so this
time nothing was inferred:

```
UEFI stage alive, build 08d3b4e27601 debug, on its own stack (sp 0...
boot services exited, console live, build 08d3b4e27601 debug
running at EL2 after the exit
dropped from EL2 to EL1, on our own tables and vectors (GIC system registers: not implemented)
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: port 3 reset, speed=4
xhci: keyboard ready
usb-msd block device installed
```

Then the same session as the boots before it: the stick on SuperSpeed root
port 3 through #194's wait, the keyboard on port 1.4 behind the hub, `exFAT
mounted`, `login: root` (a first attempt with an empty name said `Login
incorrect`, as it should), `ls` from the stick, `shutdown` powered off. No
fault, no timeout, no `WARNING`. **The barrier on every register write costs
nothing visible on the board.** (`netd: boot identity: boot 1`: the card was
restaged, so its boot counter started again.) `screen -L` appends, so the
capture also holds the previous boot above this one; the build line is what
tells the two apart.

**2026-10-04: #200 on the board, ERDP before ERSTBA.** Card and stick staged
together from `main` at `d89908f`, which is #200's kernel (`097da77`) plus a
docs-only commit, so the build line names `d89908f` and the ERDP order is
the round's only variable. `screenlog.0` was moved aside first, so the
capture holds these two boots and nothing older:

```
UEFI stage alive, build d89908f1d0ab debug, on its own stack (sp 0...
boot services exited, console live, build d89908f1d0ab debug
running at EL2 after the exit
dropped from EL2 to EL1, on our own tables and vectors (GIC system registers: not implemented)
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: port 3 reset, speed=4
usb-msd block device installed
```

Two boots, both to the shell: the stick on SuperSpeed root port 3 through
#194's wait, `exFAT mounted`, `login: root`, then `ls`, `ls -l`, `ls etc`,
`ls bin`, `cat etc/passwd`, `man`, `chello`, `bootid` and `shutdown`, which
powered off both times. `bootid` said `boot 1` and then `boot 2`: the card
was restaged, so its counter started again. No fault, no command timeout,
no `Host System Error`, no `WARNING`. **The ERDP order costs nothing visible
on the board.** No keyboard and no display this time, on purpose: Hans typed
over the serial console, so the hub reported four ports and nothing behind
them (`keyboard not available`), which is the empty hub and not a finding.
The keyboard behind the hub was last seen working on `08d3b4e`, the boot
above.

**2026-10-04, later: #201 on the board, ERSTSZ and ERSTBA keeping their
RsvdP bits.** Stick then card staged from `main` at `c07804a` (#201's
merge), the card's `BOOTAA64.EFI` compared byte for byte with `build/esp`'s
before the eject. One boot, over the serial console:

```
UEFI stage alive, build c07804a040e7 debug, on its own stack (sp 0...
boot services exited, console live, build c07804a040e7 debug
dropped from EL2 to EL1, on our own tables and vectors (GIC system registers: not implemented)
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: RsvdP kept: ERSTSZ 0x0, ERSTBA 0x0
xhci: port 3 reset, speed=4
usb-msd block device installed
```

**The VL805's RsvdP bits read 0 after HCRST**, as on QEMU, so this change
writes exactly what the code before it wrote: the read-modify-write is spec
form on this controller, and now observed to be. Then `exFAT mounted`,
`login: root`, `ls`, `ls etc`, `ls bin`, `bootid` (`boot 1`, the card
restaged), `shutdown`, powered off. No fault, no command timeout, no `Host
System Error`, no `WARNING`; no keyboard, by choice.

**2026-10-04, later still: #202 on the board, every RsvdP write logged.**
Stick then card staged from `main` at `9ff7e4a` (#202's merge), the card's
kernel compared byte for byte with `build/esp`'s. One boot over serial:

```
UEFI stage alive, build 9ff7e4a9831b debug, on its own stack (sp 0...
boot services exited, console live, build 9ff7e4a9831b debug
dropped from EL2 to EL1, on our own tables and vectors (GIC system registers: not implemented)
xhci: controller @ 0x600000000, max_slots=32 max_ports=5 scratchpads=31
xhci: RsvdP before the reset: USBCMD 0x0
xhci: RsvdP after the reset, kept by each write: USBCMD 0x0, CONFIG 0x0, CRCR 0x0, ERSTSZ 0x0, ERSTBA 0x0
xhci: port 3 reset, speed=4
usb-msd block device installed
```

**Every RsvdP bit the driver now keeps reads 0 on the VL805, USBCMD's even
before the reset, where the firmware's state is**, so #202 writes what the
code before it wrote. Then `exFAT mounted` and `login: root` (the account
files read off the stick), and the shell answering three empty lines; no
commands beyond that this time, and no `shutdown` in the capture. No fault,
no command timeout, no `Host System Error`, no `WARNING`.

**2026-10-04, evening: #203 on the board, the controller as handed over.**
Stick then card staged from `main` at `7f39b6e` (#203's merge), the card's
kernel compared byte for byte with `build/esp`'s. One boot over serial:

```
UEFI stage alive, build 7f39b6ea226c debug, on its own stack (sp 0x37...
boot services exited, console live, build 7f39b6ea226c debug
xhci: as handed over: USBCMD 0x8 (R/S false, RsvdP 0x0), USBSTS 0x19 (HCH true, CNR false)
xhci: at the reset write: USBSTS 0x19 (HCH true)
xhci: RsvdP after the reset, kept by each write: USBCMD 0x0, CONFIG 0x0, CRCR 0x0, ERSTSZ 0x0, ERSTBA 0x0
usb-msd block device installed
```

**The firmware hands the VL805 over halted**: R/S 0 and HCH 1 both as
handed over and at the HCRST write, so the kernel does not reset a running
controller on this board. What the firmware leaves set is not a running
controller but its traces: USBCMD's HSEE (bit 3), and in USBSTS a pending
EINT (bit 3) and a Port Change Detect (bit 4), all cleared by the reset.
Then `exFAT mounted`, `login: root`, `ls`, `ls bin`, `man ls`, `shutdown`,
powered off. No fault, no command timeout, no `Host System Error`, no
`WARNING`.

**2026-10-04, night: #205 on the board, the halt before HCRST.** Stick then
card staged from `main` at `676797f` (#205's merge and its records), the
card's kernel compared byte for byte with `build/esp`'s. Two boots over
serial in one capture: the firmware's settings had been reset, its boot
order now starting with the network, so the first boot sat a long time at
the network attempt before it reached the card; Hans set the SD card first
in the firmware's menu and booted again.

```
boot services exited, console live, build 676797f83e01 debug
xhci: as handed over: USBCMD 0x8 (R/S false, RsvdP 0x0), USBSTS 0x19 (HCH true, CNR false)
xhci: at the reset write: USBSTS 0x19 (HCH true, found running false)
xhci: RsvdP after the reset, kept by each write: USBCMD 0x0, CONFIG 0x0, CRCR 0x0, ERSTSZ 0x0, ERSTBA 0x0
usb-msd block device installed
```

**Handed over halted, so nothing was written before the reset, as
predicted.** Both boots, from `boot services exited` to the first `login:`,
match #203's line for line but for the new field and one task region a page
higher. The RAM span is #203's (`0x3b0000-0x3b400000`), so the firmware's
reset left "Limit RAM to 3 GB" on. No fault, no command timeout, no `Host
System Error`, no `WARNING`.

**New, and not the kernel's change: `root` was refused at first.** Boot 1:
`login: root`, `Login incorrect`, twice, then a reboot into the firmware.
Boot 2: `root` refused once, then `user` logged in, `exit`, and `root`
logged in. Every earlier capture logs `root` in on the first try. The
staged secrets are version 2, so no login rewrites `/etc/shadow`; the
likelier path is that the shadow read failed and nothing said so:
`find_account_line` retries only `NO_FS`, and any other error from `fsd`
reads as a wrong password. Unconfirmed: the password is not echoed and the
refusal does not say why. On the roadmap.

Narrowed in the same session: after a logout `root` logged in; after a
reboot it was refused 5 s after the prompt and accepted 5 s later, nothing
printed between. **#206 then made the login say why** (a read error with its
code, no entry, an entry that does not parse; a bare `Login incorrect` is a
wrong password). On the board with it (`build 0af55b5c3b81 debug`, stick and
card re-staged): four boots, `root` typed at once each time, logged in every
time, no line, the boot counter 1 to 4. Not reproduced; the line stays armed.

---

## 7. Risks, ranked

<a name="risk-1"></a>
### Risk 1 — the `virtio_mmio_probe_safe` heuristic's premise breaks here

The flag is `discovery.is_some()` (confirmed: `main.rs:144`), and its own
comment is candid about what grounds it: *"QEMU, the only platform this scan
has ever been confirmed safe on, also always has a working ACPI/SPCR console."*
The Pi 4 is the first platform to break that correlation in the dangerous
direction — it will (predicted) have a working SPCR console **and no virtio
transport anywhere on the board**. The flag goes true, and
`virtio_mmio::find_device` scans 32 slots from `SLOT_BASE = 0x0a00_0000`
(confirmed: `virtio_mmio.rs:75`).

**Predicted outcome: benign.** Pi 4 RAM starts at `0x0` and runs contiguous, so
`0x0a00_0000` is ordinary mapped RAM rather than the unmapped device hole that
faulted on Parallels. The scan only reads, the magic value will not match, and
`find_device` returns `NotFound` quietly.

**If that prediction is wrong**, the signature is the familiar one: `ESR_EL1`
`EC=0x25`, `DFSC=0x10`, with `FAR_EL1` equal to `0x0a000000` exactly. The fix is
not another heuristic — it is to gate the scan on an actual virtio node found in
devicetree or ACPI, which is the real-discovery mechanism `main.rs`'s comment
already notes the scan lacks.

### Risk 2 — DMA above 3 GB

Covered in §5. Leave the firmware limit enabled; buy the 4 GB board.

### Risk 3 — PL011 vs. mini UART mismatch

pftf auto-detects which UART is in use from whether `config.txt` contains the
relevant overlay (confirmed: pftf/RPi4 readme). If the firmware's SPCR describes
one UART and your cable is wired to the pins driven by the other, the symptom is
a **silent serial console while HDMI works fine** — which reads exactly like a
hang if HDMI isn't connected. Fix it in `config.txt` (`dtoverlay=disable-bt`
moves the PL011 onto GPIO14/15), not in the kernel.

### Risk 4 — the `netd` boot race transfers directly

[`testing-parallels.md`](testing-parallels.md)'s Risk #1: `load_auth` blocks
`serve()` while it reads `/etc/cluster/id` and `authorized` through `fsd`, and
the `\NOEXEC` probe ahead of them, and USB-MSD is markedly
slower than QEMU's virtio-blk, which can push the health-ping supervisor into a
restart loop that QEMU never shows. The Pi's runtime storage is USB-MSD too
(§6), so this risk arrives unchanged — and the Pi's USB stack has one more layer
of real hardware under it than Parallels' did.

### <a name="risk-4b--net-wait-is-not-a-sleep"></a>Risk 4b — `NET_WAIT` is not a sleep, so the retry budget may not exist

**DEFERRED TO THIS SESSION ON PURPOSE, with the reason written down.** It is a
known defect with a known fix and no way to test the fix on QEMU — this is the
rig that can, so it is a task the first bench session should expect to pick up.

`load_auth`'s retry loops call `NET_WAIT(40)` expecting a 40 ms sleep. They do
not necessarily get one: `tasks.rs` wakes a `WaitReason::NetInput` waiter when
`net_has_frame() || has_queued_message(waiter) || timed_out`, and it **consumes
nothing**. `load_auth`'s own `read_file_chunk` is a *sender-filtered* `MSG_CALL`
on `FSD_TASK`, so it never drains anything else. Once the supervisor's health
ping is queued — `PING_INTERVAL` is 64 ticks ≈ 1.28 s, and `netd` is Blocked for
almost all of `load_auth` — that message sits in the mailbox, every later
`NET_WAIT(40)` returns in microseconds, and the documented "~2 s at 40 ms a try"
collapses into a busy-spin that burns the whole budget in a moment.

**Why QEMU cannot test it:** measured, not assumed — instrumented, the `\NOEXEC`
probe retries **0 times** there, because virtio-blk has `fsd` ready before `netd`
asks. The loop never executes, so neither the bug nor a fix for it is
observable. USB-MSD on this board is the first rig where the loop runs at all.

**What to watch for**, the same signature as Risk 4 above: `netd` restarting at
boot, or coming up with `export CLOSED` on a card holding a perfectly good
`/etc/cluster/id`. If the disk mounts later than ~1.3 s the budget is already
spent, and the id/authorized reads get one attempt each.

**The fix, if it fires:** `load_auth` must drain its mailbox while it waits —
answering the health ping rather than ignoring it — instead of treating
`NET_WAIT` as a timer. That touches supervision, which is why it was not written
blind against a rig that cannot run it.

### Risk 5 — no second console when the first one fails

Unlike a VM, there is no host-side window to fall back on. Serial and HDMI are
the whole diagnostic surface, and §6 step 2 is the case where you lose serial
entirely. **Connect both from the first boot**, before anything needs debugging.

---

### Risk 6: firmware refuses the exclusive root-bridge open

Before the fix, `pci::discover_xhci` opened `PciRootBridgeIo` with
`open_protocol_exclusive`, which asks firmware to disconnect every driver bound
anywhere below that root bridge. If any of them refused, the open failed with
`ACCESS_DENIED`, the loop skipped the bridge without a word, and the result was
`no xHCI controller found`.

**Observed on QEMU on 2026-09-27, and it has been happening since 2026-08-29.**
`make run-usb-kbd` has found no keyboard since `-device virtio-rng-device`
joined every disk target (#26). One variable at a time: with the RNG and
QEMU's default virtio-net-pci NIC, the open is refused; drop the RNG, or keep
it and drop the NIC (`-nic none`), and the controller is found and the
keyboard comes up. The likely mechanism, not confirmed: this EDK2 starts its
network stack only when an RNG protocol exists, and that stack will not let
go of the NIC. The same shape is how `framebuffer.rs` once killed the firmware
console on Parallels, the bug its module doc records.

**Why it matters here:** on the Pi the firmware has the VL805 bound to its own
xHCI driver with the USB keyboard as console input, and a hardware RNG.

**Fixed on branch `pi400/bar-translation`.** Every root-bridge open in
`pci.rs` is now read-only (`GetProtocol`), and the one exclusive open left is
of the xHCI controller's own `PciIo`, which asks only firmware's xHCI driver
and the USB devices under it to stop. Checked on QEMU with the RNG and the NIC
both present: the controller is found, and keystrokes injected through the
monitor reach the shell; the three-device rig (`run-usb-multi`'s keyboard,
tablet and storage stick) finds all three. **Not checked:** Parallels, where
the old exclusive opens were the confirmed-working shape. Its firmware drivers
for other PCI devices now stay bound until `exit_boot_services`, where before
they may have been torn down. The signature, if firmware refuses the narrower
open on the board: `pci: xHCI controller at … skipped (xHCI controller: firmware
refused its PciIo (…))`, then `xHCI discovery failed` with the same reason.

**Also fixed with it: the takeover now happens last before
`exit_boot_services`.** Stopping firmware's xHCI driver also stops the USB
storage under it, and a Pi booted entirely from a USB stick has its ESP there.
Discovery used to run before the loader read the boot programs and before the
boot identity read its counter file, both off the ESP; it now runs after both.
**Checked on QEMU, with a control that fails:** booted from a USB stick holding
`build/esp.img` and no other disk, `main`'s kernel took the controller and then
panicked with `failed to load shell program: couldn't open the boot volume`;
this branch loads every program over firmware's USB stack, takes the
controller, and then mounts the same stick through `usb_msd.rs` as its runtime
disk (`FAT32 mounted`). So one USB 3 stick can be the Pi's whole disk, boot and
runtime, as far as the kernel is concerned. Whether pftf itself boots from USB
is a firmware question the board settles.

**Also a new dependency:** both PCI discovery paths (the xHCI and the 16550
console) now need firmware to have a `PciIo` handle for the device and to
answer `GetBarAttributes`; before, the root bridge alone was enough. Without
either, the device is refused (`no firmware PciIo handle at its location`, or
`no BAR0 attributes from firmware`). That is deliberate, the fail-closed
choice, and it is also why the address is now checked before the controller
is taken, not after: a refused controller is left with firmware's driver
still running it.

**One consequence not checked anywhere but QEMU:** firmware's drivers for the
*other* PCI devices now stay bound until `exit_boot_services`. Under the old
exclusive open, where it succeeded (Parallels), they were stopped first. A
firmware driver that kept a device writing to memory by DMA past
`exit_boot_services` would corrupt memory the kernel has taken back. UEFI
drivers are required to stop DMA at that point, but only a Parallels run
settles it.

### Risk 8: PCIe DMA is not cache-coherent, and every xHCI buffer is cacheable

**Confirmed from sources, 2026-09-27; this blocks all USB on the Pi until it
is fixed.** On the BCM2711, DMA by PCIe devices, the VL805 xHCI included, is
not coherent with the Cortex-A72's data caches:

- The pftf firmware's own ACPI table for the PCIe root says so:
  `Name(_CCA, 0)    // Mark the PCI noncoherent`, with a `_DMA` window of
  `0x0`-`0xbfffffff`, translation 0 (edk2-platforms
  `Platform/RaspberryPi/AcpiTables/Pci.asl`; `Xhci.asl` has `_CCA 0x0` too).
- The firmware does cache maintenance for PCIe DMA itself:
  `DmaLib|EmbeddedPkg/Library/NonCoherentDmaLib/NonCoherentDmaLib.inf`
  and `NonCoherentIoMmuDxe` (edk2-platforms `Platform/RaspberryPi/RPi4/RPi4.dsc`).
- Linux's devicetree has no `dma-coherent` on `pcie@7d500000` or anywhere in
  `bcm2711.dtsi`, `bcm2711-rpi-4-b.dts` or `bcm2711-rpi-400.dts` (upstream and
  the Raspberry Pi tree), so Linux treats it as non-coherent and does cache
  maintenance.
- FreeBSD's `sys/arm/broadcom/bcm2835/bcm2838_pci.c` creates its DMA tag
  without `BUS_DMA_COHERENT`.

**Why it matters here:** `xhci.rs` keeps its command ring, event ring, device
contexts, transfer rings and the keyboard and control buffers in statics that
`mmu.rs` maps as Normal write-back cacheable, and does no cache maintenance
anywhere; `usb_msd.rs`'s sector buffers are the same. QEMU and Parallels
model coherent DMA, so nothing has ever shown the difference. On the Pi the
controller can read stale memory (a TRB still sitting in the CPU's cache) and
the CPU can read stale cache lines (missing an event the controller wrote).
Predicted signature: xHCI discovery and the controller reset succeed
(registers are MMIO, mapped Device), then the first command times out,
`xhci: keyboard not available (command ring: timed out waiting for a completion
event)`, or behaves erratically. (predicted)

**Fixed on branch `pi400/noncacheable-dma` (2026-09-27), checked on QEMU
as far as QEMU can check it.** All of `xhci.rs`'s and `usb_msd.rs`'s DMA
memory now lives in one page-aligned pool (`xhci::DMA_POOL`, 14 pages on
QEMU), and `mmu.rs` maps it Normal Non-cacheable, cleaned from the cache at
install - **only where the firmware declares DMA non-coherent**: the kernel
scans the ACPI DSDT/SSDTs for `Name(_CCA, Zero)` (`acpi::dma_noncoherent`),
which the pftf firmware writes for the PCIe root and QEMU does not (it
writes `_CCA One`); under a hypervisor that emulates xHCI (Parallels), a
guest-side non-cacheable mapping could disagree with the host's cacheable
one, so a coherent platform keeps the pool ordinary memory. QEMU models no
caches, so it cannot show the bug or the fix; what it can show is the
mapping, through the CPU's own table walker, and the kernel checks that at
every boot (with the scan's answer forced on QEMU, the pool walks
non-cacheable and USB still works). The lines to read on the board: early,
before `exit_boot_services`, `ACPI declares DMA non-coherent (_CCA 0, in
<table> at offset <n>)` (the table and offset say which `_CCA` matched, which
matters because the rule is "any device": a match that is not the PCIe
root's can be looked up in a disassembly of that table); and
just before `shell ready`, `mmu: <pool> mapped Normal Non-cacheable` and
`mmu: <pool> walks as Normal Non-cacheable (attr 0x44) on every page in all
N views` (the mapping's report is held until the consoles are up, so a
framebuffer-only boot draws it too - but the console server clears the
screen when it starts, and the kernel keeps no log to read back, so read
these on the SERIAL console). `ACPI declares no non-coherent DMA` there
means the scan missed the firmware's `_CCA` (and USB will misbehave); a
`WARNING: mmu:` line means the mapping is wrong. (predicted to work; the
board is its first real test)

**What the fix was, as planned:** map every xHCI and USB-storage DMA buffer
Normal Non-cacheable (MAIR `0x44`), with no cacheable alias, or clean and
invalidate the caches around every DMA. The first is the usual choice for
rings, and it is the same `mmu.rs` mechanism Risk 7's framebuffer needs: a
way to map chosen physical ranges non-cacheable at 4 KB granularity. The
addresses need no change: the PCIe inbound window is identity (translation
0), and every buffer must lie below 3 GB (`0xC0000000`), which the
firmware's "Limit RAM to 3 GB" setting (§5) guarantees while it stays on.

### Risk 7: the HDMI console is mapped cacheable

**The mapping is confirmed; the symptom is predicted.** `mmu.rs` maps every
1 GB block of the RAM span as Normal write-back cacheable memory, and gives a
discovered device region its own Device mapping only when it lies *outside*
RAM (`build_tables`'s `l1_span_devices`, applied only where the RAM loop left
the block unmapped). A framebuffer inside RAM therefore gets the cacheable
RAM mapping, silently. Checked on QEMU 2026-09-27 with `-device ramfb`, whose
framebuffer OVMF allocates from guest RAM: `GOP framebuffer @ 0x5c7a0000`,
inside `identity map RAM 0x40000000-0x60000000`, and no `device region …
mapped as its own device block` line for it. QEMU cannot show the
consequence (TCG models no caches), and Parallels never met it (its
framebuffer is a PCI BAR, outside RAM).

On the Pi 4/400 the firmware's framebuffer sits in the VideoCore's share of
the first gigabyte, inside the RAM span. (predicted) The CPU's pixel writes
then land in its data cache, and the display engine, which reads memory
directly, sees whatever reached memory: **stale or half-drawn text on HDMI,
correcting itself in patches as cache lines are evicted**, while the serial
console is fine. That is the signature, and it is a display artefact, not a
hang.

**Fixed on branch `pi400/noncacheable-dma` (2026-09-27), by cleaning, not
by remapping.** The framebuffer stays ordinary cacheable memory, and every
write path in `fbdev.rs` and `fbconsole.rs` ends by cleaning the bytes it
wrote out to memory (`mmu::clean_to_poc`, `dc cvac`), where the display
engine reads them. A non-cacheable mapping was built first and replaced
after review: under a hypervisor whose host reads guest memory cacheable
(QEMU `ramfb` with hardware acceleration), a guest non-cacheable
framebuffer can show stale text, and scrolling a non-cacheable framebuffer
reads about 8 MB of uncached memory per line at 1080p. The clean is right
on the Pi, harmless under a hypervisor, and a no-op on a framebuffer that
is a PCI BAR (Parallels). QEMU models no caches, so a screendump can show
only that rendering still works, not that the clean is needed; the board
is its test (the signature above: stale or half-drawn HDMI text).

**The fix, as it was planned:** map an in-RAM framebuffer as Normal
Non-cacheable (what Linux uses for a framebuffer: write-combining), at 4 KB
granularity at its edges so no kernel memory next to it loses its caching,
and merged with the per-task EL0 page splits that share the same gigabyte.
The smaller alternative is to clean the written range to the point of
coherency (`dc cvac`) after every framebuffer write in `fbdev.rs` and
`fbconsole.rs`. Either way, `mmu.rs`'s module doc is the required reading
first.

## Develop on QEMU first (raspi3b / raspi4b) — you don't have to wait for the boards

QEMU emulates the Raspberry Pi, so Pi-specific bring-up can start on the fast dev
loop before any hardware is on the bench. Confirmed available in this project's
QEMU (`qemu-system-aarch64 -machine help` lists **`raspi3b`** and **`raspi4b`**,
plus `raspi2b`, `raspi3ap`, …).

**The nuance that decides how useful this is: our kernel is UEFI-native.** It
builds as a UEFI application (`BOOTAA64.EFI`, target `aarch64-unknown-uefi`) and
boots via firmware; QEMU's `raspi4b` machine, by contrast, boots the **raw**
BCM2711 path — a `kernel8.img` loaded with `-kernel`, no UEFI underneath. So the
two Pi routes map onto QEMU differently:

- **The preferred route — UEFI (pftf firmware) — is already covered by QEMU's
  `virt` + OVMF**, which is the *current* dev loop (`make run*`). Everything the
  UEFI/ACPI/GOP/MADT stack does is exercised there today with no Pi machine at
  all. The only Pi-UEFI-specific bits that `virt` can't show (the actual pftf
  firmware's ACPI tables, real peripheral addresses) need the pftf image or real
  hardware — see §1 and §6.
- **The fallback route — raw `kernel8.img` — is what QEMU's `raspi3b`/`raspi4b`
  emulate**, and that's where they earn their place: a rig for developing the
  Pi's own peripheral drivers (the real PL011 base at the BCM2711 peripheral
  window, GIC-400 = GICv2, the mailbox/GPIO) on QEMU before hardware. Using it
  means a *raw-boot build variant* we don't have yet (link at the Pi load
  address, no boot services), so it's a small project of its own — worth it only
  if/when the UEFI route is abandoned (§7 Risk 1 is the trigger).

A starting command for the raw path, for when that variant exists:

```sh
qemu-system-aarch64 -M raspi4b -kernel kernel8.img -serial stdio -display none
```

Caveats: peripheral coverage on the `raspi*` machines is **partial and varies by
QEMU version** (networking and USB especially — the same "no NIC on this target"
story as §2), so verify against the version in use. See the QEMU Arm docs
(<https://www.qemu.org/docs/master/system/arm/raspi.html>) and
[`resources.md`](../resources.md) for the OSDev-wiki bare-metal-Pi references.

## 8. When the boards arrive

The first session is not "run the test matrix." It is:

1. Wire serial, boot the **stock pftf firmware alone** with no Ouroboros files
   on the card, and confirm you reach the UEFI shell over the serial cable. This
   separates every firmware-and-cabling problem from every kernel problem, and
   it is worth the ten minutes twice over.
2. Add the Ouroboros tree, boot, and **capture the full log to a file** —
   `screen -L`, or `tee` from `minicom`. The checkpoint list in §6 is the
   checklist; record which prediction each line confirmed or broke.
3. Only then run the single-machine matrix from
   [`testing-parallels.md`](testing-parallels.md) §"What you *can* validate."
4. **Pick up the deferred `NET_WAIT` task** (§7 [Risk 4b](#risk-4b--net-wait-is-not-a-sleep)).
   It is queued for this session specifically: the defect is understood, the fix
   is sketched, and QEMU cannot exercise either — instrument `load_auth`'s retry
   count on this board and see whether the loop runs at all before deciding
   whether the fix is needed. This is the one piece of open work that has been
   waiting on **hardware** rather than on a decision.

Then update this document in place: turn every **(predicted)** into
**(confirmed)** or into a numbered entry in a new postmortem. If §7 Risk 1 fires,
that postmortem is already half-written — the prediction, the signature, and the
fix are all above, which is the whole reason for writing them down first.

---

## Sources

- [pftf/RPi4 — Raspberry Pi 4 UEFI firmware](https://github.com/pftf/RPi4) —
  install procedure, the ACPI/devicetree and 3 GB RAM settings and why the
  latter exists, 115200 default baud, PL011/mini-UART auto-detection.
- [worproject/rpi5-uefi](https://github.com/worproject/rpi5-uefi) — archived
  February 2025; the basis for not choosing a Pi 5.
- [edk2-porting/edk2-rk3588](https://github.com/edk2-porting/edk2-rk3588) — the
  RK3588 alternative.
- [Platform/RPi4: ACPI improvements (edk2 patch series)](https://patchew.org/EDK2/20191218114156.9036-1-pete@akeo.ie/) —
  confirms the RPi4 firmware ships FADT, SPCR, DBG2, an XHCI table and a dummy
  MCFG, which is what §6 step 2's prediction rests on.
- [rust-embedded/rust-raspberrypi-OS-tutorials](https://github.com/rust-embedded/rust-raspberrypi-OS-tutorials) —
  the raw BCM2711 register facts, if the UEFI path ever has to be abandoned. See
  [`research-redox-and-pi.md`](../research/research-redox-and-pi.md) Part 2.
- [OSDev Wiki](https://wiki.osdev.org/) — bare-metal reference: the
  *Raspberry_Pi_Bare_Bones* / *ARM_RaspberryPi* / *PL011* / *GIC* pages are the
  register-level companion to the tutorials above. Curated with the other
  external references in [`resources.md`](../resources.md).
- [QEMU Arm — Raspberry Pi boards](https://www.qemu.org/docs/master/system/arm/raspi.html) —
  the `raspi3b`/`raspi4b` machine types (see "Develop on QEMU first" above).
- [Pi 400 keyboard HID descriptors (Gadgetoid/pi400kb #7)](https://github.com/Gadgetoid/pi400kb/issues/7)
  and [raspberrypi/firmware #64](https://github.com/raspberrypi/firmware/issues/64):
  user `lsusb` output showing the VIA Labs hub `2109:3431` on BCM2711 boards and
  the Pi 400's Holtek keyboard `04d9:0007`, the basis for §1b.
- [edk2-platforms `RPi4.dsc`](https://github.com/tianocore/edk2-platforms/blob/master/Platform/RaspberryPi/RPi4/RPi4.dsc)
  and `Silicon/Broadcom/Bcm27xx/Library/Bcm2711PciHostBridgeLib`: the PCIe bus
  and CPU windows and the aperture translation, the basis for checkpoint 4's
  correction; edk2 `MdeModulePkg/Bus/Pci/PciHostBridgeDxe/PciRootBridgeIo.c`
  for the convention that apertures are reported as CPU addresses.
- Risk 8 (PCIe DMA is not cache-coherent):
  [Linux `bcm2711.dtsi`](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/arch/arm/boot/dts/broadcom/bcm2711.dtsi)
  (no `dma-coherent`; the `pcie0` `dma-ranges` and its 3 GB comment);
  edk2-platforms [`Platform/RaspberryPi/AcpiTables/Pci.asl`](https://github.com/tianocore/edk2-platforms/blob/master/Platform/RaspberryPi/AcpiTables/Pci.asl)
  and `Xhci.asl` (`_CCA 0`), and [`RPi4.dsc`](https://github.com/tianocore/edk2-platforms/blob/master/Platform/RaspberryPi/RPi4/RPi4.dsc)
  (`NonCoherentDmaLib`, `NonCoherentIoMmuDxe`); FreeBSD
  [`sys/arm/broadcom/bcm2835/bcm2838_pci.c`](https://github.com/freebsd/freebsd-src/blob/main/sys/arm/broadcom/bcm2835/bcm2838_pci.c)
  (a DMA tag without `BUS_DMA_COHERENT`).
- This repository: `kernel/src/main.rs`, `virtio_mmio.rs`, `pci.rs`, `madt.rs`,
  `block.rs`, `loader.rs`, and the `Makefile`'s `esp`/`image` targets.
- [Raspberry Pi Debug Probe](https://www.raspberrypi.com/documentation/microcontrollers/debug-probe.html):
  the UART cable's colours (orange TX, yellow RX, black GND, no VCC) and the
  macOS device name, the basis for §3's wiring.

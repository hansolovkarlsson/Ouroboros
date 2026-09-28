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
- **A 3.3 V USB-TTL serial adapter** (CP2102 or FT232). This is the single most
  important item in the list.
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

On macOS, `ls /dev/tty.usbserial-*` after plugging the adapter in, then:

```sh
screen /dev/tty.usbserial-XXXXXXXX 115200
```

115200 8N1 is the pftf firmware default (confirmed: pftf/RPi4 readme). Exit
`screen` with `Ctrl-a k`.

---

## 4. Building the card

The Pi's boot medium holds two things that do not collide: the firmware at the
root of the FAT partition, and Ouroboros's ESP tree underneath `EFI/`. The
firmware boots `RPI_EFI.fd` via `armstub`, which then boots
`\EFI\BOOT\BOOTAA64.EFI` from that same partition — the well-known removable-media
path `make esp` already writes to (confirmed: `Makefile`'s `esp` target).

```sh
make esp     # populates build/esp/ - do NOT use `make image` here
```

**Do not `dd` `build/esp.img` onto the card.** It is a fixed 64 MB `hdiutil`
image (confirmed: `Makefile`'s `image` target) with no room for the firmware,
and writing it would leave the rest of the card unusable. Copy the tree instead:

```sh
# 1. Format the card as MS-DOS (FAT32) with an MBR partition map.
#    Disk Utility: "MS-DOS (FAT)" + "Master Boot Record". Or:
#    diskutil eraseDisk MS-DOS OUROBOROS MBRFormat /dev/diskN     # CHECK diskN FIRST

# 2. Firmware first.
curl -LO https://github.com/pftf/RPi4/releases/latest/download/RPi4_UEFI_Firmware_v1.42.zip
unzip -o RPi4_UEFI_Firmware_v1.42.zip -d /Volumes/OUROBOROS
rm -f /Volumes/OUROBOROS/Readme.md

# 3. Ouroboros on top.
cp -R build/esp/ /Volumes/OUROBOROS/

# 4. Strip the AppleDouble sidecars, same reason `make image` does
#    (FAT holds no xattrs, so macOS spills them into ._* files that show up
#    in `ls` as mangled 8.3 aliases).
find /Volumes/OUROBOROS -name '._*' -delete
find /Volumes/OUROBOROS -name '.DS_Store' -delete
diskutil eject /Volumes/OUROBOROS
```

Check the release tag against [the releases page](https://github.com/pftf/RPi4/releases)
rather than trusting the version pinned above. Keep the firmware files' names
exactly as shipped — the readme is explicit that renaming them breaks boot.

Once this stabilises it is worth a `make sdcard SDCARD=/Volumes/OUROBOROS`
target next to `parallels-hdd`, so the round trip is one command like every
other target in this project.

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
   proves the card layout and the firmware handoff, nothing else.

2. **`console @ {base:#x} (via {source})`.** *(predicted: `via acpi`)* — the
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
**USB 3 stick in a blue port**, as on Parallels. This is not a Pi limitation; it is the
same architecture that made USB-MSD necessary there in the first place.

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

**The fix, not yet made:** map an in-RAM framebuffer as Normal
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
- This repository: `kernel/src/main.rs`, `virtio_mmio.rs`, `pci.rs`, `madt.rs`,
  `block.rs`, `loader.rs`, and the `Makefile`'s `esp`/`image` targets.

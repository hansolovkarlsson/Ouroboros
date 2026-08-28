# External resources — reference links for when we get stuck

A curated, annotated list of *external* references (as opposed to the
`research-*.md` notes, which are our own synthesis). Kept so a link that
proved useful once is findable again — especially the hardware bring-up
material we'll want when Pi 3/Pi 4 work starts.

## OSDev Wiki — the OS-developer's reference

**<https://wiki.osdev.org/>** — the community wiki for hobby/bare-metal OS
development. Broad and uneven in places, but the single best starting point for
"how does *X* work on bare metal and what's the minimal code," with concrete
register-level detail the vendor manuals bury. Useful across the whole project,
but especially for the ARM/Raspberry Pi bring-up ahead.

Pages worth going to first (paths are `wiki.osdev.org/<Page_Name>`):

- **Raspberry_Pi_Bare_Bones**, **ARM_RaspberryPi**, **Raspberry_Pi_4** — the
  canonical bare-metal Pi tutorials: memory map, the mailbox interface, the
  GPIO/UART init dance, `kernel8.img` boot. The complement to
  `rust-raspberrypi-OS-tutorials` (see [`research-redox-and-pi.md`](research-redox-and-pi.md)
  Part 2) — the wiki explains the *why*, the tutorials give working Rust.
- **PL011** — the UART we already drive (`kernel/src/uart.rs`); the Pi's PL011
  register layout is the same, only the base address differs (BCM2711 peripheral
  base `0xFE00_0000`).
- **GIC** / **GICv2** — the interrupt controller; the Pi 4's GIC-400 is a GICv2,
  which our `madt.rs`/`gicv2.rs` path already handles.
- **AArch64** family (exception levels, the MMU, the generic timer) — the same
  ground `kernel/src/{exceptions,mmu,timer}.rs` cover, useful for cross-checking.
- **UEFI**, **ACPI** — for the pftf/RPi4 UEFI route (our *preferred* Pi 4 path).

Caveat: treat the wiki as a starting point, not gospel — cross-check register
values and bit layouts against the authoritative source (ARM ARM, the BCM2711
peripherals datasheet, Linux headers) the way `mmu.rs`/`gic.rs` already do.

## QEMU can emulate the Pi — start there before the boards arrive

QEMU has machine types for the Raspberry Pi, so Pi-specific bring-up can begin
on the fast dev loop *before* any hardware is on the bench (confirmed available
in this project's QEMU: `qemu-system-aarch64 -machine help` lists **`raspi3b`**
and **`raspi4b`**, plus `raspi2b`, `raspi3ap`, etc.).

What this gives us and its limits — the full write-up (which path it exercises,
the raw-vs-UEFI nuance, and a starting command) lives in
[`testing-pi4.md`](testing-pi4.md) under "Develop on QEMU first." In short:
QEMU's `raspi4b` boots the **raw BCM2711** path (a `kernel8.img`, our *fallback*
route), which is exactly the right rig for developing Pi peripheral drivers
(the real PL011 base, GIC-400, the mailbox) without hardware; the *preferred*
UEFI route stays best exercised on QEMU's `virt` + OVMF (our existing loop) and
then real hardware. Peripheral coverage on the `raspi*` machines is partial and
varies by QEMU version — verify against the version in use.

- QEMU Arm system-emulation docs (Raspberry Pi boards):
  <https://www.qemu.org/docs/master/system/arm/raspi.html>

## See also (in-repo)

- [`testing-pi4.md`](testing-pi4.md) — the Pi 4 test plan (UEFI-first, the
  serial rig, boot checkpoints, ranked risks).
- [`research-redox-and-pi.md`](research-redox-and-pi.md) Part 2 — the Pi 4
  bring-up cookbook mapped onto our stack (peripheral base, GIC-400, the boot
  routes).
- [`testing-qemu.md`](testing-qemu.md) — the current QEMU dev/test loop.

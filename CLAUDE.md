# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Ouroboros is an ARM64 (aarch64) operating system written in Rust (plus some
assembly where needed), still in its earliest stages. Design goals, taken
from `notes.txt` and `README.md`:

- Microkernel architecture
- POSIX-ish system calls
- Preemptive multitasking
- Filesystem choice still undecided (research needed; likely a simple FS first)
- Draws ideas from Linux, Minix, and Plan 9
- Primary test target is Parallels on Apple Silicon, not just an emulator

## Boot architecture (read this before touching boot/entry code)

The kernel builds directly as a UEFI application for the
`aarch64-unknown-uefi` target — there is no separate bootloader stage yet.
This is a deliberate choice, not a placeholder to "fix later" casually:
Parallels boots ARM VMs exclusively through UEFI firmware, with no
equivalent to QEMU's `-kernel` direct-boot shortcut, so UEFI is the only
boot path that works on both the fast QEMU dev loop and the real Parallels
test target. Don't reach for direct-kernel-boot conveniences (multiboot,
raw `-kernel` loading, etc.) without accounting for the fact that they won't
work on the Parallels target.

The `[[bin]]` in `kernel/Cargo.toml` is named `BOOTAA64` deliberately: UEFI
firmware auto-boots removable media at `\EFI\BOOT\BOOTAA64.EFI`, so the
build output can be staged straight into an ESP layout with no renaming step.

As the kernel outgrows what's reasonable to run under UEFI boot services,
expect a split into a thin UEFI bootloader stage that loads and hands off to
a separate kernel binary. That split hasn't happened yet.

`main()` never returns `Status::SUCCESS` — it logs a boot message and parks
the core in a `wfe` spin loop (`halt()` in `kernel/src/main.rs`) instead.
Returning to firmware is a dead end for kernel code.

`main()` now calls `boot::exit_boot_services(None)` partway through and
permanently leaves the UEFI environment. Everything before that call may use
`log::*`, `alloc`, and UEFI protocols as normal; everything after may not —
the UEFI logger and global allocator are boot-services-backed and will
panic/misbehave if touched post-exit. Console output after that point goes
through `kernel/src/uart.rs`, a polling PL011 driver taking a runtime base
address — no fallback address, see below.

### Console discovery: devicetree doesn't work here — confirmed on both platforms

`kernel/src/devicetree.rs` tries to discover the console UART from the
devicetree UEFI publishes (`EFI_DTB_TABLE_GUID` in the UEFI configuration
table). **Tested on both platforms, not a hunch: neither publishes one.**
QEMU's bundled firmware (Homebrew's `edk2-stable202408-prebuilt.qemu.org`)
and Parallels' own firmware both report `DiscoveryError::NoDtb` — both are
ACPI-oriented. So the devicetree path is real and exercised (it correctly
detects and reports the absence rather than crashing or hanging), but has
never actually found a console on anything this project targets. The real
next step for a discovered console — on either platform — is ACPI table
parsing (specifically the SPCR table, which plays the same role as DTB's
`/chosen/stdout`), not devicetree. Devicetree discovery is left in place in
case it's ever useful on other hardware, but don't expect it to start
working on QEMU or Parallels without SPCR support being added alongside it.

`find_dtb`/`discover_pl011` both run **before** `exit_boot_services`, even
though parsing the blob itself doesn't need boot services — so the result
gets logged through the UEFI console (works on any platform) before any raw
MMIO is touched. Keep new discovery mechanisms (ACPI/SPCR included) on this
side of the `exit_boot_services` call for the same reason: whatever address
you find, you want to have already reported it before you risk writing to it.

### There is no fallback UART address, and there should not be one again

There used to be one — `uart::QEMU_VIRT_PL011_BASE`, written to whenever
devicetree discovery failed. It was removed after direct confirmation that
it hard-crashes real Parallels hardware: nothing is mapped at QEMU's PL011
address on Parallels' virtual chipset, so the write faults, and with no
exception vectors installed yet, that fault has nowhere to go and takes the
whole VM down. `main.rs` now only ever constructs a `Uart` when
`discover_pl011` actually returned an address (`if let Ok(base) = discovery`
in `main()`) — no confirmed address means no post-exit console output, not
a guess. Don't reintroduce a hardcoded fallback address without also
building exception vectors first, so a bad guess degrades to a caught fault
instead of a VM crash.

### Parallels disk attachment: a real trap, not a hunch

Getting `esp.img` to actually boot in Parallels took a few wrong turns worth
recording so they don't get re-discovered:

- Parallels' Hard Disk device rejects a raw `.img` outright — it only takes
  its own `.hdd` container format.
- Attaching the raw image to the **CD/DVD** device instead *looks* like it
  works (Parallels accepts arbitrary files there) but doesn't: the optical
  driver expects an ISO9660 filesystem, and `esp.img` is MBR+FAT32 (a hard
  disk layout). Symptom was firmware seeing a `BLK` device in the EFI Shell
  but resolving zero `FS` mappings — the device was visible, the content
  just wasn't recognized as anything the CD-ROM driver understands.
- The actual path: `make parallels-hdd` converts `esp.img` → `esp.dmg` (via
  `hdiutil convert`) → `esp.hdd` (via `prl_disk_tool create --hdd ... --dmg
  ...`, `prl_disk_tool`'s only documented way to build a `.hdd` from an
  existing raw image). `prl_disk_tool` silently fails ("Unable to open the
  disk image") if given relative paths — always pass absolute ones. Attach
  the resulting `esp.hdd` as the Hard Disk device, not `esp.img` and not
  CD/DVD.
- `esp.hdd` stores a pointer to `esp.dmg`'s absolute path rather than a copy
  of its data — the two files must stay together.

This was all confirmed by directly booting `esp.img` in QEMU with a real
`-drive format=raw` (not the `vvfat` passthrough `make run` uses) before
touching Parallels at all, to first rule out `make image` producing a
malformed disk. It didn't — the image was always fine; only the Parallels
attachment mechanism was wrong.

### Next milestone

Two viable candidates, not yet decided between:

- **ACPI/SPCR console discovery** — the actual path to getting console
  output back on both QEMU and Parallels, now that devicetree is confirmed
  to be a dead end for both.
- **Exception vectors** — independently overdue: right now any bad memory
  access anywhere in the kernel is an unrecoverable platform-level crash
  (exactly what just happened), with zero diagnostic beyond "the VM
  stopped." Having even a minimal synchronous-exception handler that could
  report `ESR_EL2`/`FAR_EL2` before halting would make every future bug in
  this codebase dramatically faster to debug than the process this session
  just went through (crash → guess → re-test → repeat).

Either could reasonably come first; exception vectors arguably pay for
themselves immediately by making the next round of hardware-discovery work
safer to iterate on. After whichever comes first: MMU/paging setup, a
timer-driven preemption tick, then the first steps toward the
microkernel/syscall boundary.

## Commands

```sh
make build                  # cargo build (debug)
make build PROFILE=release  # release profile
make run                    # stage ESP dir + boot in QEMU (aarch64 virt machine, OVMF firmware)
make image                  # build esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make parallels-hdd          # wrap esp.img into esp.hdd, a Parallels-native virtual hard disk
make clean
```

`make run` requires QEMU (`brew install qemu`, which also provides the
aarch64 OVMF firmware `make run` points at). `make image` requires macOS's
`hdiutil`. `make parallels-hdd` additionally requires Parallels Desktop
installed (uses its bundled `prl_disk_tool`).

There is no test suite yet — this is pre-alpha kernel code that only proves
it boots.

## Toolchain

Pinned via `rust-toolchain.toml` (stable channel, `aarch64-unknown-uefi`
target — installs automatically on first `cargo build`). The target ships
prebuilt `core`/`alloc` on stable, so no nightly toolchain or
`-Z build-std` is needed. `.cargo/config.toml` defaults the build target to
`aarch64-unknown-uefi`, so plain `cargo build`/`cargo clippy` at the repo
root already target the right platform.

## Structure

```
kernel/
  src/main.rs        #[entry] point: UEFI init, ExitBootServices, then halt()
  src/uart.rs        raw PL011 console driver, used only after ExitBootServices
  src/devicetree.rs  console UART discovery via the UEFI-provided devicetree
```

Single-crate workspace for now (`Cargo.toml` at the root is a workspace with
one member). Expect this to grow into more crates as the bootloader/kernel
split happens and shared code (e.g. a syscall ABI crate) emerges.

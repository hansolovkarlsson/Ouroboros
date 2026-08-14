# Ouroboros

A microkernel operating system for ARM64 (aarch64), written in Rust with some assembly.

## Design goals

- Microkernel architecture, POSIX-ish system calls
- Preemptive multitasking
- ARM64 target, developed and tested primarily in Parallels on Apple Silicon
- Filesystem support TBD — likely a simple filesystem to start
- Draws inspiration from Linux, Minix, and Plan 9

## Boot strategy

The kernel boots as a UEFI application (`kernel/`, built for the
`aarch64-unknown-uefi` target), rather than being loaded directly by an
emulator's `-kernel` flag. This is because Parallels boots ARM VMs through
UEFI firmware and has no equivalent of QEMU's direct-kernel-boot shortcut, so
UEFI is the one boot path that works on both the fast QEMU dev loop and the
real Parallels test target.

Right now the kernel *is* the UEFI application — there's no separate
bootloader stage yet. As the kernel grows past what's reasonable to run under
UEFI boot services, expect this to split into a thin UEFI bootloader that
loads and hands off to a separate kernel binary.

## Prerequisites

- Rust, via `rustup` (the pinned toolchain and `aarch64-unknown-uefi` target
  install automatically from `rust-toolchain.toml` on first `cargo build`)
- [QEMU](https://www.qemu.org/) for the local dev loop: `brew install qemu`
  (this also provides the aarch64 OVMF firmware used by `make run`)
- Parallels Desktop for testing against the real target environment

## Building

```sh
make build        # cargo build, debug profile
make build PROFILE=release
```

## Running in QEMU

```sh
make run
```

This stages `target/aarch64-unknown-uefi/debug/BOOTAA64.efi` into `esp/EFI/BOOT/BOOTAA64.EFI`
(the well-known path UEFI firmware boots automatically from removable media)
and boots it in `qemu-system-aarch64 -machine virt` against Homebrew's
aarch64 OVMF firmware.

## Testing in Parallels

```sh
make image
```

Produces `esp.img`, a FAT32 disk image with the same `EFI/BOOT/BOOTAA64.EFI`
layout. Create a new Parallels VM of type "Other" / generic ARM64 with no
installed OS, attach `esp.img` as its boot disk, and start it — UEFI firmware
will pick up `BOOTAA64.EFI` automatically.

## Layout

```
kernel/     the kernel, built as a UEFI application (aarch64-unknown-uefi)
```

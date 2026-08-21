TARGET       := aarch64-unknown-uefi
USER_TARGET  := aarch64-unknown-none
PROFILE      ?= debug
KERNEL       := target/$(TARGET)/$(PROFILE)/BOOTAA64.efi
# Userland programs (shell/) always build in release profile, regardless
# of $(PROFILE) above - a hard requirement, not a style choice, since the
# relocating loader work switched to position-independent linking
# (.cargo/config.toml). Confirmed by direct experiment: a *debug* build
# fails to link at all under that model - rust-lld rejects an
# R_AARCH64_ABS64 relocation inside `core::fmt::builders::PadAdapter`,
# pulled in from the prebuilt (not rebuilt-per-project) libcore.rlib by
# ordinary debug-build panic/bounds-check formatting machinery, which
# that prebuilt rlib was never compiled with -C relocation-model=pic
# support for. A *release* build's optimizer eliminates enough of that
# unreachable-in-practice panic-formatting code that the poisoned object
# code never gets pulled into the link at all. See loader.rs's module
# doc comment and docs/processes.md's "Binary format" section for the
# full writeup - this is a real, hard-won toolchain constraint, not a
# guess, and not something -Z build-std would be worth reintroducing a
# nightly-toolchain dependency to work around (see rust-toolchain.toml's
# own "no nightly, no -Z build-std needed" comment).
SHELL_ELF    := target/$(USER_TARGET)/release/shell
SHELL_BIN    := target/$(USER_TARGET)/release/shell.bin
HELLO_ELF    := target/$(USER_TARGET)/release/hello
HELLO_BIN    := target/$(USER_TARGET)/release/hello.bin
PONG_ELF     := target/$(USER_TARGET)/release/pong
PONG_BIN     := target/$(USER_TARGET)/release/pong.bin
FSD_ELF      := target/$(USER_TARGET)/release/fsd
FSD_BIN      := target/$(USER_TARGET)/release/fsd.bin
UPPER_ELF    := target/$(USER_TARGET)/release/upper
UPPER_BIN    := target/$(USER_TARGET)/release/upper.bin
COND_ELF     := target/$(USER_TARGET)/release/cond
COND_BIN     := target/$(USER_TARGET)/release/cond.bin
ESP_DIR      := esp
OVMF         := $(shell brew --prefix qemu 2>/dev/null)/share/qemu/edk2-aarch64-code.fd
PDT          := /Applications/Parallels Desktop.app/Contents/MacOS/prl_disk_tool

# llvm-objcopy ships with the `llvm-tools` rustup component (added via
# rust-toolchain.toml), but unlike cargo/rustc itself it isn't on PATH or
# reachable via `rustup which` - only cargo-binutils' `cargo objcopy`
# subcommand knows to find it that way, and pulling in cargo-binutils for
# one call felt like more dependency than this needs. Its real location is
# a fixed, discoverable path under the toolchain's own sysroot instead.
HOST_TRIPLE  := $(shell rustc -vV | sed -n 's/^host: //p')
OBJCOPY      := $(shell rustc --print sysroot)/lib/rustlib/$(HOST_TRIPLE)/bin/llvm-objcopy

CARGO_FLAGS :=
ifeq ($(PROFILE),release)
CARGO_FLAGS += --release
endif

.PHONY: build shell-bin hello-bin pong-bin fsd-bin upper-bin cond-bin esp run run-virtio-console run-usb-kbd run-usb-multi run-gicv3 image run-image parallels-hdd test-parallels clean

# Overridable by `make test-parallels VM_NAME=... CMDS=... BOOT_WAIT=...`.
VM_NAME     ?= Ouroboros
CMDS        ?= help
BOOT_WAIT   ?= 12

build:
	cargo build $(CARGO_FLAGS)

# Built separately from `build`: a different target (aarch64-unknown-none,
# not the workspace-default aarch64-unknown-uefi - see Cargo.toml's
# default-members comment), always --release (see SHELL_ELF's own comment
# above for why), and staged as a real, if stripped, ELF file - not
# `objcopy -O binary` anymore. `loader.rs` is a real (if narrowly-scoped)
# ELF loader now: it needs the program header table and a `.rela.dyn`
# section to actually load and relocate this correctly, both of which a
# raw-binary conversion would have thrown away. `--strip-all` alone
# (dropping `-O binary`) still shrinks the file by removing the symbol
# table and debug info - confirmed by direct comparison to still leave
# every section the loader actually reads (program headers, section
# headers, `.rela.dyn`'s contents) fully intact.
shell-bin:
	cargo build -p shell --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(SHELL_ELF) $(SHELL_BIN)

# Same recipe as shell-bin for the second userland program (hello/) -
# same target, same release-only constraint, same shared linker script
# (the -T flag in .cargo/config.toml is workspace-relative).
hello-bin:
	cargo build -p hello --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(HELLO_ELF) $(HELLO_BIN)

pong-bin:
	cargo build -p pong --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PONG_ELF) $(PONG_BIN)

# The filesystem server (fsd/) - boot-loaded by the kernel from
# \EFI\ORBS\FSD.BIN into its reserved task slot 2, not spawned.
fsd-bin:
	cargo build -p fsd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(FSD_ELF) $(FSD_BIN)

upper-bin:
	cargo build -p upper --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(UPPER_ELF) $(UPPER_BIN)

# The console server (cond/) - boot-loaded by the kernel from
# \EFI\ORBS\COND.BIN into its reserved task slot 3, not spawned (same
# shape as the filesystem server in slot 2).
cond-bin:
	cargo build -p cond --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(COND_ELF) $(COND_BIN)

# Stage the EFI System Partition layout QEMU/Parallels expect: a removable
# UEFI drive boots \EFI\BOOT\BOOTAA64.EFI automatically, no boot manager
# entry needed. \EFI\ORBS\ (must fit FAT's 8.3 short-name limit, which
# the full 9-character project name doesn't - see loader.rs's
# CONFIG_PATH doc comment for why) holds everything that isn't the kernel
# itself: the default shell binary and the config file (loader.rs's
# CONFIG_PATH) naming which program to load - edit INIT.CFG and rebuild
# just that program to swap it out, no kernel rebuild required.
esp: build shell-bin hello-bin pong-bin fsd-bin upper-bin cond-bin
	mkdir -p $(ESP_DIR)/EFI/BOOT $(ESP_DIR)/EFI/ORBS
	cp $(KERNEL) $(ESP_DIR)/EFI/BOOT/BOOTAA64.EFI
	cp $(SHELL_BIN) $(ESP_DIR)/EFI/ORBS/SH.BIN
	cp $(HELLO_BIN) $(ESP_DIR)/EFI/ORBS/HELLO.BIN
	cp $(PONG_BIN) $(ESP_DIR)/EFI/ORBS/PONG.BIN
	cp $(FSD_BIN) $(ESP_DIR)/EFI/ORBS/FSD.BIN
	cp $(UPPER_BIN) $(ESP_DIR)/EFI/ORBS/UPPER.BIN
	cp $(COND_BIN) $(ESP_DIR)/EFI/ORBS/COND.BIN
	printf '\\EFI\\ORBS\\SH.BIN' > $(ESP_DIR)/EFI/ORBS/INIT.CFG

# Boots the ESP directory directly in QEMU (no disk image needed) against
# the aarch64 OVMF firmware installed by `brew install qemu`.
#
# The drive is explicitly attached as a virtio-mmio block device
# (if=none + -device virtio-blk-device), not left to QEMU's own default -
# a plain `-drive ...,media=disk` on this machine type auto-attaches as
# virtio-blk-*pci* instead, which firmware boots from just as well but
# would need this kernel's own runtime driver to walk PCI/ECAM config
# space to find (a real subsystem on its own - see virtio_mmio.rs's
# module doc comment). virtio-mmio.force-legacy=false selects the modern
# (non-legacy) register interface - QEMU defaults virtio-mmio to legacy
# mode, confirmed via `-device virtio-mmio,help`'s printed default, kept
# only for old-guest compatibility this project has no need to imitate.
run: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Same as `run`, plus a virtio-net device on virtio-mmio with QEMU's
# user-mode (SLIRP) networking - the dev loop for the network stack
# (kernel/src/virtio_net.rs, docs/roadmap.md's Stage 1). SLIRP answers ARP
# for its gateway (10.0.2.2), which is what init_net's boot-time probe
# exercises. `-object filter-dump` writes every frame to net.pcap for
# independent host-side inspection (tcpdump/tshark), the same "verify against
# a source outside the kernel's own output" discipline used elsewhere.
run-net: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=net.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Same as `run`, plus a virtio-mmio console device (device ID 3, the
# virtio-console/virtio-serial class - see kernel/src/virtio_console.rs)
# attached via a separate chardev, for testing that driver. On this
# machine (default OVMF firmware, ACPI present), devicetree/ACPI/PCI
# console discovery always wins before virtio_console.rs ever gets a
# chance to run - this target only makes the *device* available, it
# doesn't organically force the fallback to trigger. To actually
# exercise it, temporarily force main.rs's `if !found_console_early`
# check to `if true` (see CLAUDE.md's virtio-console section for how
# this was originally verified, and why acpi=off doesn't work for this -
# it makes devicetree discovery succeed instead, a real surprise, not a
# path to "everything fails").
run-virtio-console: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-device virtio-serial-device \
		-device virtconsole,chardev=vcon0 \
		-chardev file,id=vcon0,path=vcon.log \
		-nographic

# Same as `run`, plus a real xHCI (USB3) host controller with a virtual
# USB HID keyboard attached (kernel/src/xhci.rs) and a QEMU HMP monitor
# socket for injecting keystrokes with `sendkey` (there's no way to type
# into an emulated USB keyboard via piped stdin the way the PL011 console
# tests inject bytes - this is genuinely different hardware). Unlike
# `run-virtio-console`, no source-level force is needed to exercise this:
# the xHCI driver isn't gated behind console discovery at all (see
# xhci.rs's module doc comment), so it always runs on this target.
run-usb-kbd: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-device qemu-xhci,id=xhci0 \
		-device usb-kbd,bus=xhci0.0 \
		-monitor unix:qemu-monitor.sock,server,nowait \
		-nographic

# Same as `run-usb-kbd`, plus two more USB devices on the same xHCI
# controller: a usb-tablet (a stand-in for Parallels' virtual mouse - a
# HID device that is *not* a keyboard) and a usb-storage stick backed by
# a small scratch image (created on demand) - the three-device rig for
# exercising xhci.rs's multi-device port scan. The storage device should
# show up in the boot log as a recognized-but-not-driven mass storage
# interface (class 0x08); the keyboard must still type normally via the
# monitor's `sendkey`.
# A real FAT32 image for the USB stick (not the old zeroed scratch file)
# so the mass-storage driver has something to actually mount - built the
# same hdiutil way as esp.img, with a marker file to ls/cat. 64MB: small
# hdiutil FAT32 requests can silently produce FAT16 (same class of
# surprise as make run's vvfat - see fat32.rs's module doc comment).
usbstick.img:
	rm -rf usbstick-src && mkdir usbstick-src
	printf 'hello from the USB stick\n' > usbstick-src/USBTEST.TXT
	hdiutil create -size 64m -fs FAT32 -volname USBSTICK -srcfolder usbstick-src -format UDTO -ov usbstick.cdr
	mv usbstick.cdr usbstick.img
	rm -rf usbstick-src

run-usb-multi: esp usbstick.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-device qemu-xhci,id=xhci0 \
		-device usb-kbd,bus=xhci0.0 \
		-device usb-tablet,bus=xhci0.0 \
		-drive file=usbstick.img,format=raw,if=none,id=usbstick \
		-device usb-storage,drive=usbstick,bus=xhci0.0 \
		-monitor unix:qemu-monitor.sock,server,nowait \
		-nographic

# Same as `run`, but forces QEMU's virt machine onto GICv3 instead of its
# default GICv2 (`-machine virt,help` confirms `gic-version` accepts
# 2/3/4/x-5/host/max - real, checked, not assumed). For exercising
# kernel/src/gicv3.rs and kernel/src/madt.rs's GICv3 discovery path without
# needing real Parallels hardware for every iteration - see CLAUDE.md's
# MADT/GICv3 scoping notes. Confirmed via a devicetree dump
# (`qemu-system-aarch64 -machine virt,gic-version=3,dumpdtb=...`) that this
# QEMU build describes GICv3 as one contiguous GICR region (GICD @
# 0x08000000, GICR @ 0x080a0000 size 0xf60000), not per-CPU GICC.GICR
# fields - madt.rs's parser confirmed independently to report the exact
# same addresses from the real ACPI MADT, not just the devicetree.
run-gicv3: esp
	qemu-system-aarch64 \
		-machine virt,gic-version=3 \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Builds a real MBR+FAT32 .img (a valid raw UEFI-bootable disk - verified by
# booting it directly in QEMU with -drive format=raw, not just the vvfat
# passthrough `run` uses). This is NOT directly attachable in Parallels: its
# Hard Disk device only accepts Parallels' own .hdd format. For Parallels,
# use `make parallels-hdd` instead, which wraps this image into one.
image: esp
	rm -f $(ESP_DIR).img
	hdiutil create -size 64m -fs FAT32 -volname OUROBOROS -srcfolder $(ESP_DIR) -format UDTO -ov $(ESP_DIR).cdr
	mv $(ESP_DIR).cdr $(ESP_DIR).img

# Boots the real esp.img (genuine FAT32) instead of `run`'s vvfat
# passthrough - needed for anything that reads the filesystem at runtime
# (fat32.rs and up), not just the fast kernel-dev loop `run` is for.
# **`run`'s vvfat is FAT16, not FAT32** - confirmed by decoding its BPB
# directly (BS_FilSysType literally reads "FAT16   ", and RootEntryCount/
# FATSz16 are both nonzero, which real FAT32 requires to be zero): QEMU's
# vvfat driver apparently can't produce FAT32 at all. esp.img (built by
# `hdiutil -fs FAT32`, what Parallels ultimately boots from too via
# `parallels-hdd`) is genuinely FAT32 - confirmed the same way, decoding
# its BPB directly with `xxd` before writing any parser code. Use this
# target, not `run`, whenever the on-disk filesystem format actually
# matters.
run-image: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Wraps esp.img into esp.hdd, a Parallels-native virtual hard disk, via
# prl_disk_tool's `--dmg` import (its only documented way to build a .hdd
# from an existing raw image; needs a real .dmg container, not the raw .img,
# and prl_disk_tool silently fails without absolute paths). Attach esp.hdd
# as the VM's Hard Disk device in Parallels - not esp.img, and not CD/DVD
# (that's for optical filesystems, and MBR+FAT32 isn't one).
#
# esp.hdd only stores a pointer to esp.dmg's absolute path, not a copy of
# its data - keep both files together, don't delete esp.dmg separately.
parallels-hdd: image
	rm -f $(ESP_DIR).dmg
	hdiutil convert $(ESP_DIR).img -format UDZO -o $(ESP_DIR).dmg
	rm -rf $(ESP_DIR).hdd
	"$(PDT)" create --hdd "$(CURDIR)/$(ESP_DIR).hdd" --dmg "$(CURDIR)/$(ESP_DIR).dmg"

# Scripted real-hardware test loop against Parallels - the manual "boot,
# watch the screen, type on a keyboard, report back" round trip every
# postmortem in docs/ paid wall-clock time for, now driven headlessly via
# prlctl (Parallels Desktop's own CLI - `man prlctl`): rebuilds esp.hdd,
# boots the registered VM named $(VM_NAME), types each ;-separated
# command in $(CMDS) through Parallels' own virtual keyboard
# (`prlctl send-key-event`, confirmed to land on the same
# xhci::poll_key interrupt-endpoint path a real physical USB keyboard
# does - see docs/xhci-keyboard-postmortem.md), and saves a screenshot
# after each step instead of needing a human watching live. See
# scripts/test-parallels.sh for the full mechanics.
#
# Needs the VM already registered in Parallels with its Hard Disk device
# pointed at this repo's esp.hdd (see `parallels-hdd`'s own doc comment
# above for how that gets built and attached in the first place - this
# target only rebuilds the disk image, it doesn't create/register a VM).
#
#   make test-parallels
#   make test-parallels CMDS="help;echo hi;uptime"
#   make test-parallels VM_NAME="Ouroboros" BOOT_WAIT=15
test-parallels:
	VM_NAME="$(VM_NAME)" CMDS="$(CMDS)" BOOT_WAIT="$(BOOT_WAIT)" ./scripts/test-parallels.sh

clean:
	cargo clean
	rm -rf $(ESP_DIR) $(ESP_DIR).img $(ESP_DIR).dmg $(ESP_DIR).hdd vcon.log qemu-monitor.sock usbstick.img usbstick.cdr

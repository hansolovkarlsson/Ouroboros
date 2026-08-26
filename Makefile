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
NETD_ELF     := target/$(USER_TARGET)/release/netd
NETD_BIN     := target/$(USER_TARGET)/release/netd.bin
ARGS_ELF     := target/$(USER_TARGET)/release/args
ARGS_BIN     := target/$(USER_TARGET)/release/args.bin
ECHO_ELF     := target/$(USER_TARGET)/release/echo
ECHO_BIN     := target/$(USER_TARGET)/release/echo.bin
UPTIME_ELF   := target/$(USER_TARGET)/release/uptime
UPTIME_BIN   := target/$(USER_TARGET)/release/uptime.bin
CLEAR_ELF    := target/$(USER_TARGET)/release/clear
CLEAR_BIN    := target/$(USER_TARGET)/release/clear.bin
LS_ELF       := target/$(USER_TARGET)/release/ls
LS_BIN       := target/$(USER_TARGET)/release/ls.bin
CAT_ELF      := target/$(USER_TARGET)/release/cat
CAT_BIN      := target/$(USER_TARGET)/release/cat.bin
MKDIR_ELF    := target/$(USER_TARGET)/release/mkdir
MKDIR_BIN    := target/$(USER_TARGET)/release/mkdir.bin
RMDIR_ELF    := target/$(USER_TARGET)/release/rmdir
RMDIR_BIN    := target/$(USER_TARGET)/release/rmdir.bin
TOUCH_ELF    := target/$(USER_TARGET)/release/touch
TOUCH_BIN    := target/$(USER_TARGET)/release/touch.bin
RM_ELF       := target/$(USER_TARGET)/release/rm
RM_BIN       := target/$(USER_TARGET)/release/rm.bin
CP_ELF       := target/$(USER_TARGET)/release/cp
CP_BIN       := target/$(USER_TARGET)/release/cp.bin
MV_ELF       := target/$(USER_TARGET)/release/mv
MV_BIN       := target/$(USER_TARGET)/release/mv.bin
WRITEAT_ELF  := target/$(USER_TARGET)/release/writeat
WRITEAT_BIN  := target/$(USER_TARGET)/release/writeat.bin
PING_ELF     := target/$(USER_TARGET)/release/ping
PING_BIN     := target/$(USER_TARGET)/release/ping.bin
WC_ELF       := target/$(USER_TARGET)/release/wc
WC_BIN       := target/$(USER_TARGET)/release/wc.bin
GREP_ELF     := target/$(USER_TARGET)/release/grep
GREP_BIN     := target/$(USER_TARGET)/release/grep.bin
HEAD_ELF     := target/$(USER_TARGET)/release/head
HEAD_BIN     := target/$(USER_TARGET)/release/head.bin
RESOLVE_ELF  := target/$(USER_TARGET)/release/resolve
RESOLVE_BIN  := target/$(USER_TARGET)/release/resolve.bin
FETCH_ELF    := target/$(USER_TARGET)/release/fetch
FETCH_BIN    := target/$(USER_TARGET)/release/fetch.bin
# All generated artifacts land under $(BUILD_DIR) so the repo root stays
# source-only. The cargo `target/` dir is separate (cargo owns it). Every
# path below is derived from BUILD_DIR, so pointing it elsewhere moves the
# whole lot. $(BUILD_DIR) is created lazily by the `esp` staging step
# (mkdir -p $(ESP_DIR)/...), which every image target depends on.
BUILD_DIR    := build
ESP_DIR      := $(BUILD_DIR)/esp
ESP_IMG      := $(ESP_DIR).img
ESP_HDD      := $(ESP_DIR).hdd
# The shared DEV cluster secret staged into the ESP as \CLUSTER.KEY (netd reads
# it at boot to authenticate the 9P export - the export-hardening phase). NOT a
# real secret; overridable on the command line for a mismatched-key test, e.g.
# `make run-image-2vm-b CLUSTER_KEY=wrong-key` to prove the export refuses.
CLUSTER_KEY  ?= ouroboros-dev-cluster-key-v1
GPT_IMG      := $(BUILD_DIR)/espgpt.img
EXFAT_IMG    := $(BUILD_DIR)/espexfat.img
EXFAT_PART   := $(BUILD_DIR)/exfatpart.img
EXT2_IMG     := $(BUILD_DIR)/espext2.img
EXT2_PART    := $(BUILD_DIR)/ext2part.img
USBSTICK_IMG := $(BUILD_DIR)/usbstick.img
NET_PCAP     := $(BUILD_DIR)/net.pcap
# mke2fs (ext2 image builder) from Homebrew's keg-only e2fsprogs - macOS has no
# native ext2 tooling. `brew install e2fsprogs` provides it. Used only by the
# ext2part.img target (fsd/src/ext2.rs testing).
MKE2FS       := $(shell brew --prefix e2fsprogs 2>/dev/null)/sbin/mke2fs
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

.PHONY: build shell-bin hello-bin pong-bin fsd-bin upper-bin cond-bin netd-bin args-bin echo-bin uptime-bin clear-bin ls-bin cat-bin mkdir-bin rmdir-bin touch-bin rm-bin cp-bin mv-bin writeat-bin ping-bin resolve-bin fetch-bin wc-bin grep-bin head-bin esp run run-virtio-console run-usb-kbd run-usb-multi run-gicv3 image run-image run-image-9p run-image-9p-client run-image-2vm-a run-image-2vm-b image-gpt run-image-gpt image-exfat run-image-exfat image-ext2 run-image-ext2 parallels-hdd release test-parallels clean

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

# The network server (netd/) - loaded into reserved task slot 4, same shape
# as fsd (slot 2) and cond (slot 3).
netd-bin:
	cargo build -p netd --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(NETD_ELF) $(NETD_BIN)

# The argv proof program (args/) - spawned via `exec`, prints its argument
# vector. Same recipe as every other userland program.
args-bin:
	cargo build -p args --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(ARGS_ELF) $(ARGS_BIN)

# Externalized commands (standalone-binaries Stage 4): former shell builtins,
# now real /bin programs sharing the `ulib` support crate. Same recipe as
# every other userland program.
echo-bin:
	cargo build -p echo --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(ECHO_ELF) $(ECHO_BIN)

uptime-bin:
	cargo build -p uptime --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(UPTIME_ELF) $(UPTIME_BIN)

clear-bin:
	cargo build -p clear --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CLEAR_ELF) $(CLEAR_BIN)

ls-bin:
	cargo build -p ls --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(LS_ELF) $(LS_BIN)

cat-bin:
	cargo build -p cat --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CAT_ELF) $(CAT_BIN)

mkdir-bin:
	cargo build -p mkdir --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(MKDIR_ELF) $(MKDIR_BIN)

rmdir-bin:
	cargo build -p rmdir --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RMDIR_ELF) $(RMDIR_BIN)

touch-bin:
	cargo build -p touch --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(TOUCH_ELF) $(TOUCH_BIN)

rm-bin:
	cargo build -p rm --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RM_ELF) $(RM_BIN)

cp-bin:
	cargo build -p cp --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(CP_ELF) $(CP_BIN)

mv-bin:
	cargo build -p mv --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(MV_ELF) $(MV_BIN)

writeat-bin:
	cargo build -p writeat --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(WRITEAT_ELF) $(WRITEAT_BIN)

ping-bin:
	cargo build -p ping --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(PING_ELF) $(PING_BIN)

wc-bin:
	cargo build -p wc --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(WC_ELF) $(WC_BIN)

grep-bin:
	cargo build -p grep --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(GREP_ELF) $(GREP_BIN)

head-bin:
	cargo build -p head --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(HEAD_ELF) $(HEAD_BIN)

resolve-bin:
	cargo build -p resolve --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(RESOLVE_ELF) $(RESOLVE_BIN)

fetch-bin:
	cargo build -p fetch --target $(USER_TARGET) --release
	"$(OBJCOPY)" --strip-all $(FETCH_ELF) $(FETCH_BIN)

# Stage the EFI System Partition layout QEMU/Parallels expect: a removable
# UEFI drive boots \EFI\BOOT\BOOTAA64.EFI automatically, no boot manager
# entry needed. \EFI\ORBS\ (must fit FAT's 8.3 short-name limit, which
# the full 9-character project name doesn't - see loader.rs's
# CONFIG_PATH doc comment for why) holds everything that isn't the kernel
# itself: the default shell binary and the config file (loader.rs's
# CONFIG_PATH) naming which program to load - edit INIT.CFG and rebuild
# just that program to swap it out, no kernel rebuild required.
esp: build shell-bin hello-bin pong-bin fsd-bin upper-bin cond-bin netd-bin args-bin echo-bin uptime-bin clear-bin ls-bin cat-bin mkdir-bin rmdir-bin touch-bin rm-bin cp-bin mv-bin writeat-bin ping-bin resolve-bin fetch-bin wc-bin grep-bin head-bin
	mkdir -p $(ESP_DIR)/EFI/BOOT $(ESP_DIR)/EFI/ORBS $(ESP_DIR)/bin
	cp $(KERNEL) $(ESP_DIR)/EFI/BOOT/BOOTAA64.EFI
	cp $(SHELL_BIN) $(ESP_DIR)/EFI/ORBS/SH.BIN
	cp $(HELLO_BIN) $(ESP_DIR)/EFI/ORBS/HELLO.BIN
	cp $(PONG_BIN) $(ESP_DIR)/EFI/ORBS/PONG.BIN
	cp $(FSD_BIN) $(ESP_DIR)/EFI/ORBS/FSD.BIN
	cp $(UPPER_BIN) $(ESP_DIR)/EFI/ORBS/UPPER.BIN
	cp $(UPPER_BIN) $(ESP_DIR)/bin/UPPER
	cp $(COND_BIN) $(ESP_DIR)/EFI/ORBS/COND.BIN
	cp $(NETD_BIN) $(ESP_DIR)/EFI/ORBS/NETD.BIN
	cp $(ARGS_BIN) $(ESP_DIR)/EFI/ORBS/ARGS.BIN
	printf '\\EFI\\ORBS\\SH.BIN' > $(ESP_DIR)/EFI/ORBS/INIT.CFG
	# The cluster secret netd reads at boot to authenticate the 9P export
	# (the export-hardening phase). A fixed DEV key, shared by every machine
	# built from this tree, so two-VM runs (2vm-a/b, both derived from esp.img)
	# authenticate each other with no per-machine config. NOT a real secret -
	# it lives in the repo; a deployment would use its own. Fail-closed: with no
	# CLUSTER.KEY, netd refuses all remote clients. The host peers
	# (scripts/np9p_*.py) read this same value. Kept in sync with CLUSTER_KEY.
	printf '%s' '$(CLUSTER_KEY)' > $(ESP_DIR)/CLUSTER.KEY
	# /bin: programs the shell finds via PATH by bare name (Stage 2 of the
	# standalone-binaries arc). Named uppercase, no extension (8.3-legal);
	# fsd's case-insensitive lookup matches a lowercase-typed command. For
	# now the argv proof program plus the first externalized commands
	# (echo/uptime/clear - Stage 4), each a real program sharing `ulib`.
	cp $(ARGS_BIN) $(ESP_DIR)/bin/ARGS
	cp $(ECHO_BIN) $(ESP_DIR)/bin/ECHO
	cp $(UPTIME_BIN) $(ESP_DIR)/bin/UPTIME
	cp $(CLEAR_BIN) $(ESP_DIR)/bin/CLEAR
	cp $(LS_BIN) $(ESP_DIR)/bin/LS
	cp $(CAT_BIN) $(ESP_DIR)/bin/CAT
	cp $(MKDIR_BIN) $(ESP_DIR)/bin/MKDIR
	cp $(RMDIR_BIN) $(ESP_DIR)/bin/RMDIR
	cp $(TOUCH_BIN) $(ESP_DIR)/bin/TOUCH
	cp $(RM_BIN) $(ESP_DIR)/bin/RM
	cp $(CP_BIN) $(ESP_DIR)/bin/CP
	cp $(MV_BIN) $(ESP_DIR)/bin/MV
	cp $(WRITEAT_BIN) $(ESP_DIR)/bin/WRITEAT
	cp $(PING_BIN) $(ESP_DIR)/bin/PING
	cp $(RESOLVE_BIN) $(ESP_DIR)/bin/RESOLVE
	cp $(FETCH_BIN) $(ESP_DIR)/bin/FETCH
	cp $(WC_BIN) $(ESP_DIR)/bin/WC
	cp $(GREP_BIN) $(ESP_DIR)/bin/GREP
	cp $(HEAD_BIN) $(ESP_DIR)/bin/HEAD

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
# exercises. `-object filter-dump` writes every frame to $(NET_PCAP) for
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
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
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
		-chardev file,id=vcon0,path=$(BUILD_DIR)/vcon.log \
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
		-monitor unix:$(BUILD_DIR)/qemu-monitor.sock,server,nowait \
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
# same hdiutil way as build/esp.img, with a marker file to ls/cat. 64MB: small
# hdiutil FAT32 requests can silently produce FAT16 (same class of
# surprise as make run's vvfat - see fat32.rs's module doc comment).
$(USBSTICK_IMG):
	mkdir -p $(BUILD_DIR)
	rm -rf $(BUILD_DIR)/usbstick-src && mkdir $(BUILD_DIR)/usbstick-src
	printf 'hello from the USB stick\n' > $(BUILD_DIR)/usbstick-src/USBTEST.TXT
	hdiutil create -size 64m -fs FAT32 -volname USBSTICK -srcfolder $(BUILD_DIR)/usbstick-src -format UDTO -ov $(BUILD_DIR)/usbstick.cdr
	mv $(BUILD_DIR)/usbstick.cdr $(USBSTICK_IMG)
	rm -rf $(BUILD_DIR)/usbstick-src

run-usb-multi: esp $(USBSTICK_IMG)
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
		-drive file=$(USBSTICK_IMG),format=raw,if=none,id=usbstick \
		-device usb-storage,drive=usbstick,bus=xhci0.0 \
		-monitor unix:$(BUILD_DIR)/qemu-monitor.sock,server,nowait \
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
	# Strip the macOS AppleDouble sidecars (._* files) hdiutil sprinkles
	# onto the FAT volume during the copy - FAT can't hold extended
	# attributes, so macOS spills them into these files, which then show up
	# in the shell's `ls` as mangled 8.3 aliases (_EFI~1, etc.) since the
	# FAT32 reader doesn't decode long filenames. Attach the flat image
	# read-write, delete them, detach.
	MP=$$(mktemp -d); \
	hdiutil attach -nobrowse -mountpoint "$$MP" $(ESP_DIR).img >/dev/null; \
	find "$$MP" -name '._*' -delete; \
	hdiutil detach "$$MP" >/dev/null; \
	rmdir "$$MP"

# Boots the real build/esp.img (genuine FAT32) instead of `run`'s vvfat
# passthrough - needed for anything that reads the filesystem at runtime
# (fat32.rs and up), not just the fast kernel-dev loop `run` is for.
# **`run`'s vvfat is FAT16, not FAT32** - confirmed by decoding its BPB
# directly (BS_FilSysType literally reads "FAT16   ", and RootEntryCount/
# FATSz16 are both nonzero, which real FAT32 requires to be zero): QEMU's
# vvfat driver apparently can't produce FAT32 at all. build/esp.img (built by
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

# $(GPT_IMG): build/esp.img's FAT32 partition wrapped in a *GPT* disk (protective
# MBR + primary/backup GPT headers, an EFI-System-Partition entry) - built by
# scripts/mkgpt.py because macOS has no GPT tooling and hdiutil only makes MBR.
# For testing fsd's GPT partition discovery (the more-filesystems arc).
image-gpt: image
	python3 scripts/mkgpt.py

# Boot the GPT disk instead of the MBR build/esp.img: UEFI boots BOOTAA64 from the
# ESP, and fsd discovers the FAT32 partition through the GPT path (the disk has
# no real MBR partition table, only a protective 0xEE entry).
run-image-gpt: image-gpt
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(GPT_IMG),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-nographic

# $(EXFAT_PART): a raw exFAT filesystem holding /bin (the externalized commands,
# so the shell can run them off exFAT) plus a few test files, built with macOS's
# newfs_exfat (hdiutil can't make exFAT). This is the payload scripts/mkexfat.py
# drops into the exFAT partition of the combined disk. For testing the exFAT
# reader (fsd/src/exfat.rs, the more-filesystems arc's read-only exFAT step).
$(EXFAT_PART): esp
	rm -rf $(BUILD_DIR)/exfat-src && mkdir -p $(BUILD_DIR)/exfat-src/bin
	cp $(ESP_DIR)/bin/* $(BUILD_DIR)/exfat-src/bin/
	printf 'hello from an exFAT volume\r\n' > $(BUILD_DIR)/exfat-src/HELLO.TXT
	printf 'line one\r\nline two has several words\r\nthird and final line\r\n' > $(BUILD_DIR)/exfat-src/README.TXT
	mkdir -p $(BUILD_DIR)/exfat-src/SUB
	printf 'a nested file on exFAT\r\n' > $(BUILD_DIR)/exfat-src/SUB/NESTED.TXT
	printf 'this exercises a long UTF-16 name\r\n' > "$(BUILD_DIR)/exfat-src/a-long-exfat-name.txt"
	rm -f $(EXFAT_PART)
	dd if=/dev/zero of=$(EXFAT_PART) bs=1m count=24 2>/dev/null
	DEV=$$(hdiutil attach -nomount -imagekey diskimage-class=CRawDiskImage $(EXFAT_PART) | head -1 | awk '{print $$1}'); \
	newfs_exfat -v OUROEXFAT "$$DEV" >/dev/null; \
	diskutil mount "$$DEV" >/dev/null; \
	MP=$$(diskutil info "$$DEV" | awk -F': *' '/Mount Point/{print $$2}'); \
	cp -R $(BUILD_DIR)/exfat-src/. "$$MP/"; \
	find "$$MP" -name '._*' -delete; rm -rf "$$MP/.fseventsd" "$$MP/.Trashes" "$$MP/.Spotlight-V100"; \
	diskutil unmount "$$DEV" >/dev/null; hdiutil detach "$$DEV" >/dev/null
	rm -rf $(BUILD_DIR)/exfat-src

# $(EXFAT_IMG): a two-partition MBR disk - partition 1 exFAT (fsd mounts it),
# partition 2 the FAT32 ESP (UEFI boots it). See scripts/mkexfat.py for why the
# exFAT partition must come first. Exercises fsd's Filesystem-enum fallthrough
# (FAT32 probe fails on the exFAT partition, exFAT probe succeeds).
image-exfat: image $(EXFAT_PART)
	python3 scripts/mkexfat.py

# Boot the combined disk: UEFI boots BOOTAA64 from the FAT32 ESP (partition 2),
# then fsd mounts the exFAT partition (partition 1) - so `ls`/`cat`/pipelines
# all read from exFAT (their /bin binaries live there too).
run-image-exfat: image-exfat
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(EXFAT_IMG),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-nographic

# $(EXT2_PART): a raw ext2 filesystem holding /bin (so the shell runs off ext2)
# plus test files, built with Homebrew e2fsprogs' mke2fs (macOS has no native
# ext2 tooling). Block size forced to 1024 so a >12 KiB file (the /bin/CAT
# binary) spills past the 12 direct block pointers into single-indirect blocks -
# exercising fsd/src/ext2.rs's indirection. The payload scripts/mkext2.py drops
# into the ext2 partition of the combined disk.
$(EXT2_PART): esp
	@test -x "$(MKE2FS)" || { echo "mke2fs not found - run: brew install e2fsprogs"; exit 1; }
	mkdir -p $(BUILD_DIR)
	rm -rf $(BUILD_DIR)/ext2-src && mkdir -p $(BUILD_DIR)/ext2-src/bin
	# ext2 is case-sensitive (Unix), and the shell probes /bin/<command> as
	# typed (lowercase), so /bin gets lowercase names here - unlike the FAT/
	# exFAT images, whose 8.3-heritage uppercase names only work because those
	# filesystems match case-insensitively.
	for f in $(ESP_DIR)/bin/*; do cp "$$f" "$(BUILD_DIR)/ext2-src/bin/$$(basename "$$f" | tr A-Z a-z)"; done
	printf 'hello from an ext2 volume\n' > $(BUILD_DIR)/ext2-src/HELLO.TXT
	printf 'line one\nline two has several words\nthird and final line\n' > $(BUILD_DIR)/ext2-src/README.TXT
	mkdir -p $(BUILD_DIR)/ext2-src/sub
	printf 'a nested file on ext2\n' > $(BUILD_DIR)/ext2-src/sub/NESTED.TXT
	printf 'ext2 is case-sensitive, unlike FAT\n' > $(BUILD_DIR)/ext2-src/CaseSensitive.txt
	rm -f $(EXT2_PART)
	"$(MKE2FS)" -q -t ext2 -b 1024 -d $(BUILD_DIR)/ext2-src -F $(EXT2_PART) 24m
	rm -rf $(BUILD_DIR)/ext2-src

# build/espext2.img: a two-partition MBR disk - partition 1 ext2 (fsd mounts it),
# partition 2 the FAT32 ESP (UEFI boots it). See scripts/mkext2.py. Exercises the
# Filesystem-enum probe reaching its third arm (FAT32 + exFAT probes both fail on
# the ext2 partition, ext2 succeeds).
image-ext2: image $(EXT2_PART)
	python3 scripts/mkext2.py

# Boot the combined disk: UEFI boots BOOTAA64 from the FAT32 ESP (partition 2),
# then fsd mounts the ext2 partition (partition 1) read-only - so `ls`/`cat`/
# pipelines read from ext2 (their /bin binaries live there too).
run-image-ext2: image-ext2
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(EXT2_IMG),format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-global virtio-mmio.force-legacy=false \
		-nographic

# `run-image` (real FAT32, so disk commands work) *and* the NIC from
# `run-net` in one boot - the fullest QEMU run: the filesystem server mounts,
# the shell/disk commands work, and init_net's boot-time ARP probe exercises
# the network. Every frame is dumped to $(NET_PCAP) for host-side inspection.
run-image-net: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Like `run-image-net`, but forwards host port 5555 to the guest's TCP port 80
# (SLIRP hostfwd), so `netd`'s HTTP server (the guest answering the network) is
# reachable from the host: boot this, then on the host run
#   curl http://localhost:5555/
# and the from-scratch TCP stack serves its page. Every frame still goes to
# $(NET_PCAP) for host-side inspection.
run-image-server: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0,hostfwd=tcp::5555-:80 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# run-image-server plus a second hostfwd for netd's 9P-export listener (port
# 564): the cluster Phase 1 export gateway (docs/roadmap-cluster-phase1.md). A
# host 9P client (scripts/np9p_client.py) reaches the guest's exported fsd via
#   python3 scripts/np9p_client.py localhost 5640 readdir /
# reading the guest's disk over TCP. curl http://localhost:5555/ still works.
run-image-9p: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0,hostfwd=tcp::5555-:80,hostfwd=tcp::5640-:564 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# The remote-mount *client* side of cluster Phase 1 (step 1c): a NIC + a real
# FAT32 disk (so /bin's ls/cat load) and nothing else - the guest reaches a
# host-run 9P server at 10.0.2.2 over SLIRP with no hostfwd needed (SLIRP routes
# guest->host automatically). On the host, first start the test server:
#   python3 scripts/np9p_server.py 5641
# then in the guest:
#   mount -r 10.0.2.2:5641 /mnt/a
#   ls /mnt/a ; cat /mnt/a/HELLO.TXT
# reading the *host's* filesystem over TCP - the "aha", one VM, host as server.
run-image-9p-client: image
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR).img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev user,id=net0 \
		-device virtio-net-device,netdev=net0 \
		-object filter-dump,id=f0,netdev=net0,file=$(NET_PCAP) \
		-global virtio-mmio.force-legacy=false \
		-nographic

# The two-VM integration - the cluster Phase 1 "aha" (step 1d): two guests share
# an L2 link via QEMU socket networking (a virtual hub, no SLIRP), each with a
# distinct MAC that netd derives a distinct static IP from (:0a -> 10.0.2.10,
# :0b -> 10.0.2.11). Machine A *exports* its disk (netd's port-564 listener is
# always on); machine B *remote-mounts* A over the shared link. Run in two
# terminals - **A first** (it listens; B's socket connect fails if nothing is):
#   Terminal 1:  make run-image-2vm-a
#   Terminal 2:  make run-image-2vm-b
# then in B's shell:
#   mount -r 10.0.2.10:564 /mnt/a
#   ls /mnt/a            # A's disk: BIN/ EFI/
#   cat /mnt/a/EFI/ORBS/INIT.CFG
# Each VM gets its own disk copy (two QEMU write-locks can't share one file) and
# its own pcap (build/net-a.pcap / build/net-b.pcap) for tcpdump inspection.
run-image-2vm-a: image
	cp $(ESP_DIR).img $(ESP_DIR)-a.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR)-a.img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev socket,id=net0,listen=127.0.0.1:12340 \
		-device virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a \
		-object filter-dump,id=f0,netdev=net0,file=$(BUILD_DIR)/net-a.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

run-image-2vm-b: image
	cp $(ESP_DIR).img $(ESP_DIR)-b.img
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=$(ESP_DIR)-b.img,format=raw,if=none,id=hd0 \
		-device virtio-blk-device,drive=hd0 \
		-netdev socket,id=net0,connect=127.0.0.1:12340 \
		-device virtio-net-device,netdev=net0,mac=52:54:00:12:34:0b \
		-object filter-dump,id=f0,netdev=net0,file=$(BUILD_DIR)/net-b.pcap \
		-global virtio-mmio.force-legacy=false \
		-nographic

# Wraps build/esp.img into build/esp.hdd, a Parallels-native virtual hard disk, via
# prl_disk_tool's `--dmg` import (its only documented way to build a .hdd
# from an existing raw image; needs a real .dmg container, not the raw .img,
# and prl_disk_tool silently fails without absolute paths). Attach build/esp.hdd
# as the VM's Hard Disk device in Parallels - not build/esp.img, and not CD/DVD
# (that's for optical filesystems, and MBR+FAT32 isn't one).
#
# build/esp.hdd only stores a pointer to build/esp.dmg's absolute path, not a copy of
# its data - keep both files together, don't delete build/esp.dmg separately.
parallels-hdd: image
	rm -f $(ESP_DIR).dmg
	hdiutil convert $(ESP_DIR).img -format UDZO -o $(ESP_DIR).dmg
	rm -rf $(ESP_DIR).hdd
	"$(PDT)" create --hdd "$(CURDIR)/$(ESP_DIR).hdd" --dmg "$(CURDIR)/$(ESP_DIR).dmg"

# Cut a release: build the release-profile disk images and package the
# downloadable artifacts (esp.img.zip + esp.hdd.zip + SHA256SUMS) under
# build/release/. Local and repeatable. The outward-facing publish step
# (tag + push + GitHub Release) is deliberately NOT a make target - run
# `scripts/release.sh publish` by hand. Version comes from ./VERSION.
# See docs/RELEASING.md.
release:
	scripts/release.sh build

# Scripted real-hardware test loop against Parallels - the manual "boot,
# watch the screen, type on a keyboard, report back" round trip every
# postmortem in docs/ paid wall-clock time for, now driven headlessly via
# prlctl (Parallels Desktop's own CLI - `man prlctl`): rebuilds build/esp.hdd,
# boots the registered VM named $(VM_NAME), types each ;-separated
# command in $(CMDS) through Parallels' own virtual keyboard
# (`prlctl send-key-event`, confirmed to land on the same
# xhci::poll_key interrupt-endpoint path a real physical USB keyboard
# does - see docs/xhci-keyboard-postmortem.md), and saves a screenshot
# after each step instead of needing a human watching live. See
# scripts/test-parallels.sh for the full mechanics.
#
# Needs the VM already registered in Parallels with its Hard Disk device
# pointed at this repo's build/esp.hdd (see `parallels-hdd`'s own doc comment
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
	rm -rf $(BUILD_DIR)

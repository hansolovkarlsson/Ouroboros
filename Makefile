TARGET   := aarch64-unknown-uefi
PROFILE  ?= debug
KERNEL   := target/$(TARGET)/$(PROFILE)/BOOTAA64.efi
ESP_DIR  := esp
OVMF     := $(shell brew --prefix qemu 2>/dev/null)/share/qemu/edk2-aarch64-code.fd
PDT      := /Applications/Parallels Desktop.app/Contents/MacOS/prl_disk_tool

CARGO_FLAGS :=
ifeq ($(PROFILE),release)
CARGO_FLAGS += --release
endif

.PHONY: build esp run image parallels-hdd clean

build:
	cargo build $(CARGO_FLAGS)

# Stage the EFI System Partition layout QEMU/Parallels expect: a removable
# UEFI drive boots \EFI\BOOT\BOOTAA64.EFI automatically, no boot manager entry needed.
esp: build
	mkdir -p $(ESP_DIR)/EFI/BOOT
	cp $(KERNEL) $(ESP_DIR)/EFI/BOOT/BOOTAA64.EFI

# Boots the ESP directory directly in QEMU (no disk image needed) against
# the aarch64 OVMF firmware installed by `brew install qemu`.
run: esp
	qemu-system-aarch64 \
		-machine virt \
		-cpu cortex-a72 \
		-m 512M \
		-bios $(OVMF) \
		-drive file=fat:rw:$(ESP_DIR),format=raw,media=disk \
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

clean:
	cargo clean
	rm -rf $(ESP_DIR) $(ESP_DIR).img $(ESP_DIR).dmg $(ESP_DIR).hdd

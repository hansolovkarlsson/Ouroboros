TARGET   := aarch64-unknown-uefi
PROFILE  ?= debug
KERNEL   := target/$(TARGET)/$(PROFILE)/BOOTAA64.efi
ESP_DIR  := esp
OVMF     := $(shell brew --prefix qemu 2>/dev/null)/share/qemu/edk2-aarch64-code.fd

CARGO_FLAGS :=
ifeq ($(PROFILE),release)
CARGO_FLAGS += --release
endif

.PHONY: build esp run image clean

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

# Builds a real FAT32 .img for use as a Parallels virtual disk.
image: esp
	rm -f $(ESP_DIR).img
	hdiutil create -size 64m -fs FAT32 -volname OUROBOROS -srcfolder $(ESP_DIR) -format UDTO -ov $(ESP_DIR).cdr
	mv $(ESP_DIR).cdr $(ESP_DIR).img

clean:
	cargo clean
	rm -rf $(ESP_DIR) $(ESP_DIR).img

#!/usr/bin/env python3
"""Assemble build/disk.img: one disk carrying the boot partition and the root,
the ESP FIRST (one-disk step 4, docs/roadmap/roadmap-one-disk.md).

    partition 1  FAT32 ESP, type 0x0c, bootable: what the firmware boots, and
                 on the Raspberry Pi what its boot ROM reads, which takes
                 only partition 1
    partition 2  ext2 root, type 0x83: what fsd mounts as `/`, the one
                 filesystem where it enforces permissions (decision D1, Hans,
                 2026-10-13)

The opposite order from scripts/mkext2.py's espext2.img, which put the root
first because fsd mounted the first partition it could. Since step 4 the
kernel tells fsd which partition it booted from (BLOCK_BOOT_START) and fsd
passes it over while another mounts, so the conventional order works.

Inputs: build/esp.img (`make image`) for the FAT32 payload and
build/ext2part.img (its Makefile recipe, mke2fs) for the root. The disk is
signed (scripts/disksig.py) before this script returns, so it cannot write
an unsigned one. Run from the repo root; `make disk` runs it.
"""
import os
import struct
import sys

import disksig

ESP, ROOT, DST, SECT = "build/esp.img", "build/ext2part.img", "build/disk.img", 512

for f in (ESP, ROOT):
    if not os.path.exists(f):
        sys.exit(f"{f} not found - run `make image` and the ext2part.img recipe first")

# The FAT32 payload out of esp.img's own MBR, so it tracks `make image`.
esp = open(ESP, "rb").read()
fat_start = fat_size = None
for i in range(4):
    e = 0x1be + i * 16
    if esp[e + 4] != 0:
        fat_start = struct.unpack("<I", esp[e + 8:e + 12])[0]
        fat_size = struct.unpack("<I", esp[e + 12:e + 16])[0]
        break
if fat_start is None:
    sys.exit(f"no MBR partition found in {ESP}")
fat = esp[fat_start * SECT:(fat_start + fat_size) * SECT]

# The ext2 filesystem is the whole raw image; its offsets are relative to the
# partition start, which fsd/src/ext2.rs adds, so it drops in as it is.
root = open(ROOT, "rb").read()
if struct.unpack("<H", root[1080:1082])[0] != 0xEF53:
    sys.exit(f"{ROOT} is not an ext2 filesystem (no 0xEF53 magic)")
root_sectors = (len(root) + SECT - 1) // SECT


def align_up(lba, to=2048):  # 1 MiB alignment
    return (lba + to - 1) // to * to


P1_START = 2048                          # FAT32 ESP
P2_START = align_up(P1_START + fat_size)  # ext2 root
disk_sectors = align_up(P2_START + root_sectors)
disk = bytearray(disk_sectors * SECT)


def mbr_entry(buf, idx, ptype, start, size, boot=0x00):
    e = 0x1be + idx * 16
    buf[e] = boot
    buf[e + 1:e + 4] = bytes([0xff, 0xff, 0xff])   # CHS start (LBA-only reader)
    buf[e + 4] = ptype
    buf[e + 5:e + 8] = bytes([0xff, 0xff, 0xff])   # CHS end
    buf[e + 8:e + 12] = struct.pack("<I", start)
    buf[e + 12:e + 16] = struct.pack("<I", size)


mbr_entry(disk, 0, 0x0c, P1_START, fat_size, boot=0x80)   # FAT32 LBA (bootable ESP)
mbr_entry(disk, 1, 0x83, P2_START, root_sectors)          # Linux (ext2 root)
disk[510], disk[511] = 0x55, 0xAA

disk[P1_START * SECT:P1_START * SECT + len(fat)] = fat
disk[P2_START * SECT:P2_START * SECT + len(root)] = root

with open(DST, "wb") as f:
    f.write(disk)
print(f"disksig: {DST}: {disksig.set_signature(DST):#010x}")
print(f"wrote {DST}: {disk_sectors} sectors "
      f"(FAT32 ESP @ LBA {P1_START}, {fat_size} sectors; "
      f"ext2 root @ LBA {P2_START}, {root_sectors} sectors)")

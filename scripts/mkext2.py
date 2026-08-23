#!/usr/bin/env python3
"""Assemble build/espext2.img: a two-partition MBR disk whose *first* partition
is ext2 (for fsd to mount) and whose *second* is the FAT32 ESP (for UEFI to
boot). Same trick as scripts/mkexfat.py - UEFI can only boot FAT, and fsd mounts
the first partition it can, so the ext2 partition comes first (fsd probes it
FAT32-then-exFAT-then-ext2, the first two fail, ext2 succeeds) and the FAT32 ESP
comes second (UEFI ignores the ext2 partition it can't read and boots BOOTAA64).

Inputs (build them first): build/esp.img (`make image`) for the FAT32 payload,
and build/ext2part.img (the ext2part.img Makefile recipe, built with mke2fs).
Run from the repo root.
"""
import struct, os, sys

ESP, EXT2, DST, SECT = "build/esp.img", "build/ext2part.img", "build/espext2.img", 512

for f in (ESP, EXT2):
    if not os.path.exists(f):
        sys.exit(f"{f} not found - run `make image` and the ext2part.img recipe first")

# FAT32 payload out of esp.img's own MBR (tracks `make image`).
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

# The ext2 filesystem is the whole raw image (superblock 1024 bytes in). Its
# block/group offsets are all relative to the partition start, which
# fsd/src/ext2.rs adds part_lba to - so it drops into a partition as-is.
ext2 = open(EXT2, "rb").read()
if struct.unpack("<H", ext2[1080:1082])[0] != 0xEF53:  # s_magic at superblock+56
    sys.exit(f"{EXT2} is not an ext2 filesystem (no 0xEF53 magic)")
ext2_sectors = (len(ext2) + SECT - 1) // SECT


def align_up(lba, to=2048):  # 1 MiB alignment
    return (lba + to - 1) // to * to


P1_START = 2048                                  # ext2
P1_END = P1_START + ext2_sectors - 1
P2_START = align_up(P1_END + 1)                  # FAT32 ESP
P2_END = P2_START + fat_size - 1
disk_sectors = align_up(P2_END + 1)
disk = bytearray(disk_sectors * SECT)


def mbr_entry(buf, idx, ptype, start, size, boot=0x00):
    e = 0x1be + idx * 16
    buf[e] = boot
    buf[e + 1:e + 4] = bytes([0xff, 0xff, 0xff])   # CHS start (LBA-only reader)
    buf[e + 4] = ptype
    buf[e + 5:e + 8] = bytes([0xff, 0xff, 0xff])   # CHS end
    buf[e + 8:e + 12] = struct.pack("<I", start)
    buf[e + 12:e + 16] = struct.pack("<I", size)


mbr_entry(disk, 0, 0x83, P1_START, ext2_sectors)          # Linux (ext2)
mbr_entry(disk, 1, 0x0b, P2_START, fat_size, boot=0x80)   # FAT32 (bootable ESP)
disk[510], disk[511] = 0x55, 0xAA

disk[P1_START * SECT:P1_START * SECT + len(ext2)] = ext2
disk[P2_START * SECT:P2_START * SECT + len(fat)] = fat

with open(DST, "wb") as f:
    f.write(disk)
print(f"wrote {DST}: {disk_sectors} sectors "
      f"(ext2 @ LBA {P1_START}, {ext2_sectors} sectors; "
      f"FAT32 ESP @ LBA {P2_START}, {fat_size} sectors)")

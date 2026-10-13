#!/usr/bin/env python3
"""Assemble build/espext2.img: a two-partition MBR disk whose *first* partition
is ext2 (for fsd to mount) and whose *second* is the FAT32 ESP (for UEFI to
boot). Same trick as scripts/mkexfat.py - UEFI can only boot FAT, and fsd mounts
the first partition it can, so the ext2 partition comes first (fsd probes it
FAT32-then-exFAT-then-ext2, the first two fail, ext2 succeeds) and the FAT32 ESP
comes second (UEFI ignores the ext2 partition it can't read and boots BOOTAA64).

Inputs (build them first): build/esp.img (`make image`) for the FAT32 payload,
and build/ext2part.img (the ext2part.img Makefile recipe, built with mke2fs).
Run from the repo root. The disk is assembled and signed by scripts/mbrdisk.py.
Since one-disk step 4 fsd tries the boot partition last, so this order is no
longer needed; it is kept so the image tests what it always did, and
build/disk.img (scripts/mkdisk.py) is the ESP-first layout.
"""
import os
import sys

import mbrdisk

ESP, EXT2, DST = "build/esp.img", "build/ext2part.img", "build/espext2.img"

for f in (ESP, EXT2):
    if not os.path.exists(f):
        sys.exit(f"{f} not found - run `make image` and the ext2part.img recipe first")

layout = mbrdisk.assemble(DST, [
    (0x83, mbrdisk.ext2_payload(EXT2), False),  # Linux (ext2), first so fsd took it before step 4
    (0x0B, mbrdisk.esp_payload(ESP), True),     # FAT32, the bootable ESP
])
(ext2_start, ext2_sectors), (esp_start, esp_sectors) = layout
print(f"wrote {DST}: ext2 @ LBA {ext2_start}, {ext2_sectors} sectors; "
      f"FAT32 ESP @ LBA {esp_start}, {esp_sectors} sectors")

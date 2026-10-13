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
import sys

import mbrdisk

ESP, ROOT, DST = "build/esp.img", "build/ext2part.img", "build/disk.img"

for f in (ESP, ROOT):
    if not os.path.exists(f):
        sys.exit(f"{f} not found - run `make image` and the ext2part.img recipe first")

layout = mbrdisk.assemble(DST, [
    (0x0C, mbrdisk.esp_payload(ESP), True),     # FAT32 LBA, the bootable ESP
    (0x83, mbrdisk.ext2_payload(ROOT), False),  # Linux, the ext2 root
])
(esp_start, esp_sectors), (root_start, root_sectors) = layout
print(f"wrote {DST}: FAT32 ESP @ LBA {esp_start}, {esp_sectors} sectors; "
      f"ext2 root @ LBA {root_start}, {root_sectors} sectors")

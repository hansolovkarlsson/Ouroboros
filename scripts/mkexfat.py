#!/usr/bin/env python3
"""Assemble espexfat.img: a two-partition MBR disk whose *first* partition is
exFAT (for fsd to mount) and whose *second* is the FAT32 ESP (for UEFI to boot).

This is how the exFAT reader (fsd/src/exfat.rs, the more-filesystems arc's
"exFAT read-only" step) is tested end to end. The wrinkle it solves:

  - UEFI can only boot from a FAT filesystem, so the disk it boots must carry a
    FAT32 ESP (the kernel + servers, built by `make image` into esp.img).
  - But fsd's vfs::mount takes the *first* partition it can mount, so to make it
    mount exFAT rather than the ESP, the exFAT partition must come first in the
    MBR table.

So: MBR entry 0 = exFAT partition (fsd discovers it first, FAT32 probe fails ->
exFAT probe succeeds - the real Filesystem-enum fallthrough), MBR entry 1 = the
FAT32 ESP (UEFI ignores the exFAT partition it can't read and boots BOOTAA64
from the FAT32 one). One block device, no slot-ordering ambiguity.

Inputs (build them first): esp.img (`make image`) for the FAT32 payload, and
exfatpart.img (the `exfatpart.img` Makefile recipe) for the raw exFAT
filesystem. Run from the repo root. The disk is assembled and signed by
scripts/mbrdisk.py. Since one-disk step 4 fsd tries the boot partition last,
so this order is no longer needed; it is kept so the image tests what it
always did, and build/disk.img (scripts/mkdisk.py) is the ESP-first layout.
"""
import os
import sys

import mbrdisk

ESP, EXF, DST = "build/esp.img", "build/exfatpart.img", "build/espexfat.img"

for f in (ESP, EXF):
    if not os.path.exists(f):
        sys.exit(f"{f} not found - run `make image` and the exfatpart.img recipe first")

layout = mbrdisk.assemble(DST, [
    (0x07, mbrdisk.exfat_payload(EXF), False),  # exFAT / NTFS / HPFS, first so fsd took it before step 4
    (0x0B, mbrdisk.esp_payload(ESP), True),     # FAT32, the bootable ESP
])
(exf_start, exf_sectors), (esp_start, esp_sectors) = layout
print(f"wrote {DST}: exFAT @ LBA {exf_start}, {exf_sectors} sectors; "
      f"FAT32 ESP @ LBA {esp_start}, {esp_sectors} sectors")

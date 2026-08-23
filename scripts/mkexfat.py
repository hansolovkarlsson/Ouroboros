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
filesystem. Run from the repo root.
"""
import struct, os, sys

ESP, EXF, DST, SECT = "esp.img", "exfatpart.img", "espexfat.img", 512

for f in (ESP, EXF):
    if not os.path.exists(f):
        sys.exit(f"{f} not found - run `make image` and the exfatpart.img recipe first")

# The FAT32 filesystem payload out of esp.img's own MBR (tracks `make image`).
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

# The exFAT filesystem is the whole raw image (VBR at sector 0). Its internal
# FatOffset/ClusterHeapOffset are partition-relative, which is exactly what
# fsd/src/exfat.rs adds partition_lba to - so it drops into a partition as-is.
exf = open(EXF, "rb").read()
if exf[3:11] != b"EXFAT   ":
    sys.exit(f"{EXF} is not an exFAT filesystem (no 'EXFAT   ' signature)")
exf_sectors = (len(exf) + SECT - 1) // SECT


def align_up(lba, to=2048):  # 1 MiB alignment
    return (lba + to - 1) // to * to


P1_START = 2048                                  # exFAT
P1_END = P1_START + exf_sectors - 1              # inclusive
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


mbr_entry(disk, 0, 0x07, P1_START, exf_sectors)          # exFAT / NTFS / HPFS
mbr_entry(disk, 1, 0x0b, P2_START, fat_size, boot=0x80)  # FAT32 (bootable ESP)
disk[510], disk[511] = 0x55, 0xAA

disk[P1_START * SECT:P1_START * SECT + len(exf)] = exf
disk[P2_START * SECT:P2_START * SECT + len(fat)] = fat

with open(DST, "wb") as f:
    f.write(disk)
print(f"wrote {DST}: {disk_sectors} sectors "
      f"(exFAT @ LBA {P1_START}, {exf_sectors} sectors; "
      f"FAT32 ESP @ LBA {P2_START}, {fat_size} sectors)")

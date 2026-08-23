#!/usr/bin/env python3
"""Wrap esp.img's FAT32 partition in a GPT disk (espgpt.img) for testing fsd's
GPT partition discovery (the more-filesystems arc, GPT + multi-partition step).

macOS ships no GPT tooling (no sgdisk/gdisk), and `make image` builds an MBR
FAT32 disk via hdiutil - so this hand-builds a valid, *bootable* GPT container
around the same FAT32 filesystem: a protective MBR, primary + backup GPT headers
with correct CRC32s (UEFI validates them to boot), and one EFI-System-Partition
entry pointing at the FAT32 payload. UEFI boots BOOTAA64 from the ESP, and fsd
then discovers the partition through the GPT path instead of the MBR path.

Reads the FAT32 partition offset/size from esp.img's own MBR, so it tracks
whatever `make image` produced. Run from the repo root after `make image`.
"""
import struct, zlib, os, uuid, sys

SRC, DST, SECT = "build/esp.img", "build/espgpt.img", 512

if not os.path.exists(SRC):
    sys.exit(f"{SRC} not found - run `make image` first")

with open(SRC, "rb") as f:
    img = f.read()

# Find the first non-empty partition in esp.img's MBR.
start = size = None
for i in range(4):
    e = 0x1be + i * 16
    if img[e + 4] != 0:
        start = struct.unpack("<I", img[e + 8:e + 12])[0]
        size = struct.unpack("<I", img[e + 12:e + 16])[0]
        break
if start is None:
    sys.exit(f"no MBR partition found in {SRC}")
fat = img[start * SECT:(start + size) * SECT]

PART_START = 2048                      # 1 MiB alignment
PART_END = PART_START + size - 1       # inclusive
ARR_SECT = 32                          # 128 entries * 128 B = 16 KiB
last_lba = PART_END + ARR_SECT + 1
disk_sectors = last_lba + 1
disk = bytearray(disk_sectors * SECT)

# EFI System Partition type GUID (mixed-endian on-disk byte order).
ESP_TYPE = bytes([0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11,
                  0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b])
disk_guid = uuid.uuid4().bytes
part_guid = uuid.uuid4().bytes

# Partition entry array (128 * 128), one used entry.
entry = bytearray(128)
entry[0:16] = ESP_TYPE
entry[16:32] = part_guid
entry[32:40] = struct.pack("<Q", PART_START)
entry[40:48] = struct.pack("<Q", PART_END)
name = "ESP".encode("utf-16-le")
entry[56:56 + len(name)] = name
arr = bytes(entry) + b"\x00" * (128 * 128 - 128)
arr_crc = zlib.crc32(arr) & 0xffffffff


def gpt_header(cur, bak, arr_lba):
    h = bytearray(92)
    h[0:8] = b"EFI PART"
    h[8:12] = struct.pack("<I", 0x00010000)   # revision 1.0
    h[12:16] = struct.pack("<I", 92)          # header size
    h[24:32] = struct.pack("<Q", cur)         # this header's LBA
    h[32:40] = struct.pack("<Q", bak)         # backup header LBA
    h[40:48] = struct.pack("<Q", 34)          # first usable LBA
    h[48:56] = struct.pack("<Q", PART_END)    # last usable LBA
    h[56:72] = disk_guid
    h[72:80] = struct.pack("<Q", arr_lba)     # partition entry array LBA
    h[80:84] = struct.pack("<I", 128)         # number of entries
    h[84:88] = struct.pack("<I", 128)         # size of an entry
    h[88:92] = struct.pack("<I", arr_crc)
    h[16:20] = struct.pack("<I", zlib.crc32(bytes(h)) & 0xffffffff)
    return bytes(h)


# Protective MBR (LBA 0): one 0xEE entry spanning the disk.
mbr = bytearray(512)
e = 0x1be
mbr[e + 1:e + 4] = bytes([0x00, 0x02, 0x00])   # CHS start
mbr[e + 4] = 0xEE                              # protective type
mbr[e + 5:e + 8] = bytes([0xFF, 0xFF, 0xFF])   # CHS end
mbr[e + 8:e + 12] = struct.pack("<I", 1)
mbr[e + 12:e + 16] = struct.pack("<I", min(disk_sectors - 1, 0xFFFFFFFF))
mbr[510], mbr[511] = 0x55, 0xAA
disk[0:512] = mbr

disk[1 * SECT:1 * SECT + 92] = gpt_header(1, last_lba, 2)      # primary header
disk[2 * SECT:2 * SECT + len(arr)] = arr                      # primary array
disk[PART_START * SECT:PART_START * SECT + len(fat)] = fat    # FAT32 payload
bak_arr = last_lba - ARR_SECT
disk[bak_arr * SECT:bak_arr * SECT + len(arr)] = arr          # backup array
disk[last_lba * SECT:last_lba * SECT + 92] = gpt_header(last_lba, 1, bak_arr)

with open(DST, "wb") as f:
    f.write(disk)
print(f"wrote {DST}: {disk_sectors} sectors, FAT32 partition at LBA {PART_START} ({size} sectors)")

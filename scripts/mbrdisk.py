"""Assembling an MBR disk image from partition payloads: the one copy of it.

scripts/mkdisk.py (build/disk.img, the ESP first), mkext2.py (espext2.img)
and mkexfat.py (espexfat.img) each lay two filesystems into an MBR disk, and
until the high review of #288 each carried its own copy of the same code:
the FAT32 payload out of esp.img, the 1 MiB alignment, the MBR entries, the
signing. Copies drift (they already differed in the ESP's type byte), so the
assembly is here and each script states only its layout.
"""
import struct
import sys

import disksig

SECT = 512
ESP_TYPE = 0x0C   # FAT32 LBA
EXT2_TYPE = 0x83  # Linux


def esp_payload(esp_path):
    """The FAT32 filesystem out of esp.img's first MBR partition, so every
    disk built from it tracks `make image`."""
    esp = open(esp_path, "rb").read()
    for i in range(4):
        e = 0x1BE + i * 16
        if esp[e + 4] != 0:
            start = struct.unpack("<I", esp[e + 8:e + 12])[0]
            size = struct.unpack("<I", esp[e + 12:e + 16])[0]
            return esp[start * SECT:(start + size) * SECT]
    sys.exit(f"no MBR partition found in {esp_path}")


def ext2_payload(path):
    """A raw ext2 image, checked by its magic. Its offsets are relative to the
    partition start, which fsd/src/ext2.rs adds, so it drops in as it is."""
    data = open(path, "rb").read()
    if struct.unpack("<H", data[1080:1082])[0] != 0xEF53:
        sys.exit(f"{path} is not an ext2 filesystem (no 0xEF53 magic)")
    return data


def exfat_payload(path):
    """A raw exFAT image, checked by its signature. FatOffset and
    ClusterHeapOffset are partition-relative, as fsd/src/exfat.rs expects."""
    data = open(path, "rb").read()
    if data[3:11] != b"EXFAT   ":
        sys.exit(f"{path} is not an exFAT filesystem (no 'EXFAT   ' signature)")
    return data


def align_up(lba, to=2048):
    """1 MiB alignment, in sectors."""
    return (lba + to - 1) // to * to


def entry(ptype, start, sectors, bootable=False):
    """One 16-byte MBR partition entry, LBA-only (the CHS fields 0xFF): the one
    statement of the layout, which `assemble` and `set_entry` both write."""
    return (bytes([0x80 if bootable else 0x00, 0xFF, 0xFF, 0xFF, ptype, 0xFF, 0xFF, 0xFF])
            + struct.pack("<II", start, sectors))


def set_entry(path, number, ptype, start, sectors, bootable=False):
    """Rewrite MBR entry `number` (1 to 4) of the disk image at `path` in
    place; the rest of the image is left as it is, its signature included."""
    with open(path, "r+b") as f:
        f.seek(0x1BE + (number - 1) * 16)
        f.write(entry(ptype, start, sectors, bootable))


def assemble(dst, parts):
    """Write `dst`: an MBR disk whose partitions are `parts`, a list of
    (MBR type, payload bytes, bootable), in table order, the first at LBA
    2048 and each after it on the next 1 MiB boundary. Signs the disk
    (scripts/disksig.py) before returning, so no caller can write an
    unsigned one. Returns [(start LBA, sectors)] per partition."""
    if len(parts) > 4:
        sys.exit(f"{dst}: an MBR holds four partitions, not {len(parts)}")
    layout = []
    lba = 2048
    for _, data, _ in parts:
        sectors = (len(data) + SECT - 1) // SECT
        layout.append((lba, sectors))
        lba = align_up(lba + sectors)
    disk = bytearray(lba * SECT)
    for i, ((ptype, data, bootable), (start, sectors)) in enumerate(zip(parts, layout)):
        e = 0x1BE + i * 16
        disk[e:e + 16] = entry(ptype, start, sectors, bootable)
        disk[start * SECT:start * SECT + len(data)] = data
    disk[510], disk[511] = 0x55, 0xAA
    with open(dst, "wb") as f:
        f.write(disk)
    print(f"disksig: {dst}: {disksig.set_signature(dst):#010x}")
    return layout


def one_disk(esp, root):
    """The one-disk layout (one-disk step 4), the one statement of it that
    scripts/mkdisk.py and scripts/bootstick.py both write: the FAT32 ESP as
    partition 1, bootable, the ext2 root as partition 2."""
    return [(ESP_TYPE, esp, True), (EXT2_TYPE, root, False)]


def entries(sector0):
    """[(number, type, start, sectors)] of an MBR's used entries, or None
    when sector 0 is no MBR or is a GPT's protective one."""
    if len(sector0) < SECT or sector0[510:512] != b"\x55\xaa":
        return None
    parts = []
    for i in range(4):
        e = 0x1BE + i * 16
        ptype = sector0[e + 4]
        start, sectors = struct.unpack_from("<II", sector0, e + 8)
        if ptype == 0xEE:
            return None
        if ptype and sectors:
            parts.append((i + 1, ptype, start, sectors))
    return parts

"""exFAT on the host, for the rigs: the ONE copy of the helpers the exFAT rigs
share, so a change to fsck's output, to how sets are walked or built, or to
how a volume is read is made once. Until 2026-10-26 `Volume`, `set_checksum`
and `entry_set` lived in test-stale-cursor.py and test-grow-dir.py, which the
other rigs ran as modules to take them, and `copy`, `root_end` and the fsck
rule were repeated in four (the high review of 2026-10-22).

    start = exfatimg.copy_exfat(image)           # build/espexfat.img copied, its exFAT's start
    vol = exfatimg.Volume(image, start)          # chains, the bitmap, listings (FAT32 too)
    for s in exfatimg.sets(dir_bytes): ...       # every File set, in order
    hit = exfatimg.find(dir_bytes, "VEND")       # by name, case folded as fsd does
    s = exfatimg.entry_set("NAME", first, size)  # a set as fsd builds it, sealed
    ok, lines = exfatimg.fsck_clean(image)       # fsck_exfat -n, nothing to repair
    with exfatimg.attached(image) as dev: rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
"""
import contextlib
import os
import shutil
import struct
import subprocess
import sys
from collections import namedtuple

import fat32img
import mbrdisk

BUILD = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "build")

ENTRY = 32
FILE, STREAM, NAME = 0x85, 0xC0, 0xC1

Set = namedtuple("Set", "offset count name data")


def sets(d):
    """Every File set in directory bytes `d` up to its first 0x00 slot, as
    (byte offset, entry count, name, the set's bytes), counted by the File's
    SecondaryCount. A set cut off by the end of `d` is not yielded: the walk
    stops there, no IndexError."""
    i = 0
    while i + ENTRY <= len(d) and d[i]:
        if d[i] != FILE:
            i += ENTRY
            continue
        count = d[i + 1] + 1
        end = i + ENTRY * count
        if count < 2 or end > len(d):
            return
        s = d[i:end]
        name = ""
        if s[ENTRY] == STREAM:
            chars = s[ENTRY + 3]
            raw = b"".join(s[k + 2:k + ENTRY] for k in range(2 * ENTRY, len(s), ENTRY) if s[k] == NAME)
            # NameLength counts UTF-16 units, so the cut is in bytes: a
            # surrogate pair is one Python character and two units.
            name = raw[:2 * chars].decode("utf-16le", "replace")
        yield Set(i, count, name, bytes(s))
        i = end


def find(d, name):
    """The set named `name` in `d`, matched ignoring ASCII case as fsd's
    lookups do, or None."""
    for s in sets(d):
        if s.name.lower() == name.lower():
            return s
    return None


@contextlib.contextmanager
def attached(image):
    """`image` attached without mounting; yields its /dev/diskN, detached after."""
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", image],
                         capture_output=True, text=True)
    if att.returncode != 0:
        raise RuntimeError(f"hdiutil attach {image} failed: {att.stderr.strip()}")
    dev = att.stdout.split()[0]
    try:
        yield dev
    finally:
        if subprocess.run(["hdiutil", "detach", dev], capture_output=True).returncode != 0:
            print(f"exfatimg: hdiutil detach {dev} failed: the device is still attached", file=sys.stderr)


def fsck(dev, tool):
    """`tool -n dev` (fsck_exfat or fsck_msdos): (exit code, its lines)."""
    r = subprocess.run([tool, "-n", dev], capture_output=True, text=True)
    return r.returncode, (r.stdout + r.stderr).strip().splitlines()


def complaints(lines):
    """fsck_exfat's complaints: its indented lines."""
    return [ln for ln in lines if ln.startswith("   ")]


def unexpected(lines):
    """Lines that are none of fsck_exfat's stages, complaints or its `NO`
    to a repair."""
    return [ln for ln in lines if not (ln.startswith(("** ", "   ")) or ln.endswith("? NO"))]


def exfat_start(image):
    """The first sector of `image`'s exFAT partition (MBR type 0x07)."""
    with open(image, "rb") as fh:
        parts = mbrdisk.entries(fh.read(512))
    return [p for p in parts if p[1] == 0x07][0][2]


def copy_exfat(image):
    """build/espexfat.img copied to `image`; answers its exFAT partition's
    first sector."""
    shutil.copyfile(os.path.join(BUILD, "espexfat.img"), image)
    return exfat_start(image)


def root_end(d):
    """The byte offset of directory bytes `d`'s first 0x00 slot, its end."""
    i = 0
    while i < len(d) and d[i]:
        i += ENTRY
    return i


def fsck_clean(image, part="s1"):
    """fsck_exfat -n on `image`'s partition `part`: (whether it ran its
    checks and found nothing to repair, its lines)."""
    with attached(image) as dev:
        rc, lines = fsck(dev + part, "fsck_exfat")
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not complaints(lines) and not unexpected(lines))
    return ok, lines


def set_checksum(entries):
    """An exFAT entry set's SetChecksum: every byte but the primary's 2 and 3."""
    total = 0
    for i, b in enumerate(entries):
        if i not in (2, 3):
            total = (((total << 15) | (total >> 1)) + b) & 0xFFFF
    return total


def entry_set(name, first=0, size=0):
    """A file's entry set, FAT-chained, as fsd's build_entry_set makes it."""
    n = name.encode("utf-16le")
    k = -(-len(name) // 15)
    s = bytearray(32 * (2 + k))
    s[0], s[1] = 0x85, 1 + k
    s[32], s[33], s[35] = 0xC0, 0x01, len(name)
    h = 0
    for b in name.upper().encode("utf-16le"):
        h = (((h << 15) | (h >> 1)) + b) & 0xFFFF
    struct.pack_into("<H", s, 36, h)
    struct.pack_into("<Q", s, 40, size)  # ValidDataLength
    struct.pack_into("<I", s, 52, first)
    struct.pack_into("<Q", s, 56, size)  # DataLength
    for e in range(k):
        chunk = n[30 * e:30 * e + 30]
        s[64 + 32 * e] = 0xC1
        s[64 + 32 * e + 2:64 + 32 * e + 2 + len(chunk)] = chunk
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


class Volume:
    """A FAT32 or exFAT partition of a disk image as the host reads it: the
    cluster size, chains, and paths of live entries. On FAT32 the FAT is
    fat32img's: its entries, its chain rule and `fat`, the byte offset a rig
    writes an entry at, all the live copy's (by BPB_ExtFlags). On exFAT
    `fat` is the first FAT, which fsd reads (a volume of two is read-only
    there)."""

    def __init__(self, image, start_lba):
        self.fh = open(image, "rb")
        base = start_lba * 512
        self.fh.seek(base)
        b = self.fh.read(512)
        self.exfat = b[3:11] == b"EXFAT   "
        if self.exfat:
            sector = 1 << b[108]
            fat, _, heap, self.count, self.root = struct.unpack_from("<IIIII", b, 80)
            self.cluster = sector << b[109]
            self.end = 0xFFFFFFF7
        else:
            sector = struct.unpack_from("<H", b, 11)[0]
            reserved, nfats = struct.unpack_from("<H", b, 14)[0], b[16]
            fatsz = struct.unpack_from("<I", b, 36)[0]
            self.root = struct.unpack_from("<I", b, 44)[0]
            fat, heap = reserved, reserved + nfats * fatsz
            self.count = fatsz * sector // 4
            self.cluster = b[13] * sector
            self.end = 0x0FFFFFF8
        self.fat32 = None if self.exfat else fat32img.Fat32(self.fh, start_lba)
        self.fat = base + fat * sector if self.exfat else (self.fat32.fat + self.fat32.live * self.fat32.fatsz) * 512
        self.heap = base + heap * sector
        if self.exfat:
            # The allocation bitmap's first cluster, from the root's 0x81 entry
            # (assumed contiguous, as fsd's exfat.rs assumes it).
            d = self.data(self.chain(self.root))
            hits = [struct.unpack_from("<I", d, i + 20)[0] for i in range(0, len(d), 32) if d[i] == 0x81]
            if not hits:
                raise ValueError("no allocation bitmap in the exFAT root")
            self.bitmap = hits[0]

    def used(self, c):
        """Whether cluster `c` is allocated: its FAT entry on FAT32, its
        allocation bitmap bit on exFAT (a contiguous file leaves its FAT
        entries 0)."""
        if not self.exfat:
            return self.entry(c) != 0
        self.fh.seek(self.heap + (self.bitmap - 2) * self.cluster + (c - 2) // 8)
        return bool(self.fh.read(1)[0] >> ((c - 2) % 8) & 1)

    def entry(self, c):
        if self.fat32:
            return self.fat32.entry(c)
        self.fh.seek(self.fat + c * 4)
        return struct.unpack("<I", self.fh.read(4))[0]

    def chain(self, first):
        if self.fat32:
            return self.fat32.chain(first)
        out = [first]
        while 2 <= self.entry(out[-1]) < self.end:
            if len(out) > self.count:
                raise ValueError(f"the chain from cluster {first} is longer than the volume: a loop")
            out.append(self.entry(out[-1]))
        return out

    def data(self, chain):
        out = b""
        for c in chain:
            self.fh.seek(self.heap + (c - 2) * self.cluster)
            out += self.fh.read(self.cluster)
        return out

    def listing(self, first, chained=True):
        """{name: (first cluster, size, chained)} of the live entries of a directory."""
        d = self.data(self.chain(first) if chained else [first])
        out = {}
        if not self.exfat:
            for j in range(0, len(d), 32):
                e = d[j:j + 32]
                if e[0] == 0:
                    break
                if e[0] != 0xE5 and e[11] != 0x0F:
                    base, ext = e[:8].decode("latin1").rstrip(), e[8:11].decode("latin1").rstrip()
                    out[base + ("." + ext if ext else "")] = (
                        (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0],
                        struct.unpack_from("<I", e, 28)[0], True)
            return out
        i = 0
        while i < len(d) and d[i]:
            count = d[i + 1] + 1 if d[i] == 0x85 else 1
            if d[i] == 0x85:
                stream = d[i + 32:i + 64]
                name = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
                out[name.decode("utf-16le")[:stream[3]]] = (
                    struct.unpack_from("<I", stream, 20)[0], struct.unpack_from("<Q", stream, 24)[0],
                    not stream[1] & 0x02)
            i += 32 * count
        return out

    def file_chain(self, path):
        """`path`'s chain, its parts matched without case. Raises ValueError for
        a missing part or a contiguous file (fsd writes every file chained)."""
        first, chained = self.root, True
        for part in path.strip("/").split("/"):
            names = {k.upper(): v for k, v in self.listing(first, chained).items()}
            if part.upper() not in names:
                raise ValueError(f"no {path} on the disk")
            first, _, chained = names[part.upper()]
        if not chained:
            raise ValueError(f"{path} is contiguous: fsd writes every file FAT-chained")
        return self.chain(first)

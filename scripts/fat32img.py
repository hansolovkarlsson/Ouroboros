"""A FAT32 volume inside a disk image, as the host reads it: the ONE copy of
the reader the rigs share (test-write-walk.py and test-fat-link.py), so fsd's
rules that a rig mirrors, `cluster_end` above all, are stated once here.

    vol = fat32img.Fat32(fh, start)    # fh open on the image, start its LBA

Geometry from the boot sector, FAT entries from the copy BPB_ExtFlags
names live (copy 0 when the FATs are mirrored; `set_entry` writes every copy
then, the live one alone when they are not, as fsd does since 2026-10-26),
chains, the short entries of a directory, and a file's bytes. Not a
filesystem: nothing here allocates or makes a name.
"""
import struct

SECTOR = 512
EOC_MIN = 0x0FFFFFF8
# The bad-cluster mark: the first value that names no cluster.
BAD_CLUSTER = 0x0FFFFFF7


def dotted(name):
    """A short name as `listing` gives it (the 11 bytes, trailing blanks
    cut) written BASE.EXT, or BASE when it has no extension."""
    base, ext = name[:8].rstrip(), name[8:].rstrip()
    return base + ("." + ext if ext else "")


class Fat32:
    def __init__(self, fh, start):
        self.fh, self.start = fh, start
        fh.seek(start * SECTOR)
        b = fh.read(SECTOR)
        self.spc, reserved, self.nfats = b[13], struct.unpack_from("<H", b, 14)[0], b[16]
        self.fatsz = struct.unpack_from("<I", b, 36)[0]
        self.root = struct.unpack_from("<I", b, 44)[0]
        self.fat = start + reserved
        self.fat_end = self.fat + self.nfats * self.fatsz
        self.data = self.fat_end
        self.count = self.fatsz * SECTOR // 4
        # BPB_ExtFlags: with bit 7 set only the copy in bits 0-3 is live.
        ext = struct.unpack_from("<H", b, 40)[0]
        self.mirrored = not ext & 0x80
        self.live = 0 if self.mirrored else ext & 0x0F
        self.cluster_bytes = self.spc * SECTOR
        total = struct.unpack_from("<H", b, 19)[0] or struct.unpack_from("<I", b, 32)[0]
        # fsd's cluster_end (fat32.rs, mount_at): one past the last cluster,
        # the data sectors over the cluster size plus 2, never past what one
        # FAT copy describes nor as far as the bad-cluster mark.
        self.cluster_end = min((total - reserved - self.nfats * self.fatsz) // self.spc + 2,
                               self.count, BAD_CLUSTER)

    def in_heap(self, c):
        return 2 <= c < self.cluster_end

    def lba(self, c):
        """The first sector of cluster `c`, wherever that is (a rig plants
        clusters past the volume on purpose)."""
        return self.data + (c - 2) * self.spc

    def fat_lba(self, c, copy=0):
        """The sector holding cluster `c`'s entry in FAT copy `copy`."""
        return self.fat + copy * self.fatsz + c * 4 // SECTOR

    def entry(self, c, copy=None):
        """Cluster `c`'s entry, low 28 bits, from FAT copy `copy` (the live
        one by default)."""
        copy = self.live if copy is None else copy
        if not 0 <= c < self.count:
            raise ValueError(f"cluster {c} is past the FAT")
        self.fh.seek((self.fat + copy * self.fatsz) * SECTOR + c * 4)
        return struct.unpack("<I", self.fh.read(4))[0] & 0x0FFFFFFF

    def set_entry(self, c, value):
        """Write cluster `c`'s entry in every FAT copy when mirrored, the live
        one alone when not, its top four bits kept, as fsd's write_fat_entry
        does. `fh` must be open for writing."""
        if not 0 <= c < self.count:
            raise ValueError(f"cluster {c} is past the FAT")
        for copy in (range(self.nfats) if self.mirrored else [self.live]):
            at = (self.fat + copy * self.fatsz) * SECTOR + c * 4
            self.fh.seek(at)
            old = struct.unpack("<I", self.fh.read(4))[0]
            self.fh.seek(at)
            self.fh.write(struct.pack("<I", (old & 0xF0000000) | (value & 0x0FFFFFFF)))

    def chain(self, first, copy=None):
        """`first` and every cluster after it, as fsd walks it, through FAT
        copy `copy` (the live one by default): a link outside the volume's
        clusters, or an end of chain, ends it."""
        out = [first]
        while self.in_heap(out[-1]) and self.in_heap(self.entry(out[-1], copy)):
            if len(out) == self.count:
                raise ValueError(f"the FAT32 chain from cluster {first} is longer than the volume: a loop")
            out.append(self.entry(out[-1], copy))
        return out

    def listing(self, first):
        """[(index of the directory's cluster holding the entry, short name, first cluster, size)]."""
        out = []
        for i, c in enumerate(self.chain(first)):
            self.fh.seek(self.lba(c) * SECTOR)
            d = self.fh.read(self.cluster_bytes)
            for j in range(0, len(d), 32):
                e = d[j:j + 32]
                if e[0] == 0:
                    return out
                if e[0] != 0xE5 and e[11] != 0x0F:
                    first_c = (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0]
                    out.append((i, e[:11].decode("latin1").rstrip(), first_c,
                                struct.unpack_from("<I", e, 28)[0]))
        return out

    def locate(self, first, name):
        """The byte offset in the image of short entry `name` (as `listing`
        gives it) in the directory at `first`, or None."""
        for c in self.chain(first):
            base = self.lba(c) * SECTOR
            self.fh.seek(base)
            d = self.fh.read(self.cluster_bytes)
            for j in range(0, len(d), 32):
                e = d[j:j + 32]
                if e[0] == 0:
                    return None
                if e[0] != 0xE5 and e[11] != 0x0F and e[:11].decode("latin1").rstrip() == name:
                    return base + j
        return None

    def set_first(self, at, cluster):
        """Point the short entry at byte offset `at` at first cluster `cluster`."""
        self.fh.seek(at + 20)
        self.fh.write(struct.pack("<H", cluster >> 16))
        self.fh.seek(at + 26)
        self.fh.write(struct.pack("<H", cluster & 0xFFFF))

    def read(self, chain, size):
        out = b""
        for c in chain:
            self.fh.seek(self.lba(c) * SECTOR)
            out += self.fh.read(self.cluster_bytes)
        return out[:size]

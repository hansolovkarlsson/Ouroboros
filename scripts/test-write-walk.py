#!/usr/bin/env python3
"""A write walks a file's cluster chain once, on exFAT and on FAT32, exactly.

    python3 scripts/test-write-walk.py    (or `make test-write-walk`, which builds the image)

`cp` writes its destination 2 KiB a request (`fs_write_at`), and until
2026-10-17 each request's `write_at` walked the file's chain from its first
cluster to the offset. Each filesystem keeps a write cursor now: the next
write of the same file at or past where the last one ended seeks from there.
No program's output can show it; only the disk can, so this rig counts
QEMU's virtio-blk reads of the destination's FAT (scripts/disktrace.py).

On a copy of build/espexfat.img (an exFAT partition 1 mounted as `/` and the
FAT32 ESP as partition 2) the host puts /HUGE, 640 KiB, and /EMPTY on both
partitions. Each filesystem keeps its own FAT, FAT cache and cursors, so a
copy from one to the other puts the source's reads in one FAT and the
destination's writes in the other. Two pairs of boots, each pair the same but
for one `cp` after `mount 1 /mnt/f`:

- exFAT: `cp /mnt/f/HUGE /CHAIN` against `cp /mnt/f/EMPTY /CEMPTY`;
- FAT32: `cp /HUGE /mnt/f/C32` against `cp /EMPTY /mnt/f/CEMPTY`.

The FAT sectors of the destination read in one boot of a pair more than in
the other must be what the design predicts from the chain `cp` built, read by
the host off the boot's disk, simulated a 2 KiB request at a time through
the one-sector FAT cache: each request's path resolution looks up the
entries of the directory clusters it passes, and its write, with the cursor,
looks up only the entry it steps past to extend the chain, then allocates
(exFAT: the new cluster's entry and the link; FAT32: the free-cluster scan
from its hint, each FAT write reading the second FAT copy too). Where the
cache starts comes from the disk as well: on exFAT the last /bin cluster the
shell's lookup of `cp` stepped past, on the ESP the root's one FAT sector.
The copy must hold /HUGE's bytes, and its chain must cross a FAT sector, or
the cursor could not be seen; each is checked.

About four minutes.
"""
import importlib.util
import os
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disktrace  # noqa: E402

_spec = importlib.util.spec_from_file_location("spawn_reads", os.path.join(HERE, "test-spawn-reads.py"))
spawn_reads = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(spawn_reads)

SECTOR = 512
IMAGE = "espexfat.img"
HUGE = 640 * 1024
REQUEST = 2048  # cp's chunk, SAFECOPY_MAX


class Fat32:
    """The ESP partition of an image as the host reads it: geometry, FAT
    entries from the first copy, chains, directories (short names only)."""

    def __init__(self, image):
        start, _ = spawn_reads.partition(image, 0x0B)
        self.fh = open(image, "rb")
        self.fh.seek(start * SECTOR)
        b = self.fh.read(SECTOR)
        self.spc, reserved, self.nfats = b[13], struct.unpack_from("<H", b, 14)[0], b[16]
        fatsz = struct.unpack_from("<I", b, 36)[0]
        self.root = struct.unpack_from("<I", b, 44)[0]
        self.fat = start + reserved
        self.fat_end = self.fat + self.nfats * fatsz
        self.data = self.fat_end
        self.count = fatsz * SECTOR // 4

    def entry(self, c):
        self.fh.seek(self.fat * SECTOR + c * 4)
        return struct.unpack("<I", self.fh.read(4))[0] & 0x0FFFFFFF

    def chain(self, first):
        out = [first]
        while 2 <= self.entry(out[-1]) < 0x0FFFFFF8:
            if len(out) == self.count:
                raise ValueError(f"the FAT32 chain from cluster {first} is longer than the volume: a loop")
            out.append(self.entry(out[-1]))
        return out

    def listing(self, first):
        """[(index of the directory's cluster holding the entry, short name, first cluster)]."""
        out = []
        for i, c in enumerate(self.chain(first)):
            self.fh.seek((self.data + (c - 2) * self.spc) * SECTOR)
            d = self.fh.read(self.spc * SECTOR)
            for j in range(0, len(d), 32):
                e = d[j:j + 32]
                if e[0] == 0:
                    return out
                if e[0] != 0xE5 and e[11] != 0x0F:
                    first_c = (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0]
                    out.append((i, e[:11].decode("latin1").rstrip(), first_c))
        return out

    def read(self, chain, size):
        out = b""
        for c in chain:
            self.fh.seek((self.data + (c - 2) * self.spc) * SECTOR)
            out += self.fh.read(self.spc * SECTOR)
        return out[:size]


def exfat_read(image, chain, size):
    g = spawn_reads.exfat_layout(image)
    out = b""
    with open(image, "rb") as fh:
        for c in chain:
            fh.seek((g["heap"] + (c - 2) * g["spc"]) * SECTOR)
            out += fh.read(g["spc"] * SECTOR)
    return out[:size]


class Cache:
    """The one-sector FAT cache both filesystems keep, counting the reads it misses."""

    def __init__(self, sector=None):
        self.sector, self.reads = sector, 0

    def access(self, cluster):
        s = cluster * 4 // SECTOR
        if s != self.sector:
            self.reads, self.sector = self.reads + 1, s


def simulate(size, cluster_bytes, resolve, new_cluster, cache):
    """Run `cp`'s writes of `size` bytes, REQUEST bytes each, through `cache`:
    each request first resolves the path (`resolve(cache)`), then allocates,
    in order, each cluster index its bytes reach that the file does not have
    yet (`new_cluster(cache, index)`). With the write cursor nothing else in
    a write looks up the FAT. Returns the reads, or None if `new_cluster`
    found the model does not describe the copy."""
    have = 0
    for start in range(0, size, REQUEST):
        resolve(cache)
        last = (min(start + REQUEST, size) - 1) // cluster_bytes
        while have <= last:
            if new_cluster(cache, have) is False:
                return None
            have += 1
    return cache.reads


def exfat_reads(chain, cluster_bytes, start_sector):
    """exFAT: the head's allocation writes its entry; each later cluster looks
    up the full one's entry (the end), writes the new one's and links the full
    one. The root is one cluster, so a resolution looks up nothing."""
    def new(cache, i):
        for c in ([chain[0]] if i == 0 else [chain[i - 1], chain[i], chain[i - 1]]):
            cache.access(c)
    return simulate(HUGE, cluster_bytes, lambda cache: None, new, Cache(start_sector))


def fat32_reads(chain, cluster_bytes, free_before, nfats, passed, start_sector):
    """FAT32: find_free_cluster scans from its hint (2 at mount) through the
    cache to the first free entry, which must be the chain's next cluster;
    write_fat_entry writes the first copy through the cache and reads every
    other copy from the disk; a resolution looks up the root clusters it
    steps past (`passed`)."""
    state = {"hint": 2}

    def find_free(cache):
        c = state["hint"]
        while True:
            cache.access(c)
            if free_before(c):
                state["hint"] = c + 1
                return c
            c += 1

    def write(cache, c):
        cache.access(c)
        cache.reads += nfats - 1

    def new(cache, i):
        if i > 0:
            cache.access(chain[i - 1])
        if find_free(cache) != chain[i]:
            return False
        write(cache, chain[i])
        if i > 0:
            write(cache, chain[i - 1])

    def resolve(cache):
        for c in passed:
            cache.access(c)

    return simulate(HUGE, cluster_bytes, resolve, new, Cache(start_sector))


def exfat_entries(image, chain):
    """[(index of the directory's cluster holding the entry set, name)] of an
    exFAT directory whose clusters are `chain`."""
    g = spawn_reads.exfat_layout(image)
    cluster_bytes = g["spc"] * SECTOR
    data = exfat_read(image, chain, len(chain) * cluster_bytes)
    out, i = [], 0
    while i < len(data) and data[i]:
        count = data[i + 1] + 1 if data[i] == 0x85 else 1
        if data[i] == 0x85:
            stream = data[i + 32:i + 64]
            names = b"".join(data[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            out.append((i // cluster_bytes, names.decode("utf-16le")[:stream[3]]))
        i += 32 * count
    return out


def boot(tag, prepared, cmd, inspect=None):
    steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f"), ("# ", cmd), ("# ", "")]
    problems, lines, count = disktrace.boot(f"test-write-walk-{tag}", prepared, IMAGE, steps, inspect)
    refused = [ln for ln in lines if ln.startswith("cp:")]
    if refused:
        problems.append(f"`{cmd}` said: {refused[0]}")
    return problems, count


def grade(name, prepared, base_cmd, cmd, region, inspect):
    """Boot the pair, then compare the destination FAT's extra reads with the
    prediction `inspect` leaves in `found` ({"want", "what"}) or the problems
    it found; print the verdict and return 1 on a failure."""
    found, inspect_problems = {}, []

    def run_inspect(copy):
        try:
            inspect(copy, found, inspect_problems)
        except ValueError as e:
            inspect_problems.append(str(e))

    p0, base = boot(f"{name}-base", prepared, base_cmd)
    p1, more = boot(f"{name}-more", prepared, cmd, run_inspect)
    problems = p0 + p1 + inspect_problems
    if not problems and "want" not in found:
        problems.append("the copy's disk was not read")
    delta = want = None
    if not problems:
        lo, hi = region
        more_problems, per = disktrace.per_run(base, more, 1)
        problems += more_problems
        delta = sum(c for s, c in per.items() if lo <= s < hi)
        want = found["want"]
        if not problems and delta != want:
            problems.append(f"`{cmd}` reads {delta} FAT sectors, not {want}: one walk of the chain it "
                            "writes, through the FAT cache, is the rule")
    verdict = "FAIL" if problems else "ok  "
    print(f"{verdict} {name}: `{cmd}` ({found.get('what', '?')}) reads {delta} FAT sectors of the "
          f"destination, {want} for one walk of its chain")
    for p in problems:
        print(f"     {p}")
    return 1 if problems else 0


def main() -> int:
    data = bytes((i * 7 + i // 4096) & 0xFF for i in range(HUGE))
    files = {"HUGE": data, "EMPTY": b""}
    prepared = os.path.join(ROOT, "build", "test-write-walk-prepared.img")
    shutil.copyfile(os.path.join(ROOT, "build", IMAGE), prepared)
    failed = 0
    try:
        spawn_reads.place_mounted(prepared, files, part=1)
        spawn_reads.place_mounted(prepared, files, part=2)
        g = spawn_reads.exfat_layout(prepared)
        f32 = Fat32(prepared)
        f32_region = (f32.fat, f32.fat_end)
        f32_root_len = len(f32.chain(f32.root))
        f32.fh.close()

        def exfat_inspect(copy, found, problems):
            root, chain = spawn_reads.exfat_chain(copy, "CHAIN")
            _, bin_chain = spawn_reads.exfat_chain(copy, "bin")
            if chain is None or bin_chain is None:
                problems.append("/CHAIN or /bin is contiguous: fsd writes every file FAT-chained, "
                                "and the model reads where the cache starts off /bin's chain")
                return
            sectors = {c * 4 // SECTOR for c in chain}
            cp = [i for i, n in exfat_entries(copy, bin_chain) if n.upper() == "CP"]
            if len(root) > 1:
                problems.append(f"the exFAT root spans {len(root)} clusters: resolving /CHAIN may step along it")
            elif len(sectors) < 2:
                problems.append("/CHAIN's chain lies in one FAT sector: a walk from its start would cost no "
                                "more reads, so the cursor could not be seen")
            elif len(cp) != 1 or cp[0] == 0:
                problems.append("/bin/CP's entry is not past /bin's first cluster: where the FAT cache "
                                "starts the copy is not known from the disk")
            elif exfat_read(copy, chain, HUGE) != data:
                problems.append("/CHAIN does not hold /HUGE's bytes")
            else:
                start = bin_chain[cp[0] - 1] * 4 // SECTOR
                found["want"] = exfat_reads(chain, g["spc"] * SECTOR, start)
                found["what"] = f"{len(chain)} clusters across {len(sectors)} FAT sectors"

        def fat32_inspect(copy, found, problems):
            fs = Fat32(copy)
            try:
                root = fs.chain(fs.root)
                dst = [e for e in fs.listing(fs.root) if e[1] == "C32"]
                if len(dst) != 1:
                    problems.append("no C32 in the ESP's root after the copy")
                    return
                chain = fs.chain(dst[0][2])
                mine = set(chain)
                sectors = {c * 4 // SECTOR for c in chain}
                if len(root) != f32_root_len:
                    problems.append("the ESP's root grew during the copy: the model does not count that")
                elif len({c * 4 // SECTOR for c in root}) != 1:
                    problems.append("the ESP's root crosses a FAT sector: where the cache starts is not known")
                elif len(sectors) < 2:
                    problems.append("C32's chain lies in one FAT sector: the cursor could not be seen")
                elif fs.read(chain, HUGE) != data:
                    problems.append("C32 does not hold /HUGE's bytes")
                else:
                    want = fat32_reads(chain, fs.spc * SECTOR, lambda c: fs.entry(c) == 0 or c in mine,
                                       fs.nfats, root[:dst[0][0]], root[0] * 4 // SECTOR)
                    if want is None:
                        problems.append("C32's chain is not the one the free-cluster scan chooses: the "
                                        "model does not describe this copy")
                    else:
                        found["want"] = want
                        found["what"] = f"{len(chain)} clusters across {len(sectors)} FAT sectors"
            finally:
                fs.fh.close()

        failed += grade("exfat", prepared, "cp /mnt/f/EMPTY /CEMPTY", "cp /mnt/f/HUGE /CHAIN",
                        (g["fat"], g["fat_end"]), exfat_inspect)
        failed += grade("fat32", prepared, "cp /EMPTY /mnt/f/CEMPTY", "cp /HUGE /mnt/f/C32",
                        f32_region, fat32_inspect)
    finally:
        os.remove(prepared)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

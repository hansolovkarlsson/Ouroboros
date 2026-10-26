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
from its hint, each FAT write reading no copy but the first). Where the
cache starts comes from the disk as well: on exFAT the last /bin cluster the
shell's lookup of `cp` stepped past, on the ESP the root's one FAT sector.
The copy must hold /HUGE's bytes, and its chain must cross a FAT sector, or
the cursor could not be seen; each is checked.

Two more pairs copy within one volume:

- exFAT: `cp /SRC /CHAIN` against `cp /EMPTY /CEMPTY`, after a first copy
  from the ESP has made /SRC FAT-chained in both boots of the pair (macOS
  writes /HUGE contiguous, which reads no FAT);
- FAT32: `cp /mnt/f/HUGE /mnt/f/C32` against `cp /mnt/f/EMPTY /mnt/f/CEMPTY`.

Here the source's reads share the FAT and its cache with the destination's
writes, and the model walks the source on from its read cursor before each
write. Since 2026-10-18 a FAT write drops the cursors only when it rewrites a
link, so the destination's allocations, which write only entries where a walk
stopped, leave the source's read cursor standing (on these images, where a
free cluster's FAT entry is 0: fsd's own frees write it so, but on exFAT a
driver may leave a link in a freed cluster's entry, Linux's does, and an
allocation over it drops both cursors, which this pair would show as a
higher count); with the old rule (a drop at every FAT write) the source walks
from its first cluster after each, and the pairs read 867 and 5,048
FAT sectors where the model has 323 and 3,333 (measured on the
tree of 2026-10-18; the counts move with every file staged on the image).

Since 2026-10-21 a FAT32 FAT write takes the first copy's sector, patches
the entry and writes that sector to every copy; until then it read each
other copy's sector and patched it, which was 2,559 of the 2,689 FAT reads
of the copy onto the ESP. The host plants the bad-cluster mark at every
free entry of the ESP's second copy before the boots, which fsd never reads,
and after each FAT32 copy every FAT sector of the destination's chain must be
the first copy's in the second, byte for byte (it fails with each copy's own
sector patched, sector 54 differing); one of those sectors must still hold a
free entry, and a sector the copy did not write must still differ (the
planted marks survived the boot), or the check could not fail. A free
(`rm`) is not planted against: it writes through the same write_fat_entry.
On the tree of 2026-10-21 the FAT32
pairs read 134 and 774 FAT sectors.

A boot QEMU's firmware never starts is INCONCLUSIVE (exit 100,
drive-qemu.py's classifier, through disktrace.py), and so is its pair,
unless another of its boots ran and failed, which fails it.

About two and a half minutes.
"""
import functools
import importlib.util
import os
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disktrace  # noqa: E402
import fat32img  # noqa: E402

_spec = importlib.util.spec_from_file_location("spawn_reads", os.path.join(HERE, "test-spawn-reads.py"))
spawn_reads = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(spawn_reads)

SECTOR = 512
IMAGE = "espexfat.img"
HUGE = 640 * 1024
REQUEST = 2048  # cp's chunk, SAFECOPY_MAX


def Fat32(image):
    """The ESP partition of an image as the host reads it (scripts/fat32img.py,
    the reader the rigs share): geometry, FAT entries from the first copy,
    chains, directories (short names only)."""
    start, _ = spawn_reads.partition(image, 0x0B)
    return fat32img.Fat32(open(image, "rb"), start)


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


def simulate(size, cluster_bytes, passed, new_cluster, cache, src=None, src_passed=()):
    """Run `cp`'s writes of `size` bytes, REQUEST bytes each, through `cache`:
    each request first resolves the path, looking up the entries of the
    directory clusters it steps past (`passed`), then allocates, in order,
    each cluster index its bytes reach that the file does not have yet
    (`new_cluster(cache, index)`). With the write cursor nothing else in a
    write looks up the FAT. Returns the reads, or None if `new_cluster` found
    the model does not describe the copy.

    `src`, the source's chain for a copy within one volume (its resolution
    stepping past `src_passed`): before each write, `cp` reads the same
    REQUEST bytes of the source through the same cache, resolving its path
    and then walking on from its read cursor, which no allocation drops
    (since 2026-10-18, where free clusters' FAT entries are 0), looking up the entry of each cluster it leaves for
    the next, up to the one holding the request's last byte. The read that
    returns 0 at the end resolves the path too."""
    def resolve(through):
        for c in through:
            cache.access(c)

    have = 0
    # The source's read cursor, a cluster index: the first read is at offset
    # 0, which starts at the head (cp's 1-byte probe goes through
    # NP_READ_FILE, which leaves the cursor alone).
    at = 0
    for start in range(0, size, REQUEST):
        end = min(start + REQUEST, size)
        if src:
            resolve(src_passed)
            while (at + 1) * cluster_bytes < end:
                cache.access(src[at])
                at += 1
        resolve(passed)
        last = (end - 1) // cluster_bytes
        while have <= last:
            if new_cluster(cache, have) is False:
                return None
            have += 1
    if src:
        resolve(src_passed)
    return cache.reads


def exfat_reads(chain, cluster_bytes, start_sector, src=None):
    """exFAT: the head's allocation writes its entry; each later cluster looks
    up the full one's entry (the end), writes the new one's and links the full
    one. The root is one cluster, so a resolution looks up nothing. `src`:
    the source's chain, for a copy within the volume."""
    def new(cache, i):
        for c in ([chain[0]] if i == 0 else [chain[i - 1], chain[i], chain[i - 1]]):
            cache.access(c)
    return simulate(HUGE, cluster_bytes, (), new, Cache(start_sector), src)


def fat32_reads(chain, cluster_bytes, free_before, passed, start_sector, end, src=None, src_passed=()):
    """FAT32: find_free_cluster scans from its hint (2 at mount) through the
    cache to the first free entry below `end` (the volume's cluster_end),
    which must be the chain's next cluster;
    write_fat_entry patches the first copy's sector through the cache and
    writes it to every copy, reading no other (since 2026-10-21; before, it
    read each other copy's sector from the disk); a resolution looks up the root clusters it
    steps past (`passed`, `src_passed` for the source of a copy within the
    volume, whose chain is `src`)."""
    state = {"hint": 2}

    def find_free(cache):
        c = state["hint"]
        while c < end:
            cache.access(c)
            if free_before(c):
                state["hint"] = c + 1
                return c
            c += 1
        return None  # fsd would wrap to cluster 2, which the model does not follow

    def write(cache, c):
        cache.access(c)

    def new(cache, i):
        if i > 0:
            cache.access(chain[i - 1])
        if find_free(cache) != chain[i]:
            return False
        write(cache, chain[i])
        if i > 0:
            write(cache, chain[i - 1])

    return simulate(HUGE, cluster_bytes, passed, new, Cache(start_sector), src, src_passed)


def exfat_entries(image, chain):
    """[(index of the directory's cluster holding the entry set, name, data
    length)] of an exFAT directory whose clusters are `chain`."""
    g = spawn_reads.exfat_layout(image)
    cluster_bytes = g["spc"] * SECTOR
    data = exfat_read(image, chain, len(chain) * cluster_bytes)
    out, i = [], 0
    while i < len(data) and data[i]:
        count = data[i + 1] + 1 if data[i] == 0x85 else 1
        if data[i] == 0x85:
            stream = data[i + 32:i + 64]
            names = b"".join(data[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            out.append((i // cluster_bytes, names.decode("utf-16le")[:stream[3]],
                        struct.unpack_from("<Q", stream, 24)[0]))
        i += 32 * count
    return out


def plant_backup(image):
    """Make every free entry of the ESP's second FAT copy differ from the
    first: the bad-cluster mark where the first copy has 0. fsd reads no copy
    but the first, so the model is unchanged; `mirrored` then sees which
    sectors fsd wrote whole. Returns the entries planted."""
    start, _ = spawn_reads.partition(image, 0x0B)
    with open(image, "r+b") as fh:
        fs = fat32img.Fat32(fh, start)
        if fs.nfats < 2:
            raise ValueError("the ESP has one FAT copy: there is no mirror to check")
        fh.seek(fs.fat * SECTOR)
        first = bytearray(fh.read(fs.fatsz * SECTOR))
        second = bytearray(first)
        planted = 0
        for c in range(2, fs.cluster_end):
            if struct.unpack_from("<I", first, c * 4)[0] & 0x0FFFFFFF == 0:
                struct.pack_into("<I", second, c * 4, fat32img.BAD_CLUSTER)
                planted += 1
        fh.seek((fs.fat + fs.fatsz) * SECTOR)
        fh.write(second)
    return planted


def mirrored(fs, sectors):
    """Each FAT sector in `sectors` (an index into one copy) must be the
    first copy's, byte for byte, in every other copy: write_fat_entry writes
    the first copy's sector to all of them. `plant_backup` left the bad-cluster
    mark in the second copy at every entry the first had free, so a sector
    that still has a free entry differs if only the written entries were
    patched; at least one such sector is required, and at least one sector
    fsd did not write must still differ between the copies (the planted
    marks survived the boot: nothing else, the firmware's FAT driver say,
    mirrored the FAT), or the check could not fail. A free (`rm`) is not
    planted against: it writes through the same write_fat_entry."""
    problems, open_sectors = [], 0
    still_planted = False
    for s in range(fs.fatsz):
        if s in sectors:
            continue
        fs.fh.seek((fs.fat + s) * SECTOR)
        first = fs.fh.read(SECTOR)
        fs.fh.seek((fs.fat + fs.fatsz + s) * SECTOR)
        if fs.fh.read(SECTOR) != first:
            still_planted = True
            break
    if not still_planted:
        problems.append("no FAT sector the copy did not write differs between the copies after the boot: "
                        "the planted marks did not survive, so the mirror check could not fail")
    for s in sorted(sectors):
        fs.fh.seek((fs.fat + s) * SECTOR)
        first = fs.fh.read(SECTOR)
        if any(struct.unpack_from("<I", first, j)[0] & 0x0FFFFFFF == 0 for j in range(0, SECTOR, 4)):
            open_sectors += 1
        for copy in range(1, fs.nfats):
            fs.fh.seek((fs.fat + copy * fs.fatsz + s) * SECTOR)
            if fs.fh.read(SECTOR) != first:
                problems.append(f"FAT sector {s} of copy {copy} is not copy 0's after the write: "
                                "write_fat_entry must write the first copy's sector to every copy")
    if not open_sectors:
        problems.append("no sector C32's chain wrote has a free entry left: the mirror check could not fail")
    return problems


def boot(name, prepared, cmd, inspect=None, setup=()):
    """One boot's answer (disktrace.boot's), or None for a firmware stall."""
    typed = list(setup) + [cmd]
    steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f")]
    steps += [("# ", c) for c in typed] + [("# ", "")]

    def said(lines, count):
        # Each `cp:` line is charged to the command typed before it.
        problems, current = [], None
        for ln in lines:
            current = next((c for c in typed if ln.rstrip().endswith(c)), current)
            if ln.startswith("cp:"):
                problems.append(f"`{current}` said: {ln.rstrip()}")
        return problems

    return disktrace.boot("test-write-walk", name, prepared, IMAGE, steps, inspect, said)


def grade(tally, name, prepared, base_cmd, cmd, region, inspect, setup=()):
    """Boot the pair, each after the commands `setup`, then compare the
    destination FAT's extra reads with the prediction `inspect(copy,
    problems, found)` leaves in `found` ({"want", "what"}), its problems the
    second boot's; print the verdict and close the case in `tally`, unless a
    boot stalled and neither failed (disktrace's Tally.settle)."""
    found = {}
    base_boot = boot(f"{name}-base", prepared, base_cmd, setup=setup)
    more_boot = boot(f"{name}-more", prepared, cmd, functools.partial(inspect, found=found), setup=setup)
    problems = tally.settle([base_boot, more_boot])
    if problems is None:
        return
    # With no problem, the second boot's inspect ran and left its prediction.
    delta, want = None, found.get("want")
    if base_boot and more_boot:
        lo, hi = region
        differ, per = disktrace.per_run(base_boot[2], more_boot[2], 1, region)
        problems += differ
        if per is not None:
            delta = sum(c for s, c in per.items() if lo <= s < hi)
        # A bug in this rig, not in fsd, if it does not hold.
        assert problems or want is not None, "the second boot ran with no problem, and its inspect left no prediction"
        if not problems and delta != want:
            problems.append(f"`{cmd}` reads {delta} FAT sectors, not {want}: one walk of each chain it "
                            "reads and writes, through the FAT cache, is the rule")
    verdict = "FAIL" if problems else "ok  "
    print(f"{verdict} {name}: `{cmd}` ({found.get('what', '?')}) reads {'?' if delta is None else delta} "
          f"sectors of the destination's FAT, {'?' if want is None else want} for one walk of each chain it "
          "reads and writes there")
    for p in problems:
        print(f"     {p}")
    tally.close(problems)


def main() -> int:
    data = bytes((i * 7 + i // 4096) & 0xFF for i in range(HUGE))
    files = {"HUGE": data, "EMPTY": b""}
    prepared = os.path.join(ROOT, "build", "test-write-walk-prepared.img")
    shutil.copyfile(os.path.join(ROOT, "build", IMAGE), prepared)
    tally = disktrace.Tally("test-write-walk")
    try:
        spawn_reads.place_mounted(prepared, files, part=1)
        spawn_reads.place_mounted(prepared, files, part=2)
        planted = plant_backup(prepared)
        print(f"     the ESP's second FAT copy differs from the first at {planted} free entries")
        g = spawn_reads.exfat_layout(prepared)
        f32 = Fat32(prepared)
        f32_region = (f32.fat, f32.fat_end)
        f32_root_len = len(f32.chain(f32.root))
        f32.fh.close()

        def exfat_inspect(copy, problems, found, src_name=None):
            root, chain = spawn_reads.exfat_chain(copy, "CHAIN")
            _, bin_chain = spawn_reads.exfat_chain(copy, "bin")
            src = spawn_reads.exfat_chain(copy, src_name)[1] if src_name else None
            if chain is None or bin_chain is None or (src_name and src is None):
                problems.append("/CHAIN, /bin or the source is contiguous: fsd writes every file FAT-chained, "
                                "and the model reads where the cache starts off /bin's chain")
                return
            sectors = {c * 4 // SECTOR for c in chain}
            cp = [i for i, n, _ in exfat_entries(copy, bin_chain) if n.upper() == "CP"]
            size = [n_len for _, n, n_len in exfat_entries(copy, root) if n == "CHAIN"]
            cluster_bytes = g["spc"] * SECTOR
            if len(root) > 1:
                problems.append(f"the exFAT root spans {len(root)} clusters: resolving /CHAIN may step along it")
            elif len(sectors) < 2:
                problems.append("/CHAIN's chain lies in one FAT sector: a walk from its start would cost no "
                                "more reads, so the cursor could not be seen")
            elif len(cp) != 1 or cp[0] == 0:
                problems.append("/bin/CP's entry is not past /bin's first cluster: where the FAT cache "
                                "starts the copy is not known from the disk")
            elif size != [HUGE] or len(chain) != -(-HUGE // cluster_bytes):
                problems.append(f"/CHAIN records {size} bytes in {len(chain)} clusters, not {HUGE} in "
                                f"{-(-HUGE // cluster_bytes)}")
            elif exfat_read(copy, chain, HUGE) != data:
                problems.append("/CHAIN does not hold /HUGE's bytes")
            elif src and len({c * 4 // SECTOR for c in src} | sectors) < 3:
                problems.append("the source's and the copy's chains lie in fewer than three FAT sectors: "
                                "a source walked from its start could cost no more reads")
            else:
                start = bin_chain[cp[0] - 1] * 4 // SECTOR
                found["want"] = exfat_reads(chain, g["spc"] * SECTOR, start, src)
                found["what"] = f"{len(chain)} clusters across {len(sectors)} FAT sectors"

        def fat32_inspect(copy, problems, found, src_name=None):
            fs = Fat32(copy)
            try:
                root = fs.chain(fs.root)
                listing = fs.listing(fs.root)
                dst = [e for e in listing if e[1] == "C32"]
                srcs = [e for e in listing if e[1] == src_name]
                if len(dst) != 1 or (src_name and len(srcs) != 1):
                    problems.append("no C32, or no source, in the ESP's root after the copy")
                    return
                chain = fs.chain(dst[0][2])
                mine = set(chain)
                sectors = {c * 4 // SECTOR for c in chain}
                problems += mirrored(fs, sectors)
                src, src_passed = None, ()
                if src_name:
                    # The source's chain and the root clusters its resolution
                    # steps past.
                    src, src_passed = fs.chain(srcs[0][2]), root[:srcs[0][0]]
                if len(root) != f32_root_len:
                    problems.append("the ESP's root grew during the copy: the model does not count that")
                elif len({c * 4 // SECTOR for c in root}) != 1:
                    problems.append("the ESP's root crosses a FAT sector: where the cache starts is not known")
                elif len(sectors) < 2:
                    problems.append("C32's chain lies in one FAT sector: the cursor could not be seen")
                elif dst[0][3] != HUGE or len(chain) != -(-HUGE // (fs.spc * SECTOR)):
                    problems.append(f"C32 records {dst[0][3]} bytes in {len(chain)} clusters, not {HUGE} "
                                    f"in {-(-HUGE // (fs.spc * SECTOR))}")
                elif fs.read(chain, HUGE) != data:
                    problems.append("C32 does not hold /HUGE's bytes")
                else:
                    want = fat32_reads(chain, fs.spc * SECTOR, lambda c: fs.entry(c) == 0 or c in mine,
                                       root[:dst[0][0]], root[0] * 4 // SECTOR,
                                       fs.cluster_end, src, src_passed)
                    if want is None:
                        problems.append("C32's chain is not the one the free-cluster scan chooses: the "
                                        "model does not describe this copy")
                    else:
                        found["want"] = want
                        found["what"] = f"{len(chain)} clusters across {len(sectors)} FAT sectors"
            finally:
                fs.fh.close()

        grade(tally, "exfat", prepared, "cp /mnt/f/EMPTY /CEMPTY", "cp /mnt/f/HUGE /CHAIN",
              (g["fat"], g["fat_end"]), exfat_inspect)
        grade(tally, "fat32", prepared, "cp /EMPTY /mnt/f/CEMPTY", "cp /HUGE /mnt/f/C32",
              f32_region, fat32_inspect)
        # Within one volume. On exFAT the source is made FAT-chained by a copy
        # from the other first, in both boots of the pair (macOS wrote /HUGE
        # contiguous, which reads no FAT); every FAT32 file is chained.
        grade(tally, "exfat-same", prepared, "cp /EMPTY /CEMPTY", "cp /SRC /CHAIN",
              (g["fat"], g["fat_end"]), functools.partial(exfat_inspect, src_name="SRC"),
              setup=["cp /mnt/f/HUGE /SRC"])
        grade(tally, "fat32-same", prepared, "cp /mnt/f/EMPTY /mnt/f/CEMPTY", "cp /mnt/f/HUGE /mnt/f/C32",
              f32_region, functools.partial(fat32_inspect, src_name="HUGE"))
    finally:
        os.remove(prepared)
    return tally.verdict()


if __name__ == "__main__":
    sys.exit(main())

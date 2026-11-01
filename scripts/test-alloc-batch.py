#!/usr/bin/env python3
"""exFAT sets a request's FAT entries and bits a sector at a time.

    python3 scripts/test-alloc-batch.py    (or `make test-alloc-batch`, which builds the image)

Until 2026-10-31 every FAT entry exFAT set was a FAT sector write of its
own, and every cluster allocated a bitmap read and write, the scan starting
at the bitmap's first byte. A request allocating hundreds of clusters (a gap
fill of up to MAX_GAP_FILL, 1 MiB) or linking a contiguous file as a chain
(`ensure_chained`, at the first append to a file another driver wrote,
thousands of clusters on a camera's file) was one USB round trip an entry
on the Pi's stick, and could pass the supervisor's wedge limit. Such a
request is now a FAT batch (`in_fat_batch`): the entries are set in the FAT
cache and its sector written when the cache moves to another or the batch
ends, the allocator's bits held in one bitmap sector and written before any
FAT sector, and the scan going on from its last find. Only the disk can show
the difference, so this rig counts QEMU's virtio-blk writes
(scripts/disktrace.py).

On a copy of build/espexfat.img the host plants /CONT, 2 MiB, on the exFAT
root as a contiguous (NoFatChain) run at the heap's end, read back and
fsck_exfat -n clean (macOS's driver wrote it FAT-chained into a hole). Both
boots of the pair, the exFAT root at `/`, fill the free clusters in the
bitmap's first sector with files made by `writeat` gap fills, all but 128,
and `touch /G`. The graded boot then runs

- `writeat /G 1048000 x`: 256 clusters in one request, the first 128 the
  last free in the bitmap's first sector and the rest in its second, so the
  batch crosses a bitmap sector and its scan's start is seen: a scan from
  byte 0 reads the first sector again for every cluster past it;
- `echo x >> /CONT`: `ensure_chained` links /CONT's 512 clusters over four
  FAT sectors, then the append takes a cluster.

The FAT writes, bitmap writes and bitmap reads the graded boot makes more
than the base must be what a model of the batch predicts from the chains the
host reads off the graded boot's disk, and those writes and the two sets'
(the root sectors holding /G's and /CONT's File and Stream entries) must be
the boot's last to those sectors in exactly the model's order. The model
holds the order the batch promises: every cluster's bit on the disk before
any FAT sector linking to it is written, and the set naming the chain after
the chain's last write. Then /G is 1048001 bytes, zeros and `x`, /CONT its
bytes and `x\\r\\n`, FAT-chained, and fsck_exfat -n clean. Each case must cross
what it stands for, or the counts could not tell a sector from a cluster:
/G both bitmap sectors and two FAT sectors, /CONT four FAT sectors.

Measured on 2026-10-31: 14 FAT writes, 5 bitmap writes and 4 bitmap reads,
the 21 writes in the model's order. Controls, each failing here: main's
exfat.rs before that day (1,025 FAT writes, 257 bitmap writes, 386 bitmap
reads); the batch's scan starting at byte 0 (131 bitmap writes and 258
reads, the first sector read again a cluster past it); flush_fat writing the
FAT sector before the allocator's bits (the order alone, the counts as the
model's); write_at's set written inside the batch (the order alone); and
in_fat_batch not writing what is pending at its end (/CONT's chain lost, the
fill short, fsck's bitmap complaint). The model also asserts that every link
reaches the disk after the entry it names (the medium review of #380). That
holds of links alone: a chain's head is no link, its end of chain waits in the
cache too, so a refused first flush leaves the head's old entry on the disk.
A refused batch is make test-batch-refused's: the give-back that does not
walk such a head, and the cursors dropped with a refused sector.

A boot QEMU's firmware never starts is INCONCLUSIVE (exit 100, drive-qemu.py's
classifier through disktrace.py), unless the other boot of the pair failed,
which fails it.

Two boots, about three minutes.
"""
import collections
import functools
import os
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disktrace  # noqa: E402
import exfatimg  # noqa: E402

RIG = "test-alloc-batch"
SECTOR = 512
IMAGE = "espexfat.img"
CONT = 2 << 20
GAP = 1048000  # /G's gap fill: 256 clusters of 4 KiB, MAX_GAP_FILL is 1 MiB
LEFT = 128  # free clusters left in the bitmap's first sector before /G
BITS = SECTOR * 8


class Model:
    """The batch as fsd runs it: one FAT sector held, written before another
    is read when it holds entries set (the allocator's bitmap sector first,
    when it holds bits set), and at the batch's end; one bitmap sector held
    by the allocator, read when its scan enters it, written when it leaves it
    with bits set. `seq` is the writes in order, `reads` the bitmap reads by
    sector. Raises AssertionError if a FAT sector is written with a link to
    a cluster whose bit is not on the disk, or to one allocated here whose
    own entry, in another sector, is not on the disk yet (a refused flush
    would then leave a link on the disk into a stale entry), so the model
    holds both rules. The chain's head is no link, and is not checked here:
    test-batch-refused grades a refused head."""

    def __init__(self, fat_lba, bitmap_lba):
        self.fat_lba, self.bitmap_lba = fat_lba, bitmap_lba
        self.seq, self.reads = [], collections.Counter()
        self.fat, self.fat_dirty, self.links = None, False, set()
        self.allocated, self.on_disk = set(), set()
        self.entries, self.entries_on_disk = set(), set()

    def fsec(self, c):
        return self.fat_lba + c * 4 // SECTOR

    def bsec(self, byte):
        return self.bitmap_lba + byte // SECTOR

    def begin(self):
        self.bits, self.bits_dirty, self.bits_held, self.scan_from = None, False, set(), 0

    def write_bits(self):
        if self.bits_dirty:
            self.seq.append(self.bits)
            self.on_disk |= self.bits_held
            self.bits_dirty = False

    def flush_fat(self):
        self.write_bits()
        missing = (self.links & self.allocated) - self.on_disk
        assert not missing, f"FAT sector {self.fat} written linking to {sorted(missing)[:3]} before their bits"
        early = {t for t in self.links & self.allocated
                 if self.fsec(t) != self.fat and t not in self.entries_on_disk}
        assert not early, f"FAT sector {self.fat} written linking to {sorted(early)[:3]} before their entries"
        self.seq.append(self.fat)
        self.entries_on_disk |= self.entries
        self.fat_dirty, self.links, self.entries = False, set(), set()

    def touch(self, c):
        """`next_cluster(c)`: the FAT sector of `c`, through the cache."""
        s = self.fsec(c)
        if self.fat_dirty and self.fat != s:
            self.flush_fat()
        self.fat = s

    def set(self, c, value):
        self.touch(c)  # patch_fat_entry asks next_cluster first
        self.fat_dirty = True
        self.entries.add(c)
        if value is not None:
            self.links.add(value)

    def alloc(self, c):
        target = (c - 2) // 8
        for byte_sector in range(self.scan_from // SECTOR, target // SECTOR + 1):
            s = self.bitmap_lba + byte_sector
            if self.bits != s:
                self.write_bits()
                self.bits, self.bits_held = s, set()
                self.reads[s] += 1
        self.bits_held.add(c)
        self.allocated.add(c)
        self.bits_dirty = True
        self.scan_from = target
        self.set(c, c)  # its end of chain; `c` itself must be on the disk first

    def end(self, set_sectors):
        if self.fat_dirty:
            self.flush_fat()
        else:
            self.write_bits()
        self.seq.extend(set_sectors)


def main() -> int:
    prepared = os.path.join(ROOT, "build", f"{RIG}-prepared.img")
    shutil.copyfile(os.path.join(ROOT, "build", IMAGE), prepared)
    tally = disktrace.Tally(RIG)
    try:
        g = exfatimg.exfat_layout(prepared)
        cluster = g["spc"] * SECTOR
        start, _ = exfatimg.partition(prepared, 0x07)
        cont = bytes((i * 7 + i // 1000) & 0xFF for i in range(CONT))
        pre = []
        with exfatimg.Volume(prepared, start) as vol:
            bitmap_lba = g["heap"] + (vol.bitmap - 2) * g["spc"]
            # /CONT contiguous at the heap's end, past every cluster the boot
            # allocates (macOS's driver wrote it FAT-chained into a hole).
            run = list(range(vol.count + 2 - CONT // cluster, vol.count + 2))
            if any(vol.used(c) for c in run):
                pre.append(f"the heap's last {len(run)} clusters are not free for /CONT")
            rchain = vol.chain(vol.root)
            d = bytearray(vol.data(rchain))
            end = exfatimg.root_end(d)
            heap_off, bitmap_off = vol.heap, vol.heap + (vol.bitmap - 2) * vol.cluster
        st = bytearray(exfatimg.entry_set("CONT"))
        st[33] = 0x03  # GeneralSecondaryFlags: AllocationPossible, NoFatChain
        struct.pack_into("<Q", st, 40, CONT)  # ValidDataLength
        struct.pack_into("<I", st, 52, run[0])
        struct.pack_into("<Q", st, 56, CONT)  # DataLength
        struct.pack_into("<H", st, 2, exfatimg.set_checksum(st))
        if end + len(st) + 32 > len(d):
            pre.append("no room in the root for /CONT's set")
        if not pre:
            d[end:end + len(st)] = st
            with open(prepared, "r+b") as fh:
                for k, c in enumerate(rchain):
                    fh.seek(heap_off + (c - 2) * cluster)
                    fh.write(d[k * cluster:(k + 1) * cluster])
                for c in run:
                    fh.seek(bitmap_off + (c - 2) // 8)
                    b = fh.read(1)[0]
                    fh.seek(-1, 1)
                    fh.write(bytes([b | 1 << (c - 2) % 8]))
                fh.seek(heap_off + (run[0] - 2) * cluster)
                fh.write(cont)
        with exfatimg.Volume(prepared, start) as vol:
            listed = {exfatimg.fold(k): v for k, v in vol.listing(vol.root).items()}.get("cont")
            if not pre and (listed != (run[0], CONT, False) or vol.data(run) != cont
                            or not all(vol.used(c) for c in run)):
                pre.append(f"/CONT does not read back as planted: {listed}")
            free_low = [c for c in range(2, 2 + BITS) if c < vol.count + 2 and not vol.used(c)]
        if not pre:
            ok, lines = exfatimg.fsck_clean(prepared)
            if not ok:
                pre.append("fsck_exfat -n is not clean with /CONT planted: " + " | ".join(lines[-6:]))
        fill = len(free_low) - LEFT
        fat_region = (g["fat"], g["fat_end"])
        bitmap_region = (bitmap_lba, bitmap_lba + -(-g["count"] // BITS))

        setup = []
        per = (GAP + cluster) // cluster  # clusters one full gap fill takes
        k = 0
        while fill > 0:
            n = min(per, fill)
            setup.append(f"touch /F{k}")
            setup.append(f"writeat /F{k} {GAP if n == per else (n - 1) * cluster} f")
            fill -= n
            k += 1
        setup.append("touch /G")
        graded = [f"writeat /G {GAP} x", "echo x >> /CONT"]

        def steps(cmds):
            out = [("login:", "root"), ("assword", "root")]
            out += [("# ", c) for c in cmds]
            return out + [("# ", "")]

        def said(typed, lines, count):
            problems, current = [], None
            for ln in lines:
                current = next((c for c in typed if ln.rstrip().endswith(c)), current)
                if current and ln.startswith(("writeat:", "echo:", "touch:", "sh:", "fsd:")):
                    problems.append(f"`{current}` said: {ln.rstrip()}")
            return problems

        found = {}

        def base_inspect(copy, problems):
            with exfatimg.Volume(copy, start) as v:
                left = [c for c in range(2, 2 + BITS) if c < v.count + 2 and not v.used(c)]
                if len(left) != LEFT:
                    problems.append(f"the fill left {len(left)} clusters free in the bitmap's first sector, "
                                    f"not {LEFT}: the root grew, or a file took other than its count")

        def more_inspect(copy, problems):
            with exfatimg.Volume(copy, start) as v:
                try:
                    gch, cch = v.file_chain("/G"), v.file_chain("/CONT")
                except ValueError as e:
                    problems.append(f"after the boot: {e}")
                    return
                gdata = v.data(gch)[:GAP + 1]
                if len(gch) != per or gdata != bytes(GAP) + b"x":
                    problems.append(f"/G is {len(gch)} clusters, not {per}, or not {GAP} zeros and x")
                if cch[:len(run)] != run or len(cch) != len(run) + 1:
                    problems.append(f"/CONT's chain is not its run of {len(run)} and one cluster after")
                elif v.data(cch)[:CONT + 3] != cont + b"x\r\n":
                    problems.append("/CONT does not hold its bytes and x\\r\\n")
                rchain = v.chain(v.root)
                rdata = v.data(rchain)
                sets = {}
                for name in ("G", "CONT"):
                    hit = exfatimg.find(rdata, name)
                    if hit is None:
                        problems.append(f"/{name}'s set is not in the root")
                        return
                    secs = []
                    for off in (hit.offset, hit.offset + 32):
                        sec = g["heap"] + (rchain[off // cluster] - 2) * g["spc"] + off % cluster // SECTOR
                        if sec not in secs:
                            secs.append(sec)
                    sets[name] = secs
            gbits = {(c - 2) // BITS for c in gch}
            gfat = {g["fat"] + c * 4 // SECTOR for c in gch}
            cfat = {g["fat"] + c * 4 // SECTOR for c in run}
            if len(gbits) != 2 or len(gfat) < 2:
                problems.append(f"/G is in {len(gbits)} bitmap sectors and {len(gfat)} FAT sectors, not two "
                                "and two or more: the scan's start and a sector at a time could not be seen")
            if len(cfat) < 4:
                problems.append(f"/CONT's run is in {len(cfat)} FAT sectors, not four or more")
            found.update(g_chain=gch, c_new=cch[-1], sets=sets)
            ok, lines = exfatimg.fsck_clean(copy)
            if not ok:
                problems.append("fsck_exfat -n is not clean after the boot: " + " | ".join(lines[-6:]))

        base_writes, more_writes, more_order = collections.Counter(), collections.Counter(), []
        if pre:
            problems = pre
            base = more = None
        else:
            base = disktrace.boot(RIG, "base", prepared, IMAGE, steps(setup), base_inspect,
                                  functools.partial(said, setup), writes=base_writes)
            more = disktrace.boot(RIG, "more", prepared, IMAGE, steps(setup + graded), more_inspect,
                                  functools.partial(said, setup + graded), writes=more_writes, order=more_order)
            problems = tally.settle([base, more])
            if problems is None:
                return tally.verdict()
        got = {}
        if base and more and "sets" in found and not problems:
            m = Model(g["fat"], bitmap_lba)
            # writeat /G: the head allocated, then each step past a cluster's
            # end asks its FAT entry, allocates and links it.
            m.begin()
            gch = found["g_chain"]
            m.alloc(gch[0])
            for prev, c in zip(gch, gch[1:]):
                m.touch(prev)
                m.alloc(c)
                m.set(prev, c)
            m.end(found["sets"]["G"])
            # echo >> /CONT: ensure_chained links the run, the walk asks
            # every entry, and the last step allocates and links one.
            m.begin()
            for i, c in enumerate(run):
                m.set(c, c + 1 if i + 1 < len(run) else None)
            for c in run:
                m.touch(c)
            m.alloc(found["c_new"])
            m.set(run[-1], found["c_new"])
            m.end(found["sets"]["CONT"])
            seq = m.seq
            set_secs = set(found["sets"]["G"]) | set(found["sets"]["CONT"])
            want_fat, want_bits = collections.Counter(), collections.Counter()
            for sec in seq:
                if fat_region[0] <= sec < fat_region[1]:
                    want_fat[sec] += 1
                elif bitmap_region[0] <= sec < bitmap_region[1]:
                    want_bits[sec] += 1
            for what, base_c, more_c, region, want in (
                    ("FAT writes", base_writes, more_writes, fat_region, want_fat),
                    ("bitmap writes", base_writes, more_writes, bitmap_region, want_bits),
                    ("bitmap reads", base[2], more[2], bitmap_region, m.reads)):
                differ, per_sec = disktrace.per_run(base_c, more_c, 1, region)
                problems += [f"{what}: {d}" for d in differ]
                if per_sec is None:
                    continue
                got[what] = (sum(per_sec.values()), sum(want.values()))
                if per_sec != dict(want):
                    problems.append(f"the two requests make {sum(per_sec.values())} {what} "
                                    f"({dict(sorted(per_sec.items()))}), not {sum(want.values())} "
                                    f"({dict(sorted(want.items()))}): a sector at a time is the rule")
            ours = [w for w in more_order if fat_region[0] <= w < fat_region[1]
                    or bitmap_region[0] <= w < bitmap_region[1] or w in set_secs]
            tail = ours[len(ours) - len(seq):]
            if not seq:
                problems.append("the model expects no writes: the rig built nothing to allocate")
            elif tail != seq:
                at = next((i for i, (x, y) in enumerate(zip(tail, seq)) if x != y), min(len(tail), len(seq)))
                problems.append(f"the two requests' writes are not in the batch's order (bits, then the "
                                f"links to them, then the set): from write {at} of {len(seq)}, "
                                f"{tail[at:at + 4]} where the model has {seq[at:at + 4]}")
            else:
                got["in order"] = (len(seq), len(seq))
        verdict = "FAIL" if problems else "ok  "
        counts = ", ".join(f"{g_} {w} (want {m_})" for w, (g_, m_) in got.items() if w != "in order") or "not compared"
        if "in order" in got:
            counts += f", the {got['in order'][0]} writes in the model's order"
        print(f"{verdict} alloc: a 1 MiB gap fill across a bitmap sector and a 2 MiB contiguous file's "
              f"first append: {counts}")
        for p in problems:
            print(f"     {p}")
        tally.close(problems)
    finally:
        os.remove(prepared)
    return tally.verdict()


if __name__ == "__main__":
    sys.exit(main())

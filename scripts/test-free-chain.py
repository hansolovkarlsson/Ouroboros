#!/usr/bin/env python3
"""exFAT frees a FAT-chained file a sector at a time, not a cluster at a time.

    python3 scripts/test-free-chain.py    (or `make test-free-chain`, which builds the image)

Until 2026-10-31 `free_data`'s chained walk read and wrote a bitmap sector
and wrote a FAT sector for every cluster it freed, three round trips a
cluster on the Pi's USB stick, so an `rm` of a large file could pass the
supervisor's wedge limit and have fsd restarted in the middle of the free.
It now holds one FAT sector and one bitmap sector and writes each when the
walk leaves it. No program's output can show the difference; only the disk
can, so this rig counts QEMU's virtio-blk writes (scripts/disktrace.py).

On a copy of build/espexfat.img the host puts /BIG (1 MiB) and /BLK (one
cluster) on the ESP. Both boots of the pair make the same files on the exFAT
root, mounted as `/`, with the ESP mounted at /mnt/f:

- /CHAIN, by `cp /mnt/f/BIG /CHAIN` (fsd writes every file FAT-chained);
- /P and /Q, a cluster at a time in turn by `cat /mnt/f/BLK >> /P` and
  `>> /Q`, so their chains interleave and share their FAT and bitmap sectors;
- /E1, /E2 and /E3, empty, by `touch`.

The base boot then removes the three empty files, which hold no cluster, and
the graded boot removes /CHAIN, /P and /Q. The FAT and bitmap sectors the
graded boot writes and reads more than the base must be what free_chain's
rule predicts from the three chains the host reads off the base boot's
disk: one read of each bitmap sector a walk enters, one write of each FAT
sector, and the bitmap sector written again before each FAT sector, so that
a cluster's cleared bit is on the disk before its zeroed FAT entry. A free
cut short (fsd restarted by the supervisor mid-free) then leaves a free
cluster with a stale link, which exFAT ignores, never a zeroed entry with
its bit still set, a leak until a fsck. The frees' writes, the boot's last
to those two regions in the order QEMU handled them, must be the model's
sequence exactly, and the model refuses to build a sequence that breaks
that order. After the graded boot every cluster of the three chains must be
clear in the bitmap and 0 in the FAT, the files gone and fsck_exfat -n
clean. Each chain must cross the case it stands for: /CHAIN two FAT
sectors, /P and /Q a FAT sector each other's clusters are in, or the count
could not tell a sector from a cluster.

Measured on 2026-10-31: the 280 clusters take 10 FAT writes, 3 bitmap reads
and MEASURED bitmap writes. Controls, each failing here: main's walk before
that day (280 FAT writes, 280 bitmap reads and writes); #379's first
version, which wrote a FAT sector before the bitmap sector holding its
clusters' bits (the order; the medium review of #379); and free_chain's
last FAT write removed (14 of the 280 clusters left set).

A boot QEMU's firmware never starts is INCONCLUSIVE (exit 100, drive-qemu.py's
classifier through disktrace.py), unless the other boot of the pair failed,
which fails it.

Two boots, about two minutes.
"""
import collections
import functools
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disktrace  # noqa: E402
import exfatimg  # noqa: E402

RIG = "test-free-chain"
SECTOR = 512
IMAGE = "espexfat.img"
BIG = 1 << 20
APPENDS = 12  # clusters each of /P and /Q
FREED = ("CHAIN", "P", "Q")
EMPTY = ("E1", "E2", "E3")


def model(chains, fat_lba, bitmap_lba):
    """The frees of `chains`, one after another, as free_chain makes them:
    (the FAT and bitmap sectors written, in order, {bitmap sector: reads}).
    A walk holds one FAT sector and one bitmap sector; before it writes the
    FAT sector it is leaving, it writes the bitmap sector if it has bits
    cleared since its last write; it writes the bitmap sector it is leaving
    the same way, reads the one it enters, and at its end writes the bitmap
    sector and then the FAT sector. Raises AssertionError if the order it
    builds ever writes a FAT sector zeroing a cluster whose cleared bit is
    not yet on the disk, so the model holds the rule the rig grades."""
    seq, reads = [], {}
    cleared_on_disk = set()
    for chain in chains:
        bsec, bdirty, bheld, fsec, zeroed = None, False, set(), None, set()

        def write_bits():
            nonlocal bdirty
            if bdirty:
                seq.append(bsec)
                cleared_on_disk.update(bheld)
                bdirty = False

        def write_fat():
            missing = zeroed - cleared_on_disk
            assert not missing, f"FAT sector {fsec} written before the bits of {sorted(missing)[:3]}"
            seq.append(fsec)
            zeroed.clear()

        for c in chain:
            f, b = fat_lba + c * 4 // SECTOR, bitmap_lba + (c - 2) // (SECTOR * 8)
            if fsec is not None and fsec != f:
                write_bits()
                write_fat()
            if bsec != b:
                write_bits()
                bsec, bheld = b, set()
                reads[b] = reads.get(b, 0) + 1
            bheld.add(c)
            bdirty = True
            fsec = f
            zeroed.add(c)
        write_bits()
        if fsec is not None:
            write_fat()
    return seq, reads


def main() -> int:
    prepared = os.path.join(ROOT, "build", f"{RIG}-prepared.img")
    shutil.copyfile(os.path.join(ROOT, "build", IMAGE), prepared)
    tally = disktrace.Tally(RIG)
    try:
        g = exfatimg.exfat_layout(prepared)
        cluster = g["spc"] * SECTOR
        start, _ = exfatimg.partition(prepared, 0x07)
        with exfatimg.Volume(prepared, start) as vol:
            bitmap_lba = g["heap"] + (vol.bitmap - 2) * g["spc"]
        big = bytes((i * 13 + i // 4096) & 0xFF for i in range(BIG))
        exfatimg.place_mounted(prepared, {"BIG": big, "BLK": b"b" * cluster}, part=2)
        fat_region = (g["fat"], g["fat_end"])
        bitmap_region = (bitmap_lba, bitmap_lba + -(-g["count"] // (SECTOR * 8)))

        setup = ["mount 1 /mnt/f", "cp /mnt/f/BIG /CHAIN"]
        for _ in range(APPENDS):
            setup += ["cat /mnt/f/BLK >> /P", "cat /mnt/f/BLK >> /Q"]
        setup += [f"touch /{e}" for e in EMPTY]

        def steps(removed):
            out = [("login:", "root"), ("assword", "root")]
            out += [("# ", c) for c in setup + [f"rm /{f}" for f in removed]]
            return out + [("# ", "")]

        def said(typed, lines, count):
            # A complaint is charged to the command typed before it; fsd's
            # boot lines come before the first.
            problems, current = [], None
            for ln in lines:
                current = next((c for c in typed if ln.rstrip().endswith(c)), current)
                if current and ln.startswith(("cp:", "cat:", "rm:", "touch:", "fsd:")):
                    problems.append(f"`{current}` said: {ln.rstrip()}")
            return problems

        found = {}

        def base_inspect(copy, problems):
            with exfatimg.Volume(copy, start) as v:
                chains = {f: v.file_chain(f"/{f}") for f in FREED}
                if v.data(chains["CHAIN"])[:BIG] != big:
                    problems.append("/CHAIN does not hold /BIG's bytes")
            fsec = {f: {g["fat"] + c * 4 // SECTOR for c in ch} for f, ch in chains.items()}
            if len(chains["CHAIN"]) != BIG // cluster or len(fsec["CHAIN"]) < 2:
                problems.append(f"/CHAIN is {len(chains['CHAIN'])} clusters in {len(fsec['CHAIN'])} FAT "
                                f"sectors, not {BIG // cluster} in two or more: a sector at a time could "
                                "not be told from a cluster at a time")
            if any(len(chains[f]) != APPENDS for f in ("P", "Q")):
                problems.append(f"/P or /Q is not {APPENDS} clusters")
            elif not (set(chains["P"]) & {c - 1 for c in chains["Q"]} or set(chains["Q"]) & {c - 1 for c in chains["P"]}):
                problems.append("/P and /Q do not interleave: the appends in turn did not share their sectors")
            elif not fsec["P"] & fsec["Q"]:
                problems.append("/P and /Q share no FAT sector")
            found["chains"] = chains

        def more_inspect(copy, problems):
            chains = found.get("chains")
            if chains is None:
                problems.append("the base boot left no chains to check the frees against")
                return
            with exfatimg.Volume(copy, start) as v:
                names = {exfatimg.fold(n) for n in v.listing(v.root)}
                for f in FREED:
                    if exfatimg.fold(f) in names:
                        problems.append(f"/{f} is still in the root after its rm")
                    left = [c for c in chains[f] if v.used(c) or v.entry(c) != 0]
                    if left:
                        problems.append(f"{len(left)} of /{f}'s {len(chains[f])} clusters are still set in the "
                                        f"bitmap or the FAT after its rm, first {left[0]}")
            ok, lines = exfatimg.fsck_clean(copy)
            if not ok:
                problems.append("fsck_exfat -n is not clean after the frees: " + " | ".join(lines[-6:]))

        base_writes, more_writes, more_order = collections.Counter(), collections.Counter(), []
        base_typed = setup + [f"rm /{f}" for f in EMPTY]
        more_typed = setup + [f"rm /{f}" for f in FREED]
        base = disktrace.boot(RIG, "base", prepared, IMAGE, steps(EMPTY), base_inspect,
                              functools.partial(said, base_typed), writes=base_writes)
        more = disktrace.boot(RIG, "more", prepared, IMAGE, steps(FREED), more_inspect,
                              functools.partial(said, more_typed), writes=more_writes, order=more_order)
        problems = tally.settle([base, more])
        if problems is None:
            return tally.verdict()
        got = {}
        if base and more and "chains" in found and not problems:
            seq, want_reads = model([found["chains"][f] for f in FREED], g["fat"], bitmap_lba)
            want_fat, want_bits = collections.Counter(), collections.Counter()
            for sec in seq:
                (want_fat if fat_region[0] <= sec < fat_region[1] else want_bits)[sec] += 1
            for what, base_c, more_c, region, want in (
                    ("FAT writes", base_writes, more_writes, fat_region, want_fat),
                    ("bitmap writes", base_writes, more_writes, bitmap_region, want_bits),
                    ("bitmap reads", base[2], more[2], bitmap_region, want_reads)):
                differ, per = disktrace.per_run(base_c, more_c, 1, region)
                problems += [f"{what}: {d}" for d in differ]
                if per is None:
                    continue
                got[what] = (sum(per.values()), sum(want.values()))
                if per != dict(want):
                    problems.append(f"the frees make {sum(per.values())} {what} ({dict(sorted(per.items()))}), "
                                    f"not {sum(want.values())} ({dict(sorted(want.items()))}): one a sector "
                                    "a walk enters, and the bitmap's again before each FAT sector, is the rule")
            # The frees are the boot's last writes to the FAT and the bitmap.
            ours = [w for w in more_order
                    if fat_region[0] <= w < fat_region[1] or bitmap_region[0] <= w < bitmap_region[1]]
            tail = ours[-len(seq):]
            if tail != seq:
                at = next((i for i, (x, y) in enumerate(zip(tail, seq)) if x != y), min(len(tail), len(seq)))
                problems.append(f"the frees' FAT and bitmap writes are not in the order that puts every bit "
                                f"on the disk before its FAT entry's zero: from write {at} of {len(seq)}, "
                                f"{tail[at:at + 4]} where the rule has {seq[at:at + 4]}")
            else:
                got["in order"] = (len(seq), len(seq))
        clusters = sum(len(c) for c in found.get("chains", {}).values())
        verdict = "FAIL" if problems else "ok  "
        counts = ", ".join(f"{g_} {w} (want {m})" for w, (g_, m) in got.items() if w != "in order") or "not compared"
        if "in order" in got:
            counts += f", the {got['in order'][0]} writes in the rule's order"
        print(f"{verdict} free: `rm` of /CHAIN, /P and /Q, {clusters} clusters: {counts}")
        for p in problems:
            print(f"     {p}")
        tally.close(problems)
    finally:
        os.remove(prepared)
    return tally.verdict()


if __name__ == "__main__":
    sys.exit(main())

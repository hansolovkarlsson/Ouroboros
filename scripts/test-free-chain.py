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
graded boot writes more than the base must be what one write a sector the
walk passes through predicts, from the three chains the host reads off the
base boot's disk: a chain's FAT writes are the number of times its walk
enters a FAT sector, and its bitmap writes and reads the number of times it
enters a bitmap sector. On the tree of 2026-10-31 the 280 clusters take
10 FAT writes and 3 bitmap reads and writes; main's walk before that day
made 280 of each, and fails here. After the graded boot
every cluster of the three chains must be clear in the bitmap and 0 in the
FAT, the files gone and fsck_exfat -n clean, so a walk that leaves its last
sector unwritten fails too (with free_chain's last FAT write removed, 14 of
the 280 clusters were left set). Each chain must cross the case it stands for:
/CHAIN two FAT sectors, /P and /Q a FAT sector each other's clusters are in,
or the count could not tell a sector from a cluster.

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
    """{sector: writes} the frees of `chains`, one after another, make in
    the FAT and the bitmap, and {sector: reads} in the bitmap, one write a
    sector a walk enters (and one read, for the bitmap)."""
    fat, bits = {}, {}
    for chain in chains:
        fwalk = [fat_lba + c * 4 // SECTOR for c in chain]
        bwalk = [bitmap_lba + (c - 2) // (SECTOR * 8) for c in chain]
        for walk, out in ((fwalk, fat), (bwalk, bits)):
            for i, s in enumerate(walk):
                if i == 0 or s != walk[i - 1]:
                    out[s] = out.get(s, 0) + 1
    return fat, bits


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

        base_writes, more_writes = collections.Counter(), collections.Counter()
        base_typed = setup + [f"rm /{f}" for f in EMPTY]
        more_typed = setup + [f"rm /{f}" for f in FREED]
        base = disktrace.boot(RIG, "base", prepared, IMAGE, steps(EMPTY), base_inspect,
                              functools.partial(said, base_typed), writes=base_writes)
        more = disktrace.boot(RIG, "more", prepared, IMAGE, steps(FREED), more_inspect,
                              functools.partial(said, more_typed), writes=more_writes)
        problems = tally.settle([base, more])
        if problems is None:
            return tally.verdict()
        got = {}
        if base and more and "chains" in found and not problems:
            want_fat, want_bits = model([found["chains"][f] for f in FREED], g["fat"], bitmap_lba)
            for what, base_c, more_c, region, want in (
                    ("FAT writes", base_writes, more_writes, fat_region, want_fat),
                    ("bitmap writes", base_writes, more_writes, bitmap_region, want_bits),
                    ("bitmap reads", base[2], more[2], bitmap_region, want_bits)):
                differ, per = disktrace.per_run(base_c, more_c, 1, region)
                problems += [f"{what}: {d}" for d in differ]
                if per is None:
                    continue
                got[what] = (sum(per.values()), sum(want.values()))
                if per != want:
                    problems.append(f"the frees make {sum(per.values())} {what} ({dict(sorted(per.items()))}), "
                                    f"not {sum(want.values())} ({dict(sorted(want.items()))}): one a sector "
                                    "a walk enters is the rule")
        clusters = sum(len(c) for c in found.get("chains", {}).values())
        verdict = "FAIL" if problems else "ok  "
        counts = ", ".join(f"{g_} {w} (want {m})" for w, (g_, m) in got.items()) or "not compared"
        print(f"{verdict} free: `rm` of /CHAIN, /P and /Q, {clusters} clusters: {counts}")
        for p in problems:
            print(f"     {p}")
        tally.close(problems)
    finally:
        os.remove(prepared)
    return tally.verdict()


if __name__ == "__main__":
    sys.exit(main())

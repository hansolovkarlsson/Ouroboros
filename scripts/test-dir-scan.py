#!/usr/bin/env python3
"""Making files in an exFAT directory reads its sectors a few passes a file,
not one a slot.

    python3 scripts/test-dir-scan.py     (or `make test-dir-scan`, which builds the image)

To create a file, exFAT's `find_free_run` scans the directory for a run of
free slots, and `write_set_at`, `delete_set` and `patch_stream_ext` address
slots one by one. Until 2026-10-19 every slot was found by walking the
directory's chain from its first cluster, and the scan read the slot's whole
sector for each slot it tested: sixteen reads of a sector where one serves.
Now one cursor steps through a walk and the scan holds the sector it read.

Two boots of a copy of build/espexfat.img (exFAT root), the same but for the
count: `dirprobe /D 1` and `dirprobe /D 100` (/bin/DIRPROBE: make the files,
then open, list and touch each). QEMU counts every virtio-blk read
(scripts/disktrace.py); after the second boot the host reads /D's chain off
the disk. Graded: the reads of /D's own sectors in the second boot, against
W, the sum over the 100 creates of the sectors holding an entry when each
began (each file's set is three slots, F000 to F099, so ceil(3i/16), and at
least one), the cost of one pass over what the directory held a create; and
the writes to /D's sectors, at most two and a half a file (its set written
once a sector, its Stream patched once, the growths' zeroing: 1.36 on the
tree of 2026-10-19), where a set written a slot at a time was three by
itself (3.24 a file). The rig prints how many more sectors the
100-file boot read in all than the 1-file boot (the records' 4,686 against
19,026). The reads
must be at most PASSES x W: fsd now makes a few passes a create (the
existence check, the free-slot scan, the set's write) and the probe a few
more over the finished directory. Fails with the scan reading a sector a
slot again (the main tree before 2026-10-19: 18,496 reads, 19.0 passes,
against 4,152 and 4.3 after). It grades the directory's sectors only: the
cursor's saving is chain walks, whose FAT lookups mostly hit fsd's one-sector
FAT cache, which no disk counter sees, so it is not graded. Reports both numbers.
A boot QEMU's firmware never starts is INCONCLUSIVE (exit 100,
drive-qemu.py's classifier, through disktrace.py), and so is its run,
unless another of its boots ran and failed, which fails it. About a
minute and a half. Run it whenever exFAT's find_free_run, slot_at,
rewrite_slots, write_set_at, delete_set, grow_dir, walk_dir or room changes.
"""
import collections
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import exfatimg  # noqa: E402
import mbrdisk  # noqa: E402


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


disktrace = load("disktrace", "disktrace.py")
stale_cursor = load("test_stale_cursor", "test-stale-cursor.py")

SOURCE = os.path.join(ROOT, "build", "espexfat.img")
COUNT = 100
SET = 3  # slots a file's set takes: File, Stream, one name entry (F000)
SLOTS_PER_SECTOR = 16
PASSES = 8
# A file's set is written once a sector it touches (one, or two across a
# sector edge), and its Stream patched once by the probe's write; the
# directory's growths zero their clusters. Slot by slot, the set alone was
# three writes.
WRITES_PER_FILE = 2.5  # 1.36 on the tree of 2026-10-19, 3.24 slot by slot


def steps(n):
    return [("login:", "root"), ("assword", "root"), ("# ", f"dirprobe /D {n}"),
            (r"dirprobe: (ok|FAIL)\r?\n", "")]


def said_ok(lines, count):
    return [] if any("dirprobe: ok" in ln for ln in lines) else ["dirprobe did not say ok"]


def main():
    with open(SOURCE, "rb") as fh:
        start = [p for p in mbrdisk.entries(fh.read(512)) if p[1] == 0x07][0][2]
    found = {}

    def inspect(copy, problems):
        vol = stale_cursor.Volume(copy, start)
        root = vol.data(vol.chain(vol.root))
        d = exfatimg.find(root, "D")
        if d is None:
            problems.append("/D not found on the disk after the boot")
            return
        first = int.from_bytes(d.data[52:56], "little")
        size = int.from_bytes(d.data[56:64], "little")
        clusters = vol.chain(first) if not d.data[33] & 0x02 else \
            [first + k for k in range(-(-size // vol.cluster))]
        spc = vol.cluster // 512
        heap = vol.heap // 512
        found["sectors"] = {heap + (c - 2) * spc + k for c in clusters for k in range(spc)}
        if not found["sectors"]:
            problems.append("/D holds no sectors on the disk after the boot")

    written = collections.Counter()
    w = sum(-(-(i * SET) // SLOTS_PER_SECTOR) or 1 for i in range(COUNT))

    def bounded(lines, count):
        """The 100-file boot's own grade: /D's reads and writes, against W."""
        problems = said_ok(lines, count)
        sectors = found.get("sectors")
        if not sectors:
            return problems
        reads = sum(count[s] for s in sectors)
        writes = sum(written[s] for s in sectors)
        print(f"     /D: {len(sectors)} sectors, read {reads} times ({reads / w:.1f} passes of W = {w}), "
              f"written {writes} times ({writes / COUNT:.2f} a file)")
        if not 0 < reads <= PASSES * w:
            problems.append(f"/D's sectors read {reads} times, not at most {PASSES} x W = {PASSES * w}")
        if not 0 < writes <= COUNT * WRITES_PER_FILE:
            problems.append(f"/D's sectors written {writes} times, not at most {WRITES_PER_FILE} a file "
                            f"({COUNT * WRITES_PER_FILE:g})")
        return problems

    tally = disktrace.Tally("test-dir-scan")
    one = disktrace.boot("test-dir-scan", "1", SOURCE, "espexfat.img", steps(1), check=said_ok)
    hundred = disktrace.boot("test-dir-scan", str(COUNT), SOURCE, "espexfat.img", steps(COUNT),
                             inspect, bounded, writes=written)
    problems = tally.settle([one, hundred])
    if problems is not None:
        if one and hundred:
            more = sum(hundred[2].values()) - sum(one[2].values())
            print(f"     the {COUNT}-file boot read {more} more sectors in all than the 1-file boot")
        print(("FAIL" if problems else "ok  ") + f" {COUNT} files made in /D: its sectors read at most "
              f"{PASSES} passes of the directory a create and written at most {WRITES_PER_FILE} times a file")
        for p in problems:
            print("     " + p)
        tally.close(problems)
    return tally.verdict()


if __name__ == "__main__":
    sys.exit(main())

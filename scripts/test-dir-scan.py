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
FAT cache, which no disk counter sees, so it is not graded. Reports both numbers. About a
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


def main():
    with open(SOURCE, "rb") as fh:
        start = [p for p in mbrdisk.entries(fh.read(512)) if p[1] == 0x07][0][2]
    found = {}

    def inspect(copy):
        vol = stale_cursor.Volume(copy, start)
        root = vol.data(vol.chain(vol.root))
        d = exfatimg.find(root, "D")
        if d is None:
            return
        first = int.from_bytes(d.data[52:56], "little")
        size = int.from_bytes(d.data[56:64], "little")
        clusters = vol.chain(first) if not d.data[33] & 0x02 else \
            [first + k for k in range(-(-size // vol.cluster))]
        spc = vol.cluster // 512
        heap = vol.heap // 512
        found["sectors"] = {heap + (c - 2) * spc + k for c in clusters for k in range(spc)}

    problems, lines1, count1 = disktrace.boot("test-dir-scan-1", SOURCE, "espexfat.img", steps(1))
    written = collections.Counter()
    more_problems, lines, count = disktrace.boot("test-dir-scan-100", SOURCE, "espexfat.img", steps(COUNT),
                                                 inspect, writes=written)
    problems += more_problems
    sectors = found.get("sectors", set())
    reads = sum(count[s] for s in sectors)
    writes = sum(written[s] for s in sectors)
    more = sum(count.values()) - sum(count1.values())
    w = sum(-(-(i * SET) // SLOTS_PER_SECTOR) or 1 for i in range(COUNT))
    checks = [
        ("both boots driven, no fault lines", not problems),
        ("dirprobe ok in both", any("dirprobe: ok" in ln for ln in lines1) and any("dirprobe: ok" in ln for ln in lines)),
        (f"/D found on the disk after the boot, {len(sectors)} sectors", bool(sectors)),
        (f"/D's sectors read {reads} times, at most {PASSES} x W = {PASSES * w} (W = {w}, {reads / w:.1f} passes)",
         bool(sectors) and 0 < reads <= PASSES * w),
        (f"/D's sectors written {writes} times, at most {WRITES_PER_FILE} a file ({COUNT * WRITES_PER_FILE:g}; "
         f"{writes / COUNT:.2f} a file)",
         bool(sectors) and 0 < writes <= COUNT * WRITES_PER_FILE),
    ]
    print(f"      the {COUNT}-file boot read {more} more sectors in all than the 1-file boot")
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    for p in problems:
        print("      " + p)
    print(f"test-dir-scan: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""A listing page's walk stops at the page's end, seen by QEMU's disk trace.

    python3 scripts/test-listing-walk.py    (or `make test-listing-walk`, which builds the images)

Since 2026-10-17 fsd stops walking a directory at the first entry that does
not fit an NP_READDIR page (`ReaddirFill::push` answers `Break`, and each
filesystem's `list_dir` stops on it). No listing check can see that: a
walker that ignored `Break` would give the same pages, since `ReaddirFill`
drops what is pushed after a full page. Only the disk can, so this rig counts
QEMU's `virtio_blk_handle_read` events (`-trace`, into the -d log): QEMU
counts the reads, not anything in the guest.

Each image gets a directory to list: /include on FAT32 (build/esp.img) and
exFAT (build/espexfat.img), two pages; on ext2 (build/espext2.img) /walk,
300 empty files, three pages over four 1 KiB blocks. ext2 needs its own:
since ext2's listing stopped reading an inode for every entry (2026-10-17),
/include is two blocks there and page one ends in the second, so every walk
reads both and the stop has nothing to show. The rig rebuilds the copy's
ext2 root from the image's own tree (debugfs rdump) plus /walk and /kinds
(two directories, two files) with mke2fs -d, twice: with the `filetype`
feature, whose listing takes each kind from the entry, and without it, the
one path that still reads an inode for the kind. e2fsck -fn must pass on
each, and ownership is not kept (rdump cannot set it without root), which a
rig that lists as root does not see. Two boots of each copy, each logging
in, listing DIR once to warm fsd's caches (the same in both, so every
measured run starts from the same state: FAT32 keeps the last FAT sector it
read across requests), and then typing one command RUNS times:

- `echo DIR/qqq*`: the shell lists DIR page by page for the glob (the same
  `readdir_pages` loop as `ls`), matches nothing, and keeps the word as
  typed, so `echo` prints it;
- `echo DIR/qqqq`: the same binary and an argument of the same length, and
  no listing.

Spawn stages a program 512 bytes a call, each resolving its path again, so
any other baseline (a different binary, or `ls` on a file, which walks DIR
to that file) puts its own reads into the difference. These two differ by
the listings alone: the difference, per sector, over RUNS is one listing.

Both boots then type a glob that must match (`/include/std*`, or
`/walk/f00*`) and must print exactly the names it should: a listing that
failed would leave the glob literal too, and read less, which would pass
for a saving. And each `echo` must print its word back.

The check: a listing of `p` pages asks fsd for `p + 2` (each page, the
question past any end, the empty page that ends it). Without the stop
those requests do the same work, each resolving the path and walking the
whole directory, so every sector is read a multiple of `p + 2` times a
listing and the counts share that factor. With it, the walks of the pages
before the last end early and the sectors past their ends are read fewer
times, so the counts must share no factor above 1. A count that is not a
whole number a listing (the two boots doing other work apart from the
listings) fails too. Controls: main before the stop fails every image
(factor 4 on /include), and a single walker ignoring `Break` fails its own
image alone.

On ext2 with `filetype` one listing must also read fewer sectors than
/walk has entries: until 2026-10-17 ext2's walk read every entry's inode,
entries walked past included, so a listing read at least one sector an
entry. It takes the kind from the entry's `file_type` byte now. Fails on
main before that (3,698 sectors). On both ext2 volumes `ls /kinds` must
mark exactly the two directories: fails without `filetype` when the byte
(0 there) is trusted with the feature off.

The sectors one listing reads are reported, not graded: 70 for /include on
FAT32 and 126 on exFAT (72 and 132 before the stop, measured without the
warm-up listing, which adds FAT32 one read a run), and 74 for /walk on
ext2 (3,698 when its walk read an inode an entry, and still without
`filetype`). The boots are deterministic: the same build reads the same
sectors each time. About six minutes; needs e2fsprogs (`brew install
e2fsprogs`), as the ext2 image does.
"""
import math
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disktrace  # noqa: E402  (the trace-counting boot, shared with test-spawn-reads.py)
import bootstick  # noqa: E402  (its e2fsprogs lookup, `tool`)
import mbrdisk  # noqa: E402  (its MBR reader, `entries`)

RUNS = 2
SECTOR = 512
WALK_FILES = 300


def std_names():
    """The staged names /include/std* must match."""
    d = os.path.join(ROOT, "build", "esp", "include")
    return sorted(f"/include/{n}" for n in os.listdir(d) if n.startswith("std"))


def walk_names():
    """The names /walk/f00* must match."""
    return [f"/walk/f{i:03d}" for i in range(10)]


KINDS = ["d1/", "d2/", "f1", "f2"]


def ext2_rebuilt(copy, filetype):
    """Rebuild the ext2 partition of `copy` (MBR type 0x83) from its own
    tree plus /walk, WALK_FILES empty files, and /kinds (KINDS): the tree
    dumped by debugfs, the partition made again by mke2fs -d, with the
    `filetype` feature or without it, and written back. Ownership is not
    kept (rdump cannot set it without root), which a rig that lists as root
    does not see. debugfs exits 0 whatever a command did, so its complaints
    are read (as scripts/bootstick.py does), and e2fsck -fn must pass."""
    with open(copy, "rb") as fh:
        parts = mbrdisk.entries(fh.read(SECTOR))
    ext2 = [p for p in parts or [] if p[1] == 0x83]
    if len(ext2) != 1:
        raise SystemExit(f"{copy}: not one ext2 partition (MBR type 0x83): {parts}")
    _, _, start, count = ext2[0]
    try:
        debugfs, mke2fs, e2fsck = (bootstick.tool(t) for t in ("debugfs", "mke2fs", "e2fsck"))
    except bootstick.Refused as e:
        raise SystemExit(str(e))
    with tempfile.TemporaryDirectory() as tmp:
        part, tree = os.path.join(tmp, "part.img"), os.path.join(tmp, "tree")
        with open(copy, "rb") as src, open(part, "wb") as dst:
            src.seek(start * SECTOR)
            dst.write(src.read(count * SECTOR))
        os.mkdir(tree)
        out = subprocess.run([debugfs, "-R", f"rdump / {tree}", part], capture_output=True, text=True)
        errors = [ln for ln in out.stderr.splitlines() if ln.strip()
                  and not ln.startswith("debugfs ") and "changing ownership" not in ln]
        if out.returncode != 0 or errors:
            raise SystemExit("dumping the ext2 tree failed: " + "; ".join(errors[:5]))
        os.mkdir(os.path.join(tree, "walk"))
        for i in range(WALK_FILES):
            open(os.path.join(tree, "walk", f"f{i:03d}"), "wb").close()
        os.mkdir(os.path.join(tree, "kinds"))
        for k in KINDS:
            if k.endswith("/"):
                os.mkdir(os.path.join(tree, "kinds", k[:-1]))
            else:
                open(os.path.join(tree, "kinds", k), "wb").close()
        os.remove(part)
        made = subprocess.run([mke2fs, "-q", "-t", "ext2", "-b", "1024",
                               "-O", "filetype" if filetype else "^filetype",
                               "-d", tree, "-F", part, f"{count * SECTOR // 1024}k"],
                              capture_output=True, text=True)
        if made.returncode != 0:
            raise SystemExit(f"mke2fs failed: {made.stderr.strip()}")
        check = subprocess.run([e2fsck, "-fn", part], capture_output=True, text=True)
        if check.returncode != 0:
            raise SystemExit(f"the rebuilt ext2 does not check clean (e2fsck -fn):\n{check.stdout}")
        features = subprocess.run([debugfs, "-R", "features", part], capture_output=True, text=True)
        if ("filetype" in features.stdout.split()) != filetype:
            raise SystemExit(f"the rebuilt ext2's features are not as asked: {features.stdout.strip()}")
        if os.path.getsize(part) != count * SECTOR:
            raise SystemExit("the rebuilt ext2 is not the partition's size")
        with open(part, "rb") as src, open(copy, "r+b") as dst:
            dst.seek(start * SECTOR)
            dst.write(src.read())


# (name, image, directory, glob that must match, its names, setup of the
# copy, whether /kinds is there to list). ext2 twice: with `filetype`, its
# listing takes each kind from the entry; without it, from the inode, the one
# path that still reads one.
IMAGES = [
    ("fat32", "esp.img", "/include", "/include/std*", std_names, None, False),
    ("ext2", "espext2.img", "/walk", "/walk/f00*", walk_names,
     lambda c: ext2_rebuilt(c, True), True),
    ("ext2-nofiletype", "espext2.img", "/walk", "/walk/f00*", walk_names,
     lambda c: ext2_rebuilt(c, False), True),
    ("exfat", "espexfat.img", "/include", "/include/std*", std_names, None, False),
]
# The images whose listing must read fewer sectors than its entries, and how
# many entries that is: ext2 with `filetype`, which read an inode an entry
# until 2026-10-17. Without the feature it still must, by design.
UNDER_ONE_AN_ENTRY = {"ext2": WALK_FILES}


def boot(name, prepared, image, warm, word, check, want, kinds):
    """Boot a copy of `prepared`, log in, `echo warm` once, `echo word` RUNS
    times, then the check glob, then (with `kinds`) `ls /kinds`, which must
    mark exactly its directories. Returns (problems, sectors read by sector
    number). Both boots start with the same `warm`, a listing, so each
    measured run starts from the same state of fsd's caches (FAT32 keeps the
    last FAT sector it read across requests: without it the first run of
    each boot differed by one read, from whatever login left there)."""
    steps = [("login:", "root"), ("assword", "root"), ("# ", f"echo {warm}")]
    steps += [("# ", f"echo {word}")] * RUNS + [("# ", f"echo {check}")]
    steps += [("# ", "ls /kinds")] if kinds else []
    steps += [("# ", "")]
    problems, lines, count = disktrace.boot(f"test-listing-walk-{name}", prepared, image, steps)
    wanted = RUNS + (word == warm)
    if lines.count(word) < wanted:
        problems.append(f"`echo {word}` printed it back {lines.count(word)} times, not {wanted}")
    if not any(sorted(line.split()) == want for line in lines):
        problems.append(f"`echo {check}` did not print {' '.join(want)}: the listing failed")
    if kinds and not any(sorted(line.split()) == sorted(KINDS) for line in lines):
        problems.append(f"`ls /kinds` did not print {' '.join(KINDS)}: a kind is wrong")
    return problems, count


def main() -> int:
    failed = 0
    for name, image, d, check, names, setup, kinds in IMAGES:
        source = os.path.join(ROOT, "build", image)
        prepared = os.path.join(ROOT, "build", f"test-listing-walk-{name}-prepared.img")
        if setup:
            shutil.copyfile(source, prepared)
        try:
            if setup:
                setup(prepared)
            want = names()
            src, warm = (prepared if setup else source), f"{d}/qqq*"
            p0, base = boot(name, src, image, warm, f"{d}/qqqq", check, want, kinds)
            p1, listed = boot(name, src, image, warm, f"{d}/qqq*", check, want, kinds)
        finally:
            if setup and os.path.exists(prepared):
                os.remove(prepared)
        differ, counts = disktrace.per_run(base, listed, RUNS)
        problems = p0 + p1 + differ
        per = list(counts.values())
        common = math.gcd(*per) if per else 0
        total = sum(per)
        if not problems and common != 1:
            problems.append(f"every sector is read a multiple of {common} times a listing: "
                            "the walks are all alike, so no page's walk stopped at its end")
        entries = UNDER_ONE_AN_ENTRY.get(name)
        if not problems and entries is not None and total >= entries:
            problems.append(f"a listing of {entries} entries reads {total} sectors: "
                            "at least one an entry, as an inode read for each would")
        verdict = "FAIL" if problems else "ok  "
        print(f"{verdict} {name}: one listing of {d} reads {total} sectors, "
              f"over {len(per)} sectors, read a multiple of {common} times each")
        for p in problems:
            print(f"     {p}")
        failed += bool(problems)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

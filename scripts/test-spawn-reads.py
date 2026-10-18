#!/usr/bin/env python3
"""Spawning a program resolves its path once a read request, exactly.

    python3 scripts/test-spawn-reads.py    (or `make test-spawn-reads`, which builds the images)

The shell spawns a program by reading its file through fsd and staging it in
the kernel. Until 2026-10-17 it read 512 bytes a request (`NP_READ_AT`), and
fsd resolved the path from the root for each one: one run of `ls` read 1,201
sectors on FAT32 (1,032 on ext2, 1,942 on exFAT), against 199, 187 and 290
since. Now the shell reads `SPAWN_READ` (8 KiB) a request through `NP_READ`,
and fsd resolves the path once a request and copies the file to the shell's
grant a `SAFECOPY_MAX` (2 KiB) piece at a time (`vfs::read_pieces`). No
program's output can show either half; only the disk can, so this rig counts
QEMU's virtio-blk reads (scripts/disktrace.py).

On a copy of each image (FAT32 build/esp.img, ext2 build/espext2.img, exFAT
build/espexfat.img) the host puts two copies of /bin/ARGS in the root:
/SMALL as it is, and /BIG zero-padded to BIG bytes (the loader reads only
what the program headers name, so both run the same). Two boots, each
logging in and spawning `/SMALL w` once to warm fsd's caches, then RUNS
spawns of `/SMALL a` in one boot and of `/BIG a` in the other; each must
print `argv[1] = a`.

The check is exact. Every resolution of `/SMALL` or `/BIG` reads the root
directory's first sector once, and everything else a run does is the same
in both boots, so per run that sector's count differs between the boots by
the difference in requests: ceil(size / SPAWN_READ) + 1 each (the last read
of 0 ends the loop). It must equal that. Resolving once a 2 KiB piece (fsd's
half undone) makes it about four times as much, and once a 512 bytes (the
shell's half undone, as on main before 2026-10-17) about sixteen times.
The root directory's first sector is found on the host: from the FAT32 or
exFAT boot sector, and on ext2 by debugfs. Reported too: the sectors one
spawn of each reads.

On exFAT two more boots grade the walk along a file's cluster chain. macOS
may write the host's files contiguous (NoFatChain, read with no FAT at all)
or chained, so each boot copies /HUGE, 640 KiB, to /CHAIN with the guest's
`cp`: fsd writes every file it makes FAT-chained, and 160 clusters are more
than one FAT sector's 128 entries. One boot then spawns /CHAIN twice more
than the other, and the FAT sectors one spawn reads must be what the
design predicts from /CHAIN's chain, read by the host off the first boot's
disk: one walk from its first cluster (the read cursor resuming each 2 KiB
piece where the last ended), each entry through the one-sector FAT cache.
The chain must cross a FAT sector, or a walk from the start would cost no
more and the cursor could not be seen. Ignoring the cursor reads 1,436,
bypassing the cache 159 (a read a step), and a cache not refreshed by
write_fat_entry stops /CHAIN running at all.

About four minutes; needs e2fsprogs, as the ext2 image does.
"""
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import bootstick  # noqa: E402  (its e2fsprogs lookup, `tool`)
import disktrace  # noqa: E402
import mbrdisk  # noqa: E402  (its MBR reader, `entries`)

SECTOR = 512
RUNS = 2
SPAWN_READ = 8 * 1024  # the shell's SPAWN_READ
BIG = 200 * 1024
HUGE = 640 * 1024  # 160 clusters of 4 KiB, past one FAT sector's 128 entries
IMAGES = [("fat32", "esp.img", 0x0B), ("ext2", "espext2.img", 0x83), ("exfat", "espexfat.img", 0x07)]


def requests(size):
    """The NP_READ requests spawn_path makes for a file of `size` bytes."""
    return math.ceil(size / SPAWN_READ) + 1


def partition(image, ptype):
    """(start LBA, sectors) of the one partition of MBR type `ptype`."""
    with open(image, "rb") as fh:
        parts = mbrdisk.entries(fh.read(SECTOR)) or []
    found = [p for p in parts if p[1] == ptype]
    if len(found) != 1:
        raise SystemExit(f"{image}: not one partition of type {ptype:#x}: {parts}")
    return found[0][2], found[0][3]


def place_mounted(image, files, part=1):
    """Write `files` ({name: bytes}) to the root of partition `part` of
    `image` (FAT32 or exFAT), attached without mounting and mounted alone,
    since the exFAT image has the ESP beside it."""
    out = subprocess.run(["hdiutil", "attach", "-nobrowse", "-nomount", image],
                         check=True, capture_output=True, text=True).stdout
    disk = out.split()[0]
    mp = tempfile.mkdtemp(prefix="test-spawn-reads.")
    try:
        subprocess.run(["diskutil", "mount", "-mountPoint", mp, f"{disk}s{part}"], check=True, capture_output=True)
        for name, data in files.items():
            with open(os.path.join(mp, name), "wb") as fh:
                fh.write(data)
        for f in os.listdir(mp):
            if f.startswith("._"):
                os.remove(os.path.join(mp, f))
        subprocess.run(["diskutil", "unmount", mp], check=True, capture_output=True)
    finally:
        subprocess.run(["hdiutil", "detach", disk], capture_output=True)
        os.rmdir(mp)


def place_ext2(image, files):
    """Write `files` to the root of `image`'s ext2 partition with debugfs,
    whose complaints are read (it exits 0 whatever a command did), and
    return the root directory's first block's LBA."""
    start, count = partition(image, 0x83)
    try:
        debugfs, e2fsck = bootstick.tool("debugfs"), bootstick.tool("e2fsck")
    except bootstick.Refused as e:
        raise SystemExit(str(e))
    with tempfile.TemporaryDirectory() as tmp:
        part, cmds = os.path.join(tmp, "part.img"), os.path.join(tmp, "cmds")
        with open(image, "rb") as src, open(part, "wb") as dst:
            src.seek(start * SECTOR)
            dst.write(src.read(count * SECTOR))
        with open(cmds, "w") as fh:
            for name, data in files.items():
                host = os.path.join(tmp, name)
                with open(host, "wb") as f:
                    f.write(data)
                fh.write(f"write {host} {name}\n")
        out = subprocess.run([debugfs, "-w", "-f", cmds, part], capture_output=True, text=True)
        errors = [ln for ln in out.stderr.splitlines() if ln.strip() and not ln.startswith("debugfs ")]
        if out.returncode != 0 or errors:
            raise SystemExit("writing the ext2 root failed: " + "; ".join(errors[:5]))
        if subprocess.run([e2fsck, "-fn", part], capture_output=True).returncode != 0:
            raise SystemExit("the ext2 root does not check clean after the writes (e2fsck -fn)")
        blocks = subprocess.run([debugfs, "-R", "blocks /", part], capture_output=True, text=True).stdout.split()
        bsize = re.search(r"Block size:\s+(\d+)", subprocess.run(
            [debugfs, "-R", "stats", part], capture_output=True, text=True).stdout)
        with open(part, "rb") as src, open(image, "r+b") as dst:
            dst.seek(start * SECTOR)
            dst.write(src.read())
    return start + int(blocks[0]) * int(bsize.group(1)) // SECTOR


def root_fat32(image):
    start, _ = partition(image, 0x0B)
    with open(image, "rb") as fh:
        fh.seek(start * SECTOR)
        b = fh.read(SECTOR)
    per_clus, reserved, nfats = b[13], struct.unpack_from("<H", b, 14)[0], b[16]
    fatsz, root = struct.unpack_from("<I", b, 36)[0], struct.unpack_from("<I", b, 44)[0]
    return start + reserved + nfats * fatsz + (root - 2) * per_clus


def exfat_layout(image):
    """The exFAT partition's geometry from its boot sector: its start, the
    FAT's first LBA and length, the cluster heap's first LBA, the root
    directory's first cluster and the cluster size in sectors."""
    start, _ = partition(image, 0x07)
    with open(image, "rb") as fh:
        fh.seek(start * SECTOR)
        b = fh.read(SECTOR)
    fat, fat_len, heap, count, root = struct.unpack_from("<IIIII", b, 80)
    return {"fat": start + fat, "fat_end": start + fat + fat_len, "heap": start + heap,
            "count": count, "root": root, "spc": 1 << b[109]}


def root_exfat(image):
    g = exfat_layout(image)
    return g["heap"] + (g["root"] - 2) * g["spc"]


def exfat_chain(image, name):
    """(the root directory's clusters, `name`'s) in the exFAT root of `image`,
    each in chain order, read by the host from the FAT; `name`'s is None when
    the file is contiguous (NoFatChain). Raises ValueError for a chain longer
    than the volume (a loop) or a name not there."""
    g = exfat_layout(image)
    with open(image, "rb") as fh:
        def fat(c):
            fh.seek((g["fat"] + c * 4 // SECTOR) * SECTOR + c * 4 % SECTOR)
            return struct.unpack("<I", fh.read(4))[0]

        def chain(first):
            out = [first]
            while 2 <= fat(out[-1]) < 0xFFFFFFF7:
                if len(out) == g["count"]:
                    raise ValueError(f"the FAT chain from cluster {first} is longer than the volume: a loop")
                out.append(fat(out[-1]))
            return out

        root = chain(g["root"])
        data = b""
        for c in root:
            fh.seek((g["heap"] + (c - 2) * g["spc"]) * SECTOR)
            data += fh.read(g["spc"] * SECTOR)
        i = 0
        while i < len(data) and data[i]:
            count = data[i + 1] + 1 if data[i] == 0x85 else 1
            if data[i] == 0x85:
                stream = data[i + 32:i + 64]
                names = b"".join(data[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
                if names.decode("utf-16le")[:stream[3]] == name:
                    first = struct.unpack_from("<I", stream, 20)[0]
                    return root, None if stream[1] & 0x02 else chain(first)
            i += 32 * count
    raise ValueError(f"no {name} in the exFAT root of the boot's disk")


def chain_reads(chain, g):
    """The FAT sectors one more read of `chain` from its start to its end
    makes, as the design has it: one walk (the read cursor), each entry
    looked up through a one-sector cache, which the read before left on the
    sector of the last entry looked up. Nothing else in a spawn looks up a
    FAT entry while the root directory is one cluster: each resolution finds
    the name in it without stepping on (chain_walk checks that it is)."""
    sector = lambda c: c * 4 // SECTOR
    cached, reads = sector(chain[-2]), 0
    for c in chain[:-1]:
        if sector(c) != cached:
            reads, cached = reads + 1, sector(c)
    return reads


def boot(name, prepared, image, program, runs=RUNS, setup=(), inspect=None):
    steps = [("login:", "root"), ("assword", "root"), ("# ", "/SMALL w")]
    steps += [("# ", cmd) for cmd in setup]
    steps += [("# ", f"{program} a")] * runs + [("# ", "")]
    problems, lines, count = disktrace.boot(f"test-spawn-reads-{name}", prepared, image, steps,
                                            inspect)
    said = sum(1 for ln in lines if ln.strip() == "argv[1] = a")
    if said < runs:
        problems.append(f"`{program} a` printed argv[1] = a {said} times, not {runs}")
    return problems, count


def main() -> int:
    with open(os.path.join(ROOT, "build", "esp", "bin", "ARGS"), "rb") as fh:
        small = fh.read()
    files = {"SMALL": small, "BIG": small + bytes(BIG - len(small))}
    want = requests(BIG) - requests(len(small))
    failed = 0
    for name, image, ptype in IMAGES:
        prepared = os.path.join(ROOT, "build", f"test-spawn-reads-{name}-prepared.img")
        shutil.copyfile(os.path.join(ROOT, "build", image), prepared)
        try:
            if ptype == 0x83:
                root = place_ext2(prepared, files)
            else:
                huge = {"HUGE": small + bytes(HUGE - len(small))} if ptype == 0x07 else {}
                place_mounted(prepared, {**files, **huge})
                root = root_fat32(prepared) if ptype == 0x0B else root_exfat(prepared)
            p0, base = boot(name, prepared, image, "/SMALL")
            p1, more = boot(name, prepared, image, "/BIG")
            if ptype == 0x07:
                failed += chain_walk(prepared, image)
        finally:
            os.remove(prepared)
        problems = p0 + p1
        delta = (more[root] - base[root]) / RUNS
        if not problems and delta != want:
            problems.append(f"the root directory's first sector (LBA {root}) is read {delta:g} more "
                            f"times a spawn of /BIG than of /SMALL, not {want}: one resolution "
                            "a request is the rule")
        verdict = "FAIL" if problems else "ok  "
        print(f"{verdict} {name}: /BIG ({BIG} bytes) resolves {delta:g} more times than /SMALL "
              f"({len(small)}), {want} more requests; the root sector is LBA {root}")
        for p in problems:
            print(f"     {p}")
        failed += bool(problems)
    return 1 if failed else 0


def chain_walk(prepared, image):
    """The two exFAT boots that grade the walk along a FAT chain; 1 on a failure."""
    g = exfat_layout(prepared)
    setup = ["cp /HUGE /CHAIN", "/CHAIN w"]
    read, read_problems = [], []

    def inspect(copy):
        try:
            read.append(exfat_chain(copy, "CHAIN"))
        except ValueError as e:
            read_problems.append(str(e))

    p0, base = boot("exfat-chain-base", prepared, image, "/CHAIN", RUNS, setup, inspect)
    p1, more = boot("exfat-chain-more", prepared, image, "/CHAIN", 2 * RUNS, setup)
    problems = p0 + p1 + read_problems
    root, chain = read[0] if read else (None, None)
    if not problems and not read:
        problems.append("the first boot's disk was not read")
    elif not problems and not chain:
        problems.append("/CHAIN, written by the guest's cp, is contiguous: no FAT chain to walk")
    elif not problems and len(root) > 1:
        problems.append(f"the root directory spans {len(root)} clusters: a resolution may step "
                        "along its chain, which the prediction does not count")
    elif not problems and len({c * 4 // SECTOR for c in chain[:-1]}) < 2:
        problems.append(f"/CHAIN's chain lies in one FAT sector: a walk from its start would cost "
                        "no more reads, so the cursor could not be seen")
    if problems:
        delta = want = None
    else:
        more_problems, per = disktrace.per_run(base, more, RUNS)
        problems += more_problems
        delta = sum(c for s, c in per.items() if g["fat"] <= s < g["fat_end"])
        want = chain_reads(chain, g)
        if not problems and delta != want:
            problems.append(f"a spawn of /CHAIN reads {delta} FAT sectors, not {want}: one walk of "
                            "its chain a spawn, through the FAT sector cache, is the rule")
    verdict = "FAIL" if problems else "ok  "
    print(f"{verdict} exfat: /CHAIN ({HUGE} bytes, written by the guest's cp, "
          f"{len(chain) if chain else '?'} clusters FAT-chained) reads {delta} FAT sectors a spawn, "
          f"{want} for one walk of its chain")
    for p in problems:
        print(f"     {p}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

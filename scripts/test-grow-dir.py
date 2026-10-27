#!/usr/bin/env python3
"""A directory grown past its first cluster lists and finds every entry.

    python3 scripts/test-grow-dir.py    (or `make test-grow-dir`, which builds the image)

When a directory's clusters are full, fsd links another to its chain
(`grow_dir`) and writes the new entry there. FAT32 has no size for a
directory: its chain is its extent. exFAT has one, the DataLength in the
directory's stream extension, and a reader stops there (fsd's own walk_dir
does, and so does every other driver), so a grown directory whose DataLength
stays at its first cluster has entries no reader sees.

The image is assembled here from the same build/exfatpart.img and
build/esp.img as build/espexfat.img, with three directories macOS makes on
the exFAT root first, each one contiguous (NoFatChain) cluster, as it made
/etc, /Users and /SUB; two of them the host then changes. One boot runs
/bin/DIRPROBE (programs/shellutils/dirprobe) in five directories:

- /MAC, as macOS made it, COUNT files: it must be given a FAT chain before
  it grows, and a free-slot scan not stopped at its size wrote into the
  clusters after it (another directory, the new files' data);
- /ZERO, made a directory of size 0 with no cluster (the spec allows one,
  and other drivers and fsck repairs make them), COUNT files: it must be
  given its first cluster, where fsd once wrote FAT entry 0 and an entry
  set before the cluster heap;
- /NAMED, made a NoFatChain directory of size 0 that still names the
  cluster macOS gave it, that cluster freed in the bitmap (a tool may leave
  one), COUNT files: it must be given a cluster of its own, where fsd once
  took the named one without its bit, which the next allocation could hand
  back (a FAT loop, or another file over its entries);
- /RMZ, made a FAT-chained directory of size 0 naming /man's directory
  cluster, in use, and /Z0FILE and /Z0APP empty files naming the same: the
  guest removes RMZ, overwrites Z0FILE and appends to Z0APP, and /man's
  cluster must stay allocated and as it was, where freeing the chain a
  size-0 entry names freed another directory's, and an append wrote into
  the cluster it names; and /LONGF, a one-byte file whose cluster's FAT
  entry links on, past its size, to /man's cluster: the guest removes it,
  and /man's cluster must stay as it was, where freeing a file walked its
  whole chain; and /LONGA, a file of one whole cluster linked on the same
  way: the guest appends to it, which must extend it, where the write
  followed the link and wrote into /man's cluster;
- /OLD, as fsd before 2026-10-18 left a directory it grew: a FAT chain of
  two clusters, a DataLength of one, and OLD_COUNT files' sets across both,
  so a reader sees 42; those past the size (HIDDEN) hold their own names.
  fsd reads it to its size, as every driver does, and refuses to write to
  it (as Linux does) until a fsck repairs it: a read of a hidden file finds
  nothing, an append to one, a create, a mkdir and a write of a new file are
  refused while a touch of a file inside the size goes ahead (it creates
  nothing), and nothing of OLD changes on the disk (nor is a cluster
  allocated for a refused create: fsck would say the bitmap needs repair), where taking its chain as its size on a write alone
  truncated the append's file, and a create's name check at the size wrote
  a second set under a hidden name;
- /grow, a new directory on the exFAT root, 4 KiB clusters: 128 entries a
  cluster, three to each file's entry set, so COUNT files grow it twice;
- /mnt/f/grow on the FAT32 ESP, 512-byte clusters: the control, a FAT32
  directory grown six times.

The probe makes the files, finds each by its path, lists the directory and
touches every file again (a no-op where fsd finds the file, a second entry
where it does not). After the boot the host reads each directory off the
disk and checks that it grew (the check that the rig tested anything; OLD
must not, its two clusters hold its files), that every file's entry is
there once, and on exFAT that the directory's DataLength and
ValidDataLength cover its whole chain. On the exFAT partition every
cluster fsd had no reason to write (all but the root's and the bitmap's,
the three directories' as they were before the boot, and those the boot
allocated) must hold the bytes it held before, and so must everything
before the cluster heap but the FAT entries of clusters. Then fsck_exfat -n
and fsck_msdos -n on the two partitions, the foreign observer.
KEEP_IMAGE=1 keeps build/test-grow-dir.img for a look, and a failing run
keeps it anyway. One boot, about a
minute and a half.
"""
import importlib.util
import os
import re
import struct
import subprocess
import sys
import time

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


drive_qemu = load("drive_qemu", "drive-qemu.py")
# The host's reader of a FAT32 or exFAT partition (chains, listings, the bitmap).
Volume, entry_set = exfatimg.Volume, exfatimg.entry_set

BUILD = os.path.join(ROOT, "build")
IMAGE = os.path.join(BUILD, "test-grow-dir.img")
PART = os.path.join(BUILD, "test-grow-dir-x.part")
COUNT = 100
# 150 slots: past OLD's first cluster (128 slots, F042's set across the two)
# and within its second.
OLD_COUNT = 50
# OLD's files whose sets lie past its first cluster's slot 125, so past the
# size: each given a cluster holding its name.
HIDDEN = [f"F{i:03d}" for i in range(42, OLD_COUNT)]
MADE = ("MAC", "ZERO", "NAMED", "OLD", "RMZ")


def mac_dirs():
    """PART: a copy of build/exfatpart.img with the empty directories MADE
    made by macOS, which makes a directory one contiguous (NoFatChain)
    cluster, as it made /SUB, /Users and /etc."""
    with open(os.path.join(BUILD, "exfatpart.img"), "rb") as src, open(PART, "wb") as dst:
        dst.write(src.read())
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", PART],
                         capture_output=True, text=True)
    if att.returncode != 0:
        sys.exit(f"test-grow-dir: hdiutil attach failed: {att.stderr.strip()}")
    dev = att.stdout.split()[0]
    try:
        if subprocess.run(["diskutil", "mount", dev], capture_output=True).returncode != 0:
            sys.exit(f"test-grow-dir: diskutil mount {dev} failed")
        info = subprocess.run(["diskutil", "info", dev], capture_output=True, text=True).stdout
        found = re.search(r"Mount Point:\s*(\S.*)$", info, re.M)
        if not found:
            subprocess.run(["diskutil", "unmount", dev], capture_output=True)
            sys.exit(f"test-grow-dir: diskutil info {dev} names no mount point")
        mp = found.group(1)
        try:
            for name in MADE:
                os.mkdir(os.path.join(mp, name))
            for name in ("Z0FILE", "Z0APP"):
                open(os.path.join(mp, name), "w").close()
            with open(os.path.join(mp, "LONGF"), "w") as fh:
                fh.write("L")
            with open(os.path.join(mp, "LONGA"), "w") as fh:
                fh.write("A" * 4096)
            subprocess.run(["find", mp, "-name", "._*", "-delete"], capture_output=True)
            for junk in (".fseventsd", ".Trashes", ".Spotlight-V100", ".DS_Store",
                         *(f"{name}/.DS_Store" for name in MADE)):
                subprocess.run(["rm", "-rf", os.path.join(mp, junk)], capture_output=True)
        finally:
            # macOS may hold the volume a moment: until it lets go, PART may
            # not hold what it wrote.
            for _ in range(5):
                if subprocess.run(["diskutil", "unmount", dev], capture_output=True).returncode == 0:
                    break
                time.sleep(1)
            else:
                sys.exit(f"test-grow-dir: diskutil unmount {dev} failed")
    finally:
        if subprocess.run(["hdiutil", "detach", dev], capture_output=True).returncode != 0:
            sys.exit(f"test-grow-dir: hdiutil detach {dev} failed")


def man_cluster(vol):
    """/man's first cluster, a directory in use that RMZ names."""
    found = exfat_dir(vol, "man")
    if not found or found[0] < 2:
        sys.exit("test-grow-dir: no /man directory on the exFAT image")
    return found[0]


def plant():
    """OLD made as fsd before 2026-10-18 left a directory it grew, and ZERO a
    directory of size 0 with no cluster (see the module doc). Returns the
    names a reader stopping at the size sees in OLD before the boot, which
    the run checks, and the clusters planted for HIDDEN's data."""
    vol = Volume(PART, 0)
    root = vol.chain(vol.root)
    d = bytearray(vol.data(root))
    sets = {s.name: s for s in exfatimg.sets(d)}
    for name in MADE:
        made = exfatimg.extent(sets[name]) if name in sets else None
        if not made or made.chained or made.size != vol.cluster:
            sys.exit(f"test-grow-dir: macOS did not make {name} one NoFatChain cluster")
    with open(PART, "r+b") as fh:
        def bitmap(c, on):
            fh.seek(vol.heap + (vol.bitmap - 2) * vol.cluster + (c - 2) // 8)
            b = fh.read(1)[0]
            fh.seek(-1, 1)
            fh.write(bytes([b | 1 << (c - 2) % 8 if on else b & ~(1 << (c - 2) % 8)]))

        def fat(c, value):
            fh.seek(vol.fat + c * 4)
            fh.write(struct.pack("<I", value))

        def stream(name, flags, first, size):
            exfatimg.restream(d, sets[name], flags, first, size)

        def first_of(name):
            return exfatimg.extent(sets[name]).first

        old = first_of("OLD")
        free = [c for c in range(2, vol.count + 2) if not vol.used(c)]
        second, data = free[0], dict(zip(HIDDEN, free[1:]))
        for c in [second, *data.values()]:
            bitmap(c, True)
            fat(c, 0xFFFFFFFF)
        fat(old, second)
        for name, c in data.items():
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(name.encode().ljust(vol.cluster, b"\0"))
        files = b"".join(entry_set(n, data.get(n, 0), len(n) if n in data else 0)
                         for n in (f"F{i:03d}" for i in range(OLD_COUNT)))
        if len(files) > 2 * vol.cluster:
            sys.exit(f"test-grow-dir: OLD's sets need {len(files)} bytes, more than two {vol.cluster}-byte clusters")
        files = files.ljust(2 * vol.cluster, b"\0")
        for c, piece in ((old, files[:vol.cluster]), (second, files[vol.cluster:])):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(piece)
        stream("OLD", 0x01, old, vol.cluster)

        zero = first_of("ZERO")
        bitmap(zero, False)
        stream("ZERO", 0x01, 0, 0)

        named = first_of("NAMED")
        bitmap(named, False)
        stream("NAMED", 0x03, named, 0)

        rmz = first_of("RMZ")
        bitmap(rmz, False)
        stream("RMZ", 0x01, man_cluster(vol), 0)
        stream("Z0FILE", 0x01, man_cluster(vol), 0)
        stream("Z0APP", 0x01, man_cluster(vol), 0)

        # LONGF FAT-chained, one byte, its cluster linked on to /man's.
        longf = first_of("LONGF")
        stream("LONGF", 0x01, longf, 1)
        fat(longf, man_cluster(vol))
        # LONGA the same, its one cluster full.
        longa = first_of("LONGA")
        stream("LONGA", 0x01, longa, vol.cluster)
        fat(longa, man_cluster(vol))

        for k, c in enumerate(root):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])
    vol = Volume(PART, 0)
    if exfat_dir(vol, "ZERO")[:2] != (0, 0):
        sys.exit(f"test-grow-dir: ZERO is not a directory of size 0 with no cluster: {exfat_dir(vol, 'ZERO')}")
    if exfat_dir(vol, "NAMED")[1:] != (0, 0, False) or vol.used(named):
        sys.exit("test-grow-dir: NAMED is not a NoFatChain directory of size 0 naming a free cluster")
    first, size, _, _ = exfat_dir(vol, "OLD")
    longf = exfat_dir(vol, "LONGF")
    if not (longf and longf[1:] == (1, 1, True) and vol.chain(longf[0])[:2] == [longf[0], man_cluster(vol)]):
        sys.exit(f"test-grow-dir: LONGF is not a one-byte chained file linked on to /man's cluster: {longf}")
    longa = exfat_dir(vol, "LONGA")
    if not (longa and longa[1:] == (vol.cluster, vol.cluster, True)
            and vol.chain(longa[0])[:2] == [longa[0], man_cluster(vol)]):
        sys.exit(f"test-grow-dir: LONGA is not a one-cluster chained file linked on to /man's cluster: {longa}")
    for name in ("RMZ", "Z0FILE", "Z0APP"):
        if exfat_dir(vol, name) != (man_cluster(vol), 0, 0, True):
            sys.exit(f"test-grow-dir: {name} is not a FAT-chained entry of size 0 naming /man's cluster")
    return exfat_names(vol.data([first])[:size]), set(data.values())


def exfat_dir(vol, name):
    """The root's entry `name` on exFAT: (first cluster, DataLength,
    ValidDataLength, chained), or None."""
    found = exfatimg.find(vol.data(vol.chain(vol.root)), name)
    return exfatimg.extent(found) if found else None


def exfat_names(d):
    """Every live file name in the entry sets of directory bytes `d`, in
    order, a name twice if two sets carry it; a set cut off by the end of
    `d` is not read."""
    return [s.name for s in exfatimg.sets(d)]


def fat32_names(d):
    """Every live short name in FAT32 directory bytes `d`, `.` and `..` left out."""
    out = []
    for j in range(0, len(d), 32):
        e = d[j:j + 32]
        if e[0] == 0:
            break
        if e[0] != 0xE5 and e[11] != 0x0F and e[0] != ord("."):
            base, ext = e[:8].decode("latin1").rstrip(), e[8:11].decode("latin1").rstrip()
            out.append(base + ("." + ext if ext else ""))
    return out


def names_check(where, names, count=COUNT):
    """The listing on the disk holds each of `count` files once and nothing else."""
    want = [f"F{i:03d}" for i in range(count)]
    dup = sorted({n for n in names if names.count(n) > 1})
    missing = [n for n in want if n not in names]
    extra = sorted(set(names) - set(want))
    for what, which in (("missing", missing), ("twice", dup), ("not the probe's", extra)):
        if which:
            print(f"     {where}: on the disk, {what}: {which[:8]}{' ...' if len(which) > 8 else ''}")
    return (f"{where}: on the disk the directory holds each of the {count} files once", not (dup or missing or extra))


def clusters(vol):
    """Every cluster's bytes and whether it is in use, by cluster number."""
    return {c: (vol.data([c]), vol.used(c)) for c in range(2, vol.count + 2)}


def before_heap(vol, start):
    """The bytes of the exFAT partition at LBA `start` before its cluster heap
    but the FAT entries of clusters: the boot region, FAT entries 0 and 1
    (the media descriptor and a constant), and what lies between the FAT
    and the heap."""
    fat_end = vol.fat + 4 * (vol.count + 2)
    out = []
    for lo, hi in ((start * 512, vol.fat + 8), (fat_end, vol.heap)):
        vol.fh.seek(lo)
        out.append(vol.fh.read(hi - lo))
    return out


def snapshot(vol, start):
    """What the boot is compared with: every cluster, the bytes before the
    heap, and the clusters fsd may write without allocating them: the
    root's, the bitmap's, those of the directories MADE but OLD, which fsd
    must leave as it is, and its files' data with it, and LONGF's."""
    bitmap = -(-vol.count // 8 // vol.cluster)  # its clusters, ceil(count / 8 / cluster)
    mine = set(vol.chain(vol.root)) | set(range(vol.bitmap, vol.bitmap + bitmap))
    for name in MADE:
        first, size, _, _ = exfat_dir(vol, name)
        if size and name != "OLD":
            mine.add(first)  # one NoFatChain cluster
    # LONGF's own cluster, which its removal frees for the boot to reuse.
    mine.add(exfat_dir(vol, "LONGF")[0])
    return clusters(vol), before_heap(vol, start), mine


def exfat_checks(exf, where, name, count):
    """Directory `name` on the exFAT root holds `count` files, read as a
    driver reads it (no further than DataLength), grown past two clusters,
    its DataLength and ValidDataLength its whole chain."""
    found = exfat_dir(exf, name)
    if not found:
        return [(f"{where}: on the disk", False)]
    first, size, valid, chained = found
    if first < 2:
        print(f"     {where}: no cluster, DataLength {size}")
        return [(f"{where}: given a cluster", False)]
    chain = exf.chain(first) if chained else [first]
    whole = len(chain) * exf.cluster
    print(f"     {where}: first cluster {first}, {'chained' if chained else 'NoFatChain'}, "
          f"chain of {len(chain)} clusters, DataLength {size}, ValidDataLength {valid}")
    return [
        (f"{where}: grew past its first cluster ({count} files), FAT-chained", chained and len(chain) >= 3),
        (f"{where}: DataLength and ValidDataLength are its whole chain", size == whole and valid == whole),
        names_check(where, exfat_names(exf.data(chain)[:size]), count),
    ]


def disk_checks(parts, snap, old):
    """The two partitions after the boot, against `snap` (`snapshot`)."""
    exf, esp = (Volume(IMAGE, start) for start, _ in parts)
    before, head, mine = snap
    checks = []
    for where, name in (("exfat /MAC", "MAC"), ("exfat /ZERO", "ZERO"), ("exfat /NAMED", "NAMED"),
                        ("exfat root /grow", "grow")):
        checks += exfat_checks(exf, where, name, COUNT)
    man = man_cluster(exf)
    checks.append(("exfat /RMZ: removed", exfat_dir(exf, "RMZ") is None))
    # Allocated and as it was: a cluster freed early in the boot is soon
    # allocated again, to a file whose bytes then replace /man's entries.
    checks.append(("exfat /LONGF: removed", exfat_dir(exf, "LONGF") is None))
    longa = exfat_dir(exf, "LONGA")
    checks.append(("exfat /LONGA: grew by the append, on a chain of its own",
                   bool(longa) and longa[1] > exf.cluster and man not in exf.chain(longa[0])))
    checks.append(("exfat /RMZ, /Z0FILE, /Z0APP, /LONGF and /LONGA: /man's cluster, which they named, allocated and as it was",
                   exf.used(man) and exf.data([man]) == before[man][0]))
    print(f"     exfat /OLD: before the boot {old}, after it {exfat_dir(exf, 'OLD')}")
    checks.append(("exfat /OLD: its set as it was (first cluster, DataLength, ValidDataLength, chained)",
                   exfat_dir(exf, "OLD") == old))
    # Each cluster of the probed directories and their files has one owner:
    # a directory that took a cluster it named but did not hold shares it
    # with the file the allocator later gave it to.
    owners = {}
    for name in ("MAC", "ZERO", "NAMED", "OLD", "grow"):
        found = exfat_dir(exf, name)
        if not found or found[0] < 2:
            continue
        first, _, _, chained = found
        dirs = exf.chain(first) if chained else [first]
        for c in dirs:
            owners.setdefault(c, []).append(f"/{name}")
        for fname, (ffirst, fsize, fchained) in exf.listing(first, chained).items():
            if ffirst >= 2 and fsize:
                run = exf.chain(ffirst) if fchained else range(ffirst, ffirst + -(-fsize // exf.cluster))
                for c in run:
                    owners.setdefault(c, []).append(f"/{name}/{fname}")
    shared = {c: who for c, who in owners.items() if len(who) > 1}
    if shared:
        print(f"     exfat: clusters with two owners: {dict(list(shared.items())[:4])}")
    checks.append(("exfat: every cluster of the probed directories and their files has one owner", not shared))

    # A scan for free slots that ran past MAC's one cluster wrote into
    # whatever lay after it, in use or free; a directory of size 0 had its
    # set written before the heap.
    after = clusters(exf)
    stray = [c for c, (data, used) in after.items()
             if c not in mine and data != before[c][0] and not (used and not before[c][1])]
    if stray:
        print(f"     exfat: clusters written with no reason: {stray[:8]}{' ...' if len(stray) > 8 else ''}"
              f" ({', '.join('in use' if before[c][1] else 'free' for c in stray[:8])} before the boot)")
    checks.append(("exfat: no cluster changed but the root's, the bitmap's, MAC's, ZERO's, NAMED's "
                   "and the boot's allocations (OLD's and its files' data not among them)", not stray))
    checks.append(("exfat: nothing before the cluster heap changed but the FAT entries of clusters",
                   before_heap(exf, parts[0][0]) == head))

    names = {k.upper(): v for k, v in esp.listing(esp.root).items()}
    if "GROW" not in names:
        return checks + [("fat32 /mnt/f: /GROW is on the disk", False)]
    chain = esp.chain(names["GROW"][0])
    print(f"     fat32 /mnt/f: /GROW: chain of {len(chain)} clusters")
    checks.append((f"fat32 /mnt/f: /GROW grew past its first cluster ({COUNT} files)", len(chain) >= 3))
    checks.append(names_check("fat32 /mnt/f", fat32_names(esp.data(chain))))
    return checks


def fsck():
    """fsck_exfat -n and fsck_msdos -n on the image's two partitions."""
    checks, notes = [], []
    try:
        with exfatimg.attached(IMAGE) as dev:
            for part, tool, where in (("s1", "fsck_exfat", "exfat root"), ("s2", "fsck_msdos", "fat32 /mnt/f")):
                rc, lines = exfatimg.fsck(dev + part, tool)
                if tool == "fsck_exfat":
                    # It ran its checks, it names /OLD's chain, the case planted
                    # and left for a fsck, and nothing else: every line is one of
                    # its stages, a complaint or its "No" to a repair.
                    ok = ("** Checking file system hierarchy." in lines and not exfatimg.unexpected(lines) and
                          exfatimg.complaints(lines) == ["   Cluster chain for /OLD has too many clusters for its size."])
                    checks.append((f"{where}: {tool} -n finds nothing to repair but /OLD's chain, as planted", ok))
                else:
                    ok = rc == 0
                    checks.append((f"{where}: {tool} -n finds nothing to repair", ok))
                if not ok:
                    notes += [f"{where}: {ln}" for ln in lines]
    except RuntimeError as e:
        return [("the image attaches for fsck", False)], [str(e)]
    return checks, notes


def main() -> int:
    try:
        mac_dirs()
        old_seen, _ = plant()
        parts = mbrdisk.assemble(IMAGE, [
            (0x07, mbrdisk.exfat_payload(PART), False),
            (0x0B, mbrdisk.esp_payload(os.path.join(BUILD, "esp.img")), True),
        ])
    finally:
        if os.path.exists(PART):
            os.remove(PART)
    try:
        # OLD's case is built only if a reader stops short of its files.
        print(f"     exfat /OLD: before the boot a reader sees {len(old_seen)} of its {OLD_COUNT} files")
        if len(old_seen) >= OLD_COUNT:
            sys.exit("test-grow-dir: OLD's files are all in view before the boot: the case is not built")
        before = Volume(IMAGE, parts[0][0])
        snap, old = snapshot(before, parts[0][0]), exfat_dir(before, "OLD")
        # MAC first, before anything else on the volume is allocated.
        runs = [("exfat /MAC", "/MAC", COUNT), ("exfat /ZERO", "/ZERO", COUNT), ("exfat /NAMED", "/NAMED", COUNT),
                ("exfat root /grow", "/grow", COUNT), ("fat32 /mnt/f", "/mnt/f/grow", COUNT)]
        # OLD: a read of a file past its size, an append to one (the
        # shell's read of it first), and a create.
        refused = [f"echo x >> /OLD/{HIDDEN[-1]}", "touch /OLD/NEW", "mkdir /OLD/X", "write /OLD/NEW2 abc"]
        allowed = ["touch /OLD/F000", "rmdir /RMZ", "write /Z0FILE y", "echo y >> /Z0APP", "rm /LONGF",
                   "echo z >> /LONGA"]
        reads = [f"cat /OLD/{HIDDEN[3]}", *refused, *allowed, "rmdir /OLD"]
        steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f")]
        steps += [("# ", cmd) for cmd in reads]
        prev = "# "
        for _, d, count in runs:
            steps.append((prev, f"dirprobe {d} {count}"))
            prev = r"dirprobe: (ok|FAIL)\r?\n"
        steps.append((prev, ""))
        guest = drive_qemu.Guest(IMAGE, label="test-grow-dir: ")
        try:
            driven = guest.run(steps)
            out = guest.transcript().replace("\r\n", "\n")
            faults = guest.aborts()
        finally:
            guest.stop()
        with open(os.path.join(BUILD, "test-grow-dir.txt"), "w") as fh:
            fh.write(out)

        checks = [("driven to the end", driven), ("no fault lines", faults == 0)]

        def said(cmd):
            """Everything `cmd` printed, up to the next prompt (the kernel's
            line for the task's exit can come before the shell's message),
            or None if it was not run."""
            i = out.find(cmd + "\n")
            if i < 0:
                return None
            rest = out[i + len(cmd) + 1:]
            m = re.search(r"(?m)^# ", rest)
            return rest[:m.start()] if m else rest
        said_by = {cmd: said(cmd) for cmd in reads}
        for cmd, got in said_by.items():
            print(f"     exfat /OLD: `{cmd}` printed {got!r}")
        checks.append((f"exfat /OLD: {HIDDEN[3]}, past the size, is not found (read to the size)",
                       "no such file" in (said_by[reads[0]] or "")))
        checks.append(("exfat /OLD: `rmdir /OLD` answers that it is not empty, before any refusal",
                       "directory not empty" in (said_by["rmdir /OLD"] or "")))
        for cmd in allowed:
            got = said_by[cmd]
            checks.append((f"`{cmd}` goes ahead", got is not None and "error" not in got and "(code 0)" in got))
        for cmd in refused:
            checks.append((f"exfat /OLD: `{cmd}` is refused with an I/O error", "I/O error" in (said_by[cmd] or "")))
        pieces = re.split(r"dirprobe (/\S+) \d+\n", out)
        by_dir = dict(zip(pieces[1::2], pieces[2::2]))
        for where, d, _ in runs:
            lines = [ln.rstrip() for ln in re.findall(r"^dirprobe: .*$", by_dir.get(d, ""), re.M)]
            for ln in lines:
                print(f"     {where}: {ln}")
            for step in ("create", "find", "list", "retouch"):
                checks.append((f"{where}: ok {step}", f"dirprobe: ok {step}" in lines))
            checks.append((f"{where}: the probe's summary is ok", lines[-1:] == ["dirprobe: ok"]))
        try:
            checks += disk_checks(parts, snap, old)
        except (ValueError, struct.error, UnicodeDecodeError) as exc:
            # A reader of a damaged disk: the image is kept for a look.
            print(f"     the host could not read the disk: {exc!r}")
            checks.append(("the host reads the disk after the boot", False))
        more, notes = fsck()
        checks += more
        for n in notes:
            print(f"     {n}")
    finally:
        failed = not all(ok for _, ok in checks) if "checks" in locals() else True
        if os.environ.get("KEEP_IMAGE") != "1" and not failed:
            os.remove(IMAGE)
        elif os.path.exists(IMAGE):
            print(f"     kept {os.path.relpath(IMAGE, ROOT)}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcript: build/test-grow-dir.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

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
- /OLD, as fsd before 2026-10-18 left a directory it grew: a FAT chain of
  two clusters, a DataLength of one, and OLD_COUNT empty files' sets across
  both, so a reader sees 42; the probe writes the same names, and each must
  replace its file, where a name check at the old size wrote a second set;
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
KEEP_IMAGE=1 keeps build/test-grow-dir.img for a look. One boot, about a
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
import mbrdisk  # noqa: E402


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
stale_cursor = load("test_stale_cursor", "test-stale-cursor.py")
# The host's reader of a FAT32 or exFAT partition (chains, listings, the bitmap).
Volume = stale_cursor.Volume
set_checksum = stale_cursor.set_checksum

BUILD = os.path.join(ROOT, "build")
IMAGE = os.path.join(BUILD, "test-grow-dir.img")
PART = os.path.join(BUILD, "test-grow-dir-x.part")
COUNT = 100
# 150 slots: past OLD's first cluster (128 slots, F042's set across the two)
# and within its second.
OLD_COUNT = 50
MADE = ("MAC", "ZERO", "OLD")


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


def entry_set(name):
    """An empty file's entry set, as fsd's build_entry_set makes it."""
    n = name.encode("utf-16le")
    k = -(-len(name) // 15)
    s = bytearray(32 * (2 + k))
    s[0], s[1] = 0x85, 1 + k
    s[32], s[33], s[35] = 0xC0, 0x01, len(name)
    h = 0
    for b in name.upper().encode("utf-16le"):
        h = (((h << 15) | (h >> 1)) + b) & 0xFFFF
    struct.pack_into("<H", s, 36, h)
    for e in range(k):
        chunk = n[30 * e:30 * e + 30]
        s[64 + 32 * e] = 0xC1
        s[64 + 32 * e + 2:64 + 32 * e + 2 + len(chunk)] = chunk
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def plant():
    """OLD made as fsd before 2026-10-18 left a directory it grew, and ZERO a
    directory of size 0 with no cluster (see the module doc). Returns the
    names a reader sees in OLD before the boot, which the run checks."""
    vol = Volume(PART, 0)
    root = vol.chain(vol.root)
    d = bytearray(vol.data(root))
    sets = {}
    i = 0
    while i < len(d) and d[i]:
        count = d[i + 1] + 1 if d[i] == 0x85 else 1
        if d[i] == 0x85:
            stream = d[i + 32:i + 64]
            got = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            sets[got.decode("utf-16le")[:stream[3]]] = (i, count)
        i += 32 * count
    for name in MADE:
        at = sets.get(name, (0, 0))[0]
        stream = d[at + 32:at + 64]
        if name not in sets or not stream[1] & 0x02 or struct.unpack_from("<Q", stream, 24)[0] != vol.cluster:
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
            at, count = sets[name]
            d[at + 33] = flags
            struct.pack_into("<Q", d, at + 32 + 8, size)   # ValidDataLength
            struct.pack_into("<I", d, at + 32 + 20, first)
            struct.pack_into("<Q", d, at + 32 + 24, size)  # DataLength
            struct.pack_into("<H", d, at + 2, set_checksum(d[at:at + 32 * count]))

        old = struct.unpack_from("<I", d, sets["OLD"][0] + 32 + 20)[0]
        second = next(c for c in range(2, vol.count + 2) if not vol.used(c))
        bitmap(second, True)
        fat(old, second)
        fat(second, 0xFFFFFFFF)
        files = b"".join(entry_set(f"F{i:03d}") for i in range(OLD_COUNT))
        files = files.ljust(2 * vol.cluster, b"\0")
        for c, piece in ((old, files[:vol.cluster]), (second, files[vol.cluster:])):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(piece)
        stream("OLD", 0x01, old, vol.cluster)

        zero = struct.unpack_from("<I", d, sets["ZERO"][0] + 32 + 20)[0]
        bitmap(zero, False)
        stream("ZERO", 0x01, 0, 0)

        for k, c in enumerate(root):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])
    vol = Volume(PART, 0)
    if exfat_dir(vol, "ZERO")[:2] != (0, 0):
        sys.exit(f"test-grow-dir: ZERO is not a directory of size 0 with no cluster: {exfat_dir(vol, 'ZERO')}")
    first, size, _, _ = exfat_dir(vol, "OLD")
    return exfat_names(vol.data(vol.chain(first))[:size])


def exfat_dir(vol, name):
    """The root's entry `name` on exFAT: (first cluster, DataLength,
    ValidDataLength, chained), or None."""
    d = vol.data(vol.chain(vol.root))
    i = 0
    while i < len(d) and d[i]:
        count = d[i + 1] + 1 if d[i] == 0x85 else 1
        if d[i] == 0x85:
            stream = d[i + 32:i + 64]
            got = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            if got.decode("utf-16le")[:stream[3]].upper() == name.upper():
                return (struct.unpack_from("<I", stream, 20)[0], struct.unpack_from("<Q", stream, 24)[0],
                        struct.unpack_from("<Q", stream, 8)[0], not stream[1] & 0x02)
        i += 32 * count
    return None


def exfat_names(d):
    """Every live file name in the entry sets of directory bytes `d`, in
    order, a name twice if two sets carry it; a set cut off by the end of
    `d` is not read."""
    out = []
    i = 0
    while i + 32 <= len(d) and d[i]:
        count = d[i + 1] + 1 if d[i] == 0x85 else 1
        if i + 32 * count > len(d):
            break
        if d[i] == 0x85:
            stream = d[i + 32:i + 64]
            got = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            out.append(got.decode("utf-16le")[:stream[3]])
        i += 32 * count
    return out


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
    root's, the bitmap's, and those of the directories MADE."""
    bitmap = -(-vol.count // 8 // vol.cluster)  # its clusters, ceil(count / 8 / cluster)
    mine = set(vol.chain(vol.root)) | set(range(vol.bitmap, vol.bitmap + bitmap))
    for name in MADE:
        first, _, _, chained = exfat_dir(vol, name)
        if first >= 2:
            mine |= set(vol.chain(first)) if chained else {first}
    return clusters(vol), before_heap(vol, start), mine


def exfat_checks(exf, where, name, count, grows):
    """Directory `name` on the exFAT root holds `count` files, read as a
    driver reads it (no further than DataLength), its DataLength and
    ValidDataLength its whole chain; `grows` past two clusters, or, if not,
    still the two it had."""
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
    if grows:
        grew = (f"{where}: grew past its first cluster ({count} files), FAT-chained", chained and len(chain) >= 3)
    else:
        grew = (f"{where}: still its two clusters, which hold its {count} files", chained and len(chain) == 2)
    return [
        grew,
        (f"{where}: DataLength and ValidDataLength are its whole chain", size == whole and valid == whole),
        names_check(where, exfat_names(exf.data(chain)[:size]), count),
    ]


def disk_checks(parts, snap):
    """The two partitions after the boot, against `snap` (`snapshot`)."""
    exf, esp = (Volume(IMAGE, start) for start, _ in parts)
    before, head, mine = snap
    checks = []
    for where, name, count, grows in (("exfat /MAC", "MAC", COUNT, True), ("exfat /ZERO", "ZERO", COUNT, True),
                                      ("exfat /OLD", "OLD", OLD_COUNT, False),
                                      ("exfat root /grow", "grow", COUNT, True)):
        checks += exfat_checks(exf, where, name, count, grows)

    # A scan for free slots that ran past MAC's one cluster wrote into
    # whatever lay after it, in use or free; a directory of size 0 had its
    # set written before the heap.
    after = clusters(exf)
    stray = [c for c, (data, used) in after.items()
             if c not in mine and data != before[c][0] and not (used and not before[c][1])]
    if stray:
        print(f"     exfat: clusters written with no reason: {stray[:8]}{' ...' if len(stray) > 8 else ''}"
              f" ({', '.join('in use' if before[c][1] else 'free' for c in stray[:8])} before the boot)")
    checks.append(("exfat: no cluster changed but the root's, the bitmap's, the three made directories' "
                   "and the boot's allocations", not stray))
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
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", IMAGE],
                         capture_output=True, text=True)
    if att.returncode != 0:
        return [("the image attaches for fsck", False)], [att.stderr.strip()]
    dev = att.stdout.split()[0]
    checks, notes = [], []
    try:
        for part, tool, where in (("s1", "fsck_exfat", "exfat root"), ("s2", "fsck_msdos", "fat32 /mnt/f")):
            r = subprocess.run([tool, "-n", dev + part], capture_output=True, text=True)
            checks.append((f"{where}: {tool} -n finds nothing to repair", r.returncode == 0))
            if r.returncode != 0:
                notes += [f"{where}: {ln}" for ln in (r.stdout + r.stderr).strip().splitlines()]
    finally:
        if subprocess.run(["hdiutil", "detach", dev], capture_output=True).returncode != 0:
            notes.append(f"hdiutil detach {dev} failed: the device is still attached")
    return checks, notes


def main() -> int:
    try:
        mac_dirs()
        old_seen = plant()
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
        snap = snapshot(Volume(IMAGE, parts[0][0]), parts[0][0])
        # MAC first, before anything else on the volume is allocated.
        runs = [("exfat /MAC", "/MAC", COUNT), ("exfat /ZERO", "/ZERO", COUNT), ("exfat /OLD", "/OLD", OLD_COUNT),
                ("exfat root /grow", "/grow", COUNT), ("fat32 /mnt/f", "/mnt/f/grow", COUNT)]
        steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f")]
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
        pieces = re.split(r"dirprobe (/\S+) \d+\n", out)
        by_dir = dict(zip(pieces[1::2], pieces[2::2]))
        for where, d, _ in runs:
            lines = [ln.rstrip() for ln in re.findall(r"^dirprobe: .*$", by_dir.get(d, ""), re.M)]
            for ln in lines:
                print(f"     {where}: {ln}")
            for step in ("create", "find", "list", "retouch"):
                checks.append((f"{where}: ok {step}", f"dirprobe: ok {step}" in lines))
            checks.append((f"{where}: the probe's summary is ok", lines[-1:] == ["dirprobe: ok"]))
        checks += disk_checks(parts, snap)
        more, notes = fsck()
        checks += more
        for n in notes:
            print(f"     {n}")
    finally:
        if os.environ.get("KEEP_IMAGE") != "1":
            os.remove(IMAGE)
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcript: build/test-grow-dir.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

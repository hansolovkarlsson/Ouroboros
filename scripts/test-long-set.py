#!/usr/bin/env python3
"""exFAT entry sets as the spec shapes them, and only those, are patched whole.

    python3 scripts/test-long-set.py     (or `make test-long-set`, which builds the image)

When fsd changes a file's Stream entry (its first cluster and length, on a
write), `patch_stream_ext` rewrites File and Stream and recomputes the set's
SetChecksum. Until 2026-10-19 it summed at most MAX_SET_ENTRIES (19)
entries, the longest set fsd builds, though the spec allows longer (vendor
secondaries after the name, which cameras write), so such a set was left with
a checksum over part of it, which fsck_exfat offers to repair by deleting the
set. And `walk_dir` closed a set that was not contiguous, or whose first
secondary was not the Stream, so the patch wrote Stream fields into whatever
slot followed the File. And until 2026-10-21 a rename built the new set
afresh (File, Stream, name), dropping the File entry's attributes and times
and every vendor entry. And until 2026-10-23 `rm`, `rmdir` and a replacing
`mv` left the clusters a vendor allocation entry (0xE1) names set in the
bitmap, with no set naming them. Five boots, each of a copy of
build/espexfat.img (exFAT root, partition 1):

- LONG: the host gives the root a second cluster and plants /VEND, an empty
  file whose set is 20 entries (File, Stream, one name, 17 Vendor Extension
  entries 0xE0) starting in the first cluster's last slot, so File and Stream
  lie in different sectors and clusters (the two-write path, and a slot
  step across a cluster); the slots before it deleted entries. Before the
  boot the set must read back and fsck_exfat -n pass (the observer's own
  check). `echo hello >> /VEND` and `cat /VEND`; after, the set still 20
  entries, the vendor entries as planted, the Stream naming a cluster and
  exactly the 7 bytes written, the SetChecksum over all 20, the data
  `hello\\r\\n`, and fsck_exfat -n passing again;
- SHAPE: two sets the spec does not allow, planted after the root's last set:
  GAPPY (File, a deleted slot, Stream, name) and PRIM (File, Stream, a volume
  label primary, name). `echo x >> /GAPPY` and `echo x >> /PRIM` must leave
  every planted slot byte for byte as it was (fsd sees no such file and makes
  new ones), and `ls /` lists each name once;
- RENAME: /VEND's 20-entry set after the root's last, its File entry given
  the archive attribute and three timestamps. `mv /VEND /VendorRenamedLong`
  (two name entries: the vendor entries move right) and `mv
  /VendorRenamedLong /V2` (one: they move left), `ls /` after each; after,
  no set under either old name, /V2's 20 entries with the File entry's
  bytes past its checksum, the Stream's flags, ValidDataLength, cluster and
  DataLength (a contiguous cluster of its own, 7 bytes of it valid) and the
  17 vendor entries as planted, the SetChecksum whole; and STRAY, its name
  entry after a vendor entry (fsck_exfat's one complaint before the boot),
  `mv`'d to /S2, which must be File, Stream, the new name, the vendor entry,
  with fsck_exfat -n passing after;
- VALLOC: sets after the root's last on fifteen free clusters in a run the
  host marks used, each run followed by a cluster that must stay used, and
  fsck_exfat -n clean first: VA1 (7 bytes in a contiguous cluster, a 0xE1
  naming two contiguous) removed by `rm`; VA2 (empty, a 0xE1 naming two
  FAT-chained, the last partly used) replaced by `mv -f /SRC /VA2`, where
  SRC holds 5 bytes and a vendor cluster of its own; VD (an empty
  directory, a vendor cluster) removed by `rmdir`; VE2, its benign
  secondary of the undefined type 0xE2 naming a cluster, removed; VE0, a
  0xE0 (no AllocationPossible) whose bytes name W3's cluster, removed; VA3
  renamed to VA4 and then to va4 by case alone through `mv -f`. After: no
  command answered anything, `cat /VA2` prints SRC's bytes, the removed and
  replaced sets' clusters free (VA2's chain in the FAT too), SRC's two and
  VA3's vendor cluster still used with VA2's and va4's sets carrying their
  entries, every witness still used, fsck_exfat -n clean;
- VCROSS: damaged sets, which fsck_exfat flags before the boot: SRC2 and
  DST2 sharing data and a vendor cluster, VA5 whose vendor run starts at its
  own data and reaches W5's cluster, VBAD's FAT-chained 0xE1 outside the
  heap, and VSYS's two on the allocation bitmap and the up-case table. `mv
  -f /SRC2 /DST2`, `rm` of VA5, VBAD and VSYS: no command answers anything,
  the shared clusters and W5's still used, VA5's data free, the bitmap's
  and up-case table's clusters still used, fsck_exfat -n clean after.

No fault lines in QEMU's trace. Fails with the sum capped at 19 entries (the
checksum, and fsck: "Incorrect checksum for directory entry set. Delete?")
with walk_dir taking either misshapen set (its slots written), with a rename
building its set afresh (main before 2026-10-21: the vendor entries, the
attributes and the times gone) and with the vendor entries left where the
old name ended (the set's shape, and fsck), with the Stream built afresh
(ValidDataLength raised) and with only the name entries after the Stream
dropped (STRAY's carried). VALLOC and VCROSS fail with each of twelve
mutants of the free (2026-10-23): no secondary freed, walk_dir not noting
one, only 0xE1 freed (VE2), AllocationPossible not read (W3), a contiguous
run one cluster long (witnesses), one bit cleared a bitmap sector (VA1),
the Stream's first cluster freed again as a secondary's (W5), the survivor
not consulted (SRC2's), the volume's own clusters freed (VSYS), a first
cluster outside the heap taken (VBAD's `rm` an I/O error), a case-only
`mv -f` freeing (va4's) and a replacing `mv` freeing its source (SRC's).
Not graded by a boot: the order (the set deleted before its clusters are
freed), which shows only on an error or a crash between the two. About two
minutes. Run it whenever exFAT's patch_stream_ext, set_checksum, walk_dir's
set assembly, slot_at, read_slots, mv, rm, rmdir, renamed_set, delete_set,
free_data or is_system changes.
"""
import importlib.util
import os
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import exfatimg  # noqa: E402
import mbrdisk  # noqa: E402


# The boots QEMU's firmware never handed to the kernel (drive_qemu.boot_stalled).
STALLED = []

def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
stale_cursor = load("test_stale_cursor", "test-stale-cursor.py")
grow_dir = load("test_grow_dir", "test-grow-dir.py")
Volume, set_checksum, entry_set = stale_cursor.Volume, stale_cursor.set_checksum, grow_dir.entry_set

IMAGE = os.path.join(ROOT, "build", "test-long-set.img")
VENDOR = 17
GUID = bytes(range(0x10, 0x20))
DELETED = bytes([0x05]) + bytes(31)  # a deleted File entry: in-use bit clear


def vendor_entry(k):
    """A Vendor Extension entry (0xE0): flags, a GUID, then data of its own."""
    e = bytearray(32)
    e[0] = 0xE0
    e[2:18] = GUID
    e[18:32] = bytes((k * 14 + j) & 0xFF for j in range(14))
    return bytes(e)


# A FAT timestamp, 2024-01-02 03:04:06, which a rename must keep.
STAMP = ((2024 - 1980) << 25) | (1 << 21) | (2 << 16) | (3 << 11) | (4 << 5) | 3
LONG_NAME = "VendorRenamedLong"  # 17 characters: two name entries


def long_set(stamped=False):
    s = bytearray(entry_set("VEND"))
    if stamped:
        struct.pack_into("<H", s, 4, 0x20)  # FileAttributes: archive
        struct.pack_into("<III", s, 8, STAMP, STAMP + 1, STAMP + 2)  # create, modify, access
        s[20:22] = bytes([7, 9])  # their 10 ms parts
    s += b"".join(vendor_entry(k) for k in range(VENDOR))
    s[1] = len(s) // 32 - 1  # SecondaryCount
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def shaped(name, between, counted):
    """`name`'s set (File, Stream, one name entry) with `between` put before the
    entry at index `at`: (at, entry). SecondaryCount counts the inserted entry
    when `counted`, as a reader that took it for a secondary would, and the
    checksum is over the slots the count covers: each set is one walk_dir
    before 2026-10-19 closed, under the name, so a write reached it."""
    s = bytearray(entry_set(name))
    at, entry = between
    out = bytearray(s[:32 * at] + entry + s[32 * at:])
    out[1] = s[1] + (1 if counted else 0)
    covered = out[:32 * (out[1] + 1)] if counted else out[:32] + out[64:]
    struct.pack_into("<H", out, 2, set_checksum(covered))
    return bytes(out)


def copy():
    shutil.copyfile(os.path.join(ROOT, "build", "espexfat.img"), IMAGE)
    with open(IMAGE, "rb") as fh:
        parts = mbrdisk.entries(fh.read(512))
    return [p for p in parts if p[1] == 0x07][0][2]


def root_end(d):
    i = 0
    while i < len(d) and d[i]:
        i += 32
    return i


def write_root(vol, d):
    with open(IMAGE, "r+b") as fh:
        for k, c in enumerate(vol.chain(vol.root)):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])


def grow_root(vol):
    """Give the root a second cluster: a free one, zeroed, marked in the bitmap
    and linked after the root's last."""
    free = next(c for c in range(2, vol.count + 2) if not vol.used(c))
    last = vol.chain(vol.root)[-1]
    with open(IMAGE, "r+b") as fh:
        fh.seek(vol.heap + (vol.bitmap - 2) * vol.cluster + (free - 2) // 8)
        b = fh.read(1)[0]
        fh.seek(-1, 1)
        fh.write(bytes([b | 1 << (free - 2) % 8]))
        for c, value in ((last, free), (free, 0xFFFFFFFF)):
            fh.seek(vol.fat + c * 4)
            fh.write(struct.pack("<I", value))
        fh.seek(vol.heap + (free - 2) * vol.cluster)
        fh.write(bytes(vol.cluster))


def fsck():
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not exfatimg.complaints(lines) and not exfatimg.unexpected(lines))
    return ok, lines


def boot(label, commands):
    steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in commands] + [("# ", "")]
    guest = drive_qemu.Guest(IMAGE, label=f"test-long-set {label}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", f"test-long-set-{label}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def run_long():
    checks, notes = [], []
    start = copy()
    vol = Volume(IMAGE, start)
    if len(vol.chain(vol.root)) != 1:
        return [("long: the image's root is one cluster, as the rig assumes", False)], notes
    grow_root(vol)
    vol = Volume(IMAGE, start)
    d = bytearray(vol.data(vol.chain(vol.root)))
    end, at = root_end(d), vol.cluster - 32  # the first cluster's last slot
    s = long_set()
    d[end:at] = DELETED * ((at - end) // 32)
    d[at:at + len(s)] = s
    write_root(vol, d)
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    ok, lines = fsck()
    checks += [
        ("long: /VEND's set reads back, 20 entries from the first cluster's last slot, its checksum whole",
         hit is not None and hit.offset == at and hit.count == 20 and hit.data == s
         and struct.unpack_from("<H", s, 2)[0] == set_checksum(s)),
        ("long: fsck_exfat -n passes the image before the boot", ok),
    ]
    if not ok:
        notes += [f"long before: {ln}" for ln in lines]
    if not all(c for _, c in checks):
        return checks, notes
    driven, out, faults = boot("long", ["echo hello >> /VEND", "cat /VEND"])
    if drive_qemu.boot_stalled("test-long-set", "long", out, faults):
        STALLED.append("long")
        return checks, notes
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    got = hit.data if hit else b""
    first, size = (struct.unpack_from("<I", got, 52)[0], struct.unpack_from("<Q", got, 56)[0]) if hit else (0, 0)
    data = vol.data(vol.chain(first))[:size] if first >= 2 else b""
    ok, lines = fsck()
    checks += [
        ("long: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        ("long: /VEND's set still 20 entries in place, File, Stream, a name, 17 vendor entries",
         hit is not None and hit.offset == at and hit.count == 20 and got[0] == 0x85 and got[32] == 0xC0
         and got[64] == 0xC1 and all(got[32 * k] == 0xE0 for k in range(3, 20))),
        ("long: its vendor entries byte for byte as planted", got[96:] == s[96:]),
        (f"long: its Stream names a cluster and the 7 bytes written (cluster {first}, {size} bytes)",
         first >= 2 and size == 7),
        ("long: its SetChecksum is the sum of all 20 entries",
         hit is not None and struct.unpack_from("<H", got, 2)[0] == set_checksum(got)),
        (f"long: its data is exactly hello\\r\\n ({data!r})", data == b"hello\r\n"),
        ("long: fsck_exfat -n passes the image after the boot", ok),
        ("long: no fault lines", faults == 0),
    ]
    if not ok:
        notes += [f"long after: {ln}" for ln in lines]
    return checks, notes


def run_shape():
    start = copy()
    vol = Volume(IMAGE, start)
    d = bytearray(vol.data(vol.chain(vol.root)))
    end = root_end(d)
    label = bytes([0x83]) + bytes(31)  # a volume label primary, empty
    gappy = shaped("GAPPY", (1, DELETED), False)  # File, deleted, Stream, name
    prim = shaped("PRIM", (2, label), True)       # File, Stream, label, name
    planted = gappy + prim
    if end + len(planted) + 32 > len(d):
        return [("shape: room for the planted sets in the root", False)], []
    d[end:end + len(planted)] = planted
    write_root(vol, d)
    driven, out, faults = boot("shape", ["echo x >> /GAPPY", "echo x >> /PRIM", "ls /"])
    if drive_qemu.boot_stalled("test-long-set", "shape", out, faults):
        STALLED.append("shape")
        return [], []
    vol = Volume(IMAGE, start)
    after = vol.data(vol.chain(vol.root))
    listing = out.split("# ls /", 1)[-1].split("# ", 1)[0].split()
    return [
        ("shape: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        ("shape: GAPPY's and PRIM's planted slots byte for byte as they were",
         after[end:end + len(planted)] == planted),
        ("shape: ls / lists GAPPY and PRIM once each (made new)",
         listing.count("GAPPY") == 1 and listing.count("PRIM") == 1),
        ("shape: no fault lines", faults == 0),
    ], []


def run_rename():
    """/VEND's 20-entry set, its File entry stamped, renamed to a name of two
    entries and back to one: the vendor entries move right and then left."""
    start = copy()
    vol = Volume(IMAGE, start)
    d = bytearray(vol.data(vol.chain(vol.root)))
    end = root_end(d)
    # /VEND holds a cluster of its own, contiguous (NoFatChain), its
    # ValidDataLength 7 within a DataLength of the whole cluster, junk past
    # the 7 bytes: a rename must keep all four.
    free = next(c for c in range(2, vol.count + 2) if not vol.used(c))
    s = bytearray(long_set(stamped=True))
    s[33] = 0x03  # GeneralSecondaryFlags: AllocationPossible, NoFatChain
    struct.pack_into("<Q", s, 40, 7)
    struct.pack_into("<I", s, 52, free)
    struct.pack_into("<Q", s, 56, vol.cluster)
    struct.pack_into("<H", s, 2, set_checksum(s))
    s = bytes(s)
    # STRAY: File, Stream, a vendor entry, then its name entry. walk_dir
    # takes a name entry anywhere among the secondaries; a rename must drop
    # it with the others, not carry it after the new name.
    t = bytearray(entry_set("STRAY"))
    stray = bytes(t[:64]) + vendor_entry(99) + bytes(t[64:96])
    stray = bytearray(stray)
    stray[1] = 3
    struct.pack_into("<H", stray, 2, set_checksum(stray))
    stray = bytes(stray)
    if end + len(s) + len(stray) + 32 > len(d):
        return [("rename: room for /VEND's and STRAY's sets in the root", False)], []
    d[end:end + len(s)] = s
    d[end + len(s):end + len(s) + len(stray)] = stray
    write_root(vol, d)
    with open(IMAGE, "r+b") as fh:
        fh.seek(vol.heap + (vol.bitmap - 2) * vol.cluster + (free - 2) // 8)
        b = fh.read(1)[0]
        fh.seek(-1, 1)
        fh.write(bytes([b | 1 << (free - 2) % 8]))
        fh.seek(vol.heap + (free - 2) * vol.cluster)
        fh.write(b"hello\r\n" + b"\xEE" * (vol.cluster - 7))
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    # STRAY's shape is fsck's one complaint before the boot (the rename
    # writes it in the spec's shape, so none after).
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
    ok = exfatimg.complaints(lines) == ["   Invalid file name in /."] and not exfatimg.unexpected(lines)
    checks = [
        ("rename: /VEND's stamped set reads back, 20 entries, its checksum whole, its cluster used",
         hit is not None and hit.count == 20 and hit.data == s and s[4] == 0x20 and vol.used(free)
         and struct.unpack_from("<H", s, 2)[0] == set_checksum(s)),
        ("rename: fsck_exfat -n names only STRAY's misplaced name before the boot", ok),
    ]
    notes = [f"rename before: {ln}" for ln in lines] if not ok else []
    if not all(c for _, c in checks):
        return checks, notes
    cmds = [f"mv /VEND /{LONG_NAME}", "ls /", f"mv /{LONG_NAME} /V2", "ls /", "mv /STRAY /S2"]
    driven, out, faults = boot("rename", cmds)
    if drive_qemu.boot_stalled("test-long-set", "rename", out, faults):
        STALLED.append("rename")
        return checks, notes
    listings = [blk.split("# ", 1)[0].split() for blk in out.split("# ls /")[1:3]]
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    hit = exfatimg.find(root, "V2")
    got = hit.data if hit else b""
    s2 = exfatimg.find(root, "S2")
    s2_types = [s2.data[k] for k in range(0, len(s2.data), 32)] if s2 else []
    ok, lines = fsck()
    checks += [
        ("rename: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"rename: ls / lists {LONG_NAME} and not VEND, then V2 and neither ({listings})",
         len(listings) == 2 and LONG_NAME in listings[0] and "VEND" not in listings[0]
         and "V2" in listings[1] and LONG_NAME not in listings[1]),
        ("rename: no set named VEND or the long name is left",
         exfatimg.find(root, "VEND") is None and exfatimg.find(root, LONG_NAME) is None),
        ("rename: /V2's set is 20 entries, File, Stream, a name, 17 vendor entries",
         hit is not None and hit.count == 20 and got[0] == 0x85 and got[32] == 0xC0
         and got[64] == 0xC1 and all(got[32 * k] == 0xE0 for k in range(3, 20))),
        ("rename: its vendor entries byte for byte as planted", got[96:] == s[96:]),
        ("rename: its File entry's attributes and times as planted", got[4:32] == s[4:32]),
        (f"rename: its Stream's flags, ValidDataLength, cluster and DataLength as planted "
         f"({got[33:34].hex()} {got[40:48].hex()} {got[52:64].hex()})",
         got[33] == s[33] and got[40:48] == s[40:48] and got[52:64] == s[52:64]),
        ("rename: its Stream's NameLength is V2's", len(got) > 35 and got[35] == 2),
        (f"rename: STRAY renamed to S2 is File, Stream, the new name, its vendor entry ({[hex(t) for t in s2_types]})",
         s2_types == [0x85, 0xC0, 0xC1, 0xE0] and s2.data[96:] == vendor_entry(99)
         and exfatimg.find(root, "STRAY") is None),
        ("rename: its SetChecksum is the sum of all 20 entries",
         hit is not None and struct.unpack_from("<H", got, 2)[0] == set_checksum(got)),
        ("rename: fsck_exfat -n passes the image after the boot", ok),
        ("rename: no fault lines", faults == 0),
    ]
    if not ok:
        notes += [f"rename after: {ln}" for ln in lines]
    return checks, notes


def secondary(kind, first, size, flags):
    """A benign secondary of type `kind` (0xE1 a vendor allocation, 0xE2 one
    the spec leaves undefined) naming `size` bytes from cluster `first`, its
    GeneralSecondaryFlags `flags` (0x01 AllocationPossible, 0x02 NoFatChain)."""
    e = bytearray(32)
    e[0], e[1] = kind, flags
    e[2:18] = GUID
    struct.pack_into("<I", e, 20, first)
    struct.pack_into("<Q", e, 24, size)
    return bytes(e)


def plant_set(name, first=0, size=0, extra=(), is_dir=False, contiguous=True):
    """`name`'s set, its data `size` bytes from `first` (contiguous, or
    FAT-chained for a directory fsd made), with the secondaries `extra` after
    its name, sealed."""
    s = bytearray(entry_set(name, first, size))
    if size and contiguous:
        s[33] = 0x03
    if is_dir:
        struct.pack_into("<H", s, 4, 0x10)
    s += b"".join(extra)
    s[1] = len(s) // 32 - 1
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def free_run(vol, n):
    return next((c for c in range(2, vol.count + 2 - n) if not any(vol.used(c + k) for k in range(n))), None)


def plant(vol, start, sets, used, links=(), fills=()):
    """Write `sets` after the root's last, mark the clusters `used` in the
    bitmap, write the FAT `links` (cluster, value) and the cluster `fills`
    (cluster, bytes, zero-padded). False when the root has no room."""
    d = bytearray(vol.data(vol.chain(vol.root)))
    end = root_end(d)
    planted = b"".join(sets)
    if end + len(planted) + 32 > len(d):
        return False
    d[end:end + len(planted)] = planted
    write_root(vol, d)
    c = vol.cluster
    with open(IMAGE, "r+b") as fh:
        for k in used:
            fh.seek(vol.heap + (vol.bitmap - 2) * c + (k - 2) // 8)
            b = fh.read(1)[0]
            fh.seek(-1, 1)
            fh.write(bytes([b | 1 << (k - 2) % 8]))
        for at, value in links:
            fh.seek(vol.fat + at * 4)
            fh.write(struct.pack("<I", value))
        for at, fill in fills:
            fh.seek(vol.heap + (at - 2) * c)
            fh.write(fill + bytes(c - len(fill)))
    return True


def system_clusters(vol):
    """The up-case table's first cluster, from the root's 0x82 entry."""
    d = vol.data(vol.chain(vol.root))
    return [struct.unpack_from("<I", d, i + 20)[0] for i in range(0, len(d), 32) if d[i] == 0x82][:1]


def printed(out, cmd):
    """What `cmd` printed, the kernel's own lines left out."""
    return [ln.strip() for ln in out.split("# " + cmd, 1)[-1].split("# ", 1)[0].splitlines()[1:]
            if ln.strip() and not ln.startswith("Ouroboros kernel:")]


def run_valloc():
    """Allocations a set's benign secondaries name, freed with the set and
    only then, on a volume fsck_exfat passes. Fifteen clusters in a run, each
    contiguous run followed by one that must stay used."""
    start = copy()
    vol = Volume(IMAGE, start)
    r = free_run(vol, 15)
    if r is None:
        return [("valloc: fifteen free clusters in a run on the image", False)], []
    c = vol.cluster
    E1, E2 = 0xE1, 0xE2
    sets = {
        "VA1": plant_set("VA1", r, 7, [secondary(E1, r + 1, 2 * c, 0x03)]),      # rm
        "W": plant_set("W", r + 3, c),                                           # witness
        "VA2": plant_set("VA2", extra=[secondary(E1, r + 4, 2 * c - 100, 0x01)]),  # replaced
        "VA3": plant_set("VA3", extra=[secondary(E1, r + 6, 1, 0x03)]),          # renamed twice
        "VD": plant_set("VD", r + 7, c, [secondary(E1, r + 8, c, 0x03)], is_dir=True, contiguous=False),
        "SRC": plant_set("SRC", r + 9, 5, [secondary(E1, r + 10, c, 0x03)]),     # mv -f source
        "VE2": plant_set("VE2", extra=[secondary(E2, r + 13, c, 0x03)]),         # undefined type
        "VE0": plant_set("VE0", extra=[secondary(0xE0, r + 14, c, 0x00)]),       # no allocation
        "W3": plant_set("W3", r + 14, c),                                        # witness
        "W2": plant_set("W2", r + 11, 2 * c),                                    # witness
    }
    used = [k for k in range(r, r + 15)]
    ok_room = plant(vol, start, list(sets.values()), used,
                    links=[(r + 4, r + 5), (r + 5, 0xFFFFFFFF), (r + 7, 0xFFFFFFFF)],
                    fills=[(r, b"hello\r\n"), (r + 7, b""), (r + 9, b"src\r\n")])
    if not ok_room:
        return [("valloc: room for the planted sets in the root", False)], []
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    ok, lines = fsck()
    checks = [
        ("valloc: every planted set reads back, all fifteen clusters used",
         all(exfatimg.find(root, n) is not None and exfatimg.find(root, n).data == p for n, p in sets.items())
         and all(vol.used(r + k) for k in range(15))),
        ("valloc: fsck_exfat -n passes the image before the boot", ok),
    ]
    notes = [f"valloc before: {ln}" for ln in lines] if not ok else []
    if not all(ok for _, ok in checks):
        return checks, notes
    cmds = ["mv -f /SRC /VA2", "cat /VA2", "rm /VA1", "mv /VA3 /VA4", "mv -f /VA4 /va4",
            "rmdir /VD", "rm /VE2", "rm /VE0", "ls /"]
    driven, out, faults = boot("valloc", cmds)
    if drive_qemu.boot_stalled("test-long-set", "valloc", out, faults):
        STALLED.append("valloc")
        return checks, notes
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    gone = [n for n in ("VA1", "VD", "SRC", "VE2", "VE0") if exfatimg.find(root, n) is None]
    va2, va4 = exfatimg.find(root, "VA2"), exfatimg.find(root, "va4")
    used = {k: vol.used(k) for k in range(r, r + 15)}
    ok, lines = fsck()
    free = lambda *ks: [used[k] for k in ks]
    checks += [
        ("valloc: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"valloc: VA1, VD, SRC, VE2 and VE0 gone from the root ({gone})", len(gone) == 5),
        ("valloc: every mv, rm and rmdir answered nothing (no error)",
         all(printed(out, k) == [] for k in cmds if k.split()[0] in ("mv", "rm", "rmdir"))),
        (f"valloc: `cat /VA2` prints SRC's src ({printed(out, 'cat /VA2')})", printed(out, "cat /VA2") == ["src"]),
        (f"valloc: rm /VA1 freed its data cluster and its vendor's two ({free(r, r + 1, r + 2)})",
         not any(free(r, r + 1, r + 2))),
        (f"valloc: the mv over /VA2 freed its old vendor chain, bitmap and FAT "
         f"({free(r + 4, r + 5)}, {[vol.entry(k) for k in (r + 4, r + 5)]})",
         not any(free(r + 4, r + 5)) and vol.entry(r + 4) == 0 and vol.entry(r + 5) == 0),
        (f"valloc: ... and kept SRC's data and vendor cluster, VA2's set now naming both ({free(r + 9, r + 10)})",
         all(free(r + 9, r + 10)) and va2 is not None and va2.data[-32:] == sets["SRC"][-32:]),
        (f"valloc: rmdir /VD freed its cluster and its vendor's ({free(r + 7, r + 8)})", not any(free(r + 7, r + 8))),
        (f"valloc: rm /VE2 freed what its undefined benign secondary names ({free(r + 13)})", not used[r + 13]),
        ("valloc: two renames, the second by case alone through mv -f, kept VA3's vendor cluster, "
         "va4's set carrying its entry as planted",
         used[r + 6] and va4 is not None and va4.name == "va4" and va4.data[-32:] == sets["VA3"][-32:]),
        (f"valloc: every witness cluster still used: W after VA1's run, W2, W3 under VE0's fields, "
         f"SRC's after VD's ({free(r + 3, r + 11, r + 12, r + 14, r + 9)})",
         all(free(r + 3, r + 11, r + 12, r + 14, r + 9))),
        ("valloc: fsck_exfat -n passes the image after the boot", ok),
        ("valloc: no fault lines", faults == 0),
    ]
    if not ok:
        notes += [f"valloc after: {ln}" for ln in lines]
    return checks, notes


def run_vcross():
    """Damaged sets, which fsck_exfat flags before the boot: allocations two
    sets share, one a set names twice, and ones naming no cluster of the
    heap or the volume's own. Each removal must free only what no live set
    or the volume needs, and leave the volume fsck clean."""
    start = copy()
    vol = Volume(IMAGE, start)
    r = free_run(vol, 7)
    if r is None:
        return [("vcross: seven free clusters in a run on the image", False)], []
    last = vol.count + 1  # the heap's last cluster
    if vol.used(last) or r + 7 > last:
        return [("vcross: the heap's last cluster free, after the run", False)], []
    c = vol.cluster
    up = system_clusters(vol)
    if not up:
        return [("vcross: the image's root names its up-case table", False)], []
    up = up[0]
    E1 = 0xE1
    sets = {
        # SRC2 and DST2 share data and a vendor cluster: a cross-link.
        "SRC2": plant_set("SRC2", r, 5, [secondary(E1, r + 1, c, 0x03)]),
        "DST2": plant_set("DST2", r, 5, [secondary(E1, r + 1, c, 0x03)]),
        # VA5's vendor run starts at its own data and runs into W5's cluster.
        "VA5": plant_set("VA5", r + 2, 7, [secondary(E1, r + 2, 2 * c, 0x03)]),
        "W5": plant_set("W5", r + 3, c),
        # Allocations outside the heap, and on the bitmap and up-case table.
        "VBAD": plant_set("VBAD", extra=[secondary(E1, 0xFFFFFFF0, c, 0x01)]),
        "VSYS": plant_set("VSYS", extra=[secondary(E1, vol.bitmap, c, 0x03), secondary(E1, up, c, 0x01)]),
        # VA6's vendor run starts a cluster before its data and ends a
        # cluster after: W6's and W7's, the first clusters differing.
        "W6": plant_set("W6", r + 4, c),
        "VA6": plant_set("VA6", r + 5, 7, [secondary(E1, r + 4, 3 * c, 0x03)]),
        "W7": plant_set("W7", r + 6, c),
        # VHEAP: contiguous, on the heap's last cluster, sized three clusters,
        # two past the heap: a write chains it first (ensure_chained).
        "VHEAP": plant_set("VHEAP", last, 3 * c),
    }
    if not plant(vol, start, list(sets.values()), list(range(r, r + 7)) + [last],
                 fills=[(r, b"two\r\n"), (r + 2, b"hello\r\n"), (r + 5, b"hello\r\n")]):
        return [("vcross: room for the planted sets in the root", False)], []
    # The bitmap's own bit cleared, as main's rm of a set naming it left it.
    with open(IMAGE, "r+b") as fh:
        at = vol.heap + (vol.bitmap - 2) * c + (vol.bitmap - 2) // 8
        fh.seek(at)
        b = fh.read(1)[0]
        fh.seek(at)
        fh.write(bytes([b & ~(1 << (vol.bitmap - 2) % 8)]))
        fh.seek(vol.fat + (last + 1) * 4)
        past = fh.read(8)  # the FAT's bytes for the two clusters past the heap
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    checks = [("vcross: every planted set reads back, its clusters and the up-case table's used, the bitmap's clear",
               all(exfatimg.find(root, n) is not None and exfatimg.find(root, n).data == p for n, p in sets.items())
               and all(vol.used(r + k) for k in range(7)) and vol.used(last) and not vol.used(vol.bitmap)
               and vol.used(up))]
    if not checks[0][1]:
        return checks, []
    cmds = ["echo new > /NEW", "mv -f /SRC2 /DST2", "cat /DST2", "rm /VA5", "rm /VBAD", "rm /VSYS", "rm /VA6",
            "writeat /VHEAP 0 x", "rm /VHEAP", "ls /"]
    driven, out, faults = boot("vcross", cmds)
    if drive_qemu.boot_stalled("test-long-set", "vcross", out, faults):
        STALLED.append("vcross")
        return checks, []
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    gone = [n for n in ("SRC2", "VA5", "VBAD", "VSYS", "VA6", "VHEAP") if exfatimg.find(root, n) is None]
    dst2, new = exfatimg.find(root, "DST2"), exfatimg.find(root, "NEW")
    new_first = struct.unpack_from("<I", new.data, 52)[0] if new else 0
    with open(IMAGE, "rb") as fh:
        fh.seek(vol.fat + (last + 1) * 4)
        past_after = fh.read(8)
    ok, lines = fsck()
    checks += [
        ("vcross: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"vcross: SRC2, VA5, VBAD, VSYS, VA6 and VHEAP gone from the root ({gone})", len(gone) == 6),
        (f"vcross: every mv and rm answered nothing, VBAD's outside the heap too ({printed(out, 'rm /VBAD')})",
         all(printed(out, k) == [] for k in cmds if k.split()[0] in ("mv", "rm"))),
        (f"vcross: the allocation passed over the bitmap's clear bit, setting it (NEW at {new_first}, "
         f"the bitmap at {vol.bitmap})",
         new is not None and new_first >= 2 and new_first != vol.bitmap and vol.used(vol.bitmap)
         and printed(out, "cat /DST2") == ["two"]),
        (f"vcross: rm /VA6 freed its data, not W6's or W7's, which its vendor run covers "
         f"({vol.used(r + 4)}, {vol.used(r + 5)}, {vol.used(r + 6)})",
         vol.used(r + 4) and not vol.used(r + 5) and vol.used(r + 6)),
        (f"vcross: `writeat /VHEAP 0 x` answered an error and wrote no FAT entry past the heap "
         f"({printed(out, 'writeat /VHEAP 0 x')}, {past.hex()} -> {past_after.hex()})",
         printed(out, "writeat /VHEAP 0 x") != [] and past_after == past),
        (f"vcross: `cat /DST2` prints two ({printed(out, 'cat /DST2')})", printed(out, "cat /DST2") == ["two"]),
        (f"vcross: the mv over DST2 kept the data and vendor cluster SRC2 shares with it ({vol.used(r)}, {vol.used(r + 1)})",
         vol.used(r) and vol.used(r + 1) and dst2 is not None and dst2.data[-32:] == sets["SRC2"][-32:]),
        (f"vcross: rm /VA5 freed its data, not W5's cluster its vendor run reaches ({vol.used(r + 2)}, {vol.used(r + 3)})",
         not vol.used(r + 2) and vol.used(r + 3)),
        (f"vcross: rm /VSYS left the bitmap's and the up-case table's clusters used ({vol.used(vol.bitmap)}, {vol.used(up)})",
         vol.used(vol.bitmap) and vol.used(up)),
        ("vcross: fsck_exfat -n passes the image after the boot", ok),
        ("vcross: no fault lines", faults == 0),
    ]
    notes = [f"vcross after: {ln}" for ln in lines] if not ok else []
    return checks, notes


def run_extent(kind):
    """The root's 0x82 (up-case table) or 0x81 (bitmap) entry with a damaged
    DataLength, which fsd must take as absent, restored on the host after the
    boot so fsck_exfat reads the volume as fsd left it."""
    label = {0x82: "vupcase", 0x81: "vbitmap"}[kind]
    start = copy()
    vol = Volume(IMAGE, start)
    r = free_run(vol, 1)
    c = vol.cluster
    d = bytearray(vol.data(vol.chain(vol.root)))
    at = next((i for i in range(0, len(d), 32) if d[i] == kind), None)
    if r is None or at is None:
        return [(f"{label}: a free cluster and the root's {kind:#x} entry", False)], []
    if not plant(vol, start, [plant_set("VF", r, 7)], [r], fills=[(r, b"hello\r\n")]):
        return [(f"{label}: room for VF in the root", False)], []
    good = d[at + 24:at + 32]
    bad = struct.pack("<Q", vol.count * c if kind == 0x82 else struct.unpack("<Q", good)[0] + 512)
    patch_entry(vol, at, bad)
    # NEWB first, so its cluster is not the one rm gives back.
    cmds = ["echo new > /NEWB", "rm /VF", "ls /"]
    driven, out, faults = boot(label, cmds)
    if drive_qemu.boot_stalled("test-long-set", label, out, faults):
        STALLED.append(label)
        return [], []
    patch_entry(vol, at, good)
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    vf, newb = exfatimg.find(root, "VF"), exfatimg.find(root, "NEWB")
    newb_size = struct.unpack_from("<Q", newb.data, 56)[0] if newb else 0
    ok, lines = fsck()
    checks = [(f"{label}: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out)]
    if kind == 0x82:
        checks += [(f"{label}: an up-case table claiming the heap is no table: rm /VF freed VF's cluster "
                    f"({vol.used(r)}), and NEWB was written", vf is None and not vol.used(r) and newb_size == 5
                    and printed(out, "rm /VF") == [])]
    else:
        checks += [(f"{label}: a bitmap a sector too long is no bitmap: rm /VF refused with VF and its cluster "
                    f"kept ({printed(out, 'rm /VF')}, {vol.used(r)}), NEWB's write refused "
                    f"({printed(out, 'echo new > /NEWB')}, {newb_size} bytes)",
                    vf is not None and vol.used(r) and printed(out, "rm /VF") != []
                    and printed(out, "echo new > /NEWB") != [] and newb_size == 0)]
    checks += [(f"{label}: fsck_exfat -n passes the image after the boot, the entry restored", ok),
               (f"{label}: no fault lines", faults == 0)]
    return checks, [f"{label} after: {ln}" for ln in lines] if not ok else []


def patch_entry(vol, at, length):
    """The root's entry at byte `at`: its DataLength set to `length` (8 bytes)."""
    d = bytearray(vol.data(vol.chain(vol.root)))
    d[at + 24:at + 32] = length
    write_root(vol, d)


def main():
    checks, notes = [], []
    for run in (run_long, run_shape, run_rename, run_valloc, run_vcross,
                lambda: run_extent(0x82), lambda: run_extent(0x81)):
        try:
            c, n = run()
        finally:
            if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
                os.remove(IMAGE)
        checks += c
        notes += n
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    for n in notes:
        print("      " + n)
    return drive_qemu.verdict("test-long-set", failed, len(STALLED))


if __name__ == "__main__":
    sys.exit(main())

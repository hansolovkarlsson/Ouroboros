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
slot followed the File. Two boots, each of a copy of build/espexfat.img
(exFAT root, partition 1):

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
  new ones), and `ls /` lists each name once.

No fault lines in QEMU's trace. Fails with the sum capped at 19 entries (the
checksum, and fsck: "Incorrect checksum for directory entry set. Delete?")
and with walk_dir taking either misshapen set (its slots written). About two
minutes. Run it whenever exFAT's patch_stream_ext, set_checksum, walk_dir's
set assembly or slot_at changes.
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
    s = long_set(stamped=True)
    if end + len(s) + 32 > len(d):
        return [("rename: room for /VEND's set in the root", False)], []
    d[end:end + len(s)] = s
    write_root(vol, d)
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    ok, lines = fsck()
    checks = [
        ("rename: /VEND's stamped set reads back, 20 entries, its checksum whole",
         hit is not None and hit.count == 20 and hit.data == s and s[4] == 0x20
         and struct.unpack_from("<H", s, 2)[0] == set_checksum(s)),
        ("rename: fsck_exfat -n passes the image before the boot", ok),
    ]
    notes = [f"rename before: {ln}" for ln in lines] if not ok else []
    if not all(c for _, c in checks):
        return checks, notes
    cmds = [f"mv /VEND /{LONG_NAME}", "ls /", f"mv /{LONG_NAME} /V2", "ls /"]
    driven, out, faults = boot("rename", cmds)
    listings = [blk.split("# ", 1)[0].split() for blk in out.split("# ls /")[1:3]]
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    hit = exfatimg.find(root, "V2")
    got = hit.data if hit else b""
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
        ("rename: its SetChecksum is the sum of all 20 entries",
         hit is not None and struct.unpack_from("<H", got, 2)[0] == set_checksum(got)),
        ("rename: fsck_exfat -n passes the image after the boot", ok),
        ("rename: no fault lines", faults == 0),
    ]
    if not ok:
        notes += [f"rename after: {ln}" for ln in lines]
    return checks, notes


def main():
    checks, notes = [], []
    for run in (run_long, run_shape, run_rename):
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
    print(f"test-long-set: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

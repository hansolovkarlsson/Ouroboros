#!/usr/bin/env python3
"""An exFAT entry set longer than any fsd builds keeps a whole checksum.

    python3 scripts/test-long-set.py     (or `make test-long-set`, which builds the image)

When fsd changes a file's Stream entry (its first cluster and length, on a
write), `patch_stream_ext` recomputes the set's SetChecksum. Until
2026-10-19 it summed at most MAX_SET_ENTRIES (19) entries, the longest set
fsd itself builds, but a set may be longer: the spec allows vendor
secondaries after the name, and cameras write them. Such a set was left with
a checksum over part of it, which fsck_exfat reports and may repair by
deleting the set. File and Stream also went out as two sector writes even in
one sector.

On a copy of build/espexfat.img (exFAT root, partition 1) the host plants in
the root directory /VEND, an empty file whose set is 20 entries: File,
Stream, one File-Name entry and 17 Vendor Extension entries (0xE0, a GUID
and a byte pattern of their own), its checksum over all 20. Before the boot
the host reads it back and fsck_exfat -n must pass the image (the observer's
own check). On the guest, `echo hello >> /VEND` (a write that gives it a
cluster: patch_stream_ext) and `cat /VEND`. After the boot the set must still
be the 20 entries, the vendor entries byte for byte as planted, the Stream
naming a cluster and the file's length, the stored SetChecksum the sum of all
20 entries, the file's data `hello`, and fsck_exfat -n must pass the image
again. Fails with the sum capped at 19 entries again (the checksum, and
fsck). One boot, about a minute. Run it whenever exFAT's patch_stream_ext,
set_checksum, walk_dir's set assembly or slot_addr changes.
"""
import importlib.util
import os
import shutil
import struct
import subprocess
import sys

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
grow_dir = load("test_grow_dir", "test-grow-dir.py")
Volume, set_checksum, entry_set = stale_cursor.Volume, stale_cursor.set_checksum, grow_dir.entry_set

IMAGE = os.path.join(ROOT, "build", "test-long-set.img")
NAME = "VEND"
VENDOR = 17
GUID = bytes(range(0x10, 0x20))


def vendor_entry(k):
    """A Vendor Extension entry (0xE0): flags, a GUID, then data of its own."""
    e = bytearray(32)
    e[0] = 0xE0
    e[2:18] = GUID
    e[18:32] = bytes((k * 14 + j) & 0xFF for j in range(14))
    return bytes(e)


def the_set():
    s = bytearray(entry_set(NAME))
    s += b"".join(vendor_entry(k) for k in range(VENDOR))
    s[1] = len(s) // 32 - 1  # SecondaryCount
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def find(vol, name):
    """(byte offset in the root's data, entry count, the set's bytes) of the
    set named `name`, by its declared SecondaryCount, or None; and the offset
    of the root's end (its first 0x00 slot)."""
    d = vol.data(vol.chain(vol.root))
    i, hit = 0, None
    while i < len(d) and d[i]:
        count = d[i + 1] + 1 if d[i] == 0x85 else 1
        if d[i] == 0x85 and d[i + 32] == 0xC0:
            chars = d[i + 32 + 3]
            got = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count) if d[i + 32 * k] == 0xC1)
            if got.decode("utf-16le", "replace")[:chars] == name:
                hit = (i, count, d[i:i + 32 * count])
        i += 32 * count
    return hit, i, d


def plant(start):
    vol = Volume(IMAGE, start)
    hit, end, d = find(vol, NAME)
    s = the_set()
    if hit or end + len(s) + 32 > len(d):
        sys.exit(f"test-long-set: /{NAME} exists, or no room for {len(s) // 32} entries in the root")
    root = vol.chain(vol.root)
    with open(IMAGE, "r+b") as fh:
        d = bytearray(d)
        d[end:end + len(s)] = s
        for k, c in enumerate(root):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])


def fsck():
    """fsck_exfat -n on the image's exFAT partition: (passed, its lines)."""
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", IMAGE],
                         capture_output=True, text=True)
    if att.returncode != 0:
        return False, [att.stderr.strip()]
    dev = att.stdout.split()[0]
    try:
        r = subprocess.run(["fsck_exfat", "-n", dev + "s1"], capture_output=True, text=True)
        lines = (r.stdout + r.stderr).strip().splitlines()
        problems = [ln for ln in lines if ln.startswith("   ") or ln.endswith("? NO")]
        return r.returncode == 0 and not problems and "** Checking file system hierarchy." in lines, lines
    finally:
        subprocess.run(["hdiutil", "detach", dev], capture_output=True)


def main():
    checks, notes = [], []
    try:
        shutil.copyfile(os.path.join(ROOT, "build", "espexfat.img"), IMAGE)
        with open(IMAGE, "rb") as fh:
            parts = mbrdisk.entries(fh.read(512))
        start = [p for p in parts if p[1] == 0x07][0][2]
        plant(start)
        hit, _, _ = find(Volume(IMAGE, start), NAME)
        planted = the_set()
        ok, lines = fsck()
        checks += [
            ("plant: /VEND's set reads back, 20 entries, its checksum over all of them",
             hit is not None and hit[1] == 20 and hit[2] == planted
             and struct.unpack_from("<H", planted, 2)[0] == set_checksum(planted)),
            ("plant: fsck_exfat -n passes the image", ok),
        ]
        if not ok:
            notes += [f"before: {ln}" for ln in lines]
        if not all(c for _, c in checks):
            raise RuntimeError
        guest = drive_qemu.Guest(IMAGE, label="test-long-set: ")
        try:
            driven = guest.run([("login:", "root"), ("assword", "root"), ("# ", f"echo hello >> /{NAME}"),
                                ("# ", f"cat /{NAME}"), ("# ", "")])
            out = guest.transcript()
            faults = guest.aborts()
        finally:
            guest.stop()
        with open(os.path.join(ROOT, "build", "test-long-set.txt"), "w") as fh:
            fh.write(out)
        vol = Volume(IMAGE, start)
        hit, _, _ = find(vol, NAME)
        s = hit[2] if hit else b""
        first, size = (struct.unpack_from("<I", s, 52)[0], struct.unpack_from("<Q", s, 56)[0]) if hit else (0, 0)
        data = vol.data(vol.chain(first))[:size] if first >= 2 else b""
        ok, lines = fsck()
        checks += [
            ("driven to the end", driven),
            ("fsd mounted exFAT", "fsd: exFAT mounted" in out),
            ("/VEND's set still 20 entries, File, Stream, a name, 17 vendor entries",
             hit is not None and hit[1] == 20 and s[0] == 0x85 and s[32] == 0xC0 and s[64] == 0xC1
             and all(s[32 * k] == 0xE0 for k in range(3, 20))),
            ("its vendor entries byte for byte as planted", s[96:] == planted[96:]),
            (f"its Stream names a cluster and the file's length (cluster {first}, {size} bytes)",
             first >= 2 and size == len(data) and size > 0),
            ("its SetChecksum is the sum of all 20 entries",
             hit is not None and struct.unpack_from("<H", s, 2)[0] == set_checksum(s)),
            (f"its data is hello ({data!r})", data.startswith(b"hello")),
            ("fsck_exfat -n passes the image after the boot", ok),
            ("no fault lines", faults == 0),
        ]
        if not ok:
            notes += [f"after: {ln}" for ln in lines]
    except RuntimeError:
        pass
    finally:
        if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
            os.remove(IMAGE)
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

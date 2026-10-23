#!/usr/bin/env python3
"""exFAT bytes past a file's ValidDataLength are zeros, read or written.

    python3 scripts/test-valid-length.py     (or `make test-valid-length`, which builds the image)

An exFAT file's Stream entry carries two lengths: DataLength, the file's
size, and ValidDataLength, the bytes written; a driver that preallocates
leaves the second below the first, and the spec has the bytes between them
read as zeros, whatever the clusters hold. Until 2026-10-22 fsd read to
DataLength (the stale bytes read as the file's), filled a write's gap from
DataLength (leaving them in place) and wrote both lengths as one on every
write (raising ValidDataLength over them, for every driver after). One boot of
a copy of build/espexfat.img (exFAT root, partition 1), where the host plants
five files, each a contiguous run of its own (NoFatChain, one cluster but
BIG's) with DataLength the whole run, ValidDataLength 7, `hello\\r\\n` then
0xEE to the run's end:

- RD: `cp /RD /RDC`; RDC must be the cluster's size, `hello\\r\\n` then zeros,
  its ValidDataLength its size, and RD's set and cluster as planted;
- MID: `writeat /MID 100 x`, inside the preallocation; ValidDataLength must be
  101 and DataLength the cluster's, the bytes `hello\\r\\n`, zeros, `x`; then
  `cp /MID /MIDC`, which must read zeros past the `x` (the rest of its sector
  still holds 0xEE on the disk, past the valid length);
- APP: `echo hi >> /APP`, at DataLength; both lengths the cluster's size and
  4, the gap zeros;
- LOW: `writeat /LOW 2 Z`, inside the valid bytes; ValidDataLength still 7,
  DataLength still the cluster's, the bytes `heZlo\\r\\n`;
- NEW: `write /NEW made`, a file made whole with its data (`write_file`, not
  `write_at`); ValidDataLength its size;
- BIG: 2 MiB preallocated, ValidDataLength 7: `echo y >> /BIG`, more than
  MAX_GAP_FILL (1 MiB) past the valid bytes, must be refused (Hans, option A),
  its set and bytes as planted.

fsck_exfat -n passes the image before the boot and after it; no fault lines.
Fails with the read ignoring ValidDataLength (RDC and MIDC hold 0xEE), with
the gap filled from DataLength (MID's and APP's 0xEE), with both lengths
written as one (MID's, LOW's ValidDataLength the cluster's), with
ValidDataLength not decoded (main's reads and writes), with a written
file's ValidDataLength short of its size (RDC's and MIDC's) and with a file
made with its data given ValidDataLength 0 (NEW's) and with the gap bound
measured from the size (BIG's append taken). A
boot QEMU's firmware never starts is INCONCLUSIVE (exit 100). About a minute.
Run it whenever exFAT's read_entry, read_file, write_at, patch_stream_ext or
walk_dir's Stream decoding changes.
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

IMAGE = os.path.join(ROOT, "build", "test-valid-length.img")
HELLO = b"hello\r\n"
STALE = 0xEE
FILES = ("RD", "MID", "APP", "LOW", "BIG")
# BIG's DataLength: past ValidDataLength by more than fsd's MAX_GAP_FILL
# (1 MiB), so an append at its size must be refused.
BIG_BYTES = 2 << 20


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


def span(vol, name):
    """The clusters `name` is planted with: one, BIG's 2 MiB."""
    return BIG_BYTES // vol.cluster if name == "BIG" else 1


def planted_bytes(vol, n):
    return HELLO + bytes([STALE]) * (n * vol.cluster - len(HELLO))


def preallocated(vol, name, cluster):
    """`name`'s set: a contiguous run of its span, DataLength the run's,
    ValidDataLength 7."""
    s = bytearray(entry_set(name))
    s[33] = 0x03  # GeneralSecondaryFlags: AllocationPossible, NoFatChain
    struct.pack_into("<Q", s, 40, len(HELLO))
    struct.pack_into("<I", s, 52, cluster)
    struct.pack_into("<Q", s, 56, span(vol, name) * vol.cluster)
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def free_runs(vol, names):
    """A free contiguous run for each name, in order: {name: first cluster}."""
    out, c = {}, 2
    for n in names:
        need = span(vol, n)
        while c + need <= vol.count + 2 and any(vol.used(k) for k in range(c, c + need)):
            c += 1
        if c + need > vol.count + 2:
            return None
        out[n] = c
        c += need
    return out


def plant():
    """The five files in the root, each set read back. Returns (start, the
    planted sets by name, the checks)."""
    start = copy()
    vol = Volume(IMAGE, start)
    chain = vol.chain(vol.root)
    d = bytearray(vol.data(chain))
    end = root_end(d)
    first = free_runs(vol, FILES)
    if first is None:
        return start, {}, [("plant: a free run for each file", False)]
    sets = {n: preallocated(vol, n, c) for n, c in first.items()}
    blob = b"".join(sets.values())
    if end + len(blob) + 32 > len(d):
        return start, sets, [("plant: room for the five sets in the root", False)]
    d[end:end + len(blob)] = blob
    with open(IMAGE, "r+b") as fh:
        for k, c in enumerate(chain):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])
        for n, f in first.items():
            for c in range(f, f + span(vol, n)):
                fh.seek(vol.heap + (vol.bitmap - 2) * vol.cluster + (c - 2) // 8)
                b = fh.read(1)[0]
                fh.seek(-1, 1)
                fh.write(bytes([b | 1 << (c - 2) % 8]))
            fh.seek(vol.heap + (f - 2) * vol.cluster)
            fh.write(planted_bytes(vol, span(vol, n)))
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    back = all((h := exfatimg.find(root, n)) is not None and h.data == s for n, s in sets.items())
    stale = all(vol.data(list(range(f, f + span(vol, n)))) == planted_bytes(vol, span(vol, n))
                and all(vol.used(c) for c in range(f, f + span(vol, n))) for n, f in first.items())
    return start, sets, [
        ("plant: the five sets read back as planted, each checksum whole",
         back and all(struct.unpack_from("<H", s, 2)[0] == set_checksum(s) for s in sets.values())),
        (f"plant: each file's clusters used, hello\\r\\n then 0xEE to their end ({vol.cluster}-byte clusters, "
         f"BIG {BIG_BYTES} bytes)", stale),
    ]


def stream(vol, root, name):
    """(ValidDataLength, first cluster, DataLength, data to DataLength), or None."""
    hit = exfatimg.find(root, name)
    if hit is None:
        return None
    s = hit.data
    valid, first, size = struct.unpack_from("<Q", s, 40)[0], struct.unpack_from("<I", s, 52)[0], struct.unpack_from("<Q", s, 56)[0]
    contiguous = s[33] & 0x02
    n = -(-size // vol.cluster)
    clusters = list(range(first, first + n)) if contiguous else vol.chain(first)
    return valid, first, size, vol.data(clusters)[:size] if first >= 2 else b""


def fsck(when):
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not exfatimg.complaints(lines) and not exfatimg.unexpected(lines))
    return ok, [] if ok else [f"{when}: {ln}" for ln in lines]


def main():
    try:
        start, sets, checks = plant()
        ok, notes = fsck("before")
        checks.append(("fsck_exfat -n passes the image before the boot", ok))
        stalled = 0
        if all(c for _, c in checks):
            cmds = ["cp /RD /RDC", "writeat /MID 100 x", "echo hi >> /APP", "writeat /LOW 2 Z", "cp /MID /MIDC", "write /NEW made", "echo y >> /BIG"]
            steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in cmds] + [("# ", "")]
            guest = drive_qemu.Guest(IMAGE, label="test-valid-length: ")
            try:
                driven = guest.run(steps)
                out = guest.transcript()
                faults = guest.aborts()
            finally:
                guest.stop()
            with open(os.path.join(ROOT, "build", "test-valid-length.txt"), "w") as fh:
                fh.write(out)
            if drive_qemu.boot_stalled("test-valid-length", "boot", out, faults):
                stalled = 1
            else:
                checks += graded(start, sets, driven, out, faults)
                ok, after = fsck("after")
                checks.append(("fsck_exfat -n passes the image after the boot", ok))
                notes += after
    finally:
        if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
            os.remove(IMAGE)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    for n in notes:
        print("      " + n)
    return drive_qemu.verdict("test-valid-length", failed, stalled)


def graded(start, sets, driven, out, faults):
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    size = vol.cluster
    got = {n: stream(vol, root, n) for n in FILES + ("RDC", "MIDC", "NEW")}
    new = got["NEW"] or (0, 0, 0, b"")
    none = (0, 0, 0, b"")
    rd, mid, app, low, big, rdc, midc = (got[n] or none for n in FILES + ("RDC", "MIDC"))
    zeros = lambda n: bytes(n)  # noqa: E731
    mid_want = HELLO + zeros(100 - len(HELLO)) + b"x"
    return [
        ("driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        ("RD's set and cluster as planted",
         (h := exfatimg.find(root, "RD")) is not None and h.data == sets["RD"]
         and rd[3] == HELLO + bytes([STALE]) * (size - len(HELLO))),
        (f"RDC, RD's copy, is {size} bytes, hello\\r\\n then zeros ({rdc[2]} bytes, "
         f"{rdc[3].count(STALE)} of 0xEE)",
         rdc[2] == size and rdc[3] == HELLO + zeros(size - len(HELLO))),
        (f"RDC's ValidDataLength is its size ({rdc[0]})", rdc[0] == rdc[2] == size),
        (f"MID: ValidDataLength 101, DataLength {size} ({mid[0]}, {mid[2]})", mid[0] == 101 and mid[2] == size),
        ("MID: hello\\r\\n, zeros to 100, then x", mid[3][:101] == mid_want),
        (f"MIDC, MID's copy, reads zeros past the x ({midc[3].count(STALE)} of 0xEE)",
         midc[2] == size and midc[3] == mid_want + zeros(size - 101)),
        (f"MIDC's ValidDataLength is its size ({midc[0]})", midc[0] == midc[2] == size),
        (f"APP: both lengths {size + 4} ({app[0]}, {app[2]})", app[0] == app[2] == size + 4),
        (f"APP: hello\\r\\n, zeros, then hi\\r\\n ({app[3].count(STALE)} of 0xEE)",
         app[3] == HELLO + zeros(size - len(HELLO)) + b"hi\r\n"),
        (f"LOW: ValidDataLength still 7, DataLength {size} ({low[0]}, {low[2]})", low[0] == 7 and low[2] == size),
        ("LOW: its valid bytes heZlo\\r\\n", low[3][:7] == b"heZlo\r\n"),
        (f"NEW, made whole by `write /NEW made`, has ValidDataLength its size ({new[0]}, {new[2]})",
         new[2] == 4 and new[0] == 4 and new[3] == b"made"),
        (f"BIG: the append past the gap bound refused, its set as planted ({big[0]}, {big[2]})",
         (h := exfatimg.find(root, "BIG")) is not None and h.data == sets["BIG"]),
        (f"BIG: its {BIG_BYTES} bytes as planted ({big[3].count(STALE)} of 0xEE)",
         big[3] == planted_bytes(vol, span(vol, "BIG"))),
        ("no fault lines", faults == 0),
    ]


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""A failed exFAT `mv` is rolled back: both files as they were, never two sets.

    python3 scripts/test-mv-undo.py     (or `make test-mv-undo`, which builds the images)

A `mv` on exFAT writes the source's set again under the new name (`relink`)
and then deletes the old one; a replacing `mv` deletes the destination's
set too. Until 2026-10-26 a failure at a delete returned the error with
the new set left on the disk beside the source's, both naming the source's
clusters, so a later `rm` of either freed them under the other (the high
review of 2026-10-24). A delete clears each slot's in-use bit and nothing
else, and the clusters are freed only once both sets are gone, so fsd now
rolls a failure back: the sets deleted put back (`put_back`, the bits set
again) and the new set cleared (`move_set`), and a failed `mv -f` leaves
both files as they were, as POSIX `rename` does. (A first version gave the
new set back alone, which with the destination deleted first destroyed the
destination: the high review of #357.)

One boot. The firmware boots build/esp.img from NVMe (which the kernel
cannot drive), and virtio holds a copy of build/espexfat.img through QEMU's
blkdebug with every write of two sectors of the exFAT root, X and Y (X + 2),
failing EIO (test-probe-ioerr's shape, for writes alone: `iotype =
"write"`, or a read of them fails too once any write has been made). The
host gives the root a second cluster and plants on it, after its last set:
U, W and W2, each a file on a cluster of its own; at least 16 deleted
slots, where the boot's new sets go; S2's File entry in the last slot
before X, its Stream and name at X's start; S whole in X; deleted slots to
the last slot before Y, V's File entry there and its Stream and name at Y's
start. Before the boot each reads back where it was meant to and fsck_exfat
-n passes. Then, logged in, each `mv` must answer an I/O error and leave
every file as it was:

- `mv /S2 /T2`: S2's delete clears its File entry and fails at X, so S2 is
  put back and T2 cleared (`cat /S2` prints `s2`);
- `mv /S /T`, `mv -f /S /s` (case-only) and `mv -f /S /U` (replacing): S's
  delete fails at X untouched, the new set cleared (`cat /U` prints `u`);
- `mv -f /W /V`: W's delete works, V's clears its File entry and fails at
  Y, so V and W are put back and the new V cleared (`cat /V` prints `v`,
  `cat /W` `w`);
- `mv -f /W2 /S`: W2's delete works, S's fails at X untouched, so W2 is put
  back and the new S cleared (`cat /W2` prints `w2`, `cat /S` `src`);
- `ls /` lists the six and none of T, T2 or s, and fsd prints no line that
  a rollback failed.

After the boot, on the host: each of the six once, on its own cluster,
every one used, none named T, T2 or s, and fsck_exfat -n clean (S2's and
V's sets whole: their entries at X and Y were never cleared). No fault
lines; a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100).
Fails with the undo removed (T and U left on S's cluster), with a source
not put back (T2 kept, S2 gone), with the destination's failure not rolled
back (two files named S), with a destination not put back (V's bytes W's),
and with the destination deleted before the source (U lost). Not graded
by a boot: a rollback whose own writes fail, which completes the move or
leaves two sets with a line, and relink's own partial write cleared
(insert_set). About a minute. Run it whenever exFAT's mv, move_set,
put_back, relink, file_in_use, clear_slots, insert_set, unlink_set or
delete_set changes.
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
Volume, set_checksum = stale_cursor.Volume, stale_cursor.set_checksum
entry_set = load("test_grow_dir", "test-grow-dir.py").entry_set

BUILD = os.path.join(ROOT, "build")
ESP = os.path.join(BUILD, "esp.img")
IMAGE = os.path.join(BUILD, "test-mv-undo.img")
BLKDEBUG = os.path.join(BUILD, "test-mv-undo.blkdebug")
SLOT, SECTOR = 32, 512
DELETED = bytes([0x05]) + bytes(31)  # a deleted File entry: in-use bit clear
FILES = {"U": b"u\r\n", "W": b"w\r\n", "W2": b"w2\r\n", "S2": b"s2\r\n", "S": b"src\r\n", "V": b"v\r\n"}
FAILS = ["mv /S2 /T2", "mv /S /T", "mv -f /S /s", "mv -f /S /U", "mv -f /W /V", "mv -f /W2 /S"]
CATS = ["S2", "U", "V", "W", "W2", "S"]  # each read after every mv has run
CMDS = FAILS + [f"cat /{n}" for n in CATS] + ["ls /"]


def contiguous_set(name, cluster, size):
    """`name`'s set (File, Stream, one name entry) on one contiguous cluster."""
    s = bytearray(entry_set(name, cluster, size))
    s[33] = 0x03  # AllocationPossible, NoFatChain
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def root_end(d):
    i = 0
    while i < len(d) and d[i]:
        i += SLOT
    return i


def plant():
    """The copy, planted: (partition start, {name: cluster}, [X, Y] as
    absolute sectors), or a reason the root had no room."""
    shutil.copyfile(os.path.join(BUILD, "espexfat.img"), IMAGE)
    with open(IMAGE, "rb") as fh:
        start = [p for p in mbrdisk.entries(fh.read(SECTOR)) if p[1] == 0x07][0][2]
    vol = Volume(IMAGE, start)
    free = [c for c in range(2, vol.count + 2) if not vol.used(c)][:len(FILES) + 1]
    # The root given a second cluster, zeroed, so the plants fit.
    grown, free = free[0], free[1:]
    with open(IMAGE, "r+b") as fh:
        for k, value in ((vol.chain(vol.root)[-1], grown), (grown, 0xFFFFFFFF)):
            fh.seek(vol.fat + k * 4)
            fh.write(struct.pack("<I", value))
        fh.seek(vol.heap + (grown - 2) * vol.cluster)
        fh.write(bytes(vol.cluster))
        bit = vol.heap + (vol.bitmap - 2) * vol.cluster + (grown - 2) // 8
        fh.seek(bit)
        byte = fh.read(1)[0]
        fh.seek(bit)
        fh.write(bytes([byte | 1 << (grown - 2) % 8]))
    vol = Volume(IMAGE, start)
    clusters = dict(zip(FILES, free))
    chain = vol.chain(vol.root)
    d = bytearray(vol.data(chain))
    at = root_end(d)
    for name in ("U", "W", "W2"):
        d[at:at + 3 * SLOT] = contiguous_set(name, clusters[name], len(FILES[name]))
        at += 3 * SLOT
    # At least 16 deleted slots, to the last slot before a sector edge: X.
    x = -(-(at + 17 * SLOT) // SECTOR) * SECTOR
    y = x + 2 * SECTOR
    if y + 3 * SLOT > len(d):
        return None, "the root has no room for the plants"
    d[at:x - SLOT] = DELETED * ((x - SLOT - at) // SLOT)
    d[x - SLOT:x + 2 * SLOT] = contiguous_set("S2", clusters["S2"], len(FILES["S2"]))
    d[x + 2 * SLOT:x + 5 * SLOT] = contiguous_set("S", clusters["S"], len(FILES["S"]))
    d[x + 5 * SLOT:y - SLOT] = DELETED * ((y - SLOT - x - 5 * SLOT) // SLOT)
    d[y - SLOT:y + 2 * SLOT] = contiguous_set("V", clusters["V"], len(FILES["V"]))
    with open(IMAGE, "r+b") as fh:
        for k, c in enumerate(chain):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])
        for name, c in clusters.items():
            bit = vol.heap + (vol.bitmap - 2) * vol.cluster + (c - 2) // 8
            fh.seek(bit)
            byte = fh.read(1)[0]
            fh.seek(bit)
            fh.write(bytes([byte | 1 << (c - 2) % 8]))
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(FILES[name] + bytes(vol.cluster - len(FILES[name])))
    failing = [(vol.heap + (chain[b // vol.cluster] - 2) * vol.cluster + b % vol.cluster) // SECTOR for b in (x, y)]
    return (start, clusters, failing), None


def root_sets(start):
    vol = Volume(IMAGE, start)
    return vol, vol.data(vol.chain(vol.root))


def fsck():
    with exfatimg.attached(IMAGE) as dev:
        return exfatimg.fsck(dev + "s1", "fsck_exfat")


def answer(out, cmd, last=False):
    marker = "# " + cmd + "\r"
    if marker not in out:
        return None
    after = out.rsplit(marker, 1)[1] if last else out.split(marker, 1)[1]
    lines = after.split("# ", 1)[0].splitlines()
    return "\n".join(ln for ln in lines if not ln.startswith("Ouroboros kernel:")).strip()


def run():
    placed, why = plant()
    if not placed:
        return [(f"before: {why}", False)], [], 0
    start, clusters, failing = placed
    vol, d = root_sets(start)
    names = [s.name for s in exfatimg.sets(d)]
    rc, lines = fsck()
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not exfatimg.complaints(lines) and not exfatimg.unexpected(lines))
    chain = vol.chain(vol.root)

    def sector_of(name):
        hit = exfatimg.find(d, name)
        return hit and (vol.heap + (chain[hit.offset // vol.cluster] - 2) * vol.cluster
                        + hit.offset % vol.cluster) // SECTOR

    x, y = failing
    checks = [(f"before: the six read back once each, S whole in X ({x}), S2's File entry before it, V's "
               f"before Y ({y}), U, W and W2 before X, and fsck_exfat -n passes",
               all(names.count(n) == 1 for n in FILES) and ok and sector_of("S") == x
               and sector_of("S2") == x - 1 and sector_of("V") == y - 1
               and all(sector_of(n) < x - 1 for n in ("U", "W", "W2")))]
    notes = [f"before: {ln}" for ln in lines] if not ok else []
    if not checks[0][1]:
        return checks, notes, 0

    with open(BLKDEBUG, "w") as fh:
        for sector in failing:
            fh.write(f'[inject-error]\nevent = "write_aio"\niotype = "write"\nerrno = "5"\nsector = "{sector}"\n\n')
    steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in CMDS] + [("# ", "")]
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, which it boots and
        # the kernel cannot drive, so the virtio copy goes in as the fallback.
        "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
        "-drive", f"file=blkdebug:{BLKDEBUG}:{IMAGE},format=raw,if=none,id=hd0,werror=report",
        "-device", "virtio-blk-device,drive=hd0",
    ]
    guest = drive_qemu.Guest(IMAGE, extra_args=extra, virtio_disk=False, stamp_from=ESP,
                             label="test-mv-undo: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        if os.path.exists(BLKDEBUG):
            os.remove(BLKDEBUG)
    with open(os.path.join(BUILD, "test-mv-undo.txt"), "w") as fh:
        fh.write(out)
    if drive_qemu.boot_stalled("test-mv-undo", "boot", out, faults):
        return checks, notes, 1

    vol, d = root_sets(start)
    sets = list(exfatimg.sets(d))
    names = [s.name for s in sets]
    first = {s.name: struct.unpack_from("<I", s.data, 52)[0] for s in sets}
    rc, lines = fsck()
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not exfatimg.complaints(lines) and not exfatimg.unexpected(lines))
    listing = (answer(out, "ls /") or "").split()
    said = {c: answer(out, c) for c in FAILS}
    cats = {n: answer(out, f"cat /{n}") for n in CATS}
    used = {n: vol.used(c) for n, c in clusters.items()}
    checks += [
        ("driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"each mv answered an I/O error ({said})", all("I/O error" in (v or "") for v in said.values())),
        (f"cat of each printed its own bytes ({cats})",
         all(cats[n] == FILES[n].decode().strip() for n in CATS)),
        ("fsd printed no line that a rollback failed", "fsd: a failed mv" not in out),
        (f"ls / lists the six and none of T, T2 or s ({listing})",
         set(FILES) <= set(listing) and not {"T", "T2", "s"} & set(listing)),
        (f"on the disk each of the six once on its own cluster, none named T, T2 or s ({names})",
         all(names.count(n) == 1 and first.get(n) == clusters[n] for n in FILES)
         and not {"T", "T2", "s"} & set(names)),
        (f"every planted cluster used ({used})", all(used.values())),
        ("fsck_exfat -n passes after", ok),
        (f"no fault lines in QEMU's trace ({faults})", faults == 0),
    ]
    if not ok:
        notes += [f"after: {ln}" for ln in lines]
    return checks, notes, 0


def main():
    try:
        checks, notes, stalled = run()
    finally:
        if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
            os.remove(IMAGE)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    for n in notes:
        print("      " + n)
    return drive_qemu.verdict("test-mv-undo", failed, stalled)


if __name__ == "__main__":
    sys.exit(main())

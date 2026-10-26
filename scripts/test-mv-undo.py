#!/usr/bin/env python3
"""A failed exFAT `mv` gives back the set it wrote, never two over one file.

    python3 scripts/test-mv-undo.py     (or `make test-mv-undo`, which builds the images)

A `mv` on exFAT writes the source's set again under the new name (`relink`)
and then deletes the old one; a replacing `mv` deletes the destination's
set between. Until 2026-10-26 a failure at either delete returned the error
with the new set left on the disk beside the source's, both naming the
source's clusters, so a later `rm` of either freed them under the other (the
high review of 2026-10-24). Now `unrelink` deletes the new set, as a rename
deletes, while the source's File entry still reads in use; a source whose
File entry was cleared before the failure is gone, and the new set is kept
as the file's.

One boot. The firmware boots build/esp.img from NVMe (which the kernel
cannot drive), and virtio holds a copy of build/espexfat.img through QEMU's
blkdebug with every write of one sector, X, of the exFAT root failing EIO
(test-probe-ioerr's shape, for writes). The host plants on the root, after
its last set: U (`u`, its own cluster), deleted slots to a sector edge, then
S2's File entry in the sector before X with its Stream and name at X's
start, then S (`src`, its own cluster) whole in X; before the boot each
reads back and fsck_exfat -n passes. Then, logged in:

- `mv /S2 /T2`: the delete clears S2's File entry and fails at X, so S2 is
  gone and T2, the set relink wrote, must stay: `cat /T2` prints `s2`;
- `mv /S /T`, `mv -f /S /s` (a case-only rename) and `mv -f /S /U` (a
  replacing one, U's set deleted before S's delete fails): each answers an
  error and leaves S whole, with no T, no `s` and, for the last, no U;
- `ls /` lists S and T2 and none of S2, T, s or U, and `cat /S` prints
  `src`.

After the boot, on the host, with S2's orphaned Stream and name entries
(still in use at X) cleared first: one set named S, on its cluster and
used; one named T2 on S2's cluster; none named T, s, S2 or U; U's cluster
free; fsck_exfat -n clean. No fault lines; a boot QEMU's firmware never
starts is INCONCLUSIVE (exit 100). Fails with the undo removed (T and a
U on S's cluster left beside S, `mv -f /S /U` answering no error, fsck
failing) and with the undo made whatever the source's File entry reads (T2
deleted with S2: the file lost). Blkdebug's rule names `iotype = "write"`:
without it a read of X fails too, once any write has been made. Not
graded by a boot: an undo that fails itself, and a source whose File entry
does not read after the failure, which both leave the two sets as before.
About a minute. Run it whenever exFAT's mv, relink, unrelink, insert_set,
unlink_set or delete_set changes.
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
FILES = {"U": b"u\r\n", "S2": b"s2\r\n", "S": b"src\r\n"}
CMDS = ["mv /S2 /T2", "cat /T2", "mv /S /T", "mv -f /S /s", "mv -f /S /U", "ls /", "cat /S"]


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
    """The copy, planted: (partition start, {name: cluster}, X's absolute
    sector), or a reason the root had no room."""
    shutil.copyfile(os.path.join(BUILD, "espexfat.img"), IMAGE)
    with open(IMAGE, "rb") as fh:
        start = [p for p in mbrdisk.entries(fh.read(SECTOR)) if p[1] == 0x07][0][2]
    vol = Volume(IMAGE, start)
    free = [c for c in range(2, vol.count + 2) if not vol.used(c)][:len(FILES)]
    clusters = dict(zip(FILES, free))
    chain = vol.chain(vol.root)
    d = bytearray(vol.data(chain))
    at = root_end(d)
    d[at:at + 3 * SLOT] = contiguous_set("U", clusters["U"], len(FILES["U"]))
    at += 3 * SLOT
    # Deleted slots, at least eight (room for the sets the boot writes, T2's
    # among them, before X), to the slot before a sector edge: S2's File.
    x = -(-(at + 9 * SLOT) // SECTOR) * SECTOR
    d[at:x - SLOT] = DELETED * ((x - SLOT - at) // SLOT)
    d[x - SLOT:x + 2 * SLOT] = contiguous_set("S2", clusters["S2"], len(FILES["S2"]))
    d[x + 2 * SLOT:x + 5 * SLOT] = contiguous_set("S", clusters["S"], len(FILES["S"]))
    if x + 6 * SLOT > len(d):
        return None, "the root has no room for the plants"
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
    # X: the sector of the root holding root byte `x`.
    k = x // vol.cluster
    failing = (vol.heap + (chain[k] - 2) * vol.cluster + x % vol.cluster) // SECTOR
    return (start, clusters, failing), None


def root_sets(start):
    vol = Volume(IMAGE, start)
    return vol, vol.data(vol.chain(vol.root))


def fsck():
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not exfatimg.complaints(lines) and not exfatimg.unexpected(lines))
    return ok, lines


def answer(out, cmd, last=False):
    marker = "# " + cmd + "\r"
    if marker not in out:
        return None
    after = out.rsplit(marker, 1)[1] if last else out.split(marker, 1)[1]
    lines = after.split("# ", 1)[0].splitlines()
    return "\n".join(ln for ln in lines if not ln.startswith("Ouroboros kernel:")).strip()


def clear_orphans(start, failing):
    """S2's Stream and name entries, left in use at X when its delete failed
    there, cleared, so fsck reads the volume as fsd meant to leave it."""
    with open(IMAGE, "r+b") as fh:
        fh.seek(failing * SECTOR)
        sec = bytearray(fh.read(SECTOR))
        for i in range(0, 2 * SLOT, SLOT):
            if sec[i] in (0xC0, 0xC1):
                sec[i] &= 0x7F
        fh.seek(failing * SECTOR)
        fh.write(sec)


def run():
    placed, why = plant()
    if not placed:
        return [(f"before: {why}", False)], [], 0
    start, clusters, failing = placed
    vol, d = root_sets(start)
    names = [s.name for s in exfatimg.sets(d)]
    ok, lines = fsck()
    chain = vol.chain(vol.root)

    def sector_of(name):
        hit = exfatimg.find(d, name)
        return hit and (vol.heap + (chain[hit.offset // vol.cluster] - 2) * vol.cluster
                        + hit.offset % vol.cluster) // SECTOR

    checks = [(f"before: U, S2 and S read back once each, S whole in sector X ({failing}), S2's File "
               "entry in the sector before it and U before X, and fsck_exfat -n passes",
               all(names.count(n) == 1 for n in FILES) and ok and sector_of("S") == failing
               and sector_of("S2") == failing - 1 and sector_of("U") < failing)]
    notes = [f"before: {ln}" for ln in lines] if not ok else []
    if not checks[0][1]:
        return checks, notes, 0

    with open(BLKDEBUG, "w") as fh:
        fh.write(f'[inject-error]\nevent = "write_aio"\niotype = "write"\nerrno = "5"\nsector = "{failing}"\n')
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

    clear_orphans(start, failing)
    vol, d = root_sets(start)
    sets = list(exfatimg.sets(d))
    names = [s.name for s in sets]
    first = {s.name: struct.unpack_from("<I", s.data, 52)[0] for s in sets}
    ok, lines = fsck()
    if not ok:
        notes += [f"after: {ln}" for ln in lines]
    listing = (answer(out, "ls /") or "").split()
    said = {c: answer(out, c) for c in ("mv /S /T", "mv -f /S /s", "mv -f /S /U")}
    checks += [
        ("driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"mv /S2 /T2, S2's File entry cleared before X failed, kept T2: cat /T2 printed s2 "
         f"({answer(out, 'cat /T2')!r})", answer(out, "cat /T2") == "s2"),
        (f"mv /S /T, -f /S /s and -f /S /U each answered an error ({said})", all(said.values())),
        (f"ls / lists S and T2, and none of S2, T, s or U ({[n for n in listing if n in ('S', 'T2', 'S2', 'T', 's', 'U')]})",
         "S" in listing and "T2" in listing and not {"S2", "T", "s", "U"} & set(listing)),
        (f"cat /S printed src ({answer(out, 'cat /S')!r})", answer(out, "cat /S") == "src"),
        (f"on the disk one set S on its cluster and one T2 on S2's, none named T, s, S2 or U ({names})",
         names.count("S") == 1 and names.count("T2") == 1 and not {"T", "s", "S2", "U"} & set(names)
         and first.get("S") == clusters["S"] and first.get("T2") == clusters["S2"]),
        (f"S's cluster used, U's free ({vol.used(clusters['S'])}, {vol.used(clusters['U'])})",
         vol.used(clusters["S"]) and not vol.used(clusters["U"])),
        ("fsck_exfat -n passes after, S2's orphaned entries cleared", ok),
        (f"no fault lines in QEMU's trace ({faults})", faults == 0),
    ]
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

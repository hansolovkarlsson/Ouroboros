#!/usr/bin/env python3
"""A failed exFAT `mv` gives back the set it wrote, never two over one file.

    python3 scripts/test-mv-undo.py     (or `make test-mv-undo`, which builds the images)

A `mv` on exFAT writes the source's set again under the new name (`relink`)
and then deletes the old one; a replacing `mv` deletes the destination's
set too. Until 2026-10-26 a failure at a delete returned the error with
the new set left on the disk beside the source's, both naming the source's
clusters, so a later `rm` of either freed them under the other (the high
review of 2026-10-24). Now `move_set` clears the new set while the source's
File entry still reads in use; a source whose File entry was cleared before
the failure is gone, and the new set is kept as the file's. A replacing
`mv` deletes the source's set before the destination's (the high review of
#357: the other order, with the undo, destroyed the destination when the
source's delete failed), and when the destination's delete fails it frees
the destination's run if its File entry was cleared, or leaves it beside
the new set of its name, saying so.

One boot. The firmware boots build/esp.img from NVMe (which the kernel
cannot drive), and virtio holds a copy of build/espexfat.img through QEMU's
blkdebug with every write of two sectors of the exFAT root, X and Y (X + 2),
failing EIO (test-probe-ioerr's shape, for writes alone: `iotype =
"write"`, or a read of them fails too once any write has been made). The
host gives the root a second cluster and plants on it, after its last set: U, W and W2, each a file on
a cluster of its own; at least 16 deleted slots, where the boot's new sets
go; S2's File entry in the last slot before X, its Stream and name at X's
start; S whole in X; deleted slots to the last slot before Y, V's File
entry there and its Stream and name at Y's start. Before the boot each
reads back where it was meant to and fsck_exfat -n passes. Then, logged in:

- `mv /S2 /T2`: the delete clears S2's File entry and fails at X, so S2 is
  gone and T2, the set relink wrote, must stay: `cat /T2` prints `s2`;
- `mv /S /T`, `mv -f /S /s` (case-only) and `mv -f /S /U` (replacing) each
  answer an I/O error and leave S and U whole (`cat /U` prints `u`);
- `mv -f /W /V`: W's delete works, V's clears its File entry and fails at
  Y, so V must name W's bytes (`cat /V` prints `w`) and V's old cluster be
  freed;
- `mv -f /W2 /S`: W2's delete works, S's fails at X with S untouched, so
  two files are named S, on S's cluster and W2's, and fsd says
  `fsd: a failed mv -f left two files of one name`;
- `ls /` lists S, T2, U and V and none of S2, T, s, W or W2.

After the boot, on the host, with the orphaned Stream and name entries at
X and Y (S2's and V's, still in use) cleared first: two sets named S on S's
and W2's clusters, T2 on S2's, U on its own, V on W's, none named T, s, S2,
W or W2; V's old cluster free and every other planted one used; fsck_exfat
-n clean but for its one complaint, `Directory / contains a duplicate
name (S).` No fault lines;
a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100). Fails with
the undo removed (T and U left on S's cluster), with the undo made whatever
the source's File entry reads (T2 lost), with the destination deleted
before the source (U lost), and with the destination's run not freed when
its File entry was cleared (V's old cluster leaked). Not graded by a boot:
a give-back that fails, a source whose File entry does not read after the
failure, which both leave two sets with a line, and relink's own partial
write cleared (insert_set). About a minute. Run it whenever exFAT's mv,
move_set, relink, file_in_use, clear_slots, insert_set, unlink_set or
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
TWO = "fsd: a failed mv -f left two files of one name"
FAILS = ["mv /S /T", "mv -f /S /s", "mv -f /S /U", "mv -f /W /V", "mv -f /W2 /S"]
CMDS = ["mv /S2 /T2", "cat /T2", "mv /S /T", "mv -f /S /s", "mv -f /S /U", "cat /U",
        "mv -f /W /V", "cat /V", "cat /S", "mv -f /W2 /S", "ls /"]


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


def clear_orphans(failing):
    """The Stream and name entries left in use at X and Y (S2's and V's)
    when their sets' deletes failed there, cleared, so fsck reads the volume
    as fsd meant to leave it."""
    with open(IMAGE, "r+b") as fh:
        for sector in failing:
            fh.seek(sector * SECTOR)
            sec = bytearray(fh.read(SECTOR))
            for i in range(0, 2 * SLOT, SLOT):
                if sec[i] in (0xC0, 0xC1):
                    sec[i] &= 0x7F
            fh.seek(sector * SECTOR)
            fh.write(sec)


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

    clear_orphans(failing)
    vol, d = root_sets(start)
    sets = list(exfatimg.sets(d))
    names = [s.name for s in sets]
    firsts = sorted(struct.unpack_from("<I", s.data, 52)[0] for s in sets if s.name == "S")
    first = {s.name: struct.unpack_from("<I", s.data, 52)[0] for s in sets}
    rc, lines = fsck()
    # Two sets of one name, as the last mv leaves them, are fsck's one
    # complaint, and must be it.
    rest = [ln for ln in exfatimg.complaints(lines) if ln.strip() != "Directory / contains a duplicate name (S)."]
    ok = ("** Checking file system hierarchy." in lines and not rest and not exfatimg.unexpected(lines)
          and len(exfatimg.complaints(lines)) == 1)
    notes += [f"after: {ln}" for ln in lines]
    listing = (answer(out, "ls /") or "").split()
    said = {c: answer(out, c) for c in FAILS}
    used = {n: vol.used(c) for n, c in clusters.items()}
    checks += [
        ("driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"mv /S2 /T2, S2's File entry cleared before X failed, kept T2: cat /T2 printed s2 "
         f"({answer(out, 'cat /T2')!r})", answer(out, "cat /T2") == "s2"),
        (f"each failing mv answered an I/O error ({said})", all("I/O error" in (v or "") for v in said.values())),
        (f"mv -f /S /U failing at S's delete left U whole: cat /U printed u ({answer(out, 'cat /U')!r})",
         answer(out, "cat /U") == "u"),
        (f"mv -f /W /V failing at V's delete, its File entry cleared: cat /V printed w ({answer(out, 'cat /V')!r})",
         answer(out, "cat /V") == "w"),
        (f"cat /S printed src ({answer(out, 'cat /S')!r})", answer(out, "cat /S") == "src"),
        (f"mv -f /W2 /S failing at S's delete, S untouched: fsd said so ({TWO!r} in the transcript)", TWO in out),
        ("no line that a give-back failed", "left two entries on one file's clusters" not in out),
        (f"ls / lists S, T2, U and V, none of S2, T, s, W or W2 ({listing})",
         {"S", "T2", "U", "V"} <= set(listing) and not {"S2", "T", "s", "W", "W2"} & set(listing)),
        (f"on the disk two sets S on S's and W2's clusters, T2 on S2's, U on its own, V on W's, none named T, s, "
         f"S2, W or W2 ({names})",
         names.count("S") == 2 and firsts == sorted([clusters["S"], clusters["W2"]])
         and first.get("T2") == clusters["S2"] and first.get("U") == clusters["U"] and first.get("V") == clusters["W"]
         and names.count("T2") == names.count("U") == names.count("V") == 1
         and not {"T", "s", "S2", "W", "W2"} & set(names)),
        (f"V's old cluster free, every other planted one used ({used})",
         not used["V"] and all(v for n, v in used.items() if n != "V")),
        (f"fsck_exfat -n finds nothing to repair but the two sets of one name ({rest})", ok),
        (f"no fault lines in QEMU's trace ({faults})", faults == 0),
    ]
    return checks, notes if not checks[-2][1] else [], 0


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

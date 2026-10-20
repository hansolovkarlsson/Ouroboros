#!/usr/bin/env python3
"""A volume claiming more than its partition is not mounted.

    python3 scripts/test-volume-bound.py     (or `make test-volume-bound`, which builds the images)

fsd took a volume's size from its own boot sector alone: FAT32's total
sectors, exFAT's VolumeLength, ClusterCount and offsets, ext2's block count.
`mount_at` was given where its partition starts but not how long it is. Since
2026-10-19 FAT32 checks every cluster it takes to the disk against
`cluster_end`, and exFAT against its cluster count, so those bounds were the
only guard left, and they came from the boot sector: one claiming more than
its partition holds put the next partition's sectors inside the volume, and a
stale one left by a larger image (`erase disk` zeroes the first 1 MiB and
`partition` starts there) named a root past the end of the disk, whose read
failed, so since #329 `format` refused that disk with an I/O error. Since
2026-10-20 each `mount_at` is given the most the volume may span
(`vfs::volume_bound`: the table's length, or none when it gives 0, cut to the
start of the next partition, the disk and sector 2^32) and refuses a boot sector claiming
more, or one whose layout does not fit inside what it claims.

Thirteen boots. In each the firmware boots esp.img from NVMe, which the kernel
cannot drive, so the virtio disk the rig builds (scripts/mbrdisk.py, from the
images' own volumes) goes in as the fallback, and the boot partition is not
known to fsd. Where the disk's first partition is the ext2 root of
build/espext2.img, it mounts at boot and the rig logs in, then mounts each
other partition with `mount N /mnt/x`, which must answer that it holds no
filesystem, or for a volume that is whole, `ls` of it must list it.

- PARTS: the FAT32 of build/esp.img, the exFAT of build/espexfat.img and a
  second ext2, each cut short (half, 4,096 and 8,192 sectors), the next
  partition laid after it, inside the volume the boot sector claims. All
  three refused.
- MIXED: the FAT32 in a table entry of size 0, which must mount (bounded by
  the disk, as it mounted from its boot sector before), the exFAT whole,
  which must mount, and the FAT32 again at sector 2^32 - 65,536, past which
  every arm's 32-bit sectors would wrap, which must be refused. The disk is
  a sparse file of 2 TiB.
- STALE: a 16 MiB disk with one partition from 2,048 to its end, holding
  the exFAT boot sector of a 24 MiB volume with its root moved past the end
  of the disk. The boot line must say there is no mountable partition, not
  an I/O error; `format ext2` must format it and `mount -a` mount the ext2.
- LONG: the same disk with its table entry claiming the 24 MiB the volume
  does, past the end of the device. The same answers: the bound is cut to
  the disk, and `format` lays its volume inside the disk.
- WRAP: a copy of esp.img whose BPB has 2 FATs of 0x80000000 sectors, a
  product that wraps to 0 in 32 bits, under which the old check passed. Not
  mounted.
- SHAPES-A to SHAPES-G: whole volumes in partitions of their own
  size, each with one field of its boot sector changed so that it breaks
  exactly one of the rules the mounts check, which a model of those rules
  confirms on the host before the boot: exFAT's heap past its volume, its
  FatOffset inside the boot regions, its root past the heap (A); its FAT too
  short for its clusters, its cluster shift 64, FAT32's root at its
  cluster_end (B); FAT32 with no FATs, ext2's first data block 0 with 1 KiB
  blocks (the superblock's own is 1, so its descriptor table would be the
  superblock), ext2 with more groups than its blocks make (C); exFAT's FAT running
  into its heap (D); FAT32 with no reserved sectors (its FAT on its BPB),
  ext2's block-size shift 32 (masked to 0 in a release build, so only the
  shift rule refuses it), its descriptor table past its blocks (E); ext2
  with no inodes (F); ext2 with more blocks or inodes a group than one
  bitmap block holds (G). All refused.
- BESIDE: the FAT32 cut short in a table entry of size 0, the next
  partition inside its volume (refused: the bound stops at the next
  partition), and the exFAT whole with its bitmap moved to the cluster past
  its heap, the next partition's first sector: it mounts, a write to it must
  answer an I/O error, and the partition after it must be as it was, every
  byte (bitmap_sector_lba refuses a sector outside the bitmap's extent in
  the heap).

No fault line in QEMU's trace. Fails with the partition's length not passed
(PARTS's three mount, STALE and LONG answer an I/O error), with the length
not cut to the disk (LONG), not cut at the next partition (BESIDE), with a
size-0 entry refused (MIXED), without the 2^32 cut (MIXED's last mounts),
with FAT32's sum in u32 (WRAP), with each shape rule removed (its SHAPES
case mounts), and with the bitmap's sectors unbounded (BESIDE: the write
lands on the next partition). Not reached: FAT32's sums made before the
bound overflow only in a build with overflow checks, exFAT's sector-size
shift likewise, and ext2's first-block rule, which since the first-data rule
can break only for a volume of one block (it guards the subtraction the
groups rule makes). About eight minutes. Run it
whenever a filesystem's `mount_at`, fsd's probe or `volume_bound`, or the
partition table's sizes change.
"""
import hashlib
import importlib.util
import os
import re
import shutil
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BUILD = os.path.join(ROOT, "build")
sys.path.insert(0, HERE)
import mbrdisk  # noqa: E402
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

SECT = mbrdisk.SECT
ESP = os.path.join(BUILD, "esp.img")
EXFAT = os.path.join(BUILD, "espexfat.img")
EXT2 = os.path.join(BUILD, "espext2.img")
DISK = os.path.join(BUILD, "test-volume-bound.img")
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)
NO_FS = "mount: no such partition, no filesystem on it, or `/` not mounted yet (`mount -a` first)"
HIGH = (1 << 32) - 65536
# (start, sectors) of a partition the boot must leave as it was, and its
# digest before the boot; set by a build that has one (BESIDE).
WITNESS = {}


def volume(image, number):
    """The bytes of MBR partition `number` (1-based) of `image`."""
    with open(image, "rb") as fh:
        entry = [e for e in mbrdisk.entries(fh.read(SECT)) if e[0] == number][0]
        fh.seek(entry[2] * SECT)
        return fh.read(entry[3] * SECT)


# --- A model of the mounts' rules, to confirm each case breaks one ----------

def broken(vol, bound):
    """The rules of fsd's mount_at a volume breaks, given its bound."""
    u = lambda fmt, off: struct.unpack_from(fmt, vol, off)[0]  # noqa: E731
    if vol[82:90] == b"FAT32   ":
        spc, res, nf, fz, rc = vol[13], u("<H", 14), vol[16], u("<I", 36), u("<I", 44)
        total = u("<H", 19) or u("<I", 32)
        meta = res + nf * fz
        out = set()
        if nf == 0:
            return {"fat32-nofats"}
        if res == 0:
            return {"fat32-reserved"}
        if spc == 0 or total <= meta:
            return {"fat32-meta"}
        if total > bound:
            out.add("length")
        end = min((total - meta) // spc + 2, fz * 128, 0x0FFFFFF7)
        if not 2 <= rc < end:
            out.add("fat32-root")
        return out
    if vol[3:11] == b"EXFAT   ":
        vl, fo, fl, ho, cc, rc, sh = u("<Q", 72), u("<I", 80), u("<I", 84), u("<I", 88), u("<I", 92), u("<I", 96), vol[109]
        if sh > 16:
            return {"exfat-shift"}
        rules = {
            "length": vl > bound,
            "exfat-heap": ho + (cc << sh) > vl,
            "exfat-fatoffset": fo < 24,
            "exfat-fatplace": fo + fl > ho,
            "exfat-fatlength": fl * 128 < cc + 2,
            "exfat-root": not 2 <= rc < cc + 2,
        }
        return {k for k, v in rules.items() if v}
    assert u("<H", 1024 + 56) == 0xEF53, "no volume"
    ic, bc, fdb, lb, bpg, ipg = (u("<I", 1024 + o) for o in (0, 4, 20, 24, 32, 40))
    if lb > 2:
        return {"ext2-blocksize"}
    out = set()
    bs = 1024 << lb
    if bc * (bs // SECT) > bound:
        out.add("length")
    if bpg == 0 or bpg > bs * 8:
        out.add("ext2-bpg")
    if ipg > bs * 8:
        out.add("ext2-ipg")
    if fdb != 1024 // bs:
        out.add("ext2-firstdata")
    groups = -(-ic // ipg)
    if fdb >= bc:
        # Past the first-data rule only when the volume has one block: the
        # first-block rule guards fsd's subtraction below.
        return out | {"ext2-firstblock", "ext2-bgdt"}
    if ic < 2:
        out.add("ext2-inodes")
    if groups > -(-(bc - fdb) // bpg):
        out.add("ext2-groups")
    if fdb + 1 + -(-(groups * 32) // bs) > bc:
        out.add("ext2-bgdt")
    return out


def patched(vol, edits):
    """`vol` with each (struct format, offset, value) written into it; the
    ext2 superblock's offsets are given from its start, 1024 bytes in."""
    vol = bytearray(vol)
    for fmt, off, value in edits:
        struct.pack_into(fmt, vol, off, value)
    return bytes(vol)


def u32(vol, off):
    return struct.unpack_from("<I", vol, off)[0]


# --- The disks ---------------------------------------------------------------

def build_parts(ext2, fat, exfat):
    short = [(0x0B, fat[:len(fat) // 2], "the FAT32"), (0x07, exfat[:4096 * SECT], "the exFAT"),
             (0x83, ext2[:8192 * SECT], "the second ext2")]
    layout = mbrdisk.assemble(DISK, [(0x83, ext2, False)] + [(k, v, False) for k, v, _ in short])
    pre, cases = [], []
    for n, ((_, data, what), whole, (start, sectors)) in enumerate(zip(short, (fat, exfat, ext2), layout[1:]), 1):
        nxt = layout[n + 1][0] if n + 1 < len(layout) else None
        pre.append((f"{what}'s boot sector breaks only the length rule ({sectors} sectors)",
                    broken(whole, sectors) == {"length"}))
        if nxt:
            pre.append((f"the partition after {what} starts inside its volume", nxt < start + len(whole) // SECT))
        cases.append((n, what, False))
    return pre, cases


def build_mixed(ext2, fat, exfat):
    layout = mbrdisk.assemble(DISK, [(0x83, ext2, False), (0x0B, fat, False), (0x07, exfat, False)])
    mbrdisk.set_entry(DISK, 2, 0x0B, layout[1][0], 0)  # the FAT32's entry: no size
    mbrdisk.set_entry(DISK, 4, 0x0B, HIGH, len(fat) // SECT)
    with open(DISK, "r+b") as fh:
        fh.seek(HIGH * SECT)
        fh.write(fat)
    pre = [
        ("the size-0 FAT32 fits the disk after it (it mounted before)", broken(fat, 1 << 40) == set()),
        ("the exFAT is whole", broken(exfat, layout[2][1]) == set()),
        (f"the high FAT32 breaks only the length rule, cut at 2^32 ({(1 << 32) - HIGH} sectors)",
         broken(fat, (1 << 32) - HIGH) == {"length"}),
    ]
    return pre, [(1, "the FAT32 with no size", True), (2, "the whole exFAT", True), (3, "the FAT32 at 2^32", False)]


def bitmap_entry(vol):
    """The byte offset in `vol` of the exFAT root directory's bitmap entry."""
    ho, rc, sh = u32(vol, 88), u32(vol, 96), vol[109]
    root = (ho + ((rc - 2) << sh)) * SECT
    for off in range(root, root + (SECT << sh), 32):
        if vol[off] == 0x81:
            return off
    sys.exit("the exFAT root holds no bitmap entry")


def build_beside(ext2, fat, exfat):
    """The FAT32 cut short in an entry of size 0, the next partition inside
    it; the exFAT whole with its bitmap moved to the first cluster past its
    heap, the next partition's first sector, with an ext2 there as witness."""
    cc = u32(exfat, 92)
    bitmapped = bytearray(exfat)
    struct.pack_into("<I", bitmapped, bitmap_entry(exfat) + 20, cc + 2)
    layout = mbrdisk.assemble(DISK, [(0x83, ext2, False), (0x0B, fat[:len(fat) // 2], False),
                                     (0x07, bytes(bitmapped), False), (0x83, ext2, False)])
    mbrdisk.set_entry(DISK, 2, 0x0B, layout[1][0], 0)
    (fat_start, _), (ex_start, ex_sectors), (wit_start, wit_sectors) = layout[1:]
    WITNESS["range"] = (wit_start, wit_sectors)
    WITNESS["before"] = digest(wit_start, wit_sectors)
    pre = [
        ("the size-0 FAT32 breaks only the length rule, cut at the next partition",
         broken(fat, ex_start - fat_start) == {"length"}),
        ("the exFAT passes every mount rule", broken(bytes(bitmapped), ex_sectors) == set()),
        ("its bitmap's first sector is the next partition's first", ex_start + ex_sectors == wit_start),
    ]
    return pre, [(1, "the FAT32 with no size", False), (2, "the exFAT with its bitmap past its heap", "write")]


def digest(start, sectors):
    with open(DISK, "rb") as fh:
        fh.seek(start * SECT)
        return hashlib.sha256(fh.read(sectors * SECT)).hexdigest()


def stale_volume(exfat):
    """24 MiB exFAT boot sector, its root moved to the heap's last cluster."""
    vbr = bytearray(exfat[:SECT])
    count = u32(vbr, 92)
    struct.pack_into("<I", vbr, 96, count - 1)
    return bytes(vbr), 2048 + u32(vbr, 88) + ((count - 3) << vbr[109])


def build_stale(exfat, claim_long):
    vbr, root = stale_volume(exfat)
    disk = 16 * 2048
    (start, sectors), = mbrdisk.assemble(DISK, [(0x07, vbr + bytes((disk - 2048) * SECT - len(vbr)), False)])
    if claim_long:
        mbrdisk.set_entry(DISK, 1, 0x07, start, struct.unpack_from("<Q", vbr, 72)[0])
    claimed = struct.unpack_from("<Q", vbr, 72)[0]
    pre = [(f"the exFAT claims {claimed} sectors, the disk holds {disk - start} after 2048", claimed > disk - start),
           (f"its root, sector {root}, is past the disk's end ({disk})", root >= disk)]
    if claim_long:
        pre.append(("the table entry claims as much as the volume", True))
    return pre, []


def build_wrap(fat):
    vol = patched(fat, [("<B", 16, 2), ("<I", 36, 0x80000000)])
    mbrdisk.assemble(DISK, [(0x0B, vol, False)])
    return [("the FATs' size wraps to 0 in 32 bits", (2 * 0x80000000) & 0xFFFFFFFF == 0)], []


def build_shapes(ext2, fat, exfat, which):
    vl, fo, fl, ho, cc = (struct.unpack_from(f, exfat, o)[0] for f, o in
                          (("<Q", 72), ("<I", 80), ("<I", 84), ("<I", 88), ("<I", 92)))
    sh = exfat[109]
    total = struct.unpack_from("<H", fat, 19)[0] or u32(fat, 32)
    meta = struct.unpack_from("<H", fat, 14)[0] + fat[16] * u32(fat, 36)
    fat_end = min((total - meta) // fat[13] + 2, u32(fat, 36) * 128, 0x0FFFFFF7)
    sb = 1024
    ic, bc, fdb, bpg, ipg = (u32(ext2, sb + o) for o in (0, 4, 20, 32, 40))
    groups = -(-(bc - fdb) // bpg)
    cases = {
        "SHAPES-A": [
            ("exfat-heap", exfat, [("<I", 88, vl - (cc << sh) + 8)]),
            ("exfat-fatoffset", exfat, [("<I", 80, 8)]),
            ("exfat-root", exfat, [("<I", 96, cc + 2)]),
        ],
        "SHAPES-B": [
            ("exfat-fatlength", exfat, [("<I", 84, (cc + 2) // 128 - 1)]),
            ("exfat-shift", exfat, [("<B", 109, 64)]),
            ("fat32-root", fat, [("<I", 44, fat_end)]),
        ],
        "SHAPES-C": [
            ("fat32-nofats", fat, [("<B", 16, 0)]),
            ("ext2-firstdata", ext2, [("<I", sb + 20, 0)]),
            ("ext2-groups", ext2, [("<I", sb + 0, ipg * (groups + 1))]),
        ],
        "SHAPES-D": [
            ("exfat-fatplace", exfat, [("<I", 84, ho - fo + 1)]),
        ],
        "SHAPES-E": [
            ("fat32-reserved", fat, [("<H", 14, 0)]),
            ("ext2-blocksize", ext2, [("<I", sb + 24, 32)]),  # masked to 0 in release: only the shift rule refuses it
            ("ext2-bgdt", ext2, [("<I", sb + 20, bc - 1), ("<I", sb + 0, ipg)]),
        ],
        "SHAPES-F": [
            ("ext2-inodes", ext2, [("<I", sb + 0, 0)]),
        ],
        "SHAPES-G": [
            ("ext2-bpg", ext2, [("<I", sb + 32, 1024 * 8 + 8)]),
            ("ext2-ipg", ext2, [("<I", sb + 40, 1024 * 8 + 8)]),
        ],
    }[which]
    vols = [(rule, patched(base, edits)) for rule, base, edits in cases]
    parts = [(0x83, ext2, False)] + [
        (0x0B if v[82:90] == b"FAT32   " else 0x07 if v[3:11] == b"EXFAT   " else 0x83, v, False) for _, v in vols]
    layout = mbrdisk.assemble(DISK, parts)
    pre = []
    for (rule, v), (_, sectors) in zip(vols, layout[1:]):
        got = broken(v, sectors)
        want = {rule}
        pre.append((f"the {rule} case breaks only that rule (got {sorted(got)})", got == want))
    return pre, [(n, f"the {rule} case", False) for n, (rule, _) in enumerate(vols, 1)]


BOOTS = ["PARTS", "MIXED", "STALE", "LONG", "WRAP", "SHAPES-A", "SHAPES-B", "SHAPES-C", "SHAPES-D", "SHAPES-E",
         "SHAPES-F", "SHAPES-G", "BESIDE"]


def build(name):
    ext2, fat, exfat = volume(EXT2, 1), volume(ESP, 1), volume(EXFAT, 1)
    if name == "PARTS":
        return build_parts(ext2, fat, exfat)
    if name == "MIXED":
        return build_mixed(ext2, fat, exfat)
    if name in ("STALE", "LONG"):
        return build_stale(exfat, name == "LONG")
    if name == "WRAP":
        return build_wrap(fat)
    if name == "BESIDE":
        return build_beside(ext2, fat, exfat)
    return build_shapes(ext2, fat, exfat, name)


def commands(name, cases):
    if name in ("STALE", "LONG"):
        return ["format ext2", "mount -a", "mount"]
    if name == "WRAP":
        return ["mount"]
    out = []
    for n, _, whole in cases:
        out.append(f"mount {n} /mnt/p{n}")
        if whole == "write":
            out.append(f"echo bitmap > /mnt/p{n}/NEW.TXT")
        elif whole:
            out.append(f"ls /mnt/p{n}")
    return out + ["ls /"]


def boot(name, cmds):
    steps = [("# ", cmd) for cmd in cmds] + [("# ", "")]
    if name not in ("STALE", "LONG", "WRAP"):
        # The ext2 root mounts, with accounts on it: login as root first.
        steps = [("login:", "root"), ("assword", "root")] + steps
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, which it boots and
        # the kernel cannot drive (test-boot-partition's fallback case).
        "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
        "-drive", f"file={DISK},format=raw,if=none,id=hd0",
        "-device", "virtio-blk-device,drive=hd0",
    ]
    guest = drive_qemu.Guest(DISK, extra_args=extra, virtio_disk=False, stamp_from=ESP,
                             label=f"test-volume-bound[{name}]: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
        guest.stop()
        if "range" in WITNESS:
            WITNESS["after"] = digest(*WITNESS["range"])
    finally:
        guest.stop()
        if os.path.exists(DISK):
            os.remove(DISK)
    with open(os.path.join(BUILD, f"test-volume-bound-{name.lower()}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def outputs(out, cmds):
    """[output of each command], in order: the lines after its prompt line
    (`# cmd`) up to the next prompt, the kernel's exit lines dropped. netd's
    boot lines can land after a prompt, so a command may be echoed on a line
    of its own."""
    got = []
    at = 0
    for cmd in cmds:
        m = re.compile(rf"^(?:# )?{re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            got.append(None)
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        block = out[m.end():nxt.start() if nxt else len(out)]
        at = nxt.start() if nxt else len(out)
        got.append(EXIT.sub("", block).strip())
    return got


def checks_of(name):
    pre, cases = build(name)
    if not all(ok for _, ok in pre):
        # The case was not built: booting it would prove nothing.
        return [(f"{name}: {label}", ok) for label, ok in pre]
    cmds = commands(name, cases)
    driven, out, faults = boot(name, cmds)
    o = dict(zip(cmds, outputs(out, cmds)))
    boot_log = out.split(f"# {cmds[0]}")[0]
    checks = pre + [("driven to the end, every command's output found", driven and None not in o.values())]
    if name in ("STALE", "LONG"):
        checks += [
            ("fsd's boot line: no mountable partition, not an I/O error",
             "fsd: mount failed (no mountable partition on the disk) - disk commands won't work\r\n" in boot_log),
            (f"format ext2 formats it (got {o['format ext2']!r})", o["format ext2"] == "formatted - run `mount -a`, then `ls`"),
            (f"mount -a mounts the new ext2 (got {o['mount -a']!r})",
             (o["mount -a"] or "").endswith("mounted - disk commands available") and "fsd: ext2 mounted" in (o["mount -a"] or "")),
            (f"mount names ext2 at LBA 2048 (got {o['mount']!r})", (o["mount"] or "").startswith("ext2 mounted at partition LBA 2048 ")),
        ]
    elif name == "WRAP":
        checks += [
            ("fsd's boot line: no mountable partition",
             "fsd: mount failed (no mountable partition on the disk) - disk commands won't work\r\n" in boot_log),
            (f"mount: nothing mounted (got {o['mount']!r})", (o["mount"] or "").startswith("nothing mounted")),
        ]
    else:
        checks.append(("the ext2 root mounted at boot", "fsd: ext2 mounted, disk commands available" in boot_log))
        for n, what, whole in cases:
            got = o[f"mount {n} /mnt/p{n}"]
            if whole == "write":
                wrote = o[f"echo bitmap > /mnt/p{n}/NEW.TXT"] or ""
                checks += [
                    (f"mount {n}, {what}, mounts (got {got!r})", got == ""),
                    (f"a write to it is refused (got {wrote!r})", "error" in wrote),
                    ("the partition after it as it was, every byte",
                     WITNESS.get("after") is not None and WITNESS.get("after") == WITNESS.get("before")),
                ]
            elif whole:
                # Both whole volumes carry an /etc of their own.
                listing = [w.lower() for w in (o[f"ls /mnt/p{n}"] or "").split()]
                checks.append((f"mount {n}, {what}, mounts and lists (got {got!r}, ls {listing[:4]})",
                               got == "" and "etc/" in listing))
            else:
                checks.append((f"mount {n}, {what}, refused (got {got!r})", got == NO_FS))
        checks.append(("ls / still lists the root", "etc/" in (o["ls /"] or "").split()))
    checks.append(("no fault lines", faults == 0))
    return [(f"{name}: {label}", ok) for label, ok in checks]


def main():
    for f in (ESP, EXFAT, EXT2):
        if not os.path.exists(f):
            sys.exit(f"{f} not found - run `make test-volume-bound`, which builds it")
    only = sys.argv[1:] or BOOTS
    checks = []
    for name in only:
        checks += checks_of(name)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    print(f"test-volume-bound: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

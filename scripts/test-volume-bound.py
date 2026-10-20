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
2026-10-20 each `mount_at` is given the partition's length and refuses a
volume larger than it, as "not this filesystem".

Three boots. In each the firmware boots esp.img from NVMe, which the kernel
cannot drive, so the virtio disk the rig builds goes in as the fallback, and
the boot partition is not known to fsd.

- PARTS: one disk, built from the images' own volumes: the ext2 root of
  build/espext2.img (type 0x83, at its full size), then the FAT32 of
  build/esp.img in a partition half its size, the exFAT of
  build/espexfat.img in one of 4,096 sectors and a second copy of the ext2
  in one of 8,192, each next partition starting inside the volume before it,
  so the volume as its boot sector claims it runs into the next partition.
  As root, after the ext2 root mounts: `mount 1 /mnt/a`, `mount 2 /mnt/b`,
  `mount 3 /mnt/c` must each answer that the partition holds no filesystem,
  and `mount 0 /mnt/d` (the root, already mounted) must not.
- STALE: a 16 MiB disk with one partition from 2,048 to its end, holding
  the exFAT boot sector of build/espexfat.img (a 24 MiB volume) with its
  root cluster moved past the end of the disk. fsd's boot line must say
  there is no mountable partition, not an I/O error; `format ext2` must
  format it and `mount -a` must mount the new ext2.
- WRAP: a copy of esp.img whose BPB has 2 FATs of 0x80000000 sectors, a
  product that wraps to 0 in 32 bits, under which the old check passed. fsd's
  boot line must say there is no mountable partition.

Before each boot the rig checks, on the host, that each volume's boot sector
does claim more than its partition (or wraps), so the case was built. No
fault line in QEMU's trace. Fails with the partition's length ignored (PARTS:
all three mount; STALE: the boot line and `format` answer an I/O error) and
with FAT32's sum in u32 (WRAP: the FAT32 mounts). About two minutes. Run it
whenever a filesystem's `mount_at`, fsd's probe, or the partition table's
sizes change.
"""
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
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

ESP = os.path.join(BUILD, "esp.img")
EXFAT = os.path.join(BUILD, "espexfat.img")
EXT2 = os.path.join(BUILD, "espext2.img")
DISK = os.path.join(BUILD, "test-volume-bound.img")
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)
NO_FS = "mount: no such partition, no filesystem on it, or `/` not mounted yet (`mount -a` first)"

BOOTS = {
    "PARTS": ["mount 1 /mnt/a", "mount 2 /mnt/b", "mount 3 /mnt/c", "mount 0 /mnt/d"],
    "STALE": ["format ext2", "mount -a", "mount"],
    "WRAP": ["mount"],
}


def mbr_entries(image):
    """[(type, start, sectors)] of the MBR's used entries."""
    with open(image, "rb") as fh:
        mbr = fh.read(512)
    out = []
    for i in range(4):
        e = mbr[0x1BE + 16 * i:0x1CE + 16 * i]
        if e[4]:
            out.append((e[4], *struct.unpack_from("<II", e, 8)))
    return out


def volume(image, index):
    """The bytes of partition `index` of `image`, the whole volume."""
    _, start, sectors = mbr_entries(image)[index]
    with open(image, "rb") as fh:
        fh.seek(start * 512)
        return fh.read(sectors * 512)


def write_mbr(fh, entries):
    """An MBR with `entries`, [(type, start, sectors)], and the boot mark."""
    mbr = bytearray(512)
    for i, (kind, start, sectors) in enumerate(entries):
        e = 0x1BE + 16 * i
        mbr[e + 1:e + 4] = b"\xfe\xff\xff"
        mbr[e + 4] = kind
        mbr[e + 5:e + 8] = b"\xfe\xff\xff"
        mbr[e + 8:e + 16] = struct.pack("<II", start, sectors)
    mbr[510:512] = b"\x55\xaa"
    fh.seek(0)
    fh.write(mbr)


def claims(vol):
    """The sectors a volume's boot sector says it spans (FAT32, exFAT or ext2)."""
    if vol[82:90] == b"FAT32   ":
        t16, = struct.unpack_from("<H", vol, 19)
        return t16 or struct.unpack_from("<I", vol, 32)[0]
    if vol[3:11] == b"EXFAT   ":
        return struct.unpack_from("<Q", vol, 72)[0]
    assert struct.unpack_from("<H", vol, 1024 + 56)[0] == 0xEF53, "no volume"
    blocks, = struct.unpack_from("<I", vol, 1024 + 4)
    return blocks * ((1024 << struct.unpack_from("<I", vol, 1024 + 24)[0]) // 512)


def build_parts():
    """PARTS's disk; returns [(what, claimed, partition sectors)] for the
    three short partitions."""
    ext2, fat, exfat = volume(EXT2, 0), volume(ESP, 0), volume(EXFAT, 0)
    p0 = (0x83, 2048, len(ext2) // 512)
    p1 = (0x0B, p0[1] + p0[2], 65536)
    p2 = (0x07, p1[1] + p1[2], 4096)
    p3 = (0x83, p2[1] + p2[2], 8192)
    with open(DISK, "wb") as fh:
        # Each volume whole, later ones over the tails of earlier ones, so
        # every short partition is followed by the next inside its volume.
        for (_, start, _), vol in ((p0, ext2), (p1, fat), (p2, exfat), (p3, ext2)):
            fh.seek(start * 512)
            fh.write(vol)
        write_mbr(fh, [p0, p1, p2, p3])
    return [("FAT32", claims(fat), p1[2]), ("exFAT", claims(exfat), p2[2]), ("ext2", claims(ext2), p3[2])]


def build_stale():
    """STALE's disk; returns (claimed, partition sectors, root's sector, disk sectors)."""
    vbr = bytearray(volume(EXFAT, 0)[:512])
    sectors = 16 * 2048
    part = (0x07, 2048, sectors - 2048)
    heap, = struct.unpack_from("<I", vbr, 88)
    count, = struct.unpack_from("<I", vbr, 92)
    shift = vbr[109]
    root = count - 1  # the heap's last cluster, past the end of this disk
    struct.pack_into("<I", vbr, 96, root)
    with open(DISK, "wb") as fh:
        fh.truncate(sectors * 512)
        fh.seek(2048 * 512)
        fh.write(vbr)
        write_mbr(fh, [part])
    return claims(vbr), part[2], 2048 + heap + ((root - 2) << shift), sectors


def build_wrap():
    """WRAP's disk; returns the FATs' size as the old check computed it."""
    shutil.copyfile(ESP, DISK)
    _, start, _ = mbr_entries(DISK)[0]
    with open(DISK, "r+b") as fh:
        fh.seek(start * 512)
        bpb = bytearray(fh.read(512))
        bpb[16] = 2
        struct.pack_into("<I", bpb, 36, 0x80000000)
        fh.seek(start * 512)
        fh.write(bpb)
    return (2 * 0x80000000) & 0xFFFFFFFF


def boot(name, commands):
    steps = [("# ", cmd) for cmd in commands] + [("# ", "")]
    if name == "PARTS":
        # The ext2 root mounts, with accounts on it: login as root first.
        steps = [("login:", "root"), ("assword", "root")] + steps
    subprocess.run([sys.executable, os.path.join(HERE, "disksig.py"), "set", DISK], check=True,
                   stdout=subprocess.DEVNULL)
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
    finally:
        guest.stop()
        if os.path.exists(DISK):
            os.remove(DISK)
    with open(os.path.join(BUILD, f"test-volume-bound-{name.lower()}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def outputs(out, commands):
    """[output of each command], in order: the lines after its prompt line
    (`# cmd`) up to the next prompt, the kernel's exit lines dropped. netd's
    boot lines can land after a prompt, so a command may be echoed on a line
    of its own."""
    got = []
    at = 0
    for cmd in commands:
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
    commands = BOOTS[name]
    if name == "PARTS":
        built = build_parts()
        pre = [(f"{what}'s boot sector claims {c} sectors, its partition {n}", c > n) for what, c, n in built]
    elif name == "STALE":
        c, n, root, disk = build_stale()
        pre = [(f"the exFAT claims {c} sectors, its partition {n}", c > n),
               (f"its root, sector {root}, is past the disk's end ({disk})", root >= disk)]
    else:
        wrapped = build_wrap()
        pre = [(f"the FATs' size wraps to {wrapped} in 32 bits", wrapped == 0)]
    driven, out, faults = boot(name, commands)
    o = outputs(out, commands)
    boot_log = out.split(f"# {commands[0]}")[0]
    none_at_boot = "fsd: mount failed (no mountable partition on the disk) - disk commands won't work\r\n" in boot_log
    checks = pre + [("driven to the end, every command's output found", driven and None not in o)]
    if name == "PARTS":
        checks += [
            ("the ext2 root mounted at boot", "fsd: ext2 mounted, disk commands available" in boot_log),
            *[(f"mount {i} /mnt/{d} refused, {what} (got {o[i - 1]!r})", o[i - 1] == NO_FS)
              for i, d, what in ((1, "a", "the FAT32"), (2, "b", "the exFAT"), (3, "c", "the second ext2"))],
            (f"mount 0 /mnt/d, the root, not refused (got {o[3]!r})", o[3] is not None and o[3] != NO_FS),
        ]
    if name == "STALE":
        checks += [
            ("fsd's boot line: no mountable partition, not an I/O error", none_at_boot),
            (f"format ext2 formats it (got {o[0]!r})", o[0] == "formatted - run `mount -a`, then `ls`"),
            (f"mount -a mounts the new ext2 (got {o[1]!r})",
             (o[1] or "").endswith("mounted - disk commands available") and "fsd: ext2 mounted" in (o[1] or "")),
            (f"mount names ext2 at LBA 2048 (got {o[2]!r})", (o[2] or "").startswith("ext2 mounted at partition LBA 2048 ")),
        ]
    if name == "WRAP":
        checks += [
            ("fsd's boot line: no mountable partition", none_at_boot),
            (f"mount: nothing mounted (got {o[0]!r})", (o[0] or "").startswith("nothing mounted")),
        ]
    checks.append(("no fault lines", faults == 0))
    return [(f"{name}: {label}", ok) for label, ok in checks]


def main():
    for f in (ESP, EXFAT, EXT2):
        if not os.path.exists(f):
            sys.exit(f"{f} not found - run `make test-volume-bound`, which builds it")
    checks = []
    for name in BOOTS:
        checks += checks_of(name)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    print(f"test-volume-bound: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

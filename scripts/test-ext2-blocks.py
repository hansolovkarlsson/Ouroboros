#!/usr/bin/env python3
"""ext2 never reaches past its volume, by a block number read off the disk.

    python3 scripts/test-ext2-blocks.py     (or `make test-ext2-blocks`, which builds the images)

Until 2026-10-20 ext2 took any block number it read to the disk: an inode's
pointer, an indirect block's, a descriptor's bitmap. A damaged one sent a
read or a write past the volume, into the next partition or past the disk.
Since #330 the volume itself is bounded by its partition at mount, and now
every sector the arm reads or writes goes through one `Volume`, which refuses
a sector past `s_blocks_count` blocks as an I/O error; an inode number past
`s_inodes_count` is refused the same way, and the allocator takes no bit of
the last group's bitmaps past the volume's last block or inode.

The firmware boots esp.img from NVMe, so the virtio disk the rig builds goes
in as the fallback: build/espext2.img's ext2 root as partition 0 (it mounts
at boot), three 4 MiB ext2 volumes made by mke2fs with 1 KiB blocks, A, C and
B, as partitions 1 to 3, and after B, unpartitioned, a WITNESS of 0xA5 bytes
with two zeroed blocks, which nothing may change. The host plants:

- on A, RD (two blocks), its second pointer into the witness: `cat` must
  stop at an I/O error, not read the witness's bytes; WR (1,100 bytes), its
  second pointer into the witness: an append must be refused and write
  nothing; IND (twelve blocks), its indirect pointer at a zeroed witness
  block: an append must be refused, where it wrote the new block's pointer
  there; GHOST, its directory entry naming inode `inodes_per_group` past
  SRC's, in a second group whose descriptor slot holds a copy of the first
  group's: `cat` must answer an I/O error, not SRC's bytes; SRC itself must
  still read whole (100 bytes); and `rm` of RD must remove it and free only
  its own block, not the witness block's bit in another group's bitmap;
- on C, group 0's block bitmap pointed at the other zeroed witness block: a
  new file's write must be refused, where it set a bit there;
- on B, every block of the volume marked used and the last group's padding
  bits clear, so the next free bit is the block just past the volume (the
  witness's first): `echo x > NEW` must answer disk full; and the inode count
  cut to one more than the inodes in use, so after NEW takes it the next free
  bit is past the count: `touch NEW2` must answer disk full and leave the bit
  clear.

Before the boot the host reads back every plant and checks it lands where
this says (the observer's own checks). Fails with Volume's bound taken out
(RD reads 2,048 bytes, WR's and IND's appends and C's bitmap write the
witness), with the inode-number
bound out (GHOST reads SRC), with the allocator's block cap out (B's write an
I/O error, not disk full: Volume refuses the block the allocator handed
out), with its inode cap out (NEW2 an I/O error, its bit set past the count)
and with free_block freeing a block past the volume (a bit of A's bitmap
cleared, through the second group's descriptor). One boot, about a minute, needs
e2fsprogs. Run it whenever ext2's Volume, block_sector, inode_place,
alloc_block, alloc_inode, free_block or ensure_block changes.
"""
import importlib.util
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BUILD = os.path.join(ROOT, "build")
sys.path.insert(0, HERE)
import bootstick  # noqa: E402
import mbrdisk  # noqa: E402

_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

SECT = mbrdisk.SECT
ESP = os.path.join(BUILD, "esp.img")
EXT2 = os.path.join(BUILD, "espext2.img")
DISK = os.path.join(BUILD, "test-ext2-blocks.img")
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)
WITNESS_BYTES = 1024 * 1024
# 1 KiB blocks into the witness each plant names, apart so a changed sector
# names the case that wrote it. B's overflow lands at block 0.
AT_RD, AT_WR, AT_IND, AT_BMP, AT_RMI, AT_OWI = 8, 16, 24, 32, 40, 48
ZEROED = (AT_IND, AT_BMP)
FILES_A = {"RD": b"R" * 2048, "WR": b"W" * 1100, "IND": b"I" * 12 * 1024, "SRC": b"S" * 100,
           "GHOST": b"G" * 10, "RMI": b"M" * 13 * 1024, "OWI": b"O" * 13 * 1024}
COMMANDS = [
    "mount 1 /mnt/a", "mount 2 /mnt/c", "mount 3 /mnt/b",
    "ls /mnt/a",
    "cat /mnt/a/SRC | wc",
    "cat /mnt/a/RD | wc",
    "echo tail >> /mnt/a/WR",
    "echo tail >> /mnt/a/IND",
    "cat /mnt/a/GHOST | wc",
    "rm /mnt/a/RD",
    "rm /mnt/a/RMI",
    "echo x > /mnt/a/OWI",
    "cat /mnt/a/OWI",
    "echo x > /mnt/c/NEW",
    "echo x > /mnt/b/NEW",
    "touch /mnt/b/NEW2",
]


class Ext2:
    """The few ext2 structures the rig plants and reads back, in the volume
    at sector `start` of the open disk image `fh`."""

    def __init__(self, fh, start):
        self.fh, self.base = fh, start * SECT
        sb = self.read(1024, 1024)
        u = lambda o: struct.unpack_from("<I", sb, o)[0]  # noqa: E731
        self.ic, self.bc, self.fdb, lb, self.bpg, self.ipg = (u(o) for o in (0, 4, 20, 24, 32, 40))
        self.bs = 1024 << lb
        self.isz = struct.unpack_from("<H", sb, 88)[0] if u(76) >= 1 else 128

    def read(self, off, n):
        self.fh.seek(self.base + off)
        return self.fh.read(n)

    def write(self, off, data):
        self.fh.seek(self.base + off)
        self.fh.write(data)

    def u32(self, off):
        return struct.unpack("<I", self.read(off, 4))[0]

    def set_u32(self, off, v):
        self.write(off, struct.pack("<I", v))

    def desc(self, g):
        return (self.fdb + 1) * self.bs + g * 32

    def inode_off(self, ino):
        g, i = divmod(ino - 1, self.ipg)
        return self.u32(self.desc(g) + 8) * self.bs + i * self.isz

    def ptr(self, ino, n):
        return self.u32(self.inode_off(ino) + 40 + 4 * n)

    def set_ptr(self, ino, n, v):
        self.set_u32(self.inode_off(ino) + 40 + 4 * n, v)

    def size(self, ino):
        return self.u32(self.inode_off(ino) + 4)

    def entries(self):
        """{name: (inode, byte offset of the entry)} of the root directory."""
        out = {}
        for n in range(-(-self.size(2) // self.bs)):
            blk = self.ptr(2, n)
            data = self.read(blk * self.bs, self.bs)
            at = 0
            while at < self.bs:
                ino, rec, nlen = struct.unpack_from("<IHB", data, at)
                if rec < 8:
                    break
                if ino:
                    out[data[at + 8:at + 8 + nlen].decode()] = (ino, blk * self.bs + at)
                at += rec
        return out

    def bitmap(self, which):
        """Group 0's block (0) or inode (4) bitmap block."""
        return self.read(self.u32(self.desc(0) + which) * self.bs, self.bs)

    def set_bitmap(self, which, data):
        self.write(self.u32(self.desc(0) + which) * self.bs, data)


def bit(bm, n):
    return bm[n // 8] >> (n % 8) & 1


def with_bits(bm, lo, hi, value):
    bm = bytearray(bm)
    for n in range(lo, hi):
        bm[n // 8] = bm[n // 8] | 1 << (n % 8) if value else bm[n // 8] & ~(1 << (n % 8))
    return bytes(bm)


def mkvol(files):
    """A 4 MiB ext2 with 1 KiB blocks holding `files`, made by mke2fs -d."""
    mke2fs = bootstick.tool("mke2fs")
    with tempfile.TemporaryDirectory(dir=BUILD) as tmp:
        src = os.path.join(tmp, "src")
        os.mkdir(src)
        for name, data in files.items():
            with open(os.path.join(src, name), "wb") as fh:
                fh.write(data)
        img = os.path.join(tmp, "vol.img")
        made = subprocess.run([mke2fs, "-q", "-t", "ext2", "-b", "1024", "-d", src, "-F", img, "4m"],
                              capture_output=True, text=True)
        if made.returncode:
            raise SystemExit(f"mke2fs failed: {made.stderr.strip()}")
        return open(img, "rb").read()


def witness_bytes():
    w = bytearray(b"\xa5" * WITNESS_BYTES)
    for blk in ZEROED:
        w[blk * 1024:(blk + 1) * 1024] = bytes(1024)
    return bytes(w)


def build():
    """Write the disk and plant the damage; return the plan and the checks."""
    root = volume(EXT2, 1)
    a, c, b = mkvol(FILES_A), mkvol({}), mkvol({})
    layout = mbrdisk.assemble(DISK, [(mbrdisk.EXT2_TYPE, v, False) for v in (root, a, c, b)])
    (_, _), (a_start, _), (c_start, _), (b_start, b_sectors) = layout
    wit_start = os.path.getsize(DISK) // SECT
    witness = witness_bytes()
    with open(DISK, "ab") as fh:
        fh.write(witness)
    wit_end = wit_start + len(witness) // SECT
    plan = {"a": a_start, "b": b_start, "c": c_start, "witness": (wit_start, len(witness) // SECT)}
    checks = []
    with open(DISK, "r+b") as fh:
        va, vb, vc = Ext2(fh, a_start), Ext2(fh, b_start), Ext2(fh, c_start)

        def wblock(v, start, k):  # the block number of v naming witness block k
            return ((wit_start - start) * SECT + k * 1024) // v.bs

        def in_wit(v, start, blk, k):
            at = start + blk * v.bs // SECT
            return at == wit_start + k * 2 and blk >= v.bc and at < wit_end

        ents = va.entries()
        checks.append(("plant: A has 1 KiB blocks and its seven files", va.bs == 1024 and set(FILES_A) <= set(ents)))
        if not checks[-1][1]:
            return plan, checks
        ino = {n: ents[n][0] for n in FILES_A}
        # A: RD's and WR's second pointers, IND's indirect pointer.
        va.set_ptr(ino["RD"], 1, wblock(va, a_start, AT_RD))
        va.set_ptr(ino["WR"], 1, wblock(va, a_start, AT_WR))
        indirect_before = va.ptr(ino["IND"], 12)
        va.set_ptr(ino["IND"], 12, wblock(va, a_start, AT_IND))
        # A: RMI's and OWI's own indirect blocks swapped for witness blocks.
        own_indirect = [va.ptr(ino[n], 12) for n in ("RMI", "OWI")]
        va.set_ptr(ino["RMI"], 12, wblock(va, a_start, AT_RMI))
        va.set_ptr(ino["OWI"], 12, wblock(va, a_start, AT_OWI))
        # A: GHOST names SRC's inode one group on, whose descriptor is group 0's.
        ghost = va.ipg + ino["SRC"]
        va.set_u32(ents["GHOST"][1], ghost)
        va.write(va.desc(1), va.read(va.desc(0), 32))
        plan.update(ino=ino, rd_block0=va.ptr(ino["RD"], 0), a_bitmap=va.bitmap(0),
                    direct={n: [va.ptr(ino[n], k) for k in range(12)] for n in ("RMI", "OWI")})
        # C: its block bitmap in the witness.
        vc.set_u32(vc.desc(0), wblock(vc, c_start, AT_BMP))
        # B: every block used, the padding past the volume clear; the inode
        # count one past the inodes in use.
        real = vb.bc - vb.fdb
        vb.set_bitmap(0, with_bits(with_bits(vb.bitmap(0), 0, real, 1), real, vb.bpg, 0))
        ibm = vb.bitmap(4)
        used = next(n for n in range(vb.ipg) if not bit(ibm, n))
        vb.set_u32(1024 + 0, used + 1)
        plan.update(b_inodes_used=used)
        vb = Ext2(fh, b_start)
        bbm, ibm = vb.bitmap(0), vb.bitmap(4)
        checks += [
            ("plant: RD's and WR's second pointers name witness blocks past A",
             in_wit(va, a_start, va.ptr(ino["RD"], 1), AT_RD) and in_wit(va, a_start, va.ptr(ino["WR"], 1), AT_WR)),
            ("plant: IND has twelve blocks and no indirect block of its own; its pointer names a zeroed witness block",
             va.size(ino["IND"]) == 12 * 1024 and indirect_before == 0
             and in_wit(va, a_start, va.ptr(ino["IND"], 12), AT_IND)),
            ("plant: RMI and OWI had indirect blocks of their own, now witness blocks",
             all(own_indirect) and in_wit(va, a_start, va.ptr(ino["RMI"], 12), AT_RMI)
             and in_wit(va, a_start, va.ptr(ino["OWI"], 12), AT_OWI)),
            # Without free_block's bound RD's witness block frees through the
            # group past A's one, whose descriptor slot holds group 0's: the
            # bitmap check can see it only while that group is 1.
            ("plant: RD's witness block lies in group 1, whose descriptor slot copies group 0's",
             (va.ptr(ino["RD"], 1) - va.fdb) // va.bpg == 1),
            ("plant: GHOST names an inode past A's count, whose group's descriptor names SRC's table",
             va.entries()["GHOST"][0] == ghost > va.ic and va.read(va.desc(1), 32) == va.read(va.desc(0), 32)
             and va.ic == va.ipg),
            ("plant: C's block bitmap names a zeroed witness block",
             in_wit(vc, c_start, vc.u32(vc.desc(0)), AT_BMP)),
            ("plant: B ends where the witness starts, its next free block bit the block past it",
             b_start + b_sectors == wit_start and vb.bc * vb.bs // SECT == b_sectors
             and all(bit(bbm, n) for n in range(real)) and not any(bit(bbm, n) for n in range(real, vb.bpg))),
            (f"plant: B's inode count {vb.ic} one past the {used} in use, the bits after them clear",
             vb.ic == used + 1 and all(bit(ibm, n) for n in range(used))
             and not any(bit(ibm, n) for n in range(used, used + 8))),
        ]
    return plan, checks


def volume(image, number):
    """The bytes of MBR partition `number` (1-based) of `image`."""
    with open(image, "rb") as fh:
        entry = [e for e in mbrdisk.entries(fh.read(SECT)) if e[0] == number][0]
        fh.seek(entry[2] * SECT)
        return fh.read(entry[3] * SECT)


def boot():
    steps = [("login:", "root"), ("assword", "root")] + [("# ", cmd) for cmd in COMMANDS] + [("# ", "")]
    extra = [
        # bootindex=0 pins the firmware to the NVMe disk, which it boots and
        # the kernel cannot drive (test-boot-partition's fallback case).
        "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
        "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
        "-drive", f"file={DISK},format=raw,if=none,id=hd0",
        "-device", "virtio-blk-device,drive=hd0",
    ]
    guest = drive_qemu.Guest(DISK, extra_args=extra, virtio_disk=False, stamp_from=ESP,
                             label="test-ext2-blocks: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(BUILD, "test-ext2-blocks.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def outputs(out):
    """{command: its output}: the lines after its prompt line up to the next
    prompt, the kernel's exit lines dropped."""
    got, at = {}, 0
    for cmd in COMMANDS:
        m = re.compile(rf"^(?:# )?{re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        at = nxt.start() if nxt else len(out)
        got[cmd] = EXIT.sub("", out[m.end():at]).strip()
    return got


def wc_bytes(block):
    # The shell prefixes `pipe: ` to the last stage's output when an earlier
    # stage failed.
    m = re.findall(r"^(?:pipe: )?\s*(\d+)\s+(\d+)\s+(\d+)\s*$", block or "", re.M)
    return int(m[-1][2]) if m else None


def verdicts(plan, driven, out, faults):
    o = outputs(out)
    io = "device I/O error"
    with open(DISK, "rb") as fh:
        wit_start, wit_sectors = plan["witness"]
        fh.seek(wit_start * SECT)
        witness = fh.read(wit_sectors * SECT)
        expect = witness_bytes()
        changed = [i // SECT for i in range(0, len(witness), SECT)
                   if witness[i:i + SECT] != expect[i:i + SECT]]
        va, vb = Ext2(fh, plan["a"]), Ext2(fh, plan["b"])
        ents = va.entries()
        ino = plan["ino"]
        # Freed: RD's own block, RMI's and OWI's twelve direct blocks; taken:
        # OWI's new block. Their own indirect blocks are no file's now.
        want_bitmap = plan["a_bitmap"]
        for blk in [plan["rd_block0"]] + plan["direct"]["RMI"] + plan["direct"]["OWI"]:
            want_bitmap = with_bits(want_bitmap, blk - va.fdb, blk - va.fdb + 1, 0)
        owi_new = va.ptr(ino["OWI"], 0) if "OWI" in ents else 0
        if owi_new:
            want_bitmap = with_bits(want_bitmap, owi_new - va.fdb, owi_new - va.fdb + 1, 1)
        owi_size = va.size(ino["OWI"])
        rmi_inode_bit = bit(va.bitmap(4), ino["RMI"] - 1)
        a_bitmap = va.bitmap(0)
        b_names = set(vb.entries())
        b_inode_bits = [bit(vb.bitmap(4), n) for n in range(plan["b_inodes_used"] + 2)]
        wr_size, ind_ptr = va.size(ino["WR"]), va.ptr(ino["IND"], 12)
        ind_size = va.size(ino["IND"])
    rd, ghost = o.get("cat /mnt/a/RD | wc"), o.get("cat /mnt/a/GHOST | wc")
    listed = set((o.get("ls /mnt/a") or "").split())
    used = plan["b_inodes_used"]
    return [
        ("driven to the end, every command's output found", driven and len(o) == len(COMMANDS)),
        ("fsd mounted ext2 as /", re.search(r"^fsd: ext2 mounted", out, re.M) is not None),
        ("A, C and B mount", all(o.get(c) == "" for c in COMMANDS[:3])),
        ("ls /mnt/a lists the seven files", set(FILES_A) <= listed),
        (f"SRC reads whole, 100 bytes (got {wc_bytes(o.get('cat /mnt/a/SRC | wc'))})",
         wc_bytes(o.get("cat /mnt/a/SRC | wc")) == 100),
        (f"RD's read stops at an I/O error, no witness byte counted (got {rd!r})",
         io in (rd or "") and (wc_bytes(rd) or 0) <= 1024),
        (f"WR's append refused with an I/O error, its size still 1100 (got {o.get('echo tail >> /mnt/a/WR')!r}, {wr_size})",
         io in (o.get("echo tail >> /mnt/a/WR") or "") and wr_size == 1100),
        (f"IND's append refused with an I/O error, its size and pointer as planted (got {o.get('echo tail >> /mnt/a/IND')!r})",
         io in (o.get("echo tail >> /mnt/a/IND") or "") and ind_size == 12 * 1024
         and ind_ptr == ((wit_start - plan["a"]) * SECT + AT_IND * 1024) // va.bs),
        (f"GHOST refused with an I/O error, not SRC's bytes (got {ghost!r})",
         io in (ghost or "") and wc_bytes(ghost) in (None, 0)),
        (f"rm of RD succeeds and RD is gone (got {o.get('rm /mnt/a/RD')!r})",
         o.get("rm /mnt/a/RD") == "" and "RD" not in ents),
        (f"rm of RMI, its indirect pointer past A, succeeds and frees its inode (got {o.get('rm /mnt/a/RMI')!r})",
         o.get("rm /mnt/a/RMI") == "" and "RMI" not in ents and rmi_inode_bit == 0),
        (f"OWI, its indirect pointer past A, overwritten without an error (got {o.get('echo x > /mnt/a/OWI')!r})",
         o.get("echo x > /mnt/a/OWI") == "" and o.get("cat /mnt/a/OWI") == "x" and owi_size == 2),
        ("A's block bitmap as before but RD's, RMI's and OWI's direct blocks freed and OWI's new one taken",
         a_bitmap == want_bitmap),
        (f"C's new file's write refused with an I/O error (got {o.get('echo x > /mnt/c/NEW')!r})",
         io in (o.get("echo x > /mnt/c/NEW") or "")),
        (f"B's write answers disk full (got {o.get('echo x > /mnt/b/NEW')!r})",
         "disk full" in (o.get("echo x > /mnt/b/NEW") or "")),
        (f"B's touch NEW2 answers disk full and makes nothing (got {o.get('touch /mnt/b/NEW2')!r})",
         "disk full" in (o.get("touch /mnt/b/NEW2") or "") and "NEW2" not in b_names),
        (f"B's inode bits: the {used} in use and NEW's set, none past the count ({b_inode_bits[-3:]})",
         b_inode_bits == [1] * (used + 1) + [0]),
        (f"the witness unchanged (changed sectors: {changed[:8]}{'...' if len(changed) > 8 else ''})",
         changed == []),
        ("no fault lines", faults == 0),
    ]


def main():
    plan, checks = build()
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    if failed:
        print(f"test-ext2-blocks: {failed} check(s) FAILED before the boot")
        if os.path.exists(DISK):
            os.remove(DISK)
        return 1
    try:
        driven, out, faults = boot()
        results = verdicts(plan, driven, out, faults)
    finally:
        if os.path.exists(DISK) and not os.environ.get("KEEP"):
            os.remove(DISK)
    for label, ok in results:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    print(f"test-ext2-blocks: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    shutil.which("qemu-system-aarch64") or sys.exit("qemu-system-aarch64 not found")
    sys.exit(main())

#!/usr/bin/env python3
"""ext2 never reaches past its volume, by a block number read off the disk.

    python3 scripts/test-ext2-blocks.py     (or `make test-ext2-blocks`, which builds the images)

Until 2026-10-20 ext2 took any block number it read to the disk: an inode's
pointer, an indirect block's, a descriptor's bitmap. A damaged one sent a
read or a write past the volume, into the next partition or past the disk.
Since #330 the volume itself is bounded by its partition at mount, and now
every sector the arm reads or writes goes through one `Volume`, which refuses
a sector past `s_blocks_count` blocks as an I/O error; an inode number past
`s_inodes_count` is refused the same way, and the allocators take no bit of
the last group's bitmaps past the volume's last block or inode. A free reads
nothing past the volume, neither a pointer block nor a bitmap, so a file
whose pointers or bitmaps lie there is still removed whole, and the free
counts rise only for a bit actually cleared; an inode holding no blocks (a
fast symlink, its target's text in its pointers, told by its type and
length, not by i_blocks) frees none, and an entry
naming an inode past the count can be removed (the reviews of #331).
Since 2026-10-21 no block of the volume's own metadata is freed or
allocated: below `meta_end` (the superblock, the descriptor table and its
reserved blocks) or a group's bitmaps or inode table, Linux's "system zone";
nor is a pointer block there read as pointers.

The firmware boots esp.img from NVMe, so the virtio disk the rig builds goes
in as the fallback: build/espext2.img's ext2 root as partition 0 (it mounts
at boot), three 4 MiB ext2 volumes made by mke2fs with 1 KiB blocks, A, C and
B, as partitions 1 to 3, and after B, unpartitioned, a WITNESS of 0xA5 bytes
with a zeroed block, which nothing may change. The host plants:

- on A, RD (two blocks), its second pointer into the witness: `cat` must
  stop at an I/O error, not read the witness's bytes; WR (1,100 bytes), its
  second pointer into the witness: an append must be refused and write
  nothing; IND (twelve blocks), its indirect pointer at a zeroed witness
  block: an append must be refused, where it wrote the new block's pointer
  there; GHOST, its directory entry naming inode `inodes_per_group` past
  SRC's, in a second group whose descriptor slot holds a copy of the first
  group's: `cat` must answer an I/O error, not SRC's bytes; SRC itself must
  still read whole (100 bytes), and `rm GHOST` must drop the entry; `rm` of
  RD must remove it and free only its own block, not the witness block's
  bit in another group's bitmap, leaving its inode with a deletion time and
  no links; L, a fast symlink whose first pointer names SRC's block: `rm`
  must leave that block allocated, its i_blocks raised to 2 as an
  extended-attribute block raises it; Z, a regular file with i_blocks
  damaged to 0: `rm` must free its block; BIGD (270 KiB, to its
  double-indirect block): `rm` must free every block it holds; RMI and
  OWI (thirteen blocks each), their indirect blocks swapped for witness
  blocks: `rm RMI` must free its inode and `echo x > OWI` must leave it
  holding `x` CR LF, each with no error; and DUP (two blocks), its second
  pointer naming a block already free: `rm` must succeed, and A's free
  counts must still match its bitmap after every case;
- on A too, since 2026-10-21, the last three blocks of group 0's inode
  table, past every inode in use, and the last two of the descriptor blocks
  resize_inode reserves, below meta_end: T, filled with 0x5A, and R, their
  bits cleared alone (damage to the bitmap, the counts left), where the next
  allocation (OWI's new block) must pass over both, setting their bits and
  counting nothing, and leave T's bytes; META, its second pointer naming
  T2: `rm` must leave T2 allocated; MI and MG (thirteen blocks each), their
  indirect pointers naming T3 and R2, every word of each naming SRC's
  block: `rm` must free none of them; and GD, its pointer naming the
  descriptor table's block: `rm` must leave it allocated;
- on C, made without the filetype feature (a listing reads each entry's
  inode), H's entry naming an inode past the count: `ls` must list F, G and
  H. Until 2026-10-21 C's bitmaps were planted in the witness as well; a
  descriptor outside its group is refused at mount now (test-volume-bound's
  SHAPES-H), so bitmap_free's and the allocator's bounds past the volume
  sit behind that refusal and no rig reaches them; and since 2026-10-21 C
  left one block free, its counts saying so, with DIND ending at its last
  single-indirect block and TWELVE at its last direct one: appends to them
  (three blocks needed, and two) must answer disk full and leave both files
  as planted, and `echo q > ONEB` must then get the block;
- on B, every block of the volume marked used and the last group's padding
  bits clear, so the next free bit is the block just past the volume (the
  witness's first): `echo x > NEW` must answer disk full; and the inode count
  cut to one more than the inodes in use, so after NEW takes it the next free
  bit is past the count: `touch NEW2` must answer disk full and leave the bit
  clear.

Before the boot the host reads back every plant and checks it lands where
this says (the observer's own checks). Fails with Volume's bound taken out
(RD reads 2,048 bytes, WR's and IND's appends write the witness),
with the inode-number bound out (GHOST reads SRC), with the allocator's
block cap out (B's write an I/O error, not disk full: Volume refuses the
block the allocator handed out), with its inode cap out (NEW2 an I/O error,
its bit set past the count), with free_block freeing a block past the volume
(a bit of A's bitmap cleared, through the second group's descriptor), with
free_pointed reading a pointer block past the volume (RMI's rm and OWI's
overwrite an I/O error), with the free counts raised for a bit already clear
(A's counts one past its bitmap), with the listing reading an inode past
the count (`ls /mnt/c` an I/O error), with the fast-symlink test taken out
(SRC's block freed by `rm L`) or made by i_blocks (that, and Z's block
kept), with the pointer walk's span a block a pointer at every depth (BIGD's
last data block kept), with rm reading GHOST's inode (an I/O
error), with the deletion time left out of a freed inode, with the
allocator taking a metadata block (OWI's new block is T, its pattern
overwritten, or R with the allocator's meta_end arm out), with the
allocator counting a block it passes over (A's counts two below its
bitmap), with free_block freeing a group's bitmap or table block
(META's T2 and MI's T3 freed) or one below meta_end (GD's descriptor-table
block freed), and with free_pointed reading a metadata block as pointers
(SRC's block freed through T3, and through R2 with is_metadata's meta_end
arm out), and with ensure_block taking a block before it knows it can take
them all (ONEB disk full: main's order before 2026-10-21, the give-back
out, the indirect branch not asking for its pointer block). One boot, about a minute, needs
e2fsprogs. Run it whenever ext2's Volume, block_sector, inode_place,
alloc_block, alloc_inode, free_block, free_inode, bitmap_free,
free_all_blocks, free_pointed, pointer_sector, ensure_block, list_dir, rm,
write_inode, is_metadata, in_zone or meta_end changes.
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
AT_RD, AT_WR, AT_IND, AT_RMI, AT_OWI = 8, 16, 24, 40, 48
ZEROED = (AT_IND,)
# The pattern a planted inode-table block holds, which an allocation over it
# would overwrite.
TABLE_FILL = b"\x5a"
FILES_A = {"RD": b"R" * 2048, "WR": b"W" * 1100, "IND": b"I" * 12 * 1024, "SRC": b"S" * 100,
           "GHOST": b"G" * 10, "RMI": b"M" * 13 * 1024, "OWI": b"O" * 13 * 1024, "DUP": b"D" * 2048, "Z": b"Z" * 1024,
           "BIGD": b"B" * 270 * 1024, "META": b"T" * 2048, "MI": b"N" * 13 * 1024,
           "GD": b"Q" * 1024, "MG": b"K" * 13 * 1024}
# TWELVE ends at its last direct block and DIND at its last single-indirect
# one, so an append to either needs a pointer block it does not have.
FILES_C = {"F": b"F" * 1024, "G": b"g" * 1024, "H": b"h" * 10, "TWELVE": b"2" * 12 * 1024,
           "DIND": b"3" * (12 + 256) * 1024}
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
    "rm /mnt/a/DUP",
    "rm /mnt/a/L",
    "rm /mnt/a/Z",
    "rm /mnt/a/BIGD",
    "rm /mnt/a/GHOST",
    "rm /mnt/a/META",
    "rm /mnt/a/MI",
    "rm /mnt/a/GD",
    "rm /mnt/a/MG",
    "ls /mnt/c",
    "echo y >> /mnt/c/DIND",
    "echo y >> /mnt/c/TWELVE",
    "echo q > /mnt/c/ONEB",
    "rm /mnt/c/G",
    "cp /mnt/c/TWELVE /mnt/c/CP",
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

    def mode(self, ino):
        return struct.unpack("<H", self.read(self.inode_off(ino), 2))[0]

    def i_blocks(self, ino):
        return self.u32(self.inode_off(ino) + 28)

    def pointers(self, blk):
        return [p for p in struct.unpack(f"<{self.bs // 4}I", self.read(blk * self.bs, self.bs)) if p]

    def all_blocks(self, ino):
        """Every block inode `ino` holds: data and pointer blocks, to double
        indirect."""
        out = [p for p in (self.ptr(ino, k) for k in range(12)) if p]
        if self.ptr(ino, 12):
            out += [self.ptr(ino, 12)] + self.pointers(self.ptr(ino, 12))
        if self.ptr(ino, 13):
            out.append(self.ptr(ino, 13))
            for sib in self.pointers(self.ptr(ino, 13)):
                out += [sib] + self.pointers(sib)
        return out

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


def intact(fh, start):
    """Whether the volume at `start` still has an ext2 superblock the reader
    can walk: a control can zero it (C's, by a write to block 1)."""
    fh.seek(start * SECT + 1024)
    sb = fh.read(1024)
    return struct.unpack_from("<H", sb, 56)[0] == 0xEF53 and struct.unpack_from("<I", sb, 40)[0] > 0


def bit(bm, n):
    return bm[n // 8] >> (n % 8) & 1


def with_bits(bm, lo, hi, value):
    bm = bytearray(bm)
    for n in range(lo, hi):
        bm[n // 8] = bm[n // 8] | 1 << (n % 8) if value else bm[n // 8] & ~(1 << (n % 8))
    return bytes(bm)


def free_delta(v):
    """The superblock's and group 0's free-block counts less the free bits of
    the bitmap: what a free path must leave as it was."""
    bm = v.bitmap(0)
    zeros = sum(1 for n in range(v.bc - v.fdb) if not bit(bm, n))
    return (v.u32(1024 + 12) - zeros, struct.unpack("<H", v.read(v.desc(0) + 12, 2))[0] - zeros)


def mkvol(files, features=(), links=None):
    """A 4 MiB ext2 with 1 KiB blocks holding `files` and the symlinks
    `links` ({name: target}), made by mke2fs -d."""
    mke2fs = bootstick.tool("mke2fs")
    with tempfile.TemporaryDirectory(dir=BUILD) as tmp:
        src = os.path.join(tmp, "src")
        os.mkdir(src)
        for name, data in files.items():
            with open(os.path.join(src, name), "wb") as fh:
                fh.write(data)
        for name, target in (links or {}).items():
            os.symlink(target, os.path.join(src, name))
        img = os.path.join(tmp, "vol.img")
        made = subprocess.run([mke2fs, "-q", "-t", "ext2", "-b", "1024", *features, "-d", src, "-F", img, "4m"],
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
    a, c, b = mkvol(FILES_A, links={"L": "ab"}), mkvol(FILES_C, ("-O", "^filetype")), mkvol({})
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
        checks.append((f"plant: A has 1 KiB blocks and its {len(FILES_A)} files",
                       va.bs == 1024 and set(FILES_A) <= set(ents)))
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
        # A, before GHOST's descriptor copy (T's count is group 0's): the last three blocks of group 0's inode table, which hold no
        # inode in use. T: its bit cleared, the free counts raised to match,
        # filled with a pattern; an allocation must pass over it. META's
        # second pointer names T2: rm must leave it allocated. MI's indirect
        # pointer names T3, which holds SRC's block number in every word: rm
        # must read no pointer there and free neither.
        table = va.u32(va.desc(0) + 8)
        itb = -(-(va.ipg * va.isz) // va.bs)
        t, t2, t3 = table + itb - 1, table + itb - 2, table + itb - 3
        # R and R2: the last two of the descriptor blocks resize_inode
        # reserves, below meta_end. R's bit cleared, as T's, before T's in the
        # bitmap: the allocation passes over both. MG's indirect pointer
        # names R2, which holds SRC's block number in every word.
        reserved = struct.unpack("<H", va.read(1024 + 206, 2))[0] if va.u32(1024 + 92) & 0x10 else 0
        meta_end = va.fdb + 1 + -(-(-(-va.ic // va.ipg) * 32) // va.bs) + reserved
        r, r2 = meta_end - 1, meta_end - 2
        delta_before = free_delta(va)
        va.write(t * va.bs, TABLE_FILL * va.bs)
        # The bits cleared alone, the free counts as they were: damage to the
        # bitmap, which an allocation passing over sets again and counts
        # nothing for (Linux's ext2_new_blocks).
        for blk in (r, t):
            va.set_bitmap(0, with_bits(va.bitmap(0), blk - va.fdb, blk - va.fdb + 1, 0))
        va.write(r2 * va.bs, struct.pack("<I", va.ptr(ino["SRC"], 0)) * (va.bs // 4))
        mg_direct = [va.ptr(ino["MG"], k) for k in range(12)]
        va.set_ptr(ino["MG"], 12, r2)
        va.set_ptr(ino["META"], 1, t2)
        va.write(t3 * va.bs, struct.pack("<I", va.ptr(ino["SRC"], 0)) * (va.bs // 4))
        mi_direct = [va.ptr(ino["MI"], k) for k in range(12)]
        va.set_ptr(ino["MI"], 12, t3)
        first_table_ino = (t3 - table) * (va.bs // va.isz) + 1
        # GD's one pointer names the descriptor table's block, below every
        # group's bitmaps: rm must leave it allocated.
        gd_own = va.ptr(ino["GD"], 0)
        va.set_ptr(ino["GD"], 0, va.fdb + 1)
        plan.update(t=t, t2=t2, t3=t3, r=r, r2=r2, meta0=va.ptr(ino["META"], 0), mi_direct=mi_direct,
                    mg_direct=mg_direct)
        # A: GHOST names SRC's inode one group on, whose descriptor is group 0's.
        ghost = va.ipg + ino["SRC"]
        va.set_u32(ents["GHOST"][1], ghost)
        va.write(va.desc(1), va.read(va.desc(0), 32))
        plan.update(ino=ino, rd_block0=va.ptr(ino["RD"], 0), a_bitmap=va.bitmap(0),
                    direct={n: [va.ptr(ino[n], k) for k in range(12)] for n in ("RMI", "OWI")})
        # C: H's entry naming an inode past its count, on a volume without
        # filetype (so a listing reads each entry's inode). Its bitmaps were
        # planted in the witness until 2026-10-21, when a descriptor outside
        # its group came to be refused at mount (test-volume-bound's SHAPES-H).
        c_ents = vc.entries()
        vc.set_u32(c_ents["H"][1], vc.ic + 5)
        # C: one block left free, the highest, and the counts saying so. An
        # append to DIND needs three, one to TWELVE two: both must take none.
        c_real = vc.bc - vc.fdb
        c_free = max(n for n in range(c_real) if not bit(vc.bitmap(0), n))
        vc.set_bitmap(0, with_bits(with_bits(vc.bitmap(0), 0, c_real, 1), c_free, c_free + 1, 0))
        vc.set_u32(1024 + 12, 1)
        vc.write(vc.desc(0) + 12, struct.pack("<H", 1))
        c_ino = {n: vc.entries()[n][0] for n in ("TWELVE", "DIND")}
        plan.update(c_free=vc.fdb + c_free, c_ino=c_ino, g_block=vc.ptr(vc.entries()["G"][0], 0),
                    c_blocks={n: (vc.size(i), vc.i_blocks(i), vc.ptr(i, 12), vc.ptr(i, 13)) for n, i in c_ino.items()})
        # A: L, a fast symlink (its target's text in its pointers), made to
        # name SRC's block as its text's first four bytes, as a target of
        # those bytes would.
        lino = ents["L"][0]
        va.set_ptr(lino, 0, va.ptr(ino["SRC"], 0))
        # Its i_blocks raised as an extended-attribute block raises it, and
        # Z's (a regular file) damaged to 0: neither tells a fast symlink.
        va.set_u32(va.inode_off(lino) + 28, va.bs // SECT)
        va.set_u32(va.inode_off(ino["Z"]) + 28, 0)
        plan.update(l_ino=lino, src_block=va.ptr(ino["SRC"], 0), z_block=va.ptr(ino["Z"], 0),
                    bigd=va.all_blocks(ino["BIGD"]))
        # A: DUP's second pointer names a block already free.
        free_blk = va.fdb + max(n for n in range(va.bc - va.fdb) if not bit(va.bitmap(0), n))
        va.set_ptr(ino["DUP"], 1, free_blk)
        plan.update(dup=(va.ptr(ino["DUP"], 0), free_blk), a_free_delta=free_delta(va))
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
            ("plant: T, T2 and T3 are the last blocks of A's group 0 inode table, past every inode in use",
             not any(bit(va.bitmap(4), n) for n in range(first_table_ino - 1, va.ipg))
             and t3 >= table and t < table + itb),
            ("plant: T's and R's bits are clear, the free counts as they were, T's bytes the pattern",
             not bit(va.bitmap(0), t - va.fdb) and not bit(va.bitmap(0), r - va.fdb)
             and va.read(t * va.bs, va.bs) == TABLE_FILL * va.bs
             and free_delta(va) == tuple(d - 2 for d in delta_before)),
            (f"plant: A reserves descriptor blocks ({reserved}), R and R2 the last two, below its first bitmap; "
             "MG's indirect pointer names R2, which names SRC's block",
             reserved >= 2 and r < va.u32(va.desc(0)) and va.ptr(ino["MG"], 12) == r2
             and va.u32(r2 * va.bs) == va.ptr(ino["SRC"], 0) and all(mg_direct)),
            ("plant: GD's pointer names the descriptor table's block, set in A's bitmap",
             va.ptr(ino["GD"], 0) == va.fdb + 1 and bit(va.bitmap(0), 1) and gd_own != 0),
            ("plant: META's second pointer names T2, and MI's indirect pointer T3, which names SRC's block",
             va.ptr(ino["META"], 1) == t2 and va.ptr(ino["MI"], 12) == t3
             and va.u32(t3 * va.bs) == va.ptr(ino["SRC"], 0) != 0 and all(mi_direct)),
            ("plant: C has no filetype feature, its files, and H naming an inode past its count",
             not vc.u32(1024 + 96) & 2 and set(FILES_C) <= set(c_ents) and vc.entries()["H"][0] > vc.ic),
            ("plant: C has one block free, its counts 1; TWELVE has no indirect block, DIND an indirect "
             "and no double-indirect block, each ending at its last block",
             vc.bc - vc.fdb <= vc.bpg and [n for n in range(vc.bc - vc.fdb) if not bit(vc.bitmap(0), n)] == [plan["c_free"] - vc.fdb]
             and free_delta(vc) == (0, 0)
             and plan["c_blocks"]["TWELVE"][0] == 12 * 1024 and plan["c_blocks"]["TWELVE"][2] == 0
             and plan["c_blocks"]["DIND"][0] == 268 * 1024 and plan["c_blocks"]["DIND"][2] != 0
             and plan["c_blocks"]["DIND"][3] == 0),
            ("plant: L is a fast symlink (2 bytes) whose first pointer names SRC's block, its i_blocks 2; Z's 0",
             va.mode(lino) & 0xF000 == 0xA000 and va.size(lino) == 2 and va.i_blocks(lino) == va.bs // SECT
             and va.i_blocks(ino["Z"]) == 0 and va.ptr(ino["Z"], 0) != 0
             and va.ptr(lino, 0) == va.ptr(ino["SRC"], 0) != 0),
            (f"plant: BIGD reaches its double-indirect block, {len(plan['bigd'])} blocks in all",
             va.ptr(ino["BIGD"], 13) != 0 and len(plan["bigd"]) == 270 + 1 + 1 + 1),
            ("plant: DUP's second pointer names a block A's bitmap holds free",
             not bit(va.bitmap(0), va.ptr(ino["DUP"], 1) - va.fdb) and va.ptr(ino["DUP"], 1) < va.bc),
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
        whole = {k: intact(fh, plan[k]) for k in ("a", "b", "c")}
        if not (whole["a"] and whole["b"]):
            return [(f"A's and B's superblocks intact ({whole})", False),
                    (f"the witness unchanged (changed sectors: {changed[:8]})", changed == [])]
        va, vb = Ext2(fh, plan["a"]), Ext2(fh, plan["b"])
        vc = Ext2(fh, plan["c"]) if whole["c"] else None
        c_after = ({n: (vc.size(i), vc.i_blocks(i), vc.ptr(i, 12), vc.ptr(i, 13)) for n, i in plan["c_ino"].items()}
                   if vc else {})
        c_ents = vc.entries() if vc else {}
        oneb = vc.ptr(c_ents["ONEB"][0], 0) if vc and "ONEB" in c_ents else None
        c_counts = (free_delta(vc), bit(vc.bitmap(0), plan["c_free"] - vc.fdb)) if vc else None
        cp_ino = c_ents["CP"][0] if vc and "CP" in c_ents else 0
        cp_now = ((vc.size(cp_ino), vc.i_blocks(cp_ino), vc.ptr(cp_ino, 0), vc.read(vc.ptr(cp_ino, 0) * vc.bs, 1024))
                  if cp_ino else None)
        g_bit = bit(vc.bitmap(0), plan["g_block"] - vc.fdb) if vc else None
        a_free_delta = free_delta(va)
        ents = va.entries()
        ino = plan["ino"]
        # Freed: RD's own block, RMI's and OWI's twelve direct blocks; taken:
        # OWI's new block. Their own indirect blocks are no file's now.
        want_bitmap = plan["a_bitmap"]
        for blk in ([plan["rd_block0"], plan["dup"][0], plan["z_block"], plan["meta0"]] + plan["direct"]["RMI"]
                    + plan["direct"]["OWI"] + plan["bigd"] + plan["mi_direct"]
                    + plan["mg_direct"]):
            want_bitmap = with_bits(want_bitmap, blk - va.fdb, blk - va.fdb + 1, 0)
        # T and R, planted free, set again by the allocation that passes over them.
        for blk in (plan["t"], plan["r"]):
            want_bitmap = with_bits(want_bitmap, blk - va.fdb, blk - va.fdb + 1, 1)
        t_bytes = va.read(plan["t"] * va.bs, va.bs)
        t2_bit, t3_bit = (bit(va.bitmap(0), plan[k] - va.fdb) for k in ("t2", "t3"))
        gdt_bit = bit(va.bitmap(0), 1)
        r2_bit = bit(va.bitmap(0), plan["r2"] - va.fdb)
        # The overwrite may give OWI a new inode: read it by its entry.
        owi = ents["OWI"][0] if "OWI" in ents else 0
        owi_new = va.ptr(owi, 0) if owi else 0
        if owi_new:
            want_bitmap = with_bits(want_bitmap, owi_new - va.fdb, owi_new - va.fdb + 1, 1)
        owi_size = va.size(owi) if owi else None
        owi_data = va.read(owi_new * va.bs, owi_size) if owi_new and owi_size else b""
        rmi_inode_bit = bit(va.bitmap(4), ino["RMI"] - 1)
        src_bit = bit(va.bitmap(0), plan["src_block"] - va.fdb)
        z_bit = bit(va.bitmap(0), plan["z_block"] - va.fdb)
        # A freed inode carries a deletion time and no links (e2fsck reads a
        # small i_dtime as an orphan-list pointer).
        rd_off = va.inode_off(ino["RD"])
        rd_dead = (va.u32(rd_off + 20), struct.unpack("<H", va.read(rd_off + 26, 2))[0])
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
        ("C's superblock intact after the boot", whole["c"]),
        (f"ls /mnt/a lists the {len(FILES_A)} files", set(FILES_A) <= listed),
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
        (f"rm of RD succeeds and RD is gone, its inode with a deletion time and no links (got {o.get('rm /mnt/a/RD')!r}, {rd_dead})",
         o.get("rm /mnt/a/RD") == "" and "RD" not in ents and rd_dead[0] != 0 and rd_dead[1] == 0),
        (f"rm of RMI, its indirect pointer past A, succeeds and frees its inode (got {o.get('rm /mnt/a/RMI')!r})",
         o.get("rm /mnt/a/RMI") == "" and "RMI" not in ents and rmi_inode_bit == 0),
        (f"OWI, its indirect pointer past A, overwritten without an error (got {o.get('echo x > /mnt/a/OWI')!r}, holding {owi_data!r})",
         o.get("echo x > /mnt/a/OWI") == "" and o.get("cat /mnt/a/OWI") == "x" and owi_data == b"x\r\n"
         and owi == ino["OWI"]),
        ("A's block bitmap as before but RD's, DUP's, Z's, META's first, RMI's, OWI's and MI's direct and BIGD's every "
         "block freed, OWI's new one taken and T's set again",
         a_bitmap == want_bitmap),
        (f"OWI's new block is neither T, the inode-table block A's bitmap held free, nor R, the reserved "
         f"descriptor block it held free, and T holds its pattern still (OWI's block {owi_new}, T {plan['t']}, "
         f"R {plan['r']})",
         owi_new not in (0, plan["t"], plan["r"]) and t_bytes == TABLE_FILL * va.bs),
        (f"rm of META, its second pointer naming an inode-table block, succeeds and leaves that block allocated "
         f"(got {o.get('rm /mnt/a/META')!r})",
         o.get("rm /mnt/a/META") == "" and "META" not in ents and t2_bit == 1),
        (f"rm of MI, its indirect pointer naming an inode-table block holding SRC's block number, succeeds and "
         f"frees neither (got {o.get('rm /mnt/a/MI')!r})",
         o.get("rm /mnt/a/MI") == "" and "MI" not in ents and t3_bit == 1 and src_bit == 1),
        (f"rm of GD, its pointer naming the descriptor table's block, succeeds and leaves that block allocated "
         f"(got {o.get('rm /mnt/a/GD')!r})",
         o.get("rm /mnt/a/GD") == "" and "GD" not in ents and gdt_bit == 1),
        (f"rm of MG, its indirect pointer naming a reserved descriptor block holding SRC's block number, "
         f"succeeds and frees neither (got {o.get('rm /mnt/a/MG')!r})",
         o.get("rm /mnt/a/MG") == "" and "MG" not in ents and r2_bit == 1 and src_bit == 1),
        (f"rm of DUP, its second block already free, succeeds (got {o.get('rm /mnt/a/DUP')!r})",
         o.get("rm /mnt/a/DUP") == "" and "DUP" not in ents),
        (f"rm of L succeeds and SRC's block is still allocated (got {o.get('rm /mnt/a/L')!r})",
         o.get("rm /mnt/a/L") == "" and "L" not in ents and src_bit == 1),
        (f"rm of Z, its i_blocks 0, frees its block (got {o.get('rm /mnt/a/Z')!r})",
         o.get("rm /mnt/a/Z") == "" and "Z" not in ents and z_bit == 0),
        (f"rm of BIGD, past single indirect, succeeds (got {o.get('rm /mnt/a/BIGD')!r})",
         o.get("rm /mnt/a/BIGD") == "" and "BIGD" not in ents),
        (f"rm of GHOST, its inode past the count, drops the entry (got {o.get('rm /mnt/a/GHOST')!r})",
         o.get("rm /mnt/a/GHOST") == "" and "GHOST" not in ents),
        # T's and R's bits were cleared with the counts left: set again with
        # nothing counted, the counts are back level with the bitmap.
        (f"A's free counts match its bitmap as before T's and R's bits were cleared "
         f"({a_free_delta} vs {plan['a_free_delta']} with them clear)",
         a_free_delta == tuple(d + 2 for d in plan["a_free_delta"])),
        (f"ls /mnt/c lists F, G and H, H's inode past the count (got {o.get('ls /mnt/c')!r})",
         set(FILES_C) <= set((o.get("ls /mnt/c") or "").split())),
        (f"C's appends to DIND and TWELVE answer disk full (got {o.get('echo y >> /mnt/c/DIND')!r}, "
         f"{o.get('echo y >> /mnt/c/TWELVE')!r})",
         all("disk full" in (o.get(f"echo y >> /mnt/c/{n}") or "") for n in ("DIND", "TWELVE"))),
        (f"DIND and TWELVE as planted: size, i_blocks, indirect and double-indirect pointers ({c_after})",
         c_after == plan["c_blocks"]),
        (f"C's one free block taken by ONEB, the counts level with the bitmap (got {o.get('echo q > /mnt/c/ONEB')!r}, "
         f"ONEB's block {oneb}, the free block {plan['c_free']}, {c_counts})",
         o.get("echo q > /mnt/c/ONEB") == "" and oneb == plan["c_free"] and c_counts == ((0, 0), 1)),
        # cp writes 2 KiB a request: the first block takes G's, the second
        # finds the disk full, and the file keeps the one it filled.
        (f"cp onto C with one block free answers disk full and CP keeps its first block, G's "
         f"(got {o.get('cp /mnt/c/TWELVE /mnt/c/CP')!r}, rm {o.get('rm /mnt/c/G')!r}, "
         f"CP {cp_now[:3] if cp_now else None}, G's block {plan['g_block']} set {g_bit}, counts {c_counts})",
         "disk full" in (o.get("cp /mnt/c/TWELVE /mnt/c/CP") or "") and o.get("rm /mnt/c/G") == ""
         and cp_now == (1024, 1024 // SECT, plan["g_block"], b"2" * 1024) and g_bit == 1
         and c_counts is not None and c_counts[0] == (0, 0)),
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

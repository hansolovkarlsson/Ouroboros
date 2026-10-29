#!/usr/bin/env python3
"""exFAT entry sets as the spec shapes them, and only those, are patched whole.

    python3 scripts/test-long-set.py     (or `make test-long-set`, which builds the image)
    python3 scripts/test-long-set.py --disk-gate   (or `make test-disk-gate`)

--disk-gate (since 2026-10-29) runs the read-only boots alone, VBITMAP,
VOVERLAP, VFIRST, VTWOFAT and VRUNS, on a copy of esp.img carrying fsd's
`disk-gate-rig` build, whose verb gate does not ask an arm, so each refused
write is exFAT's own disk gate's (`gate::Gated`), which no boot of the
shipped fsd reaches since the verb gate of 2026-10-28. Every boot, in either
mode, checks which fsd it ran by that build's startup line.

When fsd changes a file's Stream entry (its first cluster and length, on a
write), `patch_stream_ext` rewrites File and Stream and recomputes the set's
SetChecksum. Until 2026-10-19 it summed at most MAX_SET_ENTRIES (19)
entries, the longest set fsd builds, though the spec allows longer (vendor
secondaries after the name, which cameras write), so such a set was left with
a checksum over part of it, which fsck_exfat offers to repair by deleting the
set. And `walk_dir` closed a set that was not contiguous, or whose first
secondary was not the Stream, so the patch wrote Stream fields into whatever
slot followed the File. And until 2026-10-21 a rename built the new set
afresh (File, Stream, name), dropping the File entry's attributes and times
and every vendor entry. And until 2026-10-23 `rm`, `rmdir` and a replacing
`mv` left the clusters a vendor allocation entry (0xE1) names set in the
bitmap, with no set naming them. Fourteen boots, each of a copy of
build/espexfat.img (exFAT root, partition 1):

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
  new ones), and `ls /` lists each name once;
- RENAME: /VEND's 20-entry set after the root's last, its File entry given
  the archive attribute and three timestamps. `mv /VEND /VendorRenamedLong`
  (two name entries: the vendor entries move right) and `mv
  /VendorRenamedLong /V2` (one: they move left), `ls /` after each; after,
  no set under either old name, /V2's 20 entries with the File entry's
  bytes past its checksum, the Stream's flags, ValidDataLength, cluster and
  DataLength (a contiguous cluster of its own, 7 bytes of it valid) and the
  17 vendor entries as planted, the SetChecksum whole; and STRAY, its name
  entry after a vendor entry (fsck_exfat's one complaint before the boot),
  `mv`'d to /S2, which must be File, Stream, the new name, the vendor entry,
  with fsck_exfat -n passing after;
- VALLOC: sets after the root's last on fifteen free clusters in a run the
  host marks used, each run followed by a cluster that must stay used, and
  fsck_exfat -n clean first: VA1 (7 bytes in a contiguous cluster, a 0xE1
  naming two contiguous) removed by `rm`; VA2 (empty, a 0xE1 naming two
  FAT-chained, the last partly used) replaced by `mv -f /SRC /VA2`, where
  SRC holds 5 bytes and a vendor cluster of its own; VD (an empty
  directory, a vendor cluster) removed by `rmdir`; VE2, its benign
  secondary of the undefined type 0xE2 naming a cluster, removed; VE0, a
  0xE0 (no AllocationPossible) whose bytes name W3's cluster, removed; VA3
  renamed to VA4 and then to va4 by case alone through `mv -f`. After: no
  command answered anything, `cat /VA2` prints SRC's bytes, the removed and
  replaced sets' clusters free (VA2's chain in the FAT too), SRC's two and
  VA3's vendor cluster still used with VA2's and va4's sets carrying their
  entries, every witness still used, fsck_exfat -n clean;
- VCROSS: damaged sets, each planted as a cross-linking tool or a damaged
  disk would leave it (fsck_exfat is not run before this boot): SRC2 and
  DST2 sharing data, each its own size, and DST2's vendor entry naming the
  second cluster of SRC2's two-cluster vendor run; VA5 whose vendor run
  starts at its own data and reaches W5's cluster; VBAD's FAT-chained 0xE1
  outside the heap; VSYS's three on the allocation bitmap, the up-case
  table and the root's first cluster; VA6 whose vendor run starts a
  cluster before its data and ends a cluster after (W6's and W7's); VHEAP,
  contiguous on the heap's last cluster and sized two clusters past it;
  SRC3, FAT-chained over two clusters with one between, and DST3 on that
  one, its vendor run on SRC3's second cluster; SRC4, empty, with eight
  vendor runs of a cluster each, and DST4 on a cluster of its own; and the
  bitmap's own bit cleared. `echo new > /NEW` (first), `mv -f /SRC2
  /DST2`, `rm` of VA5, VBAD, VSYS and VA6, `writeat /VHEAP 0 x`, `rm
  /VHEAP`, `mv -f /SRC3 /DST3`, `mv -f /SRC4 /DST4`: no `mv` or `rm`
  answers anything, the writeat an error with VHEAP's own FAT entry and
  the FAT's bytes past the heap unchanged, NEW not on the bitmap's cluster and
  its bit set again, DST2's set SRC2's and all three of SRC2's clusters
  used, W5's, W6's and W7's still used, VA5's and VA6's data free, the
  bitmap's, up-case table's and root's clusters still used, VHEAP's
  cluster still used (a run past the heap is damage, freed not at all;
  the host gives it back after the boot), DST3's cluster free and both of
  SRC3's used under DST3's set, DST4's cluster free and SRC4's eight used
  under DST4's set, fsck_exfat -n clean after;
- VMEET: SRC FAT-chained over three clusters but sized two, DST on a
  cluster of its own with a FAT-chained vendor run of three clusters whose
  first is its own and whose last two are SRC's second and third: `mv -f
  /SRC /DST` must free DST's cluster and keep SRC's chain and the whole
  vendor run (its first cluster leaked, and SRC's third, past its size,
  mended on the host after the boot), DST's set SRC's, fsck_exfat -n
  clean after. SRC's walk stops at its size, before the run's last
  cluster, so the meeting is found only by the second walk of the run.
  In the same boot VW, whose vendor run lies on its own data cluster:
  `write /VW new` must give it a new cluster and keep the old one;
  VSYSW, a contiguous file of two clusters from the bitmap's: `writeat
  /VSYSW 0 x` must be refused with the bitmap's FAT entry and first byte
  unchanged (a run of one would leave its entry end of chain, as it was,
  even when linked); and VSYSC, a FAT-chained file on the bitmap's
  cluster, and VLINK, one on a cluster of its own linked into the bitmap's,
  must each be refused a `writeat` with the bitmap's first byte
  unchanged; `rm` of all three answers nothing and VLINK's own cluster is
  freed;
- VROOT (since 2026-10-26): the root given a second cluster, its runs of
  deleted slots filled with empty files' sets and the rest filled to its last
  two slots, so a create grows it. RX contiguous on the root's second
  cluster; SD a FAT-chained directory of two clusters, its own (every slot
  in use) linked to the bitmap's last; RR FAT-chained on the bitmap's last;
  LX FAT-chained on a cluster of its own linked to the bitmap's last and HX
  on it, each a whole number of clusters. `writeat /RX 0 x` refused with the
  root's second cluster's first sector as it was; `touch /SD/N`, `ls /SD`
  and `cat /RR` each an I/O error (a write and a read through a damaged
  chain, and a read of a damaged first cluster, refused where the cluster
  becomes a sector); `writeat` at LX's and HX's ends refused with the
  bitmap's last FAT entry as it was (a write there writes none of the file's
  sectors and extends from that entry, so only the link checks stop it);
  `touch /GROWN` and `touch /AFTER` grow the root to three clusters and are
  found in it; `rm /RX`, last, answers nothing and leaves the root's cluster
  used; the bitmap every byte as it was but the root's new cluster's bit;
  fsck_exfat -n clean after SD, RR, LX and HX are removed on the host;
- VUPCASE, VBITMAP, VOVERLAP and VFIRST: the root's up-case and bitmap entries
  damaged, restored on the host after the boot, VF (7 bytes) and VG
  planted. VUPCASE: the up-case table claiming the whole heap (taken as
  absent) and the bitmap 512 bytes longer than a bit a cluster, inside its
  cluster (the spec allows it, and it is kept): `echo new > /NEWB` then
  `rm /VF` both work and VF's cluster is free. VBITMAP, the bitmap a byte
  short, and VOVERLAP, the up-case table moved onto the bitmap's cluster:
  the bitmap is taken as absent and the volume is read-only, so `echo new
  > /NEWB`, `write /VF` (a truncation to nothing), `echo t > /VF`, `mv -f
  /VG /VF`, `mv /VG /VH`, `touch /VT` and `rm /VF` must each answer
  read-only, with VF one set of its 7 bytes on its cluster, VG as it was
  and no NEWB, VH or VT; and (since 2026-10-28, /bin/CCREAT) an open of VG
  with O_RDONLY|O_CREAT must read its 4 bytes while one of a missing VC is
  EROFS at the open with no VC made, as POSIX asks of O_CREAT on a
  read-only volume (fsd's `refused_read_only`), and (since the same day's
  O_EXCL fix) one of VG with O_WRONLY|O_CREAT|O_EXCL must be EEXIST, not
  EROFS, as POSIX asks of an existing file, while one of a missing VX is
  EROFS with no VX made, in every read-only boot below (VRUNS opens
  /etc/passwd, having no VG), and in VRUNS (since 2026-10-29, /bin/OPENPROBE)
  an NP_OPEN of /etc for writing is FS_ERR_NOT_A_FILE, before the read-only
  refusal, as Linux orders them. VFIRST: the bitmap's entry naming cluster 0, and a
  good copy of it planted after VG, as a volume of two FATs has FAT 2's
  bitmap entry after FAT 1's: the first is the bitmap, damaged, and the
  volume is read-only as in VBITMAP (the copy deleted on the host after
  the boot). VTWOFAT: no entry damaged, the boot sector's NumberOfFats
  made 2 (ActiveFat 0; restored after the boot): fsd cannot tell which of
  two bitmaps is live, so the volume is read-only as in VBITMAP. VLATER
  (since 2026-10-26): the up-case table claiming the heap, as in VUPCASE,
  the root given a second cluster and the bitmap's entry moved to its start
  (the first cluster's slots after the last set deleted ones): the volume
  mounts writable, and `echo new > /NEWB` and `rm /VF` work. fsck_exfat
  -n clean after each. Since 2026-10-26 too fsd's mount line must say why
  a volume is read-only (`fsd: mounted read-only: ` and the reason: the
  bitmap's entry damaged in VBITMAP and VFIRST, the bitmap and up-case
  table overlapping in VOVERLAP, more than one FAT in VTWOFAT), and must
  say nothing of it in VUPCASE and VLATER;
- VRUNS (since 2026-10-26): the root grown on the host to 65 runs of
  clusters, a free cluster between each and the next, as a root grows on a
  volume where files are made between its growths, fsck_exfat -n clean
  first: one more than fsd records (ROOT_RUNS), so the volume is
  read-only, the mount line must say `its root directory is in more than
  64 runs of clusters` and `echo new > /NEWB` answer read-only with no
  NEWB on the disk.

No fault lines in QEMU's trace. Fails with the sum capped at 19 entries (the
checksum, and fsck: "Incorrect checksum for directory entry set. Delete?")
with walk_dir taking either misshapen set (its slots written), with a rename
building its set afresh (main before 2026-10-21: the vendor entries, the
attributes and the times gone) and with the vendor entries left where the
old name ended (the set's shape, and fsck), with the Stream built afresh
(ValidDataLength raised) and with only the name entries after the Stream
dropped (STRAY's carried). The five boots of 2026-10-23 fail with each of
twenty-four mutants of the free and of the read-only volume: no secondary
freed, walk_dir not noting one, only 0xE1 freed (VE2), AllocationPossible
not read (W3), a contiguous run one cluster too long (witnesses), one bit
cleared a bitmap sector (VA1), no overlap test (W5), the overlap test by
first clusters alone (W6, W7), the survivor not consulted (SRC2's), the
survivor's runs compared by first cluster (SRC2's second vendor cluster),
the volume's own clusters freed (VSYS), the root's first cluster not
among them (VSYS), a first cluster outside the heap taken (VBAD's `rm` an
I/O error), a contiguous run past the heap freed to its end (VHEAP's), a
case-only `mv -f` freeing (va4's), a replacing `mv` freeing its source
(SRC's), the allocator handing out the bitmap's cluster (NEW), the
extents not checked (VUPCASE, VBITMAP), the bitmap held to exactly a bit
a cluster (VUPCASE), the up-case table dropped where it lies on the
bitmap (VOVERLAP), a FAT entry written past the heap (VHEAP), and the
read-only gate out, or out of `mv` alone (VBITMAP and VOVERLAP: `mv -f
/VG /VF` an I/O error, not read-only, and what is left not as planted) or of the
whole-file write alone (`write /VF` the same). They fail with each of four
mutants of the high review of 2026-10-24 too: a contiguous run linked for a
write without the heap's bound (VHEAP's own FAT entry written), a chain
tested against a contiguous run by its first cluster (SRC3's second
cluster freed), an empty Stream given a place among the survivor's runs
(DST4's cluster kept), and the scan for the bitmap taking the first entry
naming a cluster above 0 (VFIRST writable, written). And with each of five
mutants of the second high review that day: the second walk of a chain
dropped, or taken on the wrong test (VMEET's SRC chain freed), a volume of
two FATs left writable (VTWOFAT written), and a replacing `mv`'s source
or a case-only `mv -f` freed as removed (VCROSS, VMEET and VALLOC). And
with each of two of the third high review: a whole-file write freeing an
old cluster its set's vendor run names (VW's), and a contiguous run linked
over the volume's own clusters (VSYSW's `x` in the bitmap). And with each
of four of the fourth: a chained file's first cluster not tested (VSYSC's
`x` in the bitmap), a link not tested (VLINK's), a contiguous run not
tested against the volume's own clusters (the bitmap's FAT entry linked)
and the heap's fit not tested (VCROSS's VHEAP). And with each of five of
2026-10-26 (VROOT): the root's record not consulted, only its first cluster
(RX's cluster freed, the creates after the growth refused), `sector_of`
passing every cluster for a file or subdirectory (the set and the reads in
the bitmap), the root's new cluster not recorded at its growth (GROWN
refused), and `file_step`'s or `write_at`'s link check removed (the
bitmap's FAT entry linked); and with the table scan recording each extent
as it finds it, before `check_extents` (VLATER's mount an I/O error, found
by the high review that day). And with the reason line not printed (every
read-only boot), and with ROOT_RUNS raised to 65, its
const assert with it (VRUNS writable). Not graded by a boot: `sector_of`'s rule for the
root (a damaged root chain into the bitmap), the tables held off the root's
later clusters at mount, a root whose chain loops made read-only, a
growth refused when the record cannot take its cluster, and that cluster
given back then or when the root's link fails;
the order of a removal (the set deleted, then its clusters freed), of a
whole-file write (the set repointed, then the old chain freed) and of a
replacing `mv` (the destination's set deleted, then the source's, then
the destination's clusters freed), which show only on an error or a crash
between them, nor a replacing `mv` freeing the destination's clusters
when its source's delete fails; a chain tested against another whose walk
ends at its end of chain (the first walk alone answers, and VMEET's run
ends past its source's size); the shape test on the slots a removal
deletes, which now refuses before it clears any; and the bound on the scan for the bitmap and up-case entries,
which no boot gives a looped root.
About six minutes. Run it whenever fsd's mount lines or exFAT's patch_stream_ext,
set_checksum, walk_dir's set assembly, slot_at, read_slots, mv, rm, rmdir,
renamed_set, delete_set, unlink_set, free_runs, overlaps, runs_of,
free_data, is_system, sector_of, record_root, writable, check_extents,
locate_tables, ensure_chained, file_step, grow_extent, alloc_cluster or
write_fat_entry changes.
"""
import importlib.util
import os
import re
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import exfatimg  # noqa: E402


# The boots QEMU's firmware never handed to the kernel (drive_qemu.boot_stalled).
STALLED = []

def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
Volume, set_checksum, entry_set = exfatimg.Volume, exfatimg.set_checksum, exfatimg.entry_set

IMAGE = os.path.join(ROOT, "build", "test-long-set.img")

# --disk-gate (`make test-disk-gate`, since 2026-10-29): the read-only boots
# alone, on espexfat-diskgate.img, whose ESP carries fsd's `disk-gate-rig`
# build (FSD_GATE_BIN), so a write reaches exFAT's own disk gate rather than
# fsd's verb gate. Every boot must print GATE_LINE in that mode, and must not
# print it otherwise, so a boot of the wrong fsd fails either way.
DISK_GATE = "--disk-gate" in sys.argv[1:]
SOURCE = "espexfat-diskgate.img" if DISK_GATE else "espexfat.img"
GATE_LINE = "fsd: the disk-gate rig's build: the verb gate does not ask an arm"
FSD_GATE_BIN = os.path.join(ROOT, "target", "disk-gate", "aarch64-unknown-none", "release", "fsd.bin")


def gate_image():
    """build/espexfat-diskgate.img: espexfat.img's layout (scripts/mkexfat.py)
    with a copy of esp.img whose \\EFI\\ORBS\\FSD.BIN is FSD_GATE_BIN."""
    import mbrdisk
    if not os.path.exists(FSD_GATE_BIN):
        sys.exit(f"{FSD_GATE_BIN} not found - run `make fsd-disk-gate-bin` (or `make test-disk-gate`)")
    esp = os.path.join(ROOT, "build", "esp-diskgate.img")
    shutil.copyfile(os.path.join(ROOT, "build", "esp.img"), esp)
    mp = drive_qemu.attach(esp, readonly=False)
    try:
        shutil.copyfile(FSD_GATE_BIN, os.path.join(mp, "EFI", "ORBS", "FSD.BIN"))
    finally:
        drive_qemu.detach(mp)
    mbrdisk.assemble(os.path.join(ROOT, "build", SOURCE), [
        (0x07, mbrdisk.exfat_payload(os.path.join(ROOT, "build", "exfatpart.img")), False),
        (0x0B, mbrdisk.esp_payload(esp), True),
    ])
    os.remove(esp)


def gate_check(label, out):
    """The fsd this boot ran: the rig's build in --disk-gate mode, the shipped
    one otherwise."""
    said = GATE_LINE in out
    if DISK_GATE:
        return (f"{label}: fsd is the disk-gate rig's build ({GATE_LINE!r} printed)", said)
    return (f"{label}: fsd is the shipped build (no {GATE_LINE!r})", not said)
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


def write_root(vol, d):
    with open(IMAGE, "r+b") as fh:
        for k, c in enumerate(vol.chain(vol.root)):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])


def set_bit(fh, vol, k, used=True):
    """Cluster `k`'s bit in the bitmap, set (`used`) or cleared, through the
    open image `fh`."""
    at = vol.heap + (vol.bitmap - 2) * vol.cluster + (k - 2) // 8
    fh.seek(at)
    b = fh.read(1)[0]
    fh.seek(at)
    fh.write(bytes([b | 1 << (k - 2) % 8 if used else b & ~(1 << (k - 2) % 8)]))


def grow_root(vol, free=None, last=None):
    """Give the root another cluster: `free` (the first free one by default),
    zeroed, marked in the bitmap and linked after `last` (the root's last by
    default, read through `vol`, which a caller growing it again passes)."""
    if free is None:
        free = next(c for c in range(2, vol.count + 2) if not vol.used(c))
    if last is None:
        last = vol.chain(vol.root)[-1]
    with open(IMAGE, "r+b") as fh:
        set_bit(fh, vol, free)
        for c, value in ((last, free), (free, 0xFFFFFFFF)):
            fh.seek(vol.fat + c * 4)
            fh.write(struct.pack("<I", value))
        fh.seek(vol.heap + (free - 2) * vol.cluster)
        fh.write(bytes(vol.cluster))


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
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    if len(vol.chain(vol.root)) != 1:
        return [("long: the image's root is one cluster, as the rig assumes", False)], notes
    grow_root(vol)
    vol = Volume(IMAGE, start)
    d = bytearray(vol.data(vol.chain(vol.root)))
    end, at = exfatimg.root_end(d), vol.cluster - 32  # the first cluster's last slot
    s = long_set()
    d[end:at] = DELETED * ((at - end) // 32)
    d[at:at + len(s)] = s
    write_root(vol, d)
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    ok, lines = exfatimg.fsck_clean(IMAGE)
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
    if drive_qemu.boot_stalled("test-long-set", "long", out, faults):
        STALLED.append("long")
        return checks, notes
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    got = hit.data if hit else b""
    first, size = exfatimg.extent(hit)[:2] if hit else (0, 0)
    data = vol.data(vol.chain(first))[:size] if first >= 2 else b""
    ok, lines = exfatimg.fsck_clean(IMAGE)
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
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    d = bytearray(vol.data(vol.chain(vol.root)))
    end = exfatimg.root_end(d)
    label = bytes([0x83]) + bytes(31)  # a volume label primary, empty
    gappy = shaped("GAPPY", (1, DELETED), False)  # File, deleted, Stream, name
    prim = shaped("PRIM", (2, label), True)       # File, Stream, label, name
    planted = gappy + prim
    if end + len(planted) + 32 > len(d):
        return [("shape: room for the planted sets in the root", False)], []
    d[end:end + len(planted)] = planted
    write_root(vol, d)
    driven, out, faults = boot("shape", ["echo x >> /GAPPY", "echo x >> /PRIM", "ls /"])
    if drive_qemu.boot_stalled("test-long-set", "shape", out, faults):
        STALLED.append("shape")
        return [], []
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
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    d = bytearray(vol.data(vol.chain(vol.root)))
    end = exfatimg.root_end(d)
    # /VEND holds a cluster of its own, contiguous (NoFatChain), its
    # ValidDataLength 7 within a DataLength of the whole cluster, junk past
    # the 7 bytes: a rename must keep all four.
    free = next(c for c in range(2, vol.count + 2) if not vol.used(c))
    s = bytearray(long_set(stamped=True))
    s[33] = 0x03  # GeneralSecondaryFlags: AllocationPossible, NoFatChain
    struct.pack_into("<Q", s, 40, 7)
    struct.pack_into("<I", s, 52, free)
    struct.pack_into("<Q", s, 56, vol.cluster)
    struct.pack_into("<H", s, 2, set_checksum(s))
    s = bytes(s)
    # STRAY: File, Stream, a vendor entry, then its name entry. walk_dir
    # takes a name entry anywhere among the secondaries; a rename must drop
    # it with the others, not carry it after the new name.
    t = bytearray(entry_set("STRAY"))
    stray = bytes(t[:64]) + vendor_entry(99) + bytes(t[64:96])
    stray = bytearray(stray)
    stray[1] = 3
    struct.pack_into("<H", stray, 2, set_checksum(stray))
    stray = bytes(stray)
    if end + len(s) + len(stray) + 32 > len(d):
        return [("rename: room for /VEND's and STRAY's sets in the root", False)], []
    d[end:end + len(s)] = s
    d[end + len(s):end + len(s) + len(stray)] = stray
    write_root(vol, d)
    with open(IMAGE, "r+b") as fh:
        set_bit(fh, vol, free)
        fh.seek(vol.heap + (free - 2) * vol.cluster)
        fh.write(b"hello\r\n" + b"\xEE" * (vol.cluster - 7))
    vol = Volume(IMAGE, start)
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), "VEND")
    # STRAY's shape is fsck's one complaint before the boot (the rename
    # writes it in the spec's shape, so none after).
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
    ok = exfatimg.complaints(lines) == ["   Invalid file name in /."] and not exfatimg.unexpected(lines)
    checks = [
        ("rename: /VEND's stamped set reads back, 20 entries, its checksum whole, its cluster used",
         hit is not None and hit.count == 20 and hit.data == s and s[4] == 0x20 and vol.used(free)
         and struct.unpack_from("<H", s, 2)[0] == set_checksum(s)),
        ("rename: fsck_exfat -n names only STRAY's misplaced name before the boot", ok),
    ]
    notes = [f"rename before: {ln}" for ln in lines] if not ok else []
    if not all(c for _, c in checks):
        return checks, notes
    cmds = [f"mv /VEND /{LONG_NAME}", "ls /", f"mv /{LONG_NAME} /V2", "ls /", "mv /STRAY /S2"]
    driven, out, faults = boot("rename", cmds)
    if drive_qemu.boot_stalled("test-long-set", "rename", out, faults):
        STALLED.append("rename")
        return checks, notes
    listings = [blk.split("# ", 1)[0].split() for blk in out.split("# ls /")[1:3]]
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    hit = exfatimg.find(root, "V2")
    got = hit.data if hit else b""
    s2 = exfatimg.find(root, "S2")
    s2_types = [s2.data[k] for k in range(0, len(s2.data), 32)] if s2 else []
    ok, lines = exfatimg.fsck_clean(IMAGE)
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
        (f"rename: its Stream's flags, ValidDataLength, cluster and DataLength as planted "
         f"({got[33:34].hex()} {got[40:48].hex()} {got[52:64].hex()})",
         got[33] == s[33] and got[40:48] == s[40:48] and got[52:64] == s[52:64]),
        ("rename: its Stream's NameLength is V2's", len(got) > 35 and got[35] == 2),
        (f"rename: STRAY renamed to S2 is File, Stream, the new name, its vendor entry ({[hex(t) for t in s2_types]})",
         s2_types == [0x85, 0xC0, 0xC1, 0xE0] and s2.data[96:] == vendor_entry(99)
         and exfatimg.find(root, "STRAY") is None),
        ("rename: its SetChecksum is the sum of all 20 entries",
         hit is not None and struct.unpack_from("<H", got, 2)[0] == set_checksum(got)),
        ("rename: fsck_exfat -n passes the image after the boot", ok),
        ("rename: no fault lines", faults == 0),
    ]
    if not ok:
        notes += [f"rename after: {ln}" for ln in lines]
    return checks, notes


def secondary(kind, first, size, flags):
    """A benign secondary of type `kind` (0xE1 a vendor allocation, 0xE2 one
    the spec leaves undefined) naming `size` bytes from cluster `first`, its
    GeneralSecondaryFlags `flags` (0x01 AllocationPossible, 0x02 NoFatChain)."""
    e = bytearray(32)
    e[0], e[1] = kind, flags
    e[2:18] = GUID
    struct.pack_into("<I", e, 20, first)
    struct.pack_into("<Q", e, 24, size)
    return bytes(e)


def plant_set(name, first=0, size=0, extra=(), is_dir=False, contiguous=True):
    """`name`'s set, its data `size` bytes from `first` (contiguous, or
    FAT-chained for a directory fsd made), with the secondaries `extra` after
    its name, sealed."""
    s = bytearray(entry_set(name, first, size))
    if size and contiguous:
        s[33] = 0x03
    if is_dir:
        struct.pack_into("<H", s, 4, 0x10)
    s += b"".join(extra)
    s[1] = len(s) // 32 - 1
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def free_run(vol, n):
    """The first of `n` free clusters in a run, the heap's last among them."""
    return next((c for c in range(2, vol.count + 3 - n) if not any(vol.used(c + k) for k in range(n))), None)


def plant(vol, sets, used, links=(), fills=()):
    """Write `sets` after the root's last, mark the clusters `used` in the
    bitmap, write the FAT `links` (cluster, value) and the cluster `fills`
    (cluster, bytes, zero-padded). False when the root has no room."""
    d = bytearray(vol.data(vol.chain(vol.root)))
    end = exfatimg.root_end(d)
    planted = b"".join(sets)
    if end + len(planted) + 32 > len(d):
        return False
    d[end:end + len(planted)] = planted
    write_root(vol, d)
    c = vol.cluster
    with open(IMAGE, "r+b") as fh:
        for k in used:
            set_bit(fh, vol, k)
        for at, value in links:
            fh.seek(vol.fat + at * 4)
            fh.write(struct.pack("<I", value))
        for at, fill in fills:
            fh.seek(vol.heap + (at - 2) * c)
            fh.write(fill + bytes(c - len(fill)))
    return True


def upcase_cluster(vol):
    """The up-case table's first cluster, from the root's 0x82 entry, or None."""
    d = vol.data(vol.chain(vol.root))
    return next((struct.unpack_from("<I", d, i + 20)[0] for i in range(0, len(d), 32) if d[i] == 0x82), None)


def printed(out, cmd):
    """What `cmd` printed, the kernel's own lines left out."""
    return [ln.strip() for ln in out.split("# " + cmd, 1)[-1].split("# ", 1)[0].splitlines()[1:]
            if ln.strip() and not ln.startswith("Ouroboros kernel:")]


def run_valloc():
    """Allocations a set's benign secondaries name, freed with the set and
    only then, on a volume fsck_exfat passes. Fifteen clusters in a run, each
    contiguous run followed by one that must stay used."""
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    r = free_run(vol, 15)
    if r is None:
        return [("valloc: fifteen free clusters in a run on the image", False)], []
    c = vol.cluster
    E1, E2 = 0xE1, 0xE2
    sets = {
        "VA1": plant_set("VA1", r, 7, [secondary(E1, r + 1, 2 * c, 0x03)]),      # rm
        "W": plant_set("W", r + 3, c),                                           # witness
        "VA2": plant_set("VA2", extra=[secondary(E1, r + 4, 2 * c - 100, 0x01)]),  # replaced
        "VA3": plant_set("VA3", extra=[secondary(E1, r + 6, 1, 0x03)]),          # renamed twice
        "VD": plant_set("VD", r + 7, c, [secondary(E1, r + 8, c, 0x03)], is_dir=True, contiguous=False),
        "SRC": plant_set("SRC", r + 9, 5, [secondary(E1, r + 10, c, 0x03)]),     # mv -f source
        "VE2": plant_set("VE2", extra=[secondary(E2, r + 13, c, 0x03)]),         # undefined type
        "VE0": plant_set("VE0", extra=[secondary(0xE0, r + 14, c, 0x00)]),       # no allocation
        "W3": plant_set("W3", r + 14, c),                                        # witness
        "W2": plant_set("W2", r + 11, 2 * c),                                    # witness
    }
    ok_room = plant(vol, list(sets.values()), range(r, r + 15),
                    links=[(r + 4, r + 5), (r + 5, 0xFFFFFFFF), (r + 7, 0xFFFFFFFF)],
                    fills=[(r, b"hello\r\n"), (r + 7, b""), (r + 9, b"src\r\n")])
    if not ok_room:
        return [("valloc: room for the planted sets in the root", False)], []
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    ok, lines = exfatimg.fsck_clean(IMAGE)
    checks = [
        ("valloc: every planted set reads back, all fifteen clusters used",
         all(exfatimg.find(root, n) is not None and exfatimg.find(root, n).data == p for n, p in sets.items())
         and all(vol.used(r + k) for k in range(15))),
        ("valloc: fsck_exfat -n passes the image before the boot", ok),
    ]
    notes = [f"valloc before: {ln}" for ln in lines] if not ok else []
    if not all(ok for _, ok in checks):
        return checks, notes
    cmds = ["mv -f /SRC /VA2", "cat /VA2", "rm /VA1", "mv /VA3 /VA4", "mv -f /VA4 /va4",
            "rmdir /VD", "rm /VE2", "rm /VE0", "ls /"]
    driven, out, faults = boot("valloc", cmds)
    if drive_qemu.boot_stalled("test-long-set", "valloc", out, faults):
        STALLED.append("valloc")
        return checks, notes
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    gone = [n for n in ("VA1", "VD", "SRC", "VE2", "VE0") if exfatimg.find(root, n) is None]
    va2, va4 = exfatimg.find(root, "VA2"), exfatimg.find(root, "va4")
    in_use = {k: vol.used(k) for k in range(r, r + 15)}
    ok, lines = exfatimg.fsck_clean(IMAGE)

    def uses(*ks):
        return [in_use[k] for k in ks]
    checks += [
        ("valloc: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"valloc: VA1, VD, SRC, VE2 and VE0 gone from the root ({gone})", len(gone) == 5),
        ("valloc: every mv, rm and rmdir answered nothing (no error)",
         all(printed(out, k) == [] for k in cmds if k.split()[0] in ("mv", "rm", "rmdir"))),
        (f"valloc: `cat /VA2` prints SRC's src ({printed(out, 'cat /VA2')})", printed(out, "cat /VA2") == ["src"]),
        (f"valloc: rm /VA1 freed its data cluster and its vendor's two ({uses(r, r + 1, r + 2)})",
         not any(uses(r, r + 1, r + 2))),
        (f"valloc: the mv over /VA2 freed its old vendor chain, bitmap and FAT "
         f"({uses(r + 4, r + 5)}, {[vol.entry(k) for k in (r + 4, r + 5)]})",
         not any(uses(r + 4, r + 5)) and vol.entry(r + 4) == 0 and vol.entry(r + 5) == 0),
        (f"valloc: ... and kept SRC's data and vendor cluster, VA2's set now naming both ({uses(r + 9, r + 10)})",
         all(uses(r + 9, r + 10)) and va2 is not None and va2.data[-32:] == sets["SRC"][-32:]),
        (f"valloc: rmdir /VD freed its cluster and its vendor's ({uses(r + 7, r + 8)})", not any(uses(r + 7, r + 8))),
        (f"valloc: rm /VE2 freed what its undefined benign secondary names ({uses(r + 13)})", not in_use[r + 13]),
        ("valloc: two renames, the second by case alone through mv -f, kept VA3's vendor cluster, "
         "va4's set carrying its entry as planted",
         in_use[r + 6] and va4 is not None and va4.name == "va4" and va4.data[-32:] == sets["VA3"][-32:]),
        (f"valloc: every witness cluster still used: W after VA1's run, W2, W3 under VE0's fields, "
         f"SRC's after VD's ({uses(r + 3, r + 11, r + 12, r + 14, r + 9)})",
         all(uses(r + 3, r + 11, r + 12, r + 14, r + 9))),
        ("valloc: fsck_exfat -n passes the image after the boot", ok),
        ("valloc: no fault lines", faults == 0),
    ]
    if not ok:
        notes += [f"valloc after: {ln}" for ln in lines]
    return checks, notes


def run_vcross():
    """Damaged sets, each removal of which must free only what no live set or
    the volume needs, and leave the volume fsck clean: allocations two sets
    share, a run reaching another's at a later cluster, ones naming no
    cluster of the heap or the volume's own, a run past the heap, a run over
    a later cluster of a surviving chain, and an empty survivor with eight
    vendor runs."""
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    r = free_run(vol, 20)
    if r is None:
        return [("vcross: twenty free clusters in a run on the image", False)], []
    last = vol.count + 1  # the heap's last cluster
    if vol.used(last) or r + 20 > last:
        return [("vcross: the heap's last cluster free, after the run", False)], []
    c = vol.cluster
    up = upcase_cluster(vol)
    if up is None:
        return [("vcross: the image's root names its up-case table", False)], []
    E1 = 0xE1
    sets = {
        # SRC2 and DST2 share data, each its own size; SRC2's vendor run is
        # two clusters, and DST2's vendor entry names the second, sharing
        # SRC2's run at a cluster past its first.
        "SRC2": plant_set("SRC2", r, 5, [secondary(E1, r + 1, 2 * c, 0x03)]),
        "DST2": plant_set("DST2", r, 3, [secondary(E1, r + 2, c, 0x03)]),
        # VA5's vendor run starts at its own data and runs into W5's cluster.
        "VA5": plant_set("VA5", r + 3, 7, [secondary(E1, r + 3, 2 * c, 0x03)]),
        "W5": plant_set("W5", r + 4, c),
        # Allocations outside the heap, and on the bitmap, the up-case table
        # and the root's first cluster.
        "VBAD": plant_set("VBAD", extra=[secondary(E1, 0xFFFFFFF0, c, 0x01)]),
        "VSYS": plant_set("VSYS", extra=[secondary(E1, vol.bitmap, c, 0x03), secondary(E1, up, c, 0x01),
                                         secondary(E1, vol.root, c, 0x01)]),
        # VA6's vendor run starts a cluster before its data and ends a
        # cluster after: W6's and W7's, the first clusters differing.
        "W6": plant_set("W6", r + 5, c),
        "VA6": plant_set("VA6", r + 6, 7, [secondary(E1, r + 5, 3 * c, 0x03)]),
        "W7": plant_set("W7", r + 7, c),
        # VHEAP: contiguous, on the heap's last cluster, sized three clusters,
        # two past the heap: a write chains it first (ensure_chained).
        "VHEAP": plant_set("VHEAP", last, 3 * c),
        # SRC3 FAT-chained, r + 8 then r + 10; DST3 on r + 9, its vendor run
        # on r + 10 alone, a later cluster of SRC3's chain, not its first.
        "SRC3": plant_set("SRC3", r + 8, 2 * c, contiguous=False),
        "DST3": plant_set("DST3", r + 9, 4, [secondary(E1, r + 10, c, 0x03)]),
        # SRC4 empty, naming no cluster, with eight vendor runs, r + 11 to
        # r + 18; DST4 on r + 19.
        "SRC4": plant_set("SRC4", extra=[secondary(E1, r + 11 + k, c, 0x03) for k in range(8)]),
        "DST4": plant_set("DST4", r + 19, 4),
    }
    if not plant(vol, list(sets.values()), list(range(r, r + 20)) + [last],
                 links=[(r + 8, r + 10), (r + 10, 0xFFFFFFFF)],
                 fills=[(r, b"two\r\n"), (r + 3, b"hello\r\n"), (r + 6, b"hello\r\n"), (r + 8, b"three\r\n")]):
        return [("vcross: room for the planted sets in the root", False)], []
    with open(IMAGE, "r+b") as fh:
        # The bitmap's own bit cleared, as main's rm of a set naming it left it.
        set_bit(fh, vol, vol.bitmap, used=False)
        fh.seek(vol.fat + last * 4)
        own = fh.read(4)  # VHEAP's FAT entry, the one in the heap
        past = fh.read(8)  # the FAT's bytes for the two clusters past the heap
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    checks = [("vcross: every planted set reads back, its clusters and the up-case table's used, the bitmap's clear",
               all(exfatimg.find(root, n) is not None and exfatimg.find(root, n).data == p for n, p in sets.items())
               and all(vol.used(r + k) for k in range(20)) and vol.used(last) and not vol.used(vol.bitmap)
               and vol.used(up) and sets["SRC2"] != sets["DST2"])]
    if not checks[0][1]:
        return checks, []
    cmds = ["echo new > /NEW", "mv -f /SRC2 /DST2", "cat /DST2", "rm /VA5", "rm /VBAD", "rm /VSYS", "rm /VA6",
            "writeat /VHEAP 0 x", "rm /VHEAP", "mv -f /SRC3 /DST3", "mv -f /SRC4 /DST4", "ls /"]
    driven, out, faults = boot("vcross", cmds)
    if drive_qemu.boot_stalled("test-long-set", "vcross", out, faults):
        STALLED.append("vcross")
        return checks, []
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    gone = [n for n in ("SRC2", "VA5", "VBAD", "VSYS", "VA6", "VHEAP", "SRC3", "SRC4") if exfatimg.find(root, n) is None]
    dst2, new = exfatimg.find(root, "DST2"), exfatimg.find(root, "NEW")
    dst3, dst4 = exfatimg.find(root, "DST3"), exfatimg.find(root, "DST4")
    new_first = struct.unpack_from("<I", new.data, 52)[0] if new else 0
    heap_kept = vol.used(last)
    with open(IMAGE, "r+b") as fh:
        fh.seek(vol.fat + last * 4)
        own_after = fh.read(4)
        past_after = fh.read(8)
        # VHEAP's run, past the heap, is damage the free leaves whole: its
        # one cluster in the heap leaked on purpose, given back here so
        # fsck_exfat reads the rest of the volume.
        set_bit(fh, vol, last, used=False)
    vol = Volume(IMAGE, start)
    ok, lines = exfatimg.fsck_clean(IMAGE)
    checks += [
        ("vcross: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"vcross: SRC2, VA5, VBAD, VSYS, VA6, VHEAP, SRC3 and SRC4 gone from the root ({gone})", len(gone) == 8),
        (f"vcross: every mv and rm answered nothing, VBAD's outside the heap too ({printed(out, 'rm /VBAD')})",
         all(printed(out, k) == [] for k in cmds if k.split()[0] in ("mv", "rm"))),
        (f"vcross: the allocation passed over the bitmap's clear bit, setting it (NEW at {new_first}, "
         f"the bitmap at {vol.bitmap})",
         new is not None and new_first >= 2 and new_first != vol.bitmap and vol.used(vol.bitmap)),
        (f"vcross: rm /VA6 freed its data, not W6's or W7's, which its vendor run covers "
         f"({vol.used(r + 5)}, {vol.used(r + 6)}, {vol.used(r + 7)})",
         vol.used(r + 5) and not vol.used(r + 6) and vol.used(r + 7)),
        (f"vcross: `writeat /VHEAP 0 x` answered an error and wrote no FAT entry, its own or past the heap "
         f"({printed(out, 'writeat /VHEAP 0 x')}, {(own + past).hex()} -> {(own_after + past_after).hex()})",
         printed(out, "writeat /VHEAP 0 x") != [] and past_after == past and own_after == own),
        (f"vcross: rm /VHEAP left its cluster used, its run past the heap freeing none of it ({heap_kept})", heap_kept),
        (f"vcross: `cat /DST2` prints two ({printed(out, 'cat /DST2')})", printed(out, "cat /DST2") == ["two"]),
        (f"vcross: the mv over DST2 kept the data and the vendor run SRC2 shares with it, the second cluster "
         f"too ({vol.used(r)}, {vol.used(r + 1)}, {vol.used(r + 2)}), DST2's set SRC2's",
         vol.used(r) and vol.used(r + 1) and vol.used(r + 2) and dst2 is not None
         and dst2.data[33] == sets["SRC2"][33] and dst2.data[40:64] == sets["SRC2"][40:64]
         and dst2.data[-32:] == sets["SRC2"][-32:]),
        (f"vcross: rm /VA5 freed its data, not W5's cluster its vendor run reaches ({vol.used(r + 3)}, {vol.used(r + 4)})",
         not vol.used(r + 3) and vol.used(r + 4)),
        (f"vcross: rm /VSYS left the bitmap's, the up-case table's and the root's clusters used "
         f"({vol.used(vol.bitmap)}, {vol.used(up)}, {vol.used(vol.root)})",
         vol.used(vol.bitmap) and vol.used(up) and vol.used(vol.root)),
        (f"vcross: the mv over DST3 freed DST3's cluster and kept SRC3's chain, its second cluster under "
         f"DST3's vendor run ({vol.used(r + 9)}, {vol.used(r + 8)}, {vol.used(r + 10)}), DST3's set SRC3's",
         not vol.used(r + 9) and vol.used(r + 8) and vol.used(r + 10)
         and dst3 is not None and dst3.data[40:64] == sets["SRC3"][40:64]),
        (f"vcross: the mv of the empty SRC4, with eight vendor runs, over DST4 freed DST4's cluster and kept "
         f"the eight ({vol.used(r + 19)}, {[vol.used(r + 11 + k) for k in range(8)]})",
         not vol.used(r + 19) and all(vol.used(r + 11 + k) for k in range(8))
         and dst4 is not None and dst4.data[-8 * 32:] == sets["SRC4"][-8 * 32:]),
        ("vcross: fsck_exfat -n passes the image after the boot, VHEAP's cluster given back", ok),
        ("vcross: no fault lines", faults == 0),
    ]
    notes = [f"vcross after: {ln}" for ln in lines] if not ok else []
    return checks, notes


def run_vmeet():
    """A replacing `mv` whose destination's FAT-chained vendor run meets the
    source's chain: the run held for the survivor, though the survivor's
    walk, cut at its size, stops short of the run's last cluster. And two
    writes to damaged sets: VW, whose vendor run lies on its own data, given
    new data by a whole-file write (its old cluster kept for the run), and
    VSYSW, a contiguous file on the bitmap's cluster, VSYSC, a FAT-chained
    one, and VLINK, a FAT-chained file whose link leads into the bitmap,
    each refused a `writeat` with nothing written. A boot of its own:
    VCROSS's root has no room."""
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    r = free_run(vol, 7)
    if r is None:
        return [("vmeet: seven free clusters in a run on the image", False)], []
    c = vol.cluster
    sets = {
        # SRC FAT-chained r, r + 1, r + 2 but sized two clusters; DST on
        # r + 3, its vendor run FAT-chained r + 4, r + 1, r + 2, three
        # clusters to its end of chain. The chains meet at r + 1, inside
        # SRC's size, and SRC's walk stops before the run's last cluster, so
        # the meeting is found only by walking the run again for SRC's last.
        "SRC": plant_set("SRC", r, 2 * c, contiguous=False),
        "DST": plant_set("DST", r + 3, 4, [secondary(0xE1, r + 4, 3 * c, 0x01)]),
        # VW on r + 5, its vendor run the same cluster.
        "VW": plant_set("VW", r + 5, 7, [secondary(0xE1, r + 5, c, 0x03)]),
        # VSYSW contiguous from the bitmap's first cluster, two clusters
        # long: a run linked before its refusal writes the bitmap's own FAT
        # entry, which a run of one leaves end of chain as it was.
        "VSYSW": plant_set("VSYSW", vol.bitmap, 2 * c),
        # VSYSC on the bitmap's cluster, FAT-chained, one cluster; VLINK FAT-chained on r + 6, its link
        # to the bitmap's first cluster, two clusters long.
        "VSYSC": plant_set("VSYSC", vol.bitmap, 4, contiguous=False),
        "VLINK": plant_set("VLINK", r + 6, 2 * c, contiguous=False),
    }
    if not plant(vol, list(sets.values()), range(r, r + 7),
                 links=[(r, r + 1), (r + 1, r + 2), (r + 2, 0xFFFFFFFF), (r + 4, r + 1), (r + 6, vol.bitmap)],
                 fills=[(r, b"src\r\n"), (r + 5, b"hello\r\n")]):
        return [("vmeet: room for the planted sets in the root", False)], []
    nb = -(-vol.count // 8 // c) or 1  # the bitmap's clusters
    with open(IMAGE, "rb") as fh:
        fh.seek(vol.fat + vol.bitmap * 4)
        bitmap_fat = fh.read(4 * nb)
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    checks = [("vmeet: the six planted sets read back, their clusters used",
               all(exfatimg.find(root, n) is not None and exfatimg.find(root, n).data == p for n, p in sets.items())
               and all(vol.used(r + k) for k in range(7)))]
    if not checks[0][1]:
        return checks, []
    cmds = ["mv -f /SRC /DST", "write /VW new", "writeat /VSYSW 0 x", "rm /VSYSW", "writeat /VSYSC 0 x",
            "rm /VSYSC", f"writeat /VLINK {c} x", "rm /VLINK", "ls /"]
    driven, out, faults = boot("vmeet", cmds)
    if drive_qemu.boot_stalled("test-long-set", "vmeet", out, faults):
        STALLED.append("vmeet")
        return checks, []
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    dst, vw = exfatimg.find(root, "DST"), exfatimg.find(root, "VW")
    used = [vol.used(r + k) for k in range(5)]
    vw_first = struct.unpack_from("<I", vw.data, 52)[0] if vw else 0
    vw_kept = vol.used(r + 5)
    bitmap_byte = vol.data([vol.bitmap])[0]
    with open(IMAGE, "rb") as fh:
        fh.seek(vol.fat + vol.bitmap * 4)
        bitmap_fat_after = fh.read(4 * nb)
    with open(IMAGE, "r+b") as fh:
        # The vendor run, held for SRC's chain it meets, leaves its own first
        # cluster leaked, and SRC's chain runs past its size: both damage
        # planted, mended here so fsck_exfat reads the rest of the volume.
        for k, value in ((4, 0), (2, 0), (1, 0xFFFFFFFF)):
            if k != 1:
                set_bit(fh, vol, r + k, used=False)
            fh.seek(vol.fat + (r + k) * 4)
            fh.write(struct.pack("<I", value))
    ok, lines = exfatimg.fsck_clean(IMAGE)
    checks += [
        ("vmeet: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"vmeet: `mv -f /SRC /DST` answered nothing ({printed(out, 'mv -f /SRC /DST')})",
         printed(out, "mv -f /SRC /DST") == []),
        (f"vmeet: SRC gone, DST's set SRC's, DST's cluster freed and SRC's chain and the vendor run meeting it "
         f"kept (r to r + 4: {used})",
         exfatimg.find(root, "SRC") is None and dst is not None and dst.data[40:64] == sets["SRC"][40:64]
         and used == [True, True, True, False, True]),
        (f"vmeet: `write /VW new` gave VW a new cluster and kept its old one, which its vendor run names "
         f"(VW at {vw_first}, r + 5 used {vw_kept}; {printed(out, 'write /VW new')})",
         vw is not None and vw_first not in (0, r + 5) and vw_kept and printed(out, "write /VW new") == []),
        (f"vmeet: `writeat /VSYSW 0 x` on the bitmap's cluster refused, its FAT entries and first byte as they "
         f"were ({printed(out, 'writeat /VSYSW 0 x')}, {bitmap_fat.hex()} -> {bitmap_fat_after.hex()}, "
         f"{bitmap_byte:#x}), and `rm /VSYSW` answered nothing ({printed(out, 'rm /VSYSW')})",
         printed(out, "writeat /VSYSW 0 x") != [] and bitmap_fat_after == bitmap_fat and bitmap_byte != ord("x")
         and printed(out, "rm /VSYSW") == [] and exfatimg.find(root, "VSYSW") is None),
        (f"vmeet: the chained VSYSC on the bitmap's cluster and VLINK's link into it refused their writeat "
         f"({printed(out, 'writeat /VSYSC 0 x')}, {printed(out, f'writeat /VLINK {c} x')}), the bitmap's first "
         f"byte {bitmap_byte:#x}, both removed ({printed(out, 'rm /VSYSC')}, {printed(out, 'rm /VLINK')}), "
         f"VLINK's own cluster freed ({vol.used(r + 6)})",
         printed(out, "writeat /VSYSC 0 x") != [] and printed(out, f"writeat /VLINK {c} x") != []
         and bitmap_byte != ord("x") and printed(out, "rm /VSYSC") == [] and printed(out, "rm /VLINK") == []
         and exfatimg.find(root, "VSYSC") is None and exfatimg.find(root, "VLINK") is None
         and not vol.used(r + 6)),
        ("vmeet: fsck_exfat -n passes the image after the boot, the leak and the long chain mended", ok),
        ("vmeet: no fault lines", faults == 0),
    ]
    return checks, [f"vmeet after: {ln}" for ln in lines] if not ok else []


def filler(n, prefix):
    """Empty files' sets filling exactly `n` slots (at least 3): names of
    three slots (`prefix` and three digits), and one of four (sixteen
    characters, two name entries) or five (thirty-one, three) for the
    remainder. Four-slot names alone gave 4 or 8 slots for 1, 2 or 5 (the
    high review of 2026-10-26)."""
    if n < 3:
        raise ValueError(f"filler: {n} slots hold no set")
    tail = {0: [], 1: [entry_set(f"{prefix}LongFiller000xx")], 2: [entry_set(f"{prefix}{'L' * 27}000")]}[n % 3]
    b = (n - sum(len(t) for t in tail) // 32) // 3
    return b"".join([entry_set(f"{prefix}{k:03d}") for k in range(b)] + tail)


def fill_deleted(vol):
    """Every run of deleted slots before the root's end given empty files'
    sets of three slots, so no create fits in one: the image's root has 34
    such slots, which took the creates meant to grow it."""
    d = bytearray(vol.data(vol.chain(vol.root)))
    end, i, k = exfatimg.root_end(d), 0, 0
    while i < end:
        n = 0
        while i + 32 * n < end and d[i + 32 * n] & 0x80 == 0:
            n += 1
        for j in range(n // 3):
            d[i + 96 * j:i + 96 * (j + 1)] = entry_set(f"G{k:03d}")
            k += 1
        i += 32 * max(n, 1)
    write_root(vol, d)


def delete_set(vol, name):
    """`name`'s set in the root deleted on the host: each entry's in-use bit
    cleared."""
    hit = exfatimg.find(vol.data(vol.chain(vol.root)), name)
    if hit:
        patch_root(vol, hit.offset, bytes(b & 0x7F if i % 32 == 0 else b for i, b in enumerate(hit.data)))


def run_vroot():
    """The root's later clusters among the volume's own, and a read or write
    reaching one of the volume's own clusters through a damaged set or chain,
    refused where the cluster becomes a sector (`sector_of`, since
    2026-10-26). A boot of its own: the root is filled to its last two slots."""
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    if len(vol.chain(vol.root)) != 1:
        return [("vroot: the image's root is one cluster, as the rig assumes", False)], []
    grow_root(vol)
    vol = Volume(IMAGE, start)
    fill_deleted(vol)
    r2 = vol.chain(vol.root)[1]
    d = free_run(vol, 2)
    e = d + 1
    c = vol.cluster
    nb = -(-vol.count // 8 // c) or 1  # the bitmap's clusters
    lb = vol.bitmap + nb - 1  # its last, whose FAT entry ends a chain
    sets = {
        # RX contiguous on the root's second cluster, one cluster long.
        "RX": plant_set("RX", r2, c),
        # SD a directory of two clusters, FAT-chained: its own, every slot
        # in use, then the bitmap's last; a create in it scans into that.
        "SD": plant_set("SD", d, 2 * c, is_dir=True, contiguous=False),
        # RR FAT-chained on the bitmap's last cluster, 4 bytes.
        "RR": plant_set("RR", lb, 4, contiguous=False),
        # LX FAT-chained on a cluster of its own linked to the bitmap's last,
        # and HX on the bitmap's last, each whole: a write at its end writes
        # none of its sectors, and extends the chain from the bitmap's FAT
        # entry unless the link (`file_step`) or the first cluster
        # (`write_at`) is refused.
        "LX": plant_set("LX", e, 2 * c, contiguous=False),
        "HX": plant_set("HX", lb, c, contiguous=False),
    }
    root = vol.data(vol.chain(vol.root))
    free = (len(root) - exfatimg.root_end(root) - sum(len(x) for x in sets.values())) // 32
    # Two slots left: a create needs three, so `touch /GROWN` grows the root.
    if free < 5 or not plant(vol, [*sets.values(), filler(free - 2, "F")], [d, e],
                             links=[(d, lb), (e, lb)], fills=[(d, filler(c // 32, "D"))]):
        return [("vroot: room for the planted sets in the root", False)], []
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    bitmap = vol.data(range(vol.bitmap, vol.bitmap + nb))
    lb_fat = vol.entry(lb)
    r2_head = vol.data([r2])[:512]
    runs = [i for i in range(0, exfatimg.root_end(root) - 64, 32) if all(root[i + 32 * j] & 0x80 == 0 for j in range(3))]
    checks = [("vroot: the planted sets read back, the root two clusters with two slots free and no run of three "
               f"deleted ones ({runs})",
               all(exfatimg.find(root, n) is not None and exfatimg.find(root, n).data == p for n, p in sets.items())
               and exfatimg.root_end(root) == len(root) - 64 and not runs and all(vol.used(k) and vol.entry(k) == lb
                                                                        for k in (d, e)))]
    if not checks[0][1]:
        return checks, []
    # `rm /RX` last: the three slots it frees would take a create.
    cmds = ["writeat /RX 0 x", "touch /SD/N", "ls /SD", "cat /RR", f"writeat /LX {2 * c} x", f"writeat /HX {c} x",
            "touch /GROWN", "touch /AFTER", "rm /RX"]
    driven, out, faults = boot("vroot", cmds)
    if drive_qemu.boot_stalled("test-long-set", "vroot", out, faults):
        STALLED.append("vroot")
        return checks, []
    vol = Volume(IMAGE, start)
    chain = vol.chain(vol.root)
    root = vol.data(chain)
    # The bitmap as it was, but for the root's new cluster.
    want = bytearray(bitmap)
    if len(chain) == 3:
        want[(chain[2] - 2) // 8] |= 1 << (chain[2] - 2) % 8
    bitmap_after = vol.data(range(vol.bitmap, vol.bitmap + nb))
    r2_after = vol.data([r2])[:512]
    errs = {cmd: any("error" in ln for ln in printed(out, cmd)) for cmd in ("touch /SD/N", "ls /SD", "cat /RR")}
    checks += [
        ("vroot: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"vroot: `writeat /RX 0 x` on the root's second cluster refused, its first sector as it was "
         f"({printed(out, 'writeat /RX 0 x')}), and `rm /RX` answered nothing and left the cluster used "
         f"({printed(out, 'rm /RX')}, {vol.used(r2)})",
         printed(out, "writeat /RX 0 x") != [] and r2_after == r2_head and printed(out, "rm /RX") == []
         and exfatimg.find(root, "RX") is None and vol.used(r2)),
        (f"vroot: `touch /SD/N`, `ls /SD` and `cat /RR`, through SD's link into the bitmap and RR's first "
         f"cluster on it, each answered an I/O error ({errs})", all(errs.values())),
        (f"vroot: `touch /GROWN` and `touch /AFTER` answered nothing, the root grown to three clusters and "
         f"both in it ({printed(out, 'touch /GROWN')}, {printed(out, 'touch /AFTER')}, {len(chain)} clusters)",
         printed(out, "touch /GROWN") == [] and printed(out, "touch /AFTER") == [] and len(chain) == 3
         and exfatimg.find(root, "GROWN") is not None and exfatimg.find(root, "AFTER") is not None),
        (f"vroot: `writeat /LX {2 * c} x` and `writeat /HX {c} x` refused, the bitmap's last cluster's FAT entry "
         f"as it was ({printed(out, f'writeat /LX {2 * c} x')}, {printed(out, f'writeat /HX {c} x')}, "
         f"{lb_fat:#x} -> {vol.entry(lb):#x})",
         printed(out, f"writeat /LX {2 * c} x") != [] and printed(out, f"writeat /HX {c} x") != []
         and vol.entry(lb) == lb_fat),
        ("vroot: the bitmap every byte as it was but the root's new cluster's bit", bytes(want) == bitmap_after),
        ("vroot: no fault lines", faults == 0),
    ]
    # SD and RR are damage planted: deleted here, SD's cluster given back,
    # so fsck_exfat reads the rest of the volume.
    for name in ("SD", "RR", "LX", "HX"):
        delete_set(vol, name)
    with open(IMAGE, "r+b") as fh:
        for k in (d, e):
            set_bit(fh, vol, k, used=False)
            fh.seek(vol.fat + k * 4)
            fh.write(struct.pack("<I", 0))
    ok, lines = exfatimg.fsck_clean(IMAGE)
    checks.append(("vroot: fsck_exfat -n passes the image after the boot, SD, RR, LX and HX removed", ok))
    return checks, [f"vroot after: {ln}" for ln in lines] if not ok else []


# The damaged extents of the last five boots (VTWOFAT's none, its boot
# sector changed instead): (the root's entry type, the
# field's offset in it, its value from the good one) for each, whether the
# volume must still be writable, and whether a good copy of the bitmap's
# entry is planted after the damaged one.
EXTENTS = {
    # The up-case table claiming the whole heap, and the bitmap 512 bytes
    # longer than a bit a cluster, inside its cluster (the spec allows it).
    "vupcase": ([(0x82, 24, lambda good, vol: vol.count * vol.cluster),
                 (0x81, 24, lambda good, vol: good + 512)], True, False),
    # The bitmap a byte short of a bit a cluster.
    "vbitmap": ([(0x81, 24, lambda good, vol: good - 1)], False, False),
    # The up-case table moved onto the bitmap's cluster.
    "voverlap": ([(0x82, 20, lambda good, vol: vol.bitmap)], False, False),
    # The bitmap's entry naming cluster 0, a good one after it: the first is
    # the bitmap, damaged, as a volume of two FATs has FAT 2's after it.
    "vfirst": ([(0x81, 20, lambda good, vol: 0)], False, True),
    # No entry damaged: the boot sector's NumberOfFats made 2 (VBR below).
    "vtwofat": ([], False, False),
    # The up-case table claiming the whole heap, as in VUPCASE, and the
    # bitmap's entry at the start of the root's second cluster (LATE): the
    # scan reads that cluster with the damaged table found, and must not
    # take it for the table's before `check_extents` drops it.
    "vlater": ([(0x82, 24, lambda good, vol: vol.count * vol.cluster)], True, False),
}

# The boots whose root is given a second cluster and its bitmap entry moved
# to its start, the first cluster's slots after the last set deleted ones,
# so the scan reaches it (the high review of 2026-10-26: the mount failed).
LATE = {"vlater"}

# What fsd's mount line must give as the reason a read-only boot of
# EXTENTS is read-only (`fsd: mounted read-only: ...`, since 2026-10-26).
REASONS = {
    "vbitmap": "its allocation bitmap's entry is damaged",
    "voverlap": "its allocation bitmap and up-case table overlap",
    "vfirst": "its allocation bitmap's entry is damaged",
    "vtwofat": "it has more than one FAT",
}

# A boot sector byte changed for the boot and restored after it: (offset,
# value). NumberOfFats 2 with ActiveFat 0, as a TexFAT volume may be: fsd
# cannot tell which bitmap is live, so the volume is read-only. fsd does
# not check the boot checksum, so it is left as it was.
VBR = {"vtwofat": (110, 2)}
EROFS = 30  # picolibc's sys/errno.h, what libc's file.c gives FS_ERR_READ_ONLY
EEXIST = 17  # the same, for FS_ERR_ALREADY_EXISTS


def run_extent(label):
    """The root's 0x81 (bitmap) and 0x82 (up-case table) entries damaged as
    EXTENTS says, restored on the host after the boot so fsck_exfat reads
    the volume as fsd left it. A volume left writable must write and free;
    one left with no bitmap must refuse every write, before it writes."""
    damage, writable, second = EXTENTS[label]
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    if label in LATE:
        grow_root(vol)
        vol = Volume(IMAGE, start)
    r = free_run(vol, 2)
    d = bytearray(vol.data(vol.chain(vol.root)))
    places = [(next((i for i in range(0, len(d), 32) if d[i] == kind), None), field, value)
              for kind, field, value in damage]
    if r is None or any(at is None for at, _, _ in places):
        return [(f"{label}: two free clusters and the root's bitmap and up-case entries", False)], []
    width = {20: "<I", 24: "<Q"}
    good = [d[at + field:at + field + struct.calcsize(width[field])] for at, field, _ in places]
    bitmap_entry = bytes(d[places[0][0]:places[0][0] + 32]) if second else b""
    if not plant(vol, [plant_set("VF", r, 7), plant_set("VG", r + 1, 4), bitmap_entry], [r, r + 1],
                 fills=[(r, b"hello\r\n"), (r + 1, b"vg\r\n")]):
        return [(f"{label}: room for VF and VG in the root", False)], []
    for (at, field, value), was in zip(places, good):
        patch_root(vol, at + field, struct.pack(width[field], value(struct.unpack(width[field], was)[0], vol)))
    if label in LATE:
        d = bytearray(vol.data(vol.chain(vol.root)))
        at = next(i for i in range(0, len(d), 32) if d[i] == 0x81)
        entry, c = bytes(d[at:at + 32]), vol.cluster
        d[at] = 0x01  # deleted
        for i in range(exfatimg.root_end(d), c, 32):
            d[i:i + 32] = DELETED
        d[c:c + 32] = entry
        write_root(vol, d)
        moved = vol.data(vol.chain(vol.root))
        if not (moved[c] == 0x81 and exfatimg.root_end(moved) == c + 32 and 0x81 not in moved[0:c:32]):
            return [(f"{label}: the bitmap's entry moved to the root's second cluster, none before it", False)], []
    vbr_was = None
    if label in VBR:
        off, value = VBR[label]
        with open(IMAGE, "r+b") as fh:
            fh.seek(start * 512 + off)
            vbr_was = fh.read(1)
            fh.seek(start * 512 + off)
            fh.write(bytes([value]))
    # NEWB first, so its cluster is not the one rm gives back.
    cmds = (["echo new > /NEWB", "rm /VF", "ls /"] if writable else
            ["echo new > /NEWB", "write /VF", "echo t > /VF", "mv -f /VG /VF", "mv /VG /VH", "touch /VT", "rm /VF", "ls /"])
    # O_RDONLY|O_CREAT on a read-only volume: an existing file opens and
    # reads, a missing one is EROFS at the open (fsd's `refused_read_only`,
    # the high and medium reviews of #364).
    # O_CREAT|O_EXCL: EEXIST for VG before the read-only refusal, EROFS for
    # a missing VX (the O_EXCL fix of 2026-10-28).
    creat = [] if writable else ["ccreat /VG", "ccreat /VC", "ccreat -x /VG", "ccreat -x /VX"]
    driven, out, faults = boot(label, cmds[:-1] + creat + cmds[-1:])
    if drive_qemu.boot_stalled("test-long-set", label, out, faults):
        STALLED.append(label)
        return [], []
    for (at, field, _), was in zip(places, good):
        patch_root(vol, at + field, was)
    if vbr_was is not None:
        with open(IMAGE, "r+b") as fh:
            fh.seek(start * 512 + VBR[label][0])
            fh.write(vbr_was)
    if second:
        vol = Volume(IMAGE, start)
        d = vol.data(vol.chain(vol.root))
        copies = [i for i in range(0, len(d), 32) if d[i] == 0x81]
        patch_root(vol, copies[-1], b"\x01")  # the planted copy, deleted
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    vf, newb = exfatimg.find(root, "VF"), exfatimg.find(root, "NEWB")
    newb_size = struct.unpack_from("<Q", newb.data, 56)[0] if newb else 0
    ok, lines = exfatimg.fsck_clean(IMAGE)
    said = [ln.strip() for ln in out.splitlines() if "fsd: mounted read-only" in ln]
    checks = [(f"{label}: driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
              gate_check(label, out)]
    if writable:
        checks.append((f"{label}: no read-only line at the mount ({said})", not said))
    else:
        checks.append((f"{label}: the mount line says why it is read-only, {REASONS[label]!r} ({said})",
                       len(said) == 1 and said[0].startswith("fsd: mounted read-only: " + REASONS[label])))
    if writable:
        checks += [(f"{label}: an up-case table claiming the heap is no table and a longer bitmap is one: "
                    f"rm /VF freed VF's cluster ({vol.used(r)}), and NEWB was written ({newb_size} bytes)",
                    vf is None and not vol.used(r) and newb_size == 5 and printed(out, "rm /VF") == [])]
    else:
        names = [st.name for st in exfatimg.sets(root)]
        vf_bytes = vol.data([r])[:7]
        refused = {k: printed(out, k) for k in cmds[:-1]}
        checks += [
            (f"{label}: with no bitmap every write is refused read-only ({refused})",
             all(any("read-only" in ln for ln in v) for v in refused.values())),
            (f"{label}: ... and nothing written: VF one set, its 7 bytes and cluster kept, VG as it was, "
             f"no NEWB, VH or VT ({names.count('VF')} VF, {vf_bytes!r}, {vol.used(r)})",
             names.count("VF") == 1 and vf is not None and struct.unpack_from("<Q", vf.data, 56)[0] == 7
             and vf_bytes == b"hello\r\n" and vol.used(r) and vol.used(r + 1) and names.count("VG") == 1
             and not {"NEWB", "VH", "VT"} & set(names)),
            (f"{label}: O_RDONLY|O_CREAT opens an existing file and reads it, and is EROFS ({EROFS}) "
             f"for a missing one, with no VC made ({printed(out, 'ccreat /VG')}, {printed(out, 'ccreat /VC')})",
             printed(out, "ccreat /VG") == ["ccreat: /VG read 4"]
             and printed(out, "ccreat /VC") == [f"ccreat: /VC open errno {EROFS}"] and "VC" not in names),
            (f"{label}: O_WRONLY|O_CREAT|O_EXCL is EEXIST ({EEXIST}) for VG, not EROFS, and EROFS for a "
             f"missing VX, none made ({printed(out, 'ccreat -x /VG')}, {printed(out, 'ccreat -x /VX')})",
             printed(out, "ccreat -x /VG") == [f"ccreat: /VG open errno {EEXIST}"]
             and printed(out, "ccreat -x /VX") == [f"ccreat: /VX open errno {EROFS}"] and "VX" not in names),
        ]
    checks += [(f"{label}: fsck_exfat -n passes the image after the boot, the entries restored", ok),
               (f"{label}: no fault lines", faults == 0)]
    return checks, [f"{label} after: {ln}" for ln in lines] if not ok else []


def run_vruns():
    """The root grown on the host to 65 runs of clusters, a free cluster
    left between each and the next, as a root grows on a volume where files
    are made between its growths: past ROOT_RUNS (64), so not recorded
    whole. The volume is read-only, and the mount line must say why."""
    start = exfatimg.copy_exfat(IMAGE, SOURCE)
    vol = Volume(IMAGE, start)
    chain = vol.chain(vol.root)
    # Each cluster added is a run of its own: past the root's last cluster
    # and a cluster after the one before it.
    runs, last, c = len(runs_of(chain)), chain[-1], max(chain) + 2
    while runs < 65 and c < vol.count + 2:
        if vol.used(c) or c == last + 1:
            c += 1
            continue
        grow_root(vol, c, last)
        runs, last, c = runs + 1, c, c + 2
    vol = Volume(IMAGE, start)
    chain = vol.chain(vol.root)
    ok, lines = exfatimg.fsck_clean(IMAGE)
    checks = [(f"vruns: the root is {len(runs_of(chain))} runs of clusters, 65 wanted, and fsck_exfat -n "
               f"passes the image before the boot", len(runs_of(chain)) == 65 and ok)]
    if not checks[0][1]:
        return checks, [f"vruns before: {ln}" for ln in lines]
    driven, out, faults = boot("vruns", ["echo new > /NEWB", "ccreat /etc/passwd", "ccreat /VC",
                                         "ccreat -x /etc/passwd", "openprobe 2 /etc", "ls /"])
    if drive_qemu.boot_stalled("test-long-set", "vruns", out, faults):
        STALLED.append("vruns")
        return [], []
    said = [ln.strip() for ln in out.splitlines() if "fsd: mounted read-only" in ln]
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    checks += [
        (f"vruns: driven to the end, fsd mounted exFAT and its mount line says the root is in more than "
         f"64 runs ({said})", driven and "fsd: exFAT mounted" in out and len(said) == 1
         and said[0] == "fsd: mounted read-only: its root directory is in more than 64 runs of clusters"),
        (f"vruns: echo new > /NEWB refused read-only ({printed(out, 'echo new > /NEWB')}), no NEWB on the disk",
         any("read-only" in ln for ln in printed(out, "echo new > /NEWB")) and exfatimg.find(root, "NEWB") is None),
        (f"vruns: O_RDONLY|O_CREAT opens /etc/passwd and reads it, and is EROFS ({EROFS}) at the open for a "
         f"missing VC, none made ({printed(out, 'ccreat /etc/passwd')}, {printed(out, 'ccreat /VC')})",
         len(printed(out, "ccreat /etc/passwd")) == 1
         and re.fullmatch(r"ccreat: /etc/passwd read [1-9][0-9]*", printed(out, "ccreat /etc/passwd")[0]) is not None
         and printed(out, "ccreat /VC") == [f"ccreat: /VC open errno {EROFS}"] and exfatimg.find(root, "VC") is None),
        (f"vruns: O_WRONLY|O_CREAT|O_EXCL of /etc/passwd is EEXIST ({EEXIST}), not EROFS "
         f"({printed(out, 'ccreat -x /etc/passwd')})",
         printed(out, "ccreat -x /etc/passwd") == [f"ccreat: /etc/passwd open errno {EEXIST}"]),
        # A directory opened to write is EISDIR before the read-only refusal,
        # as Linux orders them (since 2026-10-29, the review of #370).
        (f"vruns: NP_OPEN of /etc for writing is FS_ERR_NOT_A_FILE, not read-only "
         f"({printed(out, 'openprobe 2 /etc')})",
         printed(out, "openprobe 2 /etc") == ["openprobe: /etc 2 FS_ERR_NOT_A_FILE"]),
        ("vruns: no fault lines", faults == 0),
        gate_check("vruns", out),
    ]
    return checks, []


def runs_of(chain):
    """The runs of consecutive clusters `chain` is made of, as fsd records
    the root's: (first, count) each."""
    runs = []
    for c in chain:
        if runs and runs[-1][0] + runs[-1][1] == c:
            runs[-1][1] += 1
        else:
            runs.append([c, 1])
    return runs


def patch_root(vol, at, data):
    """`data` written at byte `at` of the root directory, straight to the
    image: the root read through `vol` may be stale once the image changed."""
    chain = vol.chain(vol.root)
    with open(IMAGE, "r+b") as fh:
        fh.seek(vol.heap + (chain[at // vol.cluster] - 2) * vol.cluster + at % vol.cluster)
        fh.write(data)


def main():
    checks, notes = [], []
    runs = (run_long, run_shape, run_rename, run_valloc, run_vcross, run_vmeet, run_vroot, run_vruns,
            *(lambda label=label: run_extent(label) for label in EXTENTS))
    if DISK_GATE:
        gate_image()
        runs = (run_vruns, *(lambda label=label: run_extent(label) for label in EXTENTS if not EXTENTS[label][1]))
    for run in runs:
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
    return drive_qemu.verdict("test-long-set", failed, len(STALLED))


if __name__ == "__main__":
    sys.exit(main())

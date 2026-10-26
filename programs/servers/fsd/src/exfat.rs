//! exFAT, read-write - the second on-disk format `fsd` understands, and the
//! first real exercise of the [`Filesystem`](crate::vfs::Filesystem) enum's
//! dispatch (until now FAT32 was the only arm). Landed read-only first, then
//! read-write in four staged commits (allocation + `touch`; `write_file`/
//! `write_at`; `mkdir`/`rm`/`rmdir`; `mv`) - the big scoping lever the "more
//! filesystems" roadmap arc calls out, since FAT32's own write support was where
//! every corruption risk lived (phases 4-8).
//!
//! **Write model.** Free clusters are tracked by an allocation *bitmap*
//! ([`Fs::alloc_cluster`]/[`Fs::bitmap_clear_run`]), located at mount from the root's
//! `0x81` entry; a volume with none is read-only ([`Fs::writable`]). Files/directories we create are always FAT-*chained*
//! (`NoFatChain = 0`), so allocation parallels FAT32's `write_chain` (a
//! bitmap-set plus a FAT-link) and the reader's [`Fs::advance`] walks the
//! chain. Creating an
//! entry ([`Fs::create_entry`]) builds a full set with both required checksums -
//! the whole-set `SetChecksum` and the up-cased `NameHash`; deleting one
//! ([`Fs::unlink_set`]) clears each entry's in-use bit. Same corruption
//! discipline as the FAT32 write arc: claim before use, write the new thing
//! before freeing the old, validate names up front. Verified against a real
//! driver end to end (macOS mounts + `fsck_exfat` passes; a copied binary reads
//! back byte-identical).
//!
//! Same constraints as everything in `fsd`: hand-rolled, fixed-buffer, no
//! `alloc`, sitting on [`Disk`]'s `BLOCK_*` syscall shim. No crates - a
//! filesystem crate would assume an allocator and hit the PIE/libcore wall
//! anyway (see `fat32.rs`).
//!
//! # What exFAT is, versus FAT32
//!
//! Structurally it's still a cluster filesystem - a partition, a FAT, a data
//! region of fixed-size clusters - so the read *machinery* (cluster-to-LBA,
//! chain walking, windowed reads) is the same shape as [`fat32`](crate::fat32).
//! The genuinely different parts, and how this driver handles each:
//!
//! - **The boot sector** is a different layout: field *shifts* (`log2` of the
//!   sector and cluster sizes) instead of raw counts, and the FAT/cluster-heap
//!   offsets are explicit sector counts. Parsed in [`Fs::mount_at`].
//! - **Contiguous files skip the FAT entirely.** Each file/directory carries a
//!   `NoFatChain` flag; when set, its clusters are simply consecutive and the
//!   FAT isn't consulted at all (this is why exFAT can allocate huge files
//!   without a long chain walk). [`Fs::advance`] branches on it.
//! - **Directory entries are *entry sets***, not one 32-byte record: a File
//!   entry (`0x85`) + a Stream-Extension entry (`0xC0`, carrying the first
//!   cluster, data length, and `NoFatChain` flag) + one or more File-Name
//!   entries (`0xC1`, 15 UTF-16 chars each). [`Fs::walk_dir`] reassembles a set
//!   into one [`DirEntry`], the same shape FAT32's LFN reconstruction produces.
//! - **Names are UTF-16, up to 255 chars** - no 8.3, no LFN checksum dance.
//!   A walk hands each set's name on as UTF-16, and a lookup compares it so
//!   ([`name_answers`]); a listing renders it UTF-8 ([`utf8_name`]), which a
//!   lookup takes back unchanged, but for a name it cannot show (one past
//!   [`NAME_SHOWN_MAX`] bytes, or holding an unpaired surrogate), shown ASCII
//!   with `?` for each other unit as every name was until 2026-10-21, when
//!   `café` and `cafè` were one name.
//! - **An allocation *bitmap*** replaces FAT free-cluster scanning: reads ignore
//!   it entirely, writes drive it (see the write model above). The up-case table
//!   (`0x82`) that would drive case-insensitive comparison per spec is ignored -
//!   we approximate with ASCII case-folding ([`upcase`]), correct for the ASCII
//!   names this system uses; it is the one rule both name matching
//!   ([`name_answers`], and [`names_match`] for `NP_MV`'s guard) and the
//!   `NameHash` we write go through. A name outside ASCII is compared by it
//!   too, so `CAFÉ` is not `café` here though the spec's table says it is
//!   (the roadmap).

use core::ops::ControlFlow;

use crate::disk::{Disk, DiskError};
use crate::fat32::{split_parent, Error};
use crate::utf16::{shown_exactly, utf8_name, NAME_SHOWN_MAX};

const SECTOR_SIZE: usize = 512;
const ENTRY_SIZE: usize = 32;

// Directory entry type bytes. The high bit (0x80) is "in use"; a byte with it
// clear is a deleted/unused entry, and a literal 0x00 marks end-of-directory.
const ET_ALLOC_BITMAP: u8 = 0x81; // the allocation bitmap (a system file in root)
const ET_FILE: u8 = 0x85; // primary entry of a file/dir set
const ET_STREAM_EXT: u8 = 0xc0; // first secondary: cluster/size/flags
const ET_FILE_NAME: u8 = 0xc1; // secondary: 15 UTF-16 name chars
const ET_UPCASE: u8 = 0x82; // the up-case table (a system file in root)
/// An entry-type byte's category and importance bits (`0x60`) plus its in-use
/// bit: a benign secondary (vendor extension `0xE0`, vendor allocation
/// `0xE1`, and any later one) has all three set while it is in use.
const ET_BENIGN_SECONDARY: u8 = 0xe0;

/// The "in use" bit on an entry-type byte. Clearing it marks the entry deleted
/// (`0x85` -> `0x05`, `0xC0` -> `0x40`, `0xC1` -> `0x41`) - how a set is removed.
const ET_IN_USE: u8 = 0x80;

/// `FileAttributes` bit 4 (0x0010): this entry is a directory.
const ATTR_DIRECTORY: u16 = 0x0010;
/// `GeneralSecondaryFlags` bit 0: the cluster/length fields are meaningful.
const SECONDARY_ALLOC_POSSIBLE: u8 = 0x01;
/// `GeneralSecondaryFlags` bit 1: the data is contiguous, don't use the FAT.
const NO_FAT_CHAIN: u8 = 0x02;

/// exFAT FAT end-of-chain / bad-cluster threshold: `0xFFFFFFF7` is the bad
/// marker and `0xFFFFFFFF` is end-of-chain; anything at/above the bad marker
/// (and `0` = free) terminates a walk. Real clusters are `2..cluster_count+1`.
const END_OF_CHAIN_MIN: u32 = 0xffff_fff7;
/// The end-of-chain value we *write* into the FAT (the canonical marker).
const END_OF_CHAIN: u32 = 0xffff_ffff;

/// Whether a FAT entry stops a chain walk: `0` or end of chain (a bad-cluster
/// mark among them). [`Fs::next_cluster`]'s rule, which [`Fs::write_fat_entry`]
/// asks of it to decide whether a write leaves the cursors standing.
fn ends_walk(value: u32) -> bool {
    value == 0 || value >= END_OF_CHAIN_MIN
}

/// Longest reconstructed name we keep - the exFAT maximum.
const LONG_NAME_MAX: usize = 255;

/// Cap on a single `write_at` gap zero-fill past EOF (same as `fat32`'s), so a
/// fat-fingered huge offset can't try to zero-fill the whole volume.
const MAX_GAP_FILL: u64 = 1 << 20;

/// UTF-16 name chars per File-Name (`0xC1`) entry.
const NAME_CHARS_PER_ENTRY: usize = 15;
/// Most 32-byte entries in one set: a File entry + a Stream-Extension entry +
/// ceil(255/15)=17 File-Name entries.
const MAX_SET_ENTRIES: usize = 2 + LONG_NAME_MAX.div_ceil(NAME_CHARS_PER_ENTRY);
/// Byte size of that largest set (the fixed buffer `build_entry_set` fills).
const MAX_SET_BYTES: usize = MAX_SET_ENTRIES * ENTRY_SIZE;
/// Most entries in any set the spec allows: a File entry and up to 255
/// secondaries (SecondaryCount is a byte), vendor ones after the name.
const WHOLE_SET_ENTRIES: usize = 1 + 255;
/// Directory-entry slots per 512-byte sector.
const SLOTS_PER_SECTOR: usize = SECTOR_SIZE / ENTRY_SIZE;

/// One directory entry set, decoded into a self-contained, no-alloc form - the
/// same role `fat32::DirEntry` plays, carrying the one extra thing exFAT needs:
/// whether the data is contiguous (`NoFatChain`), which decides how the cluster
/// walk advances.
#[derive(Clone, Copy)]
struct DirEntry {
    is_dir: bool,
    size: u64,
    /// The Stream's ValidDataLength, at most `size`: the bytes past it read as
    /// zeros, whatever their clusters hold (a file another driver preallocated).
    /// A directory's is its size, kept so where the size changes
    /// (`grow_extent`); the root's size and so its valid length are 0.
    valid: u64,
    first_cluster: u32,
    contiguous: bool,
    /// The set holds a benign secondary with AllocationPossible set (a
    /// vendor allocation, 0xE1, as a camera writes): clusters of its own,
    /// which [`Fs::delete_set`] frees with the set's.
    allocs: bool,
}

/// What becomes of the clusters a set names when [`Fs::delete_set`] deletes
/// it: every caller says, so no path deletes a set and forgets them.
#[derive(Clone, Copy)]
enum SetGoes {
    /// A set written elsewhere carries them (a rename).
    Renamed,
    /// Gone for good (`rm`, `rmdir`): freed ([`Fs::free_runs`]). A
    /// replacing `mv`, whose source's set survives, frees with `free_runs`
    /// itself, after both deletes.
    Removed,
}

/// Allocations a set names, read once: its Stream's when it names a cluster
/// and the first [`Runs::MAX`] its benign secondaries name
/// ([`Fs::runs_of`]), or the secondaries' alone ([`Fs::unlink_set`]). `more`
/// when it names more than that. A surviving set's that is taken to overlap
/// every run, so a removal beside it (none a camera writes) leaks rather
/// than frees under it; a removed set's past the first are left allocated,
/// a leak a fsck finds.
#[derive(Clone, Copy)]
struct Runs {
    runs: [Option<Alloc>; Runs::MAX],
    more: bool,
}

impl Runs {
    const MAX: usize = 8;
    /// No surviving set: nothing kept.
    const NONE: Runs = Runs { runs: [None; Runs::MAX], more: false };

    fn add(&mut self, a: Alloc) {
        match self.runs.iter_mut().find(|r| r.is_none()) {
            Some(slot) => *slot = Some(a),
            None => self.more = true,
        }
    }
}

/// An allocation a benign secondary names, decoded by [`allocation`], or a
/// Stream's ([`Alloc::stream`]).
#[derive(Clone, Copy)]
struct Alloc {
    first: u32,
    contiguous: bool,
    size: u64,
}

impl Alloc {
    /// The data `entry`'s Stream names, as an allocation.
    fn stream(entry: &DirEntry) -> Alloc {
        Alloc { first: entry.first_cluster, contiguous: entry.contiguous, size: entry.size }
    }
}

/// Whether cluster `c` lies in the contiguous run of `bytes` from `first`,
/// in clusters of `cluster_bytes`: the one test of a run by its count, for
/// the volume's own tables ([`Fs::check_extents`], [`Fs::is_system`]) and
/// for an allocation ([`Fs::overlaps`]). A run starting below cluster 2
/// holds none (written three times, two of them without that guard, until
/// the high review of 2026-10-24).
fn run_has(first: u32, bytes: u64, cluster_bytes: u64, c: u32) -> bool {
    first >= 2 && c >= first && ((c - first) as u64) < bytes.div_ceil(cluster_bytes)
}

/// Whether the contiguous run of `bytes` from `first`, in clusters of
/// `cluster_bytes`, lies inside a heap of `count` clusters: the one test of
/// it, for the volume's own tables ([`Fs::check_extents`]), a free
/// ([`Fs::free_data`]) and a link ([`Fs::ensure_chained`]), each written by
/// hand until the fourth high review of 2026-10-24.
fn run_fits(first: u32, bytes: u64, cluster_bytes: u64, count: u32) -> bool {
    first >= 2 && first - 2 < count && (first - 2) as u64 + bytes.div_ceil(cluster_bytes) <= count as u64
}

/// The FirstCluster and DataLength of the 32-byte entry `raw`, at the
/// offsets a Stream, a benign secondary, the bitmap's and the up-case
/// table's entries share: the one decode of them (`walk_dir`,
/// [`allocation`], [`Fs::locate_tables`]; each decoded them itself until the
/// third high review of 2026-10-24).
fn extent(raw: &[u8]) -> (u32, u64) {
    (
        u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]),
        u64::from_le_bytes([raw[24], raw[25], raw[26], raw[27], raw[28], raw[29], raw[30], raw[31]]),
    )
}

/// The allocation the 32-byte entry `raw` names when it is an in-use benign
/// secondary with AllocationPossible set (every caller reads a live set:
/// deleted ones were taken too until the fourth high review of 2026-10-24,
/// from when a removal read its set back after deleting it), and of a
/// DataLength above 0 (none, as a Stream's: `walk_dir`). Its fields are
/// decoded by [`extent`].
fn allocation(raw: &[u8]) -> Option<Alloc> {
    if raw[0] & ET_BENIGN_SECONDARY != ET_BENIGN_SECONDARY || raw[1] & SECONDARY_ALLOC_POSSIBLE == 0 {
        return None;
    }
    let (first, size) = extent(raw);
    (size > 0).then(|| Alloc { first, contiguous: raw[1] & NO_FAT_CHAIN != 0, size })
}

/// The largest directory the format allows (a directory's DataLength is at
/// most 256 MiB in the spec's Stream Extension entry), which
/// bounds [`Fs::locate_tables`]' scan of the root.
const DIR_MAX_BYTES: u64 = 256 << 20;

/// The most runs of consecutive clusters [`Fs::root_runs`] records the
/// root's chain in. A root grows a cluster at a time, each a new run when
/// something was allocated since, so a root of 4 KiB clusters holding some
/// 2,500 files in as many runs as it has clusters fits; one in more is not
/// recorded whole, and the volume is read-only ([`Fs::record_root`]).
const ROOT_RUNS: usize = 64;

/// What a cluster's sectors are read or written for, which [`Fs::sector_of`]
/// holds the cluster to: the root directory's slots, the allocation bitmap,
/// or anything else (a file's bytes, a subdirectory's slots).
#[derive(Clone, Copy)]
enum Holder {
    Root,
    Bitmap,
    Data,
}

/// Whether the runs of `n` clusters from `a` and of `m` from `b` share a
/// cluster; a run starting below cluster 2, or of none, shares nothing.
fn runs_meet(a: u32, n: u64, b: u32, m: u64) -> bool {
    a >= 2 && b >= 2 && n > 0 && m > 0 && (a as u64) < b as u64 + m && (b as u64) < a as u64 + n
}

/// Where an entry set lies: the directory holding it, its primary slot and
/// its entry count.
#[derive(Clone, Copy)]
struct SetAt {
    dir_cluster: u32,
    dir_contiguous: bool,
    slot: usize,
    len: usize,
}

/// The clusters a directory holds, by [`Fs::dir_extent`]: how many, and the
/// last (`0` when there are none).
#[derive(Clone, Copy)]
struct Extent {
    clusters: u64,
    tail: u32,
    /// The chain goes on past `tail`, past the directory's size.
    past_size: bool,
}

/// A directory's extent as it is walked: what [`Fs::extent_next`] needs to
/// decide whether a walk goes on to the next cluster.
#[derive(Clone, Copy)]
struct ExtentRule {
    root: bool,
    contiguous: bool,
    /// The directory's size in clusters (rounded up).
    sized: u64,
}

/// The step after a cluster of a directory's extent.
enum Next {
    Cluster(u32),
    End,
    /// The extent ends at the size, and the chain goes on past it.
    PastSize,
}

/// A directory to create an entry in: its own entry, and where that entry's
/// set lies (`None` for the root, which has none). Growing a directory past
/// its clusters raises its DataLength, which other drivers stop at, so
/// whoever grows it must know where that length is recorded; `create_entry`
/// takes only this, built by `dir_at` and `parent_for_create`.
struct Dir {
    entry: DirEntry,
    at: Option<SetAt>,
}

/// A create that failed ([`Fs::create_entry`]), and whether any of its set
/// was written.
struct CreateFailed {
    error: Error,
    written: bool,
}

impl From<CreateFailed> for Error {
    fn from(f: CreateFailed) -> Self {
        f.error
    }
}

/// Where a create in a directory may write, by [`Fs::room`]: the root, whose
/// extent, its whole chain, is walked only when it grows (a create that
/// finds a slot reads no FAT, as before 2026-10-18), or a subdirectory's set
/// and its extent, found to be its size.
enum Room {
    Root,
    Sub(SetAt, Extent),
}

pub struct Fs {
    disk: Disk,
    /// The volume's first sector (the VBR) - kept so `mount`-info can report
    /// where this filesystem lives on the disk (disk-tools milestone 1).
    partition_lba: u32,
    /// Sectors per cluster (`1 << SectorsPerClusterShift`).
    sectors_per_cluster: u32,
    /// Absolute LBA of the (first) FAT.
    fat_lba: u32,
    /// Absolute LBA of cluster 2 (the start of the cluster heap).
    cluster_heap_lba: u32,
    /// Count of clusters in the heap (real clusters are `2..cluster_count+1`).
    cluster_count: u32,
    /// First cluster of the root directory.
    root_cluster: u32,
    /// First cluster of the allocation bitmap (the free-cluster map), located at
    /// mount from the root's `0x81` entry - `0` if not found (then writes that
    /// need allocation fail with `DiskFull`). The bitmap is assumed *contiguous*
    /// (the near-universal case, and what `newfs_exfat` produces); a fragmented
    /// bitmap would need FAT-chain addressing here (a known limitation).
    bitmap_first_cluster: u32,
    /// The allocation bitmap's size in bytes (`DataLength` of its `0x81` entry).
    bitmap_bytes: u64,
    /// The up-case table's first cluster and size (its `0x82` entry), `0` if
    /// not found: with the bitmap and the root's first cluster, the volume's
    /// own clusters, which [`free_data`](Self::free_data) never frees.
    upcase_first_cluster: u32,
    upcase_bytes: u64,
    /// The root directory's chain as runs of consecutive clusters (first,
    /// count), the first `root_len` of them: recorded at mount
    /// ([`record_root`](Self::record_root)) and extended where the root grows
    /// ([`grow_extent`](Self::grow_extent)). With the bitmap and the up-case
    /// table, the volume's own clusters ([`is_system`](Self::is_system)).
    /// Until 2026-10-26 only the root's first cluster was known, so a damaged
    /// run over a later one was freed and handed out again, and written.
    root_runs: [(u32, u32); ROOT_RUNS],
    root_len: usize,
    /// Whether `root_runs` holds the root's whole chain. When it does not (a
    /// loop, a chain past [`DIR_MAX_BYTES`], or more runs than
    /// [`ROOT_RUNS`]) the volume is read-only, and the root is read wherever
    /// its chain goes but the bitmap and up-case table.
    root_whole: bool,
    /// Where the last read's walk along a file's clusters ended, so the next
    /// read of the same file at or past it resumes there - `fat32::Fs`'s
    /// `read_cursor`, for the same reason. A file `fsd` writes is always
    /// FAT-chained, and [`read_each`](Self::read_each) reads one in 2 KiB
    /// pieces, so without it every piece walked the chain from the first
    /// cluster: a spawn of a 640 KiB file reads 1,436 FAT sectors that
    /// way, through the FAT cache, where one walk reads 6 (`make
    /// test-spawn-reads`, 2026-10-17). Only the position is kept, never data, and
    /// [`write_fat_entry`](Self::write_fat_entry) drops it when it rewrites a
    /// link (since 2026-10-18 a tail extension leaves it, and so does an
    /// allocation over a FAT entry of 0, which fsd's own frees leave; another
    /// driver may leave a link there, which drops it),
    /// so a read never follows a stale chain.
    read_cursor: Option<ChainCursor>,
    /// Where the last [`write_at`](Self::write_at) ended in its file's chain,
    /// so the next write of the same file at or past it seeks from there:
    /// `fat32::Fs`'s `write_cursor`, for the same reason. Kept apart from the
    /// read cursor because `cp` reads one file and writes another in turn;
    /// dropped with it when a FAT write rewrites a link, and set again at the
    /// end of a write, from the chain it has just walked and built. Always on a FAT-chained walk:
    /// `write_at` converts a contiguous file first.
    write_cursor: Option<ChainCursor>,
    /// The last FAT sector read or written, for [`fat_entry`](Self::fat_entry):
    /// `fat32::Fs`'s `fat_cache`. One sector holds 128 entries, so a walk
    /// along a chain reads each sector once, not once a step; without it
    /// `write_at` walked a file's chain from its first cluster, a disk read a
    /// step, for every 2 KiB a client wrote. Kept across requests for
    /// `fat32`'s reason: this `Fs` is the only writer of its FAT while it is
    /// mounted, and [`write_fat_entry`](Self::write_fat_entry) keeps it in
    /// step with its own writes.
    fat_cache: Option<FatSector>,
}

/// One sector of the FAT, as cached by [`Fs::fat_cache`].
struct FatSector {
    lba: u64,
    data: [u8; SECTOR_SIZE],
}

/// A point in a file's cluster walk, for [`Fs::read_cursor`] and [`Fs::write_cursor`].
#[derive(Clone, Copy)]
struct ChainCursor {
    /// The file's first cluster and its `NoFatChain` flag, which together say
    /// which file and which walk the cursor is for.
    file_cluster: u32,
    contiguous: bool,
    /// A cluster reached by walking forward from the file's first, covering
    /// the file's bytes `[cluster_pos, cluster_pos + cluster size)`.
    cluster: u32,
    cluster_pos: usize,
}

impl ChainCursor {
    /// Where a walk to byte `pos` of the file whose first cluster is `head`
    /// starts, as (cluster, its first byte's position): at `cursor` when it is
    /// that file's, on the same walk (`contiguous`), and not past `pos`, else
    /// at the head. The one rule for both cursors, as in `fat32.rs`.
    fn start(cursor: Option<Self>, head: u32, contiguous: bool, pos: usize) -> (u32, usize) {
        match cursor {
            Some(c) if c.file_cluster == head && c.contiguous == contiguous && c.cluster_pos <= pos => {
                (c.cluster, c.cluster_pos)
            }
            _ => (head, 0),
        }
    }
}

impl Fs {
    /// Mount the exFAT volume whose first sector (the boot sector / VBR) is at
    /// `partition_lba`. Returns [`Error::NotExFat`] if that sector isn't an
    /// exFAT VBR, so `vfs::mount` can try the next partition/format.
    pub fn mount_at(mut disk: Disk, partition_lba: u32, partition_sectors: u64) -> Result<Self, Error> {
        let mut vbr = [0u8; SECTOR_SIZE];
        disk.read_sector(partition_lba as u64, &mut vbr)?;

        // FileSystemName "EXFAT   " at offset 3, and the usual boot signature.
        if &vbr[3..11] != b"EXFAT   " || vbr[510] != 0x55 || vbr[511] != 0xaa {
            return Err(Error::NotExFat);
        }

        let bytes_per_sector_shift = vbr[108];
        let sectors_per_cluster_shift = vbr[109];
        // This whole stack assumes 512-byte sectors (see `disk.rs`); a volume
        // formatted with a different sector size isn't something we read.
        if bytes_per_sector_shift != 9 {
            // The size named only when it fits the error's u16: the shift is
            // the boot sector's byte, unchecked (the third review of #330).
            return Err(Error::UnsupportedSectorSize(1u16.checked_shl(bytes_per_sector_shift as u32).unwrap_or(0)));
        }

        let fat_offset = u32::from_le_bytes([vbr[80], vbr[81], vbr[82], vbr[83]]);
        let cluster_heap_offset = u32::from_le_bytes([vbr[88], vbr[89], vbr[90], vbr[91]]);
        let cluster_count = u32::from_le_bytes([vbr[92], vbr[93], vbr[94], vbr[95]]);
        let root_cluster = u32::from_le_bytes([vbr[96], vbr[97], vbr[98], vbr[99]]);
        let volume_length = u64::from_le_bytes(vbr[72..80].try_into().unwrap());
        let fat_length = u32::from_le_bytes([vbr[84], vbr[85], vbr[86], vbr[87]]);

        // The volume as its boot sector lays it out, inside its partition:
        // `in_heap` (`cluster_count`) is the bound every cluster is checked
        // against, and the FAT entry of every such cluster must lie in the
        // FAT. A boot sector claiming more than the partition holds is not
        // this volume: a stale one left by a larger image (`erase disk`
        // zeroes the first 1 MiB, `partition` starts there) named a root
        // past the end of the disk, and its probe answered an I/O error (the
        // review of #329). The spec's own limits (bytes per sector and
        // cluster at most 2^25) keep the shift sane, checked before it is
        // used; the FAT starts past the main and backup boot regions (sector
        // 24, the spec's least FatOffset), so no FAT write lands on them. In
        // u64 throughout; the volume's sectors fit 32 bits from here
        // (`volume_bound`).
        if sectors_per_cluster_shift > 25 - 9 {
            return Err(Error::NotExFat);
        }
        let heap_end = cluster_heap_offset as u64 + ((cluster_count as u64) << sectors_per_cluster_shift);
        if volume_length > partition_sectors
            || heap_end > volume_length
            || fat_offset < 24
            || fat_offset as u64 + fat_length as u64 > cluster_heap_offset as u64
            || (fat_length as u64) * (SECTOR_SIZE as u64 / 4) < cluster_count as u64 + 2
        {
            return Err(Error::NotExFat);
        }

        let mut fs = Fs {
            disk,
            partition_lba,
            sectors_per_cluster: 1u32 << sectors_per_cluster_shift,
            fat_lba: partition_lba + fat_offset,
            cluster_heap_lba: partition_lba + cluster_heap_offset,
            cluster_count,
            root_cluster,
            bitmap_first_cluster: 0,
            bitmap_bytes: 0,
            upcase_first_cluster: 0,
            upcase_bytes: 0,
            root_runs: [(0, 0); ROOT_RUNS],
            root_len: 0,
            root_whole: false,
            read_cursor: None,
            write_cursor: None,
            fat_cache: None,
        };
        // The root inside the heap, by the rule every later cluster meets.
        if !fs.in_heap(fs.root_cluster) {
            return Err(Error::NotExFat);
        }
        // The root's clusters first: the table scan reads them through
        // `sector_of`, and `check_extents` holds the tables off them.
        fs.root_whole = fs.record_root()?;
        // Best-effort: reads don't need the bitmap, so a volume without one
        // (malformed) still mounts read-only ([`writable`](Self::writable)).
        fs.locate_tables()?;
        fs.check_extents();
        // A root not recorded whole leaves clusters of its own that no check
        // knows, which an allocation could take and a write reach: read-only,
        // as a volume with no bitmap is.
        if !fs.root_whole {
            (fs.bitmap_first_cluster, fs.bitmap_bytes) = (0, 0);
        }
        // A volume of two FATs (TexFAT) keeps two bitmaps too, and
        // VolumeFlags' ActiveFat names the live pair; fsd reads the first
        // FAT and the first bitmap entry, which may be the stale pair, and
        // writing there would hand out clusters the live bitmap holds (the
        // high review of 2026-10-24). So it is read-only, as one with no
        // bitmap is ([`writable`](Self::writable)); reading the active FAT
        // is a roadmap item.
        if vbr[110] != 1 {
            (fs.bitmap_first_cluster, fs.bitmap_bytes) = (0, 0);
        }
        Ok(fs)
    }

    /// Record the root's chain in [`root_runs`](Self::root_runs), answering
    /// whether it is all there. A walk of FAT entries alone, through the FAT
    /// cache, so a root of a few clusters costs a FAT sector read.
    ///
    /// A root whose chain loops on itself ended no walk, and the mount hung
    /// (the resumed max review of 2026-10-23); bounding the walk by the
    /// heap's cluster count still read a 128 GB stick's sectors a quarter of
    /// a billion times (the high review). So the walk stops where the chain
    /// comes back to a cluster it passed (Brent's test: a mark moved to the
    /// walk at every power of two of steps, so a loop is met within twice
    /// its length and lead-in), and at the largest directory the format
    /// allows, [`DIR_MAX_BYTES`]; the root is then not whole.
    fn record_root(&mut self) -> Result<bool, Error> {
        let most = (DIR_MAX_BYTES / self.cluster_bytes()).min(self.cluster_count as u64);
        let mut cluster = self.root_cluster;
        let (mut mark, mut power, mut steps) = (cluster, 1u64, 0u64);
        for _ in 0..most {
            if !self.root_push(cluster) {
                return Ok(false);
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(true),
            }
            if cluster == mark {
                return Ok(false); // the chain loops
            }
            steps += 1;
            if steps == power {
                (mark, power, steps) = (cluster, power * 2, 0);
            }
        }
        Ok(false)
    }

    /// Add `cluster` to the root's record, after the last: the last run made
    /// longer when it follows it, else a new run. False when there is no
    /// room for one ([`root_takes`](Self::root_takes)).
    fn root_push(&mut self, cluster: u32) -> bool {
        if !self.root_takes(cluster) {
            return false;
        }
        match self.root_runs[..self.root_len].last_mut() {
            Some(last) if last.0 as u64 + last.1 as u64 == cluster as u64 => last.1 += 1,
            _ => {
                self.root_runs[self.root_len] = (cluster, 1);
                self.root_len += 1;
            }
        }
        true
    }

    /// Whether the root's record can take `cluster` after its last: it
    /// follows the last run, or there is room for a run.
    fn root_takes(&self, cluster: u32) -> bool {
        self.root_len < ROOT_RUNS
            || self.root_runs[..self.root_len].last().is_some_and(|l| l.0 as u64 + l.1 as u64 == cluster as u64)
    }

    /// Whether `cluster` is among the root's recorded clusters.
    fn in_root(&self, cluster: u32) -> bool {
        self.root_runs[..self.root_len].iter().any(|&(f, n)| cluster >= f && cluster - f < n)
    }

    /// Whether the run of `n` clusters from `first` meets the root's.
    fn meets_root(&self, first: u32, n: u64) -> bool {
        self.root_runs[..self.root_len].iter().any(|&(f, m)| runs_meet(first, n, f, m as u64))
    }

    /// Find the allocation bitmap and the up-case table by scanning the root
    /// directory for their `0x81` and `0x82` entries, over the clusters
    /// [`record_root`](Self::record_root) recorded. Records each one's first
    /// cluster and byte length, until both are found or the directory ends;
    /// leaves them `0` if one is not found. The first entry of each is taken,
    /// damaged or not: a first bitmap entry naming cluster 0 was passed over
    /// for the one after it, on a volume of two FATs the second FAT's bitmap
    /// (the high review of 2026-10-24); [`check_extents`](Self::check_extents)
    /// drops a damaged one. Which of a two-FAT volume's is live is not read
    /// here: such a volume is read-only ([`mount_at`](Self::mount_at)).
    ///
    /// The extents are kept here and recorded once the scan ends: recorded
    /// as found, an unchecked up-case entry claiming the heap made
    /// `sector_of` refuse the root's next cluster, and the mount failed
    /// where `check_extents` would have dropped the entry (the high review
    /// of 2026-10-26).
    fn locate_tables(&mut self) -> Result<(), Error> {
        let (mut bitmap, mut upcase) = (None, None);
        self.scan_tables(&mut bitmap, &mut upcase)?;
        (self.bitmap_first_cluster, self.bitmap_bytes) = bitmap.unwrap_or((0, 0));
        (self.upcase_first_cluster, self.upcase_bytes) = upcase.unwrap_or((0, 0));
        Ok(())
    }

    /// [`locate_tables`](Self::locate_tables)' scan, the first of each entry
    /// left in `bitmap` and `upcase`.
    fn scan_tables(&mut self, bitmap: &mut Option<(u32, u64)>, upcase: &mut Option<(u32, u64)>) -> Result<(), Error> {
        for r in 0..self.root_len {
            let (first, n) = self.root_runs[r];
            for cluster in first..first + n {
                let lba = self.sector_of(cluster, Holder::Root)?;
                for s in 0..self.sectors_per_cluster {
                    let mut buf = [0u8; SECTOR_SIZE];
                    self.disk.read_sector((lba + s) as u64, &mut buf)?;
                    for raw in buf.chunks_exact(ENTRY_SIZE) {
                        match raw[0] {
                            0x00 => return Ok(()), // end of directory - no bitmap
                            t @ (ET_ALLOC_BITMAP | ET_UPCASE) => {
                                let found = if t == ET_ALLOC_BITMAP { &mut *bitmap } else { &mut *upcase };
                                found.get_or_insert(extent(raw));
                                if bitmap.is_some() && upcase.is_some() {
                                    return Ok(());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Hold the bitmap's and the up-case table's extents, as the root's
    /// entries give them, to the volume: each a run inside the heap, the
    /// bitmap at least a bit a cluster (`ceil(ClusterCount / 8)` bytes; the
    /// spec, Linux and exfat-fuse take a longer one), the up-case table at
    /// most 65,536 two-byte entries, neither on a cluster of the root's
    /// recorded chain (its first alone until 2026-10-26).
    /// One that is not is taken as absent: a bitmap so is no bitmap (the
    /// volume is read-only, [`writable`](Self::writable)), an up-case table
    /// so is one [`is_system`](Self::is_system) does not count. Two on each
    /// other are damage, and which one is cannot be told: the bitmap goes,
    /// so nothing is allocated or freed on either. Without this a damaged
    /// DataLength made every cluster past it the volume's own, and every
    /// removal freed nothing (the max review of 2026-10-23).
    fn check_extents(&mut self) {
        let (cb, count) = (self.cluster_bytes(), self.cluster_count);
        let fits = |first: u32, bytes: u64| bytes > 0 && run_fits(first, bytes, cb, count);
        let covers = |first: u32, bytes: u64, c: u32| run_has(first, bytes, cb, c);
        let on_root = |fs: &Self, first: u32, bytes: u64| fs.meets_root(first, bytes.div_ceil(cb));
        let (bf, bb) = (self.bitmap_first_cluster, self.bitmap_bytes);
        if bb < self.cluster_count.div_ceil(8) as u64 || !fits(bf, bb) || on_root(self, bf, bb) {
            (self.bitmap_first_cluster, self.bitmap_bytes) = (0, 0);
        }
        let (uf, ub) = (self.upcase_first_cluster, self.upcase_bytes);
        if ub > 2 * 65_536 || !fits(uf, ub) || on_root(self, uf, ub) {
            (self.upcase_first_cluster, self.upcase_bytes) = (0, 0);
        }
        let ((bf, bb), (uf, ub)) = ((self.bitmap_first_cluster, self.bitmap_bytes), (self.upcase_first_cluster, self.upcase_bytes));
        if bb > 0 && ub > 0 && (covers(bf, bb, uf) || covers(uf, ub, bf)) {
            (self.bitmap_first_cluster, self.bitmap_bytes) = (0, 0);
        }
    }

    /// The volume's first sector - for `mount`-info reporting only.
    pub fn partition_lba(&self) -> u32 {
        self.partition_lba
    }

    /// Create a fresh exFAT volume (mkfs) in the partition
    /// `[start_lba, start_lba + total_sectors)` - the inverse of
    /// [`mount_at`](Self::mount_at). Lays down the main + backup boot regions
    /// (VBR, 8 extended boot sectors, OEM/reserved, and the boot checksum), a
    /// single FAT, and a cluster heap holding three contiguous system files -
    /// the allocation bitmap, the up-case table, and the root directory - with
    /// the root carrying their directory entries (a volume label, the `0x81`
    /// bitmap entry the reader locates at mount, and the `0x82` up-case entry).
    /// The disk-management arc, milestone 3 (step 2). Validated against macOS's
    /// `fsck_exfat`.
    ///
    /// Returns [`Error::DiskFull`] if the partition is too small to hold the
    /// system structures, or [`Error::Io`] on a write error.
    pub fn format(mut disk: Disk, start_lba: u32, total_sectors: u32) -> Result<(), Error> {
        const FAT_OFFSET: u32 = 24; // right after the 24-sector main+backup boot regions
        let volume_length = total_sectors as u64;
        let spc_shift = sectors_per_cluster_shift_for(total_sectors);
        let spc = 1u32 << spc_shift;
        let cluster_bytes = spc as u64 * SECTOR_SIZE as u64;

        // Size the FAT from an upper-bound cluster estimate, align the cluster
        // heap to a cluster boundary, then finalize the real cluster count.
        if total_sectors <= FAT_OFFSET + spc {
            return Err(Error::DiskFull);
        }
        let max_clusters = (total_sectors - FAT_OFFSET) >> spc_shift;
        let fat_length = (((max_clusters as u64 + 2) * 4).div_ceil(SECTOR_SIZE as u64)) as u32;
        let cluster_heap_offset = align_up(FAT_OFFSET + fat_length, spc);
        if total_sectors <= cluster_heap_offset {
            return Err(Error::DiskFull);
        }
        let cluster_count = (total_sectors - cluster_heap_offset) >> spc_shift;

        // System files, all single-cluster-runs for the sizes this targets
        // (the cluster-size table keeps the bitmap ~1 cluster even for big
        // disks): allocation bitmap, up-case table, root directory - laid out
        // contiguously from cluster 2.
        let bitmap_bytes = cluster_count.div_ceil(8) as u64;
        let bitmap_clusters = bitmap_bytes.div_ceil(cluster_bytes) as u32;
        let upcase_clusters = (UPCASE_TABLE_BYTES as u64).div_ceil(cluster_bytes) as u32;
        let first_bitmap = 2u32;
        let first_upcase = first_bitmap + bitmap_clusters;
        let root_cluster = first_upcase + upcase_clusters;
        let system_clusters = bitmap_clusters + upcase_clusters + 1; // +1 root
        if cluster_count < system_clusters + 1 {
            return Err(Error::DiskFull);
        }

        let fat_lba = start_lba + FAT_OFFSET;
        let heap_lba = start_lba + cluster_heap_offset;
        let cluster_lba = |c: u32| heap_lba + (c - 2) * spc;
        let zero = [0u8; SECTOR_SIZE];

        // --- FAT: zero it, then reserved entries + one EOC per system file ---
        for s in 0..fat_length {
            disk.write_sector((fat_lba + s) as u64, &zero)?;
        }
        // All system files are single clusters here, so each is its own EOC
        // chain; FAT[0]=media, FAT[1]=EOC. They fit in FAT sector 0.
        let mut fat0 = [0u8; SECTOR_SIZE];
        let mut put_fat = |c: u32, v: u32| {
            let off = c as usize * 4;
            fat0[off..off + 4].copy_from_slice(&v.to_le_bytes());
        };
        put_fat(0, 0xFFFF_FFF8);
        put_fat(1, END_OF_CHAIN);
        for c in first_bitmap..root_cluster + 1 {
            put_fat(c, END_OF_CHAIN);
        }
        disk.write_sector(fat_lba as u64, &fat0)?;

        // --- Allocation bitmap: zero its clusters, set the system bits ---
        for c in first_bitmap..first_bitmap + bitmap_clusters {
            for s in 0..spc {
                disk.write_sector((cluster_lba(c) + s) as u64, &zero)?;
            }
        }
        // Clusters 2..(2+system_clusters) are in use -> the first
        // `system_clusters` bits of the bitmap.
        let mut bm = [0u8; SECTOR_SIZE];
        let mut remaining = system_clusters as usize;
        let mut bit = 0usize;
        while remaining > 0 {
            bm[bit / 8] |= 1u8 << (bit % 8);
            bit += 1;
            remaining -= 1;
        }
        disk.write_sector(cluster_lba(first_bitmap) as u64, &bm)?;

        // --- Up-case table: zero its cluster, write the compressed table ---
        for s in 0..spc {
            disk.write_sector((cluster_lba(first_upcase) + s) as u64, &zero)?;
        }
        let mut upcase_buf = [0u8; UPCASE_TABLE_BYTES];
        let upcase_checksum = build_upcase_table(&mut upcase_buf);
        let mut upcase_sector = [0u8; SECTOR_SIZE];
        upcase_sector[..UPCASE_TABLE_BYTES].copy_from_slice(&upcase_buf);
        disk.write_sector(cluster_lba(first_upcase) as u64, &upcase_sector)?;

        // --- Root directory: zero the cluster, write the 3 system entries ---
        for s in 0..spc {
            disk.write_sector((cluster_lba(root_cluster) + s) as u64, &zero)?;
        }
        let mut root = [0u8; SECTOR_SIZE];
        // Volume Label entry (0x83).
        let label = b"OUROBOROS";
        root[0] = 0x83;
        root[1] = label.len() as u8;
        for (i, &c) in label.iter().enumerate() {
            root[2 + i * 2] = c; // UTF-16LE (ASCII -> low byte, high byte 0)
        }
        // Allocation Bitmap entry (0x81).
        let b = 32;
        root[b] = ET_ALLOC_BITMAP;
        root[b + 20..b + 24].copy_from_slice(&first_bitmap.to_le_bytes());
        root[b + 24..b + 32].copy_from_slice(&bitmap_bytes.to_le_bytes());
        // Up-case Table entry (0x82).
        let u = 64;
        root[u] = 0x82;
        root[u + 4..u + 8].copy_from_slice(&upcase_checksum.to_le_bytes());
        root[u + 20..u + 24].copy_from_slice(&first_upcase.to_le_bytes());
        root[u + 24..u + 32].copy_from_slice(&(UPCASE_TABLE_BYTES as u64).to_le_bytes());
        disk.write_sector(cluster_lba(root_cluster) as u64, &root)?;

        // --- Boot regions: build the 12-sector main region, checksum it, and
        // write it plus an identical backup at sector 12. ---
        let mut main_region = [[0u8; SECTOR_SIZE]; 12];
        let vbr = &mut main_region[0];
        vbr[0] = 0xEB;
        vbr[1] = 0x76;
        vbr[2] = 0x90;
        vbr[3..11].copy_from_slice(b"EXFAT   ");
        // 11..64 MustBeZero.
        vbr[64..72].copy_from_slice(&(start_lba as u64).to_le_bytes()); // PartitionOffset
        vbr[72..80].copy_from_slice(&volume_length.to_le_bytes()); // VolumeLength
        vbr[80..84].copy_from_slice(&FAT_OFFSET.to_le_bytes()); // FatOffset
        vbr[84..88].copy_from_slice(&fat_length.to_le_bytes()); // FatLength
        vbr[88..92].copy_from_slice(&cluster_heap_offset.to_le_bytes()); // ClusterHeapOffset
        vbr[92..96].copy_from_slice(&cluster_count.to_le_bytes()); // ClusterCount
        vbr[96..100].copy_from_slice(&root_cluster.to_le_bytes()); // FirstClusterOfRootDirectory
        vbr[100..104].copy_from_slice(&0x4F55_524Fu32.to_le_bytes()); // VolumeSerialNumber
        vbr[104] = 0x00; // FileSystemRevision = 1.00
        vbr[105] = 0x01;
        // 106..108 VolumeFlags = 0 (excluded from the boot checksum).
        vbr[108] = 9; // BytesPerSectorShift (512)
        vbr[109] = spc_shift;
        vbr[110] = 1; // NumberOfFats
        vbr[111] = 0x80; // DriveSelect
        vbr[112] = 0xFF; // PercentInUse = not available (excluded from checksum)
        // 113..120 Reserved.
        vbr[510] = 0x55;
        vbr[511] = 0xAA;
        // Extended boot sectors 1..=8: the ExtendedBootSignature 0x0000AA55 in
        // the last 4 bytes (stored little-endian: 55 AA 00 00).
        for s in main_region.iter_mut().take(9).skip(1) {
            s[508..512].copy_from_slice(&0x0000_AA55u32.to_le_bytes());
        }
        // Sector 9 (OEM parameters) and 10 (reserved) stay zero.
        // Sector 11: boot checksum over sectors 0..=10, excluding the VBR's
        // VolumeFlags (106,107) and PercentInUse (112) bytes.
        let mut first11 = [0u8; SECTOR_SIZE * 11];
        for (i, s) in main_region.iter().take(11).enumerate() {
            first11[i * SECTOR_SIZE..(i + 1) * SECTOR_SIZE].copy_from_slice(s);
        }
        let boot_checksum = checksum32(&first11, &[106, 107, 112]);
        for slot in main_region[11].chunks_exact_mut(4) {
            slot.copy_from_slice(&boot_checksum.to_le_bytes());
        }
        // Main region at sectors 0..12, identical backup at 12..24.
        for (i, s) in main_region.iter().enumerate() {
            disk.write_sector((start_lba + i as u32) as u64, s)?;
            disk.write_sector((start_lba + 12 + i as u32) as u64, s)?;
        }
        Ok(())
    }

    /// The first sector of `cluster`, read or written for `holder`: the one
    /// place a cluster becomes a sector, so the one check of it. Refused, an
    /// I/O error, outside the heap; for the bitmap, anywhere but the bitmap;
    /// for the root, on the bitmap or the up-case table, or off the root's
    /// recorded chain when it is whole; for anything else, on any of the
    /// volume's own clusters ([`is_system`](Self::is_system)).
    ///
    /// Each review of 2026-10-23 and 2026-10-24 found one more path reaching
    /// the volume's own clusters through a damaged set or link and added the
    /// check there (a contiguous run, a chained file's first cluster, a
    /// link); a directory's set written through a damaged chain
    /// (`write_set_at`) and a read still reached the bitmap until
    /// 2026-10-26, when the check came here. The checks on a link
    /// (`file_step`, `ensure_chained`, `write_at`'s first cluster) stay: a
    /// walk passes clusters whose sectors it never asks for, and extends a
    /// chain from the last one's FAT entry.
    fn sector_of(&self, cluster: u32, holder: Holder) -> Result<u32, Error> {
        let ok = self.in_heap(cluster)
            && match holder {
                Holder::Bitmap => self.is_bitmap(cluster),
                Holder::Root => !self.is_table(cluster) && (self.in_root(cluster) || !self.root_whole),
                Holder::Data => !self.is_system(cluster),
            };
        if !ok {
            return Err(Error::Io(DiskError::Io));
        }
        Ok(self.cluster_heap_lba + (cluster - 2) * self.sectors_per_cluster)
    }

    /// The next cluster in a FAT chain, or `None` at the end. exFAT FAT entries
    /// are a full 32 bits (no reserved top nibble, unlike FAT32).
    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>, Error> {
        if !self.in_heap(cluster) {
            return Ok(None);
        }
        let value = self.fat_entry(cluster)?;
        // A link outside the heap ends the chain too: every walk (a file's
        // read and write, a free, a directory's slots) then stops where
        // `extent_next` does, where one followed a damaged link out of the
        // heap and wrote there (the medium reviews of 2026-10-19).
        if ends_walk(value) || !self.in_heap(value) {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// The FAT sector holding `cluster`'s entry and the entry's offset in it.
    fn fat_place(&self, cluster: u32) -> (u64, usize) {
        let byte = cluster as u64 * 4;
        (self.fat_lba as u64 + byte / SECTOR_SIZE as u64, (byte % SECTOR_SIZE as u64) as usize)
    }

    /// The FAT sector at `lba`, through the one-sector [`fat_cache`](Self::fat_cache).
    fn fat_sector(&mut self, lba: u64) -> Result<&[u8; SECTOR_SIZE], Error> {
        let sector = match self.fat_cache.take() {
            Some(c) if c.lba == lba => c,
            _ => {
                let mut data = [0u8; SECTOR_SIZE];
                self.disk.read_sector(lba, &mut data)?;
                FatSector { lba, data }
            }
        };
        Ok(&self.fat_cache.insert(sector).data)
    }

    /// `cluster`'s FAT entry.
    fn fat_entry(&mut self, cluster: u32) -> Result<u32, Error> {
        let (lba, offset) = self.fat_place(cluster);
        let data = self.fat_sector(lba)?;
        Ok(u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]))
    }

    /// Advance one cluster, honouring the `contiguous` (`NoFatChain`) flag: a
    /// contiguous run is just consecutive clusters (the FAT isn't consulted),
    /// bounded by the heap's end; a fragmented one follows the FAT chain. The
    /// caller stops the walk by data length (file size / directory end marker),
    /// so a contiguous walk never over-reads into the following file.
    fn advance(&mut self, cluster: u32, contiguous: bool) -> Result<Option<u32>, Error> {
        if contiguous {
            let next = cluster + 1;
            if !self.in_heap(next) {
                Ok(None)
            } else {
                Ok(Some(next))
            }
        } else {
            self.next_cluster(cluster)
        }
    }

    /// The synthetic entry for the root directory (which has no on-disk entry
    /// set describing itself): a FAT-chained directory at `root_cluster`.
    fn root_entry(&self) -> DirEntry {
        DirEntry {
            is_dir: true,
            size: 0, // unknown; walk_dir stops at the end-of-directory marker
            valid: 0,
            first_cluster: self.root_cluster,
            contiguous: false,
            allocs: false,
        }
    }

    /// Reassemble every entry *set* in a directory into a [`DirEntry`], calling
    /// `f` for each and stopping early the first time it returns `true`. The
    /// directory's own data is a cluster run described by `(start_cluster,
    /// contiguous, dir_size)`, read as far as [`dir_extent`](Self::dir_extent)
    /// says: the root's whole chain, any other directory to its size (none at
    /// all for size 0). An entry of size 0 is given no cluster, whatever its
    /// set names (see where the set closes).
    ///
    /// A set is: a File entry (`0x85`, attributes + how many secondaries
    /// follow) then a Stream-Extension entry (`0xC0`, first cluster + data
    /// length + `NoFatChain`) then File-Name entries (`0xC1`, 15 UTF-16 chars
    /// each, up to the stream extension's `NameLength`). The cross-entry
    /// assembly state below is the exFAT analogue of `fat32::walk_dir`'s pending
    /// LFN run. `f` is given the set's name as its UTF-16 units beside the
    /// entry, which does not carry them: a `DirEntry` is copied and held
    /// several at a time (`mv`), and only a walk's callers read a name.
    fn walk_dir(
        &mut self,
        start_cluster: u32,
        contiguous: bool,
        dir_size: u64,
        mut f: impl FnMut(&DirEntry, &[u16], usize, usize) -> bool,
    ) -> Result<(), Error> {
        // The extent's rule, a cluster at a time: no FAT read until the walk
        // leaves a cluster, as when it stopped at the chain's end.
        let Some(rule) = self.extent_rule(start_cluster, contiguous, dir_size)? else {
            return Ok(());
        };
        let mut cluster = start_cluster;
        let mut read = 0u64; // clusters read
        let mut consumed: u64 = 0;
        let mut slot = 0usize; // global directory-entry slot index

        // Assembly state for the entry set currently being read.
        let mut in_set = false;
        let mut secondaries_left = 0u8;
        let mut set_start = 0usize; // slot of the set's primary (0x85) entry
        let mut set_len = 0usize; // declared entries in the set (1 + secondaries)
        let mut set_is_dir = false;
        let mut set_size = 0u64;
        let mut set_valid = 0u64;
        let mut set_cluster = 0u32;
        let mut set_contig = false;
        let mut set_allocs = false;
        let mut name = [0u16; LONG_NAME_MAX];
        let mut name_cap = 0usize; // total chars the set declares (NameLength)
        let mut name_len = 0usize; // chars filled so far

        let holder = if rule.root { Holder::Root } else { Holder::Data };
        loop {
            let lba = self.sector_of(cluster, holder)?;
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for raw in buf.chunks_exact(ENTRY_SIZE) {
                    if !rule.root && consumed >= dir_size {
                        return Ok(()); // at the size, as every driver stops
                    }
                    consumed += ENTRY_SIZE as u64;
                    let this_slot = slot;
                    slot += 1;

                    let t = raw[0];
                    if t == 0x00 {
                        return Ok(()); // end of directory
                    }
                    if t & 0x80 == 0 {
                        // Not in use (deleted / free slot). A set is
                        // contiguous, so one this cuts is no set: its slots
                        // would not be `set_start..set_start + set_len`, which
                        // every writer of a set addresses (2026-10-19).
                        in_set = false;
                        continue;
                    }
                    // The spec's shape, which the writers rely on: the Stream
                    // is the first secondary and the only one, and a primary
                    // cuts a set short (patch_stream_ext wrote its Stream
                    // fields over whatever sat at the set's second slot).
                    let first_secondary = in_set && this_slot == set_start + 1;

                    match t {
                        ET_FILE => {
                            secondaries_left = raw[1];
                            set_start = this_slot;
                            set_len = 1 + raw[1] as usize;
                            let attrs = u16::from_le_bytes([raw[4], raw[5]]);
                            set_is_dir = attrs & ATTR_DIRECTORY != 0;
                            set_size = 0;
                            set_valid = 0;
                            set_cluster = 0;
                            set_contig = false;
                            set_allocs = false;
                            name_cap = 0;
                            name_len = 0;
                            // No secondaries is no Stream: no set.
                            in_set = raw[1] != 0;
                        }
                        _ if !in_set => {
                            // A primary we ignore (bitmap 0x81, up-case 0x82,
                            // volume label 0x83) - not part of a File set -
                            // or a secondary of no set.
                        }
                        _ if t & 0x40 == 0 => in_set = false, // a primary inside a set
                        _ if (t == ET_STREAM_EXT) != first_secondary => in_set = false,
                        ET_STREAM_EXT => {
                            set_contig = raw[1] & NO_FAT_CHAIN != 0;
                            name_cap = raw[3] as usize;
                            set_valid = u64::from_le_bytes([
                                raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14],
                                raw[15],
                            ]);
                            (set_cluster, set_size) = extent(raw);
                            secondaries_left = secondaries_left.saturating_sub(1);
                        }
                        ET_FILE_NAME => {
                            // 15 UTF-16LE chars at bytes 2..32, capped by the
                            // set's declared NameLength (a name spans several
                            // 0xC1 entries).
                            let mut i = 2;
                            while i + 1 < ENTRY_SIZE
                                && name_len < name_cap
                                && name_len < LONG_NAME_MAX
                            {
                                name[name_len] = u16::from_le_bytes([raw[i], raw[i + 1]]);
                                name_len += 1;
                                i += 2;
                            }
                            secondaries_left = secondaries_left.saturating_sub(1);
                        }
                        _ => {
                            // A secondary we don't model (a vendor extension
                            // 0xE0, a vendor allocation 0xE1) - still counted
                            // so the set closes, and noted when it names
                            // clusters, which go with the set.
                            set_allocs |= allocation(raw).is_some();
                            secondaries_left = secondaries_left.saturating_sub(1);
                        }
                    }

                    // A set closes once every declared secondary is consumed.
                    if in_set && t != ET_FILE && secondaries_left == 0 {
                        in_set = false;
                        // DataLength 0 is no allocation: a cluster a tool left
                        // named in such a set may be free or another's, so no
                        // path (a write, a free, a rename, a directory's
                        // extent) takes it as this entry's (the high reviews
                        // of 2026-10-19).
                        let held = set_size > 0;
                        let entry = DirEntry {
                            is_dir: set_is_dir,
                            size: set_size,
                            // Past the size is damage, read as the size (all
                            // valid), as before the field was read. A
                            // directory's is its size, as the spec has it,
                            // whatever the disk says.
                            valid: if set_is_dir { set_size } else { set_valid.min(set_size) },
                            first_cluster: if held { set_cluster } else { 0 },
                            contiguous: held && set_contig,
                            allocs: set_allocs,
                        };
                        if f(&entry, &name[..name_len], set_start, set_len) {
                            return Ok(());
                        }
                    }
                }
            }
            read += 1;
            if !rule.root && read >= rule.sized {
                return Ok(()); // at the size, with no FAT read, as before
            }
            match self.extent_next(rule, cluster, read)? {
                Next::Cluster(next) => cluster = next,
                Next::End | Next::PastSize => return Ok(()),
            }
        }
    }

    /// The clusters of the directory at `first` (`contiguous`, `size`): the
    /// one rule every reader and writer of a directory takes its extent from,
    /// so a name a lookup cannot see is never one a create finds, nor the
    /// other way about (the review of 2026-10-18). The root is the directory
    /// at the volume's root cluster; a set naming that cluster would be read
    /// as the root, which its chain is.
    ///
    /// - The root has no size: its whole chain.
    /// - Any other directory of size 0 has none, whatever its FirstCluster
    ///   says: DataLength 0 is no allocation, and the cluster a tool left
    ///   named may be free or another file's.
    /// - A NoFatChain directory: its run, its size in clusters.
    /// - A FAT-chained directory: its chain, to its size at most (`past_size`
    ///   when the chain goes on), as every other driver reads it.
    ///
    /// A first cluster outside the heap, or a chain longer than the volume (a
    /// loop), is refused; a link outside the heap ends the chain, and a link
    /// past the size is not read further.
    fn dir_extent(&mut self, first: u32, contiguous: bool, size: u64) -> Result<Extent, Error> {
        let Some(rule) = self.extent_rule(first, contiguous, size)? else {
            return Ok(Extent { clusters: 0, tail: 0, past_size: false });
        };
        let mut extent = Extent { clusters: 1, tail: first, past_size: false };
        loop {
            match self.extent_next(rule, extent.tail, extent.clusters)? {
                Next::Cluster(next) => {
                    extent.tail = next;
                    extent.clusters += 1;
                }
                Next::End => return Ok(extent),
                Next::PastSize => {
                    extent.past_size = true;
                    return Ok(extent);
                }
            }
        }
    }

    /// The rule [`dir_extent`](Self::dir_extent) walks by, or `None` for a
    /// directory with no cluster; its first cluster checked.
    fn extent_rule(&self, first: u32, contiguous: bool, size: u64) -> Result<Option<ExtentRule>, Error> {
        let root = first == self.root_cluster && !contiguous;
        if !root && size == 0 {
            return Ok(None);
        }
        if !self.in_heap(first) {
            return Err(Error::Io(DiskError::Io));
        }
        let mut sized = size.div_ceil(self.cluster_bytes());
        if contiguous {
            // A run past the heap's end is read as far as the heap goes; it is
            // then shorter than its size, which `room` refuses to create in.
            sized = sized.min(self.cluster_count as u64 - (first as u64 - 2));
        }
        Ok(Some(ExtentRule { root, contiguous, sized }))
    }

    /// The cluster after `cluster`, the `clusters`th of a directory's extent.
    fn extent_next(&mut self, rule: ExtentRule, cluster: u32, clusters: u64) -> Result<Next, Error> {
        if rule.contiguous {
            return Ok(if clusters < rule.sized { Next::Cluster(cluster + 1) } else { Next::End });
        }
        let next = self.fat_entry(cluster)?;
        if ends_walk(next) {
            return Ok(Next::End);
        }
        if !rule.root && clusters >= rule.sized {
            return Ok(Next::PastSize);
        }
        if !self.in_heap(next) {
            // A link outside the heap ends the chain, as `next_cluster` ends
            // every walk there: one bad link in the root's chain refused every
            // lookup on the volume. A subdirectory cut short by one is then
            // shorter than its size, which `room` refuses to create in.
            return Ok(Next::End);
        }
        if clusters >= self.cluster_count as u64 {
            return Err(Error::Io(DiskError::Io)); // a loop
        }
        Ok(Next::Cluster(next))
    }

    fn in_heap(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster - 2 < self.cluster_count
    }

    /// A volume with no bitmap (none in its root, or one
    /// [`check_extents`](Self::check_extents) refused) is read-only: every
    /// operation that writes asks this first, before it writes anything.
    /// Asked where a removal frees, a replacing `mv` had written its new set
    /// first and left two, and a truncation had emptied the file first and
    /// lost it (the resumed max review of 2026-10-23); and a create or
    /// non-replacing `mv`, which takes no cluster, wrote.
    fn writable(&self) -> Result<(), Error> {
        if self.bitmap_first_cluster == 0 {
            return Err(Error::ReadOnly);
        }
        Ok(())
    }

    /// One of the volume's own clusters: the allocation bitmap's, the up-case
    /// table's, or the root directory's ([`root_runs`](Self::root_runs)). No
    /// set names them, so an allocation that reaches one is damage, and
    /// freeing it would let the allocator hand out its own bitmap (the max
    /// review of 2026-10-23, after ext2's `is_metadata`). Until 2026-10-26
    /// the root's first cluster alone was known.
    fn is_system(&self, cluster: u32) -> bool {
        self.in_root(cluster) || self.is_table(cluster)
    }

    /// One of the bitmap's or the up-case table's clusters.
    fn is_table(&self, cluster: u32) -> bool {
        self.is_bitmap(cluster) || run_has(self.upcase_first_cluster, self.upcase_bytes, self.cluster_bytes(), cluster)
    }

    /// One of the bitmap's clusters.
    fn is_bitmap(&self, cluster: u32) -> bool {
        run_has(self.bitmap_first_cluster, self.bitmap_bytes, self.cluster_bytes(), cluster)
    }

    /// Whether the contiguous run of `n` clusters from `first` holds one of
    /// the volume's own ([`is_system`](Self::is_system)), tested against
    /// their runs at once, where it asked `is_system` a cluster at a time
    /// (the fourth high review of 2026-10-24).
    fn meets_system(&self, first: u32, n: u64) -> bool {
        let cb = self.cluster_bytes();
        self.meets_root(first, n)
            || runs_meet(first, n, self.bitmap_first_cluster, self.bitmap_bytes.div_ceil(cb))
            || runs_meet(first, n, self.upcase_first_cluster, self.upcase_bytes.div_ceil(cb))
    }

    /// Resolve an absolute, `/`-separated path to its entry set. Matching is
    /// case-insensitive (ASCII case-fold, see the module doc). Unlike FAT32
    /// there's no `.`/`..`/cluster-0-means-root quirk to special-case.
    fn find(&mut self, path: &str) -> Result<DirEntry, Error> {
        Ok(self.find_at(path)?.0)
    }

    /// [`find`](Self::find), and where the entry's set lies (`None` for the root).
    fn find_at(&mut self, path: &str) -> Result<(DirEntry, Option<SetAt>), Error> {
        let mut current = self.root_entry();
        let mut at = None;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            if !current.is_dir {
                return Err(Error::NotADirectory);
            }
            let (entry, set) = self.entry_in(&current, component)?.ok_or(Error::NotFound)?;
            at = Some(set);
            current = entry;
        }
        Ok((current, at))
    }

    /// The directory at `path`, to create an entry in or remove
    /// (`NotADirectory` for a file).
    fn dir_at(&mut self, path: &str) -> Result<Dir, Error> {
        let (entry, at) = self.find_at(path)?;
        if !entry.is_dir {
            return Err(Error::NotADirectory);
        }
        Ok(Dir { entry, at })
    }

    /// Where a create in `dir` may write, or an I/O error for a directory
    /// whose chain or run is not exactly its size, as Linux refuses it.
    ///
    /// fsd grew directories until 2026-10-18 without raising their size, so
    /// such a directory holds sets past its size that no reader sees. A
    /// create there checked its name at the size and wrote a second set under
    /// a hidden one's name; taking the chain as the size on a write alone
    /// (2026-10-18, the review's second pass) made reads and writes disagree,
    /// so an append that saw no file truncated it; and adopting clusters past
    /// the size everywhere took a stale link to a cluster fsd had since
    /// allocated elsewhere. A size that is not whole clusters leaves room past
    /// it no reader sees either. So a create, and `rmdir`, are refused there
    /// until a fsck repairs the directory; a change to a set inside the size
    /// (an overwrite, an append, a removal) is not a create and goes ahead.
    fn room(&mut self, dir: &Dir) -> Result<Room, Error> {
        let Some(at) = dir.at else {
            return Ok(Room::Root);
        };
        let e = &dir.entry;
        let extent = self.dir_extent(e.first_cluster, e.contiguous, e.size)?;
        if extent.clusters > 0 && (extent.past_size || extent.clusters * self.cluster_bytes() != e.size) {
            return Err(Error::Io(DiskError::Io));
        }
        Ok(Room::Sub(at, extent))
    }

    fn cluster_bytes(&self) -> u64 {
        self.sectors_per_cluster as u64 * SECTOR_SIZE as u64
    }

    /// Lists a directory, calling `f(name, is_dir)` for each entry until
    /// it answers `Break`. `""` or `"/"` lists the root. Mirrors
    /// `fat32::Fs::list_dir`.
    pub fn list_dir(
        &mut self,
        path: &str,
        mut f: impl FnMut(&str, bool) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let dir = self.find(path)?;
        if !dir.is_dir {
            return Err(Error::NotADirectory);
        }
        let mut shown = [0u8; NAME_SHOWN_MAX];
        self.walk_dir(dir.first_cluster, dir.contiguous, dir.size, |entry, units, _slot, _len| {
            f(utf8_name(units, &mut shown), entry.is_dir).is_break()
        })
    }

    /// Metadata for one path: size and directory flag. Timestamps aren't yet
    /// decoded from the exFAT File entry, so `time` is `None`. Backs `ls -l`.
    pub fn stat(&mut self, path: &str) -> Result<crate::vfs::Stat, Error> {
        let e = self.find(path)?;
        Ok(crate::vfs::Stat {
            size: e.size,
            is_dir: e.is_dir,
            time: None,
            mode: None, // exFAT has no owner/permission model
        })
    }

    /// Reads a file into `buf` (up to `buf.len()` bytes), returning its real
    /// size (saturated to `u32`), same truncation-detection contract as
    /// `fat32::Fs::read_file`.
    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        // `read_entry` from the start: one copy of the read and of its
        // ValidDataLength rule, where this kept a loop of its own. The reply
        // carries `buf` up to the size, and `read_entry` defines all of it,
        // zeros where a short chain stopped, so no earlier reply's bytes go
        // out (the reviews of 2026-10-22). The read cursor is a streaming
        // reader's, so a whole-file read leaves it as it was.
        let file = self.find_file(path)?;
        let kept = self.read_cursor;
        let read = self.read_entry(&file, 0, buf);
        self.read_cursor = kept;
        read?;
        Ok(saturate_u32(file.size))
    }

    /// Reads up to `buf.len()` bytes from byte `offset`, returning how many were
    /// copied (`0` at/past EOF). The windowed-read primitive behind
    /// `FSOP_READ_AT`/`READ_BULK`; mirrors `fat32::Fs::read_at`, with the
    /// cluster step going through [`advance`](Self::advance) so a contiguous
    /// file skips the FAT.
    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        let file = self.find_file(path)?;
        self.read_entry(&file, offset, buf)
    }

    /// Read up to `want` bytes from `offset` in pieces of at most `buf.len()`,
    /// handing each to `sink`, with `path` resolved once for all of them
    /// ([`crate::vfs::read_pieces`]). Until 2026-10-17 a large read was one
    /// request a piece, each resolving the path from the root again.
    pub fn read_each(
        &mut self,
        path: &str,
        offset: u64,
        want: u64,
        buf: &mut [u8],
        sink: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<(u64, bool), Error> {
        let file = self.find_file(path)?;
        crate::vfs::read_pieces(offset, want, buf, |o, b| self.read_entry(&file, o, b), sink)
    }

    /// `path`'s entry, refused [`Error::NotAFile`] when it is a directory.
    fn find_file(&mut self, path: &str) -> Result<DirEntry, Error> {
        let file = self.find(path)?;
        if file.is_dir {
            return Err(Error::NotAFile);
        }
        Ok(file)
    }

    /// [`read_at`](Self::read_at) of a file already found. The bytes past its
    /// ValidDataLength are zeros, read off no cluster: the spec's rule, which
    /// until 2026-10-22 was not kept, so a file another driver preallocated
    /// read the stale bytes its clusters held as its own.
    fn read_entry(&mut self, file: &DirEntry, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        let total = file.size;
        if offset >= total || buf.is_empty() {
            return Ok(0);
        }
        let whole = ((total - offset) as usize).min(buf.len());
        let want = file.valid.saturating_sub(offset).min(whole as u64) as usize;
        // All of `buf[..whole]` zeroed first, so it is defined however the
        // read ends: zeros past the valid length, and zeros where a chain
        // shorter than the size stopped, or a damaged entry names no cluster
        // (the count returned says where).
        buf[..whole].fill(0);
        if file.first_cluster == 0 {
            return Ok(0);
        }
        if want == 0 {
            return Ok(whole as u32);
        }
        let offset = offset as usize;

        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        // From the cursor when it is this file's and not past `offset`, else
        // from the first cluster: the same walk either way, only shorter.
        let (mut cluster, mut cluster_pos) =
            ChainCursor::start(self.read_cursor, file.first_cluster, file.contiguous, offset);
        while cluster_pos + cluster_bytes <= offset {
            match self.advance(cluster, file.contiguous)? {
                Some(next) => cluster = next,
                None => return Ok(0), // chain shorter than the size field claims
            }
            cluster_pos += cluster_bytes;
        }

        let mut written = 0usize;
        'clusters: loop {
            let lba = self.sector_of(cluster, Holder::Data)?;
            for s in 0..self.sectors_per_cluster {
                if written >= want {
                    break 'clusters;
                }
                let sector_pos = cluster_pos + s as usize * SECTOR_SIZE;
                if sector_pos + SECTOR_SIZE <= offset {
                    continue; // entirely before the requested window
                }
                let mut sector = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut sector)?;
                let from = offset.max(sector_pos) - sector_pos;
                let n = (SECTOR_SIZE - from).min(want - written);
                buf[written..written + n].copy_from_slice(&sector[from..from + n]);
                written += n;
            }
            if written >= want {
                break;
            }
            match self.advance(cluster, file.contiguous)? {
                Some(next) => cluster = next,
                None => break,
            }
            cluster_pos += cluster_bytes;
        }
        // `cluster` and `cluster_pos` only ever moved together, so they still
        // name one cluster of this file and the bytes it holds.
        self.read_cursor = Some(ChainCursor {
            file_cluster: file.first_cluster,
            contiguous: file.contiguous,
            cluster,
            cluster_pos,
        });
        // A chain shorter than its valid bytes ends the read there, zeros or not.
        Ok(if written == want { whole } else { written } as u32)
    }

    // ---- write infrastructure (Stage A) -------------------------------------
    //
    // exFAT allocation differs from FAT32 in one structural way: free clusters
    // are tracked by an allocation *bitmap*, not by scanning the FAT for zeros.
    // Newly-created files/dirs are always FAT-*chained* here (`NoFatChain = 0`),
    // never pure-contiguous, so allocation is the direct parallel of FAT32's
    // `write_chain`: set the bitmap bit *and* link the FAT. That keeps the two
    // structures consistent and lets the reader's `advance()` walk the chain.
    // Same corruption discipline as the FAT32 write arc: claim before use, and
    // (in later stages) write the new thing before freeing the old.

    /// Writes one 32-bit FAT entry (exFAT has a single active FAT). `value` is a
    /// next-cluster number, `END_OF_CHAIN`, or `0` to free.
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), Error> {
        // A cluster outside the heap has no entry to write: past its end the
        // FAT's sectors run on into whatever follows it (FAT32's
        // `check_in_heap`, which this lacked until 2026-10-23, when a
        // contiguous file sized past the heap had `ensure_chained` link it on).
        if !self.in_heap(cluster) {
            return Err(Error::Io(DiskError::Io));
        }
        // Every FAT entry written goes through here, so the cursors are
        // dropped here when a walk would go on from this cluster:
        // `fat32.rs`'s rule and reason, asked of `next_cluster` itself. An
        // entry where every walk stopped (`0` or end of chain) had no
        // FAT-chained cursor past it, so writing it (an allocation, a tail
        // extension) moves no cluster of any walk. A cursor on a contiguous
        // file never read the FAT, and its file's clusters are reused only by
        // a FAT-chained file, which its `contiguous` key does not match.
        // `make test-stale-cursor` builds stale cases by removal; a
        // truncation or a relink has no sequence there (fsd makes neither).
        if self.next_cluster(cluster)?.is_some() {
            self.read_cursor = None;
            self.write_cursor = None;
        }
        let (lba, offset) = self.fat_place(cluster);
        let mut buf = *self.fat_sector(lba)?;
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        self.disk.write_sector(lba, &buf)?;
        self.fat_cache = Some(FatSector { lba, data: buf });
        Ok(())
    }

    /// Zeroes every sector of a cluster - a fresh directory cluster must read
    /// back as all-`0x00` so its first slot is the end-of-directory marker.
    fn zero_cluster(&mut self, cluster: u32) -> Result<(), Error> {
        let lba = self.sector_of(cluster, Holder::Data)?;
        let zero = [0u8; SECTOR_SIZE];
        for s in 0..self.sectors_per_cluster {
            self.disk.write_sector((lba + s) as u64, &zero)?;
        }
        Ok(())
    }

    /// LBA of the bitmap sector holding `byte_index` (bitmap assumed
    /// contiguous, see the field doc). The bitmap starts at
    /// `bitmap_first_cluster`. Refused with [`Error::Io`] for a byte past the bitmap's own length or a
    /// cluster of it outside the heap: both come from the root directory's
    /// bitmap entry, unchecked, and every bitmap read and write is made here,
    /// so a damaged entry (or none: a first cluster of 0, which the free
    /// path once took to sector `heap - 2 clusters`) cannot reach past the
    /// heap (the second review of #330; FAT32's `cluster_to_lba` the same).
    /// The heap's bound is [`sector_of`](Self::sector_of)'s.
    fn bitmap_sector_lba(&self, byte_index: u64) -> Result<u64, Error> {
        let cluster_bytes = self.cluster_bytes();
        let cluster = self.bitmap_first_cluster as u64 + byte_index / cluster_bytes;
        if byte_index >= self.bitmap_bytes || cluster > u32::MAX as u64 {
            return Err(Error::Io(DiskError::Io));
        }
        let lba = self.sector_of(cluster as u32, Holder::Bitmap)?;
        Ok(lba as u64 + (byte_index % cluster_bytes) / SECTOR_SIZE as u64)
    }

    /// Allocate one cluster: find a free bit in the allocation bitmap, set it,
    /// mark the cluster end-of-chain in the FAT, and return it. `DiskFull` if
    /// the bitmap is full or wasn't located at mount. A clear bit of one of
    /// the volume's own clusters ([`is_system`](Self::is_system): damage,
    /// another driver's or an older fsd's free) is set as it is passed over,
    /// never handed out, so file bytes never land on the bitmap itself:
    /// ext2's allocator passes over its metadata the same way.
    fn alloc_cluster(&mut self) -> Result<u32, Error> {
        if self.bitmap_first_cluster == 0 {
            return Err(Error::DiskFull);
        }
        let nbytes = self.cluster_count.div_ceil(8) as u64;
        let mut byte_index = 0u64;
        let mut sector_lba = u64::MAX;
        let mut buf = [0u8; SECTOR_SIZE];
        while byte_index < nbytes {
            let lba = self.bitmap_sector_lba(byte_index)?;
            if lba != sector_lba {
                self.disk.read_sector(lba, &mut buf)?;
                sector_lba = lba;
            }
            let off = (byte_index % SECTOR_SIZE as u64) as usize;
            if buf[off] != 0xff {
                let bitpos = buf[off].trailing_ones(); // first 0 bit
                let cluster = (byte_index as u32) * 8 + bitpos + 2;
                if !self.in_heap(cluster) {
                    break; // past the last real cluster
                }
                buf[off] |= 1u8 << bitpos;
                self.disk.write_sector(lba, &buf)?;
                if self.is_system(cluster) {
                    continue; // marked used, and this byte looked at again
                }
                self.write_fat_entry(cluster, END_OF_CHAIN)?;
                return Ok(cluster);
            }
            byte_index += 1;
        }
        Err(Error::DiskFull)
    }

    /// Free a data allocation (none for a first cluster outside the heap: an
    /// entry of size 0 has none, `walk_dir` giving it no cluster, and a
    /// damaged one names none of the volume's): clear each cluster's bitmap
    /// bit (and, for a FAT-chained allocation, its FAT entry). Handles both a
    /// file we created (FAT-chained) and a contiguous one another tool wrote
    /// (`NoFatChain`, walked by count from `size`). Either stops at the
    /// volume's own clusters ([`is_system`](Self::is_system)): what lies past
    /// is damage, left allocated, a leak a fsck finds. A contiguous run that
    /// does not fit in the heap is damage whole, and none of it is freed: a
    /// DataLength of a terabyte cleared every bit to the heap's end, every
    /// live file's past the run's first cluster (the resumed max review of
    /// 2026-10-23).
    fn free_data(&mut self, first: u32, contiguous: bool, size: u64) -> Result<(), Error> {
        if !self.in_heap(first) {
            return Ok(());
        }
        let n = size.div_ceil(self.cluster_bytes());
        if contiguous {
            if !run_fits(first, size, self.cluster_bytes(), self.cluster_count) {
                return Ok(());
            }
            let mut end = first;
            while ((end - first) as u64) < n && !self.is_system(end) {
                end += 1;
            }
            self.bitmap_clear_run(first, end)
        } else {
            // To its size, at most: a link past it (a crash between a chain's
            // extension and its size write, or another tool) may name a
            // cluster since given to another file, and a loop has no end.
            let mut c = first;
            for i in 0..n {
                if self.is_system(c) {
                    break;
                }
                let next = self.next_cluster(c)?;
                self.bitmap_clear_run(c, c + 1)?;
                self.write_fat_entry(c, 0)?;
                match next {
                    Some(next) if i + 1 < n => c = next,
                    _ => break,
                }
            }
            Ok(())
        }
    }

    /// Clear the bitmap bits of clusters `first..end` (all in the heap), one
    /// read and one write a bitmap sector, which holds 4,096 clusters' bits:
    /// a cluster at a time, a contiguous run of thousands (a camera's file on
    /// the Pi's stick, each sector a USB round trip) kept `rm` past the
    /// supervisor's wedge limit, with the set still live over what it had
    /// cleared (the max review of 2026-10-23).
    fn bitmap_clear_run(&mut self, first: u32, end: u32) -> Result<(), Error> {
        let mut buf = [0u8; SECTOR_SIZE];
        let mut c = first;
        while c < end {
            let lba = self.bitmap_sector_lba(((c - 2) / 8) as u64)?;
            self.disk.read_sector(lba, &mut buf)?;
            while c < end && self.bitmap_sector_lba(((c - 2) / 8) as u64)? == lba {
                let bit = c - 2;
                buf[(bit / 8) as usize % SECTOR_SIZE] &= !(1u8 << (bit % 8));
                c += 1;
            }
            self.disk.write_sector(lba, &buf)?;
        }
        Ok(())
    }

    /// A zeroed cluster, allocated, and given back if the zeroing fails.
    fn alloc_zeroed(&mut self) -> Result<u32, Error> {
        let cluster = self.alloc_cluster()?;
        if let Err(e) = self.zero_cluster(cluster) {
            let _ = self.free_data(cluster, false, self.cluster_bytes());
            return Err(e);
        }
        Ok(cluster)
    }

    // ---- directory-entry-set writing ----------------------------------------

    /// The on-disk (LBA, byte offset) of a directory's global slot `slot`
    /// (0-based over the whole directory), or `None` if it's past the current
    /// allocation, found from `cur`: forward from the cluster it holds, from
    /// the directory's first only for a slot before it. Every walk over a
    /// directory's slots in order keeps one cursor, so it walks the chain once;
    /// until 2026-10-19 each slot walked it from the first cluster again, and a
    /// free-slot scan read its sector again too, about 19,000 sector reads to
    /// make 100 files in one directory.
    fn slot_at(&mut self, cur: &mut SlotCursor, slot: usize) -> Result<Option<(u64, usize)>, Error> {
        if cur.first < 2 {
            return Ok(None); // a directory of size 0 has no cluster
        }
        if slot < cur.base {
            *cur = SlotCursor::new(cur.first, cur.contiguous);
        }
        let slots_per_cluster = self.sectors_per_cluster as usize * SLOTS_PER_SECTOR;
        while slot >= cur.base + slots_per_cluster {
            match self.advance(cur.cluster, cur.contiguous)? {
                Some(next) => {
                    cur.cluster = next;
                    cur.base += slots_per_cluster;
                }
                None => return Ok(None),
            }
        }
        let local = slot - cur.base;
        // The root's slots are the root's, by `extent_rule`'s test of a
        // directory for the root.
        let holder = if cur.first == self.root_cluster && !cur.contiguous { Holder::Root } else { Holder::Data };
        let lba = self.sector_of(cur.cluster, holder)? as u64 + (local / SLOTS_PER_SECTOR) as u64;
        Ok(Some((lba, (local % SLOTS_PER_SECTOR) * ENTRY_SIZE)))
    }

    /// Grow a directory past its extent by a cluster, and record its new size.
    ///
    /// The root has no size of its own anywhere (its chain is its extent): a
    /// zeroed cluster is linked to its chain's tail. Any other directory's
    /// DataLength and ValidDataLength, in its set in its parent, are where
    /// other drivers stop, so they are raised by the cluster; until 2026-10-18
    /// they were not, and entries written past the first cluster could not be
    /// found. The size is the chain's on entry (`room`).
    ///
    /// A directory with no cluster is given its first. A `NoFatChain`
    /// directory (as macOS and Windows make them) is first given a FAT chain
    /// over its run, as `write_at` does for a file, and its set is written
    /// FAT-chained at once, before anything is linked: a failure after the
    /// link and the size write is undone, and should the undoing fail too, the
    /// chain past the size is refused by the next `room` rather than lost,
    /// where a set still saying `NoFatChain` would have its run ended again
    /// by the next growth and the linked cluster lost.
    ///
    /// `cur`, the caller's walk over the directory's slots, is kept true to
    /// it here, where the directory changes, so no cursor outlives a growth
    /// stale: made anew when the directory got its first cluster, told its
    /// run is a FAT chain now (`ensure_chained` chains the run in place, so
    /// where the cursor stands is still a slot of the directory), and kept as
    /// it is when a cluster was linked after the tail.
    fn grow_dir(&mut self, e: &mut DirEntry, room: &mut Room, cur: &mut SlotCursor) -> Result<(), Error> {
        self.grow_extent(e, room)?;
        if cur.first != e.first_cluster {
            *cur = SlotCursor::new(e.first_cluster, e.contiguous);
        } else {
            cur.contiguous = e.contiguous;
        }
        Ok(())
    }

    /// [`grow_dir`](Self::grow_dir)'s work on the disk and on `e` and `room`.
    fn grow_extent(&mut self, e: &mut DirEntry, room: &mut Room) -> Result<(), Error> {
        let cluster_bytes = self.cluster_bytes();
        let Room::Sub(at, x) = room else {
            // The new cluster joins the root's record, or no write could
            // reach it (`sector_of`): refused when the record has no room
            // for it, and given back then or when its link fails, as the
            // subdirectory's below is (until the high review of 2026-10-26
            // a failed link leaked it, and a full record refused before the
            // allocation, though a cluster after the last run needs none).
            let root = self.dir_extent(e.first_cluster, false, 0)?;
            let new = self.alloc_zeroed()?;
            let linked = if self.root_takes(new) { self.write_fat_entry(root.tail, new) } else { Err(Error::DiskFull) };
            if let Err(err) = linked {
                let _ = self.free_data(new, false, cluster_bytes);
                return Err(err);
            }
            self.root_push(new);
            return Ok(());
        };
        let at = *at;
        if x.clusters == 0 {
            let new = self.alloc_zeroed()?;
            if let Err(err) = self.patch_stream_ext(at, new, cluster_bytes, cluster_bytes) {
                let _ = self.free_data(new, false, cluster_bytes);
                return Err(err);
            }
            e.first_cluster = new;
            e.contiguous = false;
            e.size = cluster_bytes;
            e.valid = cluster_bytes;
            *x = Extent { clusters: 1, tail: new, past_size: false };
            return Ok(());
        }
        if e.contiguous {
            let size = x.clusters * cluster_bytes;
            self.ensure_chained(e.first_cluster, true, size)?;
            self.patch_stream_ext(at, e.first_cluster, size, size)?;
            e.contiguous = false;
        }
        let new = self.alloc_zeroed()?;
        self.write_fat_entry(x.tail, new)?;
        let size = (x.clusters + 1) * cluster_bytes;
        if let Err(err) = self.patch_stream_ext(at, e.first_cluster, size, size) {
            // Undone, or the chain would run past the size, and `room` would
            // refuse every later create in the directory until a fsck (the
            // medium review of 2026-10-19): one failed write, a locked
            // directory, on the Pi with no fsck at hand.
            let _ = self.write_fat_entry(x.tail, END_OF_CHAIN);
            let _ = self.free_data(new, false, cluster_bytes);
            return Err(err);
        }
        x.clusters += 1;
        x.tail = new;
        e.size = size;
        e.valid = size;
        Ok(())
    }

    /// Global slot index of the first of `need` consecutive available slots in a
    /// directory (available = end-marker `0x00` or a deleted entry), growing the
    /// directory by a cluster whenever the walk runs off its extent. Bounded by
    /// the extent since 2026-10-18: a contiguous directory's walk ran on into
    /// the clusters after it, and the slots there were taken as free and
    /// written, another file's bytes or a free cluster.
    fn find_free_run(&mut self, e: &mut DirEntry, room: &mut Room, need: usize) -> Result<usize, Error> {
        let mut run_start = 0usize;
        let mut run = 0usize;
        let mut slot = 0usize;
        let mut cur = SlotCursor::new(e.first_cluster, e.contiguous);
        // The sector last read: sixteen slots are tested from one read.
        let mut buf = [0u8; SECTOR_SIZE];
        let mut held = None;
        loop {
            // The root's scan ends where its chain does (slot_at), as its
            // extent does.
            let slots_per_cluster = self.sectors_per_cluster as u64 * SLOTS_PER_SECTOR as u64;
            let inside = match room {
                Room::Root => true,
                Room::Sub(_, x) => (slot as u64) < x.clusters * slots_per_cluster,
            };
            let addr = if inside { self.slot_at(&mut cur, slot)? } else { None };
            match addr {
                Some((lba, off)) => {
                    if held != Some(lba) {
                        self.disk.read_sector(lba, &mut buf)?;
                        held = Some(lba);
                    }
                    let t = buf[off];
                    if t == 0x00 || t & ET_IN_USE == 0 {
                        if run == 0 {
                            run_start = slot;
                        }
                        run += 1;
                        if run >= need {
                            return Ok(run_start);
                        }
                    } else {
                        run = 0;
                    }
                    slot += 1;
                }
                None => {
                    // Then re-read this slot, past the old end: grow_dir keeps
                    // the cursor true to the directory, and no sector is held
                    // (the new cluster was zeroed after it was read).
                    self.grow_dir(e, room, &mut cur)?;
                    held = None;
                }
            }
        }
    }

    /// Rewrite the `run.len` consecutive slots of a directory from `run.slot`,
    /// one read-modify-write a sector, in order: `f(sector, i, off, k)` changes
    /// the run's slots `i..i + k`, which lie at byte `off` of the sector read.
    /// The one copy of the arithmetic that splits a run at sector edges (a
    /// sector belongs to one cluster, so a run's slots in it are adjacent),
    /// for [`write_set_at`](Self::write_set_at) and
    /// [`unlink_set`](Self::unlink_set). `missing` is the error for a slot past
    /// the directory's clusters.
    fn rewrite_slots(
        &mut self,
        run: SetAt,
        missing: fn() -> Error,
        mut f: impl FnMut(&mut [u8; SECTOR_SIZE], usize, usize, usize),
    ) -> Result<(), Error> {
        let mut cur = SlotCursor::new(run.dir_cluster, run.dir_contiguous);
        let mut i = 0;
        while i < run.len {
            let (lba, off) = self.slot_at(&mut cur, run.slot + i)?.ok_or_else(missing)?;
            let k = (run.len - i).min((SECTOR_SIZE - off) / ENTRY_SIZE);
            let mut sec = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut sec)?;
            f(&mut sec, i, off, k);
            self.disk.write_sector(lba, &sec)?;
            i += k;
        }
        Ok(())
    }

    /// Write `set` (a built entry set, `set.len()/32` entries) into
    /// consecutive slots starting at global slot `start`: one read-modify-write
    /// per sector it touches, in order (slots may straddle sector/cluster
    /// edges), where it was one a slot until 2026-10-19.
    fn write_set_at(
        &mut self,
        dir_cluster: u32,
        contiguous: bool,
        start: usize,
        set: &[u8],
    ) -> Result<(), Error> {
        let run = SetAt { dir_cluster, dir_contiguous: contiguous, slot: start, len: set.len() / ENTRY_SIZE };
        self.rewrite_slots(run, || Error::DiskFull, |sec, i, off, k| {
            sec[off..off + k * ENTRY_SIZE].copy_from_slice(&set[i * ENTRY_SIZE..(i + k) * ENTRY_SIZE]);
        })
    }

    /// Build an entry set and insert it into a directory: the full create path
    /// shared by `touch`/`mkdir`/`write_file` (a rename writes its set through
    /// [`relink`](Self::relink)). `data_len` is the file's
    /// size (or a directory's allocated size); `first_cluster` its first cluster
    /// (`0` for an empty file). `room` is the directory's [`room`](Self::room),
    /// which a caller takes before it allocates anything for the new entry: a
    /// create refused after an allocation leaked it (the high review of
    /// 2026-10-19).
    ///
    /// A failure says whether any of the set reached the disk: until then the
    /// caller may give back what it allocated for the entry, after that a
    /// partly written set may name it, and it is left (a leak a fsck finds,
    /// where a cluster freed under a set is a cross-link to come).
    #[allow(clippy::too_many_arguments)] // a directory, its room + a full entry description
    fn create_entry(
        &mut self,
        dir: &mut Dir,
        room: Room,
        name: &str,
        is_dir: bool,
        first_cluster: u32,
        data_len: u64,
        data_contiguous: bool,
    ) -> Result<(), CreateFailed> {
        let unwritten = |error| CreateFailed { error, written: false };
        let mut set = [0u8; MAX_SET_BYTES];
        let entries = build_entry_set(name, is_dir, first_cluster, data_len, data_contiguous, &mut set)
            .ok_or(Error::InvalidName)
            .map_err(unwritten)?;
        self.insert_set(dir, room, &set[..entries * ENTRY_SIZE])
    }

    /// Insert a built entry set into a directory: [`create_entry`](Self::create_entry)'s tail,
    /// and a rename's, whose set is [`renamed_set`](Self::renamed_set).
    fn insert_set(&mut self, dir: &mut Dir, mut room: Room, set: &[u8]) -> Result<(), CreateFailed> {
        let start = self
            .find_free_run(&mut dir.entry, &mut room, set.len() / ENTRY_SIZE)
            .map_err(|error| CreateFailed { error, written: false })?;
        self.write_set_at(dir.entry.first_cluster, dir.entry.contiguous, start, set)
            .map_err(|error| CreateFailed { error, written: true })
    }

    /// The entries of the set at `at`, from its entry `from` on, each given
    /// to `f` with its index in the set, until `f` answers `Break`.
    /// One read a sector the set lies in. For a reader that wants the bytes
    /// alone ([`renamed_set`](Self::renamed_set), [`runs_of`](Self::runs_of)); `patch_stream_ext`, which writes back
    /// where it read, walks the slots itself.
    fn read_slots(
        &mut self,
        at: SetAt,
        from: usize,
        mut f: impl FnMut(usize, &[u8]) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let mut cur = SlotCursor::new(at.dir_cluster, at.dir_contiguous);
        let mut sec = [0u8; SECTOR_SIZE];
        let mut sec_lba = None;
        for i in from..at.len {
            let (lba, off) = self.slot_at(&mut cur, at.slot + i)?.ok_or(Error::NotFound)?;
            if sec_lba != Some(lba) {
                self.disk.read_sector(lba, &mut sec)?;
                sec_lba = Some(lba);
            }
            if f(i, &sec[off..off + ENTRY_SIZE]).is_break() {
                break;
            }
        }
        Ok(())
    }

    /// The set a rename writes for `entry`, whose set lies at `at`, under
    /// `name`, into `buf`; returns its entry count. The File entry and every
    /// secondary past the name are the old set's, as the disk holds them:
    /// its attributes and times, and vendor extension (0xE0) and vendor
    /// allocation (0xE1) entries a camera wrote, which a rename built afresh
    /// dropped until 2026-10-21, leaking a vendor allocation's clusters. So is
    /// the Stream, but for its NameLength and NameHash: its ValidDataLength
    /// below a preallocated DataLength, raised to it, would read the stale
    /// bytes past it as the file's. A size-0 entry's Stream is built as for
    /// a create, so it still names no cluster. The name entries are new, and
    /// every old one goes, wherever among the secondaries it sat. A name
    /// that would take the set past 255 secondaries is refused.
    fn renamed_set(
        &mut self,
        at: SetAt,
        entry: &DirEntry,
        name: &str,
        buf: &mut [u8; WHOLE_SET_ENTRIES * ENTRY_SIZE],
    ) -> Result<usize, Error> {
        let mut built = [0u8; MAX_SET_BYTES];
        let made = build_entry_set(name, entry.is_dir, entry.first_cluster, entry.size, entry.contiguous, &mut built)
            .ok_or(Error::InvalidName)?;
        if at.len < 2 || at.len > WHOLE_SET_ENTRIES {
            return Err(Error::Io(DiskError::Io));
        }
        self.read_slots(at, 0, |i, raw| {
            buf[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE].copy_from_slice(raw);
            ControlFlow::Continue(())
        })?;
        // `walk_dir` closes a set only in the spec's shape: File, Stream, the
        // name, then any other secondaries. A check that the slots still hold
        // it, as in patch_stream_ext.
        if buf[0] != ET_FILE || buf[ENTRY_SIZE] != ET_STREAM_EXT {
            return Err(Error::Io(DiskError::Io));
        }
        let mut stream = [0u8; ENTRY_SIZE];
        stream.copy_from_slice(&buf[ENTRY_SIZE..2 * ENTRY_SIZE]);
        // Every secondary but the Stream and the name entries, in order,
        // packed down from slot 2 (each read at or past where it is written).
        let mut rest = 0;
        for i in 2..at.len {
            if buf[i * ENTRY_SIZE] != ET_FILE_NAME {
                buf.copy_within(i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE, (2 + rest) * ENTRY_SIZE);
                rest += 1;
            }
        }
        let total = made + rest;
        if total > WHOLE_SET_ENTRIES {
            return Err(Error::InvalidName);
        }
        // The rest moved to follow the new name, then Stream and name over
        // what was there; the File entry's bytes past its checksum stay.
        buf.copy_within(2 * ENTRY_SIZE..(2 + rest) * ENTRY_SIZE, made * ENTRY_SIZE);
        buf[ENTRY_SIZE..made * ENTRY_SIZE].copy_from_slice(&built[ENTRY_SIZE..made * ENTRY_SIZE]);
        if entry.size > 0 {
            stream[3..6].copy_from_slice(&built[ENTRY_SIZE + 3..ENTRY_SIZE + 6]); // NameLength, NameHash
            buf[ENTRY_SIZE..2 * ENTRY_SIZE].copy_from_slice(&stream);
        }
        buf[1] = (total - 1) as u8; // SecondaryCount
        let sum = set_checksum(&buf[..total * ENTRY_SIZE]);
        buf[2..4].copy_from_slice(&sum.to_le_bytes());
        Ok(total)
    }

    /// Resolve a path's *parent* to a mountable directory entry, checking it's a
    /// directory and that `name` doesn't already exist - the shared front half
    /// of `touch`/`mkdir`. Returns the parent, to create in.
    fn parent_for_create(&mut self, path: &str) -> Result<(Dir, ExistingKind), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.dir_at(parent_path)?;
        let kind = match self.entry_in(&parent.entry, name)? {
            None => ExistingKind::None,
            Some((entry, _)) if entry.is_dir => ExistingKind::Dir,
            Some(_) => ExistingKind::File,
        };
        Ok((parent, kind))
    }

    /// The entry named `name` in directory `dir`, and where its set lies: THE
    /// one place a name in a directory picks a set (since 2026-10-19). Path
    /// resolution (`find_at`), the existence check before a create
    /// (`parent_for_create`) and every write, removal and rename go through
    /// it, so no two paths can pick two sets for one name, and no caller
    /// assembles a set's location by hand. The rule it matches by is
    /// [`name_answers`], on the set's UTF-16 and `name` encoded to it once,
    /// which shares [`upcase`] with the NameHash fsd writes. A name longer
    /// than any set's is in no directory.
    fn entry_in(&mut self, dir: &DirEntry, name: &str) -> Result<Option<(DirEntry, SetAt)>, Error> {
        let mut asked = [0u16; LONG_NAME_MAX];
        let mut n = 0usize;
        for u in name.encode_utf16() {
            if n == LONG_NAME_MAX {
                return Ok(None);
            }
            asked[n] = u;
            n += 1;
        }
        let asked = &asked[..n];
        let mut found = None;
        self.walk_dir(dir.first_cluster, dir.contiguous, dir.size, |entry, units, slot, len| {
            if name_answers(units, asked) {
                let at = SetAt { dir_cluster: dir.first_cluster, dir_contiguous: dir.contiguous, slot, len };
                found = Some((*entry, at));
                true
            } else {
                false
            }
        })?;
        Ok(found)
    }

    /// Allocate and write a fresh FAT-chained cluster chain holding `data`,
    /// returning its first cluster (`0` for empty `data`). The exFAT parallel of
    /// `fat32::write_chain`: each cluster is bitmap-claimed and FAT-end-marked by
    /// `alloc_cluster`, then linked to its predecessor.
    fn write_chain(&mut self, data: &[u8]) -> Result<u32, Error> {
        let mut first = 0u32;
        let made = self.write_chain_from(data, &mut first);
        if made.is_err() && first >= 2 {
            // Out of space or an I/O error part way: nothing names the part
            // already linked, so it is given back (it leaked until 2026-10-19).
            let _ = self.free_data(first, false, data.len() as u64);
        }
        made
    }

    /// [`write_chain`](Self::write_chain)'s body, leaving its first cluster in
    /// `first` as soon as there is one.
    fn write_chain_from(&mut self, data: &[u8], first: &mut u32) -> Result<u32, Error> {
        if data.is_empty() {
            return Ok(0);
        }
        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        let num = data.len().div_ceil(cluster_bytes);
        let mut prev = 0u32;
        let mut written = 0usize;
        for i in 0..num {
            let cluster = self.alloc_cluster()?;
            if i == 0 {
                *first = cluster;
            } else {
                self.write_fat_entry(prev, cluster)?;
            }
            let lba = self.sector_of(cluster, Holder::Data)?;
            for s in 0..self.sectors_per_cluster {
                let mut sector = [0u8; SECTOR_SIZE];
                let start = written;
                let end = (written + SECTOR_SIZE).min(data.len());
                if start < end {
                    sector[..end - start].copy_from_slice(&data[start..end]);
                }
                self.disk.write_sector((lba + s) as u64, &sector)?;
                written += SECTOR_SIZE;
            }
            prev = cluster;
        }
        Ok(*first)
    }

    /// The cluster after `cluster` in a file being written: the next of its
    /// chain while that is still the file's own (`owned`), else one appended.
    /// A link to one of the volume's own clusters is damage, refused before
    /// the write reaches it: it wrote the file's bytes into the bitmap (the
    /// fourth high review of 2026-10-24). [`sector_of`](Self::sector_of)
    /// would refuse the sector, but a walk passes a cluster it does not
    /// write, and the step after it would link a new cluster from the
    /// bitmap's FAT entry.
    fn file_step(&mut self, cluster: u32, owned: bool) -> Result<u32, Error> {
        match self.next_cluster(cluster)? {
            Some(next) if owned && self.is_system(next) => Err(Error::Io(DiskError::Io)),
            Some(next) if owned => Ok(next),
            _ => self.extend_chain(cluster),
        }
    }

    /// Append one freshly allocated cluster after `tail`, returning it - the
    /// exFAT `fat32::extend_chain`.
    fn extend_chain(&mut self, tail: u32) -> Result<u32, Error> {
        let new = self.alloc_cluster()?; // bitmap + FAT-end
        self.write_fat_entry(tail, new)?;
        Ok(new)
    }

    /// Convert a contiguous (`NoFatChain`) allocation into a FAT chain in place,
    /// so it can be extended by `extend_chain`. Writes FAT links across the
    /// existing consecutive clusters; the caller then clears the entry's
    /// `NoFatChain` flag (which `patch_stream_ext` does). No-op if already
    /// chained. The bitmap is unaffected (the clusters stay allocated).
    ///
    /// An entry of size 0 never comes here: `walk_dir` gives it no cluster,
    /// whatever its set names (since 2026-10-19). Until then this ended the
    /// one cluster such an entry named, whose FAT entry a tool leaves
    /// undefined, after a write following it as a link went into whatever
    /// cluster it named (the review of #316).
    ///
    /// A run that does not fit in the heap is damage, refused before any
    /// link is written, as [`free_data`](Self::free_data) frees none of one:
    /// `write_fat_entry` refused only the first cluster past the heap, after
    /// every link inside it was written, over the links of the files whose
    /// clusters the run's size reached (the high review of 2026-10-24). So is
    /// one over the volume's own clusters ([`is_system`](Self::is_system)):
    /// its links were written over the bitmap's, and `write_at` then wrote
    /// the file's bytes into the bitmap (the third high review that day).
    fn ensure_chained(&mut self, first: u32, contiguous: bool, size: u64) -> Result<(), Error> {
        if !contiguous || first < 2 {
            return Ok(());
        }
        let n = size.div_ceil(self.cluster_bytes());
        if !run_fits(first, size, self.cluster_bytes(), self.cluster_count) || self.meets_system(first, n) {
            return Err(Error::Io(DiskError::Io));
        }
        let n = n as u32;
        for i in 0..n {
            let value = if i + 1 < n { first + i + 1 } else { END_OF_CHAIN };
            self.write_fat_entry(first + i, value)?;
        }
        Ok(())
    }

    /// Read-modify-write of a partial sector of file data (preserve the bytes
    /// outside `[in_off, in_off+bytes.len())`) - `fat32::write_partial_sector`.
    fn write_partial_sector(&mut self, lba: u64, in_off: usize, bytes: &[u8]) -> Result<(), Error> {
        let mut sector = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sector)?;
        sector[in_off..in_off + bytes.len()].copy_from_slice(bytes);
        self.disk.write_sector(lba, &sector)?;
        Ok(())
    }

    /// Update an existing entry set's stream extension to point at a new
    /// first cluster, ValidDataLength and DataLength (FAT-chained), both
    /// lengths asked for: until 2026-10-22 one length set both, so a write to
    /// a preallocated file made the stale bytes past its valid length the
    /// file's, for every driver. Then recompute the
    /// `SetChecksum` over the whole set - the exFAT equivalent of
    /// `fat32::patch_entry_cluster_size`, plus the checksum the format requires.
    /// Rewrites the File + Stream entries (the two that changed; the name
    /// entries and any others are untouched).
    ///
    /// The checksum runs over all `set_len` entries, read a sector at a time
    /// as it goes: a set may hold more than the largest fsd builds (a name of
    /// 241 to 255 characters with a vendor secondary, or vendor entries after
    /// any name, which the spec allows and cameras write), and until
    /// 2026-10-19 the sum stopped at [`MAX_SET_ENTRIES`], so such a set was
    /// left with a checksum over part of it, which a fsck may repair by
    /// deleting the set. File and Stream go out in ONE sector write when they
    /// share a sector, as they do unless the set starts in a sector's last
    /// slot: then two writes, and a crash between them leaves the checksum
    /// unmatched, which nothing short of a journal avoids.
    fn patch_stream_ext(&mut self, set: SetAt, first_cluster: u32, valid_len: u64, data_len: u64) -> Result<(), Error> {
        // File and Stream as they are, then the Stream changed.
        let mut cur = SlotCursor::new(set.dir_cluster, set.dir_contiguous);
        let mut head = [0u8; 2 * ENTRY_SIZE];
        let mut at = [(0u64, 0usize); 2];
        let mut sec = [0u8; SECTOR_SIZE];
        let mut sec_lba = None;
        for (i, place) in at.iter_mut().enumerate() {
            let (lba, off) = self.slot_at(&mut cur, set.slot + i)?.ok_or(Error::NotFound)?;
            if sec_lba != Some(lba) {
                self.disk.read_sector(lba, &mut sec)?;
                sec_lba = Some(lba);
            }
            head[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE].copy_from_slice(&sec[off..off + ENTRY_SIZE]);
            *place = (lba, off);
        }
        let se = ENTRY_SIZE;
        // `walk_dir` closes a set only in this shape, so this is a check that
        // the slots still hold the set it found, not a second rule.
        if set.len < 2 || head[0] != ET_FILE || head[se] != ET_STREAM_EXT {
            return Err(Error::Io(DiskError::Io));
        }
        head[se + 1] = SECONDARY_ALLOC_POSSIBLE; // FAT-chained now (NoFatChain clear)
        head[se + 8..se + 16].copy_from_slice(&valid_len.to_le_bytes()); // ValidDataLength
        head[se + 20..se + 24].copy_from_slice(&first_cluster.to_le_bytes());
        head[se + 24..se + 32].copy_from_slice(&data_len.to_le_bytes()); // DataLength
        // The checksum: File (but its own two bytes) and the new Stream, then
        // every secondary after them as it is on the disk.
        let mut sum = set_checksum(&head);
        for i in 2..set.len {
            let (lba, off) = self.slot_at(&mut cur, set.slot + i)?.ok_or(Error::NotFound)?;
            if sec_lba != Some(lba) {
                self.disk.read_sector(lba, &mut sec)?;
                sec_lba = Some(lba);
            }
            for &b in &sec[off..off + ENTRY_SIZE] {
                sum = checksum_step(sum, b);
            }
        }
        head[2..4].copy_from_slice(&sum.to_le_bytes());
        // Rewrite the two changed entries (File carries the new checksum): one
        // write when they share a sector, the sector already read if `sec`
        // still holds it.
        if at[0].0 == at[1].0 {
            let (lba, _) = at[0];
            if sec_lba != Some(lba) {
                self.disk.read_sector(lba, &mut sec)?;
            }
            for (i, &(_, off)) in at.iter().enumerate() {
                sec[off..off + ENTRY_SIZE].copy_from_slice(&head[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE]);
            }
            self.disk.write_sector(lba, &sec)?;
        } else {
            for (i, &(lba, off)) in at.iter().enumerate() {
                if sec_lba != Some(lba) {
                    self.disk.read_sector(lba, &mut sec)?;
                }
                sec[off..off + ENTRY_SIZE].copy_from_slice(&head[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE]);
                self.disk.write_sector(lba, &sec)?;
                sec_lba = Some(lba);
            }
        }
        Ok(())
    }

    // ---- write surface ------------------------------------------------------

    /// Create an empty (zero-byte) file, or succeed as a no-op if a file already
    /// exists there (no RTC to update, same as `fat32::touch`). An empty file
    /// needs no cluster: first cluster `0`, length `0` - just one entry set.
    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        self.writable()?;
        let (_parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (mut dir, kind) = self.parent_for_create(path)?;
        match kind {
            ExistingKind::File => return Ok(()),
            ExistingKind::Dir => return Err(Error::NotAFile),
            ExistingKind::None => {}
        }
        let room = self.room(&dir)?;
        Ok(self.create_entry(&mut dir, room, name, false, 0, 0, false)?)
    }

    /// Create a file with exactly `data`, or fully replace an existing file's
    /// contents. The new chain is allocated and written *before* the old file
    /// is touched, so a failure partway never unlinks an existing file. Then
    /// the entry is patched to name it, and the old chain freed (or a new
    /// entry created).
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        self.writable()?;
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let mut dir = self.dir_at(parent_path)?;
        let parent = dir.entry;
        let existing = self.entry_in(&parent, name)?;
        #[allow(clippy::large_enum_variant)] // one, on the stack, for one call
        enum Plan {
            Replace(DirEntry, SetAt),
            Create(Room),
        }
        // A create is refused before the chain is written, or it would leak.
        let plan = match existing {
            Some((entry, _)) if entry.is_dir => return Err(Error::NotAFile),
            Some((entry, at)) => Plan::Replace(entry, at),
            None => Plan::Create(self.room(&dir)?),
        };

        // Write the replacement chain first (nothing existing touched yet).
        let new_cluster = self.write_chain(data)?;

        match plan {
            Plan::Replace(entry, at) => {
                // The set names the new chain before the old one is freed, as
                // a removal deletes its set first (`delete_set`): an error or
                // a crash between leaks the old chain (or, if the patch
                // fails, the new one) and never leaves the set over clusters
                // the allocator may hand out again. Until 2026-10-23 the old
                // chain was freed first. Not freed where the set's own
                // vendor runs, which it keeps, overlap it (damage, as
                // `free_runs` holds a removal's back from its Stream): it was,
                // and the allocator handed the cluster to another file a
                // later `rm` freed it under (the third high review of
                // 2026-10-24). Read before the patch, as `runs_of` is.
                let mut vendor = Runs::NONE;
                self.add_secondaries(at, &entry, &mut vendor)?;
                self.patch_stream_ext(at, new_cluster, data.len() as u64, data.len() as u64)?;
                if self.holds(&vendor, Alloc::stream(&entry))? {
                    return Ok(()); // a leak, the safe side
                }
                self.free_data(entry.first_cluster, entry.contiguous, entry.size)
            }
            Plan::Create(room) => {
                let made = self.create_entry(&mut dir, room, name, false, new_cluster, data.len() as u64, false);
                if let Err(CreateFailed { written: false, .. }) = made {
                    // The directory could not take the set (it could not grow,
                    // say): nothing names the chain, so it is given back.
                    let _ = self.free_data(new_cluster, false, data.len() as u64);
                }
                Ok(made?)
            }
        }
    }

    /// Write `data` at byte `offset`, extending the file as needed without
    /// rewriting the bytes before `offset` - the primitive behind streaming
    /// `cp`, `>>`, and `writeat`. Mirrors `fat32::write_at` (grow-only size, a
    /// zero-filled gap bounded by `MAX_GAP_FILL`, a unified per-sector pass),
    /// with allocation through the bitmap, a contiguous file first converted
    /// to a FAT chain so it can be extended, and the gap starting at the
    /// file's ValidDataLength, not its size.
    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        self.writable()?;
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let (entry, at) = self.entry_in(&parent, name)?.ok_or(Error::NotFound)?;
        if entry.is_dir {
            return Err(Error::NotAFile);
        }
        let old_size = entry.size;
        // The gap is filled from the valid length, not the size: the bytes
        // between them read as zeros, and a write past the valid length makes
        // everything before it valid, so they are written as the zeros they
        // read as. A file another driver preallocated far past its valid
        // length is refused a write beyond the bound, as one past its size is.
        let valid = entry.valid;
        if offset > valid && offset - valid > MAX_GAP_FILL {
            return Err(Error::InvalidOffset);
        }

        // Make the existing allocation FAT-chained so it can be extended.
        self.ensure_chained(entry.first_cluster, entry.contiguous, old_size)?;
        // A chained file on one of the volume's own clusters is damage,
        // refused before anything is written, as `ensure_chained` refuses a
        // contiguous run over one and `file_step` a link to one: a chained
        // file on the bitmap's cluster took the write into it (the fourth
        // high review of 2026-10-24). `sector_of` refuses its sectors too,
        // but a write starting at its end writes none of them, and links a
        // new cluster from its FAT entry.
        if self.is_system(entry.first_cluster) {
            return Err(Error::Io(DiskError::Io));
        }

        let cluster_bytes = self.cluster_bytes();
        let mut head_cluster = entry.first_cluster;
        if head_cluster < 2 {
            head_cluster = self.alloc_cluster()?;
        }

        let write_start = valid.min(offset);
        let write_end = offset + data.len() as u64;
        // The bytes the file's own clusters hold: a link past them (a crash
        // between a chain's extension and its size write, or another tool)
        // may name another file's cluster, so a walk past them extends the
        // chain from there, cutting the link, where it followed it and wrote
        // into that cluster (the medium review of 2026-10-19).
        let owned = old_size.div_ceil(cluster_bytes) * cluster_bytes;

        // Walk to the cluster holding `write_start`, extending if needed:
        // from the write cursor when it is this file's and not past
        // `write_start`, else from the head cluster.
        // A write's walk is always FAT-chained: `ensure_chained` ran above.
        let (mut cluster, pos) = ChainCursor::start(self.write_cursor, head_cluster, false, write_start as usize);
        let mut cluster_pos = pos as u64;
        while cluster_pos + cluster_bytes <= write_start {
            cluster = self.file_step(cluster, cluster_pos + cluster_bytes < owned)?;
            cluster_pos += cluster_bytes;
        }

        let mut pos = write_start;
        while pos < write_end {
            while pos >= cluster_pos + cluster_bytes {
                cluster = self.file_step(cluster, cluster_pos + cluster_bytes < owned)?;
                cluster_pos += cluster_bytes;
            }
            let sector_in_cluster = (pos - cluster_pos) / SECTOR_SIZE as u64;
            let sector_lba = self.sector_of(cluster, Holder::Data)? as u64 + sector_in_cluster;
            let sector_start = cluster_pos + sector_in_cluster * SECTOR_SIZE as u64;
            let in_off = (pos - sector_start) as usize;
            let n = ((SECTOR_SIZE - in_off) as u64).min(write_end - pos) as usize;

            // Zeros before `offset` (the gap), `data` from `offset` on.
            let mut chunk = [0u8; SECTOR_SIZE];
            for (i, slot) in chunk[..n].iter_mut().enumerate() {
                let fp = pos + i as u64;
                if fp >= offset {
                    *slot = data[(fp - offset) as usize];
                }
            }

            if in_off == 0 && n == SECTOR_SIZE {
                self.disk.write_sector(sector_lba, &chunk)?;
            } else if sector_start < valid {
                // Valid bytes in the sector outside the write: kept. A
                // sector holding none is written whole, zeros after the
                // write, and not read first; stale bytes left past the write
                // in a sector read here lie past the new valid length.
                self.write_partial_sector(sector_lba, in_off, &chunk[..n])?;
            } else {
                let mut sector = [0u8; SECTOR_SIZE];
                sector[in_off..in_off + n].copy_from_slice(&chunk[..n]);
                self.disk.write_sector(sector_lba, &sector)?;
            }
            pos += n as u64;
        }

        // `cluster` and `cluster_pos` moved together through the chain this
        // write walked and extended, so they name one of its clusters.
        self.write_cursor = Some(ChainCursor {
            file_cluster: head_cluster,
            contiguous: false,
            cluster,
            cluster_pos: cluster_pos as usize,
        });
        let new_size = old_size.max(write_end);
        self.patch_stream_ext(at, head_cluster, valid.max(write_end), new_size)
    }

    /// Delete the set `entry`, at `at` ([`unlink_set`](Self::unlink_set)),
    /// then, as `goes` says, leave its clusters to the set that carries them
    /// or free them ([`free_runs`](Self::free_runs)). The caller has asked
    /// [`writable`](Self::writable).
    fn delete_set(&mut self, at: SetAt, entry: &DirEntry, goes: SetGoes) -> Result<(), Error> {
        let secondaries = self.unlink_set(at)?;
        match goes {
            SetGoes::Renamed => Ok(()),
            SetGoes::Removed => self.free_runs(Alloc::stream(entry), secondaries, &Runs::NONE),
        }
    }

    /// Mark every entry of the set at `at` deleted by clearing its in-use
    /// bit (`0x85` -> `0x05`, `0xC0` -> `0x40`, `0xC1` -> `0x41`) - how exFAT
    /// removes a set (no `0xE5` tombstone byte like FAT), one
    /// read-modify-write a sector - and return the allocations its benign
    /// secondaries name (a vendor allocation, 0xE1), decoded from the bytes
    /// as they are cleared. Until the high review of 2026-10-24 they were
    /// read back afterwards, the shape checked last, so a set already
    /// deleted and its data freed was answered an I/O error with its vendor
    /// clusters left.
    ///
    /// Slots that are not a File entry and its Stream are refused before
    /// any is cleared, as `patch_stream_ext` and `renamed_set` refuse them:
    /// they are not the set `at` was found as, and they were deleted all the
    /// same, the caller then freeing the found set's data under whatever set
    /// does name it (the fourth high review of 2026-10-24).
    #[must_use = "the runs are the set's vendor allocations, left set in the bitmap if dropped"]
    fn unlink_set(&mut self, at: SetAt) -> Result<Runs, Error> {
        let mut head = [0u8; 2];
        if at.len >= 2 {
            self.read_slots(at, 0, |i, raw| {
                head[i] = raw[0];
                if i == 1 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            })?;
        }
        if head != [ET_FILE, ET_STREAM_EXT] {
            return Err(Error::Io(DiskError::Io));
        }
        let mut runs = Runs::NONE;
        self.rewrite_slots(at, || Error::NotFound, |sec, i, off, k| {
            for j in 0..k {
                let raw = &mut sec[off + j * ENTRY_SIZE..off + (j + 1) * ENTRY_SIZE];
                if i + j >= 2 {
                    if let Some(a) = allocation(raw) {
                        runs.add(a);
                    }
                }
                raw[0] &= !ET_IN_USE;
            }
        })?;
        Ok(runs)
    }

    /// Free what a deleted set named: `stream`, its Stream's run (the file's
    /// data, or a directory's extent), and `secondaries`, its benign
    /// secondaries' ([`unlink_set`](Self::unlink_set)), which `rm`, `rmdir`
    /// and a replacing `mv` left set in the bitmap until 2026-10-23.
    ///
    /// Freed AFTER the set is deleted, as the spec orders a removal (the
    /// entry, then the FAT and bitmap): an error or a crash between leaves
    /// clusters no set names, a leak a fsck finds, never a live set over
    /// clusters the allocator may hand out again. None overlapping a run
    /// `kept` names is freed, nor a secondary's that overlaps the Stream's
    /// (damage: its run past the Stream's may be another file's, and is left
    /// allocated, as a free leaves what lies past the heap). Every run is
    /// judged before any is freed, while every chain is still linked.
    fn free_runs(&mut self, stream: Alloc, secondaries: Runs, kept: &Runs) -> Result<(), Error> {
        let free_stream = !self.holds(kept, stream)?;
        let mut free = [false; Runs::MAX];
        for (f, a) in free.iter_mut().zip(secondaries.runs) {
            if let Some(a) = a {
                *f = !self.overlaps(a, stream)? && !self.holds(kept, a)?;
            }
        }
        if free_stream {
            self.free_data(stream.first, stream.contiguous, stream.size)?;
        }
        for (f, a) in free.into_iter().zip(secondaries.runs) {
            if let (true, Some(a)) = (f, a) {
                self.free_data(a.first, a.contiguous, a.size)?;
            }
        }
        Ok(())
    }

    /// Whether two allocations share a cluster. A contiguous run is its
    /// clusters by count; a FAT-chained one is walked, to its size
    /// ([`any_cluster`](Self::any_cluster)). Two chains that meet go on as
    /// one, since a cluster has one link, so they share a cluster exactly
    /// when the one that ends first ends on a cluster of the other. A chain
    /// was tested by its first cluster alone until the high review of
    /// 2026-10-24: a run over a later cluster of a surviving chain was freed
    /// under it. One with no cluster of the heap overlaps nothing.
    ///
    /// Each chain is walked once: `a`'s to its last cluster, then `b`'s,
    /// looking for it. `b`'s last is looked for on `a`'s walk only when
    /// `b`'s chain goes on past its size, since a `b` ending at its end of
    /// chain on a cluster of `a`'s ends `a` there too. That walk was taken
    /// every time, and both chains walked twice, until the second high
    /// review of 2026-10-24.
    fn overlaps(&mut self, a: Alloc, b: Alloc) -> Result<bool, Error> {
        if !self.in_heap(a.first) || !self.in_heap(b.first) {
            return Ok(false);
        }
        let cb = self.cluster_bytes();
        let within = move |c: u32, r: Alloc| run_has(r.first, r.size, cb, c);
        Ok(match (a.contiguous, b.contiguous) {
            (true, true) => within(a.first, b) || within(b.first, a),
            (true, false) => self.any_cluster(b, |c| within(c, a))?,
            (false, true) => self.any_cluster(a, |c| within(c, b))?,
            (false, false) => {
                let (mut la, mut lb) = (a.first, b.first);
                self.any_cluster(a, |c| {
                    la = c;
                    false
                })?;
                self.any_cluster(b, |c| {
                    lb = c;
                    c == la
                })? || (self.next_cluster(lb)?.is_some() && self.any_cluster(a, |c| c == lb)?)
            }
        })
    }

    /// Whether `f` answers true for a cluster of the run `r`, in order: a
    /// contiguous run's by count, a FAT-chained one's by its links, either
    /// to its size and inside the heap, so a loop or a damaged size ends.
    fn any_cluster(&mut self, r: Alloc, mut f: impl FnMut(u32) -> bool) -> Result<bool, Error> {
        let n = r.size.div_ceil(self.cluster_bytes());
        let mut c = r.first;
        for i in 0..n {
            if !self.in_heap(c) {
                break;
            }
            if f(c) {
                return Ok(true);
            }
            if i + 1 == n {
                break;
            }
            if r.contiguous {
                c += 1;
            } else {
                match self.next_cluster(c)? {
                    Some(next) => c = next,
                    None => break,
                }
            }
        }
        Ok(false)
    }

    /// Whether `a` overlaps a run of `kept`, the set surviving a replacing
    /// `mv`. A cross-linked volume, or a retry of an `mv` that failed after
    /// writing its new set, gives two sets the same clusters, and freeing
    /// them under the survivor would hand its data out again. By overlap,
    /// not by first cluster: a run sharing a later cluster with the
    /// survivor's was freed under it (the resumed max review of 2026-10-23).
    fn holds(&mut self, kept: &Runs, a: Alloc) -> Result<bool, Error> {
        if kept.more {
            return Ok(true);
        }
        for r in kept.runs.into_iter().flatten() {
            if self.overlaps(a, r)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The runs `entry`'s set, at `at`, names ([`Runs`]): read once, before
    /// a replacing `mv` writes or deletes anything, where the set it deletes
    /// asked the survivor's slots again for each of its allocations. A Stream
    /// naming no cluster (an empty file) takes no place: it took one of the
    /// eight, and a set with eight vendor allocations then held every run
    /// (the high review of 2026-10-24).
    fn runs_of(&mut self, at: SetAt, entry: &DirEntry) -> Result<Runs, Error> {
        let mut runs = Runs::NONE;
        if self.in_heap(entry.first_cluster) {
            runs.add(Alloc::stream(entry));
        }
        self.add_secondaries(at, entry, &mut runs)?;
        Ok(runs)
    }

    /// Add to `runs` the allocations `entry`'s benign secondaries name, its
    /// set at `at` ([`runs_of`](Self::runs_of), and `write_file`, whose set
    /// keeps them): none read when the set has none.
    fn add_secondaries(&mut self, at: SetAt, entry: &DirEntry, runs: &mut Runs) -> Result<(), Error> {
        if entry.allocs {
            self.read_slots(at, 2, |_, raw| {
                if let Some(a) = allocation(raw) {
                    runs.add(a);
                }
                if runs.more { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
            })?;
        }
        Ok(())
    }

    /// Create an empty subdirectory. Fails if the name exists. An exFAT
    /// directory has *no* `.`/`..` entries (unlike FAT32) - an empty one is just
    /// a zeroed cluster whose first slot is the end-of-directory marker.
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        self.writable()?;
        let (_parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (mut dir, kind) = self.parent_for_create(path)?;
        if kind != ExistingKind::None {
            return Err(Error::AlreadyExists);
        }
        // Refused before the cluster is claimed, or it would leak.
        let room = self.room(&dir)?;
        // Claim + zero the new directory's cluster before linking it in.
        let new_cluster = self.alloc_zeroed()?;
        let cluster_bytes = self.cluster_bytes();
        let made = self.create_entry(&mut dir, room, name, true, new_cluster, cluster_bytes, false);
        if let Err(CreateFailed { written: false, .. }) = made {
            let _ = self.free_data(new_cluster, false, cluster_bytes); // nothing names it
        }
        Ok(made?)
    }

    /// Remove a file (rejects a directory with `NotAFile`): deletes its entry
    /// set, then frees its clusters (bitmap + FAT) and its vendor
    /// allocations' ([`delete_set`](Self::delete_set)).
    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        self.writable()?;
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let (entry, at) = self.entry_in(&parent, name)?.ok_or(Error::NotFound)?;
        if entry.is_dir {
            return Err(Error::NotAFile);
        }
        self.delete_set(at, &entry, SetGoes::Removed)
    }

    /// Remove an empty subdirectory (rejects a non-empty one, a file, and the
    /// root): deletes its entry set, then frees its cluster(s) and its vendor
    /// allocations' ([`delete_set`](Self::delete_set)).
    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        self.writable()?;
        split_parent(path).ok_or(Error::CannotRemoveRoot)?;
        let dir = self.dir_at(path)?;
        let entry = dir.entry;
        // Empty means the directory has no in-use entry set at all, to its
        // size: a directory with a set there is not empty, damaged or not.
        let mut empty = true;
        self.walk_dir(entry.first_cluster, entry.contiguous, entry.size, |_e, _n, _s, _l| {
            empty = false;
            true
        })?;
        if !empty {
            return Err(Error::DirectoryNotEmpty);
        }
        // Refused when its chain is not its size: a set past it would be lost.
        let Room::Sub(at, _) = self.room(&dir)? else {
            return Err(Error::CannotRemoveRoot);
        };
        self.delete_set(at, &entry, SetGoes::Removed)
    }

    /// Rename or move a file/directory. Re-points a new entry set at `src`'s
    /// existing clusters (no data copy), preserving its `NoFatChain` layout,
    /// then deletes `src`'s set - write-new-before-delete-old ordering. The
    /// new set keeps the old one's File entry and the secondaries past its
    /// name ([`renamed_set`](Self::renamed_set)). exFAT
    /// has no `..` entry, so a moved directory needs no fixup (unlike FAT32's
    /// `mv`).
    ///
    /// An existing `dst` is REPLACED when both it and `src` are ordinary files
    /// - POSIX `rename` - and the same write-new-first ordering applies, so a
    /// failure part-way leaves a transient duplicate name rather than a
    /// destroyed destination. Not atomic: an exFAT entry set carries the file's
    /// own location, so there is no inode number to re-point. A DIRECTORY on
    /// either side of an existing name is still refused.
    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), Error> {
        self.writable()?;
        let (src_parent_path, src_name) = split_parent(src).ok_or(Error::InvalidName)?;
        let (dst_parent_path, dst_name) = split_parent(dst).ok_or(Error::InvalidName)?;

        let src_parent = self.find(src_parent_path)?;
        if !src_parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let mut dst_dir = self.dir_at(dst_parent_path)?;
        // As it was found: a create that grows the directory changes its size
        // and chaining, and the slots found in it address the same clusters.
        let dst_parent = dst_dir.entry;

        let (src_entry, src_at) = self.entry_in(&src_parent, src_name)?.ok_or(Error::NotFound)?;

        // An existing destination is REPLACED when both it and `src` are
        // ordinary files. Like FAT32 and unlike ext2 this cannot be ATOMIC: an
        // exFAT entry set carries the file's own location, so the change takes
        // two writes rather than one. It is NOT a window in which the name is
        // absent: the new set is written before the old one is deleted, so a
        // reader in between finds two and gets one of the two files, never
        // nothing.
        if let Some((dst_entry, dst_at)) = self.entry_in(&dst_parent, dst_name)? {
            // Name matching is up-case folded, so `src` and `dst` can be the
            // SAME set two ways. `mv f f` is a genuine no-op; `mv FOO.TXT
            // foo.txt` is a real request to change the stored spelling, and
            // reporting Ok without doing it would claim a success that did not
            // happen. The create-first order below performs it: write the set
            // under the new name, delete the old one, and skip both the
            // source-side delete (same slot) and the data free (same file).
            // The same slot of the same first cluster is the same set on the
            // disk, whichever directory reached it: two directories a damaged
            // volume cross-links share their slots, so their sets there are
            // one, deleted once.
            let same_set = src_at.dir_cluster == dst_at.dir_cluster && src_at.slot == dst_at.slot;
            if same_set && src_name.as_bytes() == dst_name.as_bytes() {
                return Ok(());
            }
            if src_entry.is_dir || dst_entry.is_dir {
                return Err(Error::AlreadyExists);
            }
            if same_set {
                // A case-only rename: the new set carries this one's clusters.
                self.relink(&mut dst_dir, src_at, &src_entry, dst_name)?;
                return self.delete_set(dst_at, &dst_entry, SetGoes::Renamed);
            }
            // What `src`'s set names, read before anything is written; not
            // for a case-only rename, which frees nothing (read there until
            // the second high review of 2026-10-24).
            let kept = self.runs_of(src_at, &src_entry)?;
            // CREATE BEFORE DELETING, like the non-replacing path below.
            // Deleting the destination first survives a crash no worse, and is
            // wrong against an ordinary error: a failing `relink` (a
            // directory that cannot be extended, an allocation failure) would
            // return Err having already destroyed `dst` while `src` is still
            // there. Slot indices are stable across a create - `insert_set`
            // fills a free run and moves nothing - so the recorded `dst_at`
            // still addresses the old set afterwards.
            self.relink(&mut dst_dir, src_at, &src_entry, dst_name)?;
            // `dst`'s old set first, so a failure from here on never leaves
            // two sets of `dst`'s name, only `src`'s set beside its new one,
            // as a plain `mv` that fails at its delete; then `src`'s; then
            // the clusters `dst`'s named, but for any overlapping what
            // `src`'s named (a cross-linked image), held until neither set
            // is left to name them. `src`'s set went first from 2026-10-23,
            // and a failure at it left two sets named `dst` (the high review
            // of 2026-10-24). Freed even when `src`'s delete fails: `dst`'s
            // set is gone, so nothing else would free them, and `kept` holds
            // what `src`'s set, deleted or not, names (returned early, a leak,
            // until the second high review of 2026-10-24).
            let secondaries = self.unlink_set(dst_at)?;
            let src_gone = self.delete_set(src_at, &src_entry, SetGoes::Renamed);
            let freed = self.free_runs(Alloc::stream(&dst_entry), secondaries, &kept);
            return src_gone.and(freed);
        }

        // Link dst to the same clusters first, then unlink src.
        self.relink(&mut dst_dir, src_at, &src_entry, dst_name)?;
        self.delete_set(src_at, &src_entry, SetGoes::Renamed)
    }

    /// Write `src`'s set, at `src_at`, again under `name` in `dir`
    /// ([`renamed_set`](Self::renamed_set), then
    /// [`insert_set`](Self::insert_set)): the step both of `mv`'s arms take
    /// before deleting a set. Its one whole-set buffer is here, not in each
    /// arm, so a debug build's frame holds one. Slot indices are stable
    /// across it: `insert_set` fills a free run and moves nothing, so a set
    /// found before it is still where it was.
    fn relink(&mut self, dir: &mut Dir, src_at: SetAt, src_entry: &DirEntry, name: &str) -> Result<(), Error> {
        let mut set = [0u8; WHOLE_SET_ENTRIES * ENTRY_SIZE];
        let entries = self.renamed_set(src_at, src_entry, name, &mut set)?;
        let room = self.room(dir)?;
        Ok(self.insert_set(dir, room, &set[..entries * ENTRY_SIZE])?)
    }
}

/// Where a walk over a directory's slots stands ([`Fs::slot_at`]): the
/// cluster holding slots `base..` (a cluster's worth), so a later slot is
/// found by stepping on from there. Made anew whenever the directory's first
/// cluster or layout changes (a growth that chains a `NoFatChain` run).
#[derive(Clone, Copy)]
struct SlotCursor {
    first: u32,
    contiguous: bool,
    cluster: u32,
    base: usize,
}

impl SlotCursor {
    fn new(first: u32, contiguous: bool) -> Self {
        SlotCursor { first, contiguous, cluster: first, base: 0 }
    }
}

/// Whether a name already exists in a directory, and as what.
#[derive(Clone, Copy, PartialEq)]
enum ExistingKind {
    None,
    File,
    Dir,
}

/// Round `v` up to the next multiple of `align` (a power of two).
fn align_up(v: u32, align: u32) -> u32 {
    v.div_ceil(align) * align
}

/// The exFAT 32-bit rolling checksum (rotate-right-1 then add), used by both
/// the boot-region checksum and the up-case table checksum. `skip` names byte
/// indices to exclude (the boot checksum excludes VolumeFlags/PercentInUse).
fn checksum32(data: &[u8], skip: &[usize]) -> u32 {
    let mut sum = 0u32;
    for (i, &b) in data.iter().enumerate() {
        if skip.contains(&i) {
            continue;
        }
        sum = sum.rotate_right(1).wrapping_add(b as u32);
    }
    sum
}

/// Byte length of [`build_upcase_table`]'s minimal (ASCII `a-z`) compressed
/// up-case table: 30 u16 entries.
const UPCASE_TABLE_BYTES: usize = 60;

/// Build the minimal valid compressed up-case table into `out` (identity for
/// every code unit except ASCII `a-z -> A-Z`) and return its checksum. exFAT
/// permits any valid table; this is the smallest one, and matches the ASCII
/// case-folding the reader/`name_hash` use. Compression: a `0xFFFF` marker
/// followed by a count means "the next `count` code units map to themselves".
fn build_upcase_table(out: &mut [u8; UPCASE_TABLE_BYTES]) -> u32 {
    let mut u16s = [0u16; 30];
    u16s[0] = 0xFFFF; // identity run marker
    u16s[1] = 0x0061; // ...for code units 0x0000..=0x0060 (97 of them)
    for i in 0..26u16 {
        u16s[2 + i as usize] = 0x0041 + i; // 0x0061..=0x007A -> 0x0041..=0x005A
    }
    u16s[28] = 0xFFFF; // identity run marker
    u16s[29] = 0xFF85; // ...for code units 0x007B..=0xFFFF (65413 of them)
    for (i, v) in u16s.iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    checksum32(out, &[])
}

/// exFAT sectors-per-cluster shift by volume size (in 512-byte sectors),
/// following the conventional exFAT cluster-size table - used by
/// [`Fs::format`]. Bigger volumes get bigger clusters, which keeps the FAT
/// and allocation bitmap small.
fn sectors_per_cluster_shift_for(total_sectors: u32) -> u8 {
    match total_sectors {
        0..=524_288 => 3,          // <= 256 MB: 4 KB clusters
        524_289..=67_108_864 => 6, // <= 32 GB: 32 KB clusters
        _ => 8,                    // 128 KB clusters
    }
}

/// Whether path component `entry` answers to `name` on exFAT, the two encoded
/// to UTF-16 and compared by [`units_match`]. `vfs::Filesystem::names_match`
/// gives it to `NP_MV`'s guard, which compares two paths' spellings; a lookup
/// of a set ([`Fs::entry_in`]) takes [`name_answers`], which also lets a `?`
/// stand for a unit a listing could not show. Its own since 2026-10-19: until
/// then exFAT matched by FAT32's rule, a second statement of the same folding.
pub(crate) fn names_match(entry: &str, name: &str) -> bool {
    units_match(entry.encode_utf16(), name.encode_utf16())
}

/// Whether the set named by `units` answers to a lookup of `asked` (both
/// UTF-16): [`units_match`], or, for a name a listing cannot show exactly
/// ([`shown_exactly`]), the spelling it shows, each `?` asked standing for
/// one unit outside ASCII. The one rule [`Fs::entry_in`] picks a set by.
fn name_answers(units: &[u16], asked: &[u16]) -> bool {
    if units.len() != asked.len() {
        return false;
    }
    if units_match(units.iter().copied(), asked.iter().copied()) {
        return true;
    }
    !shown_exactly(units)
        && units.iter().zip(asked).all(|(&u, &a)| {
            if u >= 0x80 {
                a == u16::from(b'?')
            } else {
                upcase(u) == upcase(a)
            }
        })
}

/// Whether two names, as UTF-16 code units, are equal once each unit is put
/// through [`upcase`], the one case rule this arm has. [`name_hash`] up-cases
/// by it too, so the NameHash fsd writes and the match it looks names up by
/// cannot disagree, and reading the volume's up-case table (`0x82`) is a
/// change to `upcase` alone. Units, not bytes: until 2026-10-21 a name was
/// compared as its ASCII rendering, `?` for every other unit, so `café`
/// answered to `cafè` (and to `caf?`).
fn units_match(a: impl Iterator<Item = u16>, mut b: impl Iterator<Item = u16>) -> bool {
    for x in a {
        match b.next() {
            Some(y) if upcase(x) == upcase(y) => {}
            _ => return false,
        }
    }
    b.next().is_none()
}

/// exFAT's case rule here: ASCII `a`-`z` up-cased, every other code unit as
/// it is. The spec's up-case table maps ASCII the same way but up-cases far
/// more (`é` to `É`); the volume's own table (`0x82`) is not read, so a name
/// outside ASCII is folded and hashed by ASCII alone (the roadmap).
fn upcase(c: u16) -> u16 {
    if (0x61..=0x7a).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

/// The exFAT 16-bit rolling checksum step (rotate-right-1 then add), shared by
/// the name hash and the entry-set checksum.
fn checksum_step(sum: u16, byte: u8) -> u16 {
    let rot = (sum >> 1) | ((sum & 1) << 15);
    rot.wrapping_add(byte as u16)
}

/// `NameHash` over the up-cased name in UTF-16LE (stored in the stream
/// extension so a reader can pre-filter candidates before a full compare).
fn name_hash(name: &[u16]) -> u16 {
    let mut hash = 0u16;
    for &c in name {
        let up = upcase(c);
        hash = checksum_step(hash, (up & 0xff) as u8);
        hash = checksum_step(hash, (up >> 8) as u8);
    }
    hash
}

/// `SetChecksum` over every byte of the whole entry set, skipping the checksum
/// field itself (bytes 2-3 of the primary File entry).
fn set_checksum(entries: &[u8]) -> u16 {
    let mut sum = 0u16;
    for (i, &b) in entries.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = checksum_step(sum, b);
    }
    sum
}

/// Encode `name` to UTF-16 into `out`, validating it (non-empty, at most
/// [`LONG_NAME_MAX`] units, no control or exFAT-forbidden characters). A
/// character past U+FFFF is a surrogate pair, two units, as a lookup encodes
/// it (until 2026-10-21 it was refused, so a name a lookup reached could not
/// be made). Returns the unit count.
fn encode_name(name: &str, out: &mut [u16; LONG_NAME_MAX]) -> Option<usize> {
    let mut n = 0usize;
    for ch in name.chars() {
        if (ch as u32) < 0x20 {
            return None; // control chars
        }
        if matches!(ch, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|') {
            return None; // exFAT-forbidden filename characters
        }
        let mut pair = [0u16; 2];
        for &u in ch.encode_utf16(&mut pair).iter() {
            if n >= LONG_NAME_MAX {
                return None;
            }
            out[n] = u;
            n += 1;
        }
    }
    (n > 0).then_some(n)
}

/// Build a complete entry set into `buf`, returning the number of 32-byte
/// entries written. Files/dirs are FAT-chained (`NoFatChain = 0`; see the
/// write-infrastructure note). Timestamps are left zero (no RTC).
fn build_entry_set(
    name: &str,
    is_dir: bool,
    first_cluster: u32,
    data_len: u64,
    contiguous: bool,
    buf: &mut [u8; MAX_SET_BYTES],
) -> Option<usize> {
    let mut name16 = [0u16; LONG_NAME_MAX];
    let name_len = encode_name(name, &mut name16)?;
    let name_entries = name_len.div_ceil(NAME_CHARS_PER_ENTRY);
    let total = 2 + name_entries;
    let total_bytes = total * ENTRY_SIZE;
    for b in buf[..total_bytes].iter_mut() {
        *b = 0;
    }

    // File entry (0x85).
    buf[0] = ET_FILE;
    buf[1] = (1 + name_entries) as u8; // SecondaryCount
    let attrs: u16 = if is_dir { ATTR_DIRECTORY } else { 0 };
    buf[4..6].copy_from_slice(&attrs.to_le_bytes());

    // Stream extension (0xC0).
    let se = ENTRY_SIZE;
    buf[se] = ET_STREAM_EXT;
    buf[se + 1] = SECONDARY_ALLOC_POSSIBLE | if contiguous { NO_FAT_CHAIN } else { 0 };
    buf[se + 3] = name_len as u8;
    buf[se + 4..se + 6].copy_from_slice(&name_hash(&name16[..name_len]).to_le_bytes());
    buf[se + 8..se + 16].copy_from_slice(&data_len.to_le_bytes()); // ValidDataLength
    buf[se + 20..se + 24].copy_from_slice(&first_cluster.to_le_bytes());
    buf[se + 24..se + 32].copy_from_slice(&data_len.to_le_bytes()); // DataLength

    // File-Name entries (0xC1), 15 UTF-16 chars each.
    for e in 0..name_entries {
        let eo = ENTRY_SIZE * (2 + e);
        buf[eo] = ET_FILE_NAME;
        for k in 0..NAME_CHARS_PER_ENTRY {
            let idx = e * NAME_CHARS_PER_ENTRY + k;
            let ch = if idx < name_len { name16[idx] } else { 0 };
            let p = eo + 2 + k * 2;
            buf[p..p + 2].copy_from_slice(&ch.to_le_bytes());
        }
    }

    // SetChecksum over the whole set, written last.
    let sum = set_checksum(&buf[..total_bytes]);
    buf[2..4].copy_from_slice(&sum.to_le_bytes());
    Some(total)
}

/// Clamp a `u64` byte count to the `u32` the `FSOP_*` protocol carries (file
/// sizes here are far below 4 GiB; a pathological giant just reports `u32::MAX`).
fn saturate_u32(v: u64) -> u32 {
    if v > u32::MAX as u64 {
        u32::MAX
    } else {
        v as u32
    }
}

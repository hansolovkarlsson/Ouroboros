//! exFAT, read-write - the second on-disk format `fsd` understands, and the
//! first real exercise of the [`Filesystem`](crate::vfs::Filesystem) enum's
//! dispatch (until now FAT32 was the only arm). Landed read-only first, then
//! read-write in four staged commits (allocation + `touch`; `write_file`/
//! `write_at`; `mkdir`/`rm`/`rmdir`; `mv`) - the big scoping lever the "more
//! filesystems" roadmap arc calls out, since FAT32's own write support was where
//! every corruption risk lived (phases 4-8).
//!
//! **Write model.** Free clusters are tracked by an allocation *bitmap*
//! ([`Fs::alloc_cluster`]/[`Fs::bitmap_set`]), located at mount from the root's
//! `0x81` entry. Files/directories we create are always FAT-*chained*
//! (`NoFatChain = 0`), so allocation parallels FAT32's `write_chain` (a
//! bitmap-set plus a FAT-link) and the reader's [`Fs::advance`] walks the
//! chain. Creating an
//! entry ([`Fs::create_entry`]) builds a full set with both required checksums -
//! the whole-set `SetChecksum` and the up-cased `NameHash`; deleting one
//! ([`Fs::delete_set`]) clears each entry's in-use bit. Same corruption
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
//!   We render them ASCII (non-ASCII -> `?`), exactly as `fat32.rs` does for
//!   its own long names, since the whole userland here is ASCII-only.
//! - **An allocation *bitmap*** replaces FAT free-cluster scanning: reads ignore
//!   it entirely, writes drive it (see the write model above). The up-case table
//!   (`0x82`) that would drive case-insensitive comparison per spec is ignored -
//!   we approximate with ASCII case-folding ([`str::eq_ignore_ascii_case`], what
//!   `fat32.rs` already uses), correct for the ASCII names this system uses, and
//!   the same ASCII up-case feeds the `NameHash` we write.

use core::ops::ControlFlow;

use crate::disk::{Disk, DiskError};
use crate::fat32::{split_parent, Error};

const SECTOR_SIZE: usize = 512;
const ENTRY_SIZE: usize = 32;

// Directory entry type bytes. The high bit (0x80) is "in use"; a byte with it
// clear is a deleted/unused entry, and a literal 0x00 marks end-of-directory.
const ET_ALLOC_BITMAP: u8 = 0x81; // the allocation bitmap (a system file in root)
const ET_FILE: u8 = 0x85; // primary entry of a file/dir set
const ET_STREAM_EXT: u8 = 0xc0; // first secondary: cluster/size/flags
const ET_FILE_NAME: u8 = 0xc1; // secondary: 15 UTF-16 name chars

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
/// Directory-entry slots per 512-byte sector.
const SLOTS_PER_SECTOR: usize = SECTOR_SIZE / ENTRY_SIZE;

/// One directory entry set, decoded into a self-contained, no-alloc form - the
/// same role `fat32::DirEntry` plays, carrying the one extra thing exFAT needs:
/// whether the data is contiguous (`NoFatChain`), which decides how the cluster
/// walk advances.
#[derive(Clone, Copy)]
struct DirEntry {
    name: [u8; LONG_NAME_MAX],
    name_len: u16,
    is_dir: bool,
    size: u64,
    first_cluster: u32,
    contiguous: bool,
}

impl DirEntry {
    fn name(&self) -> &str {
        // Built from UTF-16 rendered to ASCII (non-ASCII -> '?'), always valid.
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
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

/// A directory to create an entry in: its own entry, where its entry's set
/// lies, and its extent (both `None` for the root, which has no set, and
/// whose extent, its whole chain, is walked only when it grows: a create
/// that finds a slot reads no FAT, as before 2026-10-18). Growing a
/// directory past its clusters raises its DataLength, which other drivers
/// stop at, so whoever grows it must know where that length is recorded;
/// `create_entry` takes only this, built by `dir_at` and `parent_for_create`.
struct Dir {
    entry: DirEntry,
    at: Option<SetAt>,
    extent: Option<Extent>,
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
    pub fn mount_at(mut disk: Disk, partition_lba: u32) -> Result<Self, Error> {
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
            return Err(Error::UnsupportedSectorSize(1u16 << bytes_per_sector_shift));
        }

        let fat_offset = u32::from_le_bytes([vbr[80], vbr[81], vbr[82], vbr[83]]);
        let cluster_heap_offset = u32::from_le_bytes([vbr[88], vbr[89], vbr[90], vbr[91]]);
        let cluster_count = u32::from_le_bytes([vbr[92], vbr[93], vbr[94], vbr[95]]);
        let root_cluster = u32::from_le_bytes([vbr[96], vbr[97], vbr[98], vbr[99]]);

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
            read_cursor: None,
            write_cursor: None,
            fat_cache: None,
        };
        // Best-effort: reads don't need the bitmap, so a volume without one
        // (malformed) still mounts read-only; only writes require it.
        fs.locate_bitmap()?;
        Ok(fs)
    }

    /// Find the allocation bitmap by scanning the root directory for its `0x81`
    /// entry (which always precedes any file entry). Records its first cluster
    /// and byte length; leaves them `0` if none is found.
    fn locate_bitmap(&mut self) -> Result<(), Error> {
        let mut cluster = self.root_cluster;
        loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for raw in buf.chunks_exact(ENTRY_SIZE) {
                    match raw[0] {
                        0x00 => return Ok(()), // end of directory - no bitmap
                        ET_ALLOC_BITMAP => {
                            self.bitmap_first_cluster =
                                u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]);
                            self.bitmap_bytes = u64::from_le_bytes([
                                raw[24], raw[25], raw[26], raw[27], raw[28], raw[29], raw[30],
                                raw[31],
                            ]);
                            return Ok(());
                        }
                        _ => {}
                    }
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
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

    fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.cluster_heap_lba + (cluster - 2) * self.sectors_per_cluster
    }

    /// The next cluster in a FAT chain, or `None` at the end. exFAT FAT entries
    /// are a full 32 bits (no reserved top nibble, unlike FAT32).
    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>, Error> {
        if cluster < 2 || cluster - 2 >= self.cluster_count {
            return Ok(None);
        }
        let value = self.fat_entry(cluster)?;
        if ends_walk(value) {
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
            if next < 2 || next - 2 >= self.cluster_count {
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
            name: [0; LONG_NAME_MAX],
            name_len: 0,
            is_dir: true,
            size: 0, // unknown; walk_dir stops at the end-of-directory marker
            first_cluster: self.root_cluster,
            contiguous: false,
        }
    }

    /// Reassemble every entry *set* in a directory into a [`DirEntry`], calling
    /// `f` for each and stopping early the first time it returns `true`. The
    /// directory's own data is a cluster run described by `(start_cluster,
    /// contiguous, dir_size)`; `dir_size == 0` means "unknown" (the root), in
    /// which case only the end-of-directory marker / chain end terminates it.
    ///
    /// A set is: a File entry (`0x85`, attributes + how many secondaries
    /// follow) then a Stream-Extension entry (`0xC0`, first cluster + data
    /// length + `NoFatChain`) then File-Name entries (`0xC1`, 15 UTF-16 chars
    /// each, up to the stream extension's `NameLength`). The cross-entry
    /// assembly state below is the exFAT analogue of `fat32::walk_dir`'s pending
    /// LFN run.
    fn walk_dir(
        &mut self,
        start_cluster: u32,
        contiguous: bool,
        dir_size: u64,
        mut f: impl FnMut(&DirEntry, usize, usize) -> bool,
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
        let mut set_cluster = 0u32;
        let mut set_contig = false;
        let mut name = [0u8; LONG_NAME_MAX];
        let mut name_cap = 0usize; // total chars the set declares (NameLength)
        let mut name_len = 0usize; // chars filled so far

        loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for raw in buf.chunks_exact(ENTRY_SIZE) {
                    if contiguous && consumed >= dir_size {
                        return Ok(());
                    }
                    consumed += ENTRY_SIZE as u64;
                    let this_slot = slot;
                    slot += 1;

                    let t = raw[0];
                    if t == 0x00 {
                        return Ok(()); // end of directory
                    }
                    if t & 0x80 == 0 {
                        continue; // not in use (deleted / free slot)
                    }

                    match t {
                        ET_FILE => {
                            secondaries_left = raw[1];
                            set_start = this_slot;
                            set_len = 1 + raw[1] as usize;
                            let attrs = u16::from_le_bytes([raw[4], raw[5]]);
                            set_is_dir = attrs & ATTR_DIRECTORY != 0;
                            set_size = 0;
                            set_cluster = 0;
                            set_contig = false;
                            name_cap = 0;
                            name_len = 0;
                            in_set = true;
                        }
                        ET_STREAM_EXT if in_set => {
                            set_contig = raw[1] & NO_FAT_CHAIN != 0;
                            name_cap = raw[3] as usize;
                            set_cluster =
                                u32::from_le_bytes([raw[20], raw[21], raw[22], raw[23]]);
                            set_size = u64::from_le_bytes([
                                raw[24], raw[25], raw[26], raw[27], raw[28], raw[29], raw[30],
                                raw[31],
                            ]);
                            secondaries_left = secondaries_left.saturating_sub(1);
                        }
                        ET_FILE_NAME if in_set => {
                            // 15 UTF-16LE chars at bytes 2..32, capped by the
                            // set's declared NameLength (a name spans several
                            // 0xC1 entries).
                            let mut i = 2;
                            while i + 1 < ENTRY_SIZE
                                && name_len < name_cap
                                && name_len < LONG_NAME_MAX
                            {
                                let c = u16::from_le_bytes([raw[i], raw[i + 1]]);
                                name[name_len] = if c < 0x80 { c as u8 } else { b'?' };
                                name_len += 1;
                                i += 2;
                            }
                            secondaries_left = secondaries_left.saturating_sub(1);
                        }
                        _ if in_set => {
                            // A secondary type we don't model (none exist in a
                            // File set today) - still count it so the set closes.
                            secondaries_left = secondaries_left.saturating_sub(1);
                        }
                        _ => {
                            // A primary we ignore (bitmap 0x81, up-case 0x82,
                            // volume label 0x83) - not part of a File set.
                        }
                    }

                    // A set closes once every declared secondary is consumed.
                    if in_set && t != ET_FILE && secondaries_left == 0 {
                        in_set = false;
                        let entry = DirEntry {
                            name,
                            name_len: name_len as u16,
                            is_dir: set_is_dir,
                            size: set_size,
                            first_cluster: set_cluster,
                            contiguous: set_contig,
                        };
                        if f(&entry, set_start, set_len) {
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
    /// A cluster outside the heap, or a chain longer than the volume (a
    /// loop), is refused; a link past the size is not read further.
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
        let sized = size.div_ceil(self.cluster_bytes());
        if contiguous && first as u64 - 2 + sized > self.cluster_count as u64 {
            return Err(Error::Io(DiskError::Io));
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
        if !self.in_heap(next) || clusters >= self.cluster_count as u64 {
            return Err(Error::Io(DiskError::Io));
        }
        Ok(Next::Cluster(next))
    }

    fn in_heap(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster - 2 < self.cluster_count
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
            let mut found: Option<(DirEntry, usize, usize)> = None;
            self.walk_dir(
                current.first_cluster,
                current.contiguous,
                current.size,
                |entry, slot, len| {
                    if entry.name().eq_ignore_ascii_case(component) {
                        found = Some((*entry, slot, len));
                        true
                    } else {
                        false
                    }
                },
            )?;
            let (entry, slot, len) = found.ok_or(Error::NotFound)?;
            at = Some(SetAt {
                dir_cluster: current.first_cluster,
                dir_contiguous: current.contiguous,
                slot,
                len,
            });
            current = entry;
        }
        Ok((current, at))
    }

    /// The directory at `path`, to create an entry in or remove
    /// (`NotADirectory` for a file).
    ///
    /// A FAT-chained directory whose chain is not its size is refused, as
    /// Linux refuses it: fsd grew directories until 2026-10-18 without
    /// raising their size, and such a directory holds entries past it that
    /// no reader sees. A create there checked its name at the size and wrote
    /// a second set under a hidden one's name, and taking the chain as the
    /// size on a write (2026-10-18, the review's second pass) made reads and
    /// writes disagree: an append that saw no file truncated it. Adopting
    /// clusters past the size on both sides would take a stale link to a
    /// cluster since allocated into the directory. So the directory is left
    /// as it is, read to its size, until a fsck repairs it.
    fn dir_at(&mut self, path: &str) -> Result<Dir, Error> {
        let (entry, at) = self.find_at(path)?;
        if !entry.is_dir {
            return Err(Error::NotADirectory);
        }
        let Some(at) = at else {
            return Ok(Dir { entry, at, extent: None });
        };
        let extent = self.dir_extent(entry.first_cluster, entry.contiguous, entry.size)?;
        if !entry.contiguous
            && extent.clusters > 0
            && (extent.past_size || extent.clusters * self.cluster_bytes() != entry.size)
        {
            return Err(Error::Io(DiskError::Io));
        }
        Ok(Dir { entry, at: Some(at), extent: Some(extent) })
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
        self.walk_dir(dir.first_cluster, dir.contiguous, dir.size, |entry, _slot, _len| {
            f(entry.name(), entry.is_dir).is_break()
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
        let file = self.find(path)?;
        if file.is_dir {
            return Err(Error::NotAFile);
        }
        let real = saturate_u32(file.size);
        if file.size == 0 || file.first_cluster == 0 {
            return Ok(real);
        }

        let total = file.size as usize;
        let mut cluster = file.first_cluster;
        let mut written = 0usize;
        'clusters: loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                if written >= total || written >= buf.len() {
                    break 'clusters;
                }
                let mut sector = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut sector)?;
                let n = (total - written).min(buf.len() - written).min(SECTOR_SIZE);
                buf[written..written + n].copy_from_slice(&sector[..n]);
                written += n;
            }
            if written >= total || written >= buf.len() {
                break;
            }
            match self.advance(cluster, file.contiguous)? {
                Some(next) => cluster = next,
                None => break,
            }
        }
        Ok(real)
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

    /// [`read_at`](Self::read_at) of a file already found.
    fn read_entry(&mut self, file: &DirEntry, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        let total = file.size;
        if offset >= total || buf.is_empty() || file.first_cluster == 0 {
            return Ok(0);
        }
        let want = ((total - offset) as usize).min(buf.len());
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
            let lba = self.cluster_to_lba(cluster);
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
        Ok(written as u32)
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
        let lba = self.cluster_to_lba(cluster);
        let zero = [0u8; SECTOR_SIZE];
        for s in 0..self.sectors_per_cluster {
            self.disk.write_sector((lba + s) as u64, &zero)?;
        }
        Ok(())
    }

    /// LBA of the bitmap sector holding `byte_index` (bitmap assumed contiguous
    /// - see the field doc). The bitmap starts at `bitmap_first_cluster`.
    fn bitmap_sector_lba(&self, byte_index: u64) -> u64 {
        self.cluster_to_lba(self.bitmap_first_cluster) as u64 + byte_index / SECTOR_SIZE as u64
    }

    /// Set or clear the bitmap bit for `cluster` (bit index `cluster - 2`).
    fn bitmap_set(&mut self, cluster: u32, allocated: bool) -> Result<(), Error> {
        let bit = cluster - 2;
        let byte_index = (bit / 8) as u64;
        let lba = self.bitmap_sector_lba(byte_index);
        let off = (byte_index % SECTOR_SIZE as u64) as usize;
        let mask = 1u8 << (bit % 8);
        let mut buf = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut buf)?;
        if allocated {
            buf[off] |= mask;
        } else {
            buf[off] &= !mask;
        }
        self.disk.write_sector(lba, &buf)?;
        Ok(())
    }

    /// Allocate one cluster: find a free bit in the allocation bitmap, set it,
    /// mark the cluster end-of-chain in the FAT, and return it. `DiskFull` if
    /// the bitmap is full or wasn't located at mount.
    fn alloc_cluster(&mut self) -> Result<u32, Error> {
        if self.bitmap_first_cluster == 0 {
            return Err(Error::DiskFull);
        }
        let nbytes = self.cluster_count.div_ceil(8) as u64;
        let mut byte_index = 0u64;
        let mut sector_lba = u64::MAX;
        let mut buf = [0u8; SECTOR_SIZE];
        while byte_index < nbytes {
            let lba = self.bitmap_sector_lba(byte_index);
            if lba != sector_lba {
                self.disk.read_sector(lba, &mut buf)?;
                sector_lba = lba;
            }
            let off = (byte_index % SECTOR_SIZE as u64) as usize;
            if buf[off] != 0xff {
                let bitpos = buf[off].trailing_ones(); // first 0 bit
                let cluster = (byte_index as u32) * 8 + bitpos + 2;
                if cluster - 2 >= self.cluster_count {
                    break; // past the last real cluster
                }
                buf[off] |= 1u8 << bitpos;
                self.disk.write_sector(lba, &buf)?;
                self.write_fat_entry(cluster, END_OF_CHAIN)?;
                return Ok(cluster);
            }
            byte_index += 1;
        }
        Err(Error::DiskFull)
    }

    /// Free a data allocation: clear each cluster's bitmap bit (and, for a
    /// FAT-chained allocation, its FAT entry). Handles both a file we created
    /// (FAT-chained) and a contiguous one another tool wrote (`NoFatChain`,
    /// walked by count from `size`).
    fn free_data(&mut self, first: u32, contiguous: bool, size: u64) -> Result<(), Error> {
        if first < 2 {
            return Ok(());
        }
        if contiguous {
            let cluster_bytes = self.sectors_per_cluster as u64 * SECTOR_SIZE as u64;
            let n = size.div_ceil(cluster_bytes.max(1)) as u32;
            for i in 0..n {
                let c = first + i;
                if c - 2 >= self.cluster_count {
                    break;
                }
                self.bitmap_set(c, false)?;
            }
        } else {
            let mut c = first;
            loop {
                let next = self.next_cluster(c)?;
                self.bitmap_set(c, false)?;
                self.write_fat_entry(c, 0)?;
                match next {
                    Some(n) => c = n,
                    None => break,
                }
            }
        }
        Ok(())
    }

    // ---- directory-entry-set writing ----------------------------------------

    /// The on-disk (LBA, byte offset) of a directory's global slot `slot`
    /// (0-based over the whole directory), or `None` if it's past the current
    /// allocation. Walks the chain a cluster at a time.
    fn slot_addr(
        &mut self,
        dir_cluster: u32,
        contiguous: bool,
        slot: usize,
    ) -> Result<Option<(u64, usize)>, Error> {
        if dir_cluster < 2 {
            return Ok(None); // a directory of size 0 has no cluster
        }
        let slots_per_cluster = self.sectors_per_cluster as usize * SLOTS_PER_SECTOR;
        let mut cluster = dir_cluster;
        let mut base = 0usize;
        loop {
            if slot < base + slots_per_cluster {
                let local = slot - base;
                let sector_in = local / SLOTS_PER_SECTOR;
                let slot_in = local % SLOTS_PER_SECTOR;
                let lba = self.cluster_to_lba(cluster) as u64 + sector_in as u64;
                return Ok(Some((lba, slot_in * ENTRY_SIZE)));
            }
            base += slots_per_cluster;
            match self.advance(cluster, contiguous)? {
                Some(next) => cluster = next,
                None => return Ok(None),
            }
        }
    }

    /// Grow a directory past its extent by a cluster, and record its new size.
    ///
    /// The root has no size of its own anywhere (its chain is its extent): a
    /// zeroed cluster is linked to its chain's tail. Any other directory's
    /// DataLength and ValidDataLength, in its set in its parent, are where
    /// other drivers stop, so they are raised by the cluster; until 2026-10-18
    /// they were not, and entries written past the first cluster could not be
    /// found. The size is the chain's on entry (`dir_at`).
    ///
    /// A directory with no cluster is given its first. A `NoFatChain`
    /// directory (as macOS and Windows make them) is first given a FAT chain
    /// over its run, as `write_at` does for a file, and its set is written
    /// FAT-chained at once, before anything is linked: a failure after the
    /// link then leaves a chain past the size, which the next `dir_at`
    /// refuses, where a set still saying `NoFatChain` would have its run
    /// ended again by the next growth and the linked cluster lost.
    fn grow_dir(&mut self, dir: &mut Dir) -> Result<(), Error> {
        let cluster_bytes = self.cluster_bytes();
        let e = &mut dir.entry;
        let (Some(at), Some(x)) = (dir.at, dir.extent.as_mut()) else {
            let root = self.dir_extent(e.first_cluster, false, 0)?;
            let new = self.alloc_cluster()?;
            self.zero_cluster(new)?;
            return self.write_fat_entry(root.tail, new);
        };
        if x.clusters == 0 {
            let new = self.alloc_cluster()?;
            self.zero_cluster(new)?;
            self.patch_set_stream(at, new, cluster_bytes)?;
            e.first_cluster = new;
            e.contiguous = false;
            e.size = cluster_bytes;
            *x = Extent { clusters: 1, tail: new, past_size: false };
            return Ok(());
        }
        if e.contiguous {
            let size = x.clusters * cluster_bytes;
            self.ensure_chained(e.first_cluster, true, size)?;
            self.patch_set_stream(at, e.first_cluster, size)?;
            e.contiguous = false;
        }
        let new = self.alloc_cluster()?;
        self.zero_cluster(new)?;
        self.write_fat_entry(x.tail, new)?;
        x.clusters += 1;
        x.tail = new;
        let size = x.clusters * cluster_bytes;
        self.patch_set_stream(at, e.first_cluster, size)?;
        e.size = size;
        Ok(())
    }

    /// Global slot index of the first of `need` consecutive available slots in a
    /// directory (available = end-marker `0x00` or a deleted entry), growing the
    /// directory by a cluster whenever the walk runs off its extent. Bounded by
    /// the extent since 2026-10-18: a contiguous directory's walk ran on into
    /// the clusters after it, and the slots there were taken as free and
    /// written, another file's bytes or a free cluster.
    fn find_free_run(&mut self, dir: &mut Dir, need: usize) -> Result<usize, Error> {
        let mut run_start = 0usize;
        let mut run = 0usize;
        let mut slot = 0usize;
        loop {
            // The root's scan ends where its chain does (slot_addr), as its
            // extent does.
            let slots_per_cluster = self.sectors_per_cluster as u64 * SLOTS_PER_SECTOR as u64;
            let inside = dir.extent.is_none_or(|x| (slot as u64) < x.clusters * slots_per_cluster);
            let addr = if inside {
                self.slot_addr(dir.entry.first_cluster, dir.entry.contiguous, slot)?
            } else {
                None
            };
            match addr {
                Some((lba, off)) => {
                    let mut buf = [0u8; SECTOR_SIZE];
                    self.disk.read_sector(lba, &mut buf)?;
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
                None => self.grow_dir(dir)?, // then re-read this slot
            }
        }
    }

    /// Write `set` (a built entry set, `set.len()/32` entries) into `need`
    /// consecutive slots starting at global slot `start`, one 32-byte
    /// read-modify-write per entry (slots may straddle sector/cluster edges).
    fn write_set_at(
        &mut self,
        dir_cluster: u32,
        contiguous: bool,
        start: usize,
        set: &[u8],
    ) -> Result<(), Error> {
        for (i, entry) in set.chunks_exact(ENTRY_SIZE).enumerate() {
            let (lba, off) = self
                .slot_addr(dir_cluster, contiguous, start + i)?
                .ok_or(Error::DiskFull)?;
            let mut buf = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut buf)?;
            buf[off..off + ENTRY_SIZE].copy_from_slice(entry);
            self.disk.write_sector(lba, &buf)?;
        }
        Ok(())
    }

    /// Build an entry set and insert it into a directory: the full create path
    /// shared by `touch`/`mkdir`/`write_file`/`mv`. `data_len` is the file's
    /// size (or a directory's allocated size); `first_cluster` its first cluster
    /// (`0` for an empty file).
    fn create_entry(
        &mut self,
        dir: &mut Dir,
        name: &str,
        is_dir: bool,
        first_cluster: u32,
        data_len: u64,
        data_contiguous: bool,
    ) -> Result<(), Error> {
        let mut set = [0u8; MAX_SET_BYTES];
        let entries = build_entry_set(name, is_dir, first_cluster, data_len, data_contiguous, &mut set)
            .ok_or(Error::InvalidName)?;
        let total_bytes = entries * ENTRY_SIZE;
        let start = self.find_free_run(dir, entries)?;
        self.write_set_at(dir.entry.first_cluster, dir.entry.contiguous, start, &set[..total_bytes])
    }

    /// Resolve a path's *parent* to a mountable directory entry, checking it's a
    /// directory and that `name` doesn't already exist - the shared front half
    /// of `touch`/`mkdir`. Returns the parent, to create in.
    fn parent_for_create(&mut self, path: &str) -> Result<(Dir, ExistingKind), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.dir_at(parent_path)?;
        let p = parent.entry;
        let mut kind = ExistingKind::None;
        self.walk_dir(p.first_cluster, p.contiguous, p.size, |entry, _slot, _len| {
            if entry.name().eq_ignore_ascii_case(name) {
                kind = if entry.is_dir {
                    ExistingKind::Dir
                } else {
                    ExistingKind::File
                };
                true
            } else {
                false
            }
        })?;
        Ok((parent, kind))
    }

    /// Locate a named entry in a directory, also returning its set's on-disk
    /// position (primary slot + entry count) so the set can be patched or
    /// deleted. The located sibling of the lookup `parent_for_create` does.
    fn find_in_parent(
        &mut self,
        dir_cluster: u32,
        dir_contig: bool,
        dir_size: u64,
        name: &str,
    ) -> Result<Option<(DirEntry, usize, usize)>, Error> {
        let mut found: Option<(DirEntry, usize, usize)> = None;
        self.walk_dir(dir_cluster, dir_contig, dir_size, |entry, slot, len| {
            if entry.name().eq_ignore_ascii_case(name) {
                found = Some((*entry, slot, len));
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
        if data.is_empty() {
            return Ok(0);
        }
        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        let num = data.len().div_ceil(cluster_bytes);
        let mut first = 0u32;
        let mut prev = 0u32;
        let mut written = 0usize;
        for i in 0..num {
            let cluster = self.alloc_cluster()?;
            if i == 0 {
                first = cluster;
            } else {
                self.write_fat_entry(prev, cluster)?;
            }
            let lba = self.cluster_to_lba(cluster);
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
        Ok(first)
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
    /// An entry of size 0 that names a cluster gets that one cluster ended:
    /// the spec wants `DataLength > 0` with `NoFatChain`, but a tool may
    /// leave one, and its cluster's FAT entry is then undefined, so a write
    /// following it as a link went into whatever cluster it named (the
    /// review of #316) and could leave the write cursor on a chain the write
    /// itself had cut (the review of the finer cursor rule, 2026-10-18).
    fn ensure_chained(&mut self, first: u32, contiguous: bool, size: u64) -> Result<(), Error> {
        if !contiguous || first < 2 {
            return Ok(());
        }
        let cluster_bytes = self.sectors_per_cluster as u64 * SECTOR_SIZE as u64;
        let n = (size.div_ceil(cluster_bytes) as u32).max(1);
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
    /// first-cluster / data-length (FAT-chained), then recompute the
    /// `SetChecksum` over the whole set - the exFAT equivalent of
    /// `fat32::patch_entry_cluster_size`, plus the checksum the format requires.
    /// Reads all `set_len` entries (to checksum), rewrites the File + Stream
    /// entries (the two that changed; the name entries are untouched).
    fn patch_stream_ext(
        &mut self,
        dir_cluster: u32,
        dir_contig: bool,
        primary_slot: usize,
        set_len: usize,
        first_cluster: u32,
        data_len: u64,
    ) -> Result<(), Error> {
        let mut set = [0u8; MAX_SET_BYTES];
        let count = set_len.min(MAX_SET_ENTRIES);
        for i in 0..count {
            let (lba, off) = self
                .slot_addr(dir_cluster, dir_contig, primary_slot + i)?
                .ok_or(Error::NotFound)?;
            let mut sec = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut sec)?;
            set[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE].copy_from_slice(&sec[off..off + ENTRY_SIZE]);
        }
        let se = ENTRY_SIZE;
        set[se + 1] = SECONDARY_ALLOC_POSSIBLE; // FAT-chained now (NoFatChain clear)
        set[se + 8..se + 16].copy_from_slice(&data_len.to_le_bytes()); // ValidDataLength
        set[se + 20..se + 24].copy_from_slice(&first_cluster.to_le_bytes());
        set[se + 24..se + 32].copy_from_slice(&data_len.to_le_bytes()); // DataLength
        let sum = set_checksum(&set[..count * ENTRY_SIZE]);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        // Rewrite the two changed entries (File carries the new checksum).
        for i in 0..2 {
            let (lba, off) = self
                .slot_addr(dir_cluster, dir_contig, primary_slot + i)?
                .ok_or(Error::NotFound)?;
            let mut sec = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut sec)?;
            sec[off..off + ENTRY_SIZE]
                .copy_from_slice(&set[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE]);
            self.disk.write_sector(lba, &sec)?;
        }
        Ok(())
    }

    /// [`patch_stream_ext`](Self::patch_stream_ext) of the set at `at`.
    fn patch_set_stream(&mut self, at: SetAt, first_cluster: u32, data_len: u64) -> Result<(), Error> {
        self.patch_stream_ext(at.dir_cluster, at.dir_contiguous, at.slot, at.len, first_cluster, data_len)
    }

    // ---- write surface ------------------------------------------------------

    /// Create an empty (zero-byte) file, or succeed as a no-op if a file already
    /// exists there (no RTC to update, same as `fat32::touch`). An empty file
    /// needs no cluster: first cluster `0`, length `0` - just one entry set.
    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        let (_parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (mut dir, kind) = self.parent_for_create(path)?;
        match kind {
            ExistingKind::File => return Ok(()),
            ExistingKind::Dir => return Err(Error::NotAFile),
            ExistingKind::None => {}
        }
        self.create_entry(&mut dir, name, false, 0, 0, false)
    }

    /// Create a file with exactly `data`, or fully replace an existing file's
    /// contents. Mirrors `fat32::write_file`'s ordering: the new chain is
    /// allocated and written *before* the old file is touched, so a failure
    /// partway never unlinks an existing file. Then the old chain is freed and
    /// the entry patched (or a new entry created).
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let mut dir = self.dir_at(parent_path)?;
        let parent = dir.entry;
        let existing =
            self.find_in_parent(parent.first_cluster, parent.contiguous, parent.size, name)?;
        if let Some((entry, _, _)) = existing {
            if entry.is_dir {
                return Err(Error::NotAFile);
            }
        }

        // Write the replacement chain first (nothing existing touched yet).
        let new_cluster = self.write_chain(data)?;

        match existing {
            Some((entry, primary_slot, set_len)) => {
                if entry.first_cluster >= 2 {
                    self.free_data(entry.first_cluster, entry.contiguous, entry.size)?;
                }
                self.patch_stream_ext(
                    parent.first_cluster,
                    parent.contiguous,
                    primary_slot,
                    set_len,
                    new_cluster,
                    data.len() as u64,
                )
            }
            None => self.create_entry(&mut dir, name, false, new_cluster, data.len() as u64, false),
        }
    }

    /// Write `data` at byte `offset`, extending the file as needed without
    /// rewriting the bytes before `offset` - the primitive behind streaming
    /// `cp`, `>>`, and `writeat`. Mirrors `fat32::write_at` (grow-only size, a
    /// zero-filled gap past EOF bounded by `MAX_GAP_FILL`, a unified per-sector
    /// pass), with allocation through the bitmap and a contiguous file first
    /// converted to a FAT chain so it can be extended.
    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let (entry, primary_slot, set_len) = self
            .find_in_parent(parent.first_cluster, parent.contiguous, parent.size, name)?
            .ok_or(Error::NotFound)?;
        if entry.is_dir {
            return Err(Error::NotAFile);
        }

        let old_size = entry.size;
        if offset > old_size && offset - old_size > MAX_GAP_FILL {
            return Err(Error::InvalidOffset);
        }

        // Make the existing allocation FAT-chained so it can be extended.
        self.ensure_chained(entry.first_cluster, entry.contiguous, old_size)?;

        let cluster_bytes = self.sectors_per_cluster as u64 * SECTOR_SIZE as u64;
        let mut head_cluster = entry.first_cluster;
        if head_cluster < 2 {
            head_cluster = self.alloc_cluster()?;
        }

        let write_start = old_size.min(offset);
        let write_end = offset + data.len() as u64;

        // Walk to the cluster holding `write_start`, extending if needed:
        // from the write cursor when it is this file's and not past
        // `write_start`, else from the head cluster.
        // A write's walk is always FAT-chained: `ensure_chained` ran above.
        let (mut cluster, pos) = ChainCursor::start(self.write_cursor, head_cluster, false, write_start as usize);
        let mut cluster_pos = pos as u64;
        while cluster_pos + cluster_bytes <= write_start {
            cluster = match self.next_cluster(cluster)? {
                Some(next) => next,
                None => self.extend_chain(cluster)?,
            };
            cluster_pos += cluster_bytes;
        }

        let mut pos = write_start;
        while pos < write_end {
            while pos >= cluster_pos + cluster_bytes {
                cluster = match self.next_cluster(cluster)? {
                    Some(next) => next,
                    None => self.extend_chain(cluster)?,
                };
                cluster_pos += cluster_bytes;
            }
            let sector_in_cluster = (pos - cluster_pos) / SECTOR_SIZE as u64;
            let sector_lba = self.cluster_to_lba(cluster) as u64 + sector_in_cluster;
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
            } else if sector_start < old_size {
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
        self.patch_stream_ext(
            parent.first_cluster,
            parent.contiguous,
            primary_slot,
            set_len,
            head_cluster,
            new_size,
        )
    }

    /// Mark every entry of a set deleted by clearing its in-use bit (`0x85` ->
    /// `0x05`, `0xC0` -> `0x40`, `0xC1` -> `0x41`) - how exFAT removes a set (no
    /// `0xE5` tombstone byte like FAT). RMW one 32-byte slot at a time.
    fn delete_set(
        &mut self,
        dir_cluster: u32,
        dir_contig: bool,
        primary_slot: usize,
        set_len: usize,
    ) -> Result<(), Error> {
        for i in 0..set_len {
            let (lba, off) = self
                .slot_addr(dir_cluster, dir_contig, primary_slot + i)?
                .ok_or(Error::NotFound)?;
            let mut sec = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut sec)?;
            sec[off] &= !ET_IN_USE;
            self.disk.write_sector(lba, &sec)?;
        }
        Ok(())
    }

    /// Create an empty subdirectory. Fails if the name exists. An exFAT
    /// directory has *no* `.`/`..` entries (unlike FAT32) - an empty one is just
    /// a zeroed cluster whose first slot is the end-of-directory marker.
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        let (_parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let (mut dir, kind) = self.parent_for_create(path)?;
        if kind != ExistingKind::None {
            return Err(Error::AlreadyExists);
        }
        // Claim + zero the new directory's cluster before linking it in.
        let new_cluster = self.alloc_cluster()?;
        self.zero_cluster(new_cluster)?;
        let cluster_bytes = self.sectors_per_cluster as u64 * SECTOR_SIZE as u64;
        self.create_entry(&mut dir, name, true, new_cluster, cluster_bytes, false)
    }

    /// Remove a file (rejects a directory with `NotAFile`). Frees its clusters
    /// (bitmap + FAT), then deletes its entry set.
    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let (entry, slot, len) = self
            .find_in_parent(parent.first_cluster, parent.contiguous, parent.size, name)?
            .ok_or(Error::NotFound)?;
        if entry.is_dir {
            return Err(Error::NotAFile);
        }
        if entry.first_cluster >= 2 {
            self.free_data(entry.first_cluster, entry.contiguous, entry.size)?;
        }
        self.delete_set(parent.first_cluster, parent.contiguous, slot, len)
    }

    /// Remove an empty subdirectory (rejects a non-empty one, a file, and the
    /// root). Frees its cluster(s), then deletes its entry set.
    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        split_parent(path).ok_or(Error::CannotRemoveRoot)?;
        // Refused when its chain is not its size: a set past it would be lost.
        let dir = self.dir_at(path)?;
        let at = dir.at.ok_or(Error::CannotRemoveRoot)?;
        let entry = dir.entry;
        // Empty means the directory has no in-use entry set at all.
        let mut empty = true;
        self.walk_dir(entry.first_cluster, entry.contiguous, entry.size, |_e, _s, _l| {
            empty = false;
            true
        })?;
        if !empty {
            return Err(Error::DirectoryNotEmpty);
        }
        // Its extent: its chain, which `dir_at` found its size.
        let clusters = dir.extent.map_or(0, |x| x.clusters);
        if clusters > 0 {
            let bytes = clusters * self.cluster_bytes();
            self.free_data(entry.first_cluster, entry.contiguous, bytes)?;
        }
        self.delete_set(at.dir_cluster, at.dir_contiguous, at.slot, at.len)
    }

    /// Rename or move a file/directory. Re-points a new entry set at `src`'s
    /// existing clusters (no data copy), preserving its `NoFatChain` layout,
    /// then deletes `src`'s set - write-new-before-delete-old ordering. exFAT
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

        let (src_entry, src_slot, src_len) = self
            .find_in_parent(src_parent.first_cluster, src_parent.contiguous, src_parent.size, src_name)?
            .ok_or(Error::NotFound)?;

        // An existing destination is REPLACED when both it and `src` are
        // ordinary files. Like FAT32 and unlike ext2 this cannot be ATOMIC: an
        // exFAT entry set carries the file's own location, so the change takes
        // two writes rather than one. It is NOT a window in which the name is
        // absent: the new set is written before the old one is deleted, so a
        // reader in between finds two and gets one of the two files, never
        // nothing.
        if let Some((dst_entry, dst_slot, dst_len)) = self.find_in_parent(
            dst_parent.first_cluster,
            dst_parent.contiguous,
            dst_parent.size,
            dst_name,
        )? {
            // Name matching is up-case folded, so `src` and `dst` can be the
            // SAME set two ways. `mv f f` is a genuine no-op; `mv FOO.TXT
            // foo.txt` is a real request to change the stored spelling, and
            // reporting Ok without doing it would claim a success that did not
            // happen. The create-first order below performs it: write the set
            // under the new name, delete the old one, and skip both the
            // source-side delete (same slot) and the data free (same file).
            let same_set =
                src_parent.first_cluster == dst_parent.first_cluster && src_slot == dst_slot;
            if same_set && src_name.as_bytes() == dst_name.as_bytes() {
                return Ok(());
            }
            if src_entry.is_dir || dst_entry.is_dir {
                return Err(Error::AlreadyExists);
            }
            // CREATE BEFORE DELETING, like the non-replacing path below.
            // Deleting the destination first survives a crash no worse, and is
            // wrong against an ordinary error: a failing `create_entry` (a
            // directory that cannot be extended, an allocation failure) would
            // return Err having already destroyed `dst` while `src` is still
            // there. Slot indices are stable across a create - `create_entry`
            // fills a free run and moves nothing - so the recorded `dst_slot`
            // still addresses the old set afterwards.
            self.create_entry(
                &mut dst_dir,
                dst_name,
                src_entry.is_dir,
                src_entry.first_cluster,
                src_entry.size,
                src_entry.contiguous,
            )?;
            self.delete_set(dst_parent.first_cluster, dst_parent.contiguous, dst_slot, dst_len)?;
            if !same_set {
                self.delete_set(src_parent.first_cluster, src_parent.contiguous, src_slot, src_len)?;
                // Two guards: a case-only rename re-links the same set, and a
                // cross-linked image can give two sets the same first cluster.
                // Freeing then destroys the data the surviving name points at.
                if dst_entry.first_cluster >= 2
                    && dst_entry.first_cluster != src_entry.first_cluster
                {
                    self.free_data(dst_entry.first_cluster, dst_entry.contiguous, dst_entry.size)?;
                }
            }
            return Ok(());
        }

        // Link dst to the same clusters first, then unlink src.
        self.create_entry(
            &mut dst_dir,
            dst_name,
            src_entry.is_dir,
            src_entry.first_cluster,
            src_entry.size,
            src_entry.contiguous,
        )?;
        self.delete_set(src_parent.first_cluster, src_parent.contiguous, src_slot, src_len)
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

/// ASCII up-case one UTF-16 unit (the standard exFAT up-case table is the
/// identity for non-ASCII, and ASCII `a-z -> A-Z` for the rest - correct for
/// the ASCII names this system creates; see the module doc).
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

/// Encode `name` to UTF-16 into `out`, validating it (non-empty, in range,
/// no exFAT-forbidden characters). Returns the char count.
fn encode_name(name: &str, out: &mut [u16; LONG_NAME_MAX]) -> Option<usize> {
    let mut n = 0usize;
    for ch in name.chars() {
        if n >= LONG_NAME_MAX {
            return None;
        }
        let c = ch as u32;
        if !(0x20..=0xffff).contains(&c) {
            return None; // control chars, and astral/surrogate code points
        }
        if matches!(ch, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|') {
            return None; // exFAT-forbidden filename characters
        }
        out[n] = c as u16;
        n += 1;
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

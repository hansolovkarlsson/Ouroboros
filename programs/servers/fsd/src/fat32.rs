//! The filesystem server's FAT32 engine - the kernel's old
//! `kernel/src/fat32.rs`, moved here verbatim when the filesystem left
//! the kernel (driver isolation part 2), with exactly one structural
//! change: the `block::BlockDevice` it used to own by value became
//! [`Disk`], a zero-sized handle whose `read_sector`/`write_sector`
//! are the `BLOCK_*` syscalls (the kernel keeps the device and only
//! accepts those calls from this task). Every algorithm, ordering
//! decision, and documented bug fix is unchanged - see the original
//! module's history in `docs/CHANGELOG.md` and CLAUDE.md (phases 3b
//! through 8, directory extension, the cluster-0-means-root saga).
//!
//! Still hand-rolled, still zero-heap - a constraint that carries over
//! unchanged: userland programs here have no allocator at all (no
//! `.bss`/`.data`, stack-only state - see `docs/processes.md`), which
//! is even stricter than the post-`exit_boot_services` kernel this
//! code was written for.
//!
//! Long filenames (LFN) are **read and written** now. A name that fits an
//! 8.3 short name still becomes a plain short entry (unchanged behavior,
//! uppercased); a name that doesn't - too long, mixed dots, spaces, or
//! any character 8.3 can't hold - gets a generated `NAME~N` short alias
//! plus a run of LFN entries carrying the real name (see
//! [`Fs::insert_named_entry`]). `make_short_name` is the 8.3 fast-path
//! test; [`generate_short_alias`] builds the `~N` alias when it fails.
//! Deleting an LFN-named file frees its LFN entries too (see
//! [`Fs::free_entry_with_lfn`]), so `rm`/`rmdir`/`mv` no longer leave
//! orphaned long-name entries behind. Still first-FAT32-partition only,
//! and `make run`'s vvfat disk is still FAT16 - so mounting fails there
//! and every request answers `NO_FS`, same degradation as always.

use crate::disk::{Disk, DiskError};

const SECTOR_SIZE: usize = 512;
const DIR_ENTRY_SIZE: usize = 32;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LFN: u8 = 0x0f;
const DIR_ENTRY_FREE: u8 = 0xe5;
const DIR_ENTRY_END: u8 = 0x00;
const END_OF_CHAIN_MIN: u32 = 0x0fff_fff8;
const MAX_NAME_LEN: usize = 12; // 8 name + '.' + 3 ext, the most an 8.3 short name is ever
/// Longest reconstructed name a `DirEntry` can hold - a FAT long filename
/// (LFN) is up to 255 UTF-16 chars; an 8.3 short name uses at most
/// [`MAX_NAME_LEN`] of this buffer.
const LONG_NAME_MAX: usize = 255;
/// Characters one LFN directory entry carries (they live at three
/// non-contiguous field ranges - see [`LFN_POS`]).
const LFN_CHARS_PER_ENTRY: usize = 13;
/// The `0x40` bit set in the sequence byte of the *last* LFN entry (the
/// highest-numbered one, stored physically first), marking the start of a
/// long-name run - the read side masks it off in [`walk_dir`](Fs::walk_dir).
const LFN_LAST_MASK: u8 = 0x40;
/// Most LFN entries a single name can need: `ceil(LONG_NAME_MAX / 13)`.
/// A full name lays out `MAX_LFN_ENTRIES` LFN entries plus one short
/// entry, so the run buffers below are sized `MAX_LFN_ENTRIES + 1`.
const MAX_LFN_ENTRIES: usize = LONG_NAME_MAX.div_ceil(LFN_CHARS_PER_ENTRY); // 20
/// The 13 within-entry byte offsets of an LFN entry's UTF-16LE characters
/// (the write-side twin of the read-side positions in [`lfn_chars`]).
const LFN_POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
/// Cap on a single `write_at` gap zero-fill (`offset` past the old EOF).
/// FAT32 has no sparse representation, so a gap is zero-filled sector by
/// sector - fine for an editor/log, but a fat-fingered huge offset must not
/// try to zero-fill the whole volume. 1 MiB is far above any real use here.
const MAX_GAP_FILL: u64 = 1 << 20;

#[derive(Debug)]
// The two carried fields (the offending sector size, the disk-error
// kind) are no longer read since the kernel module's `Display` impl was
// dropped in the port (`error_name` in main.rs uses fixed strings) -
// kept anyway for fidelity with the original and any future
// diagnostics rather than flattened to unit variants.
#[allow(dead_code)]
pub enum Error {
    NoFat32Partition,
    NotFat32,
    /// The probed partition isn't an exFAT volume either - the exFAT arm's
    /// analogue of [`NotFat32`], so `vfs::mount` can try the next partition.
    NotExFat,
    /// The probed partition isn't an ext2 volume either (no `0xEF53` magic) -
    /// the ext2 arm's analogue of [`NotFat32`], so `vfs::mount` moves on.
    NotExt2,
    /// A write was attempted on a read-only filesystem (the exFAT arm, whose
    /// write support is a later milestone). FAT32 is fully read-write and
    /// never returns this. This is the shared `Error` type for every
    /// filesystem arm (see `vfs.rs`), so it lives here with the rest.
    ReadOnly,
    /// A metadata write (`chmod`/`chown`) was attempted on a filesystem that
    /// can't model mode/ownership (FAT32/exFAT/`/proc`). ext2 is the only arm
    /// that supports it; the others return this so the client degrades honestly
    /// rather than silently no-op'ing. Maps to `FS_ERR_NOT_SUPPORTED`.
    Unsupported,
    UnsupportedSectorSize(u16),
    Io(DiskError),
    NotFound,
    NotAFile,
    NotADirectory,
    InvalidName,
    AlreadyExists,
    DirectoryNotEmpty,
    CannotRemoveRoot,
    DiskFull,
    /// `write_at` was asked to write past the current end of file, which
    /// would leave a sparse gap FAT32 can't represent. Sequential/append
    /// callers never hit this; it's a guard, not an expected path.
    InvalidOffset,
}

impl From<DiskError> for Error {
    fn from(e: DiskError) -> Self {
        Error::Io(e)
    }
}

/// One directory entry, decoded into a self-contained, no-alloc form -
/// the raw 32-byte on-disk record doesn't outlive the sector buffer it
/// was read from, so callers get this instead.
#[derive(Clone, Copy)]
pub struct DirEntry {
    name: [u8; LONG_NAME_MAX],
    name_len: u8,
    pub is_dir: bool,
    pub size: u32,
    cluster: u32,
    /// The FAT "write date" (raw offset 24) and "write time" (offset 22),
    /// each a packed 16-bit field - decoded to a calendar by [`Fs::stat`].
    pub mtime_date: u16,
    pub mtime_time: u16,
}

impl DirEntry {
    pub fn name(&self) -> &str {
        // Built either from ASCII short-name bytes or an LFN reconstructed to
        // ASCII (non-ASCII chars replaced with '?'), so always valid UTF-8.
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }

    /// Decode a short (8.3) directory entry. `long_name`, when `Some`, is the
    /// reconstructed long filename from the preceding LFN entries (already
    /// checksum-validated against this short entry by the caller) and is used
    /// verbatim in place of the 8.3 name.
    fn parse(raw: &[u8], long_name: Option<&[u8]>) -> Self {
        let attr = raw[11];
        let cluster_hi = u16::from_le_bytes([raw[20], raw[21]]) as u32;
        let cluster_lo = u16::from_le_bytes([raw[26], raw[27]]) as u32;
        let size = u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]);
        let mtime_time = u16::from_le_bytes([raw[22], raw[23]]);
        let mtime_date = u16::from_le_bytes([raw[24], raw[25]]);

        let mut name = [0u8; LONG_NAME_MAX];
        let len = if let Some(long) = long_name {
            let n = long.len().min(LONG_NAME_MAX);
            name[..n].copy_from_slice(&long[..n]);
            n
        } else {
            let mut len = 0usize;
            for &b in &raw[0..8] {
                if b == b' ' {
                    break;
                }
                name[len] = b;
                len += 1;
            }
            let ext_len = raw[8..11].iter().take_while(|&&b| b != b' ').count();
            if ext_len > 0 {
                name[len] = b'.';
                len += 1;
                name[len..len + ext_len].copy_from_slice(&raw[8..8 + ext_len]);
                len += ext_len;
            }
            len
        };

        DirEntry {
            name,
            name_len: len as u8,
            is_dir: attr & ATTR_DIRECTORY != 0,
            size,
            cluster: (cluster_hi << 16) | cluster_lo,
            mtime_date,
            mtime_time,
        }
    }

    fn root(root_cluster: u32) -> Self {
        DirEntry {
            name: [0; LONG_NAME_MAX],
            name_len: 0,
            is_dir: true,
            size: 0,
            cluster: root_cluster,
            mtime_date: 0,
            mtime_time: 0,
        }
    }
}

/// Decode the FAT packed "write date"/"write time" fields into a calendar, or
/// `None` if the date is zero (no timestamp recorded). FAT date: bits 15..9 =
/// year since 1980, 8..5 = month (1-12), 4..0 = day (1-31). FAT time: bits
/// 15..11 = hour, 10..5 = minute, 4..0 = seconds/2.
fn decode_fat_time(date: u16, time: u16) -> Option<crate::vfs::CalTime> {
    if date == 0 {
        return None;
    }
    let year = 1980 + (date >> 9);
    let month = ((date >> 5) & 0x0f) as u8;
    let day = (date & 0x1f) as u8;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(crate::vfs::CalTime {
        year,
        month,
        day,
        hour: (time >> 11) as u8,
        min: ((time >> 5) & 0x3f) as u8,
        sec: ((time & 0x1f) * 2) as u8,
    })
}

/// The LFN checksum stored in every long-name entry (byte 13): a rolling
/// checksum of the associated short entry's 11-byte 8.3 name. We recompute it
/// from the short entry and require it to match before trusting the LFN -
/// otherwise an *orphaned* run of LFN entries (whose short entry was deleted
/// and its slot reused) would attach the wrong long name to the next file.
fn lfn_checksum(short_name: &[u8]) -> u8 {
    let mut sum: u8 = 0;
    for &b in &short_name[..11] {
        sum = (sum >> 1).wrapping_add((sum & 1) << 7).wrapping_add(b);
    }
    sum
}

/// Extract the up-to-13 UTF-16LE characters from one LFN entry into `out` as
/// ASCII bytes (non-ASCII -> '?'), stopping at the `0x0000` name terminator or
/// `0xFFFF` padding. Returns how many were written. The 13 chars live at three
/// non-contiguous field ranges within the 32-byte entry.
fn lfn_chars(raw: &[u8], out: &mut [u8; 13]) -> usize {
    let mut n = 0;
    for &p in &LFN_POS {
        let c = u16::from_le_bytes([raw[p], raw[p + 1]]);
        if c == 0x0000 || c == 0xffff {
            break;
        }
        out[n] = if c < 0x80 { c as u8 } else { b'?' };
        n += 1;
    }
    n
}

/// Microsoft's recommended FAT32 sectors-per-cluster by volume size (in
/// 512-byte sectors), from the fatgen103 disk-size table - used by
/// [`Fs::format`]. Keeps the cluster count in the valid FAT32 range and the
/// FAT a bounded size as the volume grows.
fn sectors_per_cluster_for(total_sectors: u32) -> u32 {
    match total_sectors {
        0..=532_480 => 1,              // <= 260 MB
        532_481..=16_777_216 => 8,     // <= 8 GB
        16_777_217..=33_554_432 => 16, // <= 16 GB
        33_554_433..=67_108_864 => 32, // <= 32 GB
        _ => 64,
    }
}

pub struct Fs {
    disk: Disk,
    /// The volume's first sector (the BPB) - kept so `mount`-info can
    /// report where on the disk this filesystem lives (the disk-tools arc,
    /// milestone 1). Not used by any read/write path.
    partition_lba: u32,
    sectors_per_cluster: u32,
    fat_start_lba: u32,
    data_start_lba: u32,
    root_cluster: u32,
    /// How many copies of the FAT the volume has (almost always 2) - only
    /// needed for writes ([`write_fat_entry`](Self::write_fat_entry)),
    /// which keep every copy in sync rather than just the first, per
    /// spec. Reads only ever consult the first copy.
    num_fats: u32,
    /// One FAT copy's size, in sectors - needed to locate the *other*
    /// copies (`fat_start_lba + i * fat_size_32`) and to bound
    /// [`find_free_cluster`](Self::find_free_cluster)'s scan.
    fat_size_32: u32,
    /// Sequential-read cursor - the fix for the "large-read fsd restart"
    /// bug. [`read_at`](Self::read_at)'s seek walks the file's cluster
    /// chain from its *start* to reach `offset`, and [`next_cluster`] reads
    /// a FAT sector per step - so a client reading a multi-MB file in
    /// [`SAFECOPY_MAX`]-sized chunks re-walks an ever-longer prefix each
    /// call: O(n^2) disk reads overall, and a single late-offset request
    /// issuing hundreds/thousands of FAT reads in one uninterrupted
    /// `handle()` call. On slow real hardware that one request runs past
    /// the supervisor's runnable-wedge threshold (`WEDGE_TICKS`, ~2.56s)
    /// and fsd is restarted mid-read, dropping the mount. Caching where the
    /// last walk landed lets a *forward* read resume from there instead:
    /// each request becomes O(chunk), fsd returns to `msg_recv` between
    /// chunks (resetting the wedge counter and servicing the health-ping -
    /// the netd "small bursts, drain between each" pattern, reached
    /// structurally), and the large read is O(n). Only the chain *position*
    /// is cached, never data - reads still fetch every sector fresh - and
    /// it is invalidated on any FAT mutation ([`write_fat_entry`]), so a
    /// read can never follow a stale chain. `None` = no valid cursor.
    read_cursor: Option<ReadCursor>,
}

/// A remembered point in a file's cluster chain for [`Fs::read_at`]'s
/// sequential-read fast path - see [`Fs::read_cursor`].
#[derive(Clone, Copy)]
struct ReadCursor {
    /// The file's start cluster - identifies which file this cursor is for.
    /// A read whose file has a different start cluster ignores the cursor.
    file_cluster: u32,
    /// A cluster in that file's chain reachable by walking forward from the
    /// file's start, and...
    cluster: u32,
    /// ...the file-byte position of that cluster's first byte (an exact
    /// multiple of the cluster size). The invariant the fast path relies
    /// on: `cluster` is the chain cluster covering `[cluster_pos,
    /// cluster_pos + cluster_bytes)`.
    cluster_pos: usize,
}

impl Fs {
    /// Mount the FAT32 volume whose first sector is at `partition_lba`: read
    /// and validate its BPB and compute the FAT/data region layout. Returns
    /// [`Error::NotFat32`] if the sector isn't a FAT32 BPB, so the caller
    /// (`vfs::mount`) can try the next partition. Partition *discovery* (MBR or
    /// GPT) is now `partition.rs`'s job - this doesn't read the partition table.
    ///
    /// The kernel must have a block device installed (`BLOCK_INFO`
    /// answering with a capacity) - every sector call in this module
    /// relies on that, probed once by the caller rather than at each
    /// call site.
    pub fn mount_at(mut disk: Disk, partition_lba: u32) -> Result<Self, Error> {
        let mut bpb = [0u8; SECTOR_SIZE];
        disk.read_sector(partition_lba as u64, &mut bpb)?;

        let bytes_per_sector = u16::from_le_bytes([bpb[11], bpb[12]]);
        if bytes_per_sector as usize != SECTOR_SIZE {
            return Err(Error::UnsupportedSectorSize(bytes_per_sector));
        }
        let sectors_per_cluster = bpb[13] as u32;
        let reserved_sectors = u16::from_le_bytes([bpb[14], bpb[15]]) as u32;
        let num_fats = bpb[16] as u32;
        let root_entry_count = u16::from_le_bytes([bpb[17], bpb[18]]);
        let fat_size_16 = u16::from_le_bytes([bpb[22], bpb[23]]);
        let fat_size_32 = u32::from_le_bytes([bpb[36], bpb[37], bpb[38], bpb[39]]);
        let root_cluster = u32::from_le_bytes([bpb[44], bpb[45], bpb[46], bpb[47]]);

        // RootEntryCount and FATSz16 are both required to be 0 on a real
        // FAT32 volume (FAT12/16 use them; FAT32 uses FATSz32/RootCluster
        // instead) - checked, not assumed, alongside the filesystem-type
        // string every `mkfs.fat32`-alike is expected to write.
        if root_entry_count != 0 || fat_size_16 != 0 || &bpb[82..90] != b"FAT32   " {
            return Err(Error::NotFat32);
        }

        let fat_start_lba = partition_lba + reserved_sectors;
        let data_start_lba = fat_start_lba + num_fats * fat_size_32;

        Ok(Fs {
            disk,
            partition_lba,
            sectors_per_cluster,
            fat_start_lba,
            data_start_lba,
            root_cluster,
            num_fats,
            fat_size_32,
            read_cursor: None,
        })
    }

    /// The volume's first sector - for `mount`-info reporting only.
    pub fn partition_lba(&self) -> u32 {
        self.partition_lba
    }

    /// Create a fresh FAT32 filesystem (mkfs) in the partition
    /// `[start_lba, start_lba + total_sectors)` - the inverse of
    /// [`mount_at`](Self::mount_at). Writes the boot sector + FSInfo (and
    /// their backup copies at reserved sectors 6/7), zeroes both FATs and
    /// initializes their three reserved entries (FAT[0]=media, FAT[1]=EOC,
    /// FAT[2]=EOC for the one-cluster root directory), and zeroes the root
    /// directory cluster. The layout is exactly what `mount_at` re-derives,
    /// and is what macOS's `fsck_msdos` validates. The disk-management arc,
    /// milestone 3.
    ///
    /// Returns [`Error::DiskFull`] if the partition is too small to hold a
    /// valid FAT32 (fewer than 65 525 clusters), or [`Error::Io`] on a write
    /// error. **Cost is O(FAT size) single-sector writes** - practical for
    /// the modest volumes this targets on QEMU; a real multi-GB stick would
    /// want multi-sector writes (`disk.rs` only exposes one-sector writes
    /// today).
    pub fn format(mut disk: Disk, start_lba: u32, total_sectors: u32) -> Result<(), Error> {
        const RESERVED: u32 = 32;
        const NUM_FATS: u32 = 2;
        const FAT32_MIN_CLUSTERS: u32 = 65_525;

        if total_sectors < RESERVED + NUM_FATS + 1 {
            return Err(Error::DiskFull);
        }
        let spc = sectors_per_cluster_for(total_sectors);
        // Microsoft fatgen103 FATSz32 computation.
        let tmp1 = total_sectors - RESERVED;
        let tmp2 = (256 * spc + NUM_FATS) / 2;
        let fat_size = tmp1.div_ceil(tmp2);
        let reserved_and_fats = RESERVED + NUM_FATS * fat_size;
        if total_sectors <= reserved_and_fats {
            return Err(Error::DiskFull);
        }
        let data_sectors = total_sectors - reserved_and_fats;
        let cluster_count = data_sectors / spc;
        if cluster_count < FAT32_MIN_CLUSTERS {
            return Err(Error::DiskFull);
        }

        let fat_start = start_lba + RESERVED;
        let root_cluster = 2u32;
        let zero = [0u8; SECTOR_SIZE];

        // Reserved region: zero it, then write the boot sector + FSInfo and
        // their backups.
        for s in 0..RESERVED {
            disk.write_sector((start_lba + s) as u64, &zero)?;
        }
        let mut boot = [0u8; SECTOR_SIZE];
        boot[0] = 0xEB;
        boot[1] = 0x58;
        boot[2] = 0x90;
        boot[3..11].copy_from_slice(b"MSWIN4.1");
        boot[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
        boot[13] = spc as u8;
        boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
        boot[16] = NUM_FATS as u8;
        // root_entry_count (17..19) and total_sectors_16 (19..21) stay 0 for FAT32.
        boot[21] = 0xF8; // media descriptor (fixed disk)
        // fat_size_16 (22..24) stays 0.
        boot[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors per track (cosmetic)
        boot[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads (cosmetic)
        boot[28..32].copy_from_slice(&start_lba.to_le_bytes()); // hidden sectors
        boot[32..36].copy_from_slice(&total_sectors.to_le_bytes());
        boot[36..40].copy_from_slice(&fat_size.to_le_bytes());
        // ext_flags (40..42), fs_version (42..44) stay 0.
        boot[44..48].copy_from_slice(&root_cluster.to_le_bytes());
        boot[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
        boot[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
        boot[64] = 0x80; // drive number
        boot[66] = 0x29; // extended boot signature
        boot[67..71].copy_from_slice(&0x4F55_524Fu32.to_le_bytes()); // volume serial
        boot[71..82].copy_from_slice(b"OUROBOROS  "); // 11-byte volume label
        boot[82..90].copy_from_slice(b"FAT32   "); // filesystem type (mount_at checks this)
        boot[510] = 0x55;
        boot[511] = 0xAA;
        disk.write_sector(start_lba as u64, &boot)?;
        disk.write_sector((start_lba + 6) as u64, &boot)?;

        let mut fsinfo = [0u8; SECTOR_SIZE];
        fsinfo[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes()); // "RRaA" lead signature
        fsinfo[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes()); // "rrAa" struct signature
        fsinfo[488..492].copy_from_slice(&(cluster_count - 1).to_le_bytes()); // free count (root uses 1)
        fsinfo[492..496].copy_from_slice(&3u32.to_le_bytes()); // next-free hint
        fsinfo[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes()); // trail signature
        disk.write_sector((start_lba + 1) as u64, &fsinfo)?;
        disk.write_sector((start_lba + 7) as u64, &fsinfo)?;

        // Both FATs: zero every sector, then write the three reserved entries
        // into each copy's first sector.
        for i in 0..NUM_FATS {
            let this_fat = fat_start + i * fat_size;
            for s in 0..fat_size {
                disk.write_sector((this_fat + s) as u64, &zero)?;
            }
            let mut fat0 = [0u8; SECTOR_SIZE];
            fat0[0..4].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes()); // FAT[0] = media | EOC bits
            fat0[4..8].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes()); // FAT[1] = EOC (clean bits)
            fat0[8..12].copy_from_slice(&0x0FFF_FFF8u32.to_le_bytes()); // FAT[2] = EOC (root dir chain)
            disk.write_sector(this_fat as u64, &fat0)?;
        }

        // Root directory cluster: zeroed (an empty directory).
        let data_start = fat_start + NUM_FATS * fat_size;
        let root_lba = data_start + (root_cluster - 2) * spc;
        for s in 0..spc {
            disk.write_sector((root_lba + s) as u64, &zero)?;
        }
        Ok(())
    }

    fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.data_start_lba + (cluster - 2) * self.sectors_per_cluster
    }

    /// The next cluster in a chain, or `None` at the end of it. FAT32 FAT
    /// entries are 32 bits wide but only the low 28 are significant; a
    /// value `>= 0x0FFFFFF8` marks end-of-chain (values above that up to
    /// `0x0FFFFFFF` are all valid EOC markers per the spec, not just one).
    fn next_cluster(&mut self, cluster: u32) -> Result<Option<u32>, Error> {
        let fat_byte_offset = cluster * 4;
        let sector = self.fat_start_lba + fat_byte_offset / SECTOR_SIZE as u32;
        let offset = (fat_byte_offset % SECTOR_SIZE as u32) as usize;
        let mut buf = [0u8; SECTOR_SIZE];
        self.disk.read_sector(sector as u64, &mut buf)?;
        let raw = u32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);
        let value = raw & 0x0fff_ffff;
        if value == 0 || value >= END_OF_CHAIN_MIN {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Writes one FAT entry, in every copy of the FAT the volume has (per
    /// spec - real FAT32 drivers only ever read the first copy, but keep
    /// every copy in sync on write so a driver that *does* trust a backup
    /// copy never sees a stale value). Preserves the existing entry's top
    /// 4 reserved bits, masking `value` to the low 28 - same split
    /// `next_cluster` already reads.
    fn write_fat_entry(&mut self, cluster: u32, value: u32) -> Result<(), Error> {
        // Any change to the FAT changes chain topology, so a cached
        // read cursor (a chain position) may no longer be valid - drop it.
        // This is the single choke point for *all* chain mutation
        // (allocation, free, extend), so invalidating here alone is
        // sufficient; an in-place data overwrite that leaves the chain
        // untouched never reaches this method and correctly keeps the
        // cursor. See `Fs::read_cursor`.
        self.read_cursor = None;
        let fat_byte_offset = cluster * 4;
        let sector_offset = fat_byte_offset / SECTOR_SIZE as u32;
        let offset = (fat_byte_offset % SECTOR_SIZE as u32) as usize;
        for i in 0..self.num_fats {
            let sector = self.fat_start_lba + i * self.fat_size_32 + sector_offset;
            let mut buf = [0u8; SECTOR_SIZE];
            self.disk.read_sector(sector as u64, &mut buf)?;
            let existing = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            let new = (existing & 0xf000_0000) | (value & 0x0fff_ffff);
            buf[offset..offset + 4].copy_from_slice(&new.to_le_bytes());
            self.disk.write_sector(sector as u64, &buf)?;
        }
        Ok(())
    }

    /// Scans the first FAT copy for a free (`0`) cluster, starting at 2
    /// (0 and 1 are reserved, real clusters start at 2 - same convention
    /// [`cluster_to_lba`](Self::cluster_to_lba) already assumes). Bounded
    /// by the FAT's own size, so a full disk returns
    /// [`Error::DiskFull`](Error::DiskFull) rather than looping forever.
    fn find_free_cluster(&mut self) -> Result<u32, Error> {
        let total_clusters = self.fat_size_32 * (SECTOR_SIZE as u32 / 4);
        let mut cluster = 2u32;
        let mut current_sector = u32::MAX;
        let mut buf = [0u8; SECTOR_SIZE];
        while cluster < total_clusters {
            let fat_byte_offset = cluster * 4;
            let sector = self.fat_start_lba + fat_byte_offset / SECTOR_SIZE as u32;
            if sector != current_sector {
                self.disk.read_sector(sector as u64, &mut buf)?;
                current_sector = sector;
            }
            let offset = (fat_byte_offset % SECTOR_SIZE as u32) as usize;
            let value = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]) & 0x0fff_ffff;
            if value == 0 {
                return Ok(cluster);
            }
            cluster += 1;
        }
        Err(Error::DiskFull)
    }

    /// Zeroes every sector of one cluster - used to give a freshly
    /// allocated directory cluster a clean slate (all-zero bytes read
    /// back as [`DIR_ENTRY_END`], so an empty new directory correctly
    /// looks "no more entries" to [`walk_dir`](Self::walk_dir)) before
    /// writing its real `.`/`..` entries into the first sector.
    fn zero_cluster(&mut self, cluster: u32) -> Result<(), Error> {
        let lba = self.cluster_to_lba(cluster);
        let zero = [0u8; SECTOR_SIZE];
        for s in 0..self.sectors_per_cluster {
            self.disk.write_sector((lba + s) as u64, &zero)?;
        }
        Ok(())
    }

    /// Frees `start`'s entire cluster chain in the FAT (each entry read
    /// *before* it's zeroed, or the chain's own links would be destroyed
    /// mid-walk). Shared by [`rm`](Self::rm), [`write_file`](Self::write_file)'s
    /// overwrite path, and - since directories can span more than one
    /// cluster via [`insert_dir_entry`](Self::insert_dir_entry)'s
    /// extension - [`rmdir`](Self::rmdir), whose original
    /// free-exactly-one-cluster code was correct only while every
    /// directory was single-cluster by construction and would have
    /// silently leaked extension clusters otherwise.
    fn free_chain(&mut self, start: u32) -> Result<(), Error> {
        let mut cluster = start;
        loop {
            let next = self.next_cluster(cluster)?;
            self.write_fat_entry(cluster, 0)?;
            match next {
                Some(n) => cluster = n,
                None => break,
            }
        }
        Ok(())
    }

    /// Calls `f` for every real entry (LFN/free/volume-ID entries are
    /// skipped) in the directory starting at `start_cluster`, stopping
    /// early the first time `f` returns `true`.
    fn walk_dir(
        &mut self,
        start_cluster: u32,
        mut f: impl FnMut(&DirEntry) -> bool,
    ) -> Result<(), Error> {
        self.walk_dir_with_location(start_cluster, |entry, _lba, _offset| f(entry))
    }

    /// Same as [`walk_dir`](Self::walk_dir), but `f` also receives the
    /// entry's exact on-disk location (sector LBA, byte offset within
    /// that sector) - needed by [`mkdir`](Self::mkdir)/
    /// [`rmdir`](Self::rmdir) to patch an existing entry in place rather
    /// than re-deriving where it lives. `walk_dir` is a thin wrapper over
    /// this that just ignores the location.
    fn walk_dir_with_location(
        &mut self,
        start_cluster: u32,
        mut f: impl FnMut(&DirEntry, u64, usize) -> bool,
    ) -> Result<(), Error> {
        let mut cluster = start_cluster;
        // Accumulated long filename from the LFN entries preceding a short
        // entry (they're stored, in reverse order, immediately before it).
        // `lfn_len == 0` means none pending. Reset by anything that breaks the
        // run (a free/volume/end entry, or after consuming a short entry).
        let mut lfn_name = [0u8; LONG_NAME_MAX];
        let mut lfn_len = 0usize;
        let mut lfn_sum = 0u8;
        loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for (i, raw) in buf.chunks_exact(DIR_ENTRY_SIZE).enumerate() {
                    match raw[0] {
                        DIR_ENTRY_END => return Ok(()),
                        DIR_ENTRY_FREE => {
                            lfn_len = 0; // a gap invalidates any pending LFN run
                            continue;
                        }
                        _ => {}
                    }
                    let attr = raw[11];
                    if attr == ATTR_LFN {
                        // Place this entry's 13 chars at (seq-1)*13; entries
                        // arrive high-seq first, but placing by seq makes order
                        // irrelevant. The terminator lives in the highest-seq
                        // entry, so `lfn_len` ends up the true length.
                        let seq = (raw[0] & 0x1f) as usize;
                        if seq >= 1 {
                            let mut chars = [0u8; 13];
                            let n = lfn_chars(raw, &mut chars);
                            let base = (seq - 1) * 13;
                            let end = (base + n).min(LONG_NAME_MAX);
                            if base < LONG_NAME_MAX {
                                lfn_name[base..end].copy_from_slice(&chars[..end - base]);
                                lfn_len = lfn_len.max(end);
                            }
                            lfn_sum = raw[13];
                        }
                        continue;
                    }
                    if attr & ATTR_VOLUME_ID != 0 {
                        lfn_len = 0; // volume label - not a real entry, breaks the run
                        continue;
                    }
                    // A short entry: use the pending long name only if it's
                    // present and its checksum matches this entry (guards
                    // against orphaned LFN runs attaching to the wrong file).
                    let long = if lfn_len > 0 && lfn_sum == lfn_checksum(&raw[0..11]) {
                        Some(&lfn_name[..lfn_len])
                    } else {
                        None
                    };
                    let entry = DirEntry::parse(raw, long);
                    lfn_len = 0;
                    if f(&entry, (lba + s) as u64, i * DIR_ENTRY_SIZE) {
                        return Ok(());
                    }
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
        }
    }

    /// Resolves an absolute, `/`-separated path (e.g.
    /// `/EFI/ORBS/SH.BIN`) to its directory entry, walking one
    /// directory per path component. Matching is case-insensitive, same
    /// as FAT itself.
    ///
    /// **A real, confirmed bug found while testing `cd ..` from a
    /// subdirectory whose parent is the root:** a directory's `..` entry
    /// conventionally stores cluster `0` to mean "the root directory",
    /// not the root's own (real, nonzero) cluster number - not a
    /// hypothetical, this is the actual on-disk value FAT32 formatters
    /// write, confirmed while diagnosing the hang below. Without the
    /// substitution here, that `0` flowed straight into
    /// [`cluster_to_lba`](Self::cluster_to_lba)'s `cluster - 2`, an
    /// unsigned underflow that wrapped to a huge, garbage sector number.
    /// The resulting read didn't fault (no exception was ever reported) -
    /// it hung the *entire* system indefinitely, because this all runs
    /// inside a syscall, and exception entry masks IRQs until the next
    /// `eret` - there was no tick left to preempt anything with. Confirmed
    /// via piped-stdin QEMU testing: `cd EFI`, `cd BOOT`, `cd ..`, `cd ..`
    /// (the second `..` - from `/EFI` back to root - is what actually
    /// walks a `..` entry whose value is `0`) hung with zero console
    /// output and zero reported exceptions, exactly matching a masked-IRQ
    /// infinite operation rather than a crash.
    fn find(&mut self, path: &str) -> Result<DirEntry, Error> {
        let mut current = DirEntry::root(self.root_cluster);
        for component in path.split('/').filter(|c| !c.is_empty()) {
            if !current.is_dir {
                return Err(Error::NotADirectory);
            }
            let mut found: Option<DirEntry> = None;
            self.walk_dir(current.cluster, |entry| {
                if entry.name().eq_ignore_ascii_case(component) {
                    found = Some(*entry);
                    true
                } else {
                    false
                }
            })?;
            current = found.ok_or(Error::NotFound)?;
            // Only a *directory* entry's cluster `0` means "this is
            // root" (see this function's doc comment above) - an empty
            // *file* (phase 5's `touch`) legitimately has cluster `0`
            // too, meaning "no clusters allocated," and must not be
            // rewritten to point at root's cluster instead. Gating on
            // `is_dir` doesn't change the `..`-resolution fix above:
            // every `..` entry is, definitionally, a directory.
            if current.is_dir && current.cluster == 0 {
                current.cluster = self.root_cluster;
            }
        }
        Ok(current)
    }

    /// Lists a directory's entries, calling `f(name, is_dir, size)` for
    /// each. `path` `""` or `"/"` lists the root.
    pub fn list_dir(
        &mut self,
        path: &str,
        mut f: impl FnMut(&str, bool, u32),
    ) -> Result<(), Error> {
        let dir = self.find(path)?;
        if !dir.is_dir {
            return Err(Error::NotADirectory);
        }
        self.walk_dir(dir.cluster, |entry| {
            f(entry.name(), entry.is_dir, entry.size);
            false
        })
    }

    /// Metadata for one path: size, directory flag, and the FAT "write" time
    /// decoded to a calendar (or `None` if the entry carries no date, e.g. the
    /// root). Backs `ls -l`.
    pub fn stat(&mut self, path: &str) -> Result<crate::vfs::Stat, Error> {
        let e = self.find(path)?;
        Ok(crate::vfs::Stat {
            size: e.size as u64,
            is_dir: e.is_dir,
            time: decode_fat_time(e.mtime_date, e.mtime_time),
            mode: None, // FAT has no owner/permission model
        })
    }

    /// Reads a file into `buf`, up to `buf.len()` bytes, returning the
    /// file's *real* size - compare against `buf.len()` to detect
    /// truncation, same idea as `snprintf`'s return value.
    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        let file = self.find(path)?;
        if file.is_dir {
            return Err(Error::NotAFile);
        }
        if file.size == 0 {
            return Ok(0);
        }

        let mut cluster = file.cluster;
        let mut written = 0usize;
        let total = file.size as usize;
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
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => break,
            }
        }
        Ok(file.size)
    }

    /// Reads up to `buf.len()` bytes of the file at `path`, starting at
    /// byte `offset`, returning how many were actually copied (`0` once
    /// `offset` is at/past the end of the file - the loop-until-short
    /// termination signal). The chunked-read primitive behind
    /// `FSOP_READ_AT` and the shell's two-step `exec` flow - the one
    /// genuinely new capability added in the move to userland;
    /// [`read_file`](Self::read_file) always starts at byte 0 and can
    /// never window into a file larger than one buffer.
    ///
    /// Whole clusters before `offset` are skipped by walking the FAT
    /// chain without reading their data sectors; within the first
    /// cluster actually read, sectors entirely before `offset` are
    /// skipped the same way.
    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        let file = self.find(path)?;
        if file.is_dir {
            return Err(Error::NotAFile);
        }
        let total = file.size as u64;
        if offset >= total || buf.is_empty() || file.cluster == 0 {
            return Ok(0);
        }
        let want = ((total - offset) as usize).min(buf.len());
        let offset = offset as usize;

        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        // Seek start: the file's first cluster, unless a cached cursor from
        // an earlier read of *this same file* already sits at or before
        // `offset` - then resume the walk from there (the sequential-read
        // fast path; see `read_cursor`). The cursor's invariant guarantees
        // its `cluster` covers `[cluster_pos, cluster_pos + cluster_bytes)`
        // of the current chain, so resuming is exactly equivalent to
        // walking from the start, only shorter.
        let (mut cluster, mut cluster_pos) = match self.read_cursor {
            Some(c) if c.file_cluster == file.cluster && c.cluster_pos <= offset => {
                (c.cluster, c.cluster_pos)
            }
            // Byte position (within the file) of `cluster`'s first byte.
            _ => (file.cluster, 0usize),
        };
        while cluster_pos + cluster_bytes <= offset {
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                // Chain shorter than the size field claims - treat as
                // end-of-file rather than erroring, same lenience as
                // read_file's own chain walk.
                None => return Ok(0),
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
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => break,
            }
            cluster_pos += cluster_bytes;
        }
        // Remember where this read ended so the next sequential read resumes
        // here instead of re-walking from the file's start. `cluster` /
        // `cluster_pos` still satisfy the cursor invariant (`cluster_pos` was
        // only ever advanced in lock-step with `next_cluster`, and every exit
        // path above leaves them paired). Only set on a real read; the early
        // `Ok(0)` returns leave any existing cursor untouched.
        self.read_cursor = Some(ReadCursor {
            file_cluster: file.cluster,
            cluster,
            cluster_pos,
        });
        Ok(written as u32)
    }

    /// Creates an empty subdirectory at `path` (must not already exist;
    /// its parent must already exist and be a directory).
    ///
    /// First write-capable command this filesystem has ever had - see
    /// `CLAUDE.md`'s mkdir/rmdir section for the full design rationale.
    /// (Its original deliberately-narrow no-directory-extension
    /// limitation is gone: a parent whose existing clusters have no free
    /// entry slot now grows by a cluster - see
    /// [`insert_dir_entry`](Self::insert_dir_entry).)
    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        // Validate the name up front, before any cluster is allocated - an
        // invalid name must never leave orphaned clusters behind. Both the
        // 8.3 and long-name paths are covered (see `validate_create_name`).
        validate_create_name(name)?;

        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }

        let mut exists = false;
        self.walk_dir(parent.cluster, |entry| {
            if entry.name().eq_ignore_ascii_case(name) {
                exists = true;
                true
            } else {
                false
            }
        })?;
        if exists {
            return Err(Error::AlreadyExists);
        }

        let new_cluster = self.find_free_cluster()?;
        // Mark it used immediately - `zero_cluster`/the `.`/`..` writes
        // below don't touch the FAT, and a failure partway through should
        // still leave this cluster claimed rather than reusable by a
        // concurrent-looking future call (this kernel has no concurrency
        // yet, but "claim before you use" is the correct order regardless).
        self.write_fat_entry(new_cluster, END_OF_CHAIN_MIN)?;
        self.zero_cluster(new_cluster)?;

        let dotdot_cluster = if parent.cluster == self.root_cluster {
            0
        } else {
            parent.cluster
        };
        let mut first_sector = [0u8; SECTOR_SIZE];
        write_raw_entry(
            &mut first_sector[0..DIR_ENTRY_SIZE],
            &dot_name(),
            ATTR_DIRECTORY,
            new_cluster,
            0,
        );
        write_raw_entry(
            &mut first_sector[DIR_ENTRY_SIZE..2 * DIR_ENTRY_SIZE],
            &dotdot_name(),
            ATTR_DIRECTORY,
            dotdot_cluster,
            0,
        );
        let lba = self.cluster_to_lba(new_cluster);
        self.disk.write_sector(lba as u64, &first_sector)?;

        self.insert_named_entry(parent.cluster, name, ATTR_DIRECTORY, new_cluster, 0)
    }

    /// Removes the empty subdirectory at `path`. Fails with
    /// [`Error::DirectoryNotEmpty`](Error::DirectoryNotEmpty) unless every
    /// entry in it is `.`/`..`, and with
    /// [`Error::CannotRemoveRoot`](Error::CannotRemoveRoot) for `/` itself.
    /// Both are checked before anything on disk is touched, so a rejected
    /// `rmdir` never partially applies.
    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        // `split_parent` returning `None` here means the path has no
        // final component to remove - i.e. it *is* the root (an empty
        // path can't reach this far; the syscall boundary rejects
        // zero-length paths). Report that as the root-removal refusal
        // it actually is, not as an invalid name - a mis-mapping that
        // was invisible while every error collapsed to one sentinel and
        // surfaced the moment `rmdir /`'s specific message read
        // "invalid name".
        let (parent_path, name) = split_parent(path).ok_or(Error::CannotRemoveRoot)?;
        let target = self.find(path)?;
        if !target.is_dir {
            return Err(Error::NotADirectory);
        }
        if target.cluster == self.root_cluster || target.cluster == 0 {
            return Err(Error::CannotRemoveRoot);
        }

        let mut empty = true;
        self.walk_dir(target.cluster, |entry| {
            if !is_dot_or_dotdot(entry.name()) {
                empty = false;
                true
            } else {
                false
            }
        })?;
        if !empty {
            return Err(Error::DirectoryNotEmpty);
        }

        // The whole chain, not just the first cluster - a directory that
        // ever grew past one cluster (insert_dir_entry's extension) still
        // owns its extension clusters even once emptied; see free_chain's
        // doc comment.
        self.free_chain(target.cluster)?;

        let parent = self.find(parent_path)?;
        let mut location: Option<(u64, usize)> = None;
        self.walk_dir_with_location(parent.cluster, |entry, lba, offset| {
            if entry.name().eq_ignore_ascii_case(name) {
                location = Some((lba, offset));
                true
            } else {
                false
            }
        })?;
        let (lba, offset) = location.ok_or(Error::NotFound)?;

        // Free the short entry and its LFN run (a directory can have a long
        // name too), same as `rm`.
        self.free_entry_with_lfn(parent.cluster, lba, offset)
    }

    /// Creates an empty (zero-byte) file at `path`, or - unlike
    /// [`mkdir`](Self::mkdir) - succeeds as a no-op if a file already
    /// exists there. Real `touch` normally updates a file's modification
    /// time on an existing file; this kernel has no RTC (see
    /// `write_raw_entry`'s doc comment) and nothing to update, so
    /// "succeed without changing anything" is the closest honest
    /// approximation. Touching an existing *directory* is rejected
    /// (`Error::NotAFile`) rather than silently doing nothing, since
    /// unlike a real timestamp update there's no way to interpret that
    /// as harmless.
    ///
    /// A zero-byte file needs no cluster allocated at all - real FAT32
    /// represents "empty file" as a directory entry with starting
    /// cluster `0` and size `0`, which is why this is simpler than
    /// `mkdir`: no `find_free_cluster`/`zero_cluster`/`.`/`..` writes,
    /// just one new directory entry. [`Fs::find`] has a matching fix
    /// (see its doc comment) so a cluster-`0` *file* isn't confused with
    /// the cluster-`0`-means-root convention that applies only to
    /// directories.
    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        validate_create_name(name)?;

        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }

        let mut existing: Option<DirEntry> = None;
        self.walk_dir(parent.cluster, |entry| {
            if entry.name().eq_ignore_ascii_case(name) {
                existing = Some(*entry);
                true
            } else {
                false
            }
        })?;
        if let Some(entry) = existing {
            return if entry.is_dir {
                Err(Error::NotAFile)
            } else {
                Ok(())
            };
        }

        // Attribute byte `0` - no directory bit, no volume-ID bit, just
        // an ordinary file (see `parse`'s `ATTR_DIRECTORY` check: this
        // makes `is_dir` false, which is all that matters here).
        self.insert_named_entry(parent.cluster, name, 0, 0, 0)
    }

    /// Removes the file at `path`. Rejects directories with
    /// [`Error::NotAFile`](Error::NotAFile), matching `read_file`'s
    /// existing convention that this error means "a directory where a
    /// file was expected" - use [`rmdir`](Self::rmdir) for those
    /// instead. Frees the file's entire
    /// cluster chain in the FAT (a no-op for an empty, `touch`-created
    /// file, which never had one), then marks its own directory entry
    /// [`DIR_ENTRY_FREE`].
    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let target = self.find(path)?;
        if target.is_dir {
            return Err(Error::NotAFile);
        }

        if target.cluster != 0 {
            self.free_chain(target.cluster)?;
        }

        let parent = self.find(parent_path)?;
        let mut location: Option<(u64, usize)> = None;
        self.walk_dir_with_location(parent.cluster, |entry, lba, offset| {
            if entry.name().eq_ignore_ascii_case(name) {
                location = Some((lba, offset));
                true
            } else {
                false
            }
        })?;
        let (lba, offset) = location.ok_or(Error::NotFound)?;

        // Free the short entry *and* any long-name (LFN) entries in front of
        // it, so a removed long-named file leaves no orphaned LFN entries.
        self.free_entry_with_lfn(parent.cluster, lba, offset)
    }

    /// Creates a file at `path` with exactly `data`'s contents, or
    /// overwrites an existing file's contents (fully replacing them, not
    /// appending). The one primitive `touch`/`cat` were missing: without
    /// this, every file this kernel could create was permanently
    /// zero bytes.
    ///
    /// **Ordering, and why it's ordered this way:** the new cluster
    /// chain is allocated and written *before* anything about an
    /// existing file is touched - both the invalid-name check (for a
    /// brand-new file) and the new chain's writes happen first, so a
    /// failure partway through never frees or unlinks a file that was
    /// already there. Only once the new content is safely on disk does
    /// this free the old chain (a no-op if the old file was empty) and
    /// patch the directory entry to point at it.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }

        let mut existing: Option<(u64, usize, u32)> = None;
        let mut found_dir = false;
        self.walk_dir_with_location(parent.cluster, |entry, lba, offset| {
            if entry.name().eq_ignore_ascii_case(name) {
                if entry.is_dir {
                    found_dir = true;
                } else {
                    existing = Some((lba, offset, entry.cluster));
                }
                true
            } else {
                false
            }
        })?;
        if found_dir {
            return Err(Error::NotAFile);
        }

        // Validated up front, before any cluster is allocated - an
        // invalid name for a brand-new file should never leave orphaned,
        // unlinked clusters behind. Not needed when overwriting an
        // existing file: whatever name is already on disk is already
        // valid by construction, and might not even fit this kernel's
        // own (conservative) creation charset if another tool wrote it.
        if existing.is_none() {
            validate_create_name(name)?;
        }

        let new_cluster = self.write_chain(data)?;

        if let Some((lba, offset, old_cluster)) = existing {
            if old_cluster != 0 {
                self.free_chain(old_cluster)?;
            }
            self.patch_entry_cluster_size(lba, offset, new_cluster, data.len() as u32)
        } else {
            self.insert_named_entry(parent.cluster, name, 0, new_cluster, data.len() as u32)
        }
    }

    /// Allocates and writes a fresh cluster chain holding `data`, linking
    /// each cluster to the next via [`write_fat_entry`](Self::write_fat_entry)
    /// as it goes (so a fresh `find_free_cluster` call never picks a
    /// cluster this same call already claimed). Returns the chain's first
    /// cluster, or `0` for empty `data` - the same "no cluster allocated"
    /// convention [`touch`](Self::touch) already relies on.
    fn write_chain(&mut self, data: &[u8]) -> Result<u32, Error> {
        if data.is_empty() {
            return Ok(0);
        }
        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        let num_clusters = data.len().div_ceil(cluster_bytes);
        let mut first_cluster = 0u32;
        let mut prev_cluster = 0u32;
        let mut written = 0usize;
        for i in 0..num_clusters {
            let cluster = self.find_free_cluster()?;
            self.write_fat_entry(cluster, END_OF_CHAIN_MIN)?;
            if i == 0 {
                first_cluster = cluster;
            } else {
                self.write_fat_entry(prev_cluster, cluster)?;
            }

            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut sector_buf = [0u8; SECTOR_SIZE];
                let start = written;
                let end = (written + SECTOR_SIZE).min(data.len());
                if start < end {
                    sector_buf[..end - start].copy_from_slice(&data[start..end]);
                }
                self.disk.write_sector((lba + s) as u64, &sector_buf)?;
                written += SECTOR_SIZE;
            }
            prev_cluster = cluster;
        }
        Ok(first_cluster)
    }

    /// Rewrites just an existing directory entry's cluster and size
    /// fields in place, leaving its name/attribute/timestamps untouched -
    /// what [`write_file`](Self::write_file) uses to point an
    /// already-existing entry at a freshly written replacement chain,
    /// without re-deriving or re-validating its short name.
    fn patch_entry_cluster_size(
        &mut self,
        lba: u64,
        offset: usize,
        cluster: u32,
        size: u32,
    ) -> Result<(), Error> {
        let mut sector = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sector)?;
        sector[offset + 20..offset + 22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        sector[offset + 26..offset + 28].copy_from_slice(&(cluster as u16).to_le_bytes());
        sector[offset + 28..offset + 32].copy_from_slice(&size.to_le_bytes());
        self.disk.write_sector(lba, &sector)?;
        Ok(())
    }

    /// Read-modify-write of a partial sector of *file data*: read the
    /// whole 512-byte sector, splice `bytes` in at `in_off`, write it
    /// back. The one primitive [`write_at`](Self::write_at) needs that no
    /// prior write path had - every existing data write built fresh whole
    /// clusters (`write_chain`) and never had bytes to preserve. Modeled
    /// on the metadata RMW [`patch_entry_cluster_size`](Self::patch_entry_cluster_size)
    /// and [`write_fat_entry`](Self::write_fat_entry) already do, just for
    /// content rather than a directory/FAT field.
    fn write_partial_sector(&mut self, lba: u64, in_off: usize, bytes: &[u8]) -> Result<(), Error> {
        let mut sector = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sector)?;
        sector[in_off..in_off + bytes.len()].copy_from_slice(bytes);
        self.disk.write_sector(lba, &sector)?;
        Ok(())
    }

    /// Appends one freshly allocated cluster to the chain whose current
    /// last cluster is `tail`, returning the new cluster. The same
    /// allocate-mark-link sequence [`write_chain`](Self::write_chain) and
    /// `insert_dir_entry`'s directory extension already use, factored out
    /// for [`write_at`](Self::write_at)'s file-data extension.
    fn extend_chain(&mut self, tail: u32) -> Result<u32, Error> {
        let new = self.find_free_cluster()?;
        self.write_fat_entry(new, END_OF_CHAIN_MIN)?;
        self.write_fat_entry(tail, new)?;
        Ok(new)
    }

    /// Writes `data` starting at byte `offset` in the file at `path`,
    /// extending the file (allocating clusters, growing the size field)
    /// as needed - **without** rewriting the bytes before `offset`,
    /// unlike [`write_file`](Self::write_file). The FAT32 primitive behind
    /// streaming `cp`, unbounded `>>`, and random-access `writeat`.
    ///
    /// Grows the file to `max(old_size, offset + data.len())`. `offset` may
    /// be **past the old end of file**: the gap `[old_size, offset)` is
    /// zero-filled on disk (FAT32 has no sparse representation, so the gap is
    /// real zero bytes), bounded by `MAX_GAP_FILL` so a fat-fingered huge
    /// offset can't try to zero-fill the volume (`Error::InvalidOffset` past
    /// that). Empty `data` is a no-op. A previously-empty (cluster-`0`,
    /// `touch`ed) file gets its first cluster allocated here and recorded in
    /// its entry.
    ///
    /// One unified per-sector pass covers the whole affected range
    /// `[min(old_size, offset), offset + data.len())`, building each sector
    /// from zeros (positions before `offset`, the gap) and `data` (positions
    /// from `offset`). A sector that overlaps existing content is
    /// read-modified-written, preserving the bytes outside the write window -
    /// including the boundary sector that straddles `old_size`; a sector
    /// entirely past the old EOF is zero-padded rather than read first; a
    /// full 512-byte write skips the read either way.
    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        // Locate the entry and, via its parent, the on-disk location of
        // its directory record (needed to patch cluster/size) - the same
        // lookup `write_file` does.
        let (parent_path, name) = split_parent(path).ok_or(Error::InvalidName)?;
        let parent = self.find(parent_path)?;
        if !parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let mut loc: Option<(u64, usize, u32, u32)> = None; // (dir_lba, dir_off, cluster, size)
        let mut found_dir = false;
        self.walk_dir_with_location(parent.cluster, |entry, lba, off| {
            if entry.name().eq_ignore_ascii_case(name) {
                if entry.is_dir {
                    found_dir = true;
                } else {
                    loc = Some((lba, off, entry.cluster, entry.size));
                }
                true
            } else {
                false
            }
        })?;
        if found_dir {
            return Err(Error::NotAFile);
        }
        let (dir_lba, dir_off, mut head_cluster, old_size_u32) = loc.ok_or(Error::NotFound)?;
        let old_size = old_size_u32 as u64;
        if offset > old_size && offset - old_size > MAX_GAP_FILL {
            return Err(Error::InvalidOffset);
        }

        let cluster_bytes = self.sectors_per_cluster as u64 * SECTOR_SIZE as u64;

        // A previously-empty file has no head cluster yet.
        if head_cluster == 0 {
            let c = self.find_free_cluster()?;
            self.write_fat_entry(c, END_OF_CHAIN_MIN)?;
            head_cluster = c;
        }

        // One unified pass over [write_start, write_end): when `offset` is
        // past the old EOF, start at `old_size` and zero-fill the gap up to
        // `offset` before the data; otherwise start at `offset`.
        let write_start = old_size.min(offset);
        let write_end = offset + data.len() as u64;

        // Walk to the cluster holding `write_start`, extending the chain if
        // the walk runs off the end.
        let mut cluster = head_cluster;
        let mut cluster_pos = 0u64; // file byte offset of `cluster`'s first byte
        while cluster_pos + cluster_bytes <= write_start {
            cluster = match self.next_cluster(cluster)? {
                Some(next) => next,
                None => self.extend_chain(cluster)?,
            };
            cluster_pos += cluster_bytes;
        }

        let mut pos = write_start;
        while pos < write_end {
            // Advance to the cluster containing `pos`, extending as needed.
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

            // This sector's bytes: zeros for positions before `offset` (the
            // gap fill), `data` from `offset` on.
            let mut chunk = [0u8; SECTOR_SIZE];
            for (i, slot) in chunk[..n].iter_mut().enumerate() {
                let fp = pos + i as u64;
                if fp >= offset {
                    *slot = data[(fp - offset) as usize];
                }
            }

            if in_off == 0 && n == SECTOR_SIZE {
                // Whole sector determined by `chunk` - nothing to preserve.
                self.disk.write_sector(sector_lba, &chunk)?;
            } else if sector_start < old_size {
                // Partial write into a sector with existing content - RMW,
                // preserving the bytes outside [in_off, in_off+n). This is
                // also the boundary sector that straddles `old_size` (its
                // post-old_size gap bytes come through as zeros in `chunk`).
                self.write_partial_sector(sector_lba, in_off, &chunk[..n])?;
            } else {
                // Partial write into a fresh sector past the old EOF -
                // zero-pad, don't read (there's nothing real to preserve).
                let mut sector = [0u8; SECTOR_SIZE];
                sector[in_off..in_off + n].copy_from_slice(&chunk[..n]);
                self.disk.write_sector(sector_lba, &sector)?;
            }
            pos += n as u64;
        }

        // Grow-only size, and record the head cluster (which changed if
        // the file was previously empty).
        let new_size = old_size.max(write_end) as u32;
        self.patch_entry_cluster_size(dir_lba, dir_off, head_cluster, new_size)
    }

    /// Renames or moves the file or directory at `src` to `dst`.
    ///
    /// An existing `dst` is REPLACED when both it and `src` are ordinary files
    /// - POSIX `rename`. Unlike ext2 this cannot be atomic: a FAT directory
    /// entry holds the file's first cluster and size itself, so there is no
    /// inode number to re-point and the old entry must be replaced. The name
    /// therefore resolves to two entries for an instant rather than to none,
    /// which is the recoverable direction. A DIRECTORY on either side of an
    /// existing name is still refused. `dst` being an existing directory does
    /// NOT move `src` inside it - `/bin/mv` does that, above this layer.
    ///
    /// Implemented uniformly for a same-directory rename and a
    /// cross-directory move alike, rather than special-casing the
    /// same-parent case as a cheaper in-place short-name rewrite: locate
    /// `src`'s own entry and read its cluster/size/kind, insert a new
    /// entry for `dst` with those same values, then free `src`'s old
    /// entry only once the new one is safely linked in - same "write the
    /// new thing before touching the old one" ordering as
    /// [`write_file`](Self::write_file)'s overwrite path, so a failure
    /// partway through (e.g. a disk-full error inserting into `dst`'s
    /// parent) never leaves `src` half-deleted. The replacing path added in
    /// 2026-09 keeps that same order for the same reason - the new entry is
    /// written before either old one is freed.
    ///
    /// **The one step this can't skip: when a *directory* moves to a
    /// *different* parent, its own `..` entry has to be patched to point
    /// at the new parent** (or cluster `0`, root's own convention, if
    /// the new parent is root) - otherwise a moved directory's `cd ..`
    /// would keep resolving to its old parent forever, silently. This is
    /// the same cluster-`0`-means-root convention documented in
    /// [`find`](Self::find)'s doc comment, reintroduced by a different
    /// code path here rather than a new bug class.
    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), Error> {
        let (src_parent_path, src_name) = split_parent(src).ok_or(Error::InvalidName)?;
        let (dst_parent_path, dst_name) = split_parent(dst).ok_or(Error::InvalidName)?;
        validate_create_name(dst_name)?;

        let src_parent = self.find(src_parent_path)?;
        if !src_parent.is_dir {
            return Err(Error::NotADirectory);
        }
        let dst_parent = self.find(dst_parent_path)?;
        if !dst_parent.is_dir {
            return Err(Error::NotADirectory);
        }

        let mut src_location: Option<(u64, usize)> = None;
        let mut src_entry: Option<DirEntry> = None;
        self.walk_dir_with_location(src_parent.cluster, |entry, lba, offset| {
            if entry.name().eq_ignore_ascii_case(src_name) {
                src_location = Some((lba, offset));
                src_entry = Some(*entry);
                true
            } else {
                false
            }
        })?;
        let (src_lba, src_offset) = src_location.ok_or(Error::NotFound)?;
        let src_entry = src_entry.ok_or(Error::NotFound)?;

        // An existing destination is REPLACED when both it and `src` are
        // ordinary files. Unlike ext2 this cannot be atomic: a FAT directory
        // entry holds the file's first cluster and size itself, so there is no
        // inode number to re-point - the old entry must go and a new one take
        // its place, and between those two writes the name resolves to
        // nothing. Said plainly rather than papered over; it is a property of
        // the on-disk format, not of this code.
        let mut dst_found: Option<(u64, usize)> = None;
        let mut dst_entry: Option<DirEntry> = None;
        self.walk_dir_with_location(dst_parent.cluster, |entry, lba, offset| {
            if entry.name().eq_ignore_ascii_case(dst_name) {
                dst_found = Some((lba, offset));
                dst_entry = Some(*entry);
                true
            } else {
                false
            }
        })?;
        if let (Some((dst_lba, dst_offset)), Some(dst_entry)) = (dst_found, dst_entry) {
            // Both walks match case-insensitively, so `src` and `dst` can be
            // the SAME entry two ways. `mv f f` is a genuine no-op. But
            // `mv FOO.TXT foo.txt` is a real request - change the stored case -
            // and returning Ok without doing it would report success for
            // nothing, which is worse than the `AlreadyExists` it used to give.
            // The insert-first order below performs it: write the entry under
            // the new spelling, free the old one, and skip both the source-side
            // free (same slot) and the chain free (same file).
            let same_entry = (dst_lba, dst_offset) == (src_lba, src_offset);
            if same_entry && src_name.as_bytes() == dst_name.as_bytes() {
                return Ok(());
            }
            if src_entry.is_dir || dst_entry.is_dir {
                return Err(Error::AlreadyExists);
            }
            // WRITE THE NEW ENTRY FIRST, exactly as the non-replacing path
            // below does. Freeing the destination first looked fine against a
            // crash, and is wrong against an ORDINARY ERROR: if the insert then
            // fails - a full directory that cannot be extended, no contiguous
            // run for the LFN entries, a write error - `mv` returns Err having
            // already destroyed `dst`, with `src` still in place. The caller
            // sees "failed" and the destination is gone for nothing. This order
            // can only leave a transient duplicate name, which is recoverable.
            let attr = if src_entry.is_dir { ATTR_DIRECTORY } else { 0 };
            self.insert_named_entry(
                dst_parent.cluster,
                dst_name,
                attr,
                src_entry.cluster,
                src_entry.size,
            )?;
            self.free_entry_with_lfn(dst_parent.cluster, dst_lba, dst_offset)?;
            if !same_entry {
                self.free_entry_with_lfn(src_parent.cluster, src_lba, src_offset)?;
                // Only when the destination was a DIFFERENT file. On a
                // case-only rename the chain is the one we just re-linked.
                if dst_entry.cluster != 0 {
                    self.free_chain(dst_entry.cluster)?;
                }
            }
            return Ok(());
        }

        let attr = if src_entry.is_dir { ATTR_DIRECTORY } else { 0 };
        self.insert_named_entry(
            dst_parent.cluster,
            dst_name,
            attr,
            src_entry.cluster,
            src_entry.size,
        )?;

        // Free src's short entry and its LFN run (src may have had a long
        // name of its own). Matches by the exact location found above, so
        // the dst entry just inserted - even into the same directory on a
        // rename - is never mistaken for it.
        self.free_entry_with_lfn(src_parent.cluster, src_lba, src_offset)?;

        if src_entry.is_dir && src_parent.cluster != dst_parent.cluster {
            let dotdot_cluster = if dst_parent.cluster == self.root_cluster {
                0
            } else {
                dst_parent.cluster
            };
            let moved_lba = self.cluster_to_lba(src_entry.cluster);
            // The `..` entry is always the second 32-byte entry in a
            // directory's first sector - `mkdir` writes it there
            // unconditionally, so every directory this kernel (or any
            // real FAT32 formatter) created has it at that fixed offset.
            self.patch_entry_cluster_size(moved_lba as u64, DIR_ENTRY_SIZE, dotdot_cluster, 0)?;
        }

        Ok(())
    }

    /// Finds a free (end-of-directory or previously-deleted) 32-byte slot
    /// in the directory starting at `start_cluster` and writes a new entry
    /// there - **extending the directory by one cluster if every existing
    /// slot is taken** (originally a deliberate first-cut gap that
    /// returned a `DirectoryFull` error instead; the variant is gone now
    /// that this is the only place it could ever have come from).
    ///
    /// Extension ordering matters, same discipline as `mkdir`'s
    /// claim-before-use and `write_file`'s write-the-new-thing-first: the
    /// fresh cluster is claimed end-of-chain in the FAT and zeroed (a
    /// directory cluster must read back as "no entries" -
    /// `DIR_ENTRY_END` is `0x00`, so an unzeroed cluster would present
    /// garbage as entries) *before* the old last cluster is linked to
    /// it - a failure partway leaves at worst a claimed-but-orphaned
    /// cluster, never a directory chain pointing at garbage.
    fn insert_dir_entry(
        &mut self,
        start_cluster: u32,
        short_name: &[u8; 11],
        attr: u8,
        cluster: u32,
        size: u32,
    ) -> Result<(), Error> {
        let mut current = start_cluster;
        loop {
            let lba = self.cluster_to_lba(current);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for (i, raw) in buf.chunks_exact(DIR_ENTRY_SIZE).enumerate() {
                    if raw[0] == DIR_ENTRY_FREE || raw[0] == DIR_ENTRY_END {
                        write_raw_entry(
                            &mut buf[i * DIR_ENTRY_SIZE..(i + 1) * DIR_ENTRY_SIZE],
                            short_name,
                            attr,
                            cluster,
                            size,
                        );
                        self.disk.write_sector((lba + s) as u64, &buf)?;
                        return Ok(());
                    }
                }
            }
            match self.next_cluster(current)? {
                Some(next) => current = next,
                None => {
                    // Every slot in the chain is taken - grow the
                    // directory by one cluster (see doc comment for the
                    // ordering reasoning), then let the loop's next
                    // iteration find the first slot of the fresh,
                    // all-zero cluster.
                    let new = self.find_free_cluster()?;
                    self.write_fat_entry(new, END_OF_CHAIN_MIN)?;
                    self.zero_cluster(new)?;
                    self.write_fat_entry(current, new)?;
                    current = new;
                }
            }
        }
    }

    /// Inserts a directory entry for `name`, choosing the on-disk
    /// representation by whether the name fits an 8.3 short name:
    ///
    /// - **Fits 8.3** ([`make_short_name`] succeeds): a single short entry,
    ///   exactly as before - no LFN entries, the name uppercased. This is
    ///   the common case and its on-disk shape is unchanged.
    /// - **Doesn't fit** (too long, extra dots, spaces, mixed case that must
    ///   be preserved, or any non-8.3 character): a generated `NAME~N` short
    ///   alias ([`generate_short_alias`], unique within this directory) plus
    ///   a run of LFN entries carrying the real name, laid down as one
    ///   physically contiguous block via [`write_entry_run`]. The alias's
    ///   [`lfn_checksum`] is stamped into every LFN entry, so the read side
    ///   ([`walk_dir`](Self::walk_dir)) reconstructs and matches the long
    ///   name.
    ///
    /// The single write-path entry point for a *named* create - `mkdir`,
    /// `touch`, `write_file`, and `mv` all funnel through here. Callers must
    /// have already validated the name with [`validate_create_name`] (before
    /// allocating any clusters), so this only fails on a genuine disk error
    /// or an exhausted alias space.
    fn insert_named_entry(
        &mut self,
        dir_cluster: u32,
        name: &str,
        attr: u8,
        cluster: u32,
        size: u32,
    ) -> Result<(), Error> {
        if let Some(short) = make_short_name(name) {
            // Fits 8.3 - plain short entry, no LFN (unchanged behavior).
            return self.insert_dir_entry(dir_cluster, &short, attr, cluster, size);
        }

        let short = self.generate_short_alias(name, dir_cluster)?;
        // Physical layout: the LFN entries (highest sequence first), then the
        // short entry last - built into one buffer, written as a contiguous
        // run. `name.len() <= LONG_NAME_MAX` is guaranteed by
        // `validate_create_name`, so `count <= MAX_LFN_ENTRIES + 1`.
        let mut entries = [0u8; (MAX_LFN_ENTRIES + 1) * DIR_ENTRY_SIZE];
        let count = build_name_entries(name.as_bytes(), &short, attr, cluster, size, &mut entries);
        self.write_entry_run(dir_cluster, &entries[..count * DIR_ENTRY_SIZE], count)
    }

    /// Generates a unique 8.3 short-name alias for the long `name` within the
    /// directory at `dir_cluster` - the `PROGRA~1`-style basis name every
    /// long name needs behind it. Derives a sanitized base (up to 6-8 valid
    /// uppercase chars) and extension from `name`, then appends `~1`, `~2`, …
    /// until the resulting 11-byte short name collides with nothing already
    /// in the directory ([`short_alias_exists`](Self::short_alias_exists)).
    /// The `~N` numeric tail grows the base is trimmed to keep the whole
    /// thing within 8 characters (see [`compose_alias`]).
    fn generate_short_alias(&mut self, name: &str, dir_cluster: u32) -> Result<[u8; 11], Error> {
        let bytes = name.as_bytes();
        // Split off an extension at the last '.', but only if it isn't the
        // leading character (a name like ".config" is all base, no ext).
        let mut dot = None;
        let mut i = bytes.len();
        while i > 0 {
            i -= 1;
            if bytes[i] == b'.' {
                dot = Some(i);
                break;
            }
        }
        let (base_src, ext_src): (&[u8], &[u8]) = match dot {
            Some(d) if d > 0 => (&bytes[..d], &bytes[d + 1..]),
            _ => (bytes, &[]),
        };

        let mut base = [0u8; 8];
        let mut base_len = 0;
        for &b in base_src {
            if base_len >= 8 {
                break;
            }
            if let Some(c) = sanitize_short_char(b) {
                base[base_len] = c;
                base_len += 1;
            }
        }
        if base_len == 0 {
            base[0] = b'_'; // a name with no 8.3-legal base char still needs one
            base_len = 1;
        }
        let mut ext = [0u8; 3];
        let mut ext_len = 0;
        for &b in ext_src {
            if ext_len >= 3 {
                break;
            }
            if let Some(c) = sanitize_short_char(b) {
                ext[ext_len] = c;
                ext_len += 1;
            }
        }

        // `~1`..`~999999` - vastly more than any real directory needs; a
        // bound rather than an unbounded loop on a pathological collision run.
        for n in 1..=999_999u32 {
            let short = compose_alias(&base[..base_len], &ext[..ext_len], n);
            if !self.short_alias_exists(dir_cluster, &short)? {
                return Ok(short);
            }
        }
        Err(Error::AlreadyExists)
    }

    /// Whether an entry with the exact 11-byte short name `short` already
    /// exists in the directory at `dir_cluster`. Compares the raw 8.3 field
    /// (not the rendered/long name), because two different long names can map
    /// to the same alias base - the collision that matters for
    /// [`generate_short_alias`](Self::generate_short_alias) is at the short
    /// name. LFN entries are skipped (they carry name *fragments*, not an 8.3
    /// name in bytes 0..11).
    fn short_alias_exists(&mut self, dir_cluster: u32, short: &[u8; 11]) -> Result<bool, Error> {
        let mut cluster = dir_cluster;
        loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for raw in buf.chunks_exact(DIR_ENTRY_SIZE) {
                    match raw[0] {
                        DIR_ENTRY_END => return Ok(false),
                        DIR_ENTRY_FREE => continue,
                        _ => {}
                    }
                    if raw[11] != ATTR_LFN && &raw[0..11] == short {
                        return Ok(true);
                    }
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => return Ok(false),
            }
        }
    }

    /// Writes `count` prepared 32-byte directory entries (`entries`,
    /// physical order) into a run of `count` *physically contiguous* free
    /// slots in the directory at `dir_cluster`, extending the directory by
    /// zeroed clusters if no such run exists yet. Contiguity is required: the
    /// LFN entries and their short entry must be adjacent in directory order
    /// for the read side to associate them.
    ///
    /// "Free" is a `0xE5` (deleted) or `0x00` (end-of-directory) slot. A run
    /// may span sector and cluster boundaries (directory order follows the
    /// cluster chain), and may begin in reclaimed `0xE5` slots and continue
    /// through the `0x00` tail. Because a newly extended cluster is zeroed, a
    /// short directory always grows enough contiguous free slots eventually.
    fn write_entry_run(
        &mut self,
        dir_cluster: u32,
        entries: &[u8],
        count: usize,
    ) -> Result<(), Error> {
        let mut locs = [(0u64, 0usize); MAX_LFN_ENTRIES + 1];
        let mut run = 0usize;
        let mut cluster = dir_cluster;
        loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba + s) as u64, &mut buf)?;
                for (i, raw) in buf.chunks_exact(DIR_ENTRY_SIZE).enumerate() {
                    if raw[0] == DIR_ENTRY_FREE || raw[0] == DIR_ENTRY_END {
                        if run < count {
                            locs[run] = ((lba + s) as u64, i * DIR_ENTRY_SIZE);
                        }
                        run += 1;
                        if run == count {
                            return self.place_entries(entries, &locs, count);
                        }
                    } else {
                        run = 0; // a used slot breaks the contiguous run
                    }
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => {
                    // No run of `count` free slots in the existing chain - grow
                    // it by one zeroed cluster (claim-before-link, same order
                    // as `insert_dir_entry`) and let the scan continue into it;
                    // the run accumulates across the boundary.
                    let new = self.find_free_cluster()?;
                    self.write_fat_entry(new, END_OF_CHAIN_MIN)?;
                    self.zero_cluster(new)?;
                    self.write_fat_entry(cluster, new)?;
                    cluster = new;
                }
            }
        }
    }

    /// Writes the `count` prepared entries in `entries` to the `count`
    /// on-disk locations in `locs` (both in the same order), reading and
    /// rewriting each affected sector exactly once. `locs` is ascending
    /// (filled in scan order by [`write_entry_run`](Self::write_entry_run)),
    /// so entries sharing a sector are contiguous in the array and grouped.
    fn place_entries(
        &mut self,
        entries: &[u8],
        locs: &[(u64, usize); MAX_LFN_ENTRIES + 1],
        count: usize,
    ) -> Result<(), Error> {
        let mut i = 0;
        while i < count {
            let lba = locs[i].0;
            let mut buf = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut buf)?;
            while i < count && locs[i].0 == lba {
                let off = locs[i].1;
                buf[off..off + DIR_ENTRY_SIZE]
                    .copy_from_slice(&entries[i * DIR_ENTRY_SIZE..(i + 1) * DIR_ENTRY_SIZE]);
                i += 1;
            }
            self.disk.write_sector(lba, &buf)?;
        }
        Ok(())
    }

    /// Frees the short directory entry at (`target_lba`, `target_off`) *and*
    /// the contiguous run of LFN entries physically preceding it (its
    /// long-name entries), marking every slot [`DIR_ENTRY_FREE`]. Without
    /// this, deleting a long-named file would strand its LFN entries as
    /// orphans - never reclaimed (an LFN entry's first byte is a sequence
    /// number, not `0xE5`, so [`insert_dir_entry`](Self::insert_dir_entry)'s
    /// free-slot scan skips it) and flagged by `fsck`.
    ///
    /// Matches the target by its exact on-disk location, not by name, so a
    /// freshly inserted entry (even one just added to the same directory by
    /// `mv`) is never confused for it. The preceding LFN run is freed only if
    /// its stored checksum matches this short entry - the same orphan guard
    /// the read path uses, so an already-orphaned run in front of the target
    /// is left alone rather than wrongly attributed to it.
    fn free_entry_with_lfn(
        &mut self,
        dir_cluster: u32,
        target_lba: u64,
        target_off: usize,
    ) -> Result<(), Error> {
        // Collected while walking: the current pending LFN run's slot
        // locations, then (once the target is reached) everything to free.
        let mut run = [(0u64, 0usize); MAX_LFN_ENTRIES];
        let mut run_len = 0usize;
        let mut run_sum = 0u8;
        let mut to_free = [(0u64, 0usize); MAX_LFN_ENTRIES + 1];
        let mut free_n = 0usize;
        let mut found = false;

        let mut cluster = dir_cluster;
        'walk: loop {
            let lba0 = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                self.disk.read_sector((lba0 + s) as u64, &mut buf)?;
                for (i, raw) in buf.chunks_exact(DIR_ENTRY_SIZE).enumerate() {
                    let this = ((lba0 + s) as u64, i * DIR_ENTRY_SIZE);
                    match raw[0] {
                        DIR_ENTRY_END => break 'walk, // reached the end without the target
                        DIR_ENTRY_FREE => {
                            run_len = 0;
                            continue;
                        }
                        _ => {}
                    }
                    let attr = raw[11];
                    if attr == ATTR_LFN {
                        if run_len < MAX_LFN_ENTRIES {
                            run[run_len] = this;
                            run_len += 1;
                        }
                        run_sum = raw[13];
                        continue;
                    }
                    if attr & ATTR_VOLUME_ID != 0 {
                        run_len = 0;
                        continue;
                    }
                    // A short entry.
                    if this == (target_lba, target_off) {
                        if run_len > 0 && run_sum == lfn_checksum(&raw[0..11]) {
                            for &slot in run.iter().take(run_len) {
                                to_free[free_n] = slot;
                                free_n += 1;
                            }
                        }
                        to_free[free_n] = this;
                        free_n += 1;
                        found = true;
                        break 'walk;
                    }
                    run_len = 0; // a different short entry ends the pending run
                }
            }
            match self.next_cluster(cluster)? {
                Some(next) => cluster = next,
                None => break 'walk,
            }
        }
        if !found {
            return Err(Error::NotFound);
        }

        // `to_free` is ascending (LFN run scanned before the target, within a
        // sector by offset, across sectors by LBA), so slots sharing a sector
        // are adjacent - one read-modify-write per sector.
        let mut i = 0;
        while i < free_n {
            let lba = to_free[i].0;
            let mut buf = [0u8; SECTOR_SIZE];
            self.disk.read_sector(lba, &mut buf)?;
            while i < free_n && to_free[i].0 == lba {
                buf[to_free[i].1] = DIR_ENTRY_FREE;
                i += 1;
            }
            self.disk.write_sector(lba, &buf)?;
        }
        Ok(())
    }
}

/// Writes one 32-byte on-disk directory entry into `dst`. No RTC on this
/// kernel, so timestamps/NTRes are left zeroed rather than faked.
fn write_raw_entry(dst: &mut [u8], short_name: &[u8; 11], attr: u8, cluster: u32, size: u32) {
    dst[0..11].copy_from_slice(short_name);
    dst[11] = attr;
    dst[12..20].fill(0); // NTRes, creation time/date, last access date
    dst[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    dst[22..26].fill(0); // write time/date
    dst[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    dst[28..32].copy_from_slice(&size.to_le_bytes());
}

/// Builds the `.` short-name field (`". "` space-padded to 11 bytes)
/// programmatically rather than via a hand-counted byte-string literal.
fn dot_name() -> [u8; 11] {
    let mut name = [b' '; 11];
    name[0] = b'.';
    name
}

/// Builds the `..` short-name field, same reasoning as [`dot_name`].
fn dotdot_name() -> [u8; 11] {
    let mut name = [b' '; 11];
    name[0] = b'.';
    name[1] = b'.';
    name
}

fn is_dot_or_dotdot(name: &str) -> bool {
    name == "." || name == ".."
}

/// Splits an absolute path into `(parent_path, name)` - e.g.
/// `/EFI/NEWDIR` -> `("/EFI", "NEWDIR")`, `/NEWDIR` -> `("/", "NEWDIR")`.
/// Returns `None` for `/` itself or a name-less path (nothing to split).
pub(crate) fn split_parent(path: &str) -> Option<(&str, &str)> {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    // Manual reverse byte scan, not `rfind('/')` - a char-pattern
    // `rfind` pulls libcore's `memrchr`, whose prebuilt (non-PIC)
    // object carries absolute relocations a PIE userland link rejects
    // outright (`R_AARCH64_ABS64 cannot be used against local symbol`)
    // - the same prebuilt-libcore constraint family as the documented
    // `slice_error_fail`/release-only cases, found the hard way when
    // this module first moved to userland.
    let slash = {
        let bytes = trimmed.as_bytes();
        let mut found = None;
        let mut i = bytes.len();
        while i > 0 {
            i -= 1;
            if bytes[i] == b'/' {
                found = Some(i);
                break;
            }
        }
        found?
    };
    // Non-panicking `.get()` slicing, not `&s[a..b]` - a userland
    // program's str-slice panic path drags non-PIC libcore objects into
    // the link and fails it outright (the documented
    // `slice_error_fail` constraint from the output-redirection
    // milestone; the offsets are guaranteed char boundaries anyway,
    // they came from `rfind`).
    let name = trimmed.get(slash + 1..)?;
    if name.is_empty() {
        return None;
    }
    let parent = if slash == 0 { "/" } else { trimmed.get(..slash)? };
    Some((parent, name))
}

/// Encodes `name` as an 11-byte 8.3 short-name field, or `None` if it
/// can't be represented that way. Deliberately conservative rather than
/// implementing FAT's full legal short-name character set: only ASCII
/// alphanumerics, `_`, and `-` are accepted, one optional `.` separating
/// an up-to-8-character base from an up-to-3-character extension. A name
/// this rejects can still exist on disk (created by a real FAT
/// formatter/OS) and [`Fs::find`]/[`walk_dir`](Fs::walk_dir) will read it
/// back fine - this limitation only applies to names *this kernel*
/// creates via `mkdir`.
fn make_short_name(name: &str) -> Option<[u8; 11]> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return None;
    }
    let (base, ext) = match name.match_indices('.').count() {
        0 => (name, ""),
        1 => {
            let dot = name.find('.')?;
            // `.get()` for the same non-panicking-slice reason as
            // `split_parent` above.
            (name.get(..dot)?, name.get(dot + 1..)?)
        }
        _ => return None,
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    if !base.bytes().all(is_valid_short_name_byte) || !ext.bytes().all(is_valid_short_name_byte) {
        return None;
    }

    let mut short = [b' '; 11];
    for (i, b) in base.bytes().enumerate() {
        short[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        short[8 + i] = b.to_ascii_uppercase();
    }
    Some(short)
}

fn is_valid_short_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Validates a name a create op wants to lay down, covering *both* the 8.3
/// and long-name paths - called by `mkdir`/`touch`/`write_file`/`mv` before
/// any cluster is allocated, so an unrepresentable name fails cleanly rather
/// than leaving orphaned clusters. Accepts anything [`make_short_name`]
/// accepts (the 8.3 fast path), otherwise requires a valid long name
/// ([`is_valid_lfn_name`]). The value it *adds* over the old
/// `make_short_name(name).ok_or(InvalidName)` is that a legal-but-not-8.3
/// name now passes here (and becomes an LFN) instead of being rejected.
fn validate_create_name(name: &str) -> Result<(), Error> {
    if make_short_name(name).is_some() || is_valid_lfn_name(name) {
        Ok(())
    } else {
        Err(Error::InvalidName)
    }
}

/// Whether `name` is a legal FAT long filename (LFN) this driver will create.
/// Rejects the characters FAT reserves (`" * / : < > ? \ |`) and control
/// bytes, anything longer than [`LONG_NAME_MAX`], the empty name, and a name
/// made only of dots/spaces (which would sanitize to no usable short-alias
/// base and isn't a meaningful filename). The read side is more permissive -
/// it renders whatever a foreign formatter wrote; this is only the gate on
/// what *we* create.
fn is_valid_lfn_name(name: &str) -> bool {
    if name.is_empty() || name.len() > LONG_NAME_MAX {
        return false;
    }
    let mut all_dot_space = true;
    for &b in name.as_bytes() {
        match b {
            b'"' | b'*' | b'/' | b':' | b'<' | b'>' | b'?' | b'\\' | b'|' => return false,
            0..=0x1f => return false,
            b'.' | b' ' => {}
            _ => all_dot_space = false,
        }
    }
    !all_dot_space
}

/// Maps a name byte to its 8.3 short-name form (uppercased) for alias
/// generation, or `None` if it can't appear in a short name (dropped rather
/// than substituted, so `"my file"` -> base `MYFILE`, not `MY_FILE`).
fn sanitize_short_char(b: u8) -> Option<u8> {
    let u = b.to_ascii_uppercase();
    if is_valid_short_name_byte(u) {
        Some(u)
    } else {
        None
    }
}

/// Composes an `NAME~N`-style 11-byte 8.3 short name from a sanitized `base`
/// (already uppercase, ≤ 8 valid bytes), `ext` (≤ 3 valid bytes), and the
/// numeric tail `n`. The `~<n>` tail always fits: the base is trimmed so
/// `base_take + 1 + digits(n) <= 8`, so even `~999999` (7 chars) leaves room
/// for one base char.
fn compose_alias(base: &[u8], ext: &[u8], n: u32) -> [u8; 11] {
    // Decimal digits of `n`, high to low, into a fixed buffer (no fmt - a
    // `write!` here would pull core::fmt's panic formatter, the documented
    // PIE relocation wall this module keeps clear of; see `split_parent`).
    let mut digits = [0u8; 7];
    let ndig;
    let mut v = n;
    if v == 0 {
        digits[0] = b'0';
        ndig = 1;
    } else {
        // fill low-to-high, then reverse
        let mut tmp = [0u8; 7];
        let mut t = 0;
        while v > 0 {
            tmp[t] = b'0' + (v % 10) as u8;
            v /= 10;
            t += 1;
        }
        for k in 0..t {
            digits[k] = tmp[t - 1 - k];
        }
        ndig = t;
    }

    let tail_len = 1 + ndig; // '~' + digits
    let base_take = base.len().min(8 - tail_len);

    let mut short = [b' '; 11];
    short[..base_take].copy_from_slice(&base[..base_take]);
    short[base_take] = b'~';
    short[base_take + 1..base_take + 1 + ndig].copy_from_slice(&digits[..ndig]);
    for (i, &b) in ext.iter().enumerate() {
        short[8 + i] = b;
    }
    short
}

/// Builds the on-disk directory entries for a long `name`: the LFN entries
/// (highest sequence number first, i.e. physical order) followed by the
/// short entry carrying `short`/`attr`/`cluster`/`size`, all into `out`.
/// Returns the number of 32-byte entries written (`num_lfn + 1`). The
/// checksum binding the LFN run to its short entry ([`lfn_checksum`]) is
/// stamped into every LFN entry, and each entry's 13 characters are placed
/// with [`put_lfn_chars`] so the read side ([`lfn_chars`]) reconstructs the
/// exact name.
fn build_name_entries(
    name: &[u8],
    short: &[u8; 11],
    attr: u8,
    cluster: u32,
    size: u32,
    out: &mut [u8],
) -> usize {
    let checksum = lfn_checksum(short);
    let num_lfn = name.len().div_ceil(LFN_CHARS_PER_ENTRY);
    for idx in 0..num_lfn {
        // Entries are stored in reverse: the highest sequence number (with the
        // "last" bit) comes physically first, sequence 1 physically last.
        let seq = num_lfn - idx;
        let e = &mut out[idx * DIR_ENTRY_SIZE..(idx + 1) * DIR_ENTRY_SIZE];
        e.fill(0);
        e[0] = seq as u8 | if idx == 0 { LFN_LAST_MASK } else { 0 };
        e[11] = ATTR_LFN;
        // e[12] type = 0, e[13] checksum, e[26..28] first-cluster = 0.
        e[13] = checksum;
        put_lfn_chars(e, name, (seq - 1) * LFN_CHARS_PER_ENTRY);
    }
    let so = num_lfn * DIR_ENTRY_SIZE;
    write_raw_entry(&mut out[so..so + DIR_ENTRY_SIZE], short, attr, cluster, size);
    num_lfn + 1
}

/// Places up to 13 of `name`'s characters (starting at index `start`) into
/// one LFN entry `e` as UTF-16LE, at the [`LFN_POS`] offsets. A `0x0000`
/// terminator follows the last real character when it fits, and remaining
/// slots are `0xFFFF` padding - exactly what [`lfn_chars`] expects when
/// reading back. Only the low byte of each character is written (ASCII), the
/// high byte left `0`; the shell produces ASCII names.
fn put_lfn_chars(e: &mut [u8], name: &[u8], start: usize) {
    for (k, &p) in LFN_POS.iter().enumerate() {
        let ci = start + k;
        if ci < name.len() {
            e[p] = name[ci];
            e[p + 1] = 0;
        } else if ci == name.len() {
            e[p] = 0x00; // null terminator
            e[p + 1] = 0x00;
        } else {
            e[p] = 0xff; // padding past the terminator
            e[p + 1] = 0xff;
        }
    }
}

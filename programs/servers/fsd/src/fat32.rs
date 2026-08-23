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
//! Long filenames (LFN) are **read** now (a name like `index.html` is
//! reconstructed from the LFN entries a real formatter writes, and
//! matched/listed by that long name), but not yet **written**:
//! `make_short_name` still only creates 8.3 names, so a file the guest
//! itself creates can't have a long name. Still first-FAT32-partition
//! only, and `make run`'s vvfat disk is still FAT16 - so mounting
//! fails there and every request answers `NO_FS`, same degradation as
//! always.

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
    /// A write was attempted on a read-only filesystem (the exFAT arm, whose
    /// write support is a later milestone). FAT32 is fully read-write and
    /// never returns this. This is the shared `Error` type for every
    /// filesystem arm (see `vfs.rs`), so it lives here with the rest.
    ReadOnly,
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
        }
    }

    fn root(root_cluster: u32) -> Self {
        DirEntry {
            name: [0; LONG_NAME_MAX],
            name_len: 0,
            is_dir: true,
            size: 0,
            cluster: root_cluster,
        }
    }
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
    const POS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
    let mut n = 0;
    for &p in &POS {
        let c = u16::from_le_bytes([raw[p], raw[p + 1]]);
        if c == 0x0000 || c == 0xffff {
            break;
        }
        out[n] = if c < 0x80 { c as u8 } else { b'?' };
        n += 1;
    }
    n
}

pub struct Fs {
    disk: Disk,
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
            sectors_per_cluster,
            fat_start_lba,
            data_start_lba,
            root_cluster,
            num_fats,
            fat_size_32,
        })
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
        let mut cluster = file.cluster;
        // Byte position (within the file) of `cluster`'s first byte.
        let mut cluster_pos = 0usize;
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
        let short_name = make_short_name(name).ok_or(Error::InvalidName)?;

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

        self.insert_dir_entry(parent.cluster, &short_name, ATTR_DIRECTORY, new_cluster, 0)
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

        let mut sector = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sector)?;
        sector[offset] = DIR_ENTRY_FREE;
        self.disk.write_sector(lba, &sector)?;
        Ok(())
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
        let short_name = make_short_name(name).ok_or(Error::InvalidName)?;

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
        self.insert_dir_entry(parent.cluster, &short_name, 0, 0, 0)
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

        let mut sector = [0u8; SECTOR_SIZE];
        self.disk.read_sector(lba, &mut sector)?;
        sector[offset] = DIR_ENTRY_FREE;
        self.disk.write_sector(lba, &sector)?;
        Ok(())
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
        let short_name = if existing.is_none() {
            Some(make_short_name(name).ok_or(Error::InvalidName)?)
        } else {
            None
        };

        let new_cluster = self.write_chain(data)?;

        if let Some((lba, offset, old_cluster)) = existing {
            if old_cluster != 0 {
                self.free_chain(old_cluster)?;
            }
            self.patch_entry_cluster_size(lba, offset, new_cluster, data.len() as u32)
        } else {
            self.insert_dir_entry(
                parent.cluster,
                &short_name.unwrap(),
                0,
                new_cluster,
                data.len() as u32,
            )
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

    /// Renames or moves the file or directory at `src` to `dst`. `dst`
    /// must not already exist - `mv` refuses rather than overwriting it
    /// or (if `dst` happens to be an existing directory) moving `src`
    /// inside it keeping its old name, narrower than a real `mv`'s full
    /// semantics.
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
    /// parent) never leaves `src` half-deleted.
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
        let short_name = make_short_name(dst_name).ok_or(Error::InvalidName)?;

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

        let mut dst_exists = false;
        self.walk_dir(dst_parent.cluster, |entry| {
            if entry.name().eq_ignore_ascii_case(dst_name) {
                dst_exists = true;
                true
            } else {
                false
            }
        })?;
        if dst_exists {
            return Err(Error::AlreadyExists);
        }

        let attr = if src_entry.is_dir { ATTR_DIRECTORY } else { 0 };
        self.insert_dir_entry(
            dst_parent.cluster,
            &short_name,
            attr,
            src_entry.cluster,
            src_entry.size,
        )?;

        let mut sector = [0u8; SECTOR_SIZE];
        self.disk.read_sector(src_lba, &mut sector)?;
        sector[src_offset] = DIR_ENTRY_FREE;
        self.disk.write_sector(src_lba, &sector)?;

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

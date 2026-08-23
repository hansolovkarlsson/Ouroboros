//! exFAT, read-only - the second on-disk format `fsd` understands, and the
//! first real exercise of the [`Filesystem`](crate::vfs::Filesystem) enum's
//! dispatch (until now FAT32 was the only arm). Read-only first, matching the
//! big scoping lever the "more filesystems" roadmap arc calls out: FAT32's own
//! write support was where every corruption risk lived (phases 4-8), so a new
//! format lands read-only as one milestone and read-write as a separate one.
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
//! The genuinely different parts, and how a read-only driver handles each:
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
//! - **An allocation *bitmap*** replaces FAT free-cluster scanning - but that's
//!   a *write* concern (finding free clusters). A read-only driver never
//!   allocates, so it ignores the bitmap directory entry (`0x81`) completely.
//! - **An up-case table** (`0x82`) drives case-insensitive comparison per spec.
//!   We approximate with ASCII case-folding ([`str::eq_ignore_ascii_case`],
//!   what `fat32.rs` already uses) - correct for ASCII names, which is all this
//!   system creates or reads; the table entry is ignored.
//!
//! Every write op returns [`Error::ReadOnly`]; see [`Fs::write_file`] and the
//! siblings below.

use crate::disk::Disk;
use crate::fat32::Error;

const SECTOR_SIZE: usize = 512;
const ENTRY_SIZE: usize = 32;

// Directory entry type bytes. The high bit (0x80) is "in use"; a byte with it
// clear is a deleted/unused entry, and a literal 0x00 marks end-of-directory.
const ET_FILE: u8 = 0x85; // primary entry of a file/dir set
const ET_STREAM_EXT: u8 = 0xc0; // first secondary: cluster/size/flags
const ET_FILE_NAME: u8 = 0xc1; // secondary: 15 UTF-16 name chars

/// `FileAttributes` bit 4 (0x0010): this entry is a directory.
const ATTR_DIRECTORY: u16 = 0x0010;
/// `GeneralSecondaryFlags` bit 1: the data is contiguous, don't use the FAT.
const NO_FAT_CHAIN: u8 = 0x02;

/// exFAT FAT end-of-chain / bad-cluster threshold: `0xFFFFFFF7` is the bad
/// marker and `0xFFFFFFFF` is end-of-chain; anything at/above the bad marker
/// (and `0` = free) terminates a walk. Real clusters are `2..cluster_count+1`.
const END_OF_CHAIN_MIN: u32 = 0xffff_fff7;

/// Longest reconstructed name we keep - the exFAT maximum.
const LONG_NAME_MAX: usize = 255;

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

pub struct Fs {
    disk: Disk,
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

        Ok(Fs {
            disk,
            sectors_per_cluster: 1u32 << sectors_per_cluster_shift,
            fat_lba: partition_lba + fat_offset,
            cluster_heap_lba: partition_lba + cluster_heap_offset,
            cluster_count,
            root_cluster,
        })
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
        let byte = cluster as u64 * 4;
        let sector = self.fat_lba as u64 + byte / SECTOR_SIZE as u64;
        let offset = (byte % SECTOR_SIZE as u64) as usize;
        let mut buf = [0u8; SECTOR_SIZE];
        self.disk.read_sector(sector, &mut buf)?;
        let value = u32::from_le_bytes([
            buf[offset],
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
        ]);
        if value == 0 || value >= END_OF_CHAIN_MIN {
            Ok(None)
        } else {
            Ok(Some(value))
        }
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
        mut f: impl FnMut(&DirEntry) -> bool,
    ) -> Result<(), Error> {
        let mut cluster = start_cluster;
        let mut consumed: u64 = 0;

        // Assembly state for the entry set currently being read.
        let mut in_set = false;
        let mut secondaries_left = 0u8;
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
                    if dir_size != 0 && consumed >= dir_size {
                        return Ok(());
                    }
                    consumed += ENTRY_SIZE as u64;

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
                        if f(&entry) {
                            return Ok(());
                        }
                    }
                }
            }
            match self.advance(cluster, contiguous)? {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
        }
    }

    /// Resolve an absolute, `/`-separated path to its entry set. Matching is
    /// case-insensitive (ASCII case-fold, see the module doc). Unlike FAT32
    /// there's no `.`/`..`/cluster-0-means-root quirk to special-case.
    fn find(&mut self, path: &str) -> Result<DirEntry, Error> {
        let mut current = self.root_entry();
        for component in path.split('/').filter(|c| !c.is_empty()) {
            if !current.is_dir {
                return Err(Error::NotADirectory);
            }
            let mut found: Option<DirEntry> = None;
            self.walk_dir(
                current.first_cluster,
                current.contiguous,
                current.size,
                |entry| {
                    if entry.name().eq_ignore_ascii_case(component) {
                        found = Some(*entry);
                        true
                    } else {
                        false
                    }
                },
            )?;
            current = found.ok_or(Error::NotFound)?;
        }
        Ok(current)
    }

    /// Lists a directory, calling `f(name, is_dir, size)` for each entry. `""`
    /// or `"/"` lists the root. Mirrors `fat32::Fs::list_dir`.
    pub fn list_dir(
        &mut self,
        path: &str,
        mut f: impl FnMut(&str, bool, u32),
    ) -> Result<(), Error> {
        let dir = self.find(path)?;
        if !dir.is_dir {
            return Err(Error::NotADirectory);
        }
        self.walk_dir(dir.first_cluster, dir.contiguous, dir.size, |entry| {
            f(entry.name(), entry.is_dir, saturate_u32(entry.size));
            false
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
        let file = self.find(path)?;
        if file.is_dir {
            return Err(Error::NotAFile);
        }
        let total = file.size;
        if offset >= total || buf.is_empty() || file.first_cluster == 0 {
            return Ok(0);
        }
        let want = ((total - offset) as usize).min(buf.len());
        let offset = offset as usize;

        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR_SIZE;
        let mut cluster = file.first_cluster;
        let mut cluster_pos = 0usize;
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
        Ok(written as u32)
    }

    // ---- write surface: read-only for now (see the module doc) --------------
    // Every write op returns `Error::ReadOnly`, which `main.rs` maps to
    // `FS_ERR_READ_ONLY`. The signatures match `fat32::Fs` exactly so `vfs`'s
    // dispatch is a uniform forward. Write support is a separate milestone.

    pub fn write_file(&mut self, _path: &str, _data: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn write_at(&mut self, _path: &str, _offset: u64, _data: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mkdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rmdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn touch(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rm(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mv(&mut self, _src: &str, _dst: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
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

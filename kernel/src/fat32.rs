//! Minimal read-only FAT32 reader over `virtio_blk::Device`. Phase 3b of
//! `docs/roadmap.md`.
//!
//! ## Hand-rolled, not a crate - a real constraint, not just precedent
//!
//! Every parser so far in this project (ACPI, devicetree, PCI, virtio) has
//! been hand-rolled rather than pulling in a crate, generally as a
//! deliberate choice to depend on no more than the data actually needs
//! (see `acpi.rs`'s module doc comment). For FAT32 there's also a hard
//! constraint pushing the same direction: this runs after
//! `exit_boot_services`, where the global allocator is no longer valid
//! (it was boot-services-backed - see `main.rs`), so anything reading the
//! filesystem at runtime has to do it with **zero heap allocation**.
//! Every existing `no_std` FAT crate surveyed assumes an allocator is
//! reachable somewhere in its stack (directory listings as `Vec`, path
//! buffers as `String`); reworking one to avoid that would likely be more
//! effort than writing exactly the subset this project needs by hand.
//!
//! ## No long filenames (LFN)
//!
//! Every file this project creates so far fits an 8.3 short name
//! (`SH.BIN`, `INIT.CFG`, `BOOTAA64.EFI`), so LFN directory entries
//! (attribute `0x0F`) are recognized and skipped, not parsed. A real gap
//! for any file with a longer name, not a permanent design decision.
//!
//! ## `run` vs `run-image` - the on-disk format actually differs
//!
//! A real, confirmed-by-inspection finding, not a formality: QEMU's
//! `vvfat` driver (`make run`'s fast dev-loop backend, `fat:rw:<dir>`)
//! produces **FAT16**, not FAT32 - confirmed by decoding its BPB directly
//! with a temporary boot-time hex dump before writing this module:
//! `BS_FilSysType` reads `"FAT16   "`, and `RootEntryCount`/`FATSz16` are
//! both nonzero, which real FAT32 requires to be zero. `esp.img` (built
//! by `hdiutil -fs FAT32`, what `make image`/`make parallels-hdd` and
//! therefore Parallels itself ultimately boot from) is genuinely FAT32,
//! confirmed the same way. [`Fs::mount`] therefore only ever works
//! against `make run-image`, never plain `make run` - see the Makefile's
//! `run-image` target for the full explanation.

use crate::virtio_blk;

const SECTOR_SIZE: usize = 512;
const DIR_ENTRY_SIZE: usize = 32;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LFN: u8 = 0x0f;
const DIR_ENTRY_FREE: u8 = 0xe5;
const DIR_ENTRY_END: u8 = 0x00;
const FAT32_PARTITION_TYPES: [u8; 2] = [0x0b, 0x0c]; // FAT32 CHS, FAT32 LBA
const END_OF_CHAIN_MIN: u32 = 0x0fff_fff8;
const MAX_NAME_LEN: usize = 12; // 8 name + '.' + 3 ext, the most an 8.3 short name is ever

#[derive(Debug)]
pub enum Error {
    NoFat32Partition,
    NotFat32,
    UnsupportedSectorSize(u16),
    Io(virtio_blk::Error),
    NotFound,
    NotAFile,
    NotADirectory,
}

impl From<virtio_blk::Error> for Error {
    fn from(e: virtio_blk::Error) -> Self {
        Error::Io(e)
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoFat32Partition => write!(f, "no FAT32 partition (type 0x0b/0x0c) in the MBR"),
            Error::NotFat32 => write!(f, "partition's BPB doesn't look like FAT32 (RootEntryCount/FATSz16 nonzero, or bad signature)"),
            Error::UnsupportedSectorSize(n) => write!(f, "unsupported sector size {n} (only 512 is implemented)"),
            Error::Io(e) => write!(f, "disk read failed: {e}"),
            Error::NotFound => write!(f, "not found"),
            Error::NotAFile => write!(f, "not a file"),
            Error::NotADirectory => write!(f, "not a directory"),
        }
    }
}

/// One directory entry, decoded into a self-contained, no-alloc form -
/// the raw 32-byte on-disk record doesn't outlive the sector buffer it
/// was read from, so callers get this instead.
#[derive(Clone, Copy)]
pub struct DirEntry {
    name: [u8; MAX_NAME_LEN],
    name_len: u8,
    pub is_dir: bool,
    pub size: u32,
    cluster: u32,
}

impl DirEntry {
    pub fn name(&self) -> &str {
        // SAFETY-free: always built from ASCII short-name bytes in `parse`.
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }

    fn parse(raw: &[u8]) -> Self {
        let attr = raw[11];
        let cluster_hi = u16::from_le_bytes([raw[20], raw[21]]) as u32;
        let cluster_lo = u16::from_le_bytes([raw[26], raw[27]]) as u32;
        let size = u32::from_le_bytes([raw[28], raw[29], raw[30], raw[31]]);

        let mut name = [0u8; MAX_NAME_LEN];
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

        DirEntry {
            name,
            name_len: len as u8,
            is_dir: attr & ATTR_DIRECTORY != 0,
            size,
            cluster: (cluster_hi << 16) | cluster_lo,
        }
    }

    fn root(root_cluster: u32) -> Self {
        DirEntry { name: [0; MAX_NAME_LEN], name_len: 0, is_dir: true, size: 0, cluster: root_cluster }
    }
}

pub struct Fs {
    device: virtio_blk::Device,
    sectors_per_cluster: u32,
    fat_start_lba: u32,
    data_start_lba: u32,
    root_cluster: u32,
}

impl Fs {
    /// Reads the MBR, finds the first FAT32-typed partition, reads and
    /// validates its BPB, and computes the FAT/data region layout.
    /// Doesn't touch anything beyond sector 0 and the partition's own
    /// first sector.
    ///
    /// `device` must already be initialized (`Device::init` called) -
    /// every `read_sector` call in this module relies on that, checked
    /// once here rather than at each call site.
    pub fn mount(mut device: virtio_blk::Device) -> Result<Self, Error> {
        let mut mbr = [0u8; SECTOR_SIZE];
        // SAFETY: `device` was already initialized by the caller (see
        // `mount`'s doc comment).
        unsafe { device.read_sector(0, &mut mbr) }?;

        let mut partition_lba = None;
        for i in 0..4 {
            let entry = 0x1be + i * 16;
            if FAT32_PARTITION_TYPES.contains(&mbr[entry + 4]) {
                partition_lba = Some(u32::from_le_bytes([
                    mbr[entry + 8],
                    mbr[entry + 9],
                    mbr[entry + 10],
                    mbr[entry + 11],
                ]));
                break;
            }
        }
        let partition_lba = partition_lba.ok_or(Error::NoFat32Partition)?;

        let mut bpb = [0u8; SECTOR_SIZE];
        // SAFETY: same as above.
        unsafe { device.read_sector(partition_lba as u64, &mut bpb) }?;

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

        Ok(Fs { device, sectors_per_cluster, fat_start_lba, data_start_lba, root_cluster })
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
        // SAFETY: `self.device` was initialized before `mount` returned.
        unsafe { self.device.read_sector(sector as u64, &mut buf) }?;
        let raw = u32::from_le_bytes([buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]]);
        let value = raw & 0x0fff_ffff;
        if value == 0 || value >= END_OF_CHAIN_MIN {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    }

    /// Calls `f` for every real entry (LFN/free/volume-ID entries are
    /// skipped) in the directory starting at `start_cluster`, stopping
    /// early the first time `f` returns `true`.
    fn walk_dir(&mut self, start_cluster: u32, mut f: impl FnMut(&DirEntry) -> bool) -> Result<(), Error> {
        let mut cluster = start_cluster;
        loop {
            let lba = self.cluster_to_lba(cluster);
            for s in 0..self.sectors_per_cluster {
                let mut buf = [0u8; SECTOR_SIZE];
                // SAFETY: same as `next_cluster`.
                unsafe { self.device.read_sector((lba + s) as u64, &mut buf) }?;
                for raw in buf.chunks_exact(DIR_ENTRY_SIZE) {
                    match raw[0] {
                        DIR_ENTRY_END => return Ok(()),
                        DIR_ENTRY_FREE => continue,
                        _ => {}
                    }
                    let attr = raw[11];
                    if attr == ATTR_LFN || attr & ATTR_VOLUME_ID != 0 {
                        continue;
                    }
                    if f(&DirEntry::parse(raw)) {
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
    /// `/EFI/OUROBORO/SH.BIN`) to its directory entry, walking one
    /// directory per path component. Matching is case-insensitive, same
    /// as FAT itself.
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
        }
        Ok(current)
    }

    /// Lists a directory's entries, calling `f(name, is_dir, size)` for
    /// each. `path` `""` or `"/"` lists the root.
    pub fn list_dir(&mut self, path: &str, mut f: impl FnMut(&str, bool, u32)) -> Result<(), Error> {
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
                // SAFETY: same as `next_cluster`.
                unsafe { self.device.read_sector((lba + s) as u64, &mut sector) }?;
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
}

//! The filesystem-multiplexing layer inside `fsd` (the VFS refactor).
//!
//! Clients already speak a filesystem-agnostic protocol (`FSOP_*`) over IPC -
//! they never know it's FAT32. What was missing was *internal* multiplexing so
//! `fsd` can drive more than one on-disk format: detect the type at mount time
//! and dispatch each op to the right driver. `Filesystem` is that dispatch.
//!
//! An **enum** (not `dyn Trait`) because `fsd` is `no_std` with no heap - the
//! same enum-over-`dyn` pattern `block::BlockDevice` and `console::Console`
//! already use for the same reason. Today FAT32 is the only arm; a second
//! filesystem (exFAT, ext2) is a new arm plus a branch in [`Filesystem::mount`]'s
//! type detection. The per-op methods forward to the arm, so this is a pure
//! refactor: `main.rs` calls `Filesystem` exactly as it called `fat32::Fs`, and
//! behaviour is byte-identical while FAT32 is the only format.

use crate::disk::Disk;
use crate::exfat;
use crate::ext2;
use crate::fat32;
use crate::partition;

/// Failure reasons, shared across every filesystem arm (defined in `fat32.rs`,
/// which held the only arm originally; now shared by the exFAT arm too).
pub use crate::fat32::Error;

/// A mounted filesystem, whatever its on-disk format.
pub enum Filesystem {
    Fat32(fat32::Fs),
    /// exFAT, read-write (see [`exfat`]).
    ExFat(exfat::Fs),
    /// ext2, read-write (see [`ext2`]).
    Ext2(ext2::Fs),
}

impl Filesystem {
    /// Discover the disk's partitions (MBR or GPT - see `partition::discover`)
    /// and mount the first one that holds a filesystem we recognize. Each
    /// partition is probed as FAT32 ([`fat32::Fs::mount_at`]) then exFAT
    /// ([`exfat::Fs::mount_at`]), taking the first that validates; a future
    /// format (ext2, ...) adds another probe here and its own arm. Returns
    /// [`Error::NoFat32Partition`] if no partition mounts as any known format.
    pub fn mount(mut disk: Disk) -> Result<Self, Error> {
        let mut parts = [0u64; partition::MAX_PARTITIONS];
        let count = partition::discover(&mut disk, &mut parts)?;
        for &lba in &parts[..count] {
            // The layout arithmetic is 32-bit (partitions past 2 TB aren't a
            // concern here); a start LBA that doesn't fit just isn't tried.
            let Ok(lba32) = u32::try_from(lba) else {
                continue;
            };
            if let Ok(fs) = fat32::Fs::mount_at(Disk, lba32) {
                return Ok(Filesystem::Fat32(fs));
            }
            if let Ok(fs) = exfat::Fs::mount_at(Disk, lba32) {
                return Ok(Filesystem::ExFat(fs));
            }
            if let Ok(fs) = ext2::Fs::mount_at(Disk, lba32) {
                return Ok(Filesystem::Ext2(fs));
            }
        }
        Err(Error::NoFat32Partition)
    }

    /// Mount the disk's `index`-th partition (same MBR/GPT discovery order as
    /// [`mount`](Self::mount), which mounts the first that validates). Probes
    /// FAT32-then-exFAT-then-ext2 at that one partition. The multi-mount entry
    /// point (cluster Phase 0): a client mounts a *specific* partition into a
    /// tree rather than taking whatever validates first. Returns
    /// [`Error::NoFat32Partition`] if `index` is out of range or the partition
    /// mounts as no known format.
    pub fn mount_partition(mut disk: Disk, index: usize) -> Result<Self, Error> {
        let mut parts = [0u64; partition::MAX_PARTITIONS];
        let count = partition::discover(&mut disk, &mut parts)?;
        if index >= count {
            return Err(Error::NoFat32Partition);
        }
        let Ok(lba32) = u32::try_from(parts[index]) else {
            return Err(Error::NoFat32Partition);
        };
        if let Ok(fs) = fat32::Fs::mount_at(Disk, lba32) {
            return Ok(Filesystem::Fat32(fs));
        }
        if let Ok(fs) = exfat::Fs::mount_at(Disk, lba32) {
            return Ok(Filesystem::ExFat(fs));
        }
        if let Ok(fs) = ext2::Fs::mount_at(Disk, lba32) {
            return Ok(Filesystem::Ext2(fs));
        }
        Err(Error::NoFat32Partition)
    }

    /// The mounted format's name, for the startup/mount log line.
    pub fn name(&self) -> &'static str {
        match self {
            Filesystem::Fat32(_) => "FAT32",
            Filesystem::ExFat(_) => "exFAT",
            Filesystem::Ext2(_) => "ext2",
        }
    }

    /// The first sector of the mounted volume - `mount`-info reporting only
    /// (disk-tools milestone 1).
    pub fn partition_lba(&self) -> u32 {
        match self {
            Filesystem::Fat32(fs) => fs.partition_lba(),
            Filesystem::ExFat(fs) => fs.partition_lba(),
            Filesystem::Ext2(fs) => fs.partition_lba(),
        }
    }

    pub fn list_dir(&mut self, path: &str, f: impl FnMut(&str, bool, u32)) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.list_dir(path, f),
            Filesystem::ExFat(fs) => fs.list_dir(path, f),
            Filesystem::Ext2(fs) => fs.list_dir(path, f),
        }
    }

    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_file(path, buf),
            Filesystem::ExFat(fs) => fs.read_file(path, buf),
            Filesystem::Ext2(fs) => fs.read_file(path, buf),
        }
    }

    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_at(path, offset, buf),
            Filesystem::ExFat(fs) => fs.read_at(path, offset, buf),
            Filesystem::Ext2(fs) => fs.read_at(path, offset, buf),
        }
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.write_file(path, data),
            Filesystem::ExFat(fs) => fs.write_file(path, data),
            Filesystem::Ext2(fs) => fs.write_file(path, data),
        }
    }

    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.write_at(path, offset, data),
            Filesystem::ExFat(fs) => fs.write_at(path, offset, data),
            Filesystem::Ext2(fs) => fs.write_at(path, offset, data),
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.mkdir(path),
            Filesystem::ExFat(fs) => fs.mkdir(path),
            Filesystem::Ext2(fs) => fs.mkdir(path),
        }
    }

    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.rmdir(path),
            Filesystem::ExFat(fs) => fs.rmdir(path),
            Filesystem::Ext2(fs) => fs.rmdir(path),
        }
    }

    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.touch(path),
            Filesystem::ExFat(fs) => fs.touch(path),
            Filesystem::Ext2(fs) => fs.touch(path),
        }
    }

    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.rm(path),
            Filesystem::ExFat(fs) => fs.rm(path),
            Filesystem::Ext2(fs) => fs.rm(path),
        }
    }

    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.mv(src, dst),
            Filesystem::ExFat(fs) => fs.mv(src, dst),
            Filesystem::Ext2(fs) => fs.mv(src, dst),
        }
    }
}

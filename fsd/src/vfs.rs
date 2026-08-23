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
use crate::fat32;
use crate::partition;

/// Failure reasons, shared across every filesystem arm (FAT32's `Error` today).
pub use crate::fat32::Error;

/// A mounted filesystem, whatever its on-disk format.
pub enum Filesystem {
    Fat32(fat32::Fs),
}

impl Filesystem {
    /// Discover the disk's partitions (MBR or GPT - see `partition::discover`)
    /// and mount the first one that holds a filesystem we recognize. Today that
    /// means trying FAT32 ([`fat32::Fs::mount_at`]) at each partition and taking
    /// the first that validates; a future format (exFAT, ext2) adds another
    /// probe here and its own arm. Returns [`Error::NoFat32Partition`] if no
    /// partition mounts.
    pub fn mount(mut disk: Disk) -> Result<Self, Error> {
        let mut parts = [0u64; partition::MAX_PARTITIONS];
        let count = partition::discover(&mut disk, &mut parts)?;
        for &lba in &parts[..count] {
            // FAT32's layout arithmetic is 32-bit (partitions past 2 TB aren't
            // a concern here); a start LBA that doesn't fit just isn't tried.
            let Ok(lba32) = u32::try_from(lba) else {
                continue;
            };
            if let Ok(fs) = fat32::Fs::mount_at(Disk, lba32) {
                return Ok(Filesystem::Fat32(fs));
            }
        }
        Err(Error::NoFat32Partition)
    }

    pub fn list_dir(&mut self, path: &str, f: impl FnMut(&str, bool, u32)) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.list_dir(path, f),
        }
    }

    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_file(path, buf),
        }
    }

    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        match self {
            Filesystem::Fat32(fs) => fs.read_at(path, offset, buf),
        }
    }

    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.write_file(path, data),
        }
    }

    pub fn write_at(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.write_at(path, offset, data),
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.mkdir(path),
        }
    }

    pub fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.rmdir(path),
        }
    }

    pub fn touch(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.touch(path),
        }
    }

    pub fn rm(&mut self, path: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.rm(path),
        }
    }

    pub fn mv(&mut self, src: &str, dst: &str) -> Result<(), Error> {
        match self {
            Filesystem::Fat32(fs) => fs.mv(src, dst),
        }
    }
}

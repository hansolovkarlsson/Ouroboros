//! Partition discovery for `fsd`'s mount step: a disk's partitions, MBR or
//! GPT, each its start LBA and whether its type marks a root, which
//! `vfs::Filesystem::mount` then probes in `vfs::root_order`'s order.
//!
//! The parsing is the pure `parttable` crate's since 2026-10-14, shared with
//! the kernel's boot-disk matcher and tested on the host (`make test`): GPT
//! validated by both CRCs with the backup header read when the primary does
//! not validate, every value bounded, and the root types (MBR 0x83, GPT's
//! AArch64 root GUID). Until then this file and the kernel's `bootdisk.rs`
//! each had a copy, which only QEMU boots reached. What is left here is the
//! disk: `BLOCK_READ` as the crate's read-sector closure.

use crate::disk::{Disk, DiskError, SECTOR_SIZE};

pub use parttable::Partition;

/// Most partitions `fsd` collects from one disk. A GPT can declare 128
/// entries, but nearly all are empty; a handful is all we ever mount from.
pub const MAX_PARTITIONS: usize = 16;

/// The disk's partitions into `out`, returning the count
/// ([`parttable::discover`], whose doc states the rules). No device is
/// [`DiskError::NoDevice`], as a read of sector 0 answered before; a read of
/// sector 0 or 1 failing is [`DiskError::Io`].
pub fn discover(disk: &mut Disk, out: &mut [Partition; MAX_PARTITIONS]) -> Result<usize, DiskError> {
    let capacity = disk.capacity_sectors()?;
    let mut read = |lba, buf: &mut [u8; SECTOR_SIZE]| disk.read_sector(lba, buf).is_ok();
    parttable::discover(&mut read, Some(capacity), out).map_err(|_| DiskError::Io)
}

/// Whether [`discover`] reads the disk as GPT ([`parttable::is_gpt`]). The
/// one test, so `format`, which writes MBR partitions only, refuses exactly
/// the disks `mount` reads as GPT (the medium review of #288).
pub fn is_gpt(disk: &mut Disk) -> Result<bool, DiskError> {
    let mut read = |lba, buf: &mut [u8; SECTOR_SIZE]| disk.read_sector(lba, buf).is_ok();
    parttable::is_gpt(&mut read).ok_or(DiskError::Io)
}

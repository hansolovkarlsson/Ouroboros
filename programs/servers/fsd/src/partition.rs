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

// No unsafe code: a disk handle conjured from nothing (`zeroed`, a
// `transmute`) or an `svc` of its own would reach the disk around the
// `Disk` this module was handed (disk.rs). `make check-disk-handle`.
#![forbid(unsafe_code)]

use crate::disk::{Disk, DiskError, SECTOR_SIZE};

pub use parttable::Partition;

/// Most partitions `fsd` collects from one disk. A GPT can declare 128
/// entries, but nearly all are empty; a handful is all we ever mount from.
pub const MAX_PARTITIONS: usize = 16;

/// The disk's partitions into `out`, returning the count
/// ([`parttable::discover`], whose doc states the rules). A capacity the
/// kernel does not answer costs only the backup GPT, as before the crate;
/// a read of sector 0 or 1 failing is the error that read gave, so a stick
/// pulled mid-scan is still [`DiskError::NoDevice`] (the review of #296).
pub fn discover(disk: &mut Disk, out: &mut [Partition; MAX_PARTITIONS]) -> Result<usize, DiskError> {
    let capacity = disk.capacity_sectors().ok();
    let mut failed = None;
    let found = parttable::discover(&mut reader(disk, &mut failed), capacity, out);
    found.map_err(|_| failed.unwrap_or(DiskError::Io))
}

/// Whether [`discover`] reads the disk as GPT ([`parttable::is_gpt`]). The
/// one test, so `format`, which writes MBR partitions only, refuses exactly
/// the disks `mount` reads as GPT (the medium review of #288).
pub fn is_gpt(disk: &mut Disk) -> Result<bool, DiskError> {
    let mut failed = None;
    let found = parttable::is_gpt(&mut reader(disk, &mut failed));
    found.ok_or_else(|| failed.unwrap_or(DiskError::Io))
}

/// `BLOCK_READ` as the crate's read-sector closure, the last failure's kind
/// kept in `failed`, since the closure can answer only yes or no.
fn reader<'a>(
    disk: &'a mut Disk,
    failed: &'a mut Option<DiskError>,
) -> impl FnMut(u64, &mut [u8; SECTOR_SIZE]) -> bool + 'a {
    move |lba, buf| match disk.read_sector(lba, buf) {
        Ok(()) => true,
        Err(e) => {
            *failed = Some(e);
            false
        }
    }
}

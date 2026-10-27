//! The filesystem server's disk access: thin safe wrappers over the
//! `BLOCK_*` syscalls, standing in for the kernel's old
//! `block::BlockDevice` (which `fat32.rs` used to own by value). The
//! kernel holds the actual device and only accepts these calls from
//! this task's slot (`syscall_abi::FSD_TASK`) - see `syscall.rs`'s
//! `block_access_allowed`.

use core::sync::atomic::{AtomicBool, Ordering};
use crate::syscall4;

/// One sector, matching `syscall_abi::BLOCK_SECTOR_SIZE` (duplicated
/// as a usize for array sizing, same as fat32.rs's own SECTOR_SIZE).
pub const SECTOR_SIZE: usize = 512;

#[derive(Debug, Clone, Copy)]
pub enum DiskError {
    /// No block device installed this boot (or the kernel refused the
    /// call - impossible from this task unless the ABI's FSD_TASK and
    /// the kernel's slot assignment ever disagree).
    NoDevice,
    /// The device-level read/write failed.
    Io,
}

/// Whether this server has rewritten the partition table this boot
/// ([`Device::table_rewritten`]). fsd is one task, so an atomic is only the
/// spelling of a mutable static, not a concurrency claim.
static TABLE_REWRITTEN: AtomicBool = AtomicBool::new(false);

/// Whether the [`Device`] has been claimed ([`Device::claim`]).
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// The block device, held once: fsd's `main` claims it at its start and
/// lends a [`Disk`] from it to each volume it mounts and to `erase`,
/// `partition` and `format`. The filesystem arms never see it, so an arm
/// writes only through the `Disk` it was handed, which on exFAT is behind
/// its disk gate. Since 2026-10-27; until then `Disk` was a unit struct any
/// code in fsd could name and write through (the high review of #361).
pub struct Device(());

impl Device {
    /// The device, to the first caller alone: fsd's `main`. A later claim,
    /// one an arm might write to reach the disk around its own handle,
    /// answers `None`, so it cannot be made to work by accident.
    pub fn claim() -> Option<Device> {
        (!CLAIMED.swap(true, Ordering::Relaxed)).then_some(Device(()))
    }

    /// A handle for one volume or one disk tool.
    pub fn disk(&self) -> Disk {
        Disk(())
    }

    /// The device's capacity in sectors, or `NoDevice` - the probe the
    /// server's auto-mount uses to decide whether mounting is worth
    /// attempting at all.
    pub fn capacity_sectors(&self) -> Result<u64, DiskError> {
        capacity_sectors()
    }

    /// Mark the partition table as rewritten by this server (`erase`,
    /// `partition`), before the first sector is written: the kernel's
    /// boot partition start was read from the table at boot, and after a
    /// rewrite it could name a new, unrelated partition, which would then be
    /// passed over (the medium review of #288). From then on this boot,
    /// [`boot_partition_start`](Self::boot_partition_start) answers `None`.
    pub fn table_rewritten(&self) {
        TABLE_REWRITTEN.store(true, Ordering::Relaxed);
    }

    /// The kernel installed another device in this one's place (fsd's own
    /// `MOUNT` rescan): the old disk's [`table_rewritten`](Self::table_rewritten)
    /// verdict is not the new one's, so a stick the kernel matched as the boot
    /// disk answers its boot partition again.
    pub fn device_replaced(&self) {
        TABLE_REWRITTEN.store(false, Ordering::Relaxed);
    }

    /// The start LBA of the partition the system booted from, when the kernel
    /// matched this device as the disk it is on, else `None`: the firmware
    /// named no partition, or the device is a fallback or a stick taken late
    /// (`BLOCK_BOOT_START`, one-disk step 4). A kernel without the call
    /// answers an error, which is `None` too.
    pub fn boot_partition_start(&self) -> Option<u64> {
        if TABLE_REWRITTEN.load(Ordering::Relaxed) {
            return None;
        }
        match syscall4(syscall_abi::BLOCK_BOOT_START, 0, 0, 0, 0) {
            0 => None,
            code if code >= syscall_abi::FS_ERR_MIN => None,
            start => Some(start),
        }
    }
}

fn capacity_sectors() -> Result<u64, DiskError> {
    match syscall4(syscall_abi::BLOCK_INFO, 0, 0, 0, 0) {
        code if code >= syscall_abi::FS_ERR_MIN => Err(DiskError::NoDevice),
        capacity => Ok(capacity),
    }
}

/// A handle on the device for one holder - all real state (the device, its
/// rings/buffers) lives in the kernel. Exists as a struct so `fat32::Fs` can
/// hold and call it exactly the way it held the old `BlockDevice`. Its field
/// is private and it is not `Clone`: the one way to get one is
/// [`Device::disk`], so code holding a `Disk` and no `Device`, every
/// filesystem arm, cannot make another.
pub struct Disk(());

impl Disk {
    /// The device's capacity in sectors, as [`Device::capacity_sectors`].
    pub fn capacity_sectors(&self) -> Result<u64, DiskError> {
        capacity_sectors()
    }

    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), DiskError> {
        match syscall4(syscall_abi::BLOCK_READ, sector, buf.as_mut_ptr() as u64, 0, 0) {
            0 => Ok(()),
            syscall_abi::BLOCK_ERR_NO_DEVICE => Err(DiskError::NoDevice),
            _ => Err(DiskError::Io),
        }
    }

    pub fn write_sector(&mut self, sector: u64, buf: &[u8; SECTOR_SIZE]) -> Result<(), DiskError> {
        match syscall4(syscall_abi::BLOCK_WRITE, sector, buf.as_ptr() as u64, 0, 0) {
            0 => Ok(()),
            syscall_abi::BLOCK_ERR_NO_DEVICE => Err(DiskError::NoDevice),
            _ => Err(DiskError::Io),
        }
    }
}

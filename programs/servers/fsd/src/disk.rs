//! The filesystem server's disk access: thin safe wrappers over the
//! `BLOCK_*` syscalls, standing in for the kernel's old
//! `block::BlockDevice` (which `fat32.rs` used to own by value). The
//! kernel holds the actual device and only accepts these calls from
//! this task's slot (`syscall_abi::FSD_TASK`) - see `syscall.rs`'s
//! `block_access_allowed`.

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

/// Zero-sized handle - all real state (the device, its rings/buffers)
/// lives in the kernel. Exists as a struct so `fat32::Fs` can hold and
/// call it exactly the way it held the old `BlockDevice`.
pub struct Disk;

impl Disk {
    /// The device's capacity in sectors, or `NoDevice` - the probe the
    /// server's auto-mount uses to decide whether mounting is worth
    /// attempting at all.
    pub fn capacity_sectors(&self) -> Result<u64, DiskError> {
        match syscall4(syscall_abi::BLOCK_INFO, 0, 0, 0, 0) {
            code if code >= syscall_abi::FS_ERR_MIN => Err(DiskError::NoDevice),
            capacity => Ok(capacity),
        }
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

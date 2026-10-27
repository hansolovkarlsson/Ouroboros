//! The filesystem server's disk access: thin safe wrappers over the
//! `BLOCK_*` syscalls, standing in for the kernel's old
//! `block::BlockDevice` (which `fat32.rs` used to own by value). The
//! kernel holds the actual device and only accepts these calls from
//! this task's slot (`syscall_abi::FSD_TASK`) - see `syscall.rs`'s
//! `block_access_allowed`.
//!
//! Every `BLOCK_*` call fsd makes is made here, through [`block_svc`], a
//! private `svc` of this file's own: the crate-wide `syscall4` refuses the
//! `BLOCK_*` numbers, so the one way to the disk is a [`Disk`], and the one
//! way to a `Disk` is the [`Device`] fsd's `main` holds. What this does not
//! stop is an `svc` written out by hand in another file's `asm!`.

use core::arch::asm;
use core::cell::Cell;
use core::sync::atomic::{AtomicBool, Ordering};

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

/// Whether the [`Device`] has been claimed ([`Device::claim`]). fsd is one
/// task, so an atomic is only the spelling of a mutable static, not a
/// concurrency claim.
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// The block device, held once: fsd's `main` claims it at its start and
/// lends a [`Disk`] from it to each volume it mounts and to `erase`,
/// `partition` and `format`. The filesystem arms never see it, so an arm
/// writes only through the `Disk` it was handed, which on exFAT is behind
/// its disk gate. Since 2026-10-27; until then `Disk` was a unit struct any
/// code in fsd could name and write through (the high review of #361).
pub struct Device {
    /// Whether this server has rewritten the partition table this boot
    /// ([`table_rewritten`](Self::table_rewritten)).
    table_rewritten: Cell<bool>,
}

impl Device {
    /// The device, to the first caller alone: fsd's `main`. A later claim,
    /// one an arm might write to reach the disk around its own handle,
    /// answers `None`, so it cannot be made to work by accident.
    pub fn claim() -> Option<Device> {
        (!CLAIMED.swap(true, Ordering::Relaxed)).then_some(Device { table_rewritten: Cell::new(false) })
    }

    /// A handle for one volume or one disk tool.
    pub fn disk(&self) -> Disk {
        Disk(())
    }

    /// Mark the partition table as rewritten by this server (`erase`,
    /// `partition`), before the first sector is written: the kernel's
    /// boot partition start was read from the table at boot, and after a
    /// rewrite it could name a new, unrelated partition, which would then be
    /// passed over (the medium review of #288). From then on this boot,
    /// [`boot_partition_start`](Self::boot_partition_start) answers `None`.
    pub fn table_rewritten(&self) {
        self.table_rewritten.set(true);
    }

    /// The kernel installed another device in this one's place (fsd's own
    /// `MOUNT` rescan): the old disk's [`table_rewritten`](Self::table_rewritten)
    /// verdict is not the new one's, so a stick the kernel matched as the boot
    /// disk answers its boot partition again.
    pub fn device_replaced(&self) {
        self.table_rewritten.set(false);
    }

    /// The start LBA of the partition the system booted from, when the kernel
    /// matched this device as the disk it is on, else `None`: the firmware
    /// named no partition, or the device is a fallback or a stick taken late
    /// (`BLOCK_BOOT_START`, one-disk step 4). A kernel without the call
    /// answers an error, which is `None` too.
    pub fn boot_partition_start(&self) -> Option<u64> {
        if self.table_rewritten.get() {
            return None;
        }
        match block_svc(syscall_abi::BLOCK_BOOT_START, 0, 0) {
            0 => None,
            code if code >= syscall_abi::FS_ERR_MIN => None,
            start => Some(start),
        }
    }
}

/// A handle on the device for one holder - all real state (the device, its
/// rings/buffers) lives in the kernel. Exists as a struct so `fat32::Fs` can
/// hold and call it exactly the way it held the old `BlockDevice`. Its field
/// is private and it is not `Clone`: the one way to get one is
/// [`Device::disk`], so code holding a `Disk` and no `Device`, every
/// filesystem arm, cannot make another. `make test` builds fsd with one
/// made by hand in `exfat.rs` and requires the build to fail
/// (`scripts/check-disk-handle.py`).
pub struct Disk(());

impl Disk {
    /// The device's capacity in sectors, or `NoDevice` - the probe the
    /// server's auto-mount uses to decide whether mounting is worth
    /// attempting at all.
    pub fn capacity_sectors(&self) -> Result<u64, DiskError> {
        match block_svc(syscall_abi::BLOCK_INFO, 0, 0) {
            code if code >= syscall_abi::FS_ERR_MIN => Err(DiskError::NoDevice),
            capacity => Ok(capacity),
        }
    }

    pub fn read_sector(&mut self, sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> Result<(), DiskError> {
        match block_svc(syscall_abi::BLOCK_READ, sector, buf.as_mut_ptr() as u64) {
            0 => Ok(()),
            syscall_abi::BLOCK_ERR_NO_DEVICE => Err(DiskError::NoDevice),
            _ => Err(DiskError::Io),
        }
    }

    pub fn write_sector(&mut self, sector: u64, buf: &[u8; SECTOR_SIZE]) -> Result<(), DiskError> {
        match block_svc(syscall_abi::BLOCK_WRITE, sector, buf.as_ptr() as u64) {
            0 => Ok(()),
            syscall_abi::BLOCK_ERR_NO_DEVICE => Err(DiskError::NoDevice),
            _ => Err(DiskError::Io),
        }
    }
}

/// Whether `number` is one of the `BLOCK_*` calls, which only [`block_svc`]
/// makes: the crate-wide `syscall4` and `syscall5` refuse these.
pub fn is_block_call(number: u64) -> bool {
    matches!(
        number,
        syscall_abi::BLOCK_INFO | syscall_abi::BLOCK_READ | syscall_abi::BLOCK_WRITE | syscall_abi::BLOCK_BOOT_START
    )
}

/// A `BLOCK_*` call, number in x8, arguments in x0 and x1, the answer in x0.
/// Private: the one `svc` in fsd that the kernel answers with the disk.
#[inline(always)]
fn block_svc(number: u64, arg0: u64, arg1: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") 0u64,
            in("x3") 0u64,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

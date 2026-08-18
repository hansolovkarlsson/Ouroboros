//! The block-device abstraction `fat32.rs` sits on - an enum over the
//! concrete drivers, the same idiom `console.rs`'s `Console` already
//! established (an enum, not a trait object: `Box<dyn ...>` would need
//! the boot-services-backed allocator that's invalid after
//! `exit_boot_services`, and there are exactly two variants to
//! dispatch over).
//!
//! Introduced when USB mass storage became the second disk path (see
//! CLAUDE.md's mass-storage milestone): `fat32.rs` was written
//! directly over `virtio_blk::Device`, which was fine while that was
//! the only block driver this kernel had - and wrong the moment real
//! Parallels hardware (which exposes no storage controller virtio or
//! otherwise, only USB) needed the same filesystem over a different
//! transport.

use crate::usb_msd;
use crate::virtio_blk;

/// One error type across every block driver, so `fat32::Error::Io`
/// doesn't need to know which transport failed.
#[derive(Debug)]
pub enum Error {
    Virtio(virtio_blk::Error),
    UsbMsd(usb_msd::Error),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Virtio(e) => write!(f, "virtio-blk: {e}"),
            Error::UsbMsd(e) => write!(f, "usb-msd: {e}"),
        }
    }
}

pub enum BlockDevice {
    Virtio(virtio_blk::Device),
    UsbMsd(usb_msd::Device),
}

impl BlockDevice {
    /// # Safety
    /// Same contract as the underlying driver's `read_sector` (device
    /// initialized, its MMIO/DMA regions mapped under this kernel's
    /// own tables).
    pub unsafe fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> Result<(), Error> {
        match self {
            BlockDevice::Virtio(d) => unsafe { d.read_sector(sector, buf) }.map_err(Error::Virtio),
            BlockDevice::UsbMsd(d) => unsafe { d.read_sector(sector, buf) }.map_err(Error::UsbMsd),
        }
    }

    /// # Safety
    /// Same contract as [`Self::read_sector`].
    pub unsafe fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> Result<(), Error> {
        match self {
            BlockDevice::Virtio(d) => unsafe { d.write_sector(sector, buf) }.map_err(Error::Virtio),
            BlockDevice::UsbMsd(d) => unsafe { d.write_sector(sector, buf) }.map_err(Error::UsbMsd),
        }
    }
}

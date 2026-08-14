//! Minimal polling driver for a PL011 UART at a runtime-supplied base
//! address. The base address is meant to come from devicetree discovery
//! (see `devicetree.rs`); [`QEMU_VIRT_PL011_BASE`] exists only as the
//! fallback for platforms where that discovery doesn't find one.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

/// QEMU's `virt` machine has a PL011 fixed at this MMIO base regardless of
/// devicetree contents, so it's a safe fallback when discovery fails.
pub const QEMU_VIRT_PL011_BASE: usize = 0x0900_0000;

const DR_OFFSET: usize = 0x00;
const FR_OFFSET: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;

pub struct Uart {
    base: usize,
}

impl Uart {
    /// # Safety
    /// `base` must be the MMIO base address of a real PL011 UART, mapped
    /// and accessible at the time of any write.
    pub unsafe fn new(base: usize) -> Self {
        Uart { base }
    }

    fn write_byte(&mut self, byte: u8) {
        unsafe {
            let fr = (self.base + FR_OFFSET) as *const u32;
            while read_volatile(fr) & FR_TXFF != 0 {}

            let dr = (self.base + DR_OFFSET) as *mut u32;
            write_volatile(dr, byte as u32);
        }
    }
}

impl fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}

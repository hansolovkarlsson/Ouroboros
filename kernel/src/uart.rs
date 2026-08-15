//! Minimal polling driver for a PL011 UART at a runtime-supplied base
//! address. The base address must come from actual hardware discovery (see
//! `devicetree.rs`) — there is deliberately no hardcoded fallback address
//! here. There used to be one (QEMU's `virt`-machine PL011 base); it was
//! removed after confirming that blindly writing to it on Parallels, where
//! nothing is mapped there, hard-crashes the VM. No confirmed address means
//! no console, not a guess.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

const DR_OFFSET: usize = 0x00;
const FR_OFFSET: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;
const FR_RXFE: u32 = 1 << 4;

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

    pub(crate) fn write_byte(&mut self, byte: u8) {
        unsafe {
            let fr = (self.base + FR_OFFSET) as *const u32;
            while read_volatile(fr) & FR_TXFF != 0 {}

            let dr = (self.base + DR_OFFSET) as *mut u32;
            write_volatile(dr, byte as u32);
        }
    }

    /// Non-blocking: `None` if the receive FIFO is empty. DR's upper bits
    /// (framing/parity/break/overrun error flags) are deliberately ignored
    /// for now — masked off, not checked — good enough for a first echo
    /// path; a real line discipline would need to surface them.
    pub(crate) fn read_byte(&mut self) -> Option<u8> {
        unsafe {
            let fr = (self.base + FR_OFFSET) as *const u32;
            if read_volatile(fr) & FR_RXFE != 0 {
                return None;
            }

            let dr = (self.base + DR_OFFSET) as *const u32;
            Some(read_volatile(dr) as u8)
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

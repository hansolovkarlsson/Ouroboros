//! Minimal polling driver for the PL011 UART.
//!
//! Hardcoded to QEMU's `virt` machine MMIO base. QEMU's PL011 emulation is
//! already clocked/configured by the time firmware hands off to us, so this
//! only needs to poll the flag register and push bytes — no baud-rate or
//! line-control setup. This address is QEMU-specific and known not to work
//! on Parallels; see CLAUDE.md for the plan there.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

const UART0_BASE: usize = 0x0900_0000;
const DR_OFFSET: usize = 0x00;
const FR_OFFSET: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;

#[derive(Default)]
pub struct Uart;

impl Uart {
    pub fn new() -> Self {
        Uart
    }

    fn write_byte(&mut self, byte: u8) {
        unsafe {
            let fr = (UART0_BASE + FR_OFFSET) as *const u32;
            while read_volatile(fr) & FR_TXFF != 0 {}

            let dr = (UART0_BASE + DR_OFFSET) as *mut u32;
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

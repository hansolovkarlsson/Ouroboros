//! Minimal polling driver for a 16550-compatible UART at a runtime-supplied
//! base address, register stride 4 (the common convention for a 16550 core
//! wired up as MMIO on an ARM platform, as opposed to its native x86 1-byte
//! I/O port stride) — used for a PCI-discovered console (`pci.rs`), which
//! is a completely different register layout from the PL011 in `uart.rs`.
//! PCI class 0x07 subclass 0x00 ("Simple Communication Controller / Serial
//! controller") is specifically the 16550 family; it is not how a PL011
//! would ever be identified over PCI.
//!
//! Register stride is a real unknown here, not a confirmed fact like
//! `uart.rs`'s PL011 offsets: nothing has exercised this against a real
//! device yet. If characters don't show up when this is actually tested,
//! stride is the first thing to question.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

const STRIDE: usize = 4;
const THR_OFFSET: usize = 0;
const RBR_OFFSET: usize = 0; // same address as THR - direction (read vs write) disambiguates
const LSR_OFFSET: usize = 5 * STRIDE;
const LSR_THRE: u8 = 1 << 5;
const LSR_DR: u8 = 1 << 0;

pub struct Uart16550 {
    base: usize,
}

impl Uart16550 {
    /// # Safety
    /// `base` must be the MMIO base address of a real 16550-compatible
    /// UART, mapped and accessible at the time of any write.
    pub unsafe fn new(base: usize) -> Self {
        Uart16550 { base }
    }

    pub(crate) fn write_byte(&mut self, byte: u8) {
        unsafe {
            let lsr = (self.base + LSR_OFFSET) as *const u8;
            while read_volatile(lsr) & LSR_THRE == 0 {}

            let thr = (self.base + THR_OFFSET) as *mut u8;
            write_volatile(thr, byte);
        }
    }

    /// Non-blocking: `None` if LSR's Data Ready bit isn't set. Untested
    /// against real hardware, same caveat as this whole module (see the
    /// module doc comment) — stride/offsets are the first thing to question
    /// if this doesn't behave once a PCI 16550 console is actually live.
    pub(crate) fn read_byte(&mut self) -> Option<u8> {
        unsafe {
            let lsr = (self.base + LSR_OFFSET) as *const u8;
            if read_volatile(lsr) & LSR_DR == 0 {
                return None;
            }

            let rbr = (self.base + RBR_OFFSET) as *const u8;
            Some(read_volatile(rbr))
        }
    }
}

impl fmt::Write for Uart16550 {
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

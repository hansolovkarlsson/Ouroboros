#![no_main]
#![no_std]

extern crate alloc;

mod devicetree;
mod uart;

use core::fmt::Write;
use uefi::boot;
use uefi::prelude::*;

use uart::Uart;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();

    log::info!("Ouroboros kernel: UEFI stage alive");

    // Must happen before exit_boot_services: the devicetree pointer lives in
    // the UEFI configuration table, which is only reachable via boot services.
    let dtb = devicetree::find_dtb();

    // SAFETY: no boot-services protocol references (console, allocator, or
    // otherwise) are held past this call. Nothing below this point may use
    // log::*, alloc, or UEFI protocols — only the raw PL011 MMIO in `uart`
    // and the devicetree blob itself (plain memory, not a boot service).
    let _memory_map = unsafe { boot::exit_boot_services(None) };

    let discovery = unsafe { devicetree::discover_pl011(dtb) };
    let base = discovery.unwrap_or(uart::QEMU_VIRT_PL011_BASE);

    // SAFETY: `base` is either a PL011 address read out of the platform's
    // own devicetree, or the known-good QEMU `virt` fallback.
    let mut uart = unsafe { Uart::new(base) };
    match discovery {
        Ok(base) => {
            let _ = writeln!(uart, "Ouroboros kernel: console @ {base:#x} (via devicetree)");
        }
        Err(reason) => {
            let _ = writeln!(
                uart,
                "Ouroboros kernel: devicetree console discovery failed ({reason:?}), \
                 falling back to QEMU virt PL011 @ {base:#x}"
            );
        }
    }

    halt()
}

/// Parks the core forever instead of returning to firmware. `wfe` is a
/// low-power spin (wait-for-event) rather than a busy loop.
fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

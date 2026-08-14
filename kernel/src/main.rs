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

    // Deliberately still before exit_boot_services, even though parsing the
    // blob itself doesn't need boot services: logging the result here goes
    // through the UEFI console, which works on any platform, so we get a
    // trustworthy diagnostic before ever touching the raw MMIO UART below —
    // which, on a platform we haven't validated, might not be mapped to
    // anything at all and fault instead of printing.
    let discovery = unsafe { devicetree::discover_pl011(dtb) };
    let base = discovery.unwrap_or(uart::QEMU_VIRT_PL011_BASE);
    match discovery {
        Ok(base) => log::info!("Ouroboros kernel: devicetree console @ {base:#x}"),
        Err(reason) => log::warn!(
            "Ouroboros kernel: devicetree console discovery failed ({reason:?}), \
             will try QEMU virt PL011 fallback @ {base:#x}"
        ),
    }

    // SAFETY: no boot-services protocol references (console, allocator, or
    // otherwise) are held past this call. Nothing below this point may use
    // log::*, alloc, or UEFI protocols — only the raw PL011 MMIO in `uart`.
    let _memory_map = unsafe { boot::exit_boot_services(None) };

    // SAFETY: `base` is either a PL011 address read out of the platform's
    // own devicetree, or the known-good QEMU `virt` fallback. Neither is
    // guaranteed valid on an untested platform — see the log line above,
    // which was captured before this line in case this one faults.
    let mut uart = unsafe { Uart::new(base) };
    let _ = writeln!(uart, "Ouroboros kernel: boot services exited, console live");

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

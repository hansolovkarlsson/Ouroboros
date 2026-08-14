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
    // trustworthy diagnostic before ever touching raw MMIO — which, without
    // a confirmed address, might not be mapped to anything at all and fault
    // instead of printing. Confirmed the hard way: writing to the QEMU virt
    // PL011 address unconditionally, as a "fallback" whenever discovery
    // failed, hard-crashed real Parallels hardware where that address isn't
    // mapped. So there is no fallback anymore — no confirmed address means
    // no post-exit console, full stop, until real hardware discovery (ACPI
    // SPCR is the likely next candidate, since neither QEMU's nor Parallels'
    // firmware publishes a devicetree) gives us one to trust.
    let discovery = unsafe { devicetree::discover_pl011(dtb) };
    match discovery {
        Ok(base) => log::info!("Ouroboros kernel: devicetree console @ {base:#x}"),
        Err(reason) => {
            log::warn!("Ouroboros kernel: devicetree console discovery failed ({reason:?})")
        }
    }

    // SAFETY: no boot-services protocol references (console, allocator, or
    // otherwise) are held past this call. Nothing below this point may use
    // log::*, alloc, or UEFI protocols — only the raw PL011 MMIO in `uart`,
    // and only when `discovery` gave us an address to trust.
    let _memory_map = unsafe { boot::exit_boot_services(None) };

    if let Ok(base) = discovery {
        // SAFETY: `base` came from the platform's own devicetree.
        let mut uart = unsafe { Uart::new(base) };
        let _ = writeln!(uart, "Ouroboros kernel: boot services exited, console live");
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

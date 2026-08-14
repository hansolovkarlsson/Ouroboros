#![no_main]
#![no_std]

extern crate alloc;

mod console;
mod devicetree;
mod exceptions;
mod uart;

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

    // First thing after exit, before anything else gets a chance to fault:
    // a bad access is still possible (e.g. the UART write below, if
    // `discovery` ever resolves an address that isn't actually valid on
    // some untested platform), but it now reports through the exception
    // handler and halts, instead of taking the whole VM down the way the
    // last untested address did on Parallels.
    exceptions::install();

    if let Ok(base) = discovery {
        // SAFETY: `base` came from the platform's own devicetree.
        let uart = unsafe { Uart::new(base) };
        console::install(uart);
        console::println!("Ouroboros kernel: boot services exited, console live");
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

#![no_main]
#![no_std]

extern crate alloc;

mod acpi;
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

    // Must happen before exit_boot_services: both the devicetree and ACPI
    // RSDP pointers live in the UEFI configuration table, only reachable
    // via boot services.
    let dtb = devicetree::find_dtb();
    let rsdp = acpi::find_rsdp();

    // Deliberately still before exit_boot_services, even though parsing
    // either blob doesn't itself need boot services: logging the result
    // here goes through the UEFI console, which works on any platform, so
    // we get a trustworthy diagnostic before ever touching raw MMIO —
    // which, without a confirmed address, might not be mapped to anything
    // at all and fault instead of printing. Confirmed the hard way: writing
    // to a hardcoded "fallback" address whenever discovery failed
    // hard-crashed real Parallels hardware where that address wasn't
    // mapped. So there is no fallback anymore — no confirmed address means
    // no post-exit console, full stop.
    //
    // Devicetree tried first, ACPI/SPCR as the fallback mechanism (not a
    // fallback *address* — a different, legitimate discovery method): both
    // QEMU's and Parallels' firmware are confirmed ACPI-oriented and never
    // publish a devicetree, so in practice this always falls through to
    // SPCR today. Devicetree is kept first in case it's ever useful on
    // other hardware.
    let discovery = match unsafe { devicetree::discover_pl011(dtb) } {
        Ok(base) => Ok((base, "devicetree")),
        Err(dt_err) => {
            log::warn!("Ouroboros kernel: devicetree console discovery failed ({dt_err:?})");
            unsafe { acpi::discover_pl011(rsdp) }
                .map(|base| (base, "ACPI SPCR"))
                .map_err(|acpi_err| {
                    log::warn!("Ouroboros kernel: ACPI SPCR console discovery failed ({acpi_err:?})");
                })
        }
    };
    if let Ok((base, source)) = discovery {
        log::info!("Ouroboros kernel: console @ {base:#x} (via {source})");
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
    // handler and halts, instead of taking the whole VM down the way an
    // untested address once did on Parallels.
    exceptions::install();

    if let Ok((base, _source)) = discovery {
        // SAFETY: `base` came from the platform's own devicetree or ACPI
        // tables.
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

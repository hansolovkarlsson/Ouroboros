//! Machine power control: shut the machine down (power off) or halt it.
//!
//! Powering off is a privileged platform operation - it goes through **PSCI**
//! (the ARM Power State Coordination Interface), an `hvc`/`smc` firmware call
//! only EL1 may make. Which conduit (`hvc` vs `smc`) the platform expects is
//! read from ACPI's FADT at boot (`discover_conduit`) and stashed here, so the
//! `POWER` syscall (which runs at EL1 in `syscall.rs`) can issue
//! `PSCI SYSTEM_OFF` without re-parsing anything. If PSCI isn't available (no
//! FADT, or not PSCI-compliant), power-off falls back to a plain CPU halt -
//! the machine stops, even if it can't cut its own power.
//!
//! `halt` is unconditional and needs no firmware: mask interrupts and park the
//! core in `wfi` forever, so nothing (not even the timer tick) resumes it - the
//! whole machine stops.

use crate::acpi;
use core::arch::asm;
use core::sync::atomic::{AtomicU8, Ordering};

/// PSCI `SYSTEM_OFF` function ID (PSCI 0.2+). Powers the machine off; does not
/// return on success.
const PSCI_SYSTEM_OFF: u32 = 0x8400_0008;

/// The FADT `ARM_BOOT_ARCH` field lives at this byte offset from the table
/// start (ACPI spec: 1 byte `RESET_VALUE` at 128, then this u16 at 129).
const FADT_ARM_BOOT_ARCH_OFFSET: usize = 129;
/// `ARM_BOOT_ARCH` bit 0: the platform implements PSCI.
const ARM_PSCI_COMPLIANT: u16 = 1 << 0;
/// `ARM_BOOT_ARCH` bit 1: use `hvc` (not `smc`) as the PSCI conduit.
const ARM_PSCI_USE_HVC: u16 = 1 << 1;

/// The PSCI conduit stashed for the syscall path: 0 = none/unknown (fall back
/// to a halt), 1 = `hvc`, 2 = `smc`. An `AtomicU8` rather than an
/// `UnsafeCell` static so the write at boot and the read at syscall time need
/// no `unsafe` and no lock (a single relaxed store/load of a scalar).
const CONDUIT_NONE: u8 = 0;
const CONDUIT_HVC: u8 = 1;
const CONDUIT_SMC: u8 = 2;

static CONDUIT: AtomicU8 = AtomicU8::new(CONDUIT_NONE);

/// Discover the PSCI conduit from ACPI's FADT (signature `FACP`) and stash it
/// for the syscall path. Call once at boot, before `exit_boot_services` (same
/// window as `madt::discover` - it reads the same ACPI tables). Safe to skip:
/// a missing/!PSCI FADT just leaves the conduit `NONE`, and power-off halts.
///
/// # Safety
/// `rsdp` must be the real RSDP pointer from the UEFI config table (or `None`).
pub unsafe fn discover_conduit(rsdp: Option<*const u8>) {
    let addr = match unsafe { acpi::find_table(rsdp, b"FACP") } {
        Ok(addr) => addr,
        Err(_) => return, // no FADT -> leave CONDUIT = NONE
    };
    // ARM_BOOT_ARCH is a u16 at a fixed offset into the FADT.
    let boot_arch =
        unsafe { core::ptr::read_unaligned(addr.add(FADT_ARM_BOOT_ARCH_OFFSET).cast::<u16>()) };
    if boot_arch & ARM_PSCI_COMPLIANT == 0 {
        return; // not PSCI-compliant -> NONE
    }
    let conduit = if boot_arch & ARM_PSCI_USE_HVC != 0 {
        CONDUIT_HVC
    } else {
        CONDUIT_SMC
    };
    CONDUIT.store(conduit, Ordering::Relaxed);
}

/// Power the machine off via `PSCI SYSTEM_OFF`, or halt if PSCI is
/// unavailable. Never returns.
pub fn power_off() -> ! {
    crate::console::println!("Ouroboros kernel: powering off");
    match CONDUIT.load(Ordering::Relaxed) {
        CONDUIT_HVC => unsafe {
            asm!("hvc #0", in("x0") PSCI_SYSTEM_OFF as u64, options(nomem, nostack));
        },
        CONDUIT_SMC => unsafe {
            asm!("smc #0", in("x0") PSCI_SYSTEM_OFF as u64, options(nomem, nostack));
        },
        _ => {}
    }
    // PSCI SYSTEM_OFF doesn't return; if we're still here, PSCI was absent or
    // refused - fall back to a halt so the machine at least stops.
    crate::console::println!("Ouroboros kernel: power off unavailable - halting");
    halt()
}

/// Halt the machine: mask all interrupts and park the core in `wfi` forever,
/// so nothing (not even the timer tick) can resume it. Never returns.
pub fn halt() -> ! {
    crate::console::println!("Ouroboros kernel: system halted");
    unsafe {
        // DAIFSet: mask Debug/SError/IRQ/FIQ so the timer tick can't wake us.
        asm!("msr daifset, #0xf", options(nomem, nostack, preserves_flags));
    }
    loop {
        unsafe {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }
    }
}

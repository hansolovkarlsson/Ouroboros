//! The exception level the firmware hands the kernel off at.
//!
//! This kernel assumes EL1 everywhere after `exit_boot_services`: the vector
//! base, the translation tables, the timer and the first `eret` into task 0
//! are all `_EL1` state. QEMU and Parallels hand off at EL1. The Raspberry
//! Pi's firmware (pftf over TF-A) hands off at **EL2**, found on 2026-10-01
//! by the early fault reporter's first dump on a Pi 4 (`SPSR` in EL2h, the
//! fault through the firmware's vectors after `exceptions::install()`), and
//! reproduced on QEMU with `-machine virt,virtualization=on` (`make
//! run-el2`). At EL2 every one of those writes lands in a register the
//! running level does not use, and the boot keeps running on the firmware's
//! EL2 tables and vectors while its log says otherwise.
//!
//! [`current_el`] turns that into a kernel fact the log states on every
//! boot. The drop to EL1 is `docs/roadmap/roadmap-el1-drop.md`.

use core::arch::asm;

/// The exception level this code runs at, 0 to 3, from `CurrentEL`
/// (bits 3:2). Readable at EL1 and above.
pub fn current_el() -> u64 {
    let el: u64;
    unsafe { asm!("mrs {0}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags)) };
    (el >> 2) & 0b11
}

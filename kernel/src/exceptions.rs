//! Minimal AArch64 exception vectors.
//!
//! Without these, any bad memory access anywhere in the kernel is an
//! unrecoverable platform-level fault with no diagnostic — confirmed
//! directly: writing to an unmapped MMIO address on Parallels didn't just
//! crash the kernel, it crashed the whole VM, reported by Parallels as a
//! guest failure with no indication of what happened. `install()` points
//! VBAR_EL1 at a real vector table so the same class of mistake instead
//! reports what it can (vector taken, ESR/FAR/ELR_EL1) through the global
//! console — if one exists yet — and then halts, rather than running off
//! into whatever undefined behavior an unconfigured VBAR_EL1 produces.
//!
//! Assumes EL1: this kernel has only ever been observed running at EL1
//! under both QEMU and Parallels (typical for a UEFI OS loader; EL2 is the
//! hypervisor's own level, not the guest's). Not verified against any
//! platform that hands off at a different EL.
//!
//! Deliberately not installed until after `exit_boot_services` — firmware
//! has its own VBAR_EL1 that its boot-services internals may depend on;
//! clobbering it while boot services are still active would be touching
//! state we don't own yet.

use core::arch::global_asm;
use core::ffi::c_void;

use crate::console;
use crate::halt;

global_asm!(
    r#"
.text
.balign 0x800
.global exception_vector_table
exception_vector_table:

.balign 0x80
mov x3, #0
b   1f
.balign 0x80
mov x3, #1
b   1f
.balign 0x80
mov x3, #2
b   1f
.balign 0x80
mov x3, #3
b   1f
.balign 0x80
mov x3, #4
b   1f
.balign 0x80
mov x3, #5
b   1f
.balign 0x80
mov x3, #6
b   1f
.balign 0x80
mov x3, #7
b   1f
.balign 0x80
mov x3, #8
b   1f
.balign 0x80
mov x3, #9
b   1f
.balign 0x80
mov x3, #10
b   1f
.balign 0x80
mov x3, #11
b   1f
.balign 0x80
mov x3, #12
b   1f
.balign 0x80
mov x3, #13
b   1f
.balign 0x80
mov x3, #14
b   1f
.balign 0x80
mov x3, #15
b   1f

1:
mrs x0, esr_el1
mrs x1, far_el1
mrs x2, elr_el1
b   {rust_handler}
"#,
    rust_handler = sym rust_exception_handler,
);

unsafe extern "C" {
    /// Opaque - only its address (the table itself) is used.
    static exception_vector_table: c_void;
}

/// Points VBAR_EL1 at [`exception_vector_table`]. Must be called after
/// `exit_boot_services`, before anything that could plausibly fault.
pub fn install() {
    unsafe {
        let table_addr = &raw const exception_vector_table as u64;
        core::arch::asm!(
            "msr vbar_el1, {0}",
            "isb",
            in(reg) table_addr,
            options(nostack),
        );
    }
}

/// The vector index, in AArch64's fixed 16-entry table order: 4 exception
/// classes (Synchronous, IRQ, FIQ, SError) x 4 source groups (current EL
/// w/ SP_EL0, current EL w/ SP_ELx, lower EL AArch64, lower EL AArch32).
extern "C" fn rust_exception_handler(esr: u64, far: u64, elr: u64, vector: u64) -> ! {
    console::println!(
        "Ouroboros kernel: EXCEPTION vector={vector} esr_el1={esr:#x} far_el1={far:#x} elr_el1={elr:#x}"
    );
    halt()
}

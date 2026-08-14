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
//!
//! ## The IRQ vector is different from the other 15
//!
//! Every vector except IRQ-at-EL1h (index 5, the only source of interrupts
//! so far — see `gic.rs`/`timer.rs`) shares one path: capture ESR/FAR/ELR,
//! report, halt. That path never returns, so it never needs to preserve
//! anything. IRQ is the first exception this kernel needs to *resume from*
//! — the interrupted code (currently just `halt()`'s `wfe` loop) has to
//! keep running afterward — so its vector slot does a full general-purpose
//! register + ELR_EL1/SPSR_EL1 save, calls into Rust normally (`bl`, not a
//! diverging `b`), restores everything, and `eret`s back.
//!
//! Floating-point/SIMD (Q0-Q31) registers are deliberately *not* saved
//! here. Nothing running today uses them, and the interrupted context is
//! only ever `halt()`'s trivial spin loop or `syscall.rs`'s EL0 demo task
//! — this stops being safe the moment there's real interruptible work with
//! FP/SIMD state, which is a problem for whenever actual task switching
//! exists, not this milestone.
//!
//! ## Two vector groups now use the resumable path, not one
//!
//! Once `syscall.rs` can drop to EL0, a timer IRQ firing *while EL0 code is
//! running* lands in a different vector slot than one firing at EL1 — the
//! table is grouped by [current EL w/ SP_EL0][current EL w/ SP_ELx][lower
//! EL AArch64][lower EL AArch32], so "IRQ at EL1h" (slot 5, our own
//! kernel code, `halt()`'s `wfe` loop) and "IRQ from lower EL AArch64"
//! (slot 9, EL0 code) are different entries entirely. Both share the exact
//! same resumable trampoline (`2:` below) — GIC ack/EOI and the timer
//! rearm don't care which EL got interrupted, and `eret` resumes the right
//! one automatically from the saved SPSR_EL1 regardless. Slot 8
//! (Synchronous, lower EL AArch64) is where EL0's `svc` lands — but that
//! same slot is also where an EL0 *fault* would land (bad memory access,
//! etc.), so it isn't unconditionally treated as a syscall: it checks
//! ESR_EL1's EC field first and only takes the resumable syscall path
//! (`3:`) for EC=0x15 (SVC64), falling through to the ordinary diverging
//! report-and-halt path otherwise.

use core::arch::global_asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::console;
use crate::gic;
use crate::halt;
use crate::syscall;
use crate::timer;

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
// slot 5: IRQ, Current EL with SP_ELx - the resumable path, see below.
b   2f
.balign 0x80
mov x3, #6
b   1f
.balign 0x80
mov x3, #7
b   1f
.balign 0x80
// slot 8: Synchronous, lower EL AArch64 - EL0's svc lands here, but so
// would an EL0 fault. Check EC before committing to the resumable path.
mrs x9, esr_el1
lsr x9, x9, #26
cmp x9, #0x15
b.eq 3f
mov x3, #8
b   1f
.balign 0x80
// slot 9: IRQ, lower EL AArch64 - a tick firing while EL0 runs. Same
// resumable path as slot 5; see module doc comment.
b   2f
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

2:
sub sp, sp, #272
stp x0, x1, [sp, #0]
stp x2, x3, [sp, #16]
stp x4, x5, [sp, #32]
stp x6, x7, [sp, #48]
stp x8, x9, [sp, #64]
stp x10, x11, [sp, #80]
stp x12, x13, [sp, #96]
stp x14, x15, [sp, #112]
stp x16, x17, [sp, #128]
stp x18, x19, [sp, #144]
stp x20, x21, [sp, #160]
stp x22, x23, [sp, #176]
stp x24, x25, [sp, #192]
stp x26, x27, [sp, #208]
stp x28, x29, [sp, #224]
str x30, [sp, #240]
mrs x0, elr_el1
mrs x1, spsr_el1
stp x0, x1, [sp, #248]
bl  {rust_irq_handler}
ldp x0, x1, [sp, #248]
msr elr_el1, x0
msr spsr_el1, x1
ldp x0, x1, [sp, #0]
ldp x2, x3, [sp, #16]
ldp x4, x5, [sp, #32]
ldp x6, x7, [sp, #48]
ldp x8, x9, [sp, #64]
ldp x10, x11, [sp, #80]
ldp x12, x13, [sp, #96]
ldp x14, x15, [sp, #112]
ldp x16, x17, [sp, #128]
ldp x18, x19, [sp, #144]
ldp x20, x21, [sp, #160]
ldp x22, x23, [sp, #176]
ldp x24, x25, [sp, #192]
ldp x26, x27, [sp, #208]
ldp x28, x29, [sp, #224]
ldr x30, [sp, #240]
add sp, sp, #272
eret

3:
sub sp, sp, #272
stp x0, x1, [sp, #0]
stp x2, x3, [sp, #16]
stp x4, x5, [sp, #32]
stp x6, x7, [sp, #48]
stp x8, x9, [sp, #64]
stp x10, x11, [sp, #80]
stp x12, x13, [sp, #96]
stp x14, x15, [sp, #112]
stp x16, x17, [sp, #128]
stp x18, x19, [sp, #144]
stp x20, x21, [sp, #160]
stp x22, x23, [sp, #176]
stp x24, x25, [sp, #192]
stp x26, x27, [sp, #208]
stp x28, x29, [sp, #224]
str x30, [sp, #240]
// ELR/SPSR saved via x2/x3 (not x0/x1): x0 (syscall arg0) and x8
// (syscall number) must survive intact to become the dispatcher's
// call arguments below.
mrs x2, elr_el1
mrs x3, spsr_el1
stp x2, x3, [sp, #248]
mov x1, x0   // arg0 (was x0) -> dispatch()'s 2nd argument
mov x0, x8   // syscall number (was x8) -> dispatch()'s 1st argument
bl  {rust_syscall_handler}
str x0, [sp, #0]   // dispatch()'s return value becomes EL0's new x0
ldp x2, x3, [sp, #248]
msr elr_el1, x2
msr spsr_el1, x3
ldp x0, x1, [sp, #0]
ldp x2, x3, [sp, #16]
ldp x4, x5, [sp, #32]
ldp x6, x7, [sp, #48]
ldp x8, x9, [sp, #64]
ldp x10, x11, [sp, #80]
ldp x12, x13, [sp, #96]
ldp x14, x15, [sp, #112]
ldp x16, x17, [sp, #128]
ldp x18, x19, [sp, #144]
ldp x20, x21, [sp, #160]
ldp x22, x23, [sp, #176]
ldp x24, x25, [sp, #192]
ldp x26, x27, [sp, #208]
ldp x28, x29, [sp, #224]
ldr x30, [sp, #240]
add sp, sp, #272
eret
"#,
    rust_handler = sym rust_exception_handler,
    rust_irq_handler = sym rust_irq_handler,
    rust_syscall_handler = sym syscall::dispatch,
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

static TICKS: AtomicU64 = AtomicU64::new(0);

const SPURIOUS_INTID: u32 = 1023;
const TICK_INTERVAL_MS: u64 = 1000;

/// Runs on every IRQ, called from the vector table's slot 5 trampoline.
/// Must return normally (the trampoline `eret`s back to whatever was
/// interrupted) — never halt or diverge from here.
extern "C" fn rust_irq_handler() {
    let intid = unsafe { gic::acknowledge() };

    if intid == timer::INTID {
        let ticks = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
        console::println!("Ouroboros kernel: tick {ticks}");
        timer::arm(TICK_INTERVAL_MS);
    }

    if intid != SPURIOUS_INTID {
        unsafe { gic::end_of_interrupt(intid) };
    }
}

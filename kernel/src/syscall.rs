//! The syscall boundary: dropping to EL0 ("user" execution) and the
//! EL0->EL1 trap path (`svc`) that lets it ask the kernel to do something
//! on its behalf. First steps only - see the limitations below before
//! treating any of this as a real security boundary.
//!
//! **No memory isolation yet.** `enter` drops to EL0 running code that
//! lives in this same kernel binary, in the same RAM block `mmu.rs` maps
//! with EL0 read/write/execute access (see its `normal_block` comment) -
//! there's no separate user region, no per-process page tables, no W^X.
//! The boundary being demonstrated here is the EL0/EL1 *privilege*
//! transition and the syscall dispatch mechanism, not memory protection.
//! Device MMIO (`mmu.rs`'s device block) stays EL1-only throughout, so
//! EL0 genuinely cannot touch hardware directly - only ask the kernel to,
//! via a syscall - which is the one piece of real isolation this milestone
//! does establish.
//!
//! Calling convention (chosen to match Linux's, a reasonable default for a
//! "POSIX-ish" OS per this project's stated goals, not because anything
//! here is Linux-ABI-compatible): syscall number in x8, first argument in
//! x0, return value in x0.

use core::arch::asm;
use core::cell::UnsafeCell;

use crate::console;

const USER_STACK_SIZE: usize = 16 * 1024;

#[repr(align(16))]
struct UserStack(UnsafeCell<[u8; USER_STACK_SIZE]>);

// SAFETY: single-core; this is EL0's only stack and is set up once, before
// `enter` ever hands control to EL0.
unsafe impl Sync for UserStack {}

static USER_STACK: UserStack = UserStack(UnsafeCell::new([0; USER_STACK_SIZE]));

/// Drops from EL1 to EL0 and starts executing `entry` there, with its own
/// stack (SP_EL0, separate from the kernel's SP_EL1). Never returns to its
/// caller — the only way back to EL1 is a trap (syscall or fault), handled
/// entirely through `exceptions.rs`'s vector table from here on.
///
/// # Safety
/// The memory `entry` executes from must actually be EL0-accessible and
/// executable under the current translation tables (true for anything in
/// `mmu.rs`'s RAM block, including this kernel's own code - see the module
/// doc comment on why that's currently true of everything in RAM, not just
/// `entry`).
pub unsafe fn enter(entry: extern "C" fn() -> !) -> ! {
    let stack_top = unsafe { (USER_STACK.0.get() as *mut u8).add(USER_STACK_SIZE) } as u64;
    unsafe {
        asm!(
            "msr sp_el0, {stack}",
            "msr elr_el1, {entry}",
            "msr spsr_el1, {spsr}",
            "eret",
            stack = in(reg) stack_top,
            entry = in(reg) entry as usize as u64,
            // SPSR_EL1 = 0: M[3:0]=0000 selects EL0t (the only mode EL0
            // has), DAIF all clear (every exception class unmasked - the
            // timer tick must still be able to preempt EL0), NZCV cleared.
            spsr = in(reg) 0u64,
            options(noreturn),
        );
    }
}

/// A minimal EL0 test payload: makes one syscall, then idles forever.
/// Proves the boundary works in both directions - the syscall round-trip
/// itself, and (via the idle loop running for a while afterward) that the
/// timer tick still correctly preempts and resumes EL0 code, not just
/// EL1's `halt()` loop, which is all `exceptions.rs`'s IRQ path had been
/// verified against before this.
pub extern "C" fn demo_task() -> ! {
    unsafe {
        asm!(
            "mov x8, #0",  // syscall 0
            "mov x0, #42", // arg0
            "svc #0",
            out("x0") _,
            out("x8") _,
            options(nostack),
        );
    }
    loop {
        unsafe {
            asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

/// Called from the exception vector's SVC trampoline (`exceptions.rs`) with
/// the syscall number (from x8) and first argument (from x0), running at
/// EL1 with the kernel's own stack and every privilege EL0 lacks - the
/// entire reason this indirection exists. Its return value becomes EL0's
/// new x0 after `eret`.
pub extern "C" fn dispatch(number: u64, arg0: u64) -> u64 {
    match number {
        0 => {
            console::println!("Ouroboros kernel: syscall from EL0 (number={number}, arg0={arg0:#x})");
            0
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

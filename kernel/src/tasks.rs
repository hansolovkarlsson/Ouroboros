//! Two EL0 tasks, alternated by the timer tick — real preemptive
//! switching, not just resuming whatever the tick happened to interrupt
//! (all `exceptions.rs`'s IRQ path could do before this).
//!
//! Reuses `syscall.rs`'s EL0 isolation approach directly: one dedicated
//! 8KB region (`EL0_REGION`), the largest size/alignment that doesn't
//! crash `rustc` on this target (see `syscall.rs`'s module doc comment for
//! the full story). Rather than adding a second 8KB region — which would
//! need `mmu.rs` to isolate a *second* L2 slot, doubling that complexity —
//! this splits the one region into two 4KB halves, one task each. 4KB is
//! also the finest granularity `mmu.rs`'s page tables support (L3), so
//! this is already as tight as the isolation gets without going smaller
//! than a full page, which the architecture doesn't allow anyway.
//!
//! Each task is a tiny hand-written loop (`global_asm!`, two nearly
//! identical blocks differing only in a hardcoded task ID): report in via
//! a syscall, then `wfe`. There's no cooperative yielding — the *only*
//! thing that ever moves execution from one task to the other is the timer
//! tick catching it mid-`wfe` and swapping its saved [`Context`] for the
//! other task's. That swap is the entire scheduler: strict round-robin
//! between exactly two tasks, no priorities, no blocking, no queue.
//!
//! FP/SIMD state still isn't part of a task's [`Context`] (see
//! `exceptions.rs`'s module doc comment) — fine for these tasks, since
//! their hand-written code never touches it, but a real limitation for
//! whatever runs here next.

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::exceptions::Context;

pub const NUM_TASKS: usize = 2;

// 8KB: the largest alignment/size that doesn't crash rustc for this target
// (see syscall.rs's module doc comment) - not chosen for any other reason.
// Split evenly: one 4KB slot per task, code copied to the bottom of each,
// stack growing down from the top.
const EL0_REGION_SIZE: usize = 0x2000;
const TASK_SLOT_SIZE: u64 = 0x1000;

#[repr(align(0x2000))]
struct El0Region(UnsafeCell<[u8; EL0_REGION_SIZE]>);

// SAFETY: single-core; written once by `init` before either task ever
// runs, and only EL0 (per-task, isolated to its own 4KB slot by `mmu.rs`)
// touches it after that.
unsafe impl Sync for El0Region {}

static EL0_REGION: El0Region = El0Region(UnsafeCell::new([0; EL0_REGION_SIZE]));

// Each task's machine code, at its ordinary (small, unpadded) link
// address - `init` copies each into its own 4KB slot in EL0_REGION rather
// than executing it here directly, since here isn't EL0-accessible. The
// two blocks are identical but for the hardcoded task ID (x0 on syscall
// 2) - not worth a runtime patching scheme for two constants.
global_asm!(
    r#"
.text
.global el0_task0_template
el0_task0_template:
mov x8, #2    // syscall 2: report
mov x0, #0    // task id 0
svc #0
1:
wfe
mov x8, #2
mov x0, #0
svc #0
b 1b
.global el0_task0_template_end
el0_task0_template_end:

.global el0_task1_template
el0_task1_template:
mov x8, #2    // syscall 2: report
mov x0, #1    // task id 1
svc #0
1:
wfe
mov x8, #2
mov x0, #1
svc #0
b 1b
.global el0_task1_template_end
el0_task1_template_end:
"#
);

unsafe extern "C" {
    /// Opaque - only used for their addresses, to find and size each
    /// template to copy.
    static el0_task0_template: c_void;
    static el0_task0_template_end: c_void;
    static el0_task1_template: c_void;
    static el0_task1_template_end: c_void;
}

struct TaskSlot(UnsafeCell<Context>);

// SAFETY: single-core; only ever touched from EL1 with IRQs masked
// (`init`, before either task runs) or from within the IRQ trampoline
// itself (`on_tick`, which by construction can't run re-entrantly - taking
// an exception masks further IRQs until the next `eret`).
unsafe impl Sync for TaskSlot {}

static TASKS: [TaskSlot; NUM_TASKS] =
    [TaskSlot(UnsafeCell::new(Context::zeroed())), TaskSlot(UnsafeCell::new(Context::zeroed()))];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// The isolated EL0 region's (start, size) — `mmu.rs` uses this to decide
/// which pages get EL0 access. Both tasks live inside this one region;
/// `mmu.rs` doesn't need to know there are two of them.
pub fn el0_region() -> (u64, u64) {
    (EL0_REGION.0.get() as u64, EL0_REGION_SIZE as u64)
}

/// Copies both tasks' code into their own 4KB slot in `EL0_REGION`, builds
/// each task's initial [`Context`] (entry at the slot's start, stack at
/// the slot's top, `SPSR_EL1` selecting EL0t with everything unmasked),
/// and disables the EL0 `wfe`/`wfi` trap both tasks' idle loops rely on.
/// Does not itself start anything — see [`start`].
///
/// # Safety
/// Must run after `mmu.rs` has mapped `el0_region()` EL0-accessible, and
/// before either task could possibly run.
pub unsafe fn init() {
    let region_start = EL0_REGION.0.get() as *mut u8;

    let templates: [(*const u8, *const u8); NUM_TASKS] = [
        (&raw const el0_task0_template as *const u8, &raw const el0_task0_template_end as *const u8),
        (&raw const el0_task1_template as *const u8, &raw const el0_task1_template_end as *const u8),
    ];

    for (i, (start, end)) in templates.into_iter().enumerate() {
        let slot_start = unsafe { region_start.add(i * TASK_SLOT_SIZE as usize) };
        let len = unsafe { end.offset_from(start) } as usize;
        unsafe { core::ptr::copy_nonoverlapping(start, slot_start, len) };

        // Standard ARM self-modifying-code sequence: the copy above went
        // through the D-cache (Normal WB memory), which isn't
        // automatically coherent with the I-side fetch path on ARM -
        // clean the written line to the point of unification before the
        // shared I-cache invalidate below.
        unsafe {
            asm!("dc cvau, {addr}", addr = in(reg) slot_start, options(nostack));
        }

        let slot_addr = slot_start as u64;
        *unsafe { &mut *TASKS[i].0.get() } = Context {
            gpr: [0; 31],
            sp_el0: slot_addr + TASK_SLOT_SIZE,
            elr_el1: slot_addr,
            // M[3:0]=0000 selects EL0t (the only mode EL0 has), DAIF all
            // clear (every exception class unmasked - the timer tick must
            // be able to preempt), NZCV cleared.
            spsr_el1: 0,
        };
    }

    unsafe {
        asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack));
    }

    // nTWE/nTWI (SCTLR_EL1 bits 18/16 - the "n" means *disable* trapping,
    // confirmed against Linux's arch/arm64/tools/sysreg): without this,
    // EL0's own `wfe`/`wfi` traps to EL1 as a synchronous exception
    // instead of executing. Set once here, globally, for both tasks -
    // not per-task state.
    unsafe {
        asm!(
            "mrs {tmp}, sctlr_el1",
            "orr {tmp}, {tmp}, {bits}",
            "msr sctlr_el1, {tmp}",
            "isb",
            tmp = out(reg) _,
            bits = in(reg) (1u64 << 18) | (1u64 << 16),
            options(nostack),
        );
    }
}

/// Drops from EL1 into task 0. Never returns to its caller — the only way
/// back to EL1 is a trap (syscall, fault, or the timer tick), handled
/// entirely through `exceptions.rs`'s vector table from here on; every
/// subsequent switch happens inside the tick's IRQ trampoline, not here.
///
/// # Safety
/// Must be called after [`init`].
pub unsafe fn start() -> ! {
    let ctx = unsafe { *TASKS[0].0.get() };
    unsafe {
        asm!(
            "msr sp_el0, {sp_el0}",
            "msr elr_el1, {elr}",
            "msr spsr_el1, {spsr}",
            "eret",
            sp_el0 = in(reg) ctx.sp_el0,
            elr = in(reg) ctx.elr_el1,
            spsr = in(reg) ctx.spsr_el1,
            options(noreturn),
        );
    }
}

/// The entire scheduler: save whatever the tick just interrupted into its
/// task's slot, load the other task's saved state into `frame` in its
/// place. `exceptions.rs`'s trampoline does the rest — it doesn't know or
/// need to know a switch happened, it just restores from `frame` and
/// `eret`s, same as if nothing here had touched it.
///
/// # Safety
/// `frame` must be the live trap frame of the exception currently being
/// handled (true when called from `exceptions.rs`'s `rust_irq_handler`,
/// its only caller).
pub unsafe fn on_tick(frame: *mut Context) {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    unsafe { *TASKS[current].0.get() = *frame };
    let next = (current + 1) % NUM_TASKS;
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
}

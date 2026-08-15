//! Two EL0 tasks, alternated by the timer tick — real preemptive
//! switching, not just resuming whatever the tick happened to interrupt
//! (all `exceptions.rs`'s IRQ path could do before this).
//!
//! **Task 0 is a real userland program, loaded from disk** (`loader.rs`,
//! before `exit_boot_services`) rather than compiled into the kernel image
//! — see `docs/processes.md` for the full design. By the time [`init`]
//! runs, `loader.rs` has already copied the program's bytes into an
//! EL0-accessible region and `mmu.rs` has mapped it; there's nothing left
//! to copy here, just cache maintenance (see [`clean_dcache_range`]) and a
//! [`Context`] pointing at it. The default program is `shell` (a separate
//! crate, `shell/`) — the interactive line editor that used to live at EL1
//! in this kernel's own (now-deleted) `shell.rs`, driven by a dedicated
//! `shell_input` syscall. That syscall is gone: a loaded program just
//! calls `try_read_char`/`putc` directly and does its own line editing, in
//! its own separately-compiled code. Which program loads is a config file
//! on the ESP, not a kernel constant — replacing the shell means replacing
//! a file, not rebuilding the kernel.
//!
//! **Task 1 is a genuine idle task** — bare `wfe` loop, still a small
//! compiled-in `global_asm!` blob like every EL0 task before this
//! milestone, since there's nothing to load for "do nothing forever" and
//! its region is tiny (4KB) enough that the alignment ceiling below never
//! applied to it in the first place (that ceiling was specifically about
//! *large* alignments — see `IDLE_REGION`'s doc comment).
//!
//! There's no cooperative yielding anywhere — the *only* thing that ever
//! moves execution from one task to the other is the timer tick catching
//! one mid-`wfe` and swapping its saved [`Context`] for the other task's.
//! That swap is the entire scheduler: strict round-robin between exactly
//! two tasks, no priorities, no blocking, no queue.
//!
//! FP/SIMD state still isn't part of a task's [`Context`] (see
//! `exceptions.rs`'s module doc comment) — fine for these tasks, since
//! neither touches it, but a real limitation for whatever runs here next.

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::exceptions::Context;
use crate::loader::LoadedProgram;

pub const NUM_TASKS: usize = 2;

const IDLE_REGION_SIZE: usize = 0x1000;

/// 4KB, well under the ~8KB rustc/PE-COFF alignment ceiling that forced
/// `loader.rs` to load task 0's program at runtime instead of compiling it
/// in (see that module's doc comment) — that ceiling only bites at large
/// alignments; a single page was always fine, which is why the idle task
/// alone never needed to move off this compile-time-static approach.
#[repr(align(0x1000))]
struct IdleRegion(UnsafeCell<[u8; IDLE_REGION_SIZE]>);

// SAFETY: single-core; written once by `init` before either task ever
// runs, and only EL0 (isolated to this one region by `mmu.rs`) touches it
// after that.
unsafe impl Sync for IdleRegion {}

static IDLE_REGION: IdleRegion = IdleRegion(UnsafeCell::new([0; IDLE_REGION_SIZE]));

global_asm!(
    r#"
.text
.global el0_idle_template
el0_idle_template:
1:
wfe
b 1b
.global el0_idle_template_end
el0_idle_template_end:
"#
);

unsafe extern "C" {
    /// Opaque - only used for its address, to find and size the template
    /// to copy.
    static el0_idle_template: c_void;
    static el0_idle_template_end: c_void;
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

/// Task 1's (the idle task's) small dedicated EL0 region — `mmu.rs` maps
/// this alongside whatever `loader.rs` loaded for task 0. Two independent
/// regions, not one shared region split in half like before this
/// milestone: task 0's is sized to whatever program got loaded, not a
/// fixed slot.
pub fn idle_region() -> (u64, u64) {
    (IDLE_REGION.0.get() as u64, IDLE_REGION_SIZE as u64)
}

/// Builds task 0's [`Context`] from an already-loaded program — `loader.rs`
/// copied its bytes into an EL0-accessible region during boot services, so
/// there's nothing left to copy here, just cache maintenance — copies task
/// 1's idle loop into its own small region, and disables the EL0
/// `wfe`/`wfi` trap both tasks' idle loops rely on. Does not itself start
/// anything — see [`start`].
///
/// # Safety
/// Must run after `mmu.rs` has mapped both `program`'s region and
/// [`idle_region`] EL0-accessible, and before either task could possibly
/// run.
pub unsafe fn init(program: &LoadedProgram) {
    // Task 0: entry is exactly the load base - shell/linker.ld places
    // `_start` at file/VA offset 0, and loader.rs's region has no header
    // or padding before the program's own bytes. Stack at the top of the
    // loaded region, growing down, same shape every EL0 task has used.
    *unsafe { &mut *TASKS[0].0.get() } = Context {
        gpr: [0; 31],
        sp_el0: program.base + program.size,
        elr_el1: program.base,
        // M[3:0]=0000 selects EL0t (the only mode EL0 has), DAIF all
        // clear (every exception class unmasked - the timer tick must be
        // able to preempt), NZCV cleared.
        spsr_el1: 0,
    };
    clean_dcache_range(program.base, program.size);

    // Task 1: idle loop, copied into its own small static region exactly
    // like every EL0 task before this milestone.
    let idle_start = IDLE_REGION.0.get() as *mut u8;
    let start = &raw const el0_idle_template as *const u8;
    let end = &raw const el0_idle_template_end as *const u8;
    let len = unsafe { end.offset_from(start) } as usize;
    unsafe { core::ptr::copy_nonoverlapping(start, idle_start, len) };

    let idle_addr = idle_start as u64;
    *unsafe { &mut *TASKS[1].0.get() } = Context {
        gpr: [0; 31],
        sp_el0: idle_addr + IDLE_REGION_SIZE as u64,
        elr_el1: idle_addr,
        spsr_el1: 0,
    };
    clean_dcache_range(idle_addr, IDLE_REGION_SIZE as u64);

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

/// Standard ARM self-modifying-code sequence (clean D-cache, invalidate
/// I-cache, before anything executes what was just written) - the
/// invalidate half is one call for the whole address space
/// (`ic ialluis`, done once by the caller after every region is cleaned),
/// but `dc cvau` only cleans a single cache line (64 bytes on cortex-a72 -
/// not queried from `CTR_EL0`, just assumed, same as the rest of this
/// project's QEMU-shaped conventions) at a time, so a range bigger than
/// that needs one call per line to actually be correct. The version this
/// replaced called `dc cvau` exactly once regardless of region size - not
/// wrong on QEMU (TCG doesn't model real cache incoherency, so this has
/// never actually been exercised against hardware that would notice), but
/// an unverified simplification that stopped being safe to carry forward
/// once task 0's code could be bigger than one cache line.
fn clean_dcache_range(addr: u64, len: u64) {
    const CACHE_LINE: u64 = 64;
    let mut offset = 0;
    while offset < len {
        unsafe {
            asm!("dc cvau, {addr}", addr = in(reg) addr + offset, options(nostack));
        }
        offset += CACHE_LINE;
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

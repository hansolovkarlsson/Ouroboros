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
//! only ever `halt()`'s trivial spin loop or one of `tasks.rs`'s EL0 tasks
//! — this stops being safe the moment there's real interruptible work with
//! FP/SIMD state.
//!
//! ## Two vector groups now use the resumable path, not one
//!
//! A timer IRQ firing *while EL0 code is running* lands in a different
//! vector slot than one firing at EL1 — the table is grouped by [current EL
//! w/ SP_EL0][current EL w/ SP_ELx][lower EL AArch64][lower EL AArch32], so
//! "IRQ at EL1h" (slot 5, our own kernel code, `halt()`'s `wfe` loop) and
//! "IRQ from lower EL AArch64" (slot 9, EL0 tasks) are different entries
//! entirely. Both share the exact same resumable trampoline (`2:` below) —
//! GIC ack/EOI, the timer rearm, and now the task switch itself
//! (`tasks::on_tick`) don't care which EL got interrupted, and `eret`
//! resumes the right one automatically from the saved SPSR_EL1 regardless.
//! Slot 8 (Synchronous, lower EL AArch64) is where EL0's `svc` lands — but
//! that same slot is also where an EL0 *fault* would land (bad memory
//! access, etc.), so it isn't unconditionally treated as a syscall: it
//! checks ESR_EL1's EC field first and only takes the resumable syscall
//! path (`3:`) for EC=0x15 (SVC64), falling through to the ordinary
//! diverging report-and-halt path otherwise.
//!
//! **The SVC trampoline (`3:`) passes up to 4 syscall arguments, not
//! just 1** — added for phase 3c's file-I/O syscalls (`fs_list_dir`/
//! `fs_read_file`, `syscall.rs`), which need a path pointer, path
//! length, output buffer pointer, and output buffer length all at once.
//! `x0`-`x3` at the moment of the `svc` become `dispatch`'s `arg0`-`arg3`
//! (AAPCS64: syscall number in `x0`, then `arg0` in `x1` through `arg3`
//! in `x4`). Implemented by reloading the original `x0`-`x3`/`x8` fresh
//! from the stack frame the initial `stp` sequence already saved, rather
//! than juggling them through live registers around the `mrs
//! elr_el1`/`spsr_el1` reads (which need `x9`/`x10` as scratch instead,
//! specifically to avoid clobbering the argument registers before
//! they're consumed).
//!
//! ## The IRQ trampoline hands its saved frame to Rust — this is what
//! ## makes real task switching possible
//!
//! The frame `2:` builds (`x0`-`x30`, `SP_EL0`, `ELR_EL1`, `SPSR_EL1` — see
//! [`Context`], which mirrors this layout exactly) isn't just scratch space
//! to preserve-and-discard anymore: its address is passed as
//! `rust_irq_handler`'s argument (`mov x0, sp` right before the `bl`), and
//! on a timer tick `tasks::on_tick` overwrites it in place — copying the
//! *interrupted* task's registers out to its saved [`Context`], then
//! copying the *next* task's saved [`Context`] in. The trampoline's own
//! restore-and-`eret` afterward doesn't know or care that the frame now
//! holds different values than it saved a moment ago; it resumes whatever
//! is there, which is the whole mechanism. `SP_EL0` specifically had to be
//! added here for this to work — the pre-tasks.rs version of this
//! trampoline never saved it, because there was only ever one EL0 context
//! and it never needed to move.

use core::arch::global_asm;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::console;
use crate::gic;
use crate::halt;
use crate::syscall;
use crate::tasks;
use crate::timer;

/// The register state a resumable exception (`2:`/`3:` below) saves and
/// restores — `x0`-`x30`, `SP_EL0`, `ELR_EL1`, `SPSR_EL1`, in exactly this
/// field order, matching the trampoline's stack offsets byte for byte
/// (`gpr[30]` i.e. `x30` at offset 240, `sp_el0` at 248, `elr_el1`/
/// `spsr_el1` at 256/264 — 272 bytes total, matching `sub sp, sp, #272`).
/// This is also a full task's saved context (`tasks.rs`) — a suspended
/// task *is* exactly this much state, nothing more, since nothing here
/// saves FP/SIMD (see module doc comment).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Context {
    pub gpr: [u64; 31],
    pub sp_el0: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
}

impl Context {
    pub const fn zeroed() -> Self {
        Context { gpr: [0; 31], sp_el0: 0, elr_el1: 0, spsr_el1: 0 }
    }
}

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
// x9 is userland's, not ours, at this point - a real bug, found and
// fixed during the relocating-loader milestone (CLAUDE.md): the old
// version clobbered x9 with `mrs x9, esr_el1` before "3:"'s own save
// sequence ever ran, permanently losing whatever value userland had
// live in x9 at the moment of the `svc` - "3:" would faithfully
// save/restore *a* x9, just the wrong one (the shifted ESR_EL1/EC value,
// not userland's). Never actually observed before this milestone only
// because no prior userland build happened to keep a value live in x9
// across a syscall - PIC codegen's register allocation for a loop this
// project already had (print_u64_decimal's digit-print loop) was the
// first to do so, surfacing a bug that was always there. Fixed by
// saving x9 to a scratch stack slot *before* the check, and having "3:"
// recover it before its own save sequence runs.
sub sp, sp, #16
str x9, [sp]
mrs x9, esr_el1
lsr x9, x9, #26
cmp x9, #0x15
b.eq 3f
// Not an svc: a genuine EL0 fault (data/instruction abort, undefined
// instruction, ...). Unlike every EL1 fault - which still takes the
// diverging report-and-halt path "1:" below, honestly, since a kernel
// fault has nothing safe to resume - an EL0 fault is *contained*: the
// "4:" trampoline kills just the faulting task and resumes the next
// runnable one. This is the actual payoff of process isolation; before
// it existed, any userland wild pointer halted the whole system.
b   4f
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
mrs x0, sp_el0
str x0, [sp, #248]
mrs x0, elr_el1
mrs x1, spsr_el1
stp x0, x1, [sp, #256]
mov x0, sp   // frame pointer -> rust_irq_handler's argument
bl  {rust_irq_handler}
ldr x0, [sp, #248]
msr sp_el0, x0
ldp x0, x1, [sp, #256]
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
// Recover the real x9, saved to this scratch slot by the EC check above
// before it clobbered the live register - see that check's own comment.
ldr x9, [sp]
add sp, sp, #16
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
// SP_EL0/ELR/SPSR read into x9/x10 (not x0-x3) precisely so the
// original x0-x3 - already safely on the stack from the stp sequence
// above, untouched by anything since - can be reloaded fresh below
// rather than juggled through live registers. Laid out at offsets
// 248/256/264 to exactly match Context's own field order (gpr, then
// sp_el0, then elr_el1, then spsr_el1) - this frame is no longer just
// scratch space to save-and-blindly-restore the way it was before
// blocking syscalls existed: tasks::block_current_and_switch treats it
// as a real, interchangeable Context, the same way the IRQ path's "2:"
// trampoline already does, and that only works if the byte layout
// genuinely matches. SP_EL0 specifically didn't need saving before -
// hardware leaves it untouched across a synchronous SVC trap - but a
// *blocked* task can be resumed with a *different* task's SP_EL0 now,
// so it has to be real saved/restored state, not an assumption.
mrs x9, sp_el0
str x9, [sp, #248]
mrs x9, elr_el1
mrs x10, spsr_el1
stp x9, x10, [sp, #256]
// dispatch()'s AAPCS64 argument registers: syscall number in x0 (from
// the original x8), up to 4 syscall arguments in x1-x4 (from the
// original x0-x3) - reloaded from the stack rather than shuffled live,
// since every register needed is already sitting there untouched. x5
// gets the frame pointer itself (sp, at this exact point, already *is*
// the frame's base address - the same fact the SP_EL0/ELR/SPSR
// save/restore around this call already relies on) - a real blocking
// syscall can hand this to tasks::block_current_and_switch to suspend
// the caller and resume a different task instead of returning to this
// one; every other syscall just ignores the extra argument.
ldr x0, [sp, #64]
ldp x1, x2, [sp, #0]
ldp x3, x4, [sp, #16]
mov x5, sp
bl  {rust_syscall_handler}
str x0, [sp, #0]   // dispatch()'s return value becomes EL0's new x0
ldr x2, [sp, #248]
msr sp_el0, x2
ldp x2, x3, [sp, #256]
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

4:
// The resumable EL0-fault path: same frame-interchange contract as
// "2:" (save a full Context, hand its address to Rust, blindly restore
// whatever the handler left there and eret) - the handler kills the
// faulting task and copies the next runnable task's saved Context into
// the frame, so the restore below resumes the survivor, not the
// faulter. Entry mirrors "3:": recover the real x9 from the EC check's
// scratch slot first.
ldr x9, [sp]
add sp, sp, #16
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
mrs x0, sp_el0
str x0, [sp, #248]
mrs x0, elr_el1
mrs x1, spsr_el1
stp x0, x1, [sp, #256]
mov x0, sp   // frame pointer -> rust_el0_fault_handler's argument
bl  {rust_el0_fault_handler}
ldr x0, [sp, #248]
msr sp_el0, x0
ldp x0, x1, [sp, #256]
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
"#,
    rust_handler = sym rust_exception_handler,
    rust_irq_handler = sym rust_irq_handler,
    rust_syscall_handler = sym syscall::dispatch,
    rust_el0_fault_handler = sym rust_el0_fault_handler,
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
    console::println_force!(
        "Ouroboros kernel: EXCEPTION vector={vector} esr_el1={esr:#x} far_el1={far:#x} elr_el1={elr:#x}"
    );
    halt()
}

/// The resumable EL0-fault path's Rust half, called from the vector
/// table's "4:" trampoline with the faulting task's saved [`Context`] -
/// the containment payoff of process isolation: report the fault, tear
/// down *just the faulting task* (the same teardown order as the `KILL`
/// syscall's arm), switch the frame to the next runnable task, and let
/// the trampoline's blind restore resume the survivor. Before this
/// existed, any userland wild pointer took the diverging
/// report-and-halt path and stopped the whole system.
///
/// Tasks 0 (the boot shell - the keyboard owner; nothing meaningful
/// survives its death) and 1 (idle - it faulting means a kernel bug,
/// its code is 8 bytes of `nop; b`) still halt, honestly. If the dead
/// task is the filesystem server, the kernel restarts it from the
/// image kept at boot - see `syscall::restart_fsd`.
extern "C" fn rust_el0_fault_handler(frame: *mut Context) {
    let (esr, far, elr): (u64, u64, u64);
    // SAFETY: pure system-register reads; still valid - nothing has
    // re-trapped since the fault (IRQs are masked from exception entry
    // until the eret).
    unsafe {
        core::arch::asm!(
            "mrs {0}, esr_el1",
            "mrs {1}, far_el1",
            "mrs {2}, elr_el1",
            out(reg) esr,
            out(reg) far,
            out(reg) elr,
            options(nomem, nostack),
        );
    }
    let current = tasks::current_task();
    console::println_force!(
        "Ouroboros kernel: EL0 FAULT task={current} esr_el1={esr:#x} far_el1={far:#x} elr_el1={elr:#x}"
    );
    if current <= 1 {
        console::println_force!(
            "Ouroboros kernel: task {current} is the boot shell/idle - nothing to resume, halting"
        );
        halt();
    }
    console::println_force!("Ouroboros kernel: task {current} killed after fault");
    // Same teardown order as the KILL arm (syscall.rs): reclaim RAM
    // (LIFO-or-leak), hand the keyboard back if the faulter held it,
    // fail anyone blocked mid-call to it, then discard its context and
    // switch the frame to the next runnable task.
    let (base, size) = tasks::task_region(current);
    tasks::free_runtime_region(base, size);
    tasks::revert_input_owner_if(current);
    tasks::fail_calls_to(current);
    // SAFETY: `frame` is the live trap frame of this very fault (the
    // "4:" trampoline's contract).
    unsafe { tasks::kill_current_and_switch(frame) };
    // Any supervised server (the filesystem or console server) is
    // restarted from its kept image - the generalized version of the
    // fsd-only restart, now covering cond too. See supervisor.rs.
    if crate::supervisor::is_supervised(current) {
        crate::supervisor::restart(current);
    }
    // One rebuild covers both the dropped region and (if the server
    // was restarted) its fresh one.
    // SAFETY: IRQs are masked for the whole exception - single core,
    // nothing else can observe the table set mid-rebuild, the same
    // contract as the EXIT/KILL arms' rebuilds.
    unsafe { crate::mmu::rebuild_with_el0_regions(tasks::el0_regions()) };
}

static TICKS: AtomicU64 = AtomicU64::new(0);

/// The number of preemption ticks since boot - `syscall.rs`'s `get_ticks`
/// (6) is what actually exposes this to userland; a real "uptime" needs
/// `timer::TICK_INTERVAL_MS` to convert this into a duration, since a tick
/// count alone doesn't say how much wall-clock time it represents.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

const SPURIOUS_INTID: u32 = 1023;

/// Runs on every IRQ, called from the vector table's slots 5/9 trampoline
/// with a pointer to its saved [`Context`] — `frame` *is* whatever was
/// interrupted; overwriting it (as `tasks::on_tick` does) is how a task
/// switch happens. Must return normally (the trampoline restores from
/// `frame` and `eret`s back) — never halt or diverge from here.
extern "C" fn rust_irq_handler(frame: *mut Context) {
    let intid = unsafe { gic::acknowledge() };

    if intid == timer::INTID {
        // No longer logged every tick (used to print "tick N" here): now
        // that task 0 is a real interactive shell (tasks.rs/shell.rs), a
        // debug line firing this often (timer::TICK_INTERVAL_MS - shortened
        // to 20ms specifically to fix round-robin input lag, see its doc
        // comment) would constantly interleave with and corrupt whatever
        // the user is typing. TICKS is kept for whenever something wants an
        // uptime/tick-count query.
        TICKS.fetch_add(1, Ordering::Relaxed);
        timer::arm(timer::TICK_INTERVAL_MS);
        // Unconditional again - see tasks.rs's `el0_idle_template` doc
        // comment for the real, found-and-fixed bug this used to work
        // around: the task switch itself hung the very first time it ran
        // on real Parallels hardware, isolated (via a single-variable
        // diagnostic - temporarily skipping just this call) to prove
        // GIC/timer IRQ delivery was solid and the bug was specifically
        // here. Root cause traced to task 1's idle loop using `wfe` -
        // confirmed by swapping it for a busy-spin and re-testing on real
        // hardware: task switching, including the real interrupt-
        // delivery-plus-context-swap combination that never once ran on
        // real hardware before this session, now works correctly there
        // (a sustained real-hardware test showed a real, correctly-
        // incrementing tick count - e.g. 1526 -> 1976 - across multiple
        // interactive commands, no hang).
        unsafe { tasks::on_tick(frame) };
    }

    if intid != SPURIOUS_INTID {
        unsafe { gic::end_of_interrupt(intid) };
    }
}

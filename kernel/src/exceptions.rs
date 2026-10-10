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
//! Runs at EL1. QEMU and Parallels hand off at EL1 (typical for a UEFI OS
//! loader; EL2 is the hypervisor's own level, not the guest's), and the
//! VBAR_EL1 write in [`install`] is live at once. The Raspberry Pi's
//! firmware hands off at EL2 (found 2026-10-01 by `earlyfault.rs`'s first
//! dump on a Pi 4): there the same write, made from EL2, installs nothing
//! for the running level and takes effect when `el2.rs` drops to EL1
//! inside the identity-map install; until then a fault goes through the
//! firmware's EL2 vectors. The write is made from EL2 all the same, so
//! there is one owner of it; it is refused only under a VHE firmware
//! (`HCR_EL2.E2H`), where the `_EL1` name would reach `VBAR_EL2` and
//! replace the firmware's own vectors while it is still the handler.
//!
//! Deliberately not installed until after `exit_boot_services` — firmware
//! has its own VBAR_EL1 that its boot-services internals may depend on;
//! clobbering it while boot services are still active would be touching
//! state we don't own yet. A fault before that point is reported by
//! `earlyfault.rs`, which registers a handler through the firmware's own
//! CPU protocol rather than replacing its table.
//!
//! ## The IRQ vector is different from the other 15
//!
//! Every vector except the tick (the only source of interrupts so far,
//! see `gic.rs`/`timer.rs`) shares one path: capture ESR/FAR/ELR, report,
//! halt. That path never returns, so it never needs to preserve anything.
//! IRQ is the first exception this kernel needs to *resume from*, since
//! the interrupted task has to keep running afterward, so its vector slot
//! does a full general-purpose register + ELR_EL1/SPSR_EL1 save, calls
//! into Rust normally (`bl`, not a diverging `b`), restores everything,
//! and `eret`s back. Historically that slot was index 5 (IRQ at EL1h),
//! when the interrupted code was `halt()`'s `wfe` loop at EL1; since
//! 2026-09-20 it is slot 9 alone (IRQ from EL0), see the next section.
//!
//! Floating-point/SIMD state (`q0`-`q31`, `FPCR`, `FPSR`) is saved and
//! restored by every resumable path too, and is part of [`Context`], so a
//! task switch carries it. Until 2026-10-07 it was not, on the reasoning
//! that nothing used it; by then everything did. The kernel's own memcpy
//! and struct copies run through `q0`, and every userland program keeps
//! values in vector registers, so a register live across a syscall came
//! back changed. Found while building `cond`'s reverse video (#228), whose
//! second reversed glyph had two rows wrong: `cond` keeps shift constants
//! in `q0`-`q3` across its `FB_BLIT` calls. **Eager stays: decided
//! 2026-10-10 (multi-core step 3, decision 1 of
//! `docs/roadmap/roadmap-smp.md`), on a measurement.** `make
//! measure-syscost` (`/bin/SYSCOST`, 200,000 `GET_TICKS` calls) gave a
//! median of 24,934 ns a syscall on this tree and 25,232 ns with
//! `SAVE_FPSIMD` and `RESTORE_FPSIMD` emptied: under QEMU's TCG the
//! exception round trip costs about 25 µs and the 64 `stp`/`ldp` of the
//! save are within its noise. On hardware the round trip is a few hundred
//! nanoseconds and the save about a hundred cycles, a real fraction but a
//! small absolute cost, and it buys what a lazy scheme (`CPACR_EL1.FPEN`
//! trapping the first FP use after a switch) would spend per core: a
//! trap handler, a per-core "owner of the FP state" word, and the kernel
//! still running under the task's `FPCR` until the trap. The figure to
//! revisit is the Pi's, owed with the rest of the hardware checks. `make test-fpsimd` is the
//! check (`/bin/FPPROBE`).
//!
//! ## The tick is taken from EL0 only; an IRQ at EL1 is a fault
//!
//! A timer IRQ firing *while EL0 code is running* lands in a different
//! vector slot than one firing at EL1 — the table is grouped by [current EL
//! w/ SP_EL0][current EL w/ SP_ELx][lower EL AArch64][lower EL AArch32], so
//! "IRQ at EL1h" (slot 5, our own kernel code) and "IRQ from lower EL
//! AArch64" (slot 9, EL0 tasks) are different entries entirely. Slot 9
//! takes the resumable trampoline (`2:` below): GIC ack/EOI, the timer
//! rearm, and the task switch itself (`tasks::on_tick`). Slot 5 used to
//! share it, from the days when the interrupted code was `halt()`'s `wfe`
//! loop; it now takes the diverging report-and-halt path, because the
//! kernel never runs at EL1 with IRQs unmasked (`main.rs` masks them at
//! `exit_boot_services` and nothing unmasks until the first `eret` into
//! task 0 restores its SPSR), and every mutable static rests on exactly
//! that (`synccell.rs`). An IRQ taken at EL1 is therefore a broken
//! invariant, and reporting it (`EXCEPTION vector=5`) beats saving a
//! kernel frame as a task context and finding out later.
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
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use crate::console;
use crate::gic;
use crate::syscall;
use crate::tasks;
use crate::timer;

/// The register state a resumable exception (`2:`/`3:`/`4:` below) saves
/// and restores, in exactly this field order, matching the trampolines'
/// stack offsets byte for byte: `x0`-`x30` at 0 (`gpr[30]` at 240),
/// `sp_el0` at 248, `elr_el1`/`spsr_el1` at 256/264, then the FP/SIMD
/// state, `q0`-`q31` at 272 to 784 and `fpcr`/`fpsr` at 784/792: 800
/// bytes, matching `sub sp, sp, #800`. The asserts below hold the layout
/// to those numbers. This is also a full task's saved context
/// (`tasks.rs`): a suspended task is exactly this much state.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Context {
    pub gpr: [u64; 31],
    pub sp_el0: u64,
    pub elr_el1: u64,
    pub spsr_el1: u64,
    pub fpsimd: [u128; 32],
    pub fpcr: u64,
    pub fpsr: u64,
}

const _: () = assert!(core::mem::offset_of!(Context, sp_el0) == 248);
const _: () = assert!(core::mem::offset_of!(Context, elr_el1) == 256);
const _: () = assert!(core::mem::offset_of!(Context, fpsimd) == 272);
const _: () = assert!(core::mem::offset_of!(Context, fpcr) == 784);
const _: () = assert!(core::mem::offset_of!(Context, fpsr) == 792);
const _: () = assert!(core::mem::size_of::<Context>() == 800);

impl Context {
    pub const fn zeroed() -> Self {
        Context { gpr: [0; 31], sp_el0: 0, elr_el1: 0, spsr_el1: 0, fpsimd: [0; 32], fpcr: 0, fpsr: 0 }
    }

}

global_asm!(
    r#"
// The FP/SIMD half of a resumable frame: q0-q31 at offsets 272..784,
// then FPCR and FPSR at 784/792, matching Context's `fpsimd`, `fpcr` and
// `fpsr`. Every resumable path saves it after the general registers and
// restores it before them, so the two scratch registers it is given are
// already saved, or about to be reloaded. Without it a task's vector
// registers did not survive a syscall: the kernel's own code uses them
// (memcpy, struct copies), and so does every userland program's.
.macro SAVE_FPSIMD a, b
stp q0, q1, [sp, #272]
stp q2, q3, [sp, #304]
stp q4, q5, [sp, #336]
stp q6, q7, [sp, #368]
stp q8, q9, [sp, #400]
stp q10, q11, [sp, #432]
stp q12, q13, [sp, #464]
stp q14, q15, [sp, #496]
stp q16, q17, [sp, #528]
stp q18, q19, [sp, #560]
stp q20, q21, [sp, #592]
stp q22, q23, [sp, #624]
stp q24, q25, [sp, #656]
stp q26, q27, [sp, #688]
stp q28, q29, [sp, #720]
stp q30, q31, [sp, #752]
mrs \a, fpcr
mrs \b, fpsr
str \a, [sp, #784]   // past stp's reach for x registers (504)
str \b, [sp, #792]
.endm
.macro RESTORE_FPSIMD a, b
ldp q0, q1, [sp, #272]
ldp q2, q3, [sp, #304]
ldp q4, q5, [sp, #336]
ldp q6, q7, [sp, #368]
ldp q8, q9, [sp, #400]
ldp q10, q11, [sp, #432]
ldp q12, q13, [sp, #464]
ldp q14, q15, [sp, #496]
ldp q16, q17, [sp, #528]
ldp q18, q19, [sp, #560]
ldp q20, q21, [sp, #592]
ldp q22, q23, [sp, #624]
ldp q24, q25, [sp, #656]
ldp q26, q27, [sp, #688]
ldp q28, q29, [sp, #720]
ldp q30, q31, [sp, #752]
ldr \a, [sp, #784]
ldr \b, [sp, #792]
msr fpcr, \a
msr fpsr, \b
.endm

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
// slot 5: IRQ, Current EL with SP_ELx - an interrupt taken while KERNEL
// code runs. Diverging on purpose: EL1 never runs with IRQs unmasked
// (masked at exit_boot_services, and the first eret into task 0 is what
// unmasks, for EL0 only - see synccell.rs), so this slot is the check on
// that invariant. It used to take the resumable path "2:" like slot 9;
// then a future EL1 unmask would have had the tick save a KERNEL frame as
// a task's context and eret into it later, silently. Now it reports and
// halts, vector=5, the moment it happens.
mov x3, #5
b   1f
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
// slot 9: IRQ, lower EL AArch64 - the tick, firing while an EL0 task
// runs: the ONE resumable IRQ path, and the only place a task switch
// happens. See the module doc comment.
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
sub sp, sp, #800
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
SAVE_FPSIMD x0, x1
mov x0, sp   // frame pointer -> rust_irq_handler's argument
bl  {rust_irq_handler}
// The one entry into a task from a frame: restores a full Context at sp,
// pops it and erets. `tasks::start` enters task 0 here too (multi-core
// step 3), so no eret into a task exists outside the trampolines.
.global resume_frame
resume_frame:
RESTORE_FPSIMD x0, x1
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
add sp, sp, #800
eret

3:
// Recover the real x9, saved to this scratch slot by the EC check above
// before it clobbered the live register - see that check's own comment.
ldr x9, [sp]
add sp, sp, #16
sub sp, sp, #800
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
SAVE_FPSIMD x9, x10
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
RESTORE_FPSIMD x2, x3
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
add sp, sp, #800
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
sub sp, sp, #800
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
SAVE_FPSIMD x0, x1
mov x0, sp   // frame pointer -> rust_el0_fault_handler's argument
bl  {rust_el0_fault_handler}
RESTORE_FPSIMD x0, x1
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
add sp, sp, #800
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

/// The vector table's address, for a secondary core's `VBAR_EL1`
/// (`smp.rs`): the same table on every core.
pub(crate) fn vector_table_addr() -> u64 {
    &raw const exception_vector_table as u64
}

/// Points VBAR_EL1 at [`exception_vector_table`]. Must be called after
/// `exit_boot_services`, before anything that could plausibly fault. The
/// one place the vectors are written, at any handoff level: at EL1 live at
/// once, at EL2 live at the drop (see the module doc). At EL2 under
/// `HCR_EL2.E2H` the write is skipped: the name would reach `VBAR_EL2`,
/// and the drop halts on E2H anyway, with a line.
pub fn install() {
    if crate::el2::current_el() == 2 {
        const HCR_EL2_E2H: u64 = 1 << 34;
        let hcr: u64;
        unsafe { core::arch::asm!("mrs {0}, hcr_el2", out(reg) hcr, options(nomem, nostack, preserves_flags)) };
        if hcr & HCR_EL2_E2H != 0 {
            return;
        }
    }
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
        "Ouroboros kernel: EXCEPTION core={} vector={vector} esr_el1={esr:#x} far_el1={far:#x} elr_el1={elr:#x}",
        crate::smp::core_index()
    );
    let (canary_intact, stack_base) = crate::smp::this_core_stack_canary();
    if !canary_intact {
        console::println_force!(
            "Ouroboros kernel:   this core's kernel stack's canary is OVERWRITTEN: the stack overflowed below {stack_base:#x}"
        );
    }
    // Whose halt: a core inside an entry (holding the kernel lock) or the
    // boot core halts the kernel, a secondary holding nothing halts alone
    // (power::halt, lock.rs). main.rs's silent halt was the old end here,
    // and said nothing about either.
    crate::power::halt()
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
/// task is a supervised server (fsd, cond, netd, accountd), the kernel
/// restarts it from the image kept at boot - see `supervisor::restart`.
extern "C" fn rust_el0_fault_handler(frame: *mut Context) {
    // The kernel lock around the whole handler (lock.rs); a halting arm
    // below halts the kernel with it held (power::halt), every core
    // stopping at its next entry.
    crate::lock::acquire();
    el0_fault_locked(frame);
    crate::lock::release();
}

fn el0_fault_locked(frame: *mut Context) {
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
    let current = tasks::current_index();
    let slot = current.index();
    console::println_force!(
        "Ouroboros kernel: EL0 FAULT task={slot} esr_el1={esr:#x} far_el1={far:#x} elr_el1={elr:#x}"
    );
    if tasks::is_boot_or_idle(current) {
        console::println_force!(
            "Ouroboros kernel: task {slot} is the boot shell/idle - nothing to resume, halting"
        );
        crate::power::halt();
    }
    console::println_force!("Ouroboros kernel: task {slot} killed after fault");
    // Both halves of the teardown (RAM, keyboard, pending calls; then
    // state and tables), and the switch to the next runnable task.
    // SAFETY: `frame` is the live trap frame of this very fault (the
    // "4:" trampoline's contract).
    unsafe { tasks::kill_current_and_switch(frame) };
    // Any supervised server is restarted from its kept image; which
    // slots count is supervisor.rs's to say, not this comment's.
    if crate::supervisor::is_supervised(slot) {
        crate::supervisor::restart(slot);
    }
    // One rebuild covers both the dropped region and (if the server
    // was restarted) its fresh one.
    // SAFETY: IRQs are masked for the whole exception and this core holds
    // the kernel lock, so no other kernel entry observes the table set
    // mid-rebuild, the same contract as the EXIT/KILL arms' rebuilds. The
    // idling secondary cores (step 4(b)) walk view 1's tables meanwhile;
    // see rebuild_with_el0_regions for why that is sound today.
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

/// The GIC INTID the NIC's receive interrupt is wired to, or [`NO_NET_INTID`]
/// if no NIC was enabled this boot. Set by `main.rs` once, after
/// `gic::enable_interrupt`, and read by [`rust_irq_handler`] to route a NIC
/// IRQ to the network wake (`tasks::on_net_irq`). Same single-core "set
/// once at boot, read from the IRQ handler" reasoning as [`TICKS`].
const NO_NET_INTID: u32 = u32::MAX;
static NET_INTID: AtomicU32 = AtomicU32::new(NO_NET_INTID);

/// Registers which GIC INTID belongs to the NIC's receive interrupt, so
/// [`rust_irq_handler`] can route it to the network wake. Called once from
/// `main.rs` after the NIC's interrupt is enabled at the GIC. No-op in
/// effect until then (the default never matches a real INTID).
pub fn set_net_intid(intid: u32) {
    NET_INTID.store(intid, Ordering::Relaxed);
}

/// Runs on every IRQ, called from the vector table's slots 5/9 trampoline
/// with a pointer to its saved [`Context`] — `frame` *is* whatever was
/// interrupted; overwriting it (as `tasks::on_tick` does) is how a task
/// switch happens. Must return normally (the trampoline restores from
/// `frame` and `eret`s back) — never halt or diverge from here.
extern "C" fn rust_irq_handler(frame: *mut Context) {
    // The kernel lock around the whole tick (lock.rs).
    crate::lock::acquire();
    irq_locked(frame);
    crate::lock::release();
}

fn irq_locked(frame: *mut Context) {
    let intid = unsafe { gic::acknowledge() };

    // A secondary core (multi-core step 4(b)) takes its own timer and
    // nothing else: a device interrupt that reached it (a GICv2 SPI routed
    // by interface number, say) is acknowledged below and dropped, never
    // acted on, since acting on it (on_net_irq) would switch this core
    // into a task behind the scheduler's back. Devices are the boot
    // core's, and so are the tick's jobs (step 3, decision 2).
    let boot_core = crate::smp::is_boot_core();
    if intid == timer::INTID {
        // No longer logged every tick (used to print "tick N" here): now
        // that task 0 is a real interactive shell (tasks.rs/shell.rs), a
        // debug line firing this often (timer::TICK_INTERVAL_MS - shortened
        // to 20ms specifically to fix round-robin input lag, see its doc
        // comment) would constantly interleave with and corrupt whatever
        // the user is typing. TICKS is kept for whenever something wants an
        // uptime/tick-count query.
        timer::arm(timer::TICK_INTERVAL_MS);
        if !boot_core {
            crate::smp::secondary_tick();
            unsafe { gic::end_of_interrupt(intid) };
            return;
        }
        TICKS.fetch_add(1, Ordering::Relaxed);
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
    } else if boot_core && intid == NET_INTID.load(Ordering::Relaxed) {
        // A NIC receive frame arrived (TX completions are suppressed at the
        // device, so this INTID always means receive - see virtio_net.rs).
        // Ack the device so it can raise the next one, then wake the network
        // server (blocked in NET_WAIT) and switch to it now - the latency
        // win over the tick-poll, which stays as a fallback (see
        // tasks::on_net_irq).
        syscall::net_ack_interrupt();
        unsafe { tasks::on_net_irq(frame) };
    }

    if intid != SPURIOUS_INTID {
        unsafe { gic::end_of_interrupt(intid) };
    }
}

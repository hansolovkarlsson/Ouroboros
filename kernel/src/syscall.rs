//! The syscall boundary: dropping to EL0 ("user" execution) and the
//! EL0->EL1 trap path (`svc`) that lets it ask the kernel to do something
//! on its behalf. First steps only - see the limitations below before
//! treating any of this as a real security boundary.
//!
//! **A genuinely separate EL0 region, not the whole kernel image.** An
//! earlier version had EL0 execute code living anywhere in the same RAM
//! block as the rest of the kernel (see `mmu.rs`'s module doc comment for
//! why that got reverted: making that whole block EL0-accessible hard-
//! faulted for reasons a thorough investigation didn't resolve). This
//! version instead reserves one dedicated, 8KB-aligned, 8KB-*sized* Rust
//! static (`EL0_REGION`) — since its size exactly matches its alignment,
//! it exclusively occupies its own aligned slot with nothing else from the
//! kernel sharing it.
//!
//! **8KB, not 2MB, and not by choice — a real, precisely bisected `rustc`
//! limit.** The original plan used a 2MB-aligned region to match `mmu.rs`'s
//! L2 (2MB) block granularity, first via a `global_asm!`-level `.balign
//! 0x200000`, then via `#[repr(align(0x200000))]` on this same static.
//! Both crashed `rustc` outright (SIGTRAP, no diagnostic, independent of
//! `-C debuginfo`). Bisected precisely with a minimal isolated repro (just
//! a `#[repr(align(N))] struct` with a same-sized byte array, nothing
//! else): `align(0x2000)` (8KB) compiles, `align(0x4000)` (16KB) crashes,
//! every time. That boundary is exactly `IMAGE_SCN_ALIGN_8192BYTES` — 8192
//! bytes is the *largest section alignment the PE/COFF object format can
//! express at all*. Asking for more apparently isn't a supported,
//! gracefully-rejected request on this backend; it's an unhandled case
//! that crashes the compiler. Given that, `mmu.rs` had to grow a fourth
//! translation table level (L3, 4KB pages) to isolate a region this small
//! precisely — its 2MB L2 blocks alone aren't fine-grained enough.
//!
//! The EL0 demo task itself is still hand-written machine code (a tiny
//! `global_asm!` block, no padding - nowhere near the crash threshold), but
//! lives at its normal, unremarkable link address; `enter` copies it into
//! `EL0_REGION` at runtime and does the standard ARM self-modifying-code
//! cache maintenance (clean D-cache to PoU, invalidate I-cache, barriers)
//! before ever executing from there. `mmu.rs` gives only `EL0_REGION`'s
//! pages EL0 access; everything else, including all the kernel code that
//! keeps running immediately after the table switch, stays on the
//! proven-safe EL1-only permissions.
//!
//! Still no per-region W^X within the EL0 region itself (code and stack
//! share one slot, both executable) and no per-process isolation — just
//! enough separation to test whether isolating the EL0 region from
//! *actively-executing kernel code* is what the previous attempt was
//! missing.
//!
//! Device MMIO (`mmu.rs`'s device block) stays EL1-only throughout, so EL0
//! genuinely cannot touch hardware directly - only ask the kernel to, via
//! a syscall.
//!
//! Calling convention (chosen to match Linux's, a reasonable default for a
//! "POSIX-ish" OS per this project's stated goals, not because anything
//! here is Linux-ABI-compatible): syscall number in x8, first argument in
//! x0, return value in x0.

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::ffi::c_void;

use crate::console;

// 8KB: the largest alignment/size that doesn't crash rustc for this target
// (see module doc comment) - not chosen for any other reason.
const EL0_REGION_SIZE: usize = 0x2000;

#[repr(align(0x2000))]
struct El0Region(UnsafeCell<[u8; EL0_REGION_SIZE]>);

// SAFETY: single-core; written once by `enter` before EL0 ever runs, and
// EL0 only ever reads/executes it after that.
unsafe impl Sync for El0Region {}

static EL0_REGION: El0Region = El0Region(UnsafeCell::new([0; EL0_REGION_SIZE]));

// The EL0 demo task's machine code, at its ordinary (small, unpadded) link
// address - `enter` copies it into EL0_REGION rather than executing it
// here directly, since here isn't EL0-accessible.
global_asm!(
    r#"
.text
.global el0_demo_task_template
el0_demo_task_template:
mov x8, #0    // syscall 0
mov x0, #42   // arg0
svc #0
1:
wfe
b 1b
.global el0_demo_task_template_end
el0_demo_task_template_end:
"#
);

unsafe extern "C" {
    /// Opaque - only used for their addresses, to find and size the
    /// template to copy.
    static el0_demo_task_template: c_void;
    static el0_demo_task_template_end: c_void;
}

/// The isolated EL0 region's (start, size) — `mmu.rs` uses this to decide
/// which single 2MB slot gets EL0 access.
pub fn el0_region() -> (u64, u64) {
    (EL0_REGION.0.get() as u64, EL0_REGION_SIZE as u64)
}

/// Copies the EL0 demo task into `EL0_REGION`, then drops from EL1 to EL0
/// and starts executing it there, with a stack carved from the top of the
/// same region (SP_EL0, separate from the kernel's SP_EL1). Never returns
/// to its caller — the only way back to EL1 is a trap (syscall or fault),
/// handled entirely through `exceptions.rs`'s vector table from here on.
///
/// # Safety
/// `EL0_REGION` must actually be EL0-accessible and executable under the
/// current translation tables (true once `mmu.rs` has mapped it that way).
pub unsafe fn enter() -> ! {
    let region_start = EL0_REGION.0.get() as *mut u8;
    let template_start = &raw const el0_demo_task_template as *const u8;
    let template_end = &raw const el0_demo_task_template_end as *const u8;
    let template_len = unsafe { template_end.offset_from(template_start) } as usize;

    unsafe {
        core::ptr::copy_nonoverlapping(template_start, region_start, template_len);
    }

    // Standard ARM self-modifying-code sequence: the copy above went
    // through the D-cache (Normal WB memory), which isn't automatically
    // coherent with the I-side fetch path on ARM - clean the written line
    // to the point of unification, then invalidate the I-cache, with
    // barriers so nothing races ahead of either step.
    unsafe {
        asm!(
            "dc cvau, {addr}",
            "dsb ish",
            "ic ialluis",
            "dsb ish",
            "isb",
            addr = in(reg) region_start,
            options(nostack),
        );
    }

    // nTWE/nTWI (SCTLR_EL1 bits 18/16 - the "n" means *disable* trapping,
    // confirmed against Linux's arch/arm64/tools/sysreg): without this,
    // EL0's own `wfe`/`wfi` traps to EL1 as a synchronous exception
    // instead of executing - confirmed directly, this is exactly what the
    // demo task's idle loop hit on the first real end-to-end test (a
    // Trapped WFE exception, EC=0x01, right after the syscall round-trip
    // itself worked correctly). Not a bug in the syscall path; just an
    // unconfigured control bit, left however firmware set it.
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

    let entry = region_start as u64;
    let stack_top = region_start as u64 + EL0_REGION_SIZE as u64;
    unsafe {
        asm!(
            "msr sp_el0, {stack}",
            "msr elr_el1, {entry}",
            "msr spsr_el1, {spsr}",
            "eret",
            stack = in(reg) stack_top,
            entry = in(reg) entry,
            // SPSR_EL1 = 0: M[3:0]=0000 selects EL0t (the only mode EL0
            // has), DAIF all clear (every exception class unmasked - the
            // timer tick must still be able to preempt EL0), NZCV cleared.
            spsr = in(reg) 0u64,
            options(noreturn),
        );
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

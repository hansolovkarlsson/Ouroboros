//! ARM generic timer (non-secure EL1 physical timer, CNTP) — the source of
//! the preemption tick.
//!
//! Confirmed via the same devicetree dump used for `gic.rs`: `timer {
//! compatible = "arm,armv8-timer"; interrupts = <1 13 ...>, <1 14 ...>, <1
//! 11 ...>, <1 10 ...>; }`. The arm,gic devicetree binding's fixed order
//! for this compatible string is secure-physical, non-secure-physical,
//! virtual, hypervisor — so PPI 14 is the non-secure EL1 physical timer,
//! the one available without any EL2 involvement. PPI-to-INTID is `16 +
//! PPI number` (SGIs occupy INTID 0-15), so GIC INTID 30. Control register
//! bit positions (ENABLE/IMASK/ISTATUS) cross-checked against Linux's
//! include/clocksource/arm_arch_timer.h.

use core::arch::asm;

pub const INTID: u32 = 30;

/// The preemption tick's period. Single source of truth for both the
/// initial arm in `main.rs` and every re-arm in `exceptions.rs`'s IRQ
/// handler - was 1000 (once/sec) through the task-switching milestone,
/// which turned out to be the actual cause of a real, user-visible bug:
/// `tasks.rs`'s round-robin swaps unconditionally on every tick regardless
/// of whether there's anything to do, so a keystroke arriving while the
/// idle task happens to be scheduled sits in the UART FIFO for up to one
/// full tick period before the shell task gets a turn again - one second,
/// worst case, easily perceived as input lag. 20ms keeps that worst case
/// low enough to feel instant without materially increasing tick overhead.
pub const TICK_INTERVAL_MS: u64 = 20;

// Enabled (bit0=1), not masked (bit1=0 - IMASK omitted, not written).
const CTL_ENABLE: u64 = 1 << 0;

pub(crate) fn frequency_hz() -> u64 {
    let freq: u64;
    unsafe {
        asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nomem, nostack, preserves_flags));
    }
    freq
}

/// Reads the free-running physical counter (`CNTPCT_EL0`) - a pure
/// system-register access with the same "no GIC, no interrupts, safe on
/// any ARMv8 CPU by construction" property `frequency_hz`/`arm` already
/// have (see this module's own doc comment). Exposed (`pub(crate)`) for
/// any module that needs to bound a busy-poll by real elapsed wall-clock
/// time rather than a fixed iteration count - `xhci.rs` is the first
/// caller, after a fixed iteration count there turned out not to be a
/// safe proxy for real time under real virtualization: a real hypervisor
/// can deschedule this guest's vCPU for an arbitrary real duration (e.g.
/// while compositing a live VM window on the host) without the loop's
/// own iteration count reflecting that at all - confirmed by a real,
/// reproducible xHCI command-ring timeout on real Parallels hardware
/// that a merely larger iteration budget wouldn't reliably fix, since
/// the underlying problem is about elapsed *time*, not instruction
/// count.
pub(crate) fn now_ticks() -> u64 {
    let ticks: u64;
    unsafe {
        asm!("mrs {0}, cntpct_el0", out(reg) ticks, options(nomem, nostack, preserves_flags));
    }
    ticks
}

/// Arms the timer to fire once, `interval_ms` milliseconds from now.
/// Re-arming (calling this again) is how a periodic tick is built — see
/// the IRQ handler in `exceptions.rs`, which calls this again each time the
/// timer fires.
pub fn arm(interval_ms: u64) {
    let ticks = frequency_hz() / 1000 * interval_ms;
    unsafe {
        asm!(
            "msr cntp_tval_el0, {ticks}",
            "msr cntp_ctl_el0, {ctl}",
            ticks = in(reg) ticks,
            ctl = in(reg) CTL_ENABLE,
            options(nomem, nostack, preserves_flags),
        );
    }
}

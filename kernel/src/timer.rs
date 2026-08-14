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

// Enabled (bit0=1), not masked (bit1=0 - IMASK omitted, not written).
const CTL_ENABLE: u64 = 1 << 0;

fn frequency_hz() -> u64 {
    let freq: u64;
    unsafe {
        asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nomem, nostack, preserves_flags));
    }
    freq
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

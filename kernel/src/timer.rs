//! ARM generic timer (non-secure EL1 physical timer, CNTP), the source of
//! the preemption tick.
//!
//! Confirmed via the same devicetree dump used for `gic.rs`: `timer {
//! compatible = "arm,armv8-timer"; interrupts = <1 13 ...>, <1 14 ...>, <1
//! 11 ...>, <1 10 ...>; }`. The arm,gic devicetree binding's fixed order
//! for this compatible string is secure-physical, non-secure-physical,
//! virtual, hypervisor, so PPI 14 is the non-secure EL1 physical timer,
//! the one available without any EL2 involvement. PPI-to-INTID is `16 +
//! PPI number` (SGIs occupy INTID 0-15), so GIC INTID 30. Control register
//! bit positions (ENABLE/IMASK/ISTATUS) cross-checked against Linux's
//! include/clocksource/arm_arch_timer.h.

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

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

/// The lowest counter frequency the kernel keeps time with. A millisecond
/// deadline is `frequency_hz() / 1000 * ms` (`arm`'s tick, `xhci.rs`'s
/// polls, `smp.rs`'s wait, `syscall.rs`'s), and the division truncates
/// first: at 0, [`monotonic_us`] divides by zero; below 1 kHz a
/// millisecond is 0 ticks, every poll times out at once and the tick
/// fires without end; at 1999 Hz every deadline is about half what it
/// asks. At 1 MHz the truncation is at most a thousandth (the review of
/// #488 found 1 kHz let it be a half). The kernel does not guess a
/// frequency, as it does not guess a UART's address: [`init`] refuses one
/// below this, and the boot stops with a line.
pub(crate) const MIN_FREQUENCY_HZ: u64 = 1_000_000;

/// The counter's frequency, read and checked once by [`init`] on the boot
/// core before anything keeps time, and the one value every core uses
/// after. `CNTFRQ_EL0` is a register of each core, written by firmware
/// and not by the counter, which all the cores share; a secondary whose
/// firmware left its own at 0 still keeps time by the checked value (the
/// review of #488). 0 until [`init`].
static FREQUENCY_HZ: AtomicU64 = AtomicU64::new(0);

/// This core's `CNTFRQ_EL0`, as the firmware left it.
fn read_cntfrq() -> u64 {
    let freq: u64;
    unsafe {
        asm!("mrs {0}, cntfrq_el0", out(reg) freq, options(nomem, nostack, preserves_flags));
    }
    freq
}

/// Reads the boot core's `CNTFRQ_EL0` and keeps it as the kernel's
/// frequency, or refuses it below [`MIN_FREQUENCY_HZ`]: `Err` carries the
/// frequency read, and nothing is kept. The first act of `kernel_main`
/// after the logger, so no caller of [`frequency_hz`] comes before it.
pub(crate) fn init() -> Result<u64, u64> {
    let hz = read_cntfrq();
    if hz < MIN_FREQUENCY_HZ {
        return Err(hz);
    }
    // Release: a secondary started after this reads it with Acquire.
    FREQUENCY_HZ.store(hz, Ordering::Release);
    Ok(hz)
}

/// The counter's frequency as [`init`] checked it: at least
/// [`MIN_FREQUENCY_HZ`] on every core, from the boot's start on.
pub(crate) fn frequency_hz() -> u64 {
    FREQUENCY_HZ.load(Ordering::Acquire)
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

/// The counter value `us` microseconds from now: a deadline for a busy-poll
/// bounded by elapsed time ([`now_ticks`]'s doc says why that, and not an
/// iteration count). Rounded up, so a wait is never shorter than asked, and
/// at least one tick past now.
pub(crate) fn deadline_us(us: u64) -> u64 {
    let ticks = (frequency_hz() * us).div_ceil(1_000_000).max(1);
    now_ticks() + ticks
}

/// Spins until `us` microseconds have passed ([`deadline_us`]).
pub(crate) fn delay_us(us: u64) {
    let until = deadline_us(us);
    while now_ticks() < until {
        core::hint::spin_loop();
    }
}

/// Microseconds since boot from the free-running counter, computed
/// overflow-safe (a naive `now_ticks() * 1_000_000` overflows a u64 in a few
/// days at 62.5 MHz): whole seconds plus the sub-second remainder. The one
/// spelling of the clock `MONOTONIC_US` answers and `KEY_WAIT_UNTIL`'s
/// deadline is in. Its division is unchecked because [`frequency_hz`] is
/// the value [`init`] checked, never 0 after it.
pub(crate) fn monotonic_us() -> u64 {
    let freq = frequency_hz();
    let ticks = now_ticks();
    (ticks / freq) * 1_000_000 + ((ticks % freq) * 1_000_000) / freq
}

/// Microseconds from `began`, an earlier [`now_ticks`] reading, to now.
/// `None` where the counter's frequency reads 0, so a caller that only
/// wants to print a duration can say it does not know one: the division
/// is checked here as well as by [`init`],
/// for a caller that must not be able to end the boot by asking
/// (`genetnic::quiet`, around the stop of a device that may be writing
/// memory).
pub(crate) fn us_since(began: u64) -> Option<u64> {
    now_ticks().saturating_sub(began).saturating_mul(1_000_000).checked_div(frequency_hz())
}

/// Arms the timer to fire once, `interval_ms` milliseconds from now.
/// Re-arming (calling this again) is how a periodic tick is built. See
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

//! Version-dispatching facade over `gicv2.rs` (GICv2) and `gicv3.rs`
//! (GICv3) — the real GIC version is now discovered at boot via
//! `madt.rs`, not assumed, so this module exists to keep `main.rs`'s and
//! `exceptions.rs`'s call sites (`init`/`enable_interrupt`/`acknowledge`/
//! `end_of_interrupt`) exactly the same shape they were when this file
//! *was* the GICv2 driver directly. `exceptions.rs`'s two call sites
//! (`acknowledge`/`end_of_interrupt`, both inside the resumable IRQ path)
//! don't change at all; `main.rs` gains one `configure` call ahead of the
//! existing `init`/`enable_interrupt` pair — see CLAUDE.md's MADT/GICv3
//! scoping notes for why a facade was chosen over matching
//! `madt::GicVersion` at every call site instead (more churn for no real
//! benefit, since every call site would need the match anyway).

use core::cell::Cell;

use crate::madt::{GicInfo, GicVersion};
use crate::{gicv2, gicv3};

struct GicCell(Cell<Option<GicInfo>>);

// SAFETY: single-core, no preemption, no interrupts unmasked until after
// `init` has run - same reasoning `console.rs`'s `ConsoleCell` already
// documents for the identical pattern.
unsafe impl Sync for GicCell {}

static INFO: GicCell = GicCell(Cell::new(None));

/// Records which GIC this platform actually has, from `madt::discover`'s
/// real MADT parse. Must be called once, before [`init`].
pub fn configure(info: GicInfo) {
    INFO.0.set(Some(info));
}

fn info() -> GicInfo {
    INFO.0
        .get()
        .expect("gic:: called before gic::configure - main.rs should only reach this after a successful madt::discover")
}

/// Enables the distributor and this CPU's interface. See
/// `gicv2::init`/`gicv3::init` for what that means on each version.
///
/// # Safety
/// [`configure`] must have already been called with a real discovered
/// `GicInfo`. Must run after `mmu.rs`'s identity map is installed (the
/// discovered GICD/GICC/GICR addresses must be mapped) and before
/// unmasking IRQ in DAIF.
pub unsafe fn init() {
    let info = info();
    match info.version {
        GicVersion::V2 => unsafe { gicv2::init(info.gicd_base as usize, info.gicc_base as usize) },
        GicVersion::V3 => unsafe {
            gicv3::init(
                info.gicd_base as usize,
                info.gicr_base as usize,
                info.gicr_size as usize,
            )
        },
    }
}

/// Enables forwarding of `intid` (e.g. the timer PPI, 30).
///
/// # Safety
/// Must run after [`init`].
pub unsafe fn enable_interrupt(intid: u32) {
    let info = info();
    match info.version {
        GicVersion::V2 => unsafe { gicv2::enable_interrupt(info.gicd_base as usize, intid) },
        GicVersion::V3 => unsafe { gicv3::enable_interrupt(intid) },
    }
}

/// Reads the highest-priority pending interrupt ID and acknowledges it.
///
/// # Safety
/// Must run after [`init`], from IRQ-handling context.
pub unsafe fn acknowledge() -> u32 {
    match info().version {
        GicVersion::V2 => unsafe { gicv2::acknowledge(info().gicc_base as usize) },
        GicVersion::V3 => unsafe { gicv3::acknowledge() },
    }
}

/// Signals that the interrupt `intid` (as returned by [`acknowledge`]) has
/// been fully handled.
///
/// # Safety
/// Must run after [`init`], with `intid` from a matching [`acknowledge`]
/// call.
pub unsafe fn end_of_interrupt(intid: u32) {
    match info().version {
        GicVersion::V2 => unsafe { gicv2::end_of_interrupt(info().gicc_base as usize, intid) },
        GicVersion::V3 => unsafe { gicv3::end_of_interrupt(intid) },
    }
}

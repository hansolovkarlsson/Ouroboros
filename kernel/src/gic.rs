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
//!
//! Multi-core step 4(c) changed the IRQ path's two call sites after all:
//! `acknowledge` returns an [`Acked`], whose ID is [`Acked::intid`] and
//! whose whole value only [`end_of_interrupt`] reads, since a GICv2 SGI's
//! acknowledge carries its sending CPU and the end of interrupt must too.
//! The same step added [`send_sgi`] and [`note_this_core`].

use core::cell::Cell;

use crate::madt::{GicInfo, GicVersion};
use crate::{gicv2, gicv3};

struct GicCell(Cell<Option<GicInfo>>);

// SAFETY: written once, by `configure` on the boot core before any second
// core runs (`smp::start` comes after `init`), and read-only after: every
// core's acknowledge, end-of-interrupt and per-core setup read it, and a
// second `configure` is refused below, so the one write happens before
// the first concurrent read. A `Cell` rather than a `SyncCell` because its
// users want `get`/`set` by value.
unsafe impl Sync for GicCell {}

static INFO: GicCell = GicCell(Cell::new(None));

/// Records which GIC this platform actually has, from `madt::discover`'s
/// real MADT parse. Once, before [`init`] and before any second core: a
/// second call is the write-once rule broken, and halts with a line.
pub fn configure(info: GicInfo) {
    if INFO.0.get().is_some() {
        crate::console::println_force!("Ouroboros kernel: gic::configure called twice; the GIC description is written once");
        crate::power::halt();
    }
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

/// Brings up THIS core's side of the GIC only, on a secondary core: the
/// GICv2 CPU interface (banked per core at the same address) or this
/// core's GICv3 redistributor and system registers. The distributor was
/// enabled once by [`init`] on the boot core. `Err` says why a GICv3 core
/// could not (no redistributor frame for its MPIDR, or the system
/// register interface refused), for the caller to log and park on: a
/// panic on a secondary has no reporting path.
///
/// # Safety
/// After [`init`] ran on the boot core; on a core with its MMU on the
/// shared tables, IRQs masked.
pub unsafe fn init_this_core() -> Result<ThisCore, &'static str> {
    let info = info();
    match info.version {
        GicVersion::V2 => {
            unsafe { gicv2::init_cpu_interface(info.gicc_base as usize) };
            Ok(ThisCore { sgi_base: 0 })
        }
        GicVersion::V3 => unsafe { gicv3::init_this_core(info.gicr_base as usize, info.gicr_size as usize) }
            .map(|sgi_base| ThisCore { sgi_base }),
    }
}

/// What [`init_this_core`] found for a secondary core: its GICv3 SGI_base
/// frame (0 on GICv2), the one place its PPIs are enabled.
#[derive(Clone, Copy)]
pub struct ThisCore {
    sgi_base: usize,
}

/// Enables the PPI or SGI `intid` (below 32) on THIS core, on a secondary:
/// GICv2's enable register for interrupts 0-31 is banked per core at one
/// address, GICv3's is in this core's own redistributor frame, `this`'s. A
/// PPI is per core by nature (the timer, 30), and so is whether a core
/// takes an SGI (`smp::KICK_SGI`), so [`enable_interrupt`], which the boot
/// core used for its own, enables nothing for another core.
///
/// # Safety
/// After [`init_this_core`] on this core, `this` being what it returned.
pub unsafe fn enable_ppi_this_core(intid: u32, this: ThisCore) {
    let info = info();
    match info.version {
        GicVersion::V2 => unsafe { gicv2::enable_interrupt(info.gicd_base as usize, intid) },
        GicVersion::V3 => unsafe { gicv3::enable_ppi_on(this.sgi_base, intid) },
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

/// An acknowledged interrupt: what the CPU interface's acknowledge read,
/// whole, for [`end_of_interrupt`] to write back. On GICv2 an SGI's value
/// carries the sending CPU in bits 12:10 and the end of interrupt must
/// carry it too, so the interrupt's ID is [`Acked::intid`], and the raw
/// value is reachable only through the end of interrupt (step 4(c), the
/// first interrupt whose acknowledge is not its ID).
#[derive(Clone, Copy)]
pub struct Acked {
    raw: u32,
}

impl Acked {
    /// The interrupt's ID (1023 when spurious).
    pub fn intid(self) -> u32 {
        match info().version {
            GicVersion::V2 => gicv2::intid_of(self.raw),
            GicVersion::V3 => self.raw,
        }
    }
}

/// Reads the highest-priority pending interrupt and acknowledges it.
///
/// # Safety
/// Must run after [`init`], from IRQ-handling context.
pub unsafe fn acknowledge() -> Acked {
    let raw = match info().version {
        GicVersion::V2 => unsafe { gicv2::acknowledge(info().gicc_base as usize) },
        GicVersion::V3 => unsafe { gicv3::acknowledge() },
    };
    Acked { raw }
}

/// Signals that the interrupt `acked` (from [`acknowledge`]) has been fully
/// handled.
///
/// # Safety
/// Must run after [`init`], with `acked` from a matching [`acknowledge`]
/// call.
pub unsafe fn end_of_interrupt(acked: Acked) {
    match info().version {
        GicVersion::V2 => unsafe { gicv2::end_of_interrupt(info().gicc_base as usize, acked.raw) },
        GicVersion::V3 => unsafe { gicv3::end_of_interrupt(acked.raw) },
    }
}

/// Records what another core needs to send THIS core an SGI, under `core`,
/// this core's MADT index: on GICv2 its CPU interface mask, which only the
/// core itself can read; on GICv3 nothing, since an SGI names its target by
/// the MADT's affinity. Every core, the boot core once its index is the
/// MADT's. `Err` says why this core cannot be sent one.
///
/// # Safety
/// After [`init`] (and on a secondary [`init_this_core`]), on the core
/// whose index is `core`.
pub unsafe fn note_this_core(core: usize) -> Result<(), &'static str> {
    let info = info();
    match info.version {
        GicVersion::V2 => unsafe { gicv2::note_this_core(info.gicd_base as usize, core) },
        GicVersion::V3 => Ok(()),
    }
}

/// Sends SGI `sgi` (0 to 15) to the core whose MADT index is `core`. Never
/// waits for it to be taken: a sender may hold the kernel lock, and the
/// target's handler takes that lock (`smp::kick`). Stores made before the
/// call are visible to the target's handler. `Err` says why the core
/// cannot be reached.
///
/// # Safety
/// After [`init`]; `sgi` below 16; `core` a core [`note_this_core`] ran on.
pub unsafe fn send_sgi(sgi: u32, core: usize) -> Result<(), &'static str> {
    let info = info();
    match info.version {
        GicVersion::V2 => unsafe { gicv2::send_sgi(info.gicd_base as usize, sgi, core) },
        GicVersion::V3 => {
            let cores = crate::madt::cores();
            if core >= cores.count.min(crate::madt::MAX_CORES) {
                return Err("no such core in the MADT");
            }
            unsafe { gicv3::send_sgi(sgi, cores.mpidr[core]) }
        }
    }
}

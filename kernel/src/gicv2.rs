//! GICv2 (Generic Interrupt Controller) backend — just enough to enable
//! one PPI (the timer tick, see `timer.rs`) and acknowledge/complete
//! interrupts as they arrive. Selected by `gic.rs`'s facade whenever
//! `madt::discover` reports `GicVersion::V2` (confirmed via the real ACPI
//! MADT now, not just the QEMU devicetree dump this module's addresses
//! were originally pinned down by — see `madt.rs`'s module doc comment).
//!
//! Addresses are no longer hardcoded here: `init` takes them as
//! parameters, sourced from `madt::GicInfo`. Originally confirmed for
//! QEMU by dumping its internal devicetree
//! (`qemu-system-aarch64 -machine virt,dumpdtb=...`, not anything our own
//! kernel reads at boot): `intc@8000000 { compatible =
//! "arm,cortex-a15-gic"; reg = <... 0x8000000 ... 0x8010000 ...> }` —
//! GICv2 (the `cortex-a15-gic` compatible string), distributor at
//! 0x08000000, CPU interface at 0x08010000 — and now separately
//! cross-checked against the real MADT via `madt.rs`, giving the same
//! values through a completely independent path.
//!
//! Register offsets cross-checked against Linux's
//! include/linux/irqchip/arm-gic.h rather than transcribed from memory,
//! same discipline as `mmu.rs`'s descriptor bits.

use core::ptr::{read_volatile, write_volatile};

const GICD_CTLR: usize = 0x000;
const GICD_ISENABLER: usize = 0x100; // + 4 * (intid / 32)

const GICC_CTLR: usize = 0x000;
const GICC_PMR: usize = 0x004;
const GICC_IAR: usize = 0x00c;
const GICC_EOIR: usize = 0x010;

const GICD_CTLR_ENABLE: u32 = 1 << 0;
const GICC_CTLR_ENABLE: u32 = 1 << 0;
const GICC_PMR_ALLOW_ALL: u32 = 0xff; // accept every priority

unsafe fn write_reg(base: usize, offset: usize, value: u32) {
    unsafe { write_volatile((base + offset) as *mut u32, value) };
}

unsafe fn read_reg(base: usize, offset: usize) -> u32 {
    unsafe { read_volatile((base + offset) as *const u32) }
}

/// Enables the distributor and this CPU's interface, with the priority
/// mask wide open (every priority accepted) since nothing here juggles
/// interrupt priorities yet.
///
/// # Safety
/// Must run after `mmu.rs`'s identity map is installed (`gicd_base`/
/// `gicc_base` must be mapped) and before unmasking IRQ in DAIF.
pub unsafe fn init(gicd_base: usize, gicc_base: usize) {
    unsafe {
        write_reg(gicd_base, GICD_CTLR, GICD_CTLR_ENABLE);
        write_reg(gicc_base, GICC_PMR, GICC_PMR_ALLOW_ALL);
        write_reg(gicc_base, GICC_CTLR, GICC_CTLR_ENABLE);
    }
}

/// Enables forwarding of `intid` (e.g. the timer PPI, 30) from the
/// distributor to CPU interfaces.
///
/// # Safety
/// Must run after [`init`].
pub unsafe fn enable_interrupt(gicd_base: usize, intid: u32) {
    let reg_offset = GICD_ISENABLER + 4 * ((intid / 32) as usize);
    let bit = 1u32 << (intid % 32);
    unsafe { write_reg(gicd_base, reg_offset, bit) };
}

/// Reads the highest-priority pending interrupt ID and acknowledges it
/// (removing it from the pending set) — must be paired with [`end_of_interrupt`]
/// once handled, or the GIC will never consider it complete.
///
/// # Safety
/// Must run after [`init`], from IRQ-handling context.
pub unsafe fn acknowledge(gicc_base: usize) -> u32 {
    unsafe { read_reg(gicc_base, GICC_IAR) }
}

/// Signals that the interrupt `intid` (as returned by [`acknowledge`]) has
/// been fully handled.
///
/// # Safety
/// Must run after [`init`], with `intid` from a matching [`acknowledge`]
/// call.
pub unsafe fn end_of_interrupt(gicc_base: usize, intid: u32) {
    unsafe { write_reg(gicc_base, GICC_EOIR, intid) };
}

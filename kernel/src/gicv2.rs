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
const GICD_ITARGETSR: usize = 0x800; // + intid (one target byte per intid)

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
        init_cpu_interface(gicc_base);
    }
}

/// This core's CPU interface (the GICC registers are banked per core at
/// one address): the priority mask open, the interface enabled. The
/// per-core half of [`init`], which a secondary core runs for itself.
///
/// # Safety
/// As [`init`].
pub unsafe fn init_cpu_interface(gicc_base: usize) {
    unsafe {
        write_reg(gicc_base, GICC_PMR, GICC_PMR_ALLOW_ALL);
        write_reg(gicc_base, GICC_CTLR, GICC_CTLR_ENABLE);
    }
}

/// This core's bit in a `GICD_ITARGETSR` byte: the banked registers
/// ITARGETSR0 to 7 (interrupts 0 to 31) read back as the reading core's
/// own mask in every byte, on a core whose byte is nonzero; the first
/// nonzero byte of the eight is it (Linux's `gic_get_cpumask` scans the
/// same way). A GIC on which every byte reads 0 gives 0x01, CPU 0, the
/// constant this used to be, and a line says so, since an SPI routed to a
/// mask of zero reaches no core and nothing would otherwise report it.
///
/// # Safety
/// After [`init`], on the core whose mask is wanted.
unsafe fn own_target_mask(gicd_base: usize) -> u32 {
    for i in 0..8 {
        let word = unsafe { read_reg(gicd_base, GICD_ITARGETSR + 4 * i) };
        for byte in 0..4 {
            let mask = (word >> (8 * byte)) & 0xff;
            if mask != 0 {
                return mask;
            }
        }
    }
    crate::console::println!("Ouroboros kernel: GICv2: this core's target mask reads 0 in ITARGETSR0-7; routing SPIs to CPU 0");
    0x01
}

/// Enables forwarding of `intid` from the distributor to CPU interfaces.
///
/// A PPI (intid < 32, e.g. the timer, 30) is per-CPU/banked and needs only
/// the enable bit - its target is implicitly this CPU. An SPI (intid >= 32,
/// e.g. a virtio-mmio device) is shared and, until [`GICD_ITARGETSR`] names
/// a target, reaches no core, so this routes an SPI to the core that
/// enables it (its own target mask, read back from the banked ITARGETSR
/// registers by [`own_target_mask`]) before enabling it. Priority
/// (`GICD_IPRIORITYR`) and trigger mode
/// (`GICD_ICFGR`) are left at reset: reset priority (0) passes the wide-open
/// `GICC_PMR` set in [`init`], and QEMU's virtio-mmio drives its line
/// level-style, matching the level reset default - so no extra config is
/// needed for this project's one SPI user.
///
/// # Safety
/// Must run after [`init`].
pub unsafe fn enable_interrupt(gicd_base: usize, intid: u32) {
    if intid >= 32 {
        // Route this SPI to THIS core (ITARGETSR is a CPU bitmask, one byte
        // per intid). Which bit this core is comes from ITARGETSR0, whose
        // bytes read back as the reading core's own mask (the GICv2 way to
        // learn one's interface number); it was 0x01, CPU 0, which is the
        // boot core only when the firmware started on CPU 0, and a boot on
        // another core would have sent every device interrupt to a core
        // that idles outside the scheduler (the high review of 4(b)).
        // Read-modify-write the containing word so the neighbouring intids'
        // targets are preserved.
        let me = unsafe { own_target_mask(gicd_base) };
        let word = GICD_ITARGETSR + ((intid as usize) & !3);
        let shift = (intid % 4) * 8;
        let mut targets = unsafe { read_reg(gicd_base, word) };
        targets &= !(0xffu32 << shift);
        targets |= me << shift;
        unsafe { write_reg(gicd_base, word, targets) };
    }
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

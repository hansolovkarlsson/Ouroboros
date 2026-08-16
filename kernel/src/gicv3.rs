//! GICv3 backend — selected by `gic.rs`'s facade whenever `madt::discover`
//! reports `GicVersion::V3` (the version Parallels almost certainly runs,
//! per Apple Silicon virtualization convention — see `madt.rs`'s module
//! doc comment for why this exists at all: `gicv2.rs`'s addresses and
//! register layout are QEMU-shaped and already confirmed unsafe on real
//! Parallels hardware).
//!
//! Real, material differences from `gicv2.rs`, each cross-checked against
//! Linux's `include/linux/irqchip/arm-gic-v3.h` rather than transcribed
//! from memory (same discipline `gicv2.rs`/`mmu.rs` already hold their
//! register-bit sourcing to):
//!
//! - **The CPU interface is system-register-based, not memory-mapped at
//!   all.** `ICC_SRE_EL1` must be set (and read back to confirm it stuck —
//!   this project's "verify, don't assume a system-register write took
//!   effect" discipline, established hard by `mmu.rs`'s TCR_EL1 work)
//!   before `ICC_*_EL1` accesses mean anything; then `ICC_PMR_EL1`
//!   (priority mask, same role as `gicv2.rs`'s `GICC_PMR`),
//!   `ICC_IGRPEN1_EL1` (enable Group 1), `ICC_IAR1_EL1` (acknowledge —
//!   read), `ICC_EOIR1_EL1` (end of interrupt — write).
//! - **PPI enable moves to the Redistributor, not the Distributor.**
//!   `GICD_ISENABLER` (what `gicv2.rs::enable_interrupt` writes) only
//!   covers SPIs (intid >= 32) in GICv3 — the timer PPI (30) is enabled
//!   via the SGI_base frame's `GICR_ISENABLER0` instead, at the *same*
//!   register offset (0x100) coincidentally, but a completely different
//!   base address.
//! - **Redistributor wake-up, no GICv2 equivalent at all.** Must clear
//!   `GICR_WAKER.ProcessorSleep` and poll `GICR_WAKER.ChildrenAsleep`
//!   until it clears before the redistributor accepts anything.
//! - **Finding *this* CPU's redistributor frame.** `madt::GicInfo`'s
//!   `gicr_base`/`gicr_size` describe a region that may hold more than
//!   one CPU's redistributor (QEMU's default `virt,gic-version=3` region
//!   is sized for many, confirmed via both a devicetree dump and this
//!   driver's own MADT parse — see `main.rs`'s MADT log line and
//!   CLAUDE.md's MADT/GICv3 scoping notes). `init` walks redistributor
//!   frames (stride `0x2_0000`, or `0x4_0000` if `GICR_TYPER.VLPIS` is
//!   set — a real possibility this driver checks for rather than
//!   assuming away) comparing each frame's `GICR_TYPER` affinity field
//!   against this CPU's own `MPIDR_EL1`, stopping at a match or at
//!   `GICR_TYPER`'s "Last" bit. Real discovery, not hardcoded to frame 0,
//!   even though this kernel is single-core — matches this project's
//!   existing discipline of discovering rather than assuming (e.g.
//!   `xhci.rs` scanning every port rather than trusting the first device
//!   found).

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicUsize, Ordering};

const GICR_TYPER: usize = 0x0008;
const GICR_WAKER: usize = 0x0014;
const SGI_BASE_OFFSET: usize = 0x1_0000;
const GICR_IGROUPR0: usize = 0x0080; // within the SGI_base frame
const GICR_ISENABLER0: usize = 0x0100; // within the SGI_base frame

const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

const GICR_TYPER_VLPIS: u64 = 1 << 1;
const GICR_TYPER_LAST: u64 = 1 << 4;

const REDIST_STRIDE: usize = 0x2_0000;
const REDIST_STRIDE_VLPIS: usize = 0x4_0000;

const ICC_SRE_SRE: u64 = 1 << 0;
const ICC_IGRPEN1_ENABLE: u64 = 1 << 0;
const ICC_PMR_ALLOW_ALL: u64 = 0xff;

// GICD_CTLR: enabling just bit 0 (this driver's first attempt, mirroring
// gicv2.rs's single GICD_CTLR_ENABLE bit) left the timer PPI silently
// undelivered - no fault, no crash, `uptime` just stuck at 0 ticks
// forever, confirmed via a `-d int` cross-check showing IRQs still
// firing system-wide but never reaching this kernel's own handler.
// Root cause, cross-checked against Linux's own gic_dist_init
// (irq-gic-v3.c): GICv3's GICD_CTLR bit 0 alone only enables Group 0
// (FIQ) delivery - Group 1 (IRQ, the group `icc_igrpen1_el1` in `init`
// below actually enables) needs its own bit, and one more
// (`ARE_NS`) is needed for correct affinity routing on a two-security-
// -state system. Linux ORs all three together rather than trying to
// detect which single-security-state-vs-two view applies at runtime,
// and this driver does the same - real, sourced values, not guessed.
const GICD_CTLR_ENABLE_G1: u32 = 1 << 0;
const GICD_CTLR_ENABLE_G1A: u32 = 1 << 1;
const GICD_CTLR_ARE_NS: u32 = 1 << 4;
const GICD_CTLR_RWP: u32 = 1 << 31;
const GICD_CTLR_INIT: u32 = GICD_CTLR_ENABLE_G1 | GICD_CTLR_ENABLE_G1A | GICD_CTLR_ARE_NS;

/// This CPU's own redistributor SGI_base frame, found once by [`init`] and
/// reused by [`enable_interrupt`] — `acknowledge`/`end_of_interrupt` need
/// no memory address at all, only the `ICC_*_EL1` system registers.
/// `AtomicUsize` for the same reason `exceptions.rs`'s `TICKS` counter is
/// an atomic rather than a bare `static mut` - this project's established
/// idiom for kernel-global state, even though nothing here is genuinely
/// concurrent (single core, and `init` always completes before any IRQ
/// that could call the others is unmasked).
static SGI_BASE: AtomicUsize = AtomicUsize::new(0);

unsafe fn read_reg32(base: usize, offset: usize) -> u32 {
    unsafe { read_volatile((base + offset) as *const u32) }
}

unsafe fn write_reg32(base: usize, offset: usize, value: u32) {
    unsafe { write_volatile((base + offset) as *mut u32, value) };
}

unsafe fn read_reg64(base: usize, offset: usize) -> u64 {
    unsafe { read_volatile((base + offset) as *const u64) }
}

fn read_mpidr() -> u64 {
    let mpidr: u64;
    unsafe {
        asm!("mrs {0}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags));
    }
    mpidr
}

/// Packs `MPIDR_EL1`'s Aff0/Aff1/Aff2/Aff3 fields into the same 32-bit
/// layout `GICR_TYPER`'s Affinity field uses (Aff3:Aff2:Aff1:Aff0, one
/// byte each) — the standard comparison every GICv3 redistributor probe
/// (this driver's `init` included) has to do to find its own frame.
fn mpidr_affinity_packed(mpidr: u64) -> u32 {
    let aff0 = (mpidr & 0xff) as u32;
    let aff1 = ((mpidr >> 8) & 0xff) as u32;
    let aff2 = ((mpidr >> 16) & 0xff) as u32;
    let aff3 = ((mpidr >> 32) & 0xff) as u32;
    (aff3 << 24) | (aff2 << 16) | (aff1 << 8) | aff0
}

/// Finds this CPU's own redistributor frame within `[gicr_base,
/// gicr_base + gicr_size)`, returning its RD_base address. See this
/// module's doc comment for why the walk (not a fixed frame-0 guess) and
/// the VLPIS-dependent stride matter.
fn find_own_redistributor(gicr_base: usize, gicr_size: usize) -> Option<usize> {
    let want = mpidr_affinity_packed(read_mpidr());
    let mut offset = 0usize;
    while offset < gicr_size {
        let rd_base = gicr_base + offset;
        let typer = unsafe { read_reg64(rd_base, GICR_TYPER) };
        let affinity = (typer >> 32) as u32;
        if affinity == want {
            return Some(rd_base);
        }
        let stride = if typer & GICR_TYPER_VLPIS != 0 {
            REDIST_STRIDE_VLPIS
        } else {
            REDIST_STRIDE
        };
        if typer & GICR_TYPER_LAST != 0 {
            break;
        }
        offset += stride;
    }
    None
}

/// Wakes the redistributor (clears `ProcessorSleep`, polls
/// `ChildrenAsleep` until clear — no GICv2 equivalent, see this module's
/// doc comment), enables the distributor, routes the CPU interface to
/// system registers (`ICC_SRE_EL1`, read back to confirm it stuck), and
/// opens the priority mask/Group 1 enable.
///
/// # Safety
/// Must run after `mmu.rs`'s identity map is installed (`gicd_base` and
/// the `[gicr_base, gicr_base + gicr_size)` region must be mapped) and
/// before unmasking IRQ in DAIF. Panics (via a failed `Option::expect`,
/// same fail-fast posture `loader.rs` uses for its own "nothing to run"
/// case) if this CPU's own redistributor frame can't be found in the
/// given region — a real, confirmed-safe address that doesn't actually
/// contain a matching frame is a genuine discovery-logic bug, not
/// something to silently limp past.
pub unsafe fn init(gicd_base: usize, gicr_base: usize, gicr_size: usize) {
    let rd_base =
        find_own_redistributor(gicr_base, gicr_size).expect("no matching GICv3 redistributor frame found for this CPU's MPIDR_EL1 in the discovered GICR region");
    let sgi_base = rd_base + SGI_BASE_OFFSET;
    SGI_BASE.store(sgi_base, Ordering::Relaxed);

    unsafe {
        // Wake the redistributor before touching anything else in it.
        let mut waker = read_reg32(rd_base, GICR_WAKER);
        waker &= !GICR_WAKER_PROCESSOR_SLEEP;
        write_reg32(rd_base, GICR_WAKER, waker);
        while read_reg32(rd_base, GICR_WAKER) & GICR_WAKER_CHILDREN_ASLEEP != 0 {}

        // Configure every SGI/PPI (including the timer PPI) as
        // non-secure Group 1 - the group `icc_igrpen1_el1` below actually
        // enables. Without this, a PPI defaults to Group 0 (FIQ, not
        // IRQ) and the tick never reaches this kernel's IRQ handler at
        // all, even though the redistributor/distributor/CPU-interface
        // setup below all "succeed" with no fault - a real bug found
        // exactly this way on the first real test (uptime stuck at 0
        // ticks, zero aborts in a -d int cross-check). Cross-checked
        // against Linux's own gic_cpu_init (irq-gic-v3.c), which writes
        // this exact value for this exact reason.
        write_reg32(sgi_base, GICR_IGROUPR0, 0xffff_ffff);

        // Distributor: unlike gicv2.rs, this needs more than one bit -
        // see GICD_CTLR_INIT's own doc comment for the real bug this
        // fixed. Wait for the write to actually complete (GICD_CTLR.RWP)
        // before touching anything else, same "verify, don't assume a
        // register write took effect" discipline as the ICC_SRE_EL1
        // read-back below.
        write_reg32(gicd_base, 0x000, GICD_CTLR_INIT);
        while read_reg32(gicd_base, 0x000) & GICD_CTLR_RWP != 0 {}

        // CPU interface: route to system registers, then verify the
        // write actually stuck rather than assuming it did (some
        // hypervisors trap/restrict this - see this module's doc
        // comment).
        asm!("msr icc_sre_el1, {0}", "isb", in(reg) ICC_SRE_SRE, options(nostack, preserves_flags));
        let sre: u64;
        asm!("mrs {0}, icc_sre_el1", out(reg) sre, options(nomem, nostack, preserves_flags));
        assert!(
            sre & ICC_SRE_SRE != 0,
            "ICC_SRE_EL1.SRE did not stick - system-register GIC access unavailable"
        );

        asm!("msr icc_pmr_el1, {0}", in(reg) ICC_PMR_ALLOW_ALL, options(nomem, nostack, preserves_flags));
        asm!("msr icc_igrpen1_el1, {0}", "isb", in(reg) ICC_IGRPEN1_ENABLE, options(nostack, preserves_flags));
    }
}

/// Enables forwarding of `intid` (e.g. the timer PPI, 30) via this CPU's
/// own redistributor frame (found by [`init`]) — unlike GICv2, the
/// distributor plays no part in enabling a PPI.
///
/// # Safety
/// Must run after [`init`].
pub unsafe fn enable_interrupt(intid: u32) {
    let sgi_base = SGI_BASE.load(Ordering::Relaxed);
    let bit = 1u32 << (intid % 32);
    unsafe { write_reg32(sgi_base, GICR_ISENABLER0, bit) };
}

/// Reads the highest-priority pending interrupt ID and acknowledges it —
/// must be paired with [`end_of_interrupt`], same contract as
/// `gicv2.rs::acknowledge`. Pure system-register access, no memory
/// address involved at all.
///
/// # Safety
/// Must run after [`init`], from IRQ-handling context.
pub unsafe fn acknowledge() -> u32 {
    let iar: u64;
    unsafe {
        asm!("mrs {0}, icc_iar1_el1", out(reg) iar, options(nomem, nostack, preserves_flags));
    }
    (iar & 0xff_ffff) as u32
}

/// Signals that the interrupt `intid` (as returned by [`acknowledge`]) has
/// been fully handled.
///
/// # Safety
/// Must run after [`init`], with `intid` from a matching [`acknowledge`]
/// call.
pub unsafe fn end_of_interrupt(intid: u32) {
    unsafe {
        asm!("msr icc_eoir1_el1, {0}", in(reg) intid as u64, options(nomem, nostack, preserves_flags));
    }
}

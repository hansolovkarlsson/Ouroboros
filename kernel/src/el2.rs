//! The exception level the firmware hands the kernel off at, and the drop
//! from EL2 to EL1 when it is EL2.
//!
//! This kernel runs at EL1 after `exit_boot_services`: the vector base, the
//! translation tables, the timer and the first `eret` into task 0 are all
//! `_EL1` state. QEMU and Parallels hand off at EL1. The Raspberry Pi's
//! firmware (pftf over TF-A) hands off at **EL2**, found on 2026-10-01 by
//! the early fault reporter's first dump on a Pi 4 (`SPSR` in EL2h, the
//! fault through the firmware's vectors after `exceptions::install()`), and
//! reproduced on QEMU with `-machine virt,virtualization=on` (`make
//! run-el2`). At EL2 every one of those writes lands in a register the
//! running level does not use, and the boot keeps running on the firmware's
//! EL2 tables and vectors while its log says otherwise: the MMU's own
//! hardware-walk check passed there on tables nobody was running on.
//!
//! [`current_el`] turns that into a kernel fact the log states on every
//! boot. [`drop_to_el1`] is the fix, the shape every general-purpose
//! kernel's entry code has (Linux's `el2_setup`): at EL2, set up EL1 as a
//! plain AArch64 guest of itself, no stage 2, no traps, interrupts routed
//! to EL1, then `eret` into it. Staying at EL2 was weighed and refused:
//! without VHE (the Cortex-A72 is ARMv8.0) EL0 cannot run under EL2's
//! translation regime, so the kernel's whole EL0 story would have to be
//! rebuilt. The plan is `docs/roadmap/roadmap-el1-drop.md`.
//!
//! ## Prepare EL1 at EL2, then `eret` into a running MMU
//!
//! The naive drop, `eret` to EL1 with its MMU off and switch on from
//! there, opens a window where the kernel runs with every access
//! Device-nGnRnE while dirty lines written under the firmware's cacheable
//! EL2 mapping (the stack, the statics, the memory map) sit in the cache
//! unseen. So the drop takes the identity map `mmu.rs` has already BUILT
//! (its build half writes memory only) and sets EL1's regime to it from
//! EL2, `SCTLR_EL1` with the MMU and both caches on; the `eret` then lands
//! at the next instruction, on the same stack, with the same cacheability
//! it had a moment before, on the kernel's own tables, with the kernel's
//! own vectors (`exceptions::install` wrote `VBAR_EL1` from EL2 already,
//! so there is no instant at EL1 without them).
//!
//! `SCTLR_EL1` is written whole, not read-modify-written: its reset value
//! on the EL2 handoff is not the firmware's EL1 configuration. The value
//! is the one QEMU's firmware leaves on an EL1 handoff, read on 2026-10-02
//! (`0x30d0198d`: the ARMv8.0 RES1 bits, M, C, SA, ITD, SED, I), so the EL1
//! the drop builds is the EL1 the EL1 platforms hand off; `tasks.rs` adds
//! nTWE/nTWI on both paths as before.

use crate::mmu::El1Regime;
use core::arch::asm;

/// The exception level this code runs at, 0 to 3, from `CurrentEL`
/// (bits 3:2). Readable at EL1 and above.
pub fn current_el() -> u64 {
    let el: u64;
    unsafe { asm!("mrs {0}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags)) };
    (el >> 2) & 0b11
}

/// Whether the GIC's system register interface (`ICC_*_EL1`, GICv3 and
/// later) is implemented: `ID_AA64PFR0_EL1.GIC` (bits 27:24) nonzero. The
/// hardware's own answer, read rather than taken from the MADT: at EL2
/// those registers reach EL1 only through `ICC_SRE_EL2.Enable`, and on a
/// GICv2 machine (the Raspberry Pi) `ICC_SRE_EL2` does not exist and the
/// write would be an undefined instruction.
fn gic_sysregs_implemented() -> bool {
    let pfr0: u64;
    unsafe { asm!("mrs {0}, id_aa64pfr0_el1", out(reg) pfr0, options(nomem, nostack, preserves_flags)) };
    (pfr0 >> 24) & 0xf != 0
}

/// `SCTLR_EL1` for the drop: QEMU's firmware's EL1 value (see the module
/// doc). RES1 `0x30d00800` | M (0) | C (2) | SA (3) | ITD (7) | SED (8) | I (12).
const SCTLR_EL1_MMU_ON: u64 = 0x30d0_198d;
/// `HCR_EL2.RW`: EL1 is AArch64. Nothing else: no `VM` (no stage 2), no
/// `TGE`, no `IMO`/`FMO`/`AMO` (interrupts route to EL1, where the firmware
/// had them routed to EL2), no traps.
const HCR_EL2_RW: u64 = 1 << 31;
/// `CNTHCTL_EL2.EL1PCTEN | EL1PCEN` (with `HCR_EL2.E2H` 0): EL1 may read
/// the physical counter and use the physical timer, which `timer.rs` does.
const CNTHCTL_EL2_EL1_PHYS: u64 = (1 << 0) | (1 << 1);
/// `CPTR_EL2` with no FP/SIMD trap: the RES1 pattern, `TFP` (bit 10) clear.
const CPTR_EL2_NO_TRAPS: u64 = 0x33ff;
/// `CPACR_EL1.FPEN = 0b11`: EL0 and EL1 may use FP/SIMD. On an EL1 handoff
/// the firmware had already allowed it; here nobody has.
const CPACR_EL1_FPEN: u64 = 0b11 << 20;
/// `SPSR_EL2` for the `eret`: D, A, I, F masked, M = EL1h (`0b0101`). The
/// kernel never runs at EL1 unmasked; the first `eret` into task 0 is what
/// unmasks, for EL0 only (`synccell.rs`).
const SPSR_EL1H_MASKED: u64 = 0x3c5;
/// `ICC_SRE_EL2.SRE | Enable`: the system register interface at EL2, and
/// EL1's access to its own (`gicv3.rs` writes `ICC_SRE_EL1` later).
const ICC_SRE_EL2_SRE_ENABLE: u64 = (1 << 0) | (1 << 3);

/// Drops from EL2 to EL1 and returns at EL1, on the same stack, with the
/// identity map `regime` describes live and the kernel's vectors installed.
/// Logs the drop once it has happened, through the console, which the
/// tables map.
///
/// # Safety
/// Must be called exactly once, at EL2, after `exit_boot_services` with
/// IRQs masked; `mmu::build_identity_map` must have written the tables
/// `regime` points at and `exceptions::install` must have written
/// `VBAR_EL1`. Nothing after this returns runs at EL2 again.
pub unsafe fn drop_to_el1(regime: El1Regime) {
    let gicv3 = gic_sysregs_implemented();
    if gicv3 {
        // SAFETY: the register exists, by the ID register read above.
        unsafe { asm!("msr icc_sre_el2, {0}", "isb", in(reg) ICC_SRE_EL2_SRE_ENABLE, options(nomem, nostack, preserves_flags)) };
    }
    unsafe {
        asm!(
            // EL1's translation regime, from EL2: nothing walks it until
            // the eret, so there is no "old tables, new attributes"
            // window here as there is in mmu::switch_full. The table
            // writes must be visible to the walker before it starts.
            "dsb ishst",
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr0}",
            "tlbi vmalle1",            // a regime that has never run: drop anything cached for it
            "dsb ish",
            "msr sctlr_el1, {sctlr}",  // MMU and caches on, from the first EL1 instruction
            // The hypervisor controls: a plain AArch64 EL1 with no stage
            // 2, no traps, its own interrupts, its own counter and timer.
            "msr hcr_el2, {hcr}",
            "msr cnthctl_el2, {cnthctl}",
            "msr cntvoff_el2, xzr",
            "msr cnthp_ctl_el2, xzr",  // the firmware's EL2 timer, still armed from boot services
            "msr cptr_el2, {cptr}",
            "msr hstr_el2, xzr",
            "msr cpacr_el1, {cpacr}",
            "isb",
            // The drop itself: same stack, next instruction, EL1h, masked.
            "mov {tmp}, sp",
            "msr sp_el1, {tmp}",
            "adr {tmp}, 9f",
            "msr elr_el2, {tmp}",
            "msr spsr_el2, {spsr}",
            "ic ialluis",              // I-cache lines tagged under the firmware's tables
            "dsb ish",
            "isb",
            "eret",
            "9:",
            mair = in(reg) regime.mair,
            tcr = in(reg) regime.tcr,
            ttbr0 = in(reg) regime.ttbr0,
            sctlr = in(reg) SCTLR_EL1_MMU_ON,
            hcr = in(reg) HCR_EL2_RW,
            cnthctl = in(reg) CNTHCTL_EL2_EL1_PHYS,
            cptr = in(reg) CPTR_EL2_NO_TRAPS,
            cpacr = in(reg) CPACR_EL1_FPEN,
            spsr = in(reg) SPSR_EL1H_MASKED,
            tmp = out(reg) _,
            options(nostack),
        );
    }
    crate::console::println!(
        "Ouroboros kernel: dropped from EL2 to EL{}, on our own tables and vectors (GIC system registers: {})",
        current_el(),
        if gicv3 { "enabled for EL1" } else { "not implemented" }
    );
}

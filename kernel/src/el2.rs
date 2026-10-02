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
//! ## What faulted before the drop
//!
//! On QEMU `virtualization=on` the boot reached `shell ready` and then
//! faulted at address 0 through the firmware's EL2 vectors. The dump said
//! what it was: `esr=0x86000007` (an instruction abort taken at the same
//! EL, translation fault), `far=0`, `elr=0`, `spsr=0x800003c9` (EL2h,
//! D/A/I/F masked). That is `tasks::start`'s first `eret` into task 0,
//! made at EL2: an `eret` restores `ELR_EL2` and `SPSR_EL2`, which the
//! kernel never wrote (it wrote `ELR_EL1` and `SPSR_EL1`), so it landed at
//! the firmware's stale `ELR_EL2`, address 0, still at EL2h with
//! interrupts masked, and fetched from an unmapped page. No interrupt was
//! involved; the earlier accounts (the firmware's EL2 timer, then the
//! kernel's tick routed by `IMO`) were inferences nobody checked against
//! the dump's fields. What closed it is the drop making that `eret` an EL1
//! `eret`.
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
//! The firmware's EL2 vectors stay where they are after the drop: nothing
//! is routed to them any more (no traps, no interrupts, `hvc` disabled by
//! `HCR_EL2.HCD`, and `smc` goes to EL3), so the kernel installs no EL2
//! table of its own. If a reason to trap to EL2 ever appears, a minimal
//! report-and-halt EL2 table belongs here.
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

/// Whether this boot drops to EL1 (it was handed off at EL2). The one
/// predicate: `mmu::switch_to_identity_map` drops on it, and `power.rs`
/// refuses an `hvc` conduit on it, so the two cannot disagree. True before
/// the drop and false after, since it reads the level.
pub fn drops_to_el1() -> bool {
    current_el() == 2
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

/// The `SCTLR_EL1` bits that are RES1 on an ARMv8.0 core and named fields
/// on later ones, all left at 1: `EOS` (11, exception exit is
/// context-synchronising), `TSCXT` (20), `EIS` (22, exception entry is
/// context-synchronising), `SPAN` (23: with FEAT_PAN, PSTATE.PAN is left
/// alone on exception entry; 0 would set PAN and fault every syscall that
/// reads an EL0 buffer by raw pointer), `nTLSMD` (28) and `LSMAOE` (29).
const SCTLR_EL1_EOS: u64 = 1 << 11;
const SCTLR_EL1_TSCXT: u64 = 1 << 20;
const SCTLR_EL1_EIS: u64 = 1 << 22;
const SCTLR_EL1_SPAN: u64 = 1 << 23;
const SCTLR_EL1_NTLSMD: u64 = 1 << 28;
const SCTLR_EL1_LSMAOE: u64 = 1 << 29;
const SCTLR_EL1_RES1: u64 =
    SCTLR_EL1_LSMAOE | SCTLR_EL1_NTLSMD | SCTLR_EL1_SPAN | SCTLR_EL1_EIS | SCTLR_EL1_TSCXT | SCTLR_EL1_EOS;
const SCTLR_EL1_M: u64 = 1 << 0;
const SCTLR_EL1_C: u64 = 1 << 2;
const SCTLR_EL1_SA: u64 = 1 << 3;
const SCTLR_EL1_ITD: u64 = 1 << 7;
const SCTLR_EL1_SED: u64 = 1 << 8;
const SCTLR_EL1_I: u64 = 1 << 12;
/// `SCTLR_EL1` for the drop, composed from the bits above: equal to
/// `0x30d0198d`, QEMU's firmware's EL1 value (see the module doc), which
/// the constant is checked against below rather than copied from.
const SCTLR_EL1_MMU_ON: u64 =
    SCTLR_EL1_RES1 | SCTLR_EL1_M | SCTLR_EL1_C | SCTLR_EL1_SA | SCTLR_EL1_ITD | SCTLR_EL1_SED | SCTLR_EL1_I;
const _: () = assert!(SCTLR_EL1_MMU_ON == 0x30d0_198d);
/// `HCR_EL2.RW | HCD`: EL1 is AArch64, and `hvc` is disabled, so one from
/// EL1 or EL0 is an undefined instruction reported through the kernel's
/// own vectors (EL1h halts, an EL0 task is killed) rather than a trap into
/// the firmware's leftover EL2 vectors with boot services gone. Nothing
/// else: no `VM` (no stage 2), no `TGE`, no `IMO`/`FMO`/`AMO` (interrupts
/// route to EL1, where the firmware had them routed to EL2), no `E2H`, no
/// traps. `smc` is untouched (no `TSC`): PSCI through TF-A still works.
const HCR_EL2_RW_HCD: u64 = (1 << 31) | (1 << 29);
/// `HCR_EL2.API | APK | ATA`: EL1 pointer authentication (FEAT_PAuth, bits
/// 41 and 40) and memory tagging (FEAT_MTE, bit 56) do not trap to EL2,
/// where nobody is home. RES0 on a core without the feature (the A72), so
/// set unconditionally, as Linux's non-VHE host flags set API and APK.
const HCR_EL2_NO_PAUTH_MTE_TRAPS: u64 = (1 << 41) | (1 << 40) | (1 << 56);
/// What the drop writes to `HCR_EL2`: `0xa0000000` plus the two feature
/// bits above. The records cite this constant rather than restating it.
const HCR_EL2_VALUE: u64 = HCR_EL2_RW_HCD | HCR_EL2_NO_PAUTH_MTE_TRAPS;
/// `MDCR_EL2`'s `HPMN` field (bits 4:0), the one part kept: the rest is
/// debug and PMU trap bits (`TDRA`, `TDOSA`, `TDA`, `TDE`, `TPM`, `TPMCR`),
/// all cleared so nothing EL1 does traps to EL2. `HPMN` is kept rather than
/// written because 0 is CONSTRAINED UNPREDICTABLE without FEAT_HPMN0.
const MDCR_EL2_HPMN_MASK: u64 = 0x1f;
/// `HCR_EL2.E2H`: with it set, every `_EL1` register name at EL2 reaches
/// the `_EL2` register instead (VHE, ARMv8.1). The Cortex-A72 has no VHE,
/// but the drop does not assume it: `HCR_EL2` is written first, and the
/// vectors again after it.
const HCR_EL2_E2H: u64 = 1 << 34;
/// `CNTHCTL_EL2.EL1PCTEN | EL1PCEN` (with `HCR_EL2.E2H` 0): EL1 may read
/// the physical counter and use the physical timer, which `timer.rs` does.
const CNTHCTL_EL2_EL1_PHYS: u64 = (1 << 0) | (1 << 1);
/// `CPTR_EL2` with no FP/SIMD trap: the ARMv8.0 RES1 pattern, `TFP` (bit
/// 10) clear. Bit 8 is `TZ` on a core with SVE and bit 12 `TSM` on one with
/// SME, RES1 otherwise, so [`cptr_el2_value`] clears each only when the ID
/// registers say the feature exists. The fine-grained trap registers
/// (FEAT_FGT) and `HCRX_EL2` are left at reset: none of the kernel's
/// targets has them, and on a board with EL3 the writes themselves trap
/// unless `SCR_EL3` allows them.
const CPTR_EL2_NO_TRAPS: u64 = 0x33ff;
const CPTR_EL2_TZ: u64 = 1 << 8;
const CPTR_EL2_TSM: u64 = 1 << 12;

/// [`CPTR_EL2_NO_TRAPS`], with `TZ` clear when `ID_AA64PFR0_EL1.SVE` (bits
/// 35:32) is nonzero and `TSM` clear when `ID_AA64PFR1_EL1.SME` (bits
/// 27:24) is, so SVE and SME at EL1 or EL0 do not trap to an EL2 with
/// nobody home.
fn cptr_el2_value() -> u64 {
    let (pfr0, pfr1): (u64, u64);
    unsafe {
        asm!("mrs {0}, id_aa64pfr0_el1", "mrs {1}, id_aa64pfr1_el1", out(reg) pfr0, out(reg) pfr1, options(nomem, nostack, preserves_flags));
    }
    let mut cptr = CPTR_EL2_NO_TRAPS;
    if (pfr0 >> 32) & 0xf != 0 {
        cptr &= !CPTR_EL2_TZ;
    }
    if (pfr1 >> 24) & 0xf != 0 {
        cptr &= !CPTR_EL2_TSM;
    }
    cptr
}
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
/// Must be called exactly once, at EL2, after `exit_boot_services` and
/// `exceptions::install`; `mmu::build_identity_map` must have written the
/// tables `regime` points at. Masks D, A, I and F itself and leaves them
/// masked, as the EL1 path does. Nothing after this returns runs at EL2
/// again.
pub unsafe fn drop_to_el1(regime: El1Regime) {
    // Masked, all four, as mmu::switch_full's first instruction masks them:
    // until the eret the firmware's HCR_EL2 routes FIQ and SError to its
    // own vectors, and nothing here can be interrupted half-way through
    // rewriting the hypervisor controls.
    let hcr_before: u64;
    unsafe {
        asm!("msr daifset, #0xf", "mrs {0}, hcr_el2", out(reg) hcr_before, options(nomem, nostack, preserves_flags));
    }
    // A VHE firmware (E2H set) is not supported: clearing E2H re-lays-out
    // the EL2 regime the firmware is still running (TCR_EL2's shape,
    // TTBR1_EL2 gone), so the stack and the console may vanish at the
    // write. Said BEFORE the write, while the console still works, and
    // then halted, which is the honest end of a boot this code was not
    // written for. The A72 has no VHE.
    if hcr_before & HCR_EL2_E2H != 0 {
        crate::console::println!(
            "Ouroboros kernel: handed off with HCR_EL2.E2H set ({hcr_before:#x}): a VHE firmware, which the drop to EL1 does not support"
        );
        crate::power::halt();
    }
    // HCR_EL2 before any `_EL1` register: with E2H the `_EL1` names below
    // would reach the `_EL2` registers of the level still running, and
    // HCD must be set before EL1 can run an hvc. The vectors need no write
    // here: exceptions::install wrote VBAR_EL1 from EL2 at the exit (the
    // one owner of that write; it refuses only under E2H, which halted
    // above), and the write takes effect at the eret.
    unsafe {
        asm!("msr hcr_el2, {0}", "isb", in(reg) HCR_EL2_VALUE, options(nomem, nostack, preserves_flags));
    }
    let gicv3 = gic_sysregs_implemented();
    if gicv3 {
        // SAFETY: the registers exist, by the ID register read above.
        // ICH_HCR_EL2 zeroed as Linux's el2 setup does: its trap bits
        // (TC, TALL0, TALL1, TDIR) would send EL1's ICC_* accesses to EL2.
        unsafe {
            asm!("msr icc_sre_el2, {0}", "isb", "msr ich_hcr_el2, xzr", in(reg) ICC_SRE_EL2_SRE_ENABLE, options(nomem, nostack, preserves_flags))
        };
    }
    let cptr = cptr_el2_value();
    unsafe {
        asm!(
            // EL1's translation regime, from EL2: nothing walks it until
            // the eret, so there is no "old tables, new attributes"
            // window here as there is in mmu::switch_full. The table
            // writes must be visible to the walker before it starts.
            "msr vttbr_el2, xzr",      // VMID 0 for every EL1&0 TLB entry and every tlbi, this one first
            "isb",
            "dsb ishst",
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr0}",
            "tlbi vmalle1",            // a regime that has never run under VMID 0: drop anything cached for it
            "dsb ish",
            "msr sctlr_el1, {sctlr}",  // MMU and caches on, from the first EL1 instruction
            // What EL1 reads as MIDR_EL1 and MPIDR_EL1 once an EL2 exists
            // (gicv3.rs finds its redistributor by MPIDR): the real ones.
            "mrs {tmp}, midr_el1",
            "msr vpidr_el2, {tmp}",
            "mrs {tmp}, mpidr_el1",
            "msr vmpidr_el2, {tmp}",
            // The rest of the hypervisor controls: no traps, EL1's own
            // counter and timer, the firmware's timer off.
            "msr cnthctl_el2, {cnthctl}",
            "msr cntvoff_el2, xzr",
            "msr cnthp_ctl_el2, xzr",  // the EL2 physical timer off, so nothing at EL2 is left armed
            "msr cptr_el2, {cptr}",
            "msr hstr_el2, xzr",
            "mrs {tmp}, mdcr_el2",     // debug/PMU traps off, HPMN kept
            "and {tmp}, {tmp}, {hpmn}",
            "msr mdcr_el2, {tmp}",
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
            cnthctl = in(reg) CNTHCTL_EL2_EL1_PHYS,
            cptr = in(reg) cptr,
            cpacr = in(reg) CPACR_EL1_FPEN,
            spsr = in(reg) SPSR_EL1H_MASKED,
            hpmn = in(reg) MDCR_EL2_HPMN_MASK,
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

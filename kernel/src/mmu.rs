//! Replaces UEFI firmware's translation tables with our own.
//!
//! The MMU is already on when we get control — UEFI runs with paging
//! active, and we've been running under firmware's own tables this whole
//! time. That's fine for a boot-services guest but not something to build a
//! kernel on: those tables aren't ours to reason about or extend (device
//! mappings for future drivers, W^X, guard pages, a user/kernel VA split
//! for whenever there's a userspace). This installs a minimal identity map
//! we control instead — same technique either way, just swapping which
//! tables TTBR0_EL1 points at while the MMU stays continuously enabled, no
//! disable/re-enable step.
//!
//! Deliberately coarse for this first cut: two kinds of 1GB block mapping,
//! nothing finer-grained yet.
//! - RAM: whatever the UEFI memory map actually reports (not a hardcoded
//!   address range — that exact mistake already burned this project once,
//!   with a hardcoded QEMU UART address that hard-crashed Parallels; see
//!   `uart.rs`'s history). Every 1GB block overlapping any "general RAM"
//!   descriptor (everything except MMIO/MMIO_PORT_SPACE/RESERVED/
//!   UNACCEPTED — that includes our own currently-executing LOADER_CODE/
//!   LOADER_DATA image and stack, not just CONVENTIONAL) gets mapped
//!   Normal, cacheable, executable, EL1-only.
//! - Device: the low 1GB (0x0-0x3FFF_FFFF), hardcoded as Device-nGnRnE,
//!   non-executable. This one *is* still a QEMU-shaped convention (low
//!   memory = MMIO, matching every console address discovered so far on
//!   both QEMU and Parallels), unlike the RAM range. Needed so the console,
//!   if one was discovered, keeps working after the table switch instead
//!   of becoming unmapped out from under it.
//!
//! ## Two levels (L0 -> L1), not one — a real bug, not a style choice
//!
//! The walk starts at L0 with `T0SZ = 20` (44-bit VA), matching *firmware's
//! own* TCR_EL1 configuration read back at the start of
//! [`install_identity_map`] — not a coincidence, and not simplifiable to a
//! single L1 table with `T0SZ = 25` (39-bit VA, which would still legally
//! cover every address this module maps). That single-table version was
//! the first thing tried here, and it hard-faulted: `ESR_EL1` decoded to a
//! Permission fault at translation level 2 on the very next instruction
//! fetch after the switch, then an identical fault on the exception vector
//! table itself, in an infinite loop — despite every table entry, MAIR_EL1,
//! and TCR_EL1 bit verified correct by hand against authoritative bit-layout
//! references (Linux's `pgtable-hwdef.h` and `arch/arm64/tools/sysreg`) and
//! independently re-derived with a Python script from the actual runtime
//! values. PXN/UXN weren't it either — removing them entirely changed
//! nothing. What actually fixed it was matching firmware's L0-start walk
//! instead of switching to a different starting level. Changing MAIR_EL1/
//! TCR_EL1/TTBR0_EL1 together while the MMU stays continuously enabled
//! appears to tolerate attribute changes but not a starting-level change,
//! at least on QEMU's cortex-a72 TCG model - not fully explained, but
//! directly, repeatably confirmed: identical setup with only T0SZ (and the
//! matching L0->L1 structure) changed is the entire diff between hard fault
//! and clean boot. Don't collapse this back to a single L1 table without
//! re-verifying against a real fault trace first.
//!
//! ## RAM is EL1-only, not EL0-accessible — a second unresolved mystery
//!
//! `syscall.rs` needs EL0 to be able to read/write/execute its demo task
//! and stack, both of which live in this same RAM block. The obvious fix
//! (`AP[2:1] = 01`, EL1+EL0 R/W, on `normal_block`) was tried and
//! extensively tested — and it hard-faults, in exactly the same shape as
//! the starting-level bug above: the very first instruction fetch after
//! the table switch takes a Permission fault and loops forever. Unlike
//! that bug, **this one was not resolved.** What was ruled out, each by a
//! direct, repeatable test, not by assumption:
//! - Wrong AP bit position — cross-checked bit-for-bit against Linux's
//!   `pgtable-hwdef.h` three separate times, including a from-scratch
//!   Python re-derivation of the actual runtime descriptor value. Correct
//!   every time.
//! - UXN — removing it entirely (on top of the AP change) changed nothing.
//! - PAN/EPAN (ARMv8.1, would plausibly block EL1 executing EL0-accessible
//!   memory) — cortex-a72 is ARMv8.0 and doesn't implement it; confirmed
//!   directly by trying to clear PSTATE.PAN via its raw system-register
//!   encoding (`S3_0_C4_C2_3`, since the assembler doesn't even recognize
//!   the named `PAN` mnemonic here) and getting an EC=0 "Unknown reason"
//!   trap — the register access itself is undefined on this CPU.
//! - `AP[2:1] = 10` (EL1 read-only, still no EL0) was also tried as a
//!   control: it correctly produces a *Data* Abort (denied write) instead
//!   of an *Instruction* Abort — proof the AP bit positions and their
//!   general effect are being interpreted correctly. Only the *specific*
//!   combination of "AP grants EL1 exec" + "AP != 00" faults on execute,
//!   which shouldn't be possible per the architecture (AP doesn't gate
//!   execute at all; only PXN/UXN do).
//! - Shareability (SH_INNER vs none) and `ic ialluis` (removed entirely) —
//!   both changed nothing.
//! - Granularity — rebuilt the RAM block as an L2 sub-table (2MB blocks,
//!   same AP=01 permissions) instead of a single L1 1GB block: identical
//!   failure. Not a huge-page-specific issue.
//!
//! Net result: reverted to AP=EL1-only (below) — proven working, including
//! through a full EL0 entry/exit round trip (`syscall.rs`'s `enter`
//! correctly drops to EL0 and correctly gets an Instruction Abort reported
//! back through the lower-EL vector when EL0 can't execute its own demo
//! task, exactly as expected with no EL0 access granted). The syscall/EL0
//! mechanism itself is verified sound; only "give EL0 some actual memory to
//! run from" remains unsolved. Next things worth trying, not yet attempted:
//! a genuinely separate (non-identity, non-kernel-image) EL0 region rather
//! than reusing kernel RAM; comparing against `-cpu max` or a different
//! QEMU CPU model in case this is cortex-a72-model-specific; or reporting
//! upstream to QEMU if it keeps reproducing on unrelated configurations.

use core::arch::asm;
use core::cell::UnsafeCell;

use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned, MemoryType};

const GIB: u64 = 1 << 30;
const ENTRIES_PER_TABLE: usize = 512;

#[repr(align(4096))]
struct Table(UnsafeCell<[u64; ENTRIES_PER_TABLE]>);

// SAFETY: single-core, no preemption - nothing else touches these
// concurrently. Each is populated once, before it's ever reachable via
// TTBR0_EL1.
unsafe impl Sync for Table {}

static L0_TABLE: Table = Table(UnsafeCell::new([0; ENTRIES_PER_TABLE]));
static L1_TABLE: Table = Table(UnsafeCell::new([0; ENTRIES_PER_TABLE]));

// Stage-1 descriptor bit positions (VMSAv8-64, 4KB granule), cross-checked
// against Linux's arch/arm64/include/asm/pgtable-hwdef.h rather than
// transcribed from memory: getting a bit position wrong here wouldn't
// necessarily fault, it'd just silently mistranslate.
const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1; // set = table descriptor; clear = block
const ATTRINDX_SHIFT: u64 = 2;
const AP_EL1_RW_ONLY: u64 = 0b00 << 6; // AP[2:1]: EL1 R/W, no EL0 access
const SH_INNER: u64 = 0b11 << 8;
const SH_OUTER: u64 = 0b10 << 8;
const AF: u64 = 1 << 10;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
const OUTPUT_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000; // bits 47:12

const MAIR_IDX_DEVICE_NGNRNE: u64 = 0;
const MAIR_IDX_NORMAL_WB: u64 = 1;
const MAIR_ATTR_DEVICE_NGNRNE: u64 = 0x00;
const MAIR_ATTR_NORMAL_WB: u64 = 0xff;

fn table_desc(next_level_addr: u64) -> u64 {
    DESC_VALID | DESC_TABLE | (next_level_addr & OUTPUT_ADDR_MASK)
}

fn device_block(base: u64) -> u64 {
    (base & !(GIB - 1))
        | DESC_VALID
        | (MAIR_IDX_DEVICE_NGNRNE << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_OUTER
        | AF
        | PXN
        | UXN
}

fn normal_block(base: u64) -> u64 {
    // AP_EL1_RW_ONLY, UXN: EL0-accessible RAM is not yet possible - see
    // the module doc comment above ("RAM is EL1-only... a second
    // unresolved mystery"). `syscall.rs`'s EL0 demo task currently cannot
    // execute for exactly that reason.
    (base & !(GIB - 1))
        | DESC_VALID
        | (MAIR_IDX_NORMAL_WB << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_INNER
        | AF
        | UXN
}

fn is_general_ram(ty: MemoryType) -> bool {
    !matches!(
        ty,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE | MemoryType::RESERVED | MemoryType::UNACCEPTED
    )
}

/// Builds the identity map and switches TTBR0_EL1 to it. The MMU stays
/// enabled throughout — no SCTLR_EL1.M toggle, just barriers around each
/// step so the table walker and TLB never see a half-updated config.
///
/// # Safety
/// Must be called after `exit_boot_services` (touches TCR_EL1/MAIR_EL1/
/// TTBR0_EL1 directly) and with `memory_map` being the map that call
/// returned. The discovered RAM span must actually cover the code
/// currently executing and its stack — true for any UEFI-loaded image,
/// since firmware reports it as LOADER_CODE/LOADER_DATA in the same map.
pub unsafe fn install_identity_map(memory_map: &MemoryMapOwned) {
    let l1 = unsafe { &mut *L1_TABLE.0.get() };

    // Device: fixed low 1GB. See module doc comment for why this one stays
    // a hardcoded convention rather than discovered.
    l1[0] = device_block(0);

    // RAM: real discovered span, not a guess.
    let mut min_addr = u64::MAX;
    let mut max_addr = 0u64;
    for desc in memory_map.entries().filter(|d| is_general_ram(d.ty)) {
        let start = desc.phys_start;
        let end = start + desc.page_count * 4096;
        min_addr = min_addr.min(start);
        max_addr = max_addr.max(end);
    }
    if min_addr <= max_addr {
        let first_block = min_addr / GIB;
        let last_block = (max_addr - 1) / GIB;
        for block in first_block..=last_block {
            let idx = block as usize;
            if idx < ENTRIES_PER_TABLE {
                l1[idx] = normal_block(block * GIB);
            }
        }
        crate::console::println!(
            "Ouroboros kernel: identity map RAM {min_addr:#x}-{max_addr:#x} (1GB blocks {first_block}..={last_block}), device 0x0-{:#x}",
            GIB - 1
        );
    }

    // L0[0] covers VA [0, 512GB) - everything this module ever maps fits
    // inside it, so it's the only L0 entry that needs to be valid.
    let l0 = unsafe { &mut *L0_TABLE.0.get() };
    l0[0] = table_desc(L1_TABLE.0.get() as u64);

    let mair_el1: u64 =
        (MAIR_ATTR_DEVICE_NGNRNE << (8 * MAIR_IDX_DEVICE_NGNRNE)) | (MAIR_ATTR_NORMAL_WB << (8 * MAIR_IDX_NORMAL_WB));

    // T0SZ=20 -> 44-bit input address space, walk starting at L0 -
    // deliberately matching firmware's own T0SZ, not a smaller/simpler
    // table config that would still legally cover our mapped range. See
    // the module doc comment: this isn't a style choice, a different
    // starting level was tried first and hard-faulted.
    //
    // TTBR1 fields are set to matching-but-unused values (EPD1=1 disables
    // TTBR1 walks entirely; we have no upper-half mapping and don't need
    // one yet). IPS comes from hardware (ID_AA64MMFR0_EL1.PARange), not a
    // guessed constant, same reasoning as the RAM span above.
    let parange: u64;
    unsafe {
        asm!("mrs {0}, id_aa64mmfr0_el1", out(reg) parange, options(nomem, nostack, preserves_flags));
    }
    let ips = parange & 0xf;

    let tcr_el1: u64 = 20            // T0SZ
        | (0b01 << 8)                // IRGN0: Normal WB RA WA
        | (0b01 << 10)               // ORGN0: Normal WB RA WA
        | (0b11 << 12)               // SH0: Inner Shareable
        // TG0 (bits 15:14) = 0b00, 4KB granule: the all-zero encoding, no
        // bits to OR in.
        | (20 << 16)                 // T1SZ (unused, EPD1=1)
        | (1 << 23)                  // EPD1: disable TTBR1 walks
        | (0b01 << 24)               // IRGN1 (unused)
        | (0b01 << 26)               // ORGN1 (unused)
        | (0b11 << 28)               // SH1 (unused)
        | (0b10 << 30)               // TG1: 4KB granule (TTBR1 encoding)
        | (ips << 32); // IPS: from hardware

    let ttbr0_el1 = L0_TABLE.0.get() as u64;

    // Masked and left masked: between the MAIR/TCR write and the TTBR0
    // switch below, code is still running under firmware's *old* tables
    // but *new* attribute-index semantics - a narrow, correctly-barriered
    // window, but not one worth letting an interrupt land in for free.
    // (main.rs deliberately re-unmasks IRQ later, right before dropping to
    // EL0 - this masking is only about surviving this specific transition.)
    unsafe {
        asm!(
            "msr daifset, #0xf",      // mask D, A, I, F
            "dsb ishst",              // table writes visible before use
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "isb",                    // MAIR/TCR visible before TTBR0 switch
            "msr ttbr0_el1, {ttbr0}",
            "isb",                    // TTBR0 switch takes effect
            "tlbi vmalle1",           // drop stale entries from firmware's tables
            "ic ialluis",             // drop stale I-cache lines tagged under the old tables
            "dsb ish",
            "isb",
            mair = in(reg) mair_el1,
            tcr = in(reg) tcr_el1,
            ttbr0 = in(reg) ttbr0_el1,
            options(nostack),
        );
    }
}

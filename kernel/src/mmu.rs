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
//!   Normal, cacheable, executable, EL1-only — except one small carved-out
//!   region for `tasks.rs`'s EL0 code, see below.
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
//! ## RAM is EL1-only, not EL0-accessible — a second mystery (resolved below)
//!
//! `tasks.rs` needs EL0 to be able to read/write/execute its demo task
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
//! Net result at the time: reverted to AP=EL1-only for all of RAM, proven
//! working including through a full EL0 entry/exit round trip.
//!
//! ## Resolution: isolate the EL0 region from actively-executing kernel code
//!
//! The fix was the first candidate listed above: give EL0 access to a
//! *separate* region instead of the same RAM block containing the kernel
//! code that's actively executing right after the table switch.
//! `tasks.rs` carves out one dedicated 8KB slot (`el0_region()`) holding
//! its two EL0 tasks and their stacks — nothing else from the kernel
//! shares it. 8KB, not something rounder like 2MB, because 2MB alignment
//! turned out to be unachievable at all on this target — see `tasks.rs`'s
//! module doc comment for the precisely-bisected `rustc`/PE-COFF limit
//! that forced this. Because 8KB is far smaller than this module's L2
//! (2MB) block granularity, giving *only* the EL0 region EL0 access
//! required a fourth translation table level: the one L2 slot that
//! contains it gets split into a real L3 sub-table (4KB pages,
//! `EL0_L3_TABLE`), where only the region's own page(s) get EL0 access
//! (`el0_page_4k`) and every other page in that same 2MB slot — the ~2MB
//! of surrounding kernel code/data that happens to share it — stays
//! EL1-only (`kernel_page_4k`), same as every other 2MB slot in the block
//! (`kernel_block_2m`) and every 1GB block that doesn't overlap the EL0
//! region at all (`normal_block`).
//!
//! **Confirmed working, sustained, not just "boots once":** the EL0 demo
//! task's real `svc` round-trip succeeds (`syscall from EL0 (number=0,
//! arg0=0x2a)`), and 14 consecutive timer ticks fired correctly over 20+
//! seconds afterward with no repeated faults — meaning EL0 reached and
//! stayed in its post-syscall idle loop rather than faulting again.
//! Cross-checked against QEMU's own `-d int` trace for the same kind of
//! run: exactly one `[SVC]` exception (the single syscall the demo task
//! makes) and zero aborts across the whole session. (Getting a clean idle
//! loop also needed one more fix, now in `tasks.rs`: EL0's own `wfe`
//! traps to EL1 by default — `SCTLR_EL1.nTWE`/`nTWI`, unrelated to the
//! mapping work here.)
//!
//! ## Since generalized to two independent regions, not one shared 8KB slot
//!
//! The paragraphs above describe the original shape: one 8KB compile-time
//! static, holding both EL0 tasks, isolated via one L2/L3 split. Once task
//! 0 became a program loaded from disk at a runtime-determined address
//! (`loader.rs`) instead of a compile-time constant, that stopped fitting
//! the "one region" model — task 0's region and task 1's small idle region
//! (`tasks.rs::IdleRegion`) are now unrelated allocations that could
//! easily land in different 1GB blocks or 2MB slots. [`install_identity_map`]
//! takes an array of regions instead of one tuple, and `EL0_L2_TABLES`/
//! `EL0_L3_TABLES` are both sized for up to [`MAX_EL0_REGIONS`] independent
//! splits rather than one. The underlying technique (walk down to L3 only
//! for the slot(s) that need it, everything else stays a coarse block) is
//! unchanged — see `MAX_EL0_REGIONS`'s own doc comment for the alignment
//! invariant that keeps this from needing to handle a *single* region
//! spanning multiple slots, which would be a bigger change.

use core::arch::asm;
use core::cell::UnsafeCell;

use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned, MemoryType};

const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 * 1024 * 1024;
const ENTRIES_PER_TABLE: usize = 512;

#[repr(align(4096))]
struct Table(UnsafeCell<[u64; ENTRIES_PER_TABLE]>);

// SAFETY: single-core, no preemption - nothing else touches these
// concurrently. Each is populated once, before it's ever reachable via
// TTBR0_EL1.
unsafe impl Sync for Table {}

static L0_TABLE: Table = Table(UnsafeCell::new([0; ENTRIES_PER_TABLE]));
static L1_TABLE: Table = Table(UnsafeCell::new([0; ENTRIES_PER_TABLE]));

// Two independent EL0 regions now, not one shared region split in half:
// task 0's loaded-program region (loader.rs, arbitrary size, 2MB-aligned
// by construction - see its module doc comment) and task 1's small fixed
// idle region (tasks.rs). Each is independently guaranteed to fit within
// one 2MB slot, so at most 2 slots (and therefore at most 2 1GB blocks,
// and at most 2 L3 splits) can ever need EL0 handling - the same
// "generous, two only if it straddles a boundary" reasoning as before,
// just now covering "two independent small regions" instead of "one
// region that might straddle". If either count is ever exceeded (it
// shouldn't be, with exactly two regions each pre-aligned not to
// straddle), the affected slot fails safe to EL1-only rather than
// corrupting a table - see the WARNING branches in
// `install_identity_map`.
const MAX_EL0_REGIONS: usize = 2;
const MAX_EL0_L2_TABLES: usize = 2;
const MAX_EL0_L3_TABLES: usize = 2;

static EL0_L2_TABLES: [Table; MAX_EL0_L2_TABLES] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
];

static EL0_L3_TABLES: [Table; MAX_EL0_L3_TABLES] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
];

// Stage-1 descriptor bit positions (VMSAv8-64, 4KB granule), cross-checked
// against Linux's arch/arm64/include/asm/pgtable-hwdef.h rather than
// transcribed from memory: getting a bit position wrong here wouldn't
// necessarily fault, it'd just silently mistranslate.
const DESC_VALID: u64 = 1 << 0;
const DESC_TABLE: u64 = 1 << 1; // set = table descriptor; clear = block
const ATTRINDX_SHIFT: u64 = 2;
const AP_EL1_RW_ONLY: u64 = 0b00 << 6; // AP[2:1]: EL1 R/W, no EL0 access
const AP_EL1_EL0_RW: u64 = 0b01 << 6; // AP[2:1]: EL1 R/W, EL0 R/W too
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

/// Plain 1GB block, EL1-only: used for every RAM block that doesn't
/// overlap `tasks::el0_region()`.
fn normal_block(base: u64) -> u64 {
    (base & !(GIB - 1))
        | DESC_VALID
        | (MAIR_IDX_NORMAL_WB << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_INNER
        | AF
        | UXN
}

/// 2MB block, EL1-only - same permissions as `normal_block`, just at L2
/// granularity. Used for every 2MB slot in a split block *except* the one
/// EL0 region.
fn kernel_block_2m(base: u64) -> u64 {
    (base & !(MIB2 - 1))
        | DESC_VALID
        | (MAIR_IDX_NORMAL_WB << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_INNER
        | AF
        | UXN
}

/// 4KB page, EL1-only - same permissions as `kernel_block_2m`, just at L3
/// granularity. Used for every page in the one L2 slot that gets split for
/// `tasks::el0_region()`, except the page(s) the region itself occupies.
/// Valid L3 entries always have bits[1:0] = 0b11 - the same bit pattern
/// `DESC_TABLE` uses at L0-L2 to mean "table", reinterpreted by hardware
/// as "page" at the last level. Reusing the constant here is intentional,
/// not a copy-paste mistake.
fn kernel_page_4k(base: u64) -> u64 {
    (base & !0xfff)
        | DESC_VALID
        | DESC_TABLE
        | (MAIR_IDX_NORMAL_WB << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_INNER
        | AF
        | UXN
}

/// 4KB page, EL1+EL0 R/W and executable - used for exactly the page(s)
/// `tasks::el0_region()` occupies.
fn el0_page_4k(base: u64) -> u64 {
    (base & !0xfff)
        | DESC_VALID
        | DESC_TABLE
        | (MAIR_IDX_NORMAL_WB << ATTRINDX_SHIFT)
        | AP_EL1_EL0_RW
        | SH_INNER
        | AF
}

fn is_general_ram(ty: MemoryType) -> bool {
    !matches!(
        ty,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE | MemoryType::RESERVED | MemoryType::UNACCEPTED
    )
}

fn overlaps_any(regions: &[(u64, u64); MAX_EL0_REGIONS], start: u64, end: u64) -> bool {
    regions.iter().any(|&(region_start, region_size)| {
        let region_end = region_start + region_size;
        region_start < end && region_end > start
    })
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
/// `el0_regions` are the (start, size) pairs that get EL0 access -
/// everything else in RAM stays EL1-only. Each must independently fit
/// within one 2MB-aligned slot (true by construction: `loader.rs`
/// over-allocates and trims to guarantee this for task 0's loaded
/// program, and `tasks.rs`'s `IdleRegion` is a single 4KB page, which can
/// never straddle a 2MB boundary regardless of where it lands) - see
/// `MAX_EL0_REGIONS`'s doc comment for what happens if that's ever
/// violated.
///
/// `framebuffer`, if `Some((base, size))`, is a GOP framebuffer
/// (`framebuffer::discover`) that needs to stay mapped and writable after
/// this switch too (`fbconsole.rs`). Most of the time this needs no extra
/// work at all: on QEMU's `ramfb`, the framebuffer address already falls
/// inside the discovered-RAM span below, so the ordinary RAM loop already
/// covers it (Normal WB, EL1-only - fine, nothing but this kernel's own
/// EL1 code ever touches it). Only if its containing 1GB block is *still*
/// unmapped after that loop - real hardware might genuinely have a
/// framebuffer outside the RAM span reported by the memory map, unverified
/// either way since this has only run against QEMU so far - does this add
/// one more Device-nGnRnE block for it, the same convention as the fixed
/// low-1GB device block above just at whatever address the framebuffer
/// actually reports.
pub unsafe fn install_identity_map(
    memory_map: &MemoryMapOwned,
    el0_regions: [(u64, u64); MAX_EL0_REGIONS],
    framebuffer: Option<(u64, u64)>,
) {
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
        let mut next_el0_l2_table = 0usize;
        let mut next_el0_l3_table = 0usize;
        for block in first_block..=last_block {
            let idx = block as usize;
            if idx >= ENTRIES_PER_TABLE {
                continue;
            }
            let block_start = block * GIB;
            let block_end = block_start + GIB;
            let overlaps_el0 = overlaps_any(&el0_regions, block_start, block_end);

            if overlaps_el0 && next_el0_l2_table < MAX_EL0_L2_TABLES {
                let l2 = unsafe { &mut *EL0_L2_TABLES[next_el0_l2_table].0.get() };
                for (i, entry) in l2.iter_mut().enumerate() {
                    let sub_base = block_start + (i as u64) * MIB2;
                    let sub_end = sub_base + MIB2;
                    let is_el0_slot = overlaps_any(&el0_regions, sub_base, sub_end);
                    if is_el0_slot && next_el0_l3_table < MAX_EL0_L3_TABLES {
                        // This one 2MB slot contains an (much smaller) EL0
                        // region - split it further into 4KB pages so only
                        // the region's own pages get EL0 access, not the
                        // other ~2MB of kernel code/data (or the other EL0
                        // region, if it happens to share this slot) that
                        // shares it.
                        let l3 = unsafe { &mut *EL0_L3_TABLES[next_el0_l3_table].0.get() };
                        for (j, page) in l3.iter_mut().enumerate() {
                            let page_base = sub_base + (j as u64) * 4096;
                            let page_end = page_base + 4096;
                            let is_el0_page = overlaps_any(&el0_regions, page_base, page_end);
                            *page = if is_el0_page { el0_page_4k(page_base) } else { kernel_page_4k(page_base) };
                        }
                        *entry = table_desc(EL0_L3_TABLES[next_el0_l3_table].0.get() as u64);
                        next_el0_l3_table += 1;
                    } else {
                        if is_el0_slot {
                            crate::console::println!(
                                "Ouroboros kernel: WARNING: out of EL0 L3 tables, denying EL0 access to 2MB slot {sub_base:#x}"
                            );
                        }
                        *entry = kernel_block_2m(sub_base);
                    }
                }
                l1[idx] = table_desc(EL0_L2_TABLES[next_el0_l2_table].0.get() as u64);
                next_el0_l2_table += 1;
            } else {
                if overlaps_el0 {
                    crate::console::println!(
                        "Ouroboros kernel: WARNING: out of EL0 L2 tables, denying EL0 access to 1GB block {block_start:#x}"
                    );
                }
                l1[idx] = normal_block(block_start);
            }
        }
        crate::console::println!(
            "Ouroboros kernel: identity map RAM {min_addr:#x}-{max_addr:#x} (1GB blocks {first_block}..={last_block}), device 0x0-{:#x}, EL0 regions {el0_regions:x?}",
            GIB - 1
        );
    }

    // Framebuffer fallback - see this function's doc comment. Only fires
    // if the RAM loop above (and the fixed low-1GB device block) left the
    // framebuffer's own 1GB block unmapped.
    if let Some((fb_base, fb_size)) = framebuffer {
        if fb_size > 0 {
            let idx = (fb_base / GIB) as usize;
            if idx < ENTRIES_PER_TABLE && l1[idx] == 0 {
                l1[idx] = device_block(fb_base);
                crate::console::println!(
                    "Ouroboros kernel: framebuffer {fb_base:#x} (size {fb_size:#x}) outside RAM span, mapped as its own device block"
                );
            }
        }
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

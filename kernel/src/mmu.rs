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

/// One EL0 region slot per task (`tasks::NUM_TASKS`, kept in sync by
/// the array literals both sides carry) - and, since the per-task
/// page-tables milestone, also the number of translation-table
/// *views*: view i grants EL0 access to region i alone. Each region is
/// independently guaranteed to fit within one 2MB-aligned slot (see
/// the safety comments on `install_identity_map`), so a view needs
/// exactly one L2 split and one L3 split - its own region's.
const MAX_EL0_REGIONS: usize = 5;

// Per-task translation-table views (the per-task page-tables
// milestone): view i is the table set task i runs under - identical
// kernel/device mappings in every view, but EL0 access granted *only*
// to el0_regions[i], task i's own region. That's the entire
// enforcement mechanism: under view i, every other task's memory is
// ordinary EL1-only RAM, and an EL0 touch of it faults (and, per the
// fault-isolation milestone, kills only the toucher). One L1 per view
// rather than sharing: the L1 is where a view's one EL0-split block
// diverges from the others'. The *extra device* L1 tables further
// below stay shared - their entries are identical in every view.
static L0_TABLES: [Table; MAX_EL0_REGIONS] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
];
static L1_TABLES: [Table; MAX_EL0_REGIONS] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
];

/// The most `extra_devices` entries `install_identity_map` has ever been
/// called with - matches `main.rs`'s own fixed-size staging array
/// (framebuffer, xHCI BAR, GICD, GICR).
const MAX_EXTRA_DEVICES: usize = 4;

/// [`install_identity_map`]'s inputs, kept around after its first call so
/// a later runtime caller (`rebuild_with_el0_regions`, for
/// `tasks::spawn` - dynamic task creation) can rebuild the whole table
/// set again without needing UEFI boot services, long gone by then, to
/// reconstruct either of them. `MemoryMapOwned` isn't `Clone`, so this
/// takes real ownership (moved out of `install_identity_map`'s own
/// parameter) rather than copying - the caller only ever needs to supply
/// it once.
struct StoredMapCell(UnsafeCell<Option<MemoryMapOwned>>);
struct StoredExtraDevicesCell(UnsafeCell<([(u64, u64); MAX_EXTRA_DEVICES], usize)>);

// SAFETY: single-core; written once by `install_identity_map`'s first
// call, before any code that could race it exists, and only ever read
// (not mutated) afterward.
unsafe impl Sync for StoredMapCell {}
unsafe impl Sync for StoredExtraDevicesCell {}

static STORED_MEMORY_MAP: StoredMapCell = StoredMapCell(UnsafeCell::new(None));
static STORED_EXTRA_DEVICES: StoredExtraDevicesCell =
    StoredExtraDevicesCell(UnsafeCell::new(([(0, 0); MAX_EXTRA_DEVICES], 0)));

// One L0 entry (`L1_TABLE` above, always L0 index 0) covers the first
// 512GB of VA space (512 entries * 1GB each) - enough for every RAM
// span, the fixed low-1GB device block, and the framebuffer this project
// has ever seen. A real PCI 64-bit BAR doesn't respect that: QEMU's
// `virt` machine places its high MMIO window for 64-bit BARs starting
// exactly at 512GB (confirmed directly - the xHCI controller's BAR, from
// `pci::discover_xhci`, came back as `0x8000004000`, i.e. L0 index 1),
// and real hardware's own PCI resource allocator could in principle put
// a 64-bit BAR anywhere. `extra_devices` regions therefore aren't
// guaranteed to land in `L1_TABLE`'s span the way every other region
// this module maps is - these two spare L1 tables, allocated into
// whichever L0 index(es) an `extra_devices` entry actually needs, are
// what let `install_identity_map` reach those addresses instead of
// silently leaving them unmapped (which is exactly what happened before
// this existed: a Translation fault at level 0, `FAR_EL1` matching the
// BAR address exactly, on the very first read of an xHCI capability
// register).
// Bumped from 2 to 4 for the MADT/GICv3 work (see `madt.rs`/`gicv3.rs`):
// `main.rs`'s `extra_devices` can now carry up to four entries
// (framebuffer, xHCI BAR, GICD, GICR) instead of two, and unlike the
// framebuffer/xHCI addresses seen on QEMU so far, real Parallels
// hardware's GIC addresses are completely unconfirmed - nothing rules out
// GICD and GICR needing two more distinct L0 indices on top of whatever
// xHCI's BAR needs, and running out of slots here would silently leave a
// device unmapped, then hard-fault the moment `gic.rs` touches it (this
// kernel has no resumable EL1 synchronous-fault path - see
// `exceptions.rs`'s module doc comment). Cheap to size generously: each
// slot is one 4KB table.
const MAX_EXTRA_L1_TABLES: usize = 4;
static EXTRA_L1_TABLES: [Table; MAX_EXTRA_L1_TABLES] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
];

// One L2 + one L3 per *view* (not per region-in-a-shared-map, the
// pre-per-task-tables design): view i only ever fine-grains the single
// 1GB block and single 2MB slot containing its own region - every
// other task's region is, from view i's perspective, ordinary EL1-only
// RAM needing no split at all. A genuinely simpler shape than the old
// shared map's up-to-five-splits bookkeeping.

static EL0_L2_TABLES: [Table; MAX_EL0_REGIONS] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
];

static EL0_L3_TABLES: [Table; MAX_EL0_REGIONS] = [
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
    Table(UnsafeCell::new([0; ENTRIES_PER_TABLE])),
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

/// Whether one (start, size) region overlaps [start, end) - the only
/// overlap question a per-task view ever asks (about its own region).
fn overlaps(region: (u64, u64), start: u64, end: u64) -> bool {
    let (region_start, region_size) = region;
    let region_end = region_start + region_size;
    region_size != 0 && region_start < end && region_end > start
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
/// `extra_devices` are additional discovered device regions (`(base,
/// size)`) that need to stay mapped and accessible after this switch, on
/// top of the fixed low-1GB device block above - currently the GOP
/// framebuffer (`framebuffer::discover`, needed by `fbconsole.rs`) and the
/// xHCI controller's PCI BAR (`pci::discover_xhci`, needed by `xhci.rs`).
/// Most of the time a given region needs no extra work at all: on QEMU's
/// `ramfb`, the framebuffer address already falls inside the
/// discovered-RAM span below, so the ordinary RAM loop already covers it
/// (Normal WB, EL1-only - fine, nothing but this kernel's own EL1 code
/// ever touches it). Only if a region's containing 1GB block is *still*
/// unmapped after that loop - real hardware might genuinely have a device
/// outside the RAM span reported by the memory map - does this add one
/// more Device-nGnRnE block for it, the same convention as the fixed
/// low-1GB device block above just at whatever address the device
/// actually reports. Unlike the fixed low-1GB block (a QEMU-shaped
/// convention, confirmed unsafe to *assume* present on Parallels - see
/// `virtio_mmio.rs`), every address in `extra_devices` is independently
/// discovered (GOP protocol query, PCI config-space BAR read), not
/// guessed, so mapping it carries none of that risk regardless of platform.
///
/// Takes ownership of `memory_map` (not a reference) and keeps both it
/// and `extra_devices` around afterward - see [`rebuild_with_el0_regions`],
/// the counterpart this exists for: a later runtime caller (dynamic task
/// creation's `spawn` syscall) needs to rebuild the whole table set again
/// with one more `el0_regions` entry, long after UEFI boot services (and
/// therefore any way to reconstruct `memory_map` from scratch) are gone.
pub unsafe fn install_identity_map(
    memory_map: MemoryMapOwned,
    el0_regions: [(u64, u64); MAX_EL0_REGIONS],
    extra_devices: &[(u64, u64)],
) {
    let mut stored = [(0u64, 0u64); MAX_EXTRA_DEVICES];
    let count = extra_devices.len().min(MAX_EXTRA_DEVICES);
    stored[..count].copy_from_slice(&extra_devices[..count]);
    unsafe { *STORED_EXTRA_DEVICES.0.get() = (stored, count) };
    unsafe { *STORED_MEMORY_MAP.0.get() = Some(memory_map) };
    // Re-borrow from the stash rather than the original parameter (now
    // moved) - the rest of this function is unchanged either way.
    let memory_map = unsafe { (*STORED_MEMORY_MAP.0.get()).as_ref() }.unwrap();
    unsafe { build_tables(memory_map, el0_regions, extra_devices) };
}

/// The runtime counterpart to [`install_identity_map`]: rebuilds the
/// whole table set again, exactly the same operation, just with a
/// different (larger) `el0_regions` array and reusing the `memory_map`/
/// `extra_devices` [`install_identity_map`]'s first call already stashed.
/// This is `tasks::spawn`'s only way to make a newly-allocated program's
/// region EL0-accessible, since nothing after `exit_boot_services` can
/// supply a fresh `MemoryMapOwned` to call `install_identity_map` with
/// directly.
/// Deliberately *not* a new, incremental "just add one more mapping"
/// mechanism - this kernel already proved, at boot, that swapping
/// `TTBR0_EL1` to an entirely new table set while code is *actively
/// executing* under the old one is safe (that's how firmware's own
/// tables got replaced in the first place); doing that same proven
/// operation a second time, later, is lower-risk than inventing live
/// break-before-make remapping for a first version of this.
///
/// # Safety
/// `install_identity_map` must have already run at least once (its own
/// safety requirements otherwise apply identically) - the same
/// requirement `main.rs`'s single call site already satisfies for every
/// caller of this function, since none can run before boot completes.
/// Called from an SVC handler with interrupts masked throughout (true
/// for `syscall.rs`'s `spawn`, its only caller) - single-core, so no
/// other code can observe the table set mid-rebuild.
pub(crate) unsafe fn rebuild_with_el0_regions(el0_regions: [(u64, u64); MAX_EL0_REGIONS]) {
    let memory_map = unsafe { (*STORED_MEMORY_MAP.0.get()).as_ref() }
        .expect("install_identity_map must run before rebuild_with_el0_regions");
    let (extra_devices, count) = unsafe { *STORED_EXTRA_DEVICES.0.get() };
    unsafe { build_tables(memory_map, el0_regions, &extra_devices[..count]) };
}

/// The discovered general-RAM span `(min_addr, max_addr)` - the same
/// computation `build_tables` already does internally, exposed for
/// `tasks::init_runtime_allocator` to pick a safe starting point for
/// dynamically `spawn`ed programs' regions from. Requires
/// `install_identity_map` to have already stashed a `memory_map`, same
/// as `rebuild_with_el0_regions`.
pub(crate) fn ram_span() -> (u64, u64) {
    let memory_map =
        unsafe { (*STORED_MEMORY_MAP.0.get()).as_ref() }.expect("install_identity_map must run before ram_span");
    let mut min_addr = u64::MAX;
    let mut max_addr = 0u64;
    for desc in memory_map.entries().filter(|d| is_general_ram(d.ty)) {
        let start = desc.phys_start;
        let end = start + desc.page_count * 4096;
        min_addr = min_addr.min(start);
        max_addr = max_addr.max(end);
    }
    (min_addr, max_addr)
}

unsafe fn build_tables(memory_map: &MemoryMapOwned, el0_regions: [(u64, u64); MAX_EL0_REGIONS], extra_devices: &[(u64, u64)]) {
    // RAM: real discovered span, not a guess - computed once, used by
    // every view.
    let mut min_addr = u64::MAX;
    let mut max_addr = 0u64;
    for desc in memory_map.entries().filter(|d| is_general_ram(d.ty)) {
        let start = desc.phys_start;
        let end = start + desc.page_count * 4096;
        min_addr = min_addr.min(start);
        max_addr = max_addr.max(end);
    }

    // Extra discovered device regions - see `install_identity_map`'s doc
    // comment and `MAX_EXTRA_L1_TABLES`'s. Planned once, shared by every
    // view: an entry either lands inside a view's own L1 span (L0 index
    // 0 - each view's L1 gets it, if the RAM loop left that block
    // unmapped) or in a *shared* extra L1 table (L0 index != 0 - every
    // view's L0 points at the same table, since the entries are
    // identical across views).
    const L0_ENTRY_SIZE: u64 = GIB * ENTRIES_PER_TABLE as u64; // 512GB
    let mut extra_l1_owners = [usize::MAX; MAX_EXTRA_L1_TABLES]; // L0 index each pool slot covers
    let mut next_extra_l1_table = 0usize;
    // In-L1-span device blocks each view must add if RAM didn't cover
    // them: (l1 index, base).
    let mut l1_span_devices = [(usize::MAX, 0u64); MAX_EXTRA_DEVICES];
    let mut l1_span_device_count = 0usize;
    for &(base, size) in extra_devices {
        if size == 0 {
            continue;
        }
        let l0_idx = (base / L0_ENTRY_SIZE) as usize;
        let l1_idx = ((base / GIB) % ENTRIES_PER_TABLE as u64) as usize;
        if l0_idx >= ENTRIES_PER_TABLE {
            crate::console::println!("Ouroboros kernel: WARNING: device region {base:#x} is out of range for this identity map, leaving unmapped");
            continue;
        }
        if l0_idx == 0 {
            l1_span_devices[l1_span_device_count] = (l1_idx, base);
            l1_span_device_count += 1;
            continue;
        }
        let slot = match extra_l1_owners.iter().position(|&owner| owner == l0_idx) {
            Some(slot) => slot,
            None => {
                if next_extra_l1_table >= MAX_EXTRA_L1_TABLES {
                    crate::console::println!(
                        "Ouroboros kernel: WARNING: out of extra L1 tables, leaving device region {base:#x} unmapped"
                    );
                    continue;
                }
                let slot = next_extra_l1_table;
                next_extra_l1_table += 1;
                extra_l1_owners[slot] = l0_idx;
                slot
            }
        };
        let shared_l1 = unsafe { &mut *EXTRA_L1_TABLES[slot].0.get() };
        if shared_l1[l1_idx] == 0 {
            shared_l1[l1_idx] = device_block(base);
            crate::console::println!(
                "Ouroboros kernel: device region {base:#x} (size {size:#x}) outside RAM span, mapped as its own device block (L0 index {l0_idx})"
            );
        }
    }

    // Build each task's view: identical kernel/device mappings, EL0
    // access granted only to that view's own region.
    for (view, &region) in el0_regions.iter().enumerate() {
        unsafe {
            build_view(
                view,
                region,
                min_addr,
                max_addr,
                &extra_l1_owners,
                next_extra_l1_table,
                &l1_span_devices[..l1_span_device_count],
            )
        };
    }

    crate::console::println!(
        "Ouroboros kernel: identity map RAM {min_addr:#x}-{max_addr:#x}, device 0x0-{:#x}, per-task EL0 regions {el0_regions:x?}",
        GIB - 1
    );

    unsafe { switch_full(crate::tasks::current_task()) };
}

/// Builds one task's translation-table view: the shared kernel shape
/// (fixed low-1GB device block, discovered RAM as EL1-only 1GB blocks,
/// the shared extra-device L1 tables wired into L0) with exactly one
/// difference per view - `el0_region` (this task's own region, `(0, 0)`
/// for an unused slot) gets its containing 1GB block split down to 4KB
/// pages so only the region's own pages carry EL0 access.
unsafe fn build_view(
    view: usize,
    el0_region: (u64, u64),
    min_addr: u64,
    max_addr: u64,
    extra_l1_owners: &[usize; MAX_EXTRA_L1_TABLES],
    extra_l1_count: usize,
    l1_span_devices: &[(usize, u64)],
) {
    let l1 = unsafe { &mut *L1_TABLES[view].0.get() };

    // Device: fixed low 1GB. See module doc comment for why this one stays
    // a hardcoded convention rather than discovered.
    l1[0] = device_block(0);

    if min_addr <= max_addr {
        let first_block = min_addr / GIB;
        let last_block = (max_addr - 1) / GIB;
        for block in first_block..=last_block {
            let idx = block as usize;
            if idx >= ENTRIES_PER_TABLE {
                continue;
            }
            let block_start = block * GIB;
            let block_end = block_start + GIB;

            if overlaps(el0_region, block_start, block_end) {
                // This view's own region lives in this block - split it
                // so only the region's own pages get EL0 access. One L2
                // and one L3 per view is always enough: a region fits
                // one 2MB slot by construction (see
                // `install_identity_map`'s safety comment), so it
                // touches exactly one 1GB block and one 2MB slot.
                let l2 = unsafe { &mut *EL0_L2_TABLES[view].0.get() };
                for (i, entry) in l2.iter_mut().enumerate() {
                    let sub_base = block_start + (i as u64) * MIB2;
                    let sub_end = sub_base + MIB2;
                    if overlaps(el0_region, sub_base, sub_end) {
                        let l3 = unsafe { &mut *EL0_L3_TABLES[view].0.get() };
                        for (j, page) in l3.iter_mut().enumerate() {
                            let page_base = sub_base + (j as u64) * 4096;
                            let page_end = page_base + 4096;
                            *page = if overlaps(el0_region, page_base, page_end) {
                                el0_page_4k(page_base)
                            } else {
                                kernel_page_4k(page_base)
                            };
                        }
                        *entry = table_desc(EL0_L3_TABLES[view].0.get() as u64);
                    } else {
                        *entry = kernel_block_2m(sub_base);
                    }
                }
                l1[idx] = table_desc(EL0_L2_TABLES[view].0.get() as u64);
            } else {
                l1[idx] = normal_block(block_start);
            }
        }
    }

    // In-span device blocks the RAM loop didn't cover (e.g. a
    // framebuffer outside the reported RAM span but inside the first
    // 512GB).
    for &(l1_idx, base) in l1_span_devices {
        if l1[l1_idx] == 0 {
            l1[l1_idx] = device_block(base);
        }
    }

    let l0 = unsafe { &mut *L0_TABLES[view].0.get() };
    l0[0] = table_desc(L1_TABLES[view].0.get() as u64);
    // Shared extra-device L1 tables (64-bit PCI BARs past 512GB, etc.) -
    // identical entries in every view, so every view's L0 points at the
    // same tables.
    for slot in 0..extra_l1_count {
        let l0_idx = extra_l1_owners[slot];
        if l0_idx != usize::MAX && l0_idx < ENTRIES_PER_TABLE {
            l0[l0_idx] = table_desc(EXTRA_L1_TABLES[slot].0.get() as u64);
        }
    }
}

/// The per-context-switch table switch: points TTBR0_EL1 at `view`'s
/// table set and invalidates the TLB. Called by `tasks.rs` wherever
/// the current task changes - the moment the following `eret` lands in
/// EL0, the new task can only see its own region.
///
/// The full `tlbi vmalle1` on every switch is the stage-2
/// correctness-first design: with a single ASID, entries cached under
/// the previous view would otherwise satisfy the next task's walks.
/// The planned stage-3 refinement (per-task ASIDs + nG-tagged EL0
/// entries) makes this a plain TTBR0 write; if that ever misbehaves on
/// real hardware, this version is the known-correct fallback.
pub(crate) fn activate_task(view: usize) {
    let ttbr0 = L0_TABLES[view.min(MAX_EL0_REGIONS - 1)].0.get() as u64;
    unsafe {
        asm!(
            "msr ttbr0_el1, {0}",
            "isb",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            in(reg) ttbr0,
            options(nostack),
        );
    }
}

/// The full MAIR/TCR/TTBR0 configuration sequence, switching to
/// `view`'s table set - the boot-time install and every runtime
/// rebuild end here. [`activate_task`] is the lighter switch-only
/// sibling used on every context switch.
unsafe fn switch_full(view: usize) {
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

    let ttbr0_el1 = L0_TABLES[view.min(MAX_EL0_REGIONS - 1)].0.get() as u64;

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

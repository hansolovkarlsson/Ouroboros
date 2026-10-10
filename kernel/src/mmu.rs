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
//! ## Since generalized to one independent region per task slot
//!
//! The paragraphs above describe the original shape: one 8KB compile-time
//! static, holding both EL0 tasks, isolated via one L2/L3 split. Once task
//! 0 became a program loaded from disk at a runtime-determined address
//! (`loader.rs`) instead of a compile-time constant, that stopped fitting
//! the "one region" model: task 0's region and task 1's small idle region
//! (`tasks.rs::IdleRegion`) became unrelated allocations that could
//! easily land in different 1GB blocks or 2MB slots. [`install_identity_map`]
//! took an array of regions instead of one tuple, first sized for those
//! two, and now sized for every scheduler slot: `EL0_L2_TABLES`/
//! `EL0_L3_TABLES` hold [`MAX_EL0_REGIONS`] (that is, `tasks::NUM_TASKS`)
//! independent splits, one per task. The underlying technique (walk down
//! to L3 only for the slot(s) that need it, everything else stays a coarse
//! block) is unchanged. The alignment invariant that keeps this from ever
//! needing to handle a *single* region spanning multiple slots, which
//! would be a bigger change, is the loader's: see the safety comments on
//! `install_identity_map` and the `SLOT_ALIGN` assert near the top of this
//! file.

//!
//! ## Non-cacheable ranges (2026-09-27, for the Raspberry Pi 4/400)
//!
//! Everything above maps RAM Normal write-back cacheable, and nothing in
//! this kernel maintains caches. That is only correct where DMA is
//! cache-coherent (QEMU, Parallels). The Raspberry Pi's PCIe DMA is not
//! (`docs/testing/testing-pi4.md` Risk 8: the pftf firmware's own ACPI
//! says `_CCA 0`). So a third memory type, Normal Non-cacheable (MAIR
//! index 2, `0x44`), is mapped for exactly the ranges
//! `install_identity_map` is given in `uncached`, and of those only the
//! ones inside RAM: `main.rs` passes the xHCI/USB DMA pool
//! (`xhci::dma_region`) only when the firmware's ACPI declares DMA
//! non-coherent (`_CCA 0`, `acpi::dma_noncoherent`) - under a hypervisor
//! that emulates the device, a guest-side non-cacheable mapping could
//! disagree with the host's cacheable one, so coherent platforms keep the
//! pool ordinary memory. Nothing is made non-cacheable implicitly: MMIO
//! must stay Device.
//!
//! The framebuffer (Risk 7: in RAM on the Pi, read by a display engine
//! that does not look in the CPU's caches) is deliberately NOT one of
//! these ranges. It stays ordinary cacheable memory, and every write to it
//! is cleaned out to memory ([`clean_to_poc`], from `fbdev.rs` and
//! `fbconsole.rs`): correct on bare metal and under a hypervisor alike,
//! where a non-cacheable mapping would have the same host-disagreement
//! problem as the pool, and a scroll reads cached memory, not 8 MB of
//! uncached memory per line.
//!
//! An [`NcPlan`], built once per table build, holds the ranges (rounded
//! out to pages) and the SHARED tables that carry them, identical in every
//! view: an L2 table per 1GB block holding a range, in which a 2MB slot
//! the range covers completely is one non-cacheable block and a slot it
//! covers partly gets an L3 table, page by page - so no kernel data next
//! to a range loses its caching (exclusive, that is atomic, accesses to
//! non-cacheable memory are not guaranteed to work). A view's own EL0
//! split in the same block takes every slot's entry from the plan except
//! its own, and its own L3 marks non-cacheable pages too.
//!
//! At install the ranges are cleaned and invalidated from the data cache
//! (they were written through cacheable mappings until then), and
//! `check_attributes` asks the CPU's own table walker (`AT S1E1R`,
//! `PAR_EL1`), in every view, what memory type every page of every range
//! gets, and the page on either side of it, and the kernel's own data,
//! logging `walks as Normal Non-cacheable ... on every page in all N
//! views` or a WARNING. QEMU models no caches, so that walk is the part
//! of this it CAN check; the difference the memory type makes shows only
//! on the Pi.

use core::arch::asm;
use crate::synccell::SyncCell;
use core::cell::UnsafeCell;

use crate::tasks::TaskIndex;

use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned, MemoryType};

use core::sync::atomic::{AtomicU64, Ordering};

use crate::loader::{guard_page_addr, GUARD_PAGES, SLOT_ALIGN, SLOT_PAGES};

/// The slot `build_view` splits (`MIB2`) and the slot the loader bounds
/// every region to (`SLOT_ALIGN`) are one quantity in two modules; pinned
/// so the loader's bound cannot silently stop meaning "fits one slot".
/// Likewise the loader's bound in pages and the L3 table `build_view`
/// fills: the bound is provably the table's capacity only if they agree.
const _: () = assert!(SLOT_ALIGN == MIB2, "the loader's region slot must be the L2 slot build_view splits");
const _: () = assert!(SLOT_PAGES == ENTRIES_PER_TABLE as u64, "the loader's slot in pages must be one L3 table");

/// A region `build_view` refused (see there) while no console existed to
/// say so: the boot-time views are built before any console on the
/// platforms that have only a framebuffer, so the warning is kept here
/// and `report_deferred_warnings` prints it once a console is up. Base
/// and size of the last such region; `0` size means none.
static DEFERRED_REFUSAL: (AtomicU64, AtomicU64) = (AtomicU64::new(0), AtomicU64::new(0));

/// What the boot-time non-cacheable plan and its self-check found, kept
/// here and printed by [`report_deferred_warnings`] instead of at the
/// moment: `install_identity_map` runs before the framebuffer console
/// exists, so on a board watched over HDMI alone (a Raspberry Pi without
/// its serial cable) a line printed then would be lost - including the
/// ones `docs/testing/testing-pi4.md` says to read.
#[derive(Clone, Copy)]
enum NcNote {
    /// A range the plan mapped non-cacheable, `(start, end)`.
    Mapped(u64, u64),
    /// A range the plan refused, `(base, size, why)`.
    Refused(u64, u64, &'static str),
    /// The plan needed more shared tables than exist and mapped nothing.
    Exhausted,
    /// The self-check passed for `(start, end)` in this many views.
    Checked(u64, u64, usize),
    /// The self-check failed: `(start, end, address, walked attribute,
    /// view, what)`.
    Bad(u64, u64, u64, Option<u64>, usize, &'static str),
}

const MAX_NC_NOTES: usize = 3 * MAX_NC_RANGES + 1;
static NC_NOTES: SyncCell<([Option<NcNote>; MAX_NC_NOTES], usize)> = SyncCell::new(([None; MAX_NC_NOTES], 0));

fn nc_note(note: NcNote) {
    let notes = unsafe { &mut *NC_NOTES.get() };
    if notes.1 < MAX_NC_NOTES {
        notes.0[notes.1] = Some(note);
        notes.1 += 1;
    }
}

/// Prints what boot-time table building could not print yet: a
/// `build_view` refusal, and the non-cacheable plan and its self-check
/// ([`NcNote`]). Called from `main.rs` once every console mechanism has
/// had its turn.
pub(crate) fn report_deferred_warnings() {
    let size = DEFERRED_REFUSAL.1.swap(0, Ordering::Relaxed);
    if size != 0 {
        let base = DEFERRED_REFUSAL.0.load(Ordering::Relaxed);
        refusal_warning(base, size);
    }
    let notes = unsafe { &mut *NC_NOTES.get() };
    for note in notes.0[..notes.1].iter().flatten() {
        match *note {
            NcNote::Mapped(start, end) => {
                crate::console::println!("Ouroboros kernel: mmu: {start:#x}-{end:#x} mapped Normal Non-cacheable")
            }
            NcNote::Refused(base, size, why) => crate::console::println_force!(
                "Ouroboros kernel: WARNING: mmu: {base:#x}+{size:#x} not mapped non-cacheable ({why})"
            ),
            NcNote::Exhausted => crate::console::println_force!(
                "Ouroboros kernel: WARNING: mmu: the non-cacheable ranges need more shared tables than exist, so none is mapped non-cacheable"
            ),
            NcNote::Checked(start, end, views) => crate::console::println!(
                "Ouroboros kernel: mmu: {start:#x}-{end:#x} walks as Normal Non-cacheable (attr {MAIR_ATTR_NORMAL_NC:#04x}) on every page in all {views} views, its neighbours and kernel data do not"
            ),
            NcNote::Bad(start, end, va, a, view, what) => crate::console::println_force!(
                "Ouroboros kernel: WARNING: mmu: range {start:#x}-{end:#x}: {va:#x} walks as {} in view {view} ({what}) - the non-cacheable mapping is wrong",
                Attr(a)
            ),
        }
    }
    notes.1 = 0;
}

fn refusal_warning(base: u64, size: u64) {
    crate::console::println_force!(
        "Ouroboros kernel: WARNING: EL0 region {base:#x}+{size:#x} does not fit one 2MB slot, mapping none of it EL0"
    );
}

/// `build_view` maps exactly ONE page EL1-only at the address
/// `guard_page_addr` returns. If the loader ever reserved a wider guard,
/// the pages above the one it names would stay EL0-writable and an
/// overflow of up to a page would go silent - the same class of drift
/// sharing the constants was meant to end, so pin it where the compiler
/// enforces it.
const _: () = assert!(
    GUARD_PAGES == 1,
    "build_view maps a single guard page; widen its mapping before growing GUARD_PAGES"
);

const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 * 1024 * 1024;
const ENTRIES_PER_TABLE: usize = 512;

#[repr(align(4096))]
struct Table(SyncCell<[u64; ENTRIES_PER_TABLE]>);

impl Table {
    const fn get(&self) -> *mut [u64; ENTRIES_PER_TABLE] {
        self.0.get()
    }
}

/// One EL0 region slot per task, and, since the per-task page-tables
/// milestone, also the number of translation-table *views*: view i
/// grants EL0 access to region i alone. Each region is independently
/// guaranteed to fit within one 2MB-aligned slot (see the safety
/// comments on `install_identity_map`), so a view needs exactly one L2
/// split and one L3 split - its own region's.
///
/// One definition, not a second literal: this *is* `tasks::NUM_TASKS`,
/// so raising the slot count there scales every table pool here. A slot
/// with no view is unspellable, not clamped: see [`l0_table`].
const MAX_EL0_REGIONS: usize = crate::tasks::NUM_TASKS;

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
// `[const { … }; N]` (rather than an explicit N-element literal) so these
// auto-scale with `MAX_EL0_REGIONS` - raising the task-slot count (Stage 0)
// then touches one constant, not four table arrays. `Table` isn't `Copy`
// (`UnsafeCell` isn't), which is why the repeat needs the inline-const form.
static L0_TABLES: [Table; MAX_EL0_REGIONS] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_EL0_REGIONS];
static L1_TABLES: [Table; MAX_EL0_REGIONS] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_EL0_REGIONS];

/// The most `extra_devices` entries `install_identity_map` takes, and the
/// size of `main.rs`'s staging array, which uses this constant rather than
/// its own number (a longer slice is truncated below): framebuffer, xHCI
/// BAR, GICD, GICR, and the discovered serial console (added 2026-09-28,
/// when the Pi 4's devicetree console turned out to sit at 0xfe201000, far
/// above the fixed low-1GB device block; it was covered only by the GIC's
/// 1GB block, by coincidence).
pub(crate) const MAX_EXTRA_DEVICES: usize = 5;

/// [`install_identity_map`]'s inputs, kept around after its first call so
/// a later runtime caller (`rebuild_with_el0_regions`, for
/// `tasks::spawn` - dynamic task creation) can rebuild the whole table
/// set again without needing UEFI boot services, long gone by then, to
/// reconstruct either of them. `MemoryMapOwned` isn't `Clone`, so this
/// takes real ownership (moved out of `install_identity_map`'s own
/// parameter) rather than copying - the caller only ever needs to supply
/// it once.
struct StoredMapCell(UnsafeCell<Option<MemoryMapOwned>>);

// SAFETY: the `synccell` module's argument (one core, interrupts masked
// for the whole of every EL1 context), which `SyncCell` would state for
// us except that `MemoryMapOwned` holds a `NonNull` and is not `Send`, so
// the bound refuses it and this wrapper argues its own case: written once
// by `install_identity_map`'s first call, before any code that could race
// it exists, and only ever read (not mutated) afterward.
unsafe impl Sync for StoredMapCell {}

static STORED_MEMORY_MAP: StoredMapCell = StoredMapCell(UnsafeCell::new(None));
static STORED_EXTRA_DEVICES: SyncCell<([(u64, u64); MAX_EXTRA_DEVICES], usize)> =
    SyncCell::new(([(0, 0); MAX_EXTRA_DEVICES], 0));
static STORED_UNCACHED: SyncCell<([(u64, u64); MAX_UNCACHED], usize)> = SyncCell::new(([(0, 0); MAX_UNCACHED], 0));

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
// slot is one 4KB table. One per `extra_devices` entry, so the count
// follows `MAX_EXTRA_DEVICES` (five since the console joined the list,
// 2026-09-28) rather than being bumped by hand beside it.
const MAX_EXTRA_L1_TABLES: usize = MAX_EXTRA_DEVICES;
static EXTRA_L1_TABLES: [Table; MAX_EXTRA_L1_TABLES] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_EXTRA_L1_TABLES];

// One L2 + one L3 per *view* (not per region-in-a-shared-map, the
// pre-per-task-tables design): view i only ever fine-grains the single
// 1GB block and single 2MB slot containing its own region - every
// other task's region is, from view i's perspective, ordinary EL1-only
// RAM needing no split at all. A genuinely simpler shape than the old
// shared map's up-to-five-splits bookkeeping.

static EL0_L2_TABLES: [Table; MAX_EL0_REGIONS] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_EL0_REGIONS];

static EL0_L3_TABLES: [Table; MAX_EL0_REGIONS] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_EL0_REGIONS];

/// The most non-cacheable ranges [`install_identity_map`] takes
/// (`uncached`): today the xHCI/USB DMA pool, where the firmware declares
/// DMA non-coherent, with room to spare.
const MAX_UNCACHED: usize = 4;
/// Every range an [`NcPlan`] can hold: exactly the explicit ones. Nothing
/// is made non-cacheable implicitly - an automatic "every device region
/// inside the RAM span" rule once would have caught a GIC whose 1GB block
/// the RAM span reached (a Pi with "Limit RAM to 3 GB" off), and MMIO must
/// stay Device.
const MAX_NC_RANGES: usize = MAX_UNCACHED;
/// Shared L2 tables, one per 1GB RAM block holding a non-cacheable range;
/// a range can cross one block boundary, so two per range covers the
/// worst case. Identical in every view, like the extra-device L1 tables.
const MAX_NC_L2_TABLES: usize = 2 * MAX_NC_RANGES;
/// Shared L3 tables, one per 2MB slot a non-cacheable range covers only
/// partly: at most its two end slots, so two per range is the worst case.
const MAX_NC_L3_TABLES: usize = 2 * MAX_NC_RANGES;
static NC_L2_TABLES: [Table; MAX_NC_L2_TABLES] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_NC_L2_TABLES];
static NC_L3_TABLES: [Table; MAX_NC_L3_TABLES] =
    [const { Table(SyncCell::new([0; ENTRIES_PER_TABLE])) }; MAX_NC_L3_TABLES];

/// Where inside RAM the map says Normal Non-cacheable instead of Normal
/// write-back, and which shared tables carry that. Built once per table
/// build ([`NcPlan::build`]) and consulted by every view, so the answer
/// for a page is the same in all of them.
///
/// Ranges are whole pages (rounded outward). A 2MB slot a range covers
/// completely becomes one non-cacheable 2MB block; a slot it covers only
/// partly gets a shared L3 table, page by page, so no kernel data next to
/// it loses its caching (which matters beyond speed: exclusive, that is
/// atomic, accesses to non-cacheable memory are not guaranteed to work).
struct NcPlan {
    ranges: [(u64, u64); MAX_NC_RANGES], // (start, end), page-aligned
    count: usize,
    l2_blocks: [u64; MAX_NC_L2_TABLES], // the 1GB block each shared L2 covers
    l2_count: usize,
    l3_slots: [u64; MAX_NC_L3_TABLES], // the 2MB slot base each shared L3 covers
    l3_count: usize,
}

/// How a 2MB slot relates to an [`NcPlan`].
enum NcSlot {
    None,
    Full,
    Partial,
}

impl NcPlan {
    /// Plans `ranges` (`(base, size)`; zero sizes ignored) and fills the
    /// shared L2/L3 tables. A range is planned only if it lies inside the
    /// RAM loop's blocks (`ram_blocks`, first and last 1GB block index):
    /// one outside RAM is not RAM, keeps whatever mapping it has, and needs
    /// nothing here. A
    /// range whose base is not page-aligned is REFUSED with a warning,
    /// not rounded outward: rounding would pull whatever shares its first
    /// page into the non-cacheable mapping (the DMA pool is page-aligned
    /// by construction, and this is what checks it); the end is rounded up
    /// to a page; a range over 1GB is refused too, which bounds the table
    /// pools. If the shared tables still run out the plan is EMPTY, never
    /// partial (see the comment at the table assignment). What it did is
    /// recorded as [`NcNote`]s only when `log` (the boot build), printed
    /// later by `report_deferred_warnings`: a runtime rebuild on every
    /// spawn and exit must not repeat them over the console server's
    /// screen.
    fn build(ranges: &[(u64, u64)], ram_blocks: Option<(u64, u64)>, log: bool) -> NcPlan {
        let mut plan = NcPlan {
            ranges: [(0, 0); MAX_NC_RANGES],
            count: 0,
            l2_blocks: [0; MAX_NC_L2_TABLES],
            l2_count: 0,
            l3_slots: [0; MAX_NC_L3_TABLES],
            l3_count: 0,
        };
        for &(base, size) in ranges {
            if size == 0 || plan.count == MAX_NC_RANGES {
                continue;
            }
            let end = (base + size + 0xfff) & !0xfff;
            let in_ram = ram_blocks.is_some_and(|(first, last)| first <= base / GIB && (end - 1) / GIB <= last);
            if !in_ram {
                // Not RAM: its own mapping (Device, if any) stands. Said
                // out loud, since a range asked for non-cacheable and left
                // cacheable is otherwise invisible until DMA misbehaves.
                if log {
                    nc_note(NcNote::Refused(base, size, "outside RAM: left as it is mapped"));
                }
                continue;
            }
            if base & 0xfff != 0 {
                if log {
                    nc_note(NcNote::Refused(base, size, "not page-aligned: left cacheable rather than taking its neighbours along"));
                }
                continue;
            }
            if end - base > GIB {
                // Bounds the table pools: a range of at most 1GB touches at
                // most two 1GB blocks and has at most two partial slots.
                if log {
                    nc_note(NcNote::Refused(base, size, "larger than 1GB"));
                }
                continue;
            }
            plan.ranges[plan.count] = (base, end);
            plan.count += 1;
        }
        // Which shared tables the plan needs - an L3 per partly covered
        // slot, an L2 per 1GB block holding a range - assigned before any
        // is filled, because a plan that runs out of tables must be ALL or
        // NOTHING: a range with only some of its tables would leave a page
        // non-cacheable in one view (an EL0 split takes its slots from the
        // plan) and write-back in another, the mismatched alias the whole
        // design exists to avoid. The pools hold two of each per range and
        // a range is at most 1GB, so running out means a bug; it is still
        // handled.
        let mut exhausted = false;
        for r in 0..plan.count {
            let (start, end) = plan.ranges[r];
            let mut slot = start & !(MIB2 - 1);
            while slot < end {
                if matches!(plan.slot(slot), NcSlot::Partial) && !plan.l3_slots[..plan.l3_count].contains(&slot) {
                    if plan.l3_count == MAX_NC_L3_TABLES {
                        exhausted = true;
                    } else {
                        plan.l3_slots[plan.l3_count] = slot;
                        plan.l3_count += 1;
                    }
                }
                slot += MIB2;
            }
            let mut block = start / GIB;
            while block * GIB < end {
                if !plan.l2_blocks[..plan.l2_count].contains(&block) {
                    if plan.l2_count == MAX_NC_L2_TABLES {
                        exhausted = true;
                    } else {
                        plan.l2_blocks[plan.l2_count] = block;
                        plan.l2_count += 1;
                    }
                }
                block += 1;
            }
        }
        if exhausted {
            if log {
                nc_note(NcNote::Exhausted);
            }
            plan.count = 0;
            plan.l2_count = 0;
            plan.l3_count = 0;
            return plan;
        }
        for (i, &slot) in plan.l3_slots[..plan.l3_count].iter().enumerate() {
            let l3 = unsafe { &mut *NC_L3_TABLES[i].get() };
            for (j, page) in l3.iter_mut().enumerate() {
                let page_base = slot + (j as u64) * 4096;
                *page = if plan.page_is_nc(page_base) { nc_page_4k(page_base) } else { kernel_page_4k(page_base) };
            }
        }
        for (i, &block) in plan.l2_blocks[..plan.l2_count].iter().enumerate() {
            let l2 = unsafe { &mut *NC_L2_TABLES[i].get() };
            for (k, entry) in l2.iter_mut().enumerate() {
                *entry = plan.slot_entry(block * GIB + (k as u64) * MIB2);
            }
        }
        if log {
            for &(start, end) in &plan.ranges[..plan.count] {
                nc_note(NcNote::Mapped(start, end));
            }
        }
        plan
    }

    fn page_is_nc(&self, page_base: u64) -> bool {
        self.ranges[..self.count].iter().any(|&(start, end)| start <= page_base && page_base < end)
    }

    fn slot(&self, slot_base: u64) -> NcSlot {
        let slot_end = slot_base + MIB2;
        let mut touched = false;
        for &(start, end) in &self.ranges[..self.count] {
            if start <= slot_base && slot_end <= end {
                return NcSlot::Full;
            }
            if start < slot_end && end > slot_base {
                touched = true;
            }
        }
        if touched { NcSlot::Partial } else { NcSlot::None }
    }

    /// The L2 entry for the 2MB slot at `slot_base`, outside any EL0 split:
    /// a write-back kernel block, a non-cacheable block, or the slot's
    /// shared L3 table.
    fn slot_entry(&self, slot_base: u64) -> u64 {
        match self.slot(slot_base) {
            NcSlot::None => kernel_block_2m(slot_base),
            NcSlot::Full => nc_block_2m(slot_base),
            NcSlot::Partial => match self.l3_slots[..self.l3_count].iter().position(|&s| s == slot_base) {
                Some(i) => table_desc(NC_L3_TABLES[i].get() as u64),
                None => kernel_block_2m(slot_base), // unreachable: `build` assigns every partial slot or plans nothing
            },
        }
    }

    /// The shared L2 table for the 1GB block at index `block`, if the plan
    /// has one.
    fn l2_for_block(&self, block: u64) -> Option<u64> {
        self.l2_blocks[..self.l2_count]
            .iter()
            .position(|&b| b == block)
            .map(|i| table_desc(NC_L2_TABLES[i].get() as u64))
    }

    /// The L3 entry for `page_base` inside an EL0 split's slot, for a page
    /// that is neither the EL0 region nor its guard.
    fn kernel_page(&self, page_base: u64) -> u64 {
        if self.page_is_nc(page_base) { nc_page_4k(page_base) } else { kernel_page_4k(page_base) }
    }
}

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
/// The output-address field of a table or page descriptor, bits 47:12.
/// Shared with `earlyfault.rs`'s table walk, so one constant.
pub(crate) const OUTPUT_ADDR_MASK: u64 = 0x0000_ffff_ffff_f000; // bits 47:12

const MAIR_IDX_DEVICE_NGNRNE: u64 = 0;
const MAIR_IDX_NORMAL_WB: u64 = 1;
const MAIR_IDX_NORMAL_NC: u64 = 2;
const MAIR_ATTR_DEVICE_NGNRNE: u64 = 0x00;
const MAIR_ATTR_NORMAL_WB: u64 = 0xff;
/// Normal memory, Inner and Outer Non-cacheable: memory a device reads and
/// writes by DMA on a platform whose DMA is not cache-coherent (the
/// Raspberry Pi 4/400's PCIe, `docs/testing/testing-pi4.md` Risk 8).
/// Normal, not Device: unaligned and wide accesses stay legal.
const MAIR_ATTR_NORMAL_NC: u64 = 0x44;

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
/// Valid L3 entries always have `bits[1:0]` = 0b11 - the same bit pattern
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

/// 4KB page, Normal Non-cacheable, EL1-only, never executable - a page of
/// an [`NcPlan`] range. Outer Shareable: the architecture treats Normal
/// Non-cacheable memory as Outer Shareable whatever the field says, so it
/// says so.
fn nc_page_4k(base: u64) -> u64 {
    (base & !0xfff)
        | DESC_VALID
        | DESC_TABLE
        | (MAIR_IDX_NORMAL_NC << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_OUTER
        | AF
        | PXN
        | UXN
}

/// 2MB block, Normal Non-cacheable, EL1-only, never executable - a 2MB
/// slot an [`NcPlan`] range covers entirely.
fn nc_block_2m(base: u64) -> u64 {
    (base & !(MIB2 - 1))
        | DESC_VALID
        | (MAIR_IDX_NORMAL_NC << ATTRINDX_SHIFT)
        | AP_EL1_RW_ONLY
        | SH_OUTER
        | AF
        | PXN
        | UXN
}

/// Whether a memory-map descriptor of type `ty` is memory the kernel may
/// treat as RAM: everything but MMIO, reserved and unaccepted. One
/// predicate, shared with `earlyfault.rs`'s image walk, so the reporter's
/// notion of RAM and the identity map's cannot drift apart.
pub(crate) fn is_general_ram(ty: MemoryType) -> bool {
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
/// within one 2MB-aligned slot. That rests on two checks, not on
/// construction: `loader::elf_region_size` refuses a region larger than
/// `SLOT_ALIGN` (before it, a 1.9MB `.bss` produced a two-slot region
/// and `build_view` aliased it, 2026-09-13), and every region base is
/// 2MB-aligned (`loader.rs` over-allocates and trims task 0's, and
/// `tasks::allocate_runtime_region` rounds to `SLOT_ALIGN`); `tasks.rs`'s
/// `IdleRegion` is a single 4KB page and cannot straddle. `build_view`
/// still checks containment itself, and gives a region that fails no EL0
/// mapping at all, with a forced (or, before any console exists,
/// deferred) warning, rather than aliasing - see there.
///
/// `extra_devices` are additional discovered device regions (`(base,
/// size)`) that need to stay mapped and accessible after this switch, on
/// top of the fixed low-1GB device block above - currently the GOP
/// framebuffer (`framebuffer::discover`, needed by `fbconsole.rs`) and the
/// xHCI controller's PCI BAR (`pci::discover_xhci`, needed by `xhci.rs`).
/// A region inside the discovered-RAM span (a framebuffer the firmware
/// allocated from RAM: QEMU's `ramfb`, the Raspberry Pi's) is covered by
/// the RAM loop, write-back cacheable; for a framebuffer that is correct
/// only because its writers clean every write out to memory for the
/// display engine (`clean_to_poc`, Risk 7 - see "Non-cacheable ranges" in
/// the module doc for why it is not mapped non-cacheable instead).
/// Only if a region's containing 1GB block is *still*
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
/// `uncached` are the ranges to map Normal Non-cacheable where they lie
/// inside RAM: memory a device reaches by DMA on a platform whose DMA is
/// not cache-coherent (`xhci::dma_region` on the Raspberry Pi 4/400, Risk
/// 8). A range outside RAM keeps whatever mapping it has. Each
/// must start on a page and own its pages outright (a misaligned one is
/// refused): the mapping is page-granular, and anything sharing those
/// pages would become non-cacheable too.
///
/// Takes ownership of `memory_map` (not a reference) and keeps it,
/// `extra_devices` and `uncached` around afterward - see [`rebuild_with_el0_regions`],
/// the counterpart this exists for: a later runtime caller (dynamic task
/// creation's `spawn` syscall) needs to rebuild the whole table set again
/// with one more `el0_regions` entry, long after UEFI boot services (and
/// therefore any way to reconstruct `memory_map` from scratch) are gone.
///
/// Two halves, [`build_identity_map`] and [`switch_to_identity_map`], and
/// this is their composition. The build half writes memory only (the
/// tables, and the stashes above); the switch half writes the registers
/// and then runs the checks that need the new tables live. They are
/// separate because the exception level decides what the switch is: at
/// EL1 it is [`switch_full`], the same register sequence as every runtime
/// rebuild; on a platform that hands off at EL2 (the Raspberry Pi) the
/// `_EL1` registers are set from EL2 and the switch is the `eret` that
/// drops into them (`docs/roadmap/roadmap-el1-drop.md`). The switch half
/// takes the build half's result, so it cannot be called without it, and
/// both halves are private: this composition is the one entry, called
/// once from `main.rs`, since the build half takes the memory map by value
/// and stashes it.
pub unsafe fn install_identity_map(
    memory_map: MemoryMapOwned,
    el0_regions: [(u64, u64); MAX_EL0_REGIONS],
    extra_devices: &[(u64, u64)],
    uncached: &[(u64, u64)],
) {
    let planned = unsafe { build_identity_map(memory_map, el0_regions, extra_devices, uncached) };
    unsafe { switch_to_identity_map(planned) };
}

/// The non-cacheable ranges the build half planned, which the switch half
/// cleans from the cache and checks through the hardware walker once the
/// tables are live. Only [`build_tables`] makes one, and [`switch_full`]
/// requires one, so neither the boot install nor the runtime rebuild can
/// switch onto tables that were not just built.
struct Planned {
    ranges: [(u64, u64); MAX_NC_RANGES],
    count: usize,
}

/// The build half of [`install_identity_map`]: stashes the inputs for the
/// runtime rebuilds and writes every table, and no register. The tables
/// are complete and consistent when this returns; nothing walks them until
/// the switch half.
///
/// # Safety
/// As [`install_identity_map`].
unsafe fn build_identity_map(
    memory_map: MemoryMapOwned,
    el0_regions: [(u64, u64); MAX_EL0_REGIONS],
    extra_devices: &[(u64, u64)],
    uncached: &[(u64, u64)],
) -> Planned {
    let mut stored = [(0u64, 0u64); MAX_EXTRA_DEVICES];
    let count = extra_devices.len().min(MAX_EXTRA_DEVICES);
    stored[..count].copy_from_slice(&extra_devices[..count]);
    unsafe { *STORED_EXTRA_DEVICES.get() = (stored, count) };
    let mut stored_nc = [(0u64, 0u64); MAX_UNCACHED];
    let nc_count = uncached.len().min(MAX_UNCACHED);
    stored_nc[..nc_count].copy_from_slice(&uncached[..nc_count]);
    unsafe { *STORED_UNCACHED.get() = (stored_nc, nc_count) };
    unsafe { *STORED_MEMORY_MAP.0.get() = Some(memory_map) };
    // Re-borrow from the stash rather than the original parameter (now
    // moved) - the rest of this function is unchanged either way.
    let memory_map = unsafe { (*STORED_MEMORY_MAP.0.get()).as_ref() }.unwrap();
    unsafe { build_tables(memory_map, el0_regions, extra_devices, &stored_nc[..nc_count], true) }
}

/// The EL1 regime the boot core switched to, kept for the secondary cores,
/// which share its tables (`smp.rs`; class A of the multi-core plan's
/// inventory, written once at the switch).
static BOOT_REGIME: SyncCell<Option<El1Regime>> = SyncCell::new(None);

/// The regime [`switch_to_identity_map`] installed on the boot core.
pub(crate) fn boot_regime() -> El1Regime {
    // SAFETY: written once at the switch, before any second core runs.
    unsafe { *BOOT_REGIME.get() }.expect("mmu::boot_regime before the identity map was installed")
}

/// The switch half of [`install_identity_map`]: onto the current task's
/// view, then the one-time cache clean of the non-cacheable ranges and the
/// walker check of every view, both of which need the new tables live.
/// The exception level decides the switch. At EL1 it is [`switch_full`],
/// the sequence every runtime rebuild uses. At EL2 (the Raspberry Pi's
/// handoff) those `_EL1` writes would install nothing for the running
/// level, so the switch is `el2::drop_to_el1`: EL1's regime set from EL2
/// and an `eret` into it; this returns at EL1 either way.
///
/// A handoff at any other level (EL3, or EL0, which cannot happen) halts
/// with a line: before the drop existed an unexpected level was logged
/// and then trusted, and every post-exit line described registers the
/// running level did not use.
///
/// # Safety
/// [`build_identity_map`] must have returned `planned` on this boot, after
/// `exit_boot_services`, with IRQs masked and `exceptions::install` done
/// (at EL2 that write takes effect at the drop).
unsafe fn switch_to_identity_map(planned: Planned) {
    let view = crate::tasks::current_index();
    // SAFETY: the one write, on the boot core, before any second core.
    unsafe { *BOOT_REGIME.get() = Some(el1_regime(view, &planned)) };
    match crate::el2::current_el() {
        1 => unsafe { switch_full(view, &planned) },
        2 => unsafe { crate::el2::drop_to_el1(el1_regime(view, &planned)) },
        el => {
            crate::console::println!(
                "Ouroboros kernel: handed off at EL{el}, which this kernel has no path for (EL1 runs, EL2 drops)"
            );
            crate::power::halt();
        }
    }
    let planned = &planned.ranges[..planned.count];

    // The non-cacheable ranges were written through cacheable mappings
    // until a moment ago (firmware zeroed `.bss` and drew its splash that
    // way), so the data cache may still hold lines for them - and a dirty
    // line evicted later would land on top of what a device wrote, a clean
    // one would shadow nothing but still breaks the no-cacheable-alias
    // rule. Clean and invalidate them once, now that nothing maps them
    // cacheable any more. Only at install: a rebuild maps them the same.
    // Only the ranges actually planned: those are mapped (a cache
    // operation on an unmapped address faults at EL1), and they are the
    // only ones that were ever cacheable RAM.
    for &(start, end) in planned {
        clean_invalidate(start, end - start);
    }
    check_attributes(planned);
}

/// The smallest data-cache line, in bytes (`CTR_EL0.DminLine`, log2 of
/// words): the stride every cache-maintenance loop here steps by.
fn dcache_line() -> u64 {
    let ctr: u64;
    unsafe { asm!("mrs {0}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    4u64 << ((ctr >> 16) & 0xf)
}

/// Which cache-maintenance-by-address operation [`dcache_lines`] runs.
#[derive(Clone, Copy)]
enum DcOp {
    /// `dc cvac`: write dirty lines back to the point of coherency.
    Clean,
    /// `dc civac`: write them back and drop them from the cache.
    CleanInvalidate,
}

/// Runs `op` on every data-cache line of `[base, base+len)`, with NO
/// barrier: the one line-stepping loop, which the callers below finish
/// with a single `dsb sy` however many ranges they cover. Works on a line
/// whatever the page's memory type now is.
fn dcache_lines(op: DcOp, base: u64, len: u64) {
    if len == 0 {
        return;
    }
    let line = dcache_line();
    let mut addr = base & !(line - 1);
    while addr < base + len {
        match op {
            DcOp::Clean => unsafe { asm!("dc cvac, {0}", in(reg) addr, options(nostack, preserves_flags)) },
            DcOp::CleanInvalidate => unsafe { asm!("dc civac, {0}", in(reg) addr, options(nostack, preserves_flags)) },
        }
        addr += line;
    }
}

fn dcache_barrier() {
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// Cleans and invalidates `[base, base+size)` from the data cache to the
/// point of coherency (`dc civac`).
fn clean_invalidate(base: u64, size: u64) {
    dcache_lines(DcOp::CleanInvalidate, base, size);
    dcache_barrier();
}

/// Cleans `rows` ranges of `len` bytes, `stride` bytes apart (a glyph's
/// pixel rows, say; `rows` 1 for one range), to the point of coherency
/// (`dc cvac`): writes any dirty lines back to memory, where something that
/// does not look in the CPU's caches can see them - a display engine
/// reading a framebuffer (`fbdev.rs`, `fbconsole.rs`:
/// `docs/testing/testing-pi4.md` Risk 7). A no-op on memory that is not
/// cached (a framebuffer that is a PCI BAR, mapped Device), so callers need
/// not know which kind they have. ONE barrier at the end, not one per
/// range: a barrier per 32-byte glyph row once cost a 1080p screen clear
/// about 259,000 of them.
pub(crate) fn clean_to_poc(base: u64, len: u64, rows: u64, stride: u64) {
    if len == 0 || rows == 0 {
        return;
    }
    for r in 0..rows {
        dcache_lines(DcOp::Clean, base + r * stride, len);
    }
    dcache_barrier();
}

/// A walked attribute for a log line: `0x44`, or `fault`.
struct Attr(Option<u64>);

impl core::fmt::Display for Attr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(a) => write!(f, "{a:#04x}"),
            None => write!(f, "fault"),
        }
    }
}

/// The memory attribute the CPU's own table walk gives `va` (`AT S1E1R`,
/// then `PAR_EL1.ATTR`), or `None` if the walk faults. What the MMU
/// really does, not what the table builder meant it to do.
fn walked_attr(va: u64) -> Option<u64> {
    let par: u64;
    unsafe {
        asm!("at s1e1r, {va}", "isb", "mrs {par}, par_el1", va = in(reg) va, par = out(reg) par, options(nostack, preserves_flags));
    }
    if par & 1 != 0 { None } else { Some(par >> 56) }
}

/// Whether `va` walks as Device memory in the current tables: for a
/// device whose own block may have given way to RAM's (see `main.rs`'s
/// console check after `install_identity_map`).
pub(crate) fn walks_as_device(va: u64) -> bool {
    walked_attr(va) == Some(MAIR_ATTR_DEVICE_NGNRNE)
}

/// Boot-time self-check of the non-cacheable mapping, through the
/// hardware walker, in EVERY task's view (a view with its task's region
/// in the same 1GB block reaches a range through its own split, an empty
/// view through the shared L2, and checking one would miss the other):
/// - every page of every planned range walks as Normal Non-cacheable
///   (`0x44`), not just its two ends, so a wrong middle slot is caught;
/// - the page just before each range and the page at its end do NOT, so
///   a range bleeding into its neighbours is caught;
/// - the kernel's own data (this module's tables) walks as write-back
///   (`0xff`), so kernel memory made non-cacheable by mistake is caught -
///   exclusive (atomic) accesses to it would not be guaranteed to work.
///
/// One line per range, or a WARNING naming the view and address, both
/// recorded and printed later (see [`NcNote`]). Checks
/// the ranges the plan actually made (`planned`), not a second derivation
/// of which ones it should have. QEMU models no caches, so this walk is
/// the part of the non-coherent-DMA fix it CAN check. Switches views to
/// walk them and switches back; runs at boot, interrupts masked, before
/// any task has run.
fn check_attributes(planned: &[(u64, u64)]) {
    let current = crate::tasks::current_index();
    let kernel_va = L0_TABLES.as_ptr() as u64;
    let in_planned = |va: u64| planned.iter().any(|&(s, e)| s <= va && va < e);
    for &(start, end) in planned {
        let mut views = 0usize;
        let mut bad: Option<(usize, u64, Option<u64>, &'static str)> = None;
        for view in TaskIndex::all() {
            activate_task(view);
            let mut page = start;
            while page < end && bad.is_none() {
                let a = walked_attr(page);
                if a != Some(MAIR_ATTR_NORMAL_NC) {
                    bad = Some((view.index(), page, a, "inside the range, not Normal Non-cacheable"));
                }
                page += 4096;
            }
            // A neighbour that is itself planned (two abutting ranges) is
            // meant to be non-cacheable, and is checked as its own range.
            for outside in [start.wrapping_sub(4096), end] {
                let a = walked_attr(outside);
                if a == Some(MAIR_ATTR_NORMAL_NC) && !in_planned(outside) && bad.is_none() {
                    bad = Some((view.index(), outside, a, "outside the range, but Normal Non-cacheable"));
                }
            }
            let k = walked_attr(kernel_va);
            if k != Some(MAIR_ATTR_NORMAL_WB) && bad.is_none() {
                bad = Some((view.index(), kernel_va, k, "kernel data, not write-back"));
            }
            views += 1;
        }
        activate_task(current);
        nc_note(match bad {
            None => NcNote::Checked(start, end, views),
            Some((view, va, a, what)) => NcNote::Bad(start, end, va, a, view, what),
        });
    }
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
/// Must be called with interrupts masked throughout - single-core, so
/// no other code can observe the table set mid-rebuild. Every caller
/// today is an exception entry (SVC, IRQ or EL0 fault) or runs inside
/// one, which is what satisfies this; a caller from ordinary EL1 code
/// with interrupts enabled would not, and must mask them first.
pub(crate) unsafe fn rebuild_with_el0_regions(el0_regions: [(u64, u64); MAX_EL0_REGIONS]) {
    let memory_map = unsafe { (*STORED_MEMORY_MAP.0.get()).as_ref() }
        .expect("install_identity_map must run before rebuild_with_el0_regions");
    let (extra_devices, count) = unsafe { *STORED_EXTRA_DEVICES.get() };
    let (uncached, nc_count) = unsafe { *STORED_UNCACHED.get() };
    let planned = unsafe { build_tables(memory_map, el0_regions, &extra_devices[..count], &uncached[..nc_count], false) };
    unsafe { switch_full(crate::tasks::current_index(), &planned) };
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

/// Writes every view's tables from the inputs, and returns the
/// non-cacheable ranges it planned. Memory only: no register is written
/// here, so the tables in use are unchanged when this returns, and the
/// caller switches ([`switch_full`], or the EL2 drop).
unsafe fn build_tables(
    memory_map: &MemoryMapOwned,
    el0_regions: [(u64, u64); MAX_EL0_REGIONS],
    extra_devices: &[(u64, u64)],
    uncached: &[(u64, u64)],
    log: bool,
) -> Planned {
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
        let shared_l1 = unsafe { &mut *EXTRA_L1_TABLES[slot].get() };
        if shared_l1[l1_idx] == 0 {
            shared_l1[l1_idx] = device_block(base);
            if log {
                crate::console::println!(
                    "Ouroboros kernel: device region {base:#x} (size {size:#x}) outside RAM span, mapped as its own device block (L0 index {l0_idx})"
                );
            }
        }
    }

    // Non-cacheable ranges: exactly the explicit ones (`uncached`), and of
    // those only the ones inside the RAM loop's blocks (see
    // `NcPlan::build`).
    let ram_blocks = (min_addr <= max_addr).then(|| (min_addr / GIB, (max_addr - 1) / GIB));
    let nc = NcPlan::build(&uncached[..uncached.len().min(MAX_UNCACHED)], ram_blocks, log);

    // Build each task's view: identical kernel/device mappings, EL0
    // access granted only to that view's own region.
    // Indexed by the view, not zipped: both bounds are NUM_TASKS, so this
    // is total, where a zip would stop at the shorter side without a word.
    for view in TaskIndex::all() {
        let region = el0_regions[view.index()];
        unsafe {
            build_view(
                view,
                region,
                (min_addr, max_addr),
                &nc,
                &extra_l1_owners,
                next_extra_l1_table,
                &l1_span_devices[..l1_span_device_count],
            )
        };
    }

    // Boot-only: this is a long diagnostic (the full region array). On a
    // framebuffer-only platform (Parallels) the kernel console and the
    // userland console server share the screen, so re-logging it on every
    // runtime rebuild (spawn/exit) would corrupt the server's rendering -
    // see CLAUDE.md's "Driver isolation, part 3".
    if log {
        crate::console::println!(
            "Ouroboros kernel: identity map RAM {min_addr:#x}-{max_addr:#x}, device 0x0-{:#x}, per-task EL0 regions {el0_regions:x?}",
            GIB - 1
        );
    }

    let mut ranges = [(0u64, 0u64); MAX_NC_RANGES];
    ranges[..nc.count].copy_from_slice(&nc.ranges[..nc.count]);
    Planned { ranges, count: nc.count }
}

/// Builds one task's translation-table view: the shared kernel shape
/// (fixed low-1GB device block, discovered RAM as EL1-only 1GB blocks,
/// the shared extra-device L1 tables wired into L0) with exactly one
/// difference per view - `el0_region` (this task's own region, `(0, 0)`
/// for an unused slot) gets its containing 1GB block split down to 4KB
/// pages so only the region's own pages carry EL0 access.
unsafe fn build_view(
    view: TaskIndex,
    el0_region: (u64, u64),
    ram: (u64, u64), // the discovered RAM span, (min, max)
    nc: &NcPlan,
    extra_l1_owners: &[usize; MAX_EXTRA_L1_TABLES],
    extra_l1_count: usize,
    l1_span_devices: &[(usize, u64)],
) {
    let l1 = unsafe { &mut *L1_TABLES[view.index()].get() };
    let (min_addr, max_addr) = ram;

    // One L2 and one L3 per view is enough because a region fits one 2MB
    // slot (the loader's bound plus 2MB-aligned bases, see
    // `install_identity_map`'s safety comment), so it touches exactly one
    // 1GB block and one 2MB slot. Checked rather than trusted, and checked
    // as CONTAINMENT (first and last byte in the same slot) rather than as
    // "a second sub-slot in this block": a region straddling a 1GB
    // boundary would refill the same per-view L2 for the second block, a
    // case a per-block flag cannot see. A region that fails gets NO EL0
    // mapping at all, so the task faults cleanly on its first instruction
    // (a spawned task is then killed and reaped; a supervised server is
    // restarted up to its per-boot cap, repeating this warning; the boot
    // shell or idle task halts the kernel, see `exceptions.rs`) rather
    // than half of it aliasing the other half (the program then executes
    // zeros at its own base, which is how this was found). Forced past
    // the console's quiet mode, since on the framebuffer platforms it is
    // the only trace the fault will leave; and DEFERRED when there is no
    // console yet at all, which is the case for every boot-time view on
    // those same platforms (the framebuffer console comes up after
    // `install_identity_map`), so `main.rs` prints it later.
    let (base, size) = el0_region;
    let contained = size == 0 || base / MIB2 == (base + size - 1) / MIB2;
    let el0_region = if contained {
        el0_region
    } else {
        if crate::console::is_installed() {
            refusal_warning(base, size);
        } else {
            DEFERRED_REFUSAL.0.store(base, Ordering::Relaxed);
            DEFERRED_REFUSAL.1.store(size, Ordering::Relaxed);
        }
        (0, 0)
    };

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
                // so only the region's own pages get EL0 access (one
                // sub-slot: the containment check above).
                let l2 = unsafe { &mut *EL0_L2_TABLES[view.index()].get() };
                for (i, entry) in l2.iter_mut().enumerate() {
                    let sub_base = block_start + (i as u64) * MIB2;
                    let sub_end = sub_base + MIB2;
                    if overlaps(el0_region, sub_base, sub_end) {
                        let l3 = unsafe { &mut *EL0_L3_TABLES[view.index()].get() };
                        // The loader owns the region layout; ask it.
                        let guard = guard_page_addr(el0_region);
                        for (j, page) in l3.iter_mut().enumerate() {
                            let page_base = sub_base + (j as u64) * 4096;
                            let page_end = page_base + 4096;
                            *page = if Some(page_base) == guard {
                                // The stack guard page: inside the EL0
                                // region but mapped EL1-only, so a stack
                                // overflow into it takes a clean EL0 fault
                                // instead of corrupting the code below.
                                kernel_page_4k(page_base)
                            } else if overlaps(el0_region, page_base, page_end) {
                                el0_page_4k(page_base)
                            } else {
                                nc.kernel_page(page_base)
                            };
                        }
                        *entry = table_desc(EL0_L3_TABLES[view.index()].get() as u64);
                    } else {
                        // The same entry every view's shared tables carry
                        // for this slot (non-cacheable where the plan says).
                        *entry = nc.slot_entry(sub_base);
                    }
                }
                l1[idx] = table_desc(EL0_L2_TABLES[view.index()].get() as u64);
            } else if let Some(shared) = nc.l2_for_block(block) {
                // A block holding a non-cacheable range, and not this
                // view's EL0 block: the shared L2 every view uses.
                l1[idx] = shared;
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

    let l0 = unsafe { &mut *L0_TABLES[view.index()].get() };
    l0[0] = table_desc(L1_TABLES[view.index()].get() as u64);
    // Shared extra-device L1 tables (64-bit PCI BARs past 512GB, etc.) -
    // identical entries in every view, so every view's L0 points at the
    // same tables.
    for slot in 0..extra_l1_count {
        let l0_idx = extra_l1_owners[slot];
        if l0_idx != usize::MAX && l0_idx < ENTRIES_PER_TABLE {
            l0[l0_idx] = table_desc(EXTRA_L1_TABLES[slot].get() as u64);
        }
    }
}

/// The L0 table for `view`. A [`TaskIndex`] is below `NUM_TASKS` by
/// construction and `MAX_EL0_REGIONS` *is* `NUM_TASKS`, so this index is
/// total: a slot with no view cannot be spelled. (This replaced, in
/// turn, a clamp onto the last view, which would have run an
/// out-of-range task under another task's tables, and then a runtime
/// refusal, which no real caller could reach.)
fn l0_table(view: TaskIndex) -> &'static Table {
    &L0_TABLES[view.index()]
}

/// The per-context-switch table switch: points TTBR0_EL1 at `view`'s
/// table set and invalidates the TLB. Called by `tasks.rs` wherever
/// the current task changes - the moment the following `eret` lands in
/// EL0, the new task can only see its own region.
///
/// `view` is a task slot, and there are exactly as many views as slots
/// (`MAX_EL0_REGIONS` is `NUM_TASKS`); [`l0_table`] is the lookup.
/// Taking a [`TaskIndex`] rather than a `usize` is what makes a slot
/// with no view unspellable at every call site.
///
/// The full `tlbi vmalle1` on every switch is the stage-2
/// correctness-first design: with a single ASID, entries cached under
/// the previous view would otherwise satisfy the next task's walks.
/// The planned stage-3 refinement (per-task ASIDs + nG-tagged EL0
/// entries) makes this a plain TTBR0 write; if that ever misbehaves on
/// real hardware, this version is the known-correct fallback.
pub(crate) fn activate_task(view: TaskIndex) {
    let ttbr0 = l0_table(view).get() as u64;
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

/// The EL1 translation regime's register values for `view`'s table set:
/// what [`switch_full`] writes at EL1, and what the EL2 drop writes into
/// the `_EL1` registers from EL2 before its `eret` (`el2.rs`). One
/// derivation, so the two paths cannot disagree, and constructible only
/// with the [`Planned`] a build returned, so the drop, like
/// [`switch_full`], cannot be spelled without a build.
#[derive(Clone, Copy)]
pub(crate) struct El1Regime {
    pub(crate) mair: u64,
    pub(crate) tcr: u64,
    pub(crate) ttbr0: u64,
}

fn el1_regime(view: TaskIndex, _built: &Planned) -> El1Regime {
    let mair_el1: u64 =
        (MAIR_ATTR_DEVICE_NGNRNE << (8 * MAIR_IDX_DEVICE_NGNRNE))
            | (MAIR_ATTR_NORMAL_WB << (8 * MAIR_IDX_NORMAL_WB))
            | (MAIR_ATTR_NORMAL_NC << (8 * MAIR_IDX_NORMAL_NC));

    // T0SZ=20 -> 44-bit input address space, walk starting at L0 -
    // deliberately matching firmware's own T0SZ (on an EL1 handoff; on
    // the EL2 drop there is no firmware EL1 regime to match and the same
    // value is kept, see el2.rs), not a smaller/simpler
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

    let ttbr0_el1 = l0_table(view).get() as u64;
    El1Regime { mair: mair_el1, tcr: tcr_el1, ttbr0: ttbr0_el1 }
}

/// The full MAIR/TCR/TTBR0 configuration sequence, switching to
/// `view`'s table set - the boot-time install at EL1 and every runtime
/// rebuild end here, each with the [`Planned`] its build just returned,
/// which is what makes this unspellable without a build. [`activate_task`]
/// is the lighter switch-only sibling used on every context switch; both
/// look the view up through [`l0_table`].
unsafe fn switch_full(view: TaskIndex, built: &Planned) {
    let El1Regime { mair: mair_el1, tcr: tcr_el1, ttbr0: ttbr0_el1 } = el1_regime(view, built);

    // Masked and left masked: between the MAIR/TCR write and the TTBR0
    // switch below, code is still running under firmware's *old* tables
    // but *new* attribute-index semantics - a narrow, correctly-barriered
    // window, but not one worth letting an interrupt land in for free.
    // (Nothing at EL1 ever unmasks again: the first `eret` into task 0
    // restores that task's SPSR, and the tick is delivered to EL0 only -
    // see `synccell.rs` for why that is load-bearing.)
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

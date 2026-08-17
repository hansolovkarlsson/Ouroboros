//! Two EL0 tasks, alternated by the timer tick — real preemptive
//! switching, not just resuming whatever the tick happened to interrupt
//! (all `exceptions.rs`'s IRQ path could do before this).
//!
//! **Task 0 is a real userland program, loaded from disk** (`loader.rs`,
//! before `exit_boot_services`) rather than compiled into the kernel image
//! — see `docs/processes.md` for the full design. By the time [`init`]
//! runs, `loader.rs` has already copied the program's bytes into an
//! EL0-accessible region and `mmu.rs` has mapped it; there's nothing left
//! to copy here, just cache maintenance (see [`clean_dcache_range`]) and a
//! [`Context`] pointing at it. The default program is `shell` (a separate
//! crate, `shell/`) — the interactive line editor that used to live at EL1
//! in this kernel's own (now-deleted) `shell.rs`, driven by a dedicated
//! `shell_input` syscall. That syscall is gone: a loaded program just
//! calls `try_read_char`/`putc` directly and does its own line editing, in
//! its own separately-compiled code. Which program loads is a config file
//! on the ESP, not a kernel constant — replacing the shell means replacing
//! a file, not rebuilding the kernel.
//!
//! **Task 1 is a genuine idle task** — a busy-spin loop (`nop; b 1b`),
//! still a small compiled-in `global_asm!` blob like every EL0 task
//! before this milestone, since there's nothing to load for "do nothing
//! forever" and its region is tiny (4KB) enough that the alignment
//! ceiling below never applied to it in the first place (that ceiling
//! was specifically about *large* alignments — see `IDLE_REGION`'s doc
//! comment).
//!
//! **This was `wfe` (wait-for-event, the architecturally "correct",
//! power-efficient choice) until a real, confirmed bug on real Parallels
//! hardware forced it to change - worth knowing before reverting this.**
//! The very first real task switch on real Parallels hardware (the
//! MADT/GICv3 milestone - see `CLAUDE.md` - finally made GIC/timer IRQs
//! work there at all) hung the whole system outright: no exception
//! reported, keystrokes stopped being echoed, indistinguishable from a
//! dead machine. A single-variable diagnostic (temporarily skip just the
//! task-switch call, leave GIC/timer otherwise fully active) proved
//! IRQ delivery itself was solid there - `uptime` kept incrementing
//! correctly. That isolated the hang to *this loop specifically*, and
//! swapping `wfe` for a busy-spin (with task switching left fully
//! enabled) fixed it completely, confirmed by a sustained real-hardware
//! test across several interactive commands with no hang. **Root cause
//! not fully confirmed, only worked around** - the leading hypothesis is
//! that real hardware's `wfe` is trapped/emulated by the host hypervisor
//! (Apple's own virtualization layer, one level above this guest kernel
//! entirely) in a way QEMU/TCG's `wfe` never is: this kernel's
//! `nTWE`/`nTWI` bits (see [`init`]) only ever controlled whether EL0's
//! own `wfe` traps *to EL1*, a decision this kernel owns completely;
//! whatever EL2 does to `wfe` above that is outside this kernel's
//! control or visibility. Not yet tested further. The practical
//! consequence of the fix is just power efficiency (this task now spins
//! instead of actually idling) - correctness-wise it's indistinguishable
//! from `wfe`, and this kernel doesn't optimize for idle power anywhere
//! else either (task 0's own I/O polling already busy-waits).
//!
//! There's no cooperative yielding anywhere — the *only* thing that ever
//! moves execution from one task to the other is the timer tick catching
//! one mid-loop and swapping its saved [`Context`] for the other task's.
//! That swap is the entire scheduler: strict round-robin between exactly
//! two tasks, no priorities, no blocking, no queue.
//!
//! FP/SIMD state still isn't part of a task's [`Context`] (see
//! `exceptions.rs`'s module doc comment) — fine for these tasks, since
//! neither touches it, but a real limitation for whatever runs here next.

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::exceptions::Context;
use crate::loader::LoadedProgram;

/// Slots 0/1 are always the loaded program and the idle task (`init`);
/// 2/3 start `Unused` and are only ever filled in by `spawn` (dynamic
/// task creation). A small, fixed bound rather than anything growable -
/// "generous but bounded," the same philosophy `mmu.rs`'s
/// `MAX_EXTRA_L1_TABLES` already states for a similar "how many of these
/// might we need" question - not a real limit this kernel has any way to
/// raise gracefully yet (`spawn` just fails with `SpawnError::NoFreeSlot`
/// past this).
pub const NUM_TASKS: usize = 4;

/// 2MB - matches `loader.rs`'s own `SLOT_ALIGN` (a plain numeric
/// constant, not shared across modules - simplest to just duplicate the
/// value rather than build a shared-constants module for one number).
/// Every region [`allocate_runtime_region`] hands out is a multiple of
/// this, satisfying `mmu.rs`'s "each EL0 region fits inside one 2MB slot"
/// invariant the same way `loader.rs`'s own over-allocate-and-trim trick
/// already does for task 0's boot-time-loaded program.
const RUNTIME_SLOT_ALIGN: u64 = 0x20_0000;

/// Bump allocator for dynamically `spawn`ed programs' EL0 regions -
/// deliberately the simplest correct thing, not a real allocator: grows
/// *downward* from the top of discovered RAM (`init_runtime_allocator`),
/// never frees, never reuses an address. No destruction exists yet (see
/// CLAUDE.md's "dynamic task creation" milestone for why that's a
/// deliberate scope cut, not an oversight), so this can never be asked to
/// hand back an address it already gave out. Growing *downward* from
/// `mmu::ram_span`'s `max_addr` means this never needs to know where the
/// kernel's own image, task 0's loaded program, or task 1's idle region
/// actually sit - all three are guaranteed to be somewhere *below*
/// wherever this starts, since `max_addr` is by definition past every
/// general-RAM descriptor the UEFI memory map ever reported, including
/// whichever ones cover them.
static NEXT_RUNTIME_REGION_TOP: AtomicU64 = AtomicU64::new(0);

/// Must be called once, after `mmu::install_identity_map`'s first call
/// (needs `mmu::ram_span`, which needs a stashed `memory_map`) and before
/// any `allocate_runtime_region` call.
pub(crate) fn init_runtime_allocator() {
    let (_, max_addr) = crate::mmu::ram_span();
    NEXT_RUNTIME_REGION_TOP.store(max_addr & !(RUNTIME_SLOT_ALIGN - 1), Ordering::Relaxed);
}

/// Hands out `size` bytes (rounded up to a 2MB multiple) of fresh RAM,
/// already identity-mapped EL1-accessible (all of discovered RAM is,
/// unconditionally - see `mmu.rs`) but not yet EL0-accessible; the caller
/// still has to fold the returned `(base, size)` into a fresh call to
/// `mmu::rebuild_with_el0_regions` before any EL0 task can actually touch
/// it. Never fails, never reuses an address - see this module's own doc
/// comment for why that's an accepted, deliberate limit for now rather
/// than a real allocator.
pub(crate) fn allocate_runtime_region(size: u64) -> u64 {
    let aligned_size = size.next_multiple_of(RUNTIME_SLOT_ALIGN);
    NEXT_RUNTIME_REGION_TOP.fetch_sub(aligned_size, Ordering::Relaxed) - aligned_size
}

/// Gives a region back to the bump allocator **iff it was the most
/// recent allocation** (the cursor still sits exactly at its base -
/// LIFO order); anything else leaks, deliberately: this is a bump
/// cursor, not a real allocator with a free list, and the common
/// exec-then-exit pattern is exactly the LIFO case. `size` must be the
/// same value the allocation was asked for (re-rounded to the same 2MB
/// multiple here). Called from the `EXIT` syscall's teardown
/// (`syscall.rs`), where a leak is a bounded, documented cost - a
/// long-lived middle task exiting after a later allocation just means
/// that one region stays unavailable for the rest of the boot.
pub(crate) fn free_runtime_region(base: u64, size: u64) {
    let aligned_size = size.next_multiple_of(RUNTIME_SLOT_ALIGN);
    let _ = NEXT_RUNTIME_REGION_TOP.compare_exchange(
        base,
        base + aligned_size,
        Ordering::Relaxed,
        Ordering::Relaxed,
    );
}

const IDLE_REGION_SIZE: usize = 0x1000;

/// 4KB, well under the ~8KB rustc/PE-COFF alignment ceiling that forced
/// `loader.rs` to load task 0's program at runtime instead of compiling it
/// in (see that module's doc comment) — that ceiling only bites at large
/// alignments; a single page was always fine, which is why the idle task
/// alone never needed to move off this compile-time-static approach.
#[repr(align(0x1000))]
struct IdleRegion(UnsafeCell<[u8; IDLE_REGION_SIZE]>);

// SAFETY: single-core; written once by `init` before either task ever
// runs, and only EL0 (isolated to this one region by `mmu.rs`) touches it
// after that.
unsafe impl Sync for IdleRegion {}

static IDLE_REGION: IdleRegion = IdleRegion(UnsafeCell::new([0; IDLE_REGION_SIZE]));

global_asm!(
    r#"
.text
.global el0_idle_template
el0_idle_template:
1:
nop
b 1b
.global el0_idle_template_end
el0_idle_template_end:
"#
);

unsafe extern "C" {
    /// Opaque - only used for its address, to find and size the template
    /// to copy.
    static el0_idle_template: c_void;
    static el0_idle_template_end: c_void;
}

struct TaskSlot(UnsafeCell<Context>);

// SAFETY: single-core; only ever touched from EL1 with IRQs masked
// (`init`, before either task runs) or from within the IRQ trampoline
// itself (`on_tick`, which by construction can't run re-entrantly - taking
// an exception masks further IRQs until the next `eret`).
unsafe impl Sync for TaskSlot {}

static TASKS: [TaskSlot; NUM_TASKS] = [
    TaskSlot(UnsafeCell::new(Context::zeroed())),
    TaskSlot(UnsafeCell::new(Context::zeroed())),
    TaskSlot(UnsafeCell::new(Context::zeroed())),
    TaskSlot(UnsafeCell::new(Context::zeroed())),
];

static CURRENT: AtomicUsize = AtomicUsize::new(0);

/// Each task's own `(base, size)` EL0 region, recorded once at creation
/// time (`init` for slots 0/1, `spawn` for any later slot) and never
/// changed after - there's no task destruction yet, so a region, once
/// assigned, is permanent for the rest of this boot. Needed so `spawn`
/// can rebuild the *full* `el0_regions` array `mmu::rebuild_with_el0_regions`
/// expects (every still-live task's region, not just the newly-added
/// one) without walking anything else to reconstruct it.
struct RegionSlot(UnsafeCell<(u64, u64)>);

// SAFETY: same argument as `TaskSlot` above.
unsafe impl Sync for RegionSlot {}

static REGIONS: [RegionSlot; NUM_TASKS] = [
    RegionSlot(UnsafeCell::new((0, 0))),
    RegionSlot(UnsafeCell::new((0, 0))),
    RegionSlot(UnsafeCell::new((0, 0))),
    RegionSlot(UnsafeCell::new((0, 0))),
];

/// The `el0_regions` array every `mmu::rebuild_with_el0_regions` call
/// needs: every task slot's own region, `(0, 0)` for any `Unused` one
/// (already treated as "no region" by `mmu.rs`'s `overlaps_any`, same
/// convention `main.rs`'s own boot-time call already relies on).
/// Which task is currently executing - `syscall.rs`'s `EXIT` arm uses
/// this to refuse exits from tasks 0/1 and to know whose region to free.
pub(crate) fn current_task() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

/// The `(base, size)` region recorded for task `i` at creation time
/// (`(0, 0)` for the unused/never-created case).
pub(crate) fn task_region(i: usize) -> (u64, u64) {
    unsafe { *REGIONS[i].0.get() }
}

/// Task `i`'s state as its ABI code (`syscall-abi`'s `TASK_STATE_*`) -
/// the `TASK_STATE` syscall's whole implementation, bounds check aside.
pub(crate) fn task_state_code(i: usize) -> u64 {
    match unsafe { *STATES[i].0.get() } {
        TaskState::Unused => syscall_abi::TASK_STATE_UNUSED,
        TaskState::Runnable => syscall_abi::TASK_STATE_RUNNABLE,
        TaskState::Blocked(_) => syscall_abi::TASK_STATE_BLOCKED,
    }
}

pub(crate) fn el0_regions() -> [(u64, u64); NUM_TASKS] {
    core::array::from_fn(|i| unsafe { *REGIONS[i].0.get() })
}

/// The one task whose `Blocked(WaitReason::Keyboard)` the wake-check
/// will ever actually poll hardware for - see `on_tick`'s doc comment
/// for why exactly one owner exists and what happens to every other
/// task blocked the same way. Runtime state now (it was a hardcoded
/// `const 0` until job control existed): the `FG` syscall reassigns it
/// ([`set_input_owner`]), and **any death of the current owner reverts
/// it to task 0** ([`revert_input_owner_if`], wired into both the
/// `EXIT` and `KILL` teardowns) - task 0 can never die (both refuse
/// it), so the revert target is always valid, the same permanence
/// argument the original hardcoding relied on, now load-bearing for
/// the revert too.
static INPUT_OWNER: AtomicUsize = AtomicUsize::new(0);

/// `FG`'s effect: hand the keyboard to task `owner`. Validation (index
/// in range, slot occupied, not idle) is the syscall layer's job -
/// this just stores.
pub(crate) fn set_input_owner(owner: usize) {
    INPUT_OWNER.store(owner, Ordering::Relaxed);
}

/// If `dying` currently owns the keyboard, hand it back to task 0 -
/// called from both task-death paths (`EXIT`'s teardown and `KILL`'s),
/// so a foregrounded task's death always returns the terminal to the
/// boot shell instead of leaving input routed at an empty slot.
pub(crate) fn revert_input_owner_if(dying: usize) {
    let _ = INPUT_OWNER.compare_exchange(dying, 0, Ordering::Relaxed, Ordering::Relaxed);
}

/// What a blocked task is waiting for. One variant today - a byte from
/// the keyboard/console (`syscall_abi::READ_CHAR`, `syscall.rs`) - but
/// deliberately a real enum, not a bool, since the mechanism below
/// (`on_tick`'s wake-check, `block_current_and_switch`) doesn't care how
/// many reasons exist, only that each one can be evaluated to
/// `Option<u64>` (the value to hand back to the task once it's ready).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum WaitReason {
    Keyboard,
}

impl WaitReason {
    /// `Some(value)` if this reason is satisfied right now, consuming
    /// whatever made it so (e.g. the keyboard byte itself) - `None` means
    /// keep waiting. Delegates to `syscall.rs` since that's already where
    /// the console/xhci polling logic lives; `tasks.rs` doesn't otherwise
    /// know anything about consoles or keyboards.
    fn poll(self) -> Option<u64> {
        match self {
            WaitReason::Keyboard => crate::syscall::poll_keyboard_byte().map(u64::from),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TaskState {
    /// No real task in this slot - `spawn`'s only candidate for a new
    /// one. Slots 0/1 (the loaded program, the idle task) never reach
    /// this state; they're always one of the other two once `init` runs.
    Unused,
    Runnable,
    Blocked(WaitReason),
}

struct StateSlot(UnsafeCell<TaskState>);

// SAFETY: same argument as `TaskSlot` above - single-core, only touched
// from EL1 with IRQs masked.
unsafe impl Sync for StateSlot {}

static STATES: [StateSlot; NUM_TASKS] = [
    StateSlot(UnsafeCell::new(TaskState::Runnable)),
    StateSlot(UnsafeCell::new(TaskState::Runnable)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
];

/// Scans forward from `from` (exclusive), wrapping, for the next
/// `Runnable` task - falls back to `from` itself if nothing else is
/// runnable. That fallback is unreachable today (task 1, the idle task,
/// never blocks - see this module's doc comment for why it can't safely
/// use `wfe` either, so it has to stay a real, always-runnable busy-spin),
/// but a safe "stay put" is the right behavior if that ever stops being
/// true, not a panic.
fn next_runnable(from: usize) -> usize {
    for offset in 1..=NUM_TASKS {
        let candidate = (from + offset) % NUM_TASKS;
        if unsafe { *STATES[candidate].0.get() } == TaskState::Runnable {
            return candidate;
        }
    }
    from
}

/// Suspends the calling task (whichever one is live in `frame` right now)
/// until `reason` is satisfied, and switches to another runnable task
/// immediately - called from `syscall.rs`'s dispatch table when a
/// blocking syscall has nothing to return yet.
///
/// **Why this, and not just returning a "would block" sentinel and
/// letting the caller `wfe`:** real Parallels hardware has a confirmed,
/// unresolved hang when an EL0 task executes `wfe` (see this module's
/// doc comment - task 1's idle loop had to become a busy-spin because of
/// it). A task that blocked by calling `wfe` itself would almost
/// certainly hit the same hang. This function never executes `wfe`
/// anywhere: the calling task's context is saved exactly like a normal
/// preemption, and a different task's already-saved context is loaded in
/// its place - the blocked task simply doesn't run again until `on_tick`
/// wakes it, the same mechanism that already safely idles task 1 today.
///
/// **The return value is not a normal function result - the caller
/// (`syscall::dispatch`) must return it unmodified as its own return
/// value.** `exceptions.rs`'s SVC trampoline unconditionally writes
/// `dispatch`'s return value into `frame`'s `x0` slot after the call
/// (`str x0, [sp, #0]`) - fine for a syscall that resumes its own
/// caller, but this function just overwrote `*frame` with a *different*
/// task's entire saved context. Returning `frame.gpr[0]` (that task's
/// own `x0`, already sitting there after the overwrite) turns the
/// trampoline's blind write into a harmless no-op instead of clobbering
/// the resumed task's real return value - the whole mechanism works
/// without changing the trampoline's control flow at all.
///
/// # Safety
/// `frame` must be the live trap frame of the syscall currently being
/// dispatched (true when called from `syscall::dispatch`, its only
/// caller).
pub(crate) unsafe fn block_current_and_switch(frame: *mut Context, reason: WaitReason) -> u64 {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    unsafe { *TASKS[current].0.get() = *frame };
    unsafe { *STATES[current].0.get() = TaskState::Blocked(reason) };
    let next = next_runnable(current);
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
    frame.gpr[0]
}

/// Destroys the calling task (whichever one is live in `frame`) and
/// switches to another runnable one - the `EXIT` syscall's mechanism,
/// mirroring [`block_current_and_switch`]'s proven shape exactly, with
/// one difference: the current task's context is *discarded* rather
/// than saved (its slot goes `Unused`, its region record is cleared so
/// the caller's `mmu::rebuild_with_el0_regions` drops the mapping - the
/// `(0, 0)` entry the mmu region loop already tolerates). The same
/// return-value contract applies: the caller must return this value
/// unmodified, so the SVC trampoline's blind `x0` write is a no-op for
/// the *resumed* task (see `block_current_and_switch`'s doc comment).
///
/// `next_runnable` can't hand back the now-`Unused` current task: its
/// "stay put" fallback only fires if *nothing* is runnable, and task 1
/// (idle) is always runnable - the same standing guarantee that
/// fallback's own doc comment records.
///
/// The caller is responsible for refusing tasks 0/1 *before* calling
/// this, and for the region free + mmu rebuild around it - see
/// `syscall.rs`'s `EXIT` arm for the full teardown order.
///
/// # Safety
/// Same contract as [`block_current_and_switch`]: `frame` must be the
/// live trap frame of the syscall currently being dispatched.
pub(crate) unsafe fn exit_current_and_switch(frame: *mut Context) -> u64 {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    unsafe { *STATES[current].0.get() = TaskState::Unused };
    unsafe { *REGIONS[current].0.get() = (0, 0) };
    let next = next_runnable(current);
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
    frame.gpr[0]
}

/// `KILL`'s teardown: destroys task `i` - which must not be the
/// currently-running task (the syscall layer's validation guarantees
/// this today: only tasks ≥ 2 can be killed, and the caller is always
/// whichever task is running). Same bookkeeping as
/// [`exit_current_and_switch`] minus the context switch - a
/// non-current task isn't executing (single core, IRQs masked
/// throughout SVC dispatch; it's parked at an `eret` boundary), so its
/// saved context is simply discarded. The caller handles the
/// region-free, owner-revert, and mmu rebuild around this, same as the
/// `EXIT` arm does - see `syscall.rs`.
pub(crate) fn kill_task(i: usize) {
    unsafe { *STATES[i].0.get() = TaskState::Unused };
    unsafe { *REGIONS[i].0.get() = (0, 0) };
}

/// Whether slot `i` currently holds a task (`Runnable` or `Blocked`) -
/// the `KILL`/`FG` syscalls' existence check.
pub(crate) fn task_exists(i: usize) -> bool {
    i < NUM_TASKS && unsafe { *STATES[i].0.get() } != TaskState::Unused
}

pub(crate) enum SpawnError {
    /// Every task slot already holds a real task - see `NUM_TASKS`'s own
    /// doc comment for why this kernel doesn't grow past a small fixed
    /// bound rather than trying to handle an unbounded number of tasks.
    NoFreeSlot,
}

/// Adds a new task, running `context` (already pointed at the loaded
/// program's real entry point and its own stack, same shape `init` gives
/// task 0) in the first `Unused` slot found - `syscall.rs`'s `spawn`
/// syscall's only caller, after it's already allocated the program's
/// region (`allocate_runtime_region`), loaded it (`loader::populate_region`),
/// and made that region EL0-accessible (`mmu::rebuild_with_el0_regions`,
/// which needs `region` recorded here *before* it's called - see
/// `el0_regions`). This kernel calls it *spawn*, not POSIX
/// *exec*-replaces-current-process semantics: the calling task keeps
/// running unchanged, a new one joins it. Never touches slots 0/1 (the
/// loaded program and the idle task) - only scans from slot 2 onward.
pub(crate) fn spawn(context: Context, region: (u64, u64)) -> Result<usize, SpawnError> {
    for i in 2..NUM_TASKS {
        if unsafe { *STATES[i].0.get() } == TaskState::Unused {
            unsafe { *TASKS[i].0.get() = context };
            unsafe { *REGIONS[i].0.get() = region };
            unsafe { *STATES[i].0.get() = TaskState::Runnable };
            return Ok(i);
        }
    }
    Err(SpawnError::NoFreeSlot)
}

/// Task 1's (the idle task's) small dedicated EL0 region — `mmu.rs` maps
/// this alongside whatever `loader.rs` loaded for task 0. Two independent
/// regions, not one shared region split in half like before this
/// milestone: task 0's is sized to whatever program got loaded, not a
/// fixed slot.
pub fn idle_region() -> (u64, u64) {
    (IDLE_REGION.0.get() as u64, IDLE_REGION_SIZE as u64)
}

/// Builds task 0's [`Context`] from an already-loaded program — `loader.rs`
/// copied its bytes into an EL0-accessible region during boot services, so
/// there's nothing left to copy here, just cache maintenance — copies task
/// 1's idle loop into its own small region, and disables the EL0
/// `wfe`/`wfi` trap both tasks' idle loops rely on. Does not itself start
/// anything — see [`start`].
///
/// # Safety
/// Must run after `mmu.rs` has mapped both `program`'s region and
/// [`idle_region`] EL0-accessible, and before either task could possibly
/// run.
pub unsafe fn init(program: &LoadedProgram) {
    // Task 0: entry is the program's real ELF entry point, not just its
    // load base - loader.rs computes `entry = base + e_entry` (they
    // happen to be equal today, since shell/linker.ld keeps `_start` at
    // file/VA offset 0, but tasks.rs shouldn't assume that itself - see
    // LoadedProgram::entry's own doc comment). Stack at the top of the
    // loaded region, growing down, same shape every EL0 task has used.
    *unsafe { &mut *TASKS[0].0.get() } = Context {
        gpr: [0; 31],
        sp_el0: program.base + program.size,
        elr_el1: program.entry,
        // M[3:0]=0000 selects EL0t (the only mode EL0 has), DAIF all
        // clear (every exception class unmasked - the timer tick must be
        // able to preempt), NZCV cleared.
        spsr_el1: 0,
    };
    unsafe { *REGIONS[0].0.get() = (program.base, program.size) };
    clean_dcache_range(program.base, program.size);

    // Task 1: idle loop, copied into its own small static region exactly
    // like every EL0 task before this milestone.
    let idle_start = IDLE_REGION.0.get() as *mut u8;
    let start = &raw const el0_idle_template as *const u8;
    let end = &raw const el0_idle_template_end as *const u8;
    let len = unsafe { end.offset_from(start) } as usize;
    unsafe { core::ptr::copy_nonoverlapping(start, idle_start, len) };

    let idle_addr = idle_start as u64;
    *unsafe { &mut *TASKS[1].0.get() } = Context {
        gpr: [0; 31],
        sp_el0: idle_addr + IDLE_REGION_SIZE as u64,
        elr_el1: idle_addr,
        spsr_el1: 0,
    };
    unsafe { *REGIONS[1].0.get() = (idle_addr, IDLE_REGION_SIZE as u64) };
    clean_dcache_range(idle_addr, IDLE_REGION_SIZE as u64);

    unsafe {
        asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack));
    }

    // nTWE/nTWI (SCTLR_EL1 bits 18/16 - the "n" means *disable* trapping,
    // confirmed against Linux's arch/arm64/tools/sysreg): without this,
    // EL0's own `wfe`/`wfi` traps to EL1 as a synchronous exception
    // instead of executing. Set once here, globally, for both tasks -
    // not per-task state.
    unsafe {
        asm!(
            "mrs {tmp}, sctlr_el1",
            "orr {tmp}, {tmp}, {bits}",
            "msr sctlr_el1, {tmp}",
            "isb",
            tmp = out(reg) _,
            bits = in(reg) (1u64 << 18) | (1u64 << 16),
            options(nostack),
        );
    }
}

/// Standard ARM self-modifying-code sequence (clean D-cache, invalidate
/// I-cache, before anything executes what was just written) - the
/// invalidate half is one call for the whole address space
/// (`ic ialluis`, done once by the caller after every region is cleaned),
/// but `dc cvau` only cleans a single cache line (64 bytes on cortex-a72 -
/// not queried from `CTR_EL0`, just assumed, same as the rest of this
/// project's QEMU-shaped conventions) at a time, so a range bigger than
/// that needs one call per line to actually be correct. The version this
/// replaced called `dc cvau` exactly once regardless of region size - not
/// wrong on QEMU (TCG doesn't model real cache incoherency, so this has
/// never actually been exercised against hardware that would notice), but
/// an unverified simplification that stopped being safe to carry forward
/// once task 0's code could be bigger than one cache line.
fn clean_dcache_range(addr: u64, len: u64) {
    const CACHE_LINE: u64 = 64;
    let mut offset = 0;
    while offset < len {
        unsafe {
            asm!("dc cvau, {addr}", addr = in(reg) addr + offset, options(nostack));
        }
        offset += CACHE_LINE;
    }
}

/// Drops from EL1 into task 0. Never returns to its caller — the only way
/// back to EL1 is a trap (syscall, fault, or the timer tick), handled
/// entirely through `exceptions.rs`'s vector table from here on; every
/// subsequent switch happens inside the tick's IRQ trampoline, not here.
///
/// # Safety
/// Must be called after [`init`].
pub unsafe fn start() -> ! {
    let ctx = unsafe { *TASKS[0].0.get() };
    unsafe {
        asm!(
            "msr sp_el0, {sp_el0}",
            "msr elr_el1, {elr}",
            "msr spsr_el1, {spsr}",
            "eret",
            sp_el0 = in(reg) ctx.sp_el0,
            elr = in(reg) ctx.elr_el1,
            spsr = in(reg) ctx.spsr_el1,
            options(noreturn),
        );
    }
}

/// The entire scheduler: first, give every `Blocked` task a chance to
/// wake (see the wake-check below); then save whatever the tick just
/// interrupted into its task's slot, and load the next *runnable* task's
/// saved state into `frame` in its place. `exceptions.rs`'s trampoline
/// does the rest — it doesn't know or need to know a switch happened, it
/// just restores from `frame` and `eret`s, same as if nothing here had
/// touched it.
///
/// **Wake-check:** for every task currently `Blocked(reason)`, evaluate
/// `reason` (`WaitReason::poll`) - if it's satisfied, stash the resulting
/// value into that task's *saved* `Context.gpr[0]` (its `x0`) and mark it
/// `Runnable`. When that task is later switched to (here or on a future
/// tick), it resumes exactly where its blocking syscall left off, with
/// `x0` already holding the value - indistinguishable, from the task's
/// own point of view, from the syscall having simply returned it. This
/// is the only place a blocked task's wait condition is ever checked -
/// worst-case wake latency is one tick period (`timer::TICK_INTERVAL_MS`),
/// the same bound this kernel's keyboard input has always had, blocking
/// or not.
///
/// **Keyboard input is routed to exactly one task, [`INPUT_OWNER`],
/// not to "whichever blocked task this loop reaches first" - a real bug,
/// not a hypothetical.** `poll_keyboard_byte` (what `WaitReason::Keyboard`
/// polls) destructively consumes a byte the instant *any* caller asks for
/// one - it has no notion of which task "should" get it. Polling it once
/// per blocked task, in index order, meant a single keystroke went to
/// whichever task happened to still be `Blocked(Keyboard)` at that exact
/// tick, which flips from tick to tick as tasks trade being blocked and
/// running - confirmed by testing (`exec`ing a second interactive shell
/// and watching keystrokes arrive split, letter by letter, between the
/// two). Skipping the poll entirely for every non-owner task fixes this
/// without needing any buffering of its own: an unconsumed byte simply
/// stays queued in the console/xHCI driver's own hardware buffer -
/// `poll_keyboard_byte` never touches it - until the owner task's own
/// wait is what asks. A task other than the owner that blocks on
/// `Keyboard` just stays blocked (this kernel has no `fg`/`bg` or
/// controlling-task handoff to ever change that) - honest, not silently
/// wrong, and it's what makes `exec`ing a second program that reads
/// input behave like a real background task rather than a second
/// terminal racing the first for every keystroke.
///
/// # Safety
/// `frame` must be the live trap frame of the exception currently being
/// handled (true when called from `exceptions.rs`'s `rust_irq_handler`,
/// its only caller).
pub unsafe fn on_tick(frame: *mut Context) {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);

    for i in 0..NUM_TASKS {
        let TaskState::Blocked(reason) = (unsafe { *STATES[i].0.get() }) else { continue };
        if reason == WaitReason::Keyboard && i != INPUT_OWNER.load(Ordering::Relaxed) {
            continue;
        }
        if let Some(value) = reason.poll() {
            unsafe { (*TASKS[i].0.get()).gpr[0] = value };
            unsafe { *STATES[i].0.get() = TaskState::Runnable };
        }
    }

    let next = next_runnable(current);
    if next == current {
        // Nothing else runnable - stay put rather than pointlessly saving
        // and reloading the same context.
        return;
    }
    unsafe { *TASKS[current].0.get() = *frame };
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
}

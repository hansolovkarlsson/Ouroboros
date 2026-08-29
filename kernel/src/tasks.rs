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
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use crate::exceptions::Context;
use crate::loader::LoadedProgram;

/// Slots 0/1 are always the loaded program and the idle task (`init`);
/// slots 2/3/4 are reserved for the boot-loaded servers - the filesystem
/// server (`syscall_abi::FSD_TASK`, `loader::load_fsd`), the console server
/// (`syscall_abi::CON_TASK`, `loader::load_cond`), and the network server
/// (`syscall_abi::NET_TASK`). Each server stays `Unused` if its `*.BIN`
/// doesn't exist, and `spawn` never fills a reserved slot: a spawned program
/// landing in one would inherit its role. Slots [`FIRST_SPAWNABLE`]..NUM_TASKS
/// (6..11) start `Unused` and are only ever filled by `spawn` (dynamic task
/// creation) - the pool a foreground command, a background task, and a
/// pipeline draw from. A small, fixed bound rather than anything growable -
/// "generous but bounded," the same philosophy `mmu.rs`'s
/// `MAX_EXTRA_L1_TABLES` already states for a similar "how many of these might
/// we need" question. Raising it is a one-constant change now (`spawn` just
/// fails with `SpawnError::NoFreeSlot` past this): the per-task arrays and the
/// `mmu.rs` table pool (`MAX_EL0_REGIONS`, which must stay equal) auto-scale.
pub const NUM_TASKS: usize = 11;

/// The first slot `spawn` may use - everything below it is fixed
/// infrastructure (boot shell, idle, filesystem server, console server,
/// network server, account server).
///
/// Adding the account server raised this from 5 to 6, and `NUM_TASKS` from 10
/// to 11 with it: a fifth server would otherwise have eaten one of the five
/// spawnable slots, which is the pool a pipeline's stages come from. Keeping
/// five spawnable was worth one more slot's worth of fixed arrays.
///
/// **`pub` on purpose, and load-bearing.** Every "is this slot protected?" guard
/// in `syscall.rs` (`EXIT`/`KILL`/`WAIT`/`FG`) must derive from this constant
/// rather than spell out a literal. They used to say `<= 4`, and when the
/// account server took slot 5 those literals silently stopped covering it - a
/// code review found `fg 5` could hand the keyboard to a task that never reads
/// it (unrecoverable: the Ctrl+C detector only polls for the owner, and the
/// tick's kill is gated to spawnable slots) and `kill 5` could take the server
/// down four times over, past the supervisor's restart cap. That is the same
/// "a new task-number lever inherits every old guard" failure the
/// interactive-shell arc already wrote a postmortem about; the fix is that the
/// bound has exactly one definition.
pub const FIRST_SPAWNABLE: usize = 6;

// Per-slot capabilities (the capability model for who-may-do-what).
// Because task-slot roles are static (see `NUM_TASKS`'s doc comment - 0
// shell, 1 idle, 2 fsd, 3 cond, 4 netd, 5..10 spawnable), a task's
// capabilities are a pure function of its slot: no stored table, no mutable
// state, and a restarted server or a spawned child automatically gets the
// right caps. (0 shell, 1 idle, 2 fsd, 3 cond, 4 netd, 5 accountd,
// 6..11 spawnable.) The whole policy lives in `caps_for_slot`. Packed in one `u32`:
// the low `NUM_TASKS` bits are the IPC send-mask (added in the who-may-call-
// whom stage), and the resource caps live at bit 16 and up - clear of the
// send-mask for any `NUM_TASKS` up to 16, so raising the slot count can't
// collide with them (it nearly did at NUM_TASKS=10, when the send-mask first
// reached bit 9 - CAP_CON's old home).
//
/// `CAP_BLOCK`: may use the `BLOCK_*` syscalls (raw disk access). Only the
/// filesystem server holds it - the "supervised" in "supervised EL0
/// process".
pub(crate) const CAP_BLOCK: u32 = 1 << 16;
/// `CAP_CON`: may use `CON_WRITE`/`CON_INFO`/`FB_*` (the console device).
/// Only the console server holds it - ordinary tasks reach the console
/// only through it (a `DSPOP_WRITE` message).
pub(crate) const CAP_CON: u32 = 1 << 17;
/// `CAP_NET`: may use `NET_SEND`/`NET_RECV` (the virtio-net device). Only
/// the network server holds it - ordinary tasks reach the network only
/// through it (a `NETOP_*` message).
pub(crate) const CAP_NET: u32 = 1 << 18;

// Send-mask bits (low `NUM_TASKS` bits of a `Caps` word): bit `t` set means
// "may initiate a send/call to slot `t`". Named per target for readability.
const TO_SHELL: u32 = 1 << 0; // slot 0
const TO_FSD: u32 = 1 << (syscall_abi::FSD_TASK as u32); // slot 2
const TO_CON: u32 = 1 << (syscall_abi::CON_TASK as u32); // slot 3
const TO_NET: u32 = 1 << (syscall_abi::NET_TASK as u32); // slot 4
const TO_ACCT: u32 = 1 << (syscall_abi::ACCT_TASK as u32); // slot 5
/// Every spawnable slot's bit ([`FIRST_SPAWNABLE`]..NUM_TASKS) - the shell's
/// send-mask so it can relay pipe input to any child it spawns. Computed from
/// the two constants, so raising `NUM_TASKS` widens it automatically.
const TO_SPAWNABLE: u32 = ((1u32 << NUM_TASKS) - 1) & !((1u32 << FIRST_SPAWNABLE) - 1);

/// The capability set for `slot`, a pure function of the slot's static
/// role. This is the single source of truth for the capability policy -
/// the send-mask (who this slot may initiate IPC to) plus resource caps.
/// Validated against every real IPC flow (see the plan / CLAUDE.md's
/// capability-model section): servers reply freely via the reply exemption
/// in [`may_send`], so only *unsolicited* sends need a mask bit.
fn caps_for_slot(slot: usize) -> u32 {
    match slot as u64 {
        // Shell: calls the filesystem, console, and network servers, and
        // sends pipe input to its spawned children.
        0 => TO_FSD | TO_CON | TO_NET | TO_SPAWNABLE,
        // Idle: never sends.
        1 => 0,
        // Filesystem server: logs to the console server; owns the disk.
        // (Replies to its clients ride the reply exemption.)
        syscall_abi::FSD_TASK => TO_CON | CAP_BLOCK,
        // Console server: only ever replies (reply-exempt), so no send-mask
        // bits; owns the console device.
        syscall_abi::CON_TASK => CAP_CON,
        // Network server: logs to the console server, and calls the
        // filesystem server (its HTTP server reads files from fsd to serve
        // them - netd is fsd's first non-shell client); owns the NIC.
        // (Replies to its own clients ride the reply exemption.) Holds TO_NET
        // (a self-send bit) *only* so it may DELEGATE the reply capability to a
        // child it spawns - the remote-exec (Plan 9 `cpu`, cluster Phase 4a)
        // child whose stdout it captures, which pipes its output back to netd.
        syscall_abi::NET_TASK => TO_FSD | TO_CON | TO_NET | CAP_NET,
        // Account server: reads and writes /etc/passwd + /etc/shadow through
        // the filesystem server, and logs to the console. It holds no device
        // capability at all - its privilege is not a resource but a *policy*
        // one (it will write a password for a caller who cannot write
        // /etc/shadow), and that lives in its own code, gated on the caller
        // identity the kernel reports. (Replies ride the reply exemption.)
        syscall_abi::ACCT_TASK => TO_FSD | TO_CON,
        // Spawnable slots: may reach the servers a program legitimately needs
        // (fsd for a nested shell, cond for output, accountd to change one's
        // own password) and message the shell (e.g. pong's unsolicited echo).
        // Not the NIC, not each other, not idle, no devices.
        //
        // TO_ACCT is deliberately given to *every* spawnable slot rather than
        // delegated per-command: `passwd` is a program any user may run, and
        // holding the capability to ASK is not permission to succeed - the
        // server checks the caller's identity itself. Same reasoning that lets
        // every slot reach fsd, which then enforces file permissions.
        _ => TO_SHELL | TO_FSD | TO_CON | TO_ACCT,
    }
}

/// Whether `slot` holds capability `cap` (a `CAP_*` bit).
pub(crate) fn cap_has(slot: usize, cap: u32) -> bool {
    caps_for_slot(slot) & cap != 0
}

/// Whether task `src` may initiate an IPC send/call to `dest` (the
/// who-may-call-whom enforcement, checked at the `MSG_SEND`/`MSG_CALL`
/// boundary). Two ways it's allowed:
/// - **A reply to an authorized call.** If `dest` is currently blocked in a
///   `MSG_CALL` to `src` (`Blocked(Message{from: Some(src)})`), this send
///   completes that round trip rather than initiating a new one - always
///   allowed regardless of the send-mask. This is the same "the client is
///   blocked in a call to me" condition `SAFECOPY` keys off, and it's what
///   lets a server (e.g. `cond`, whose send-mask is empty) reply to any
///   caller.
/// - **An unsolicited send permitted by the send-mask.** Otherwise `src`'s
///   `caps_for_slot` send-mask must have `dest`'s bit set.
///
/// The kernel's own supervisor ping bypasses this entirely - it calls
/// [`send_message`] directly (not through the syscall boundary), and its
/// ack is intercepted before validation.
pub(crate) fn may_send(src: usize, dest: usize) -> bool {
    if let TaskState::Blocked(WaitReason::Message { from: Some(f), .. }) = unsafe { *STATES[dest].0.get() } {
        if f == src {
            return true;
        }
    }
    if dest < NUM_TASKS && caps_for_slot(src) & (1 << dest) != 0 {
        return true;
    }
    // A runtime-delegated send capability (the `DELEGATE` syscall) - `src`
    // was handed the right to reach `dest` at runtime by a task that
    // statically held it. See [`DELEGATED_SEND`].
    dest < NUM_TASKS && DELEGATED_SEND[src].load(Ordering::Relaxed) == dest as u64
}

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

// `[const { … }; NUM_TASKS]` (not an explicit literal) so these per-slot
// arrays auto-scale when NUM_TASKS changes (Stage 0) - the initializer is the
// same for every slot, and the wrapper types aren't `Copy`, so the repeat
// needs the inline-const form.
static TASKS: [TaskSlot; NUM_TASKS] =
    [const { TaskSlot(UnsafeCell::new(Context::zeroed())) }; NUM_TASKS];

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

static REGIONS: [RegionSlot; NUM_TASKS] =
    [const { RegionSlot(UnsafeCell::new((0, 0))) }; NUM_TASKS];

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
        TaskState::Zombie(_) => syscall_abi::TASK_STATE_ZOMBIE,
    }
}

/// The `WAIT` syscall's non-blocking half: `Some(status)` if `target`
/// is already a zombie (reaped here - see `WaitReason::TaskExit`),
/// `None` if it's still alive and the caller should block on
/// `WaitReason::TaskExit(target)`. Validation (range, protected,
/// self-wait) is the syscall layer's job.
pub(crate) fn try_reap(target: usize) -> Option<u64> {
    match unsafe { *STATES[target].0.get() } {
        TaskState::Zombie(status) => {
            unsafe { *STATES[target].0.get() = TaskState::Unused };
            Some(status)
        }
        _ => None,
    }
}

/// A zombie's exit status **without reaping it** (`Some(status)` only when
/// `target` is a `Zombie`; `None` for any live/unused slot) - the read-only
/// peek behind the `TASK_EXIT_CODE` syscall, so `ps` can show the code while
/// the slot is still held. Unlike [`try_reap`], leaves the state untouched.
pub(crate) fn zombie_status(target: usize) -> Option<u64> {
    match unsafe { *STATES[target].0.get() } {
        TaskState::Zombie(status) => Some(status),
        _ => None,
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

/// A foreground task the Ctrl+C escape hatch (`interrupt_key_check`) has marked
/// for death, killed by [`on_tick`] at a safe point (the only place this kernel
/// tears a task down and switches away). `usize::MAX` = nothing pending. The
/// kill is deferred to `on_tick` rather than done inline in the keyboard poll
/// because the poll runs in contexts (a wake-check pass, a syscall) where
/// switching away from a task isn't safe - `on_tick` already owns that.
static PENDING_KILL: AtomicUsize = AtomicUsize::new(usize::MAX);

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

/// The Ctrl+C escape hatch. When `byte` is Ctrl+C (`0x03`, ETX) and a task
/// other than the boot shell owns the keyboard (a foreground `/bin` program is
/// running, having been handed the keyboard by `run_found_command`), that
/// program is marked for death ([`PENDING_KILL`], killed by [`on_tick`]) and the
/// byte is swallowed (returns `true`). When it dies the keyboard reverts to the
/// shell, whose `WAIT` on it then wakes with `TASK_KILLED_STATUS`. When task 0
/// (the boot shell) already owns the keyboard, Ctrl+C is not special: it passes
/// through as an ordinary byte the line editor ignores (returns `false`), so the
/// normal single-shell case is unchanged.
///
/// This is the interrupt that makes interactive `/bin` programs viable: run an
/// editor or a REPL, and Ctrl+C kills it and drops back to the shell. It is
/// still not a *signal*; nothing is delivered to the program for it to catch (an
/// editor can't offer "save first"). That's a later refinement (signals, or a
/// raw-input mode). Ctrl+C means terminate.
pub(crate) fn interrupt_key_check(byte: u8) -> bool {
    const ETX: u8 = 0x03; // Ctrl+C
    let owner = INPUT_OWNER.load(Ordering::Relaxed);
    if byte != ETX || owner == 0 {
        return false;
    }
    PENDING_KILL.store(owner, Ordering::Relaxed);
    true
}

/// One queued IPC message: who sent it, how long it is, and the bytes
/// (copied in at send time, copied out at receive time - no shared
/// memory anywhere in this design, deliberately: copying is the
/// isolation-friendly semantics, and at 64 bytes the cost is nothing).
#[derive(Clone, Copy)]
struct Message {
    sender: u8,
    /// u16, not u8: messages grew past 255 bytes when the filesystem
    /// protocol's payloads moved inline (see syscall-abi's
    /// MSG_MAX_LEN doc).
    len: u16,
    data: [u8; MSG_MAX],
}

const MSG_MAX: usize = syscall_abi::MSG_MAX_LEN as usize;
const MSG_QUEUE_DEPTH: usize = 4;

/// One task's bounded mailbox - a tiny ring of pending messages.
struct Mailbox {
    msgs: [Message; MSG_QUEUE_DEPTH],
    head: usize,
    count: usize,
}

impl Mailbox {
    const fn new() -> Self {
        Mailbox {
            msgs: [Message { sender: 0, len: 0, data: [0; MSG_MAX] }; MSG_QUEUE_DEPTH],
            head: 0,
            count: 0,
        }
    }
}

struct MailboxSlot(UnsafeCell<Mailbox>);
// SAFETY: single-core, only touched from SVC dispatch and the tick
// handler's wake-check - the same never-reentrant reasoning as every
// other per-task cell in this module.
unsafe impl Sync for MailboxSlot {}
static MAILBOXES: [MailboxSlot; NUM_TASKS] =
    [const { MailboxSlot(UnsafeCell::new(Mailbox::new())) }; NUM_TASKS];

/// `MSG_SEND`'s core: deliver `data` to `dest`. The caller validates
/// `dest` exists; this only enforces the size and depth bounds.
///
/// **Direct delivery first, mailbox second:** if the destination is
/// already blocked waiting for exactly this message (a plain `recv`,
/// or a call-reply wait naming this sender - see
/// [`WaitReason::Message`]'s `from` filter), the mailbox is skipped
/// entirely - the bytes are copied straight into its waiting buffer,
/// the packed result stashed in its saved `x0` (the same slot
/// `on_tick`'s wake-check writes), and the task marked runnable. This
/// is the eager version of the wake-check's own delivery, not a new
/// mechanism - it exists so a send -> recv handoff completes without
/// waiting for the next tick, which is what makes synchronous
/// request/response round trips (the `MSG_CALL` syscall, and the
/// filesystem server built on it) fast enough to put every disk
/// operation through.
pub(crate) fn send_message(sender: usize, dest: usize, data: &[u8]) -> u64 {
    if data.len() > MSG_MAX {
        return syscall_abi::MSG_ERR_TOO_BIG;
    }
    if let TaskState::Blocked(WaitReason::Message { buf, len, from }) =
        unsafe { *STATES[dest].0.get() }
    {
        if from.is_none() || from == Some(sender) {
            let copy_len = data.len().min(len as usize);
            let dst = buf as *mut u8;
            for (i, &b) in data[..copy_len].iter().enumerate() {
                // SAFETY: the receiver validated (buf, len) at its own
                // syscall boundary; single address space, same trust
                // model as the wake-check's copy-out.
                unsafe { core::ptr::write_volatile(dst.add(i), b) };
            }
            unsafe { (*TASKS[dest].0.get()).gpr[0] = ((sender as u64) << 32) | copy_len as u64 };
            unsafe { *STATES[dest].0.get() = TaskState::Runnable };
            return 0;
        }
    }
    let mailbox = unsafe { &mut *MAILBOXES[dest].0.get() };
    if mailbox.count >= MSG_QUEUE_DEPTH {
        return syscall_abi::MSG_ERR_FULL;
    }
    let slot = (mailbox.head + mailbox.count) % MSG_QUEUE_DEPTH;
    let msg = &mut mailbox.msgs[slot];
    msg.sender = sender as u8;
    msg.len = data.len() as u16;
    msg.data[..data.len()].copy_from_slice(data);
    mailbox.count += 1;
    // A destination parked in NET_WAIT isn't in a Message wait, so the
    // direct-delivery path above didn't fire - but it *does* want to wake on
    // a queued message. Mark it runnable now so a client call reaches it this
    // switch (sub-tick), not only on the next tick's wake-check. It drains
    // the mailbox itself on resume (NET_WAIT returns; the value is ignored).
    if let TaskState::Blocked(WaitReason::NetInput { .. }) = unsafe { *STATES[dest].0.get() } {
        unsafe { *STATES[dest].0.get() = TaskState::Runnable };
    }
    0
}

/// Pops `task`'s oldest queued message into `(buf, buf_len)` (raw
/// copy - single address space, the same trust model as every `fs_*`
/// buffer), returning the packed `(sender << 32) | copied_len` the
/// receive syscalls hand to userland - or `None` if the mailbox is
/// empty. The unfiltered form of [`try_recv_message_from`].
pub(crate) fn try_recv_message(task: usize, buf: u64, buf_len: u64) -> Option<u64> {
    try_recv_message_from(task, buf, buf_len, None)
}

/// [`try_recv_message`] with an optional sender filter: pops the oldest
/// queued message *from `from`* (any sender when `None`), leaving
/// non-matching messages queued in their original order - the selective
/// receive a call-reply wait needs, so an unrelated task's message
/// can't be mistaken for the reply. Removal from the middle of the
/// ring shifts the younger entries down one slot - bounded by the
/// 4-deep queue, so the cost is irrelevant.
pub(crate) fn try_recv_message_from(
    task: usize,
    buf: u64,
    buf_len: u64,
    from: Option<usize>,
) -> Option<u64> {
    let mailbox = unsafe { &mut *MAILBOXES[task].0.get() };
    let mut found = None;
    for i in 0..mailbox.count {
        let slot = (mailbox.head + i) % MSG_QUEUE_DEPTH;
        if from.is_none() || from == Some(mailbox.msgs[slot].sender as usize) {
            found = Some(i);
            break;
        }
    }
    let i = found?;
    let msg = mailbox.msgs[(mailbox.head + i) % MSG_QUEUE_DEPTH];
    for j in i..mailbox.count - 1 {
        let dst_slot = (mailbox.head + j) % MSG_QUEUE_DEPTH;
        let src_slot = (mailbox.head + j + 1) % MSG_QUEUE_DEPTH;
        mailbox.msgs[dst_slot] = mailbox.msgs[src_slot];
    }
    mailbox.count -= 1;
    let copy_len = (msg.len as u64).min(buf_len) as usize;
    let dst = buf as *mut u8;
    for (k, &b) in msg.data[..copy_len].iter().enumerate() {
        unsafe { core::ptr::write_volatile(dst.add(k), b) };
    }
    Some(((msg.sender as u64) << 32) | copy_len as u64)
}

/// A dead task's queued mail dies with it - called from both teardown
/// paths (`exit`/`kill`), so a later occupant of the same slot can
/// never receive a predecessor's messages.
pub(crate) fn clear_mailbox(task: usize) {
    let mailbox = unsafe { &mut *MAILBOXES[task].0.get() };
    mailbox.head = 0;
    mailbox.count = 0;
}

/// Per-task argv store: the argument-vector blob a task was spawned with
/// (`ARGS_STAGE` blob format - see syscall-abi), filled at spawn from the
/// kernel's staging buffer and read back by the child via the
/// `GET_ARGC`/`GET_ARG` syscalls. Delivered kernel-side and fetched (not
/// injected into the new task's registers/stack), the same shape as the
/// per-task stdout target - so a spawned program's start-up state is
/// unchanged. Boot-loaded tasks (the shell, the servers) get none (len 0).
const ARGV_CAP: usize = syscall_abi::ARGV_MAX as usize;

struct Argv {
    data: [u8; ARGV_CAP],
    len: usize,
}

impl Argv {
    const fn new() -> Self {
        Argv { data: [0; ARGV_CAP], len: 0 }
    }
}

struct ArgvSlot(UnsafeCell<Argv>);
// SAFETY: single-core, non-reentrant - the same per-task-cell reasoning as
// MAILBOXES; set_argv (from SPAWN) and the getters (from the child's own
// syscalls) never run concurrently.
unsafe impl Sync for ArgvSlot {}
static ARGVS: [ArgvSlot; NUM_TASKS] =
    [const { ArgvSlot(UnsafeCell::new(Argv::new())) }; NUM_TASKS];

/// Store a freshly spawned task's argv blob (from the `SPAWN` handler, which
/// copies it out of the staging buffer). Truncated to `ARGV_CAP`.
pub(crate) fn set_argv(task: usize, blob: &[u8]) {
    let argv = unsafe { &mut *ARGVS[task].0.get() };
    let n = blob.len().min(ARGV_CAP);
    argv.data[..n].copy_from_slice(&blob[..n]);
    argv.len = n;
}

/// The fixed name of a supervised server slot (`fsd`/`cond`/`netd`/`accountd`),
/// or `None`
/// for any other slot. One source of truth for both the boot naming (`init`)
/// and the re-naming a supervised restart needs (`supervisor::restart`, since a
/// crash teardown clears the argv store this reads).
pub(crate) fn server_name(slot: usize) -> Option<&'static [u8]> {
    match slot {
        s if s == syscall_abi::FSD_TASK as usize => Some(b"fsd"),
        s if s == syscall_abi::CON_TASK as usize => Some(b"cond"),
        s if s == syscall_abi::NET_TASK as usize => Some(b"netd"),
        s if s == syscall_abi::ACCT_TASK as usize => Some(b"accountd"),
        _ => None,
    }
}

/// Give a boot-loaded task a name by synthesizing a single-argument argv blob
/// (`[argc=1][len][bytes]`, the `ARGS_STAGE` format) so it shows up in `ps` via
/// the `TASK_NAME` syscall - the loader calls this for the servers and init,
/// which are loaded (not `SPAWN`ed) and would otherwise have an empty argv.
pub(crate) fn set_name(task: usize, name: &[u8]) {
    let argv = unsafe { &mut *ARGVS[task].0.get() };
    let n = name.len().min(ARGV_CAP - 8); // 4-byte argc + 4-byte len header
    argv.data[0..4].copy_from_slice(&1u32.to_le_bytes());
    argv.data[4..8].copy_from_slice(&(n as u32).to_le_bytes());
    argv.data[8..8 + n].copy_from_slice(&name[..n]);
    argv.len = 8 + n;
}

/// The argv blob for `task` (empty if it was spawned with none) - read by
/// the `GET_ARGC`/`GET_ARG` syscall arms for the calling task.
pub(crate) fn argv_blob(task: usize) -> &'static [u8] {
    // SAFETY: single-core, non-reentrant (see ArgvSlot); the data lives in a
    // static for the whole boot, and the caller copies from it immediately.
    let argv = unsafe { &*ARGVS[task].0.get() };
    &argv.data[..argv.len]
}

/// Drop a dead task's argv so a later occupant of the slot can't read it -
/// wired into every teardown path alongside `clear_mailbox`/`clear_grant`.
pub(crate) fn clear_argv(task: usize) {
    let argv = unsafe { &mut *ARGVS[task].0.get() };
    argv.len = 0;
}

/// Per-task environment store: the env blob a task inherited at spawn (the
/// same `[count][len][NAME=VALUE]...` encoding as argv), latched via
/// `ENV_STAGE` and read back by the child via `GET_ENVC`/`GET_ENV`. Same
/// deliver-kernel-side, fetch-by-child shape as argv; boot-loaded tasks get
/// none (len 0). Cleared on death so a slot's next occupant can't read it.
const ENV_CAP: usize = syscall_abi::ENV_MAX as usize;

struct EnvBlob {
    data: [u8; ENV_CAP],
    len: usize,
}

impl EnvBlob {
    const fn new() -> Self {
        EnvBlob { data: [0; ENV_CAP], len: 0 }
    }
}

struct EnvSlot(UnsafeCell<EnvBlob>);
// SAFETY: single-core, non-reentrant - identical reasoning to ArgvSlot.
unsafe impl Sync for EnvSlot {}
static ENVS: [EnvSlot; NUM_TASKS] =
    [const { EnvSlot(UnsafeCell::new(EnvBlob::new())) }; NUM_TASKS];

/// Store a freshly spawned task's environment blob (from the `SPAWN` handler,
/// which copies it out of the env staging buffer). Truncated to `ENV_CAP`.
pub(crate) fn set_env(task: usize, blob: &[u8]) {
    let env = unsafe { &mut *ENVS[task].0.get() };
    let n = blob.len().min(ENV_CAP);
    env.data[..n].copy_from_slice(&blob[..n]);
    env.len = n;
}

/// The env blob for `task` (empty if it inherited none) - read by the
/// `GET_ENVC`/`GET_ENV` syscall arms (which reuse the argv blob decoders).
pub(crate) fn env_blob(task: usize) -> &'static [u8] {
    // SAFETY: single-core, non-reentrant (see EnvSlot); static for the whole
    // boot, and the caller copies from it immediately.
    let env = unsafe { &*ENVS[task].0.get() };
    &env.data[..env.len]
}

/// Drop a dead task's env, alongside `clear_argv`/`clear_cwd`.
pub(crate) fn clear_env(task: usize) {
    let env = unsafe { &mut *ENVS[task].0.get() };
    env.len = 0;
}

/// Per-task working directory: the cwd a task was spawned with (the shell's
/// cwd at spawn time), so a spawned command can resolve relative paths and
/// default to the current directory. Delivered kernel-side and fetched via
/// `GET_CWD`, the same shape as argv. Boot-loaded tasks get none (len 0).
const CWD_CAP: usize = syscall_abi::CWD_MAX as usize;

struct Cwd {
    data: [u8; CWD_CAP],
    len: usize,
}

impl Cwd {
    const fn new() -> Self {
        Cwd { data: [0; CWD_CAP], len: 0 }
    }
}

struct CwdSlot(UnsafeCell<Cwd>);
// SAFETY: same single-core, non-reentrant per-task-cell reasoning as ARGVS.
unsafe impl Sync for CwdSlot {}
static CWDS: [CwdSlot; NUM_TASKS] =
    [const { CwdSlot(UnsafeCell::new(Cwd::new())) }; NUM_TASKS];

/// Store a freshly spawned task's working directory (from the `SPAWN`
/// handler, out of the staging buffer). Truncated to `CWD_CAP`.
pub(crate) fn set_cwd(task: usize, path: &[u8]) {
    let cwd = unsafe { &mut *CWDS[task].0.get() };
    let n = path.len().min(CWD_CAP);
    cwd.data[..n].copy_from_slice(&path[..n]);
    cwd.len = n;
}

/// The working directory for `task` (empty if none) - read by the `GET_CWD`
/// syscall arm for the calling task.
pub(crate) fn cwd_path(task: usize) -> &'static [u8] {
    // SAFETY: single-core, non-reentrant (see CwdSlot); static-lifetime data,
    // copied from immediately by the caller.
    let cwd = unsafe { &*CWDS[task].0.get() };
    &cwd.data[..cwd.len]
}

/// Drop a dead task's cwd - wired into every teardown path alongside
/// `clear_argv`.
pub(crate) fn clear_cwd(task: usize) {
    let cwd = unsafe { &mut *CWDS[task].0.get() };
    cwd.len = 0;
}

/// Per-task namespace: the `bind` table a task was spawned with (its parent's
/// namespace at spawn time), mapping a path prefix to a mount subtree - the
/// Plan 9 per-task namespace (cluster Phase 0). Opaque bytes here, exactly like
/// the cwd: the kernel stores and delivers them, userland (`ulib`/shell)
/// interprets them. Delivered kernel-side and fetched via `GET_NS`. Boot-loaded
/// tasks get none (len 0 = the identity-to-tree-0 default).
const NS_CAP: usize = syscall_abi::NS_MAX as usize;

struct Namespace {
    data: [u8; NS_CAP],
    len: usize,
}

impl Namespace {
    const fn new() -> Self {
        Namespace { data: [0; NS_CAP], len: 0 }
    }
}

struct NsSlot(UnsafeCell<Namespace>);
// SAFETY: same single-core, non-reentrant per-task-cell reasoning as CWDS.
unsafe impl Sync for NsSlot {}
static NAMESPACES: [NsSlot; NUM_TASKS] =
    [const { NsSlot(UnsafeCell::new(Namespace::new())) }; NUM_TASKS];

/// Store a freshly spawned task's namespace (from the `SPAWN` handler, out of
/// the staging buffer). Truncated to `NS_CAP`.
pub(crate) fn set_namespace(task: usize, blob: &[u8]) {
    let ns = unsafe { &mut *NAMESPACES[task].0.get() };
    let n = blob.len().min(NS_CAP);
    ns.data[..n].copy_from_slice(&blob[..n]);
    ns.len = n;
}

/// The namespace blob for `task` (empty if none) - read by the `GET_NS` syscall
/// arm for the calling task.
pub(crate) fn namespace(task: usize) -> &'static [u8] {
    // SAFETY: single-core, non-reentrant (see NsSlot); static-lifetime data,
    // copied from immediately by the caller.
    let ns = unsafe { &*NAMESPACES[task].0.get() };
    &ns.data[..ns.len]
}

/// Drop a dead task's namespace - wired into every teardown path alongside
/// `clear_cwd`.
pub(crate) fn clear_namespace(task: usize) {
    let ns = unsafe { &mut *NAMESPACES[task].0.get() };
    ns.len = 0;
}

/// Per-task stdout target: the task index a program's output should go to,
/// set by whoever `spawn`ed it (see the `SPAWN`/`STDOUT_TARGET` syscalls).
/// `CON_TASK` (the console server) by default, and for every boot-loaded
/// task; a shell orchestrating a program-to-program pipe or an
/// `exec … > file` redirect sets a spawned program's target to itself, so
/// it can relay or capture that program's output. Reset to the console
/// default on task death so a reused slot never inherits a stale target.
static STDOUT_TARGET: [AtomicU64; NUM_TASKS] = [const { AtomicU64::new(syscall_abi::CON_TASK) }; NUM_TASKS];

/// Record `task`'s stdout target - called by `spawn` once a slot is chosen.
pub(crate) fn set_stdout_target(task: usize, target: u64) {
    STDOUT_TARGET[task].store(target, Ordering::Relaxed);
}

/// The stdout target of `task`, for the `STDOUT_TARGET` syscall.
pub(crate) fn stdout_target_of(task: usize) -> u64 {
    STDOUT_TARGET[task].load(Ordering::Relaxed)
}

/// Reset `task`'s stdout target to the console default - called on task
/// death alongside `clear_mailbox`/`clear_grant`.
fn reset_stdout_target(task: usize) {
    STDOUT_TARGET[task].store(syscall_abi::CON_TASK, Ordering::Relaxed);
}

/// Per-task **user identity**: each slot's owning `(gid << 32) | uid`, packed in
/// one atomic. Default `0` (root:root) - every boot task starts privileged; the
/// shell's `login` drops itself to a real user via `SET_ID`, and children
/// inherit it at `SPAWN` ([`inherit_id`]). The kernel is the unforgeable root of
/// trust for the task->identity binding (only it knows an IPC message's real
/// sender); names/passwords/policy are entirely userland. Mirrors
/// [`STDOUT_TARGET`]'s atomic-per-slot shape.
static IDS: [AtomicU64; NUM_TASKS] = [const { AtomicU64::new(0) }; NUM_TASKS];

/// Per-task **saved identity** (POSIX saved-set-uid): the identity a task may
/// *restore* to even once it is no longer root. When a task drops identity
/// ([`apply_id`] saves its old current here), this remembers where it came
/// from, so the shell can drop to a user for a login session and restore to
/// root on logout to re-prompt - without the shell ever leaving slot 0. The
/// escalation guard is [`inherit_id`]: a child's saved is set to its *own*
/// (inherited) current, **not** the parent's saved, so a user's spawned
/// programs can never restore to root. Default `0`.
static SAVED_IDS: [AtomicU64; NUM_TASKS] = [const { AtomicU64::new(0) }; NUM_TASKS];

/// Per-task **supplementary groups**: the gids a task belongs to *in addition*
/// to the primary gid packed into [`IDS`]. Written only by the root-gated
/// `SET_GROUPS` syscall, inherited at spawn ([`inherit_id`]), cleared on death.
///
/// A flat `[[AtomicU32; MAX]; NUM_TASKS]` plus a count, rather than anything
/// cleverer, for the same reason as the rest of the per-task state: fixed size,
/// no allocation, and a slot's array is only ever touched by that slot's own
/// syscalls plus the fsd read path.
static GROUPS: [[AtomicU32; syscall_abi::MAX_SUPP_GROUPS]; NUM_TASKS] =
    [const { [const { AtomicU32::new(0) }; syscall_abi::MAX_SUPP_GROUPS] }; NUM_TASKS];

/// How many entries of each task's [`GROUPS`] row are live.
static GROUP_COUNTS: [AtomicU32; NUM_TASKS] = [const { AtomicU32::new(0) }; NUM_TASKS];

/// Replace `task`'s supplementary group list (the `SET_GROUPS` syscall, whose
/// root-only permission decision is made by the caller).
pub(crate) fn set_groups(task: usize, gids: &[u32]) {
    let n = gids.len().min(syscall_abi::MAX_SUPP_GROUPS);
    for (i, g) in gids[..n].iter().enumerate() {
        GROUPS[task][i].store(*g, Ordering::Relaxed);
    }
    GROUP_COUNTS[task].store(n as u32, Ordering::Relaxed);
}

/// Copy `task`'s supplementary gids into `out`, returning the task's true count
/// (which may exceed what fitted).
pub(crate) fn groups_of(task: usize, out: &mut [u32]) -> usize {
    let n = GROUP_COUNTS[task].load(Ordering::Relaxed) as usize;
    let copy = n.min(out.len());
    for (i, slot) in out.iter_mut().enumerate().take(copy) {
        *slot = GROUPS[task][i].load(Ordering::Relaxed);
    }
    n
}

/// Whether `task` is a live task - one that could actually have sent a message
/// that is still worth authorizing. A `Zombie` has run and died; an `Unused`
/// slot has had its identity reset to root by [`reset_id`], which is precisely
/// why `GET_ID` must not answer for either (see the `GET_ID` syscall arm).
pub(crate) fn is_live(task: usize) -> bool {
    !matches!(
        unsafe { *STATES[task].0.get() },
        TaskState::Unused | TaskState::Zombie(_)
    )
}

/// `task`'s owning uid (the low half of its packed identity).
pub(crate) fn uid_of(task: usize) -> u32 {
    IDS[task].load(Ordering::Relaxed) as u32
}

/// The packed `(gid << 32) | uid` of `task` - the `GET_ID` syscall's value.
pub(crate) fn id_of(task: usize) -> u64 {
    IDS[task].load(Ordering::Relaxed)
}

/// `task`'s packed *saved* identity - the value a non-root task is allowed to
/// `SET_ID` back to (see [`apply_id`] and the `SET_ID` syscall arm).
pub(crate) fn saved_id_of(task: usize) -> u64 {
    SAVED_IDS[task].load(Ordering::Relaxed)
}

/// Change `task`'s identity to `new` (packed `(gid << 32) | uid`), stashing the
/// *old* current as the new saved identity. The permission decision (root may
/// drop to anyone; a non-root task may only restore to its saved identity)
/// lives in the `SET_ID` syscall arm; this performs the transition it approved.
pub(crate) fn apply_id(task: usize, new: u64) {
    SAVED_IDS[task].store(IDS[task].load(Ordering::Relaxed), Ordering::Relaxed);
    IDS[task].store(new, Ordering::Relaxed);
}

/// A child inherits its parent's *current* identity at `SPAWN` - a task always
/// runs as the user that started it. Its **saved** identity is set to that same
/// inherited value (NOT the parent's saved), so a child can never restore to a
/// privilege the parent held but had dropped - the escalation guard that makes
/// the shell's drop-to-user safe. Unlike argv/cwd/env, identity isn't *staged*
/// per-spawn; it's carried, so this copies rather than reads a staging buffer.
pub(crate) fn inherit_id(child: usize, parent: usize) {
    let cur = IDS[parent].load(Ordering::Relaxed);
    IDS[child].store(cur, Ordering::Relaxed);
    SAVED_IDS[child].store(cur, Ordering::Relaxed);
    // Supplementary groups travel with the identity: a command run by a user in
    // `staff` must be able to touch the group's files, exactly as the user can.
    let n = GROUP_COUNTS[parent].load(Ordering::Relaxed);
    let live = (n as usize).min(syscall_abi::MAX_SUPP_GROUPS);
    for (dst, src) in GROUPS[child].iter().zip(GROUPS[parent].iter()).take(live) {
        dst.store(src.load(Ordering::Relaxed), Ordering::Relaxed);
    }
    GROUP_COUNTS[child].store(n, Ordering::Relaxed);
}

/// Reset `task`'s identity (current *and* saved) to root on death, alongside the
/// other per-slot clears. A fresh spawn overwrites it via [`inherit_id`], but an
/// `Unused` slot must not read back as some dead task's non-root user.
fn reset_id(task: usize) {
    IDS[task].store(0, Ordering::Relaxed);
    SAVED_IDS[task].store(0, Ordering::Relaxed);
    // An Unused slot must not read back as some dead task's group memberships
    // any more than as its uid.
    GROUP_COUNTS[task].store(0, Ordering::Relaxed);
}

/// Runtime capability delegation (the `DELEGATE` syscall). The static
/// send-mask in [`caps_for_slot`] is fixed per slot; delegation lets a task
/// that *statically holds* a send-capability hand it to another task at
/// runtime - the mechanism a shell uses to authorize a pipeline's producer
/// to stream directly to its consumer (relay-free `programA | programB`),
/// taking the shell out of the byte hot path.
///
/// One delegated send-target per task (a slot index, or `NO_DELEGATION`),
/// matching the single-slot `GRANTS`/`STDOUT_TARGET` precedent: a task
/// orchestrates one delegated stream at a time, so one slot suffices
/// (`a | b | c`, whose middle stage would both send and receive, stays out
/// of scope). No transitive re-delegation - [`may_delegate`] consults only
/// the *static* mask, so a delegated capability can never be laundered
/// onward, which is also why only the shell (the one slot holding
/// `TO_SPAWN_*`) can ever authorize producer->consumer streaming.
const NO_DELEGATION: u64 = NUM_TASKS as u64;
static DELEGATED_SEND: [AtomicU64; NUM_TASKS] =
    [const { AtomicU64::new(NO_DELEGATION) }; NUM_TASKS];

/// Whether `delegator` may delegate the right to send to `target` - it must
/// *statically* hold that send-capability itself (the delegated mask is
/// deliberately not consulted, so nothing can be re-delegated onward). This
/// is what confines inter-child streaming to the shell: only slot 0 holds
/// `TO_SPAWN_*`, so only it can authorize a spawnable task to reach another.
pub(crate) fn may_delegate(delegator: usize, target: usize) -> bool {
    target < NUM_TASKS && caps_for_slot(delegator) & (1 << target) != 0
}

/// `DELEGATE`'s core: grant `grantee` the runtime capability to send to
/// `target`. The syscall arm has already checked [`may_delegate`] and that
/// `grantee`/`target` are valid live slots - this only records it.
pub(crate) fn set_delegate(grantee: usize, target: usize) {
    DELEGATED_SEND[grantee].store(target as u64, Ordering::Relaxed);
}

/// Clear every delegation involving `task` - both the one `task` was granted
/// (delegated-out) and any pointing *at* `task`, so a reused slot can never
/// inherit a delegation aimed at a now-dead target. Called from every
/// teardown path alongside [`clear_grant`]/[`reset_stdout_target`].
fn clear_delegate(task: usize) {
    DELEGATED_SEND[task].store(NO_DELEGATION, Ordering::Relaxed);
    for slot in DELEGATED_SEND.iter() {
        if slot.load(Ordering::Relaxed) == task as u64 {
            slot.store(NO_DELEGATION, Ordering::Relaxed);
        }
    }
}

/// One task's single outstanding grant - the enforced bulk-transfer
/// capability behind the `GRANT`/`SAFECOPY` syscalls. `grantee` (a slot
/// index, the same identity IPC uses everywhere) may bulk-copy the
/// `len`-byte buffer at `ptr` - which lives in *this granter's* own EL0
/// region - in the directions `dir` permits (a mask of
/// `GRANT_READ`/`GRANT_WRITE`). `active` distinguishes "no grant" from a
/// real one. Deliberately a single slot per task, not a table: a task
/// makes one blocking call at a time, so it can have at most one grant
/// in flight; a new grant overwrites the old.
#[derive(Clone, Copy)]
struct Grant {
    grantee: usize,
    ptr: u64,
    len: u64,
    dir: u64,
    active: bool,
}

struct GrantSlot(UnsafeCell<Grant>);
// SAFETY: same single-core, SVC/tick-only, never-reentrant reasoning as
// MailboxSlot and every other per-task cell in this module.
unsafe impl Sync for GrantSlot {}
static GRANTS: [GrantSlot; NUM_TASKS] = [const {
    GrantSlot(UnsafeCell::new(Grant { grantee: 0, ptr: 0, len: 0, dir: 0, active: false }))
}; NUM_TASKS];

/// `GRANT`'s core: record `granter`'s outstanding grant. The caller
/// (the syscall arm) has already validated that `grantee` exists, `dir`
/// is a valid non-zero direction mask, `len` is within `SAFECOPY_MAX`,
/// and `[ptr, ptr+len)` lies inside `granter`'s own region - this only
/// stores it.
pub(crate) fn set_grant(granter: usize, grantee: usize, ptr: u64, len: u64, dir: u64) {
    let g = unsafe { &mut *GRANTS[granter].0.get() };
    *g = Grant { grantee, ptr, len, dir, active: true };
}

/// A dead task's grant dies with it - called from every teardown path
/// alongside [`clear_mailbox`], so a later occupant of the same slot
/// can't inherit a predecessor's grant.
pub(crate) fn clear_grant(task: usize) {
    let g = unsafe { &mut *GRANTS[task].0.get() };
    g.active = false;
}

/// `SAFECOPY`'s core: `server` copies `len` bytes between `client`'s
/// granted buffer (at `client_off` within it) and the already-validated
/// server-local address `local` (the syscall arm checked `local`/`len`
/// against `server`'s own region before calling). `dir` is a *single*
/// direction bit (`GRANT_READ` = read client -> local, `GRANT_WRITE` =
/// write local -> client). Returns `Some(len)` on success, `None` if
/// the copy is unauthorized - the enforcement is here:
///
/// 1. `client`'s grant is active, names `server` as grantee, and its
///    `dir` mask permits this direction;
/// 2. `client` is *currently* blocked in a `MSG_CALL` to `server` (a
///    stale grant is inert - once the call returns the client is
///    runnable, not blocked-calling-me);
/// 3. `[client_off, client_off+len)` stays within the granted buffer;
/// 4. the resulting client range stays within `client`'s live region
///    (defence in depth - the grant was region-checked when set, and a
///    blocked task's region doesn't move, but the kernel is about to
///    dereference the address, so it re-checks).
pub(crate) fn safecopy(
    server: usize,
    client: usize,
    client_off: u64,
    local: u64,
    len: u64,
    dir: u64,
) -> Option<u64> {
    if client >= NUM_TASKS {
        return None;
    }
    // Exactly one direction bit, and the grant must permit it.
    if dir != syscall_abi::GRANT_READ && dir != syscall_abi::GRANT_WRITE {
        return None;
    }
    let grant = unsafe { *GRANTS[client].0.get() };
    if !grant.active || grant.grantee != server || grant.dir & dir == 0 {
        return None;
    }
    // The client must be actively blocked in a call to this server.
    let blocked_calling_me = matches!(
        unsafe { *STATES[client].0.get() },
        TaskState::Blocked(WaitReason::Message { from: Some(f), .. }) if f == server
    );
    if !blocked_calling_me {
        return None;
    }
    // Bounds within the granted buffer.
    let end = client_off.checked_add(len)?;
    if end > grant.len {
        return None;
    }
    let client_addr = grant.ptr.checked_add(client_off)?;
    // Re-validate against the client's live region.
    let (base, size) = task_region(client);
    if size == 0 || client_addr < base || client_addr.checked_add(len)? > base + size {
        return None;
    }
    // Both addresses are in identity-mapped, EL1-RW RAM in every view
    // (only the EL0-access overlay is per-task - see mmu.rs), so this
    // copy works regardless of which task's TTBR0 is currently active.
    unsafe {
        if dir == syscall_abi::GRANT_READ {
            core::ptr::copy_nonoverlapping(client_addr as *const u8, local as *mut u8, len as usize);
        } else {
            core::ptr::copy_nonoverlapping(local as *const u8, client_addr as *mut u8, len as usize);
        }
    }
    Some(len)
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
    /// Waiting for the task at this slot index to die (`WAIT` syscall).
    /// Satisfied by the target reaching `Zombie` (the poll *reaps* it -
    /// collecting the status is what frees the slot) or `Unused` (it
    /// was killed out from under the waiter - `TASK_KILLED_STATUS`).
    TaskExit(usize),
    /// Waiting for a message (`MSG_RECV`, and `MSG_CALL`'s reply half):
    /// the receiver's buffer, into which delivery copies the oldest
    /// matching queued message. `from` is the sender filter - `None`
    /// for a plain receive-from-any (`MSG_RECV`), `Some(task)` for a
    /// call waiting on that specific task's reply (`MSG_CALL`), so an
    /// unrelated task's message can never be mistaken for the reply.
    Message { buf: u64, len: u64, from: Option<usize> },
    /// Waiting for network input *or* a message (`NET_WAIT`): satisfied
    /// when the NIC has a frame waiting or the task's mailbox is non-empty.
    /// Unlike `Message` it consumes neither - the waiter drains both
    /// sources itself after waking (`NET_RECV` / `MSG_TRY_RECV`). This is
    /// the async-receive primitive the network server uses to multiplex
    /// incoming frames and client IPC without busy-polling either.
    /// `deadline` (a tick count from `exceptions::ticks()`; 0 = none) also
    /// wakes it once that tick is reached even with no input - the timer the
    /// network server's TCP retransmit timeout (RTO) needs to fire when a
    /// peer goes silent and no frames arrive.
    NetInput { deadline: u64 },
}

impl WaitReason {
    /// `Some(value)` if this reason is satisfied right now, consuming
    /// whatever made it so (e.g. the keyboard byte itself) - `None` means
    /// keep waiting. Keyboard delegates to `syscall.rs` since that's
    /// already where the console/xhci polling logic lives; `TaskExit`'s
    /// logic is native to this module. `waiter` is which task this poll
    /// is being evaluated for - `TaskExit` needs it for the Ctrl+C
    /// interrupt check (see below).
    fn poll(self, waiter: usize) -> Option<u64> {
        match self {
            WaitReason::Keyboard => crate::syscall::poll_keyboard_byte().map(u64::from),
            WaitReason::TaskExit(target) => {
                // A wait must stay interruptible or one `wait` on a
                // never-exiting task bricks the whole session (the
                // Ctrl+C hatch lives in the keyboard poll, which
                // nothing would otherwise be running while the sole
                // typist is blocked here). Scoped to exactly the
                // dangerous case - the waiter that owns the keyboard:
                // drain one byte if available; Ctrl+C interrupts the
                // wait (the target keeps running); any other typed
                // byte is deliberately discarded, same spirit as
                // typing at a busy foreground job in `sh`.
                if waiter == INPUT_OWNER.load(Ordering::Relaxed) {
                    if let Some(byte) = crate::syscall::poll_keyboard_byte() {
                        if byte == 0x03 {
                            return Some(syscall_abi::WAIT_INTERRUPTED);
                        }
                    }
                }
                match unsafe { *STATES[target].0.get() } {
                    TaskState::Zombie(status) => {
                        // Collecting the status is the reap - the slot
                        // becomes spawnable again here and only here.
                        unsafe { *STATES[target].0.get() = TaskState::Unused };
                        Some(status)
                    }
                    TaskState::Unused => Some(syscall_abi::TASK_KILLED_STATUS),
                    _ => None,
                }
            }
            WaitReason::Message { buf, len, from } => {
                // Same Ctrl+C escape hatch as TaskExit's - a `recv`
                // (or a call to a wedged server) with no reply coming
                // must not brick the session.
                if waiter == INPUT_OWNER.load(Ordering::Relaxed) {
                    if let Some(byte) = crate::syscall::poll_keyboard_byte() {
                        if byte == 0x03 {
                            return Some(syscall_abi::RECV_INTERRUPTED);
                        }
                    }
                }
                try_recv_message_from(waiter, buf, len, from)
            }
            WaitReason::NetInput { deadline } => {
                // Woken by a frame, a message, or the deadline (a bare timer
                // wake with no input - what the RTO needs); consume nothing
                // (the waiter drains sources itself). The value isn't
                // meaningful to NET_WAIT's caller.
                let timed_out = deadline != 0 && crate::exceptions::ticks() >= deadline;
                if crate::syscall::net_has_frame() || has_queued_message(waiter) || timed_out {
                    Some(0)
                } else {
                    None
                }
            }
        }
    }
}

/// Whether `task`'s mailbox holds at least one queued message - the
/// message half of `WaitReason::NetInput`'s wake condition.
fn has_queued_message(task: usize) -> bool {
    unsafe { (*MAILBOXES[task].0.get()).count > 0 }
}

#[derive(Clone, Copy, PartialEq)]
enum TaskState {
    /// No real task in this slot - `spawn`'s only candidate for a new
    /// one. Slots 0/1 (the loaded program, the idle task) never reach
    /// this state; they're always Runnable or Blocked once `init` runs.
    Unused,
    Runnable,
    Blocked(WaitReason),
    /// Exited, status not yet collected - the slot is held (not
    /// spawnable) until a `WAIT` reaps it. Holds *only* the status:
    /// the task's memory was already freed and unmapped at death
    /// (see `exit_current_and_switch`'s caller), so an un-waited
    /// zombie costs a slot, not RAM.
    Zombie(u64),
}

struct StateSlot(UnsafeCell<TaskState>);

// SAFETY: same argument as `TaskSlot` above - single-core, only touched
// from EL1 with IRQs masked.
unsafe impl Sync for StateSlot {}

// Not uniform (slots 0/1 boot Runnable - the loaded program and idle - the
// rest Unused until a server is installed or `spawn` fills them), so this one
// stays an explicit literal rather than a `[const { … }; NUM_TASKS]` repeat:
// two Runnable, then NUM_TASKS-2 Unused.
static STATES: [StateSlot; NUM_TASKS] = [
    StateSlot(UnsafeCell::new(TaskState::Runnable)),
    StateSlot(UnsafeCell::new(TaskState::Runnable)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
    StateSlot(UnsafeCell::new(TaskState::Unused)),
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

/// The idle task's slot (task 1 - see this module's doc comment). Named here so
/// [`next_runnable_skip_idle`] doesn't hardcode a bare `1`.
const IDLE_TASK: usize = 1;

/// Like [`next_runnable`], but skips the idle task unless it's the only thing
/// runnable. A *voluntary* yield ([`yield_current_and_switch`]) wants to hand
/// the CPU to real work; parking on idle would just wait for the next tick,
/// which is exactly the stall the yield exists to avoid.
fn next_runnable_skip_idle(from: usize) -> usize {
    for offset in 1..=NUM_TASKS {
        let candidate = (from + offset) % NUM_TASKS;
        if candidate != IDLE_TASK
            && unsafe { *STATES[candidate].0.get() } == TaskState::Runnable
        {
            return candidate;
        }
    }
    // Nothing else runnable - fall back (idle, or stay put).
    next_runnable(from)
}

/// Voluntarily give up the CPU (the `YIELD` syscall): save the current task -
/// which stays `Runnable`, this is a yield, not a block - and switch to another
/// runnable task, preferring real work over idle ([`next_runnable_skip_idle`]).
/// Returns `0` to the yielding task when it resumes.
///
/// The reason this exists: a pipe producer whose consumer's mailbox is full
/// (`MSG_ERR_FULL`) otherwise busy-spins re-sending until the next tick lets
/// the consumer run - at a 1-second tick, a real stall on hardware, and a waste
/// of a CPU that could be running the consumer. Yielding hands the CPU straight
/// to the consumer, which drains (or exits early, like `head`, at which point
/// the producer's next send fails fast and it stops). Same trap-frame /
/// return-value contract as [`block_current_and_switch`] (see `next_runnable`'s
/// doc comment for why the return is `frame.gpr[0]`).
///
/// # Safety
/// `frame` must be the live trap frame of the syscall currently being
/// dispatched (true when called from `syscall::dispatch`, its only caller).
pub(crate) unsafe fn yield_current_and_switch(frame: *mut Context) -> u64 {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    let next = next_runnable_skip_idle(current);
    if next == current {
        return frame.gpr[0]; // nothing else to run - just carry on
    }
    // YIELD's own return value, seen by `current` when it is resumed later.
    frame.gpr[0] = 0;
    unsafe { *TASKS[current].0.get() = *frame };
    // STATES[current] stays Runnable - a yield doesn't block.
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
    crate::mmu::activate_task(next);
    frame.gpr[0]
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
    unsafe { block_current_and_switch_to(frame, reason, None) }
}

/// Like [`block_current_and_switch`], but switches straight to `prefer`
/// when it's given and currently runnable, instead of round-robining to
/// the next runnable task (which, with an always-runnable idle task, is
/// usually idle, so a plain `MSG_CALL` reply would otherwise wait a full
/// tick). `MSG_CALL` uses this to switch directly to the server it just
/// delivered a request to: that server runs, replies (direct-delivered
/// back, waking this caller), and blocks again, all before the next tick,
/// the sub-tick synchronous round trip the ABI's `MSG_CALL` doc
/// describes. Safe regardless of what `prefer` actually does: if it never
/// replies, this caller stays blocked exactly as it would have, woken by
/// the eventual reply or Ctrl+C.
///
/// # Safety
/// Same contract as [`block_current_and_switch`].
pub(crate) unsafe fn block_current_and_switch_to(
    frame: *mut Context,
    reason: WaitReason,
    prefer: Option<usize>,
) -> u64 {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    unsafe { *TASKS[current].0.get() = *frame };
    unsafe { *STATES[current].0.get() = TaskState::Blocked(reason) };
    let next = match prefer {
        Some(p)
            if p != current && unsafe { *STATES[p].0.get() } == TaskState::Runnable =>
        {
            p
        }
        _ => next_runnable(current),
    };
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
    crate::mmu::activate_task(next);
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
pub(crate) unsafe fn exit_current_and_switch(frame: *mut Context, status: u64) -> u64 {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    // Zombie, not Unused: the status (masked to a byte, POSIX-style, so
    // it can never collide with the ABI's error band) is kept until a
    // WAIT collects it - which is also what makes the slot spawnable
    // again. The memory is still freed at death (the caller's job),
    // not at reap.
    unsafe { *STATES[current].0.get() = TaskState::Zombie(status & 0xff) };
    unsafe { *REGIONS[current].0.get() = (0, 0) };
    clear_mailbox(current);
    clear_grant(current);
    clear_delegate(current);
    clear_argv(current);
    clear_cwd(current);
    clear_env(current);
    clear_namespace(current);
    reset_stdout_target(current);
    reset_id(current);
    let next = next_runnable(current);
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
    crate::mmu::activate_task(next);
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
    clear_mailbox(i);
    clear_grant(i);
    clear_delegate(i);
    clear_argv(i);
    clear_cwd(i);
    clear_env(i);
    clear_namespace(i);
    reset_stdout_target(i);
    reset_id(i);
}

/// The EL0-fault teardown's context switch: destroys the *currently
/// running* task (whichever one is live in `frame` - it just faulted)
/// and switches to the next runnable one. [`exit_current_and_switch`]'s
/// exact shape with kill semantics instead of exit's: the slot goes
/// straight to `Unused` (reap-immediately, like [`kill_task`] - a
/// `WAIT`er on this task wakes with `TASK_KILLED_STATUS` via the
/// existing `Unused` poll arm; there's no meaningful exit status to
/// keep from a crash). No return value: the fault trampoline ("4:" in
/// `exceptions.rs`) restores the frame blindly with no post-call `x0`
/// write, unlike the SVC trampoline, so there's nothing to pass
/// through.
///
/// # Safety
/// Same contract as [`block_current_and_switch`]: `frame` must be the
/// live trap frame of the exception currently being handled.
pub(crate) unsafe fn kill_current_and_switch(frame: *mut Context) {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    unsafe { *STATES[current].0.get() = TaskState::Unused };
    unsafe { *REGIONS[current].0.get() = (0, 0) };
    clear_mailbox(current);
    clear_grant(current);
    clear_delegate(current);
    clear_argv(current);
    clear_cwd(current);
    clear_env(current);
    clear_namespace(current);
    reset_stdout_target(current);
    reset_id(current);
    let next = next_runnable(current);
    *frame = unsafe { *TASKS[next].0.get() };
    CURRENT.store(next, Ordering::Relaxed);
    crate::mmu::activate_task(next);
}

/// Fails every task blocked in a call to `dead` (a
/// `Blocked(Message { from: Some(dead) })` reply wait): stashes
/// `TASK_ERR_NO_SUCH_TASK` in the waiter's saved `x0` and wakes it -
/// the same stash-and-mark delivery the wake-check and direct delivery
/// already use. Without this, a client mid-`MSG_CALL` to a task that
/// dies waits forever (Ctrl+C being the only rescue, and only for the
/// keyboard owner); with it, the caller's call simply fails - which
/// the shell's `fs_call` maps to `NO_FS`, exactly right for "the
/// filesystem server died under your request". Wired into all three
/// death paths (`EXIT`, `KILL`, the EL0-fault teardown). Plain
/// `MSG_RECV` waits (`from: None`) are deliberately untouched -
/// they're not waiting on any particular task.
pub(crate) fn fail_calls_to(dead: usize) {
    for i in 0..NUM_TASKS {
        if let TaskState::Blocked(WaitReason::Message { from: Some(target), .. }) =
            unsafe { *STATES[i].0.get() }
        {
            if target == dead {
                unsafe { (*TASKS[i].0.get()).gpr[0] = syscall_abi::TASK_ERR_NO_SUCH_TASK };
                unsafe { *STATES[i].0.get() = TaskState::Runnable };
            }
        }
    }
}

/// Installs a task directly into a specific slot - the filesystem
/// server's *restart* path (`syscall.rs::restart_fsd`), which must
/// land in slot 2 exactly ([`spawn`] deliberately scans from
/// [`FIRST_SPAWNABLE`] and can never fill the reserved slot). The
/// caller guarantees the slot is `Unused` (the fault teardown just
/// made it so) and handles the mmu rebuild that actually makes the
/// region EL0-accessible. Does the same freshly-written-code cache
/// maintenance as [`init`]/[`spawn`].
pub(crate) fn install_task(slot: usize, context: Context, region: (u64, u64)) {
    unsafe { *TASKS[slot].0.get() = context };
    unsafe { *REGIONS[slot].0.get() = region };
    unsafe { *STATES[slot].0.get() = TaskState::Runnable };
    flush_new_code(region.0, region.1);
}

/// Whether slot `i` currently holds a *live* task (`Runnable` or
/// `Blocked`) - the `KILL`/`FG`/`WAIT` syscalls' existence check.
/// Zombies are deliberately not "live": `fg`/`kill` on one would
/// target a task that no longer runs (`wait` is how a zombie is
/// dealt with - `ps` says so).
pub(crate) fn task_exists(i: usize) -> bool {
    i < NUM_TASKS
        && matches!(
            unsafe { *STATES[i].0.get() },
            TaskState::Runnable | TaskState::Blocked(_)
        )
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
/// running unchanged, a new one joins it. Never touches slots 0-2 (the
/// loaded program, the idle task, and the filesystem server's reserved
/// slot - see `NUM_TASKS`'s doc comment) - only scans from
/// [`FIRST_SPAWNABLE`] onward.
pub(crate) fn spawn(context: Context, region: (u64, u64)) -> Result<usize, SpawnError> {
    for i in FIRST_SPAWNABLE..NUM_TASKS {
        if unsafe { *STATES[i].0.get() } == TaskState::Unused {
            unsafe { *TASKS[i].0.get() = context };
            unsafe { *REGIONS[i].0.get() = region };
            unsafe { *STATES[i].0.get() = TaskState::Runnable };
            // Freshly-written code needs the same clean/invalidate
            // sequence `init` gives the boot-loaded programs - a
            // latent gap until the fault-isolation milestone added
            // flush_new_code: never visible on QEMU (TCG doesn't model
            // cache incoherency) and never *observed* on real
            // Parallels hardware, but never correct to omit either.
            flush_new_code(region.0, region.1);
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
/// 1's idle loop into its own small region, sets up the filesystem
/// server as task 2 the same way as task 0 when one was loaded (`fsd`,
/// `loader::load_fsd` - `None` leaves the slot `Unused`, and the boot
/// proceeds without a filesystem), and disables the EL0 `wfe`/`wfi`
/// trap both tasks' idle loops rely on. Does not itself start
/// anything — see [`start`].
///
/// # Safety
/// Must run after `mmu.rs` has mapped `program`'s region, `fsd`'s
/// region (when present), and [`idle_region`] EL0-accessible, and
/// before any task could possibly run.
pub unsafe fn init(
    program: &LoadedProgram,
    fsd: Option<&LoadedProgram>,
    cond: Option<&LoadedProgram>,
    netd: Option<&LoadedProgram>,
    accountd: Option<&LoadedProgram>,
) {
    // Task 0: entry is the program's real ELF entry point, not just its
    // load base - loader.rs computes `entry = base + e_entry` (they
    // happen to be equal today, since programs/linker.ld keeps `_start` at
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
    // Name the boot task so `ps` shows it (spawned tasks carry their own
    // argv[0]; these loaded ones would otherwise be nameless). Task 0 is the
    // init program named in INIT.CFG - the shell, in every configuration so
    // far, so name it that.
    set_name(0, b"shell");
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
    set_name(1, b"idle");
    clean_dcache_range(idle_addr, IDLE_REGION_SIZE as u64);

    // Task 2: the filesystem server, exactly task 0's setup shape -
    // entry point, stack at the top of its region, everything unmasked.
    if let Some(fsd) = fsd {
        *unsafe { &mut *TASKS[2].0.get() } = Context {
            gpr: [0; 31],
            sp_el0: fsd.base + fsd.size,
            elr_el1: fsd.entry,
            spsr_el1: 0,
        };
        unsafe { *REGIONS[2].0.get() = (fsd.base, fsd.size) };
        unsafe { *STATES[2].0.get() = TaskState::Runnable };
        if let Some(n) = server_name(2) {
            set_name(2, n);
        }
        clean_dcache_range(fsd.base, fsd.size);
    }

    // Task 3: the console server, same setup shape as the filesystem
    // server - entry point, stack at the top of its region, everything
    // unmasked. Absent (no COND.BIN) leaves the slot `Unused`, and the
    // boot proceeds with the kernel's own console handling all output.
    if let Some(cond) = cond {
        *unsafe { &mut *TASKS[3].0.get() } = Context {
            gpr: [0; 31],
            sp_el0: cond.base + cond.size,
            elr_el1: cond.entry,
            spsr_el1: 0,
        };
        unsafe { *REGIONS[3].0.get() = (cond.base, cond.size) };
        unsafe { *STATES[3].0.get() = TaskState::Runnable };
        if let Some(n) = server_name(3) {
            set_name(3, n);
        }
        clean_dcache_range(cond.base, cond.size);
    }

    // Task 4: the network server, same setup shape as the filesystem and
    // console servers. Absent (no NETD.BIN) leaves the slot `Unused`, and
    // the boot proceeds with no network - `ping` reports no server.
    if let Some(netd) = netd {
        *unsafe { &mut *TASKS[4].0.get() } = Context {
            gpr: [0; 31],
            sp_el0: netd.base + netd.size,
            elr_el1: netd.entry,
            spsr_el1: 0,
        };
        unsafe { *REGIONS[4].0.get() = (netd.base, netd.size) };
        unsafe { *STATES[4].0.get() = TaskState::Runnable };
        if let Some(n) = server_name(4) {
            set_name(4, n);
        }
        clean_dcache_range(netd.base, netd.size);
    }

    // Task 5: the account server, same shape again. Absent (no ACCOUNTD.BIN)
    // leaves the slot `Unused` and the system boots without self-service
    // password changes - exactly as it did before there was one.
    if let Some(accountd) = accountd {
        *unsafe { &mut *TASKS[5].0.get() } = Context {
            gpr: [0; 31],
            sp_el0: accountd.base + accountd.size,
            elr_el1: accountd.entry,
            spsr_el1: 0,
        };
        unsafe { *REGIONS[5].0.get() = (accountd.base, accountd.size) };
        unsafe { *STATES[5].0.get() = TaskState::Runnable };
        if let Some(n) = server_name(5) {
            set_name(5, n);
        }
        clean_dcache_range(accountd.base, accountd.size);
    }

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
/// The full freshly-written-code sequence for one region: clean the
/// D-cache over the range, then invalidate the I-cache, barriered -
/// what `init` does inline for the boot-loaded programs, packaged for
/// the runtime paths (`spawn`, `install_task`) that write code after
/// boot.
pub(crate) fn flush_new_code(base: u64, size: u64) {
    if size == 0 {
        return;
    }
    clean_dcache_range(base, size);
    unsafe {
        asm!("dsb ish", "ic ialluis", "dsb ish", "isb", options(nostack));
    }
}

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
    // The boot-time build already left task 0's view active
    // (build_tables switches to the current task's view, and CURRENT
    // starts at 0) - activated again here for explicitness, so this
    // function's contract doesn't silently depend on that ordering.
    crate::mmu::activate_task(0);
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
        if let Some(value) = reason.poll(i) {
            unsafe { (*TASKS[i].0.get()).gpr[0] = value };
            unsafe { *STATES[i].0.get() = TaskState::Runnable };
        }
    }

    // Ctrl+C from a *running* foreground child (a compute loop that isn't
    // reading input - the wake-check loop above only polls the keyboard for a
    // child blocked *on* it). Poll once when the foreground owner is the
    // interrupted (running) task; `interrupt_key_check` inside the poll marks a
    // Ctrl+C for the kill below. A non-Ctrl+C byte here is type-ahead the busy
    // child wasn't reading and is dropped - rare (a program that reads input is
    // Blocked, not running, at the tick), and the price of catching Ctrl+C in a
    // runaway loop with no read to piggyback on.
    let owner = INPUT_OWNER.load(Ordering::Relaxed);
    if owner != 0 && owner == current {
        let _ = crate::syscall::poll_keyboard_byte();
    }

    // Honor a Ctrl+C kill (marked by `interrupt_key_check` from either poll
    // site): tear the foreground child down - the `KILL` syscall's teardown,
    // split on whether the victim is the interrupted (current) task, exactly
    // like the supervised-server restart below but without a restart. Its death
    // reverts the keyboard to the shell, whose `WAIT` wakes with
    // `TASK_KILLED_STATUS`.
    let victim = PENDING_KILL.swap(usize::MAX, Ordering::Relaxed);
    // Only a spawnable slot (>= FIRST_SPAWNABLE) may be terminated - the same
    // protected set KILL/EXIT/FG enforce (never the shell, idle, or a
    // supervised server). FG refuses to foreground 0-4, so a real keyboard
    // owner is always in range; this guard is the belt-and-braces backstop.
    if (FIRST_SPAWNABLE..NUM_TASKS).contains(&victim) {
        crate::console::println!("Ouroboros kernel: Ctrl+C - foreground task {victim} terminated");
        let (base, size) = task_region(victim);
        free_runtime_region(base, size);
        revert_input_owner_if(victim);
        fail_calls_to(victim);
        if victim == current {
            unsafe { kill_current_and_switch(frame) };
            unsafe { crate::mmu::rebuild_with_el0_regions(el0_regions()) };
            return;
        }
        kill_task(victim);
        unsafe { crate::mmu::rebuild_with_el0_regions(el0_regions()) };
    }

    // Heartbeat: catch a supervised server (fsd/cond) wedged in a loop -
    // it never returns to a Blocked state and never faults, so the crash
    // path can't see it. A healthy server (idle in recv, or briefly busy)
    // is observed Blocked; a wedged one stays Runnable. On a wedge,
    // restart it on the exact teardown path the fault handler uses.
    // `slot` is a real index passed to is_supervised/heartbeat/restart/
    // task_region, not just an array subscript.
    #[allow(clippy::needless_range_loop)]
    for slot in 0..NUM_TASKS {
        if !crate::supervisor::is_supervised(slot) {
            continue;
        }
        let blocked = matches!(unsafe { *STATES[slot].0.get() }, TaskState::Blocked(_));

        // Passive heartbeat: a server observed continuously `Runnable` is a
        // non-faulting loop wedge.
        let runnable_wedge = crate::supervisor::heartbeat(slot, blocked);

        // Active ping: catches the failure the passive heartbeat can't - a
        // server stuck `Blocked` forever (deadlocked mid-request), which
        // looks identical to a healthy idle server from the outside. Poke a
        // blocked server, and declare it wedged if a prior poke went
        // unacked past the timeout. The ping rides the ordinary message
        // machinery (sender KERNEL_SENDER); the server's reply, addressed
        // back to that sentinel, is the ack (see supervisor.rs / the
        // MSG_SEND syscall arm). Injecting into a server idle in its main
        // recv wakes it (direct delivery); into one stuck mid-sub-call it
        // just queues, unseen - which is the deadlock we want to catch.
        let blocked_wedge = match crate::supervisor::poll_ping(slot, blocked) {
            crate::supervisor::PingAction::Inject => {
                let mut ping = [0u8; syscall_abi::FS_REQ_PAYLOAD as usize];
                ping[..8].copy_from_slice(&syscall_abi::SYSOP_PING.to_le_bytes());
                let _ = send_message(syscall_abi::KERNEL_SENDER as usize, slot, &ping);
                false
            }
            crate::supervisor::PingAction::Wedged => true,
            crate::supervisor::PingAction::None => false,
        };

        if runnable_wedge || blocked_wedge {
            let why = if blocked_wedge { "unresponsive (ping timeout)" } else { "no progress (runnable)" };
            crate::console::println!("Ouroboros kernel: server slot {slot} wedged - {why} - restarting");
            let (base, size) = task_region(slot);
            free_runtime_region(base, size);
            revert_input_owner_if(slot);
            fail_calls_to(slot);
            if slot == current {
                // The wedged server is the interrupted task: discard its
                // frame and switch away, exactly like the fault handler.
                unsafe { kill_current_and_switch(frame) };
                crate::supervisor::restart(slot);
                // SAFETY: IRQs masked for the whole tick, single core -
                // same contract the fault handler's rebuild relies on.
                unsafe { crate::mmu::rebuild_with_el0_regions(el0_regions()) };
                return;
            }
            // Not the current task: tear it down in place, leave `current`
            // running, and fall through to the ordinary round-robin.
            kill_task(slot);
            crate::supervisor::restart(slot);
            unsafe { crate::mmu::rebuild_with_el0_regions(el0_regions()) };
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
    // The eret this returns into lands in `next`'s EL0 code - it must
    // run under `next`'s own table view (per-task page tables).
    crate::mmu::activate_task(next);
}

/// Called from `rust_irq_handler` on a NIC receive interrupt: wakes the
/// task blocked in [`WaitReason::NetInput`] (the network server) and
/// switches to it immediately, so a delivered frame is handled without
/// waiting for the next tick. This is the latency win of IRQ-driven receive
/// over the tick-poll - which stays in place as a fallback, so a missed IRQ
/// degrades to at worst one tick of delay, never a lost frame (`on_tick`'s
/// wake-check still evaluates `NetInput`'s `net_has_frame()` condition).
///
/// A NIC interrupt means a frame is in the receive ring - transmit
/// completions are suppressed at the device (`virtio_net::init` sets the TX
/// ring's NO_INTERRUPT flag), so the shared per-device line only ever fires
/// for receive - so this wakes the `NetInput` waiter unconditionally rather
/// than re-polling. If no task is blocked on `NetInput` (the server is
/// already running, or none exists), it does nothing and resumes the
/// interrupted task; the frame stays queued for the server's own next poll.
///
/// The frame-overwrite contract is identical to [`on_tick`]'s: `frame` is
/// the interrupted task's live context, saved here and replaced with the
/// woken task's, so the trampoline's blind restore resumes the server.
///
/// # Safety
/// `frame` must be the live IRQ trap frame (the slots-5/9 trampoline's
/// contract), same as [`on_tick`].
pub unsafe fn on_net_irq(frame: *mut Context) {
    let frame = unsafe { &mut *frame };
    let current = CURRENT.load(Ordering::Relaxed);
    for i in 0..NUM_TASKS {
        if let TaskState::Blocked(WaitReason::NetInput { .. }) = unsafe { *STATES[i].0.get() } {
            // NET_WAIT's return value is ignored (the server drains its
            // sources itself), matching `NetInput`'s `poll` Some(0).
            unsafe { (*TASKS[i].0.get()).gpr[0] = 0 };
            unsafe { *STATES[i].0.get() = TaskState::Runnable };
            if i != current {
                unsafe { *TASKS[current].0.get() = *frame };
                *frame = unsafe { *TASKS[i].0.get() };
                CURRENT.store(i, Ordering::Relaxed);
                crate::mmu::activate_task(i);
            }
            // Only one task ever blocks on NetInput (the network server).
            return;
        }
    }
}

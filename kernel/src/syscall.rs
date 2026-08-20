//! The `svc` trap path from EL0 back to EL1 — the dispatch table itself.
//! Dropping to EL0 and the tasks that run there now live in `tasks.rs`;
//! this module is just what they call into.
//!
//! Calling convention (chosen to match Linux's, a reasonable default for a
//! "POSIX-ish" OS per this project's stated goals, not because anything
//! here is Linux-ABI-compatible): syscall number in x8, up to 4 arguments
//! in x0-x3, return value in x0. `exceptions.rs`'s slot-8 trampoline is
//! what actually marshals registers into and out of [`dispatch`]'s call
//! signature — see its module doc comment for why that trampoline exists,
//! how it differs from every other vector, and why it grew from 1 argument
//! to 4 for phase 3c's file-I/O syscalls.
//!
//! Syscall numbers and sentinel return values live in the `syscall-abi`
//! crate now, not as local consts here - shared with every userland
//! program (`shell/`) that calls `svc`, so the two sides can't drift
//! silently apart the way hand-duplicated numbers did before. See
//! `syscall-abi/src/lib.rs` for the full list and `docs/processes.md`'s
//! "known rough edges" for why this crate exists.
//!
//! ## Pointer arguments are validated against the caller's own region
//!
//! Several syscalls (`MSG_SEND`/`MSG_RECV`/`MSG_CALL`, `BLOCK_READ`/
//! `BLOCK_WRITE`, `SPAWN_STAGE`) take raw `(pointer, length)` pairs -
//! valid to dereference directly from EL1 because everything is
//! identity-mapped and the kernel's own mappings are identical in
//! every per-task table view. Since the per-task page-tables
//! milestone, every such pair is also checked to fall inside the
//! *calling task's own* region (`in_caller_region` below) - the
//! long-documented "trusted, not validated" gap, closed the moment it
//! stopped being merely theoretical: with the MMU enforcing isolation
//! at EL0, an unvalidated syscall pointer would have been the one
//! remaining way for a task to reach another task's memory (by having
//! the kernel do the touching for it).

use core::sync::atomic::{AtomicU64, Ordering};

use syscall_abi::{FS_ERROR, NO_CHAR, SPAWN_ERROR};

use crate::console;
use crate::exceptions;
use crate::exceptions::Context;
use crate::loader;
use crate::mmu;
use crate::tasks;

/// One independent counter per `tasks.rs` slot, proof — alongside
/// `double`/`print` below — that syscalls arriving from *different*,
/// actually-switching contexts still land in the right dispatch arm with
/// the right per-caller state, not just that the dispatch table has more
/// than one entry. Sized by `NUM_TASKS` like everything else in
/// `tasks.rs` keyed per-slot - grew from 2 to 4 alongside it for dynamic
/// task creation, even though only the original two tasks have ever
/// actually called `report`.
static TASK_REPORTS: [AtomicU64; crate::tasks::NUM_TASKS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// The raw block device the `BLOCK_*` syscalls operate on - the
/// kernel's entire remaining role in storage since the filesystem
/// moved to userland (the fsd server): hold the device, do raw
/// sectors on request, for exactly one authorized task. `None` is the
/// expected state whenever no disk was discovered this boot.
struct BlockCell(core::cell::UnsafeCell<Option<crate::block::BlockDevice>>);
// SAFETY: same single-core, non-reentrant-dispatch reasoning as FsCell.
unsafe impl Sync for BlockCell {}
static BLOCK: BlockCell = BlockCell(core::cell::UnsafeCell::new(None));

/// Installs the block device the `BLOCK_*` syscalls serve. First
/// installed wins, same as the filesystem itself.
pub fn install_block_device(device: crate::block::BlockDevice) {
    unsafe { *BLOCK.0.get() = Some(device) };
}

/// Whether a block device is installed - `MOUNT`'s "already" answer on
/// the device level.
pub fn block_device_installed() -> bool {
    unsafe { (*BLOCK.0.get()).is_some() }
}

/// Activates the USB mass-storage device (if any) and installs it as
/// the block device the filesystem server operates on - shared by the
/// boot-time attempt (`main.rs`, covers QEMU where the device is
/// present at scan time) and the `MOUNT` syscall (covers real
/// Parallels, where a passed-through stick attaches a few seconds
/// after boot - see the enumeration diagnostics; the syscall arm has
/// its own replace-capable variant inline). First-installed wins: a
/// no-op if a device (virtio or a previous USB one) is already in the
/// cell. Returns whether a device is installed after the attempt. The
/// *filesystem* half of mounting lives in the server now
/// (`FSOP_MOUNT`), not here - the kernel only handles the device.
pub fn try_install_usb_block_device() -> bool {
    if block_device_installed() {
        return true;
    }
    if !crate::xhci::storage_present() {
        return false;
    }
    match crate::usb_msd::Device::init() {
        Ok(device) => {
            install_block_device(crate::block::BlockDevice::UsbMsd(device));
            console::println!("Ouroboros kernel: usb-msd block device installed");
            true
        }
        Err(e) => {
            console::println!("Ouroboros kernel: usb-msd init failed ({e})");
            false
        }
    }
}

/// Whether the calling task may use the `BLOCK_*` syscalls: whoever holds
/// `CAP_BLOCK`, which by the capability policy (`tasks::caps_for_slot`) is
/// the filesystem server alone. This gate is the "supervised" in
/// "supervised EL0 process" - the kernel holds the device, and exactly one
/// task is allowed to ask it to touch the disk. (Was a hardcoded
/// `== FSD_TASK` check; folded into the capability model, equivalent by
/// construction since fsd is the only `CAP_BLOCK` slot.)
fn block_access_allowed() -> bool {
    tasks::cap_has(tasks::current_task(), tasks::CAP_BLOCK)
}

/// Whether the calling task may use [`CON_WRITE`]/`CON_INFO`/`FB_*`:
/// whoever holds `CAP_CON`, which by the capability policy is the console
/// server alone. The console analogue of [`block_access_allowed`] - the
/// kernel owns the console, and exactly one task is allowed to push
/// steady-state output through it (ordinary tasks reach it only via a
/// `DSPOP_WRITE` message to that server; the kernel's own `console::*`
/// stays the emergency/boot path). (Was a hardcoded `== CON_TASK` check.)
fn con_access_allowed() -> bool {
    tasks::cap_has(tasks::current_task(), tasks::CAP_CON)
}

/// Falls back to the USB keyboard (xhci.rs) when the byte-stream console
/// has nothing waiting - no shell/ABI changes needed to wire keyboard
/// input in, since both sources feed the same syscalls (`TRY_READ_CHAR`,
/// `READ_CHAR`). `crate::xhci::poll_key()` is a no-op returning `None`
/// immediately if no keyboard was ever found/installed this boot. Also
/// called from `tasks.rs`'s `on_tick` wake-check, evaluating
/// `WaitReason::Keyboard` for a blocked task - the same check either way,
/// just a different caller deciding what to do with the result.
pub(crate) fn poll_keyboard_byte() -> Option<u8> {
    let byte = console::read_byte().or_else(crate::xhci::poll_key)?;
    // The fg escape hatch - see tasks::interrupt_key_check's doc
    // comment. Intercepted here, the single choke point every keyboard
    // path funnels through (the wake-check, READ_CHAR's fast path, and
    // TRY_READ_CHAR alike), so Ctrl+C reclaims the keyboard no matter
    // which path would have consumed it.
    if tasks::interrupt_key_check(byte) {
        console::println!("Ouroboros kernel: Ctrl+C - keyboard returned to the boot shell");
        return None;
    }
    Some(byte)
}

/// Size cap for a userland `(pointer, length)` argument pair.
const MAX_USER_LEN: u64 = 512;

/// Whether `[ptr, ptr+len)` (or the single byte at `ptr`, for a
/// zero-length pair) falls entirely inside the *calling task's own*
/// mapped region - real validation since the per-task page-tables
/// milestone, not just a sanity bound. Without it, a task could
/// launder access to memory its own tables deny it *through* the
/// kernel's copies (a `msg_send` sourced from another task's region,
/// a `block_write` into one, ...) - the MMU would enforce isolation at
/// EL0 while every syscall quietly bypassed it. Every legitimate
/// caller's buffers (stack, locals, `.rodata` literals) live inside
/// its own loaded region, so nothing real is refused.
fn in_caller_region(ptr: u64, len: u64) -> bool {
    let (base, size) = tasks::task_region(tasks::current_task());
    if size == 0 {
        return false;
    }
    ptr >= base && ptr.saturating_add(len.max(1)) <= base + size
}

fn valid_user_range(ptr: u64, len: u64) -> bool {
    ptr != 0 && len != 0 && len <= MAX_USER_LEN && in_caller_region(ptr, len)
}

/// The message syscalls' own bound - messages grew past
/// [`MAX_USER_LEN`] when the filesystem protocol's payloads moved
/// inline (`syscall_abi::MSG_MAX_LEN`, 768), so they can't share the
/// 512-byte check the sector/staging buffers still use. Same
/// caller-region containment as [`valid_user_range`].
fn valid_msg_range(ptr: u64, len: u64) -> bool {
    ptr != 0 && len != 0 && len <= syscall_abi::MSG_MAX_LEN && in_caller_region(ptr, len)
}

/// Sized generously for a real userland program - the default shell is
/// currently ~45KB. A fixed EL1 static, not a heap allocation: `alloc` is
/// unavailable this deep into boot (see `loader.rs`'s own reasoning for
/// why it's fine on the *boot-services* side but not here), and this
/// matches this project's established "fixed static buffer, no heap"
/// pattern for every other runtime buffer (`fat32.rs`'s callers,
/// `shell/src/main.rs`'s own read buffers).
const SPAWN_STAGING_SIZE: usize = 128 * 1024;

struct SpawnStagingCell(core::cell::UnsafeCell<[u8; SPAWN_STAGING_SIZE]>);
// SAFETY: single-core; only ever touched from the SPAWN_STAGE/SPAWN
// dispatch arms, which by construction can't run re-entrantly (same
// reasoning as `BLOCK`).
unsafe impl Sync for SpawnStagingCell {}
static SPAWN_STAGING: SpawnStagingCell = SpawnStagingCell(core::cell::UnsafeCell::new([0; SPAWN_STAGING_SIZE]));

/// `syscall_abi::SPAWN`'s real work: parse+relocate the program image
/// previously fed into [`SPAWN_STAGING`] chunk by chunk (the
/// `SPAWN_STAGE` syscall - the kernel contains no filesystem to read a
/// path with anymore, so the caller reads the file via the filesystem
/// server and stages it here first), into a freshly allocated region
/// (`tasks::allocate_runtime_region`, `loader::elf_region_size`/
/// `populate_region` - the same ELF-loading core `loader.rs`'s
/// boot-time path uses, just with a different memory source), add a
/// new task for it (`tasks::spawn`), and make its region
/// EL0-accessible (`mmu::rebuild_with_el0_regions`). Failures return
/// `SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_BAD_ELF`/
/// `SPAWN_ERR_NO_FREE_SLOT` - the collapsed [`SPAWN_ERROR`] is only
/// the dispatch arm's argument-validation fallback. Nothing
/// already-running is touched until the very last step, so there's no
/// partial state to unwind - and a failure *after*
/// `allocate_runtime_region` (a bad ELF, no free slot) gives the
/// memory back via `tasks::free_runtime_region`, which always succeeds
/// here since a failed spawn's allocation is by construction the most
/// recent one (the LIFO case).
fn spawn_staged(total_len: u64) -> u64 {
    let staging = unsafe { &mut *SPAWN_STAGING.0.get() };
    let size = total_len as usize;
    // A program bigger than the staging buffer can't have been staged
    // in the first place (SPAWN_STAGE bounds every chunk), and an
    // empty one isn't a program - refused outright, matching the old
    // path-reading implementation's own refusal.
    if size == 0 || size > staging.len() {
        return syscall_abi::SPAWN_ERR_TOO_LARGE;
    }
    let program = &staging[..size];

    let (header, phdrs, region_size) = match loader::elf_region_size(program) {
        Ok(result) => result,
        Err(_) => return syscall_abi::SPAWN_ERR_BAD_ELF,
    };
    let region_base = tasks::allocate_runtime_region(region_size);
    // SAFETY: `region_base` was just handed out by
    // `allocate_runtime_region`, fresh and at least `region_size` bytes -
    // the same size `elf_region_size` computed for this exact program.
    let loaded = match unsafe { loader::populate_region(program, &header, phdrs.as_slice(), region_base, region_size) } {
        Ok(loaded) => loaded,
        Err(_) => {
            tasks::free_runtime_region(region_base, region_size);
            return syscall_abi::SPAWN_ERR_BAD_ELF;
        }
    };

    let context = Context {
        gpr: [0; 31],
        sp_el0: loaded.base + loaded.size,
        elr_el1: loaded.entry,
        spsr_el1: 0,
    };
    match tasks::spawn(context, (loaded.base, loaded.size)) {
        Ok(slot) => {
            // SAFETY: called from an SVC handler with interrupts masked
            // throughout - single-core, so nothing else can observe the
            // table set mid-rebuild.
            unsafe { mmu::rebuild_with_el0_regions(tasks::el0_regions()) };
            // The new task's slot index, not a bare 0 - the caller
            // needs it to wait on, send to, or kill what it just
            // started (the shell's pipeline flow does all three).
            slot as u64
        }
        Err(_) => {
            tasks::free_runtime_region(region_base, region_size);
            syscall_abi::SPAWN_ERR_NO_FREE_SLOT
        }
    }
}

/// Called from the exception vector's SVC trampoline (`exceptions.rs`)
/// with the syscall number (from x8) and up to 4 arguments (from x0-x3),
/// running at EL1 with the kernel's own stack and every privilege EL0
/// lacks - the entire reason this indirection exists. Its return value
/// becomes EL0's new x0 after `eret`.
///
/// Sixteen syscalls now (`shell_input`, a seventeenth by original
/// numbering, was removed - see below).
/// `double`/`print` were deliberately chained by the original single-task
/// demo (double's return value fed straight into print's argument) to
/// prove a return value survives the trampoline intact; `report` is what
/// `tasks.rs`'s original two demo tasks called, each with its own task ID
/// as `arg0` (task 1 has since become a plain idle loop that calls
/// nothing - see `tasks.rs`). `try_read_char`/`putc` are the real
/// input/output primitives userland programs are built on - task 0 is now
/// a real loaded program (`loader.rs`/`shell/`) that calls these directly
/// and does its own line editing in its own code, which is what made
/// `shell_input` (previously: hand these bytes to the kernel's own
/// EL1-resident line editor in a now-deleted `shell.rs`) unnecessary.
/// Deliberately left a gap at number 5 rather than renumbering `putc`'s
/// neighbors - a stable ABI matters more than a dense one, and this is
/// exactly the kind of churn the `syscall-abi` crate (see this module's
/// doc comment) now exists to prevent from drifting silently between the
/// kernel and userland sides. `get_ticks` (6) is the
/// first syscall added for phase 2 (commands) - the shell's `uptime`
/// builtin needs *some* real kernel state to report, now that it can't
/// just read `exceptions.rs`'s statics directly the way kernel-resident
/// code used to. `fs_list_dir`/`fs_read_file` (7/8) are phase 3c's - the
/// first syscalls needing more than one argument, which is why `dispatch`
/// grew from 1 argument to 4 (see this module's doc comment and
/// `exceptions.rs`'s). `fs_mkdir`/`fs_rmdir` (9/10) are the first write
/// support this kernel has ever exposed to userland - see
/// `fat32.rs`/`virtio_blk.rs` for the on-disk write path underneath them.
/// `fs_touch`/`fs_rm` (11/12) round out phase 5's file create/delete,
/// reusing the same on-disk write primitives `fs_mkdir`/`fs_rmdir` do.
/// `fs_write_file` (13) is the first syscall able to give a file more
/// than zero bytes - without it, `fs_touch` was the only way to create
/// one, and it only ever produces empty files. `fs_mv` (14) renames or
/// moves a file or directory, reusing its existing cluster chain rather
/// than reading and rewriting its content. `read_char` (15) is
/// `try_read_char`'s blocking counterpart - the first syscall able to
/// suspend its own caller and switch to a different task mid-dispatch
/// (`tasks::block_current_and_switch`), which is why `dispatch` grew a
/// 6th parameter, the calling task's own trap frame (see
/// `exceptions.rs`'s "3:" SVC path). `spawn` (16) is the first syscall
/// able to add a *new* task rather than act on an existing one - see
/// `spawn_program`'s own doc comment for the real work involved.
/// Match arms below use the `syscall_abi` constants rather than bare
/// numbers, so this table and the userland caller can never silently
/// disagree about what number means what.
///
/// `frame` (`x5`, per AAPCS64 - `number`/`arg0..arg3` already fill
/// `x0..x4`) is the calling task's full trap frame, the same pointer
/// `rust_irq_handler` already gets via `mov x0, sp`; `exceptions.rs`'s
/// "3:" SVC path passes it the identical way (`mov x5, sp` right before
/// this call). Unused by every syscall today - a plain non-blocking
/// dispatch table doesn't need it - but real blocking syscalls do: a
/// syscall that has nothing to return yet can hand `frame` to
/// `tasks::block_current_and_switch` to suspend the caller and switch to
/// another task mid-syscall, instead of spinning inside the handler with
/// interrupts masked (see that function's own doc comment for why this
/// is the only safe way to block on this kernel).
pub extern "C" fn dispatch(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, frame: *mut Context) -> u64 {
    match number {
        syscall_abi::PRINT => {
            console::println!("Ouroboros kernel: syscall from EL0: print(arg0={arg0:#x})");
            0
        }
        syscall_abi::DOUBLE => {
            let result = arg0.wrapping_mul(2);
            console::println!("Ouroboros kernel: syscall from EL0: double(arg0={arg0:#x}) = {result:#x}");
            result
        }
        syscall_abi::REPORT => {
            let task_id = arg0 as usize;
            match TASK_REPORTS.get(task_id) {
                Some(counter) => {
                    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    console::println!("Ouroboros kernel: task {task_id} report #{count}");
                    0
                }
                // Not an `fs_*` syscall - u64::MAX here just means "bad
                // task ID", reusing the same bit pattern as FS_ERROR
                // without borrowing its name for an unrelated failure.
                None => u64::MAX,
            }
        }
        syscall_abi::TRY_READ_CHAR => match poll_keyboard_byte() {
            Some(byte) => byte as u64,
            None => NO_CHAR,
        },
        // Blocks instead of returning NO_CHAR: if nothing's waiting yet,
        // suspends this task and switches to another runnable one - see
        // tasks::block_current_and_switch's own doc comment for why this
        // is safe on real hardware where a task-side `wfe` isn't. `frame`
        // is what makes that possible; every other arm above ignores it.
        syscall_abi::READ_CHAR => match poll_keyboard_byte() {
            Some(byte) => byte as u64,
            None => unsafe { tasks::block_current_and_switch(frame, tasks::WaitReason::Keyboard) },
        },
        syscall_abi::PUTC => {
            console::putc(arg0 as u8);
            0
        }
        syscall_abi::CON_WRITE => {
            // The console server's byte-stream backend: write a batch of
            // bytes to the kernel's console in one syscall (vs one PUTC
            // per byte). Gated to CON_TASK, and the buffer must lie in
            // the server's own region - same trust model as BLOCK_READ's
            // sector buffer.
            if !con_access_allowed() || !valid_user_range(arg0, arg1) {
                return syscall_abi::FS_ERROR;
            }
            // SAFETY: pointer/length sanity-checked in the caller's own
            // region just above.
            let bytes = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            for &b in bytes {
                console::putc(b);
            }
            0
        }
        syscall_abi::CON_INFO => {
            // Lets the console server discover its backend (byte-stream vs
            // framebuffer) and the framebuffer's cell grid at startup.
            if !con_access_allowed() {
                return syscall_abi::FS_ERROR;
            }
            match arg0 {
                syscall_abi::CON_INFO_KIND => {
                    if crate::fbdev::is_present() {
                        syscall_abi::CON_KIND_FRAMEBUFFER
                    } else {
                        syscall_abi::CON_KIND_BYTESTREAM
                    }
                }
                syscall_abi::CON_INFO_COLS => crate::fbdev::cols() as u64,
                syscall_abi::CON_INFO_ROWS => crate::fbdev::rows() as u64,
                _ => 0,
            }
        }
        syscall_abi::FB_BLIT => {
            // Plot arg1 glyph bitmaps (arg0, in the server's region) at
            // cells (arg2.., arg3). Gated + buffer-validated like BLOCK_*.
            let count = arg1 as usize;
            let bytes = count * crate::fbdev::GLYPH_BYTES;
            if !con_access_allowed() || !valid_user_range(arg0, bytes as u64) {
                return syscall_abi::FS_ERROR;
            }
            // SAFETY: pointer/length validated in the caller's own region.
            let glyphs = unsafe { core::slice::from_raw_parts(arg0 as *const u8, bytes) };
            crate::fbdev::blit_glyphs(glyphs, count, arg2 as usize, arg3 as usize);
            0
        }
        syscall_abi::FB_SCROLL => {
            if !con_access_allowed() {
                return syscall_abi::FS_ERROR;
            }
            crate::fbdev::scroll(arg0 as usize);
            0
        }
        syscall_abi::FB_CLEAR => {
            if !con_access_allowed() {
                return syscall_abi::FS_ERROR;
            }
            crate::fbdev::clear();
            0
        }
        syscall_abi::GET_TICKS => exceptions::ticks(),
        syscall_abi::SPAWN => spawn_staged(arg0),
        syscall_abi::SPAWN_STAGE => {
            // arg0 = offset into the staging buffer, arg1 = chunk
            // pointer, arg2 = chunk length. Bounded by both the
            // ordinary per-syscall cap and the staging buffer itself.
            if !valid_user_range(arg1, arg2) {
                return SPAWN_ERROR;
            }
            let staging = unsafe { &mut *SPAWN_STAGING.0.get() };
            let offset = arg0 as usize;
            let len = arg2 as usize;
            let Some(end) = offset.checked_add(len) else { return SPAWN_ERROR };
            if end > staging.len() {
                return SPAWN_ERROR;
            }
            // SAFETY: bounds sanity-checked above, same trust model as
            // every other userland pointer argument.
            let chunk = unsafe { core::slice::from_raw_parts(arg1 as *const u8, len) };
            staging[offset..end].copy_from_slice(chunk);
            0
        }
        syscall_abi::EXIT => {
            let current = tasks::current_task();
            if current <= 3 {
                // Task 0 (the boot shell - nothing would own the
                // keyboard, see tasks::INPUT_OWNER_TASK), task 1
                // (idle - never makes syscalls, refused for
                // completeness), task 2 (the filesystem server -
                // its death would strand the disk for the rest of the
                // boot, and its slot is block-syscall-privileged), and
                // task 3 (the console server - its death would strand
                // steady-state output; the kernel's emergency console
                // still works, but the shell wouldn't render) may not
                // exit. The only case where EXIT returns to its caller.
                return syscall_abi::EXIT_DENIED;
            }
            console::println!("Ouroboros kernel: task {current} exited (code {arg0})");
            // Teardown order: reclaim the RAM (LIFO-or-leak, see
            // free_runtime_region), discard the task and clear its
            // region record, then rebuild the identity map so the
            // cleared record actually drops the EL0 mapping - the same
            // masked-IRQ rebuild spawn_program already proved safe.
            // The return value must be passed through unmodified - see
            // exit_current_and_switch's doc comment.
            let (base, size) = tasks::task_region(current);
            tasks::free_runtime_region(base, size);
            // A foregrounded task's death hands the keyboard back to
            // the boot shell - see tasks::revert_input_owner_if.
            tasks::revert_input_owner_if(current);
            // Anyone blocked mid-MSG_CALL to this task gets a failed
            // call instead of waiting forever - see fail_calls_to.
            tasks::fail_calls_to(current);
            // SAFETY: `frame` is the live trap frame of this very
            // syscall (dispatch's contract with the SVC trampoline).
            let resumed_x0 = unsafe { tasks::exit_current_and_switch(frame, arg0) };
            // SAFETY: same contract as spawn_program's rebuild - IRQs
            // are masked for the whole SVC dispatch, no EL0 code runs
            // mid-rebuild.
            unsafe { mmu::rebuild_with_el0_regions(tasks::el0_regions()) };
            resumed_x0
        }
        syscall_abi::TASK_STATE => {
            let i = arg0 as usize;
            if i >= tasks::NUM_TASKS {
                syscall_abi::TASK_STATE_INVALID
            } else {
                tasks::task_state_code(i)
            }
        }
        syscall_abi::KILL => {
            let i = arg0 as usize;
            if i <= 3 {
                // The boot shell (the permanent keyboard owner), idle,
                // the filesystem server, and the console server are
                // protected - same reasoning as EXIT's own refusal of
                // them.
                return syscall_abi::TASK_ERR_PROTECTED;
            }
            if !tasks::task_exists(i) {
                return syscall_abi::TASK_ERR_NO_SUCH_TASK;
            }
            console::println!("Ouroboros kernel: task {i} killed");
            // Same teardown order as EXIT's arm, minus the context
            // switch (the killed task isn't the one running - see
            // tasks::kill_task's doc comment).
            let (base, size) = tasks::task_region(i);
            tasks::free_runtime_region(base, size);
            tasks::revert_input_owner_if(i);
            tasks::fail_calls_to(i);
            tasks::kill_task(i);
            // SAFETY: same masked-IRQ single-core contract as
            // spawn_program's and EXIT's rebuilds.
            unsafe { mmu::rebuild_with_el0_regions(tasks::el0_regions()) };
            0
        }
        syscall_abi::WAIT => {
            let i = arg0 as usize;
            if i <= 3 || i == tasks::current_task() {
                // Waiting on task 0/1/2/3 (they never die - the boot
                // shell, idle, the filesystem server, and the console
                // server are all exit/kill-protected) or on yourself is
                // a guaranteed deadlock - refused up front.
                return syscall_abi::TASK_ERR_PROTECTED;
            }
            if i >= tasks::NUM_TASKS {
                return syscall_abi::TASK_ERR_NO_SUCH_TASK;
            }
            // Already a zombie: collect-and-reap immediately, no block.
            if let Some(status) = tasks::try_reap(i) {
                return status;
            }
            if !tasks::task_exists(i) {
                return syscall_abi::TASK_ERR_NO_SUCH_TASK;
            }
            // Still alive: block until it dies (or Ctrl+C - see
            // WaitReason::TaskExit's poll). Same pass-through-unmodified
            // return contract as READ_CHAR's blocking path.
            unsafe { tasks::block_current_and_switch(frame, tasks::WaitReason::TaskExit(i)) }
        }
        syscall_abi::MOUNT => {
            // The device half only - the filesystem half is the
            // server's FSOP_MOUNT request (see cmd_mount's server-first
            // flow). arg0 != 0 means "replace": the caller has already
            // confirmed with the server that nothing is mounted, so an
            // installed-but-unmountable device (e.g. `make run`'s
            // FAT16 vvfat virtio disk) may be swapped for a found USB
            // stick - without the flag, an occupied cell answers
            // MOUNT_ALREADY untouched. Swapping under a *mounted*
            // filesystem would hand the server's cached geometry a
            // different disk; the server-first check is what rules
            // that out. Rescan first (devices that attached after the
            // boot scan - the Parallels case), then install. Runs with
            // IRQs masked like every SVC - ticks pause for the
            // sub-second setup, an accepted cost.
            let replace = arg0 != 0;
            if !replace && block_device_installed() {
                return syscall_abi::MOUNT_ALREADY;
            }
            crate::xhci::rescan_ports();
            if !crate::xhci::storage_present() {
                return syscall_abi::MOUNT_NO_DEVICE;
            }
            match crate::usb_msd::Device::init() {
                Ok(device) => {
                    install_block_device(crate::block::BlockDevice::UsbMsd(device));
                    console::println!("Ouroboros kernel: usb-msd block device installed");
                    0
                }
                Err(e) => {
                    console::println!("Ouroboros kernel: usb-msd init failed ({e})");
                    syscall_abi::MOUNT_NO_DEVICE
                }
            }
        }
        syscall_abi::MSG_SEND => {
            let dest = arg0 as usize;
            // A supervised server's reply to a liveness ping is addressed
            // to KERNEL_SENDER (the sender the ping was injected under -
            // see supervisor.rs / syscall-abi's SYSOP_PING). That's not a
            // real task, so intercept it here, before the buffer/dest
            // validation below, as the ack: the reply payload is
            // deliberately ignored (we only care that the server got far
            // enough around its loop to reply at all). A non-server, or a
            // server with no ping outstanding, is a harmless no-op inside
            // note_ack.
            if arg0 == syscall_abi::KERNEL_SENDER {
                crate::supervisor::note_ack(tasks::current_task());
                return 0;
            }
            // A zero-length message is legal - it's the end-of-stream
            // marker in the shell's pipeline convention (see the
            // MSG_SEND doc in syscall-abi). The pointer must still be
            // non-null and inside the caller's own region; only the
            // length may be 0.
            if arg1 == 0 || arg2 > syscall_abi::MSG_MAX_LEN || !in_caller_region(arg1, arg2) {
                return FS_ERROR;
            }
            if dest >= tasks::NUM_TASKS || !tasks::task_exists(dest) {
                return syscall_abi::TASK_ERR_NO_SUCH_TASK;
            }
            // SAFETY: bounds sanity-checked above, same trust model as
            // every fs_* buffer (see the module doc comment).
            let data = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2 as usize) };
            tasks::send_message(tasks::current_task(), dest, data)
        }
        syscall_abi::MSG_RECV => {
            if !valid_msg_range(arg0, arg1) {
                return FS_ERROR;
            }
            // Fast path: a message is already queued.
            if let Some(packed) = tasks::try_recv_message(tasks::current_task(), arg0, arg1) {
                return packed;
            }
            // Block until one arrives (or Ctrl+C) - same pass-through
            // return contract as READ_CHAR/WAIT. No sender filter: a
            // plain recv takes the oldest message from anyone.
            unsafe {
                tasks::block_current_and_switch(
                    frame,
                    tasks::WaitReason::Message { buf: arg0, len: arg1, from: None },
                )
            }
        }
        syscall_abi::MSG_TRY_RECV => {
            if !valid_msg_range(arg0, arg1) {
                return FS_ERROR;
            }
            tasks::try_recv_message(tasks::current_task(), arg0, arg1).unwrap_or(syscall_abi::NO_MSG)
        }
        syscall_abi::MSG_CALL => {
            let dest = arg0 as usize;
            // Request pointer/length are caller-supplied; the reply
            // buffer's length is the fixed MSG_MAX_LEN (the 4-argument
            // ABI is exactly full - see the syscall-abi doc).
            if !valid_msg_range(arg1, arg2) || !valid_msg_range(arg3, syscall_abi::MSG_MAX_LEN) {
                return FS_ERROR;
            }
            if dest == tasks::current_task() {
                // Calling yourself would block waiting for a reply
                // only you could send - a guaranteed deadlock, same
                // up-front refusal as WAIT's self-wait.
                return syscall_abi::TASK_ERR_PROTECTED;
            }
            if dest >= tasks::NUM_TASKS || !tasks::task_exists(dest) {
                return syscall_abi::TASK_ERR_NO_SUCH_TASK;
            }
            // SAFETY: bounds sanity-checked above, same trust model as
            // MSG_SEND.
            let data = unsafe { core::slice::from_raw_parts(arg1 as *const u8, arg2 as usize) };
            let sent = tasks::send_message(tasks::current_task(), dest, data);
            if sent != 0 {
                return sent;
            }
            // The send above direct-delivers if dest is blocked in a
            // recv (waking it); blocking here then switches straight
            // to it (block_current_and_switch_to's `prefer`) - the
            // synchronous handoff that makes the round trip sub-tick
            // rather than waiting for the round-robin to come back
            // around past the idle task. The reply can't exist yet
            // (dest hasn't run since the request was queued), so there's
            // no fast path to check first.
            unsafe {
                tasks::block_current_and_switch_to(
                    frame,
                    tasks::WaitReason::Message {
                        buf: arg3,
                        len: syscall_abi::MSG_MAX_LEN,
                        from: Some(dest),
                    },
                    Some(dest),
                )
            }
        }
        syscall_abi::BLOCK_INFO => {
            if !block_access_allowed() {
                return syscall_abi::BLOCK_ERR_DENIED;
            }
            match unsafe { &*BLOCK.0.get() } {
                Some(device) => device.capacity_sectors(),
                None => syscall_abi::BLOCK_ERR_NO_DEVICE,
            }
        }
        syscall_abi::BLOCK_READ => {
            // arg0 = LBA, arg1 = buffer pointer; length is implied
            // (exactly one 512-byte sector), so the validation passes
            // the fixed size rather than a caller-supplied one.
            if !block_access_allowed() || !valid_user_range(arg1, syscall_abi::BLOCK_SECTOR_SIZE) {
                return syscall_abi::BLOCK_ERR_DENIED;
            }
            let Some(device) = (unsafe { &mut *BLOCK.0.get() }) else {
                return syscall_abi::BLOCK_ERR_NO_DEVICE;
            };
            // SAFETY: pointer sanity-checked above (same trust model as
            // every fs_* buffer); the device was initialized before
            // install_block_device, its regions mapped.
            let buf = unsafe { &mut *(arg1 as *mut [u8; 512]) };
            match unsafe { device.read_sector(arg0, buf) } {
                Ok(()) => 0,
                Err(_) => syscall_abi::BLOCK_ERR_IO,
            }
        }
        syscall_abi::BLOCK_WRITE => {
            if !block_access_allowed() || !valid_user_range(arg1, syscall_abi::BLOCK_SECTOR_SIZE) {
                return syscall_abi::BLOCK_ERR_DENIED;
            }
            let Some(device) = (unsafe { &mut *BLOCK.0.get() }) else {
                return syscall_abi::BLOCK_ERR_NO_DEVICE;
            };
            // SAFETY: same as BLOCK_READ.
            let buf = unsafe { &*(arg1 as *const [u8; 512]) };
            match unsafe { device.write_sector(arg0, buf) } {
                Ok(()) => 0,
                Err(_) => syscall_abi::BLOCK_ERR_IO,
            }
        }
        syscall_abi::FG => {
            let i = arg0 as usize;
            if i == 1 {
                // Foregrounding idle would strand the keyboard on a
                // task that never reads it, with nothing able to type
                // the way back. Index 0 is allowed - an explicit
                // "give it back".
                return syscall_abi::TASK_ERR_PROTECTED;
            }
            if !tasks::task_exists(i) {
                return syscall_abi::TASK_ERR_NO_SUCH_TASK;
            }
            tasks::set_input_owner(i);
            0
        }
        syscall_abi::GRANT => {
            // arg0 = grantee, arg1 = buf ptr, arg2 = buf len, arg3 = dir.
            let grantee = arg0 as usize;
            let dir = arg3;
            let valid_dir =
                dir != 0 && dir & !(syscall_abi::GRANT_READ | syscall_abi::GRANT_WRITE) == 0;
            if !valid_dir
                || grantee >= tasks::NUM_TASKS
                || !tasks::task_exists(grantee)
                || arg2 == 0
                || arg2 > syscall_abi::SAFECOPY_MAX
                || !in_caller_region(arg1, arg2)
            {
                return syscall_abi::GRANT_ERR;
            }
            tasks::set_grant(tasks::current_task(), grantee, arg1, arg2, dir);
            0
        }
        syscall_abi::SAFECOPY => {
            // arg0 = client, arg1 = client offset, arg2 = local buf ptr,
            // arg3 = len; the 5th argument (direction) doesn't fit the
            // 4-arg dispatch signature - it's read from the saved frame
            // (x4), exactly as the trampoline's doc comment describes.
            let client = arg0 as usize;
            let local = arg2;
            let len = arg3;
            let dir = unsafe { (*frame).gpr[4] };
            if len == 0 || len > syscall_abi::SAFECOPY_MAX || !in_caller_region(local, len) {
                return syscall_abi::SAFECOPY_ERR;
            }
            match tasks::safecopy(tasks::current_task(), client, arg1, local, len, dir) {
                Some(n) => n,
                None => syscall_abi::SAFECOPY_ERR,
            }
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

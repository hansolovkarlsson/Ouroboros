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
//! ## Pointer arguments trust the caller - a real, known simplification
//!
//! `fs_list_dir`/`fs_read_file` take raw `(pointer, length)` pairs for
//! paths and output buffers - valid to dereference directly from EL1
//! because this kernel has exactly one address space (no per-process
//! virtual memory yet), so an EL0-valid pointer from the one loaded
//! program is automatically EL1-valid too. What isn't done: verifying the
//! pointer/length actually falls within that program's own mapped region
//! before touching it. Fine while there's exactly one, currently-trusted
//! userland program and no isolation guarantee promised yet - a real gap
//! once that stops being true. `valid_user_range` below is a minimal
//! sanity bound (non-null, non-zero, capped length), not a real
//! memory-safety check.

use core::sync::atomic::{AtomicU64, Ordering};

use syscall_abi::{FS_ERROR, NO_CHAR, NO_FS};

use crate::console;
use crate::exceptions;
use crate::exceptions::Context;
use crate::fat32;
use crate::tasks;

/// Two independent counters (one per `tasks.rs` task), proof — alongside
/// `double`/`print` below — that syscalls arriving from *different*,
/// actually-switching contexts still land in the right dispatch arm with
/// the right per-caller state, not just that the dispatch table has more
/// than one entry.
static TASK_REPORTS: [AtomicU64; crate::tasks::NUM_TASKS] = [AtomicU64::new(0), AtomicU64::new(0)];

/// The mounted FAT32 filesystem (if any) `fs_list_dir`/`fs_read_file`
/// operate on - `None` is a valid, expected state (e.g. `make run`'s
/// vvfat disk is FAT16, not FAT32 - see `fat32.rs`), not a bug; both
/// syscalls just report an error to userland rather than the kernel
/// refusing to boot without one.
struct FsCell(core::cell::UnsafeCell<Option<fat32::Fs>>);
// SAFETY: single-core; only ever touched from syscall dispatch, which by
// construction can't run re-entrantly (taking an exception masks further
// exceptions until the next `eret`) - same reasoning as every other
// single-instance global in this project.
unsafe impl Sync for FsCell {}
static FS: FsCell = FsCell(core::cell::UnsafeCell::new(None));

/// Installs the filesystem `fs_list_dir`/`fs_read_file` operate on. Called
/// once at boot (`main.rs`), after a successful `fat32::Fs::mount` - never
/// called again, so no locking beyond `FsCell`'s existing single-core
/// reasoning is needed.
pub fn install_fs(fs: fat32::Fs) {
    unsafe { *FS.0.get() = Some(fs) };
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
    console::read_byte().or_else(crate::xhci::poll_key)
}

/// Minimal sanity bound for a userland `(pointer, length)` argument pair -
/// see the module doc comment's note on what this isn't.
const MAX_USER_LEN: u64 = 512;

fn valid_user_range(ptr: u64, len: u64) -> bool {
    ptr != 0 && len != 0 && len <= MAX_USER_LEN
}

/// Same bound as [`valid_user_range`], except a zero length is
/// accepted (with a still-non-null pointer) rather than rejected -
/// needed for `fs_write_file`'s data argument, where empty input is a
/// real, meaningful case (writing/truncating a file to zero bytes),
/// unlike every other `(pointer, length)` pair this module validates
/// (a path can't be empty, an output buffer with zero length is
/// pointless). A real bug caught by testing `write <file>` with no
/// content words: without this, an empty write was indistinguishable
/// from a rejected one.
fn valid_user_range_allow_empty(ptr: u64, len: u64) -> bool {
    if len == 0 { ptr != 0 } else { valid_user_range(ptr, len) }
}

/// Formats `path`'s directory entries into `buf` as `name\n` (`name/\n`
/// for subdirectories), stopping before an entry would overflow `buf`
/// rather than erroring - a truncated listing is more useful to a caller
/// than none at all, same spirit as `read_file`'s own truncation
/// behavior. Returns the number of bytes written, [`NO_FS`] if there's no
/// mounted filesystem, or [`FS_ERROR`] if `path` doesn't resolve to a
/// directory.
fn fs_list_dir(path: &str, buf: &mut [u8]) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    // `list_dir`'s callback can't signal "stop early" (unlike
    // `walk_dir`'s internal one - see fat32.rs), so once `buf` is full
    // this just stops *writing*, not iterating: wasted work on a huge
    // directory, harmless and simple for the small ones this project
    // deals with.
    let mut written = 0usize;
    let result = fs.list_dir(path, |name, is_dir, _size| {
        let suffix: &[u8] = if is_dir { b"/\n" } else { b"\n" };
        let entry_len = name.len() + suffix.len();
        if written + entry_len > buf.len() {
            return;
        }
        buf[written..written + name.len()].copy_from_slice(name.as_bytes());
        written += name.len();
        buf[written..written + suffix.len()].copy_from_slice(suffix);
        written += suffix.len();
    });
    match result {
        Ok(()) => written as u64,
        Err(_) => FS_ERROR,
    }
}

/// Reads `path`'s contents into `buf`, same truncation contract as
/// `fat32::Fs::read_file` (returns the file's real size, which may exceed
/// `buf.len()` - compare to detect truncation). [`NO_FS`] if there's no
/// mounted filesystem, [`FS_ERROR`] if `path` doesn't resolve to a file.
fn fs_read_file(path: &str, buf: &mut [u8]) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.read_file(path, buf) {
        Ok(size) => size as u64,
        Err(_) => FS_ERROR,
    }
}

/// Creates an empty directory at `path`. `0` on success, [`NO_FS`] if
/// there's no mounted filesystem, [`FS_ERROR`] if [`fat32::Fs::mkdir`]
/// itself failed (already exists, invalid name, parent missing, disk
/// full, ...) - every distinct `fat32::Error` still collapses to that one
/// sentinel, same as `fs_list_dir`/`fs_read_file` already do; a userland
/// program that needs to know *why* still has no way to (a real gap, not
/// new to this syscall).
fn fs_mkdir(path: &str) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.mkdir(path) {
        Ok(()) => 0,
        Err(_) => FS_ERROR,
    }
}

/// Removes the empty directory at `path`. Same success/error contract as
/// [`fs_mkdir`].
fn fs_rmdir(path: &str) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.rmdir(path) {
        Ok(()) => 0,
        Err(_) => FS_ERROR,
    }
}

/// Creates an empty file at `path`, or succeeds as a no-op if a file
/// already exists there (see [`fat32::Fs::touch`]). Same `NO_FS`/
/// `FS_ERROR` contract as [`fs_mkdir`].
fn fs_touch(path: &str) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.touch(path) {
        Ok(()) => 0,
        Err(_) => FS_ERROR,
    }
}

/// Removes the file at `path`. Same `NO_FS`/`FS_ERROR` contract as
/// [`fs_mkdir`].
fn fs_rm(path: &str) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.rm(path) {
        Ok(()) => 0,
        Err(_) => FS_ERROR,
    }
}

/// Creates or fully overwrites the file at `path` with `data`. Same
/// `NO_FS`/`FS_ERROR` contract as [`fs_mkdir`] - the first syscall able
/// to give a file more than zero bytes (see [`fat32::Fs::write_file`]).
fn fs_write_file(path: &str, data: &[u8]) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.write_file(path, data) {
        Ok(()) => 0,
        Err(_) => FS_ERROR,
    }
}

/// Renames or moves the file or directory at `src` to `dst`. Same
/// `NO_FS`/`FS_ERROR` contract as [`fs_mkdir`] (see
/// [`fat32::Fs::mv`]).
fn fs_mv(src: &str, dst: &str) -> u64 {
    let Some(fs) = (unsafe { &mut *FS.0.get() }) else {
        return NO_FS;
    };
    match fs.mv(src, dst) {
        Ok(()) => 0,
        Err(_) => FS_ERROR,
    }
}

/// Called from the exception vector's SVC trampoline (`exceptions.rs`)
/// with the syscall number (from x8) and up to 4 arguments (from x0-x3),
/// running at EL1 with the kernel's own stack and every privilege EL0
/// lacks - the entire reason this indirection exists. Its return value
/// becomes EL0's new x0 after `eret`.
///
/// Fourteen syscalls now (`shell_input`, a fifteenth by original
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
/// than reading and rewriting its content.
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
        syscall_abi::GET_TICKS => exceptions::ticks(),
        syscall_abi::FS_LIST_DIR => {
            if !valid_user_range(arg0, arg1) || !valid_user_range(arg2, arg3) {
                return FS_ERROR;
            }
            // SAFETY: bounds sanity-checked above; see the module doc
            // comment's note on what that check does and doesn't cover.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            let buf = unsafe { core::slice::from_raw_parts_mut(arg2 as *mut u8, arg3 as usize) };
            fs_list_dir(path, buf)
        }
        syscall_abi::FS_READ_FILE => {
            if !valid_user_range(arg0, arg1) || !valid_user_range(arg2, arg3) {
                return FS_ERROR;
            }
            // SAFETY: same as FS_LIST_DIR.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            let buf = unsafe { core::slice::from_raw_parts_mut(arg2 as *mut u8, arg3 as usize) };
            fs_read_file(path, buf)
        }
        syscall_abi::FS_MKDIR => {
            if !valid_user_range(arg0, arg1) {
                return FS_ERROR;
            }
            // SAFETY: bounds sanity-checked above; see the module doc
            // comment's note on what that check does and doesn't cover.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            fs_mkdir(path)
        }
        syscall_abi::FS_RMDIR => {
            if !valid_user_range(arg0, arg1) {
                return FS_ERROR;
            }
            // SAFETY: same as FS_MKDIR.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            fs_rmdir(path)
        }
        syscall_abi::FS_TOUCH => {
            if !valid_user_range(arg0, arg1) {
                return FS_ERROR;
            }
            // SAFETY: same as FS_MKDIR.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            fs_touch(path)
        }
        syscall_abi::FS_RM => {
            if !valid_user_range(arg0, arg1) {
                return FS_ERROR;
            }
            // SAFETY: same as FS_MKDIR.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            fs_rm(path)
        }
        syscall_abi::FS_WRITE_FILE => {
            // `data`'s length is allowed to be zero (see
            // valid_user_range_allow_empty's doc comment) - the path
            // never is.
            if !valid_user_range(arg0, arg1) || !valid_user_range_allow_empty(arg2, arg3) {
                return FS_ERROR;
            }
            // SAFETY: same as FS_LIST_DIR/FS_READ_FILE.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return FS_ERROR };
            let data = unsafe { core::slice::from_raw_parts(arg2 as *const u8, arg3 as usize) };
            fs_write_file(path, data)
        }
        syscall_abi::FS_MV => {
            // Both `src` and `dst` are paths - neither can legitimately
            // be empty, so this uses the stricter valid_user_range for
            // both, unlike FS_WRITE_FILE's data argument.
            if !valid_user_range(arg0, arg1) || !valid_user_range(arg2, arg3) {
                return FS_ERROR;
            }
            // SAFETY: same as FS_LIST_DIR/FS_READ_FILE.
            let src = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(src) = core::str::from_utf8(src) else { return FS_ERROR };
            let dst = unsafe { core::slice::from_raw_parts(arg2 as *const u8, arg3 as usize) };
            let Ok(dst) = core::str::from_utf8(dst) else { return FS_ERROR };
            fs_mv(src, dst)
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

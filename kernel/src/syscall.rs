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

use crate::console;
use crate::exceptions;
use crate::fat32;

/// Two independent counters (one per `tasks.rs` task), proof — alongside
/// `double`/`print` below — that syscalls arriving from *different*,
/// actually-switching contexts still land in the right dispatch arm with
/// the right per-caller state, not just that the dispatch table has more
/// than one entry.
static TASK_REPORTS: [AtomicU64; crate::tasks::NUM_TASKS] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Sentinel `try_read_char` returns in x0 when no byte is waiting -
/// out of range for any real byte (0-255), so callers can tell the two
/// apart with a single comparison.
pub const NO_CHAR: u64 = u64::MAX;

/// Generic failure sentinel for the `fs_*` syscalls (not found, not a
/// directory, already exists, disk full, ...) - every distinct
/// `fat32::Error` still collapses to this one value; a userland program
/// that needs to know *why* still can't (a real gap, unchanged by
/// [`NO_FS`] below).
const FS_ERROR: u64 = u64::MAX;

/// A second, distinguishable sentinel the `fs_*` syscalls return
/// specifically when there's no mounted filesystem at all (e.g. `make
/// run`'s vvfat disk is FAT16, not FAT32 - see `fat32.rs`), rather than
/// collapsing into the same generic [`FS_ERROR`] every other failure
/// uses. Added after real user confusion: without this, `ls`/`mkdir`
/// failing on `make run` looked identical to a genuinely broken path or
/// a corrupt disk, and the actual cause (no FAT32 partition found at
/// boot) was only ever visible in the kernel's own boot log, not to the
/// shell. Safe to keep numerically distinct from any real return value:
/// `fs_list_dir`/`fs_read_file` only ever return small byte counts/file
/// sizes (bounded by `MAX_USER_LEN`/a `u32` file size), nowhere near
/// `u64::MAX - 1`.
pub const NO_FS: u64 = u64::MAX - 1;

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

/// Minimal sanity bound for a userland `(pointer, length)` argument pair -
/// see the module doc comment's note on what this isn't.
const MAX_USER_LEN: u64 = 512;

fn valid_user_range(ptr: u64, len: u64) -> bool {
    ptr != 0 && len != 0 && len <= MAX_USER_LEN
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

/// Called from the exception vector's SVC trampoline (`exceptions.rs`)
/// with the syscall number (from x8) and up to 4 arguments (from x0-x3),
/// running at EL1 with the kernel's own stack and every privilege EL0
/// lacks - the entire reason this indirection exists. Its return value
/// becomes EL0's new x0 after `eret`.
///
/// Ten syscalls now (`shell_input`, an eleventh by original numbering, was
/// removed - see below).
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
/// neighbors - a stable ABI matters more than a dense one, and this is a
/// preview of exactly the kind of churn a shared syscall-ABI crate should
/// prevent once EL0 code isn't only ever built in this same repo (see
/// `docs/processes.md`'s "known rough edges"). `get_ticks` (6) is the
/// first syscall added for phase 2 (commands) - the shell's `uptime`
/// builtin needs *some* real kernel state to report, now that it can't
/// just read `exceptions.rs`'s statics directly the way kernel-resident
/// code used to. `fs_list_dir`/`fs_read_file` (7/8) are phase 3c's - the
/// first syscalls needing more than one argument, which is why `dispatch`
/// grew from 1 argument to 4 (see this module's doc comment and
/// `exceptions.rs`'s). `fs_mkdir`/`fs_rmdir` (9/10) are the first write
/// support this kernel has ever exposed to userland - see
/// `fat32.rs`/`virtio_blk.rs` for the on-disk write path underneath them.
pub extern "C" fn dispatch(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    match number {
        0 => {
            console::println!("Ouroboros kernel: syscall from EL0: print(arg0={arg0:#x})");
            0
        }
        1 => {
            let result = arg0.wrapping_mul(2);
            console::println!("Ouroboros kernel: syscall from EL0: double(arg0={arg0:#x}) = {result:#x}");
            result
        }
        2 => {
            let task_id = arg0 as usize;
            match TASK_REPORTS.get(task_id) {
                Some(counter) => {
                    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    console::println!("Ouroboros kernel: task {task_id} report #{count}");
                    0
                }
                None => u64::MAX,
            }
        }
        3 => match console::read_byte() {
            Some(byte) => byte as u64,
            None => NO_CHAR,
        },
        4 => {
            console::putc(arg0 as u8);
            0
        }
        6 => exceptions::ticks(),
        7 => {
            if !valid_user_range(arg0, arg1) || !valid_user_range(arg2, arg3) {
                return u64::MAX;
            }
            // SAFETY: bounds sanity-checked above; see the module doc
            // comment's note on what that check does and doesn't cover.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return u64::MAX };
            let buf = unsafe { core::slice::from_raw_parts_mut(arg2 as *mut u8, arg3 as usize) };
            fs_list_dir(path, buf)
        }
        8 => {
            if !valid_user_range(arg0, arg1) || !valid_user_range(arg2, arg3) {
                return u64::MAX;
            }
            // SAFETY: same as syscall 7.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return u64::MAX };
            let buf = unsafe { core::slice::from_raw_parts_mut(arg2 as *mut u8, arg3 as usize) };
            fs_read_file(path, buf)
        }
        9 => {
            if !valid_user_range(arg0, arg1) {
                return u64::MAX;
            }
            // SAFETY: bounds sanity-checked above; see the module doc
            // comment's note on what that check does and doesn't cover.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return u64::MAX };
            fs_mkdir(path)
        }
        10 => {
            if !valid_user_range(arg0, arg1) {
                return u64::MAX;
            }
            // SAFETY: same as syscall 9.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let Ok(path) = core::str::from_utf8(path) else { return u64::MAX };
            fs_rmdir(path)
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

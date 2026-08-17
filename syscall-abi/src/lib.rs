//! Shared syscall ABI constants between the kernel's dispatch table
//! (`kernel/src/syscall.rs`) and every userland program that calls `svc`
//! (currently just `shell/`). Before this crate existed, these numbers
//! and sentinel values were hand-duplicated in both places, kept in sync
//! only by convention - a real, growing risk once syscalls
//! reached double digits and nothing about the calling convention itself
//! would catch the two sides drifting silently apart. See
//! `docs/processes.md`'s "known rough edges" for the history this
//! replaces.
//!
//! `#![no_std]`, no logic, just constants - safe to depend on from either
//! target this project builds for (`aarch64-unknown-uefi` for the
//! kernel, `aarch64-unknown-none` for userland programs), since nothing
//! here is target-specific and every value is a plain integer inlined at
//! the use site, not a pointer/reference needing relocation - so this
//! carries none of the "no comparing a slice/string against a literal"
//! risk documented for userland programs elsewhere in this project (see
//! `docs/processes.md`).
//!
//! Calling convention: syscall number in `x8`, up to 4 arguments in
//! `x0`-`x3`, return value in `x0`. See `docs/architecture.md`'s syscall
//! table for the full picture, including what each syscall actually does
//! and why the gap at 5 exists.

#![no_std]

/// `print` - demo/debug only, logs `arg0` through the kernel console.
pub const PRINT: u64 = 0;

/// `double` - demo only, returns `arg0 * 2`; proves a return value
/// survives the trampoline intact.
pub const DOUBLE: u64 = 1;

/// `report` - demo only, `tasks.rs`'s original two-task milestone's proof
/// of per-task syscall state. `arg0` is a task ID.
pub const REPORT: u64 = 2;

/// Non-blocking: returns a byte, or [`NO_CHAR`] if none is waiting.
pub const TRY_READ_CHAR: u64 = 3;

/// Raw single-byte console write, no newline translation. `arg0` is the
/// byte to write.
pub const PUTC: u64 = 4;

// 5 is a deliberate gap, not an oversight - `shell_input` used to live
// here; removed when line editing moved out of the kernel and into
// userland (see CLAUDE.md's "shell becomes a real disk-loaded process"
// section). Left unfilled rather than renumbering every syscall after
// it: a stable ABI matters more than a dense one.

/// Preemption tick count since boot - the first syscall added
/// specifically so a loaded program could read real kernel state, not
/// just do I/O.
pub const GET_TICKS: u64 = 6;

/// `(path ptr, path len, buf ptr, buf len)` -> bytes written, [`NO_FS`],
/// or [`FS_ERROR`]. Formats each entry as `name\n`/`name/\n`, truncating
/// rather than erroring if the buffer is too small.
pub const FS_LIST_DIR: u64 = 7;

/// `(path ptr, path len, buf ptr, buf len)` -> the file's real size
/// (which may exceed the buffer's length - compare to detect
/// truncation), [`NO_FS`], or [`FS_ERROR`].
pub const FS_READ_FILE: u64 = 8;

/// `(path ptr, path len)` -> `0` on success, [`NO_FS`], or [`FS_ERROR`].
/// Creates an empty directory.
pub const FS_MKDIR: u64 = 9;

/// `(path ptr, path len)` -> `0` on success, [`NO_FS`], or [`FS_ERROR`].
/// Removes an empty directory.
pub const FS_RMDIR: u64 = 10;

/// `(path ptr, path len)` -> `0` on success, [`NO_FS`], or [`FS_ERROR`].
/// Creates an empty (zero-byte) file, or succeeds as a no-op if a file
/// already exists there (there's no RTC to update a modification time
/// with, so "no-op" is the closest honest approximation of real
/// `touch`'s behavior on an existing file).
pub const FS_TOUCH: u64 = 11;

/// `(path ptr, path len)` -> `0` on success, [`NO_FS`], or [`FS_ERROR`].
/// Removes a file (not a directory - use [`FS_RMDIR`] for those).
pub const FS_RM: u64 = 12;

/// `(path ptr, path len, data ptr, data len)` -> `0` on success, [`NO_FS`],
/// or [`FS_ERROR`]. Creates a file with exactly `data`'s contents, or
/// fully overwrites (not appends to) an existing file's contents. The
/// first syscall able to give a file more than zero bytes - without
/// this, [`FS_TOUCH`] was the only way to create a file, and it only
/// ever produces empty ones.
pub const FS_WRITE_FILE: u64 = 13;

/// `(src ptr, src len, dst ptr, dst len)` -> `0` on success, [`NO_FS`],
/// or [`FS_ERROR`]. Renames or moves the file or directory at `src` to
/// `dst`. `dst` must not already exist - this refuses rather than
/// overwriting it or moving `src` inside it.
pub const FS_MV: u64 = 14;

/// Blocking: waits until a byte is available and returns it, rather than
/// returning [`NO_CHAR`] immediately like [`TRY_READ_CHAR`]. The caller
/// simply doesn't run again until then - the kernel suspends it and
/// schedules another task in its place (see `tasks.rs`'s
/// `block_current_and_switch`), not a spin-wait on either side.
pub const READ_CHAR: u64 = 15;

/// `(path ptr, path len)` -> `0` on success, [`NO_FS`], or
/// [`SPAWN_ERROR`]. Loads a second program from disk and starts it
/// running *alongside* the caller - a real `spawn`, not POSIX
/// exec-replaces-current-process semantics; the calling task is
/// completely untouched. See `tasks.rs`'s `spawn` for the mechanism and
/// its real, deliberate limits (a small fixed number of extra task
/// slots, no destruction once spawned).
pub const SPAWN: u64 = 16;

/// Sentinel `try_read_char` returns when no byte is waiting - out of
/// range for any real byte (0-255), so callers can tell the two apart
/// with a single comparison.
pub const NO_CHAR: u64 = u64::MAX;

/// Generic failure sentinel for the `fs_*` syscalls: the filesystem is
/// mounted, but this specific operation failed (not found, not a
/// directory, already exists, disk full, ...). Every distinct
/// `fat32::Error` still collapses to this one value - a userland program
/// that needs to know *why* still can't (a real gap, unchanged by
/// [`NO_FS`]).
pub const FS_ERROR: u64 = u64::MAX;

/// A second, distinguishable sentinel the `fs_*` syscalls return
/// specifically when there's no mounted filesystem at all this boot
/// (e.g. `make run`'s vvfat disk is FAT16, not FAT32), rather than
/// collapsing into the same generic [`FS_ERROR`] every other failure
/// uses. Added after real user confusion: without this distinction,
/// every disk command failing on `make run` looked identical to a
/// genuinely broken path, and the real cause was only ever visible in
/// the kernel's own boot log. Safe to keep numerically distinct from any
/// real return value: `fs_list_dir`/`fs_read_file` only ever return
/// small byte counts/file sizes, nowhere near `u64::MAX - 1`.
pub const NO_FS: u64 = u64::MAX - 1;

/// Generic failure sentinel for [`SPAWN`] - same bit pattern as
/// [`FS_ERROR`] (a bad ELF, no free task slot, and a disk read failure
/// all collapse to this one value, matching the `fs_*` syscalls' own
/// "one generic failure sentinel" precedent), given its own name since
/// it's a semantically distinct concept even though the value coincides.
/// [`SPAWN`] returns [`NO_FS`] separately when there's no mounted
/// filesystem at all, same as every `fs_*` syscall.
pub const SPAWN_ERROR: u64 = u64::MAX;

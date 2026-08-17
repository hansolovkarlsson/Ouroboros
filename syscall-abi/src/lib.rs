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
/// slots).
pub const SPAWN: u64 = 16;

/// `(exit code)` -> **does not return** on success: the calling task is
/// destroyed (slot freed for a future [`SPAWN`], EL0 mapping removed,
/// RAM reclaimed when the runtime allocator's LIFO order allows - see
/// `tasks.rs`'s `free_runtime_region`) and another runnable task is
/// switched to in its place. The one case where this *does* return to
/// the caller: [`EXIT_DENIED`], for the two tasks that are refused -
/// task 0 (the boot shell; nothing would own the keyboard, see
/// `tasks.rs`'s `INPUT_OWNER_TASK`) and task 1 (idle; it never makes
/// syscalls anyway, refused for completeness). The exit code carries no
/// meaning yet beyond the kernel's own `task N exited (code X)` log
/// line - nothing waits on or reaps tasks.
pub const EXIT: u64 = 17;

/// [`EXIT`]'s only possible return value (a successful exit never
/// returns): the calling task is one of the two that may not exit.
pub const EXIT_DENIED: u64 = u64::MAX;

/// Sentinel `try_read_char` returns when no byte is waiting - out of
/// range for any real byte (0-255), so callers can tell the two apart
/// with a single comparison.
pub const NO_CHAR: u64 = u64::MAX;

/// Generic/unknown failure for the `fs_*` syscalls - the fallback when
/// no more specific `FS_ERR_*` code below applies (today that's exactly
/// the argument-validation rejections: a bad `(pointer, length)` pair
/// never reaches the filesystem at all). Every *filesystem* failure now
/// returns one of the specific codes instead - the old
/// "every distinct `fat32::Error` collapses to this one value" gap is
/// closed.
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

// Specific `fs_*` failure codes - the split of the old single collapsed
// [`FS_ERROR`], one code per cause a caller can meaningfully act on,
// mapped from `fat32::Error` by `kernel/src/syscall.rs::fs_error_code`.
// All live in the same reserved top band as the sentinels above
// (see [`FS_ERR_MIN`]), so every real success value (byte counts, file
// sizes) stays valid - the same safety argument [`NO_FS`] already made.

/// The path (or its parent) doesn't resolve.
pub const FS_ERR_NOT_FOUND: u64 = u64::MAX - 2;
/// The path resolves to a directory where a file was required
/// (`cat`/`rm`/`write` on a directory).
pub const FS_ERR_NOT_A_FILE: u64 = u64::MAX - 3;
/// The path resolves to a file where a directory was required
/// (`ls`/`cd`/`rmdir` on a file).
pub const FS_ERR_NOT_A_DIRECTORY: u64 = u64::MAX - 4;
/// The name doesn't fit this kernel's conservative 8.3 short-name
/// subset (see `fat32.rs::make_short_name`).
pub const FS_ERR_INVALID_NAME: u64 = u64::MAX - 5;
/// An entry with this name already exists.
pub const FS_ERR_ALREADY_EXISTS: u64 = u64::MAX - 6;
/// `rmdir` on a directory that still has entries.
pub const FS_ERR_NOT_EMPTY: u64 = u64::MAX - 7;
/// `rmdir` on the root directory.
pub const FS_ERR_IS_ROOT: u64 = u64::MAX - 8;
/// No free cluster left on the volume.
pub const FS_ERR_DISK_FULL: u64 = u64::MAX - 9;
/// A device-level (virtio-blk) read/write failure - or one of the
/// mount-shape errors that can't actually occur through an
/// already-mounted filesystem, mapped here rather than omitted.
pub const FS_ERR_IO: u64 = u64::MAX - 10;

// [`SPAWN`]-specific failure codes, in the same reserved band - the
// split of the old collapsed [`SPAWN_ERROR`], mirroring the `FS_ERR_*`
// split. A spawn that fails *reading* the program file returns the
// ordinary `FS_ERR_*` code for what went wrong (e.g.
// [`FS_ERR_NOT_FOUND`]); these three cover the causes the filesystem
// codes can't express.

/// The file was read, but isn't a loadable program (bad ELF header,
/// unsupported relocation, malformed program headers, ...).
pub const SPAWN_ERR_BAD_ELF: u64 = u64::MAX - 11;
/// The program is larger than the kernel's fixed staging buffer (or
/// empty) - refused outright rather than loaded truncated.
pub const SPAWN_ERR_TOO_LARGE: u64 = u64::MAX - 12;
/// Every task slot already holds a live task.
pub const SPAWN_ERR_NO_FREE_SLOT: u64 = u64::MAX - 13;

/// Floor of the reserved error band (with headroom for future codes):
/// **any `fs_*` return value `>= FS_ERR_MIN` is an error**, everything
/// below is a real result. The predicate callers actually need, since
/// `fs_read_file`/`fs_list_dir` return arbitrary byte counts on success
/// and can't enumerate every non-error value in a `match`.
pub const FS_ERR_MIN: u64 = u64::MAX - 15;

/// Generic failure sentinel for [`SPAWN`] - same bit pattern as
/// [`FS_ERROR`] (a bad ELF, no free task slot, and a disk read failure
/// all collapse to this one value, matching the `fs_*` syscalls' own
/// "one generic failure sentinel" precedent), given its own name since
/// it's a semantically distinct concept even though the value coincides.
/// [`SPAWN`] returns [`NO_FS`] separately when there's no mounted
/// filesystem at all, same as every `fs_*` syscall.
pub const SPAWN_ERROR: u64 = u64::MAX;

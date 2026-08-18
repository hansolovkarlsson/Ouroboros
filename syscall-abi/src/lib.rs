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

// 7 through 14 are deliberate gaps now, like 5 - the eight fs_*
// syscalls (list_dir/read_file/mkdir/rmdir/touch/rm/write_file/mv)
// lived here until the filesystem moved out of the kernel entirely
// (driver isolation part 2). Their exact contracts survive unchanged
// as the filesystem server's request protocol - see the `FSOP_*`
// constants further below - and their old numbers stay unfilled, same
// stable-ABI-over-dense-ABI reasoning as the gap at 5.

/// Blocking: waits until a byte is available and returns it, rather than
/// returning [`NO_CHAR`] immediately like [`TRY_READ_CHAR`]. The caller
/// simply doesn't run again until then - the kernel suspends it and
/// schedules another task in its place (see `tasks.rs`'s
/// `block_current_and_switch`), not a spin-wait on either side.
pub const READ_CHAR: u64 = 15;

/// `(total staged length)` -> **the new task's slot index** on success
/// (needed to wait on, send to, or kill what was just started - the
/// shell's pipeline flow does all three), [`SPAWN_ERROR`] (bad
/// argument), or a `SPAWN_ERR_*` code. Parses, relocates, and starts
/// the program previously fed into the kernel's staging buffer via
/// [`SPAWN_STAGE`], as a new task running *alongside* the caller - a
/// real `spawn`, not POSIX exec-replaces-current-process semantics;
/// the calling task is completely untouched. **Contract change with
/// the userland-filesystem milestone:** this used to take a path and
/// read the file itself, but the kernel no longer contains a
/// filesystem to read with - the caller reads the program (via the
/// filesystem server) and stages it chunk by chunk first. See
/// `tasks.rs`'s `spawn` for the mechanism and its real, deliberate
/// limits (a small fixed number of extra task slots).
pub const SPAWN: u64 = 16;

/// `(exit code)` -> **does not return** on success: the calling task is
/// destroyed (slot freed for a future [`SPAWN`], EL0 mapping removed,
/// RAM reclaimed when the runtime allocator's LIFO order allows - see
/// `tasks.rs`'s `free_runtime_region`) and another runnable task is
/// switched to in its place. The one case where this *does* return to
/// the caller: [`EXIT_DENIED`], for the three tasks that are refused -
/// task 0 (the boot shell; nothing would own the keyboard, see
/// `tasks.rs`'s `INPUT_OWNER_TASK`), task 1 (idle; it never makes
/// syscalls anyway, refused for completeness), and task 2 (the
/// filesystem server, [`FSD_TASK`] - its death would strand the disk
/// for the rest of the boot). The exit code is masked to a
/// byte (`0..=255`, POSIX-style) and kept until collected by a
/// [`WAIT`]er - see [`WAIT`] for the full reaping model.
pub const EXIT: u64 = 17;

/// [`EXIT`]'s only possible return value (a successful exit never
/// returns): the calling task is one of the three that may not exit.
pub const EXIT_DENIED: u64 = u64::MAX;

/// `(task index)` -> one of the `TASK_STATE_*` values below, or
/// [`TASK_STATE_INVALID`] for an index past the scheduler's slot count -
/// which is also how a caller discovers that count without a separate
/// constant leaking into the ABI: probe indices upward until invalid
/// comes back (see the shell's `ps` builtin). Read-only observability
/// for the spawn/exit lifecycle - the first way userland can see what's
/// actually running.
pub const TASK_STATE: u64 = 18;

/// The slot has no task (never spawned, or exited).
pub const TASK_STATE_UNUSED: u64 = 0;
/// The task is runnable (running or waiting for its round-robin turn -
/// the two are indistinguishable to the caller, who is by definition
/// the one running at the moment it asks).
pub const TASK_STATE_RUNNABLE: u64 = 1;
/// The task is blocked on a wait reason (keyboard input, or another
/// task's exit - see [`WAIT`]).
pub const TASK_STATE_BLOCKED: u64 = 2;
/// The task has exited but its status hasn't been collected yet - the
/// slot is held (not spawnable) until someone [`WAIT`]s on it.
pub const TASK_STATE_ZOMBIE: u64 = 3;
/// [`TASK_STATE`]'s "no such slot" answer.
pub const TASK_STATE_INVALID: u64 = u64::MAX;

/// `(task index)` -> `0` on success, [`TASK_ERR_PROTECTED`] (tasks 0/1
/// are permanent), or [`TASK_ERR_NO_SUCH_TASK`]. Destroys *another*
/// task - same teardown as a voluntary [`EXIT`] (slot freed, mapping
/// removed, RAM reclaimed in the LIFO case), minus the context switch:
/// the killed task isn't the one running. If the killed task held the
/// keyboard (see [`FG`]), ownership reverts to task 0.
pub const KILL: u64 = 19;

/// `(task index)` -> `0` on success, [`TASK_ERR_PROTECTED`] (idle can't
/// be foregrounded), or [`TASK_ERR_NO_SUCH_TASK`]. Hands keyboard
/// ownership to the given task - the caller's own next blocking read
/// then waits, unwoken, until the foregrounded task exits or is killed
/// (ownership reverts to task 0 automatically on the owner's death).
/// **Ctrl+C (`0x03`) is the escape hatch**: typed while a task other
/// than the boot shell owns the keyboard, the kernel intercepts it,
/// reverts ownership to task 0, and swallows the byte - the
/// foregrounded task keeps running in the background (nothing is
/// delivered to or done to it; this is keyboard reclamation, not a
/// signal). Index 0 is allowed as an explicit "give it back".
pub const FG: u64 = 20;

/// `(task index)` -> the task's exit status (`0..=255` - [`EXIT`] masks
/// its argument to a byte, POSIX-style, so a status can never collide
/// with this ABI's error band), [`TASK_KILLED_STATUS`] if the waited
/// task was killed out from under the waiter, [`WAIT_INTERRUPTED`] if
/// the user typed Ctrl+C during the wait (the target keeps running),
/// [`TASK_ERR_PROTECTED`] (waiting on task 0/1 or on yourself is a
/// guaranteed deadlock), or [`TASK_ERR_NO_SUCH_TASK`]. Blocks until the
/// target dies if it's still alive; returns immediately with the status
/// if it's already a zombie. **Collecting the status is what reaps**:
/// the zombie's slot only becomes spawnable again once waited (or the
/// task is `kill`ed, which reaps immediately - the killer already knows
/// the outcome).
pub const WAIT: u64 = 21;

/// `(replace)` -> `0` (a USB storage device was found and installed
/// as the kernel's block device), [`MOUNT_ALREADY`] (a device is
/// already installed and `replace` was `0`; nothing changed), or
/// [`MOUNT_NO_DEVICE`]. Rescans the USB (xHCI) ports for storage
/// devices that attached after boot - on Parallels, a passed-through
/// stick appears a few seconds *after* the kernel's boot-time scan
/// (confirmed by the enumeration diagnostics). **The device half
/// only** since the filesystem moved to userland: actually mounting
/// what the device holds is the server's [`FSOP_MOUNT`] request, and
/// the shell's `mount` command composes the two (server first - see
/// its cmd_mount). `replace != 0` allows swapping out an installed
/// device; callers may only pass it after the server confirms nothing
/// is mounted, or a live filesystem's cached geometry would suddenly
/// describe a different disk.
pub const MOUNT: u64 = 22;

/// `(dest task, buf ptr, len)` -> `0`, [`TASK_ERR_NO_SUCH_TASK`],
/// [`MSG_ERR_TOO_BIG`] (over [`MSG_MAX_LEN`]), or [`MSG_ERR_FULL`].
/// Delivers the message to `dest` - straight into a matching blocked
/// receiver's buffer (direct delivery), or into its bounded mailbox.
/// No shared memory, no blocking sends. Send-to-self is allowed (it's
/// just a queue). A task's queued mail dies with it (cleared on
/// exit/kill); senders are never notified. **A zero-length message is
/// legal** (the pointer must still be non-null): it's the
/// end-of-stream marker in the shell's pipeline convention - a
/// pipeline child (`left | program`) receives its input as a stream of
/// 1-to-[`MSG_MAX_LEN`]-byte data messages followed by one empty
/// message meaning "no more input; finish and exit".
pub const MSG_SEND: u64 = 23;

/// `(buf ptr, len)` -> `(sender << 32) | copied_len`, or
/// [`RECV_INTERRUPTED`] on Ctrl+C. **Blocks** until a message arrives,
/// with the same scheduler-level suspension as [`READ_CHAR`]/[`WAIT`];
/// the oldest queued message is copied into the buffer (truncated to
/// the buffer length if needed).
pub const MSG_RECV: u64 = 24;

/// [`MSG_RECV`]'s non-blocking sibling: same contract, but returns
/// [`NO_MSG`] immediately when the mailbox is empty - the same pairing
/// as [`TRY_READ_CHAR`]/[`READ_CHAR`].
pub const MSG_TRY_RECV: u64 = 25;

/// `(dest task, req ptr, req len, reply ptr)` -> the packed
/// `(sender << 32) | copied_len` of the reply (sender is always
/// `dest`), [`RECV_INTERRUPTED`] on Ctrl+C, [`TASK_ERR_NO_SUCH_TASK`],
/// [`TASK_ERR_PROTECTED`] (calling yourself is a guaranteed deadlock),
/// or a `MSG_ERR_*` code if the send half fails. The synchronous
/// request/response primitive (MINIX's `sendrec` shape): sends the
/// request to `dest`, then blocks until a reply *from `dest`
/// specifically* arrives - a message from any other task stays queued
/// for a later [`MSG_RECV`] rather than being mistaken for the reply.
/// The reply buffer is a fixed [`MSG_MAX_LEN`] bytes (implied, not
/// passed - the 4-argument syscall ABI is exactly full). With direct
/// delivery on both hops (see `tasks.rs::send_message`), a call to a
/// server blocked in [`MSG_RECV`] round-trips without waiting for a
/// tick on either side.
pub const MSG_CALL: u64 = 29;

/// `()` -> the block device's capacity in sectors, or
/// [`BLOCK_ERR_NO_DEVICE`]/[`BLOCK_ERR_DENIED`]. Raw block-device
/// introspection for the filesystem server - see the note on
/// [`BLOCK_READ`] for who may call this (only [`FSD_TASK`]).
pub const BLOCK_INFO: u64 = 26;

/// `(lba, buf ptr)` -> `0` on success, or a `BLOCK_ERR_*` code. Reads
/// exactly one [`BLOCK_SECTOR_SIZE`]-byte sector into `buf` (the length
/// is implied, not passed). **Gated to [`FSD_TASK`]**: any other caller
/// gets [`BLOCK_ERR_DENIED`] - the filesystem server is the only task
/// allowed to touch the disk, the actual "supervised" part of moving
/// the filesystem out of the kernel.
pub const BLOCK_READ: u64 = 27;

/// `(lba, buf ptr)` -> `0` on success, or a `BLOCK_ERR_*` code. Writes
/// exactly one [`BLOCK_SECTOR_SIZE`]-byte sector from `buf`. Same
/// [`FSD_TASK`]-only gate as [`BLOCK_READ`].
pub const BLOCK_WRITE: u64 = 28;

/// The fixed sector size the `BLOCK_*` syscalls transfer, in bytes.
/// Matches the kernel's own `block::BlockDevice` (a hardcoded
/// `[u8; 512]` on both backends) and FAT32's `SECTOR_SIZE`.
pub const BLOCK_SECTOR_SIZE: u64 = 512;

/// The fixed task slot the filesystem server (`fsd/`) runs in - loaded
/// at boot alongside the shell, protected from `kill`/`exit` like
/// tasks 0/1. Clients hardcode this as the destination for filesystem
/// requests (see the `FSOP_*` protocol below); the `BLOCK_*` syscalls
/// accept only this task.
pub const FSD_TASK: u64 = 2;

/// `(offset, chunk ptr, chunk len)` -> `0` on success or
/// [`SPAWN_ERROR`]. Copies one chunk of a program image into the
/// kernel's fixed 128KB spawn staging buffer at `offset` - the feed
/// half of the two-step spawn (see [`SPAWN`]'s contract-change note).
/// Chunks are bounded by the same 512-byte per-syscall buffer cap as
/// everything else; offsets past the staging buffer are refused.
pub const SPAWN_STAGE: u64 = 30;

// ---------------------------------------------------------------------
// The filesystem server's request protocol (not syscalls) - messages
// sent to FSD_TASK, normally via MSG_CALL. A request is FS_REQUEST_LEN
// bytes: the op as a little-endian u64 at offset 0, then six
// little-endian u64 arguments at offsets 8, 16, ... 48 (pointers and
// lengths into the *caller's* memory - the server reads/writes client
// buffers directly, the same single-address-space trust model the old
// fs_* syscalls' pointer arguments had). The reply is a single
// little-endian u64 in the reply message's first 8 bytes, carrying
// exactly the old fs_* syscalls' return-value semantics: byte counts /
// real sizes / 0 on success, or NO_FS / an FS_ERR_* code / FS_ERROR
// from the reserved band. A call to an empty FSD_TASK slot fails with
// TASK_ERR_NO_SUCH_TASK at the MSG_CALL layer itself - the "no
// filesystem server this boot" case.
// ---------------------------------------------------------------------

/// A filesystem request message's exact size in bytes (op + 6 args).
pub const FS_REQUEST_LEN: u64 = 56;

/// args: `(path ptr, path len, buf ptr, buf len)` -> bytes written.
/// Formats each entry as `name\n`/`name/\n`, truncating rather than
/// erroring if the buffer is too small.
pub const FSOP_LIST_DIR: u64 = 1;
/// args: `(path ptr, path len, buf ptr, buf len)` -> the file's real
/// size (which may exceed the buffer's length - compare to detect
/// truncation).
pub const FSOP_READ_FILE: u64 = 2;
/// args: `(path ptr, path len, offset, buf ptr, buf len)` -> bytes
/// copied from the file starting at byte `offset` (`0` once the offset
/// is at/past the end of the file). The chunked-read primitive the
/// two-step [`SPAWN`] flow is built on - the one genuinely new
/// capability in the protocol, everything else is the old syscalls'
/// contracts verbatim.
pub const FSOP_READ_AT: u64 = 3;
/// args: `(path ptr, path len, data ptr, data len)` -> `0`. Creates a
/// file with exactly `data`'s contents, or fully overwrites (not
/// appends to) an existing file's contents. Empty `data` is valid
/// (truncate-to-empty).
pub const FSOP_WRITE_FILE: u64 = 4;
/// args: `(path ptr, path len)` -> `0`. Creates an empty directory.
pub const FSOP_MKDIR: u64 = 5;
/// args: `(path ptr, path len)` -> `0`. Removes an empty directory.
pub const FSOP_RMDIR: u64 = 6;
/// args: `(path ptr, path len)` -> `0`. Creates an empty file, or
/// succeeds as a no-op if a file already exists there.
pub const FSOP_TOUCH: u64 = 7;
/// args: `(path ptr, path len)` -> `0`. Removes a file (not a
/// directory - use [`FSOP_RMDIR`] for those).
pub const FSOP_RM: u64 = 8;
/// args: `(src ptr, src len, dst ptr, dst len)` -> `0`. Renames or
/// moves `src` to `dst`; `dst` must not already exist.
pub const FSOP_MV: u64 = 9;
/// no args -> `0` (mounted now), [`MOUNT_ALREADY`], or [`NO_FS`] (a
/// device is present but carries no mountable FAT32). The FS half of
/// the `mount` command - the device half is the [`MOUNT`] syscall,
/// which must succeed (or report already-installed) first.
pub const FSOP_MOUNT: u64 = 10;

/// The largest message [`MSG_SEND`] accepts, in bytes.
pub const MSG_MAX_LEN: u64 = 64;

/// [`WAIT`]'s answer when the waited task was killed rather than
/// exiting: `0x100`, one past the largest real exit status.
pub const TASK_KILLED_STATUS: u64 = 0x100;

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

// Task-management failure codes ([`KILL`]/[`FG`]), same reserved band.

/// The index is out of range or the slot holds no task.
pub const TASK_ERR_NO_SUCH_TASK: u64 = u64::MAX - 14;
/// Task 0 (the boot shell), task 1 (idle), and task 2 (the filesystem
/// server, [`FSD_TASK`]) are permanent - they can't be killed or
/// waited on, and idle can't be foregrounded.
pub const TASK_ERR_PROTECTED: u64 = u64::MAX - 15;

/// A [`WAIT`] cut short by Ctrl+C - the waited task keeps running,
/// nothing was collected. In the reserved band like every other
/// non-value result, though it's an outcome more than an error.
pub const WAIT_INTERRUPTED: u64 = u64::MAX - 16;

/// [`MOUNT`]: a filesystem was already mounted - the rescan still ran,
/// but nothing about the mounted filesystem changed.
pub const MOUNT_ALREADY: u64 = u64::MAX - 17;
/// [`MOUNT`]: no mountable USB storage device was found (none
/// attached, activation failed, or its filesystem isn't FAT32 - the
/// kernel log has the specific reason).
pub const MOUNT_NO_DEVICE: u64 = u64::MAX - 18;

/// A [`MSG_RECV`] cut short by Ctrl+C - nothing was received; same
/// escape-hatch semantics as [`WAIT_INTERRUPTED`].
pub const RECV_INTERRUPTED: u64 = u64::MAX - 19;
/// [`MSG_SEND`]: the destination's mailbox is full.
pub const MSG_ERR_FULL: u64 = u64::MAX - 20;
/// [`MSG_SEND`]: the message exceeds [`MSG_MAX_LEN`].
pub const MSG_ERR_TOO_BIG: u64 = u64::MAX - 21;
/// [`MSG_TRY_RECV`]: the mailbox is empty right now.
pub const NO_MSG: u64 = u64::MAX - 22;

/// `BLOCK_*`: no block device has been discovered/installed this boot
/// (nothing attached, or activation failed).
pub const BLOCK_ERR_NO_DEVICE: u64 = u64::MAX - 23;
/// `BLOCK_*`: the device-level read/write itself failed.
pub const BLOCK_ERR_IO: u64 = u64::MAX - 24;
/// `BLOCK_*`: the caller isn't [`FSD_TASK`] (or passed a bad buffer).
/// Only the filesystem server may touch the disk.
pub const BLOCK_ERR_DENIED: u64 = u64::MAX - 25;

/// Floor of the reserved error band (with headroom for future codes):
/// **any error-capable syscall's return value `>= FS_ERR_MIN` is an
/// error**, everything below is a real result. The predicate callers
/// actually need, since `fs_read_file`/`fs_list_dir` return arbitrary
/// byte counts on success and can't enumerate every non-error value in
/// a `match`. (Moved down from `MAX-15` when the `TASK_ERR_*` codes
/// consumed the original headroom - safe, since both sides of the ABI
/// import this from the same crate and no real success value
/// approaches it either way.)
pub const FS_ERR_MIN: u64 = u64::MAX - 31;

/// Generic failure sentinel for [`SPAWN`] - same bit pattern as
/// [`FS_ERROR`] (a bad ELF, no free task slot, and a disk read failure
/// all collapse to this one value, matching the `fs_*` syscalls' own
/// "one generic failure sentinel" precedent), given its own name since
/// it's a semantically distinct concept even though the value coincides.
/// [`SPAWN`] returns [`NO_FS`] separately when there's no mounted
/// filesystem at all, same as every `fs_*` syscall.
pub const SPAWN_ERROR: u64 = u64::MAX;

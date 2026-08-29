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

/// `(total staged length, stdout target, argv blob length)` -> **the new
/// task's slot index** on success (needed to wait on, send to, or kill what
/// was just started - the shell's pipeline flow does all three), [`SPAWN_ERROR`]
/// (bad argument), or a `SPAWN_ERR_*` code. Parses, relocates, and starts
/// the program previously fed into the kernel's staging buffer via
/// [`SPAWN_STAGE`], as a new task running *alongside* the caller - a
/// real `spawn`, not POSIX exec-replaces-current-process semantics;
/// the calling task is completely untouched. **Contract change with
/// the userland-filesystem milestone:** this used to take a path and
/// read the file itself, but the kernel no longer contains a
/// filesystem to read with - the caller reads the program (via the
/// filesystem server) and stages it chunk by chunk first. See
/// `tasks.rs`'s `spawn` for the mechanism and its real, deliberate
/// limits (a small fixed number of extra task slots). The **stdout
/// target** (arg1) is the task index the spawned program's output
/// should go to - normally [`CON_TASK`] (the console), but a shell
/// orchestrating a program-to-program pipe or an `exec … > file`
/// redirect sets it to itself so it can relay/capture the program's
/// output. The program reads it back via [`STDOUT_TARGET`]; a program
/// that ignores it (outputs straight to the console) is unaffected. The
/// **argv blob length** (arg2) attaches the argument vector previously
/// staged via [`ARGS_STAGE`] (`0` = no args); the child reads it via
/// [`GET_ARGC`]/[`GET_ARG`]. The **cwd length** (arg3) attaches the working
/// directory staged via [`CWD_STAGE`] (`0` = none); the child reads it via
/// [`GET_CWD`].
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
/// The reply buffer is a fixed [`MSG_MAX_LEN`] bytes - all 768 of
/// them (implied, not passed - the 4-argument syscall ABI is exactly
/// full), so a caller must always supply a full-size buffer. With direct
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

/// The fixed task slot the console server (`cond/`) runs in - loaded at
/// boot alongside the shell and the filesystem server, protected from
/// `kill`/`exit`/`wait` like tasks 0/1/2. The second component moved out
/// of the EL1 kernel: it owns the steady-state console (userland text
/// output flows to it over IPC), while the kernel keeps only a minimal
/// emergency console for boot/fault reporting. Clients hardcode this as
/// the destination for console-output requests.
pub const CON_TASK: u64 = 3;

/// The fixed task slot the network server (`netd/`) runs in - loaded at
/// boot alongside the shell, filesystem, and console servers, protected
/// from `kill`/`exit`/`wait` like tasks 0-3. It owns the network protocol
/// stack (ARP/IPv4/ICMP/...) in userland; the kernel keeps only the
/// DMA-owning virtio-net driver, reached by this task alone through the
/// gated [`NET_SEND`]/[`NET_RECV`] syscalls (the `BLOCK_*` -> [`FSD_TASK`]
/// pattern). Inserting it here shifted the spawnable slots up to 5-6.
pub const NET_TASK: u64 = 4;

/// The **account server**'s task slot: the fifth boot-loaded, supervised,
/// protected server. It owns `/etc/passwd` and `/etc/shadow` *as a policy
/// matter* - not exclusively (the files still live on `fsd`'s disk), but it is
/// the only component that will write a password on behalf of a caller who
/// could not write `/etc/shadow` themselves.
///
/// It exists because `/etc/shadow` is mode 0600 root: with the secrets out of
/// the world-readable file, a normal user changing their *own* password needs
/// something privileged to do it for them. A kernel setuid bit was the
/// alternative and doesn't fit here - the kernel doesn't read files, so "this
/// binary is setuid" would be asserted by the user-controlled shell that loads
/// it, which the capability model exists to distrust. A server can instead ask
/// the kernel *who is calling* ([`GET_ID`] on the message sender), which is
/// unforgeable, and decide for itself.
pub const ACCT_TASK: u64 = 5;

/// `(offset, chunk ptr, chunk len)` -> `0` on success or
/// [`SPAWN_ERROR`]. Copies one chunk of a program image into the
/// kernel's fixed 128KB spawn staging buffer at `offset` - the feed
/// half of the two-step spawn (see [`SPAWN`]'s contract-change note).
/// Chunks are bounded by the same 512-byte per-syscall buffer cap as
/// everything else; offsets past the staging buffer are refused.
pub const SPAWN_STAGE: u64 = 30;

/// `(grantee task, buf ptr, buf len, dir)` -> `0` on success, or
/// [`GRANT_ERR`]. Records, in the caller's own single per-task grant
/// slot, that task `grantee` may bulk-copy the `buf len`-byte buffer at
/// `buf ptr` - which must lie inside the caller's own EL0 region - in
/// direction `dir` (a mask of [`GRANT_READ`]/[`GRANT_WRITE`], from the
/// *granter's* point of view: `GRANT_READ` lets the grantee read *from*
/// this buffer, `GRANT_WRITE` lets it write *into* it). The first half
/// of the enforced capability-based bulk-transfer primitive that lifts
/// the [`FS_DATA_MAX`] per-op cap: the grant names an exact buffer, and
/// the kernel enforces the grantee can touch only those bytes, and only
/// while the granter is actively blocked in a [`MSG_CALL`] to it (see
/// [`SAFECOPY`]). Each task has exactly one grant slot; a new grant
/// overwrites the old, and a task's grant is cleared when it dies.
/// `buf len` is capped at [`SAFECOPY_MAX`].
pub const GRANT: u64 = 31;

/// `(client task, client offset, local buf ptr, len, dir)` -> `len` on
/// success, or [`SAFECOPY_ERR`]. Issued by a *server* to copy `len`
/// bytes between a client's granted buffer (at `client offset` within
/// it) and the server's own `local buf ptr`, in direction `dir`
/// ([`GRANT_READ`] = server reads client -> local; [`GRANT_WRITE`] =
/// server writes local -> client). Authorized only when **all** hold:
/// the client's grant is set with `grantee == caller` and a `dir` that
/// permits this direction; the client is *currently* blocked in a
/// [`MSG_CALL`] to the caller (a stale grant is inert - the client is
/// runnable, not blocked-calling-me, once its call has returned);
/// `client offset + len` stays within the granted buffer; and
/// `local buf ptr`/`len` lies inside the caller's own region. `len` is
/// capped at [`SAFECOPY_MAX`]. Note: takes five arguments - the arm
/// reads `frame`-saved registers, same as the multi-arg `fs_*` syscalls
/// once did. Unlike the `BLOCK_*` syscalls this is *not* gated to one
/// task: the grant plus the active call is the whole capability, so any
/// task acting as a server can use it.
pub const SAFECOPY: u64 = 32;

/// The largest buffer a single [`GRANT`]/[`SAFECOPY`] may name, in
/// bytes. This is the per-operation bulk-transfer chunk size; callers
/// wanting more loop, streaming one chunk at a time (the shell's `cat`
/// does exactly this). A 4x lift over the old [`FS_DATA_MAX`], chosen
/// to sit comfortably inside both a client's and the filesystem
/// server's fixed 8KB stacks alongside their other buffers - tunable if
/// that headroom ever changes (there's no userland heap or stack guard
/// page yet). The genuine ceiling on a *single* transfer stays
/// userland-memory-bound regardless; this primitive lifts the per-op
/// cap and lets streaming callers move arbitrarily much in total.
pub const SAFECOPY_MAX: u64 = 2048;

/// [`GRANT`]/[`SAFECOPY`] direction bit: the grantee may **read** from
/// the granted buffer (the granter is providing data - e.g. a bulk
/// file write, where the server reads the client's data out).
pub const GRANT_READ: u64 = 1;
/// [`GRANT`]/[`SAFECOPY`] direction bit: the grantee may **write** into
/// the granted buffer (the granter is receiving data - e.g. a bulk file
/// read, where the server writes the file's bytes into the client's
/// buffer).
pub const GRANT_WRITE: u64 = 2;

/// `(buf ptr, len)` -> `0` on success, or a reserved-band error. The
/// console server's byte-stream backend primitive: writes `len` bytes
/// from `buf` (in the server's own region) straight to the kernel's
/// console. **Gated to [`CON_TASK`]** alone, exactly like the `BLOCK_*`
/// syscalls are gated to [`FSD_TASK`] - ordinary tasks reach the console
/// only through the server (an `ninep_abi::NP_WRITE_FILE` message), while the
/// kernel's own `console::putc`/`println!` stay the emergency/boot path.
/// Bytes are written raw, no newline translation (same as [`PUTC`]).
/// `len` is bounded by the same per-buffer cap as the sector/staging
/// buffers.
pub const CON_WRITE: u64 = 33;

/// `(field)` -> the requested geometry value, or `0`. Lets the console
/// server discover which backend it has and how big the screen is at
/// startup. **Gated to [`CON_TASK`]**. Fields: [`CON_INFO_KIND`]
/// ([`CON_KIND_BYTESTREAM`] or [`CON_KIND_FRAMEBUFFER`]),
/// [`CON_INFO_COLS`], [`CON_INFO_ROWS`] (the framebuffer's character-cell
/// grid, `0` on a byte-stream backend).
pub const CON_INFO: u64 = 34;

/// `(glyphs ptr, count, col, row)` -> `0`, or a reserved-band error.
/// Plots `count` consecutive 8-byte glyph bitmaps (from the server's own
/// font, in its own region) at framebuffer character cells
/// `(col..col+count, row)`. **Gated to [`CON_TASK`]**. The dumb blit half
/// of the framebuffer backend - the server owns the font, the cursor,
/// wrap, and scroll *decisions*; this just puts the pixels on screen.
/// Cells past the last column/row are skipped.
pub const FB_BLIT: u64 = 35;

/// `(count)` -> `0`. Scrolls the framebuffer up by `count` character rows
/// (memmove within the framebuffer), blanking the newly-exposed bottom.
/// **Gated to [`CON_TASK`]**. The server decides *when* to scroll (its
/// cursor hit the bottom); this does the pixel move.
pub const FB_SCROLL: u64 = 36;

/// `()` -> `0`. Blanks the entire framebuffer. **Gated to [`CON_TASK`]**.
/// Used by the server's startup and its `clear`/ANSI-`2J` handling.
pub const FB_CLEAR: u64 = 37;

/// `()` -> the calling task's **stdout target** (the task index its
/// output should be sent to, set by whoever [`SPAWN`]ed it; [`CON_TASK`]
/// by default, and for boot-loaded tasks). A program routes its output
/// there: if it's [`CON_TASK`], via the console server (an `ninep_abi::NP_WRITE_FILE`
/// message); otherwise as a raw byte stream (1-to-[`MSG_MAX_LEN`]-byte
/// data messages, then one empty end-of-stream message - the same
/// convention the shell's `builtin | program` pipe already uses) to that
/// task, which relays or captures it. This is what makes a *task's own*
/// output capturable, enabling program-to-program pipes and
/// `exec … > file`. A program that doesn't call this simply always
/// outputs to the console, as before.
pub const STDOUT_TARGET: u64 = 38;

/// `()` -> the calling task's own slot index. A task's identity, which it
/// otherwise has no way to learn (every other task-aware syscall takes an
/// index rather than reporting the caller's). The shell needs it to
/// orchestrate a program-to-program pipe: it spawns the producer with a
/// stdout target of *itself* (this value) so the producer's output routes
/// back to the shell to be relayed on to the consumer - and the boot shell
/// (task 0) and a foreground-spawned shell (some higher slot) must each use
/// their real index, not a hardcoded 0.
pub const SELF: u64 = 39;

/// `(field)` -> the requested heap-area geometry, or `0`. Each program's
/// EL0 region carries a fixed **heap area** (a raw buffer between its code
/// and its stack guard page) that it can read/write via a `&mut [u8]` -
/// space far larger than the 16KB stack, for holding data a fixed stack
/// buffer can't (the shell backs its redirect/pipe capture with it, so
/// `cat big > file` captures the whole file). It is *not* a
/// `GlobalAlloc`-backed heap: `alloc`'s collections can't link under this
/// PIE loader (prebuilt `liballoc` has `R_AARCH64_ABS64` relocations a
/// `-pie` link rejects, and rebuilding it needs nightly `-Z build-std`),
/// so it's a raw buffer, not `Vec`/`Box`/`String`. Fields:
/// [`HEAP_INFO_BASE`] (the area's base address, `0` if the region is too
/// small to have one - e.g. the idle task) and [`HEAP_INFO_SIZE`] (its
/// length in bytes).
pub const HEAP_INFO: u64 = 40;

/// [`HEAP_INFO`] field: the heap area's base address (`0` if none).
pub const HEAP_INFO_BASE: u64 = 0;
/// [`HEAP_INFO`] field: the heap area's size in bytes.
pub const HEAP_INFO_SIZE: u64 = 1;

/// `(grantee, target)` -> `0` on success, [`MSG_ERR_DENIED`] otherwise.
/// Runtime capability delegation: grant `grantee` (a task slot) the right to
/// initiate IPC sends to `target` (a task slot) - a dynamic addition to
/// `grantee`'s static send-mask. The caller may only delegate a send
/// capability it *statically holds itself* (no transitive re-delegation),
/// which in practice confines this to the shell authorizing a pipeline's
/// producer to stream directly to its consumer (relay-free
/// `programA | programB`): only the shell holds the send-caps for the
/// spawnable slots. The delegation is cleared automatically when either the
/// grantee or the target dies, so no explicit revoke is needed for that
/// flow. Enforced at the `MSG_SEND`/`MSG_CALL` boundary (the kernel's
/// `may_send`).
pub const DELEGATE: u64 = 41;

/// `(frame ptr, frame len)` -> `0` on success, or an error sentinel.
/// Transmits one raw Ethernet frame through the kernel's virtio-net driver.
/// **Gated to [`NET_TASK`]** (the network server) alone - the DMA-owning
/// NIC driver stays in the kernel (no IOMMU), reached only by the one task
/// that owns the protocol stack, exactly like `BLOCK_*` -> [`FSD_TASK`].
pub const NET_SEND: u64 = 42;

/// `(buf ptr, buf len)` -> the received frame's length (its bytes copied
/// into `buf`, truncated to `buf len`), `NET_NO_FRAME` if none is waiting,
/// or an error sentinel. Non-blocking poll of the virtio-net receive ring.
/// Gated to [`NET_TASK`] like [`NET_SEND`].
pub const NET_RECV: u64 = 43;

/// `()` -> the NIC's 6-byte MAC address packed little-endian into a `u64`
/// (`mac[0]` in bits 0-7 ... `mac[5]` in bits 40-47), or [`NET_ERROR`].
/// The network server needs it to build the Ethernet source of every frame.
/// Gated to [`NET_TASK`] like [`NET_SEND`].
pub const NET_MAC: u64 = 44;

/// `()` -> blocks the caller until either a frame arrives on the NIC *or* a
/// message lands in its mailbox, then returns (the value is not meaningful -
/// the caller drains both sources itself via [`NET_RECV`] and
/// [`MSG_TRY_RECV`]). The async-receive primitive: it lets the network
/// server wait on network input and client IPC at once, rather than
/// busy-polling one and starving the other. A minimal poll/select, scoped to
/// exactly the two sources the network server multiplexes. Gated to
/// [`NET_TASK`] like [`NET_SEND`].
pub const NET_WAIT: u64 = 45;

/// `()` -> microseconds since boot, from the ARM generic timer's
/// free-running counter (`CNTPCT_EL0` scaled by `CNTFRQ_EL0`). A
/// high-resolution monotonic clock, unlike [`GET_TICKS`]'s 20 ms preemption
/// tick - needed anywhere a real elapsed duration matters (the network
/// server's TCP round-trip-time estimation is the first user; a fetch RTT
/// of tens of ms is 1-2 of `GET_TICKS`'s ticks, far too coarse to estimate
/// from). Not gated - a monotonic clock is a harmless read for any task.
/// Only meaningful as a *difference* of two readings; the absolute value is
/// "since boot," and it wraps after ~584,000 years, so callers need not
/// worry about it.
pub const MONOTONIC_US: u64 = 46;

/// `(blob pointer, blob length)` -> `0` on success, [`SPAWN_ERROR`] on a bad
/// range or an over-long blob. Stages an **argv blob** into a kernel buffer,
/// to be attached to the next [`SPAWN`] (its arg2 is the blob length; `0` =
/// no args). The blob encodes the argument vector as
/// `[argc: u32 LE]` then, for each arg, `[len: u32 LE][bytes]` - all
/// little-endian, read with unaligned loads. Bounded by [`ARGV_MAX`]. The
/// child reads it back via [`GET_ARGC`]/[`GET_ARG`]. Delivered kernel-side
/// (stored per-task, fetched by the child) exactly like [`STDOUT_TARGET`],
/// so a spawned program's start-up register/stack state is unchanged.
pub const ARGS_STAGE: u64 = 47;

/// `()` -> the number of arguments (argv entries) the current task was
/// spawned with. `0` for a task spawned with no argv (every boot-loaded task
/// - the shell, the servers - and any [`SPAWN`] with arg2 = 0).
pub const GET_ARGC: u64 = 48;

/// `(index, out pointer, out capacity)` -> the true length of argument
/// `index` (copying up to `out capacity` of its bytes into the buffer), or
/// [`NO_ARG`] if `index >= argc`. A zero-length real argument returns `0`
/// (distinct from [`NO_ARG`]).
pub const GET_ARG: u64 = 49;

/// [`GET_ARG`]'s return for an out-of-range index. `u64::MAX` is safely
/// distinct from any real argument length ([`ARGV_MAX`]-bounded).
pub const NO_ARG: u64 = u64::MAX;

/// Maximum size of a staged argv blob (and of the per-task argv store),
/// matching [`MAX_USER_LEN`](self)'s spirit - bounded today by the shell's
/// 128-byte input line, so 512 bytes of blob is ample.
pub const ARGV_MAX: u64 = 512;

/// `(cwd pointer, cwd length)` -> `0` on success, [`SPAWN_ERROR`] on a bad
/// range or an over-long path. Stages the **working directory** for the next
/// [`SPAWN`] (its arg3 is the cwd length; `0` = none), so a spawned command
/// inherits the shell's cwd and can resolve relative paths / default to the
/// current directory. Delivered kernel-side and fetched via [`GET_CWD`],
/// exactly like argv and the stdout target. Bounded by [`CWD_MAX`].
pub const CWD_STAGE: u64 = 50;

/// `(out pointer, out capacity)` -> the length of the current task's working
/// directory (copying up to `out capacity` of its bytes into the buffer), or
/// `0` if it was spawned without one (a boot-loaded task, or a [`SPAWN`] with
/// arg3 = 0).
pub const GET_CWD: u64 = 51;

/// Maximum length of a staged working-directory path (and the per-task cwd
/// store) - the shell's own `PATH_SIZE`.
pub const CWD_MAX: u64 = 128;

/// `(namespace-blob pointer, blob length)` -> `0` on success, [`SPAWN_ERROR`]
/// on a bad range or an over-long blob. Sets the **calling task's own
/// namespace** - the per-task table of `bind`ings that maps a path prefix to a
/// mount subtree (the Plan 9 namespace, cluster Phase 0). A child **inherits its
/// parent's namespace automatically at [`SPAWN`]** (the kernel copies it), so
/// `bind` needs only to update the caller's own view; and every task reads its
/// own via [`GET_NS`] to resolve paths the same way its parent did. An empty
/// namespace means identity-to-tree-0 (the default), so a task that never calls
/// this behaves exactly as before namespaces existed. Bounded by [`NS_MAX`].
pub const NS_SET: u64 = 52;

/// `(out pointer, out capacity)` -> the length of the current task's namespace
/// blob (copying up to `out capacity` of its bytes into the buffer), or `0` if
/// it has none (never called [`NS_SET`], and inherited an empty one). The blob
/// is a sequence of bindings, each
/// `[tree:u8][prefix_len:u8][target_len:u8][prefix bytes][target bytes]`.
pub const GET_NS: u64 = 53;

/// Maximum size of a namespace blob (and the per-task namespace store). A
/// handful of bindings of `CWD_MAX`-ish paths - 256 bytes is ample for now.
pub const NS_MAX: u64 = 256;

/// `(task index, out pointer, out capacity)` -> the length of task `index`'s
/// name (`argv[0]`, copying up to `out capacity` of its bytes into the buffer),
/// or `0` if the slot has no name (empty/unused). Read-only, like [`TASK_STATE`]:
/// the companion that turns `ps`'s slot list into named processes. Boot-loaded
/// tasks (idle/`fsd`/`cond`/`netd`/init) are named by the loader; spawned tasks
/// carry their `argv[0]`.
pub const TASK_NAME: u64 = 54;

/// Maximum name length [`TASK_NAME`] will report - a process name is short, and
/// this caps the `ps` buffer without pulling in the full `ARGV_MAX`.
pub const TASK_NAME_MAX: u64 = 32;

/// `(task index)` -> the exit status (`0..=255`) of a zombie task, **without
/// reaping it** (unlike [`WAIT`], which collects the status and frees the slot),
/// or [`TASK_NO_EXIT_CODE`] if the slot isn't a zombie. Lets `ps` show *why* a
/// zombie is holding its slot before anyone waits on it. A killed task is not a
/// zombie (it goes straight to unused, no status), so this never reports one.
pub const TASK_EXIT_CODE: u64 = 55;

/// [`TASK_EXIT_CODE`]'s answer for a slot that isn't a zombie (running, blocked,
/// unused, or out of range). `u64::MAX` is safely distinct from any real status,
/// which is masked to a byte (`0..=255`) at exit.
pub const TASK_NO_EXIT_CODE: u64 = u64::MAX;

/// `(mode)` -> does not return on success. Machine power control: `mode`
/// [`POWER_OFF`] powers the machine off (via PSCI `SYSTEM_OFF`, falling back to
/// a halt if PSCI is unavailable), [`POWER_HALT`] halts the CPU (masks
/// interrupts and parks forever). The kernel prints a final line and stops; an
/// unrecognized mode returns [`POWER_BAD_MODE`] instead. Not gated - a
/// single-user machine lets its shell turn itself off.
pub const POWER: u64 = 56;

/// `()` -> `0`. Voluntarily give up the rest of this task's time slice: the
/// kernel saves the (still-runnable) caller and switches to another runnable
/// task, preferring real work over the idle task, then resumes the caller on a
/// later switch. A cooperative yield - not a block, so nothing has to wake it.
/// Its purpose is to let a *consumer* run when a producer can't make progress:
/// `pipe_out`, on a momentarily-full consumer mailbox (`MSG_ERR_FULL`), yields
/// instead of busy-spinning until the next tick, so the consumer drains (or
/// exits early, like `head`) and the producer's retry then succeeds or fails
/// fast. Harmless if nothing else is runnable (it just carries on).
pub const YIELD: u64 = 57;

/// `(blob pointer, blob length)` -> `0` on success, [`SPAWN_ERROR`] on a bad
/// range or an over-long blob. Stages an **environment blob** to be attached
/// to the **next** [`SPAWN`] (a pending latch consumed by that spawn - unlike
/// argv/cwd, `SPAWN`'s four args are full, so the env has no length arg and is
/// latched instead; staging again before the next spawn replaces it). The blob
/// uses the *same* encoding as an argv blob - `[count: u32 LE]` then, for each
/// variable, `[len: u32 LE][bytes]` - where each entry's bytes are a
/// `NAME=VALUE` string. The child reads it via [`GET_ENVC`]/[`GET_ENV`] (and
/// `ulib::getenv`). Bounded by [`ENV_MAX`]; delivered kernel-side (stored
/// per-task) exactly like argv, so start-up register/stack state is unchanged.
pub const ENV_STAGE: u64 = 58;

/// `()` -> the number of environment variables the current task inherited (`0`
/// for a task spawned with none - every boot-loaded task, or a [`SPAWN`] with
/// no preceding [`ENV_STAGE`]).
pub const GET_ENVC: u64 = 59;

/// `(index, out pointer, out capacity)` -> the true length of environment
/// entry `index` as a `NAME=VALUE` string (copying up to `out capacity` bytes
/// into the buffer), or [`NO_ARG`] if `index >= envc`. Mirrors [`GET_ARG`];
/// `ulib::getenv` splits the `NAME=VALUE` on the first `=`. Note the out buffer
/// is validated like every user pointer, so its capacity must be `<=` the
/// syscall boundary's user-range cap (512) - read **one entry at a time** into
/// a small buffer, not one of [`ENV_MAX`] (which is the whole-blob store size).
pub const GET_ENV: u64 = 60;

/// Maximum size of a staged environment **blob** (and the per-task env store) -
/// *not* a per-entry read size (see [`GET_ENV`]). The shell's env holds up to
/// 16 vars of a name + a 128-byte value each; 2048 bytes covers a realistic
/// environment (a maximally-full one truncates, dropping trailing vars,
/// documented in `ulib`/the shell).
pub const ENV_MAX: u64 = 2048;

/// **Set the calling task's user identity** (uid + gid): `arg0` = uid,
/// `arg1` = gid (each a `u32` in the low bits). Returns `0` on success, or
/// [`SET_ID_DENIED`] if the caller isn't root.
///
/// The privilege model is deliberately minimal: **only a task whose current
/// uid is 0 (root) may change identity.** Root can *drop* to any user (the
/// mechanism a `login` will use), but a non-root task can't change its uid at
/// all - no escalation, and `su`-back-to-root needs authentication (the login
/// step, not this one). Children inherit their parent's identity at [`SPAWN`],
/// so a command runs as whoever started it.
///
/// The kernel owns this binding because it is the *only* component that
/// unforgeably knows an IPC message's real sender - so it is the root of trust
/// a permission check (a later step) builds on. Names, passwords, `/etc/passwd`,
/// and `/home` are entirely userland; the kernel only knows uid/gid are numbers.
pub const SET_ID: u64 = 61;

/// **Read a task's user identity**: `arg0` = task index. Returns the packed
/// `(gid << 32) | uid`, or [`GET_ID_ERR`] for an out-of-range task. A task reads
/// its own by passing its [`SELF`] index (ulib's `getuid`/`getgid` wrap that).
/// Backs `/bin/id` and, later, the permission check in `fsd` (which is handed
/// the sender's task index by the IPC layer).
pub const GET_ID: u64 = 62;

/// **Fill a buffer with hardware entropy**: `arg0` = out pointer, `arg1` = out
/// capacity (bounded like every user pointer). Returns the number of bytes
/// written, or [`RANDOM_UNAVAILABLE`] when this machine has no entropy device.
///
/// Backed by a virtio-rng device (`kernel/src/virtio_rng.rs`), which QEMU only
/// has when `-device virtio-rng-device` is passed and which Parallels and the
/// Pi do not expose at all. **"No entropy device" is therefore the ordinary
/// case, not an error**: callers are expected to degrade *loudly* (say the
/// value is weak) rather than fail, which is what `accounts::make_salt`'s
/// clock-derived fallback does.
///
/// The device may legitimately return fewer bytes than asked for, so a caller
/// needing exactly N must loop or treat a short read as unavailable - the
/// kernel does not pad, because a partly-random value presented as a full one
/// is the quiet weakness this device exists to remove.
pub const RANDOM: u64 = 63;

// --- The account server's request protocol (`ACCTOP_*`) -------------------
//
// One op today. The shape follows `NETOP_*` (a small numbered op set with
// inline payloads) rather than the `NP_*` file verbs, because this is not a
// filesystem: there is no path, and the interesting argument is *who is asking*,
// which the kernel supplies rather than the message.

/// **Change a password.** params: `(name len, old len, new len)`; payload:
/// `name || old_password || new_password`, in that order.
///
/// An empty `name` means "my own account", resolved from the caller's uid.
///
/// Policy, enforced by the server against [`GET_ID`] of the *sender* (which the
/// kernel binds, so it cannot be spoofed):
/// - **root** may set anyone's password and need not supply the old one.
/// - **anyone else** may change only their own, and must supply the correct
///   current password. That last check is what makes the server safe to expose
///   to every spawnable slot: holding the capability to *ask* is not the same as
///   being allowed.
///
/// Replies `0`, or one of the `ACCT_ERR_*` codes.
pub const ACCTOP_PASSWD: u64 = 1;

/// [`ACCTOP_PASSWD`]: the caller may not change that account's password (a
/// non-root caller naming someone else).
pub const ACCT_ERR_DENIED: u64 = u64::MAX - 1;
/// [`ACCTOP_PASSWD`]: no such account in `/etc/passwd`.
pub const ACCT_ERR_NO_USER: u64 = u64::MAX - 2;
/// [`ACCTOP_PASSWD`]: the supplied current password is wrong.
pub const ACCT_ERR_WRONG_PASSWORD: u64 = u64::MAX - 3;
/// [`ACCTOP_PASSWD`]: the account database could not be read or written.
pub const ACCT_ERR_IO: u64 = u64::MAX - 4;
/// [`ACCTOP_PASSWD`]: the request was malformed (lengths past the payload).
pub const ACCT_ERR_BAD_REQUEST: u64 = u64::MAX - 5;

/// Most supplementary groups a task may carry, on top of its primary gid.
/// Bounds the per-task kernel array; a user in more groups than this keeps the
/// first [`MAX_SUPP_GROUPS`] (`login` fills them in `/etc/group` order).
pub const MAX_SUPP_GROUPS: usize = 8;

/// **Set the calling task's supplementary groups**: `arg0` = pointer to an array
/// of `u32` gids, `arg1` = how many (capped at [`MAX_SUPP_GROUPS`]). Returns `0`,
/// or [`SET_GROUPS_DENIED`] for a non-root caller.
///
/// **Root only**, like POSIX `setgroups`, and for the same reason: group
/// membership is a permission grant, so a task that could add its own groups
/// could grant itself access to any group-readable file. The trusted shell calls
/// this *before* dropping to the user (while it is still root) and clears it
/// again on logout; children inherit the list at [`SPAWN`] exactly as they
/// inherit uid/gid.
///
/// The primary gid stays in the packed [`SET_ID`] word - this is strictly the
/// *additional* list, so `fsd`'s group check is "owner gid == primary gid, or
/// owner gid is in this list".
pub const SET_GROUPS: u64 = 64;

/// **Read a task's supplementary groups**: `arg0` = task index, `arg1` = out
/// pointer, `arg2` = out capacity in **gids** (not bytes). Copies up to that
/// many `u32` gids and returns the task's true count, or [`GET_ID_ERR`] for an
/// out-of-range task.
///
/// Ungated like [`GET_ID`] - group membership is not a secret, and `fsd` needs
/// the *sender's* list to decide the group triad on every op.
pub const GET_GROUPS: u64 = 65;

/// [`SET_GROUPS`] refused: the calling task isn't root.
pub const SET_GROUPS_DENIED: u64 = u64::MAX;

/// [`RANDOM`] on a machine with no entropy device (or with an invalid output
/// buffer). Distinct from `0`, which would mean "a device answered, with
/// nothing" - the caller should treat this one as "there is no RNG here".
pub const RANDOM_UNAVAILABLE: u64 = u64::MAX;

/// [`SET_ID`] refused: the calling task isn't root (uid 0). Distinct from the
/// `0` success return.
pub const SET_ID_DENIED: u64 = u64::MAX;

/// [`GET_ID`] with an out-of-range task index. No real packed identity reaches
/// `u64::MAX` (that would be uid = gid = `0xFFFF_FFFF`), so it's an unambiguous
/// sentinel.
pub const GET_ID_ERR: u64 = u64::MAX;

/// [`POWER`] mode: power the machine off.
pub const POWER_OFF: u64 = 0;
/// [`POWER`] mode: halt the CPU (stop, without cutting power).
pub const POWER_HALT: u64 = 1;

/// [`POWER`]'s return for an unrecognized `mode` (the only case it returns at
/// all). Distinct from `0` so a caller can tell "bad mode" from a value.
pub const POWER_BAD_MODE: u64 = u64::MAX;

/// [`NET_RECV`] returned when no frame is currently available (a poll that
/// found the receive ring empty) - distinct from a real length (`u64::MAX`
/// is out of any frame's range) and from the error sentinel.
pub const NET_NO_FRAME: u64 = u64::MAX;

/// [`NET_SEND`]/[`NET_RECV`] error sentinel: no NIC installed this boot, the
/// caller isn't [`NET_TASK`], or a bad buffer. Distinct from [`NET_NO_FRAME`].
pub const NET_ERROR: u64 = u64::MAX - 1;

// The `NETOP_*` request protocol clients speak to the network server
// (`netd`, [`NET_TASK`]) over `MSG_CALL`, mirroring the `FSOP_*` shape. A
// request is an op (LE u64) plus op-specific args; the reply is a single
// LE u64 status. The whole protocol stack (ARP/IPv4/ICMP) lives in `netd`;
// clients just name what they want.
//
/// [`NETOP_PING`] request: `[op: u64][target IPv4: u64]` where the target is
/// its 4 octets packed little-endian (`a | b<<8 | c<<16 | d<<24`). `netd`
/// ARP-resolves the target, sends an ICMP echo request, waits for the reply,
/// and replies with one of the `NET_PING_*` status codes below.
pub const NETOP_PING: u64 = 1;

/// [`NETOP_PING`] reply: an ICMP echo reply came back - the host is up.
pub const NET_PING_OK: u64 = 0;
/// [`NETOP_PING`] reply: the target was resolved but no echo reply arrived
/// before the deadline (host down, or dropping ICMP).
pub const NET_PING_TIMEOUT: u64 = 1;
/// [`NETOP_PING`] reply: ARP resolution failed - no host answered for that
/// IP (nothing at that address on the local network).
pub const NET_PING_NO_ARP: u64 = 2;
/// [`NETOP_PING`] reply: no NIC is installed this boot, so nothing can be
/// sent at all.
pub const NET_PING_NO_NIC: u64 = 3;

/// [`NETOP_RESOLVE`] request: `[op: u64][hostname bytes...]` (the hostname
/// fills the rest of the message, no length prefix - `netd` takes it from
/// the message length). `netd` sends a **DNS A-record query over UDP** to
/// the QEMU user-net DNS server (`10.0.2.3`), waits for the response, and
/// replies `[status: u64][ipv4: u64]` where a `NET_RESOLVE_OK` status means
/// the four resolved octets are packed little-endian in the second word.
/// The first end-to-end UDP application in the stack.
pub const NETOP_RESOLVE: u64 = 2;

/// [`NETOP_RESOLVE`] reply: resolved - the IPv4 is in the reply's second u64.
pub const NET_RESOLVE_OK: u64 = 0;
/// [`NETOP_RESOLVE`] reply: no DNS response before the deadline.
pub const NET_RESOLVE_TIMEOUT: u64 = 1;
/// [`NETOP_RESOLVE`] reply: the server answered but with no A record (the
/// name doesn't resolve, or an encoding this minimal parser can't read).
pub const NET_RESOLVE_NXDOMAIN: u64 = 2;
/// [`NETOP_RESOLVE`] reply: no NIC this boot.
pub const NET_RESOLVE_NO_NIC: u64 = 3;

/// [`NETOP_FETCH`] request: `[op: u64][hostname bytes...]` (same shape as
/// [`NETOP_RESOLVE`]). `netd` resolves the hostname, opens a **client TCP
/// connection** to it on port 80, sends a minimal `GET / HTTP/1.0` request,
/// reads the response, and closes. The reply is `[status: u64][total: u64]
/// [response bytes...]` where `total` is the full response length and the
/// bytes are the response truncated to what fits one message. The first TCP
/// application in the stack.
pub const NETOP_FETCH: u64 = 3;

/// [`NETOP_FETCH`] reply: connected, sent, and read a response.
pub const NET_FETCH_OK: u64 = 0;
/// [`NETOP_FETCH`] reply: no reply progressed before the deadline (SYN or a
/// data segment was never answered).
pub const NET_FETCH_TIMEOUT: u64 = 1;
/// [`NETOP_FETCH`] reply: the peer refused the connection (a TCP RST).
pub const NET_FETCH_REFUSED: u64 = 2;
/// [`NETOP_FETCH`] reply: the hostname didn't resolve, or the next hop
/// (host or gateway) didn't answer ARP.
pub const NET_FETCH_NO_ROUTE: u64 = 3;
/// [`NETOP_FETCH`] reply: no NIC this boot.
pub const NET_FETCH_NO_NIC: u64 = 4;

/// [`NETOP_RMOUNT`] request (cluster Phase 1c - the remote-mount client):
/// `[op: u64][ip:4][port:2 (LE)][pad:2][NP message...]`. The endpoint is at
/// bytes 8..16 (an IPv4 and a big-... no, a *little*-endian TCP port, padded to
/// a `u64`), and the rest of the message is a verbatim `ninep-abi` NP request
/// (its 48-byte header + payload) that `netd` frames onto a TCP connection to
/// `ip:port` (a machine's 9P export listener, [`ninep_abi::NP_NET_PORT`]), does
/// one request/reply round trip against, and returns the reply body from. The
/// reply to the client is the NP reply body verbatim - `[status: u64][data...]`,
/// exactly the shape a local `MSG_CALL` to `fsd` returns - so the fs-helper
/// layer routes a remote resolution here and is otherwise unchanged. Bounded by
/// [`MSG_MAX_LEN`] both ways, so a remote read/write chunk is inline and small
/// (the client loops, as `cat` already does). Gated by no capability beyond the
/// [`NET_TASK`] send the shell already delegates at spawn.
pub const NETOP_RMOUNT: u64 = 4;

/// [`NETOP_RMOUNT`] byte offset of the endpoint (IPv4 + port) in the request.
pub const NETOP_RMOUNT_ENDPOINT: usize = 8;
/// [`NETOP_RMOUNT`] byte offset where the embedded NP message begins.
pub const NETOP_RMOUNT_MSG: usize = 16;

/// [`NETOP_RUN`] request (cluster Phase 4a - remote execution, the Plan 9 `cpu`
/// model): `[op: u64][ip:4][port:2 LE][pad:2][command line...]` - the same
/// endpoint layout as [`NETOP_RMOUNT`] (reuse [`NETOP_RMOUNT_ENDPOINT`] /
/// [`NETOP_RMOUNT_MSG`]), with the command line (program name + space-separated
/// args) where the NP message would be. `netd` opens a connection to the remote
/// export, sends an `ninep_abi::NP_RUN` frame, and collects the spawned command's
/// output; the reply to the client is the **first** [`MSG_MAX_LEN`]-byte chunk of
/// that output, and the shell pulls the rest with [`NETOP_RUN_MORE`]. `netd`
/// holds the collected output (bounded ~2 KB by the remote's buffer) between the
/// pull calls. The shell's `cpu <host:port> <command>` builtin.
pub const NETOP_RUN: u64 = 5;

/// [`NETOP_RUN`] follow-up: pull the next [`MSG_MAX_LEN`]-byte chunk of the last
/// run's output (request is just `[op: u64]`; only the task that issued the
/// `NETOP_RUN` may pull its output). The reply is the next chunk, or **empty**
/// once the output is exhausted (end of stream). This is the shell-side chunked
/// delivery that lifts `cpu` output past one message; truly unbounded streaming
/// (the remote sending as it produces) is a later refinement - see
/// `docs/roadmap-cluster.md`.
pub const NETOP_RUN_MORE: u64 = 6;

/// [`CON_INFO`] field: the backend kind ([`CON_KIND_*`]).
pub const CON_INFO_KIND: u64 = 0;
/// [`CON_INFO`] field: framebuffer columns (character cells wide).
pub const CON_INFO_COLS: u64 = 1;
/// [`CON_INFO`] field: framebuffer rows (character cells tall).
pub const CON_INFO_ROWS: u64 = 2;

/// [`CON_INFO_KIND`]: no framebuffer - the server forwards text to the
/// kernel's byte-stream console via [`CON_WRITE`] (QEMU's UART path).
pub const CON_KIND_BYTESTREAM: u64 = 0;
/// [`CON_INFO_KIND`]: a framebuffer is present - the server renders
/// glyphs itself via [`FB_BLIT`]/[`FB_SCROLL`]/[`FB_CLEAR`] (Parallels,
/// QEMU `ramfb`).
pub const CON_KIND_FRAMEBUFFER: u64 = 1;

// ---------------------------------------------------------------------
// The filesystem server's request protocol (not syscalls) - messages
// sent to FSD_TASK, normally via MSG_CALL. **v2, fully self-contained**
// (v1 passed raw pointers into the caller's memory, which per-task
// page tables made impossible for the server to dereference): a
// request is a header - the op as a little-endian u64 at offset 0,
// then four little-endian u64 parameters at offsets 8/16/24/32 - 
// followed by the inline payload (path bytes, then data bytes for the
// ops that carry data) starting at FS_REQ_PAYLOAD offset. The reply is
// a status u64 at offset 0 (carrying exactly the old fs_* syscalls'
// return-value semantics: byte counts / real sizes / 0, or NO_FS / an
// FS_ERR_* code / FS_ERROR from the reserved band) followed by the
// inline result payload (directory listings, file data) at offset 8.
// Everything is copied task-to-task by the kernel's message machinery;
// no pointer ever crosses a task boundary. Per-operation payloads are
// capped at FS_DATA_MAX. A call to an empty FSD_TASK slot fails at the
// MSG_CALL layer itself with TASK_ERR_NO_SUCH_TASK - the "no
// filesystem server this boot" case.
// ---------------------------------------------------------------------

/// A request's header size: op + four parameter u64s. The inline
/// payload starts here.
pub const FS_REQ_PAYLOAD: u64 = 40;

/// A reply's header size: the status u64. The inline result payload
/// starts here.
pub const FS_REPLY_PAYLOAD: u64 = 8;

/// The per-operation payload cap, in bytes - one path, one data
/// buffer, or one result may each be at most this long (the historical
/// per-syscall buffer cap, kept as the protocol's own).
pub const FS_DATA_MAX: u64 = 512;

/// params: `(path len, want len)`; payload: path -> status = bytes of
/// listing written to the reply payload (capped at `want len`).
/// Formats each entry as `name\n`/`name/\n`, truncating rather than
/// erroring if the cap is too small.
pub const FSOP_LIST_DIR: u64 = 1;
/// params: `(path len, want len)`; payload: path -> status = the
/// file's *real* size (compare against `want len` to detect
/// truncation); reply payload = the first `min(size, want)` bytes.
pub const FSOP_READ_FILE: u64 = 2;
/// params: `(path len, offset, want len)`; payload: path -> status =
/// bytes copied from the file starting at byte `offset` (`0` once the
/// offset is at/past the end); reply payload = those bytes. The
/// chunked-read primitive the two-step [`SPAWN`] flow is built on.
pub const FSOP_READ_AT: u64 = 3;
/// params: `(path len, data len)`; payload: path ++ data -> status =
/// `0`. Creates a file with exactly the data's contents, or fully
/// overwrites an existing file. Zero-length data is valid
/// (truncate-to-empty).
pub const FSOP_WRITE_FILE: u64 = 4;
/// params: `(path len)`; payload: path -> `0`. Creates an empty
/// directory.
pub const FSOP_MKDIR: u64 = 5;
/// params: `(path len)`; payload: path -> `0`. Removes an empty
/// directory.
pub const FSOP_RMDIR: u64 = 6;
/// params: `(path len)`; payload: path -> `0`. Creates an empty file,
/// or succeeds as a no-op if a file already exists there.
pub const FSOP_TOUCH: u64 = 7;
/// params: `(path len)`; payload: path -> `0`. Removes a file (not a
/// directory - use [`FSOP_RMDIR`] for those).
pub const FSOP_RM: u64 = 8;
/// params: `(src len, dst len)`; payload: src ++ dst -> `0`. Renames
/// or moves `src` to `dst`; `dst` must not already exist.
pub const FSOP_MV: u64 = 9;
/// no params -> `0` (mounted now), [`MOUNT_ALREADY`], or [`NO_FS`] (a
/// device is present but carries no mountable FAT32). The FS half of
/// the `mount` command - the device half is the [`MOUNT`] syscall,
/// which must succeed (or report already-installed) first.
pub const FSOP_MOUNT: u64 = 10;

/// params: `(path len, offset, want)`; payload: path. The bulk sibling
/// of [`FSOP_READ_AT`]: instead of returning the bytes inline (capped
/// at [`FS_DATA_MAX`]), the server reads up to `min(want, `[`SAFECOPY_MAX`]`)`
/// bytes from byte `offset` and [`SAFECOPY`]s them straight into the
/// client's granted buffer (the client must [`GRANT`] a [`GRANT_WRITE`]
/// buffer of at least that size to [`FSD_TASK`] first; `want` is
/// normally the granted buffer's length, so a client can use a buffer
/// smaller than [`SAFECOPY_MAX`] without the server overrunning the
/// grant). status = bytes delivered this chunk (`0` once `offset` is
/// at/past the end). Loop with a rising `offset` to stream a file of
/// any size - the shell's `cat` does exactly this.
pub const FSOP_READ_BULK: u64 = 11;
/// params: `(path len, data len)`; payload: path. The bulk sibling of
/// [`FSOP_WRITE_FILE`]: the data does *not* travel inline. The client
/// [`GRANT`]s a [`GRANT_READ`] buffer holding `data len` (up to
/// [`SAFECOPY_MAX`]) bytes to [`FSD_TASK`]; the server [`SAFECOPY`]s it
/// in, then creates/overwrites the file with exactly those bytes.
/// status = `0` on success, or an `FS_ERR_*` code. Raises the write cap
/// from [`FS_DATA_MAX`] to [`SAFECOPY_MAX`].
pub const FSOP_WRITE_BULK: u64 = 12;
/// params: `(path len, offset, data len)`; payload: path. Writes `data`
/// (granted via [`GRANT_READ`], up to [`SAFECOPY_MAX`]) at byte `offset`
/// within the file, **extending it without rewriting the bytes before
/// `offset`** - the FAT32 offset-write primitive behind streaming `cp`
/// and unbounded `>>`. The client [`GRANT`]s a [`GRANT_READ`] buffer to
/// [`FSD_TASK`] first, exactly like [`FSOP_WRITE_BULK`]. status = `0` on
/// success, or an `FS_ERR_*` code (a write past the current end of file
/// is refused - no sparse gaps). Loop with a rising `offset` to write a
/// file of any size, one chunk at a time.
pub const FSOP_WRITE_AT: u64 = 13;

/// no params -> status `0` with an inline info block when a filesystem is
/// mounted, or [`NO_FS`] when nothing is (a bare status reply then). The
/// query behind `mount` with no argument (disk-tools arc, milestone 1).
/// On success the reply payload (from [`FS_REPLY_PAYLOAD`]) is:
/// `partition_lba: u64` (the volume's first sector), then
/// `capacity_sectors: u64` (the whole disk's 512-byte-sector count), then
/// the format name as ASCII bytes running to the end of the reply
/// (`"FAT32"`/`"exFAT"`/`"ext2"`). The client formats these itself.
pub const FSOP_MOUNT_INFO: u64 = 14;
/// no params -> status `0` (was mounted, now dropped) or [`NO_FS`]
/// (nothing was mounted). Drops the server's mounted filesystem so the
/// disk can be reformatted or a different volume mounted (disk-tools arc,
/// milestone 1). The device the kernel holds is untouched - a subsequent
/// [`FSOP_MOUNT`] re-probes and re-mounts it.
pub const FSOP_UNMOUNT: u64 = 15;

/// params: `(sectors,)` -> status `0` (wiped) / [`MOUNT_ALREADY`] (refused:
/// a filesystem is mounted - `unmount` first) / [`MOUNT_NO_DEVICE`] (no block
/// device) / [`FS_ERR_IO`]. Zeroes the disk's first `sectors` 512-byte sectors
/// (clamped to the disk's capacity; `0` means the milestone-2 default,
/// [`ERASE_DEFAULT_SECTORS`]) - enough to destroy the MBR/GPT partition tables
/// and any filesystem metadata living near the start of the disk, so a
/// subsequent [`FSOP_PARTITION`] starts from a clean slate. Refused while a
/// filesystem is mounted (the mount would be reading stale structures). Disk
/// management arc, milestone 2.
pub const FSOP_ERASE: u64 = 16;
/// params: `(type byte,)` -> status `0` / [`MOUNT_ALREADY`] (refused: mounted)
/// / [`MOUNT_NO_DEVICE`] / [`FS_ERR_DISK_FULL`] (disk too small) / [`FS_ERR_IO`].
/// Writes a fresh **MBR** with a single primary partition spanning the disk
/// from LBA [`PARTITION_START_LBA`] to the end, of the given MBR partition
/// **type byte** (e.g. `0x0C` FAT32-LBA, `0x07` exFAT/NTFS, `0x83` Linux) - a
/// type byte of `0` defaults to `0x0C`. Only LBA 0 (the partition table) is
/// written; the partition's contents are left as-is for [`FSOP_FORMAT`] (a
/// later milestone) to lay a filesystem into. GPT is a later step. Refused
/// while mounted. Disk management arc, milestone 2.
pub const FSOP_PARTITION: u64 = 17;

/// params: `(fstype,)` -> status `0` / [`MOUNT_ALREADY`] (refused: mounted)
/// / [`MOUNT_NO_DEVICE`] / [`FS_ERR_NOT_FOUND`] (no partition table - run
/// [`FSOP_PARTITION`] first) / [`FS_ERR_DISK_FULL`] (partition too small for
/// the format) / [`FS_ERROR`] (unsupported `fstype`) / [`FS_ERR_IO`]. Lays a
/// fresh filesystem of type `fstype` ([`FMT_FAT32`]/[`FMT_EXFAT`]/[`FMT_EXT2`])
/// into the disk's first MBR partition - the inverse of the read/write engines
/// (mkfs). Writes the on-disk metadata (for FAT32: the boot sector + FSInfo +
/// their backups, both FATs with their reserved entries, and a zeroed root
/// directory); the partition must already exist ([`FSOP_PARTITION`]). Refused
/// while mounted. Disk management arc, milestone 3. FAT32 first; exFAT/ext2
/// later steps (an unsupported `fstype` returns [`FS_ERROR`]).
pub const FSOP_FORMAT: u64 = 18;

/// `(partition index)` -> the **tree id** (0..) the mount was placed in, or an
/// error `>= FS_ERR_MIN`: [`NO_FS`] (no such partition / it mounts as no known
/// format), [`MOUNT_ALREADY`] (no free mount slot). Mounts the disk's
/// `index`-th partition (from the same MBR/GPT discovery the boot auto-mount
/// uses) into a fresh mount slot, so several filesystems can be mounted at once
/// (cluster Phase 0 multi-mount). The caller `bind`s a namespace prefix to the
/// returned tree so paths under it resolve to this mount. Unlike [`FSOP_MOUNT`]
/// (which mounts the first validating partition at tree 0), this selects a
/// specific partition and returns where it landed. A small tree id (0..3) is
/// always well below [`FS_ERR_MIN`], so it can't be mistaken for an error.
pub const FSOP_MOUNT_AT: u64 = 19;

/// [`FSOP_FORMAT`] `fstype` selectors.
pub const FMT_FAT32: u64 = 0;
/// exFAT format (a later milestone-3 step; currently returns [`FS_ERROR`]).
pub const FMT_EXFAT: u64 = 1;
/// ext2 format (a later milestone-3 step; currently returns [`FS_ERROR`]).
pub const FMT_EXT2: u64 = 2;

/// [`FSOP_ERASE`]'s default wipe length when the request passes `0`: 2048
/// sectors (1 MiB at 512 B/sector), the conventional wipe span - it covers
/// the MBR (LBA 0), a GPT primary header + entry array (LBA 1..33), and any
/// filesystem superblock/boot sector at a 1 MiB-aligned partition start.
pub const ERASE_DEFAULT_SECTORS: u64 = 2048;
/// The first sector [`FSOP_PARTITION`] gives its single partition: LBA 2048
/// (1 MiB alignment, the near-universal modern convention that keeps the
/// filesystem aligned to erase blocks / RAID stripes).
pub const PARTITION_START_LBA: u64 = 2048;

// ---------------------------------------------------------------------
// The console server (CON_TASK) no longer has a bespoke protocol. Writing
// the console is a write to the console "file": an `ninep_abi::NP_WRITE_FILE`
// message (inline text as the data), sent to CON_TASK via MSG_CALL - the same
// uniform verb set fsd speaks (cluster Phase 0). cond serves only the console,
// so it ignores the tree/path and renders the data (see the shell's/ulib's
// `con_write`). A call to an empty CON_TASK slot fails at the MSG_CALL layer
// with TASK_ERR_NO_SUCH_TASK - the "no console server this boot" case, which
// clients handle by falling back to the kernel console via PUTC. The old
// `DSPOP_WRITE` op was retired here when cond adopted the verb set.
// ---------------------------------------------------------------------

/// A reserved *sender* sentinel the kernel's supervisor uses to inject a
/// liveness **ping** into a supervised server's mailbox, and the *dest*
/// a server's reply to that ping carries (which the kernel intercepts as
/// the ack - see [`SYSOP_PING`]). Fits `Message.sender`'s `u8` and sits
/// clear of every real task index (`0..NUM_TASKS`), so it can never be
/// mistaken for one. Not a task: an ordinary [`MSG_SEND`] to it never
/// reaches a mailbox - the kernel treats `dest == KERNEL_SENDER` as "this
/// task is acking a supervisor ping" and returns `0`. A non-server, or a
/// server with no ping outstanding, sending to it is a harmless no-op
/// (the same single-address-space trust model as every other message).
pub const KERNEL_SENDER: u64 = 0xFE;

/// The op the supervisor's liveness ping carries in its message header
/// (offset 0, the same slot `FSOP_*`/`NP_*` verbs occupy). Deliberately well
/// clear of every real server op (which start at 1), so a server that
/// *does* inspect it can fast-path it - though none needs to: a server
/// replies harmlessly to any unknown op, and that reply, addressed to
/// [`KERNEL_SENDER`], is itself the ack. The active half of server
/// supervision (`kernel/src/supervisor.rs`), catching a server stuck
/// `Blocked` forever - which the passive heartbeat (a server observed
/// continuously `Runnable`) structurally can't see.
pub const SYSOP_PING: u64 = 0xFFFF;

/// The largest message [`MSG_SEND`] accepts, in bytes. Raised from the
/// original 64 when per-task page tables landed: the filesystem
/// protocol's requests/replies became fully self-contained (payloads
/// travel *inside* the message, kernel-copied task-to-task) once the
/// server could no longer dereference pointers into a client's
/// now-isolated memory - see the `FSOP_*` protocol section. Sized so
/// one request holds a full path (up to [`FS_DATA_MAX`]... in practice
/// paths are far shorter) plus a full data payload, and one reply
/// holds the status plus a full result.
pub const MSG_MAX_LEN: u64 = 768;

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
/// Cluster authentication failed on a remote (9P-over-TCP) operation: the
/// export rejected the request's client-nonce MAC (wrong/missing cluster
/// key), or refused a remote-run because the exporter has `NP_RUN` disabled
/// (the no-exec lever). Surfaces as a remote-mount / `cpu` failure so the
/// shell can say "authentication failed" distinctly from [`NO_FS`] (an
/// unreachable peer). In the `FS_ERR_*` band, so `>= FS_ERR_MIN` treats it as
/// an error like every other. See `docs/roadmap-cluster.md`'s security phase.
/// (`MAX - 30`: the one free slot left in the `FS_ERR_*` band between
/// [`FS_ERR_READ_ONLY`] at `MAX - 29` and [`FS_ERR_MIN`] at `MAX - 31` - the
/// low offsets are already taken by the `SPAWN_ERR_*`/`MSG_ERR_*` codes.)
pub const FS_ERR_AUTH: u64 = u64::MAX - 30;

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
/// Task 0 (the boot shell), task 1 (idle), task 2 (the filesystem
/// server, [`FSD_TASK`]), and task 3 (the console server, [`CON_TASK`])
/// are permanent - they can't be killed or waited on, and idle can't be
/// foregrounded.
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

/// [`GRANT`]: a bad argument - grantee out of range/nonexistent, an
/// invalid direction, a zero/oversized (`> `[`SAFECOPY_MAX`]) length,
/// or a buffer that isn't inside the caller's own region.
pub const GRANT_ERR: u64 = u64::MAX - 26;
/// [`SAFECOPY`]: the copy was refused - no matching grant, the client
/// isn't blocked in a call to the caller, a direction the grant doesn't
/// permit, an out-of-bounds `client offset`/`len`, or a bad local
/// buffer.
pub const SAFECOPY_ERR: u64 = u64::MAX - 27;

/// [`MSG_SEND`]/[`MSG_CALL`]: the send was refused by the IPC capability
/// policy - the calling task's per-slot send-mask doesn't permit reaching
/// the destination (and it isn't an authorized reply to a pending call).
/// The topological half of isolation: a task can only initiate IPC to the
/// endpoints its capabilities allow. In normal operation no legitimate
/// flow hits this (the policy is derived from the real call graph); it's
/// the enforcement backstop against an unauthorized send.
pub const MSG_ERR_DENIED: u64 = u64::MAX - 28;

/// A write op (`FSOP_MKDIR`/`RMDIR`/`TOUCH`/`RM`/`MV`/`WRITE_*`) was
/// refused because the mounted filesystem is read-only. Today only the
/// exFAT arm returns it (read-only support - writes are a later
/// milestone, the same read-first/write-later split FAT32 followed);
/// FAT32 is fully read-write and never does.
pub const FS_ERR_READ_ONLY: u64 = u64::MAX - 29;

/// A metadata write (`FSOP_CHMOD`/`FSOP_CHOWN`) was refused because the
/// mounted filesystem can't model the attribute. Ownership + permission
/// bits are an ext2 concept; FAT32/exFAT/`/proc` return this rather than
/// silently pretending to succeed - the same honest per-filesystem
/// degradation the read side (`stat`'s `mode_valid` byte) already uses.
pub const FS_ERR_NOT_SUPPORTED: u64 = u64::MAX - 31;

/// **Permission denied**: the caller's uid/gid isn't allowed the requested
/// access to a file by its owner + mode bits. Returned by `fsd`'s permission
/// enforcement (the users/permissions arc, step 3) - ext2 only (the one arm
/// that models an owner/mode; FAT32/exFAT/`/proc` stay unrestricted). Root
/// (uid 0) bypasses the check, so this is only ever a *non-root* refusal.
pub const FS_ERR_PERM: u64 = u64::MAX - 32;

/// Floor of the reserved error band (with headroom for future codes):
/// **any error-capable syscall's return value `>= FS_ERR_MIN` is an
/// error**, everything below is a real result. The predicate callers
/// actually need, since `fs_read_file`/`fs_list_dir` return arbitrary
/// byte counts on success and can't enumerate every non-error value in
/// a `match`. (Moved down from `MAX-15` when the `TASK_ERR_*` codes
/// consumed the original headroom, then to `MAX-32` for
/// [`FS_ERR_NOT_SUPPORTED`], then `MAX-33` for [`FS_ERR_PERM`] - safe, since
/// both sides of the ABI import this from the same crate and no real success
/// value approaches it either way.)
pub const FS_ERR_MIN: u64 = u64::MAX - 33;

/// Generic failure sentinel for [`SPAWN`] - same bit pattern as
/// [`FS_ERROR`] (a bad ELF, no free task slot, and a disk read failure
/// all collapse to this one value, matching the `fs_*` syscalls' own
/// "one generic failure sentinel" precedent), given its own name since
/// it's a semantically distinct concept even though the value coincides.
/// [`SPAWN`] returns [`NO_FS`] separately when there's no mounted
/// filesystem at all, same as every `fs_*` syscall.
pub const SPAWN_ERROR: u64 = u64::MAX;

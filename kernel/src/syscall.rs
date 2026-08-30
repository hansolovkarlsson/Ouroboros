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
static TASK_REPORTS: [AtomicU64; crate::tasks::NUM_TASKS] =
    [const { AtomicU64::new(0) }; crate::tasks::NUM_TASKS];

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

/// The virtio-net device the `NET_*` syscalls operate on - the kernel's
/// entire remaining role in networking since the protocol stack moved to
/// userland (the netd server): hold the NIC, send/receive raw frames on
/// request, for exactly one authorized task (netd), the `BLOCK_*` -> fsd
/// pattern. `None` whenever no NIC was discovered this boot.
struct NetCell(core::cell::UnsafeCell<Option<crate::virtio_net::Device>>);
// SAFETY: same single-core, non-reentrant-dispatch reasoning as BlockCell.
unsafe impl Sync for NetCell {}
static NET: NetCell = NetCell(core::cell::UnsafeCell::new(None));

/// The virtio-rng device the `RANDOM` syscall draws from. `None` whenever the
/// machine has no entropy device - the ordinary case on every platform except a
/// QEMU run started with `-device virtio-rng-device`, which is why `RANDOM`
/// answers `RANDOM_UNAVAILABLE` rather than treating absence as a failure.
///
/// Unlike `BLOCK`/`NET`, this one is **not** gated to a single privileged task:
/// entropy is not a device anybody can misuse by reading it, there is no state
/// to corrupt, and every account tool needs it. The cost of a hostile caller is
/// draining the host's entropy pool, which QEMU's device does not meaningfully
/// suffer from.
struct RngCell(core::cell::UnsafeCell<Option<crate::virtio_rng::Device>>);
// SAFETY: same single-core, non-reentrant-dispatch reasoning as BlockCell.
unsafe impl Sync for RngCell {}
static RNG: RngCell = RngCell(core::cell::UnsafeCell::new(None));

/// Installs the entropy source the `RANDOM` syscall serves (from
/// `main.rs::init_entropy`).
pub fn install_rng_device(device: crate::virtio_rng::Device) {
    unsafe { *RNG.0.get() = Some(device) };
}

/// Installs the NIC the `NET_*` syscalls serve (from `main.rs::init_net`).
pub fn install_net_device(device: crate::virtio_net::Device) {
    unsafe { *NET.0.get() = Some(device) };
}

/// Whether the calling task may use the `NET_*` syscalls: whoever holds
/// `CAP_NET`, which by the capability policy is the network server alone -
/// the networking analogue of [`block_access_allowed`].
fn net_access_allowed() -> bool {
    tasks::cap_has(tasks::current_task(), tasks::CAP_NET)
}

/// Whether the NIC has a frame waiting (a non-consuming peek) - the tick
/// wake-check calls this to wake a task blocked in `WaitReason::NetInput`.
/// `false` if no NIC is installed. Not gated: it reads no frame data and is
/// only ever reached from the kernel's own wake-check, not a syscall.
pub(crate) fn net_has_frame() -> bool {
    match unsafe { &*NET.0.get() } {
        // SAFETY: single-core, non-reentrant; has_frame only reads the used
        // ring index (see virtio_net::has_frame).
        Some(device) => unsafe { device.has_frame() },
        None => false,
    }
}

/// The GIC INTID the installed NIC's receive interrupt is wired to, or
/// `None` if no NIC was installed this boot - `main.rs` uses it to enable
/// the interrupt at the GIC and register it with the IRQ handler.
pub(crate) fn net_intid() -> Option<u32> {
    unsafe { &*NET.0.get() }.as_ref().map(|device| device.intid())
}

/// Acknowledges the NIC's pending interrupt at the device (from the IRQ
/// handler - see `exceptions::rust_irq_handler`), so it can raise the next
/// one. A no-op if no NIC is installed. Not gated: it touches only the
/// device's own interrupt-status/ack registers and is reached only from the
/// kernel's IRQ handler, never a syscall.
pub(crate) fn net_ack_interrupt() {
    if let Some(device) = unsafe { &*NET.0.get() } {
        // SAFETY: single-core IRQ context; device installed (init ran).
        unsafe { device.ack_interrupt() };
    }
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
    // The Ctrl+C escape hatch - see tasks::interrupt_key_check's doc comment.
    // Intercepted here, the single choke point every keyboard path funnels
    // through (the wake-check, READ_CHAR's fast path, and TRY_READ_CHAR alike),
    // so Ctrl+C is caught no matter which path would have consumed it. It marks
    // the foreground program for termination (tasks::on_tick does the kill) and
    // swallows the byte.
    if tasks::interrupt_key_check(byte) {
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

/// Staging buffer for a spawn's argv blob (`ARGS_STAGE` writes it here; the
/// next `SPAWN` copies `arg2` bytes of it into the new task's per-slot argv
/// store). Small - `ARGV_MAX` (512), bounded by the shell's input line.
const ARGS_STAGING_SIZE: usize = syscall_abi::ARGV_MAX as usize;
struct ArgsStagingCell(core::cell::UnsafeCell<[u8; ARGS_STAGING_SIZE]>);
// SAFETY: single-core; only touched from the ARGS_STAGE/SPAWN arms, which
// can't run re-entrantly - same reasoning as SPAWN_STAGING.
unsafe impl Sync for ArgsStagingCell {}
static ARGS_STAGING: ArgsStagingCell = ArgsStagingCell(core::cell::UnsafeCell::new([0; ARGS_STAGING_SIZE]));

/// Staging buffer for a spawn's working directory (`CWD_STAGE` writes it
/// here; the next `SPAWN` copies `arg3` bytes into the child's per-slot cwd).
const CWD_STAGING_SIZE: usize = syscall_abi::CWD_MAX as usize;
struct CwdStagingCell(core::cell::UnsafeCell<[u8; CWD_STAGING_SIZE]>);
// SAFETY: single-core, non-reentrant - same as ARGS_STAGING.
unsafe impl Sync for CwdStagingCell {}
static CWD_STAGING: CwdStagingCell = CwdStagingCell(core::cell::UnsafeCell::new([0; CWD_STAGING_SIZE]));

/// Staging buffer for a spawn's **environment** blob (`ENV_STAGE` writes it
/// here). Unlike argv/cwd, `SPAWN`'s four args are already full, so the env
/// isn't passed a length by `SPAWN` - instead `ENV_STAGE` latches the length
/// in [`PENDING_ENV_LEN`], and the next `SPAWN` consumes and clears it.
const ENV_STAGING_SIZE: usize = syscall_abi::ENV_MAX as usize;
struct EnvStagingCell(core::cell::UnsafeCell<[u8; ENV_STAGING_SIZE]>);
// SAFETY: single-core, non-reentrant - same as ARGS_STAGING.
unsafe impl Sync for EnvStagingCell {}
static ENV_STAGING: EnvStagingCell = EnvStagingCell(core::cell::UnsafeCell::new([0; ENV_STAGING_SIZE]));
/// Length of the env blob staged for the *next* `SPAWN` (`0` = none). Set by
/// `ENV_STAGE`, consumed and reset to `0` by the next `SPAWN` - a single-slot
/// latch, safe because the shell always stages immediately before spawning and
/// the kernel is single-core / non-reentrant across the SVC boundary.
static PENDING_ENV_LEN: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// Upper bound for a namespace blob (matches the per-task store), used to bound
/// the `NS_SET` copy. A task's namespace is set directly by `NS_SET` and a
/// child inherits its parent's at spawn (no staging buffer needed).
const NS_MAX_SIZE: usize = syscall_abi::NS_MAX as usize;

/// Number of arguments in an `ARGS_STAGE` blob (`[argc: u32 LE]` header).
fn argv_count(blob: &[u8]) -> u64 {
    if blob.len() < 4 {
        return 0;
    }
    u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as u64
}

/// The bytes of argument `index` within an `ARGS_STAGE` blob (`[len: u32 LE]
/// [bytes]` per arg after the header), or `None` if out of range / malformed.
fn argv_get(blob: &[u8], index: usize) -> Option<&[u8]> {
    let argc = argv_count(blob) as usize;
    if index >= argc {
        return None;
    }
    let mut off = 4usize;
    for k in 0..=index {
        if off + 4 > blob.len() {
            return None;
        }
        let len = u32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]) as usize;
        off += 4;
        if off + len > blob.len() {
            return None;
        }
        if k == index {
            return Some(&blob[off..off + len]);
        }
        off += len;
    }
    None
}

/// Read and clear the env-staging latch ([`PENDING_ENV_LEN`]) - the length of
/// the env blob `ENV_STAGE` staged for the next spawn, or `0` if none. Reset to
/// `0` so each spawn consumes it exactly once.
fn pending_env_len() -> usize {
    PENDING_ENV_LEN.swap(0, core::sync::atomic::Ordering::Relaxed)
}

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
fn spawn_staged(total_len: u64, stdout_target: u64, argv_len: u64, cwd_len: u64) -> u64 {
    // Consume the env latch up front so *any* outcome of this spawn (success or
    // an early failure) clears it - a failed spawn must not leak its staged env
    // onto the next one.
    let staged_env_len = pending_env_len();
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
            // Record where this program's output should go (the console by
            // default; the shell for a pipe/redirect). See STDOUT_TARGET.
            tasks::set_stdout_target(slot, stdout_target);
            // Attach the argv blob staged via ARGS_STAGE (arg2 = its length;
            // 0 = no args). The child reads it via GET_ARGC/GET_ARG. Bounded
            // by the staging buffer itself.
            let argv_len = (argv_len as usize).min(ARGS_STAGING_SIZE);
            if argv_len > 0 {
                let staged = unsafe { &*ARGS_STAGING.0.get() };
                tasks::set_argv(slot, &staged[..argv_len]);
            }
            // Attach the working directory staged via CWD_STAGE (arg3 = its
            // length; 0 = none). The child reads it via GET_CWD.
            let cwd_len = (cwd_len as usize).min(CWD_STAGING_SIZE);
            if cwd_len > 0 {
                let staged = unsafe { &*CWD_STAGING.0.get() };
                tasks::set_cwd(slot, &staged[..cwd_len]);
            }
            // Attach the environment latched by ENV_STAGE (its length isn't a
            // SPAWN arg - the four are full - so it rode PENDING_ENV_LEN,
            // captured into `staged_env_len` at the top). The child reads it via
            // GET_ENVC/GET_ENV.
            let env_len = staged_env_len.min(ENV_STAGING_SIZE);
            if env_len > 0 {
                let staged = unsafe { &*ENV_STAGING.0.get() };
                tasks::set_env(slot, &staged[..env_len]);
            }
            // A child inherits its parent's (the spawning task's) namespace -
            // Plan 9 semantics. Copying it here means `bind` in the shell is
            // seen by every command it spawns, and the child reads it via
            // GET_NS. An empty parent namespace copies as empty (the default).
            tasks::set_namespace(slot, tasks::namespace(tasks::current_task()));
            // A child also inherits its parent's user identity (uid/gid) - a
            // command runs as whoever started it. Unlike the staged attributes
            // above, identity is carried, not chosen per-spawn.
            tasks::inherit_id(slot, tasks::current_task());
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
        syscall_abi::MONOTONIC_US => {
            // Microseconds since boot from the generic timer's free-running
            // counter, computed overflow-safe (a naive now_ticks * 1_000_000
            // overflows a u64 in a few days at 62.5 MHz): split into whole
            // seconds plus the sub-second remainder. Pure system-register
            // reads (no GIC, no interrupts) - see timer.rs.
            let freq = crate::timer::frequency_hz();
            let ticks = crate::timer::now_ticks();
            (ticks / freq) * 1_000_000 + ((ticks % freq) * 1_000_000) / freq
        }
        syscall_abi::SPAWN => spawn_staged(arg0, arg1, arg2, arg3),
        syscall_abi::CWD_STAGE => {
            // arg0 = cwd pointer, arg1 = length. Copies the working-directory
            // string into the staging buffer for the next SPAWN (arg3).
            if !valid_user_range(arg0, arg1) {
                return SPAWN_ERROR;
            }
            let len = arg1 as usize;
            if len > CWD_STAGING_SIZE {
                return SPAWN_ERROR;
            }
            let staging = unsafe { &mut *CWD_STAGING.0.get() };
            // SAFETY: range sanity-checked above, same trust model as every
            // other userland pointer argument.
            let path = unsafe { core::slice::from_raw_parts(arg0 as *const u8, len) };
            staging[..len].copy_from_slice(path);
            0
        }
        syscall_abi::GET_CWD => {
            // arg0 = out pointer, arg1 = out capacity. Copies up to capacity
            // bytes of the current task's cwd, returns its true length.
            if !valid_user_range(arg0, arg1) {
                return 0;
            }
            let cwd = tasks::cwd_path(tasks::current_task());
            let n = (cwd.len() as u64).min(arg1) as usize;
            // SAFETY: out range validated above.
            let dst = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, n) };
            dst.copy_from_slice(&cwd[..n]);
            cwd.len() as u64
        }
        syscall_abi::NS_SET => {
            // arg0 = namespace-blob pointer, arg1 = length. Sets the calling
            // task's own namespace directly; children inherit it at SPAWN.
            if !valid_user_range(arg0, arg1) {
                return SPAWN_ERROR;
            }
            let len = arg1 as usize;
            if len > NS_MAX_SIZE {
                return SPAWN_ERROR;
            }
            // SAFETY: range sanity-checked above, same trust model as every
            // other userland pointer argument.
            let blob = unsafe { core::slice::from_raw_parts(arg0 as *const u8, len) };
            tasks::set_namespace(tasks::current_task(), blob);
            0
        }
        syscall_abi::GET_NS => {
            // arg0 = out pointer, arg1 = out capacity. Copies up to capacity
            // bytes of the current task's namespace, returns its true length.
            if !valid_user_range(arg0, arg1) {
                return 0;
            }
            let ns = tasks::namespace(tasks::current_task());
            let n = (ns.len() as u64).min(arg1) as usize;
            // SAFETY: out range validated above.
            let dst = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, n) };
            dst.copy_from_slice(&ns[..n]);
            ns.len() as u64
        }
        syscall_abi::ARGS_STAGE => {
            // arg0 = blob pointer, arg1 = blob length. Copies the whole argv
            // blob into the staging buffer (a single call - the blob is small,
            // bounded by ARGV_MAX / valid_user_range's cap). The next SPAWN
            // copies arg2 bytes of it into the new task's argv store.
            if !valid_user_range(arg0, arg1) {
                return SPAWN_ERROR;
            }
            let len = arg1 as usize;
            if len > ARGS_STAGING_SIZE {
                return SPAWN_ERROR;
            }
            let staging = unsafe { &mut *ARGS_STAGING.0.get() };
            // SAFETY: range sanity-checked above, same trust model as every
            // other userland pointer argument.
            let blob = unsafe { core::slice::from_raw_parts(arg0 as *const u8, len) };
            staging[..len].copy_from_slice(blob);
            0
        }
        syscall_abi::GET_ARGC => argv_count(tasks::argv_blob(tasks::current_task())),
        syscall_abi::GET_ARG => {
            // arg0 = index, arg1 = out pointer, arg2 = out capacity. Copies up
            // to capacity bytes of argument `index`, returns its true length,
            // or NO_ARG if out of range.
            if !valid_user_range(arg1, arg2) {
                return syscall_abi::NO_ARG;
            }
            let blob = tasks::argv_blob(tasks::current_task());
            match argv_get(blob, arg0 as usize) {
                Some(bytes) => {
                    let n = (bytes.len() as u64).min(arg2) as usize;
                    // SAFETY: out range validated above.
                    let dst = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, n) };
                    dst.copy_from_slice(&bytes[..n]);
                    bytes.len() as u64
                }
                None => syscall_abi::NO_ARG,
            }
        }
        syscall_abi::ENV_STAGE => {
            // arg0 = blob pointer, arg1 = blob length. Copies the env blob into
            // the env staging buffer and latches its length for the next SPAWN
            // (SPAWN's four args are full, so unlike argv there's no length arg).
            if !valid_user_range(arg0, arg1) {
                return SPAWN_ERROR;
            }
            let len = arg1 as usize;
            if len > ENV_STAGING_SIZE {
                return SPAWN_ERROR;
            }
            let staging = unsafe { &mut *ENV_STAGING.0.get() };
            // SAFETY: range sanity-checked above, same trust model as ARGS_STAGE.
            let blob = unsafe { core::slice::from_raw_parts(arg0 as *const u8, len) };
            staging[..len].copy_from_slice(blob);
            PENDING_ENV_LEN.store(len, core::sync::atomic::Ordering::Relaxed);
            0
        }
        syscall_abi::GET_ENVC => argv_count(tasks::env_blob(tasks::current_task())),
        syscall_abi::GET_ENV => {
            // arg0 = index, arg1 = out pointer, arg2 = out capacity. Same shape
            // as GET_ARG (the env blob reuses the argv encoding), returning the
            // index-th NAME=VALUE string or NO_ARG if out of range.
            if !valid_user_range(arg1, arg2) {
                return syscall_abi::NO_ARG;
            }
            let blob = tasks::env_blob(tasks::current_task());
            match argv_get(blob, arg0 as usize) {
                Some(bytes) => {
                    let n = (bytes.len() as u64).min(arg2) as usize;
                    // SAFETY: out range validated above.
                    let dst = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, n) };
                    dst.copy_from_slice(&bytes[..n]);
                    bytes.len() as u64
                }
                None => syscall_abi::NO_ARG,
            }
        }
        syscall_abi::STDOUT_TARGET => tasks::stdout_target_of(tasks::current_task()),
        syscall_abi::SELF => tasks::current_task() as u64,
        syscall_abi::SET_ID => {
            // Root (uid 0) may drop to any identity; a non-root task may only
            // RESTORE to its saved identity (POSIX saved-set-uid) - the shell
            // uses this to log a user in (root drops) and out (restore to root
            // to re-prompt). Anything else is refused: no escalation, and a
            // user's spawned children can't restore to root because their saved
            // identity is their own (see tasks::inherit_id).
            //
            // arg2/arg3 carry the supplementary group list, so identity and
            // membership change together. TWO permission rules, and combining
            // the call must not combine the gates: the identity half is the rule
            // above; the group half is ROOT ONLY (membership is a permission
            // grant, so a task that could add its own groups could hand itself
            // any group-readable file). A non-root caller may pass only an EMPTY
            // list - dropping memberships only removes privilege, which is what
            // logout does. Every pre-existing caller passes 0/0 and so clears
            // them, which is the right default: an identity change carrying a
            // stale group list is a privilege leak.
            let cur = tasks::current_task();
            let new = ((arg1 as u32 as u64) << 32) | (arg0 as u32 as u64);
            let is_root = tasks::uid_of(cur) == 0;
            if !is_root && new != tasks::saved_id_of(cur) {
                return syscall_abi::SET_ID_DENIED;
            }
            let n = (arg3 as usize).min(syscall_abi::MAX_SUPP_GROUPS);
            if !is_root && n > 0 {
                return syscall_abi::SET_ID_DENIED;
            }
            let mut gids = [0u32; syscall_abi::MAX_SUPP_GROUPS];
            if n > 0 {
                let bytes = (n * core::mem::size_of::<u32>()) as u64;
                if !valid_user_range(arg2, bytes) {
                    return syscall_abi::SET_ID_DENIED;
                }
                for (i, g) in gids.iter_mut().enumerate().take(n) {
                    // Read bytewise: nothing guarantees userland aligned the array.
                    let mut b = [0u8; 4];
                    for (k, byte) in b.iter_mut().enumerate() {
                        // SAFETY: range validated above.
                        *byte = unsafe { core::ptr::read((arg2 as *const u8).add(i * 4 + k)) };
                    }
                    *g = u32::from_le_bytes(b);
                }
            }
            tasks::set_groups(cur, &gids[..n]);
            tasks::apply_id(cur, new);
            0
        }
        syscall_abi::GET_GROUPS => {
            // arg0 = task index, arg1 = out pointer, arg2 = capacity in gids.
            // Ungated like GET_ID - membership isn't secret, and fsd needs the
            // sender's list on every permission check. A dead slot reports no
            // identity (see GET_ID), so it reports no groups either.
            let t = arg0 as usize;
            if t >= tasks::NUM_TASKS || !tasks::is_live(t) {
                return syscall_abi::GET_ID_ERR;
            }
            let cap = (arg2 as usize).min(syscall_abi::MAX_SUPP_GROUPS);
            let bytes = (cap * core::mem::size_of::<u32>()) as u64;
            if cap > 0 && !valid_user_range(arg1, bytes) {
                return syscall_abi::GET_ID_ERR;
            }
            let mut gids = [0u32; syscall_abi::MAX_SUPP_GROUPS];
            let total = tasks::groups_of(t, &mut gids[..cap]);
            for (i, g) in gids.iter().enumerate().take(cap.min(total)) {
                let b = g.to_le_bytes();
                for (k, byte) in b.iter().enumerate() {
                    // SAFETY: range validated above.
                    unsafe { core::ptr::write((arg1 as *mut u8).add(i * 4 + k), *byte) };
                }
            }
            total as u64
        }
        syscall_abi::GET_ID => {
            // arg0 = task index -> its packed (gid << 32) | uid. Identity isn't
            // secret (uid/gid aren't credentials), so any task may read any
            // slot's - `ps` shows owners, `fsd` checks the sender's later.
            let t = arg0 as usize;
            // A DEAD slot must not report an identity. `reset_id` stores 0
            // (root) into a slot on task death, so a server that authorizes on
            // GET_ID of the message sender would read a caller who sent a
            // request and then exited as ROOT - send it, exit, be authorized.
            // Report unavailable and let the caller fail closed.
            if t >= tasks::NUM_TASKS || !tasks::is_live(t) {
                syscall_abi::GET_ID_ERR
            } else {
                tasks::id_of(t)
            }
        }
        syscall_abi::SENDER_ID => {
            // No arguments: the credential of whoever sent the message this
            // task last received, captured by the kernel at SEND time. See the
            // ABI doc and tasks::SENDER_CREDS for why GET_ID(sender) is the
            // wrong question for a server that is authorizing a request.
            tasks::sender_id_of(tasks::current_task()).unwrap_or(syscall_abi::GET_ID_ERR)
        }
        syscall_abi::SENDER_GROUPS => {
            // arg0 = out pointer, arg1 = capacity in gids - GET_GROUPS' shape,
            // against the captured credential rather than the live slot.
            let cap = (arg1 as usize).min(syscall_abi::MAX_SUPP_GROUPS);
            let bytes = (cap * core::mem::size_of::<u32>()) as u64;
            if cap > 0 && !valid_user_range(arg0, bytes) {
                return syscall_abi::GET_ID_ERR;
            }
            let mut gids = [0u32; syscall_abi::MAX_SUPP_GROUPS];
            let Some(total) = tasks::sender_groups_of(tasks::current_task(), &mut gids[..cap]) else {
                return syscall_abi::GET_ID_ERR;
            };
            for (i, g) in gids.iter().enumerate().take(cap.min(total)) {
                let b = g.to_le_bytes();
                for (k, byte) in b.iter().enumerate() {
                    // SAFETY: range validated above.
                    unsafe { core::ptr::write((arg0 as *mut u8).add(i * 4 + k), *byte) };
                }
            }
            total as u64
        }
        syscall_abi::RANDOM => {
            // arg0 = out pointer, arg1 = out capacity. Fills the caller's buffer
            // with hardware entropy, returning the byte count written - or
            // RANDOM_UNAVAILABLE when this machine has no entropy device, which
            // is the ordinary case rather than an error (see the ABI doc).
            if !valid_user_range(arg0, arg1) {
                return syscall_abi::RANDOM_UNAVAILABLE;
            }
            // SAFETY: single-core, dispatch is not re-entrant.
            let cell = unsafe { &mut *RNG.0.get() };
            match cell {
                None => syscall_abi::RANDOM_UNAVAILABLE,
                Some(dev) => {
                    // SAFETY: out range validated above.
                    let out = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, arg1 as usize) };
                    // SAFETY: the device was installed after a successful init.
                    unsafe { dev.fill(out) as u64 }
                }
            }
        }
        syscall_abi::HEAP_INFO => {
            let (base, size) = tasks::task_region(tasks::current_task());
            let (heap_base, heap_size) = loader::heap_area(base, size);
            match arg0 {
                syscall_abi::HEAP_INFO_BASE => heap_base,
                syscall_abi::HEAP_INFO_SIZE => heap_size,
                _ => 0,
            }
        }
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
            if current < tasks::FIRST_SPAWNABLE {
                // No slot below FIRST_SPAWNABLE may exit. Stated as the
                // bound rather than as a list, because the list is what
                // goes stale: every one of these comments enumerated
                // "0-4" until a fifth server arrived, and a sixth will
                // do it again. Today that set is the boot shell (0 -
                // nothing would own the keyboard, see
                // tasks::INPUT_OWNER_TASK), idle (1 - never makes
                // syscalls, refused for completeness), the filesystem
                // server (2 - its death would strand the disk for the
                // rest of the boot, and its slot is
                // block-syscall-privileged), the console server (3 - its
                // death would strand steady-state output; the kernel's
                // emergency console still works, but the shell wouldn't
                // render), the network server (4 -
                // block-syscall-privileged for the NIC), and the account
                // server (5 - the only task that may write /etc/shadow).
                // The only case where EXIT returns to its caller.
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
        syscall_abi::TASK_NAME => {
            // arg0 = task index, arg1 = out pointer, arg2 = out capacity.
            // Copies up to capacity bytes of task `index`'s name (argv[0]),
            // returns its true length, or 0 if the slot has no name. The
            // read-only companion to TASK_STATE (see the shell's `ps`).
            if !valid_user_range(arg1, arg2) {
                return 0;
            }
            let i = arg0 as usize;
            if i >= tasks::NUM_TASKS {
                return 0;
            }
            match argv_get(tasks::argv_blob(i), 0) {
                Some(name) => {
                    let n = (name.len() as u64).min(arg2) as usize;
                    // SAFETY: out range validated above.
                    let dst = unsafe { core::slice::from_raw_parts_mut(arg1 as *mut u8, n) };
                    dst.copy_from_slice(&name[..n]);
                    name.len() as u64
                }
                None => 0,
            }
        }
        syscall_abi::TASK_EXIT_CODE => {
            // arg0 = task index. The exit status of a zombie (peeked, NOT
            // reaped - unlike WAIT), or TASK_NO_EXIT_CODE for any other slot.
            // Lets `ps` show why a zombie is holding its slot.
            let i = arg0 as usize;
            if i >= tasks::NUM_TASKS {
                return syscall_abi::TASK_NO_EXIT_CODE;
            }
            tasks::zombie_status(i).unwrap_or(syscall_abi::TASK_NO_EXIT_CODE)
        }
        syscall_abi::POWER => {
            // arg0 = mode. Powers off or halts the machine - neither returns.
            // An unrecognized mode is a no-op error (POWER_BAD_MODE).
            match arg0 {
                syscall_abi::POWER_OFF => crate::power::power_off(),
                syscall_abi::POWER_HALT => crate::power::halt(),
                _ => syscall_abi::POWER_BAD_MODE,
            }
        }
        syscall_abi::YIELD => {
            // Cooperative yield: switch to another runnable task and resume
            // this one later. The return value is the switched-in task's own
            // (see block_current_and_switch's contract), passed through
            // unmodified by the SVC trampoline.
            unsafe { tasks::yield_current_and_switch(frame) }
        }
        syscall_abi::KILL => {
            let i = arg0 as usize;
            if i < tasks::FIRST_SPAWNABLE {
                // Every slot below FIRST_SPAWNABLE is protected - the
                // boot shell (the permanent keyboard owner), idle, and
                // the supervised servers - same reasoning as EXIT's own
                // refusal of them, and see EXIT for why this is phrased
                // as the bound and not as a list.
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
            if i < tasks::FIRST_SPAWNABLE || i == tasks::current_task() {
                // Waiting on any slot below FIRST_SPAWNABLE (they never
                // die - the boot shell, idle, and the supervised servers
                // are all exit/kill-protected) or on yourself is a
                // guaranteed deadlock - refused up front.
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
            // Capability check: may this task send to `dest`? A reply to a
            // pending call is exempt (see may_send); an unsolicited send
            // needs the send-mask bit.
            if !tasks::may_send(tasks::current_task(), dest) {
                return syscall_abi::MSG_ERR_DENIED;
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
            // Capability check: may this task call `dest`? (The request half
            // of a call is an unsolicited send, so it's mask-governed; the
            // reply rides the reply exemption in may_send.)
            if !tasks::may_send(tasks::current_task(), dest) {
                return syscall_abi::MSG_ERR_DENIED;
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
        syscall_abi::NET_SEND => {
            // arg0 = frame ptr, arg1 = frame len (a raw Ethernet frame; the
            // driver prepends the virtio-net header itself). Validated with
            // in_caller_region, not valid_user_range - a frame exceeds the
            // 512-byte MAX_USER_LEN cap.
            if !net_access_allowed()
                || arg0 == 0
                || arg1 == 0
                || arg1 > crate::virtio_net::MAX_FRAME as u64
                || !in_caller_region(arg0, arg1)
            {
                return syscall_abi::NET_ERROR;
            }
            let Some(device) = (unsafe { &mut *NET.0.get() }) else {
                return syscall_abi::NET_ERROR;
            };
            // SAFETY: bounds sanity-checked above, same trust model as BLOCK_*.
            let frame = unsafe { core::slice::from_raw_parts(arg0 as *const u8, arg1 as usize) };
            let timeout = crate::timer::frequency_hz() / 1000 * 100; // ~100ms
            match unsafe { device.send_frame(frame, timeout) } {
                Ok(()) => 0,
                Err(_) => syscall_abi::NET_ERROR,
            }
        }
        syscall_abi::NET_MAC => {
            if !net_access_allowed() {
                return syscall_abi::NET_ERROR;
            }
            match unsafe { &*NET.0.get() } {
                Some(device) => {
                    let m = device.mac();
                    // Pack the 6 MAC bytes little-endian into the low 48 bits.
                    (m[0] as u64)
                        | (m[1] as u64) << 8
                        | (m[2] as u64) << 16
                        | (m[3] as u64) << 24
                        | (m[4] as u64) << 32
                        | (m[5] as u64) << 40
                }
                None => syscall_abi::NET_ERROR,
            }
        }
        syscall_abi::NET_RECV => {
            // arg0 = buffer ptr, arg1 = buffer len; a waiting frame is copied
            // in (truncated to len) and its true length returned, or
            // NET_NO_FRAME if the receive ring is empty.
            if !net_access_allowed()
                || arg0 == 0
                || arg1 == 0
                || arg1 > crate::virtio_net::MAX_FRAME as u64
                || !in_caller_region(arg0, arg1)
            {
                return syscall_abi::NET_ERROR;
            }
            let Some(device) = (unsafe { &mut *NET.0.get() }) else {
                return syscall_abi::NET_ERROR;
            };
            // SAFETY: same as NET_SEND.
            let buf = unsafe { core::slice::from_raw_parts_mut(arg0 as *mut u8, arg1 as usize) };
            match unsafe { device.poll_frame(buf) } {
                Some(len) => len as u64,
                None => syscall_abi::NET_NO_FRAME,
            }
        }
        syscall_abi::NET_WAIT => {
            if !net_access_allowed() {
                return syscall_abi::NET_ERROR;
            }
            // arg0 = timeout in milliseconds (0 = block indefinitely). A
            // nonzero timeout also wakes the caller once it elapses with no
            // input - the timer the network server's TCP retransmit timeout
            // needs when a peer goes silent. Block until a frame arrives, a
            // message is queued, or the deadline passes (or one is already
            // true - the poll checks up front, never sleeping through pending
            // input). Same frame-overwrite blocking contract as READ_CHAR.
            let deadline = if arg0 == 0 {
                0
            } else {
                let ticks = arg0.div_ceil(crate::timer::TICK_INTERVAL_MS);
                crate::exceptions::ticks() + ticks.max(1)
            };
            unsafe {
                tasks::block_current_and_switch(frame, tasks::WaitReason::NetInput { deadline })
            }
        }
        syscall_abi::FG => {
            let i = arg0 as usize;
            if (1..tasks::FIRST_SPAWNABLE).contains(&i) {
                // Foregrounding idle (1) or any supervised server (every
                // slot from 2 up to FIRST_SPAWNABLE) is refused, same
                // protected set as KILL/EXIT:
                // idle would strand the keyboard on a task that never reads it,
                // and a server has no reason to own the terminal - worse,
                // foregrounding one then hitting Ctrl+C would route the
                // terminate at a protected task (the kernel would kill it).
                // Index 0 is allowed - an explicit "give it back".
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
        syscall_abi::DELEGATE => {
            // arg0 = grantee slot, arg1 = target slot: grant `grantee` the
            // runtime capability to send to `target`. The caller may only
            // delegate a send-cap it *statically holds* (may_delegate),
            // which confines inter-child streaming to the shell - see the
            // DELEGATE doc in syscall-abi and tasks::may_delegate.
            let grantee = arg0 as usize;
            let target = arg1 as usize;
            if grantee >= tasks::NUM_TASKS
                || !tasks::task_exists(grantee)
                || target >= tasks::NUM_TASKS
                || !tasks::task_exists(target)
                || !tasks::may_delegate(tasks::current_task(), target)
            {
                return syscall_abi::MSG_ERR_DENIED;
            }
            tasks::set_delegate(grantee, target);
            0
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

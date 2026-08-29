//! Server supervision - the general version of what used to be fsd-only
//! crash recovery (MINIX's reincarnation server / Helix's self-heal,
//! minimal edition). A registry of supervised userland servers (the
//! filesystem server in slot 2 and the console server in slot 3 today),
//! each with its raw ELF image kept from boot so a dead one can be
//! restarted without a filesystem to reload it from - which matters,
//! since one of the servers *is* the filesystem.
//!
//! Three things restart a supervised server, all reusing the same reload:
//! - **A crash.** `exceptions.rs`'s EL0-fault handler, on a fault in a
//!   supervised slot, tears the task down and calls [`restart`].
//! - **A *runnable* wedge.** A server stuck in an infinite loop never
//!   faults, so the crash path never fires. The passive [`heartbeat`],
//!   driven by `tasks::on_tick` every tick, catches it: a healthy server
//!   (idle in `msg_recv`, or briefly busy) keeps returning to a `Blocked`
//!   state; a wedged one stays `Runnable`. Continuously `Runnable` for
//!   [`WEDGE_TICKS`] ⇒ wedged ⇒ restart.
//! - **A *blocked* wedge (the active ping).** A server stuck `Blocked`
//!   forever (deadlocked mid-request, waiting on a reply that never comes)
//!   is invisible to both paths above: it never faults, and it never goes
//!   `Runnable`, so a healthy idle server and a deadlocked one look
//!   identical from the outside. The only way to tell them apart is to
//!   *poke* it. [`poll_ping`], also driven by `on_tick`, does: it has the
//!   caller inject a `SYSOP_PING` message (sender [`KERNEL_SENDER`]) into a
//!   server that's been sitting `Blocked`. A server idle in its main
//!   `msg_recv` is woken by direct delivery and replies; that reply,
//!   addressed back to `KERNEL_SENDER`, is intercepted by the `MSG_SEND`
//!   syscall arm as the ack ([`note_ack`]). A server stuck mid-sub-call
//!   does *not* get woken (the ping just queues, unseen) - so an
//!   outstanding ping older than [`PING_TIMEOUT`] ⇒ wedged ⇒ restart.
//!
//! Why a ping and not just more state-reading: the runnable-wedge detector
//! is passive (it reads task state, no server cooperation), but a
//! blocked-forever server can't be distinguished from a healthy idle one
//! without a response, so the active ping is the only tool for it. It
//! needs *no server changes* regardless - a server replies to any unknown
//! op, and that reply is the ack. `tasks::fail_calls_to` already rescues
//! callers of a server that *dies*; the ping is what catches a server that
//! wedges *without* dying while some caller waits on it. On this
//! single-user, fast-request system a healthy server acks within a tick or
//! two, far inside [`PING_TIMEOUT`], so a genuine deadlock is caught while
//! a healthy idle server never trips.
//!
//! [`KERNEL_SENDER`]: syscall_abi::KERNEL_SENDER

use core::cell::UnsafeCell;

use crate::console;
use crate::exceptions::Context;
use crate::loader;
use crate::tasks;

/// Largest supervised-server image kept for restart (was `FSD_IMAGE_SIZE`).
///
/// **Sized from the real binaries, not a guess.** This was 128 KB, and `fsd` -
/// the server whose crash recovery the whole isolation arc was built around -
/// had quietly outgrown it (~136 KB as of 2026-08-29), so it registered as
/// "image too large" and was **not restartable at all**. Nothing failed loudly:
/// the boot log said so, in a line easy to read past, and a crash simply killed
/// the filesystem for that boot instead of restarting it.
///
/// Kept well above the largest server with room to grow, and checked against
/// `ls -l build/esp/EFI/ORBS/*.BIN` when a server gains weight. The cost is
/// [`MAX_SUPERVISED`] × this, in kernel `.bss`.
const IMG_CAP: usize = 192 * 1024;

/// How many servers can be supervised at once. **Exactly saturated today** by
/// fsd/cond/netd/accountd, so a **fifth** server needs this raised - each entry
/// costs [`IMG_CAP`] (192 KB) of kernel `.bss`, which is why it is not simply
/// set higher "just in case". [`register`] reports a full registry distinctly
/// from an oversized image so that limit announces itself rather than being
/// mistaken for a file-size problem.
const MAX_SUPERVISED: usize = 4;

/// Per-boot restart cap per server (covers crashes *and* wedges together):
/// a server that fails on its own startup would otherwise restart forever.
/// Past this the kernel gives up on that slot - it stays `Unused` (a dead
/// FS server degrades exactly like a missing FSD.BIN), or, for a wedge
/// past the cap, is simply left looping (its CPU share wasted, honestly).
const MAX_RESTARTS: u32 = 3;

/// Consecutive ticks a supervised server may be observed `Runnable` before
/// the heartbeat declares it wedged. At `timer::TICK_INTERVAL_MS` (20ms)
/// this is ~2.5s - safely above any real request (servers return to
/// `Blocked(recv)` in far less than one tick), while still recovering a
/// genuine wedge promptly. Tunable.
const WEDGE_TICKS: u32 = 128;

/// How often (in ticks) the active ping pokes a server that's been sitting
/// `Blocked` - about once a second at the 20ms tick. A healthy idle server
/// is woken, acks, and goes back to `Blocked`; this is just the poke
/// cadence, not the failure threshold.
const PING_INTERVAL: u32 = 64;

/// How long (in ticks) an injected ping may go unacked before the server is
/// declared wedged. Far above a healthy ack's round trip (a woken server
/// acks within a tick or two), so a genuine `Blocked`-deadlock is caught
/// while a healthy idle server never trips.
const PING_TIMEOUT: u32 = 8;

/// [`poll_ping`]'s verdict for one supervised server this tick.
pub enum PingAction {
    /// Nothing to do - the server is making progress, or a ping is
    /// outstanding but not yet timed out, or it isn't yet time to ping.
    None,
    /// Inject a fresh ping into this server's mailbox (the caller does the
    /// actual `tasks::send_message`, since the send machinery lives there).
    Inject,
    /// An outstanding ping went unacked past [`PING_TIMEOUT`] - the server
    /// is wedged while `Blocked`. Restart it on the same teardown path the
    /// Runnable-wedge and fault handler use.
    Wedged,
}

struct Entry {
    /// The task slot this server runs in once registered; `None` = free.
    slot: Option<usize>,
    image: [u8; IMG_CAP],
    image_len: usize,
    /// Crash + wedge restarts so far this boot, vs [`MAX_RESTARTS`].
    restarts: u32,
    /// Heartbeat: consecutive ticks observed `Runnable` (reset on any
    /// `Blocked` state, and on a restart).
    runnable_ticks: u32,
    /// Active ping: whether a ping is currently awaiting an ack.
    ping_outstanding: bool,
    /// Ticks the outstanding ping has waited (only meaningful while
    /// `ping_outstanding`); vs [`PING_TIMEOUT`].
    ping_wait: u32,
    /// Ticks the server has sat `Blocked` with no ping outstanding; vs
    /// [`PING_INTERVAL`], the poke cadence.
    idle_ticks: u32,
}

impl Entry {
    const fn empty() -> Self {
        Entry {
            slot: None,
            image: [0; IMG_CAP],
            image_len: 0,
            restarts: 0,
            runnable_ticks: 0,
            ping_outstanding: false,
            ping_wait: 0,
            idle_ticks: 0,
        }
    }

    /// Clears all liveness state (heartbeat + ping) - a fresh server owes
    /// nothing yet. Called on register and on every restart.
    fn reset_liveness(&mut self) {
        self.runnable_ticks = 0;
        self.ping_outstanding = false;
        self.ping_wait = 0;
        self.idle_ticks = 0;
    }
}

struct Registry(UnsafeCell<[Entry; MAX_SUPERVISED]>);
// SAFETY: single-core; entries are filled once at boot (`register`, during
// boot services) and afterward only touched from the EL0-fault handler and
// `on_tick`, both of which run with all exceptions masked - never
// reentrant, the same contract the old `FSD_IMAGE` static relied on.
unsafe impl Sync for Registry {}
static REGISTRY: Registry = Registry(UnsafeCell::new([const { Entry::empty() }; MAX_SUPERVISED]));

/// Stash `slot`'s ELF image and mark it supervised. Called once per server
/// by `loader.rs` during boot services (`load_fsd`/`load_cond`). Returns
/// whether the image fit; a too-big image (or a full registry) just means
/// that server won't be restartable this boot, not a boot failure.
pub fn register(slot: usize, image: &[u8]) -> Registered {
    if image.is_empty() || image.len() > IMG_CAP {
        return Registered::ImageTooLarge;
    }
    let reg = unsafe { &mut *REGISTRY.0.get() };
    let idx = reg
        .iter()
        .position(|e| e.slot == Some(slot))
        .or_else(|| reg.iter().position(|e| e.slot.is_none()));
    let Some(idx) = idx else {
        return Registered::RegistryFull;
    };
    let e = &mut reg[idx];
    e.slot = Some(slot);
    e.image[..image.len()].copy_from_slice(image);
    e.image_len = image.len();
    e.restarts = 0;
    e.reset_liveness();
    Registered::Ok
}

/// The outcome of [`register`]. Two failures that used to be one `false`: the
/// image not fitting [`IMG_CAP`], and the registry being full. They call for
/// completely different fixes, and with [`MAX_SUPERVISED`] now exactly
/// saturated (fsd/cond/netd/accountd) the second one is the live risk - a sixth
/// server would otherwise boot unsupervised while the log blamed its file size.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Registered {
    Ok,
    /// The image is empty or larger than [`IMG_CAP`].
    ImageTooLarge,
    /// All [`MAX_SUPERVISED`] entries are taken - raise it (each entry costs
    /// `IMG_CAP`, 192 KB, so this is a deliberate memory trade, not a typo).
    RegistryFull,
}

impl Registered {
    /// Why registration failed, for the loader's boot warning - or `None` on
    /// success.
    pub fn why(self) -> Option<&'static str> {
        match self {
            Registered::Ok => None,
            Registered::ImageTooLarge => Some("image too large to keep for crash recovery"),
            Registered::RegistryFull => {
                Some("the supervision registry is full (raise MAX_SUPERVISED)")
            }
        }
    }
}

/// Whether `slot` is a supervised server - the generic replacement for the
/// fault handler's old `if current == FSD_TASK` check.
pub fn is_supervised(slot: usize) -> bool {
    let reg = unsafe { &*REGISTRY.0.get() };
    reg.iter().any(|e| e.slot == Some(slot))
}

/// Restart the server in `slot` from its kept image (the generalized
/// `restart_fsd`). Reparses and reloads the image into a fresh region and
/// installs a fresh task into the same slot; the **caller** has already
/// torn the dead task down (freed its region, failed its callers) and does
/// the mmu rebuild that makes the fresh region EL0-accessible. A per-slot
/// cap guards crash/wedge loops. Safe to call whether the dead task was
/// the current one or not - it only touches this slot.
pub fn restart(slot: usize) {
    let reg = unsafe { &mut *REGISTRY.0.get() };
    let Some(e) = reg.iter_mut().find(|e| e.slot == Some(slot)) else {
        return;
    };
    if e.image_len == 0 {
        console::println!("Ouroboros kernel: no kept image for server slot {slot} - not restarting");
        return;
    }
    e.restarts += 1;
    let attempts = e.restarts;
    if attempts > MAX_RESTARTS {
        console::println!(
            "Ouroboros kernel: server slot {slot} failed more than {MAX_RESTARTS} times this boot - giving up"
        );
        return;
    }
    // Fresh server, fresh liveness (heartbeat + ping).
    e.reset_liveness();
    let image_len = e.image_len;
    let (header, phdrs, region_size) = match loader::elf_region_size(&e.image[..image_len]) {
        Ok(result) => result,
        Err(_) => {
            console::println!("Ouroboros kernel: kept image for slot {slot} failed to parse - not restarting");
            return;
        }
    };
    let region_base = tasks::allocate_runtime_region(region_size);
    // SAFETY: `region_base` was just handed out by `allocate_runtime_region`,
    // fresh and at least `region_size` bytes - the same contract the fault
    // path's restart already relied on.
    let loaded = match unsafe {
        loader::populate_region(&e.image[..image_len], &header, phdrs.as_slice(), region_base, region_size)
    } {
        Ok(loaded) => loaded,
        Err(_) => {
            tasks::free_runtime_region(region_base, region_size);
            console::println!("Ouroboros kernel: kept image for slot {slot} failed to load - not restarting");
            return;
        }
    };
    let context = Context {
        gpr: [0; 31],
        sp_el0: loaded.base + loaded.size,
        elr_el1: loaded.entry,
        spsr_el1: 0,
    };
    tasks::install_task(slot, context, (loaded.base, loaded.size));
    // The crash teardown cleared this slot's argv (the store `TASK_NAME`
    // reads), so re-apply the server's name - otherwise a restarted server
    // would go nameless in `ps`.
    if let Some(name) = tasks::server_name(slot) {
        tasks::set_name(slot, name);
    }
    console::println!("Ouroboros kernel: server slot {slot} restarted (attempt {attempts}/{MAX_RESTARTS})");
}

/// One heartbeat observation for a supervised server, called from
/// `on_tick`. `blocked` = the server is currently in *any* `Blocked` state
/// (idle in `msg_recv`, or waiting on a sub-call - both mean it's making
/// progress, not wedged). Returns `true` exactly once, when the server has
/// been continuously `Runnable` for [`WEDGE_TICKS`] - the wedge signal, on
/// which the caller restarts it. A no-op for an unregistered slot.
pub fn heartbeat(slot: usize, blocked: bool) -> bool {
    let reg = unsafe { &mut *REGISTRY.0.get() };
    let Some(e) = reg.iter_mut().find(|e| e.slot == Some(slot)) else {
        return false;
    };
    if blocked {
        e.runnable_ticks = 0;
        return false;
    }
    e.runnable_ticks = e.runnable_ticks.saturating_add(1);
    // Exactly `==` (not `>=`) so it fires once; a restart resets the
    // counter, and a give-up (past the cap) leaves it climbing past the
    // threshold so it never re-fires.
    e.runnable_ticks == WEDGE_TICKS
}

/// Record that `slot` acked a liveness ping - a supervised server replied
/// to a ping (its reply, addressed to [`KERNEL_SENDER`], is intercepted by
/// the `MSG_SEND` syscall arm, which calls this). Clears the outstanding
/// ping so the poke cadence starts over. A no-op for an unregistered slot
/// or one with no ping outstanding - so a stray message to `KERNEL_SENDER`
/// from anyone, at any time, does nothing.
///
/// [`KERNEL_SENDER`]: syscall_abi::KERNEL_SENDER
pub fn note_ack(slot: usize) {
    let reg = unsafe { &mut *REGISTRY.0.get() };
    if let Some(e) = reg.iter_mut().find(|e| e.slot == Some(slot)) {
        e.ping_outstanding = false;
        e.ping_wait = 0;
        e.idle_ticks = 0;
    }
}

/// One active-ping observation for a supervised server, called from
/// `on_tick` alongside [`heartbeat`]. `blocked` = the server is currently
/// in any `Blocked` state. The active ping catches the failure the passive
/// heartbeat can't: a server stuck `Blocked` forever (deadlocked
/// mid-request). Returns what the caller should do this tick - see
/// [`PingAction`]. A no-op ([`PingAction::None`]) for an unregistered slot.
///
/// The state machine, per supervised server:
/// - **Runnable** (making progress, or a Runnable-wedge the passive
///   heartbeat owns): reset all ping state; never ping a running server.
/// - **Blocked, ping outstanding**: count ticks; past [`PING_TIMEOUT`] with
///   no ack, it's wedged ([`PingAction::Wedged`], and reset since a restart
///   follows).
/// - **Blocked, no ping outstanding**: count idle ticks; every
///   [`PING_INTERVAL`], ask the caller to inject one
///   ([`PingAction::Inject`]) and mark it outstanding. One ping outstanding
///   at a time, so the server's 4-deep mailbox can never fill with pings.
pub fn poll_ping(slot: usize, blocked: bool) -> PingAction {
    let reg = unsafe { &mut *REGISTRY.0.get() };
    let Some(e) = reg.iter_mut().find(|e| e.slot == Some(slot)) else {
        return PingAction::None;
    };
    if !blocked {
        // A running server needs no ping; a Runnable *wedge* is the passive
        // heartbeat's job. Drop any half-finished ping cycle.
        e.ping_outstanding = false;
        e.ping_wait = 0;
        e.idle_ticks = 0;
        return PingAction::None;
    }
    if e.ping_outstanding {
        e.ping_wait = e.ping_wait.saturating_add(1);
        if e.ping_wait >= PING_TIMEOUT {
            e.reset_liveness();
            return PingAction::Wedged;
        }
        return PingAction::None;
    }
    e.idle_ticks = e.idle_ticks.saturating_add(1);
    if e.idle_ticks >= PING_INTERVAL {
        e.ping_outstanding = true;
        e.ping_wait = 0;
        e.idle_ticks = 0;
        return PingAction::Inject;
    }
    PingAction::None
}

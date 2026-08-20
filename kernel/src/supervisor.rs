//! Server supervision - the general version of what used to be fsd-only
//! crash recovery (MINIX's reincarnation server / Helix's self-heal,
//! minimal edition). A registry of supervised userland servers (the
//! filesystem server in slot 2 and the console server in slot 3 today),
//! each with its raw ELF image kept from boot so a dead one can be
//! restarted without a filesystem to reload it from - which matters,
//! since one of the servers *is* the filesystem.
//!
//! Two things restart a supervised server, both reusing the same reload:
//! - **A crash.** `exceptions.rs`'s EL0-fault handler, on a fault in a
//!   supervised slot, tears the task down and calls [`restart`].
//! - **A wedge.** A server stuck in an infinite loop never faults, so the
//!   crash path never fires - the failure mode nothing else detects. The
//!   [`heartbeat`], driven by `tasks::on_tick` every tick, catches it: a
//!   healthy server (idle in `msg_recv`, or briefly busy) keeps returning
//!   to a `Blocked` state; a wedged one stays `Runnable`. Continuously
//!   `Runnable` for [`WEDGE_TICKS`] ⇒ wedged ⇒ restart on the same path.
//!
//! The heartbeat is deliberately *passive* (it reads task state, no
//! server-side ping). It can't catch a server blocked forever on a
//! deadlocked call (rarer; `tasks::fail_calls_to` already handles the
//! dead-target case), which an active ping could - a clean future
//! refinement. On this single-user, fast-request system, "continuously
//! runnable for seconds" is a reliable wedge signal.

use core::cell::UnsafeCell;

use crate::console;
use crate::exceptions::Context;
use crate::loader;
use crate::tasks;

/// Largest supervised-server image kept for restart (was `FSD_IMAGE_SIZE`).
/// fsd is the biggest today (tens of KB); cond is a few KB.
const IMG_CAP: usize = 128 * 1024;

/// How many servers can be supervised at once - fsd, cond, and headroom
/// for whatever moves out of the kernel next.
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
}

impl Entry {
    const fn empty() -> Self {
        Entry { slot: None, image: [0; IMG_CAP], image_len: 0, restarts: 0, runnable_ticks: 0 }
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
pub fn register(slot: usize, image: &[u8]) -> bool {
    if image.is_empty() || image.len() > IMG_CAP {
        return false;
    }
    let reg = unsafe { &mut *REGISTRY.0.get() };
    let idx = reg
        .iter()
        .position(|e| e.slot == Some(slot))
        .or_else(|| reg.iter().position(|e| e.slot.is_none()));
    let Some(idx) = idx else {
        return false;
    };
    let e = &mut reg[idx];
    e.slot = Some(slot);
    e.image[..image.len()].copy_from_slice(image);
    e.image_len = image.len();
    e.restarts = 0;
    e.runnable_ticks = 0;
    true
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
    // Fresh server, fresh heartbeat.
    e.runnable_ticks = 0;
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

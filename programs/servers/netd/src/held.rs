//! **Held user keys**: step 4 of `docs/roadmap/roadmap-user-keys.md`
//! (Decision 4).
//!
//! `login` derives a user's cluster key from the password and hands the seed to
//! `netd` with `NETOP_KEY_HOLD`, then wipes its own copy. `netd` holds it while
//! the user is logged in; from step 7 it signs that user's credentials with it,
//! inside its own event loop, so the key never leaves this task again.
//!
//! The rules, each one a line below:
//!
//! - **Only root fills the table** (`SENDER_ID` uid 0: `login`, before it drops
//!   privileges). So a full table refuses a hold rather than evicting: nothing
//!   but a login can have put the others there.
//! - **A hold replaces the same owner's key.** The boot shell logs in and out in
//!   one task that `KILL` refuses, so its packed identity is the same every
//!   session and no liveness check ever retires its key.
//! - **A drop by handle** is the holder's or root's; anyone else is refused.
//!   **A drop of "mine"** takes every key the sender holds (the login loop sends
//!   it before every prompt, so a lost logout drop cannot outlive the session).
//! - **A dead owner's key goes** in the idle pass: an owner is alive while its
//!   slot still holds the same packed identity and is not a zombie.
//! - **Every seed is wiped** when its entry goes, whichever way it goes.
//!
//! What a held key means: while `user` is logged in here, anything running as
//! uid 1000 can have credentials signed with it. That is no more than root on
//! this node can do already; what the plan closes is a node acting for users
//! who are NOT logged in on it.

use syscall_abi::NET_KEY_MAX;

use crate::syscall;

/// One held key: the uid it signs for, the Ed25519 seed, and the packed task
/// identity of the login that holds it.
struct HeldKey {
    uid: u32,
    seed: [u8; 32],
    owner: u64,
}

impl Drop for HeldKey {
    /// Wipe the seed. VOLATILE, so the compiler cannot drop the stores as
    /// dead: the entry is going away, which is exactly when an optimiser would.
    fn drop(&mut self) {
        for b in self.seed.iter_mut() {
            // SAFETY: a byte of this struct, which we have &mut to.
            unsafe { core::ptr::write_volatile(b, 0) };
        }
    }
}

/// The table. On `serve`'s frame for the whole boot (no mutable statics); a
/// `netd` restart loses every key, and users log in again to have one held.
pub struct Held {
    slots: [Option<HeldKey>; NET_KEY_MAX],
}

impl Held {
    pub fn new() -> Self {
        Held { slots: core::array::from_fn(|_| None) }
    }

    /// Hold `seed` for `uid` on behalf of `owner`, replacing any key `owner`
    /// already holds. `Some(handle)`, or `None` when every slot is another
    /// owner's live key.
    pub fn hold(&mut self, uid: u32, seed: &[u8; 32], owner: u64) -> Option<usize> {
        self.drop_mine(owner);
        let i = self.slots.iter().position(|s| s.is_none())?;
        self.slots[i] = Some(HeldKey { uid, seed: *seed, owner });
        Some(i)
    }

    /// Drop the key under `handle`, if the sender is its holder or root.
    pub fn drop_handle(&mut self, handle: u64, sender: u64, sender_uid: u32) -> u64 {
        let Some(slot) = usize::try_from(handle).ok().and_then(|h| self.slots.get_mut(h)) else {
            return syscall_abi::NET_KEY_NOT_HELD;
        };
        match slot {
            None => syscall_abi::NET_KEY_NOT_HELD,
            Some(k) if k.owner != sender && sender_uid != 0 => syscall_abi::NET_KEY_DENIED,
            Some(_) => {
                *slot = None;
                syscall_abi::NET_KEY_OK
            }
        }
    }

    /// Drop every key `owner` holds, returning how many.
    pub fn drop_mine(&mut self, owner: u64) -> usize {
        let mut n = 0;
        for slot in self.slots.iter_mut() {
            if matches!(slot, Some(k) if k.owner == owner) {
                *slot = None;
                n += 1;
            }
        }
        n
    }

    /// The idle pass: drop the key of every owner that is gone.
    pub fn reap_dead(&mut self) {
        for slot in self.slots.iter_mut() {
            if matches!(slot, Some(k) if !alive(k.owner)) {
                *slot = None;
            }
        }
    }

    /// The uids with a key held, into `out`; returns how many (the diagnostic
    /// `NETOP_KEY_LIST`). Never the keys.
    pub fn uids(&self, out: &mut [u32; NET_KEY_MAX]) -> usize {
        let mut n = 0;
        for k in self.slots.iter().flatten() {
            out[n] = k.uid;
            n += 1;
        }
        n
    }
}

/// Whether the task `owner` names is still running: its slot holds the same
/// packed identity and is runnable or blocked, not a zombie.
///
/// State FIRST, identity second, so a slot recycled at any point between the
/// two reads fails the identity comparison: a state read from the new occupant
/// is never paired with the old occupant's identity.
fn alive(owner: u64) -> bool {
    let slot = syscall_abi::task_id_slot(owner);
    let state = syscall(syscall_abi::TASK_STATE, slot);
    let now = syscall(syscall_abi::TASK_IDENTITY, slot);
    now == owner && (state == syscall_abi::TASK_STATE_RUNNABLE || state == syscall_abi::TASK_STATE_BLOCKED)
}

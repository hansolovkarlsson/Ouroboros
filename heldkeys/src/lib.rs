//! **Held user keys**: step 4 of `docs/roadmap/roadmap-user-keys.md`
//! (Decision 4).
//!
//! `login` derives a user's cluster key from the password and hands the seed to
//! `netd` with `NETOP_KEY_HOLD`, then wipes its own copy. `netd` holds it while
//! the user is logged in, and since step 8 signs that user's credentials with
//! it (`sign_parked`), inside its own event loop, so the key never leaves this
//! task again. A session opened under a credential is closed when its uid's
//! last key goes (`close_unheld`).
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
//!   slot still holds the same packed identity and is not a zombie. Until then
//!   it is held but never used: `seed_for` and `holds_live` ask liveness
//!   themselves.
//! - **Every seed is wiped** when its entry goes, whichever way it goes.
//!
//! What a held key means: while `user` is logged in here, anything running as
//! uid 1000 can have credentials signed with it. That is no more than root on
//! this node can do already; what the plan closes is a node acting for users
//! who are NOT logged in on it.
//!
//! **A crate of its own since 2026-11-10**, moved out of `netd`'s `held.rs`.
//! The table asked the kernel itself whether an owner was alive, so nothing
//! but a booted `netd` could run it, and no driven boot can place a request
//! between a login's death and the idle pass that reaps its key: the rule
//! "held but never used" passed every rig with the rule taken out. Liveness
//! is now [`Tasks`], given once when the table is made, and the tests below
//! say which owners are alive. `netd` gives the kernel's answer
//! (`programs/servers/netd/src/held.rs`).

#![cfg_attr(not(test), no_std)]

use syscall_abi::NET_KEY_MAX;

/// What the table asks of the task table: whether the task a packed identity
/// names is still running. Given to [`Held::new`] once, so no caller of
/// `seed_for` or `holds_live` chooses the answer, or can leave the question
/// out.
pub trait Tasks {
    /// Whether `owner`, a packed task identity, is still that task and still
    /// running (not exited, not replaced in its slot).
    fn alive(&self, owner: u64) -> bool;
}

/// One held key: the uid it signs for, the Ed25519 seed, the packed task
/// identity of the login that holds it, and when it was held (`Held::next`),
/// so the newest of a uid's keys is the one that signs.
struct HeldKey {
    uid: u32,
    seed: [u8; 32],
    owner: u64,
    seq: u64,
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
pub struct Held<T: Tasks> {
    slots: [Option<HeldKey>; NET_KEY_MAX],
    /// The next hold's `seq`: holds in the order they were made.
    next: u64,
    /// Who is alive.
    tasks: T,
}

impl<T: Tasks> Held<T> {
    pub fn new(tasks: T) -> Self {
        Held { slots: core::array::from_fn(|_| None), next: 0, tasks }
    }

    /// Hold `seed` for `uid` on behalf of `owner`, replacing any key `owner`
    /// already holds. `Some(handle)`, or `None` when every slot is another
    /// owner's live key.
    pub fn hold(&mut self, uid: u32, seed: &[u8; 32], owner: u64) -> Option<usize> {
        self.drop_mine(owner);
        let i = self.slots.iter().position(|s| s.is_none())?;
        self.slots[i] = Some(HeldKey { uid, seed: *seed, owner, seq: self.next });
        self.next += 1;
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
        let tasks = &self.tasks;
        for slot in self.slots.iter_mut() {
            if matches!(slot, Some(k) if !tasks.alive(k.owner)) {
                *slot = None;
            }
        }
    }

    /// The seed held for `uid` by a login that is still alive, for signing
    /// that user's credential (step 8): the NEWEST of those keys. Two logins
    /// of one user hold the same key, derived from the same name, realm and
    /// password, unless the password changed between them; then only the
    /// later login's key matches the registry the password change rewrote,
    /// and the slot order says nothing about which that is (high review of
    /// step 8). A dead login's key is not used, though the idle pass has not
    /// reaped it yet: a killed login's user is not attested by it (medium
    /// review of step 8).
    pub fn seed_for(&self, uid: u32) -> Option<&[u8; 32]> {
        self.slots.iter().flatten().filter(|k| k.uid == uid && self.tasks.alive(k.owner)).max_by_key(|k| k.seq).map(|k| &k.seed)
    }

    /// Whether a key is held for `uid` by a login that is still alive, asked
    /// without waiting for the idle pass to reap a dead one. The one test of
    /// "this user is held" every caller asks: what signs the credential
    /// (`seed_for`), what lets a session opened under it be used
    /// (`park_session`, `sign_parked`) and what keeps it open
    /// (`close_unheld`), so no two of them disagree in the window between a
    /// login's death and its reap (medium review of step 8).
    pub fn holds_live(&self, uid: u32) -> bool {
        self.seed_for(uid).is_some()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    /// The owners that are dead; everyone else is alive.
    struct Dead<'a>(&'a RefCell<BTreeSet<u64>>);

    impl Tasks for Dead<'_> {
        fn alive(&self, owner: u64) -> bool {
            !self.0.borrow().contains(&owner)
        }
    }

    const USER: u32 = 1000;
    const GUEST: u32 = 1001;
    /// Packed identities: a generation above the slot bits, the slot below.
    const LOGIN_A: u64 = (1 << 8) | 6;
    const LOGIN_B: u64 = (1 << 8) | 7;
    const LOGIN_C: u64 = (2 << 8) | 6;

    fn uids<T: Tasks>(h: &Held<T>) -> Vec<u32> {
        let mut out = [0u32; NET_KEY_MAX];
        let n = h.uids(&mut out);
        out[..n].to_vec()
    }

    /// The window no rig reaches: the login is dead and the idle pass has not
    /// run. The key is still in the table and signs nothing.
    #[test]
    fn a_dead_logins_key_is_held_and_never_used() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[7; 32], LOGIN_A).unwrap();
        assert_eq!(h.seed_for(USER), Some(&[7; 32]));
        assert!(h.holds_live(USER));

        dead.borrow_mut().insert(LOGIN_A);
        assert_eq!(uids(&h), [USER], "not reaped yet: the key is still in the table");
        assert_eq!(h.seed_for(USER), None, "a dead login's key signed");
        assert!(!h.holds_live(USER), "a dead login's key counted as held");

        h.reap_dead();
        assert_eq!(uids(&h), [] as [u32; 0]);
    }

    /// One user's death is not another's: the dead login's uid loses its
    /// standing and the live login's keeps it, and the reap takes the dead
    /// one's key alone.
    #[test]
    fn liveness_is_asked_of_the_keys_own_owner() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        h.hold(GUEST, &[2; 32], LOGIN_B).unwrap();
        dead.borrow_mut().insert(LOGIN_A);
        assert_eq!(h.seed_for(USER), None);
        assert_eq!(h.seed_for(GUEST), Some(&[2; 32]));
        h.reap_dead();
        assert_eq!(uids(&h), [GUEST]);
    }

    /// Two logins of one user: the newest live key signs. When the newer
    /// login dies, the older one's key signs again, and the user is held
    /// until the last of them goes.
    #[test]
    fn the_newest_live_key_signs() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        h.hold(USER, &[2; 32], LOGIN_B).unwrap();
        assert_eq!(h.seed_for(USER), Some(&[2; 32]));

        dead.borrow_mut().insert(LOGIN_B);
        assert_eq!(h.seed_for(USER), Some(&[1; 32]));
        assert!(h.holds_live(USER));

        dead.borrow_mut().insert(LOGIN_A);
        assert!(!h.holds_live(USER));
    }

    /// Newest by the order of the holds, not by the slot: a key held later
    /// in a slot an earlier drop freed is still the newest.
    #[test]
    fn newest_is_the_order_held_not_the_slot() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        let first = h.hold(GUEST, &[9; 32], LOGIN_C).unwrap();
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        assert_eq!(h.drop_mine(LOGIN_C), 1);
        let reused = h.hold(USER, &[2; 32], LOGIN_B).unwrap();
        assert_eq!(reused, first, "the test needs the later hold in the lower slot");
        assert_eq!(h.seed_for(USER), Some(&[2; 32]));
    }

    /// A slot's next occupant is another owner: the same slot under a new
    /// generation holds nothing of the old one's, and its death takes
    /// nothing of the old one's either.
    #[test]
    fn an_owner_is_the_whole_identity_not_the_slot() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        assert_eq!(h.drop_mine(LOGIN_C), 0);
        dead.borrow_mut().insert(LOGIN_C);
        h.reap_dead();
        assert_eq!(h.seed_for(USER), Some(&[1; 32]));
    }

    #[test]
    fn a_hold_replaces_the_same_owners_key() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        h.hold(GUEST, &[2; 32], LOGIN_A).unwrap();
        assert_eq!(uids(&h), [GUEST]);
        assert_eq!(h.seed_for(USER), None);
    }

    /// A full table refuses a hold, evicting nothing, and takes one again
    /// once a key has gone.
    #[test]
    fn a_full_table_refuses_and_evicts_nothing() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        for i in 0..NET_KEY_MAX as u64 {
            assert!(h.hold(USER, &[i as u8; 32], (1 << 8) | (10 + i)).is_some());
        }
        assert_eq!(h.hold(GUEST, &[0xee; 32], LOGIN_A), None);
        assert_eq!(uids(&h), vec![USER; NET_KEY_MAX]);
        assert_eq!(h.drop_mine((1 << 8) | 10), 1);
        assert!(h.hold(GUEST, &[0xee; 32], LOGIN_A).is_some());
    }

    #[test]
    fn a_drop_by_handle_is_the_holders_or_roots() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        let a = h.hold(USER, &[1; 32], LOGIN_A).unwrap() as u64;
        let b = h.hold(GUEST, &[2; 32], LOGIN_B).unwrap() as u64;
        assert_eq!(h.drop_handle(a, LOGIN_B, GUEST), syscall_abi::NET_KEY_DENIED);
        assert!(h.holds_live(USER));
        assert_eq!(h.drop_handle(a, LOGIN_A, USER), syscall_abi::NET_KEY_OK);
        assert_eq!(h.drop_handle(a, LOGIN_A, USER), syscall_abi::NET_KEY_NOT_HELD);
        assert_eq!(h.drop_handle(b, LOGIN_A, 0), syscall_abi::NET_KEY_OK, "root drops any key");
        assert_eq!(h.drop_handle(NET_KEY_MAX as u64, LOGIN_A, 0), syscall_abi::NET_KEY_NOT_HELD);
        assert_eq!(h.drop_handle(u64::MAX, LOGIN_A, 0), syscall_abi::NET_KEY_NOT_HELD);
    }

    /// The seed is zeros once its entry has gone. Read through a
    /// `ManuallyDrop`, so the bytes are still this test's to read after the
    /// drop has run.
    #[test]
    fn a_dropped_keys_seed_is_wiped() {
        let mut k = core::mem::ManuallyDrop::new(HeldKey { uid: USER, seed: [0x5a; 32], owner: LOGIN_A, seq: 0 });
        // SAFETY: dropped once, here, and never again (ManuallyDrop).
        unsafe { core::mem::ManuallyDrop::drop(&mut k) };
        assert_eq!(k.seed, [0; 32]);
    }
}

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
//!   themselves, a list (`uids`) does not show it, and the next hold reaps it,
//!   so the dead never fill the table against the living.
//! - **Every seed is wiped** when its entry goes, whichever way it goes.
//!
//! What a held key means: while `user` is logged in here, anything running as
//! uid 1000 can have credentials signed with it. That is no more than root on
//! this node can do already; what the plan closes is a node acting for users
//! who are NOT logged in on it.
//!
//! Since step 8 the table also says what a parked request may do with a
//! key: [`Standing`] is whose request it is, made at the park; [`Held::at_send`]
//! is the credential its frame carries, or that it is not sent; and
//! [`Held::stands`] is whether a run sent under a credential is still its
//! user's. [`cut_short`] and [`end_with_note`] are how such a run ends here.
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

/// The table. In `netd`'s `Tables` for the whole boot (its heap region since
/// 2026-11-11, `serve`'s frame before; no mutable statics); a
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
    /// owner's live key: the dead are reaped here first, so a table full of
    /// killed logins' keys does not refuse a live login in the wake that
    /// killed them.
    pub fn hold(&mut self, uid: u32, seed: &[u8; 32], owner: u64) -> Option<usize> {
        self.reap_dead();
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
    fn seed_for(&self, uid: u32) -> Option<&[u8; 32]> {
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

    /// A `cpu` run's park: the run is its user's from the moment they typed
    /// `cpu`, so it is attested from here on if a key is held for them now
    /// and the machine dialled has a known key to bind the credential to
    /// (`anchored`). A key lost before the send then ends the run
    /// ([`Held::at_send`]) where it would otherwise go out unattested and
    /// outlive its user.
    pub fn park_run(&self, uid: u32, anchored: bool) -> Standing {
        Standing { uid, anchored, attested: anchored && self.holds_live(uid) }
    }

    /// What a parked request's signed frame carries, decided at the send for
    /// the uid its park recorded. A credential when a live login holds that
    /// user's key and the export's key was known at the park, and the
    /// request is attested from then on; nothing when none is held; and a
    /// request attested at its park whose key has gone since is not sent.
    pub fn at_send(&self, standing: &mut Standing) -> AtSend<'_> {
        match self.seed_of(standing) {
            Some(seed) => {
                standing.attested = true;
                AtSend::Credential(seed)
            }
            None if standing.attested => AtSend::Refused,
            None => AtSend::Bare,
        }
    }

    /// The seed that signs for a parked request's user, if a live login
    /// holds one and the export's key was known at the park. For a frame
    /// whose sending is already decided (a session's second phase, the verb
    /// held behind an `NP_SESSION` that went out): it refuses nothing, which
    /// is [`Held::at_send`]'s to do.
    ///
    /// Every way to a seed takes a [`Standing`], and none takes a uid. That
    /// is a shape for review to hold, not a guarantee: a `Standing` can be
    /// made wherever a uid is, so a `Standing::parked` outside a park is the
    /// thing to look for.
    pub fn seed_of(&self, standing: &Standing) -> Option<&[u8; 32]> {
        if !standing.anchored {
            return None;
        }
        self.seed_for(standing.uid)
    }

    /// Whether a parked request still stands: one that went out, or was
    /// parked, under its user's credential stands only while a live login
    /// holds that user's key, asked now and not after the idle pass. One
    /// never attested stands whatever is held: nobody's key vouched for it.
    pub fn stands(&self, standing: &Standing) -> bool {
        !standing.attested || self.holds_live(standing.uid)
    }

    /// The uids with a key held by a live login, into `out`; returns how many
    /// (the diagnostic `NETOP_KEY_LIST`). Never the keys. A dead login's key
    /// is not listed, reaped or not: a list answered in the wake of a kill
    /// shows what signs, as `seed_for` reads it.
    pub fn uids(&self, out: &mut [u32; NET_KEY_MAX]) -> usize {
        let mut n = 0;
        for k in self.slots.iter().flatten().filter(|k| self.tasks.alive(k.owner)) {
            out[n] = k.uid;
            n += 1;
        }
        n
    }
}

/// Whose a parked request is, and whether that user's key vouches for it.
///
/// To be made at the park and nowhere else ([`Standing::parked`],
/// [`Held::park_run`]), from the uid of the message being handled then and
/// from whether the export's key is known. The send, which runs later in the
/// same drain of the mailbox or in another, is given only this: nothing here
/// takes a uid or an anchor at the send, so the park and the send cannot
/// disagree on either (user-keys step 8, "the uid is captured at the park";
/// no driven boot could land another user's message between the two, so the
/// rule had no check, and has this shape in its place). The type cannot stop
/// one being made elsewhere; see [`Held::seed_of`].
pub struct Standing {
    uid: u32,
    /// The export's key was known at the park: there is something to bind a
    /// credential to.
    anchored: bool,
    attested: bool,
}

impl Standing {
    /// A park of anything but a run: not attested until its send carries a
    /// credential.
    pub const fn parked(uid: u32, anchored: bool) -> Self {
        Standing { uid, anchored, attested: false }
    }

    /// The uid the park recorded.
    pub const fn uid(&self) -> u32 {
        self.uid
    }

    /// Whether the request was parked, or sent, under its user's credential.
    pub const fn attested(&self) -> bool {
        self.attested
    }
}

/// [`Held::at_send`]'s answer.
#[derive(Debug, PartialEq, Eq)]
pub enum AtSend<'a> {
    /// Sign the user's credential with this seed.
    Credential(&'a [u8; 32]),
    /// No key is held for the user: the frame goes on the machine's word.
    Bare,
    /// Attested at the park and the key has gone: do not send.
    Refused,
}

/// Whether a run is cut short here because its user has gone: it no longer
/// stands, and it has not finished in this same pass. A run that finished is
/// complete, whatever became of its user since, and is told nothing more;
/// `stands` is not asked for it, since asking costs the kernel's answer for
/// each of the user's keys.
pub fn cut_short(finished: bool, stands: impl FnOnce() -> bool) -> bool {
    !finished && !stands()
}

/// End a cut-short run's output with `note`, returning the new length. The
/// note always fits: where the output has filled `out` it replaces the
/// output's tail, since a run cut off with no reason given reads as one that
/// ended on its own. `note` is no longer than `out` (the caller asserts it at
/// build time; a longer one is cut to fit here rather than trusted).
pub fn end_with_note(out: &mut [u8], len: usize, note: &[u8]) -> usize {
    let n = note.len().min(out.len());
    let at = len.min(out.len() - n);
    out[at..at + n].copy_from_slice(&note[..n]);
    at + n
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
        assert!(h.slots.iter().flatten().any(|k| k.uid == USER), "not reaped yet: the key is still in the table");
        assert_eq!(h.seed_for(USER), None, "a dead login's key signed");
        assert!(!h.holds_live(USER), "a dead login's key counted as held");
        assert_eq!(uids(&h), [] as [u32; 0], "a dead login's key was listed");

        h.reap_dead();
        assert!(h.slots.iter().all(|k| k.is_none()));
    }

    /// A table full of dead logins' keys, none reaped: a live login is held
    /// all the same, and the dead ones' seeds are gone with their entries.
    #[test]
    fn a_table_full_of_the_dead_takes_a_live_login() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        for i in 0..NET_KEY_MAX as u64 {
            h.hold(GUEST, &[i as u8; 32], (1 << 8) | (10 + i)).unwrap();
            dead.borrow_mut().insert((1 << 8) | (10 + i));
        }
        assert!(h.hold(USER, &[7; 32], LOGIN_A).is_some(), "the dead refused the living");
        assert_eq!(uids(&h), [USER]);
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

    /// The window between a run's park and its send: the key went, so the
    /// run is not sent, and stays attested so its caller is told why.
    #[test]
    fn a_run_whose_key_went_before_the_send_is_not_sent() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        let mut run = h.park_run(USER, true);
        assert!(run.attested());

        dead.borrow_mut().insert(LOGIN_A);
        assert_eq!(h.at_send(&mut run), AtSend::Refused);
        assert!(run.attested(), "the refusal is read as a run's, by this flag");
        assert!(!h.stands(&run));
    }

    /// The same window with the key dropped outright, a logout, not a death.
    #[test]
    fn a_run_whose_user_logged_out_before_the_send_is_not_sent() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        let mut run = h.park_run(USER, true);
        h.drop_mine(LOGIN_A);
        assert_eq!(h.at_send(&mut run), AtSend::Refused);
    }

    /// A run parked with no key held was never its user's key's to vouch
    /// for: it goes bare, stands whatever is held, and a key held later
    /// makes its send a credentialed one.
    #[test]
    fn a_run_parked_with_no_key_goes_bare_and_stands() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        let mut run = h.park_run(USER, true);
        assert!(!run.attested());
        assert_eq!(h.at_send(&mut run), AtSend::Bare);
        assert!(!run.attested());
        assert!(h.stands(&run));

        let mut later = h.park_run(USER, true);
        h.hold(USER, &[3; 32], LOGIN_A).unwrap();
        assert_eq!(h.at_send(&mut later), AtSend::Credential(&[3; 32]));
        assert!(later.attested());
    }

    /// No known key for the machine dialled, nothing to bind a credential
    /// to: the run is not attested at the park and its send is bare, key
    /// held or not.
    #[test]
    fn no_anchor_no_credential() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        let mut run = h.park_run(USER, false);
        assert!(!run.attested());
        assert_eq!(h.at_send(&mut run), AtSend::Bare);
        assert_eq!(h.seed_of(&run), None);
        assert!(!run.attested());
    }

    /// An attested request stands while its user is held and not after; the
    /// send marks an ordinary park attested.
    #[test]
    fn an_attested_request_stands_only_while_its_user_is_held() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        let mut verb = Standing::parked(USER, true);
        assert!(!verb.attested());
        assert_eq!(h.at_send(&mut verb), AtSend::Credential(&[1; 32]));
        assert!(verb.attested());
        assert!(h.stands(&verb));
        dead.borrow_mut().insert(LOGIN_A);
        assert!(!h.stands(&verb), "an attested run counted past its user's death");
    }

    /// The credential is for the uid the park recorded, whoever else holds a
    /// key and whoever was held last: root's key never signs for `user`, nor
    /// `user`'s for `guest`, and a user with none goes bare beside two who
    /// have one.
    #[test]
    fn the_key_that_signs_is_the_parked_uids() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        let mut user = Standing::parked(USER, true);
        let mut guest = Standing::parked(GUEST, true);
        let mut root = Standing::parked(0, true);
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        h.hold(0, &[9; 32], LOGIN_B).unwrap();
        assert_eq!(h.at_send(&mut user), AtSend::Credential(&[1; 32]));
        assert_eq!(h.at_send(&mut root), AtSend::Credential(&[9; 32]));
        assert_eq!(h.at_send(&mut guest), AtSend::Bare);
        assert_eq!(h.seed_of(&user), Some(&[1; 32]));
        assert_eq!(user.uid(), USER);
    }

    /// A run that finished in the pass its user went is complete, and gets
    /// no note; one still running is cut short; one that stands is neither.
    #[test]
    fn only_an_unfinished_run_is_cut_short() {
        assert!(!cut_short(true, || false), "a finished run was told it was ended");
        assert!(cut_short(false, || false));
        assert!(!cut_short(false, || true));
        assert!(!cut_short(true, || true));
    }

    /// A finished run's standing is not asked at all.
    #[test]
    fn a_finished_run_asks_nothing() {
        assert!(!cut_short(true, || panic!("a finished run's standing was asked")));
    }

    /// An ordinary park with no known export key carries no credential, key
    /// held or not, and is not marked attested by its send.
    #[test]
    fn an_unanchored_park_goes_bare() {
        let dead = RefCell::new(BTreeSet::new());
        let mut h = Held::new(Dead(&dead));
        h.hold(USER, &[1; 32], LOGIN_A).unwrap();
        let mut verb = Standing::parked(USER, false);
        assert_eq!(h.at_send(&mut verb), AtSend::Bare);
        assert!(!verb.attested());
        assert_eq!(h.seed_of(&verb), None);
    }

    #[test]
    fn the_note_follows_the_output_when_there_is_room() {
        let mut out = [b'.'; 16];
        out[..3].copy_from_slice(b"abc");
        assert_eq!(end_with_note(&mut out, 3, b"NOTE"), 7);
        assert_eq!(&out[..7], b"abcNOTE");
    }

    /// Output that filled the buffer, or came within a note of filling it,
    /// loses its tail to the note: the reason is never the part dropped.
    #[test]
    fn the_note_replaces_the_tail_of_a_full_output() {
        let mut out = [b'x'; 8];
        assert_eq!(end_with_note(&mut out, 8, b"NOTE"), 8);
        assert_eq!(&out, b"xxxxNOTE");
        let mut out = [b'x'; 8];
        assert_eq!(end_with_note(&mut out, 6, b"NOTE"), 8);
        assert_eq!(&out, b"xxxxNOTE");
        let mut out = [b'x'; 8];
        assert_eq!(end_with_note(&mut out, 0, b"NOTE"), 4);
        assert_eq!(&out[..4], b"NOTE");
        let mut out = [b'x'; 2];
        assert_eq!(end_with_note(&mut out, 2, b"NOTE"), 2, "a note longer than the buffer is cut, not a panic");
    }
}

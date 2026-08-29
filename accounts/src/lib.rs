//! Shared account-database logic for Ouroboros: `/etc/passwd` and `/etc/group`
//! parsing/formatting, SHA-256 password hashing, salt derivation, and
//! name<->id lookups.
//!
//! This is the single home for the machinery the users/permissions surface
//! needs, so the shell's `login` and every `/bin` account tool
//! (`id`/`su`/`passwd`/`useradd`/`groupadd`/`usermod`) share one implementation
//! instead of copying it. It is **pure**: no file I/O, no syscalls. Callers read
//! `/etc/passwd`/`/etc/group` their own way (the shell via its fs layer, `/bin`
//! via `ulib`) and hand the byte buffer in; the salt entropy (`MONOTONIC_US`) is
//! likewise passed in, not read here.
//!
//! ## File formats
//! `/etc/passwd`, one account per line:
//!   `name:uid:gid:home:salt_hex:hash_hex`   where `hash = SHA-256(salt || password)`
//! `/etc/group`, one group per line:
//!   `name:gid:members`   (`members` = comma-separated usernames, may be empty)
//!
//! The group file omits the traditional Unix `passwd` (`x`) field to match this
//! project's own `passwd` shape (no per-group password); it is our format, not
//! POSIX's. Group *membership* here is informational (names + the members list):
//! enforcement uses each task's single kernel-owned primary gid, so "assign a
//! user to a group" means setting that user's **primary** gid (the passwd `gid`
//! field). Full supplementary-group membership is a deliberate later tier (it
//! needs the kernel identity to carry a group list); see the users/permissions
//! roadmap arc.
//!
//! ## PIE safety
//! Everything is byte-only (no `&str` slicing by a runtime index, which would
//! pull in `core::fmt`'s panic formatter and an `R_AARCH64_ABS64` relocation the
//! `-pie` link rejects — the recurring trap). The password check is
//! constant-time ([`digest_eq`]).

#![cfg_attr(not(test), no_std)]

pub mod sha256;

pub use sha256::{digest_eq, sha256_two, DIGEST};

/// Maximum salt length we store/parse (bytes). We generate 8-byte salts; the
/// parser accepts up to this so a hand-written or future longer salt still reads.
pub const SALT_MAX: usize = 16;

/// The conventional first uid/gid for a normal (non-system) account, the base
/// [`next_free_uid`]/[`next_free_gid`] allocate from — matching the dev
/// accounts' `1000` and the common Linux `UID_MIN`.
pub const FIRST_NORMAL_ID: u32 = 1000;

/// One parsed `/etc/passwd` account. `name`/`home` borrow the passwd buffer; the
/// salt and hash are decoded into owned fixed arrays (small, no heap).
pub struct Account<'a> {
    pub name: &'a [u8],
    pub uid: u32,
    pub gid: u32,
    pub home: &'a [u8],
    pub salt: [u8; SALT_MAX],
    pub salt_len: usize,
    pub hash: [u8; DIGEST],
}

impl Account<'_> {
    /// Constant-time check of `password` against this account's stored salt+hash.
    pub fn verify(&self, password: &[u8]) -> bool {
        let digest = sha256_two(&self.salt[..self.salt_len], password);
        digest_eq(&digest, &self.hash)
    }
}

/// One parsed `/etc/group` entry. All fields borrow the group buffer.
pub struct Group<'a> {
    pub name: &'a [u8],
    pub gid: u32,
    /// Comma-separated member usernames, exactly as stored (may be empty).
    pub members: &'a [u8],
}

/// Iterate `/etc/passwd` accounts, skipping blank lines. Returns `None` for a
/// malformed line rather than erroring the whole file — callers use the finder
/// helpers, which just skip a bad line.
fn parse_account(line: &[u8]) -> Option<Account<'_>> {
    let mut f = line.split(|&c| c == b':');
    let name = f.next()?;
    if name.is_empty() {
        return None;
    }
    let uid = parse_dec(f.next()?)? as u32;
    let gid = parse_dec(f.next()?)? as u32;
    let home = f.next()?;
    let salt_hex = f.next()?;
    let hash_hex = f.next()?;
    let mut salt = [0u8; SALT_MAX];
    let salt_len = hex_decode(salt_hex, &mut salt)?;
    let mut hash = [0u8; DIGEST];
    if hex_decode(hash_hex, &mut hash)? != DIGEST {
        return None;
    }
    Some(Account { name, uid, gid, home, salt, salt_len, hash })
}

fn parse_group(line: &[u8]) -> Option<Group<'_>> {
    let mut f = line.split(|&c| c == b':');
    let name = f.next()?;
    if name.is_empty() {
        return None;
    }
    let gid = parse_dec(f.next()?)? as u32;
    // members is optional (a group with no listed members has an empty field or
    // no field at all).
    let members = f.next().unwrap_or(b"");
    Some(Group { name, gid, members })
}

/// Find the account named `name` in a `/etc/passwd` buffer.
pub fn find_user_by_name<'a>(passwd: &'a [u8], name: &[u8]) -> Option<Account<'a>> {
    for line in passwd.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(acct) = parse_account(line) {
            if acct.name == name {
                return Some(acct);
            }
        }
    }
    None
}

/// Find the first account with uid `uid` (for id/ls name resolution).
pub fn find_user_by_uid(passwd: &[u8], uid: u32) -> Option<Account<'_>> {
    for line in passwd.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(acct) = parse_account(line) {
            if acct.uid == uid {
                return Some(acct);
            }
        }
    }
    None
}

/// Find the group named `name` in a `/etc/group` buffer.
pub fn find_group_by_name<'a>(group: &'a [u8], name: &[u8]) -> Option<Group<'a>> {
    for line in group.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(g) = parse_group(line) {
            if g.name == name {
                return Some(g);
            }
        }
    }
    None
}

/// Find the first group with gid `gid` (for id/ls group-name resolution).
pub fn find_group_by_gid(group: &[u8], gid: u32) -> Option<Group<'_>> {
    for line in group.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(g) = parse_group(line) {
            if g.gid == gid {
                return Some(g);
            }
        }
    }
    None
}

/// Collect the gids of every group in `group` whose member list contains
/// `name`, skipping `primary_gid` (already carried as the task's primary) and
/// any duplicate. Writes into `out` and returns how many were written; a user in
/// more groups than `out` holds keeps the first ones, in file order.
///
/// This is the `/etc/group` half of supplementary group membership: the members
/// field stopped being informational the moment the kernel could carry a group
/// *list*, and this is what turns it into one.
pub fn supplementary_gids(group: &[u8], name: &[u8], primary_gid: u32, out: &mut [u32]) -> usize {
    let mut n = 0usize;
    for line in group.split(|&c| c == b'\n') {
        if line.is_empty() || n == out.len() {
            continue;
        }
        let Some(g) = parse_group(line) else { continue };
        if g.gid == primary_gid || out[..n].contains(&g.gid) {
            continue;
        }
        if members_contain(g.members, name) {
            out[n] = g.gid;
            n += 1;
        }
    }
    n
}

/// Whether a comma-separated member list contains exactly `name` (not merely as
/// a substring - `bob` must not match `bobby`).
fn members_contain(members: &[u8], name: &[u8]) -> bool {
    members.split(|&c| c == b',').any(|m| m == name)
}

/// Add `name` to `group_name`'s member list, returning the rewritten file in
/// `out` and whether anything changed (`false` if the group doesn't exist or
/// already lists the user). The `/etc/group` write behind `usermod -G`.
pub fn add_group_member(
    group: &[u8],
    out: &mut [u8],
    group_name: &[u8],
    name: &[u8],
) -> Option<(usize, bool)> {
    let Some(g) = find_group_by_name(group, group_name) else {
        return Some((0, false));
    };
    if members_contain(g.members, name) {
        return Some((0, false));
    }
    let mut line = [0u8; 256];
    let mut w = Writer::new(&mut line);
    w.bytes(g.name)?;
    w.byte(b':')?;
    w.dec(g.gid as u64)?;
    w.byte(b':')?;
    if !g.members.is_empty() {
        w.bytes(g.members)?;
        w.byte(b',')?;
    }
    w.bytes(name)?;
    let len = w.len;
    let (n, replaced) = replace_line(group, out, group_name, &line[..len])?;
    Some((n, replaced))
}

/// Remove `name` from every group's member list, returning the rewritten file
/// and whether anything changed. Used by `usermod -G` to make the given list the
/// user's *complete* supplementary membership rather than an addition.
pub fn remove_group_member_everywhere(
    group: &[u8],
    out: &mut [u8],
    name: &[u8],
) -> Option<(usize, bool)> {
    let mut w = 0usize;
    let mut changed = false;
    for line in group.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        match parse_group(line) {
            Some(g) if members_contain(g.members, name) => {
                changed = true;
                let mut buf = [0u8; 256];
                let mut lw = Writer::new(&mut buf);
                lw.bytes(g.name)?;
                lw.byte(b':')?;
                lw.dec(g.gid as u64)?;
                lw.byte(b':')?;
                let mut first = true;
                for m in g.members.split(|&c| c == b',') {
                    if m.is_empty() || m == name {
                        continue;
                    }
                    if !first {
                        lw.byte(b',')?;
                    }
                    first = false;
                    lw.bytes(m)?;
                }
                let len = lw.len;
                for &b in &buf[..len] {
                    *out.get_mut(w)? = b;
                    w += 1;
                }
            }
            _ => {
                for &b in line {
                    *out.get_mut(w)? = b;
                    w += 1;
                }
            }
        }
        *out.get_mut(w)? = b'\n';
        w += 1;
    }
    Some((w, changed))
}

/// True if `passwd` already has an account named `name` (useradd's collision
/// check).
pub fn user_exists(passwd: &[u8], name: &[u8]) -> bool {
    find_user_by_name(passwd, name).is_some()
}

/// True if `group` already has a group named `name` (groupadd's collision check).
pub fn group_exists(group: &[u8], name: &[u8]) -> bool {
    find_group_by_name(group, name).is_some()
}

/// The next free uid at or above [`FIRST_NORMAL_ID`]: one past the highest uid
/// in that range (so ids stay stable and don't reuse a just-deleted slot).
pub fn next_free_uid(passwd: &[u8]) -> u32 {
    let mut max = FIRST_NORMAL_ID - 1;
    for line in passwd.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(acct) = parse_account(line) {
            if acct.uid >= FIRST_NORMAL_ID && acct.uid > max {
                max = acct.uid;
            }
        }
    }
    max + 1
}

/// The next free gid at or above [`FIRST_NORMAL_ID`] in a `/etc/group` buffer.
pub fn next_free_gid(group: &[u8]) -> u32 {
    let mut max = FIRST_NORMAL_ID - 1;
    for line in group.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        if let Some(g) = parse_group(line) {
            if g.gid >= FIRST_NORMAL_ID && g.gid > max {
                max = g.gid;
            }
        }
    }
    max + 1
}

/// Derive an 8-byte salt from a `MONOTONIC_US` sample (or any per-call entropy).
///
/// **This is a weak salt** by design of the current milestone: the only entropy
/// source available to userland is the monotonic clock, which is predictable.
/// We hash it (spreading the low-entropy input across all 8 bytes) so two
/// registrations a microsecond apart still differ, but an attacker who can
/// bound the registration time can bound the salt. The real fix is a
/// virtio-entropy driver + a `RANDOM` syscall (a noted follow-up); this keeps
/// the salt *per-account-unique* meanwhile, which is what defeats a shared
/// rainbow table across our own accounts.
/// Build a password salt from the best entropy the caller could obtain, and say
/// which it was: `(salt, strong)`.
///
/// `random` is eight bytes from a hardware RNG (`ulib::random_bytes8`) when the
/// machine has one — those *are* the salt, used directly, since they are already
/// uniform. `None` falls back to [`make_salt`]'s clock derivation, and `strong`
/// comes back `false` so the caller can **say so out loud** rather than quietly
/// storing a guessable salt. This crate stays pure: the caller does the syscall
/// and passes the result in.
pub fn salt_from(random: Option<[u8; 8]>, clock: u64) -> ([u8; 8], bool) {
    match random {
        Some(bytes) => (bytes, true),
        None => (make_salt(clock), false),
    }
}

pub fn make_salt(entropy: u64) -> [u8; 8] {
    let digest = sha256_two(&entropy.to_le_bytes(), b"ouroboros-salt");
    let mut salt = [0u8; 8];
    salt.copy_from_slice(&digest[..8]);
    salt
}

/// `SHA-256(salt || password)` — the stored hash for a new/changed password.
pub fn hash_password(salt: &[u8], password: &[u8]) -> [u8; DIGEST] {
    sha256_two(salt, password)
}

/// Format one `/etc/passwd` line into `buf` (no trailing newline), returning its
/// length, or `None` if it wouldn't fit.
#[allow(clippy::too_many_arguments)]
pub fn format_account_line(
    buf: &mut [u8],
    name: &[u8],
    uid: u32,
    gid: u32,
    home: &[u8],
    salt: &[u8],
    hash: &[u8],
) -> Option<usize> {
    let mut w = Writer::new(buf);
    w.bytes(name)?;
    w.byte(b':')?;
    w.dec(uid as u64)?;
    w.byte(b':')?;
    w.dec(gid as u64)?;
    w.byte(b':')?;
    w.bytes(home)?;
    w.byte(b':')?;
    w.hex(salt)?;
    w.byte(b':')?;
    w.hex(hash)?;
    Some(w.len)
}

/// Format one `/etc/group` line into `buf` (no trailing newline).
pub fn format_group_line(buf: &mut [u8], name: &[u8], gid: u32, members: &[u8]) -> Option<usize> {
    let mut w = Writer::new(buf);
    w.bytes(name)?;
    w.byte(b':')?;
    w.dec(gid as u64)?;
    w.byte(b':')?;
    w.bytes(members)?;
    Some(w.len)
}

/// Rebuild an account/group file into `out`: copy every non-empty line of `src`,
/// replacing the first line whose leading colon-field equals `name` with
/// `replacement`. Returns `(new_len, replaced)` or `None` if it wouldn't fit.
/// The output is normalized to one `\n` per line (trailing newline included) -
/// used by `passwd`/`usermod` to rewrite one account in place.
pub fn replace_line(
    src: &[u8],
    out: &mut [u8],
    name: &[u8],
    replacement: &[u8],
) -> Option<(usize, bool)> {
    let mut w = 0usize;
    let mut replaced = false;
    for line in src.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        let first = line.split(|&c| c == b':').next().unwrap_or(b"");
        let emit: &[u8] = if !replaced && first == name {
            replaced = true;
            replacement
        } else {
            line
        };
        for &b in emit {
            *out.get_mut(w)? = b;
            w += 1;
        }
        *out.get_mut(w)? = b'\n';
        w += 1;
    }
    Some((w, replaced))
}

/// Rebuild an account/group file into `out` with the **first** line whose
/// leading colon-field equals `name` removed. Returns `(new_len, removed)`, or
/// `None` if it wouldn't fit. Output is normalized to one `\n` per line, like
/// [`replace_line`].
///
/// The inverse of [`append_line`]: `useradd` uses it to roll back a group it
/// added when a later step fails, so a half-made account never reaches the
/// database.
pub fn remove_line(src: &[u8], out: &mut [u8], name: &[u8]) -> Option<(usize, bool)> {
    let mut w = 0usize;
    let mut removed = false;
    for line in src.split(|&c| c == b'\n') {
        if line.is_empty() {
            continue;
        }
        let first = line.split(|&c| c == b':').next().unwrap_or(b"");
        if !removed && first == name {
            removed = true;
            continue;
        }
        for &b in line {
            *out.get_mut(w)? = b;
            w += 1;
        }
        *out.get_mut(w)? = b'\n';
        w += 1;
    }
    Some((w, removed))
}

/// Copy `src` into `out` and append `line` (with a trailing newline), ensuring
/// exactly one newline joins them. Returns the new length or `None` if it
/// wouldn't fit - used by `useradd`/`groupadd` to add an entry.
pub fn append_line(src: &[u8], out: &mut [u8], line: &[u8]) -> Option<usize> {
    let mut w = 0usize;
    for &b in src {
        *out.get_mut(w)? = b;
        w += 1;
    }
    if w > 0 && out[w - 1] != b'\n' {
        *out.get_mut(w)? = b'\n';
        w += 1;
    }
    for &b in line {
        *out.get_mut(w)? = b;
        w += 1;
    }
    *out.get_mut(w)? = b'\n';
    w += 1;
    Some(w)
}

// --- small byte-buffer writer (PIE-safe, no core::fmt) ---

struct Writer<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> Writer<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Writer { buf, len: 0 }
    }
    fn byte(&mut self, b: u8) -> Option<()> {
        if self.len >= self.buf.len() {
            return None;
        }
        self.buf[self.len] = b;
        self.len += 1;
        Some(())
    }
    fn bytes(&mut self, src: &[u8]) -> Option<()> {
        for &b in src {
            self.byte(b)?;
        }
        Some(())
    }
    fn dec(&mut self, mut v: u64) -> Option<()> {
        let mut tmp = [0u8; 20];
        let mut i = tmp.len();
        if v == 0 {
            return self.byte(b'0');
        }
        while v > 0 {
            i -= 1;
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        self.bytes(&tmp[i..])
    }
    fn hex(&mut self, src: &[u8]) -> Option<()> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for &b in src {
            self.byte(HEX[(b >> 4) as usize])?;
            self.byte(HEX[(b & 0xf) as usize])?;
        }
        Some(())
    }
}

// --- shared codecs ---

/// Decimal `u64` from bytes, or `None` on empty/non-digit/overflow.
pub fn parse_dec(b: &[u8]) -> Option<u64> {
    if b.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u64)?;
    }
    Some(v)
}

/// Decode a hex string into `out`, returning the byte count, or `None` on an odd
/// length, an oversized input, or a non-hex digit.
pub fn hex_decode(hex: &[u8], out: &mut [u8]) -> Option<usize> {
    if !hex.len().is_multiple_of(2) || hex.len() / 2 > out.len() {
        return None;
    }
    for i in 0..hex.len() / 2 {
        let hi = hex_val(hex[i * 2])?;
        let lo = hex_val(hex[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(hex.len() / 2)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A stored hash is always exactly 32 bytes (64 hex chars); the parser skips a
    // line whose hash isn't, so these fixtures use full-length hex.
    const H: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    const GROUP: &[u8] = b"root:0:\nuser:1000:\nstaff:1001:alice,bob\n";

    fn passwd() -> String {
        format!(
            "root:0:0:/:00:{H}\nuser:1000:1000:/Users/user:1122:{H}\nbob:1001:1001:/Users/bob:aabb:{H}\n"
        )
    }

    #[test]
    fn find_by_name_and_uid() {
        let p = passwd();
        let pw = p.as_bytes();
        let u = find_user_by_name(pw, b"user").unwrap();
        assert_eq!(u.uid, 1000);
        assert_eq!(u.gid, 1000);
        assert_eq!(u.home, b"/Users/user");
        assert_eq!(find_user_by_uid(pw, 1001).unwrap().name, b"bob");
        assert!(find_user_by_name(pw, b"nobody").is_none());
    }

    #[test]
    fn group_lookups() {
        assert_eq!(find_group_by_name(GROUP, b"staff").unwrap().gid, 1001);
        assert_eq!(find_group_by_gid(GROUP, 1000).unwrap().name, b"user");
        assert_eq!(find_group_by_name(GROUP, b"staff").unwrap().members, b"alice,bob");
        assert!(group_exists(GROUP, b"root"));
        assert!(!group_exists(GROUP, b"wheel"));
    }

    #[test]
    fn next_free_ids() {
        assert_eq!(next_free_uid(passwd().as_bytes()), 1002); // max normal uid is 1001
        assert_eq!(next_free_gid(GROUP), 1002);
        assert_eq!(next_free_uid(b"root:0:0:/:aa:bb\n"), 1000); // only a system acct
    }

    #[test]
    fn hash_verify_roundtrip() {
        // Format an account with a fresh salt+hash, parse it back, verify.
        let salt = make_salt(123456789);
        let hash = hash_password(&salt, b"hunter2");
        let mut line = [0u8; 256];
        let n = format_account_line(&mut line, b"carol", 1005, 1005, b"/Users/carol", &salt, &hash)
            .unwrap();
        let acct = find_user_by_name(&line[..n], b"carol").unwrap();
        assert_eq!(acct.uid, 1005);
        assert!(acct.verify(b"hunter2"));
        assert!(!acct.verify(b"wrong"));
    }

    #[test]
    fn supplementary_group_membership() {
        const G: &[u8] = b"root:0:\nuser:1000:\nstaff:1500:alice,bob\ndevs:1600:bob\nbobby:1700:bobby\n";
        let mut out = [0u32; 8];
        // bob is in staff and devs; his primary (1000) is skipped, and the
        // group literally named "bobby" must not match the user "bob".
        let n = supplementary_gids(G, b"bob", 1000, &mut out);
        assert_eq!(&out[..n], &[1500, 1600]);
        // alice is only in staff; her own primary group is excluded.
        let n = supplementary_gids(G, b"alice", 1500, &mut out);
        assert_eq!(n, 0);
        let n = supplementary_gids(G, b"alice", 1000, &mut out);
        assert_eq!(&out[..n], &[1500]);
        // Someone in no group at all.
        assert_eq!(supplementary_gids(G, b"nobody", 1000, &mut out), 0);
        // A tight output buffer keeps the first ones rather than overflowing.
        let mut one = [0u32; 1];
        assert_eq!(supplementary_gids(G, b"bob", 1000, &mut one), 1);
        assert_eq!(one[0], 1500);
    }

    #[test]
    fn group_membership_edits() {
        const G: &[u8] = b"root:0:\nstaff:1500:alice\ndevs:1600:\n";
        let mut out = [0u8; 512];
        // Append to a non-empty list, and to an empty one.
        let (n, changed) = add_group_member(G, &mut out, b"staff", b"bob").unwrap();
        assert!(changed);
        assert_eq!(find_group_by_name(&out[..n], b"staff").unwrap().members, b"alice,bob");
        let (n, changed) = add_group_member(G, &mut out, b"devs", b"bob").unwrap();
        assert!(changed);
        assert_eq!(find_group_by_name(&out[..n], b"devs").unwrap().members, b"bob");
        // Already a member, or no such group: no change.
        assert_eq!(add_group_member(G, &mut out, b"staff", b"alice").unwrap().1, false);
        assert_eq!(add_group_member(G, &mut out, b"ghosts", b"bob").unwrap().1, false);
        // Removal everywhere, leaving the other members intact.
        const G2: &[u8] = b"root:0:\nstaff:1500:alice,bob\ndevs:1600:bob\n";
        let (n, changed) = remove_group_member_everywhere(G2, &mut out, b"bob").unwrap();
        assert!(changed);
        let rebuilt = &out[..n];
        assert_eq!(find_group_by_name(rebuilt, b"staff").unwrap().members, b"alice");
        assert_eq!(find_group_by_name(rebuilt, b"devs").unwrap().members, b"");
        assert_eq!(find_group_by_name(rebuilt, b"root").unwrap().gid, 0);
        // A user in no group leaves the file untouched.
        assert_eq!(remove_group_member_everywhere(G2, &mut out, b"carol").unwrap().1, false);
    }

    #[test]
    fn salt_from_prefers_hardware_entropy() {
        // Real entropy is used verbatim - it is already uniform, so hashing it
        // would only lose the property we went to the device for.
        let bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let (salt, strong) = salt_from(Some(bytes), 12345);
        assert_eq!(salt, bytes);
        assert!(strong);
        // With no device, the clock fallback is used and flagged as weak.
        let (weak_salt, strong) = salt_from(None, 12345);
        assert_eq!(weak_salt, make_salt(12345));
        assert!(!strong);
        // The fallback still differs per call, which is what it is for.
        assert_ne!(salt_from(None, 12345).0, salt_from(None, 12346).0);
    }

    #[test]
    fn salt_is_per_call_unique() {
        // Two different clock samples give different salts (defeats a shared
        // rainbow table across accounts, the point of the weak salt).
        assert_ne!(make_salt(1000), make_salt(1001));
    }

    #[test]
    fn replace_line_rewrites_one() {
        let p = passwd();
        let pw = p.as_bytes();
        let replacement = format!("user:1000:42:/Users/user:1122:{H}");
        let mut out = [0u8; 512];
        let (n, replaced) = replace_line(pw, &mut out, b"user", replacement.as_bytes()).unwrap();
        assert!(replaced);
        let rebuilt = &out[..n];
        // The target's gid changed; the others are untouched.
        assert_eq!(find_user_by_name(rebuilt, b"user").unwrap().gid, 42);
        assert_eq!(find_user_by_name(rebuilt, b"root").unwrap().uid, 0);
        assert_eq!(find_user_by_name(rebuilt, b"bob").unwrap().uid, 1001);
        // A name that isn't present reports replaced == false.
        let (_, r2) = replace_line(pw, &mut out, b"ghost", b"x").unwrap();
        assert!(!r2);
    }

    #[test]
    fn append_line_adds_entry() {
        let mut out = [0u8; 512];
        let n = append_line(GROUP, &mut out, b"wheel:1002:root").unwrap();
        let rebuilt = &out[..n];
        assert_eq!(find_group_by_name(rebuilt, b"wheel").unwrap().gid, 1002);
        assert!(find_group_by_name(rebuilt, b"user").is_some()); // originals kept
        // Appending to an empty file yields just the one line.
        let n2 = append_line(b"", &mut out, b"solo:5:").unwrap();
        assert_eq!(&out[..n2], b"solo:5:\n");
    }

    #[test]
    fn remove_line_drops_one() {
        let mut out = [0u8; 512];
        let (n, removed) = remove_line(GROUP, &mut out, b"user").unwrap();
        assert!(removed);
        let rebuilt = &out[..n];
        assert!(find_group_by_name(rebuilt, b"user").is_none());
        assert_eq!(find_group_by_name(rebuilt, b"staff").unwrap().gid, 1001);
        assert!(find_group_by_name(rebuilt, b"root").is_some());
        // A name that isn't present leaves the file intact, reporting false.
        let (n2, r2) = remove_line(GROUP, &mut out, b"ghost").unwrap();
        assert!(!r2);
        assert_eq!(&out[..n2], GROUP);
        // append_line then remove_line is a round trip (the rollback useradd needs).
        let mut added = [0u8; 512];
        let an = append_line(GROUP, &mut added, b"wheel:1002:").unwrap();
        let (bn, back) = remove_line(&added[..an], &mut out, b"wheel").unwrap();
        assert!(back);
        assert_eq!(&out[..bn], GROUP);
    }

    #[test]
    fn format_group_roundtrip() {
        let mut buf = [0u8; 64];
        let n = format_group_line(&mut buf, b"devs", 2000, b"a,b,c").unwrap();
        assert_eq!(&buf[..n], b"devs:2000:a,b,c");
    }
}

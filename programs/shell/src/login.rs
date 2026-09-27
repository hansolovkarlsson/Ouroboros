//! The shell's **login gate** (users/permissions arc, step 2).
//!
//! At the start of every session `main` calls [`login`], which prompts for a
//! username + password, verifies them against `/etc/passwd`, and - on success -
//! `SET_ID`s this task (the shell, task 0) down from root to that user. Because
//! the kernel remembers the *saved* identity, logout can restore root and
//! re-prompt (see the kernel's `SET_ID` and `tasks::inherit_id`), so the shell
//! never has to leave slot 0.
//!
//! `/etc/passwd` format, one account per line:
//!   `name:uid:gid:home:salt_hex:hash_hex`
//! where `hash = SHA-256(salt || password)`. All parsing, hashing, and the
//! constant-time password check live in the shared [`accounts`] crate (the same
//! logic the `/bin` account tools use); this module only does the keyboard I/O
//! and the `SET_ID`. If `/etc/passwd` is absent (a fresh/formatted disk, or a
//! test image without one), login falls back to a single-user **root** session
//! so the machine stays usable - the standard "no accounts configured"
//! bootstrap.
//!
//! All parsing is byte-only (no `&str` slicing by a runtime index - the PIE
//! relocation trap).

use crate::CWD_SIZE;

const PASSWD_PATH: &str = "/etc/passwd";
/// The password secrets, `name:salt_hex:hash_hex`, mode 0600 and root-owned -
/// which is the point: `/etc/passwd` stays world-readable (every `id`, `ls -l`
/// and `chown` needs the name/uid map) while the hashes an offline cracker
/// wants do not.
const SHADOW_PATH: &str = "/etc/shadow";
const GROUP_PATH: &str = "/etc/group";
/// The cluster's realm: present, this node derives a cluster key at login;
/// absent or not a realm, it derives none and logs in as it always has.
const REALM_PATH: &str = "/etc/cluster/realm";
/// Read cap for `/etc/passwd`, matching the account tools' write cap
/// ([`accounts`]/`useradd` bound writes to `SAFECOPY_MAX`). fsd caps a single
/// inline read at `FS_DATA_MAX` (512), so the shared `read_account_file` loops
/// `fs_read_at` to fill this buffer in 512-byte chunks.
///
/// Overflowing this is survivable *here* and only here: too many accounts to
/// read means `login` starts a root session, the same answer as no
/// `/etc/passwd` at all. The secrets in `/etc/shadow` deliberately do NOT come
/// through this path - overflowing there would refuse every password instead of
/// falling back, so they are looked up one streamed line at a time (see
/// `crate::find_account_line`).
const PASSWD_MAX: usize = syscall_abi::SAFECOPY_MAX as usize;
/// Room for one `/etc/shadow` line: a 32-byte name, a hex salt and a 64-char hex
/// hash with their colons come to ~130, so this is generous on purpose - an
/// entry that does not fit is skipped rather than truncated.
const SHADOW_LINE_MAX: usize = 256;
const CR: u8 = 13;
const LF: u8 = 10;
const BS: u8 = 8;
const DEL: u8 = 127;

/// A completed login: the home directory has been written into the caller's cwd
/// buffer, and this task's identity is already the logged-in user.
pub struct Session {
    /// Length of the home path written into the cwd buffer `login` was given.
    pub cwd_len: usize,
}

/// Run the login gate. Writes the session's home directory into `cwd` and
/// returns its length; on return this task's identity is the logged-in user
/// (or root, if there is no `/etc/passwd`). Loops until authentication succeeds.
pub fn login(cwd: &mut [u8; CWD_SIZE]) -> Session {
    // ORDER IS LOAD-BEARING: read the account file FIRST, warn SECOND.
    //
    // `warn_if_unprotected` used to run first and ask `fsd` whether the mounted
    // filesystem enforces permissions - a question `fsd` cannot answer until it
    // has mounted. It has no retry of its own, and read_account_file below has
    // a bounded one for precisely that race, so the two functions three lines
    // apart disagreed about whether the race exists.
    //
    // On QEMU virtio-blk mounts before login asks and the warning is correct.
    // The device that loses the race is USB-MSD on real hardware, where the
    // whole symptom is a security warning that silently does NOT print.
    //
    // Doing the read first makes the race unreachable rather than merely
    // unlikely: read_account_file returns only once `fsd` has answered
    // something, so the warning below asks a server that is up. The printed
    // order is unchanged - the warning still precedes both the "no /etc/passwd"
    // line and the first prompt.
    let mut pbuf = [0u8; PASSWD_MAX];
    let plen = crate::read_account_file(PASSWD_PATH, &mut pbuf);
    warn_if_unprotected();
    if plen == 0 {
        crate::print_line("login: no /etc/passwd - starting a root session");
        return root_at(cwd);
    }
    let passwd = &pbuf[..plen];

    loop {
        // Before EVERY prompt: drop any key this task still holds. The boot
        // shell logs in and out in one task, so a logout whose drop was lost
        // would otherwise carry the last user's key into the next session; this
        // makes that impossible rather than unlikely (Decision 4).
        drop_my_keys();
        crate::print_str("\r\nlogin: ");
        let mut ubuf = [0u8; 32];
        let ulen = read_field(&mut ubuf, true);
        // read_field returns on Enter without echoing a newline, so move to the
        // next line before the password prompt.
        crate::print_str("\r\npassword: ");
        let mut wbuf = [0u8; 64];
        let wlen = read_field(&mut wbuf, false);
        crate::print_str("\r\n");

        if let Some(acct) = accounts::find_user_by_name(passwd, &ubuf[..ulen]) {
            // The secret lives in /etc/shadow (mode 0600, root): login runs as
            // root, before the SET_ID below, which is exactly why it can read it
            // at all. A legacy passwd line with the hash inline still verifies,
            // so a disk written before /etc/shadow still logs in.
            if let Some(upgradable) = verify_password(&acct, &ubuf[..ulen], &wbuf[..wlen]) {
                // A version-1 secret becomes version 2 at its first login,
                // while this task is still root (ACCTOP_UPGRADE is root-only).
                if upgradable {
                    upgrade_secret(&ubuf[..ulen], &wbuf[..wlen]);
                }
                // The cluster key, while this task is still root (netd accepts
                // a hold only from uid 0). Derived here because the password is
                // here; the seed goes to netd and is wiped. Logout drops it by
                // task (drop_my_keys), never by handle: see that function.
                hold_cluster_key(&ubuf[..ulen], &wbuf[..wlen], acct.uid);
                // Drop from root to the user. The kernel saves root as this
                // task's saved identity, so logout can restore it.
                // Identity and memberships in ONE call: the group half of
                // SET_ID is root-only, so a two-step form would only work in one
                // order with nothing enforcing it. Children inherit both at spawn.
                let mut gids = [0u32; syscall_abi::MAX_SUPP_GROUPS];
                let n = supplementary_groups(&ubuf[..ulen], acct.gid, &mut gids);
                crate::syscall4(
                    syscall_abi::SET_ID,
                    acct.uid as u64,
                    acct.gid as u64,
                    gids.as_ptr() as u64,
                    n as u64,
                );
                let hlen = write_cwd(cwd, acct.home);
                return Session { cwd_len: hlen };
            }
        }
        crate::print_line("Login incorrect");
    }
}

/// A root session at `/` (the no-`/etc/passwd` fallback). No `SET_ID` needed -
/// the shell boots as root already.
fn root_at(cwd: &mut [u8; CWD_SIZE]) -> Session {
    Session { cwd_len: write_cwd(cwd, b"/") }
}

/// Copy `home` into the cwd buffer (defaulting to `/` if empty), returning its
/// length.
fn write_cwd(cwd: &mut [u8; CWD_SIZE], home: &[u8]) -> usize {
    let home = if home.is_empty() { b"/".as_slice() } else { home };
    let n = home.len().min(cwd.len());
    cwd[..n].copy_from_slice(&home[..n]);
    n
}

/// Look `name` up in `/etc/group` and collect the gids it belongs to beyond its
/// primary, into `out`. Returns how many. No `/etc/group` (or no memberships)
/// simply means none.
///
/// `#[inline(never)]`: the group file is a 2 KB stack buffer.
///
/// `pub(crate)` because `su` needs exactly this: both spellings of `su` and the
/// login gate must derive memberships identically, or the same user ends up
/// with different effective permissions depending on how the session started.
#[inline(never)]
pub(crate) fn supplementary_groups(name: &[u8], primary_gid: u32, out: &mut [u32]) -> usize {
    let mut gbuf = [0u8; PASSWD_MAX];
    let glen = crate::read_account_file(GROUP_PATH, &mut gbuf);
    accounts::supplementary_gids(&gbuf[..glen], name, primary_gid, out)
}

/// Check `password` for `acct`: against `/etc/shadow`'s entry if there is one,
/// else against a legacy inline secret in the passwd line. An account with
/// neither never verifies - "no password recorded" must not mean "any password".
///
/// `Some(upgradable)` on success, where `upgradable` says the secret is a
/// version-1 shadow entry with the standard 8-byte salt, which `accountd` can
/// rewrite as version 2 in place. `None` when the password is wrong.
///
/// A version-2 check is a PBKDF2 derivation, about a second on the guest: it
/// runs HERE, in the shell, which is not supervised, never in a server. It
/// reads the secret and nothing else, so no cluster file can refuse a login.
///
/// `#[inline(never)]`: the shadow file is a 2 KB stack buffer, kept out of the
/// caller's frame.
#[inline(never)]
fn verify_password(acct: &accounts::Account<'_>, name: &[u8], password: &[u8]) -> Option<bool> {
    // ONE line, streamed off the disk rather than the whole file into a buffer:
    // the size of /etc/shadow must not decide whether anyone can log in. See
    // find_account_line for the lockout that a whole-file read caused here.
    let mut sline = [0u8; SHADOW_LINE_MAX];
    if let Some(n) = crate::find_account_line(SHADOW_PATH, name, &mut sline) {
        if let Some(secret) = accounts::find_secret_by_name(&sline[..n], name) {
            let upgradable = secret.version == accounts::SecretVersion::V1 && secret.salt_len == 8;
            return secret.verify(password).then_some(upgradable);
        }
    }
    // A legacy inline secret, or false when there is none. Never upgraded here:
    // moving it into /etc/shadow changes the file's length.
    acct.verify(password).then_some(false)
}

/// Derive `name`'s cluster key from `password` and hand it to `netd` to hold,
/// returning the handle. `None`, and a normal login, when the node has no realm
/// (it is not in a cluster) or `netd` does not hold it; a full table says so
/// in one line. About a second on the guest, which is the login's second
/// derivation (docs/roadmap/roadmap-user-keys.md, step 0 budgets both).
///
/// Nothing here can REFUSE a local login: the password was already checked.
/// But the hold is a blocking `MSG_CALL`, so a login does WAIT on `netd`: a
/// busy `netd` delays it, and a wedged one delays it until the supervisor
/// restarts it (or, past its cap, tears it down), when the call fails and the
/// login goes on. Bounded, not a hang; a timed call is a recorded follow-up.
#[inline(never)]
fn hold_cluster_key(name: &[u8], password: &[u8], uid: u32) -> Option<u64> {
    let mut rbuf = [0u8; clusterkeys::REALM_FILE_MAX + 1];
    let rlen = crate::read_account_file(REALM_PATH, &mut rbuf);
    let realm = clusterkeys::parse_realm(&rbuf[..rlen])?;
    let mut seed = clusterkeys::derive_user_seed(name, realm, password)?;

    // NETOP_KEY_HOLD: [op][uid][seed].
    let mut req = [0u8; 48];
    req[..8].copy_from_slice(&syscall_abi::NETOP_KEY_HOLD.to_le_bytes());
    req[8..16].copy_from_slice(&(uid as u64).to_le_bytes());
    req[16..48].copy_from_slice(&seed);
    // MSG_MAX_LEN, as MSG_CALL requires of every reply buffer: a short one is
    // refused outright, or accepted only where the next 768 bytes happen to
    // be mapped, which is how the first version of this passed.
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = crate::syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::NET_TASK,
        req.as_ptr() as u64,
        req.len() as u64,
        reply.as_mut_ptr() as u64,
    );
    // Our copies go whatever netd said: volatile, so they are not optimised
    // away as dead stores.
    for b in seed.iter_mut().chain(req[16..48].iter_mut()) {
        // SAFETY: a byte of a local array.
        unsafe { core::ptr::write_volatile(b, 0) };
    }
    if packed >= syscall_abi::FS_ERR_MIN || (packed & 0xffff_ffff) < 8 {
        return None; // no network server this boot: nothing to hold
    }
    let status = u64::from_le_bytes(reply[..8].try_into().ok()?);
    if status == syscall_abi::NET_KEY_FULL {
        crate::print_line("login: note: the cluster key is not held (every slot is in use)");
    }
    if status != syscall_abi::NET_KEY_OK || (packed & 0xffff_ffff) < 16 {
        return None;
    }
    Some(u64::from_le_bytes(reply[8..16].try_into().ok()?))
}

/// Ask `netd` to drop every key this task holds (`NETOP_KEY_DROP_MINE`), before
/// every login prompt and at logout. Best effort: with no network server there
/// is nothing held.
///
/// BY TASK, NEVER BY HANDLE. `netd` keys this on the kernel's packed task
/// identity of the sender, so it can only ever drop this task's own keys. A
/// logout used to drop by handle, sent as root (after the `SET_ID` back), and
/// root passes `drop_handle`'s owner check: a handle gone stale across a `netd`
/// restart named whichever login's key had taken that slot since, and logout
/// wiped it (review of step 4). Handles are bare slot numbers until step 7.
pub fn drop_my_keys() {
    let req = syscall_abi::NETOP_KEY_DROP_MINE.to_le_bytes();
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    crate::syscall4(syscall_abi::MSG_CALL, syscall_abi::NET_TASK, req.as_ptr() as u64, 8, reply.as_mut_ptr() as u64);
}

/// Upgrade `name`'s version-1 secret to version 2 (docs/roadmap/roadmap-user-keys.md,
/// Decision 10): derive the new secret here, where the password is, and ask
/// `accountd` to write it in place. Best effort: any failure leaves the
/// version-1 line, which still verifies, and says so in one line; a login is
/// never refused for it.
#[inline(never)]
fn upgrade_secret(name: &[u8], password: &[u8]) {
    let mut raw = [0u8; 8];
    let got = crate::syscall4(syscall_abi::RANDOM, raw.as_mut_ptr() as u64, raw.len() as u64, 0, 0);
    let random = (got == raw.len() as u64).then_some(raw);
    let clock = crate::syscall4(syscall_abi::MONOTONIC_US, 0, 0, 0, 0);
    let (salt8, strong) = accounts::salt_from(random, clock);
    // Said out loud, as passwd and useradd do: a clock salt is weaker, and this
    // path writes it into /etc/shadow without anyone having asked for a change.
    if !strong {
        crate::print_line("login: no hardware RNG - using a weaker clock-derived salt");
    }
    let mut salt = [0u8; accounts::SALT_V2];
    salt.copy_from_slice(&salt8[..accounts::SALT_V2]);
    let secret = accounts::secret_v2(salt, password);

    // ACCTOP_UPGRADE: (name len, password len), payload name || password ||
    // new_salt || new_hash.
    const HDR: usize = 40;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[..8].copy_from_slice(&syscall_abi::ACCTOP_UPGRADE.to_le_bytes());
    req[8..16].copy_from_slice(&(name.len() as u64).to_le_bytes());
    req[16..24].copy_from_slice(&(password.len() as u64).to_le_bytes());
    let mut w = HDR;
    for part in [name, password, &salt[..], &secret.hash[..]] {
        req[w..w + part.len()].copy_from_slice(part);
        w += part.len();
    }
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = crate::syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::ACCT_TASK,
        req.as_ptr() as u64,
        w as u64,
        reply.as_mut_ptr() as u64,
    );
    // The request carries the plaintext password: wipe it, as hold_cluster_key
    // wipes its seed (volatile, so it is not optimised away as a dead store).
    for b in req[..w].iter_mut() {
        // SAFETY: a byte of a local array.
        unsafe { core::ptr::write_volatile(b, 0) };
    }
    let ok = packed < syscall_abi::FS_ERR_MIN
        && (packed & 0xffff_ffff) >= 8
        && reply[..8] == 0u64.to_le_bytes();
    if !ok {
        crate::print_line("login: note: the stored password could not be upgraded; it still works");
    }
}

/// Say so when the filesystem holding the account database cannot protect it.
///
/// `/etc/shadow` is mode 0600 root **on ext2**. FAT32 and exFAT model no mode at
/// all, so `fsd` has nothing to enforce: there, any user can read the hashes
/// and - worse - overwrite root's entry with their own and log in as root. The
/// `chmod 600` at image-build time is a host-side mode those filesystems simply
/// do not record.
///
/// This is not a regression and not fixable *on* those filesystems: with no
/// modes there is no permission model, and `/etc/passwd` was equally writable
/// before the secrets moved out of it. What would be wrong is claiming
/// otherwise, so the guarantee announces its own absence at the one moment
/// someone is thinking about credentials.
fn warn_if_unprotected() {
    if crate::mounted_fs_unprotected() {
        crate::print_line(
            "warning: this filesystem cannot enforce permissions - /etc/shadow is readable and",
        );
        crate::print_line(
            "         writable by any user here, so accounts are NOT secure (ext2 enforces them)",
        );
    }
}

/// Read one line of keyboard input into `buf` (up to its length), returning the
/// count. Submits on CR/LF; supports destructive backspace. Echoes each byte
/// when `echo` is set (the username); a password is read silently.
fn read_field(buf: &mut [u8], echo: bool) -> usize {
    let mut len = 0usize;
    loop {
        let b = crate::read_char();
        match b {
            CR | LF => return len,
            BS | DEL => {
                if len > 0 {
                    len -= 1;
                    if echo {
                        crate::putc(BS);
                        crate::putc(b' ');
                        crate::putc(BS);
                    }
                }
            }
            _ if b < 0x20 => {} // ignore other control bytes
            _ => {
                if len < buf.len() {
                    buf[len] = b;
                    len += 1;
                    if echo {
                        crate::putc(b);
                    }
                }
            }
        }
    }
}

//! `accountd` - the **account server**: the fourth of them
//! (`fsd`/`cond`/`netd`/`accountd`), in protected task slot
//! [`syscall_abi::ACCT_TASK`], 5. The slot number is not the count - protected
//! slots 0 and 1 are the boot shell and idle, which are not servers.
//!
//! ## Why a server, and not a setuid bit
//!
//! `/etc/shadow` is mode 0600 root, which is the point of it - but that leaves a
//! normal user unable to change their *own* password, because doing so means
//! writing a file they cannot write. Unix answers that with a setuid `passwd`
//! binary. **That answer does not fit this system.** The kernel does not read
//! files; `fsd` does. A `/bin` binary is read by the *shell* and handed to
//! `SPAWN`, so "this binary is setuid" would be an assertion made by a
//! user-controlled task that the kernel has no way to verify - an escalation
//! path straight through the component the capability model exists to distrust.
//!
//! A server inverts it. `accountd` never asks who a program *claims* to be: it
//! asks the kernel who sent the message (`GET_ID` on the sender slot, a binding
//! only the kernel can make), and decides for itself. The privilege lives in
//! one small program whose whole job is this policy, rather than in a bit on a
//! file.
//!
//! ## Policy (the whole of it)
//!
//! - **root** may set any account's password, and need not prove the old one.
//! - **anyone else** may change only their own, and must present the current
//!   secret's hash, which `passwd` derives from what they typed.
//! - **root only** may upgrade a version-1 secret to version 2 (`login` does,
//!   after a version-1 login), and only in place.
//!
//! ## It never derives
//!
//! A version-2 secret is PBKDF2 at the cluster's count, about a second on the
//! guest, and this server is supervised: the watchdog restarts a server that
//! stays Runnable 2.56 s, and three restarts end it for the boot. So every
//! derivation happens in the program that has the password (`passwd`, `login`,
//! `useradd`), and this server only compares hashes in constant time and
//! writes lines (docs/roadmap/roadmap-user-keys.md, Decision 10). The one
//! password it sees, `ACCTOP_UPGRADE`'s, is checked against a version-1
//! secret, one SHA-256.
//!
//! Every spawnable slot holds the capability to *call* this server, which is
//! deliberate and safe for the same reason every slot may call `fsd`: holding
//! the right to ask is not permission to succeed. The check is here.
//!
//! ## Deliberately small
//!
//! Three ops (`ACCTOP_PASSWD`, `ACCTOP_SALT`, `ACCTOP_UPGRADE`). Creating and deleting accounts stays
//! with the root-only `/bin` tools - those need no privilege they don't already
//! have, so moving them here would add a protocol without removing a problem.
//! The `accounts` crate holds all the parsing/hashing, exactly as the roadmap
//! predicted when it called this tier "a repoint, not a rewrite".
//!
//! Built like every userland program: `aarch64-unknown-none`, release-only, the
//! shared `programs/linker.ld`, staged as `\EFI\ORBS\ACCOUNTD.BIN`. The panic
//! handler comes from `ulib` (this server is a `ulib` client, unlike the older
//! servers that predate it and hand-roll their fsd calls).

#![no_std]
#![no_main]

const PASSWD_FILE: &str = "/etc/passwd";
const SHADOW_FILE: &str = "/etc/shadow";
const BUF: usize = syscall_abi::SAFECOPY_MAX as usize;
/// Request header: op word plus four parameter words, the `NP_*`/`NETOP_*`
/// layout every other server here uses.
const REQ_HDR: usize = 40;
const REPLY_LEN: usize = syscall_abi::FS_REPLY_PAYLOAD as usize;
/// Longest account name this server will accept - the same bound the copy
/// buffer in [`change_password`] uses.
const NAME_MAX: usize = 64;
/// Longest password this server will accept. Bounds the request decode; the
/// hash is fixed-size regardless.
const PW_MAX: usize = 128;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::con_write(b"accountd: account server ready\r\n");
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = ulib::syscall4(
            syscall_abi::MSG_RECV,
            req.as_mut_ptr() as u64,
            req.len() as u64,
            0,
            0,
        );
        if packed >= syscall_abi::FS_ERR_MIN {
            break;
        }
        let len = ((packed & 0xffff_ffff) as usize).min(req.len());
        reply[..REPLY_LEN].fill(0);
        let n = handle(&req[..len], &mut reply);
        // An ACCTOP_UPGRADE carries the plaintext password, and a shorter next
        // request would leave it here (review of step 4). Every request is
        // wiped, not only that one: this server sees few, and the rest carry
        // hashes. Volatile, so it is not dropped as a dead store.
        for b in req[..len].iter_mut() {
            // SAFETY: a byte of this server's own receive buffer.
            unsafe { core::ptr::write_volatile(b, 0) };
        }
        // To the call this request opened (MSG_REPLY delivers only while its
        // caller is still in it).
        let call = ulib::syscall4(syscall_abi::SENDER_CALL, 0, 0, 0, 0);
        ulib::syscall4(syscall_abi::MSG_REPLY, call, reply.as_mut_ptr() as u64, n as u64, 0);
    }
    // The receive loop only ends if the kernel refuses to deliver, which means
    // this server is being torn down; park rather than spin the scheduler.
    ulib::exit(0);
}

/// Decode one request and carry it out, writing the reply (a status word,
/// then any result) into `reply` and returning its length.
///
/// Takes no sender: the authorization identity comes from the kernel's captured
/// credential (`SENDER_ID`), not from the slot number, which is exactly the
/// point. `_start` still needs the slot to address the reply.
fn handle(req: &[u8], reply: &mut [u8]) -> usize {
    let status = |reply: &mut [u8], v: u64| {
        reply[..REPLY_LEN].copy_from_slice(&v.to_le_bytes());
        REPLY_LEN
    };
    if req.len() < REQ_HDR {
        return status(reply, syscall_abi::ACCT_ERR_BAD_REQUEST);
    }
    let op = read_u64(req, 0);
    let a0 = read_u64(req, 8) as usize;
    let a1 = read_u64(req, 16) as usize;
    let payload = &req[REQ_HDR..];

    // Who is actually asking. The kernel binds a credential to each message when
    // it is SENT, so it cannot be spoofed by the request's contents - which is
    // the entire reason this server can be trusted to make the decision.
    //
    // SENDER_ID, not GET_ID(sender): the latter answers who occupies that slot
    // *now*. A caller could send "change root's password" and immediately exit
    // (MSG_SEND does not block), and by the time this server drained its
    // mailbox the slot could hold a different task altogether. Refusing a DEAD
    // slot - which is all the earlier fix did - does not help, because a
    // RE-SPAWNED slot is perfectly alive and the message carries a bare slot
    // number with nothing to tell the two apart. If root landed there, the
    // old-password proof would be skipped entirely.
    //
    // FAIL CLOSED when the kernel has no captured credential: authorized as
    // nobody, never as root.
    let packed = ulib::sender_id();
    if packed == syscall_abi::GET_ID_ERR {
        return status(reply, syscall_abi::ACCT_ERR_DENIED);
    }
    let caller_uid = (packed & 0xffff_ffff) as u32;

    // Every length below comes from an untrusted request. Each is bounded on
    // its own BEFORE any sum, so no sum can wrap, and the payload must hold
    // them all: a naive `a + b > len` wraps in release and the slice then
    // panics, parking this server (the panic handler parks) for the boot.
    const SECRET: usize = syscall_abi::ACCT_V2_SALT_LEN + syscall_abi::ACCT_HASH_LEN;
    match op {
        syscall_abi::ACCTOP_PASSWD => {
            let (name_len, old_len) = (a0, a1);
            if name_len > NAME_MAX
                || (old_len != 0 && old_len != syscall_abi::ACCT_HASH_LEN)
                || name_len + old_len + SECRET > payload.len()
            {
                return status(reply, syscall_abi::ACCT_ERR_BAD_REQUEST);
            }
            let name = &payload[..name_len];
            let old = &payload[name_len..name_len + old_len];
            let new = &payload[name_len + old_len..name_len + old_len + SECRET];
            status(reply, change_password(caller_uid, name, old, new))
        }
        syscall_abi::ACCTOP_SALT => {
            if a0 > NAME_MAX || a0 > payload.len() {
                return status(reply, syscall_abi::ACCT_ERR_BAD_REQUEST);
            }
            report_salt(caller_uid, &payload[..a0], reply)
        }
        syscall_abi::ACCTOP_UPGRADE => {
            let (name_len, pw_len) = (a0, a1);
            if name_len > NAME_MAX || name_len == 0 || pw_len > PW_MAX || name_len + pw_len + SECRET > payload.len() {
                return status(reply, syscall_abi::ACCT_ERR_BAD_REQUEST);
            }
            let name = &payload[..name_len];
            let pw = &payload[name_len..name_len + pw_len];
            let new = &payload[name_len + pw_len..name_len + pw_len + SECRET];
            status(reply, upgrade(caller_uid, name, pw, new))
        }
        _ => status(reply, syscall_abi::ACCT_ERR_BAD_REQUEST),
    }
}

/// The account a request names, resolved against `/etc/passwd`.
struct Target {
    name: [u8; NAME_MAX],
    len: usize,
    /// A legacy secret inline in the passwd line, for an account made before
    /// `/etc/shadow`.
    legacy: Option<accounts::Secret>,
}

impl Target {
    fn name(&self) -> &[u8] {
        &self.name[..self.len]
    }
}

/// Resolve `name` (empty: the caller's own account, by uid) and apply the one
/// rule every op shares: a non-root caller may name only themselves.
///
/// `#[inline(never)]`: the passwd buffer is `SAFECOPY_MAX` bytes.
#[inline(never)]
fn find_target(caller_uid: u32, name: &[u8]) -> Result<Target, u64> {
    // read_file_checked, not read_file_all: the latter folds every failure -
    // an I/O error, a database larger than this buffer - into 0 bytes, which
    // then resolves as ACCT_ERR_NO_USER. That sends the operator after a typo
    // in a name that is actually present, and hides the real fault.
    let mut pbuf = [0u8; BUF];
    let Some(plen) = ulib::read_file_checked(PASSWD_FILE, &mut pbuf) else {
        return Err(syscall_abi::ACCT_ERR_IO);
    };
    let passwd = &pbuf[..plen];
    let acct = if name.is_empty() {
        accounts::find_user_by_uid(passwd, caller_uid)
    } else {
        accounts::find_user_by_name(passwd, name)
    };
    let Some(acct) = acct else { return Err(syscall_abi::ACCT_ERR_NO_USER) };
    if caller_uid != 0 && acct.uid != caller_uid {
        return Err(syscall_abi::ACCT_ERR_DENIED);
    }
    // REFUSE rather than truncate. NAME_MAX bounds the name in the *request*,
    // but this one comes from /etc/passwd and is bounded by nothing - and it is
    // the key the /etc/shadow rewrite matches on. A silently shortened key
    // appends a junk entry under a prefix while reporting success.
    let mut t = Target { name: [0u8; NAME_MAX], len: acct.name.len(), legacy: acct.secret };
    if t.len > NAME_MAX {
        return Err(syscall_abi::ACCT_ERR_IO);
    }
    t.name[..t.len].copy_from_slice(acct.name);
    Ok(t)
}

/// The target's current secret: its `/etc/shadow` entry, else a legacy inline
/// one, else none.
fn current_secret(shadow: &[u8], t: &Target) -> Option<accounts::Secret> {
    accounts::find_secret_by_name(shadow, t.name()).or(t.legacy)
}

/// A version-2 secret from a request's `salt || hash`.
fn v2_secret(new: &[u8]) -> accounts::Secret {
    let mut salt = [0u8; accounts::SALT_MAX];
    salt[..syscall_abi::ACCT_V2_SALT_LEN].copy_from_slice(&new[..syscall_abi::ACCT_V2_SALT_LEN]);
    let mut hash = [0u8; accounts::DIGEST];
    hash.copy_from_slice(&new[syscall_abi::ACCT_V2_SALT_LEN..]);
    accounts::Secret { version: accounts::SecretVersion::V2, salt, salt_len: syscall_abi::ACCT_V2_SALT_LEN, hash }
}

/// `ACCTOP_PASSWD`: the policy, the comparison and the write. No derivation:
/// `old` is the hash `passwd` computed, `new` a version-2 secret it derived.
///
/// `#[inline(never)]`: two `SAFECOPY_MAX` buffers live here, off the receive
/// loop's frame.
#[inline(never)]
fn change_password(caller_uid: u32, name: &[u8], old: &[u8], new: &[u8]) -> u64 {
    let t = match find_target(caller_uid, name) {
        Ok(t) => t,
        Err(e) => return e,
    };
    // read_file_checked, not read_file_all: this buffer is rewritten back over
    // /etc/shadow below, so a read error or an over-long file returning 0 would
    // replace the whole database with one line and wipe every other account's
    // secret - while cheerfully reporting success.
    let mut sbuf = [0u8; BUF];
    let Some(slen) = ulib::read_file_checked(SHADOW_FILE, &mut sbuf) else {
        return syscall_abi::ACCT_ERR_IO;
    };
    if caller_uid != 0 {
        // The caller must present the current secret's hash. An account with
        // no secret recorded cannot be changed this way: "no password" must
        // not read as "any password will do". Constant-time compare.
        let ok = match current_secret(&sbuf[..slen], &t) {
            Some(secret) => old.len() == accounts::DIGEST && accounts::digest_eq(old, &secret.hash),
            None => false,
        };
        if !ok {
            return syscall_abi::ACCT_ERR_WRONG_PASSWORD;
        }
    }
    write_secret(&sbuf[..slen], t.name(), &v2_secret(new), true)
}

/// `ACCTOP_SALT`: the version and salt of the target's secret, for `passwd` to
/// hash the old password with. Neither is secret; the hash stays here.
#[inline(never)]
fn report_salt(caller_uid: u32, name: &[u8], reply: &mut [u8]) -> usize {
    let fail = |reply: &mut [u8], v: u64| {
        reply[..REPLY_LEN].copy_from_slice(&v.to_le_bytes());
        REPLY_LEN
    };
    let t = match find_target(caller_uid, name) {
        Ok(t) => t,
        Err(e) => return fail(reply, e),
    };
    let mut sbuf = [0u8; BUF];
    let Some(slen) = ulib::read_file_checked(SHADOW_FILE, &mut sbuf) else {
        return fail(reply, syscall_abi::ACCT_ERR_IO);
    };
    let Some(secret) = current_secret(&sbuf[..slen], &t) else {
        return fail(reply, syscall_abi::ACCT_ERR_WRONG_PASSWORD);
    };
    reply[..REPLY_LEN].copy_from_slice(&0u64.to_le_bytes());
    reply[REPLY_LEN] = match secret.version {
        accounts::SecretVersion::V1 => 1,
        accounts::SecretVersion::V2 => 2,
    };
    reply[REPLY_LEN + 1] = secret.salt_len as u8;
    reply[REPLY_LEN + 2..REPLY_LEN + 2 + secret.salt_len].copy_from_slice(&secret.salt[..secret.salt_len]);
    REPLY_LEN + 2 + secret.salt_len
}

/// `ACCTOP_UPGRADE`: root only, a version-1 secret with the standard 8-byte
/// salt, the password checked against it (one SHA-256), and the version-2
/// line written IN PLACE or not at all.
#[inline(never)]
fn upgrade(caller_uid: u32, name: &[u8], pw: &[u8], new: &[u8]) -> u64 {
    if caller_uid != 0 {
        return syscall_abi::ACCT_ERR_DENIED;
    }
    let t = match find_target(caller_uid, name) {
        Ok(t) => t,
        Err(e) => return e,
    };
    let mut sbuf = [0u8; BUF];
    let Some(slen) = ulib::read_file_checked(SHADOW_FILE, &mut sbuf) else {
        return syscall_abi::ACCT_ERR_IO;
    };
    // Only a SHADOW entry: a legacy inline secret lives in /etc/passwd, and
    // moving it is a length change.
    let Some(secret) = accounts::find_secret_by_name(&sbuf[..slen], t.name()) else {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    };
    if secret.version != accounts::SecretVersion::V1 || secret.salt_len != 8 {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    }
    if secret.verify_without_deriving(pw) != Some(true) {
        return syscall_abi::ACCT_ERR_WRONG_PASSWORD;
    }
    write_secret(&sbuf[..slen], t.name(), &v2_secret(new), false)
}

/// Replace (or append) `name`'s line in `/etc/shadow` with `secret`.
///
/// `length_may_change` is REQUIRED, so no caller takes the whole-file path by
/// default: an upgrade passes `false` and is refused rather than truncating.
///
/// NON-DESTRUCTIVE where it can be. A whole-file write is truncate-then-write
/// (ext2's overwrite branch frees the old blocks first), which stakes the
/// entire credential database on the following write landing: an fsd restart
/// (documented to happen) or a power loss in that window leaves /etc/shadow
/// EMPTY and locks every account out, root included. A secret of the same
/// width leaves the file the same length with every other byte identical, so
/// writing just that range at its offset never truncates and never touches
/// another account's line.
#[inline(never)]
fn write_secret(shadow: &[u8], name: &[u8], secret: &accounts::Secret, length_may_change: bool) -> u64 {
    let mut line = [0u8; 256];
    let Some(llen) = accounts::format_secret_line(&mut line, name, secret) else {
        return syscall_abi::ACCT_ERR_IO;
    };
    let mut out = [0u8; BUF];
    let Some((olen, replaced)) = accounts::replace_line(shadow, &mut out, name, &line[..llen]) else {
        return syscall_abi::ACCT_ERR_IO;
    };
    // No existing shadow entry (an account made before /etc/shadow, or one
    // whose secret was never set): append rather than fail.
    let olen = if replaced {
        olen
    } else {
        match accounts::append_line(shadow, &mut out, &line[..llen]) {
            Some(n) => n,
            None => return syscall_abi::ACCT_ERR_IO,
        }
    };
    if olen != shadow.len() && !length_may_change {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    }
    let code = match accounts::changed_span(shadow, &out[..olen]) {
        Some((off, n)) => ulib::write_private_at(SHADOW_FILE, off as u64, &out[off..off + n]),
        // changed_span also reports None for byte-identical buffers. A fresh
        // salt makes that practically unreachable, but "no change" must mean
        // "write nothing", not "rewrite the database".
        None if shadow == &out[..olen] => 0,
        // Length changed: create-if-absent, chmod 0600, then the content. The
        // chmod is unconditional, so an /etc/shadow that already exists
        // world-readable is repaired rather than quietly filled with secrets.
        None => ulib::write_private_file(SHADOW_FILE, &out[..olen]),
    };
    if ulib::is_fs_error(code) {
        return syscall_abi::ACCT_ERR_IO;
    }
    0
}

fn read_u64(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

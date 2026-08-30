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
//! - **root** may set any account's password, and need not supply the old one.
//! - **anyone else** may change only their own, and must supply the current one.
//!
//! Every spawnable slot holds the capability to *call* this server, which is
//! deliberate and safe for the same reason every slot may call `fsd`: holding
//! the right to ask is not permission to succeed. The check is here.
//!
//! ## Deliberately small
//!
//! One op ([`syscall_abi::ACCTOP_PASSWD`]). Creating and deleting accounts stays
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
        let sender = packed >> 32;
        let len = ((packed & 0xffff_ffff) as usize).min(req.len());
        let status = handle(&req[..len]);
        reply[..REPLY_LEN].copy_from_slice(&status.to_le_bytes());
        ulib::syscall4(
            syscall_abi::MSG_SEND,
            sender,
            reply.as_mut_ptr() as u64,
            REPLY_LEN as u64,
            0,
        );
    }
    // The receive loop only ends if the kernel refuses to deliver, which means
    // this server is being torn down; park rather than spin the scheduler.
    ulib::exit(0);
}

/// Decode one request and carry it out, returning the status word to reply.
///
/// Takes no sender: the authorization identity comes from the kernel's captured
/// credential (`SENDER_ID`), not from the slot number, which is exactly the
/// point. `_start` still needs the slot to address the reply.
fn handle(req: &[u8]) -> u64 {
    if req.len() < REQ_HDR {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    }
    let op = read_u64(req, 0);
    if op != syscall_abi::ACCTOP_PASSWD {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    }
    let name_len = read_u64(req, 8) as usize;
    let old_len = read_u64(req, 16) as usize;
    let new_len = read_u64(req, 24) as usize;
    let payload = &req[REQ_HDR..];
    // CHECKED addition, not `a + b + c > len`: all three lengths come straight
    // from an untrusted request, and release builds wrap silently - so
    // `name_len = u64::MAX` would pass a naive bounds check and then panic on
    // the slice below, parking this server for good (the panic handler parks;
    // there is nowhere to unwind to). Any task holding TO_ACCT could wedge the
    // account server with one message. Every length is bounded individually too.
    let total = match name_len.checked_add(old_len).and_then(|v| v.checked_add(new_len)) {
        Some(t) => t,
        None => return syscall_abi::ACCT_ERR_BAD_REQUEST,
    };
    if total > payload.len()
        || name_len > NAME_MAX
        || old_len > PW_MAX
        || new_len > PW_MAX
        || new_len == 0
    {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    }
    let name = &payload[..name_len];
    let old = &payload[name_len..name_len + old_len];
    let new = &payload[name_len + old_len..name_len + old_len + new_len];

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
        return syscall_abi::ACCT_ERR_DENIED;
    }
    let caller_uid = (packed & 0xffff_ffff) as u32;

    change_password(caller_uid, name, old, new)
}

/// The policy and the write. Split out so the request decode above stays
/// obviously about parsing and this stays obviously about authorization.
///
/// `#[inline(never)]`: three `SAFECOPY_MAX` buffers live here, off the receive
/// loop's frame.
#[inline(never)]
fn change_password(caller_uid: u32, name: &[u8], old: &[u8], new: &[u8]) -> u64 {
    // read_file_checked, not read_file_all: the latter folds every failure -
    // an I/O error, a database larger than this buffer - into 0 bytes, which
    // then resolves as ACCT_ERR_NO_USER. That sends the operator after a typo
    // in a name that is actually present, and hides the real fault. The same
    // hazard is why the shadow read below is checked; both deserve it.
    let mut pbuf = [0u8; BUF];
    let Some(plen) = ulib::read_file_checked(PASSWD_FILE, &mut pbuf) else {
        return syscall_abi::ACCT_ERR_IO;
    };
    let passwd = &pbuf[..plen];

    // Resolve the target: an empty name means "me", by uid.
    let target = if name.is_empty() {
        match accounts::find_user_by_uid(passwd, caller_uid) {
            Some(a) => a,
            None => return syscall_abi::ACCT_ERR_NO_USER,
        }
    } else {
        match accounts::find_user_by_name(passwd, name) {
            Some(a) => a,
            None => return syscall_abi::ACCT_ERR_NO_USER,
        }
    };
    let target_uid = target.uid;
    // Copy the name out: the rewrite below reuses the buffer `target` borrows.
    //
    // REFUSE rather than truncate. NAME_MAX bounds the name in the *request*,
    // but this one comes from /etc/passwd and is bounded by nothing - and it is
    // the key the /etc/shadow rewrite matches on. A silently shortened key
    // appends a junk entry under a prefix while reporting success: the old
    // password keeps working, the new one never does, and two accounts sharing
    // a NAME_MAX-byte prefix collide onto one entry.
    let mut tname = [0u8; NAME_MAX];
    if target.name.len() > tname.len() {
        return syscall_abi::ACCT_ERR_IO;
    }
    let tn = target.name.len();
    tname[..tn].copy_from_slice(target.name);
    let target_name = &tname[..tn];
    let legacy_secret = target.secret;

    // read_file_checked, not read_file_all: this buffer is rewritten back over
    // /etc/shadow below, so a read error or an over-long file returning 0 would
    // replace the whole database with one line and wipe every other account's
    // secret - while cheerfully reporting success.
    let mut sbuf = [0u8; BUF];
    let Some(slen) = ulib::read_file_checked(SHADOW_FILE, &mut sbuf) else {
        return syscall_abi::ACCT_ERR_IO;
    };

    if caller_uid != 0 {
        // A non-root caller may only change their own password...
        if target_uid != caller_uid {
            return syscall_abi::ACCT_ERR_DENIED;
        }
        // ...and must prove they know the current one. An account with no
        // secret recorded at all cannot be changed this way: "no password" must
        // not read as "any password will do".
        let ok = match accounts::find_secret_by_name(&sbuf[..slen], target_name) {
            Some(secret) => secret.verify(old),
            None => match legacy_secret {
                Some(secret) => secret.verify(old),
                None => false,
            },
        };
        if !ok {
            return syscall_abi::ACCT_ERR_WRONG_PASSWORD;
        }
    }

    // Hardware entropy where the machine has it; the clock fallback otherwise.
    // The warning goes to the *server's* console rather than the caller's
    // reply - it is a property of the machine, not of this request.
    let (salt, strong) = accounts::salt_from(ulib::random_bytes8(), ulib::monotonic_us());
    if !strong {
        ulib::con_write(b"accountd: no hardware RNG - using a weaker clock-derived salt\r\n");
    }
    let hash = accounts::hash_password(&salt, new);

    let mut line = [0u8; 256];
    let Some(llen) = accounts::format_shadow_line(&mut line, target_name, &salt, &hash) else {
        return syscall_abi::ACCT_ERR_IO;
    };
    let mut out = [0u8; BUF];
    let Some((olen, replaced)) =
        accounts::replace_line(&sbuf[..slen], &mut out, target_name, &line[..llen])
    else {
        return syscall_abi::ACCT_ERR_IO;
    };
    // No existing shadow entry (an account made before /etc/shadow, or one
    // whose secret was never set): append rather than fail.
    let olen = if replaced {
        olen
    } else {
        match accounts::append_line(&sbuf[..slen], &mut out, &line[..llen]) {
            Some(n) => n,
            None => return syscall_abi::ACCT_ERR_IO,
        }
    };
    // NON-DESTRUCTIVE where it can be. A whole-file write is truncate-then-
    // write (ext2's overwrite branch frees the old blocks first), which stakes
    // the entire credential database on the following write landing: an fsd
    // restart - documented to happen - or a power loss in that window leaves
    // /etc/shadow EMPTY and locks every account out, root included.
    //
    // A password change never needs that. A shadow line's salt and hash are
    // fixed-width hex, so replacing one leaves the file the same length with
    // every other byte identical - and writing just that range at its offset
    // never truncates and never touches another account's line. The worst an
    // interruption can now do is damage the one entry being changed.
    //
    // Only a length change (appending the first entry for an account, or
    // rewriting a legacy inline-secret line) still needs the whole-file path.
    // That is the rarer case, it is what write_private_file exists for, and
    // falling back is explicit rather than silent.
    let code = match accounts::changed_span(&sbuf[..slen], &out[..olen]) {
        Some((off, n)) => ulib::write_private_at(SHADOW_FILE, off as u64, &out[off..off + n]),
        // changed_span also reports None for byte-identical buffers. A fresh
        // salt makes that practically unreachable, but "no change" must mean
        // "write nothing", not "rewrite the database".
        None if sbuf[..slen] == out[..olen] => 0,
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

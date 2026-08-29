//! `accountd` - the **account server**, the fifth protected server (task slot
//! [`syscall_abi::ACCT_TASK`], 5).
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
        let status = handle(sender, &req[..len]);
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
fn handle(sender: u64, req: &[u8]) -> u64 {
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
    if name_len + old_len + new_len > payload.len()
        || old_len > PW_MAX
        || new_len > PW_MAX
        || new_len == 0
    {
        return syscall_abi::ACCT_ERR_BAD_REQUEST;
    }
    let name = &payload[..name_len];
    let old = &payload[name_len..name_len + old_len];
    let new = &payload[name_len + old_len..name_len + old_len + new_len];

    // Who is actually asking. The kernel binds this to the message's real
    // sender slot, so it cannot be spoofed by the request's contents - which is
    // the entire reason this server can be trusted to make the decision.
    let caller_uid = (ulib::task_id(sender) & 0xffff_ffff) as u32;

    change_password(caller_uid, name, old, new)
}

/// The policy and the write. Split out so the request decode above stays
/// obviously about parsing and this stays obviously about authorization.
///
/// `#[inline(never)]`: three `SAFECOPY_MAX` buffers live here, off the receive
/// loop's frame.
#[inline(never)]
fn change_password(caller_uid: u32, name: &[u8], old: &[u8], new: &[u8]) -> u64 {
    let mut pbuf = [0u8; BUF];
    let plen = ulib::read_file_all(PASSWD_FILE, &mut pbuf);
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
    let mut tname = [0u8; 64];
    let tn = target.name.len().min(tname.len());
    tname[..tn].copy_from_slice(&target.name[..tn]);
    let target_name = &tname[..tn];
    let legacy_secret = target.secret;

    let mut sbuf = [0u8; BUF];
    let slen = ulib::read_file_all(SHADOW_FILE, &mut sbuf);

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
    if ulib::is_fs_error(ulib::fs_write_bulk(SHADOW_FILE, &out[..olen])) {
        return syscall_abi::ACCT_ERR_IO;
    }
    0
}

fn read_u64(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

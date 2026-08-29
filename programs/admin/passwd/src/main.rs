//! `passwd [user]` - change a password.
//!
//! **Any user may change their own**; only root may change someone else's. That
//! is a change from this tool's first version, which was root-only because it
//! wrote `/etc/shadow` itself - a file a normal user cannot write.
//!
//! It no longer writes anything. It prompts, and sends the answer to the
//! **account server** (`accountd`, task slot [`syscall_abi::ACCT_TASK`]), which
//! holds the policy: root may set any password without knowing the old one;
//! anyone else must supply their current password and may only change their
//! own. The server asks the kernel who sent the message rather than trusting
//! anything in it, so this program having the right to *ask* grants nothing.
//!
//! With no argument, changes the calling user's own password (resolved by uid
//! on the server side, so no name has to be guessed here).
//!
//! Byte-only handling (PIE-safe); the password never touches the console (echo
//! off) and never reaches a file from this process.

#![no_std]
#![no_main]

/// Request header: op word plus four parameter words - the layout every server
/// here uses.
const REQ_HDR: usize = 40;
const PW_MAX: usize = 64;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: passwd [user]  (change a password; only root may change another user's)\r\n",
    );

    let mut namebuf = [0u8; 64];
    let name_len = ulib::arg(1, &mut namebuf).unwrap_or(0);
    let name = &namebuf[..name_len];
    let am_root = ulib::getuid() == 0;

    // Root never needs the old password; anyone else always does - including
    // when changing their own, which is the only thing they may change.
    let mut old = [0u8; PW_MAX];
    let mut old_len = 0usize;
    if !am_root {
        ulib::con_write(b"Current password: ");
        old_len = ulib::read_line(&mut old, false);
        ulib::con_write(b"\r\n");
    }

    ulib::con_write(b"New password: ");
    let mut pw = [0u8; PW_MAX];
    let pwl = ulib::read_line(&mut pw, false);
    ulib::con_write(b"\r\nRetype new password: ");
    let mut pw2 = [0u8; PW_MAX];
    let pwl2 = ulib::read_line(&mut pw2, false);
    ulib::con_write(b"\r\n");
    if pw[..pwl] != pw2[..pwl2] {
        die(b"passwd: passwords do not match\r\n");
    }
    if pwl == 0 {
        die(b"passwd: password may not be empty\r\n");
    }

    // ACCTOP_PASSWD: (name len, old len, new len), payload name || old || new.
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    write_u64(&mut req, 0, syscall_abi::ACCTOP_PASSWD);
    write_u64(&mut req, 8, name_len as u64);
    write_u64(&mut req, 16, old_len as u64);
    write_u64(&mut req, 24, pwl as u64);
    let mut w = REQ_HDR;
    for src in [name, &old[..old_len], &pw[..pwl]] {
        req[w..w + src.len()].copy_from_slice(src);
        w += src.len();
    }

    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = ulib::syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::ACCT_TASK,
        req.as_ptr() as u64,
        w as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed >= syscall_abi::FS_ERR_MIN {
        die(b"passwd: no account server (is ACCOUNTD.BIN staged?)\r\n");
    }
    // The reply length must actually cover a status word: `reply` is zeroed, so
    // a short/empty reply would otherwise decode as status 0 and report a change
    // that never happened.
    if (packed as usize) < 8 {
        die(b"passwd: the account server sent no answer\r\n");
    }
    let status = read_u64(&reply, 0);
    match status {
        0 => {
            ulib::con_write(b"passwd: password updated\r\n");
            ulib::exit(0)
        }
        syscall_abi::ACCT_ERR_DENIED => die(b"passwd: only root may change another user's password\r\n"),
        syscall_abi::ACCT_ERR_NO_USER => die(b"passwd: no such user\r\n"),
        syscall_abi::ACCT_ERR_WRONG_PASSWORD => die(b"passwd: current password is wrong\r\n"),
        syscall_abi::ACCT_ERR_IO => die(b"passwd: could not update the account database\r\n"),
        _ => die(b"passwd: the account server rejected the request\r\n"),
    }
}

fn write_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

fn read_u64(b: &[u8], off: usize) -> u64 {
    let mut v = [0u8; 8];
    v.copy_from_slice(&b[off..off + 8]);
    u64::from_le_bytes(v)
}

fn die(msg: &[u8]) -> ! {
    ulib::con_write(msg);
    ulib::exit(1);
}

//! `passwd [user]` - set a user's password in `/etc/passwd` (root only).
//!
//! With no argument, changes the *current* user's password (resolved from the
//! task's uid); with a `<user>` argument, that account's. Prompts for the new
//! password twice (echo off), draws a fresh salt from the hardware RNG
//! (`RANDOM` -> `accounts::salt_from`), stores `SHA-256(salt || password)`, and
//! rewrites the one account line. On a machine with no entropy device - the
//! ordinary case off QEMU - it falls back to the weaker clock-derived salt and
//! **says so** rather than storing a guessable one quietly.
//!
//! **Root only** (the option-1 model). A non-root user changing their *own*
//! password needs a privileged path (a setuid bit or an `accountd` server) that
//! this milestone deliberately defers - the shared `accounts` machinery here is
//! exactly what that later path will reuse. Byte-only parsing (PIE-safe);
//! interactive input works because the shell hands a foreground `/bin` program
//! the keyboard.

#![no_std]
#![no_main]

const PASSWD_FILE: &str = "/etc/passwd";
const SHADOW_FILE: &str = "/etc/shadow";
const BUF: usize = syscall_abi::SAFECOPY_MAX as usize;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: passwd [user]  (set a password; root only)\r\n");
    if ulib::getuid() != 0 {
        die(b"passwd: only root may change passwords\r\n");
    }

    let mut src = [0u8; BUF];
    let slen = ulib::read_file_all(PASSWD_FILE, &mut src);
    if slen == 0 {
        die(b"passwd: cannot read /etc/passwd\r\n");
    }

    // Target name: argv[1], else the current user (uid -> name).
    let mut namebuf = [0u8; 33];
    let mut name_len = 0usize;
    if let Some(n) = ulib::arg(1, &mut namebuf) {
        name_len = n;
    } else {
        let uid = ulib::getuid();
        if let Some(acct) = accounts::find_user_by_uid(&src[..slen], uid) {
            let l = acct.name.len().min(namebuf.len());
            namebuf[..l].copy_from_slice(&acct.name[..l]);
            name_len = l;
        }
    }
    if name_len == 0 {
        die(b"passwd: cannot determine the target user (give a name)\r\n");
    }
    let name = &namebuf[..name_len];

    // The account must exist - but nothing of it is needed beyond that, since
    // only /etc/shadow is rewritten now.
    if accounts::find_user_by_name(&src[..slen], name).is_none() {
        die(b"passwd: no such user\r\n");
    }

    // Prompt for the new password twice (echo off).
    ulib::con_write(b"New password: ");
    let mut pw = [0u8; 64];
    let pwl = ulib::read_line(&mut pw, false);
    ulib::con_write(b"\r\nRetype new password: ");
    let mut pw2 = [0u8; 64];
    let pwl2 = ulib::read_line(&mut pw2, false);
    ulib::con_write(b"\r\n");
    if pw[..pwl] != pw2[..pwl2] {
        die(b"passwd: passwords do not match\r\n");
    }

    // Hardware entropy if this machine has an RNG; the clock fallback otherwise,
    // said out loud rather than stored quietly (see accounts::salt_from).
    let (salt, strong) = accounts::salt_from(ulib::random_bytes8(), ulib::monotonic_us());
    if !strong {
        ulib::con_write(b"passwd: no hardware RNG - using a weaker clock-derived salt\r\n");
    }
    let hash = accounts::hash_password(&salt, &pw[..pwl]);

    // Only /etc/shadow changes: the passwd line holds no secret any more, so a
    // password change never rewrites it (and cannot disturb uid/gid/home).
    let mut line = [0u8; 256];
    let Some(llen) = accounts::format_shadow_line(&mut line, name, &salt, &hash) else {
        die(b"passwd: shadow line too long\r\n");
    };
    let mut sbuf = [0u8; BUF];
    let Some(sslen) = ulib::read_file_checked(SHADOW_FILE, &mut sbuf) else {
        die(b"passwd: could not read /etc/shadow - refusing to rewrite it\r\n");
    };
    let mut out = [0u8; BUF];
    // Replace the user's line if present, else append one (an account created
    // before /etc/shadow existed, or one whose secret was never set).
    let (olen, replaced) = match accounts::replace_line(&sbuf[..sslen], &mut out, name, &line[..llen]) {
        Some(r) => r,
        None => die(b"passwd: /etc/shadow full\r\n"),
    };
    let olen = if replaced {
        olen
    } else {
        match accounts::append_line(&sbuf[..sslen], &mut out, &line[..llen]) {
            Some(n) => n,
            None => die(b"passwd: /etc/shadow full\r\n"),
        }
    };
    // 0600 before the secrets land - on a file that already exists as well as
    // one being created, and without truncating to get there. See
    // ulib::write_private_file for the three orderings that has to satisfy.
    let code = ulib::write_private_file(SHADOW_FILE, &out[..olen]);
    if ulib::is_fs_error(code) {
        ulib::fs_error("passwd", code);
        ulib::exit(1);
    }
    ulib::con_write(b"passwd: password updated for ");
    ulib::con_write(name);
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

fn die(msg: &[u8]) -> ! {
    ulib::con_write(msg);
    ulib::exit(1);
}

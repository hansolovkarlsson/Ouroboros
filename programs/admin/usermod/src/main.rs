//! `usermod <user> -g <group|gid>` - change a user's **primary group** in
//! `/etc/passwd` (root only). This is the "assign a user to a group" operation
//! in the primary-gid model: a task carries one kernel-owned gid, so group
//! membership *is* the passwd `gid` field. `<group>` may be a name (resolved via
//! `/etc/group`) or a numeric gid. Full supplementary-group membership is a
//! deferred tier (it needs the kernel identity to carry a group list).
//!
//! Root only. Byte-only parsing (PIE-safe). Rewrites the one account line,
//! preserving uid/home/salt/hash.

#![no_std]
#![no_main]

const PASSWD_FILE: &str = "/etc/passwd";
const GROUP_FILE: &str = "/etc/group";
const SHADOW_FILE: &str = "/etc/shadow";
const BUF: usize = syscall_abi::SAFECOPY_MAX as usize;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: usermod <user> -g <group|gid>  (root only)\r\n");
    if ulib::getuid() != 0 {
        die(b"usermod: only root may modify accounts\r\n");
    }

    // Parse: <user> and -g <group|gid>.
    let mut namebuf = [0u8; 33];
    let mut name_len = 0usize;
    let mut gbuf = [0u8; 33];
    let mut g_len = 0usize;
    let mut i = 1u64;
    let argc = ulib::argc();
    while i < argc {
        let mut a = [0u8; 33];
        let n = ulib::arg(i, &mut a).unwrap_or(0);
        let tok = &a[..n];
        if tok == b"-g" {
            i += 1;
            g_len = ulib::arg(i, &mut gbuf).unwrap_or(0);
        } else if name_len == 0 && !tok.is_empty() {
            namebuf[..n].copy_from_slice(tok);
            name_len = n;
        }
        i += 1;
    }
    if name_len == 0 || g_len == 0 {
        die(b"usage: usermod <user> -g <group|gid>\r\n");
    }
    let name = &namebuf[..name_len];
    let gspec = &gbuf[..g_len];

    let mut src = [0u8; BUF];
    // Checked: this buffer is rewritten back over /etc/passwd below, so a 0 from
    // a transient error must not rebuild the file from nothing.
    let Some(slen) = ulib::read_file_checked(PASSWD_FILE, &mut src) else {
        die(b"usermod: could not read /etc/passwd - refusing to rewrite it\r\n");
    };

    // Copy the target account's fields out (the borrow of `src` ends here so the
    // rebuild below can reuse the buffer space).
    let mut home = [0u8; 128];
    let (uid, home_len, legacy) = match accounts::find_user_by_name(&src[..slen], name) {
        Some(acct) => {
            let hl = acct.home.len().min(home.len());
            home[..hl].copy_from_slice(&acct.home[..hl]);
            (acct.uid, hl, acct.secret)
        }
        None => die(b"usermod: no such user\r\n"),
    };

    // A LEGACY passwd line carries its secret inline, and the rewritten line (the
    // current 4-field format) has nowhere to put it. Dropping it would lock the
    // account out permanently - login would find nothing in /etc/shadow and
    // `Account::verify` is false without a secret - so migrate it first, and only
    // rewrite the passwd line once that has succeeded.
    if let Some(secret) = legacy {
        if !migrate_secret(name, &secret) {
            die(b"usermod: could not migrate the password to /etc/shadow - account unchanged\r\n");
        }
    }

    let new_gid = resolve_gid(gspec);

    // Build the rewritten line and splice it in.
    let mut line = [0u8; 256];
    let Some(llen) = accounts::format_account_line(&mut line, name, uid, new_gid, &home[..home_len])
    else {
        die(b"usermod: account line too long\r\n");
    };
    let mut out = [0u8; BUF];
    let (olen, replaced) = match accounts::replace_line(&src[..slen], &mut out, name, &line[..llen]) {
        Some(r) => r,
        None => die(b"usermod: /etc/passwd full\r\n"),
    };
    if !replaced {
        die(b"usermod: no such user\r\n");
    }
    let code = ulib::fs_write_bulk(PASSWD_FILE, &out[..olen]);
    if ulib::is_fs_error(code) {
        ulib::fs_error("usermod", code);
        ulib::exit(1);
    }
    ulib::con_write(b"usermod: updated ");
    ulib::con_write(name);
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

/// Copy a legacy inline secret into `/etc/shadow`, unless an entry is already
/// there (in which case the shadow file is authoritative and the inline copy is
/// stale). Returns whether the account is safe to rewrite.
#[inline(never)]
fn migrate_secret(name: &[u8], secret: &accounts::Secret) -> bool {
    let mut cur = [0u8; BUF];
    let Some(clen) = ulib::read_file_checked(SHADOW_FILE, &mut cur) else {
        return false;
    };
    if accounts::find_secret_by_name(&cur[..clen], name).is_some() {
        return true; // already migrated; the inline copy is redundant
    }
    let mut line = [0u8; 256];
    let Some(llen) =
        accounts::format_shadow_line(&mut line, name, &secret.salt[..secret.salt_len], &secret.hash)
    else {
        return false;
    };
    let mut out = [0u8; BUF];
    let Some(olen) = accounts::append_line(&cur[..clen], &mut out, &line[..llen]) else {
        return false;
    };
    // 0600 BEFORE the hash lands, not after: this used to write the secrets and
    // then chmod, which on a disk with no /etc/shadow created it world-readable
    // with a real hash already inside it. See ulib::write_private_file.
    if ulib::is_fs_error(ulib::write_private_file(SHADOW_FILE, &out[..olen])) {
        return false;
    }
    true
}

/// Resolve a `-g` value (a numeric gid, or a group name looked up in
/// `/etc/group`). Exits with an error if a name isn't found.
fn resolve_gid(spec: &[u8]) -> u32 {
    if let Some(v) = accounts::parse_dec(spec) {
        return v as u32;
    }
    let mut gbuf = [0u8; BUF];
    let glen = ulib::read_file_all(GROUP_FILE, &mut gbuf);
    match accounts::find_group_by_name(&gbuf[..glen], spec) {
        Some(g) => g.gid,
        None => die(b"usermod: no such group\r\n"),
    }
}

fn die(msg: &[u8]) -> ! {
    ulib::con_write(msg);
    ulib::exit(1);
}

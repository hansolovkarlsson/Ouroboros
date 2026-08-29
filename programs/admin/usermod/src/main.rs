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
    ulib::usage_if_requested(
        b"usage: usermod <user> [-g <group|gid>] [-G <grp>[,<grp>...]]  (-g primary, -G supplementary; root only)\r\n",
    );
    if ulib::getuid() != 0 {
        die(b"usermod: only root may modify accounts\r\n");
    }

    // Parse: <user> and -g <group|gid>.
    let mut namebuf = [0u8; 33];
    let mut name_len = 0usize;
    let mut gbuf = [0u8; 33];
    let mut g_len = 0usize;
    let mut bigg = [0u8; 128];
    let mut bigg_len = 0usize;
    let mut have_bigg = false;
    let mut i = 1u64;
    let argc = ulib::argc();
    while i < argc {
        let mut a = [0u8; 33];
        let n = ulib::arg(i, &mut a).unwrap_or(0);
        let tok = &a[..n];
        if tok == b"-g" {
            i += 1;
            g_len = operand(i, &mut gbuf, b"-g");
        } else if tok == b"-G" {
            i += 1;
            bigg_len = operand(i, &mut bigg, b"-G");
            // The shell does no quote handling, so `-G ""` arrives as the literal
            // two-character token `""` rather than as an empty argument. It did
            // clear the list - but only by accident, because no group is named
            // `""` so nothing was added back - while printing
            // `no such group (skipped): ""` and exiting 0, which reads as a
            // failure that in fact succeeded. Recognise the spelling the usage
            // text documents and mean it.
            if bigg[..bigg_len] == b"\"\""[..] || bigg[..bigg_len] == b"''"[..] {
                bigg_len = 0;
            }
            have_bigg = true;
        } else if name_len == 0 && !tok.is_empty() {
            namebuf[..n].copy_from_slice(tok);
            name_len = n;
        }
        i += 1;
    }
    if name_len == 0 || (g_len == 0 && !have_bigg) {
        die(b"usage: usermod <user> [-g <group|gid>] [-G <grp>[,<grp>...]]\r\n");
    }
    let name = &namebuf[..name_len];

    // -G first: it rewrites /etc/group, which -g's name lookup reads. Doing it in
    // this order means `usermod u -g staff -G staff` sees a consistent file.
    if have_bigg {
        // Validate the account exists BEFORE touching /etc/group. -g has always
        // done this; without it a typo writes a nonexistent user into a group's
        // member list and reports success.
        let mut pcheck = [0u8; BUF];
        let Some(pn) = ulib::read_file_checked(PASSWD_FILE, &mut pcheck) else {
            die(b"usermod: could not read /etc/passwd\r\n");
        };
        if accounts::find_user_by_name(&pcheck[..pn], name).is_none() {
            die(b"usermod: no such user\r\n");
        }
        set_supplementary(name, &bigg[..bigg_len]);
    }
    if g_len == 0 {
        ulib::con_write(b"usermod: updated ");
        ulib::con_write(name);
        ulib::con_write(b"\r\n");
        ulib::exit(0);
    }
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

/// Make `list` (comma-separated group names) the user's complete supplementary
/// membership: drop it from every group first, then add it to each named one.
/// A name that doesn't exist is reported and skipped rather than aborting the
/// whole change - the other memberships are still worth applying.
///
/// `#[inline(never)]`: two `SAFECOPY_MAX` buffers, kept off the caller's frame.
#[inline(never)]
fn set_supplementary(name: &[u8], list: &[u8]) {
    let mut cur = [0u8; BUF];
    let Some(mut clen) = ulib::read_file_checked(GROUP_FILE, &mut cur) else {
        ulib::con_write(b"usermod: could not read /etc/group - refusing to rewrite it\r\n");
        ulib::exit(1);
    };
    // Remove from every group, so the list given is the complete membership.
    let mut out = [0u8; BUF];
    let Some((n, changed)) = accounts::remove_group_member_everywhere(&cur[..clen], &mut out, name)
    else {
        // None means the rewrite did not fit. Swallowing it left the user in
        // groups -G was supposed to remove them from, and still reported success
        // - the exact inverse of the "complete membership" contract.
        die(b"usermod: /etc/group rewrite does not fit - no memberships were changed\r\n");
    };
    {
        if changed {
            let code = ulib::fs_write_bulk(GROUP_FILE, &out[..n]);
            if ulib::is_fs_error(code) {
                ulib::fs_error("usermod", code);
                ulib::exit(1);
            }
            cur[..n].copy_from_slice(&out[..n]);
            clen = n;
        }
    }
    // Then add to each named group, one rewrite per name.
    for g in list.split(|&c| c == b',') {
        if g.is_empty() {
            continue;
        }
        // "no change" has two causes and they are not the same message: the
        // group doesn't exist, or the user is already in it (which `-G a,a`
        // reaches for a group this loop just wrote to). Ask before reporting.
        let exists = accounts::find_group_by_name(&cur[..clen], g).is_some();
        match accounts::add_group_member(&cur[..clen], &mut out, g, name) {
            Some((n, true)) => {
                let code = ulib::fs_write_bulk(GROUP_FILE, &out[..n]);
                if ulib::is_fs_error(code) {
                    ulib::fs_error("usermod", code);
                    ulib::exit(1);
                }
                cur[..n].copy_from_slice(&out[..n]);
                clen = n;
            }
            // A None from add_group_member is "the rewrite did not fit", not
            // "already a member" - distinguishing them matters, because one is a
            // silent failure to apply what was asked.
            None if exists => {
                ulib::con_write(b"usermod: /etc/group rewrite does not fit - not added to: ");
                ulib::con_write(g);
                ulib::con_write(b"\r\n");
                ulib::exit(1);
            }
            Some((_, false)) if exists => {} // already a member - nothing to do
            _ => {
                ulib::con_write(b"usermod: no such group (skipped): ");
                ulib::con_write(g);
                ulib::con_write(b"\r\n");
            }
        }
    }
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

/// Fetch the operand following a flag, refusing one that is itself a flag.
///
/// `ulib::arg` reports only "present" or "absent", so the old guard caught
/// `usermod alice -G` but not `usermod alice -G -g staff` - there the NEXT FLAG
/// was consumed as the value. The damage was silent and total: `-G` rewrote
/// alice's memberships to the single group "-g", which meant dropping every
/// real one (the rewrite removes from all groups before it adds), the `-g`
/// change was discarded because the name slot was already filled, and usermod
/// printed "updated alice" and exited 0.
///
/// A group name or gid never begins with '-', so a leading '-' means the
/// operand is missing. An explicitly EMPTY operand still clears, deliberately -
/// that is `-G ""`, distinct from `-G` with nothing after it.
fn operand(i: u64, buf: &mut [u8], flag: &[u8]) -> usize {
    match ulib::arg(i, buf) {
        Some(n) if n == 0 || buf[0] != b'-' => n,
        _ => {
            ulib::con_write(b"usermod: ");
            ulib::con_write(flag);
            ulib::con_write(b" needs a value (use -G \"\" to clear all groups)\r\n");
            ulib::exit(1);
        }
    }
}

fn die(msg: &[u8]) -> ! {
    ulib::con_write(msg);
    ulib::exit(1);
}

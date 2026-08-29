//! `useradd <name> [-u <uid>] [-g <group|gid>]` - create a user account
//! (root only).
//!
//! Appends a `name:uid:gid:home:salt:hash` line to `/etc/passwd`, prompting for
//! the initial password (echo off, clock-derived salt). The home directory is
//! `/Users/<name>` - created (`mkdir`) and chowned to the new user (the chown/
//! chmod themselves are best-effort: ext2 records the owner, FAT/exFAT can't and
//! the "not supported" reply is ignored).
//!
//! Group handling (the primary-gid model): `-g <group|gid>` sets the primary
//! group (a name is resolved via `/etc/group`); with no `-g`, a **user-private
//! group** named after the user is created in `/etc/group` with `gid = uid` (the
//! Linux `useradd` default), so `id` shows a group name. If a group of that name
//! already exists, its gid is adopted rather than a second one invented.
//!
//! ## Ordering: everything fallible happens before the account exists
//! `useradd` touches three things - `/etc/group`, the home directory, and
//! `/etc/passwd` - and there is no multi-file transaction to lean on. So the
//! order is chosen to make the **`/etc/passwd` write the single commit point**:
//! the group entry and the home directory are created *first*, and only then is
//! the account line written. If any prep step fails, nothing is committed and
//! the tool exits non-zero; if the commit itself fails, the prep steps are rolled
//! back (the group line removed, a home directory we created removed). The
//! failure mode this avoids is the one a code review flagged: an account in
//! `/etc/passwd` whose primary group or home directory never got made, reported
//! as success.
//!
//! ## `/etc/skel`
//! If `/etc/skel` exists, its **top-level files** are copied into a home this
//! run created, each chowned to the new user and given the template's mode.
//! There is no `/etc/skel` by default - a deployment creates one if it wants
//! new accounts pre-populated. Subdirectories are reported and skipped (a first
//! cut; recursion needs a path stack this fixed frame doesn't have). The copy
//! runs *after* the commit and its failures are warnings, not errors: skel
//! content is a convenience, and the account, group, and home are already
//! complete without it - which also keeps the rollback above a plain `rmdir`.
//!
//! **Root only.** Byte-only parsing (PIE-safe). Interactive password entry works
//! because the shell hands a foreground `/bin` program the keyboard.

#![no_std]
#![no_main]

const PASSWD_FILE: &str = "/etc/passwd";
const GROUP_FILE: &str = "/etc/group";
const HOME_ROOT: &str = "/Users";
/// Template directory whose files are copied into a newly created home. Absent
/// by default - a deployment creates it if it wants one.
const SKEL_DIR: &str = "/etc/skel";
const BUF: usize = syscall_abi::SAFECOPY_MAX as usize;

/// What the primary group resolved to: an existing group's gid, or a
/// user-private group we still have to create.
enum Primary {
    Existing(u32),
    Create(u32),
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: useradd <name> [-u <uid>] [-g <group|gid>]  (root only)\r\n",
    );
    if ulib::getuid() != 0 {
        die(b"useradd: only root may add users\r\n");
    }

    // Parse args.
    let mut namebuf = [0u8; 33];
    let mut name_len = 0usize;
    let mut uid_opt: Option<u32> = None;
    let mut gbuf = [0u8; 33];
    let mut g_len = 0usize;
    let mut i = 1u64;
    let argc = ulib::argc();
    while i < argc {
        let mut a = [0u8; 33];
        let n = ulib::arg(i, &mut a).unwrap_or(0);
        let tok = &a[..n];
        if tok == b"-u" {
            i += 1;
            let mut u = [0u8; 12];
            let un = ulib::arg(i, &mut u).unwrap_or(0);
            match accounts::parse_dec(&u[..un]) {
                Some(v) => uid_opt = Some(v as u32),
                None => die(b"useradd: -u needs a numeric uid\r\n"),
            }
        } else if tok == b"-g" {
            i += 1;
            g_len = ulib::arg(i, &mut gbuf).unwrap_or(0);
        } else if name_len == 0 && !tok.is_empty() {
            namebuf[..n].copy_from_slice(tok);
            name_len = n;
        }
        i += 1;
    }
    if name_len == 0 {
        die(b"usage: useradd <name> [-u <uid>] [-g <group|gid>]\r\n");
    }
    let name = &namebuf[..name_len];

    // Home path: /Users/<name>.
    let mut homebuf = [0u8; 160];
    let mut hlen = 0usize;
    push(&mut homebuf, &mut hlen, HOME_ROOT.as_bytes());
    push(&mut homebuf, &mut hlen, b"/");
    push(&mut homebuf, &mut hlen, name);
    let home = &homebuf[..hlen];
    let Ok(home_str) = core::str::from_utf8(home) else {
        die(b"useradd: user name is not valid UTF-8\r\n");
    };

    // --- Prepare (no writes yet) -------------------------------------------
    // The new /etc/passwd content is built here and held until the commit at the
    // bottom; `pbuf` is scoped so the passwd buffer isn't live across the group
    // and home steps (this program runs on a 32KB guarded stack).
    let mut out = [0u8; BUF];
    let olen;
    let uid;
    let primary;
    {
        let mut pbuf = [0u8; BUF];
        let plen = ulib::read_file_all(PASSWD_FILE, &mut pbuf);
        if accounts::user_exists(&pbuf[..plen], name) {
            die(b"useradd: user already exists\r\n");
        }
        uid = uid_opt.unwrap_or_else(|| accounts::next_free_uid(&pbuf[..plen]));
        primary = resolve_group(if g_len > 0 { Some(&gbuf[..g_len]) } else { None }, name, uid);
        let gid = match primary {
            Primary::Existing(g) | Primary::Create(g) => g,
        };

        // Prompt for the initial password (twice, echo off).
        ulib::con_write(b"New password: ");
        let mut pw = [0u8; 64];
        let pwl = ulib::read_line(&mut pw, false);
        ulib::con_write(b"\r\nRetype new password: ");
        let mut pw2 = [0u8; 64];
        let pwl2 = ulib::read_line(&mut pw2, false);
        ulib::con_write(b"\r\n");
        if pw[..pwl] != pw2[..pwl2] {
            die(b"useradd: passwords do not match\r\n");
        }
        let salt = accounts::make_salt(ulib::monotonic_us());
        let hash = accounts::hash_password(&salt, &pw[..pwl]);

        let mut line = [0u8; 256];
        let Some(llen) =
            accounts::format_account_line(&mut line, name, uid, gid, home, &salt, &hash)
        else {
            die(b"useradd: account line too long\r\n");
        };
        let Some(n) = accounts::append_line(&pbuf[..plen], &mut out, &line[..llen]) else {
            die(b"useradd: /etc/passwd full (raise the account-file cap)\r\n");
        };
        olen = n;
    }
    let gid = match primary {
        Primary::Existing(g) | Primary::Create(g) => g,
    };

    // --- Prep step 1: the user-private group, if we're making one ----------
    let made_group = matches!(primary, Primary::Create(_));
    if made_group {
        if let Err(code) = add_group(name, gid) {
            ulib::fs_error("useradd", code);
            die(b"useradd: could not create the group; no account was created\r\n");
        }
    }

    // --- Prep step 2: the home directory -----------------------------------
    let made_home = match make_home(home_str, uid, gid) {
        Ok(created) => created,
        Err(code) => {
            if made_group {
                remove_group(name);
            }
            ulib::fs_error("useradd", code);
            die(b"useradd: could not create the home directory; no account was created\r\n");
        }
    };

    // --- Commit: the account line ------------------------------------------
    let code = ulib::fs_write_bulk(PASSWD_FILE, &out[..olen]);
    if ulib::is_fs_error(code) {
        // Undo the prep so a failed run leaves nothing behind.
        if made_home {
            let _ = ulib::fs_op_path(syscall_abi::FSOP_RMDIR, home_str);
        }
        if made_group {
            remove_group(name);
        }
        ulib::fs_error("useradd", code);
        ulib::exit(1);
    }

    // The account exists now. Populating the home from /etc/skel is a
    // convenience, not part of the account's integrity, so it runs *after* the
    // commit and its failures are warnings: the rollback above can then stay a
    // plain `rmdir` (which would fail on a directory we had already filled).
    if made_home {
        populate_home(home_str, uid, gid);
    }

    ulib::con_write(b"useradd: created ");
    ulib::con_write(name);
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

/// Copy the top-level files of `/etc/skel` into a freshly created home, giving
/// each to the new user. Silent when there is no `/etc/skel` (the default); a
/// file that can't be copied is a warning, since the account itself is already
/// complete.
///
/// First cut: **top-level regular files only**. A subdirectory is reported and
/// skipped rather than copied recursively - recursion needs a path stack this
/// program's fixed frame doesn't have, and no consumer wants it yet.
///
/// `#[inline(never)]`: the listing and copy buffers stay out of the caller's
/// frame (a 32 KB guarded stack).
#[inline(never)]
fn populate_home(home: &str, uid: u32, gid: u32) {
    let mut listing = [0u8; 512];
    let n = ulib::fs_list_dir(SKEL_DIR, &mut listing);
    if ulib::is_fs_error(n) {
        return; // no /etc/skel - the normal case, nothing to say
    }
    let listing = &listing[..(n as usize).min(listing.len())];

    for entry in listing.split(|&c| c == b'\n') {
        if entry.is_empty() {
            continue;
        }
        // fsd marks a directory with a trailing '/'.
        if entry[entry.len() - 1] == b'/' {
            ulib::con_write(b"useradd: skipping /etc/skel/");
            ulib::con_write(entry);
            ulib::con_write(b" (skel subdirectories are not copied)\r\n");
            continue;
        }

        let mut srcbuf = [0u8; 160];
        let mut slen = 0usize;
        push(&mut srcbuf, &mut slen, SKEL_DIR.as_bytes());
        push(&mut srcbuf, &mut slen, b"/");
        push(&mut srcbuf, &mut slen, entry);
        let mut dstbuf = [0u8; 160];
        let mut dlen = 0usize;
        push(&mut dstbuf, &mut dlen, home.as_bytes());
        push(&mut dstbuf, &mut dlen, b"/");
        push(&mut dstbuf, &mut dlen, entry);
        let (Ok(src), Ok(dst)) = (
            core::str::from_utf8(&srcbuf[..slen]),
            core::str::from_utf8(&dstbuf[..dlen]),
        ) else {
            continue;
        };

        if let Err(code) = copy_file(src, dst) {
            ulib::con_write(b"useradd: could not copy /etc/skel/");
            ulib::con_write(entry);
            ulib::con_write(b" into the home directory\r\n");
            ulib::fs_error("useradd", code);
            continue;
        }
        // The copy is the user's file: give it to them, and carry the
        // template's mode across where the filesystem models one.
        let _ = ulib::fs_chown(dst, Some(uid as u16), Some(gid as u16));
        let mut info = [0u8; 64];
        if !ulib::is_fs_error(ulib::fs_stat(src, &mut info)) {
            if let Some((mode, _, _)) = ulib::stat_mode(&info) {
                let _ = ulib::fs_chmod(dst, mode & 0o7777);
            }
        }
    }
}

/// Stream `src` into a fresh `dst`, chunk by chunk - the same
/// truncate-then-`fs_read_bulk`/`fs_write_at` shape `/bin/cp` uses (a program
/// can't spawn `cp`, so the loop is repeated, not the code). Returns the fs
/// error code on failure.
#[inline(never)]
fn copy_file(src: &str, dst: &str) -> Result<(), u64> {
    let code = ulib::fs_write_bulk(dst, &[]); // create/truncate
    if ulib::is_fs_error(code) {
        return Err(code);
    }
    let mut chunk = [0u8; syscall_abi::SAFECOPY_MAX as usize];
    let mut offset: u64 = 0;
    loop {
        let n = ulib::fs_read_bulk(src, offset, &mut chunk);
        if ulib::is_fs_error(n) {
            return Err(n);
        }
        if n == 0 {
            return Ok(());
        }
        let n = (n as usize).min(chunk.len());
        let code = ulib::fs_write_at(dst, offset, &chunk[..n]);
        if ulib::is_fs_error(code) {
            return Err(code);
        }
        offset += n as u64;
    }
}

/// Decide the new account's primary group. `gspec` is the `-g` argument (a gid
/// or a group name); with none, the user-private-group default applies - adopt
/// an existing group of the user's own name if there is one (so the account's
/// gid always names a real group), else create one with `gid == uid`.
///
/// `#[inline(never)]`: the group file is a [`BUF`]-sized stack buffer, kept out
/// of the caller's frame.
#[inline(never)]
fn resolve_group(gspec: Option<&[u8]>, name: &[u8], uid: u32) -> Primary {
    let mut grbuf = [0u8; BUF];
    let grlen = ulib::read_file_all(GROUP_FILE, &mut grbuf);
    let group = &grbuf[..grlen];
    match gspec {
        Some(spec) => {
            if let Some(v) = accounts::parse_dec(spec) {
                Primary::Existing(v as u32)
            } else {
                match accounts::find_group_by_name(group, spec) {
                    Some(g) => Primary::Existing(g.gid),
                    None => die(b"useradd: no such group\r\n"),
                }
            }
        }
        None => match accounts::find_group_by_name(group, name) {
            Some(g) => Primary::Existing(g.gid),
            None => Primary::Create(uid),
        },
    }
}

/// Append a `name:gid:` line to `/etc/group`. Returns the fs error code on
/// failure (the caller aborts without committing an account).
#[inline(never)]
fn add_group(name: &[u8], gid: u32) -> Result<(), u64> {
    let mut grbuf = [0u8; BUF];
    let grlen = ulib::read_file_all(GROUP_FILE, &mut grbuf);
    let mut gline = [0u8; 64];
    let Some(gll) = accounts::format_group_line(&mut gline, name, gid, b"") else {
        return Err(syscall_abi::FS_ERROR);
    };
    let mut gout = [0u8; BUF];
    let Some(goutl) = accounts::append_line(&grbuf[..grlen], &mut gout, &gline[..gll]) else {
        return Err(syscall_abi::FS_ERR_DISK_FULL);
    };
    let code = ulib::fs_write_bulk(GROUP_FILE, &gout[..goutl]);
    if ulib::is_fs_error(code) {
        return Err(code);
    }
    Ok(())
}

/// Roll back [`add_group`]: rewrite `/etc/group` without the line we added.
/// Best-effort - we're already on a failure path, and a leftover group entry is
/// less harmful than the account line we refused to write.
#[inline(never)]
fn remove_group(name: &[u8]) {
    let mut grbuf = [0u8; BUF];
    let grlen = ulib::read_file_all(GROUP_FILE, &mut grbuf);
    let mut gout = [0u8; BUF];
    if let Some((n, removed)) = accounts::remove_line(&grbuf[..grlen], &mut gout, name) {
        if removed {
            let _ = ulib::fs_write_bulk(GROUP_FILE, &gout[..n]);
        }
    }
}

/// Create `/Users` (if absent) and the account's home directory, then give it to
/// the new user. Returns `Ok(true)` if we created the home (so a rollback knows
/// to remove it), `Ok(false)` if it already existed, or the fs error code.
///
/// The chown/chmod stay best-effort: only ext2 models ownership, and FAT/exFAT
/// answer `FS_ERR_NOT_SUPPORTED`, which is not a reason to refuse the account.
#[inline(never)]
fn make_home(home: &str, uid: u32, gid: u32) -> Result<bool, u64> {
    let _ = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, HOME_ROOT); // ok if it exists
    let mk = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, home);
    let created = if !ulib::is_fs_error(mk) {
        true
    } else if mk == syscall_abi::FS_ERR_ALREADY_EXISTS {
        // Something is already there - fine if it's a directory (a leftover from
        // an earlier failed run, say), but a *file* of that name can't be a home,
        // and silently chmod-ing it would be worse than refusing.
        let mut info = [0u8; 64];
        let st = ulib::fs_stat(home, &mut info);
        if ulib::is_fs_error(st) || !ulib::stat_is_dir(&info) {
            return Err(syscall_abi::FS_ERR_NOT_A_DIRECTORY);
        }
        ulib::con_write(b"useradd: home directory already existed; adopting it\r\n");
        false
    } else {
        return Err(mk);
    };
    let _ = ulib::fs_chown(home, Some(uid as u16), Some(gid as u16));
    let _ = ulib::fs_chmod(home, 0o755);
    Ok(created)
}

/// Append `src` to `buf` at `*len`, advancing `*len` (bounded).
fn push(buf: &mut [u8], len: &mut usize, src: &[u8]) {
    for &b in src {
        if *len < buf.len() {
            buf[*len] = b;
            *len += 1;
        }
    }
}

fn die(msg: &[u8]) -> ! {
    ulib::con_write(msg);
    ulib::exit(1);
}

//! `useradd <name> [-u <uid>] [-g <group|gid>]` - create a user account
//! (root only).
//!
//! Appends a `name:uid:gid:home:salt:hash` line to `/etc/passwd`, prompting for
//! the initial password (echo off, clock-derived salt). The home directory is
//! `/Users/<name>` - created (`mkdir`) and chowned to the new user (best-effort:
//! ext2 records the owner, FAT/exFAT can't and the "not supported" reply is
//! ignored).
//!
//! Group handling (the primary-gid model): `-g <group|gid>` sets the primary
//! group (a name is resolved via `/etc/group`); with no `-g`, a **user-private
//! group** named after the user is created in `/etc/group` with `gid = uid` (the
//! Linux `useradd` default), so `id` shows a group name.
//!
//! **Root only.** Byte-only parsing (PIE-safe). Interactive password entry works
//! because the shell hands a foreground `/bin` program the keyboard.

#![no_std]
#![no_main]

const PASSWD_FILE: &str = "/etc/passwd";
const GROUP_FILE: &str = "/etc/group";
const HOME_ROOT: &[u8] = b"/Users";
const BUF: usize = syscall_abi::SAFECOPY_MAX as usize;

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

    // Read passwd; refuse a duplicate.
    let mut pbuf = [0u8; BUF];
    let plen = ulib::read_file_all(PASSWD_FILE, &mut pbuf);
    if accounts::user_exists(&pbuf[..plen], name) {
        die(b"useradd: user already exists\r\n");
    }
    let uid = uid_opt.unwrap_or_else(|| accounts::next_free_uid(&pbuf[..plen]));

    // Resolve the primary group, deciding whether to create a user-private group.
    let mut make_upg = false;
    let gid = if g_len > 0 {
        let gspec = &gbuf[..g_len];
        if let Some(v) = accounts::parse_dec(gspec) {
            v as u32
        } else {
            let mut grbuf = [0u8; BUF];
            let grlen = ulib::read_file_all(GROUP_FILE, &mut grbuf);
            match accounts::find_group_by_name(&grbuf[..grlen], gspec) {
                Some(g) => g.gid,
                None => die(b"useradd: no such group\r\n"),
            }
        }
    } else {
        make_upg = true;
        uid // user-private group, gid == uid
    };

    // Home path: /Users/<name>.
    let mut homebuf = [0u8; 160];
    let mut hlen = 0usize;
    push(&mut homebuf, &mut hlen, HOME_ROOT);
    push(&mut homebuf, &mut hlen, b"/");
    push(&mut homebuf, &mut hlen, name);
    let home = &homebuf[..hlen];

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

    // Append the account line and write /etc/passwd back.
    let mut line = [0u8; 256];
    let Some(llen) = accounts::format_account_line(&mut line, name, uid, gid, home, &salt, &hash)
    else {
        die(b"useradd: account line too long\r\n");
    };
    let mut out = [0u8; BUF];
    let Some(olen) = accounts::append_line(&pbuf[..plen], &mut out, &line[..llen]) else {
        die(b"useradd: /etc/passwd full (raise the account-file cap)\r\n");
    };
    let code = ulib::fs_write_bulk(PASSWD_FILE, &out[..olen]);
    if ulib::is_fs_error(code) {
        ulib::fs_error("useradd", code);
        ulib::exit(1);
    }

    // Create the user-private group if we chose one.
    if make_upg {
        let mut grbuf = [0u8; BUF];
        let grlen = ulib::read_file_all(GROUP_FILE, &mut grbuf);
        if !accounts::group_exists(&grbuf[..grlen], name) {
            let mut gline = [0u8; 64];
            if let Some(gll) = accounts::format_group_line(&mut gline, name, gid, b"") {
                let mut gout = [0u8; BUF];
                if let Some(goutl) = accounts::append_line(&grbuf[..grlen], &mut gout, &gline[..gll])
                {
                    let _ = ulib::fs_write_bulk(GROUP_FILE, &gout[..goutl]);
                }
            }
        }
    }

    // Create and own the home directory (best-effort; chown/chmod are ext2-only).
    let home_str = core::str::from_utf8(home).unwrap_or("");
    let _ = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, "/Users"); // ok if it exists
    let mk = ulib::fs_op_path(syscall_abi::FSOP_MKDIR, home_str);
    if ulib::is_fs_error(mk) {
        ulib::con_write(b"useradd: account created, but home directory could not be made\r\n");
    } else {
        let _ = ulib::fs_chown(home_str, Some(uid as u16), Some(gid as u16));
        let _ = ulib::fs_chmod(home_str, 0o755);
    }

    ulib::con_write(b"useradd: created ");
    ulib::con_write(name);
    ulib::con_write(b"\r\n");
    ulib::exit(0);
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

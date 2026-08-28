//! `chown <owner> <file>` - change a file's owner uid and/or gid, the write
//! twin of `ls -l`'s owner columns. `<owner>` is **numeric** (there is no
//! `/etc/passwd` yet - user names come with the login/permissions arc):
//!
//! - `uid`        set the user, leave the group
//! - `uid:gid`    set both
//! - `:gid`       set the group, leave the user
//! - `uid:`       set the user, leave the group (same as `uid`)
//!
//! **ext2 only**: FAT32/exFAT/`/proc` can't model an owner and return "not
//! supported by this filesystem". Parsing works in bytes, never slicing a
//! `&str` by a runtime index (the PIE relocation trap - see `docs/processes.md`).

#![no_std]
#![no_main]

/// Parse a decimal `u16` from bytes (uid/gid are 16-bit in the ext2 inode
/// fields this driver reads), or `None` on a non-digit or overflow.
fn parse_dec_u16(b: &[u8]) -> Option<u16> {
    if b.is_empty() {
        return None;
    }
    let mut v: u16 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u16)?;
    }
    Some(v)
}

/// Parse an owner spec into `(uid, gid)`, each `None` meaning "leave unchanged".
/// Returns `None` if the spec is malformed (e.g. a bare `:`, or a non-numeric
/// field).
fn parse_owner(spec: &[u8]) -> Option<(Option<u16>, Option<u16>)> {
    // Split on the first ':'.
    let mut colon = None;
    for (i, &c) in spec.iter().enumerate() {
        if c == b':' {
            colon = Some(i);
            break;
        }
    }
    match colon {
        None => Some((Some(parse_dec_u16(spec)?), None)),
        Some(i) => {
            let ub = &spec[..i];
            let gb = &spec[i + 1..];
            let uid = if ub.is_empty() { None } else { Some(parse_dec_u16(ub)?) };
            let gid = if gb.is_empty() { None } else { Some(parse_dec_u16(gb)?) };
            // A bare ":" (both empty) changes nothing - reject as a user error.
            if uid.is_none() && gid.is_none() {
                return None;
            }
            Some((uid, gid))
        }
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: chown <uid|uid:gid|:gid> <file>  (numeric ids; ext2 only)\r\n",
    );

    let mut ownerbuf = [0u8; 32];
    let (uid, gid) = match ulib::arg(1, &mut ownerbuf) {
        Some(len) => match parse_owner(&ownerbuf[..len]) {
            Some(pair) => pair,
            None => {
                ulib::con_write(b"chown: invalid owner (want uid, uid:gid, or :gid, numeric)\r\n");
                ulib::exit(1);
            }
        },
        None => {
            ulib::con_write(b"chown: usage: chown <uid|uid:gid|:gid> <file>\r\n");
            ulib::exit(1);
        }
    };

    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(2, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };
    if arg.is_empty() {
        ulib::con_write(b"chown: missing file argument\r\n");
        ulib::exit(1);
    }

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"chown: path too long\r\n");
            ulib::exit(1);
        }
    };

    let code = ulib::fs_chown(path, uid, gid);
    if ulib::is_fs_error(code) {
        ulib::fs_error("chown", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

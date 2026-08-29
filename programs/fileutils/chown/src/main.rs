//! `chown <owner> <file>` - change a file's owner uid and/or gid, the write
//! twin of `ls -l`'s owner columns. Each field of `<owner>` is either a **name**
//! (resolved through `/etc/passwd` / `/etc/group` with the shared [`accounts`]
//! lookups, the same parser `login`/`su`/`id` use) or a **numeric id**:
//!
//! - `user`             set the user, leave the group
//! - `user:group`       set both
//! - `:group`           set the group, leave the user
//! - `user:`            set the user, leave the group (same as `user`)
//!
//! An all-digits field is taken as a numeric id, never looked up as a name -
//! the same rule `su` and `useradd -g` already follow, so an id and a name never
//! disagree about what `chown 1000 f` means.
//!
//! **Divergence from POSIX/GNU, on purpose:** there, a trailing colon
//! (`chown alice: f`) also sets the group to *alice's login group*. Here it
//! leaves the group alone, matching the numeric form's long-standing behaviour;
//! write `chown alice:alice` (or `alice:1000`) to set both.
//!
//! **ext2 only**: FAT32/exFAT/`/proc` can't model an owner and return "not
//! supported by this filesystem". Parsing works in bytes, never slicing a
//! `&str` by a runtime index (the PIE relocation trap - see `docs/processes.md`).

#![no_std]
#![no_main]

/// Read cap for the account files, matching `login`/`su`/`id`/`useradd` (~20
/// accounts) so a name lookup isn't truncated.
const ACCT_BUF: usize = syscall_abi::SAFECOPY_MAX as usize;

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

/// True if every byte is a digit (and there is at least one) - what makes a
/// field an id rather than a name.
fn all_digits(b: &[u8]) -> bool {
    !b.is_empty() && b.iter().all(|c| c.is_ascii_digit())
}

/// The two fields of an owner spec (user, group), each `None` when the spec
/// leaves that one unchanged.
type OwnerFields<'a> = (Option<&'a [u8]>, Option<&'a [u8]>);

/// Split an owner spec into its user and group fields, each `None` meaning
/// "leave unchanged". Returns `None` if the spec is malformed (empty, or a bare
/// `:` that would change nothing).
fn split_owner(spec: &[u8]) -> Option<OwnerFields<'_>> {
    match spec.iter().position(|&c| c == b':') {
        None => {
            if spec.is_empty() {
                None
            } else {
                Some((Some(spec), None))
            }
        }
        Some(i) => {
            let ub = &spec[..i];
            let gb = &spec[i + 1..];
            let u = if ub.is_empty() { None } else { Some(ub) };
            let g = if gb.is_empty() { None } else { Some(gb) };
            if u.is_none() && g.is_none() {
                return None; // a bare ":" changes nothing - a user error
            }
            Some((u, g))
        }
    }
}

/// Look up a user name in `/etc/passwd`, returning its uid.
///
/// `#[inline(never)]`: the passwd file is a 2 KB stack buffer, kept out of the
/// caller's frame (this program runs on a 32 KB guarded stack).
#[inline(never)]
fn user_by_name(name: &[u8]) -> Option<u32> {
    let mut buf = [0u8; ACCT_BUF];
    let len = ulib::read_file_all("/etc/passwd", &mut buf);
    accounts::find_user_by_name(&buf[..len], name).map(|a| a.uid)
}

/// Look up a group name in `/etc/group`, returning its gid.
#[inline(never)]
fn group_by_name(name: &[u8]) -> Option<u32> {
    let mut buf = [0u8; ACCT_BUF];
    let len = ulib::read_file_all("/etc/group", &mut buf);
    accounts::find_group_by_name(&buf[..len], name).map(|g| g.gid)
}

/// Narrow a looked-up 32-bit id to the 16-bit field the ext2 inode carries.
fn fit_u16(id: u32, what: &[u8]) -> u16 {
    if id > u16::MAX as u32 {
        ulib::con_write(b"chown: ");
        ulib::con_write(what);
        ulib::con_write(b" id is too large for this filesystem (max 65535)\r\n");
        ulib::exit(1);
    }
    id as u16
}

/// Resolve one field to an id: all digits means a numeric id, anything else is
/// a name looked up in the account files. Exits with a message on a bad field.
fn resolve_field(field: &[u8], is_user: bool) -> u16 {
    if all_digits(field) {
        return match parse_dec_u16(field) {
            Some(v) => v,
            None => {
                ulib::con_write(b"chown: id out of range (max 65535)\r\n");
                ulib::exit(1);
            }
        };
    }
    let looked_up = if is_user { user_by_name(field) } else { group_by_name(field) };
    match looked_up {
        Some(id) => fit_u16(id, if is_user { b"user" } else { b"group" }),
        None => {
            ulib::con_write(if is_user { b"chown: no such user: " } else { b"chown: no such group: " });
            ulib::con_write(field);
            ulib::con_write(b"\r\n");
            ulib::exit(1);
        }
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: chown <user|user:group|:group> <file>  (names or numeric ids; ext2 only)\r\n",
    );

    let mut ownerbuf = [0u8; 72];
    let (uid, gid) = match ulib::arg(1, &mut ownerbuf) {
        Some(len) => match split_owner(&ownerbuf[..len]) {
            Some((u, g)) => (u.map(|f| resolve_field(f, true)), g.map(|f| resolve_field(f, false))),
            None => {
                ulib::con_write(b"chown: invalid owner (want user, user:group, or :group)\r\n");
                ulib::exit(1);
            }
        },
        None => {
            ulib::con_write(b"chown: usage: chown <user|user:group|:group> <file>\r\n");
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

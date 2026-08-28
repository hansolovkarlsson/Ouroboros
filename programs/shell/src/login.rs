//! The shell's **login gate** (users/permissions arc, step 2).
//!
//! At the start of every session `main` calls [`login`], which prompts for a
//! username + password, verifies them against `/etc/passwd`, and - on success -
//! `SET_ID`s this task (the shell, task 0) down from root to that user. Because
//! the kernel remembers the *saved* identity, logout can restore root and
//! re-prompt (see the kernel's `SET_ID` and `tasks::inherit_id`), so the shell
//! never has to leave slot 0.
//!
//! `/etc/passwd` format, one account per line:
//!   `name:uid:gid:home:salt_hex:hash_hex`
//! where `hash = SHA-256(salt || password)`. The salt+hash are precomputed at
//! build time (`scripts/mkpasswd.py`); login only *verifies*, so it needs no
//! runtime randomness. If `/etc/passwd` is absent (a fresh/formatted disk, or a
//! test image without one), login falls back to a single-user **root** session
//! so the machine stays usable - the standard "no accounts configured"
//! bootstrap.
//!
//! All parsing is byte-only (no `&str` slicing by a runtime index - the PIE
//! relocation trap), and the password check is constant-time
//! ([`sha256::digest_eq`]).

use crate::sha256;
use crate::CWD_SIZE;

const PASSWD_PATH: &str = "/etc/passwd";
/// Read cap for `/etc/passwd`. Bounded by `FS_DATA_MAX` (512): the shell's
/// `fs_read_file` is one inline `NP_READ_FILE`, whose `want` fsd rejects above
/// that cap. 512 bytes holds ~5 accounts (each line is ~90 bytes); a longer
/// passwd file would need a chunked read (a documented follow-up).
const PASSWD_MAX: usize = syscall_abi::FS_DATA_MAX as usize;
const CR: u8 = 13;
const LF: u8 = 10;
const BS: u8 = 8;
const DEL: u8 = 127;

/// A completed login: the home directory has been written into the caller's cwd
/// buffer, and this task's identity is already the logged-in user.
pub struct Session {
    /// Length of the home path written into the cwd buffer `login` was given.
    pub cwd_len: usize,
}

/// The matched account's fields (borrowing the passwd buffer for `home`).
struct Cred<'a> {
    uid: u32,
    gid: u32,
    home: &'a [u8],
}

/// Run the login gate. Writes the session's home directory into `cwd` and
/// returns its length; on return this task's identity is the logged-in user
/// (or root, if there is no `/etc/passwd`). Loops until authentication succeeds.
pub fn login(cwd: &mut [u8; CWD_SIZE]) -> Session {
    let mut pbuf = [0u8; PASSWD_MAX];
    let plen = match read_passwd(&mut pbuf) {
        Some(n) => n,
        None => {
            crate::print_line("login: no /etc/passwd - starting a root session");
            return root_at(cwd);
        }
    };
    let passwd = &pbuf[..plen];

    loop {
        crate::print_str("\r\nlogin: ");
        let mut ubuf = [0u8; 32];
        let ulen = read_field(&mut ubuf, true);
        // read_field returns on Enter without echoing a newline, so move to the
        // next line before the password prompt.
        crate::print_str("\r\npassword: ");
        let mut wbuf = [0u8; 64];
        let wlen = read_field(&mut wbuf, false);
        crate::print_str("\r\n");

        if let Some(cred) = verify(passwd, &ubuf[..ulen], &wbuf[..wlen]) {
            // Drop from root to the user. The kernel saves root as this task's
            // saved identity, so logout can restore it.
            crate::syscall4(syscall_abi::SET_ID, cred.uid as u64, cred.gid as u64, 0, 0);
            let hlen = write_cwd(cwd, cred.home);
            return Session { cwd_len: hlen };
        }
        crate::print_line("Login incorrect");
    }
}

/// A root session at `/` (the no-`/etc/passwd` fallback). No `SET_ID` needed -
/// the shell boots as root already.
fn root_at(cwd: &mut [u8; CWD_SIZE]) -> Session {
    Session { cwd_len: write_cwd(cwd, b"/") }
}

/// Copy `home` into the cwd buffer (defaulting to `/` if empty), returning its
/// length.
fn write_cwd(cwd: &mut [u8; CWD_SIZE], home: &[u8]) -> usize {
    let home = if home.is_empty() { b"/".as_slice() } else { home };
    let n = home.len().min(cwd.len());
    cwd[..n].copy_from_slice(&home[..n]);
    n
}

/// Read `/etc/passwd` into `buf`, returning its length, or `None` if there's no
/// such file. Retries a bounded number of times while the reply is `NO_FS` (the
/// filesystem server may still be mounting the disk at boot); a real
/// "not found" returns `None` immediately (the root fallback).
fn read_passwd(buf: &mut [u8]) -> Option<usize> {
    let mut tries = 0;
    loop {
        let r = crate::fs_read_file(PASSWD_PATH, buf);
        if r < syscall_abi::FS_ERR_MIN {
            return Some((r as usize).min(buf.len()));
        }
        if r == syscall_abi::NO_FS && tries < 200 {
            tries += 1;
            continue; // fsd still mounting - the round trip itself paces this
        }
        return None; // no filesystem, or no /etc/passwd -> root fallback
    }
}

/// Read one line of keyboard input into `buf` (up to its length), returning the
/// count. Submits on CR/LF; supports destructive backspace. Echoes each byte
/// when `echo` is set (the username); a password is read silently.
fn read_field(buf: &mut [u8], echo: bool) -> usize {
    let mut len = 0usize;
    loop {
        let b = crate::read_char();
        match b {
            CR | LF => return len,
            BS | DEL => {
                if len > 0 {
                    len -= 1;
                    if echo {
                        crate::putc(BS);
                        crate::putc(b' ');
                        crate::putc(BS);
                    }
                }
            }
            _ if b < 0x20 => {} // ignore other control bytes
            _ => {
                if len < buf.len() {
                    buf[len] = b;
                    len += 1;
                    if echo {
                        crate::putc(b);
                    }
                }
            }
        }
    }
}

/// Find `user` in the passwd data and check `pass`. Returns the account's
/// uid/gid/home on a match, or `None` (unknown user, or wrong password - a
/// matched user with a bad password does *not* fall through to another line).
fn verify<'a>(passwd: &'a [u8], user: &[u8], pass: &[u8]) -> Option<Cred<'a>> {
    for line in passwd.split(|&c| c == LF) {
        if line.is_empty() {
            continue;
        }
        let mut fields = line.split(|&c| c == b':');
        let name = fields.next()?;
        if name != user {
            continue;
        }
        let uid = parse_dec(fields.next()?)? as u32;
        let gid = parse_dec(fields.next()?)? as u32;
        let home = fields.next()?;
        let salt_hex = fields.next()?;
        let hash_hex = fields.next()?;

        let mut salt = [0u8; 32];
        let salt_len = hex_decode(salt_hex, &mut salt)?;
        let mut stored = [0u8; sha256::DIGEST];
        if hex_decode(hash_hex, &mut stored)? != sha256::DIGEST {
            return None;
        }
        let digest = sha256::sha256_two(&salt[..salt_len], pass);
        return if sha256::digest_eq(&digest, &stored) {
            Some(Cred { uid, gid, home })
        } else {
            None
        };
    }
    None
}

/// Decimal `u64` from bytes (uid/gid fields), or `None` on a non-digit.
fn parse_dec(b: &[u8]) -> Option<u64> {
    if b.is_empty() {
        return None;
    }
    let mut v: u64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as u64)?;
    }
    Some(v)
}

/// Decode a hex string into `out`, returning the byte count, or `None` on an odd
/// length or a non-hex digit.
fn hex_decode(hex: &[u8], out: &mut [u8]) -> Option<usize> {
    if !hex.len().is_multiple_of(2) || hex.len() / 2 > out.len() {
        return None;
    }
    for i in 0..hex.len() / 2 {
        let hi = hex_val(hex[i * 2])?;
        let lo = hex_val(hex[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(hex.len() / 2)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

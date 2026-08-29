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
//! where `hash = SHA-256(salt || password)`. All parsing, hashing, and the
//! constant-time password check live in the shared [`accounts`] crate (the same
//! logic the `/bin` account tools use); this module only does the keyboard I/O
//! and the `SET_ID`. If `/etc/passwd` is absent (a fresh/formatted disk, or a
//! test image without one), login falls back to a single-user **root** session
//! so the machine stays usable - the standard "no accounts configured"
//! bootstrap.
//!
//! All parsing is byte-only (no `&str` slicing by a runtime index - the PIE
//! relocation trap).

use crate::CWD_SIZE;

const PASSWD_PATH: &str = "/etc/passwd";
/// Read cap for `/etc/passwd`, matching the account tools' write cap
/// ([`accounts`]/`useradd` bound writes to `SAFECOPY_MAX`). fsd caps a single
/// inline read at `FS_DATA_MAX` (512), so the shared `read_account_file` loops
/// `fs_read_at` to fill this buffer in 512-byte chunks. 2 KB holds ~20 accounts
/// (each line is ~90 bytes); beyond that a bigger buffer + the same loop is all
/// it takes.
const PASSWD_MAX: usize = syscall_abi::SAFECOPY_MAX as usize;
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

/// Run the login gate. Writes the session's home directory into `cwd` and
/// returns its length; on return this task's identity is the logged-in user
/// (or root, if there is no `/etc/passwd`). Loops until authentication succeeds.
pub fn login(cwd: &mut [u8; CWD_SIZE]) -> Session {
    let mut pbuf = [0u8; PASSWD_MAX];
    let plen = crate::read_account_file(PASSWD_PATH, &mut pbuf);
    if plen == 0 {
        crate::print_line("login: no /etc/passwd - starting a root session");
        return root_at(cwd);
    }
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

        if let Some(acct) = accounts::find_user_by_name(passwd, &ubuf[..ulen]) {
            if acct.verify(&wbuf[..wlen]) {
                // Drop from root to the user. The kernel saves root as this
                // task's saved identity, so logout can restore it.
                crate::syscall4(
                    syscall_abi::SET_ID,
                    acct.uid as u64,
                    acct.gid as u64,
                    0,
                    0,
                );
                let hlen = write_cwd(cwd, acct.home);
                return Session { cwd_len: hlen };
            }
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

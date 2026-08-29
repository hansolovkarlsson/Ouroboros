//! `chmod <mode> <file>` - change a file's permission bits, the write twin of
//! `ls -l`'s mode column. The mode is either **octal** (`755`, `0644`, `600`) or
//! **symbolic** (`u+x`, `go-w`, `a=rx`, `u+rw,go+r`, `g=u`); only the low 12
//! bits are used and only the permission bits change - `fsd` preserves the
//! file's type, so `chmod` can't turn a directory into a file. **ext2 only**:
//! FAT32/exFAT/`/proc` can't model a mode and return "not supported by this
//! filesystem" rather than pretending to succeed.
//!
//! A symbolic mode is *relative*, so it needs the file's current bits: `chmod`
//! stats the target first and applies the expression to what it finds (which is
//! also how the conditional `X` permission knows whether the target is a
//! directory). An octal mode is absolute and needs no stat.
//!
//! All parsing works in bytes, never slicing a `&str` by a runtime index (the
//! PIE relocation trap - see `docs/processes.md`).

#![no_std]
#![no_main]

/// The three permission fields, as a bitmask of "who" a clause names.
const WHO_U: u8 = 4;
const WHO_G: u8 = 2;
const WHO_O: u8 = 1;

/// Shift of a field's rwx triad within the mode word.
fn shift_of(field: u8) -> u16 {
    match field {
        WHO_U => 6,
        WHO_G => 3,
        _ => 0,
    }
}

/// Parse an octal permission string (e.g. `755`, `0644`) into its low-12-bit
/// value, or `None` if it isn't octal or overflows the 12 bits.
fn parse_octal(b: &[u8]) -> Option<u16> {
    if b.is_empty() {
        return None;
    }
    let mut v: u16 = 0;
    for &c in b {
        if !(b'0'..=b'7').contains(&c) {
            return None;
        }
        v = v.checked_mul(8)?.checked_add((c - b'0') as u16)?;
    }
    if v > 0o7777 {
        return None;
    }
    Some(v)
}

/// Apply a POSIX symbolic mode expression (`u+x`, `go-w`, `a=rx`, `g=u`,
/// `u+rw,go+r`) to `cur`, the target's current low-12 permission bits.
/// `is_dir` drives the conditional `X` permission. Returns the new bits, or
/// `None` if the expression is malformed.
///
/// Grammar: comma-separated clauses, each `[ugoa]* (op [rwxXst]* | op [ugo])+`
/// with `op` one of `+ - =`, applied left to right (so a later clause sees an
/// earlier one's result). Naming no "who" means `a` - this OS has no umask, so
/// a bare `+x` really is all three fields, where POSIX would mask it.
fn apply_symbolic(expr: &[u8], cur: u16, is_dir: bool) -> Option<u16> {
    let mut mode = cur & 0o7777;
    if expr.is_empty() {
        return None;
    }
    for clause in expr.split(|&c| c == b',') {
        let mut i = 0usize;

        let mut who = 0u8;
        while i < clause.len() {
            match clause[i] {
                b'u' => who |= WHO_U,
                b'g' => who |= WHO_G,
                b'o' => who |= WHO_O,
                b'a' => who |= WHO_U | WHO_G | WHO_O,
                _ => break,
            }
            i += 1;
        }
        let who = if who == 0 { WHO_U | WHO_G | WHO_O } else { who };

        // One or more `op perms` groups, applied left to right.
        if i >= clause.len() {
            return None; // a who with no operator
        }
        while i < clause.len() {
            let op = clause[i];
            if op != b'+' && op != b'-' && op != b'=' {
                return None;
            }
            i += 1;

            // perms: either a set of rwxXst, or a single copy-source u/g/o
            // (`g=u` - "give group what the owner has"). A copy source can't be
            // mixed with literal perms; a `u` here is unambiguous because the
            // who characters come *before* the operator.
            let mut rwx = 0u16;
            let mut setid = false;
            let mut sticky = false;
            if i < clause.len() && matches!(clause[i], b'u' | b'g' | b'o') {
                let src = match clause[i] {
                    b'u' => WHO_U,
                    b'g' => WHO_G,
                    _ => WHO_O,
                };
                rwx = (mode >> shift_of(src)) & 7;
                i += 1;
            } else {
                while i < clause.len() {
                    match clause[i] {
                        b'r' => rwx |= 4,
                        b'w' => rwx |= 2,
                        b'x' => rwx |= 1,
                        // X: execute only where it makes sense - a directory, or
                        // a file that already carries some execute bit.
                        b'X' => {
                            if is_dir || mode & 0o111 != 0 {
                                rwx |= 1;
                            }
                        }
                        b's' => setid = true,
                        b't' => sticky = true,
                        _ => break,
                    }
                    i += 1;
                }
            }
            // Anything left that isn't the start of the next group is a syntax error.
            if i < clause.len() && !matches!(clause[i], b'+' | b'-' | b'=') {
                return None;
            }

            // Spread the triad across every named field, plus the special bits.
            let mut bits = 0u16;
            for f in [WHO_U, WHO_G, WHO_O] {
                if who & f != 0 {
                    bits |= rwx << shift_of(f);
                }
            }
            if setid {
                if who & WHO_U != 0 {
                    bits |= 0o4000;
                }
                if who & WHO_G != 0 {
                    bits |= 0o2000;
                }
            }
            if sticky {
                bits |= 0o1000;
            }

            match op {
                b'+' => mode |= bits,
                b'-' => mode &= !bits,
                _ => {
                    // `=`: clear everything the named fields own, then set.
                    let mut clear = 0u16;
                    for f in [WHO_U, WHO_G, WHO_O] {
                        if who & f != 0 {
                            clear |= 7 << shift_of(f);
                        }
                    }
                    if who & WHO_U != 0 {
                        clear |= 0o4000;
                    }
                    if who & WHO_G != 0 {
                        clear |= 0o2000;
                    }
                    if who & WHO_O != 0 {
                        clear |= 0o1000;
                    }
                    mode = (mode & !clear) | bits;
                }
            }
        }
    }
    Some(mode & 0o7777)
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: chmod <mode> <file>  (octal 755, or symbolic u+x / go-w / a=rx; ext2 only)\r\n",
    );

    let mut modebuf = [0u8; 32];
    let mode_len = match ulib::arg(1, &mut modebuf) {
        Some(len) if len > 0 => len,
        _ => {
            ulib::con_write(b"chmod: usage: chmod <mode> <file>\r\n");
            ulib::exit(1);
        }
    };
    let mode_arg = &modebuf[..mode_len];

    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(2, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };
    if arg.is_empty() {
        ulib::con_write(b"chmod: missing file argument\r\n");
        ulib::exit(1);
    }

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"chmod: path too long\r\n");
            ulib::exit(1);
        }
    };

    // An octal mode is absolute; a symbolic one is relative, so it needs the
    // file's current bits (and whether it's a directory, for `X`).
    let mode = match parse_octal(mode_arg) {
        Some(m) => m,
        None => {
            let mut info = [0u8; ninep_abi::STAT_INFO_LEN];
            let st = ulib::fs_stat(path, &mut info);
            if ulib::is_fs_error(st) {
                ulib::fs_error("chmod", st);
                ulib::exit(1);
            }
            // No mode on this filesystem means there's nothing to build on -
            // the same refusal the chmod itself would have given.
            let Some((cur, _, _)) = ulib::stat_mode(&info) else {
                ulib::fs_error("chmod", syscall_abi::FS_ERR_NOT_SUPPORTED);
                ulib::exit(1);
            };
            match apply_symbolic(mode_arg, cur, ulib::stat_is_dir(&info)) {
                Some(m) => m,
                None => {
                    ulib::con_write(
                        b"chmod: invalid mode (want octal 755, or symbolic u+x / go-w / a=rx)\r\n",
                    );
                    ulib::exit(1);
                }
            }
        }
    };

    let code = ulib::fs_chmod(path, mode);
    if ulib::is_fs_error(code) {
        ulib::fs_error("chmod", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

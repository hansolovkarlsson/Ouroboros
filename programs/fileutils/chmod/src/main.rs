//! `chmod <octal-mode> <file>` - change a file's permission bits, the write
//! twin of `ls -l`'s mode column. The mode is an octal number (`755`, `0644`,
//! `600`); only the low 12 bits are used and only the permission bits change -
//! `fsd` preserves the file's type, so `chmod` can't turn a directory into a
//! file. **ext2 only**: FAT32/exFAT/`/proc` can't model a mode and return
//! "not supported by this filesystem" rather than pretending to succeed.
//!
//! Numeric modes only (no symbolic `u+x`) - a deliberate first cut. All parsing
//! works in bytes, never slicing a `&str` by a runtime index (the PIE
//! relocation trap - see `docs/processes.md`).

#![no_std]
#![no_main]

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

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: chmod <octal-mode> <file>  (e.g. chmod 755 prog; ext2 only)\r\n");

    let mut modebuf = [0u8; 16];
    let mode = match ulib::arg(1, &mut modebuf) {
        Some(len) => match parse_octal(&modebuf[..len]) {
            Some(m) => m,
            None => {
                ulib::con_write(b"chmod: invalid mode (want an octal number, e.g. 755)\r\n");
                ulib::exit(1);
            }
        },
        None => {
            ulib::con_write(b"chmod: usage: chmod <octal-mode> <file>\r\n");
            ulib::exit(1);
        }
    };

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

    let code = ulib::fs_chmod(path, mode);
    if ulib::is_fs_error(code) {
        ulib::fs_error("chmod", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

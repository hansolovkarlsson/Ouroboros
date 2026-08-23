//! `writeat` - externalized random-access write (standalone-binaries Stage 4).
//! `writeat <file> <offset> <text...>`: writes the text at byte `offset` in an
//! existing file, overwriting in place and zero-filling any gap past EOF. Unlike
//! `write` it does *not* create the file. The words after the offset are joined
//! with single spaces (echo's join). Resolves the path against the delivered
//! cwd; the write goes via the grant/safecopy offset-write path
//! (`ulib::fs_write_at`). Ported from the shell's `cmd_writeat`.

#![no_std]
#![no_main]

use ulib::PATH_MAX;

/// The text is bounded by the shell's argv blob (`ARGV_MAX`, 512).
const CONTENT_MAX: usize = 512;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let mut file_buf = [0u8; PATH_MAX];
    let file_len = match ulib::arg(1, &mut file_buf) {
        Some(len) if len > 0 => len,
        _ => {
            ulib::con_write(b"writeat: usage: writeat <file> <offset> <text...>\r\n");
            ulib::exit(1);
        }
    };
    let file_arg = core::str::from_utf8(&file_buf[..file_len]).unwrap_or("");

    let mut off_buf = [0u8; 24];
    let off_len = match ulib::arg(2, &mut off_buf) {
        Some(len) if len > 0 => len,
        _ => {
            ulib::con_write(b"writeat: usage: writeat <file> <offset> <text...>\r\n");
            ulib::exit(1);
        }
    };
    let off_str = core::str::from_utf8(&off_buf[..off_len]).unwrap_or("");
    let Some(offset) = ulib::parse_u64(off_str) else {
        ulib::con_write(b"writeat: offset must be a number\r\n");
        ulib::exit(1);
    };

    // Join argv[3..] with single spaces into the content buffer.
    let mut content = [0u8; CONTENT_MAX];
    let mut len = 0usize;
    let argc = ulib::argc();
    let mut word_buf = [0u8; CONTENT_MAX];
    let mut first = true;
    let mut i = 3u64;
    while i < argc {
        if let Some(wlen) = ulib::arg(i, &mut word_buf) {
            if !first && len < content.len() {
                content[len] = b' ';
                len += 1;
            }
            for &b in &word_buf[..wlen] {
                if len < content.len() {
                    content[len] = b;
                    len += 1;
                }
            }
            first = false;
        }
        i += 1;
    }
    if len == 0 {
        ulib::con_write(b"writeat: missing text argument\r\n");
        ulib::exit(1);
    }

    let mut cwd_buf = [0u8; PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwd_buf);
    let cwd = core::str::from_utf8(&cwd_buf[..cwd_len]).unwrap_or("/");

    let mut path_buf = [0u8; PATH_MAX];
    let Some(path_len) = ulib::resolve(cwd, file_arg, &mut path_buf) else {
        ulib::con_write(b"writeat: path too long\r\n");
        ulib::exit(1);
    };
    let path = core::str::from_utf8(&path_buf[..path_len]).unwrap_or("");

    let code = ulib::fs_write_at(path, offset, &content[..len]);
    if ulib::is_fs_error(code) {
        ulib::fs_error("writeat", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

//! `man <command>` - show a command's manual page. Pages are plain-text files
//! under `/man/` on disk (e.g. `/man/ls`); this reads `/man/<command>` and
//! prints it, converting `\n` to `\r\n` for the console. Long pages: pipe to
//! the pager, `man <cmd> | more`.
//!
//! No formatting: the framebuffer console only understands a couple of ANSI
//! sequences (clear/home) and silently drops the rest, so bold/colour would
//! render as nothing - the pages are plain text with UPPERCASE section headers.

#![no_std]
#![no_main]

/// A manual page fits comfortably in a few KB; a longer one is truncated.
const BUF: usize = 8192;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: man <command>  (show a command's manual page)\r\n");
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; 64];
    let Some(alen) = ulib::arg(1, &mut argbuf) else {
        ulib::con_write(b"usage: man <command>\r\n");
        ulib::exit(1);
    };
    let name = &argbuf[..alen];

    // Build "/man/<name>".
    let mut path = [0u8; 96];
    let mut p = 0usize;
    for &b in b"/man/" {
        path[p] = b;
        p += 1;
    }
    for &b in name {
        if p < path.len() {
            path[p] = b;
            p += 1;
        }
    }
    let path_str = core::str::from_utf8(&path[..p]).unwrap_or("");

    // Read the page into a buffer.
    let mut buf = [0u8; BUF];
    let mut total = 0usize;
    let chunk = syscall_abi::SAFECOPY_MAX as usize;
    let mut first = true;
    while total < buf.len() {
        let end = (total + chunk).min(buf.len());
        let n = ulib::fs_read_bulk(path_str, total as u64, &mut buf[total..end]);
        if ulib::is_fs_error(n) {
            if first {
                ulib::con_write(b"man: no manual entry for ");
                ulib::con_write(name);
                ulib::con_write(b" (pages live in /man)\r\n");
            }
            ulib::exit(1);
        }
        first = false;
        if n == 0 {
            break;
        }
        total += n as usize;
    }

    // Write it out. To the console, translate '\n' to '\r\n' (the pages use
    // bare newlines); to a pipe/file, pass the bytes through unchanged (the
    // pager adds its own CRLF).
    if target == syscall_abi::CON_TASK {
        let mut start = 0usize;
        for i in 0..total {
            if buf[i] == b'\n' {
                ulib::write_out(target, &buf[start..i]);
                ulib::write_out(target, b"\r\n");
                start = i + 1;
            }
        }
        if start < total {
            ulib::write_out(target, &buf[start..total]);
        }
    } else {
        ulib::write_out(target, &buf[..total]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

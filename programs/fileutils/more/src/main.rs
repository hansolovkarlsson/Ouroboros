//! `more [file]` (also installed as `less`) - page output a screen at a time.
//! With a file argument it pages that file; with none it pages stdin (a pipe),
//! so `<command> | more` works. At each `--More--` pause: **space** = next
//! screen, **Enter** = one more line, **q** = quit; **Ctrl+C** aborts.
//!
//! This used to be a shell builtin - a pager reads the keyboard while it runs,
//! and only the keyboard owner gets keystrokes. Now the shell hands a foreground
//! command (and a pipeline's last stage) the keyboard, so `more` is an ordinary
//! `/bin` program: it reads its content (file or pipe) into its heap, then pages
//! it with `ulib::read_char`. Content is bounded by the heap (a very large file
//! is paged up to that and then truncated).

#![no_std]
#![no_main]

/// The size assumed when the console's is unknown (a serial terminal): the
/// conventional 80 by 24.
const DEFAULT_COLS: usize = 80;
const DEFAULT_ROWS: usize = 24;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: more <file>   or   <command> | more   (page a screen at a time)\r\n");
    let heap = ulib::heap();
    let mut total = 0usize;

    let mut argbuf = [0u8; ulib::PATH_MAX];
    if let Some(alen) = ulib::arg(1, &mut argbuf) {
        // A file argument: resolve against the cwd and read it into the heap.
        let arg = core::str::from_utf8(&argbuf[..alen]).unwrap_or("");
        let mut cwdbuf = [0u8; ulib::PATH_MAX];
        let cwd_len = ulib::cwd(&mut cwdbuf);
        let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");
        let mut pathbuf = [0u8; ulib::PATH_MAX];
        let Some(plen) = ulib::resolve(cwd, arg, &mut pathbuf) else {
            ulib::con_write(b"more: path too long\r\n");
            ulib::exit(1);
        };
        let path = core::str::from_utf8(&pathbuf[..plen]).unwrap_or("");
        let chunk = syscall_abi::SAFECOPY_MAX as usize;
        let mut first = true;
        while total < heap.len() {
            let end = (total + chunk).min(heap.len());
            let n = ulib::fs_read_bulk(path, total as u64, &mut heap[total..end]);
            if ulib::is_fs_error(n) {
                if first {
                    ulib::fs_error("more", n);
                }
                ulib::exit(1);
            }
            first = false;
            if n == 0 {
                break; // EOF
            }
            total += n as usize;
        }
    } else {
        // No argument: page stdin (a pipe). Read one pipe message at a time
        // into a bounded chunk (MSG_RECV rejects a buffer larger than
        // MSG_MAX_LEN) and append to the heap. `pipe_recv` returns 0 at
        // end-of-stream (the pipe's empty terminating message).
        let mut chunk = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        while total < heap.len() {
            let n = ulib::pipe_recv(&mut chunk);
            if n == 0 {
                break;
            }
            let n = n.min(heap.len() - total);
            heap[total..total + n].copy_from_slice(&chunk[..n]);
            total += n;
        }
    }

    page(&heap[..total]);
    ulib::exit(0);
}

/// Page `content` a screen at a time, reading a key at each pause.
fn page(content: &[u8]) {
    if content.is_empty() {
        return;
    }
    let total = content.len();
    let mut pos = 0usize;
    // Screen rows per page before pausing: the console's rows, one kept for
    // the prompt (`ulib::screen_size`; 80 by 24 when the size is unknown).
    // Counted in ROWS, not lines: a line wider than the screen wraps onto
    // more than one, and a page of lines that filled the screen exactly
    // would then scroll its own top away before the prompt.
    let (cols, rows) = ulib::screen_size().unwrap_or((DEFAULT_COLS, DEFAULT_ROWS));
    let page_rows = rows.max(2) - 1;
    let mut to_show = page_rows;
    loop {
        let mut used = 0usize;
        while pos < total {
            let start = pos;
            let mut end = start;
            while end < total && content[end] != b'\n' {
                end += 1;
            }
            let next = if end < total { end + 1 } else { end };
            // Drop a trailing '\r' (DOS endings); we add our own CRLF.
            if end > start && content[end - 1] == b'\r' {
                end -= 1;
            }
            let need = screen_rows(&content[start..end], cols);
            // A line that does not fit waits for the next page, unless it
            // is the first: one taller than the whole page is shown anyway.
            if used > 0 && used + need > to_show {
                break;
            }
            ulib::con_write(&content[start..end]);
            ulib::con_write(b"\r\n");
            pos = next;
            used += need;
            if used >= to_show {
                break;
            }
        }
        if pos >= total {
            break; // everything shown
        }
        ulib::con_write(b"--More--");
        // One key, not one byte: an arrow's three bytes are one press, so
        // it moves one screen, and none of it is left for the shell.
        let key = ulib::read_key();
        ulib::con_write(b"\r        \r"); // erase the prompt (CR, spaces, CR)
        match key {
            ulib::keyseq::Fed::Byte(b'q' | b'Q') => break,
            ulib::keyseq::Fed::Byte(b'\r' | b'\n') => to_show = 1, // one more line (however many rows it takes)
            _ => to_show = page_rows, // space (or any key) = next screen
        }
    }
}

/// The screen rows `line` takes on a console `cols` wide: its printable
/// bytes (the console server draws nothing for the rest), wrapped, at least
/// one. A line of exactly `cols` is one row: the console wraps late, at the
/// next glyph, and the CRLF after it comes first.
fn screen_rows(line: &[u8], cols: usize) -> usize {
    let width = line.iter().filter(|&&b| (0x20..0x7f).contains(&b)).count();
    width.div_ceil(cols.max(1)).max(1)
}


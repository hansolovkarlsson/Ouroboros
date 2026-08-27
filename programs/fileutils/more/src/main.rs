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

/// Lines per screen before pausing (~24-row console, one kept for the prompt).
const PAGE_ROWS: usize = 23;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
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
    let mut to_show = PAGE_ROWS;
    loop {
        let mut printed = 0usize;
        while printed < to_show && pos < total {
            let start = pos;
            while pos < total && content[pos] != b'\n' {
                pos += 1;
            }
            // Drop a trailing '\r' (DOS endings); we add our own CRLF.
            let mut end = pos;
            if end > start && content[end - 1] == b'\r' {
                end -= 1;
            }
            ulib::con_write(&content[start..end]);
            ulib::con_write(b"\r\n");
            if pos < total {
                pos += 1; // step past the '\n'
            }
            printed += 1;
        }
        if pos >= total {
            break; // everything shown
        }
        ulib::con_write(b"--More--");
        let key = ulib::read_char();
        ulib::con_write(b"\r        \r"); // erase the prompt (CR, spaces, CR)
        match key {
            b'q' | b'Q' => break,
            b'\r' | b'\n' => to_show = 1, // one more line
            _ => to_show = PAGE_ROWS,      // space (or anything) = next screen
        }
    }
}

//! `ls` - directory listing. Two layouts, both **sorted by name**
//! (case-insensitive):
//!
//! - **default**: names in columns (like a terminal `ls`), directories marked
//!   with a trailing `/`.
//! - **`-l`**: one entry per line with its type, size, and modified date/time
//!   (from the `stat` op - `fs_stat`), the long form.
//!
//! Resolves its path against the shell-delivered cwd (`ulib::cwd`), the same as
//! before; talks to the filesystem server via `ulib`'s `fs_list_dir`/`fs_stat`.
//! No heap: entries are collected as offset+length records into the listing
//! buffer and insertion-sorted in place (the `tree` shape).

#![no_std]
#![no_main]

/// Assumed console width for the column layout (a terminal `ls` reads the real
/// width; this console doesn't expose one to a spawned program, so 80 is the
/// conventional fallback).
const TERM_WIDTH: usize = 80;

/// Most entries laid out per directory. The listing buffer bounds the byte
/// count; this bounds the record array (a larger directory drops the overflow).
const MAX_ENTRIES: usize = 128;

/// One directory entry: offset+length into the listing buffer, plus its kind.
#[derive(Clone, Copy)]
struct Entry {
    start: u16,
    len: u16,
    is_dir: bool,
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: ls [-l] [-a] [path]  (-l long form, -a show dotfiles)\r\n");
    let target = ulib::stdout_target();

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    // Parse args: `-l` selects the long form, `-a` shows dotfiles (including
    // `.`/`..`); both accepted in any position and combinable as `-la`/`-al`.
    // The first non-flag argument is the path (empty = the current directory).
    let mut long = false;
    let mut all = false;
    let mut argbuf = [0u8; ulib::PATH_MAX];
    let mut arg = "";
    let mut i = 1u64;
    loop {
        let mut buf = [0u8; ulib::PATH_MAX];
        let Some(len) = ulib::arg(i, &mut buf) else { break };
        if len >= 2 && buf[0] == b'-' {
            for &c in &buf[1..len] {
                match c {
                    b'l' => long = true,
                    b'a' => all = true,
                    _ => {}
                }
            }
        } else if arg.is_empty() {
            argbuf[..len].copy_from_slice(&buf[..len]);
            arg = core::str::from_utf8(&argbuf[..len]).unwrap_or("");
        }
        i += 1;
    }

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or("/"),
        None => {
            ulib::con_write(b"ls: path too long\r\n");
            ulib::exit(1);
        }
    };

    let mut listing = [0u8; 512];
    let n = ulib::fs_list_dir(path, &mut listing);
    if ulib::is_fs_error(n) {
        ulib::fs_error("ls", n);
        ulib::exit(1);
    }
    let data = &listing[..n as usize];

    // Collect entries and sort by name (case-insensitive).
    let mut entries = [Entry { start: 0, len: 0, is_dir: false }; MAX_ENTRIES];
    let mut count = 0usize;
    let mut pos = 0usize;
    while pos < data.len() && count < MAX_ENTRIES {
        let start = pos;
        while pos < data.len() && data[pos] != b'\n' {
            pos += 1;
        }
        let line = &data[start..pos];
        if pos < data.len() {
            pos += 1;
        }
        let (name_len, is_dir) = if line.last() == Some(&b'/') {
            (line.len() - 1, true)
        } else {
            (line.len(), false)
        };
        if name_len == 0 {
            continue;
        }
        // Hide dotfiles (and `.`/`..`) unless `-a`, the Unix `ls` default.
        if !all && data[start] == b'.' {
            continue;
        }
        entries[count] = Entry {
            start: start as u16,
            len: name_len as u16,
            is_dir,
        };
        count += 1;
    }
    sort_entries(&mut entries[..count], data);
    let entries = &entries[..count];

    if long {
        print_long(target, path, entries, data);
    } else {
        print_columns(target, entries, data);
    }

    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Default layout: names in columns, filled top-to-bottom then left-to-right
/// (column-major, like a terminal `ls`), directories suffixed `/`.
fn print_columns(target: u64, entries: &[Entry], data: &[u8]) {
    let n = entries.len();
    if n == 0 {
        return;
    }
    // Widest display name (+1 for a directory's trailing '/').
    let mut maxw = 1usize;
    for e in entries {
        let w = e.len as usize + if e.is_dir { 1 } else { 0 };
        if w > maxw {
            maxw = w;
        }
    }
    let col_width = maxw + 2; // two-space gap between columns
    let cols = (TERM_WIDTH / col_width).max(1);
    let rows = n.div_ceil(cols);

    let mut line = [0u8; TERM_WIDTH + 4];
    for r in 0..rows {
        let mut w = 0usize;
        // How many columns actually have an entry in this row.
        let mut last_c = 0usize;
        for c in 0..cols {
            if c * rows + r < n {
                last_c = c;
            }
        }
        for c in 0..cols {
            let idx = c * rows + r;
            if idx >= n {
                break;
            }
            let e = &entries[idx];
            let name = &data[e.start as usize..e.start as usize + e.len as usize];
            append(&mut line, &mut w, name);
            if e.is_dir {
                append(&mut line, &mut w, b"/");
            }
            // Pad to the column width, except after the row's last entry.
            if c < last_c {
                let cell = e.len as usize + if e.is_dir { 1 } else { 0 };
                for _ in cell..col_width {
                    append(&mut line, &mut w, b" ");
                }
            }
        }
        append(&mut line, &mut w, b"\r\n");
        ulib::write_out(target, &line[..w]);
    }
}

/// Long layout (`-l`): one entry per line - `<type> <size> <date time> <name>`,
/// the size and modified time from a `stat` of each entry.
fn print_long(target: u64, dir: &str, entries: &[Entry], data: &[u8]) {
    for e in entries {
        let name = &data[e.start as usize..e.start as usize + e.len as usize];

        // Stat the entry (dir/name) for size + modified time.
        let mut childbuf = [0u8; ulib::PATH_MAX];
        let mut info = [0u8; ninep_abi::STAT_INFO_LEN];
        let mut have = false;
        if let Ok(nm) = core::str::from_utf8(name) {
            if let Some(clen) = ulib::resolve(dir, nm, &mut childbuf) {
                if let Ok(child) = core::str::from_utf8(&childbuf[..clen]) {
                    have = !ulib::is_fs_error(ulib::fs_stat(child, &mut info));
                }
            }
        }

        let mut line = [0u8; ulib::PATH_MAX + 48];
        let mut w = 0usize;

        // Type: 'd' for a directory, '-' for a file.
        append(&mut line, &mut w, if e.is_dir { b"d " } else { b"- " });

        // Size, right-aligned in 9 columns.
        let size = if have { ulib::stat_size(&info) } else { 0 };
        emit_right(&mut line, &mut w, size, 9);
        append(&mut line, &mut w, b" ");

        // Modified date/time: "YYYY-MM-DD HH:MM", or a dash-filled placeholder
        // when the filesystem doesn't surface one.
        match if have { ulib::stat_time(&info) } else { None } {
            Some((year, mon, day, hour, min, _sec)) => {
                emit_pad(&mut line, &mut w, year as u64, 4);
                append(&mut line, &mut w, b"-");
                emit_pad(&mut line, &mut w, mon as u64, 2);
                append(&mut line, &mut w, b"-");
                emit_pad(&mut line, &mut w, day as u64, 2);
                append(&mut line, &mut w, b" ");
                emit_pad(&mut line, &mut w, hour as u64, 2);
                append(&mut line, &mut w, b":");
                emit_pad(&mut line, &mut w, min as u64, 2);
            }
            None => append(&mut line, &mut w, b"       -        "), // 16 wide
        }
        append(&mut line, &mut w, b" ");

        // Name (directories keep a trailing '/').
        append(&mut line, &mut w, name);
        if e.is_dir {
            append(&mut line, &mut w, b"/");
        }
        append(&mut line, &mut w, b"\r\n");
        ulib::write_out(target, &line[..w]);
    }
}

/// Insertion-sort entries by name (case-insensitive, raw-byte tiebreak).
fn sort_entries(entries: &mut [Entry], data: &[u8]) {
    for i in 1..entries.len() {
        let mut j = i;
        while j > 0 && name_less(entry_name(entries[j], data), entry_name(entries[j - 1], data)) {
            entries.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn entry_name(e: Entry, data: &[u8]) -> &[u8] {
    &data[e.start as usize..e.start as usize + e.len as usize]
}

fn name_less(a: &[u8], b: &[u8]) -> bool {
    let mut i = 0;
    while i < a.len() && i < b.len() {
        let ca = a[i].to_ascii_lowercase();
        let cb = b[i].to_ascii_lowercase();
        if ca != cb {
            return ca < cb;
        }
        i += 1;
    }
    if a.len() != b.len() {
        return a.len() < b.len();
    }
    a < b
}

/// Append `src` to `buf` at `*n`, truncating at capacity.
fn append(buf: &mut [u8], n: &mut usize, src: &[u8]) {
    for &b in src {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

/// Emit `v` as decimal right-aligned in `width` columns (space-padded); a value
/// wider than `width` is printed in full (never truncated).
fn emit_right(buf: &mut [u8], n: &mut usize, v: u64, width: usize) {
    let digits = dec_width(v);
    for _ in digits..width {
        append(buf, n, b" ");
    }
    ulib::emit_dec(buf, n, v);
}

/// Emit `v` as decimal zero-padded to exactly `width` digits (for date fields).
fn emit_pad(buf: &mut [u8], n: &mut usize, v: u64, width: usize) {
    let digits = dec_width(v);
    for _ in digits..width {
        append(buf, n, b"0");
    }
    ulib::emit_dec(buf, n, v);
}

/// Number of decimal digits in `v` (1 for 0).
fn dec_width(v: u64) -> usize {
    let mut d = 1;
    let mut x = v;
    while x >= 10 {
        x /= 10;
        d += 1;
    }
    d
}

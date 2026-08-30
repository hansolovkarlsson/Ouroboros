//! `ls` - directory listing. Two layouts, both **sorted by name**
//! (case-insensitive):
//!
//! - **default**: names in columns (like a terminal `ls`), directories marked
//!   with a trailing `/`.
//! - **`-l`**: one entry per line - permission string (`drwxr-xr-x`), owner
//!   uid/gid, size, and modified date/time (from the `stat` op - `fs_stat`),
//!   the long form. The mode/owner columns are real on ext2 (the one arm that
//!   stores them); on FAT32/exFAT/`/proc` a conventional mode is synthesized
//!   and the owner shown as `-` (they can't model an owner/permissions).
//!
//! Takes any number of operands (so shell globs like `ls *.txt` work): each is
//! `stat`ed and classified. **File** operands are listed together as a group (a
//! plain `ls file` shows just that file, not "not a directory"); **directory**
//! operands have their contents listed, with a `name:` header when there's more
//! than one operand. No operands lists the cwd. Resolves paths against the
//! shell-delivered cwd (`ulib::cwd`); talks to fsd via `fs_list_dir`/`fs_stat`.
//! No heap: entries are offset+length records insertion-sorted in place.

#![no_std]
#![no_main]

/// Assumed console width for the column layout (a terminal `ls` reads the real
/// width; this console doesn't expose one to a spawned program, so 80 is the
/// conventional fallback).
const TERM_WIDTH: usize = 80;

/// Most entries laid out per directory. The listing buffer bounds the byte
/// count; this bounds the record array (a larger directory drops the overflow).
const MAX_ENTRIES: usize = 128;

/// Most path operands accepted on one command line (a big glob is truncated).
const MAX_PATHS: usize = 64;

/// One directory entry: offset+length into the listing buffer, plus its kind.
#[derive(Clone, Copy)]
struct Entry {
    start: u16,
    len: u16,
    is_dir: bool,
    /// A `.`/`..` entry this program invented rather than one the server
    /// listed - `start` means nothing for these, and `len` (1 or 2) selects
    /// the name out of [`DOTS`]. See `show_listing` for why they are invented
    /// here instead of being returned by `fsd`.
    synth: bool,
}

/// The two synthetic names, overlapping: `.` is `DOTS[..1]`, `..` is
/// `DOTS[..2]`.
const DOTS: &[u8] = b"..";

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
    // Everything else is a path operand, collected (as typed) into `argsbuf`.
    let mut long = false;
    let mut all = false;
    let mut argsbuf = [0u8; 512];
    let mut argslen = 0usize;
    let mut offs = [(0u16, 0u16); MAX_PATHS];
    let mut nargs = 0usize;
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
        } else if nargs < MAX_PATHS && argslen + len <= argsbuf.len() {
            argsbuf[argslen..argslen + len].copy_from_slice(&buf[..len]);
            offs[nargs] = (argslen as u16, len as u16);
            argslen += len;
            nargs += 1;
        }
        i += 1;
    }
    let multi = nargs > 1;

    // Partition operands into file operands (a synthetic listing, shown as a
    // group) and directory operands (contents listed). No operands = the cwd.
    let mut filedata = [0u8; 512];
    let mut filelen = 0usize;
    let mut diroffs = [(0u16, 0u16); MAX_PATHS];
    let mut ndirs = 0usize;

    if nargs == 0 {
        diroffs[0] = (0, 0); // len 0 = the cwd
        ndirs = 1;
    } else {
        for &(s, l) in &offs[..nargs] {
            let arg = &argsbuf[s as usize..s as usize + l as usize];
            let argstr = core::str::from_utf8(arg).unwrap_or("");
            let mut pb = [0u8; ulib::PATH_MAX];
            let Some(pl) = ulib::resolve(cwd, argstr, &mut pb) else {
                ls_err(argstr);
                continue;
            };
            let resolved = core::str::from_utf8(&pb[..pl]).unwrap_or("");
            let mut info = [0u8; ninep_abi::STAT_INFO_LEN];
            if ulib::is_fs_error(ulib::fs_stat(resolved, &mut info)) {
                ls_err(argstr);
                continue;
            }
            if ulib::stat_is_dir(&info) {
                diroffs[ndirs] = (s, l);
                ndirs += 1;
            } else if filelen + (l as usize) < filedata.len() {
                // A file operand: shown as-typed in the file group.
                filedata[filelen..filelen + l as usize].copy_from_slice(arg);
                filelen += l as usize;
                filedata[filelen] = b'\n';
                filelen += 1;
            }
        }
    }

    // File operands are always shown (they were named), so `all` = true here.
    if filelen > 0 {
        show_listing(target, cwd, &filedata[..filelen], true, false, long);
    }

    // Each directory operand's contents, headed by its name when there's more
    // than one operand.
    for (d, &(s, l)) in diroffs[..ndirs].iter().enumerate() {
        let argstr = core::str::from_utf8(&argsbuf[s as usize..s as usize + l as usize]).unwrap_or("");
        let mut pb = [0u8; ulib::PATH_MAX];
        let Some(pl) = ulib::resolve(cwd, argstr, &mut pb) else { continue };
        let dirpath = core::str::from_utf8(&pb[..pl]).unwrap_or("/");
        if multi {
            // A blank line before each dir group, except before the very first
            // output.
            if filelen > 0 || d > 0 {
                ulib::write_out(target, b"\r\n");
            }
            ulib::write_out(target, argstr.as_bytes());
            ulib::write_out(target, b":\r\n");
        }
        let mut listing = [0u8; 512];
        let n = ulib::fs_list_dir(dirpath, &mut listing);
        if ulib::is_fs_error(n) {
            ls_err(argstr);
            continue;
        }
        show_listing(target, dirpath, &listing[..n as usize], all, all, long);
    }

    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Print `ls: <arg>: no such file or directory` for a bad operand.
fn ls_err(arg: &str) {
    ulib::con_write(b"ls: ");
    ulib::con_write(arg.as_bytes());
    ulib::con_write(b": no such file or directory\r\n");
}

/// Collect a `name\n`/`name/\n` listing into entries, sort by name, and display
/// it - columns (default) or the long form (`-l`). `dir` is the path the entries
/// are relative to (for `-l`'s per-entry `stat`); `all` keeps dotfiles, and
/// `dots` additionally shows `.` and `..`.
///
/// **`.` and `..` are invented here, not listed by the server.** Every
/// filesystem arm in `fsd` filters them out of a directory listing, and that
/// filter is load-bearing for every other client: `tree` would recurse
/// forever, glob expansion would match them, and a future recursive `cp`/`rm`
/// would walk in circles. Only `ls -a` wants them, so only `ls -a` makes them.
///
/// `-l` then stats them like any other name, and `ulib::resolve` normalizes
/// `<dir>/.` and `<dir>/..` before the call - so the modes shown are the real
/// ones for this directory and its parent, which is the whole point of asking.
fn show_listing(target: u64, dir: &str, data: &[u8], all: bool, dots: bool, long: bool) {
    let mut entries = [Entry { start: 0, len: 0, is_dir: false, synth: false }; MAX_ENTRIES];
    let mut count = 0usize;
    if dots {
        for len in [1u16, 2] {
            entries[count] = Entry { start: 0, len, is_dir: true, synth: true };
            count += 1;
        }
    }
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
        if !all && data[start] == b'.' {
            continue;
        }
        entries[count] = Entry {
            start: start as u16,
            len: name_len as u16,
            is_dir,
            synth: false,
        };
        count += 1;
    }
    sort_entries(&mut entries[..count], data);
    if long {
        print_long(target, dir, &entries[..count], data);
    } else {
        print_columns(target, &entries[..count], data);
    }
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
            let name = entry_name(*e, data);
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
        let name = entry_name(*e, data);

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

        let mut line = [0u8; ulib::PATH_MAX + 72];
        let mut w = 0usize;

        // Type + permission bits (drwxr-xr-x): the real mode when the filesystem
        // models it (ext2), else a conventional default synthesized from type.
        let md = if have { ulib::stat_mode(&info) } else { None };
        let mut perm = [0u8; 10];
        perm_string(&mut perm, md, e.is_dir);
        append(&mut line, &mut w, &perm);
        append(&mut line, &mut w, b" ");

        // Owner uid/gid (numeric), or dashes when the filesystem can't model one.
        match md {
            Some((_, uid, gid)) => {
                emit_right(&mut line, &mut w, uid as u64, 4);
                append(&mut line, &mut w, b" ");
                emit_right(&mut line, &mut w, gid as u64, 4);
            }
            None => append(&mut line, &mut w, b"   -    -"),
        }
        append(&mut line, &mut w, b" ");

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
    if e.synth {
        return &DOTS[..e.len as usize];
    }
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

/// Render the 10-char type+permission string (`drwxr-xr-x`) into `out`. `md` is
/// the real `(mode, uid, gid)` when the filesystem models it; otherwise a
/// conventional default is synthesized from `is_dir` (the way Linux presents a
/// mode-less mount like vfat). `mode` is ext2's `i_mode`: the `S_IFMT` type
/// nibble plus the 12 permission bits (`rwx` + setuid/setgid/sticky).
fn perm_string(out: &mut [u8; 10], md: Option<(u16, u16, u16)>, is_dir: bool) {
    let mode = match md {
        Some((m, _, _)) => m,
        None if is_dir => 0x4000 | 0o755, // S_IFDIR | rwxr-xr-x
        None => 0x8000 | 0o644,           // S_IFREG | rw-r--r--
    };
    out[0] = match mode & 0xF000 {
        0x4000 => b'd',
        0xA000 => b'l',
        0x2000 => b'c',
        0x6000 => b'b',
        0x1000 => b'p',
        0xC000 => b's',
        _ => b'-',
    };
    let rwx = *b"rwx";
    for i in 0..9 {
        out[1 + i] = if mode & (1 << (8 - i)) != 0 { rwx[i % 3] } else { b'-' };
    }
    // setuid/setgid/sticky overlay the three exec positions (s/S, s/S, t/T).
    if mode & 0o4000 != 0 {
        out[3] = if out[3] == b'x' { b's' } else { b'S' };
    }
    if mode & 0o2000 != 0 {
        out[6] = if out[6] == b'x' { b's' } else { b'S' };
    }
    if mode & 0o1000 != 0 {
        out[9] = if out[9] == b'x' { b't' } else { b'T' };
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

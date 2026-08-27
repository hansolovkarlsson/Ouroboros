//! `tree` - recursive directory listing as an indented tree (the classic
//! Unix `tree`). Lists `argv[1]` (or, with no argument, the current
//! directory), descending into each subdirectory and drawing the branch
//! structure, then prints a `N directories, M files` summary.
//!
//! Constraints that shape it: spawned `/bin` programs get a ~32 KB stack, so
//! recursion is depth-capped ([`MAX_DEPTH`]) and each frame's buffers are
//! kept small; there's no heap; and the framebuffer console renders ASCII
//! 0x20-0x7E only, so the branches are the ASCII forms (`|-- `/`` `-- ``),
//! not the Unicode box-drawing glyphs. Talks to the filesystem server via
//! `ulib`'s `fs_list_dir`, resolving paths against the shell-delivered cwd -
//! the same shape as `ls`.

#![no_std]
#![no_main]

/// Deepest level `tree` descends. The 32 KB spawn stack bounds this: each
/// recursive frame holds a listing buffer plus the child's path and prefix
/// (which the recursion borrows, so they stay live), ~900 bytes; 16 levels
/// leaves comfortable headroom. A deeper tree is simply not descended.
const MAX_DEPTH: usize = 16;

/// One directory's listing, as `fs_list_dir` fills it (`name\n`/`name/\n`).
const LIST_BUF: usize = 512;

/// Room for the accumulated indent prefix: 4 bytes per ancestor level.
const PREFIX_MAX: usize = MAX_DEPTH * 4 + 8;

struct Counts {
    dirs: u64,
    files: u64,
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    // argv[1] is the root path; absent means "the current directory".
    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(1, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let root = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or("/"),
        None => {
            ulib::con_write(b"tree: path too long\r\n");
            ulib::exit(1);
        }
    };

    // Header line: the root as given (like real `tree`, which echoes its
    // argument). An empty arg shows the resolved cwd.
    let header: &str = if arg.is_empty() { root } else { arg };
    ulib::write_out(target, header.as_bytes());
    ulib::write_out(target, b"\r\n");

    let mut counts = Counts { dirs: 0, files: 0 };
    walk(root, "", target, 0, &mut counts);

    // Summary: a blank line, then the counts (real `tree`'s trailer).
    ulib::write_out(target, b"\r\n");
    let mut line = [0u8; 64];
    let mut n = 0usize;
    ulib::emit_dec(&mut line, &mut n, counts.dirs);
    append(&mut line, &mut n, if counts.dirs == 1 { b" directory, " } else { b" directories, " });
    ulib::emit_dec(&mut line, &mut n, counts.files);
    append(&mut line, &mut n, if counts.files == 1 { b" file" } else { b" files" });
    append(&mut line, &mut n, b"\r\n");
    ulib::write_out(target, &line[..n]);

    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// List `path` and print its entries under `prefix`, recursing into
/// subdirectories. `depth` is the current level (0 = directly under the
/// root); `counts` accumulates the directory/file totals for the summary.
fn walk(path: &str, prefix: &str, target: u64, depth: usize, counts: &mut Counts) {
    let mut listing = [0u8; LIST_BUF];
    let n = ulib::fs_list_dir(path, &mut listing);
    if ulib::is_fs_error(n) {
        // Only worth reporting for the root (a deeper unreadable dir just
        // isn't descended - it was already printed as an entry).
        if depth == 0 {
            ulib::fs_error("tree", n);
        }
        return;
    }
    let data = &listing[..n as usize];

    // First pass: how many real entries (so the last one gets `` `-- ``).
    let total = count_entries(data);

    // Second pass: emit each, recursing into directories.
    let mut seen = 0usize;
    for line in Lines::new(data) {
        let (name, is_dir) = parse_entry(line);
        if name.is_empty() || is_dot(name) {
            continue;
        }
        seen += 1;
        let is_last = seen == total;

        emit_entry(target, prefix, is_last, name);

        if is_dir {
            counts.dirs += 1;
            if depth + 1 < MAX_DEPTH {
                // Child prefix = this prefix + (a pipe if more siblings
                // follow, else blank), and child path = path/name. Both live
                // on this frame while the recursion borrows them.
                let mut cpb = [0u8; PREFIX_MAX];
                let child_prefix = build_child_prefix(prefix, is_last, &mut cpb);

                let mut childbuf = [0u8; ulib::PATH_MAX];
                if let Some(clen) = resolve_child(path, name, &mut childbuf) {
                    let child = core::str::from_utf8(&childbuf[..clen]).unwrap_or("");
                    walk(child, child_prefix, target, depth + 1, counts);
                }
            }
        } else {
            counts.files += 1;
        }
    }
}

/// Emit one entry line: `<prefix><branch><name>` where the branch is
/// `` `-- `` for the last child and `|-- ` otherwise. Assembled into one
/// buffer and written once (fewer IPC round trips than a segment each).
fn emit_entry(target: u64, prefix: &str, is_last: bool, name: &[u8]) {
    let mut line = [0u8; PREFIX_MAX + 4 + ulib::PATH_MAX + 2];
    let mut n = 0usize;
    append(&mut line, &mut n, prefix.as_bytes());
    append(&mut line, &mut n, if is_last { b"`-- " } else { b"|-- " });
    append(&mut line, &mut n, name);
    append(&mut line, &mut n, b"\r\n");
    ulib::write_out(target, &line[..n]);
}

/// Build the indent prefix for the children of an entry: the parent prefix,
/// then `` `` (blank, if the entry was the last sibling) or `|   ` (a
/// continuing vertical, if more siblings follow).
fn build_child_prefix<'a>(prefix: &str, parent_is_last: bool, buf: &'a mut [u8; PREFIX_MAX]) -> &'a str {
    let mut n = 0usize;
    append(buf, &mut n, prefix.as_bytes());
    append(buf, &mut n, if parent_is_last { b"    " } else { b"|   " });
    core::str::from_utf8(&buf[..n]).unwrap_or("")
}

/// `path/name` into `out` (via `ulib::resolve`, so it's normalized). `name`
/// came from the filesystem, so it's assumed valid UTF-8.
fn resolve_child(path: &str, name: &[u8], out: &mut [u8]) -> Option<usize> {
    let name_str = core::str::from_utf8(name).ok()?;
    ulib::resolve(path, name_str, out)
}

/// Split a directory entry line into `(name, is_dir)` - a trailing `/`
/// (how `fs_list_dir` marks directories) is stripped and sets `is_dir`.
fn parse_entry(line: &[u8]) -> (&[u8], bool) {
    if let Some((&b'/', rest)) = line.split_last() {
        (rest, true)
    } else {
        (line, false)
    }
}

/// The `.`/`..` self/parent entries FAT returns - never shown, never
/// descended (descending `..` would loop forever).
fn is_dot(name: &[u8]) -> bool {
    name == b"." || name == b".."
}

/// Count the real (non-empty, non-dot) entries in a listing.
fn count_entries(data: &[u8]) -> usize {
    let mut c = 0;
    for line in Lines::new(data) {
        let (name, _) = parse_entry(line);
        if !name.is_empty() && !is_dot(name) {
            c += 1;
        }
    }
    c
}

/// Append `src` to `buf` at `*n`, advancing `*n`, truncating at capacity.
fn append(buf: &mut [u8], n: &mut usize, src: &[u8]) {
    for &b in src {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

/// Iterator over the `\n`-separated lines of a listing (a trailing line
/// without a newline is yielded too; empty lines are yielded as empty and
/// filtered by the caller).
struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lines<'a> {
    fn new(data: &'a [u8]) -> Self {
        Lines { data, pos: 0 }
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let start = self.pos;
        let mut end = start;
        while end < self.data.len() && self.data[end] != b'\n' {
            end += 1;
        }
        // Advance past the newline (or to the end).
        self.pos = if end < self.data.len() { end + 1 } else { end };
        Some(&self.data[start..end])
    }
}

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

/// Most entries `tree` sorts per directory. The listing itself is capped at
/// [`LIST_BUF`], so this is really a stack bound (the sort array is live across
/// the recursion into each child); a directory with more entries has the
/// overflow dropped, like the listing truncation.
const MAX_ENTRIES: usize = 64;

/// One directory entry as an offset+length into the current listing buffer
/// (plus its kind), so sorting reorders these lightweight records rather than
/// moving the name bytes around.
#[derive(Clone, Copy)]
struct Entry {
    start: u16,
    len: u16,
    is_dir: bool,
}

struct Counts {
    dirs: u64,
    files: u64,
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: tree [path]  (recursive directory listing, sorted)\r\n");
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

    // Collect the real entries (offset+len into `data`), skipping empty lines
    // and the `.`/`..` FAT returns, then sort them alphabetically so the tree
    // is in a stable, readable order rather than raw on-disk order.
    let mut entries = [Entry { start: 0, len: 0, is_dir: false }; MAX_ENTRIES];
    let mut count = 0usize;
    let mut i = 0usize;
    while i < data.len() && count < MAX_ENTRIES {
        let start = i;
        while i < data.len() && data[i] != b'\n' {
            i += 1;
        }
        let line = &data[start..i];
        if i < data.len() {
            i += 1; // step past the newline
        }
        // A trailing '/' marks a directory (and isn't part of the name).
        let (name_len, is_dir) = if line.last() == Some(&b'/') {
            (line.len() - 1, true)
        } else {
            (line.len(), false)
        };
        let name = &data[start..start + name_len];
        if name_len == 0 || is_dot(name) {
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

    // Emit each entry, recursing into directories - in sorted order.
    for (idx, e) in entries[..count].iter().enumerate() {
        let name = &data[e.start as usize..e.start as usize + e.len as usize];
        let is_last = idx + 1 == count;

        emit_entry(target, prefix, is_last, name);

        if e.is_dir {
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

/// Insertion-sort entries by name (case-insensitive, with a raw-byte tiebreak
/// for stability) - small `n` (a single directory's entries), no heap, and the
/// near-sorted-input case is cheap.
fn sort_entries(entries: &mut [Entry], data: &[u8]) {
    for i in 1..entries.len() {
        let mut j = i;
        while j > 0 && entry_less(entries[j], entries[j - 1], data) {
            entries.swap(j, j - 1);
            j -= 1;
        }
    }
}

fn entry_less(a: Entry, b: Entry, data: &[u8]) -> bool {
    let an = &data[a.start as usize..a.start as usize + a.len as usize];
    let bn = &data[b.start as usize..b.start as usize + b.len as usize];
    name_less(an, bn)
}

/// `a` sorts before `b`: ASCII-case-insensitive lexicographic, then a raw-byte
/// tiebreak so entries differing only in case have a deterministic order.
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

/// The `.`/`..` self/parent entries FAT returns - never shown, never
/// descended (descending `..` would loop forever).
fn is_dot(name: &[u8]) -> bool {
    name == b"." || name == b".."
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

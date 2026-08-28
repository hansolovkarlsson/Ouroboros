//! `sort [-r] [-n] [-u] [-f]` - the one line filter that *can't* stream: it
//! must read all of stdin before it can emit a single line. Unlike the other
//! filters (which keep only a fixed line buffer or a small ring), `sort`
//! buffers the whole input, so it uses this program's 256KB heap
//! (`ulib::heap`) rather than the stack - the input bytes in the front, a
//! line index (start+len per line) reinterpreted from the heap's tail.
//!
//! **Bounded, with a documented cap** (the roadmap's requirement): input is
//! held in `DATA_CAP` bytes and up to `MAX_LINES` lines; a larger input is
//! **truncated** at the cap, sorted, and emitted, with a one-line warning to
//! the console (not into the sorted output). This is the "documented size cap"
//! rather than an unbounded (impossible here) sort.
//!
//! Flags (combinable, e.g. `-rn`): `-r` reverse, `-n` numeric (compare a
//! leading integer, ties broken lexicographically), `-u` unique (drop lines
//! that compare equal to the previous emitted one, like `sort -u`), `-f` fold
//! case (ASCII). Default is a plain byte-lexicographic ascending sort. The sort
//! is an in-place heapsort over the line index (O(n log n), no recursion, no
//! scratch array).

#![no_std]
#![no_main]

use core::cmp::Ordering;

/// Most lines we index (the heap tail holds this many start+len `u32` pairs).
/// 8192 * 8 bytes = 64KB of the 256KB heap reserved for the index.
const MAX_LINES: usize = 8192;
/// Bytes of the heap reserved for the line index (`MAX_LINES` * two `u32`).
const INDEX_BYTES: usize = MAX_LINES * 8;

struct Flags {
    reverse: bool,
    numeric: bool,
    unique: bool,
    fold: bool,
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: <input> | sort [-r] [-n] [-u] [-f]  (sort lines; -r reverse, -n numeric, -u unique, -f fold case)\r\n",
    );
    let target = ulib::stdout_target();
    let flags = parse_flags(target);

    let heap = ulib::heap();
    if heap.len() <= INDEX_BYTES {
        ulib::con_write(b"sort: no heap available\r\n");
        ulib::end_of_stream(target);
        ulib::exit(1);
    }
    // Split the heap: data in the front, the line index in the (4-aligned)
    // tail. `data_cap` is a multiple of 4 so the index view aligns cleanly.
    let data_cap = (heap.len() - INDEX_BYTES) & !3;
    let (data, idx_bytes) = heap.split_at_mut(data_cap);
    // SAFETY: the heap base is page-aligned and `data_cap` is 4-aligned, so the
    // tail starts u32-aligned - `align_to_mut` yields no prefix, the whole tail
    // as `&mut [u32]`. Each line uses two entries: [2k]=start, [2k+1]=len.
    let (_, index, _) = unsafe { idx_bytes.align_to_mut::<u32>() };

    // Read all of stdin into `data` (front of the heap), truncating at the cap.
    let mut data_len = 0usize;
    let mut truncated = false;
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let got = ulib::pipe_recv(&mut buf);
        if got == 0 {
            break;
        }
        let room = data_cap - data_len;
        let take = got.min(room);
        data[data_len..data_len + take].copy_from_slice(&buf[..take]);
        data_len += take;
        if take < got {
            truncated = true; // input longer than the data cap - drop the rest
        }
    }

    // Index the lines (content excludes the trailing '\n'). A final line with no
    // newline still counts.
    let mut nlines = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < data_len {
        if data[i] == b'\n' {
            if nlines >= MAX_LINES {
                truncated = true;
                break;
            }
            index[2 * nlines] = start as u32;
            index[2 * nlines + 1] = (i - start) as u32;
            nlines += 1;
            start = i + 1;
        }
        i += 1;
    }
    // Trailing partial line (no final newline) - only if we didn't hit the cap.
    if start < data_len && nlines < MAX_LINES {
        index[2 * nlines] = start as u32;
        index[2 * nlines + 1] = (data_len - start) as u32;
        nlines += 1;
    }

    let data_ro: &[u8] = &data[..data_len];
    heapsort(data_ro, index, nlines, &flags);

    // Emit, applying -u (skip a line equal to the previously emitted one).
    let mut have_prev = false;
    let mut prev_s = 0usize;
    let mut prev_l = 0usize;
    for k in 0..nlines {
        let s = index[2 * k] as usize;
        let l = index[2 * k + 1] as usize;
        let cur = &data_ro[s..s + l];
        if flags.unique && have_prev {
            let prev = &data_ro[prev_s..prev_s + prev_l];
            if compare(cur, prev, &flags) == Ordering::Equal {
                continue;
            }
        }
        ulib::write_out(target, cur);
        ulib::write_out(target, b"\n");
        prev_s = s;
        prev_l = l;
        have_prev = true;
    }

    if truncated {
        ulib::con_write(b"sort: input too large - sorted a truncated prefix\r\n");
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Parse leading `-` flag words (each letter a flag; combinable). A `-`-word
/// with an unknown letter is an error. There are no positional operands (sort
/// reads stdin), so anything non-flag is rejected too.
fn parse_flags(target: u64) -> Flags {
    let mut f = Flags { reverse: false, numeric: false, unique: false, fold: false };
    let mut ai = 1;
    let mut argbuf = [0u8; 32];
    while let Some(len) = ulib::arg(ai, &mut argbuf) {
        if len == 0 {
            break;
        }
        let a = &argbuf[..len];
        if a.len() >= 2 && a[0] == b'-' {
            for &c in &a[1..] {
                match c {
                    b'r' => f.reverse = true,
                    b'n' => f.numeric = true,
                    b'u' => f.unique = true,
                    b'f' => f.fold = true,
                    _ => {
                        ulib::con_write(b"sort: unknown flag (use -r, -n, -u, -f)\r\n");
                        ulib::end_of_stream(target);
                        ulib::exit(1);
                    }
                }
            }
        } else {
            ulib::con_write(b"sort: reads stdin; it takes no file operands\r\n");
            ulib::end_of_stream(target);
            ulib::exit(1);
        }
        ai += 1;
    }
    f
}

/// Order two lines by the active flags. Numeric compares a leading integer
/// (ties broken lexicographically); fold compares case-insensitively (ASCII);
/// otherwise a plain byte compare. `reverse` flips the final result.
fn compare(a: &[u8], b: &[u8], f: &Flags) -> Ordering {
    let mut ord = if f.numeric {
        parse_num(a).cmp(&parse_num(b)).then_with(|| lex(a, b, f.fold))
    } else {
        lex(a, b, f.fold)
    };
    if f.reverse {
        ord = ord.reverse();
    }
    ord
}

/// Lexicographic byte comparison, optionally ASCII-case-folded.
fn lex(a: &[u8], b: &[u8], fold: bool) -> Ordering {
    let n = a.len().min(b.len());
    for i in 0..n {
        let (x, y) = if fold {
            (a[i].to_ascii_lowercase(), b[i].to_ascii_lowercase())
        } else {
            (a[i], b[i])
        };
        match x.cmp(&y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    a.len().cmp(&b.len())
}

/// Parse a leading integer (skip spaces/tabs, optional `+`/`-`, then digits).
/// No number, or overflow, saturates - enough for `sort -n`.
fn parse_num(s: &[u8]) -> i64 {
    let mut i = 0;
    while i < s.len() && (s[i] == b' ' || s[i] == b'\t') {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        neg = s[i] == b'-';
        i += 1;
    }
    let mut v: i64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((s[i] - b'0') as i64);
        i += 1;
    }
    if neg {
        -v
    } else {
        v
    }
}

/// In-place heapsort over the line index (`index[2k]`=start, `index[2k+1]`=len
/// for k in `0..n`), ordering by [`compare`]. Iterative, no scratch array - the
/// point of `sort` being the filter that can't stream is that its state is the
/// whole input, so its *working* memory stays O(1) beyond that.
fn heapsort(data: &[u8], index: &mut [u32], n: usize, f: &Flags) {
    if n < 2 {
        return;
    }
    // Build a max-heap.
    let mut start = n / 2;
    while start > 0 {
        start -= 1;
        sift_down(data, index, start, n, f);
    }
    // Repeatedly move the max to the end and restore the heap.
    let mut end = n;
    while end > 1 {
        end -= 1;
        swap_line(index, 0, end);
        sift_down(data, index, 0, end, f);
    }
}

fn sift_down(data: &[u8], index: &mut [u32], mut root: usize, end: usize, f: &Flags) {
    loop {
        let mut swap = root;
        let child = 2 * root + 1;
        if child < end && line_lt(data, index, swap, child, f) {
            swap = child;
        }
        if child + 1 < end && line_lt(data, index, swap, child + 1, f) {
            swap = child + 1;
        }
        if swap == root {
            return;
        }
        swap_line(index, root, swap);
        root = swap;
    }
}

/// Is line `a` ordered before line `b`? (max-heap uses `<`.)
fn line_lt(data: &[u8], index: &[u32], a: usize, b: usize, f: &Flags) -> bool {
    let la = &data[index[2 * a] as usize..index[2 * a] as usize + index[2 * a + 1] as usize];
    let lb = &data[index[2 * b] as usize..index[2 * b] as usize + index[2 * b + 1] as usize];
    compare(la, lb, f) == Ordering::Less
}

fn swap_line(index: &mut [u32], a: usize, b: usize) {
    index.swap(2 * a, 2 * b);
    index.swap(2 * a + 1, 2 * b + 1);
}

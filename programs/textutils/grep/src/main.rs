//! `grep [-i] [-v] [-n] [-F] PATTERN` - externalized line filter: prints the
//! stdin lines matching `PATTERN` to its stdout target.
//!
//! `PATTERN` is a **POSIX extended regular expression** (the `egrep` dialect):
//! `.` `*` `+` `?` `[...]` `^` `$` `|` `(...)` all work unescaped, via the
//! shared [`regex`] crate. `-F` turns that off and matches the pattern as a
//! plain substring, which is what this command did before regexes existed.
//!
//! Flags: `-i` case-insensitive (ASCII); `-v` invert (print the lines that do
//! *not* match); `-n` prefix each printed line with its 1-based input line
//! number; `-F` fixed-string (literal) matching. They may be given separately
//! (`-i -v`) or combined (`-iv`), before the pattern.
//!
//! Line-buffered - stdin arrives in arbitrary chunks, so bytes accumulate into
//! a line buffer and each complete line (`\n`) is tested and emitted whole if it
//! matches; a trailing partial line at end-of-stream is tested too. A line
//! longer than the buffer is tested in buffer-sized pieces (each piece counts as
//! a line for `-n`, same as the original).
//!
//! The line terminator is **stripped before matching** and restored when the
//! line is printed, so `$` anchors to the end of the visible text rather than
//! to a `\n` the user never typed.
//!
//! A pattern that can't be compiled is reported and `grep` exits 1 rather than
//! falling back to a substring search - silently matching something other than
//! what was asked for is worse than failing. If the engine hits its bounded
//! backtracking limit on some line (see `regex`'s `Match::Limit`), that line is
//! not printed and a one-time warning says so; the alternative would be to
//! quietly report "no match" for a line nobody actually decided about.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;

/// How this run tests a line: a compiled regex (the default), or the literal
/// substring search `-F` selects.
///
/// The variants are wildly different sizes (a compiled `Regex` is ~1.4 KB of
/// fixed arrays, `Fixed` carries nothing), which clippy flags as a boxing
/// opportunity - but there is exactly one of these per run, it lives on
/// `_start`'s frame, and there is no heap to box into.
#[allow(clippy::large_enum_variant)]
enum Matcher {
    Re(regex::Regex),
    Fixed,
}

/// Does `hay` contain `needle` as a contiguous substring? Hand-rolled (no std),
/// relocation-safe. An empty needle matches every line. With `fold`, both sides
/// are ASCII-lowercased per byte before comparison (case-insensitive).
fn contains(hay: &[u8], needle: &[u8], fold: bool) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        let mut j = 0;
        while j < needle.len() && eq(hay[i + j], needle[j], fold) {
            j += 1;
        }
        if j == needle.len() {
            return true;
        }
        i += 1;
    }
    false
}

/// Byte equality, optionally case-folded (ASCII).
fn eq(a: u8, b: u8, fold: bool) -> bool {
    if fold {
        let la = if a.is_ascii_uppercase() { a + 32 } else { a };
        let lb = if b.is_ascii_uppercase() { b + 32 } else { b };
        la == lb
    } else {
        a == b
    }
}

/// Emit a matched line to `target`, prefixed with `n` (right-aligned, `cat -n`
/// style) when `-n` is set. `line` includes its own trailing `\n` when the
/// input line had one; the number prefix is placed before it.
fn emit(target: u64, line: &[u8], numbered: bool, n: u64) {
    if numbered {
        let mut num = [0u8; 24];
        let w = fmt_u64_tab(&mut num, n);
        ulib::write_out(target, &num[..w]);
    }
    ulib::write_out(target, line);
}

/// Format `n` right-aligned in 6 columns followed by a tab into `buf`, returning
/// the byte count (the `nl`/`cat -n` prefix shape). No `core::fmt` - hand-rolled
/// decimal to stay clear of the PIE relocation wall filters must avoid.
fn fmt_u64_tab(buf: &mut [u8; 24], mut n: u64) -> usize {
    let mut digits = [0u8; 20];
    let mut d = 0;
    if n == 0 {
        digits[0] = b'0';
        d = 1;
    } else {
        while n > 0 {
            digits[d] = b'0' + (n % 10) as u8;
            n /= 10;
            d += 1;
        }
    }
    let mut w = 0;
    // Right-align in a 6-wide field.
    while w + d < 6 {
        buf[w] = b' ';
        w += 1;
    }
    for k in 0..d {
        buf[w] = digits[d - 1 - k];
        w += 1;
    }
    buf[w] = b'\t';
    w + 1
}

/// The line without its terminator - what the pattern is actually tested
/// against, so `$` means "end of line" and `.` never matches a newline.
fn trim_eol(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    if end > 0 && line[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && line[end - 1] == b'\r' {
        end -= 1;
    }
    &line[..end]
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: <input> | grep [-i] [-v] [-n] [-F] <pattern>  (pattern is an extended regex; -i ignore case, -v invert, -n number, -F literal)\r\n",
    );
    let target = ulib::stdout_target();

    // Parse leading `-` flags, then the pattern. Args are read one at a time via
    // ulib::arg; a `-`-prefixed arg is flags (each letter), the first non-flag
    // arg is the pattern.
    let mut fold = false;
    let mut invert = false;
    let mut numbered = false;
    let mut fixed = false;
    let mut patbuf = [0u8; MAX_LINE];
    let mut pat_len = 0usize;
    let mut have_pattern = false;

    let mut ai = 1;
    let mut argbuf = [0u8; MAX_LINE];
    while let Some(len) = ulib::arg(ai, &mut argbuf) {
        if len == 0 {
            break;
        }
        let a = &argbuf[..len];
        if !have_pattern && a.len() >= 2 && a[0] == b'-' {
            let mut ok = true;
            for &f in &a[1..] {
                match f {
                    b'i' => fold = true,
                    b'v' => invert = true,
                    b'n' => numbered = true,
                    b'F' => fixed = true,
                    _ => ok = false,
                }
            }
            if !ok {
                ulib::con_write(b"grep: unknown flag (use -i, -v, -n, -F)\r\n");
                ulib::end_of_stream(target);
                ulib::exit(1);
            }
        } else {
            pat_len = len.min(MAX_LINE);
            patbuf[..pat_len].copy_from_slice(&argbuf[..pat_len]);
            have_pattern = true;
        }
        ai += 1;
    }

    if !have_pattern {
        ulib::con_write(b"grep: usage: ... | grep [-i] [-v] [-n] [-F] <pattern>\r\n");
        ulib::end_of_stream(target);
        ulib::exit(1);
    }
    let pattern = &patbuf[..pat_len];

    // Compile once, up front: a bad pattern is a usage error, not something to
    // discover halfway through the stream.
    let matcher = if fixed {
        Matcher::Fixed
    } else {
        match regex::Regex::compile(pattern) {
            Ok(re) => Matcher::Re(re),
            Err(e) => {
                ulib::con_write(b"grep: bad pattern: ");
                ulib::con_write(e.message());
                ulib::con_write(b"\r\n");
                ulib::end_of_stream(target);
                ulib::exit(1);
            }
        }
    };

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;
    let mut lineno = 0u64;
    let mut warned = false;
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let n = ulib::pipe_recv(&mut buf);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            line[ll] = b;
            ll += 1;
            // Emit on a completed line, or when the buffer is full (a very long
            // line tested in pieces).
            if b == b'\n' || ll == line.len() {
                lineno += 1;
                if test(&matcher, &line[..ll], pattern, fold, &mut warned) == Some(!invert) {
                    emit(target, &line[..ll], numbered, lineno);
                }
                ll = 0;
            }
        }
    }
    // A trailing line with no final newline.
    if ll > 0 {
        lineno += 1;
        if test(&matcher, &line[..ll], pattern, fold, &mut warned) == Some(!invert) {
            emit(target, &line[..ll], numbered, lineno);
        }
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Test one line: `Some(true)`/`Some(false)` for a decided match, `None` when
/// the engine ran out of budget. `warned` makes the bounded-engine notice a
/// one-time message rather than one per line.
///
/// `None` is deliberately NOT a `false`. Collapsing it to one meant `-v` XORed
/// it and *printed* the line - so grep announced it was skipping lines while
/// emitting them, and an exclusion filter passed through the very lines that
/// match. An undecidable line is skipped under both polarities.
fn test(m: &Matcher, line: &[u8], pattern: &[u8], fold: bool, warned: &mut bool) -> Option<bool> {
    let text = trim_eol(line);
    match m {
        Matcher::Fixed => Some(contains(text, pattern, fold)),
        Matcher::Re(re) => match re.is_match(text, fold) {
            regex::Match::Yes => Some(true),
            regex::Match::No => Some(false),
            regex::Match::Limit => {
                if !*warned {
                    ulib::con_write(
                        b"grep: pattern too costly to decide on some lines; they are skipped\r\n",
                    );
                    *warned = true;
                }
                None
            }
        },
    }
}

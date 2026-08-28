//! `grep [-i] [-v] [-n] PATTERN` - externalized line filter: prints the stdin
//! lines that contain `PATTERN` (a plain substring, no regex), to its stdout
//! target. Flags: `-i` case-insensitive match (ASCII); `-v` invert (print the
//! lines that do *not* match); `-n` prefix each printed line with its 1-based
//! input line number.
//!
//! Flags may be given separately (`-i -v`) or combined (`-iv`), before the
//! pattern. Line-buffered - stdin arrives in arbitrary chunks, so bytes
//! accumulate into a line buffer and each complete line (`\n`) is tested and
//! emitted whole if it matches; a trailing partial line at end-of-stream is
//! tested too. A line longer than the buffer is tested in buffer-sized pieces
//! (each piece counts as a line for `-n`, same as the original).
//!
//! Substring only, deliberately: real regex is a separate, larger arc (see the
//! roadmap's option-parser / richer-commands north-star). `-i` closes the
//! "case-sensitive" half of the gap; the flags are the usable subset people
//! actually reach for.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;

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
        a.eq_ignore_ascii_case(&b)
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

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: <input> | grep [-i] [-v] [-n] <pattern>  (keep lines containing pattern; -i ignore case, -v invert, -n number)\r\n",
    );
    let target = ulib::stdout_target();

    // Parse leading `-` flags, then the pattern. Args are read one at a time via
    // ulib::arg; a `-`-prefixed arg is flags (each letter), the first non-flag
    // arg is the pattern.
    let mut fold = false;
    let mut invert = false;
    let mut numbered = false;
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
                    _ => ok = false,
                }
            }
            if !ok {
                ulib::con_write(b"grep: unknown flag (use -i, -v, -n)\r\n");
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
        ulib::con_write(b"grep: usage: ... | grep [-i] [-v] [-n] <pattern>\r\n");
        ulib::end_of_stream(target);
        ulib::exit(1);
    }
    let pattern = &patbuf[..pat_len];

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;
    let mut lineno = 0u64;
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
                if contains(&line[..ll], pattern, fold) != invert {
                    emit(target, &line[..ll], numbered, lineno);
                }
                ll = 0;
            }
        }
    }
    // A trailing line with no final newline.
    if ll > 0 {
        lineno += 1;
        if contains(&line[..ll], pattern, fold) != invert {
            emit(target, &line[..ll], numbered, lineno);
        }
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

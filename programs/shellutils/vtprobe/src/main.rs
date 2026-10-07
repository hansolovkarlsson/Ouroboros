//! `vtprobe COLS ROWS` - a probe, not a tool: draws a fixed screen with every
//! escape sequence the console's framebuffer backend acts on, so a screendump
//! can be compared with the screen that should result. Item 1 of
//! `docs/handoffs/2026-10-05-from-edit-editor-console.md` (DevTools's Edit);
//! `scripts/test-cond-vt.py` runs it and models the expected screen.
//!
//! The size comes as arguments because an ordinary program cannot ask for it
//! yet (`CON_INFO` is gated to the console server; item 3 of the same note).
//! The steps, in order, with 1-based positions as on the wire:
//!
//! 1. `CSI 2 J`, `CSI H`, then every row filled through its last column
//!    with its own letter, `a` for row 1, `b` for row 2, and so on round the
//!    alphabet: the bottom row's last glyph must not scroll the screen, and
//!    if it did, every row kept below would show the next row's letter.
//! 2. `CSI 2;5 H`, `CSI 1 J`: row 1 and row 2's columns 1 to 5 blank.
//! 3. Rows 3, 4, 5 at column 10: `CSI K`, `CSI 1 K`, `CSI 2 K`.
//! 4. Row 6 from column 5: `REV` reversed by `CSI 7 m`, `nor` after `CSI 0 m`,
//!    `x` reversed, `y` after a bare `CSI m`, `z` reversed, `w` after `CSI 27 m`.
//! 5. Row 7 at column 3: `CSI ? 25 l`, `CSI ? 25 h`, then `ok`.
//! 6. `CSI 8 H` (column omitted) then `r8`; `CSI ;5 H` (row omitted) then `d`;
//!    `CSI H` then `h`; `CSI 1;31 m` and `CSI 3 g` (ignored) then `q`.
//! 7. Row 9: `XY` ending in the last column, then `BS`, space, `BS`, `Z`:
//!    the last column reads `Z` and nothing wraps.
//! 8. `CSI 10;20 H`, `CSI J`: row 10 from column 20 and every row below blank.
//! 9. Row 11's last column: `PQ`, so `Q` wraps to row 12 column 1.
//! 10. Row 13: `ESC ( B`, an OSC ended by BEL, an OSC ended by `ESC \`, and
//!     `CSI 3 J` (the scrollback, which there is none of), then `e`: only
//!     the `e` shows, in column 1, and nothing else on the screen changes.
//! 11. `CSI 999;999 H` then `c`: clamped to the bottom-right cell, no scroll.
//! 12. `CSI 14;1 H`, so what the shell prints next lands below the checks.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let (Some(cols), Some(rows)) = (number_arg(1), number_arg(2)) else {
        ulib::write_out(target, b"usage: vtprobe COLS ROWS\r\n");
        ulib::end_of_stream(target);
        ulib::exit(1);
    };
    if cols < 24 || rows < 16 {
        ulib::write_out(target, b"vtprobe: needs at least 24 columns and 16 rows\r\n");
        ulib::end_of_stream(target);
        ulib::exit(1);
    }
    let out = |bytes: &[u8]| ulib::write_out(target, bytes);
    let at = |row: usize, col: usize| {
        let mut buf = [0u8; 24];
        let mut n = 0;
        for &b in b"\x1b[" {
            buf[n] = b;
            n += 1;
        }
        n += decimal(row, &mut buf[n..]);
        buf[n] = b';';
        n += 1;
        n += decimal(col, &mut buf[n..]);
        buf[n] = b'H';
        n += 1;
        ulib::write_out(target, &buf[..n]);
    };

    // 1. Clear, then fill every cell, each row with its own letter.
    out(b"\x1b[2J\x1b[H");
    for row in 1..=rows {
        let line = [b'a' + ((row - 1) % 26) as u8; 256];
        at(row, 1);
        let mut left = cols;
        while left > 0 {
            let n = left.min(line.len());
            out(&line[..n]);
            left -= n;
        }
    }
    // 2. Erase from the start of the screen to the cursor.
    out(b"\x1b[2;5H\x1b[1J");
    // 3. Erase in line, the three forms.
    at(3, 10);
    out(b"\x1b[K");
    at(4, 10);
    out(b"\x1b[1K");
    at(5, 10);
    out(b"\x1b[2K");
    // 4. Reverse video on and off, each way of turning it off.
    at(6, 5);
    out(b"\x1b[7mREV\x1b[0mnor\x1b[7mx\x1b[my\x1b[7mz\x1b[27mw");
    // 5. Cursor hide and show draw nothing.
    at(7, 3);
    out(b"\x1b[?25l\x1b[?25hok");
    // 6. Omitted parameters, and sequences that are parsed and ignored.
    out(b"\x1b[8Hr8\x1b[;5Hd\x1b[Hh\x1b[1;31m\x1b[3gq");
    // 7. Backspace with a wrap pending.
    at(9, cols - 1);
    out(b"XY\x08 \x08Z");
    // 8. Erase from the cursor to the end of the screen.
    out(b"\x1b[10;20H\x1b[J");
    // 9. The deferred wrap, taken.
    at(11, cols);
    out(b"PQ");
    // 10. Sequences that must draw nothing, then one glyph.
    at(13, 1);
    out(b"\x1b(B\x1b]0;title\x07\x1b]2;t\x1b\\\x1b[3Je");
    // 11. Positions clamp to the screen.
    out(b"\x1b[999;999Hc");
    // 12. Park below the checked rows.
    at(14, 1);
    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Argument `index` as a decimal number, through `ulib::parse_u64`.
fn number_arg(index: u64) -> Option<usize> {
    let mut buf = [0u8; 16];
    let len = ulib::arg(index, &mut buf)?;
    let n = ulib::parse_u64(core::str::from_utf8(buf.get(..len)?).ok()?)?;
    usize::try_from(n).ok().filter(|&n| n <= 100_000)
}

/// Writes `n` in decimal at the start of `out`, returning the length.
fn decimal(n: usize, out: &mut [u8]) -> usize {
    let mut digits = [0u8; 20];
    let mut len = 0;
    let mut v = n;
    loop {
        digits[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    for i in 0..len {
        out[i] = digits[len - 1 - i];
    }
    len
}

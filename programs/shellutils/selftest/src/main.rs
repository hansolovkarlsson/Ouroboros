//! `selftest` - the relocating-loader regression check, now a `/bin` program.
//! Exercises the two patterns that used to crash under the old, non-relocating
//! flat-binary loader - `write!`/`core::fmt::Write`, and a slice/string
//! comparison against a literal - and confirms both produce correct output.
//! Moving it to `/bin` makes it test a *spawned* program's relocation (which is
//! what actually matters - every `/bin` program is a relocated PIE binary),
//! not the shell's. A permanent regression check: these patterns must be
//! ordinary, safe Rust, not something a program author avoids by hand.

#![no_std]
#![no_main]

use core::fmt::Write as _;

/// A `core::fmt::Write` target over `ulib::con_write` - lets the test use real
/// `write!`/`format_args!` (the exact machinery that used to fail to relocate).
struct ConWriter;

impl core::fmt::Write for ConWriter {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        ulib::con_write(s.as_bytes());
        Ok(())
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let mut w = ConWriter;

    // write!/core::fmt: `n` is a runtime value, so this exercises the
    // formatting machinery's argument dispatch, not just a literal string.
    let n = 6 * 7;
    let _ = write!(w, "write!/core::fmt: {n} (expect 42)\r\n");

    // Slice-vs-literal comparison: `probe` is a runtime value compared against
    // a `b"..."` literal with `==` - the shape that crashed as `cwd_bytes != b"/"`.
    let probe: [u8; 1] = *b"/";
    let slice_ok = probe.as_slice() == b"/";
    let _ = write!(w, "slice-vs-literal comparison: {slice_ok} (expect true)\r\n");

    // &str-vs-literal comparison: same shape as the old `component == ".."` crash.
    let word_bytes = *b"hi";
    let word = core::str::from_utf8(&word_bytes).unwrap_or("");
    let str_ok = word == "hi";
    let _ = write!(w, "str-vs-literal comparison: {str_ok} (expect true)\r\n");

    ulib::exit(0);
}

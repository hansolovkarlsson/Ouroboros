//! `uniq` - externalized line filter: collapses runs of *adjacent* identical
//! stdin lines into a single line on its stdout target (the classic pairing is
//! `... | sort | uniq`, but with no `sort` yet it still de-dups already-adjacent
//! repeats). Only neighbours are compared - a line equal to an earlier but
//! non-adjacent line is kept, exactly like Unix `uniq`.
//!
//! Line-buffered, like `grep`: each completed line is compared byte-for-byte
//! against the previously *emitted* line and written only if it differs. Bounded
//! to `MAX_LINE`; a line longer than that is compared/emitted in pieces (each
//! piece treated as a unit), the shared bounded-buffer caveat of these filters.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;

    // The last line actually emitted, held for comparison.
    let mut prev = [0u8; MAX_LINE];
    let mut pl = 0usize;
    let mut have_prev = false;

    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let got = ulib::pipe_recv(&mut buf);
        if got == 0 {
            break;
        }
        for &b in &buf[..got] {
            line[ll] = b;
            ll += 1;
            if b == b'\n' || ll == MAX_LINE {
                emit_if_new(target, &line[..ll], &mut prev, &mut pl, &mut have_prev);
                ll = 0;
            }
        }
    }
    // A trailing line with no final newline.
    if ll > 0 {
        emit_if_new(target, &line[..ll], &mut prev, &mut pl, &mut have_prev);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Write `cur` only if it differs from the previously emitted line, updating the
/// stored previous line when it does.
fn emit_if_new(
    target: u64,
    cur: &[u8],
    prev: &mut [u8; MAX_LINE],
    pl: &mut usize,
    have_prev: &mut bool,
) {
    let dup = *have_prev && cur == &prev[..*pl];
    if !dup {
        ulib::write_out(target, cur);
        prev[..cur.len()].copy_from_slice(cur);
        *pl = cur.len();
        *have_prev = true;
    }
}

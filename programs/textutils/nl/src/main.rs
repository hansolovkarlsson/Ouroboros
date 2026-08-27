//! `nl` - externalized line filter: numbers every stdin line, writing a
//! right-aligned count and a tab before each line's contents to its stdout
//! target (the `cat -n` behavior - every line numbered, blank lines included).
//!
//! Streaming with a line-piece flush, like `grep`: the number is emitted once,
//! at the first piece of each logical line, so a line longer than `MAX_LINE`
//! (flushed in pieces) is still numbered exactly once. The count doesn't need
//! the whole line buffered - only "is this the start of a new line?" - so `nl`
//! keeps just a modest flush buffer.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;
/// Right-alignment width for the line number, matching classic `nl`/`cat -n`.
const NUM_WIDTH: usize = 6;

/// Emit `n` right-aligned in `NUM_WIDTH` columns, followed by a tab.
fn write_number(target: u64, n: u64) {
    let mut num = [0u8; 20];
    let mut nlen = 0usize;
    ulib::emit_dec(&mut num, &mut nlen, n);

    let mut out = [0u8; NUM_WIDTH + 20 + 1];
    let mut k = 0usize;
    let pad = NUM_WIDTH.saturating_sub(nlen);
    for _ in 0..pad {
        out[k] = b' ';
        k += 1;
    }
    out[k..k + nlen].copy_from_slice(&num[..nlen]);
    k += nlen;
    out[k] = b'\t';
    k += 1;
    ulib::write_out(target, &out[..k]);
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;
    // True while we're mid-line and have already printed this line's number
    // (so a continuation piece of a long line doesn't get numbered again).
    let mut mid_line = false;
    let mut n: u64 = 0;

    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let got = ulib::pipe_recv(&mut buf);
        if got == 0 {
            break;
        }
        for &b in &buf[..got] {
            line[ll] = b;
            ll += 1;
            let newline = b == b'\n';
            if newline || ll == MAX_LINE {
                if !mid_line {
                    n += 1;
                    write_number(target, n);
                }
                ulib::write_out(target, &line[..ll]);
                mid_line = !newline; // ended on '\n' => next byte starts a new line
                ll = 0;
            }
        }
    }
    // A trailing line with no final newline.
    if ll > 0 {
        if !mid_line {
            n += 1;
            write_number(target, n);
        }
        ulib::write_out(target, &line[..ll]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

//! `rev` - externalized line filter: reverses the character order of each stdin
//! line, writing the result to its stdout target. The trailing newline stays at
//! the end (only the content before it is reversed), so `rev` of a text file is
//! still line-shaped.
//!
//! Line-buffered, like `grep`. Reversal needs the whole line at once, so a line
//! longer than `MAX_LINE` is reversed in `MAX_LINE`-sized pieces (each piece
//! reversed independently) rather than as one span - a bounded-buffer caveat
//! shared with the other filters here, not a correctness goal for huge lines.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;

    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let got = ulib::pipe_recv(&mut buf);
        if got == 0 {
            break;
        }
        for &b in &buf[..got] {
            if b == b'\n' {
                reverse(&mut line[..ll]); // content only; the '\n' is re-added
                line[ll] = b'\n';
                ulib::write_out(target, &line[..ll + 1]);
                ll = 0;
            } else {
                line[ll] = b;
                ll += 1;
                if ll == MAX_LINE {
                    // Over-long line: flush this piece reversed (see doc caveat).
                    reverse(&mut line[..ll]);
                    ulib::write_out(target, &line[..ll]);
                    ll = 0;
                }
            }
        }
    }
    // A trailing line with no final newline.
    if ll > 0 {
        reverse(&mut line[..ll]);
        ulib::write_out(target, &line[..ll]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Reverse a byte slice in place (two-pointer swap - no_std, no alloc).
fn reverse(s: &mut [u8]) {
    if s.len() < 2 {
        return;
    }
    let mut i = 0usize;
    let mut j = s.len() - 1;
    while i < j {
        s.swap(i, j);
        i += 1;
        j -= 1;
    }
}

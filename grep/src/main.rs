//! `grep PATTERN` - externalized line filter: prints the stdin lines that
//! contain `PATTERN` (a plain substring, no regex), to its stdout target.
//! Line-buffered - stdin arrives in arbitrary chunks, so bytes accumulate into
//! a line buffer and each complete line (`\n`) is tested and emitted whole if
//! it matches; a trailing partial line at end-of-stream is tested too. A line
//! longer than the buffer is tested in buffer-sized pieces.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;

/// Does `hay` contain `needle` as a contiguous substring? Hand-rolled (no std),
/// relocation-safe. An empty needle matches every line.
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if &hay[i..i + needle.len()] == needle {
            return true;
        }
        i += 1;
    }
    false
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut patbuf = [0u8; MAX_LINE];
    let pat_len = match ulib::arg(1, &mut patbuf) {
        Some(len) if len > 0 => len,
        _ => {
            ulib::con_write(b"grep: usage: ... | grep <pattern>\r\n");
            ulib::end_of_stream(target);
            ulib::exit(1);
        }
    };
    let pattern = &patbuf[..pat_len];

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;
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
                if contains(&line[..ll], pattern) {
                    ulib::write_out(target, &line[..ll]);
                }
                ll = 0;
            }
        }
    }
    // A trailing line with no final newline.
    if ll > 0 && contains(&line[..ll], pattern) {
        ulib::write_out(target, &line[..ll]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

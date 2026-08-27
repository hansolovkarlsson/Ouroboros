//! `head [N]` - externalized line filter: prints the first `N` lines of its
//! stdin (default 10) to its stdout target, then stops. Line-buffered like
//! `grep`; once `N` complete lines are out it signals end-of-stream and exits
//! early (the upstream producer's next send fails harmlessly - see `pipe_out`'s
//! bounded retry - and it stops too).

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;
const DEFAULT_LINES: u64 = 10;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: <input> | head [N]  (first N stdin lines, default 10)\r\n");
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; 24];
    let limit = match ulib::arg(1, &mut argbuf) {
        Some(len) if len > 0 => {
            let s = core::str::from_utf8(&argbuf[..len]).unwrap_or("");
            match ulib::parse_u64(s) {
                Some(v) => v,
                None => {
                    ulib::con_write(b"head: line count must be a number\r\n");
                    ulib::end_of_stream(target);
                    ulib::exit(1);
                }
            }
        }
        _ => DEFAULT_LINES,
    };

    let mut line = [0u8; MAX_LINE];
    let mut ll = 0usize;
    let mut emitted: u64 = 0;
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];

    // Zero requested: emit nothing, finish immediately.
    if limit == 0 {
        ulib::end_of_stream(target);
        ulib::exit(0);
    }

    'outer: loop {
        let n = ulib::pipe_recv(&mut buf);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            line[ll] = b;
            ll += 1;
            if b == b'\n' {
                ulib::write_out(target, &line[..ll]);
                ll = 0;
                emitted += 1;
                if emitted >= limit {
                    break 'outer;
                }
            } else if ll == line.len() {
                // A long line, flushed in pieces - not counted until its `\n`.
                ulib::write_out(target, &line[..ll]);
                ll = 0;
            }
        }
    }
    // A trailing line with no final newline still counts toward the limit.
    if ll > 0 && emitted < limit {
        ulib::write_out(target, &line[..ll]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

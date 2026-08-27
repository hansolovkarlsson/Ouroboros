//! `tail [N]` - externalized line filter: prints the last `N` lines of its
//! stdin (default 10) to its stdout target. The complement of `head`. Unlike
//! `head`, `tail` can't stop early - it must read stdin to end-of-stream before
//! it knows which lines are the last ones - so it keeps a fixed ring of the most
//! recent lines and flushes them at EOF.
//!
//! Bounded, like every filter here: at most `MAX_KEEP` lines are retained (a
//! larger `N` is capped to that), and a single line longer than `MAX_LINE` is
//! truncated to its first `MAX_LINE` bytes. Both keep the whole working set on a
//! fixed stack buffer with no heap - see the ring below.

#![no_std]
#![no_main]

const MAX_LINE: usize = 256;
/// Ring capacity: the most lines `tail` will ever hold (and so the largest
/// effective `N`). `MAX_KEEP * MAX_LINE` is the stack footprint of the ring.
const MAX_KEEP: usize = 64;
const DEFAULT_LINES: u64 = 10;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: <input> | tail [N]  (last N stdin lines, default 10)\r\n");
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; 24];
    let requested = match ulib::arg(1, &mut argbuf) {
        Some(len) if len > 0 => {
            let s = core::str::from_utf8(&argbuf[..len]).unwrap_or("");
            match ulib::parse_u64(s) {
                Some(v) => v,
                None => {
                    ulib::con_write(b"tail: line count must be a number\r\n");
                    ulib::end_of_stream(target);
                    ulib::exit(1);
                }
            }
        }
        _ => DEFAULT_LINES,
    };

    // Cap the request to the ring capacity; a request of 0 keeps nothing.
    let keep = if requested as usize > MAX_KEEP {
        MAX_KEEP
    } else {
        requested as usize
    };
    if keep == 0 {
        // Drain stdin so the producer isn't left blocked, then finish.
        let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        while ulib::pipe_recv(&mut buf) != 0 {}
        ulib::end_of_stream(target);
        ulib::exit(0);
    }

    // The ring: `lines[slot]` holds `lens[slot]` bytes (its trailing '\n'
    // included, when present). `total` counts completed lines seen; the newest
    // `keep` of them live at slots `(total-keep..total) % keep`.
    let mut lines = [[0u8; MAX_LINE]; MAX_KEEP];
    let mut lens = [0usize; MAX_KEEP];
    let mut total: usize = 0;

    // Accumulator for the line currently arriving. `dropping` is set once it has
    // exceeded MAX_LINE, so the overflow bytes are discarded until the newline.
    let mut cur = [0u8; MAX_LINE];
    let mut cl = 0usize;
    let mut dropping = false;

    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let n = ulib::pipe_recv(&mut buf);
        if n == 0 {
            break;
        }
        for &b in &buf[..n] {
            if b == b'\n' {
                if cl < MAX_LINE {
                    cur[cl] = b;
                    cl += 1;
                }
                let slot = total % keep;
                lines[slot][..cl].copy_from_slice(&cur[..cl]);
                lens[slot] = cl;
                total += 1;
                cl = 0;
                dropping = false;
            } else if !dropping {
                cur[cl] = b;
                cl += 1;
                if cl == MAX_LINE {
                    dropping = true; // truncate the rest of this over-long line
                }
            }
        }
    }
    // A trailing line with no final newline still counts.
    if cl > 0 {
        let slot = total % keep;
        lines[slot][..cl].copy_from_slice(&cur[..cl]);
        lens[slot] = cl;
        total += 1;
    }

    let stored = if total < keep { total } else { keep };
    let start = if total < keep { 0 } else { total % keep };
    for i in 0..stored {
        let slot = (start + i) % keep;
        ulib::write_out(target, &lines[slot][..lens[slot]]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

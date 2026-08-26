//! `dial` - open a TCP connection out of a machine's NIC via the Plan 9-style
//! `/net/tcp` connection files, send an optional request, and print what comes
//! back (the export-hardening cluster's dial-out consumer). The point is that
//! `<base>` selects *whose* network dials out: `dial /net …` uses this machine's
//! NIC; `dial /mnt/a/net …` (a remote-mounted export) dials out of **machine
//! A's** NIC - "use another machine's network," the last unshared resource.
//!
//! Usage: `dial <base> <ip> <port> [request words...]`
//!   e.g. `dial /net 10.0.2.2 8000 GET / HTTP/1.0`   (local)
//!        `dial /mnt/a/net 93.184.216.34 80 GET / HTTP/1.0`  (out of A's NIC)
//!
//! It's a thin client over `ulib`'s fs helpers: read `<base>/tcp/clone` for a
//! connection number N, write `connect ip!port` to `<base>/tcp/N/ctl`, poll
//! `<base>/tcp/N/status` until Established, write the request to
//! `<base>/tcp/N/data`, then read `<base>/tcp/N/data` until the peer closes.
//! netd does all the TCP; this just moves bytes through files.

#![no_std]
#![no_main]

use ulib::{arg, argc, con_write, exit, fs_read_file, fs_write_inline, stdout_target, write_out};

/// Append `s` to `out` at `*w` (bounded); returns nothing, advances `*w`.
fn push(out: &mut [u8], w: &mut usize, s: &[u8]) {
    for &b in s {
        if *w < out.len() {
            out[*w] = b;
            *w += 1;
        }
    }
}

/// Build `<base>/tcp/<n>/<leaf>` (or `<base>/tcp/clone` when `leaf` is empty and
/// `n` is None) into `out`, returning it as a `&str`. `n` is a single digit
/// (MAX_DIAL is small), so no decimal formatting is needed.
fn join<'a>(base: &str, n: Option<u8>, leaf: &[u8], out: &'a mut [u8]) -> Option<&'a str> {
    let mut w = 0usize;
    push(out, &mut w, base.as_bytes());
    push(out, &mut w, b"/tcp/");
    match n {
        None => push(out, &mut w, b"clone"),
        Some(d) => {
            if w < out.len() {
                out[w] = b'0' + d;
                w += 1;
            }
            push(out, &mut w, b"/");
            push(out, &mut w, leaf);
        }
    }
    core::str::from_utf8(&out[..w]).ok()
}

fn fail(msg: &[u8], target: u64) -> ! {
    con_write(msg);
    ulib::end_of_stream(target);
    exit(1);
}

/// Read a `/net/tcp` file into `buf`; returns the byte count, or `None` on error.
fn read_file(path: &str, buf: &mut [u8]) -> Option<usize> {
    let r = fs_read_file(path, buf);
    if r >= syscall_abi::FS_ERR_MIN {
        None
    } else {
        Some((r as usize).min(buf.len()))
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = stdout_target();
    if argc() < 4 {
        fail(b"dial: usage: dial <base> <ip> <port> [request...]\r\n", target);
    }
    let mut a_base = [0u8; 96];
    let mut a_ip = [0u8; 24];
    let mut a_port = [0u8; 8];
    let base = match arg(1, &mut a_base) {
        Some(n) if n > 0 => core::str::from_utf8(&a_base[..n]).unwrap_or(""),
        _ => fail(b"dial: bad base\r\n", target),
    };
    let ip = match arg(2, &mut a_ip) {
        Some(n) if n > 0 => &a_ip[..n],
        _ => fail(b"dial: bad ip\r\n", target),
    };
    let port = match arg(3, &mut a_port) {
        Some(n) if n > 0 => &a_port[..n],
        _ => fail(b"dial: bad port\r\n", target),
    };

    let mut pbuf = [0u8; 128];

    // 1. clone -> connection number N.
    let clone_path = match join(base, None, b"", &mut pbuf) {
        Some(p) => p,
        None => fail(b"dial: path too long\r\n", target),
    };
    let mut nbuf = [0u8; 16];
    let Some(nlen) = read_file(clone_path, &mut nbuf) else {
        fail(b"dial: clone failed (is <base>/tcp mounted? mount -n /net)\r\n", target);
    };
    if nlen == 0 || !nbuf[0].is_ascii_digit() {
        fail(b"dial: clone gave no connection\r\n", target);
    }
    let n = nbuf[0] - b'0';

    // 2. connect: write "connect ip!port" to N/ctl.
    let mut ctlbuf = [0u8; 64];
    let mut cw = 0usize;
    push(&mut ctlbuf, &mut cw, b"connect ");
    push(&mut ctlbuf, &mut cw, ip);
    push(&mut ctlbuf, &mut cw, b"!");
    push(&mut ctlbuf, &mut cw, port);
    let ctl_path = match join(base, Some(n), b"ctl", &mut pbuf) {
        Some(p) => p,
        None => fail(b"dial: path too long\r\n", target),
    };
    if fs_write_inline(ctl_path, &ctlbuf[..cw]) >= syscall_abi::FS_ERR_MIN {
        fail(b"dial: connect refused\r\n", target);
    }

    // 3. poll status until Established (or Closed/error). Each read is a blocking
    // IPC round trip, which gives netd's event loop passes to complete the
    // handshake - so this converges in a few polls, no explicit sleep needed.
    let mut established = false;
    for _ in 0..80 {
        let status_path = match join(base, Some(n), b"status", &mut pbuf) {
            Some(p) => p,
            None => fail(b"dial: path too long\r\n", target),
        };
        let mut sbuf = [0u8; 16];
        let sn = read_file(status_path, &mut sbuf).unwrap_or(0);
        if sbuf[..sn].starts_with(b"Established") {
            established = true;
            break;
        }
        if sbuf[..sn].starts_with(b"Closed") {
            fail(b"dial: connection refused / closed\r\n", target);
        }
    }
    if !established {
        fail(b"dial: connect timed out\r\n", target);
    }

    // 4. optional request: the remaining args joined by spaces, then CRLF.
    if argc() > 4 {
        let mut reqbuf = [0u8; 256];
        let mut rw = 0usize;
        let mut i = 4u64;
        while i < argc() {
            if rw > 0 {
                push(&mut reqbuf, &mut rw, b" ");
            }
            let mut ab = [0u8; 200];
            if let Some(an) = arg(i, &mut ab) {
                push(&mut reqbuf, &mut rw, &ab[..an]);
            }
            i += 1;
        }
        push(&mut reqbuf, &mut rw, b"\r\n\r\n");
        let data_path = match join(base, Some(n), b"data", &mut pbuf) {
            Some(p) => p,
            None => fail(b"dial: path too long\r\n", target),
        };
        let _ = fs_write_inline(data_path, &reqbuf[..rw]);
    }

    // 5. read the response: drain N/data, print it, until the peer closes (a
    // Closed status with nothing more buffered). Bounded so a server that never
    // closes still terminates.
    let mut empties = 0u32;
    for _ in 0..2000 {
        let data_path = match join(base, Some(n), b"data", &mut pbuf) {
            Some(p) => p,
            None => break,
        };
        let mut dbuf = [0u8; 512];
        let dn = read_file(data_path, &mut dbuf).unwrap_or(0);
        if dn > 0 {
            write_out(target, &dbuf[..dn]);
            empties = 0;
            continue;
        }
        // Nothing buffered right now - check whether the peer has closed.
        let status_path = match join(base, Some(n), b"status", &mut pbuf) {
            Some(p) => p,
            None => break,
        };
        let mut sbuf = [0u8; 16];
        let sn = read_file(status_path, &mut sbuf).unwrap_or(0);
        if sbuf[..sn].starts_with(b"Closed") {
            break;
        }
        empties += 1;
        if empties > 200 {
            break; // give up on a server that stays open but sends nothing
        }
    }

    // 6. close.
    if let Some(ctl_path) = join(base, Some(n), b"ctl", &mut pbuf) {
        let _ = fs_write_inline(ctl_path, b"close");
    }
    ulib::end_of_stream(target);
    exit(0);
}

//! `serve` - accept one inbound TCP connection **on another machine's network**
//! via the Plan 9-style `/net/tcp` dial-in files, send a canned response, and
//! close (the dial-in consumer, the mirror of `dial`). `<base>` selects *whose*
//! network answers: `serve /net …` listens on this machine's NIC; `serve
//! /mnt/a/net …` (a remote-mounted export) listens on **machine A's** NIC, so a
//! client that connects to A's IP is answered by a program running here.
//!
//! Usage: `serve <base> <port> [response words...]`
//!   e.g. `serve /net 9000 hello from this machine`      (listen on OUR nic)
//!        `serve /mnt/a/net 9000 hi via A`                (listen on A's nic)
//!
//! Scoped to a single accept-then-exit (proves the passive-open + relay path);
//! a persistent server is a loop over the same steps. netd does all the TCP;
//! this just moves bytes through files: write `announce <port>` to
//! `<base>/tcp/N/ctl`, poll `<base>/tcp/N/listen` for an accepted connection M,
//! read `<base>/tcp/M/data` (the request), write the response, `close`.

#![no_std]
#![no_main]

use ulib::{arg, argc, con_write, exit, fs_read_file, fs_write_inline, stdout_target, write_out};

fn push(out: &mut [u8], w: &mut usize, s: &[u8]) {
    for &b in s {
        if *w < out.len() {
            out[*w] = b;
            *w += 1;
        }
    }
}

/// Build `<base>/tcp/<n>/<leaf>` (or `<base>/tcp/clone` when `n` is None).
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
    if argc() < 3 {
        fail(b"serve: usage: serve <base> <port> [response...]\r\n", target);
    }
    let mut a_base = [0u8; 96];
    let mut a_port = [0u8; 8];
    let base = match arg(1, &mut a_base) {
        Some(n) if n > 0 => core::str::from_utf8(&a_base[..n]).unwrap_or(""),
        _ => fail(b"serve: bad base\r\n", target),
    };
    let port = match arg(2, &mut a_port) {
        Some(n) if n > 0 => &a_port[..n],
        _ => fail(b"serve: bad port\r\n", target),
    };

    let mut pbuf = [0u8; 128];

    // 1. clone -> listener slot N.
    let clone_path = match join(base, None, b"", &mut pbuf) {
        Some(p) => p,
        None => fail(b"serve: path too long\r\n", target),
    };
    let mut nbuf = [0u8; 16];
    let Some(nlen) = read_file(clone_path, &mut nbuf) else {
        fail(b"serve: clone failed (is <base>/tcp mounted? mount -n /net)\r\n", target);
    };
    if nlen == 0 || !nbuf[0].is_ascii_digit() {
        fail(b"serve: clone gave no connection\r\n", target);
    }
    let n = nbuf[0] - b'0';

    // 2. announce the port.
    let mut ctlbuf = [0u8; 32];
    let mut cw = 0usize;
    push(&mut ctlbuf, &mut cw, b"announce ");
    push(&mut ctlbuf, &mut cw, port);
    let ctl_path = match join(base, Some(n), b"ctl", &mut pbuf) {
        Some(p) => p,
        None => fail(b"serve: path too long\r\n", target),
    };
    if fs_write_inline(ctl_path, &ctlbuf[..cw]) >= syscall_abi::FS_ERR_MIN {
        fail(b"serve: announce refused\r\n", target);
    }
    write_out(target, b"listening on ");
    write_out(target, port);
    write_out(target, b" (one connection)...\r\n");

    // 3. poll listen until a connection is accepted.
    let mut m = 0u8;
    let mut got = false;
    for _ in 0..6000 {
        let listen_path = match join(base, Some(n), b"listen", &mut pbuf) {
            Some(p) => p,
            None => fail(b"serve: path too long\r\n", target),
        };
        let mut lbuf = [0u8; 16];
        let ln = read_file(listen_path, &mut lbuf).unwrap_or(0);
        if ln > 0 && lbuf[0].is_ascii_digit() {
            m = lbuf[0] - b'0';
            got = true;
            break;
        }
    }
    if !got {
        fail(b"serve: no connection accepted (timed out)\r\n", target);
    }

    // 4. read the request (drain a little), then respond.
    let mut req = [0u8; 512];
    let mut rlen = 0usize;
    for _ in 0..200 {
        let data_path = match join(base, Some(m), b"data", &mut pbuf) {
            Some(p) => p,
            None => break,
        };
        let mut dbuf = [0u8; 512];
        let dn = read_file(data_path, &mut dbuf).unwrap_or(0);
        if dn > 0 {
            let take = dn.min(req.len() - rlen);
            req[rlen..rlen + take].copy_from_slice(&dbuf[..take]);
            rlen += take;
            break; // one read of the request is enough for the demo
        }
    }
    write_out(target, b"accepted; request: ");
    write_out(target, &req[..rlen]);
    write_out(target, b"\r\n");

    // 5. write the response (remaining args joined by spaces, or a default).
    let mut resp = [0u8; 256];
    let mut rw = 0usize;
    if argc() > 3 {
        let mut i = 3u64;
        while i < argc() {
            if rw > 0 {
                push(&mut resp, &mut rw, b" ");
            }
            let mut ab = [0u8; 200];
            if let Some(an) = arg(i, &mut ab) {
                push(&mut resp, &mut rw, &ab[..an]);
            }
            i += 1;
        }
    } else {
        push(&mut resp, &mut rw, b"hello from Ouroboros /net/tcp");
    }
    push(&mut resp, &mut rw, b"\r\n");
    let data_path = match join(base, Some(m), b"data", &mut pbuf) {
        Some(p) => p,
        None => fail(b"serve: path too long\r\n", target),
    };
    let _ = fs_write_inline(data_path, &resp[..rw]);

    // 6. close the accepted connection (netd flushes the response, then FINs -
    // it holds the conn independent of this program's lifetime).
    if let Some(cp) = join(base, Some(m), b"ctl", &mut pbuf) {
        let _ = fs_write_inline(cp, b"close");
    }
    // Also stop listening.
    if let Some(cp) = join(base, Some(n), b"ctl", &mut pbuf) {
        let _ = fs_write_inline(cp, b"close");
    }
    write_out(target, b"served + closed\r\n");
    ulib::end_of_stream(target);
    exit(0);
}

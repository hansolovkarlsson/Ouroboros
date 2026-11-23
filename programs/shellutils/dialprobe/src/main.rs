//! `dialprobe <ip!port>`: a probe, not a tool. Whether `netd` hands out a
//! `/net/tcp` slot it has closed itself before that slot's client writes
//! `close`, for `make test-net-dial`.
//!
//! In one process, so no other client's timing enters it: clone slot A,
//! write `connect <ip!port>` to A's `ctl` (the rig gives an address nobody
//! answers ARP for, so netd refuses it and sets A `Closed`), clone B, write
//! `close` to A's `ctl`, clone C, then close B and C. It prints one line,
//! `dialprobe: A connect <status> B C`, each slot a number or `-` for a
//! clone that failed, and `<status>` the connect write's answer in decimal.
//! Before the review of #504, netd freed a slot on `Closed` alone, so B was
//! A, and a client's late `close` reached the slot's next owner. Now B is
//! not A and C is.

#![no_std]
#![no_main]

use ulib::{arg, con_write, exit, fs_read_file, fs_write_inline};

/// Clone a slot: its number, or `None` when the clone failed.
fn clone() -> Option<u8> {
    let mut nbuf = [0u8; 16];
    let r = fs_read_file("/net/tcp/clone", &mut nbuf);
    if r >= syscall_abi::FS_ERR_MIN || r == 0 || !nbuf[0].is_ascii_digit() {
        return None;
    }
    Some(nbuf[0] - b'0')
}

/// Write `words` to slot `n`'s `ctl`; the write's answer.
fn ctl(n: u8, words: &[u8]) -> u64 {
    let mut path = *b"/net/tcp/0/ctl";
    path[9] = b'0' + n;
    fs_write_inline(core::str::from_utf8(&path).unwrap_or(""), words)
}

fn put_slot(out: &mut [u8], w: &mut usize, n: Option<u8>) {
    out[*w] = n.map_or(b'-', |n| b'0' + n);
    *w += 1;
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: dialprobe <ip!port>  (a probe for make test-net-dial)\r\n");
    let mut target = [0u8; 32];
    let Some(tlen) = arg(1, &mut target) else {
        con_write(b"dialprobe: usage: dialprobe <ip!port>\r\n");
        exit(1);
    };
    let mut connect = [0u8; 48];
    let head = b"connect ";
    connect[..head.len()].copy_from_slice(head);
    connect[head.len()..head.len() + tlen].copy_from_slice(&target[..tlen]);
    let connect = &connect[..head.len() + tlen];

    let a = clone();
    let status = a.map_or(0, |a| ctl(a, connect));
    let b = clone();
    if let Some(a) = a {
        ctl(a, b"close");
    }
    let c = clone();
    for n in [b, c].into_iter().flatten() {
        ctl(n, b"close");
    }

    let mut out = [0u8; 64];
    let mut w = 0usize;
    for &x in b"dialprobe: " {
        out[w] = x;
        w += 1;
    }
    put_slot(&mut out, &mut w, a);
    for &x in b" connect " {
        out[w] = x;
        w += 1;
    }
    let mut digits = [0u8; 20];
    let mut k = 0usize;
    let mut v = status;
    loop {
        digits[k] = b'0' + (v % 10) as u8;
        k += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    while k > 0 {
        k -= 1;
        out[w] = digits[k];
        w += 1;
    }
    out[w] = b' ';
    w += 1;
    put_slot(&mut out, &mut w, b);
    out[w] = b' ';
    w += 1;
    put_slot(&mut out, &mut w, c);
    out[w..w + 2].copy_from_slice(b"\r\n");
    w += 2;
    con_write(&out[..w]);
    exit(0);
}

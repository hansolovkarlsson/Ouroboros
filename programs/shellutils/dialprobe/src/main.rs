//! `dialprobe <ip!port>`: a probe, not a tool. Whether `netd` hands out a
//! `/net/tcp` slot it has closed itself before that slot's client writes
//! `close`, for `make test-net-dial`.
//!
//! In one process, so no other client's timing enters it: clone slot A,
//! write `connect <ip!port>` to A's `ctl` (the rig gives an address nobody
//! answers ARP for, so netd refuses it and sets A `Closed`), write the same
//! `connect` again, clone B, write `close` to A's `ctl`, clone C, then close
//! B and C. It prints one line, `dialprobe: A connect <s1> again <s2> B C`,
//! each slot a number or `-` for a clone that failed, and each status the
//! write's answer: `0`, `NO_FS`, `FS_ERROR` or a decimal.
//! Before the review of #504, netd freed a slot on `Closed` alone, so B was
//! A, and a client's late `close` reached the slot's next owner. Now B is
//! not A and C is, and the second `connect` is `FS_ERROR`: a slot is
//! connected once, from `Idle` (the second review of #504).
//!
//! `dialprobe accept <port> <ms>`: clone a slot, `announce <port>`, print
//! `dialprobe: listening <N>`, sleep `<ms>`, and only then read `listen`, so
//! a client can connect, send its request and close before the read. It
//! prints `dialprobe: accepted <M> <request>`, or `dialprobe: accepted -`
//! when `listen` hands out nothing. Before the third review of #504, netd
//! freed such a connection on `Closed` with its request unread, and the read
//! found nothing. Then it sleeps `<ms>` again holding M, which its client has
//! closed, so a client can reconnect from M's port in that time, and reads
//! `listen` once more: `dialprobe: again <M2> <request>`, or `again -`. Before
//! the fourth review of #504, a bare SYN on a Closed slot's 4-tuple was used
//! up by that slot and never accepted. Last it closes every slot it holds.

#![no_std]
#![no_main]

use ulib::{arg, argc, con_write, exit, fs_read_file, fs_write_inline};

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

/// A write's answer by name where it has one, else in decimal.
fn put_status(out: &mut [u8], w: &mut usize, v: u64) {
    let name: &[u8] = match v {
        syscall_abi::FS_ERROR => b"FS_ERROR",
        syscall_abi::NO_FS => b"NO_FS",
        _ => b"",
    };
    if !name.is_empty() {
        out[*w..*w + name.len()].copy_from_slice(name);
        *w += name.len();
        return;
    }
    let mut digits = [0u8; 20];
    let mut k = 0usize;
    let mut v = v;
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
        out[*w] = digits[k];
        *w += 1;
    }
}

fn put_slot(out: &mut [u8], w: &mut usize, n: Option<u8>) {
    out[*w] = n.map_or(b'-', |n| b'0' + n);
    *w += 1;
}

/// Read slot `n`'s `leaf` into `buf`: the byte count, 0 on an error.
fn read_leaf(n: u8, leaf: &[u8], buf: &mut [u8]) -> usize {
    let mut path = [0u8; 32];
    let head = b"/net/tcp/0/";
    path[..head.len()].copy_from_slice(head);
    path[9] = b'0' + n;
    path[head.len()..head.len() + leaf.len()].copy_from_slice(leaf);
    let p = core::str::from_utf8(&path[..head.len() + leaf.len()]).unwrap_or("");
    let r = fs_read_file(p, buf);
    if r >= syscall_abi::FS_ERR_MIN {
        0
    } else {
        (r as usize).min(buf.len())
    }
}

/// Read listener `n`'s `listen` and print `<label><M> <request>`, or
/// `<label>-`: the connection handed out, if any.
fn handed(n: u8, label: &[u8]) -> Option<u8> {
    let mut lbuf = [0u8; 16];
    let ln = read_leaf(n, b"listen", &mut lbuf);
    let m = (ln > 0 && lbuf[0].is_ascii_digit()).then(|| lbuf[0] - b'0');
    con_write(label);
    let mut req = [0u8; 64];
    let mut rl = 0usize;
    if let Some(m) = m {
        con_write(&[b'0' + m, b' ']);
        rl = read_leaf(m, b"data", &mut req);
    } else {
        con_write(b"-");
    }
    // The request, its line ends left out so the line stays one line.
    for &b in &req[..rl] {
        if b != b'\r' && b != b'\n' {
            con_write(&[b]);
        }
    }
    con_write(b"\r\n");
    m
}

/// `dialprobe accept <port> <ms>`; see the module doc.
fn accept() -> ! {
    let mut port = [0u8; 8];
    let mut ms = [0u8; 8];
    let (Some(pl), Some(ml)) = (arg(2, &mut port), arg(3, &mut ms)) else {
        con_write(b"dialprobe: usage: dialprobe accept <port> <ms>\r\n");
        exit(1);
    };
    let wait_ms = ms[..ml].iter().fold(0u64, |v, d| v * 10 + d.wrapping_sub(b'0') as u64);
    let Some(n) = clone() else {
        con_write(b"dialprobe: listening -\r\n");
        exit(1);
    };
    let mut announce = [0u8; 24];
    let head = b"announce ";
    announce[..head.len()].copy_from_slice(head);
    announce[head.len()..head.len() + pl].copy_from_slice(&port[..pl]);
    if ctl(n, &announce[..head.len() + pl]) != 0 {
        ctl(n, b"close");
        con_write(b"dialprobe: announce refused\r\n");
        exit(1);
    }
    con_write(b"dialprobe: listening ");
    con_write(&[b'0' + n]);
    con_write(b"\r\n");
    ulib::syscall(syscall_abi::SLEEP_UNTIL, ulib::monotonic_us() + wait_ms * 1000);
    let m = handed(n, b"dialprobe: accepted ");
    ulib::syscall(syscall_abi::SLEEP_UNTIL, ulib::monotonic_us() + wait_ms * 1000);
    let m2 = handed(n, b"dialprobe: again ");
    for k in [m, m2].into_iter().flatten() {
        ctl(k, b"close");
    }
    ctl(n, b"close");
    exit(0);
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: dialprobe <ip!port> | dialprobe accept <port> <ms>  (a probe for make test-net-dial)\r\n");
    let mut mode = [0u8; 8];
    if argc() == 4 && arg(1, &mut mode) == Some(6) && &mode[..6] == b"accept" {
        accept();
    }
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
    let again = a.map_or(0, |a| ctl(a, connect));
    let b = clone();
    if let Some(a) = a {
        ctl(a, b"close");
    }
    let c = clone();
    for n in [b, c].into_iter().flatten() {
        ctl(n, b"close");
    }

    let mut out = [0u8; 96];
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
    put_status(&mut out, &mut w, status);
    for &x in b" again " {
        out[w] = x;
        w += 1;
    }
    put_status(&mut out, &mut w, again);
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

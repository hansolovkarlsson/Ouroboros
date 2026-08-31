//! `/bin/clusterkey` — this machine's cluster identity.
//!
//! Step 6c of `docs/roadmap-cluster-keys.md`. Four things an operator needs:
//!
//! ```text
//! clusterkey              show this machine's public key
//! clusterkey new [-f]     generate a keypair (refuses without real entropy)
//! clusterkey peers        list the peers this machine accepts
//! clusterkey line N IP    print the authorized line to paste on another machine
//! ```
//!
//! ## Why generation refuses rather than degrades
//!
//! A private key is 32 random bytes, and a guessable one is worse than no
//! cryptography at all because it looks like security. The `accounts` crate's
//! `salt_from` degrades to a clock-derived value and says so loudly, which is
//! the right trade for a password salt; it is **not** the right trade for a
//! machine's identity, so this refuses outright when the `RANDOM` syscall
//! reports no entropy device. On QEMU that means `-device virtio-rng-device`;
//! Parallels and the Raspberry Pi have no virtio at all, so on those a key must
//! be generated elsewhere and copied in.

#![no_std]
#![no_main]

use clusterkeys::{KEY_HEX_LEN, KEY_LEN};

const ID_PATH: &str = "/etc/cluster/id";
const PUB_PATH: &str = "/etc/cluster/id.pub";
const AUTHORIZED_PATH: &str = "/etc/cluster/authorized";

/// Mode for the private key: readable by its owner and nobody else. Enforced on
/// ext2; FAT32 and exFAT model no mode at all, which the boot warning already
/// says out loud.
const ID_MODE: u16 = 0o600;

#[link_section = ".text.start"]
#[no_mangle]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut arg0 = [0u8; 32];
    let n = ulib::arg(1, &mut arg0).unwrap_or(0);
    let cmd = &arg0[..n];

    let code = if cmd.is_empty() {
        show(target)
    } else if cmd == b"new" {
        generate(target)
    } else if cmd == b"peers" {
        peers(target)
    } else if cmd == b"line" {
        line(target)
    } else {
        out(target, b"usage: clusterkey [new [-f] | peers | line <name> <ip>]\r\n");
        2
    };
    ulib::end_of_stream(target);
    ulib::exit(code);
}

/// Print this machine's public key.
fn show(target: u64) -> u64 {
    let mut buf = [0u8; 128];
    match read_file(PUB_PATH, &mut buf) {
        Some(n) => match clusterkeys::parse_key_file(&buf[..n]) {
            Some(key) => {
                let mut hex = [0u8; KEY_HEX_LEN];
                clusterkeys::encode_key(&key, &mut hex);
                out(target, &hex);
                out(target, b"\r\n");
                0
            }
            None => {
                out(target, b"clusterkey: /etc/cluster/id.pub is not a key\r\n");
                1
            }
        },
        None => {
            out(target, b"clusterkey: no identity here yet - run 'clusterkey new'\r\n");
            1
        }
    }
}

/// Generate a keypair. Refuses without real entropy, and refuses to destroy an
/// existing identity unless told twice.
fn generate(target: u64) -> u64 {
    let mut flag = [0u8; 8];
    let fn_ = ulib::arg(2, &mut flag).unwrap_or(0);
    let force = &flag[..fn_] == b"-f";

    // An existing key is this machine's membership of the cluster. Overwriting
    // it silently would cut the machine out of every peer's `authorized` file
    // with no way back, so it takes a second word.
    let mut probe = [0u8; 128];
    if read_file(ID_PATH, &mut probe).is_some() && !force {
        out(target, b"clusterkey: an identity already exists here.\r\n");
        out(target, b"  Replacing it makes every peer's authorized file stale, and\r\n");
        out(target, b"  this machine unreachable until they are all updated.\r\n");
        out(target, b"  Use 'clusterkey new -f' if that is what you want.\r\n");
        return 1;
    }

    // THE REFUSAL. Not a warning, not a weaker key: no entropy, no identity.
    let mut seed = [0u8; KEY_LEN];
    if ulib::random(&mut seed) != KEY_LEN {
        out(target, b"clusterkey: no entropy device, so no key was generated.\r\n");
        out(target, b"  A guessable machine key is worse than none: it looks like security.\r\n");
        out(target, b"  On QEMU add '-device virtio-rng-device'. On hardware without one,\r\n");
        out(target, b"  generate the key elsewhere and copy /etc/cluster/id in.\r\n");
        return 1;
    }

    let public = ed25519::public_key(&seed);
    let mut hex = [0u8; KEY_HEX_LEN + 1];
    clusterkeys::encode_key(&seed, (&mut hex[..KEY_HEX_LEN]).try_into().expect("64 bytes"));
    hex[KEY_HEX_LEN] = b'\n';
    if ulib::is_fs_error(ulib::fs_write_bulk(ID_PATH, &hex)) {
        out(target, b"clusterkey: could not write /etc/cluster/id\r\n");
        return 1;
    }
    // Restrict it immediately. On a filesystem that models no mode this is a
    // no-op, which the boot warning already covers.
    ulib::fs_chmod(ID_PATH, ID_MODE);

    let mut pub_hex = [0u8; KEY_HEX_LEN + 1];
    clusterkeys::encode_key(&public, (&mut pub_hex[..KEY_HEX_LEN]).try_into().expect("64 bytes"));
    pub_hex[KEY_HEX_LEN] = b'\n';
    if ulib::is_fs_error(ulib::fs_write_bulk(PUB_PATH, &pub_hex)) {
        out(target, b"clusterkey: could not write /etc/cluster/id.pub\r\n");
        return 1;
    }

    out(target, b"clusterkey: new identity generated. Public key:\r\n");
    out(target, &pub_hex[..KEY_HEX_LEN]);
    out(target, b"\r\n  Add it to every peer's /etc/cluster/authorized - until then, they\r\n");
    out(target, b"  will refuse this machine, which is the design working.\r\n");
    0
}

/// List the peers this machine accepts, and say so when a line is broken.
fn peers(target: u64) -> u64 {
    let mut buf = [0u8; 2048];
    let Some(n) = read_file(AUTHORIZED_PATH, &mut buf) else {
        out(target, b"clusterkey: no /etc/cluster/authorized - this machine accepts nobody\r\n");
        return 1;
    };
    let mut found = 0u32;
    let mut broken = 0u32;
    let mut lineno = 0u32;
    for raw in buf[..n].split(|&c| c == b'\n') {
        lineno += 1;
        match clusterkeys::classify(raw) {
            clusterkeys::LineKind::Peer(p) => {
                found += 1;
                out(target, p.name);
                out(target, b"  ");
                let mut ip = [0u8; 15];
                let ilen = clusterkeys::format_ip(&p.ip, &mut ip);
                out(target, &ip[..ilen]);
                out(target, b"  ");
                let mut hex = [0u8; KEY_HEX_LEN];
                clusterkeys::encode_key(&p.key, &mut hex);
                out(target, &hex);
                out(target, b"\r\n");
            }
            // The reason `classify` exists. A mistyped key is a peer that is
            // silently unauthorized, in a file that still looks right.
            clusterkeys::LineKind::Malformed => {
                broken += 1;
                out(target, b"  line ");
                put_dec(target, lineno as u64);
                out(target, b": unreadable, so this peer is NOT authorized\r\n");
            }
            _ => {}
        }
    }
    out(target, b"clusterkey: ");
    put_dec(target, found as u64);
    out(target, b" peer(s)");
    if broken > 0 {
        out(target, b", ");
        put_dec(target, broken as u64);
        out(target, b" unreadable line(s)");
    }
    out(target, b"\r\n");
    if broken > 0 {
        1
    } else {
        0
    }
}

/// Print the `authorized` line another machine should hold for this one.
fn line(target: u64) -> u64 {
    let mut name = [0u8; 64];
    let nlen = ulib::arg(2, &mut name).unwrap_or(0);
    let mut ip_text = [0u8; 32];
    let ilen = ulib::arg(3, &mut ip_text).unwrap_or(0);
    if nlen == 0 || ilen == 0 {
        out(target, b"usage: clusterkey line <name> <ip>\r\n");
        return 2;
    }
    let Some(ip) = clusterkeys::parse_ip(&ip_text[..ilen]) else {
        out(target, b"clusterkey: not an IPv4 address\r\n");
        return 2;
    };
    let mut buf = [0u8; 128];
    let Some(n) = read_file(PUB_PATH, &mut buf) else {
        out(target, b"clusterkey: no identity here yet - run 'clusterkey new'\r\n");
        return 1;
    };
    let Some(key) = clusterkeys::parse_key_file(&buf[..n]) else {
        out(target, b"clusterkey: /etc/cluster/id.pub is not a key\r\n");
        return 1;
    };
    let mut line = [0u8; 160];
    match clusterkeys::format_line(&mut line, &name[..nlen], &ip, &key) {
        Some(len) => {
            out(target, &line[..len]);
            out(target, b"\r\n");
            0
        }
        None => {
            out(target, b"clusterkey: that name cannot appear in an authorized file\r\n");
            2
        }
    }
}

/// Read a whole small file, or `None` if it is absent, unreadable or empty.
///
/// Deliberately NOT `ulib::read_file_all`, which returns a `usize` with any
/// failure folded into `0`. That conflation is what once made an oversized
/// `/etc/shadow` read as "no secret" and locked every account out, root
/// included (see docs/review-and-split-postmortem.md). Here the three cases -
/// absent, unreadable, empty - all mean "no usable key", so folding them would
/// be harmless *today*; using the call that can tell them apart costs nothing
/// and does not have to be re-reasoned about when a caller wants the difference.
fn read_file(path: &str, buf: &mut [u8]) -> Option<usize> {
    let r = ulib::fs_read_bulk(path, 0, buf);
    if ulib::is_fs_error(r) || r == 0 {
        return None;
    }
    Some((r as usize).min(buf.len()))
}

fn out(target: u64, bytes: &[u8]) {
    ulib::write_out(target, bytes);
}

fn put_dec(target: u64, v: u64) {
    let mut buf = [0u8; 32];
    let mut n = 0usize;
    ulib::emit_dec(&mut buf, &mut n, v);
    out(target, &buf[..n]);
}

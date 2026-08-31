//! `clusterkeys` — the on-disk format for per-machine cluster identity.
//!
//! Step 6a of `docs/roadmap-cluster-keys.md`. Pure: parsing and formatting only,
//! no I/O and no syscalls, so it is host-testable like `accounts` and `regex`.
//! Callers do their own file reading and hand byte buffers in.
//!
//! Three files, all under `/etc/cluster`:
//!
//! | file | mode | contents |
//! |---|---|---|
//! | `id` | 0600 | this machine's **private** key, 64 hex characters |
//! | `id.pub` | 0644 | this machine's public key, 64 hex characters |
//! | `authorized` | 0644 | one line per peer: `<name> <ipv4> <pubkey-hex>` |
//!
//! ## Why `authorized` carries an address as well as a key
//!
//! The two directions of the protocol ask different questions, and one file has
//! to answer both (see Decision 3 in the design doc):
//!
//! - An **exporter** verifying a client looks the peer up **by key** — the
//!   client offers its public key in the frame, and the only question is whether
//!   this machine accepts it.
//! - A **client** verifying an exporter's signed reply must look up **by
//!   address**, because it needs the key it *expects for the host it dialled*.
//!   Accepting any authorized key there would authenticate "some cluster member"
//!   rather than "the machine I asked", which is a much weaker claim.
//!
//! ## Everything here works in bytes
//!
//! No `&str`, no runtime string slicing. Slicing a `&str` by a runtime index
//! pulls in `core::fmt`'s char-boundary panic path, which emits
//! `R_AARCH64_ABS64` relocations this project's loader cannot process — a trap
//! this codebase has hit repeatedly. See `docs/processes.md`.

#![no_std]

/// Bytes in an Ed25519 public or private key.
pub const KEY_LEN: usize = 32;
/// Characters in a hex-encoded key.
pub const KEY_HEX_LEN: usize = KEY_LEN * 2;
/// Longest peer name accepted. Matches the cluster's user-name field, so the
/// two are not gratuitously different sizes.
pub const NAME_MAX: usize = 32;

/// One authorized peer.
#[derive(Clone, Copy)]
pub struct Peer<'a> {
    /// The machine's name, for diagnostics — never the thing authorized.
    pub name: &'a [u8],
    /// The address this peer is expected at, used when verifying a reply from
    /// a host we dialled.
    pub ip: [u8; 4],
    /// The public key, which is what actually grants access.
    pub key: [u8; KEY_LEN],
}

/// Decode exactly `KEY_HEX_LEN` hex characters into a key.
///
/// Rejects anything shorter, longer, or not hex — a truncated key that decoded
/// to a *prefix* would be a different key that might well be authorized.
pub fn decode_key(hex: &[u8]) -> Option<[u8; KEY_LEN]> {
    if hex.len() != KEY_HEX_LEN {
        return None;
    }
    let mut out = [0u8; KEY_LEN];
    for i in 0..KEY_LEN {
        let hi = hex_digit(hex[i * 2])?;
        let lo = hex_digit(hex[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

/// Hex-encode a key (lowercase) into `out`.
pub fn encode_key(key: &[u8; KEY_LEN], out: &mut [u8; KEY_HEX_LEN]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for i in 0..KEY_LEN {
        out[i * 2] = HEX[(key[i] >> 4) as usize];
        out[i * 2 + 1] = HEX[(key[i] & 0x0f) as usize];
    }
}

fn hex_digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Parse a dotted-quad IPv4 address.
pub fn parse_ip(s: &[u8]) -> Option<[u8; 4]> {
    let mut out = [0u8; 4];
    let mut field = 0usize;
    let mut value: u32 = 0;
    let mut digits = 0usize;
    for &c in s {
        match c {
            b'0'..=b'9' => {
                value = value * 10 + (c - b'0') as u32;
                digits += 1;
                if value > 255 || digits > 3 {
                    return None;
                }
            }
            b'.' => {
                if digits == 0 || field >= 3 {
                    return None;
                }
                out[field] = value as u8;
                field += 1;
                value = 0;
                digits = 0;
            }
            _ => return None,
        }
    }
    if field != 3 || digits == 0 {
        return None;
    }
    out[3] = value as u8;
    Some(out)
}

/// Format a dotted-quad address into `out`, returning its length.
pub fn format_ip(ip: &[u8; 4], out: &mut [u8; 15]) -> usize {
    let mut n = 0;
    for (i, octet) in ip.iter().enumerate() {
        if i > 0 {
            out[n] = b'.';
            n += 1;
        }
        let v = *octet;
        if v >= 100 {
            out[n] = b'0' + v / 100;
            n += 1;
        }
        if v >= 10 {
            out[n] = b'0' + (v / 10) % 10;
            n += 1;
        }
        out[n] = b'0' + v % 10;
        n += 1;
    }
    n
}

/// Parse one line of `authorized`.
///
/// Returns `None` for a blank line, a `#` comment, or anything malformed. A
/// malformed line is **skipped, never guessed at**: a line this cannot read is
/// one whose meaning is unknown, and inventing a peer from it is the one outcome
/// worse than ignoring it.
pub fn parse_line(line: &[u8]) -> Option<Peer<'_>> {
    let line = trim(line);
    if line.is_empty() || line[0] == b'#' {
        return None;
    }
    let (name, rest) = split_field(line)?;
    let (ip_text, rest) = split_field(rest)?;
    let (key_text, rest) = split_field(rest)?;
    // A trailing `# note` is accepted - it is how people annotate this kind of
    // file, and rejecting it would silently unauthorize a peer whose line still
    // looks correct. Anything else trailing means the line is not what this
    // parser thinks it is, and a line whose meaning is unclear must not become a
    // peer.
    let rest = trim(rest);
    if !rest.is_empty() && rest[0] != b'#' {
        return None;
    }
    if name.is_empty() || name.len() > NAME_MAX {
        return None;
    }
    Some(Peer { name, ip: parse_ip(ip_text)?, key: decode_key(key_text)? })
}

/// The peer whose **public key** matches — what an exporter asks when a client
/// offers a key.
pub fn find_by_key<'a>(authorized: &'a [u8], key: &[u8; KEY_LEN]) -> Option<Peer<'a>> {
    for line in authorized.split(|&c| c == b'\n') {
        if let Some(p) = parse_line(line) {
            if &p.key == key {
                return Some(p);
            }
        }
    }
    None
}

/// The peer expected at an **address** — what a client asks before trusting a
/// reply from the host it dialled.
pub fn find_by_ip<'a>(authorized: &'a [u8], ip: &[u8; 4]) -> Option<Peer<'a>> {
    for line in authorized.split(|&c| c == b'\n') {
        if let Some(p) = parse_line(line) {
            if &p.ip == ip {
                return Some(p);
            }
        }
    }
    None
}

/// Format an `authorized` line (without a trailing newline) into `out`,
/// returning its length.
pub fn format_line(
    out: &mut [u8],
    name: &[u8],
    ip: &[u8; 4],
    key: &[u8; KEY_LEN],
) -> Option<usize> {
    // A name that would make the line unreadable is refused rather than written.
    // A leading `#` is the subtle one: the line would look authorized to anyone
    // reading the file and be skipped as a comment by every lookup.
    if name.is_empty() || name.len() > NAME_MAX || contains_space(name) || name[0] == b'#' {
        return None;
    }
    let mut ip_buf = [0u8; 15];
    let ip_len = format_ip(ip, &mut ip_buf);
    let mut key_buf = [0u8; KEY_HEX_LEN];
    encode_key(key, &mut key_buf);
    let total = name.len() + 1 + ip_len + 1 + KEY_HEX_LEN;
    if out.len() < total {
        return None;
    }
    let mut n = 0;
    out[n..n + name.len()].copy_from_slice(name);
    n += name.len();
    out[n] = b' ';
    n += 1;
    out[n..n + ip_len].copy_from_slice(&ip_buf[..ip_len]);
    n += ip_len;
    out[n] = b' ';
    n += 1;
    out[n..n + KEY_HEX_LEN].copy_from_slice(&key_buf);
    Some(total)
}

/// Read a key file (`id` or `id.pub`): 64 hex characters, with any surrounding
/// whitespace ignored.
pub fn parse_key_file(contents: &[u8]) -> Option<[u8; KEY_LEN]> {
    decode_key(trim(contents))
}

fn contains_space(s: &[u8]) -> bool {
    s.iter().any(|&c| c == b' ' || c == b'\t' || c == b'\r' || c == b'\n')
}

fn trim(s: &[u8]) -> &[u8] {
    let mut a = 0;
    let mut b = s.len();
    while a < b && is_space(s[a]) {
        a += 1;
    }
    while b > a && is_space(s[b - 1]) {
        b -= 1;
    }
    &s[a..b]
}

/// Whitespace for this format — **including NUL**.
///
/// A consumer reads `authorized` into a fixed zero-initialised buffer, and if it
/// passes the whole buffer rather than just the bytes read, the trailing NUL run
/// would otherwise become a line of its own. That line is skipped as malformed,
/// which sounds harmless — except that without a trailing newline the NULs join
/// the LAST peer's line and take it with them. For a one-line file that is every
/// peer, silently. It fails closed, so it is availability rather than
/// authorization, but it is invisible, and treating NUL as whitespace removes
/// the whole class.
fn is_space(c: u8) -> bool {
    c == b' ' || c == b'\t' || c == b'\r' || c == b'\n' || c == 0
}

/// Split off the first whitespace-delimited field, returning it and the rest.
fn split_field(s: &[u8]) -> Option<(&[u8], &[u8])> {
    let s = trim_start(s);
    if s.is_empty() {
        return None;
    }
    let end = s.iter().position(|&c| is_space(c)).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

fn trim_start(s: &[u8]) -> &[u8] {
    let mut a = 0;
    while a < s.len() && is_space(s[a]) {
        a += 1;
    }
    &s[a..]
}

#[cfg(test)]
mod tests {
    //! Run with `make test`.
    //!
    //! This file's job is to decide **who may talk to this machine**, so the
    //! tests lean on what must be REFUSED at least as hard as what must parse.
    //! A permissive parser here is an authorization bug, not a formatting one.
    use super::*;

    const KEY_A: [u8; KEY_LEN] = [
        0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07,
        0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07,
        0x51, 0x1a,
    ];
    const KEY_A_HEX: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
    const KEY_B: [u8; KEY_LEN] = [
        0x3d, 0x40, 0x17, 0xc3, 0xe8, 0x43, 0x89, 0x5a, 0x92, 0xb7, 0x0a, 0xa7, 0x4d, 0x1b, 0x7e,
        0xbc, 0x9c, 0x98, 0x2c, 0xcf, 0x2e, 0xc4, 0x96, 0x8c, 0xc0, 0xcd, 0x55, 0xf1, 0x2a, 0xf4,
        0x66, 0x0c,
    ];
    const KEY_B_HEX: &str = "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c";

    /// A realistic file, including the shapes a hand-edited one grows: a comment,
    /// a blank line, indentation, and a trailing newline.
    fn sample() -> [u8; 256] {
        // Padded with NUL, not newlines. A consumer reads this file into a
        // zero-initialised buffer, so NUL padding is the realistic fixture - and
        // newline padding is the one byte that would hide a parser which cannot
        // cope with it.
        let mut buf = [0u8; 256];
        let text = b"# the cluster\nnode-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a\n\n  node-b 10.0.2.11 3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c  \n";
        buf[..text.len()].copy_from_slice(text);
        buf
    }

    #[test]
    fn finds_a_peer_by_key() {
        // What an EXPORTER asks: a client offered this key - do we accept it?
        let f = sample();
        let p = find_by_key(&f, &KEY_A).expect("node-a by key");
        assert_eq!(p.name, b"node-a");
        assert_eq!(p.ip, [10, 0, 2, 10]);
        let p = find_by_key(&f, &KEY_B).expect("node-b by key");
        assert_eq!(p.name, b"node-b");
        assert_eq!(p.ip, [10, 0, 2, 11]);
    }

    #[test]
    fn finds_a_peer_by_address() {
        // What a CLIENT asks: I dialled this address - which key must its reply
        // be signed with? Answering with "any authorized key" would be a much
        // weaker check, which is why this lookup exists separately.
        let f = sample();
        assert_eq!(find_by_ip(&f, &[10, 0, 2, 10]).expect("by ip").key, KEY_A);
        assert_eq!(find_by_ip(&f, &[10, 0, 2, 11]).expect("by ip").key, KEY_B);
    }

    #[test]
    fn an_unknown_key_or_address_is_not_found() {
        let f = sample();
        let mut stranger = KEY_A;
        stranger[0] ^= 1; // one bit different
        assert!(find_by_key(&f, &stranger).is_none(), "a near-miss key must not match");
        assert!(find_by_ip(&f, &[10, 0, 2, 99]).is_none());
    }

    #[test]
    fn revocation_is_deleting_a_line() {
        // The operational promise of the format.
        let f = sample();
        assert!(find_by_key(&f, &KEY_B).is_some());
        let text = b"# the cluster\nnode-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a\n";
        let mut without = [0u8; 256];
        without[..text.len()].copy_from_slice(text);
        assert!(find_by_key(&without, &KEY_A).is_some(), "node-a still authorized");
        assert!(find_by_key(&without, &KEY_B).is_none(), "node-b revoked");
    }

    #[test]
    fn malformed_lines_are_skipped_not_guessed_at() {
        // A line whose meaning is unknown must not become a peer. Each of these
        // is a plausible hand-editing mistake.
        let bad: &[&[u8]] = &[
            b"node-a 10.0.2.10",                                  // no key
            b"node-a",                                            // no address
            b"",                                                  // blank
            b"# node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", // commented out
            // Commented out WITHOUT a space, which is how a person actually
            // types it when revoking a peer in a hurry. Mutation testing found
            // this: with the `#` check removed, the spaced form above still
            // failed to parse (its fields shift), so nothing noticed - while
            // this form parsed cleanly as a peer named "#node-a" holding a VALID
            // KEY. Commenting a line out has to revoke it.
            b"#node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            b"   #node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f70751", // key too short
            b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a00", // key too long
            b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f70751zz", // not hex
            b"node-a 10.0.2.256 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", // octet out of range
            b"node-a 10.0.2 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", // short address
            b"node-a 10.0.2.10.5 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", // long address
            b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a extra", // trailing field
        ];
        for line in bad {
            assert!(parse_line(line).is_none(), "must not parse: {:?}", core::str::from_utf8(line));
        }
    }

    #[test]
    fn commenting_a_line_out_revokes_it() {
        // The other way an operator revokes a peer, and it must work with or
        // without a space after the `#`.
        for text in [
            &b"#node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a\n"[..],
            &b"# node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a\n"[..],
            &b"  #node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a\n"[..],
        ] {
            let mut buf = [0u8; 256];
            buf[..text.len()].copy_from_slice(text);
            assert!(find_by_key(&buf, &KEY_A).is_none(), "a commented-out peer is revoked");
            assert!(find_by_ip(&buf, &[10, 0, 2, 10]).is_none());
        }
    }

    #[test]
    fn a_zero_padded_buffer_does_not_swallow_the_last_peer() {
        // The realistic consumer mistake: read the file into a fixed buffer and
        // pass the WHOLE buffer. Without a trailing newline the NUL run joins the
        // last line; for a one-line file that is every peer, silently.
        let text = b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
        let mut buf = [0u8; 256];
        buf[..text.len()].copy_from_slice(text);
        assert!(find_by_key(&buf, &KEY_A).is_some(), "no trailing newline must still parse");
        assert!(find_by_ip(&buf, &[10, 0, 2, 10]).is_some());
    }

    #[test]
    fn a_trailing_comment_is_allowed() {
        // How people annotate a file like this. Rejecting it would silently
        // unauthorize a peer whose line still looks perfectly correct.
        let text = b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a  # primary\n";
        let mut buf = [0u8; 256];
        buf[..text.len()].copy_from_slice(text);
        let p = find_by_key(&buf, &KEY_A).expect("annotated line must still authorize");
        assert_eq!(p.name, b"node-a");
        // But a trailing field that is NOT a comment is still refused.
        let bad = b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a rubbish";
        assert!(parse_line(bad).is_none());
    }

    #[test]
    fn format_line_refuses_a_name_that_would_read_as_a_comment() {
        // format/parse must agree: writing `#node-a` would produce a line that
        // looks authorized in the file and is skipped by every lookup.
        let mut buf = [0u8; 160];
        assert!(format_line(&mut buf, b"#node-a", &[10, 0, 2, 10], &KEY_A).is_none());
    }

    #[test]
    fn a_truncated_key_never_matches_a_prefix() {
        // The dangerous shape: a short key that decoded to a PREFIX would be a
        // different key, and might be one that is authorized.
        assert!(decode_key(&KEY_A_HEX.as_bytes()[..62]).is_none());
        assert!(decode_key(b"").is_none());
        let mut long = [b'0'; KEY_HEX_LEN + 2];
        long[..KEY_HEX_LEN].copy_from_slice(KEY_A_HEX.as_bytes());
        assert!(decode_key(&long).is_none());
    }

    #[test]
    fn hex_round_trips_both_ways() {
        assert_eq!(decode_key(KEY_A_HEX.as_bytes()).expect("decode"), KEY_A);
        assert_eq!(decode_key(KEY_B_HEX.as_bytes()).expect("decode"), KEY_B);
        let mut b_out = [0u8; KEY_HEX_LEN];
        encode_key(&KEY_B, &mut b_out);
        assert_eq!(&b_out, KEY_B_HEX.as_bytes());
        let mut out = [0u8; KEY_HEX_LEN];
        encode_key(&KEY_A, &mut out);
        assert_eq!(&out, KEY_A_HEX.as_bytes());
        // Uppercase input decodes; output is always lowercase, so a file this
        // crate writes is stable byte-for-byte.
        let upper: [u8; KEY_HEX_LEN] = {
            let mut u = [0u8; KEY_HEX_LEN];
            for (i, c) in KEY_A_HEX.as_bytes().iter().enumerate() {
                u[i] = c.to_ascii_uppercase();
            }
            u
        };
        assert_eq!(decode_key(&upper).expect("uppercase decodes"), KEY_A);
    }

    #[test]
    fn formatting_a_line_produces_one_this_parses() {
        let mut buf = [0u8; 160];
        let n = format_line(&mut buf, b"node-c", &[192, 168, 1, 7], &KEY_B).expect("format");
        let p = parse_line(&buf[..n]).expect("the line we just wrote must parse");
        assert_eq!(p.name, b"node-c");
        assert_eq!(p.ip, [192, 168, 1, 7]);
        assert_eq!(p.key, KEY_B);
    }

    #[test]
    fn formatting_refuses_a_name_that_would_break_the_format() {
        let mut buf = [0u8; 160];
        // A space in a name would silently shift every field on re-read.
        assert!(format_line(&mut buf, b"node a", &[10, 0, 0, 1], &KEY_A).is_none());
        assert!(format_line(&mut buf, b"", &[10, 0, 0, 1], &KEY_A).is_none());
        let long = [b'x'; NAME_MAX + 1];
        assert!(format_line(&mut buf, &long, &[10, 0, 0, 1], &KEY_A).is_none());
        // And a buffer too small must refuse rather than truncate.
        let mut tiny = [0u8; 8];
        assert!(format_line(&mut tiny, b"node-c", &[10, 0, 0, 1], &KEY_A).is_none());
    }

    #[test]
    fn addresses_round_trip() {
        for ip in [[0u8, 0, 0, 0], [10, 0, 2, 15], [192, 168, 1, 254], [255, 255, 255, 255]] {
            let mut buf = [0u8; 15];
            let n = format_ip(&ip, &mut buf);
            assert_eq!(parse_ip(&buf[..n]).expect("round trip"), ip, "for {ip:?}");
        }
    }

    #[test]
    fn key_files_parse_with_or_without_a_trailing_newline() {
        // A file written by a shell redirect, an editor, or the host generator
        // will differ in exactly this way.
        assert_eq!(parse_key_file(KEY_A_HEX.as_bytes()).expect("bare"), KEY_A);
        let mut nl = [0u8; KEY_HEX_LEN + 1];
        nl[..KEY_HEX_LEN].copy_from_slice(KEY_A_HEX.as_bytes());
        nl[KEY_HEX_LEN] = b'\n';
        assert_eq!(parse_key_file(&nl).expect("newline"), KEY_A);
        assert!(parse_key_file(b"not a key").is_none());
        assert!(parse_key_file(b"").is_none());
    }

    #[test]
    fn the_first_matching_line_wins_and_later_duplicates_do_not_override() {
        // A duplicate is a hand-editing mistake; the behaviour just has to be
        // defined and documented rather than surprising.
        let text = b"node-a 10.0.2.10 d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a\nnode-dup 10.0.2.10 3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c\n";
        let mut buf = [0u8; 300];
        buf[..text.len()].copy_from_slice(text);
        assert_eq!(find_by_ip(&buf, &[10, 0, 2, 10]).expect("by ip").name, b"node-a");
    }
}

#[cfg(test)]
mod generator_agreement {
    //! **The Python generator writes these files; this crate reads them.** Two
    //! implementations, one format, and nothing but a test between them.
    //!
    //! The fixtures below are `scripts/mkclusterkeys.py`'s real output, pasted
    //! verbatim — not a hand-written approximation of it, which would test only
    //! that this parser agrees with my idea of the generator. Regenerate with:
    //!
    //! ```text
    //! python3 scripts/mkclusterkeys.py <dir> node-a
    //! ```
    //!
    //! If the generator's format ever drifts, this fails on the host rather than
    //! becoming a guest that silently authorizes nobody.
    use super::*;

    const GENERATED_AUTHORIZED: &str = "# Peers this machine accepts. One line per peer:\n#   <name> <ipv4> <public-key-hex>\n# Delete or comment out a line to revoke that peer.\n# DEV KEYS: derived from fixed seeds, so they are public. Not for real use.\nnode-a 10.0.2.10 de5317f86f9d763d9fc5c4589a85dda15d136d5c31d4c7d6bba980dfca37d4e6\nnode-b 10.0.2.11 e9e1630da4a29b961703d42eb3300448078ff0f1c1c7d37dde269f132d53b81d\nhost 10.0.2.2 3e71226a69d738c5921acfcad1e823d28c1d3e0b047b0af245b220b436bee964\n";
    const GENERATED_ID: &str = "92c8e58c772b748688638ab47c61185a8aaa12ff690c61caeab9e98ca44f9e9f\n";
    const GENERATED_ID_PUB: &str = "de5317f86f9d763d9fc5c4589a85dda15d136d5c31d4c7d6bba980dfca37d4e6\n";

    #[test]
    fn every_generated_peer_parses() {
        let bytes = GENERATED_AUTHORIZED.as_bytes();
        let mut found = 0;
        for line in bytes.split(|&c| c == b'\n') {
            if parse_line(line).is_some() {
                found += 1;
            }
        }
        assert_eq!(found, 3, "the generator writes three dev peers; parsed {found}");
    }

    #[test]
    fn the_generated_peers_are_findable_by_key_and_address() {
        let bytes = GENERATED_AUTHORIZED.as_bytes();
        for (name, ip) in [
            (&b"node-a"[..], [10u8, 0, 2, 10]),
            (&b"node-b"[..], [10, 0, 2, 11]),
            (&b"host"[..], [10, 0, 2, 2]),
        ] {
            let by_ip = find_by_ip(bytes, &ip).unwrap_or_else(|| panic!("{name:?} by address"));
            assert_eq!(by_ip.name, name);
            let by_key = find_by_key(bytes, &by_ip.key).expect("and back by its key");
            assert_eq!(by_key.name, name);
        }
    }

    #[test]
    fn the_generated_key_files_parse_and_correspond() {
        // `id` is the private seed and `id.pub` the public key; this crate does
        // not do curve maths, so what it can check is that both are well-formed
        // and that the public one is the peer entry for this node.
        let secret = parse_key_file(GENERATED_ID.as_bytes()).expect("id parses");
        let public = parse_key_file(GENERATED_ID_PUB.as_bytes()).expect("id.pub parses");
        assert_ne!(secret, public, "the private and public halves must differ");
        let me = find_by_key(GENERATED_AUTHORIZED.as_bytes(), &public)
            .expect("this node's own public key must appear in its authorized file");
        assert_eq!(me.name, b"node-a");
    }

    #[test]
    fn the_generators_comment_header_is_skipped_not_parsed() {
        // The file opens with four comment lines. If any became a peer, the
        // count in `every_generated_peer_parses` would be wrong - but check the
        // intent directly too, since that is the property that matters.
        let first = GENERATED_AUTHORIZED.as_bytes().split(|&c| c == b'\n').next().expect("a line");
        assert!(first.starts_with(b"#"));
        assert!(parse_line(first).is_none());
    }
}

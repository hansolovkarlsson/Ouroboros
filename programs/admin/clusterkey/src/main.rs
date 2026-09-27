//! `/bin/clusterkey` — this machine's cluster identity.
//!
//! Step 6c of `docs/roadmap/roadmap-cluster-keys.md`. Four things an operator needs:
//!
//! ```text
//! clusterkey              show this machine's public key
//! clusterkey new [-f]     generate a keypair (refuses without real entropy)
//! clusterkey peers        list the peers this machine accepts
//! clusterkey line N IP    print the authorized line to paste on another machine
//!                         (never root-flagged: that is the other machine's call)
//! clusterkey realm        show the cluster's realm
//! clusterkey realm new [-f]  generate a realm (refuses without real entropy)
//! clusterkey held         the uids netd holds a user key for (root only; a diagnostic)
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
const REALM_PATH: &str = "/etc/cluster/realm";

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
    } else if cmd == b"realm" {
        realm(target)
    } else if cmd == b"held" {
        held(target)
    } else {
        out(target, b"usage: clusterkey [new [-f] | peers | line <name> <ip> | realm [new [-f]] | held]\r\n");
        2
    };
    ulib::end_of_stream(target);
    ulib::exit(code);
}

/// Print this machine's public key.
fn show(target: u64) -> u64 {
    match public_key_of_this_machine(target) {
        Some(key) => {
            let mut hex = [0u8; KEY_HEX_LEN];
            clusterkeys::encode_key(&key, &mut hex);
            out(target, &hex);
            out(target, b"\r\n");
            0
        }
        None => {
            out(target, b"clusterkey: no identity here yet - run 'clusterkey new',\r\n");
            out(target, b"  or copy a key into /etc/cluster/id from a machine with entropy.\r\n");
            1
        }
    }
}

/// Generate a keypair. Refuses without real entropy, and refuses to destroy an
/// existing identity unless told twice.
fn generate(target: u64) -> u64 {
    // ROOT ONLY, like every other tool in programs/admin. On ext2 fsd would
    // refuse the write anyway, but FAT32 and exFAT model no mode at all - so
    // without this any logged-in user on those rigs could irreversibly replace
    // the machine's cluster identity.
    if ulib::getuid() != 0 {
        out(target, b"clusterkey: only root may change this machine's identity\r\n");
        return 1;
    }
    let mut flag = [0u8; 8];
    let fn_ = ulib::arg(2, &mut flag).unwrap_or(0);
    let force = &flag[..fn_] == b"-f";

    // An existing key is this machine's membership of the cluster. Overwriting
    // it silently would cut the machine out of every peer's `authorized` file
    // with no way back, so it takes a second word.
    let mut probe = [0u8; 128];
    match read_file(ID_PATH, &mut probe) {
        FileRead::Absent => {}
        FileRead::Bytes(_) if force => {}
        FileRead::Bytes(_) => {
            out(target, b"clusterkey: an identity already exists here.\r\n");
            out(target, b"  Replacing it makes every peer's authorized file stale, and\r\n");
            out(target, b"  this machine unreachable until they are all updated.\r\n");
            out(target, b"  Use 'clusterkey new -f' if that is what you want.\r\n");
            return 1;
        }
        // FAILS CLOSED. "I could not read id" is not "there is no id", and the
        // difference matters when the alternative is destroying a key with no
        // way back. -f does not override this: the operator asked to replace a
        // key, not to act blind.
        FileRead::Unreadable => {
            out(target, b"clusterkey: cannot tell whether an identity already exists.\r\n");
            out(target, b"  /etc/cluster/id is unreadable rather than absent, so refusing to\r\n");
            out(target, b"  overwrite what may be there. Check the disk is mounted.\r\n");
            return 1;
        }
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

    let mut failed_mode = false;
    let public = ed25519::public_key(&seed);
    let mut hex = [0u8; KEY_HEX_LEN + 1];
    clusterkeys::encode_key(&seed, (&mut hex[..KEY_HEX_LEN]).try_into().expect("64 bytes"));
    hex[KEY_HEX_LEN] = b'\n';
    if ulib::is_fs_error(ulib::fs_write_bulk(ID_PATH, &hex)) {
        out(target, b"clusterkey: could not write /etc/cluster/id\r\n");
        return 1;
    }
    // Restrict it immediately, AND CHECK. On ext2 a newly created file is 0644
    // (fsd's NEW_FILE_MODE), so this chmod is the only thing standing between a
    // private key and every user on the machine. On FAT32/exFAT it is a no-op
    // that reports FS_ERR_NOT_SUPPORTED, which is expected and not a failure -
    // those filesystems model no mode, as the boot warning says.
    let chmod = ulib::fs_chmod(ID_PATH, ID_MODE);
    if ulib::is_fs_error(chmod) && chmod != syscall_abi::FS_ERR_NOT_SUPPORTED {
        out(target, b"clusterkey: WARNING could not restrict /etc/cluster/id to 0600.\r\n");
        out(target, b"  The private key may be readable by other users on this machine.\r\n");
        out(target, b"  Fix with 'chmod 600 /etc/cluster/id' before relying on it.\r\n");
        failed_mode = true;
    }

    let mut pub_hex = [0u8; KEY_HEX_LEN + 1];
    clusterkeys::encode_key(&public, (&mut pub_hex[..KEY_HEX_LEN]).try_into().expect("64 bytes"));
    pub_hex[KEY_HEX_LEN] = b'\n';
    if ulib::is_fs_error(ulib::fs_write_bulk(PUB_PATH, &pub_hex)) {
        // NOT a broken identity: `id` is the source of truth and the public half
        // is derived from it, so a stale or missing id.pub is a cache problem
        // rather than a key mismatch - `clusterkey` will derive the right answer
        // and say the two disagree. Worth reporting all the same.
        out(target, b"clusterkey: WARNING could not write /etc/cluster/id.pub.\r\n");
        out(target, b"  The identity itself is fine - id is authoritative and the public\r\n");
        out(target, b"  half is derived from it - but the cached copy is stale.\r\n");
        failed_mode = true;
    }

    out(target, b"clusterkey: new identity generated. Public key:\r\n");
    out(target, &pub_hex[..KEY_HEX_LEN]);
    out(target, b"\r\n  Add it to every peer's /etc/cluster/authorized - until then, they\r\n");
    out(target, b"  will refuse this machine, which is the design working.\r\n");
    if failed_mode {
        1
    } else {
        0
    }
}

/// Show the realm, or with `new [-f]` generate one.
///
/// The realm is the cluster's half of every user key's salt
/// (docs/roadmap/roadmap-user-keys.md, Decision 2), the same on every node of
/// a cluster and different from every other cluster's, since a dictionary
/// precomputed for one realm serves every node in it. So it is generated from
/// the entropy device and COPIED to the other nodes, never typed in, and
/// generation refuses without entropy exactly as `new` does.
fn realm(target: u64) -> u64 {
    let mut word = [0u8; 8];
    let wn = ulib::arg(2, &mut word).unwrap_or(0);
    // REALM_FILE_MAX + 1, the size every reader uses, so a longer file is
    // refused here exactly as `login` will refuse it (see parse_realm).
    let mut buf = [0u8; clusterkeys::REALM_FILE_MAX + 1];
    let current = read_realm_file(&mut buf);
    if wn == 0 {
        return match current {
            FileRead::Bytes(n) => match clusterkeys::parse_realm(&buf[..n]) {
                Some(r) => {
                    out(target, r);
                    out(target, b"\r\n");
                    0
                }
                None => {
                    out(target, b"clusterkey: /etc/cluster/realm is not a realm (one word, at most 32\r\n");
                    out(target, b"  printable bytes; empty counts as none), so this machine derives\r\n");
                    out(target, b"  no user keys. 'clusterkey realm new -f' replaces it.\r\n");
                    1
                }
            },
            FileRead::Absent => {
                out(target, b"clusterkey: no realm here - this machine derives no user keys.\r\n");
                out(target, b"  Copy /etc/cluster/realm from another node of the cluster, or\r\n");
                out(target, b"  start a cluster with 'clusterkey realm new'.\r\n");
                1
            }
            FileRead::Unreadable => {
                out(target, b"clusterkey: /etc/cluster/realm is unreadable\r\n");
                1
            }
        };
    }
    if &word[..wn] != b"new" {
        out(target, b"usage: clusterkey realm [new [-f]]\r\n");
        return 2;
    }
    if ulib::getuid() != 0 {
        out(target, b"clusterkey: only root may change this machine's realm\r\n");
        return 1;
    }
    let mut flag = [0u8; 8];
    let fn_ = ulib::arg(3, &mut flag).unwrap_or(0);
    let force = &flag[..fn_] == b"-f";
    match current {
        FileRead::Absent => {}
        FileRead::Bytes(_) if force => {}
        FileRead::Bytes(_) => {
            out(target, b"clusterkey: a realm already exists here.\r\n");
            out(target, b"  Replacing it changes EVERY user's cluster key on this node, and\r\n");
            out(target, b"  every node of the cluster must be given the new one.\r\n");
            out(target, b"  Use 'clusterkey realm new -f' if that is what you want.\r\n");
            return 1;
        }
        // Fails closed, as `new` does: unreadable is not absent.
        FileRead::Unreadable => {
            out(target, b"clusterkey: cannot tell whether a realm already exists.\r\n");
            out(target, b"  /etc/cluster/realm is unreadable rather than absent; refusing.\r\n");
            return 1;
        }
    }
    let mut raw = [0u8; 16];
    if ulib::random(&mut raw) != raw.len() {
        out(target, b"clusterkey: no entropy device, so no realm was generated.\r\n");
        out(target, b"  A guessable realm lets one precomputed dictionary serve the cluster.\r\n");
        out(target, b"  On QEMU add '-device virtio-rng-device'. On hardware without one,\r\n");
        out(target, b"  generate it elsewhere and copy /etc/cluster/realm in.\r\n");
        return 1;
    }
    // 16 random bytes as 32 hex digits: exactly REALM_MAX, all printable.
    let mut text = [0u8; 33];
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, b) in raw.iter().enumerate() {
        text[i * 2] = HEX[(b >> 4) as usize];
        text[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    text[32] = b'\n';
    if clusterkeys::parse_realm(&text).is_none() {
        out(target, b"clusterkey: generated realm does not parse (a bug); nothing written\r\n");
        return 1;
    }
    if ulib::is_fs_error(ulib::fs_write_bulk(REALM_PATH, &text)) {
        out(target, b"clusterkey: could not write /etc/cluster/realm\r\n");
        return 1;
    }
    out(target, b"clusterkey: new realm generated:\r\n");
    out(target, &text[..32]);
    out(target, b"\r\n  Copy /etc/cluster/realm to every node of this cluster. A node with\r\n");
    out(target, b"  another realm derives different user keys and its users' claims fail.\r\n");
    0
}

/// List the uids `netd` holds a user key for (`NETOP_KEY_LIST`): the check that
/// a login held one and a logout dropped it (step 4 of
/// docs/roadmap/roadmap-user-keys.md). `netd` answers root only and names
/// uids, never keys.
fn held(target: u64) -> u64 {
    let req = syscall_abi::NETOP_KEY_LIST.to_le_bytes();
    // MSG_MAX_LEN, as MSG_CALL requires of every reply buffer.
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    // ulib::net_call, which rides out the window between the shell's spawn and
    // its TO_NET delegation, as every spawned command's call to netd must.
    let packed = ulib::net_call(&req, &mut reply);
    let len = (packed & 0xffff_ffff) as usize;
    if packed >= syscall_abi::FS_ERR_MIN || len < 8 {
        out(target, b"clusterkey: the network server did not answer\r\n");
        return 1;
    }
    let word = |i: usize| u64::from_le_bytes(reply[i..i + 8].try_into().unwrap_or([0; 8]));
    if word(0) == syscall_abi::NET_KEY_DENIED {
        out(target, b"clusterkey: only root may list held keys\r\n");
        return 1;
    }
    if word(0) != syscall_abi::NET_KEY_OK || len < 16 {
        out(target, b"clusterkey: the network server refused the request\r\n");
        return 1;
    }
    let n = (word(8) as usize).min(syscall_abi::NET_KEY_MAX);
    out(target, b"held keys: ");
    put_dec(target, n as u64);
    for i in 0..n {
        if 16 + i * 8 + 8 > len {
            break;
        }
        out(target, if i == 0 { b" (uid " } else { b", uid " });
        put_dec(target, word(16 + i * 8));
    }
    out(target, if n > 0 { b")\r\n" } else { b"\r\n" });
    0
}

/// Read `/etc/cluster/realm`. Unlike [`read_file`], an EMPTY file is its own
/// answer, `Bytes(0)`, which parses as no realm: a truncate-then-write cut
/// short by an `fsd` restart leaves exactly that, and folding it into
/// `Unreadable` made `realm new -f` refuse to replace it for good.
fn read_realm_file(buf: &mut [u8]) -> FileRead {
    let r = ulib::fs_read_bulk(REALM_PATH, 0, buf);
    if r == syscall_abi::FS_ERR_NOT_FOUND {
        FileRead::Absent
    } else if ulib::is_fs_error(r) {
        FileRead::Unreadable
    } else {
        FileRead::Bytes((r as usize).min(buf.len()))
    }
}

/// List the peers this machine accepts, and say so when a line is broken.
fn peers(target: u64) -> u64 {
    // EXACTLY WHAT `netd` AUTHORIZES, from the constant it also sizes from.
    // This was 2048, twice netd's cap, so a peer added past byte 1024 was listed
    // here with the right key and address and refused by the export - and the
    // warning below told the operator it "may still be authorized".
    let mut buf = [0u8; clusterkeys::AUTHORIZED_MAX];
    let n = match read_file(AUTHORIZED_PATH, &mut buf) {
        FileRead::Bytes(n) => n,
        FileRead::Absent => {
            out(target, b"clusterkey: no /etc/cluster/authorized - this machine accepts nobody\r\n");
            return 1;
        }
        FileRead::Unreadable => {
            out(target, b"clusterkey: /etc/cluster/authorized is unreadable\r\n");
            return 1;
        }
    };

    // A FULL BUFFER MEANS THE FILE MAY BE LONGER, and the last line is then
    // probably cut mid-way. Parsing it would report a well-formed peer as
    // "unreadable" - a false alarm from the one diagnostic this command exists
    // to provide. So the tail after the final newline is dropped and the
    // truncation is reported as itself.
    let truncated = n == buf.len();
    let end = if truncated {
        match buf[..n].iter().rposition(|&c| c == b'\n') {
            Some(i) => i + 1,
            None => 0,
        }
    } else {
        n
    };
    let n = end;
    if truncated {
        out(target, b"clusterkey: WARNING the peer list is longer than this machine reads.\r\n");
        out(target, b"  Lines beyond this point are NOT listed and are NOT authorized -\r\n");
        out(target, b"  the export ignores them too. Trim the file.\r\n");
    }
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
                // Shown, because it is the one field that changes what a peer
                // may do rather than whether it may do anything.
                if p.root {
                    out(target, b"  root");
                }
                out(target, b"\r\n");
                // A well-formed line can still carry a key that cannot ever
                // authenticate anything, and the parser deliberately does not
                // judge that - `decode_key` reads hex, and whether a key is
                // USABLE is a curve question answered in `ed25519`. But a peer
                // that is silently unauthorized in a file that lists it is the
                // invisible half this whole subcommand exists to expose, so it
                // is reported HERE, in the tool whose job is reporting.
                //
                // Two ways a key gets here: not a point at all (a corrupted
                // copy), or a small-order point (an all-zero placeholder from a
                // half-finished keygen). `verify` refuses both, so this is a
                // diagnostic, not a gate.
                match ed25519::Point::decode(&p.key) {
                    None => {
                        broken += 1;
                        out(target, b"  line ");
                        put_dec(target, lineno as u64);
                        out(target, b": that key is not a valid public key, so this peer is NOT authorized\r\n");
                    }
                    Some(pt) if pt.is_small_order() => {
                        broken += 1;
                        out(target, b"  line ");
                        put_dec(target, lineno as u64);
                        out(target, b": that key is degenerate (small order) and can never authenticate\r\n");
                    }
                    Some(_) => {}
                }
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
    // A DUPLICATE ADDRESS IS NOT A MALFORMED LINE, and that is why it needed
    // its own report: both lines parse, both name a real key, and `classify`
    // calls the file perfectly well-formed. But a client resolves a reply's
    // expected key BY ADDRESS, so two lines claiming one address means it
    // cannot tell which key to expect - `find_by_ip` now refuses rather than
    // taking the first, which is safe and completely silent. This is the voice.
    //
    // Reachable by the documented workflow: revocation is "delete a line", so
    // re-keying a node by APPENDING its new line is the natural mistake. Until
    // now the exporter kept accepting that peer (it looks up by key, scanning
    // every line) while every reply it sent failed to verify - one file working
    // in one direction only.
    let mut dups = [0u32; 8];
    let ndup = clusterkeys::duplicate_ip_lines(&buf[..n], &mut dups);
    for &l in dups.iter().take(ndup.min(dups.len())) {
        out(target, b"  line ");
        put_dec(target, l as u64);
        out(target, b": duplicate address - this address is AMBIGUOUS, so replies\r\n");
        out(target, b"          from it cannot be verified. Delete the stale line.\r\n");
    }
    out(target, b"clusterkey: ");
    put_dec(target, found as u64);
    out(target, b" peer(s)");
    if broken > 0 {
        out(target, b", ");
        put_dec(target, broken as u64);
        out(target, b" unreadable line(s)");
    }
    if ndup > 0 {
        out(target, b", ");
        put_dec(target, ndup as u64);
        out(target, b" duplicate address line(s)");
    }
    out(target, b"\r\n");
    if broken > 0 || truncated || ndup > 0 {
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
    let Some(key) = public_key_of_this_machine(target) else {
        out(target, b"clusterkey: no identity here yet - run 'clusterkey new'\r\n");
        return 1;
    };
    let mut line = [0u8; 160];
    // NOT root-flagged. Whether the OTHER machine trusts this one with root is
    // that machine's decision, made there by appending ` root` to the line; a
    // line printed here to be pasted must not make it on its behalf.
    match clusterkeys::format_line(&mut line, &name[..nlen], &ip, &key, false) {
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

/// What reading a small file produced.
///
/// The three cases are kept apart because one caller genuinely needs them:
/// `generate`'s overwrite guard must not treat "I could not tell whether a key
/// exists" as "there is no key". The first version of this returned an
/// `Option`, folding all three together - while its own doc comment explained
/// that it used `fs_read_bulk` precisely so they could be distinguished. Writing
/// down a property and then not using it is how the `/etc/shadow` lockout
/// happened too.
enum FileRead {
    /// Read `n` bytes.
    Bytes(usize),
    /// Definitely not there.
    Absent,
    /// There may or may not be a file; the read failed for some other reason,
    /// or it was empty.
    Unreadable,
}

/// Read a whole small file.
///
/// Deliberately NOT `ulib::read_file_all`, which folds every failure into `0`.
fn read_file(path: &str, buf: &mut [u8]) -> FileRead {
    let r = ulib::fs_read_bulk(path, 0, buf);
    if r == syscall_abi::FS_ERR_NOT_FOUND {
        return FileRead::Absent;
    }
    if ulib::is_fs_error(r) || r == 0 {
        return FileRead::Unreadable;
    }
    FileRead::Bytes((r as usize).min(buf.len()))
}

/// This machine's public key, derived from the PRIVATE key when possible.
///
/// `id` is the source of truth and `id.pub` a convenience copy: the public half
/// is a function of the private one, so deriving it cannot disagree with itself.
/// That matters for two reasons found in review:
///
/// - The no-entropy path tells an operator to generate a key elsewhere and copy
///   `/etc/cluster/id` in. Reading only `id.pub` made that advice a dead end -
///   the tool would report no identity, and the only way forward it offered was
///   `new -f`, which destroys the key just copied in.
/// - A generation that wrote `id` and then failed to write `id.pub` left a new
///   private key beside a stale public one, and the tool would confidently print
///   the stale key for the operator to distribute. Deriving detects it instead.
fn public_key_of_this_machine(target: u64) -> Option<[u8; KEY_LEN]> {
    let mut buf = [0u8; 128];
    if let FileRead::Bytes(n) = read_file(ID_PATH, &mut buf) {
        if let Some(seed) = clusterkeys::parse_key_file(&buf[..n]) {
            let derived = ed25519::public_key(&seed);
            // If id.pub exists and disagrees, say so rather than pick one.
            let mut pbuf = [0u8; 128];
            if let FileRead::Bytes(pn) = read_file(PUB_PATH, &mut pbuf) {
                if let Some(stored) = clusterkeys::parse_key_file(&pbuf[..pn]) {
                    if stored != derived {
                        out(target, b"clusterkey: WARNING /etc/cluster/id.pub does not match id.\r\n");
                        out(target, b"  Using the key derived from id, which is authoritative.\r\n");
                        out(target, b"  Run 'clusterkey new -f' only if you mean to replace the identity.\r\n");
                    }
                }
            }
            return Some(derived);
        }
        out(target, b"clusterkey: /etc/cluster/id is not a key\r\n");
        return None;
    }
    // No private key here: fall back to a stored public one, which is all a
    // machine holding someone else's public key would have.
    let mut pbuf = [0u8; 128];
    if let FileRead::Bytes(pn) = read_file(PUB_PATH, &mut pbuf) {
        return clusterkeys::parse_key_file(&pbuf[..pn]);
    }
    None
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

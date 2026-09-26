//! The **user key**, the **realm** and the **user-key registry**: step 3 of
//! `docs/roadmap/roadmap-user-keys.md` (Decisions 2, 3 and 9).
//!
//! A user's cluster key is derived from their password, so it exists only while
//! they are logged in somewhere: PBKDF2-HMAC-SHA-512 at
//! [`ninep_abi::USERKEY_COUNT`] iterations over a salt of
//!
//! ```text
//! SIG_DOMAIN_USERKEY ‖ len(realm):1 ‖ realm ‖ len(name):1 ‖ name
//! ```
//!
//! and the first 32 bytes are the Ed25519 seed. The two length bytes are what
//! keep the fields apart: without them realm `ouroboros-dev` with user `x` and
//! realm `ouroboros-de` with user `vx` would share a salt, and so a key.
//!
//! No node identity is in the derivation, so the same user with the same
//! password has the same key on every node of a cluster; the **realm** keeps
//! two clusters' users apart. It must be unique to its cluster, since a
//! dictionary precomputed for one realm serves every node in it.
//!
//! The **registry**, `/etc/cluster/users`, lists the users whose remote claims
//! need their own credential on this machine (Decision 5): one line per user,
//! `<name> <pubkey-hex>`, an optional `# note` after it.

use core::num::NonZeroU32;

use ninep_abi::{NP_NAME_LEN, REALM_MAX, SIG_DOMAIN_USERKEY, USERKEY_COUNT};

use crate::{contains_space, decode_key, encode_key, split_field, trim, KEY_HEX_LEN, KEY_LEN};

/// How many bytes of `/etc/cluster/users` a machine reads. About ten users at
/// 98 bytes a line. A stack budget like [`crate::AUTHORIZED_MAX`]: nothing
/// reads the registry on a server yet, and step 7, which has `netd` read it,
/// measures it there rather than assuming this figure fits.
pub const USERS_MAX: usize = 1024;

/// The longest user-key salt: the tag, two length bytes, the longest realm and
/// the longest name.
pub const USERKEY_SALT_MAX: usize = SIG_DOMAIN_USERKEY.len() + 1 + REALM_MAX + 1 + NP_NAME_LEN;

/// The most bytes of `/etc/cluster/realm` a realm file may hold: the realm and
/// room for surrounding whitespace. EVERY READER reads `REALM_FILE_MAX + 1`
/// bytes and hands [`parse_realm`] what it got, so a longer file arrives as
/// `REALM_FILE_MAX + 1` bytes and is refused the same way by all of them. A
/// reader with a smaller buffer would see a valid-looking prefix of a file the
/// others refuse, and a diagnostic tool would call a broken setup fine.
pub const REALM_FILE_MAX: usize = 64;

/// The realm in `/etc/cluster/realm`'s contents, or `None` if there is none.
///
/// Surrounding whitespace (a trailing newline) is ignored. Contents longer than
/// [`REALM_FILE_MAX`] are refused whatever they start with. The realm itself is
/// one word of 1 to [`REALM_MAX`] printable, non-space ASCII bytes; anything
/// else is `None`, which a node treats as having no realm: it derives no key
/// and sends no credential. Never a login failure (Decision 10): the password
/// check does not read this file.
pub fn parse_realm(contents: &[u8]) -> Option<&[u8]> {
    if contents.len() > REALM_FILE_MAX {
        return None;
    }
    let realm = trim(contents);
    if realm.is_empty() || realm.len() > REALM_MAX {
        return None;
    }
    if !realm.iter().all(|&c| (0x21..=0x7e).contains(&c)) {
        return None;
    }
    Some(realm)
}

/// Write the user-key salt for `realm` and `name` into `out`, returning its
/// length, or `None` if either cannot appear in one: an empty or over-long
/// realm or name.
pub fn userkey_salt(realm: &[u8], name: &[u8], out: &mut [u8; USERKEY_SALT_MAX]) -> Option<usize> {
    if realm.is_empty() || realm.len() > REALM_MAX || name.is_empty() || name.len() > NP_NAME_LEN {
        return None;
    }
    let mut n = 0;
    for part in [SIG_DOMAIN_USERKEY, &[realm.len() as u8], realm, &[name.len() as u8], name] {
        out[n..n + part.len()].copy_from_slice(part);
        n += part.len();
    }
    Some(n)
}

/// `name`'s Ed25519 seed in `realm`, from `password`, at the cluster's
/// iteration count. `None` for a realm or name [`userkey_salt`] refuses.
///
/// About one second on the QEMU guest. It belongs in a program that has the
/// password and is not supervised (`login`, `passwd`), never in a server.
pub fn derive_user_seed(name: &[u8], realm: &[u8], password: &[u8]) -> Option<[u8; KEY_LEN]> {
    derive_user_seed_with(name, realm, password, USERKEY_COUNT)
}

/// [`derive_user_seed`] at another iteration count: for tests of the salt,
/// which need not pay the full count to show two salts differ. Every key a
/// node uses comes from [`derive_user_seed`].
pub fn derive_user_seed_with(
    name: &[u8],
    realm: &[u8],
    password: &[u8],
    iterations: NonZeroU32,
) -> Option<[u8; KEY_LEN]> {
    let mut salt = [0u8; USERKEY_SALT_MAX];
    let n = userkey_salt(realm, name, &mut salt)?;
    let mut out = [0u8; 64];
    ed25519::pbkdf2_hmac_sha512(password, &salt[..n], iterations, &mut out);
    let mut seed = [0u8; KEY_LEN];
    seed.copy_from_slice(&out[..KEY_LEN]);
    Some(seed)
}

/// What the registry says about one user.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UserLookup {
    /// Exactly one key is registered for the user.
    Registered([u8; KEY_LEN]),
    /// No line names the user: a claim of them is served as today, on the
    /// machine's word (Decision 5).
    Unregistered,
    /// A line names the user but cannot be read, or two lines give different
    /// keys. **Never read as unregistered**: that would turn attestation OFF
    /// for the user whose registration was mistyped, so the caller refuses a
    /// claim of them instead.
    Broken,
}

/// Look `name` up in a registry buffer. Blank lines and `#` comments are
/// skipped; so is a line whose first field is another user's name, whatever
/// the rest of it says.
///
/// `whole` is REQUIRED: whether `users` is the entire file. A reader fills at
/// most [`USERS_MAX`] bytes, and a user whose line lies past the cut would
/// otherwise read as `Unregistered`, served without a credential: attestation
/// switched off for exactly the users who registered. So a registry that did
/// not fit answers [`UserLookup::Broken`] for every name, found or not (a
/// later line could also contradict a found one), and the caller refuses.
pub fn lookup_user(users: &[u8], name: &[u8], whole: bool) -> UserLookup {
    if !whole {
        return UserLookup::Broken;
    }
    let mut found: Option<[u8; KEY_LEN]> = None;
    for raw in users.split(|&c| c == b'\n') {
        let line = trim(raw);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let Some((who, rest)) = split_field(line) else { continue };
        if who != name {
            continue;
        }
        let Some(key) = parse_user_key(rest) else { return UserLookup::Broken };
        match found {
            Some(k) if k != key => return UserLookup::Broken,
            _ => found = Some(key),
        }
    }
    match found {
        Some(k) => UserLookup::Registered(k),
        None => UserLookup::Unregistered,
    }
}

/// The key after a registry line's name: exactly one hex key, and nothing
/// after it but a `# note`.
fn parse_user_key(rest: &[u8]) -> Option<[u8; KEY_LEN]> {
    let (key_text, after) = split_field(rest)?;
    let after = trim(after);
    if !after.is_empty() && after[0] != b'#' {
        return None;
    }
    decode_key(key_text)
}

/// Format a registry line (without a trailing newline) into `out`, returning
/// its length, or `None` for a name that could not be read back (empty, too
/// long, containing a space, or starting with `#`) or a buffer too small.
pub fn format_user_line(out: &mut [u8], name: &[u8], key: &[u8; KEY_LEN]) -> Option<usize> {
    if name.is_empty() || name.len() > NP_NAME_LEN || contains_space(name) || name[0] == b'#' {
        return None;
    }
    let total = name.len() + 1 + KEY_HEX_LEN;
    if out.len() < total {
        return None;
    }
    out[..name.len()].copy_from_slice(name);
    out[name.len()] = b' ';
    let mut hex = [0u8; KEY_HEX_LEN];
    encode_key(key, &mut hex);
    out[name.len() + 1..total].copy_from_slice(&hex);
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEW: NonZeroU32 = match NonZeroU32::new(2) {
        Some(n) => n,
        None => panic!(),
    };

    /// `python3 scripts/mkclusterkeys.py --dev-user-keys`: every dev user's
    /// public key, derived by the Python in that script, which shares no code
    /// with this file. check-wire-constants.py holds this table equal to the
    /// script's current output.
    const DEV_USER_KEYS: &[(&str, &str, &str)] = &[
        // (name, password, public key hex)
        ("root", "root", "04138f721e53b358a9ed4ae05636ab114ad9534ccdfd0fe55116372613dab9ef"),
        ("user", "user", "e9da28586b272bf011df63d204692079a1920d5b128706f2bc6d1fc87886490f"),
        ("guest", "guest", "9b12a54c8a4d9ab6de5cd6b00a9db817c8016247e40bf9fb897a2a7dae5b48a5"),
    ];

    /// The step's check: the Rust derivation, at the real count, agrees with
    /// the independent Python for every dev user.
    #[test]
    fn every_dev_user_key_agrees_with_the_python() {
        for &(name, pw, want) in DEV_USER_KEYS {
            let seed = derive_user_seed(name.as_bytes(), b"ouroboros-dev", pw.as_bytes()).expect("derives");
            let mut hex = [0u8; KEY_HEX_LEN];
            encode_key(&ed25519::public_key(&seed), &mut hex);
            assert_eq!(&hex[..], want.as_bytes(), "{name}'s key");
        }
    }

    #[test]
    fn the_salt_is_the_documented_bytes() {
        let mut out = [0u8; USERKEY_SALT_MAX];
        let n = userkey_salt(b"ouroboros-dev", b"user", &mut out).expect("salt");
        let mut want = [0u8; USERKEY_SALT_MAX];
        let mut m = 0;
        for part in [&b"ouroboros-cluster-userkey-v1\0"[..], &[13], b"ouroboros-dev", &[4], b"user"] {
            want[m..m + part.len()].copy_from_slice(part);
            m += part.len();
        }
        assert_eq!(&out[..n], &want[..m]);
    }

    /// The step's control, as a test: without the length bytes these two pairs
    /// would concatenate to the same salt and derive the same key.
    #[test]
    fn the_length_prefixes_keep_realm_and_name_apart() {
        let mut a = [0u8; USERKEY_SALT_MAX];
        let mut b = [0u8; USERKEY_SALT_MAX];
        let na = userkey_salt(b"ouroboros-dev", b"x", &mut a).expect("salt");
        let nb = userkey_salt(b"ouroboros-de", b"vx", &mut b).expect("salt");
        assert_ne!(&a[..na], &b[..nb]);
        let ka = derive_user_seed_with(b"x", b"ouroboros-dev", b"pw", FEW);
        let kb = derive_user_seed_with(b"vx", b"ouroboros-de", b"pw", FEW);
        assert_ne!(ka, kb, "the pair must derive different keys");
        // And the same bytes WITHOUT the prefixes do collide, which is what the
        // prefixes are for: this is the collision, shown to exist.
        assert_eq!([&b"ouroboros-dev"[..], b"x"].concat(), [&b"ouroboros-de"[..], b"vx"].concat());
    }

    #[test]
    fn the_realm_and_the_name_each_change_the_key() {
        let base = derive_user_seed_with(b"user", b"ouroboros-dev", b"user", FEW);
        assert_ne!(base, derive_user_seed_with(b"user", b"another-realm", b"user", FEW));
        assert_ne!(base, derive_user_seed_with(b"guest", b"ouroboros-dev", b"user", FEW));
        assert_ne!(base, derive_user_seed_with(b"user", b"ouroboros-dev", b"User", FEW));
    }

    #[test]
    fn a_salt_refuses_what_its_length_bytes_cannot_carry() {
        let mut out = [0u8; USERKEY_SALT_MAX];
        assert!(userkey_salt(b"", b"user", &mut out).is_none());
        assert!(userkey_salt(b"r", b"", &mut out).is_none());
        assert!(userkey_salt(&[b'r'; REALM_MAX + 1], b"user", &mut out).is_none());
        assert!(userkey_salt(b"r", &[b'n'; NP_NAME_LEN + 1], &mut out).is_none());
        // The longest of both fits the buffer exactly.
        let n = userkey_salt(&[b'r'; REALM_MAX], &[b'n'; NP_NAME_LEN], &mut out).expect("fits");
        assert_eq!(n, USERKEY_SALT_MAX);
    }

    #[test]
    fn a_realm_is_one_printable_word() {
        assert_eq!(parse_realm(b"ouroboros-dev\n"), Some(&b"ouroboros-dev"[..]));
        assert_eq!(parse_realm(b"  3f9a0c\r\n"), Some(&b"3f9a0c"[..]));
        for bad in [&b""[..], b"\n", b"two words", b"tab\there", &[b'r'; REALM_MAX + 1], b"caf\xc3\xa9"] {
            assert_eq!(parse_realm(bad), None, "{bad:?}");
        }
        assert!(parse_realm(&[b'r'; REALM_MAX]).is_some());
    }

    fn lookup_user_whole(users: &[u8], name: &[u8]) -> UserLookup {
        lookup_user(users, name, true)
    }

    /// FAIL-CLOSED on a registry that did not fit: every name is Broken, the
    /// registered one included, never Unregistered.
    #[test]
    fn a_registry_that_did_not_fit_is_broken_for_everyone() {
        let (buf, n) = registry(&[&["user ", KEY_HEX].concat()]);
        assert_eq!(lookup_user(&buf[..n], b"user", false), UserLookup::Broken);
        assert_eq!(lookup_user(&buf[..n], b"guest", false), UserLookup::Broken);
    }

    #[test]
    fn a_realm_file_longer_than_the_limit_is_refused_whatever_it_starts_with() {
        let mut long = [b'\n'; REALM_FILE_MAX + 1];
        long[..3].copy_from_slice(b"abc");
        assert_eq!(parse_realm(&long), None);
        assert_eq!(parse_realm(&long[..REALM_FILE_MAX]), Some(&b"abc"[..]));
    }

    const KEY_HEX: &str = "e9da28586b272bf011df63d204692079a1920d5b128706f2bc6d1fc87886490f";
    const OTHER_HEX: &str = "04138f721e53b358a9ed4ae05636ab114ad9534ccdfd0fe55116372613dab9ef";

    fn key(hex: &str) -> [u8; KEY_LEN] {
        decode_key(hex.as_bytes()).expect("key")
    }

    fn registry(lines: &[&str]) -> ([u8; 512], usize) {
        let mut buf = [0u8; 512];
        let mut n = 0;
        for l in lines {
            buf[n..n + l.len()].copy_from_slice(l.as_bytes());
            n += l.len();
            buf[n] = b'\n';
            n += 1;
        }
        (buf, n)
    }

    #[test]
    fn a_registered_user_is_found_and_others_are_not() {
        let line = ["user ", KEY_HEX, "  # alice's laptop"].concat();
        let (buf, n) = registry(&["# the registry", "", &line]);
        assert_eq!(lookup_user_whole(&buf[..n], b"user"), UserLookup::Registered(key(KEY_HEX)));
        assert_eq!(lookup_user_whole(&buf[..n], b"guest"), UserLookup::Unregistered);
        assert_eq!(lookup_user_whole(&buf[..n], b"use"), UserLookup::Unregistered);
        // A commented-out line is a revocation, not a registration.
        let (buf, n) = registry(&[&["#user ", KEY_HEX].concat()]);
        assert_eq!(lookup_user_whole(&buf[..n], b"user"), UserLookup::Unregistered);
    }

    /// FAIL-CLOSED: a line that names the user and cannot be read, or two
    /// lines that disagree, is Broken, never Unregistered.
    #[test]
    fn a_mistyped_registration_is_broken_not_absent() {
        for bad in [
            ["user ", &KEY_HEX[..63]].concat(),
            ["user ", KEY_HEX, " extra"].concat(),
            "user".to_string(),
            ["user ", &KEY_HEX[..62], "zz"].concat(),
        ] {
            let (buf, n) = registry(&[&bad]);
            assert_eq!(lookup_user_whole(&buf[..n], b"user"), UserLookup::Broken, "{bad:?}");
        }
        let (buf, n) = registry(&[&["user ", KEY_HEX].concat(), &["user ", OTHER_HEX].concat()]);
        assert_eq!(lookup_user_whole(&buf[..n], b"user"), UserLookup::Broken, "two keys disagree");
        // The same key twice is one registration, not a conflict.
        let (buf, n) = registry(&[&["user ", KEY_HEX].concat(), &["user ", KEY_HEX].concat()]);
        assert_eq!(lookup_user_whole(&buf[..n], b"user"), UserLookup::Registered(key(KEY_HEX)));
        // Another user's broken line does not break this one.
        let (buf, n) = registry(&["guest nonsense", &["user ", KEY_HEX].concat()]);
        assert_eq!(lookup_user_whole(&buf[..n], b"user"), UserLookup::Registered(key(KEY_HEX)));
    }

    #[test]
    fn a_formatted_line_reads_back() {
        let mut out = [0u8; 128];
        let n = format_user_line(&mut out, b"user", &key(KEY_HEX)).expect("format");
        assert_eq!(lookup_user_whole(&out[..n], b"user"), UserLookup::Registered(key(KEY_HEX)));
        for bad in [&b""[..], b"two words", b"#user", &[b'n'; NP_NAME_LEN + 1]] {
            assert!(format_user_line(&mut out, bad, &key(KEY_HEX)).is_none(), "{bad:?}");
        }
        assert!(format_user_line(&mut [0u8; 8], b"user", &key(KEY_HEX)).is_none());
    }
}

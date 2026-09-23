//! **HMAC-SHA-512**, RFC 2104 over the SHA-512 in `sha512.rs`. Step 2 of
//! `docs/roadmap/roadmap-session-auth.md`: on a keyed session every request and
//! reply carries `HMAC-SHA-512(key, seq ‖ message)` truncated to 32 bytes (the
//! RFC 4868 truncation) in place of an Ed25519 signature, which is the whole
//! point of the arc, so this step's gate is that a MAC costs well under a sign.
//!
//! The construction is the RFC's: a key longer than SHA-512's 128-byte block is
//! hashed first, the result is padded with zeros to a block, and
//! `H((K ^ opad) ‖ H((K ^ ipad) ‖ message))`. It streams, because the keyed
//! frame's input is the sequence number and then the message, and building the
//! concatenation would need a buffer the size of the largest frame.
//!
//! Truncation is the caller's: this returns all 64 bytes, and the protocol
//! takes the first 32. [`ct_eq`] is the compare a tag check must use.
//!
//! ## Constant time
//!
//! [`ct_eq`] ORs the difference of every byte before it looks at the result,
//! so how long it takes does not depend on where two tags first differ. A test
//! vector cannot tell an early exit from this, so the property is checked by
//! reading the function, which is short on purpose.

use crate::sha512::{Sha512, BLOCK_LEN, DIGEST_LEN};

/// Bytes in a full HMAC-SHA-512 output.
pub const HMAC_LEN: usize = DIGEST_LEN;

const IPAD: u8 = 0x36;
const OPAD: u8 = 0x5c;

/// An incremental HMAC-SHA-512: the inner hash, already primed with the
/// key XOR ipad, and the key XOR opad block kept for the outer hash.
pub struct HmacSha512 {
    inner: Sha512,
    okey: [u8; BLOCK_LEN],
}

impl HmacSha512 {
    /// Start a MAC under `key`, of any length.
    pub fn new(key: &[u8]) -> Self {
        let mut k = [0u8; BLOCK_LEN];
        if key.len() > BLOCK_LEN {
            let mut h = Sha512::new();
            h.update(key);
            k[..DIGEST_LEN].copy_from_slice(&h.finalize());
        } else {
            k[..key.len()].copy_from_slice(key);
        }
        let mut ikey = [0u8; BLOCK_LEN];
        let mut okey = [0u8; BLOCK_LEN];
        for i in 0..BLOCK_LEN {
            ikey[i] = k[i] ^ IPAD;
            okey[i] = k[i] ^ OPAD;
        }
        let mut inner = Sha512::new();
        inner.update(&ikey);
        HmacSha512 { inner, okey }
    }

    /// Absorb `data`. Any split of the message gives the same MAC.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// The 64-byte MAC.
    pub fn finalize(self) -> [u8; HMAC_LEN] {
        let inner = self.inner.finalize();
        let mut outer = Sha512::new();
        outer.update(&self.okey);
        outer.update(&inner);
        outer.finalize()
    }
}

/// HMAC-SHA-512 of `msg` under `key`, in one call.
pub fn hmac_sha512(key: &[u8], msg: &[u8]) -> [u8; HMAC_LEN] {
    let mut h = HmacSha512::new(key);
    h.update(msg);
    h.finalize()
}

/// Whether `a` and `b` are equal, taking the same time wherever they differ.
/// The lengths are compared first and are not secret (a tag's length is fixed
/// by the protocol); the bytes are all visited, with no early exit.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    //! RFC 4231 section 4, the HMAC-SHA-512 row of every test case. They were
    //! parsed out of the RFC text and each was checked against Python's `hmac`
    //! before being written here, so a disagreement is this crate's. Case 5 is
    //! the RFC's truncation case and publishes only the first 16 bytes.
    use super::*;

    /// Hex into `buf`, returning the decoded prefix.
    fn unhex<'a>(s: &str, buf: &'a mut [u8; 256]) -> &'a [u8] {
        let b = s.as_bytes();
        let n = b.len() / 2;
        assert!(n <= buf.len());
        for i in 0..n {
            buf[i] = (((b[i * 2] as char).to_digit(16).expect("hex") as u8) << 4)
                | ((b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8);
        }
        &buf[..n]
    }

    /// `(key, data, HMAC-SHA-512)`, RFC 4231 test cases 1 to 7.
    const RFC_4231: &[(&str, &str, &str)] = &[
        // Test Case 1
        (
            "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
            "4869205468657265",
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854",
        ),
        // Test Case 2
        (
            "4a656665",
            "7768617420646f2079612077616e7420666f72206e6f7468696e673f",
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737",
        ),
        // Test Case 3
        (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
            "fa73b0089d56a284efb0f0756c890be9b1b5dbdd8ee81a3655f83e33b2279d39bf3e848279a722c806b485a47e67c807b946a337bee8942674278859e13292fb",
        ),
        // Test Case 4
        (
            "0102030405060708090a0b0c0d0e0f10111213141516171819",
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            "b0ba465637458c6990e5a8c5f61d4af7e576d97ff94b872de76f8050361ee3dba91ca5c11aa25eb4d679275cc5788063a5f19741120c4f2de2adebeb10a298dd",
        ),
        // Test Case 5
        (
            "0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
            "546573742057697468205472756e636174696f6e",
            "415fad6271580a531d4179bc891d87a6",
        ),
        // Test Case 6
        (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "54657374205573696e67204c6172676572205468616e20426c6f636b2d53697a65204b6579202d2048617368204b6579204669727374",
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f3526b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598",
        ),
        // Test Case 7
        (
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "5468697320697320612074657374207573696e672061206c6172676572207468616e20626c6f636b2d73697a65206b657920616e642061206c6172676572207468616e20626c6f636b2d73697a6520646174612e20546865206b6579206e6565647320746f20626520686173686564206265666f7265206265696e6720757365642062792074686520484d414320616c676f726974686d2e",
            "e37b6a775dc87dbaa4dfa9f96e5e3ffddebd71f8867289865df5a32d20cdc944b6022cac3c4982b10d5eeb55c3e4de15134676fb6de0446065c97440fa8c6a58",
        ),
    ];

    #[test]
    fn rfc4231_sha512_rows() {
        for (i, (k, d, m)) in RFC_4231.iter().enumerate() {
            let (mut kb, mut db, mut mb) = ([0u8; 256], [0u8; 256], [0u8; 256]);
            let mac = hmac_sha512(unhex(k, &mut kb), unhex(d, &mut db));
            let want = unhex(m, &mut mb);
            assert_eq!(&mac[..want.len()], want, "RFC 4231 test case {}", i + 1);
        }
    }

    /// Streaming is what the keyed frame uses (`seq`, then the message), so
    /// every split of a message must give the one-call MAC.
    #[test]
    fn every_split_matches_one_call() {
        let (k, d, _) = RFC_4231[6];
        let (mut kb, mut db) = ([0u8; 256], [0u8; 256]);
        let (key, data) = (unhex(k, &mut kb), unhex(d, &mut db));
        let whole = hmac_sha512(key, data);
        for split in 0..=data.len() {
            let mut h = HmacSha512::new(key);
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finalize(), whole, "split at {split}");
        }
    }

    /// A key of exactly one block is used as it stands; one byte more is hashed
    /// first. Both are checked against Python's `hmac` (`key = bytes(range(128))`
    /// and `bytes(range(129))`, message `b"block"`), since no RFC 4231 case has
    /// a key of exactly 128 bytes.
    #[test]
    fn keys_at_the_block_boundary() {
        let mut k = [0u8; 129];
        for (i, b) in k.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut buf = [0u8; 256];
        assert_eq!(hmac_sha512(&k[..128], b"block"), *<&[u8; 64]>::try_from(unhex(BLOCK_128, &mut buf)).unwrap());
        let mut buf = [0u8; 256];
        assert_eq!(hmac_sha512(&k[..129], b"block"), *<&[u8; 64]>::try_from(unhex(BLOCK_129, &mut buf)).unwrap());
    }
    const BLOCK_128: &str = "fe3b51aa0cdee6724c5031b730053a4ddce3d88787fb8c09c61c1ec0b74db91edf89bf7e2a3dce311856e3927b7f8060741a43b3de68faaf15909d65948d946a";
    const BLOCK_129: &str = "fe781056e3a006849d17799804da59ce741d8603cba0d6f03225de011d53fe04e9f0673a2bc2a3e0bfbd32745a0b437459fe09f7637267e1f920be9b10fc5a5e";

    /// A flipped bit in the key, or in the message, changes the MAC: every bit
    /// of both, on RFC 4231 test case 2.
    #[test]
    fn a_flipped_bit_changes_the_mac() {
        let (k, d, _) = RFC_4231[1];
        let (mut kb, mut db) = ([0u8; 256], [0u8; 256]);
        let key = unhex(k, &mut kb).len();
        let data = unhex(d, &mut db).len();
        let whole = hmac_sha512(&kb[..key], &db[..data]);
        for bit in 0..key * 8 {
            let mut k2 = kb;
            k2[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(hmac_sha512(&k2[..key], &db[..data]), whole, "key bit {bit}");
        }
        for bit in 0..data * 8 {
            let mut d2 = db;
            d2[bit / 8] ^= 1 << (bit % 8);
            assert_ne!(hmac_sha512(&kb[..key], &d2[..data]), whole, "message bit {bit}");
        }
    }

    #[test]
    fn ct_eq_compares_every_byte() {
        let a = [7u8; 32];
        let mut b = a;
        assert!(ct_eq(&a, &b));
        b[0] ^= 1;
        assert!(!ct_eq(&a, &b));
        b = a;
        b[31] ^= 0x80;
        assert!(!ct_eq(&a, &b));
        assert!(!ct_eq(&a, &a[..31]));
        assert!(ct_eq(&[], &[]));
    }
}

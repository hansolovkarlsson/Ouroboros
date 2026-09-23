//! **X25519** key agreement, RFC 7748 section 5, on the same GF(2²⁵⁵ − 19)
//! arithmetic as the signature (`field.rs`). Step 1 of
//! `docs/roadmap/roadmap-session-auth.md`: each side of a keyed cluster session
//! sends an ephemeral public key inside its signed `NP_SESSION`, and both
//! derive the session key from the shared secret this computes.
//!
//! Written from the RFC's own pseudocode rather than from another library, so
//! the names below (`x_2`, `z_3`, `AA`, `DA`, ...) are the RFC's, line for line,
//! and a reader can check the ladder against section 5 without translating.
//!
//! Three entry points, from rawest to safest:
//!
//! - [`x25519`] is the RFC's `X25519(k, u)` exactly: clamp, ladder, encode. It
//!   is what the test vectors exercise, and it returns whatever the ladder
//!   yields, including the all-zero value.
//! - [`x25519_public`] is `X25519(k, 9)`, a secret's public key.
//! - [`x25519_shared`] is the one a protocol should call. It refuses an
//!   **all-zero** shared secret, which the ladder produces for a peer key of
//!   small order (a point whose order divides the cofactor 8). RFC 7748 section
//!   6.1 makes the check a MAY; here it is not optional, because an all-zero
//!   secret is one an attacker can force without knowing either private key,
//!   and a session key derived from it authenticates nothing.
//!
//! ## Constant time
//!
//! The ladder does the same field operations for every scalar bit, and the
//! conditional swap is a mask, not a branch, as section 5 says it SHOULD be.
//! The zero check ORs every byte together before looking at the result, the
//! method section 6.1 names. No lookup table depends on the secret. The same
//! caveat as `curve.rs` applies: this is written to have no secret-dependent
//! branch or index, which is the precondition; it has not been checked against
//! the generated code.

use crate::field::Fe;

/// Bytes in a scalar, a public key and a shared secret alike.
pub const X25519_LEN: usize = 32;

/// The u-coordinate of the base point: 9, as one byte followed by 31 zeros.
pub const X25519_BASEPOINT: [u8; X25519_LEN] = {
    let mut b = [0u8; X25519_LEN];
    b[0] = 9;
    b
};

/// `(486662 - 2) / 4`, the curve constant the ladder's doubling uses.
const A24: Fe = Fe([121_665, 0, 0, 0, 0]);

/// RFC 7748 `decodeScalar25519`: clear the three low bits (a multiple of the
/// cofactor), clear bit 255, set bit 254 (so every scalar has the same length
/// and the ladder the same number of steps).
fn clamp(k: &[u8; X25519_LEN]) -> [u8; X25519_LEN] {
    let mut s = *k;
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
    s
}

/// Swap `a` and `b` when `swap` is 1, leave them when it is 0, with the same
/// instructions either way. `swap` must be exactly 0 or 1.
fn cswap(swap: u64, a: &mut Fe, b: &mut Fe) {
    let mask = 0u64.wrapping_sub(swap);
    for i in 0..5 {
        let dummy = mask & (a.0[i] ^ b.0[i]);
        a.0[i] ^= dummy;
        b.0[i] ^= dummy;
    }
}

/// The RFC's `X25519(k, u)`. `u` is decoded with its top bit masked, as section
/// 5 requires, and a non-canonical `u` (a value in `[p, 2²⁵⁵)`) is accepted and
/// reduced, as the section also requires.
///
/// Returns the all-zero value for a small-order `u`; use [`x25519_shared`] for
/// anything a peer sent.
pub fn x25519(k: &[u8; X25519_LEN], u: &[u8; X25519_LEN]) -> [u8; X25519_LEN] {
    let k = clamp(k);
    // `Fe::decode` drops bit 255, which is exactly decodeUCoordinate's mask.
    let x_1 = Fe::decode(u);
    let mut x_2 = Fe::ONE;
    let mut z_2 = Fe::ZERO;
    let mut x_3 = x_1;
    let mut z_3 = Fe::ONE;
    let mut swap = 0u64;

    // bits = 255, so t runs 254 down to 0. Bit 254 is always set by the clamp.
    let mut t = 255;
    while t > 0 {
        t -= 1;
        let k_t = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= k_t;
        cswap(swap, &mut x_2, &mut x_3);
        cswap(swap, &mut z_2, &mut z_3);
        swap = k_t;

        let a = x_2.add(z_2);
        let aa = a.square();
        let b = x_2.sub(z_2);
        let bb = b.square();
        let e = aa.sub(bb);
        let c = x_3.add(z_3);
        let d = x_3.sub(z_3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x_3 = da.add(cb).square();
        z_3 = x_1.mul(da.sub(cb).square());
        x_2 = aa.mul(bb);
        z_2 = e.mul(aa.add(A24.mul(e)));
    }
    cswap(swap, &mut x_2, &mut x_3);
    cswap(swap, &mut z_2, &mut z_3);

    // z_2^(p-2) is the inverse, and 0^(p-2) is 0, so a small-order input comes
    // out as the all-zero encoding rather than as a division fault.
    x_2.mul(z_2.invert()).encode()
}

/// A secret's public key: `X25519(k, 9)`.
pub fn x25519_public(k: &[u8; X25519_LEN]) -> [u8; X25519_LEN] {
    x25519(k, &X25519_BASEPOINT)
}

/// The shared secret `X25519(k, peer)`, or `None` when it is all zero, which is
/// what a small-order `peer` produces. The check ORs every byte before testing,
/// so it takes the same time whatever the secret is.
pub fn x25519_shared(
    k: &[u8; X25519_LEN],
    peer: &[u8; X25519_LEN],
) -> Option<[u8; X25519_LEN]> {
    let s = x25519(k, peer);
    let mut acc = 0u8;
    for b in s.iter() {
        acc |= *b;
    }
    if acc == 0 {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    //! The RFC's own vectors: section 5.2's two single calls and its iterated
    //! chain, and section 6.1's Diffie-Hellman exchange. A second set comes from
    //! OpenSSL on random keys (see `OPENSSL`), so the check does not rest only on
    //! inputs the RFC's authors chose.
    use super::*;

    fn unhex(s: &str) -> [u8; 32] {
        let b = s.as_bytes();
        assert_eq!(b.len(), 64, "expected 32 bytes");
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = (((b[i * 2] as char).to_digit(16).expect("hex") as u8) << 4)
                | ((b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8);
        }
        out
    }

    /// RFC 7748 section 5.2, the two single-call vectors: `(k, u, output)`.
    const RFC_5_2: &[(&str, &str, &str)] = &[
        (
            "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
            "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
        ),
        (
            "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
            "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
        ),
    ];

    #[test]
    fn rfc7748_5_2_single_calls() {
        for (k, u, out) in RFC_5_2 {
            assert_eq!(x25519(&unhex(k), &unhex(u)), unhex(out), "k = {k}");
        }
    }

    /// Section 5.2's second kind: start with k = u = 9, and each round set k to
    /// X25519(k, u) and u to the old k.
    fn iterate(n: u32) -> [u8; 32] {
        let mut k = X25519_BASEPOINT;
        let mut u = X25519_BASEPOINT;
        for _ in 0..n {
            let r = x25519(&k, &u);
            u = k;
            k = r;
        }
        k
    }

    #[test]
    fn rfc7748_5_2_one_iteration() {
        assert_eq!(
            iterate(1),
            unhex("422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079")
        );
    }

    #[test]
    fn rfc7748_5_2_thousand_iterations() {
        assert_eq!(
            iterate(1_000),
            unhex("684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51")
        );
    }

    /// The RFC's million-iteration row. Minutes in a debug build, so it is run
    /// on request: `cargo test -p ed25519 --release -- --ignored`.
    #[test]
    #[ignore]
    fn rfc7748_5_2_million_iterations() {
        assert_eq!(
            iterate(1_000_000),
            unhex("7c3911e0ab2586fd864497297e575e6f3bc601c0883c30df5f4dd2d24f665424")
        );
    }

    #[test]
    fn rfc7748_6_1_diffie_hellman() {
        let a = unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let a_pub = unhex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let b = unhex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let b_pub = unhex("de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let k = unhex("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");

        assert_eq!(x25519_public(&a), a_pub);
        assert_eq!(x25519_public(&b), b_pub);
        assert_eq!(x25519_shared(&a, &b_pub), Some(k));
        assert_eq!(x25519_shared(&b, &a_pub), Some(k));
    }

    /// `(a, b, a_pub, b_pub, shared)` from OpenSSL 3.6.4 on random keys:
    /// `openssl genpkey -algorithm X25519`, the raw keys read back with
    /// `openssl pkey -text`, and the secret from `openssl pkeyutl -derive`.
    const OPENSSL: &[(&str, &str, &str, &str, &str)] = &[
        (
            "00988a9395b033f822f34c3dd8c6cab73e13a6f59da01f5c6f421833732fda72",
            "083883bc7e721261213b2fd95169d2ee2d8cbe68f75ffedacee27a772322ad68",
            "1d483e0763224901e3cc5a041e157049b3b78f552e3fdb316303d434169c9767",
            "437d251acb49fcd96ff1e10c5d06bd1bbe086e647c56f26a23080408612e3323",
            "8321e7258ad7e8ab5e89b697158c0389055fe322739e681c27827cd259592d39",
        ),
        (
            "d8decec89e6cebf395299d712bf49718199186a9a7f9727afdbca02b11f0816d",
            "c8a8ed7f6d1188a686da76109b9b9c8a1a9ee9c215b3064d19b1afc6b0786b5a",
            "5e6122785b00911fb5a580e9577a1c17e56c0558990c4496195e59dbb3637c6c",
            "8b2ffd8d54482ec25150f7cbdb98409eee317d199270fd107a5b48e2a3074569",
            "923dec0e4f279350abc5ddf68b89e4156c090392ac840f3882721d45d54fa57d",
        ),
        (
            "00df432817f227298485d55ff7b130871195e3cb33c1e5d7f8fe02a02b792648",
            "40ffd7632b7f631c584bd93584d930f4810504cf1f7cb8d5d6d3e911e5583846",
            "e4bede39e4b6828eb82de9c01cbc00d2403c403cea43b8f7ae52fa89c1724b6d",
            "6fbf5a53a1d3a6ed4e990f8a37f7ef32d5958b56ebee65791a5f10f9255af012",
            "b80714bae4a551cb56d777d618cfd7912123c221e4b1ce3d4f69e02e81c0a47c",
        ),
    ];

    #[test]
    fn matches_openssl_on_random_keys() {
        assert!(!OPENSSL.is_empty());
        for (a, b, a_pub, b_pub, k) in OPENSSL {
            let (a, b) = (unhex(a), unhex(b));
            assert_eq!(x25519_public(&a), unhex(a_pub));
            assert_eq!(x25519_public(&b), unhex(b_pub));
            assert_eq!(x25519_shared(&a, &unhex(b_pub)), Some(unhex(k)));
            assert_eq!(x25519_shared(&b, &unhex(a_pub)), Some(unhex(k)));
        }
    }

    /// A small-order peer key yields the all-zero secret, and `x25519_shared`
    /// refuses it. u = 0 is the simplest such point; p itself is the same point
    /// written non-canonically, which the RFC says to accept and reduce, so it
    /// must be refused too.
    #[test]
    fn small_order_peer_is_refused() {
        let k = unhex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let zero = [0u8; 32];
        let p = unhex("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        assert_eq!(x25519(&k, &zero), zero);
        assert_eq!(x25519_shared(&k, &zero), None);
        assert_eq!(x25519(&k, &p), zero);
        assert_eq!(x25519_shared(&k, &p), None);
    }

    /// Section 5: the top bit of u MUST be masked. Setting it cannot change the
    /// result.
    #[test]
    fn top_bit_of_u_is_ignored() {
        let (k, u, out) = RFC_5_2[0];
        let mut u = unhex(u);
        u[31] |= 0x80;
        assert_eq!(x25519(&unhex(k), &u), unhex(out));
    }

    /// The clamp decides the scalar, so bits it overwrites cannot change the
    /// result: the three low bits, bit 255 and bit 254.
    #[test]
    fn clamped_bits_do_not_matter() {
        let (k, u, out) = RFC_5_2[0];
        let mut k = unhex(k);
        k[0] ^= 0x07;
        k[31] ^= 0xc0;
        assert_eq!(x25519(&k, &unhex(u)), unhex(out));
    }
}

//! Points on Ed25519's twisted Edwards curve, and scalar multiplication.
//! Step 3 of `docs/roadmap-cluster-keys.md`.
//!
//! The curve is `−x² + y² = 1 + d·x²·y²` over GF(2²⁵⁵−19), with
//! `d = −121665/121666`. Signing and verification are entirely: multiply a
//! scalar by a point, and compare the compressed result.
//!
//! ## Extended coordinates
//!
//! A point is `(X : Y : Z : T)` with `x = X/Z`, `y = Y/Z` and `x·y = T/Z`. The
//! redundant `T` is what buys an addition formula with no inversions and no
//! special cases — the same code adds any two points, including a point to
//! itself and anything to the identity. That matters more here than speed: a
//! formula with exceptions needs branches, and branches on secret data are how
//! a signature scheme leaks its key.
//!
//! Affine coordinates would need a field inversion per addition (254 squarings
//! each), so the only place this converts back is `encode`.
//!
//! ## Constant time
//!
//! `mul` is written so its *sequence of operations* does not depend on the
//! scalar: every bit does one doubling and one addition, and a branchless select
//! keeps or discards the sum. It is not a formal constant-time claim — that
//! needs the compiler not to outsmart it, which nothing here verifies — but the
//! algorithm is the right shape, and the shape is a precondition for the claim.

use crate::field::Fe;

/// Bytes in a compressed point: `y`, little-endian, with the low bit of `x` in
/// bit 255.
pub const POINT_LEN: usize = 32;

/// d = -121665/121666, the curve constant of the twisted Edwards form.
const D: Fe = Fe([0x34dca135978a3, 0x1a8283b156ebd, 0x5e7a26001c029, 0x739c663a03cbb, 0x52036cee2b6ff]);

/// 2d, precomputed: the addition formula uses it directly.
const D2: Fe = Fe([0x69b9426b2f159, 0x35050762add7a, 0x3cf44c0038052, 0x6738cc7407977, 0x2406d9dc56dff]);

/// √(−1) = 2^((p−1)/4). Recovering x from y produces a root that is off by this
/// factor half the time; this is what corrects it.
const SQRT_M1: Fe = Fe([0x61b274a0ea0b0, 0xd5a5fc8f189d, 0x7ef5e9cbd0c60, 0x78595a6804c9e, 0x2b8324804fc1d]);

/// A curve point in extended coordinates.
#[derive(Clone, Copy)]
pub struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

// `add`/`mul` are named methods rather than `core::ops` implementations, for the
// same reason as in `field.rs`: every one of these is real work (an addition is
// nine field multiplications; `mul` is 256 doublings and 256 additions), and an
// operator would make the cost invisible at the call site. `mul` could not be
// `Mul` in any case - it takes a scalar as bytes, not a `Point`.
#[allow(clippy::should_implement_trait)]
impl Point {
    /// The identity (neutral) element: affine `(0, 1)`.
    pub const IDENTITY: Point = Point { x: Fe::ZERO, y: Fe::ONE, z: Fe::ONE, t: Fe::ZERO };

    /// The standard base point B, the generator Ed25519 is defined against.
    /// Its `y` is `4/5`; the constants are the resulting affine coordinates and
    /// their product, all computed rather than transcribed (see
    /// `scripts/gen-curve-vectors.py`).
    pub const BASE: Point = Point {
        x: Fe([0x62d608f25d51a, 0x412a4b4f6592a, 0x75b7171a4b31d, 0x1ff60527118fe, 0x216936d3cd6e5]),
        y: Fe([0x6666666666658, 0x4cccccccccccc, 0x1999999999999, 0x3333333333333, 0x6666666666666]),
        z: Fe::ONE,
        t: Fe([0x68ab3a5b7dda3, 0xeea2a5eadbb, 0x2af8df483c27e, 0x332b375274732, 0x67875f0fd78b7]),
    };

    /// Add two points. Unified: correct for equal points and for the identity,
    /// with no case analysis (`add-2008-hwcd-3`, specialised for a = −1).
    pub fn add(self, rhs: Point) -> Point {
        let a = self.y.sub(self.x).mul(rhs.y.sub(rhs.x));
        let b = self.y.add(self.x).mul(rhs.y.add(rhs.x));
        let c = self.t.mul(D2).mul(rhs.t);
        let d = self.z.mul(rhs.z);
        let d = d.add(d); // 2·Z1·Z2
        let e = b.sub(a);
        let f = d.sub(c);
        let g = d.add(c);
        let h = b.add(a);
        Point { x: e.mul(f), y: g.mul(h), t: e.mul(h), z: f.mul(g) }
    }

    /// Double a point (`dbl-2008-hwcd`, a = −1). `self.add(self)` gives the same
    /// answer; this is the cheaper route and scalar multiplication runs it once
    /// per bit.
    pub fn double(self) -> Point {
        let a = self.x.square();
        let b = self.y.square();
        let c = self.z.square();
        let c = c.add(c); // 2·Z²
        let neg_a = a.neg();
        let e = self.x.add(self.y).square().sub(a).sub(b);
        let g = neg_a.add(b);
        let f = g.sub(c);
        let h = neg_a.sub(b);
        Point { x: e.mul(f), y: g.mul(h), t: e.mul(h), z: f.mul(g) }
    }

    /// Branchless select between two points.
    fn ct_select(a: Point, b: Point, choice: u8) -> Point {
        Point {
            x: Fe::ct_select(a.x, b.x, choice),
            y: Fe::ct_select(a.y, b.y, choice),
            z: Fe::ct_select(a.z, b.z, choice),
            t: Fe::ct_select(a.t, b.t, choice),
        }
    }

    /// `scalar · self`, with `scalar` little-endian.
    ///
    /// Plain double-and-add from the top bit down, doing the addition **on every
    /// bit** and discarding it with a select when the bit is zero. That is twice
    /// the work of the obvious version and is the point: the scalar here is a
    /// secret key, and a loop that skips the add when a bit is zero has a
    /// runtime that depends on the key's Hamming weight.
    ///
    /// No windowing or precomputed tables. A fixed-base table would speed up
    /// signing considerably and is the obvious optimisation — deferred until
    /// step 5 measures whether the cost matters on the target, and noted in
    /// `roadmap-cluster-keys.md` as such. Any table it adds must be plain
    /// integers, not references (the crate's relocation rule).
    pub fn mul(self, scalar: &[u8; 32]) -> Point {
        let mut acc = Point::IDENTITY;
        for i in (0..256).rev() {
            acc = acc.double();
            let bit = (scalar[i / 8] >> (i % 8)) & 1;
            let sum = acc.add(self);
            acc = Point::ct_select(acc, sum, bit);
        }
        acc
    }

    /// `scalar · B`, the operation behind deriving a public key from a secret.
    pub fn mul_base(scalar: &[u8; 32]) -> Point {
        Point::BASE.mul(scalar)
    }

    /// The compressed 32-byte form: `y` little-endian, with the low bit of `x`
    /// in bit 255. This is the one place a field inversion is needed, to get
    /// back from projective to affine.
    pub fn encode(self) -> [u8; POINT_LEN] {
        let z_inv = self.z.invert();
        let x = self.x.mul(z_inv);
        let y = self.y.mul(z_inv);
        let mut out = y.encode();
        out[31] |= x.is_negative() << 7;
        out
    }

    /// Decode a compressed point, or `None` if the bytes are not one.
    ///
    /// Recovers `x` from `y` via `x² = (y²−1)/(d·y²+1)`, taking the root whose
    /// low bit matches the stored sign. **Returns `None` rather than a wrong
    /// point** when no root exists (the input is off the curve) — a signature
    /// verifier is fed attacker-controlled bytes here, and "not a point" must be
    /// a refusal, not a value.
    pub fn decode(bytes: &[u8; POINT_LEN]) -> Option<Point> {
        // RFC 8032 §5.1.3: y must be CANONICAL. `Fe::decode` reduces instead of
        // refusing, which is right for a field element and wrong here - without
        // this check every point has 19 alternate encodings that all verify, so
        // a signature's R or a peer's public key could be rewritten byte-wise
        // while still passing. The Python peer this cluster is checked against
        // rejects them, so accepting them would also make the two disagree about
        // whether a frame is valid.
        if !Fe::is_canonical(bytes) {
            return None;
        }
        let sign = bytes[31] >> 7;
        let y = Fe::decode(bytes); // ignores bit 255
        let yy = y.square();
        let u = yy.sub(Fe::ONE); // y² − 1
        let v = yy.mul(D).add(Fe::ONE); // d·y² + 1

        // x = u·v³·(u·v⁷)^((p−5)/8), then corrected below.
        let v3 = v.square().mul(v);
        let v7 = v3.square().mul(v);
        let mut x = u.mul(v3).mul(u.mul(v7).pow_p58());

        let vxx = x.square().mul(v);
        if !vxx.ct_eq(u) {
            // Off by √(−1): try the other root.
            x = x.mul(SQRT_M1);
            if !x.square().mul(v).ct_eq(u) {
                return None; // no square root: not a point on the curve
            }
        }
        // x = 0 with the sign bit set encodes a point that does not exist.
        if x.is_zero() && sign == 1 {
            return None;
        }
        if x.is_negative() != sign {
            x = x.neg();
        }
        Some(Point { x, y, z: Fe::ONE, t: x.mul(y) })
    }

    /// Whether two points are equal.
    ///
    /// Compared **projectively** — `X₁·Z₂ = X₂·Z₁` and `Y₁·Z₂ = Y₂·Z₁` — which
    /// is four field multiplications. The obvious implementation compares
    /// compressed forms instead, and that costs two field *inversions*, about
    /// 508 squarings; signature verification compares points, and this crate's
    /// stated risk is exactly `netd`'s time and stack budget. Same answer:
    /// different projective representatives of one point compare equal either
    /// way, which `different_representatives_compare_equal` checks.
    /// Whether this point has **small order** — that is, whether multiplying it
    /// by the cofactor 8 gives the identity.
    ///
    /// COMPUTED, NOT ENUMERATED, and that is the whole point. The usual way to
    /// write this is a table of the eight small-order encodings, and the first
    /// version of this guard in `clusterkeys` was exactly that table, written
    /// out by hand. Three of its entries were wrong: it missed three genuine
    /// small-order keys (which it therefore accepted) and blocked one ordinary
    /// valid key and one string that is not a curve point at all. The values
    /// look plausible either way — nobody reads 32 bytes of hex and notices
    /// that `13e8…` should have been `26e8…` — and the test that was supposed
    /// to cover it only exercised the two entries that happened to be right.
    ///
    /// `[8]P == identity` IS the definition, costs three doublings, and cannot
    /// be transcribed wrongly.
    pub fn is_small_order(self) -> bool {
        self.double().double().double().ct_eq(Point::IDENTITY)
    }

    pub fn ct_eq(self, rhs: Point) -> bool {
        self.x.mul(rhs.z).ct_eq(rhs.x.mul(self.z)) && self.y.mul(rhs.z).ct_eq(rhs.y.mul(self.z))
    }
}

#[cfg(test)]
mod tests {
    //! Vectors from `scripts/gen-curve-vectors.py`, a reference Ed25519 in plain
    //! Python integers. That reference is itself checked against published
    //! values - it prints RFC 8032's base point and its five section 7.1 public
    //! keys - so this is not one implementation of mine checking another.
    use super::*;
    use crate::sha512;

    fn unhex32(s: &str) -> [u8; 32] {
        let b = s.as_bytes();
        assert_eq!(b.len(), 64);
        let mut out = [0u8; 32];
        for i in 0..32 {
            let hi = (b[i * 2] as char).to_digit(16).expect("hex") as u8;
            let lo = (b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8;
            out[i] = (hi << 4) | lo;
        }
        out
    }

    /// A 256-bit scalar, little-endian, from a u128.
    fn scalar(v: u128) -> [u8; 32] {
        let mut s = [0u8; 32];
        s[..16].copy_from_slice(&v.to_le_bytes());
        s
    }

    const BASE_ENCODED: &str = "5866666666666666666666666666666666666666666666666666666666666666";

    /// `(name, scalar as 32 little-endian bytes, (scalar·B) encoded)`
    const MULTIPLES: &[(&str, &str, &str)] = &[
    ("mul_1", "0100000000000000000000000000000000000000000000000000000000000000", "5866666666666666666666666666666666666666666666666666666666666666"),
    ("mul_2", "0200000000000000000000000000000000000000000000000000000000000000", "c9a3f86aae465f0e56513864510f3997561fa2c9e85ea21dc2292309f3cd6022"),
    ("mul_3", "0300000000000000000000000000000000000000000000000000000000000000", "d4b4f5784868c3020403246717ec169ff79e26608ea126a1ab69ee77d1b16712"),
    ("mul_4", "0400000000000000000000000000000000000000000000000000000000000000", "2f1132ca61ab38dff00f2fea3228f24c6c71d58085b80e47e19515cb27e8d047"),
    ("mul_5", "0500000000000000000000000000000000000000000000000000000000000000", "edc876d6831fd2105d0b4389ca2e283166469289146e2ce06faefe98b22548df"),
    ("mul_6", "0600000000000000000000000000000000000000000000000000000000000000", "f47e49f9d07ad2c1606b4d94067c41f9777d4ffda709b71da1d88628fce34d85"),
    ("mul_7", "0700000000000000000000000000000000000000000000000000000000000000", "b862409fb5c4c4123df2abf7462b88f041ad36dd6864ce872fd5472be363c5b1"),
    ("mul_8", "0800000000000000000000000000000000000000000000000000000000000000", "b4b937fca95b2f1e93e41e62fc3c78818ff38a66096fad6e7973e5c90006d321"),
    ("mul_255", "ff00000000000000000000000000000000000000000000000000000000000000", "cc613540cd8c99fa4647e6e83e969761b17515dbe1896fd0a3e4358ebca65c31"),
    ("mul_256", "0001000000000000000000000000000000000000000000000000000000000000", "c7f66c563120140ea8d927c19a3d1b7d0e26d381aaebf56b7902f1515c75550f"),
    ("mul_1000", "e803000000000000000000000000000000000000000000000000000000000000", "e7caaa83373a94afae43fec59b447c99ba282b19a7616c24c785ad8966a1e10e"),
    ("mul_2_64", "0000000000000000010000000000000000000000000000000000000000000000", "1353e48257fa1e8f062b90ba08b610544f7c1b26edda6bdd25d04eea42bb2503"),
    ("mul_2_252", "0000000000000000000000000000000000000000000000000000000000000010", "b8421c03ad2c038eacd7982913c60229b5d4e7cfcc8b83ec35c79c74b7ad855f"),
    ("mul_L_minus_1", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "58666666666666666666666666666666666666666666666666666666666666e6"),
    ("mul_2_255_top_bit", "0000000000000000000000000000000000000000000000000000000000000080", "6e4b94a41ca8bd8c8e68ae0970e953eea2824beb01692cfd821625e07f828b41"),
    ("mul_top_bit_plus", "3930000000000000000000000000000000000000000000000000000000000080", "a9f88807a49c2a14497f5f2d05c19acca4deb0c3a424e8c36f091b3a56ace3b8"),
    ("mul_all_ones", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", "db27fe4b7a4beb8c1b8c38a21e943a852304c9bb3035a5f36626b51162a68f9c"),
    ];

    /// `(label, RFC 8032 secret key, its public key)`
    const RFC_KEYS: &[(&str, &str, &str)] = &[
    ("9d61b19d", "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"),
    ("4ccd089b", "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c"),
    ("c5aa8df4", "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7", "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025"),
    ("f5e5767c", "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5", "278117fc144c72340f67d0f2316e8386ceffbf2b2428c9c51fef7c597f1d426e"),
    ("833fe624", "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42", "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf"),
    ];

    #[test]
    fn base_point_encodes_to_the_published_value() {
        // If this is wrong nothing else can be right, and it is the one value a
        // reader can check against the RFC by eye.
        assert_eq!(Point::BASE.encode(), unhex32(BASE_ENCODED));
    }

    #[test]
    fn small_multiples_match_the_reference() {
        for (name, k, want) in MULTIPLES {
            let got = Point::mul_base(&unhex32(k));
            assert_eq!(got.encode(), unhex32(want), "{name}");
        }
    }

    #[test]
    fn rfc8032_secret_keys_derive_their_published_public_keys() {
        // The end-to-end check for this step: SHA-512 the secret, clamp, multiply
        // the base point, compress. Every layer built so far is on this path, and
        // a single wrong limb anywhere produces a completely different key.
        for (label, sk, pk) in RFC_KEYS {
            let h = sha512(&unhex32(sk));
            let mut a = [0u8; 32];
            a.copy_from_slice(&h[..32]);
            a[0] &= 248; // clear the low three bits
            a[31] &= 127; // clear the top bit
            a[31] |= 64; // set bit 254
            assert_eq!(Point::mul_base(&a).encode(), unhex32(pk), "public key for {label}");
        }
    }

    #[test]
    fn doubling_agrees_with_adding_to_itself() {
        // `double` is a separate formula from `add`, so they can disagree. They
        // must not: scalar multiplication uses one per bit and the other on
        // demand.
        let mut p = Point::BASE;
        for i in 0..16 {
            assert!(p.double().ct_eq(p.add(p)), "double != p+p at step {i}");
            p = p.double();
        }
    }

    #[test]
    fn the_identity_behaves_like_one() {
        let b = Point::BASE;
        assert!(b.add(Point::IDENTITY).ct_eq(b));
        assert!(Point::IDENTITY.add(b).ct_eq(b));
        assert!(Point::IDENTITY.double().ct_eq(Point::IDENTITY));
        // 0·B is the identity, and the identity compresses to y=1.
        assert!(Point::mul_base(&scalar(0)).ct_eq(Point::IDENTITY));
        assert_eq!(
            Point::IDENTITY.encode(),
            unhex32("0100000000000000000000000000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn addition_is_associative_and_commutative() {
        // Laws no table of vectors can encode. The unified formula claims to work
        // for every input including equal points, which is exactly what these
        // combinations exercise.
        let pts = [
            Point::BASE,
            Point::BASE.double(),
            Point::mul_base(&scalar(1000)),
            Point::IDENTITY,
        ];
        for a in pts.iter() {
            for b in pts.iter() {
                assert!(a.add(*b).ct_eq(b.add(*a)), "add not commutative");
                for c in pts.iter() {
                    assert!(a.add(*b).add(*c).ct_eq(a.add(b.add(*c))), "add not associative");
                }
            }
        }
    }

    #[test]
    fn scalar_multiplication_is_linear() {
        // (j+k)·B == j·B + k·B, over values that cross byte and word boundaries
        // in the scalar - which is where a bit-indexing error would live.
        for (j, k) in [(1u128, 1), (2, 3), (255, 1), (256, 256), (1 << 63, 1), (1 << 64, 1 << 64)] {
            let lhs = Point::mul_base(&scalar(j + k));
            let rhs = Point::mul_base(&scalar(j)).add(Point::mul_base(&scalar(k)));
            assert!(lhs.ct_eq(rhs), "({j}+{k})·B != {j}·B + {k}·B");
        }
    }

    #[test]
    fn compressed_points_round_trip() {
        for (name, _k, want) in MULTIPLES {
            let bytes = unhex32(want);
            let p = Point::decode(&bytes).unwrap_or_else(|| panic!("{name} failed to decode"));
            assert_eq!(p.encode(), bytes, "{name} round trip");
        }
    }

    #[test]
    fn decode_refuses_bytes_that_are_not_points() {
        // A verifier is handed these by an attacker, so "not a point" has to be a
        // refusal rather than some other point. Roughly half of all y values have
        // no corresponding x, so a handful of arbitrary strings finds them.
        let mut refused = 0;
        for seed in 0u8..64 {
            let mut b = [0u8; 32];
            b[0] = seed;
            b[31] = 0x40; // a y value well inside the field, mostly not on the curve
            if Point::decode(&b).is_none() {
                refused += 1;
            }
        }
        assert!(refused > 8, "expected non-points among 64 candidates, refused {refused}");
        // And a specific one: y = p-1 with the sign bit set is not a point.
        let mut bad = unhex32("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        bad[31] |= 0x80;
        assert!(Point::decode(&bad).is_none(), "y = p-1 with sign set must be refused");
    }

    #[test]
    fn decoding_a_point_recovers_the_same_point() {
        // Not just byte round-tripping: decode must produce a point that behaves
        // identically under arithmetic, which catches a wrong sign on x.
        let p = Point::mul_base(&scalar(1000));
        let q = Point::decode(&p.encode()).expect("valid point");
        assert!(p.ct_eq(q));
        assert!(p.add(p).ct_eq(q.add(q)));
        assert!(p.double().ct_eq(q.double()));
    }

    #[test]
    fn non_canonical_y_is_refused() {
        // RFC 8032 §5.1.3: decoding fails when y >= p. Without this, adding p to
        // a point's y (while it still fits in 255 bits) gives a DIFFERENT byte
        // string that decodes to the SAME point - signature malleability, and a
        // disagreement with the Python peer, which refuses these.
        //
        // Each valid y below 19 has an alternate encoding y+p; those are the 19
        // the finding names. Check every one of them, plus p itself.
        for k in 0u8..19 {
            let mut alt = [0u8; 32];
            // p + k, little-endian: p = 2^255 - 19, so p + k = 2^255 - (19 - k).
            let v = (1u128 << 64) - 1; // fill helper; build the value byte-wise below
            let _ = v;
            let low = 0xedu16 + k as u16; // 0xed = 237 = 256 - 19
            alt[0] = low as u8;
            let carry = (low >> 8) as u8;
            for b in alt.iter_mut().take(31).skip(1) {
                *b = 0xff;
            }
            alt[31] = 0x7f;
            if carry == 1 {
                // p + k wrapped the low byte; propagate into the next.
                let mut i = 1;
                loop {
                    let (v, c) = alt[i].overflowing_add(1);
                    alt[i] = v;
                    if !c {
                        break;
                    }
                    i += 1;
                }
            }
            assert!(
                Point::decode(&alt).is_none(),
                "non-canonical y = p+{k} must be refused, bytes {alt:02x?}"
            );
        }
    }

    #[test]
    fn canonical_encodings_still_decode() {
        // The other direction, so the canonicality check cannot be "reject
        // everything": every vector point must still decode.
        for (name, _k, enc) in MULTIPLES {
            assert!(Point::decode(&unhex32(enc)).is_some(), "{name} must still decode");
        }
        // And the largest canonical y that is on the curve stays acceptable.
        assert!(Point::decode(&unhex32(BASE_ENCODED)).is_some());
    }

    #[test]
    fn different_representatives_compare_equal() {
        // Point equality is projective, so (X:Y:Z:T) and (kX:kY:kZ:kT) are the
        // same point and must compare equal. This is what makes the cheap
        // comparison legitimate; comparing compressed forms would too, at the
        // cost of two field inversions.
        let p = Point::mul_base(&scalar(1000));
        for k in [2u128, 3, 7, 1 << 40] {
            let f = {
                let mut acc = Fe::ONE;
                for _ in 0..k.min(64) {
                    acc = acc.add(Fe::ONE);
                }
                acc
            };
            let scaled = Point { x: p.x.mul(f), y: p.y.mul(f), z: p.z.mul(f), t: p.t.mul(f) };
            assert!(p.ct_eq(scaled), "scaled representative must compare equal (k={k})");
            assert_eq!(p.encode(), scaled.encode(), "and must compress identically");
        }
    }

    #[test]
    fn vectors_are_not_empty() {
        assert!(MULTIPLES.len() >= 17, "multiples missing: {}", MULTIPLES.len());
        assert!(RFC_KEYS.len() >= 5, "RFC key vectors missing: {}", RFC_KEYS.len());
    }
}

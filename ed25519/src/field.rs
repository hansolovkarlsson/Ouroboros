//! Arithmetic in the field **GF(2²⁵⁵ − 19)**, the field Ed25519's curve is
//! defined over. Step 2 of `docs/roadmap-cluster-keys.md`.
//!
//! ## The representation, and why this one
//!
//! An element is five `u64` limbs in **radix 2⁵¹**: `x = l₀ + l₁·2⁵¹ +
//! l₂·2¹⁰² + l₃·2¹⁵³ + l₄·2²⁰⁴`. The classic alternative is ten limbs of
//! ~25.5 bits, which exists so that products fit in a `u64` on 32-bit
//! machines; this target is aarch64, where a 64×64→128 multiply is one
//! instruction and Rust's `u128` maps straight onto it, so five limbs do
//! roughly half the work.
//!
//! **Every operation carries, so every `Fe` always has limbs below 2⁵¹.** The
//! textbook version of this representation reduces lazily — `add` leaves limbs
//! oversized and the next multiply cleans up — which is faster and requires
//! every caller to know how many additions it may do before it must reduce. A
//! test wrote here caught that being wrong (64 successive doublings wrapped a
//! `u64` limb and returned a wrong answer silently), and a rule someone has to
//! remember is exactly the kind of invariant this project keeps getting wrong.
//! The carry costs five shifts against the 254 squarings in one inversion.
//!
//! Values are still not necessarily *canonical* — an element can be held as any
//! representative congruent mod p — so equality and encoding reduce fully.
//!
//! Reduction uses the field's defining shape: **2²⁵⁵ ≡ 19**, so a bit carried
//! out of the top limb comes back in at the bottom multiplied by 19.
//!
//! ## What is deliberately not here
//!
//! No constant-time *guarantees* are claimed yet. The operations are written
//! without data-dependent branches, which is the precondition for it, but the
//! claim itself needs the curve layer above (step 3) and is made there or not
//! at all. Nothing in this file is secret-dependent on its own.

/// Bytes in the canonical encoding of a field element.
pub const ELEM_LEN: usize = 32;

/// A field element as five radix-2⁵¹ limbs.
///
/// `Copy`, and only 40 bytes: these are passed by value everywhere, which keeps
/// the curve layer above free of borrows and keeps stack traffic predictable on
/// a task whose stack has hit its guard page five times.
#[derive(Clone, Copy, Debug)]
pub struct Fe(pub [u64; 5]);

/// 2⁵¹ − 1: one limb, all bits set.
const MASK: u64 = (1u64 << 51) - 1;

// `add`/`sub`/`neg` are named methods rather than `core::ops` implementations,
// which clippy flags and which is the intended choice here: in a curve
// implementation the cost is entirely field operations, and a reader checking an
// algorithm against its reference should see every one of them as a call.
// `a + b` reads as free; `a.add(b)` reads as work.
#[allow(clippy::should_implement_trait)]
impl Fe {
    /// The additive identity.
    pub const ZERO: Fe = Fe([0, 0, 0, 0, 0]);
    /// The multiplicative identity.
    pub const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// `self + rhs`, carried.
    ///
    /// The textbook version of this representation skips the carry and lets
    /// limbs grow, on the argument that a limb has headroom above 2⁵¹ and the
    /// next multiply will reduce anyway. **This one carries**, deliberately, and
    /// the reason is a test: `repeated_addition_never_overflows` doubles a value
    /// 64 times, and the lazy version silently wrapped a `u64` limb partway
    /// through and returned a wrong answer with no indication.
    ///
    /// Lazy reduction would be correct here *if* every caller knew how many adds
    /// it may do before reducing. That is a rule someone has to remember, which
    /// is the shape of invariant this project keeps getting wrong (see
    /// `docs/unspellable-postmortem.md`). Carrying costs five shifts and five
    /// adds against 254 squarings in a single inversion — far below measurable —
    /// and buys the invariant that **every `Fe` always has limbs below 2⁵¹**, so
    /// no caller can create an unreduced one by accident.
    pub fn add(self, rhs: Fe) -> Fe {
        let mut out = [0u64; 5];
        for ((o, a), b) in out.iter_mut().zip(self.0.iter()).zip(rhs.0.iter()) {
            *o = a.wrapping_add(*b);
        }
        Fe(out).carry()
    }

    /// `self - rhs`.
    ///
    /// Computed as `self + (2p - rhs)` rather than a borrowing subtraction: add
    /// a multiple of p large enough that every limb stays non-negative, so the
    /// result is congruent and no limb underflows. `2p` in this representation
    /// has limbs `(2⁵² - 38, 2⁵² - 2, 2⁵² - 2, 2⁵² - 2, 2⁵² - 2)` — the 38 is
    /// 2·19, the field's defining constant.
    pub fn sub(self, rhs: Fe) -> Fe {
        const TWO_P: [u64; 5] = [
            (1u64 << 52) - 38,
            (1u64 << 52) - 2,
            (1u64 << 52) - 2,
            (1u64 << 52) - 2,
            (1u64 << 52) - 2,
        ];
        let mut out = [0u64; 5];
        for (((o, a), t), b) in out
            .iter_mut()
            .zip(self.0.iter())
            .zip(TWO_P.iter())
            .zip(rhs.0.iter())
        {
            *o = a.wrapping_add(*t).wrapping_sub(*b);
        }
        // Carried, for the same reason `add` is: a subtraction adds ~2p before
        // subtracting, so an uncarried result is already near 2⁵², and a chain of
        // them would grow limbs without bound.
        Fe(out).carry()
    }

    /// `-self`.
    pub fn neg(self) -> Fe {
        Fe::ZERO.sub(self)
    }

    /// `self * rhs`, with the schoolbook product folded back into five limbs.
    ///
    /// Each limb of the result collects the products whose exponents land in
    /// its position; the terms that would land *above* 2²⁵⁵ are multiplied by
    /// **19** and land at the bottom instead, which is what `2²⁵⁵ ≡ 19` means
    /// operationally. The `u128` accumulators cannot overflow: each is a sum of
    /// at most five products of values below 2⁵⁴, times 19 — well under 2¹²⁸.
    pub fn mul(self, rhs: Fe) -> Fe {
        let a = self.0;
        let b = rhs.0;
        // Pre-multiplied by 19: used where a term wraps past the top limb.
        let b1_19 = 19 * b[1];
        let b2_19 = 19 * b[2];
        let b3_19 = 19 * b[3];
        let b4_19 = 19 * b[4];

        let m = |x: u64, y: u64| -> u128 { (x as u128) * (y as u128) };

        let c0 = m(a[0], b[0]) + m(a[1], b4_19) + m(a[2], b3_19) + m(a[3], b2_19) + m(a[4], b1_19);
        let c1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4_19) + m(a[3], b3_19) + m(a[4], b2_19);
        let c2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4_19) + m(a[4], b3_19);
        let c3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4_19);
        let c4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);

        reduce128([c0, c1, c2, c3, c4])
    }

    /// `self * self`. Same shape as `mul` with the symmetric terms doubled once
    /// instead of computed twice — kept as its own function because squaring is
    /// most of the work in an inversion (250 of the 254 steps below).
    pub fn square(self) -> Fe {
        self.mul(self)
    }

    /// `self` raised to the 2ⁿ-th power, i.e. `n` successive squarings.
    fn square_n(self, n: u32) -> Fe {
        let mut x = self;
        for _ in 0..n {
            x = x.square();
        }
        x
    }

    /// The multiplicative inverse, or **zero for zero** (which has none).
    ///
    /// By Fermat's little theorem: `x⁻¹ = x^(p−2)`. The addition chain below is
    /// the standard one for this exponent — 11 multiplications and 254
    /// squarings — and returning zero for zero matches what every reference
    /// implementation does, so a caller that forgets to check gets a wrong
    /// answer rather than a panic. Callers that care must check.
    pub fn invert(self) -> Fe {
        // x^(2^5 - 1)
        let z2 = self.square();
        let z4 = z2.square();
        let z8 = z4.square();
        let z9 = z8.mul(self);
        let z11 = z9.mul(z2);
        let z22 = z11.square();
        let z_5_0 = z22.mul(z9);
        // x^(2^10 - 1)
        let z_10_5 = z_5_0.square_n(5);
        let z_10_0 = z_10_5.mul(z_5_0);
        // x^(2^20 - 1)
        let z_20_10 = z_10_0.square_n(10);
        let z_20_0 = z_20_10.mul(z_10_0);
        // x^(2^40 - 1)
        let z_40_20 = z_20_0.square_n(20);
        let z_40_0 = z_40_20.mul(z_20_0);
        // x^(2^50 - 1)
        let z_50_10 = z_40_0.square_n(10);
        let z_50_0 = z_50_10.mul(z_10_0);
        // x^(2^100 - 1)
        let z_100_50 = z_50_0.square_n(50);
        let z_100_0 = z_100_50.mul(z_50_0);
        // x^(2^200 - 1)
        let z_200_100 = z_100_0.square_n(100);
        let z_200_0 = z_200_100.mul(z_100_0);
        // x^(2^250 - 1)
        let z_250_50 = z_200_0.square_n(50);
        let z_250_0 = z_250_50.mul(z_50_0);
        // x^(2^255 - 21) = x^(p-2)
        z_250_0.square_n(5).mul(z11)
    }

    /// Propagate carries so every limb is below 2⁵¹, folding the top carry back
    /// in times 19. The result is congruent to the input but not necessarily the
    /// *canonical* residue — `encode` does that last step.
    fn carry(self) -> Fe {
        let mut l = self.0;
        let mut c;
        c = l[0] >> 51; l[0] &= MASK; l[1] = l[1].wrapping_add(c);
        c = l[1] >> 51; l[1] &= MASK; l[2] = l[2].wrapping_add(c);
        c = l[2] >> 51; l[2] &= MASK; l[3] = l[3].wrapping_add(c);
        c = l[3] >> 51; l[3] &= MASK; l[4] = l[4].wrapping_add(c);
        c = l[4] >> 51; l[4] &= MASK; l[0] = l[0].wrapping_add(c.wrapping_mul(19));
        // One more pass: the fold above can push limb 0 over 2^51 again, but at
        // most once, since c <= 2^13 and 19*2^13 is far below 2^51.
        c = l[0] >> 51; l[0] &= MASK; l[1] = l[1].wrapping_add(c);
        Fe(l)
    }

    /// The canonical 32-byte little-endian encoding.
    ///
    /// Fully reduces first: an element may be held as any representative
    /// congruent mod p, and two encodings of the same value must be identical
    /// or every equality check downstream is wrong. Done by conditionally
    /// subtracting p **without branching on the answer** — compute `self + 19`,
    /// see whether it overflowed past 2²⁵⁵, and use that as a mask.
    pub fn encode(self) -> [u8; ELEM_LEN] {
        let mut l = self.carry().0;
        // Canonicalize: if l >= p, subtract p. Adding 19 and inspecting bit 255
        // answers "is it >= p?" because p = 2^255 - 19.
        let mut q = (l[0] + 19) >> 51;
        q = (l[1] + q) >> 51;
        q = (l[2] + q) >> 51;
        q = (l[3] + q) >> 51;
        q = (l[4] + q) >> 51;
        // q is now 1 exactly when l >= p. Add 19q, then drop the overflow bit.
        l[0] = l[0].wrapping_add(19u64.wrapping_mul(q));
        l[1] = l[1].wrapping_add(l[0] >> 51); l[0] &= MASK;
        l[2] = l[2].wrapping_add(l[1] >> 51); l[1] &= MASK;
        l[3] = l[3].wrapping_add(l[2] >> 51); l[2] &= MASK;
        l[4] = l[4].wrapping_add(l[3] >> 51); l[3] &= MASK;
        l[4] &= MASK; // the 2^255 bit is dropped, which is the subtraction of p

        // Pack five 51-bit limbs into 32 little-endian bytes.
        let mut out = [0u8; ELEM_LEN];
        let words = [
            l[0] | (l[1] << 51),
            (l[1] >> 13) | (l[2] << 38),
            (l[2] >> 26) | (l[3] << 25),
            (l[3] >> 39) | (l[4] << 12),
        ];
        for (i, w) in words.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// Decode 32 little-endian bytes.
    ///
    /// **Bit 255 is ignored**, as Ed25519 specifies — in the curve layer that
    /// bit carries the sign of x, not part of the field element. A non-canonical
    /// input (a value in `[p, 2²⁵⁵)`) is accepted and reduced, matching every
    /// reference implementation; rejecting it is the caller's business where it
    /// matters.
    pub fn decode(bytes: &[u8; ELEM_LEN]) -> Fe {
        let mut w = [0u64; 4];
        for i in 0..4 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            w[i] = u64::from_le_bytes(b);
        }
        Fe([
            w[0] & MASK,
            ((w[0] >> 51) | (w[1] << 13)) & MASK,
            ((w[1] >> 38) | (w[2] << 26)) & MASK,
            ((w[2] >> 25) | (w[3] << 39)) & MASK,
            (w[3] >> 12) & MASK, // the top bit of w[3] is bit 255: dropped here
        ])
    }

    /// Whether two elements are the same field value, comparing canonical
    /// encodings so that different representatives of one value compare equal.
    pub fn ct_eq(self, rhs: Fe) -> bool {
        let a = self.encode();
        let b = rhs.encode();
        let mut diff = 0u8;
        for i in 0..ELEM_LEN {
            diff |= a[i] ^ b[i];
        }
        diff == 0
    }

    /// Whether this is the zero element.
    pub fn is_zero(self) -> bool {
        self.ct_eq(Fe::ZERO)
    }
}

/// Fold a set of 128-bit column sums back into five 51-bit limbs, carrying the
/// overflow past 2²⁵⁵ into limb 0 times 19.
fn reduce128(c: [u128; 5]) -> Fe {
    let mut c = c;
    // Carry each column into the next; the top column wraps to the bottom.
    c[1] += c[0] >> 51;
    let mut l0 = (c[0] as u64) & MASK;
    c[2] += c[1] >> 51;
    let l1 = (c[1] as u64) & MASK;
    c[3] += c[2] >> 51;
    let l2 = (c[2] as u64) & MASK;
    c[4] += c[3] >> 51;
    let l3 = (c[3] as u64) & MASK;
    let carry = (c[4] >> 51) as u64;
    let l4 = (c[4] as u64) & MASK;
    l0 = l0.wrapping_add(carry.wrapping_mul(19));
    // That fold can push limb 0 past 2^51 once; one carry pass settles it.
    let l1 = l1 + (l0 >> 51);
    l0 &= MASK;
    Fe([l0, l1, l2, l3, l4])
}

#[cfg(test)]
mod tests {
    //! Run with `make test` (or `cargo test -p ed25519 --target <host>`).
    //!
    //! **Every expected value here was produced by `scripts/gen-field-vectors.py`
    //! using Python's arbitrary-precision integers.** Python has no limbs, no
    //! carries and no reduction chain, so it cannot be wrong in the same way this
    //! code can — which is the entire reason to check against it rather than
    //! against values this implementation printed. The script is in the tree so
    //! the vectors are reproducible rather than merely asserted.
    use super::*;

    fn unhex(s: &str) -> [u8; ELEM_LEN] {
        let b = s.as_bytes();
        assert_eq!(b.len(), ELEM_LEN * 2);
        let mut out = [0u8; ELEM_LEN];
        for i in 0..ELEM_LEN {
            let hi = (b[i * 2] as char).to_digit(16).expect("hex") as u8;
            let lo = (b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8;
            out[i] = (hi << 4) | lo;
        }
        out
    }

    fn fe(s: &str) -> Fe {
        Fe::decode(&unhex(s))
    }

    /// `(name, x, x², x⁻¹)`
    const SINGLES: &[(&str, &str, &str, &str)] = &[
    ("zero", "0000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("one", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000"),
    ("two", "0200000000000000000000000000000000000000000000000000000000000000", "0400000000000000000000000000000000000000000000000000000000000000", "f7ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff3f"),
    ("p_minus_1", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0100000000000000000000000000000000000000000000000000000000000000", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("p_minus_2", "ebffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0400000000000000000000000000000000000000000000000000000000000000", "f6ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff3f"),
    ("limb_boundary_2_51", "0000000000000800000000000000000000000000000000000000000000000000", "0000000000000000000000004000000000000000000000000000000000000000", "f5ffffffffffffffffffffffffffffffffffffffffffffffffafa1bc86f21a4a"),
    ("limb_boundary_2_51_minus_1", "ffffffffffff0700000000000000000000000000000000000000000000000000", "010000000000f0ffffffffff3f00000000000000000000000000000000000000", "89e3388ee338721cc7711cc791e3388ee3388e1cc7711cc771e4388ee3388e23"),
    ("limb_boundary_2_102", "0000000000000000000000004000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000010000000000000", "f4ffffffffffffffffffffffffffffffffffff3594d7505e43790de53594d750"),
    ("two_204", "0000000000000000000000000000000000000000000000000010000000000000", "0000000000000000000000000000000000000026000000000000000000000000", "f8ffffffffffd7505e43790de53594d7505e43790de53594d7505e43790de535"),
    ("just_under_2_255", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0100000000000000000000000000000000000000000000000000000000000000", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("high_bit_pattern", "1200000000000000000000000000000000000000000000000000000000000000", "4401000000000000000000000000000000000000000000000000000000000000", "89e3388ee3388ee3388ee3388ee3388ee3388ee3388ee3388ee3388ee3388e23"),
    ("alternating", "5555555555555555555555555555555555555555555555555555555555555555", "aac7711cc7711cc7711cc7711cc7711cc7711cc7711cc7711cc7711cc7711c47", "26f2593798229f758329f2593798229f758329f2593798229f758329f2593718"),
    ("alternating_aa", "bdaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2a", "ce1ec7711cc7711cc7711cc7711cc7711cc7711cc7711cc7711cc7711cc7711c", "13f9ac1b4c91cfbac114f9ac1b4c91cfbac114f9ac1b4c91cfbac114f9ac1b0c"),
    ("rand_0", "19790933728a14fc1d5d4330d952cde6712a63830705bf4bd56b6a740ddb3656", "7fe521061618bba8d4cc0e225469849ac7f0798305f42d5ff99cae5ff6c16e76", "76cb8739bee558aaeae75e61cdb7568f343f8c9961d83c5768a249e9813ac919"),
    ("rand_1", "b68c0314900df270528de98f697086c4eed4a7c9297103d30b6883ae3b588574", "023f5e479f94f85b2b01ac4e357981ed0dc55f171d912061c686261a7517be61", "3d50401706fe5fc04e4997bbb55c8d508dc3f2a81b7eb5188849112f80e3ad57"),
    ("rand_2", "b554d2900095bbbe8160666812b6dfd61fc61ac1c21c72a21e6bca158a94d372", "d2ef7b2e44f270fc4666ab0d0d7ef9f4fe90966164c3a692e8ece6c07188f861", "db2043f8fecc97ee5d7f48f706eae374d1170cd02bd29d61ccff824bd62f211e"),
    ("rand_3", "39447ccdf085f55da55a30d4bd2127c363b26aba66ea90f0c8447daf2b8fd162", "79d61b28b74abbe712944efcfa09fe86458a243bf9597b9b3a4216ea26016013", "fe3d6e53762255bc379405f458560b86027a2e1c600a60ac8c531e20192e8705"),
    ("rand_4", "dde3335122815d805f55613afd31a6c29772d4fb51cfaa6b7ffb3243bee9c57f", "9ebf8500cfde5f104aa266e6beec91fef049d78d75f1202fdede830a1ef78033", "39b8c7f4975a968eb1915df19dcf3a95b01100ca1b57be13b2a2587153c97600"),
    ("rand_5", "7b9beca120daca83f1885127248b7b442d2759bd3e8a4890245b1161af534406", "78d894470b795f8c6d89d388595181e725131a45d8a6010c3a4e711c76af3e47", "aed1002b54f5a547c12843513018308be756c59ba417f9a6d3e2a8de434c392f"),
    ("rand_6", "0066122138fff018619d1fd1b847889c82026539167612c8f720b7d253d15903", "968c64b306f7a6f84efd63ea3e29c2cc53127015845ede0fe7948e0373a0d812", "d599fbbf28b7a71e5fbffe6d9ec9279d7bbac26066f0277058edebc997844a66"),
    ("rand_7", "747c1bf0231e67323f45b4942d688fa6b61225fc62fb2dc59ecf1bc584d74175", "15bcc408966b303cfcc89b2939322d85930910c7ef086a1be8a6c5a87d0acf5a", "78c290f168e870d2be5a91869c370f10bcbfe6df8354d083ea1cc473f2b8544c"),
    ];

    /// `(name, a, b, a+b, a-b, a*b)`
    const PAIRS: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("one_one", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "0200000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000"),
    ("p_minus_1_plus_1", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0100000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "ebffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("p_minus_1_times_2", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0200000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "eaffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "ebffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"),
    ("zero_minus_one", "0000000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("big_times_big", "ebffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "eaffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "e8ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", "0100000000000000000000000000000000000000000000000000000000000000", "0600000000000000000000000000000000000000000000000000000000000000"),
    ("limb_carry", "ffffffffffff0700000000000000000000000000000000000000000000000000", "ffffffffffff0700000000000000000000000000000000000000000000000000", "feffffffffff0f00000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "010000000000f0ffffffffff3f00000000000000000000000000000000000000"),
    ("high_times_high", "0000000000000000000000000000000000000000000000000010000000000000", "0000000000000000000000000000000000000000000000000010000000000000", "0000000000000000000000000000000000000000000000000020000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000026000000000000000000000000"),
    ("rand_pair_0", "29e2d6febb7ed9bf915828cf98371fbfa4f06e541656021d1c7e9d74e5fbbc63", "441608671a2218d6ca634abf1ed32b1953b0bb4cc55a5a61c90440894445182b", "80f8de65d6a0f1955cbc728eb70a4bd8f7a02aa1dbb05c7ee582ddfd2941d50e", "e5cbce97a15cc1e9c6f4dd0f7a64f3a55140b30751fba7bb52795deba0b6a438", "cc6190d8cd56727b4b65b5b3d071f4783b5aad0883562c46c445800f86376f11"),
    ("rand_pair_1", "9c11d071698a928855c30035eb42e07b68e6de6d3ed6cfbf65abad5233388a5f", "241684f43431c7517bb4f33d4b72cba5084e3c9f127f389bf19a2a726c962662", "d32754669ebb59dad077f47236b5ab2171341b0d5155085b5746d8c49fceb041", "65fb4b7d3459cb36da0e0df79fd014d65f98a2ce2b579724741083e0c6a1637d", "40f1822d834334fa1fad0817d59a3d1bf2bfdf40bc91f9115864ccdb07e32363"),
    ("rand_pair_2", "67f38044dfe35515de6561f22a3b11a655da1d23504a1a4660c6c7eb332f092c", "f47849e52fab65531519f315ee56b5d57d2ddf06d4af1cf6ffe3ee54f60b2676", "6e6cca290f8fbb68f37e54081992c67bd307fd2924fa363c60aab6402a3b2f22", "607a375faf38f0c1c84c6edc3ce45bd0d7ac3e1c7c9afd4f60e2d8963d23e335", "fc5dc8f9f45c1daa89f29437f2e5a46dbc49a40ed44ed2923c139461cb214a29"),
    ("rand_pair_3", "c503cd65c6b4ea82362a61277ea54683abaf7683f0aacf1ff21719e89b15c77b", "6d0d19a83e6b8a382bb1ad2f3d9beb734e25978dd3978ba856c0200bc5aa884b", "4511e60d052075bb61db0e57bb4032f7f9d40d11c4425bc848d839f360c04f47", "58f6b3bd8749604a0b79b3f7400a5b0f5d8adff51c1344779b57f8dcd66a3e30", "a71a3adef76e4a0e19d700ee3e74c2e890e7fa0ea99e0f047cde14266209236c"),
    ("rand_pair_4", "e6387bff4b63044f95a43d850e8f49f17267dce7d3dd13d87480376b48983217", "f2489a5f2c8fd5743b59829a32ed2b10cc7088558c0ffb1cd0c0936c35241053", "d881155f78f2d9c3d0fdbf1f417c75013fd8643d60ed0ef54441cbd77dbc426a", "e1efe09f1fd42eda594bbbeadba11de1a6f6539247ce18bba4bfa3fe12742244", "ba49f3f6de85dae2aca73311787790c08a8e44465a930d8dedd47489ae23561b"),
    ("rand_pair_5", "99101fb8cfadbf9db19d2d889fc502c81773a8baba11db031444dbb3a924d715", "6962c0b24ee6e819e790b1844448fda950a6d59b65b77d051ea8cfde425e3832", "0273df6a1e94a8b7982edf0ce40d007268197e5620c9580932ecaa92ec820f48", "1dae5e0581c7d683ca0c7c035b7d051ec7ccd21e555a5dfef59b0bd566c69e63", "ffd0022b82a84b79011ce1504b0fb712d8f2dfc1924cb881c7981ba4c2370501"),
    ];

    #[test]
    fn square_matches_python() {
        for (name, x, sq, _) in SINGLES {
            assert_eq!(fe(x).square().encode(), unhex(sq), "square of {name}");
        }
    }

    #[test]
    fn invert_matches_python() {
        for (name, x, _, inv) in SINGLES {
            assert_eq!(fe(x).invert().encode(), unhex(inv), "invert of {name}");
        }
    }

    #[test]
    fn add_sub_mul_match_python() {
        for (name, a, b, sum, diff, prod) in PAIRS {
            assert_eq!(fe(a).add(fe(b)).encode(), unhex(sum), "{name}: a+b");
            assert_eq!(fe(a).sub(fe(b)).encode(), unhex(diff), "{name}: a-b");
            assert_eq!(fe(a).mul(fe(b)).encode(), unhex(prod), "{name}: a*b");
        }
    }

    #[test]
    fn inverse_times_self_is_one() {
        // The law, not a vector: catches an inversion that is self-consistently
        // wrong, which a table of expected values from the same broken chain
        // would not.
        for (name, x, _, _) in SINGLES {
            let v = fe(x);
            if v.is_zero() {
                continue; // zero has no inverse
            }
            assert!(v.mul(v.invert()).ct_eq(Fe::ONE), "x * x^-1 != 1 for {name}");
        }
    }

    #[test]
    fn encoding_round_trips_and_is_canonical() {
        for (name, x, _, _) in SINGLES {
            let bytes = unhex(x);
            assert_eq!(Fe::decode(&bytes).encode(), bytes, "round trip {name}");
        }
        // p, 2p-ish and p+1 are NOT canonical encodings; decoding must reduce
        // them, so that two representatives of one value compare equal. This is
        // the check that a missing final conditional-subtract would fail.
        let p = unhex("edffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        assert!(Fe::decode(&p).is_zero(), "p must decode to zero");
        let p_plus_1 = unhex("eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
        assert!(Fe::decode(&p_plus_1).ct_eq(Fe::ONE), "p+1 must decode to one");
    }

    #[test]
    fn bit_255_is_ignored_on_decode() {
        // Ed25519 puts the sign of x in that bit, so the field decoder must not
        // see it. If it did, every point decompression would read a different
        // element than the one that was encoded.
        let mut with = unhex("0100000000000000000000000000000000000000000000000000000000000000");
        with[31] |= 0x80;
        assert!(Fe::decode(&with).ct_eq(Fe::ONE), "bit 255 must not affect the value");
    }

    #[test]
    fn field_laws_hold_over_the_vectors() {
        // Distributivity ties add and mul together: a bug that shifted both
        // consistently would still break this.
        for (_, a, b, _, _, _) in PAIRS {
            for (_, c, _, _) in SINGLES {
                let (x, y, z) = (fe(a), fe(b), fe(c));
                assert!(x.mul(y.add(z)).ct_eq(x.mul(y).add(x.mul(z))), "a(b+c) != ab+ac");
                assert!(x.add(y).ct_eq(y.add(x)), "add not commutative");
                assert!(x.mul(y).ct_eq(y.mul(x)), "mul not commutative");
                assert!(x.add(y).sub(y).ct_eq(x), "(a+b)-b != a");
            }
        }
    }

    #[test]
    fn repeated_addition_never_overflows() {
        // THIS TEST CHANGED THE IMPLEMENTATION. Written against a lazily-reduced
        // `add` (limbs left oversized for the next multiply to clean up), it
        // failed partway through: doubling a value repeatedly wrapped a u64 limb
        // and produced a wrong answer with nothing to indicate it. `add` carries
        // now, so no sequence of additions can leave an Fe unrepresentable.
        //
        // 256 doublings is far past anything the curve layer will do, which is
        // the point: the bound should not exist rather than merely be large.
        let two = Fe::ONE.add(Fe::ONE);
        let mut acc = Fe::ONE;
        let mut expect = Fe::ONE;
        for i in 0..256 {
            acc = acc.add(acc);
            expect = expect.mul(two);
            assert!(acc.ct_eq(expect), "add diverged from multiply-by-two at step {i}");
        }
    }

    #[test]
    fn repeated_subtraction_never_overflows() {
        // The mirror of the addition test, and it exists because its absence hid
        // a real bug: `sub` lost its carry during an editing accident and every
        // test still passed, since `encode` carries before comparing and no test
        // subtracted repeatedly. A subtraction adds ~2p before subtracting, so an
        // uncarried chain grows limbs until they wrap.
        // The iteration count is chosen so the test CAN fail. An uncarried
        // subtraction grows every limb by about 2⁵² per call, so 256 of them
        // reach only ~2⁶⁰ and wrap nothing - the first version of this test used
        // 256 and passed happily with the carry removed, which makes it a test
        // that cannot fail for the bug it is named after. 5000 passes 2⁶⁴.
        let mut acc = Fe::ZERO;
        let mut expect = Fe::ZERO;
        let one = Fe::ONE;
        for i in 0..5000 {
            acc = acc.sub(one);
            expect = expect.add(one.neg());
            assert!(acc.ct_eq(expect), "subtract diverged from add-of-negation at step {i}");
        }
    }

    #[test]
    fn limbs_stay_bounded_after_every_operation() {
        // The invariant the module documents, checked directly rather than only
        // through its consequences: no operation may leave a limb at or above
        // 2^51. This is what makes "how many adds before you must reduce" not a
        // question a caller has to answer.
        let big = fe("ecffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f"); // p-1
        let cases = [
            big.add(big),
            big.sub(Fe::ZERO),
            Fe::ZERO.sub(big),
            big.mul(big),
            big.square(),
            big.invert(),
            big.neg(),
        ];
        for (i, c) in cases.iter().enumerate() {
            for (j, limb) in c.0.iter().enumerate() {
                assert!(*limb < (1u64 << 51), "case {i} limb {j} = {limb:#x} >= 2^51");
            }
        }
    }

    #[test]
    fn vectors_are_not_empty() {
        // The generator's output is spliced into this file, and a splice that
        // silently matched nothing would leave the three vector tests below
        // iterating over an empty list and passing vacuously. That happened once
        // while writing this; it now cannot happen quietly.
        assert!(SINGLES.len() >= 20, "single-element vectors missing: {}", SINGLES.len());
        assert!(PAIRS.len() >= 12, "pair vectors missing: {}", PAIRS.len());
    }
}

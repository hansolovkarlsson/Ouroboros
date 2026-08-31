//! Arithmetic modulo **L**, the order of Ed25519's prime-order subgroup, where
//! `L = 2²⁵² + 27742317777372353535851937790883648493`. Part of step 4 of
//! `docs/roadmap-cluster-keys.md`.
//!
//! Two operations matter: reducing a 512-bit hash to a scalar, and computing
//! `s = r + k·a mod L`, which is the whole of the signing equation.
//!
//! ## The reduction is the slow, obvious one, on purpose
//!
//! Reference implementations reduce with a hand-derived chain of shifts and
//! multiplies (`sc_reduce` in ref10 is about 200 lines of unexplained
//! constants). This does bit-by-bit long division instead: 512 iterations of
//! "double the remainder, add the next bit, subtract L if it fits". That is
//! perhaps twenty times slower and it is *obviously* correct — you can check it
//! against the definition of division rather than against another
//! implementation's constants.
//!
//! The trade is deliberate and revisitable: signing does this twice, against 512
//! point operations that dominate it, and step 5 measures the whole thing on the
//! target. If the measurement says this matters, the fast chain can replace it
//! behind the same tests. Starting from the fast version would have meant
//! trusting constants I could not check.

/// Bytes in an encoded scalar.
pub const SCALAR_LEN: usize = 32;

/// L as four little-endian 64-bit words.
const L: [u64; 4] = [0x5812_631a_5cf5_d3ed, 0x14de_f9de_a2f7_9cd6, 0x0, 0x1000_0000_0000_0000];

/// An integer mod L, always reduced.
#[derive(Clone, Copy)]
pub struct Scalar([u64; 4]);

impl Scalar {
    /// Zero.
    pub const ZERO: Scalar = Scalar([0; 4]);

    /// Reduce a 512-bit hash (little-endian) mod L — how both of a signature's
    /// scalars are derived.
    pub fn from_hash(bytes: &[u8; 64]) -> Scalar {
        let mut wide = [0u64; 8];
        for (i, w) in wide.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            *w = u64::from_le_bytes(b);
        }
        reduce_wide(&wide)
    }

    /// Reduce 32 little-endian bytes mod L, accepting any input.
    pub fn from_bytes_mod_order(bytes: &[u8; SCALAR_LEN]) -> Scalar {
        let mut wide = [0u64; 8];
        for (i, w) in wide.iter_mut().take(4).enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            *w = u64::from_le_bytes(b);
        }
        reduce_wide(&wide)
    }

    /// Decode 32 little-endian bytes, **rejecting a value that is not already
    /// reduced**.
    ///
    /// This is the one a verifier must use for a signature's `s`. Accepting
    /// `s ≥ L` and reducing it would make every signature come in `2⁶⁴`-ish
    /// variants that all verify — the classic malleability, and the same shape
    /// as the non-canonical point encoding that this crate's step 3 refused.
    pub fn from_canonical_bytes(bytes: &[u8; SCALAR_LEN]) -> Option<Scalar> {
        let mut w = [0u64; 4];
        for (i, word) in w.iter_mut().enumerate() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[i * 8..i * 8 + 8]);
            *word = u64::from_le_bytes(b);
        }
        if geq_l(&w) {
            return None;
        }
        Some(Scalar(w))
    }

    /// The 32-byte little-endian encoding.
    pub fn to_bytes(self) -> [u8; SCALAR_LEN] {
        let mut out = [0u8; SCALAR_LEN];
        for (i, w) in self.0.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    /// `(a · b + c) mod L` — the signing equation, `s = r + k·a`.
    pub fn mul_add(a: Scalar, b: Scalar, c: Scalar) -> Scalar {
        // Schoolbook 4×4 → 8 limbs. Both inputs are below L < 2²⁵³, so the
        // product is below 2⁵⁰⁶ and adding c cannot reach 2⁵¹².
        let mut wide = [0u64; 8];
        for i in 0..4 {
            let mut carry: u128 = 0;
            for j in 0..4 {
                let t = (a.0[i] as u128) * (b.0[j] as u128) + (wide[i + j] as u128) + carry;
                wide[i + j] = t as u64;
                carry = t >> 64;
            }
            wide[i + 4] = wide[i + 4].wrapping_add(carry as u64);
        }
        // + c
        let mut carry = 0u64;
        for (w, ci) in wide.iter_mut().zip(c.0.iter()) {
            let (t, c1) = w.overflowing_add(*ci);
            let (t, c2) = t.overflowing_add(carry);
            *w = t;
            carry = (c1 as u64) | (c2 as u64);
        }
        for w in wide.iter_mut().skip(4) {
            let (t, c1) = w.overflowing_add(carry);
            *w = t;
            carry = c1 as u64;
            if carry == 0 {
                break;
            }
        }
        reduce_wide(&wide)
    }

    /// Whether this is zero.
    pub fn is_zero(self) -> bool {
        (self.0[0] | self.0[1] | self.0[2] | self.0[3]) == 0
    }
}

/// Whether a 4-word value is `>= L`, by borrowing subtraction (no
/// short-circuit, so the work does not depend on the value).
fn geq_l(w: &[u64; 4]) -> bool {
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d, b1) = w[i].overflowing_sub(L[i]);
        let (_, b2) = d.overflowing_sub(borrow);
        borrow = (b1 as u64) | (b2 as u64);
    }
    borrow == 0
}

/// Subtract L from `w` when `w >= L`, branchlessly.
fn sub_l_if_geq(w: &mut [u64; 4]) {
    let mut diff = [0u64; 4];
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d, b1) = w[i].overflowing_sub(L[i]);
        let (d, b2) = d.overflowing_sub(borrow);
        diff[i] = d;
        borrow = (b1 as u64) | (b2 as u64);
    }
    // borrow == 0 means w >= L, so keep `diff`; otherwise keep `w`.
    let mask = 0u64.wrapping_sub(1 - borrow); // all ones when we subtract
    for i in 0..4 {
        w[i] = w[i] ^ (mask & (w[i] ^ diff[i]));
    }
}

/// Reduce a 512-bit little-endian value mod L by long division.
///
/// The remainder never exceeds L, so doubling it stays below 2²⁵⁴ and one
/// conditional subtraction per bit is enough.
fn reduce_wide(wide: &[u64; 8]) -> Scalar {
    let mut r = [0u64; 4];
    for i in (0..512).rev() {
        let bit = (wide[i / 64] >> (i % 64)) & 1;
        // r = 2r + bit
        let mut carry = bit;
        for limb in r.iter_mut() {
            let next = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = next;
        }
        debug_assert_eq!(carry, 0, "remainder outgrew 256 bits, which cannot happen for r < L");
        sub_l_if_geq(&mut r);
    }
    Scalar(r)
}

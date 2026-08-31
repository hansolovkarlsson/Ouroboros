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
//! The trade is deliberate and revisitable. Signing runs this **four** times -
//! `from_hash` for `r`, `from_hash` for `k`, `from_bytes_mod_order` for the
//! secret scalar, and once inside `mul_add` - and verification once, against the
//! point operations that dominate both. Step 5 measures the whole thing on the
//! target with those real counts. If the measurement says this matters, the fast chain can replace it
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
        // No early exit on a zero carry. In `sign` this addition is `k·a + r`,
        // where both `a` (the secret scalar) and `r` (the secret nonce) are
        // secret, so a `break` here would make the iteration count a function of
        // secret data - the very thing `geq_l` and `sub_l_if_geq` above are
        // written branchlessly to avoid. The loop is four iterations; stopping
        // early saves nothing worth the inconsistency.
        for w in wide.iter_mut().skip(4) {
            let (t, c1) = w.overflowing_add(carry);
            *w = t;
            carry = c1 as u64;
        }
        reduce_wide(&wide)
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

#[cfg(test)]
mod tests {
    //! Direct vectors for the scalar layer.
    //!
    //! This module previously had **no tests of its own** - it was covered only
    //! indirectly through the signing vectors, so a reduction bug could be seen
    //! only as a wrong signature, and only for the handful of values a signature
    //! happens to produce. These check the reduction against Python's `%`, which
    //! shares nothing with bit-by-bit long division.
    use super::*;

    fn unhex<const N: usize>(s: &str) -> [u8; N] {
        let b = s.as_bytes();
        assert_eq!(b.len(), N * 2, "expected {N} bytes");
        let mut out = [0u8; N];
        for i in 0..N {
            out[i] = (((b[i * 2] as char).to_digit(16).expect("hex") as u8) << 4)
                | ((b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8);
        }
        out
    }

    /// `(name, 64 raw bytes, the value mod L)`
    const WIDE: &[(&str, &str, &str)] = &[
    ("zero", "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("one", "01000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000"),
    ("l_minus_1", "ecd3f55c1a631258d69cf7a2def9de14000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010"),
    ("l_itself", "edd3f55c1a631258d69cf7a2def9de14000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("l_plus_1", "eed3f55c1a631258d69cf7a2def9de14000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000"),
    ("two_l_minus_1", "d9a7ebb934c624b0ac39ef45bdf3bd29000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010"),
    ("two_l", "daa7ebb934c624b0ac39ef45bdf3bd29000000000000000000000000000000200000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("2_255", "00000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000000", "85344775474a7f9723b63a8be92ae76dffffffffffffffffffffffffffffff0f"),
    ("2_256", "00000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000000", "1d95988d7431ecd670cf7d73f45befc6feffffffffffffffffffffffffffff0f"),
    ("2_511", "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000080", "77f1c8d07e3a0cfe0e98be05c38a76f232dffa0be93976e71e4d18be8da0cc09"),
    ("all_ones_512", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", "000f9c44e31106a447938568a71b0ed065bef517d273ecce3d9a307c1b419903"),
    ("rand_0", "a4f06e541656021d1c7e9d74cbf779c7441608671a2218d6ca634abf1ed32b1953b0bb4cc55a5a61c9044089898a30569c11d071698a928855c30035eb42e07b", "4f1a0afcce8f6bccdcf21d73ff02cd1ae1396b980adf20ad55711c2fd4d5630c"),
    ("rand_1", "3ed6cfbf65abad52677014bf241684f43431c7517bb4f33d4b72cba5084e3c9f127f389bf19a2a72d92c4dc467f38044dfe35515de6561f22a3b11a655da1d23", "18da252255f1f82b4fdb745078651e91d98d76a255c2d8d57c750bfecb72d201"),
    ("rand_2", "372b8ef76d0d19a83e6b8a382bb1ad2f3d9beb734e25978dd3978ba856c0200b8b551197e6387bff4b63044f95a43d850e8f49f17267dce7d3dd13d87480376b", "a00b8c3ecbee8725c44c283edc8e31337d662e64367de9f1c088d3842c595e02"),
    ("rand_3", "4ee6e819e790b1844448fda950a6d59b65b77d051ea8cfde84bc70645d324d06ce58ff0ef085d61f68f4973986d684cf10e2748a073c247a213fc16ae555b0e7", "ef811f2b78356731574566bc1e678ba592f8477d41067fc1f6ff641a3f8c2d0e"),
    ("rand_4", "f7413642dd104d0b9c2b149baee34bf5b3356dc1cef01b8236d1bdd082460bfac442b15677a319913b4b5d14f3164d7663e98ed0069736397c7fdb1dc5ce4dc5", "c9b32f906401f11bb38431eea8ed27e8d4295738585e844de136ed97b8ad1502"),
    ("rand_5", "f7d0f99235164e8438a0084eddee90eebb3de9d97eb89c11b8419199fb01f930714f0f0f948e595d01fc0a15b2f896527ddacbc7d5ae32b5b07e8bf5bc82b9e4", "a0acd6d6461c40647291076e669107b97692865d366de13c96c73edc692b650e"),
    ("rand_6", "5b8521135a8df03061441a93d4df817b315a13fd917694a6f8cf928aadf7a897358da8c8631cdc6e047cebd9811e1aa029dad24a21a07f57b12a9749fa7b1080", "c2262eee38a522bbd7745b89d92871c75ea5c9f18554466e07de6b4f6444000e"),
    ("rand_7", "87391f85f3eaa74660de794e7d3490b28530b10e04bb2fb7452c3f4a01d2f7a335666ceeb6c95a2b779a78c5a8f09b41654ac8829835efa1adbe6cfc26e3fefc", "b668673c750c40217481ebc85f586ec51de97dda1a86a8aa5e112c8d19fc240c"),
    ];

    /// `(name, a, b, c, (a·b + c) mod L)`
    const MULADD: &[(&str, &str, &str, &str, &str)] = &[
    ("zeros", "0000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("one_one_zero", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000"),
    ("max_max_max", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("l_minus_1_times_2", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "0200000000000000000000000000000000000000000000000000000000000000", "0000000000000000000000000000000000000000000000000000000000000000", "ebd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010"),
    ("carry_chain", "0000000000000000000000000000000000000000000000000000000000000010", "0000000000000000000000000000000000000000000000000000000000000010", "0000000000000000000000000000000000000000000000000000000000000010", "7cb51c4e6b93db8a4706a17f97982453bef517d273ecce3d9a307c1b4199b301"),
    ("add_wraps", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "ebd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010"),
    ("add_wraps_past", "0100000000000000000000000000000000000000000000000000000000000000", "0100000000000000000000000000000000000000000000000000000000000000", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", "0000000000000000000000000000000000000000000000000000000000000000"),
    ("rand_0", "f0ac3ae9b86afb7e0a4b8dd7170805cf619813620e61021d6103119d1845c904", "313956366662d69c4e8b6ef028c78fd0d49ea27e43e484931d614b478deedf0a", "ef056a7613b12bec9126a3262527adb8c448199bd31bcb6c789d64632e3b5809", "cbe10bb15315989d51cf6e6258b307b1525a379f21a81e9d432cba092b3a5a03"),
    ("rand_1", "cb7b045a480a2c13f155cca211b5078779370b3029e80ef4b7bc4953f38afb03", "d2cff93a7c336338f6b90d91461e645ee123865d37b0c3b6b834d6eab904fb04", "7d7722a1c260e04fad191bcebb39b71102119f333f955d0102371a0fcba42f00", "00e4bf1639cfebc3547b386cb9babe8aefb4dd8f49d02550554b0979d8b54307"),
    ("rand_2", "1188c76cde2d3cd06e7e3ec849464ed8203ef8ffea2b1310cf00e7e674b3fd08", "69b6031bacb3f0d280f83f1df4df0e79d3f4a6e52b55e22b0f8f405fe7a2e106", "a329c0fc5ef4a3c2b5c5e06b6408bf46ed44aad1f1eb2e2bff37d2cd1a5b5803", "d80dafee643755a04a0b71ccccfc44f5b862e840bcb15f7bda003d329f9ed50c"),
    ("rand_3", "749ff80854044540e4fc9c075391efa9aee16be57c0e83c185e0fb2b225a4504", "c2319a03bbe47d27308f58fcdf645ee0db2c00668f6d0d8e64b5456805276b0e", "1477d9df89c2befc7b6b2591aa7fbf7b20f8a23a6b21241f291798f5642b8d0c", "16e75285d4ffd43b4ba10ea1fbfd52631c8da58733bd24be1402edc2ade77d0a"),
    ("rand_4", "eba4412461cc7345b798db711c5036a7416a0f543a7219bbb9938ea068e38504", "3f4960074294e4a57d21760d0221a561c700b08e7764f7c50c8b0234fc9c9a09", "cd1177629bc8657da88fa0ba480e1625da646bbfca239702e857a2eecc4bca09", "a99eae92d7214f6180dc3309a8cde4a897a28af1799259485c125f7ef0b9c602"),
    ("rand_5", "ba4279dbeef7858c0fef0ac2dfbd9f37aecadaa12dccac6412bae41c2d461805", "002f7c78302e23e96bdecfa6ab24c21f6eec5f99a1ab3809a33678aad2c12402", "a110de2969bd079651b3e2a733d4c26a4b83e72b46b418f79710a858ff18c40d", "49e8ffb1dac55fe2312ccf702f9f8388c7b81d17988661e4374e03ccedc43e0c"),
    ("rand_6", "45a2970bcc33433587517ee57ad268465e4db4b46dbc965520c17654fe26b104", "718887b943d58568df5a7614e68681872f3cdeb91ab1c04aadd9b20b9e89b20b", "60ada84ccf113499a47161e63abcf83c050a3135a982284d1566b05a7b3da40e", "bb6d004cb0e0e5f4f0a078d0be2375f389c9102c8a6c31e7c57ffd012720e203"),
    ("rand_7", "2475576f0d39acc2f0f3b084cb04de2b42b8af71a22094733630abc33ce13d0a", "229ee0b8007c59fe3b1fa2a6c24ec682771ea98f9c9d9570d7f2e8504640a30a", "c1defb7304711b37dc97ee8f7aa9f89bddcce342b0cdd756c4030d57b9e13908", "0428340a2561e2592e1fe3e5c7ca295a243f5ecbf679d5689f0caee099449207"),
    ];

    /// `(name, 32 raw bytes, whether they are a canonical scalar)`
    const CANONICAL: &[(&str, &str, bool)] = &[
    ("zero", "0000000000000000000000000000000000000000000000000000000000000000", true),
    ("one", "0100000000000000000000000000000000000000000000000000000000000000", true),
    ("l_minus_1", "ecd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", true),
    ("l_itself", "edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", false),
    ("l_plus_1", "eed3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010", false),
    ("2_255_minus_1", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f", false),
    ("all_ones", "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff", false),
    ("2_252", "0000000000000000000000000000000000000000000000000000000000000010", true),
    ];

    #[test]
    fn wide_reduction_matches_python() {
        for (name, raw, want) in WIDE {
            let got = Scalar::from_hash(&unhex::<64>(raw));
            assert_eq!(got.to_bytes(), unhex::<32>(want), "reduce {name}");
        }
    }

    #[test]
    fn mul_add_matches_python() {
        for (name, a, b, c, want) in MULADD {
            let got = Scalar::mul_add(
                Scalar::from_bytes_mod_order(&unhex::<32>(a)),
                Scalar::from_bytes_mod_order(&unhex::<32>(b)),
                Scalar::from_bytes_mod_order(&unhex::<32>(c)),
            );
            assert_eq!(got.to_bytes(), unhex::<32>(want), "mul_add {name}");
        }
    }

    #[test]
    fn canonicality_is_decided_correctly() {
        // The check a verifier leans on to refuse a malleable `s`. Both
        // directions matter: refusing everything would pass a one-sided test.
        for (name, raw, want) in CANONICAL {
            let bytes = unhex::<32>(raw);
            assert_eq!(
                Scalar::from_canonical_bytes(&bytes).is_some(),
                *want,
                "canonicality of {name}"
            );
        }
    }

    #[test]
    fn a_canonical_scalar_round_trips() {
        for (name, raw, ok) in CANONICAL {
            if !ok {
                continue;
            }
            let bytes = unhex::<32>(raw);
            let s = Scalar::from_canonical_bytes(&bytes).expect("canonical");
            assert_eq!(s.to_bytes(), bytes, "round trip {name}");
        }
    }

    #[test]
    fn reduction_is_idempotent() {
        // Reducing an already-reduced value must change nothing - the property a
        // conditional subtract that fires once too often would break.
        for (name, _raw, want) in WIDE {
            let once = unhex::<32>(want);
            let twice = Scalar::from_bytes_mod_order(&once).to_bytes();
            assert_eq!(once, twice, "reduce({name}) is not idempotent");
        }
    }

    #[test]
    fn vectors_are_not_empty() {
        assert!(WIDE.len() >= 19, "wide vectors missing: {}", WIDE.len());
        assert!(MULADD.len() >= 15, "mul_add vectors missing: {}", MULADD.len());
        assert!(CANONICAL.len() >= 8, "canonicality vectors missing: {}", CANONICAL.len());
    }
}

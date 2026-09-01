//! Ed25519 signing and verification — step 4 of
//! `docs/roadmap-cluster-keys.md`, and the arc's **go/no-go gate**.
//!
//! Everything below is RFC 8032 §5.1 with no variations. The scheme is
//! deterministic: a signature is a function of the key and the message alone,
//! with no randomness anywhere. That is the property this whole arc chose
//! Ed25519 for — hardware entropy is absent on Parallels and on the Raspberry
//! Pi, and a scheme needing a fresh random nonce per signature would leak its
//! private key the first time two signatures reused one.
//!
//! ## Choices a reader should know about
//!
//! - **Verification is cofactorless**: it checks `s·B = R + k·A`, the equation
//!   RFC 8032 gives first. The cofactored variant (`[8]s·B = [8]R + [8]k·A`)
//!   accepts a slightly larger set of signatures. Neither is wrong; they differ
//!   only for points outside the prime-order subgroup, and the stricter reading
//!   is the one to take.
//! - **`s` must be canonical.** A verifier rejects `s ≥ L` outright rather than
//!   reducing it, because reducing would make every signature come in many
//!   equally-valid encodings.
//! - **Small-order public keys ARE rejected**, in `verify_prefixed`. This bullet
//!   used to say the opposite — "not rejected … this cluster's keys are
//!   generated rather than accepted from strangers … this is the check to add
//!   if that stops being true" — and it stopped being true without anyone
//!   editing the sentence: a peer offers a public key in every signed frame,
//!   and `/etc/cluster/authorized` is a file people edit by hand. Against a
//!   small-order `A` the cofactorless equation is satisfiable with no secret at
//!   all, so such a key is a universal forgery rather than a weak one.
//!
//!   The test is `[8]A == identity`, which is small order EXACTLY — it is not a
//!   prime-order-subgroup check. A **mixed-order** key (order `8L`) still
//!   passes, and that is deliberate: rejecting it needs a full `[L]A` scalar
//!   multiplication per verification, which is the whole cost of a second
//!   signature check, and a mixed-order key is not forgeable — it still
//!   requires the discrete log of its prime part.

use crate::curve::{Point, POINT_LEN};
use crate::scalar::Scalar;
use crate::sha512::Sha512;

/// Bytes in a secret key seed.
pub const SECRET_LEN: usize = 32;
/// Bytes in a public key.
pub const PUBLIC_LEN: usize = POINT_LEN;
/// Bytes in a signature: `R` then `s`.
pub const SIGNATURE_LEN: usize = 64;

/// Expand a secret seed into the scalar `a` and the nonce prefix, per RFC 8032
/// §5.1.5: SHA-512 the seed, clamp the low half, keep the high half.
///
/// The clamping — clearing the low three bits and the top bit, setting bit 254 —
/// is what forces the scalar into the prime-order subgroup and fixes its bit
/// length, which is why it is not optional and not a detail.
fn expand(secret: &[u8; SECRET_LEN]) -> ([u8; 32], [u8; 32]) {
    let h = crate::sha512(secret);
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&h[32..]);
    (a, prefix)
}

/// The public key for a secret seed: `A = a·B`, compressed.
pub fn public_key(secret: &[u8; SECRET_LEN]) -> [u8; PUBLIC_LEN] {
    let (a, _) = expand(secret);
    Point::mul_base(&a).encode()
}

/// A secret key with its expansion and public key computed once.
///
/// **This is what a repeated signer should hold.** A scalar multiplication is
/// ~4,600 field multiplications, and a signature inherently needs one (for `R`).
/// Deriving `A` needs a second — but `A` never changes, so recomputing it per
/// signature doubles the cost of every signature for nothing. `netd` reads its
/// key once at boot and signs per frame, which is exactly the shape this serves;
/// with the free `sign` below it would have paid twice on every frame, and step
/// 5's measurement would have been of the wrong thing.
#[derive(Clone, Copy)]
pub struct SigningKey {
    /// The clamped secret scalar.
    a: [u8; 32],
    /// The nonce prefix, the high half of the expanded seed.
    prefix: [u8; 32],
    /// `A = a·B`, compressed — computed once here.
    public: [u8; PUBLIC_LEN],
}

impl SigningKey {
    /// Expand a secret seed. One scalar multiplication, paid once.
    pub fn from_secret(secret: &[u8; SECRET_LEN]) -> SigningKey {
        let (a, prefix) = expand(secret);
        let public = Point::mul_base(&a).encode();
        SigningKey { a, prefix, public }
    }

    /// The public key, already computed.
    pub fn public(&self) -> [u8; PUBLIC_LEN] {
        self.public
    }

    /// Sign `message`, returning `R ‖ s`. One scalar multiplication.
    pub fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        sign_inner(&self.a, &self.prefix, &self.public, message)
    }
}

/// Sign over `prefix ‖ message` without joining them — the counterpart of
/// [`verify_prefixed`].
///
/// # Precondition: the prefix must be FIXED-LENGTH for a given signature use
///
/// The signed input is the plain concatenation `prefix ‖ message`, with no
/// length delimiter between them. So `("ab", "c")` and `("a", "bc")` produce
/// byte-identical signatures, and a caller that lets an attacker choose where
/// the split falls has an ambiguity a signature cannot see.
///
/// Every caller here satisfies this the same way, and it is not a coincidence:
/// the verifier builds the prefix ITSELF out of fixed-width fields it reads at
/// fixed offsets (a domain tag, a 16-byte nonce, a 32-byte name), so there is no
/// split for a peer to shift — the only thing it supplies is the message tail.
/// A future caller whose prefix length varies with attacker-controlled data must
/// frame the parts (a length prefix, or a delimiter that cannot occur inside
/// them) before using this. `SIG_DOMAIN_REQUEST` in `ninep-abi` is how the tags
/// themselves are kept distinct and NUL-terminated.
///
/// The property is pinned by `splitting_the_signed_bytes_changes_nothing`: if a
/// future change ever adds framing, that test is what says so.
///
pub fn sign_prefixed(
    key: &SigningKey,
    prefix: &[u8],
    message: &[u8],
) -> [u8; SIGNATURE_LEN] {
    sign_two_part(&key.a, &key.prefix, &key.public, prefix, message)
}

/// Sign `message` with `secret`, returning `R ‖ s`.
///
/// Convenience for a one-off signature and for the test vectors. It expands the
/// key and derives the public key on every call, so a caller that signs more
/// than once should hold a [`SigningKey`] instead.
pub fn sign(secret: &[u8; SECRET_LEN], message: &[u8]) -> [u8; SIGNATURE_LEN] {
    SigningKey::from_secret(secret).sign(message)
}

fn sign_inner(
    a: &[u8; 32],
    prefix: &[u8; 32],
    public: &[u8; PUBLIC_LEN],
    message: &[u8],
) -> [u8; SIGNATURE_LEN] {
    sign_two_part(a, prefix, public, &[], message)
}

/// The signing core, over `extra ‖ message`.
fn sign_two_part(
    a: &[u8; 32],
    prefix: &[u8; 32],
    public: &[u8; PUBLIC_LEN],
    extra: &[u8],
    message: &[u8],
) -> [u8; SIGNATURE_LEN] {
    // r = H(prefix ‖ M), the deterministic nonce. Hashed incrementally because
    // the message is not adjacent to the prefix in memory and may be any length.
    let mut h = Sha512::new();
    h.update(prefix);
    h.update(extra);
    h.update(message);
    let r = Scalar::from_hash(&h.finalize());
    let r_point = Point::mul_base(&r.to_bytes()).encode();

    // k = H(R ‖ A ‖ M)
    let mut h = Sha512::new();
    h.update(&r_point);
    h.update(public);
    h.update(extra);
    h.update(message);
    let k = Scalar::from_hash(&h.finalize());

    // s = r + k·a
    let s = Scalar::mul_add(k, Scalar::from_bytes_mod_order(a), r);

    let mut sig = [0u8; SIGNATURE_LEN];
    sig[..32].copy_from_slice(&r_point);
    sig[32..].copy_from_slice(&s.to_bytes());
    sig
}

/// Verify `signature` over `message` against `public`.
///
/// Returns `false` for anything malformed — a public key or `R` that is not a
/// canonical point, an `s` that is not reduced, or an equation that does not
/// hold. Every input here is attacker-controlled, so there is no path that
/// panics and none that returns a value other than a verdict.
pub fn verify(public: &[u8; PUBLIC_LEN], message: &[u8], signature: &[u8; SIGNATURE_LEN]) -> bool {
    verify_prefixed(public, &[], message, signature)
}

/// Verify a signature over `prefix ‖ message` **without joining them in memory**.
///
/// Signed data is often a concatenation a caller does not hold contiguously —
/// the cluster's frames carry a nonce and a user name in a header, and the
/// message after a public key and the signature itself. Joining them means a
/// second buffer as large as the biggest message, which for `netd` is 2 KB on a
/// 32 KB stack that has hit its guard page five times.
///
/// It is not needed: the challenge is a *hash* of `R ‖ A ‖ M`, and SHA-512 here
/// is incremental, so the two halves are simply fed in turn — exactly what
/// signing already does with its own prefix.
///
/// # Precondition: the prefix must be FIXED-LENGTH for a given signature use
///
/// The signed input is the plain concatenation `prefix ‖ message`, with no
/// length delimiter between them. So `("ab", "c")` and `("a", "bc")` produce
/// byte-identical signatures, and a caller that lets an attacker choose where
/// the split falls has an ambiguity a signature cannot see.
///
/// Every caller here satisfies this the same way, and it is not a coincidence:
/// the verifier builds the prefix ITSELF out of fixed-width fields it reads at
/// fixed offsets (a domain tag, a 16-byte nonce, a 32-byte name), so there is no
/// split for a peer to shift — the only thing it supplies is the message tail.
/// A future caller whose prefix length varies with attacker-controlled data must
/// frame the parts (a length prefix, or a delimiter that cannot occur inside
/// them) before using this. `SIG_DOMAIN_REQUEST` in `ninep-abi` is how the tags
/// themselves are kept distinct and NUL-terminated.
///
/// The property is pinned by `splitting_the_signed_bytes_changes_nothing`: if a
/// future change ever adds framing, that test is what says so.
///
pub fn verify_prefixed(
    public: &[u8; PUBLIC_LEN],
    prefix: &[u8],
    message: &[u8],
    signature: &[u8; SIGNATURE_LEN],
) -> bool {
    let mut r_bytes = [0u8; 32];
    r_bytes.copy_from_slice(&signature[..32]);
    let mut s_bytes = [0u8; 32];
    s_bytes.copy_from_slice(&signature[32..]);

    let Some(s) = Scalar::from_canonical_bytes(&s_bytes) else {
        return false; // s >= L: refused, not reduced
    };
    let Some(a_point) = Point::decode(public) else {
        return false;
    };
    // REFUSE A SMALL-ORDER PUBLIC KEY.
    //
    // The module doc used to say this check was unnecessary because "this
    // cluster generates its own keys". That stopped being true the moment a
    // public key could arrive from outside: a peer offers one in every signed
    // frame, and `/etc/cluster/authorized` is a text file someone edits.
    //
    // Against a small-order `A` the cofactorless equation `s·B == R + k·A`
    // becomes satisfiable without knowing any secret — `s = 0` with a matching
    // small-order `R` verifies whenever `k` lands in the right residue, which
    // for an attacker who can choose or grind the message is simply a retry.
    // Such a key is not a weak credential, it is a UNIVERSAL FORGERY, so it is
    // refused here rather than at the one call site that happened to think of
    // it. An honestly generated key has prime order L and is unaffected.
    //
    // This is a SMALL-ORDER check, not prime-order-subgroup membership: a
    // mixed-order key (order 8L) passes it. See the module doc for why that
    // line is drawn here.
    if a_point.is_small_order() {
        return false;
    }
    let Some(r_point) = Point::decode(&r_bytes) else {
        return false;
    };

    let mut h = Sha512::new();
    h.update(&r_bytes);
    h.update(public);
    h.update(prefix);
    h.update(message);
    let k = Scalar::from_hash(&h.finalize());

    // s·B == R + k·A
    let lhs = Point::mul_base(&s.to_bytes());
    let rhs = r_point.add(a_point.mul(&k.to_bytes()));
    lhs.ct_eq(rhs)
}

#[cfg(test)]
mod tests {
    //! **This module is the arc's go/no-go gate.** If these do not pass, the
    //! curve arithmetic is wrong and nothing should be built on top of it.
    //!
    //! Vectors from `scripts/gen-sign-vectors.py`, which asserts that it
    //! reproduces RFC 8032 section 7.1's published signatures before emitting
    //! anything - so a failure here is this crate's, not the reference's.
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

    fn unhex_sig(s: &str) -> [u8; 64] {
        let b = s.as_bytes();
        assert_eq!(b.len(), 128, "expected 64 bytes");
        let mut out = [0u8; 64];
        for i in 0..64 {
            out[i] = (((b[i * 2] as char).to_digit(16).expect("hex") as u8) << 4)
                | ((b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8);
        }
        out
    }

    /// Variable-length message bytes, decoded into a caller-provided buffer.
    fn unhex_msg<'a>(s: &str, buf: &'a mut [u8; 256]) -> &'a [u8] {
        let b = s.as_bytes();
        let n = b.len() / 2;
        assert!(n <= 256);
        for i in 0..n {
            buf[i] = (((b[i * 2] as char).to_digit(16).expect("hex") as u8) << 4)
                | ((b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8);
        }
        &buf[..n]
    }

    /// `(name, secret, public, message, signature)`
    const VECTORS: &[(&str, &str, &str, &str, &str)] = &[
    ("rfc_test1", "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "", "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
    ("rfc_test2", "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72", "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
    ("rfc_test3", "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7", "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025", "af82", "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a"),
    ("rfc_test1024", "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5", "278117fc144c72340f67d0f2316e8386ceffbf2b2428c9c51fef7c597f1d426e", "08b8b2b733424243760fe426a4b54908632110a66c2f6591eabd3345e3e4eb98fa6e264bf09efe12ee50f8f54e9f77b1e355f6c50544e23fb1433ddf73be84d879de7c0046dc4996d9e773f4bc9efe5738829adb26c81b37c93a1b270b20329d", "f35b8b58cff047f8185f17acc239e92e43b4c6fa36468a40fa62ffc223f7cd144bcb74317d31b052a2935c1c57486a1c4705fb693fb122605ed3bb685390da01"),
    ("empty_again", "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42", "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf", "", "30ce7dc477563d2a8f88301076b790176e828ab7032f0a3f368c7691042ddbdb3fffd5e769c6a3779dac465217044de4714a422bdf812b9212ac6bf0e4b81605"),
    ("one_byte_zero", "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "00", "fb6665a8e278a0d6a80450b95d4c4ef7e4bc78694db766e16c8b754f8589e1f3b39709e7d714f8173023cf7d4f46330b216cbf4e95274b22bbd6125aa14c8f0a"),
    ("len_31", "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e", "fe1b4e95c44857b309d93f2626cae0768df8e3049f631d1abb4ae85f2090ffdb23d0f4c9f0b9ea805c21ef77990c11ee32962493722408ddbdbfee70438a3304"),
    ("len_32", "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f", "00c1db988bb12fd7351a6054ae3fac90fab7e4fc56b1651c7181f5f55f896f663933d3a90605d9058e9d0ac45950ee2d3c9c9b14857415587179fe0ccac35f09"),
    ("len_63", "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e", "b3bde4314968bbf747d560b9c32a17e830a7120bb9f1504a7dc2fa6bdc9bda6cd766ad8b675ab905e34082ee26e35b685fc702b8c2d183fc3f592ce738487f08"),
    ("len_64_block", "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f", "8b267375eab85fa027501c109b1b0e972c2db047dfa6fbebc5d9e268a9fcfded8c89e82aea3c51853d5e758296ff6617f4ac48a0beabf2d8665cb71588b4e106"),
    ("len_65", "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40", "133e2a441583375e7cf2bd75a2a1cbb14bc73ba07208553c0f8559bc068bcf91b203a83e78e055f96771294e8d827c16ffb26bbb8e9aa3532d47290e6591ab07"),
    ("len_128_two_blocks", "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb", "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f", "7a74013273bd60df81f98f5f896e7adc9df272b38d7900278053178fb451b11bfd7ae589c59f41e358c5dc24422a2cc52709d3689459c455613fd2c106e5960e"),
    ("len_200", "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60", "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "00070e151c232a31383f464d545b626970777e858c939aa1a8afb6bdc4cbd2d9e0e7eef5fc030a11181f262d343b424950575e656c737a81888f969da4abb2b9c0c7ced5dce3eaf1f8ff060d141b222930373e454c535a61686f767d848b9299a0a7aeb5bcc3cad1d8dfe6edf4fb020910171e252c333a41484f565d646b727980878e959ca3aab1b8bfc6cdd4dbe2e9f0f7fe050c131a21282f363d444b525960676e757c838a91989fa6adb4bbc2c9d0d7dee5ecf3fa01080f161d242b323940474e555c636a71", "0714de7b9c8615ef625bcea3414ee87129dd38a887ddae41701ecad54260880169d02a75bd646390ce06be3e67c0edcd9c0b4b10ef9e6c4d487bc5616a4d7b07"),
    ];

    #[test]
    fn public_keys_match_the_vectors() {
        for (name, sk, pk, _, _) in VECTORS {
            assert_eq!(public_key(&unhex(sk)), unhex(pk), "public key for {name}");
        }
    }

    #[test]
    fn signatures_match_rfc8032() {
        // THE GATE. Deterministic signing means there is exactly one right
        // answer per (key, message), so this is an equality check rather than a
        // "verifies" check - far stronger, and only possible because the scheme
        // has no randomness.
        let mut buf = [0u8; 256];
        for (name, sk, _, msg, sig) in VECTORS {
            let m = unhex_msg(msg, &mut buf);
            assert_eq!(sign(&unhex(sk), m), unhex_sig(sig), "signature for {name}");
        }
    }

    #[test]
    fn signatures_verify() {
        let mut buf = [0u8; 256];
        for (name, _, pk, msg, sig) in VECTORS {
            let m = unhex_msg(msg, &mut buf);
            assert!(verify(&unhex(pk), m, &unhex_sig(sig)), "{name} must verify");
        }
    }

    #[test]
    fn our_own_signatures_verify_at_many_lengths() {
        // Sign-then-verify over lengths that cross SHA-512 block boundaries in
        // both hashes. A vector table cannot cover every length; this can.
        let sk = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pk = public_key(&sk);
        let mut msg = [0u8; 256];
        for (i, b) in msg.iter_mut().enumerate() {
            *b = (i * 31) as u8;
        }
        for len in [0usize, 1, 31, 32, 33, 63, 64, 65, 127, 128, 129, 191, 192, 255, 256] {
            let sig = sign(&sk, &msg[..len]);
            assert!(verify(&pk, &msg[..len], &sig), "length {len} must verify");
        }
    }

    #[test]
    fn a_tampered_message_does_not_verify() {
        let sk = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pk = public_key(&sk);
        let msg = b"authorize this frame";
        let sig = sign(&sk, msg);
        assert!(verify(&pk, msg, &sig));
        let mut bad = *msg;
        bad[0] ^= 1;
        assert!(!verify(&pk, &bad, &sig), "one flipped message bit must not verify");
    }

    #[test]
    fn a_tampered_signature_does_not_verify() {
        let sk = unhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb");
        let pk = public_key(&sk);
        let msg = b"a frame worth forging";
        let good = sign(&sk, msg);
        // Every single-bit flip in the signature must be rejected. This is the
        // check that would catch a verifier that ignores part of what it reads.
        for byte in 0..SIGNATURE_LEN {
            for bit in 0..8 {
                let mut sig = good;
                sig[byte] ^= 1 << bit;
                assert!(!verify(&pk, msg, &sig), "flipping sig bit {byte}:{bit} must not verify");
            }
        }
    }

    #[test]
    fn the_wrong_public_key_does_not_verify() {
        let sk_a = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let sk_b = unhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb");
        let msg = b"signed by A, checked against B";
        let sig = sign(&sk_a, msg);
        assert!(verify(&public_key(&sk_a), msg, &sig));
        assert!(!verify(&public_key(&sk_b), msg, &sig), "B must not verify A's signature");
    }

    /// A small-order public key is a UNIVERSAL FORGERY, and must never verify.
    ///
    /// THE CANDIDATES ARE DERIVED, NOT TYPED. One order-8 generator is written
    /// down; the other seven encodings come from adding it to itself. That is
    /// what makes this list complete by construction rather than by my counting
    /// to eight — the guard this replaced was a hand-written table of these same
    /// values with three entries wrong, and its test only exercised the two that
    /// happened to be right. The generator itself is checked, not trusted: a
    /// mistyped one does not have order 8, so the subgroup does not close.
    ///
    /// TWO FORGERY FAMILIES, and the second is the one that matters. The first
    /// version of this test only built `s = 0` forgeries whose `R` also came
    /// from the small-order set — so a guard placed on `R` instead of `A` caught
    /// every case it tried, and the whole suite passed with the check on the
    /// wrong point. The large-order-`R` family has no such accident: it picks
    /// any `s`, sets `R = s·B + [(8-j) mod 8]·A`, and grinds the message until
    /// `k ≡ j (mod 8)`, at which point `R + k·A = s·B` exactly.
    #[test]
    fn a_small_order_public_key_never_verifies() {
        // The 8-torsion subgroup, generated rather than transcribed.
        let generator = Point::decode(&unhex(
            "26e8958fc2b227b045c3f489f2ef98f0d5dfac05d3c63339b13802886d53fc05",
        ))
        .expect("the generator must be a point");
        let mut subgroup = [[0u8; 32]; 8];
        let mut walk = Point::IDENTITY;
        for slot in subgroup.iter_mut() {
            *slot = walk.encode();
            walk = walk.add(generator);
        }
        assert!(walk.ct_eq(Point::IDENTITY), "the generator does not have order 8");
        for i in 0..8 {
            for j in (i + 1)..8 {
                assert_ne!(subgroup[i], subgroup[j], "subgroup did not close at 8 points");
            }
        }

        for (i, a) in subgroup.iter().enumerate() {
            let a_pt = Point::decode(a).unwrap_or_else(|| panic!("candidate {i} is not a point"));
            assert!(a_pt.is_small_order(), "candidate {i} is not actually small order");

            // Family 1: s = 0, R drawn from the subgroup.
            let mut found = false;
            'zero: for r in subgroup.iter() {
                let Some(r_pt) = Point::decode(r) else { continue };
                for n in 0..64u8 {
                    let msg = [b'm', n];
                    let k = challenge(r, a, &msg);
                    if r_pt.add(a_pt.mul(&k.to_bytes())).ct_eq(Point::IDENTITY) {
                        let mut sig = [0u8; SIGNATURE_LEN];
                        sig[..32].copy_from_slice(r); // s stays zero
                        assert!(!verify(a, &msg, &sig), "candidate {i}: s=0 FORGERY VERIFIED");
                        found = true;
                        break 'zero;
                    }
                }
            }
            assert!(found, "candidate {i}: no s=0 forgery found - that arm is vacuous");

            // Family 2: a LARGE-ORDER R, so a guard on R cannot catch it.
            let mut s_bytes = [0u8; 32];
            s_bytes[0] = 7;
            let s_b = Point::mul_base(&s_bytes);
            let mut found = false;
            'large: for j in 0..8u8 {
                let mut m = [0u8; 32];
                m[0] = (8 - j) % 8;
                let r_pt = s_b.add(a_pt.mul(&m));
                let r = r_pt.encode();
                for n in 0..64u8 {
                    let msg = [b'f', n];
                    let k = challenge(&r, a, &msg);
                    if k.to_bytes()[0] & 7 == j {
                        let mut sig = [0u8; SIGNATURE_LEN];
                        sig[..32].copy_from_slice(&r);
                        sig[32..].copy_from_slice(&s_bytes);
                        // Sanity: this really is a forgery - the equation holds.
                        assert!(
                            Point::mul_base(&s_bytes)
                                .ct_eq(r_pt.add(a_pt.mul(&k.to_bytes()))),
                            "candidate {i}: the constructed forgery does not satisfy the equation"
                        );
                        assert!(
                            !verify(a, &msg, &sig),
                            "candidate {i}: large-order-R FORGERY VERIFIED"
                        );
                        found = true;
                        break 'large;
                    }
                }
            }
            assert!(found, "candidate {i}: no large-order-R forgery found - that arm is vacuous");
        }
    }

    /// `k = H(R ‖ A ‖ M)`, the verifier's challenge — shared by the two forgery
    /// constructions above so they cannot drift from what `verify` computes.
    fn challenge(r: &[u8; 32], a: &[u8; 32], msg: &[u8]) -> Scalar {
        let mut h = Sha512::new();
        h.update(r);
        h.update(a);
        h.update(msg);
        Scalar::from_hash(&h.finalize())
    }

    /// The guard must not OVER-refuse. The table it replaced rejected `13e8…`,
    /// an ordinary valid point, which no "bad keys are refused" test could
    /// notice - so this checks the other direction, over every published key
    /// the RFC gives us.
    #[test]
    fn ordinary_public_keys_are_unaffected() {
        let mut buf = [0u8; 256];
        let mut checked = 0;
        for (name, _secret, public, message, signature) in VECTORS {
            let pk = unhex(public);
            assert!(
                !Point::decode(&pk).expect("a public key is a point").is_small_order(),
                "{name}: an ordinary public key was called small-order"
            );
            let m = unhex_msg(message, &mut buf);
            assert!(verify(&pk, m, &unhex_sig(signature)), "{name} stopped verifying");
            checked += 1;
        }
        assert!(checked >= 3, "only {checked} keys checked");
    }

    #[test]
    fn a_non_canonical_s_is_refused() {
        // s + L is a different encoding of the same scalar. Accepting it would
        // make every signature come in many valid forms - the malleability that
        // `Scalar::from_canonical_bytes` exists to refuse, and the exact mirror
        // of the non-canonical point encoding step 3 refused.
        let sk = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let pk = public_key(&sk);
        let msg = b"malleability check";
        let sig = sign(&sk, msg);
        assert!(verify(&pk, msg, &sig));

        // s + L, little-endian, as long as it fits in 32 bytes.
        const L_BYTES: [u8; 32] = [
            0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9,
            0xde, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x10,
        ];
        let mut mal = sig;
        let mut carry = 0u16;
        for i in 0..32 {
            let t = mal[32 + i] as u16 + L_BYTES[i] as u16 + carry;
            mal[32 + i] = t as u8;
            carry = t >> 8;
        }
        assert_eq!(carry, 0, "s + L must still fit in 32 bytes for this test to mean anything");
        assert_ne!(mal, sig, "the malleable form must differ from the original");
        assert!(!verify(&pk, msg, &mal), "s + L must be refused, not reduced");
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        // A verifier is handed attacker-controlled bytes. None of these may
        // panic, and none may verify.
        let pk = public_key(&unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"));
        let msg = b"anything";
        for seed in 0u8..64 {
            let mut sig = [seed; SIGNATURE_LEN];
            // MASK THE TOP BYTE of s, so s < L and verification gets PAST
            // `from_canonical_bytes` into point decoding, the challenge hash and
            // the equation - the paths "no path may panic" is actually about.
            // This line used to be `sig[63] = seed`, a no-op on an array already
            // filled with `seed`, which meant 48 of these 64 cases were refused
            // at the very first line of `verify` and proved nothing about the
            // rest of it.
            sig[63] = seed & 0x0f;
            assert!(!verify(&pk, msg, &sig));
            let bad_pk = [seed; PUBLIC_LEN];
            assert!(!verify(&bad_pk, msg, &sig));
        }
        assert!(!verify(&[0xff; PUBLIC_LEN], msg, &[0xff; SIGNATURE_LEN]));
        assert!(!verify(&[0x00; PUBLIC_LEN], msg, &[0x00; SIGNATURE_LEN]));
    }

    #[test]
    fn a_signing_key_agrees_with_the_one_shot_form() {
        // SigningKey caches the public key instead of deriving it per signature.
        // The bug that would introduce is a cached value that does not match what
        // the one-shot path computes, so check both halves: the key it reports
        // and the signatures it produces.
        let mut buf = [0u8; 256];
        for (name, sk, pk, msg, sig) in VECTORS {
            let key = SigningKey::from_secret(&unhex(sk));
            assert_eq!(key.public(), unhex(pk), "cached public key for {name}");
            let m = unhex_msg(msg, &mut buf);
            assert_eq!(key.sign(m), unhex_sig(sig), "SigningKey signature for {name}");
            assert_eq!(key.sign(m), sign(&unhex(sk), m), "must match the one-shot form");
        }
    }

    #[test]
    fn splitting_the_signed_bytes_changes_nothing() {
        // The property the two-part entry points rest on: signing or verifying
        // `prefix ‖ message` must equal doing it over the concatenation. If it
        // did not, netd would be verifying different bytes than a peer signed -
        // which looks like a crypto bug and is a plumbing one.
        let key = SigningKey::from_secret(&unhex(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        ));
        let whole = b"nonce-and-name-then-the-message";
        for split in 0..=whole.len() {
            let (a, b) = whole.split_at(split);
            let sig = sign_prefixed(&key, a, b);
            assert_eq!(sig, key.sign(whole), "split at {split} must sign the same bytes");
            assert!(verify_prefixed(&key.public(), a, b, &sig), "and verify");
            assert!(verify(&key.public(), whole, &sig), "and verify as one piece");
        }
    }

    #[test]
    fn a_prefix_is_not_interchangeable_with_the_message() {
        // Moving a byte across the boundary must not change the signed bytes -
        // checked above - but a DIFFERENT prefix must not verify.
        let key = SigningKey::from_secret(&unhex(
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        ));
        let sig = sign_prefixed(&key, b"nonce-A", b"message");
        assert!(verify_prefixed(&key.public(), b"nonce-A", b"message", &sig));
        assert!(!verify_prefixed(&key.public(), b"nonce-B", b"message", &sig));
        assert!(!verify_prefixed(&key.public(), b"nonce-A", b"messagf", &sig));
        assert!(!verify_prefixed(&key.public(), b"", b"message", &sig));
    }

    #[test]
    fn vectors_are_not_empty() {
        assert!(VECTORS.len() >= 13, "signing vectors missing: {}", VECTORS.len());
    }
}

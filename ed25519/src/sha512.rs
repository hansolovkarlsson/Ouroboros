//! SHA-512 (FIPS 180-4), the hash Ed25519 is defined over.
//!
//! Structurally the same shape as the SHA-256 in
//! `programs/servers/netd/src/hmac.rs`, with the differences that matter: 64-bit
//! words instead of 32, 80 rounds instead of 64, a 128-byte block, and a
//! 128-bit length field in the padding. Kept as its own implementation rather
//! than a generalisation of the SHA-256 one — the two share a silhouette and
//! no constants, and merging them would make both harder to check against the
//! spec.
//!
//! Incremental (`Sha512::update`) as well as one-shot, because Ed25519 always
//! hashes a *concatenation* it never has in one buffer — `dom ‖ R ‖ A ‖ M` —
//! and building that buffer would mean either a heap or a cap on message size.

/// Bytes in a SHA-512 digest.
pub const DIGEST_LEN: usize = 64;

/// Bytes in a SHA-512 compression block.
const BLOCK_LEN: usize = 128;

/// The first 64 bits of the fractional parts of the cube roots of the first 80
/// primes (FIPS 180-4 §4.2.3). A plain `u64` array: no references, so no
/// relocations — see the crate docs.
const K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
    0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694,
    0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2, 0xd5a79147930aa725, 0x06ca6351e003826f, 0x142929670a0e6e70,
    0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30,
    0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b,
    0xca273eceea26619c, 0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178,
    0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

/// The first 64 bits of the fractional parts of the square roots of the first
/// eight primes (FIPS 180-4 §5.3.5) — SHA-512's initial state. SHA-384 and the
/// truncated variants differ only here, which is why this is worth naming.
const H0: [u64; 8] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
];

/// An incremental SHA-512. Fixed size, no allocation: one block of buffer, the
/// eight-word state, and a byte count.
pub struct Sha512 {
    state: [u64; 8],
    /// Bytes buffered but not yet compressed (always `< BLOCK_LEN`).
    buf: [u8; BLOCK_LEN],
    buf_len: usize,
    /// Total message length in BYTES. FIPS 180-4 specifies a 128-bit *bit*
    /// length field; a `u64` byte count covers 2^61 bytes, so the high half of
    /// that field is always zero here and is written as such. A message that
    /// could overflow this does not exist on a machine with 512 MB of RAM.
    total_len: u64,
}

impl Default for Sha512 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha512 {
    pub const fn new() -> Self {
        Sha512 { state: H0, buf: [0u8; BLOCK_LEN], buf_len: 0, total_len: 0 }
    }

    /// Absorb `data`. Any number of calls with any split points must produce the
    /// same digest as one call with the concatenation — which is exactly what
    /// the `incremental_matches_one_shot` test checks, at every split point.
    pub fn update(&mut self, data: &[u8]) {
        self.total_len = self.total_len.wrapping_add(data.len() as u64);
        let mut rest = data;
        // Top up a partial buffer first.
        if self.buf_len > 0 {
            let take = (BLOCK_LEN - self.buf_len).min(rest.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&rest[..take]);
            self.buf_len += take;
            rest = &rest[take..];
            if self.buf_len == BLOCK_LEN {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
        }
        // Then whole blocks straight out of the input, no copying.
        while rest.len() >= BLOCK_LEN {
            let (block, tail) = rest.split_at(BLOCK_LEN);
            let mut b = [0u8; BLOCK_LEN];
            b.copy_from_slice(block);
            self.compress(&b);
            rest = tail;
        }
        // Whatever is left is shorter than a block.
        if !rest.is_empty() {
            self.buf[..rest.len()].copy_from_slice(rest);
            self.buf_len = rest.len();
        }
    }

    /// Pad and produce the digest. Takes `self` by value: a SHA-512 state is
    /// finished exactly once, and consuming it makes "finalize then keep
    /// updating" un-writable rather than merely wrong.
    pub fn finalize(mut self) -> [u8; DIGEST_LEN] {
        // Padding: 0x80, then zeros, until 16 bytes remain in the block, then the
        // 128-bit big-endian bit length.
        let bit_len = self.total_len.wrapping_mul(8);
        let mut pad = [0u8; BLOCK_LEN * 2];
        pad[0] = 0x80;
        // Bytes of padding needed so that (buf_len + 1 + zeros) ≡ 112 mod 128.
        let rem = self.buf_len % BLOCK_LEN;
        let zeros = if rem < 112 { 112 - rem - 1 } else { BLOCK_LEN + 112 - rem - 1 };
        let end = 1 + zeros;
        // High 64 bits of the bit length: always zero here (see `total_len`).
        pad[end..end + 8].copy_from_slice(&0u64.to_be_bytes());
        pad[end + 8..end + 16].copy_from_slice(&bit_len.to_be_bytes());
        let pad_len = end + 16;
        // `update` would add the padding to total_len; drive the buffer directly.
        let mut rest: &[u8] = &pad[..pad_len];
        loop {
            let take = (BLOCK_LEN - self.buf_len).min(rest.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&rest[..take]);
            self.buf_len += take;
            rest = &rest[take..];
            if self.buf_len == BLOCK_LEN {
                let block = self.buf;
                self.compress(&block);
                self.buf_len = 0;
            }
            if rest.is_empty() {
                break;
            }
        }
        debug_assert_eq!(self.buf_len, 0, "padding must land on a block boundary");
        let mut out = [0u8; DIGEST_LEN];
        for (i, w) in self.state.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&w.to_be_bytes());
        }
        out
    }

    /// The FIPS 180-4 §6.4.2 compression function over one 128-byte block.
    fn compress(&mut self, block: &[u8; BLOCK_LEN]) {
        // Message schedule. 80 words: the first 16 are the block, the rest are
        // derived. Held in a fixed array - no heap, and bounded stack.
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&block[i * 8..i * 8 + 8]);
            w[i] = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..80 {
            let big_s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(big_s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let big_s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = big_s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        let add = [a, b, c, d, e, f, g, h];
        for (s, v) in self.state.iter_mut().zip(add.iter()) {
            *s = s.wrapping_add(*v);
        }
    }
}

/// One-shot SHA-512.
pub fn sha512(data: &[u8]) -> [u8; DIGEST_LEN] {
    let mut h = Sha512::new();
    h.update(data);
    h.finalize()
}

#[cfg(test)]
mod tests {
    //! Run with: `cargo test -p ed25519 --target aarch64-apple-darwin`
    //!
    //! **Every expected digest below came from Python's `hashlib`**, not from
    //! this code and not from memory. That is the point: a vector produced by
    //! the implementation it checks proves only that the code is consistent with
    //! itself, which is how a magic-byte transposition once passed a
    //! Python-to-Python cluster-auth test on both sides at once.
    use super::*;

    /// Decode a hex digest into bytes (test-only; the crate proper has no
    /// string handling).
    fn unhex(s: &str) -> [u8; DIGEST_LEN] {
        let b = s.as_bytes();
        assert_eq!(b.len(), DIGEST_LEN * 2, "digest must be 128 hex chars");
        let mut out = [0u8; DIGEST_LEN];
        for i in 0..DIGEST_LEN {
            let hi = (b[i * 2] as char).to_digit(16).expect("hex") as u8;
            let lo = (b[i * 2 + 1] as char).to_digit(16).expect("hex") as u8;
            out[i] = (hi << 4) | lo;
        }
        out
    }

    /// `(name, message, expected)`. The lengths are chosen around the padding
    /// boundaries, which is where a SHA-512 goes wrong if it goes wrong: the
    /// 128-byte block ends with a 16-byte length field, so 111 bytes is the
    /// longest message whose padding still fits its own block, and 112 is the
    /// first that spills into another one.
    fn vectors() -> [(&'static str, &'static [u8], &'static str); 13] {
        [
            ("empty", b"", "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"),
            ("abc", b"abc", "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"),
            ("one_byte", b"a", "1f40fc92da241694750979ee6cf582f2d5d7d28e18335de05abc54d0560e0f5302860c652bf08d560252aa5e74210546f369fbbbce8c12cfc7957b2652fe9a75"),
            ("len_55", &[b'a'; 55], "b0220c772cbf6c1822e2cb38a437d0e1d58772417a4bbb21c961364f8b6143e05aa6316dca8d1d7b19e16448419076395f6086cb55101fbd6d5497b148e1745f"),
            ("len_111_last_fit", &[b'a'; 111], "fa9121c7b32b9e01733d034cfc78cbf67f926c7ed83e82200ef86818196921760b4beff48404df811b953828274461673c68d04e297b0eb7b2b4d60fc6b566a2"),
            ("len_112_spills", &[b'a'; 112], "c01d080efd492776a1c43bd23dd99d0a2e626d481e16782e75d54c2503b5dc32bd05f0f1ba33e568b88fd2d970929b719ecbb152f58f130a407c8830604b70ca"),
            ("len_119", &[b'a'; 119], "130396a75cb483f2eee8c56d8a668bb3d2641f5243212c0bee2bd33da096ad9eb8179fe18f9eaacf76e09fae9de4c3f14ba13341e345be05bf76c182cc3468cb"),
            ("len_127", &[b'a'; 127], "828613968b501dc00a97e08c73b118aa8876c26b8aac93df128502ab360f91bab50a51e088769a5c1eff4782ace147dce3642554199876374291f5d921629502"),
            ("len_128_exact", &[b'a'; 128], "b73d1929aa615934e61a871596b3f3b33359f42b8175602e89f7e06e5f658a243667807ed300314b95cacdd579f3e33abdfbe351909519a846d465c59582f321"),
            ("len_129", &[b'a'; 129], "4f681e0bd53cda4b5a2041cc8a06f2eabde44fb16c951fbd5b87702f07aeab611565b19c47fde30587177ebb852e3971bbd8d3fd30da18d71037dfbd98420429"),
            ("nist_448bit", b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "204a8fc6dda82f0a0ced7beb8e08a41657c16ef468b228a8279be331a703c33596fd15c13b1b07f9aa1d3bea57789ca031ad85c7a71dd70354ec631238ca3445"),
            ("nist_896bit", b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu", "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"),
            ("binary_0_255", BINARY_0_255, "1e7b80bc8edc552c8feeb2780e111477e5bc70465fac1a77b29b35980c3f0ce4a036a6c9462036824bd56801e62af7e9feba5c22ed8a5af877bf7de117dcac6d"),
        ]
    }

    /// Every byte value 0..=255, so the vectors are not all printable ASCII —
    /// a high-bit handling bug would otherwise go unseen.
    static BINARY_0_255: &[u8; 256] = &{
        let mut b = [0u8; 256];
        let mut i = 0;
        while i < 256 {
            b[i] = i as u8;
            i += 1;
        }
        b
    };

    #[test]
    fn one_shot_matches_published_vectors() {
        for (name, msg, expected) in vectors() {
            assert_eq!(sha512(msg), unhex(expected), "vector {name} (len {})", msg.len());
        }
    }

    #[test]
    fn incremental_matches_one_shot_at_every_split() {
        // The bug this catches is a buffering error that only shows when a
        // message arrives split across a block boundary - which is precisely how
        // Ed25519 will use this (dom ‖ R ‖ A ‖ M, four separate updates).
        for (name, msg, _) in vectors() {
            let want = sha512(msg);
            for split in 0..=msg.len() {
                let mut h = Sha512::new();
                h.update(&msg[..split]);
                h.update(&msg[split..]);
                assert_eq!(h.finalize(), want, "vector {name}, split at {split}");
            }
        }
    }

    #[test]
    fn many_small_updates_match_one_shot() {
        // One byte at a time: exercises the top-up path on every call, and would
        // catch a `total_len` that counted blocks rather than bytes.
        for (name, msg, _) in vectors() {
            let mut h = Sha512::new();
            for b in msg.iter() {
                h.update(&[*b]);
            }
            assert_eq!(h.finalize(), sha512(msg), "vector {name} byte-at-a-time");
        }
    }

    #[test]
    fn long_message_spanning_many_blocks() {
        // A million 'a' - FIPS 180-4's own long-message case, and the only test
        // here that exercises the length field beyond a single block count.
        // Expected value from Python's hashlib.
        let mut h = Sha512::new();
        let chunk = [b'a'; 1000];
        for _ in 0..1000 {
            h.update(&chunk);
        }
        assert_eq!(
            h.finalize(),
            unhex("e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973ebde0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b")
        );
    }

    #[test]
    fn a_changed_bit_changes_the_digest() {
        // Not a spec check - a sanity check that the function is actually
        // reading its whole input. A digest that ignores the last byte would
        // pass every fixed vector above whose length happened to be a block
        // multiple, and this fails immediately.
        let a = sha512(b"the quick brown fox");
        let b = sha512(b"the quick brown fox!");
        let c = sha512(b"the quick brown box");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
    }
}

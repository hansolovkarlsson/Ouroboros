//! Hand-rolled SHA-256 for password verification in the shell's `login`.
//!
//! Copied verbatim from `netd/src/hmac.rs`'s SHA-256 (NIST-validated there); the
//! shell needs the bare hash (no HMAC) to check `/etc/passwd` password hashes,
//! and pulling in netd's crate isn't possible. A shared `sha256` crate would
//! dedup the two copies - a worthwhile later cleanup, deliberately deferred to
//! keep the login step from touching the shipped cluster-auth code. `no_std`,
//! no heap, byte-only so it's PIE-relocation-safe.

/// SHA-256 round constants (FIPS 180-4).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 initial hash values (FIPS 180-4).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const BLOCK: usize = 64;
/// The SHA-256 digest size (bytes).
pub const DIGEST: usize = 32;

struct Sha256 {
    h: [u32; 8],
    block: [u8; BLOCK],
    fill: usize,
    len: u64,
}

impl Sha256 {
    fn new() -> Self {
        Sha256 { h: H0, block: [0u8; BLOCK], fill: 0, len: 0 }
    }

    fn update(&mut self, data: &[u8]) {
        self.len = self.len.wrapping_add(data.len() as u64);
        let mut i = 0;
        while i < data.len() {
            let take = (BLOCK - self.fill).min(data.len() - i);
            self.block[self.fill..self.fill + take].copy_from_slice(&data[i..i + take]);
            self.fill += take;
            i += take;
            if self.fill == BLOCK {
                let blk = self.block;
                compress(&mut self.h, &blk);
                self.fill = 0;
            }
        }
    }

    fn finalize(mut self) -> [u8; DIGEST] {
        let bitlen = self.len.wrapping_mul(8);
        self.update(&[0x80u8]);
        while self.fill != BLOCK - 8 {
            self.update(&[0u8]);
        }
        self.update(&bitlen.to_be_bytes());
        let mut out = [0u8; DIGEST];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

fn compress(h: &mut [u32; 8], block: &[u8; BLOCK]) {
    let mut w = [0u32; 64];
    for t in 0..16 {
        w[t] = u32::from_be_bytes([block[t * 4], block[t * 4 + 1], block[t * 4 + 2], block[t * 4 + 3]]);
    }
    for t in 16..64 {
        let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
        let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
        w[t] = w[t - 16].wrapping_add(s0).wrapping_add(w[t - 7]).wrapping_add(s1);
    }
    let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
    let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
    for t in 0..64 {
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh.wrapping_add(big_s1).wrapping_add(ch).wrapping_add(K[t]).wrapping_add(w[t]);
        let big_s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = big_s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
}

/// One-shot SHA-256 of `part1 || part2` (two parts so `login` can hash
/// `salt || password` without a concat buffer).
pub fn sha256_two(part1: &[u8], part2: &[u8]) -> [u8; DIGEST] {
    let mut s = Sha256::new();
    s.update(part1);
    s.update(part2);
    s.finalize()
}

/// Constant-time equality of a computed digest against a stored one (no early
/// out on the first differing byte - a password check is exactly where a timing
/// side channel matters).
pub fn digest_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != DIGEST || b.len() != DIGEST {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..DIGEST {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

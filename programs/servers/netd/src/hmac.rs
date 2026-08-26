//! Hand-rolled SHA-256 + HMAC-SHA256 for the cluster-authentication arc
//! (the export-hardening phase; see `docs/roadmap-cluster.md`'s security
//! section). The distributed export (port 564) authenticates every inbound
//! request with a client-nonce MAC: a request carries `[nonce][mac]` where
//! `mac = HMAC(cluster_key, nonce || np_body)`, so the shared cluster secret
//! never crosses the wire, and an unauthorized peer (one without the key)
//! cannot forge a request.
//!
//! Hand-rolled, `no_std`, no heap, no crate - the same discipline as every
//! other primitive in this project (the FAT32 / ACPI / virtio precedent).
//! Byte-only and bounded, so it is PIE-relocation-safe (no `str` indexing, no
//! `fmt`): the `K`/`H0` tables are `const` scalar arrays that live in
//! `.rodata` and need no relocation.
//!
//! Incremental by design (a 64-byte block buffer, no large stack scratch): a
//! request body can be a couple of kilobytes, and HMAC feeds the key pad then
//! the message, so a one-shot "pad the whole thing in a buffer" API would
//! either bloat the stack or cap the message. `update`/`finalize` stream it.
//!
//! Validated against NIST SHA-256 and RFC 4231 HMAC-SHA256 known-answer
//! vectors (the "foreign observer" the project's testing discipline calls
//! for) - see the `sha256_selftest` / `hmac_selftest` functions, exercised by
//! a host harness during development.

/// SHA-256 round constants (first 32 bits of the fractional parts of the cube
/// roots of the first 64 primes). FIPS 180-4.
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

/// SHA-256 initial hash values (first 32 bits of the fractional parts of the
/// square roots of the first 8 primes). FIPS 180-4.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// The SHA-256 block size (bytes) - also the HMAC key-padding width.
pub const BLOCK: usize = 64;
/// The SHA-256 digest size (bytes) - also the HMAC/MAC output width.
pub const DIGEST: usize = 32;

/// An incremental SHA-256 state: absorb with [`Sha256::update`], read the
/// digest with [`Sha256::finalize`]. Buffers a partial 64-byte block so the
/// caller can feed arbitrary-length slices without any large scratch buffer.
pub struct Sha256 {
    h: [u32; 8],
    /// Partial-block staging (0..BLOCK bytes valid).
    block: [u8; BLOCK],
    /// Bytes currently buffered in `block`.
    fill: usize,
    /// Total message length in bytes (for the length padding).
    len: u64,
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 { h: H0, block: [0u8; BLOCK], fill: 0, len: 0 }
    }

    /// Absorb `data`, compressing every complete 64-byte block.
    pub fn update(&mut self, data: &[u8]) {
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

    /// Pad (0x80, zeros, then the 64-bit big-endian bit length) and return the
    /// 32-byte digest. Consumes the state.
    pub fn finalize(mut self) -> [u8; DIGEST] {
        let bitlen = self.len.wrapping_mul(8);
        // Append the 0x80 terminator.
        let one = [0x80u8];
        self.update(&one);
        // Zero-pad until 8 bytes short of a block boundary.
        let zero = [0u8; 1];
        while self.fill != BLOCK - 8 {
            self.update(&zero);
        }
        // The 64-bit big-endian bit length completes the final block.
        let lenbytes = bitlen.to_be_bytes();
        self.update(&lenbytes);
        // `fill` is now 0 (the last block was just compressed).
        let mut out = [0u8; DIGEST];
        for (i, word) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

/// Compress one 64-byte block into the hash state `h` (FIPS 180-4 §6.2.2).
fn compress(h: &mut [u32; 8], block: &[u8; BLOCK]) {
    let mut w = [0u32; 64];
    for t in 0..16 {
        w[t] = u32::from_be_bytes([
            block[t * 4],
            block[t * 4 + 1],
            block[t * 4 + 2],
            block[t * 4 + 3],
        ]);
    }
    for t in 16..64 {
        let s0 = w[t - 15].rotate_right(7) ^ w[t - 15].rotate_right(18) ^ (w[t - 15] >> 3);
        let s1 = w[t - 2].rotate_right(17) ^ w[t - 2].rotate_right(19) ^ (w[t - 2] >> 10);
        w[t] = w[t - 16]
            .wrapping_add(s0)
            .wrapping_add(w[t - 7])
            .wrapping_add(s1);
    }
    let mut a = h[0];
    let mut b = h[1];
    let mut c = h[2];
    let mut d = h[3];
    let mut e = h[4];
    let mut f = h[5];
    let mut g = h[6];
    let mut hh = h[7];
    for t in 0..64 {
        let big_s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(big_s1)
            .wrapping_add(ch)
            .wrapping_add(K[t])
            .wrapping_add(w[t]);
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

/// One-shot SHA-256 of `data`.
pub fn sha256(data: &[u8]) -> [u8; DIGEST] {
    let mut s = Sha256::new();
    s.update(data);
    s.finalize()
}

/// HMAC-SHA256 over two message parts `(part1 || part2)` with `key` (RFC 2104).
/// Two parts because our MAC covers `nonce || np_body` - two non-contiguous
/// slices - and streaming them avoids a concat buffer. A key longer than the
/// block is first hashed (RFC 2104); our cluster keys are <= BLOCK, but the
/// branch is kept for correctness.
pub fn hmac_sha256(key: &[u8], part1: &[u8], part2: &[u8]) -> [u8; DIGEST] {
    // Normalize the key to a full block (hash if oversized, then zero-pad).
    let mut k0 = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hk = sha256(key);
        k0[..DIGEST].copy_from_slice(&hk);
    } else {
        k0[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] = k0[i] ^ 0x36;
        opad[i] = k0[i] ^ 0x5c;
    }
    // inner = H(ipad || part1 || part2)
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(part1);
    inner.update(part2);
    let inner = inner.finalize();
    // outer = H(opad || inner)
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(&inner);
    outer.finalize()
}

/// Constant-time equality of two `DIGEST`-sized MACs (no early-out on the first
/// differing byte - a timing side channel a naive `==` would open on the auth
/// check). Bounded, byte-only.
pub fn mac_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != DIGEST || b.len() != DIGEST {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..DIGEST {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

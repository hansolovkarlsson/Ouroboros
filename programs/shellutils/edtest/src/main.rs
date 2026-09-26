//! `/bin/edtest` — the on-target check for the `ed25519` crate.
//!
//! Step 5 of `docs/roadmap/roadmap-cluster-keys.md`, and the step that decides whether
//! the design survives contact with the machine. The host tests prove the
//! arithmetic; this proves three things they cannot:
//!
//! 1. **The same vectors pass on the target.** Different pointer width, a
//!    different code generator, and a loader that relocates the binary — none of
//!    which the host exercises.
//! 2. **Peak stack use**, measured rather than assumed. `netd` has the same
//!    fixed stack every task gets (the loader's `STACK_PAGES`; this program
//!    prints the size it measures against, asked for at runtime) and has
//!    hit its guard page five times in this project's history; a signature
//!    verification is the largest computation it would ever have done.
//! 3. **Time per operation**, so the decision to use bit-by-bit scalar reduction
//!    and no fixed-base table rests on a number rather than a guess.
//!
//! Since step 1 of `docs/roadmap/roadmap-session-auth.md` it checks **X25519**
//! the same three ways: RFC 7748 section 6.1's exchange on the target, the time
//! of one scalar multiplication, and its peak stack on its own, calibrated
//! separately, because that figure is what the arc's stack gate adds to `netd`'s
//! depth at the three places a session key will be computed.
//!
//! Since step 2 of the same plan it checks **HMAC-SHA-512** too: RFC 4231's
//! test case 2 on the target, and the time of one MAC over a full
//! `NP_NET_MAX` message, which is that step's gate (well under one sign).
//!
//! Since step 0 of `docs/roadmap/roadmap-user-keys.md` it times **a PBKDF2
//! iteration**: the two SHA-512 compressions of an HMAC whose key pads were
//! absorbed once, which is what a login will pay per iteration, so the
//! iteration count (a wire constant, fixed forever) is chosen from a figure
//! measured on the slowest platform rather than estimated from another one.
//!
//! It stays in `/bin` rather than being a throwaway: the Raspberry Pi bring-up
//! will want exactly this, on a third code generator and real hardware.

#![no_std]
#![no_main]

use ed25519::{
    hmac_sha512, public_key, sign, verify, x25519_public, x25519_shared, HmacSha512, Sha512, SigningKey,
    HMAC_LEN,
};

/// RFC 8032 §7.1 TEST 1 — the same vector the host tests use, so a disagreement
/// between host and target is visible as a disagreement about a published value
/// rather than about something this project made up.
const SK1: [u8; 32] = [
    0x9d, 0x61, 0xb1, 0x9d, 0xef, 0xfd, 0x5a, 0x60, 0xba, 0x84, 0x4a, 0xf4, 0x92, 0xec, 0x2c, 0xc4,
    0x44, 0x49, 0xc5, 0x69, 0x7b, 0x32, 0x69, 0x19, 0x70, 0x3b, 0xac, 0x03, 0x1c, 0xae, 0x7f, 0x60,
];
const PK1: [u8; 32] = [
    0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64, 0x07, 0x3a,
    0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68, 0xf7, 0x07, 0x51, 0x1a,
];
const SIG1: [u8; 64] = [
    0xe5, 0x56, 0x43, 0x00, 0xc3, 0x60, 0xac, 0x72, 0x90, 0x86, 0xe2, 0xcc, 0x80, 0x6e, 0x82, 0x8a,
    0x84, 0x87, 0x7f, 0x1e, 0xb8, 0xe5, 0xd9, 0x74, 0xd8, 0x73, 0xe0, 0x65, 0x22, 0x49, 0x01, 0x55,
    0x5f, 0xb8, 0x82, 0x15, 0x90, 0xa3, 0x3b, 0xac, 0xc6, 0x1e, 0x39, 0x70, 0x1c, 0xf9, 0xb4, 0x6b,
    0xd2, 0x5b, 0xf5, 0xf0, 0x59, 0x5b, 0xbe, 0x24, 0x65, 0x51, 0x41, 0x43, 0x8e, 0x7a, 0x10, 0x0b,
];

/// RFC 8032 §7.1 TEST 2: a one-byte message, so the vectors cover both an empty
/// message and a hashed one.
const SK2: [u8; 32] = [
    0x4c, 0xcd, 0x08, 0x9b, 0x28, 0xff, 0x96, 0xda, 0x9d, 0xb6, 0xc3, 0x46, 0xec, 0x11, 0x4e, 0x0f,
    0x5b, 0x8a, 0x31, 0x9f, 0x35, 0xab, 0xa6, 0x24, 0xda, 0x8c, 0xf6, 0xed, 0x4f, 0xb8, 0xa6, 0xfb,
];
const SIG2: [u8; 64] = [
    0x92, 0xa0, 0x09, 0xa9, 0xf0, 0xd4, 0xca, 0xb8, 0x72, 0x0e, 0x82, 0x0b, 0x5f, 0x64, 0x25, 0x40,
    0xa2, 0xb2, 0x7b, 0x54, 0x16, 0x50, 0x3f, 0x8f, 0xb3, 0x76, 0x22, 0x23, 0xeb, 0xdb, 0x69, 0xda,
    0x08, 0x5a, 0xc1, 0xe4, 0x3e, 0x15, 0x99, 0x6e, 0x45, 0x8f, 0x36, 0x13, 0xd0, 0xf1, 0x1d, 0x8c,
    0x38, 0x7b, 0x2e, 0xae, 0xb4, 0x30, 0x2a, 0xee, 0xb0, 0x0d, 0x29, 0x16, 0x12, 0xbb, 0x0c, 0x00,
];

/// RFC 7748 section 6.1: Alice's private key, Alice's public key, Bob's public
/// key and the secret they share. The host tests use the same four values.
const X_A: [u8; 32] = [
    0x77, 0x07, 0x6d, 0x0a, 0x73, 0x18, 0xa5, 0x7d, 0x3c, 0x16, 0xc1, 0x72, 0x51, 0xb2, 0x66, 0x45,
    0xdf, 0x4c, 0x2f, 0x87, 0xeb, 0xc0, 0x99, 0x2a, 0xb1, 0x77, 0xfb, 0xa5, 0x1d, 0xb9, 0x2c, 0x2a,
];
const X_A_PUB: [u8; 32] = [
    0x85, 0x20, 0xf0, 0x09, 0x89, 0x30, 0xa7, 0x54, 0x74, 0x8b, 0x7d, 0xdc, 0xb4, 0x3e, 0xf7, 0x5a,
    0x0d, 0xbf, 0x3a, 0x0d, 0x26, 0x38, 0x1a, 0xf4, 0xeb, 0xa4, 0xa9, 0x8e, 0xaa, 0x9b, 0x4e, 0x6a,
];
const X_B_PUB: [u8; 32] = [
    0xde, 0x9e, 0xdb, 0x7d, 0x7b, 0x7d, 0xc1, 0xb4, 0xd3, 0x5b, 0x61, 0xc2, 0xec, 0xe4, 0x35, 0x37,
    0x3f, 0x83, 0x43, 0xc8, 0x5b, 0x78, 0x67, 0x4d, 0xad, 0xfc, 0x7e, 0x14, 0x6f, 0x88, 0x2b, 0x4f,
];
const X_K: [u8; 32] = [
    0x4a, 0x5d, 0x9d, 0x5b, 0xa4, 0xce, 0x2d, 0xe1, 0x72, 0x8e, 0x3b, 0xf4, 0x80, 0x35, 0x0f, 0x25,
    0xe0, 0x7e, 0x21, 0xc9, 0x47, 0xd1, 0x9e, 0x33, 0x76, 0xf0, 0x9b, 0x3c, 0x1e, 0x16, 0x17, 0x42,
];

/// RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?", and
/// its HMAC-SHA-512. The host tests use the same row.
const HMAC_KEY2: &[u8] = b"Jefe";
const HMAC_DATA2: &[u8] = b"what do ya want for nothing?";
const HMAC_MAC2: [u8; 64] = [
    0x16, 0x4b, 0x7a, 0x7b, 0xfc, 0xf8, 0x19, 0xe2, 0xe3, 0x95, 0xfb, 0xe7, 0x3b, 0x56, 0xe0, 0xa3,
    0x87, 0xbd, 0x64, 0x22, 0x2e, 0x83, 0x1f, 0xd6, 0x10, 0x27, 0x0c, 0xd7, 0xea, 0x25, 0x05, 0x54,
    0x97, 0x58, 0xbf, 0x75, 0xc0, 0x5a, 0x99, 0x4a, 0x6d, 0x03, 0x4f, 0x65, 0xf8, 0xf0, 0xe6, 0xfd,
    0xca, 0xea, 0xb1, 0xa3, 0x4d, 0x4a, 0x6b, 0x4b, 0x63, 0x6e, 0x07, 0x0a, 0x38, 0xbc, 0xe7, 0x37,
];

/// Which computation a stack measurement runs.
#[derive(Clone, Copy)]
enum Op {
    /// Key expansion, a sign and a verify: the export's per-request cost today.
    SignVerify,
    /// One X25519 shared-secret computation: one scalar multiplication.
    X25519,
    /// One HMAC-SHA-512 over a full `NP_NET_MAX` message.
    Hmac,
}

/// The byte a stack probe is painted with, chosen as an unlikely value to write
/// by accident.
const PAINT: u8 = 0xA5;

/// The loader jumps to the region's base, so `_start` must be the first thing in
/// `.text` - the invariant `programs/linker.ld` keeps trivially true and which
/// `docs/processes.md` lists as a required step. It works without this only
/// because the loader happens to add `e_entry`; anything assuming entry-at-zero
/// (a raw-image path, the Pi bring-up) would jump into the middle of `.text`.
#[link_section = ".text.start"]
#[no_mangle]
pub extern "C" fn _start() -> ! {
    // Write through `stdout_target`, not straight to the console: a program that
    // only ever calls `con_write` and never sends end-of-stream leaves
    // `edtest | grep peak` hanging forever in the consumer's `pipe_recv`.
    let target = ulib::stdout_target();
    out(target, b"edtest: ed25519 on the target\r\n");
    let mut failures = 0u32;

    // --- 1. the published vectors, on this machine -------------------------
    if public_key(&SK1) == PK1 {
        out(target, b"  [ok]   public key matches RFC 8032 TEST 1\r\n");
    } else {
        out(target, b"  [FAIL] public key differs from RFC 8032 TEST 1\r\n");
        failures += 1;
    }
    if sign(&SK1, b"") == SIG1 {
        out(target, b"  [ok]   signature matches RFC 8032 TEST 1 (empty message)\r\n");
    } else {
        out(target, b"  [FAIL] signature differs from RFC 8032 TEST 1\r\n");
        failures += 1;
    }
    if sign(&SK2, &[0x72]) == SIG2 {
        out(target, b"  [ok]   signature matches RFC 8032 TEST 2 (one-byte message)\r\n");
    } else {
        out(target, b"  [FAIL] signature differs from RFC 8032 TEST 2\r\n");
        failures += 1;
    }
    if verify(&PK1, b"", &SIG1) {
        out(target, b"  [ok]   verify accepts a good signature\r\n");
    } else {
        out(target, b"  [FAIL] verify rejected a good signature\r\n");
        failures += 1;
    }
    let mut bad = SIG1;
    bad[0] ^= 1;
    if !verify(&PK1, b"", &bad) {
        out(target, b"  [ok]   verify rejects a flipped bit\r\n");
    } else {
        out(target, b"  [FAIL] verify accepted a tampered signature\r\n");
        failures += 1;
    }
    if x25519_public(&X_A) == X_A_PUB {
        out(target, b"  [ok]   X25519 public key matches RFC 7748 6.1\r\n");
    } else {
        out(target, b"  [FAIL] X25519 public key differs from RFC 7748 6.1\r\n");
        failures += 1;
    }
    if x25519_shared(&X_A, &X_B_PUB) == Some(X_K) {
        out(target, b"  [ok]   X25519 shared secret matches RFC 7748 6.1\r\n");
    } else {
        out(target, b"  [FAIL] X25519 shared secret differs from RFC 7748 6.1\r\n");
        failures += 1;
    }
    if hmac_sha512(HMAC_KEY2, HMAC_DATA2) == HMAC_MAC2 {
        out(target, b"  [ok]   HMAC-SHA-512 matches RFC 4231 test case 2\r\n");
    } else {
        out(target, b"  [FAIL] HMAC-SHA-512 differs from RFC 4231 test case 2\r\n");
        failures += 1;
    }
    if x25519_shared(&X_A, &[0u8; 32]).is_none() {
        out(target, b"  [ok]   X25519 refuses a small-order peer key\r\n");
    } else {
        out(target, b"  [FAIL] X25519 accepted a small-order peer key\r\n");
        failures += 1;
    }

    // --- 2. how long an operation takes ------------------------------------
    //
    // Under QEMU's TCG this is EMULATED time and pessimistic by roughly an order
    // of magnitude against real silicon; the number is a ceiling, not a
    // prediction. What it is good for is comparison - signing against verifying,
    // and a cached key against a recomputed one.
    out(target, b"\r\n  timings (QEMU/TCG - a ceiling, not real hardware):\r\n");

    let key = SigningKey::from_secret(&SK1);
    let t0 = ulib::monotonic_us();
    let sig = key.sign(b"a cluster frame");
    let t1 = ulib::monotonic_us();
    report_us(target, b"    sign (cached key)      ", t1 - t0);

    let t0 = ulib::monotonic_us();
    let _ = sign(&SK1, b"a cluster frame");
    let t1 = ulib::monotonic_us();
    report_us(target, b"    sign (one-shot)        ", t1 - t0);

    let t0 = ulib::monotonic_us();
    let ok = verify(&PK1, b"a cluster frame", &sig);
    let t1 = ulib::monotonic_us();
    report_us(target, b"    verify                 ", t1 - t0);
    if !ok {
        out(target, b"  [FAIL] round-trip signature did not verify\r\n");
        failures += 1;
    }

    let t0 = ulib::monotonic_us();
    let _ = SigningKey::from_secret(&SK1);
    let t1 = ulib::monotonic_us();
    report_us(target, b"    key expansion          ", t1 - t0);

    let t0 = ulib::monotonic_us();
    let k = x25519_shared(&X_A, &X_B_PUB);
    let t1 = ulib::monotonic_us();
    core::hint::black_box(k);
    report_us(target, b"    X25519 (scalar mul)    ", t1 - t0);

    // The keyed frame's MAC: the sequence number, then a full-size message,
    // streamed as netd will. Step 2's gate is this against a sign.
    report_us(target, b"    HMAC (NP_NET_MAX msg)  ", time_hmac());

    // The PBKDF2 iteration: user-keys step 0's measurement.
    failures += report_pbkdf2(target);

    // --- 3. how much stack it uses -----------------------------------------
    //
    // First CALIBRATE the instrument. A stack probe that silently measured
    // nothing would report a small, plausible number, and a plausible number is
    // exactly what this step must not accept on faith - so run the same probe
    // around a function with a KNOWN extra 4 KB frame and check the reading
    // moves by about that much. If it does not, the number means nothing. Each
    // computation is calibrated on its own, so neither figure borrows the
    // other's evidence.
    failures += report_stack(target, Op::SignVerify, b"sign+verify");
    failures += report_stack(target, Op::X25519, b"X25519");
    failures += report_stack(target, Op::Hmac, b"HMAC");

    if failures == 0 {
        out(target, b"\r\nedtest: all checks passed\r\n");
        ulib::end_of_stream(target);
        ulib::exit(0);
    }
    out(target, b"\r\nedtest: FAILURES\r\n");
    ulib::end_of_stream(target);
    ulib::exit(1);
}

/// Write to wherever this program's output belongs - the console, or a pipeline
/// consumer.
fn out(target: u64, bytes: &[u8]) {
    ulib::write_out(target, bytes);
}

/// Microseconds for one HMAC over a full `NP_NET_MAX` message. Its own frame:
/// the stack readings below are depths from the top of the stack, so a 2 KB
/// buffer left in `_start` would be counted in every one of them.
#[inline(never)]
fn time_hmac() -> u64 {
    let msg = [0x5au8; ninep_abi::NP_NET_MAX];
    let t0 = ulib::monotonic_us();
    let mut h = HmacSha512::new(&SK1);
    h.update(&1u64.to_le_bytes());
    h.update(core::hint::black_box(&msg));
    let mac = h.finalize();
    let t1 = ulib::monotonic_us();
    core::hint::black_box(mac);
    t1 - t0
}

/// SHA-512's block, and so the length of HMAC's key pads (RFC 2104). The crate
/// keeps its own constant private; this one is only the pad width below.
const PAD_LEN: usize = 128;

/// The password the PBKDF2 timing runs under: the dev `user` account's. Its
/// value does not change the cost of an iteration, only of priming the pads.
const PBKDF2_PASSWORD: &[u8] = b"user";

/// Iterations in the shorter timed PBKDF2 run; the longer is twice this, so
/// the calibration can ask whether the time doubled.
///
/// LARGE ON PURPOSE: each run must span many scheduler ticks. A run is
/// preempted at every tick (`TICK_INTERVAL_MS`, 20 ms) and loses the CPU to
/// other tasks for a while, so a run short enough to fit between two ticks
/// times the iterations alone and a longer one does not. With 5,000 and
/// 10,000 the short run fitted and the long one never did (13.4 ms against
/// 52 ms, 2026-09-26), and the scaling check refused the pair. A derivation at
/// any candidate count spans hundreds of ticks, so the figure wanted is the
/// WALL time per iteration with that loss included, which is what these runs
/// (about 0.12 and 0.25 s on the QEMU guest) measure.
const PBKDF2_N: u32 = 25_000;

/// The iteration counts a login's cost is projected for: this plan's floor
/// (Decision 3) and OWASP's current figure for PBKDF2-HMAC-SHA-512.
const PBKDF2_CANDIDATES: [u64; 2] = [100_000, 210_000];

/// HMAC's inner and outer hashes, each primed with its key pad, which is the
/// state PBKDF2 precomputes once and copies per iteration.
///
/// Built HERE, not taken from the crate, because the crate has no PBKDF2 yet
/// (step 2 adds it) and its `HmacSha512` rebuilds the outer hash inside
/// `finalize`, which would time three compressions an iteration rather than
/// the two a PBKDF2 pays. `pbkdf2_step_is_hmac` checks this construction
/// against `hmac_sha512`, so the figure is the cost of a real HMAC.
fn primed_pads(password: &[u8]) -> (Sha512, Sha512) {
    let mut k = [0u8; PAD_LEN];
    k[..password.len()].copy_from_slice(password);
    let mut ikey = [0u8; PAD_LEN];
    let mut okey = [0u8; PAD_LEN];
    for i in 0..PAD_LEN {
        ikey[i] = k[i] ^ 0x36;
        okey[i] = k[i] ^ 0x5c;
    }
    let mut inner = Sha512::new();
    inner.update(&ikey);
    let mut outer = Sha512::new();
    outer.update(&okey);
    (inner, outer)
}

/// One PBKDF2 iteration: `HMAC(P, u)` from the primed pads. Two compressions:
/// the inner hash's one block (`u` and its padding fit in 128 bytes), and the
/// outer's.
#[inline(always)]
fn pbkdf2_step(inner: &Sha512, outer: &Sha512, u: &[u8; HMAC_LEN]) -> [u8; HMAC_LEN] {
    let mut i = inner.clone();
    i.update(u);
    let mut o = outer.clone();
    o.update(&i.finalize());
    o.finalize()
}

/// Whether one step from the primed pads equals `hmac_sha512`. Without it, a
/// wrong pad would still time two compressions and the number would look
/// right while measuring something that is not an HMAC.
fn pbkdf2_step_is_hmac() -> bool {
    let (inner, outer) = primed_pads(PBKDF2_PASSWORD);
    let u = hmac_sha512(PBKDF2_PASSWORD, b"a salt");
    pbkdf2_step(&inner, &outer, &u) == hmac_sha512(PBKDF2_PASSWORD, &u)
}

/// Microseconds for `n` PBKDF2 iterations after the first, the loop body of
/// RFC 8018's F: `U_j = HMAC(P, U_{j-1})`, XORed into the block. The pads are
/// primed outside the timed region, as a derivation primes them once.
#[inline(never)]
fn time_pbkdf2(n: u32) -> u64 {
    let (inner, outer) = primed_pads(PBKDF2_PASSWORD);
    let mut u = hmac_sha512(PBKDF2_PASSWORD, b"a salt\0\0\0\x01");
    let mut t = u;
    let t0 = ulib::monotonic_us();
    for _ in 0..n {
        u = pbkdf2_step(&inner, &outer, core::hint::black_box(&u));
        for k in 0..HMAC_LEN {
            t[k] ^= u[k];
        }
    }
    let t1 = ulib::monotonic_us();
    core::hint::black_box(t);
    t1 - t0
}

/// The fastest of three timed runs of `n` iterations.
fn fastest_pbkdf2(n: u32) -> u64 {
    let mut best = u64::MAX;
    for _ in 0..3 {
        best = best.min(time_pbkdf2(n));
    }
    best
}

/// Time PBKDF2 iterations, check the figure measures what it claims, and print
/// the per-iteration cost with a login's cost at each candidate count. Returns
/// the number of failures.
///
/// TWO CHECKS BEFORE THE NUMBER IS BELIEVED. The step must be a real HMAC
/// (`pbkdf2_step_is_hmac`), and the time must SCALE: twice the iterations has
/// to take about twice as long, or the loop was optimised away, the clock is
/// too coarse, or a stall dominated the run. The band is wide (1.6 to 2.5)
/// because other tasks' ticks land inside a run: two boots read 1.8 and 2.4.
/// It has refused twice already, for the two causes described at
/// `PBKDF2_N` and in the body below.
fn report_pbkdf2(target: u64) -> u32 {
    let mut failures = 0u32;
    if pbkdf2_step_is_hmac() {
        out(target, b"  [ok]   PBKDF2 step from primed pads equals hmac_sha512\r\n");
    } else {
        out(target, b"  [FAIL] PBKDF2 step from primed pads differs from hmac_sha512\r\n");
        failures += 1;
    }
    // The FASTEST of three runs at each size. A single run carried about
    // 25 ms that was not iterations (2026-09-26), transient, since the fastest
    // of three dropped the 5,000-iteration figure from 38.8 ms to 13.4 ms. The
    // likely cause, not proven: the run starts right after a line is printed,
    // and the console server renders it on the same CPU. Another task can only
    // make a run slower, so the minimum drops that. It does not drop the tick losses `PBKDF2_N`
    // describes, which every run of this length pays alike.
    let one = fastest_pbkdf2(PBKDF2_N);
    let two = fastest_pbkdf2(PBKDF2_N * 2);
    out(target, b"    PBKDF2 x");
    put_dec(target, PBKDF2_N as u64);
    out(target, b" = ");
    put_dec(target, one);
    out(target, b" us, x");
    put_dec(target, PBKDF2_N as u64 * 2);
    out(target, b" = ");
    put_dec(target, two);
    out(target, b" us\r\n");
    // Ratio in tenths, integer only: 16..=25 is 1.6 to 2.5.
    let tenths = (two * 10).checked_div(one).unwrap_or(0);
    if !(16..=25).contains(&tenths) {
        out(target, b"  [FAIL] PBKDF2 time did not scale with iterations (ratio x10 = ");
        put_dec(target, tenths);
        out(target, b"); the per-iteration figure is not reported\r\n");
        return failures + 1;
    }
    out(target, b"  [ok]   PBKDF2 time scales with iterations (ratio x10 = ");
    put_dec(target, tenths);
    out(target, b")\r\n");
    // Per iteration from the longer run, in nanoseconds so a sub-microsecond
    // difference between platforms is not rounded away.
    let ns = two * 1000 / (PBKDF2_N as u64 * 2);
    out(target, b"    PBKDF2 iteration       ");
    put_dec(target, ns);
    out(target, b" ns\r\n");
    for count in PBKDF2_CANDIDATES {
        let one_ms = ns * count / 1_000_000;
        out(target, b"    at ");
        put_dec(target, count);
        out(target, b" iterations: ");
        put_dec(target, one_ms);
        out(target, b" ms a derivation, ");
        put_dec(target, one_ms * 2);
        out(target, b" ms a login (two)\r\n");
    }
    failures
}

/// Print a labelled microsecond count.
fn report_us(target: u64, label: &[u8], us: u64) {
    let mut buf = [0u8; 32];
    let mut n = 0usize;
    ulib::emit_dec(&mut buf, &mut n, us);
    out(target, label);
    out(target, &buf[..n]);
    out(target, b" us\r\n");
}

/// Print a decimal number.
fn put_dec(target: u64, v: u64) {
    let mut buf = [0u8; 32];
    let mut n = 0usize;
    ulib::emit_dec(&mut buf, &mut n, v);
    out(target, &buf[..n]);
}

/// Measure `op`'s peak stack, calibrate the probe against a known 4 KB frame,
/// and print the figure only if the calibration held. Returns the number of
/// failures (0 or 1).
fn report_stack(target: u64, op: Op, label: &[u8]) -> u32 {
    let plain = measure_stack_inner(&SK1, op, false);
    let padded = measure_stack_inner(&SK1, op, true).map(|(used, _)| used);
    let calibrated =
        matches!((plain, padded), (Some((a, _)), Some(b)) if b > a + 3072 && b < a + 8192);
    match (plain, padded) {
        (Some((a, _)), Some(b)) if calibrated => {
            out(target, b"\r\n  [ok]   stack probe (");
            out(target, label);
            out(target, b") responds to a known 4KB frame (+");
            put_dec(target, (b - a) as u64);
            out(target, b" bytes)\r\n");
        }
        (Some((a, _)), Some(b)) => {
            out(target, b"\r\n  [FAIL] stack probe (");
            out(target, label);
            out(target, b") did not respond as expected: ");
            put_dec(target, a as u64);
            out(target, b" then ");
            put_dec(target, b as u64);
            out(target, b"\r\n");
            return 1;
        }
        _ => {
            out(target, b"\r\n  [warn] stack measurement unavailable\r\n");
            return 0;
        }
    }
    if let Some((used, stack_size)) = plain {
        out(target, b"  peak stack for ");
        out(target, label);
        out(target, b": ");
        put_dec(target, used as u64);
        out(target, b" bytes of ");
        put_dec(target, stack_size as u64);
        out(target, b" (");
        put_dec(target, (used as u64 * 100) / stack_size as u64);
        out(target, b"%)\r\n");
    }
    0
}

/// Peak stack bytes used by `op`.
///
/// Paints the unused stack below the current frame with a known byte, runs the
/// operations, then finds the lowest painted byte that changed. The stack extent
/// is not guessed: `HEAP_INFO` reports this task's stack `(base, size)` as the
/// loader laid it out, guard page excluded. Painting is therefore bounded by
/// construction and cannot wander into the guard page, which would be a fault,
/// not a measurement. (It used to add a local `STACK_BYTES` to the heap's end
/// instead, a copy of the loader's page count that went stale when the stack
/// grew and silently disabled this measurement; the sanity check below is
/// what refused, and the calibration in `report_stack` is what made that visible.)
///
/// `(peak bytes used, stack size)`, or `None` if the probe could not run.
/// With `pad`, the same work runs behind a deliberate extra 4 KB frame, which
/// is how `report_stack` proves the probe reacts.
#[inline(never)]
fn measure_stack_inner(secret: &[u8; 32], op: Op, pad: bool) -> Option<(usize, usize)> {
    let (stack_lo, stack_size) = ulib::stack_extent();
    if stack_lo == 0 || stack_size == 0 {
        return None;
    }
    let stack_hi = stack_lo + stack_size;

    let probe = 0u64;
    let sp = &probe as *const u64 as usize;
    // Sanity: if the computed window does not contain our own frame, the layout
    // assumption is wrong and painting would be dangerous. Refuse instead.
    if sp <= stack_lo || sp > stack_hi {
        return None;
    }

    // Leave a margin below the current frame untouched, so painting cannot
    // clobber anything live between here and the call below.
    let paint_hi = sp - 256;
    // SAFETY: [stack_lo, paint_hi) is this task's own stack, below the current
    // frame and above the guard page, both bounds derived from HEAP_INFO rather
    // than assumed.
    unsafe {
        let mut p = stack_lo;
        while p < paint_hi {
            core::ptr::write_volatile(p as *mut u8, PAINT);
            p += 1;
        }
    }

    if pad {
        work_with_extra_frame(secret, op);
    } else {
        work(secret, op);
    }

    // The lowest address still painted marks how deep the call went.
    let mut lowest = paint_hi;
    let mut p = stack_lo;
    while p < paint_hi {
        // SAFETY: same window that was just painted.
        let v = unsafe { core::ptr::read_volatile(p as *const u8) };
        if v != PAINT {
            lowest = p;
            break;
        }
        p += 1;
    }
    Some((stack_hi - lowest, stack_size))
}

/// The operations being measured. Each is its own non-inlined function, so
/// this frame holds only the dispatch: when the three bodies were arms of one
/// match, the HMAC arm's message buffer sat in the shared frame and inflated
/// the OTHER two readings by ~5 KB (sign+verify read 8,960 bytes where it had
/// read 3,584), the sibling-branch rule of `docs/postmortems/async-rmount-postmortem.md`.
#[inline(never)]
fn work(secret: &[u8; 32], op: Op) {
    match op {
        Op::SignVerify => work_sign_verify(secret),
        Op::X25519 => work_x25519(secret),
        Op::Hmac => work_hmac(secret),
    }
}

#[inline(never)]
fn work_sign_verify(secret: &[u8; 32]) {
    let key = SigningKey::from_secret(secret);
    let sig = key.sign(b"stack measurement");
    let ok = verify(&key.public(), b"stack measurement", &sig);
    core::hint::black_box(ok);
}

#[inline(never)]
fn work_x25519(secret: &[u8; 32]) {
    let k = x25519_shared(secret, &X_B_PUB);
    core::hint::black_box(k);
}

/// The message buffer is part of this reading, which makes it an upper bound:
/// `netd` MACs a message already in a connection's buffer.
#[inline(never)]
fn work_hmac(secret: &[u8; 32]) {
    let msg = [0x5au8; ninep_abi::NP_NET_MAX];
    let mut h = HmacSha512::new(secret);
    h.update(&1u64.to_le_bytes());
    h.update(core::hint::black_box(&msg));
    core::hint::black_box(h.finalize());
}

/// The same work behind a known 4 KB frame, for calibration. The array is
/// written and read through `black_box` so the optimiser cannot elide it.
#[inline(never)]
fn work_with_extra_frame(secret: &[u8; 32], op: Op) {
    let mut pad = [0u8; 4096];
    for (i, b) in pad.iter_mut().enumerate() {
        *b = i as u8;
    }
    core::hint::black_box(&pad);
    work(secret, op);
    core::hint::black_box(&pad);
}

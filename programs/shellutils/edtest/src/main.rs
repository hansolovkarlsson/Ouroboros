//! `/bin/edtest` — the on-target check for the `ed25519` crate.
//!
//! Step 5 of `docs/roadmap-cluster-keys.md`, and the step that decides whether
//! the design survives contact with the machine. The host tests prove the
//! arithmetic; this proves three things they cannot:
//!
//! 1. **The same vectors pass on the target.** Different pointer width, a
//!    different code generator, and a loader that relocates the binary — none of
//!    which the host exercises.
//! 2. **Peak stack use**, measured rather than assumed. `netd` has 32 KB and has
//!    hit its guard page five times in this project's history; a signature
//!    verification is the largest computation it would ever have done.
//! 3. **Time per operation**, so the decision to use bit-by-bit scalar reduction
//!    and no fixed-base table rests on a number rather than a guess.
//!
//! It stays in `/bin` rather than being a throwaway: the Raspberry Pi bring-up
//! will want exactly this, on a third code generator and real hardware.

#![no_std]
#![no_main]

use ed25519::{public_key, sign, verify, SigningKey};

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

/// The byte a stack probe is painted with, chosen as an unlikely value to write
/// by accident.
const PAINT: u8 = 0xA5;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    ulib::con_write(b"edtest: ed25519 on the target\r\n");
    let mut failures = 0u32;

    // --- 1. the published vectors, on this machine -------------------------
    if public_key(&SK1) == PK1 {
        ulib::con_write(b"  [ok]   public key matches RFC 8032 TEST 1\r\n");
    } else {
        ulib::con_write(b"  [FAIL] public key differs from RFC 8032 TEST 1\r\n");
        failures += 1;
    }
    if sign(&SK1, b"") == SIG1 {
        ulib::con_write(b"  [ok]   signature matches RFC 8032 TEST 1 (empty message)\r\n");
    } else {
        ulib::con_write(b"  [FAIL] signature differs from RFC 8032 TEST 1\r\n");
        failures += 1;
    }
    if sign(&SK2, &[0x72]) == SIG2 {
        ulib::con_write(b"  [ok]   signature matches RFC 8032 TEST 2 (one-byte message)\r\n");
    } else {
        ulib::con_write(b"  [FAIL] signature differs from RFC 8032 TEST 2\r\n");
        failures += 1;
    }
    if verify(&PK1, b"", &SIG1) {
        ulib::con_write(b"  [ok]   verify accepts a good signature\r\n");
    } else {
        ulib::con_write(b"  [FAIL] verify rejected a good signature\r\n");
        failures += 1;
    }
    let mut bad = SIG1;
    bad[0] ^= 1;
    if !verify(&PK1, b"", &bad) {
        ulib::con_write(b"  [ok]   verify rejects a flipped bit\r\n");
    } else {
        ulib::con_write(b"  [FAIL] verify accepted a tampered signature\r\n");
        failures += 1;
    }

    // --- 2. how long an operation takes ------------------------------------
    //
    // Under QEMU's TCG this is EMULATED time and pessimistic by roughly an order
    // of magnitude against real silicon; the number is a ceiling, not a
    // prediction. What it is good for is comparison - signing against verifying,
    // and a cached key against a recomputed one.
    ulib::con_write(b"\r\n  timings (QEMU/TCG - a ceiling, not real hardware):\r\n");

    let key = SigningKey::from_secret(&SK1);
    let t0 = ulib::monotonic_us();
    let sig = key.sign(b"a cluster frame");
    let t1 = ulib::monotonic_us();
    report_us(b"    sign (cached key)      ", t1 - t0);

    let t0 = ulib::monotonic_us();
    let _ = sign(&SK1, b"a cluster frame");
    let t1 = ulib::monotonic_us();
    report_us(b"    sign (one-shot)        ", t1 - t0);

    let t0 = ulib::monotonic_us();
    let ok = verify(&PK1, b"a cluster frame", &sig);
    let t1 = ulib::monotonic_us();
    report_us(b"    verify                 ", t1 - t0);
    if !ok {
        ulib::con_write(b"  [FAIL] round-trip signature did not verify\r\n");
        failures += 1;
    }

    let t0 = ulib::monotonic_us();
    let _ = SigningKey::from_secret(&SK1);
    let t1 = ulib::monotonic_us();
    report_us(b"    key expansion          ", t1 - t0);

    // --- 3. how much stack it uses -----------------------------------------
    //
    // First CALIBRATE the instrument. A stack probe that silently measured
    // nothing would report a small, plausible number, and a plausible number is
    // exactly what this step must not accept on faith - so run the same probe
    // around a function with a KNOWN extra 4 KB frame and check the reading
    // moves by about that much. If it does not, the number below means nothing.
    let plain = measure_stack(&SK1);
    let padded = measure_stack_padded(&SK1);
    match (plain, padded) {
        (Some(a), Some(b)) if b > a + 3072 && b < a + 8192 => {
            ulib::con_write(b"\r\n  [ok]   stack probe responds to a known 4KB frame (+");
            put_dec((b - a) as u64);
            ulib::con_write(b" bytes)\r\n");
        }
        (Some(a), Some(b)) => {
            ulib::con_write(b"\r\n  [FAIL] stack probe did not respond as expected: ");
            put_dec(a as u64);
            ulib::con_write(b" then ");
            put_dec(b as u64);
            ulib::con_write(b"\r\n");
            failures += 1;
        }
        _ => ulib::con_write(b"\r\n  [warn] stack measurement unavailable\r\n"),
    }

    match plain {
        Some(used) => {
            ulib::con_write(b"\r\n  peak stack for sign+verify: ");
            put_dec(used as u64);
            ulib::con_write(b" bytes of 32768 (");
            put_dec((used as u64 * 100) / 32768);
            ulib::con_write(b"%)\r\n");
        }
        None => ulib::con_write(b"\r\n  [warn] stack measurement unavailable\r\n"),
    }

    if failures == 0 {
        ulib::con_write(b"\r\nedtest: all checks passed\r\n");
        ulib::exit(0);
    }
    ulib::con_write(b"\r\nedtest: FAILURES\r\n");
    ulib::exit(1);
}

/// Print a labelled microsecond count.
fn report_us(label: &[u8], us: u64) {
    let mut buf = [0u8; 32];
    let mut n = 0usize;
    ulib::emit_dec(&mut buf, &mut n, us);
    ulib::con_write(label);
    ulib::con_write(&buf[..n]);
    ulib::con_write(b" us\r\n");
}

/// Print a decimal number.
fn put_dec(v: u64) {
    let mut buf = [0u8; 32];
    let mut n = 0usize;
    ulib::emit_dec(&mut buf, &mut n, v);
    ulib::con_write(&buf[..n]);
}

/// Peak stack bytes used by a sign followed by a verify.
///
/// Paints the unused stack below the current frame with a known byte, runs the
/// operations, then finds the lowest painted byte that changed. The stack extent
/// is not guessed: `HEAP_INFO` reports this task's heap, and the loader's layout
/// is `[code][heap][guard][stack]`, so the stack starts one guard page above the
/// heap's end and runs `STACK_BYTES` upward. Painting is therefore bounded by
/// construction and cannot wander into the guard page — which would be a fault,
/// not a measurement.
fn measure_stack(secret: &[u8; 32]) -> Option<usize> {
    measure_stack_inner(secret, false)
}

#[inline(never)]
fn measure_stack_inner(secret: &[u8; 32], pad: bool) -> Option<usize> {
    /// Must match the loader's `STACK_PAGES` (8) × 4 KB.
    const STACK_BYTES: usize = 32 * 1024;
    const PAGE: usize = 4096;

    let heap = ulib::heap();
    if heap.is_empty() {
        return None;
    }
    let stack_lo = heap.as_ptr() as usize + heap.len() + PAGE; // past the guard page
    let stack_hi = stack_lo + STACK_BYTES;

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
        work_with_extra_frame(secret);
    } else {
        work(secret);
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
    Some(stack_hi - lowest)
}

/// The same measurement, with a deliberate extra 4 KB frame in the call path.
/// Used only to prove the probe reacts - see the calibration in `_start`.
#[inline(never)]
fn measure_stack_padded(secret: &[u8; 32]) -> Option<usize> {
    measure_stack_inner(secret, true)
}

/// The operations being measured.
#[inline(never)]
fn work(secret: &[u8; 32]) {
    let key = SigningKey::from_secret(secret);
    let sig = key.sign(b"stack measurement");
    let ok = verify(&key.public(), b"stack measurement", &sig);
    core::hint::black_box(ok);
}

/// The same work behind a known 4 KB frame, for calibration. The array is
/// written and read through `black_box` so the optimiser cannot elide it.
#[inline(never)]
fn work_with_extra_frame(secret: &[u8; 32]) {
    let mut pad = [0u8; 4096];
    for (i, b) in pad.iter_mut().enumerate() {
        *b = i as u8;
    }
    core::hint::black_box(&pad);
    work(secret);
    core::hint::black_box(&pad);
}

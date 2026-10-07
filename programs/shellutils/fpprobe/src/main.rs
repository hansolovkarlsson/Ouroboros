//! `fpprobe` - a probe, not a tool: checks that a task's FP/SIMD registers
//! survive the kernel. Every resumable exception path saves `q0`-`q31`,
//! `FPCR` and `FPSR` and a task switch carries them (`kernel/src/
//! exceptions.rs`, since 2026-10-07); before that the kernel's own code
//! (its memcpy and struct copies run through `q0`) and other tasks changed
//! them under a program that had a value live in one.
//!
//! Two crossings, each inside ONE `asm!` block, since between two blocks
//! the compiler may use the vector registers itself:
//!
//! 1. **A syscall.** All 32 registers loaded with a pattern and `FPCR` set
//!    to round toward zero (not the reset value), then `YIELD`, which runs
//!    the kernel's switch path and hands the core to another task; then all
//!    of it read back.
//! 2. **Preemption.** The same, around a spin with no syscall, long enough
//!    to span timer ticks; the tick count before and after is printed, and
//!    the check needs at least two, so the spin cannot pass by being short.
//!
//! Each round uses a different pattern. One `[ok]`/`[FAIL]` line per
//! crossing; the exit code is the number of failures.
//!
//! `fpprobe -` then copies its piped input to its output, so in
//! `fpprobe | fpprobe -` both probes' lines reach the console and the two
//! spins run side by side, each preempted in favour of the other
//! (`scripts/test-fpsimd.py`).

#![no_std]
#![no_main]

const ROUNDS: u64 = 8;
/// `FPCR.RMode` = 0b11, round toward zero; the reset value is 0.
const FPCR_RZ: u64 = 0b11 << 22;
/// Iterations of the preemption spin: under QEMU's emulation 20 million
/// finished inside one 20 ms tick, so this is generous; the tick count
/// printed is what proves the spin was preempted, not this number.
const SPIN: u64 = 400_000_000;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut failures = 0u64;
    for (name, spin) in [(&b"syscall"[..], false), (&b"preemption"[..], true)] {
        let mut bad = 0u64;
        let mut first_bad = None;
        let mut ticks = u64::MAX;
        for round in 0..ROUNDS {
            let mut src = [0u128; 32];
            for (i, q) in src.iter_mut().enumerate() {
                let lo = 0x0101_0101_0101_0101u64.wrapping_mul(i as u64 + 1) ^ (round << 56);
                *q = ((!lo as u128) << 64) | lo as u128;
            }
            let mut dst = [0u128; 32];
            let before = ulib::syscall(syscall_abi::GET_TICKS, 0);
            let fpcr = cross(&src, &mut dst, spin);
            let after = ulib::syscall(syscall_abi::GET_TICKS, 0);
            ticks = ticks.min(after - before);
            for i in 0..32 {
                if dst[i] != src[i] {
                    bad += 1;
                    first_bad.get_or_insert(i);
                }
            }
            if fpcr & (0b11 << 22) != FPCR_RZ {
                bad += 1;
                first_bad.get_or_insert(32);
            }
        }
        let ok = bad == 0 && (!spin || ticks >= 2);
        if !ok {
            failures += 1;
        }
        ulib::write_out(target, if ok { b"[ok]   " } else { b"[FAIL] " });
        ulib::write_out(target, name);
        ulib::write_out(target, b": ");
        put_dec(target, ROUNDS);
        ulib::write_out(target, b" rounds, ");
        put_dec(target, bad);
        ulib::write_out(target, b" registers changed");
        if let Some(i) = first_bad {
            if i == 32 {
                ulib::write_out(target, b" (first: fpcr)");
            } else {
                ulib::write_out(target, b" (first: q");
                put_dec(target, i as u64);
                ulib::write_out(target, b")");
            }
        }
        if spin {
            ulib::write_out(target, b", fewest ticks spanned ");
            put_dec(target, ticks);
        }
        ulib::write_out(target, b"\r\n");
    }
    let mut arg = [0u8; 2];
    if ulib::arg(1, &mut arg) == Some(1) && arg[0] == b'-' {
        forward_stdin(target);
    }
    ulib::end_of_stream(target);
    ulib::exit(failures);
}

/// Loads `src` into q0-q31 and `FPCR_RZ` into FPCR, crosses the kernel
/// (`YIELD`, or a spin the tick preempts), stores q0-q31 to `dst`, and
/// returns FPCR as it came back, restoring the caller's.
fn cross(src: &[u128; 32], dst: &mut [u128; 32], spin: bool) -> u64 {
    let fpcr_after: u64;
    unsafe {
        core::arch::asm!(
            "mrs {saved}, fpcr",
            "msr fpcr, {rz}",
            "ldp q0, q1, [{src}, #0]",
            "ldp q2, q3, [{src}, #32]",
            "ldp q4, q5, [{src}, #64]",
            "ldp q6, q7, [{src}, #96]",
            "ldp q8, q9, [{src}, #128]",
            "ldp q10, q11, [{src}, #160]",
            "ldp q12, q13, [{src}, #192]",
            "ldp q14, q15, [{src}, #224]",
            "ldp q16, q17, [{src}, #256]",
            "ldp q18, q19, [{src}, #288]",
            "ldp q20, q21, [{src}, #320]",
            "ldp q22, q23, [{src}, #352]",
            "ldp q24, q25, [{src}, #384]",
            "ldp q26, q27, [{src}, #416]",
            "ldp q28, q29, [{src}, #448]",
            "ldp q30, q31, [{src}, #480]",
            "cbnz {spin}, 2f",
            "mov x8, {yield_nr}",
            "svc #0",
            "b 3f",
            "2:",
            "subs {spin}, {spin}, #1",
            "b.ne 2b",
            "3:",
            "stp q0, q1, [{dst}, #0]",
            "stp q2, q3, [{dst}, #32]",
            "stp q4, q5, [{dst}, #64]",
            "stp q6, q7, [{dst}, #96]",
            "stp q8, q9, [{dst}, #128]",
            "stp q10, q11, [{dst}, #160]",
            "stp q12, q13, [{dst}, #192]",
            "stp q14, q15, [{dst}, #224]",
            "stp q16, q17, [{dst}, #256]",
            "stp q18, q19, [{dst}, #288]",
            "stp q20, q21, [{dst}, #320]",
            "stp q22, q23, [{dst}, #352]",
            "stp q24, q25, [{dst}, #384]",
            "stp q26, q27, [{dst}, #416]",
            "stp q28, q29, [{dst}, #448]",
            "stp q30, q31, [{dst}, #480]",
            "mrs {after}, fpcr",
            "msr fpcr, {saved}",
            src = in(reg) src.as_ptr(),
            dst = in(reg) dst.as_mut_ptr(),
            rz = in(reg) FPCR_RZ,
            spin = inout(reg) if spin { SPIN } else { 0 } => _,
            yield_nr = const syscall_abi::YIELD,
            saved = out(reg) _,
            after = out(reg) fpcr_after,
            out("x0") _,
            out("x8") _,
            out("v0") _, out("v1") _, out("v2") _, out("v3") _, out("v4") _, out("v5") _, out("v6") _, out("v7") _,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _, out("v12") _, out("v13") _, out("v14") _, out("v15") _,
            out("v16") _, out("v17") _, out("v18") _, out("v19") _, out("v20") _, out("v21") _, out("v22") _, out("v23") _,
            out("v24") _, out("v25") _, out("v26") _, out("v27") _, out("v28") _, out("v29") _, out("v30") _, out("v31") _,
            options(nostack),
        );
    }
    fpcr_after
}

/// Copies piped input (`MSG_RECV`, EOF the empty message) to `target`,
/// the shape of `upper` without the uppercasing.
fn forward_stdin(target: u64) {
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = ulib::syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            break;
        }
        let len = ((packed & 0xffff_ffff) as usize).min(buf.len());
        if len == 0 {
            break;
        }
        ulib::write_out(target, &buf[..len]);
    }
}

fn put_dec(target: u64, n: u64) {
    let mut digits = [0u8; 20];
    let mut len = 0;
    let mut v = n;
    loop {
        digits[19 - len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    ulib::write_out(target, &digits[20 - len..]);
}

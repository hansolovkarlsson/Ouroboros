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
//! 1. **A syscall.** All 32 registers loaded with a pattern, `FPCR` set to
//!    a rounding mode other than the reset value and `FPSR` to a pattern of
//!    cumulative flags, then `YIELD`; then all of it read back. Run alone,
//!    nothing else is runnable and `YIELD` returns to the caller without a
//!    switch, so this is the check on the trampoline and the kernel's own
//!    code (its memcpy runs through `q0`), not on the switch path.
//! 2. **Preemption.** The same, around a spin with no syscall, long enough
//!    to span timer ticks; the tick count before and after is printed, and
//!    the check needs at least two, so the spin cannot pass by being short.
//!
//! Each round uses a different pattern. One `[ok]`/`[FAIL]` line per
//! crossing; the exit code is the number of failures.
//!
//! `fpprobe -` then copies its piped input to its output, so in
//! `fpprobe | fpprobe -` both probes' lines reach the console and the two
//! spins run side by side (`scripts/test-fpsimd.py`). The two probes hold
//! different FPCR and FPSR values, so a register that crossed from one to
//! the other is seen. That they do interleave is shown by a control, not by
//! anything the probe prints: with the kernel's FPCR restore removed, the
//! pipelined probes fail with `first: fpcr` and the lone one passes.

#![no_std]
#![no_main]

const ROUNDS: u64 = 8;
/// `FPCR.RMode`, which each probe sets to a value other than the reset
/// value 0: round toward zero alone, toward plus infinity with `-`. Two
/// probes side by side then hold different FPCRs, so one that leaked
/// across a switch is seen.
const RMODE_MASK: u64 = 0b11 << 22;
const FPCR_RZ: u64 = 0b11 << 22;
const FPCR_RP: u64 = 0b01 << 22;
/// `FPSR`'s cumulative flags, a different set per probe: IOC and QC alone,
/// DZC and IXC with `-`. The kernel's integer SIMD sets none of them.
const FPSR_MASK: u64 = (1 << 27) | 0b1001_1111;
const FPSR_ALONE: u64 = (1 << 27) | 1;
const FPSR_PIPED: u64 = 0b1_0010;
/// Iterations of the preemption spin: under QEMU's emulation 20 million
/// finished inside one 20 ms tick, so this is generous; the tick count
/// printed is what proves the spin was preempted, not this number.
const SPIN: u64 = 400_000_000;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut arg = [0u8; 2];
    let forward = ulib::arg(1, &mut arg) == Some(1) && arg[0] == b'-';
    let fpcr_want = if forward { FPCR_RP } else { FPCR_RZ };
    let fpsr_want = if forward { FPSR_PIPED } else { FPSR_ALONE };
    let mut failures = 0u64;
    let mut out = Out { buf: [0; 512], len: 0 };
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
            let (fpcr, fpsr) = cross(&src, &mut dst, spin, fpcr_want, fpsr_want);
            let after = ulib::syscall(syscall_abi::GET_TICKS, 0);
            ticks = ticks.min(after - before);
            for i in 0..32 {
                if dst[i] != src[i] {
                    bad += 1;
                    first_bad.get_or_insert(i);
                }
            }
            if fpcr & RMODE_MASK != fpcr_want {
                bad += 1;
                first_bad.get_or_insert(32);
            }
            if fpsr & FPSR_MASK != fpsr_want {
                bad += 1;
                first_bad.get_or_insert(33);
            }
        }
        let ok = bad == 0 && (!spin || ticks >= 2);
        if !ok {
            failures += 1;
        }
        out.put(if ok { b"[ok]   " } else { b"[FAIL] " });
        out.put(name);
        out.put(b": ");
        out.dec(ROUNDS);
        out.put(b" rounds, ");
        out.dec(bad);
        out.put(b" registers changed");
        if let Some(i) = first_bad {
            if i == 32 {
                out.put(b" (first: fpcr)");
            } else if i == 33 {
                out.put(b" (first: fpsr)");
            } else {
                out.put(b" (first: q");
                out.dec(i as u64);
                out.put(b")");
            }
        }
        if spin {
            out.put(b", fewest ticks spanned ");
            out.dec(ticks);
        }
        out.put(b"\r\n");
    }
    // Written once, after both crossings: a write into a pipe whose reader
    // is still in its own checks blocks, and a probe blocked there is not
    // spinning beside the other one (the FPCR mutation showed it).
    ulib::write_out(target, &out.buf[..out.len]);
    if forward {
        forward_stdin(target);
    }
    ulib::end_of_stream(target);
    ulib::exit(failures);
}

/// Loads `src` into q0-q31, `fpcr` into FPCR and `fpsr` into FPSR, crosses the kernel
/// (`YIELD`, or a spin the tick preempts), stores q0-q31 to `dst`, and
/// returns FPCR and FPSR as they came back, restoring the caller's.
fn cross(src: &[u128; 32], dst: &mut [u128; 32], spin: bool, fpcr: u64, fpsr: u64) -> (u64, u64) {
    let fpcr_after: u64;
    let fpsr_after: u64;
    unsafe {
        core::arch::asm!(
            "mrs {saved}, fpcr",
            "msr fpcr, {rz}",
            "mrs {saved_sr}, fpsr",
            "msr fpsr, {sr}",
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
            "mrs {after_sr}, fpsr",
            "msr fpsr, {saved_sr}",
            src = in(reg) src.as_ptr(),
            dst = in(reg) dst.as_mut_ptr(),
            rz = in(reg) fpcr,
            sr = in(reg) fpsr,
            saved_sr = out(reg) _,
            after_sr = out(reg) fpsr_after,
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
    (fpcr_after, fpsr_after)
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

/// The probe's report, held until both crossings are done.
struct Out {
    buf: [u8; 512],
    len: usize,
}

impl Out {
    fn put(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
    }

    fn dec(&mut self, n: u64) {
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
        self.put(&digits[20 - len..]);
    }
}

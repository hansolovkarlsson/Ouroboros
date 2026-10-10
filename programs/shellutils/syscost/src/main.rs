//! `syscost` - the cost of a syscall that does nothing with FP/SIMD, for
//! multi-core step 3's first decision (`docs/roadmap/roadmap-smp.md`): is
//! the eager save and restore of `q0`-`q31`, `FPCR` and `FPSR` on every
//! resumable kernel path (800 bytes a trip since #227) worth keeping, or
//! should a lazy scheme (`CPACR_EL1.FPEN` trapping the first FP use after a
//! switch) replace it before cores run tasks. The number this prints is
//! what the decision cites: `ROUNDS` `GET_TICKS` calls, the cheapest syscall
//! there is (one atomic load), timed by `MONOTONIC_US`, as nanoseconds a
//! call. Run on the tree and on a build with the save removed, by
//! `scripts/measure-syscost.py` (`make measure-syscost`). Under QEMU the
//! figure is TCG's, not hardware's; the ratio between the two builds is
//! what carries.

#![no_std]
#![no_main]

const ROUNDS: u64 = 200_000;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let t0 = ulib::monotonic_us();
    let mut sink = 0u64;
    for _ in 0..ROUNDS {
        sink = sink.wrapping_add(ulib::get_ticks());
    }
    let us = ulib::monotonic_us() - t0;
    let mut buf = [0u8; 96];
    let mut n = 0;
    ulib::emit(&mut buf, &mut n, b"syscost: ");
    ulib::emit_dec(&mut buf, &mut n, ROUNDS);
    ulib::emit(&mut buf, &mut n, b" GET_TICKS in ");
    ulib::emit_dec(&mut buf, &mut n, us);
    ulib::emit(&mut buf, &mut n, b" us, ");
    ulib::emit_dec(&mut buf, &mut n, us * 1000 / ROUNDS);
    ulib::emit(&mut buf, &mut n, b" ns each\r\n");
    ulib::write_out(target, &buf[..n]);
    ulib::end_of_stream(target);
    // `sink` keeps the loop honest; its value says nothing.
    ulib::exit(if sink == u64::MAX { 1 } else { 0 });
}

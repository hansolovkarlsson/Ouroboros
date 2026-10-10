//! `coreprobe` - where a program runs, for multi-core step 4(d)
//! (`docs/roadmap/roadmap-smp.md`). It spins, with no syscall inside a
//! round so the tick is what preempts it, and between rounds asks
//! `CORE_INFO` which core it is on and which cores are running a spawned
//! program at that instant (itself among them). The report says every core
//! it ran on, the most cores it saw running programs at once, and how many
//! cores take tasks:
//!
//! ```text
//! coreprobe: ran on core 1, up to 2 programs running at once, 4 cores up, 812 rounds
//! ```
//!
//! `coreprobe` alone spins for [`SPAN_TICKS`] ticks. `coreprobe -` then
//! copies its piped input to its output, as `fpprobe -` does, so in
//! `coreprobe | coreprobe -` both reports reach the console and the two
//! spins run side by side: on two cores, each sees two programs running at
//! once. Each report is written once, after the spin, since a write into a
//! pipe whose reader is still spinning blocks, and a blocked probe is not
//! spinning.
//!
//! `coreprobe hold` says which core it starts on and spins until it is
//! killed (Ctrl+C), which is how `make test-smp` ends a program running on
//! another core than the boot shell's.

#![no_std]
#![no_main]

/// Ticks a plain run spins for: two seconds at 20 ms a tick.
const SPAN_TICKS: u64 = 100;
/// Arithmetic between two looks at `CORE_INFO`, with no syscall inside.
const ROUND: u64 = 200_000;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut hold = false;
    let mut forward = false;
    let mut arg = [0u8; 16];
    for i in 1..ulib::argc() {
        match ulib::arg(i, &mut arg) {
            Some(4) if &arg[..4] == b"hold" => hold = true,
            Some(1) if arg[0] == b'-' => forward = true,
            _ => {
                ulib::write_out(target, b"usage: coreprobe [hold] [-]\r\n");
                ulib::end_of_stream(target);
                ulib::exit(2);
            }
        }
    }

    let mut out = [0u8; 160];
    let mut n = 0;
    if hold {
        ulib::emit(&mut out, &mut n, b"coreprobe: holding on core ");
        ulib::emit_dec(&mut out, &mut n, ulib::core_info(syscall_abi::CORE_INFO_THIS));
        ulib::emit(&mut out, &mut n, b"\r\n");
        ulib::write_out(target, &out[..n]);
        let mut sink = 0u64;
        loop {
            sink = spin(sink);
        }
    }

    let start = ulib::get_ticks();
    let mut cores = 0u64;
    let mut most_busy = 0u32;
    let mut rounds = 0u64;
    let mut sink = 0u64;
    loop {
        sink = spin(sink);
        cores |= 1 << (ulib::core_info(syscall_abi::CORE_INFO_THIS) & 63);
        most_busy = most_busy.max(ulib::core_info(syscall_abi::CORE_INFO_BUSY).count_ones());
        rounds += 1;
        if ulib::get_ticks() - start >= SPAN_TICKS {
            break;
        }
    }
    ulib::emit(&mut out, &mut n, b"coreprobe: ran on core");
    if cores.count_ones() > 1 {
        ulib::emit(&mut out, &mut n, b"s");
    }
    for core in 0..64 {
        if cores & (1 << core) != 0 {
            ulib::emit(&mut out, &mut n, b" ");
            ulib::emit_dec(&mut out, &mut n, core);
        }
    }
    ulib::emit(&mut out, &mut n, b", up to ");
    ulib::emit_dec(&mut out, &mut n, u64::from(most_busy));
    ulib::emit(&mut out, &mut n, b" programs running at once, ");
    ulib::emit_dec(&mut out, &mut n, ulib::core_info(syscall_abi::CORE_INFO_UP));
    ulib::emit(&mut out, &mut n, b" cores up, ");
    ulib::emit_dec(&mut out, &mut n, rounds);
    ulib::emit(&mut out, &mut n, b" rounds\r\n");
    ulib::write_out(target, &out[..n]);
    if forward {
        forward_stdin(target);
    }
    ulib::end_of_stream(target);
    // `sink` keeps the spin honest; its value says nothing.
    ulib::exit(if sink == u64::MAX { 1 } else { 0 });
}

/// One round of arithmetic the compiler cannot drop.
fn spin(mut sink: u64) -> u64 {
    for i in 0..ROUND {
        sink = core::hint::black_box(sink.wrapping_mul(6364136223846793005).wrapping_add(i));
    }
    sink
}

/// Copies piped input (`MSG_RECV`, EOF the empty message) to `target`,
/// as `fpprobe -` does.
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

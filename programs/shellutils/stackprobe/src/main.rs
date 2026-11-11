//! `stackprobe` - a known depth of stack, for `make test-stack-free`
//! (`scripts/test-stack-free.py`). A probe, not a tool: it asks the kernel
//! how much of its own stack it has never written (`TASK_STACK_FREE`), goes
//! `levels` calls deep writing [`CHUNK`] bytes in each, asks again, and
//! prints both answers:
//!
//! ```text
//! stackprobe: slot 6 size 57344 free 56912 then 48176
//! ```
//!
//! `stackprobe <levels> [zero] [hold]`. `zero` writes zeros where the default
//! writes `0x5A`: the write the kernel's paint exists to see, since a stack
//! merely zeroed at load could not tell a zeroed buffer from one never
//! reached. `hold` then blocks for good (a `MSG_RECV` nobody answers), so the
//! rig can read the same mark from outside, as `/proc/<slot>/stackfree`, and
//! end it with `kill`.
//!
//! Every level's frame is the same size, so two runs differ by a whole number
//! of frames: the rig grades the difference of two depths, where the
//! program's own start-up cost cancels.

#![no_std]
#![no_main]

/// The bytes each level writes. A level's frame is this and its own few
/// words (the frame record, the saved registers).
const CHUNK: usize = 1024;

/// One level: a buffer written whole, then the next level, then the buffer
/// read back so the call below is not the function's last act and the frame
/// cannot be given up before it.
#[inline(never)]
fn dig(levels: u64, fill: u8) -> u64 {
    let mut buf = [0u8; CHUNK];
    for b in buf.iter_mut() {
        // SAFETY: a write to this frame's own buffer, volatile so every
        // byte is written whatever its value.
        unsafe { core::ptr::write_volatile(b, fill) };
    }
    let below = if levels > 1 { dig(levels - 1, fill) } else { 0 };
    below + core::hint::black_box(&buf)[CHUNK - 1] as u64
}

fn free() -> u64 {
    ulib::syscall(syscall_abi::TASK_STACK_FREE, ulib::self_task())
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: stackprobe <levels> [zero] [hold]  (its stack's never-written bytes, before and after <levels> calls of 1 KiB each)\r\n",
    );
    let mut a = [0u8; 8];
    let levels = ulib::arg(1, &mut a)
        .and_then(|n| core::str::from_utf8(&a[..n]).ok().and_then(ulib::parse_u64))
        .unwrap_or(0);
    let mut fill = 0x5Au8;
    let mut hold = false;
    for i in 2..4 {
        match ulib::arg(i, &mut a).map(|n| &a[..n]) {
            Some(b"zero") => fill = 0,
            Some(b"hold") => hold = true,
            _ => {}
        }
    }
    let before = free();
    if levels > 0 {
        core::hint::black_box(dig(levels, fill));
    }
    let after = free();

    let mut line = [0u8; 96];
    let mut n = 0;
    ulib::emit(&mut line, &mut n, b"stackprobe: slot ");
    ulib::emit_dec(&mut line, &mut n, ulib::self_task());
    ulib::emit(&mut line, &mut n, b" size ");
    ulib::emit_dec(&mut line, &mut n, ulib::syscall(syscall_abi::HEAP_INFO, syscall_abi::HEAP_INFO_STACK_SIZE));
    ulib::emit(&mut line, &mut n, b" free ");
    emit_free(&mut line, &mut n, before);
    ulib::emit(&mut line, &mut n, b" then ");
    emit_free(&mut line, &mut n, after);
    ulib::emit(&mut line, &mut n, b"\r\n");
    ulib::con_write(&line[..n]);

    if hold {
        let mut msg = [0u8; 8];
        ulib::syscall4(syscall_abi::MSG_RECV, msg.as_mut_ptr() as u64, msg.len() as u64, 0, 0);
    }
    ulib::exit(0);
}

fn emit_free(line: &mut [u8], n: &mut usize, free: u64) {
    if free == syscall_abi::TASK_STACK_NONE {
        ulib::emit(line, n, b"none");
    } else {
        ulib::emit_dec(line, n, free);
    }
}

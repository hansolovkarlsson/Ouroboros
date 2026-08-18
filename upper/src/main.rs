//! The pipeline filter demo - the sixth userland program, and the
//! first one built to consume *piped input*: the shell's `left |
//! program` support spawns this, streams the left command's captured
//! output to it as IPC messages (1-64 bytes each), and marks
//! end-of-stream with one empty message (see `MSG_SEND`'s doc in
//! `syscall-abi`). This program uppercases every byte it receives,
//! writes the result to the console (`putc`), and exits cleanly on the
//! empty message - `echo hello | /EFI/ORBS/UPPER.BIN` prints `HELLO`.
//!
//! The shape any pipeline filter has on this kernel: no argv exists,
//! stdin is `MSG_RECV`, stdout is `putc`, EOF is the empty message,
//! and finishing means `EXIT` (the shell `wait`s on it). Same build
//! shape as every userland program: `aarch64-unknown-none`,
//! release-only, shared linker script, no static mutable state.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    let mut buf = [0u8; 64];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            // Nothing here should ever error (RECV_INTERRUPTED can't
            // reach a non-keyboard-owner) - park rather than spin.
            break;
        }
        let len = ((packed & 0xffff_ffff) as usize).min(buf.len());
        if len == 0 {
            // End of stream - the pipeline convention's empty message.
            syscall4(syscall_abi::EXIT, 0, 0, 0, 0);
            break;
        }
        for &b in &buf[..len] {
            syscall4(syscall_abi::PUTC, b.to_ascii_uppercase() as u64, 0, 0, 0);
        }
    }
    loop {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

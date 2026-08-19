//! The smallest real Ouroboros userland program, and the first one
//! besides the default shell: prints a banner and exits. Exists for two
//! reasons - as the living proof that "the shell is just a program"
//! (see `docs/processes.md`; this crate is loaded, relocated, and run
//! by exactly the same mechanism, `exec /EFI/ORBS/HELLO.BIN`), and as
//! the natural exercise of the `EXIT` syscall: a spawned *shell* can
//! never exit on its own (it blocks forever on keyboard input it will
//! never receive - only task 0 owns the keyboard), so task destruction
//! needed a program that runs to completion by itself.
//!
//! Built exactly like `shell/`: `aarch64-unknown-none`, release-only
//! (the same hard PIE/libcore toolchain constraint - see
//! `docs/processes.md`'s "Binary format" section), linked with the
//! shared `shell/linker.ld` (the `-T` flag in `.cargo/config.toml` is
//! workspace-relative and applies to every crate built for this
//! target), staged by the Makefile as `\EFI\ORBS\HELLO.BIN`.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

/// Placed first in `.text` by the shared linker script
/// (`KEEP(*(.text.start))`) - same contract as the shell's `_start`.
#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    print("Hello from a second program! (HELLO.BIN, running as its own task)\r\n");
    // Run for ~2 seconds of real tick time before exiting - deliberate,
    // not filler: it makes `wait`'s *blocking* path organically testable
    // (an instant exit would always be a zombie before `wait` could
    // even be typed), and demonstrates real concurrency - the ticks
    // this loop watches only advance because the scheduler keeps
    // running other tasks (and the waiter genuinely yields) meanwhile.
    let start = get_ticks();
    while get_ticks() < start + 100 {
        core::hint::spin_loop();
    }
    print("Goodbye - exiting now.\r\n");
    task_exit(0);
    // Unreachable for any task that's allowed to exit; a refused exit
    // (this program somehow running as task 0/1) just parks quietly.
    loop {
        core::hint::spin_loop();
    }
}

fn print(s: &str) {
    con_write(s.as_bytes());
}

/// Route output through the console server (task `CON_TASK`) as a batched
/// `DSPOP_WRITE` message, falling back to the kernel console (`PUTC`) if
/// there's no server this boot - same shape as the shell's `con_write`.
fn con_write(bytes: &[u8]) {
    let payload_off = syscall_abi::FS_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; syscall_abi::FS_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&syscall_abi::DSPOP_WRITE.to_le_bytes());
        req[8..16].copy_from_slice(&(n as u64).to_le_bytes());
        req[payload_off..payload_off + n].copy_from_slice(&bytes[off..off + n]);
        let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        let r = syscall4(
            syscall_abi::MSG_CALL,
            syscall_abi::CON_TASK,
            req.as_ptr() as u64,
            (payload_off + n) as u64,
            reply.as_mut_ptr() as u64,
        );
        if r >= syscall_abi::FS_ERR_MIN {
            for &b in &bytes[off..off + n] {
                syscall(syscall_abi::PUTC, b as u64);
            }
        }
        off += n;
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

fn task_exit(code: u64) {
    syscall(syscall_abi::EXIT, code);
}

fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

#[inline(always)]
fn syscall(number: u64, arg0: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") 0u64,
            in("x2") 0u64,
            in("x3") 0u64,
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

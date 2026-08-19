//! The message server - the fourth userland program, and the first
//! long-lived *server* process: the shape driver isolation's part 2
//! (the userland FAT32 filesystem server) will have. Loop: block on
//! `MSG_RECV`, echo each message straight back to whoever sent it,
//! repeat; a message that is exactly `quit` makes it exit cleanly.
//!
//! `exec /EFI/ORBS/PONG.BIN`, then from the shell:
//! `send 2 hello` -> `recv` -> `task 2: hello`. `send 2 quit` ends it
//! (collect the status with `wait 2`).
//!
//! Same build shape as `hello/`/`shell/`: `aarch64-unknown-none`,
//! release-only, shared linker script, constants from `syscall-abi`.

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
    print("pong server: echoing messages back to their senders (send `quit` to stop)\r\n");
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            // RECV_INTERRUPTED can't reach a non-keyboard-owner task,
            // and no other error is expected - park rather than spin on
            // a broken call.
            break;
        }
        let sender = packed >> 32;
        let len = ((packed & 0xffff_ffff) as usize).min(buf.len());
        // `quit` ends the server - a runtime comparison against a
        // literal, one of the exact patterns `selftest` proves safe
        // under the relocating loader.
        if &buf[..len] == b"quit" {
            print("pong server: quit received, exiting\r\n");
            syscall4(syscall_abi::EXIT, 0, 0, 0, 0);
            break;
        }
        syscall4(syscall_abi::MSG_SEND, sender, buf.as_ptr() as u64, len as u64, 0);
    }
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
                syscall4(syscall_abi::PUTC, b as u64, 0, 0, 0);
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

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

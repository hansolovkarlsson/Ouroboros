//! The filesystem server - the fifth userland program, and the first
//! real component moved out of the EL1 kernel (driver isolation part 2,
//! MINIX-style): it owns the FAT32 logic, speaks IPC to clients
//! (requests arrive as messages, mostly via `MSG_CALL`), and reaches
//! the disk through the `BLOCK_*` syscalls - which the kernel only
//! accepts from this task's fixed slot (`syscall_abi::FSD_TASK`).
//!
//! Boot-loaded by the kernel (`loader::load_fsd`, `\EFI\ORBS\FSD.BIN`)
//! into task slot 2, which is exit/kill-protected and never used by
//! `spawn`. Same build shape as `pong/`/`hello/`/`shell/`:
//! `aarch64-unknown-none`, release-only, shared linker script,
//! constants from `syscall-abi`.
//!
//! **Skeleton for now**: replies to every message with a fixed banner
//! text so the boot-load/IPC plumbing can be verified end to end; the
//! actual FAT32 engine and request protocol land next.

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
    print("fsd: filesystem server ready\r\n");
    let mut buf = [0u8; 64];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            // RECV_INTERRUPTED can't reach a non-keyboard-owner task,
            // and no other error is expected - park rather than spin
            // on a broken call (same posture as pong).
            break;
        }
        let sender = packed >> 32;
        let reply = b"fsd: not serving files yet";
        syscall4(syscall_abi::MSG_SEND, sender, reply.as_ptr() as u64, reply.len() as u64, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

fn print(s: &str) {
    for b in s.bytes() {
        syscall4(syscall_abi::PUTC, b as u64, 0, 0, 0);
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

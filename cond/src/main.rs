//! The console server (console daemon) - the seventh userland program,
//! and the second real component moved out of the EL1 kernel (after the
//! filesystem server, `fsd/`). It owns the *steady-state* console:
//! userland text output flows to it over IPC (a `DSPOP_WRITE` message,
//! normally via `MSG_CALL`), and it puts that text on the actual console,
//! while the kernel keeps only a minimal path for its own boot and fault
//! reporting.
//!
//! Boot-loaded by the kernel (`loader::load_cond`, `\EFI\ORBS\COND.BIN`)
//! into task slot 3 (`syscall_abi::CON_TASK`), which is exit/kill/wait-
//! protected and never used by `spawn` - exactly like the filesystem
//! server in slot 2. Same build shape as every other userland program
//! here: `aarch64-unknown-none`, release-only, shared linker script,
//! constants from `syscall-abi`, no static mutable state.
//!
//! **Stage 1 - byte-stream backend.** The only backend so far forwards
//! received text to the kernel's console through the gated `CON_WRITE`
//! syscall (a batched write, one syscall per message rather than one
//! `PUTC` per byte). On QEMU that console is a UART; the framebuffer-
//! rendering backend (glyphs, cursor, scroll, ANSI - the logic moved out
//! of the kernel's `fbconsole`) is Stage 2. The request loop, the reply
//! shape, and the `MSG_RECV`/`MSG_SEND` boilerplate are the filesystem
//! server's, cloned.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

const REQ_PAYLOAD: usize = syscall_abi::FS_REQ_PAYLOAD as usize;
const REPLY_PAYLOAD: usize = syscall_abi::FS_REPLY_PAYLOAD as usize;
const DATA_MAX: usize = syscall_abi::FS_DATA_MAX as usize;

fn main() -> ! {
    // Announce through the server's own backend, proving CON_WRITE works
    // from here before any client ever calls in.
    con_write_raw(b"cond: console server ready\r\n");

    // Request loop, the filesystem server's shape: block on MSG_RECV,
    // decode one request, act, reply. A console request carries text to
    // put on screen; the reply is a bare status.
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, req.as_mut_ptr() as u64, req.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            // RECV_INTERRUPTED can't reach a non-keyboard-owner task, and
            // no other error is expected - park rather than spin on a
            // broken call, the same posture the filesystem server takes.
            break;
        }
        let sender = packed >> 32;
        let len = ((packed & 0xffff_ffff) as usize).min(req.len());
        let reply_len = handle(&req[..len], &mut reply);
        // A full/unreachable sender mailbox drops the reply; the caller's
        // MSG_CALL stays blocked until Ctrl+C, same as the filesystem
        // server - nothing better exists to do with an undeliverable ack.
        syscall4(syscall_abi::MSG_SEND, sender, reply.as_ptr() as u64, reply_len as u64, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Decodes one request and builds a status-only reply. The only op today
/// is `DSPOP_WRITE` (put text on the console); an unknown/malformed
/// request is acked with status 0 rather than failing the caller - lost
/// output is never worth wedging a client's `MSG_CALL` over.
fn handle(req: &[u8], reply: &mut [u8]) -> usize {
    reply[..8].copy_from_slice(&0u64.to_le_bytes());
    if req.len() < REQ_PAYLOAD {
        return REPLY_PAYLOAD;
    }
    let op = read_u64(req, 0);
    if op == syscall_abi::DSPOP_WRITE {
        let text_len = read_u64(req, 8) as usize;
        let payload = &req[REQ_PAYLOAD..];
        let n = text_len.min(payload.len()).min(DATA_MAX);
        con_write_raw(&payload[..n]);
    }
    REPLY_PAYLOAD
}

/// Push bytes straight to the kernel console via the gated `CON_WRITE`
/// syscall. `bytes` must be at most `DATA_MAX` (the kernel's per-buffer
/// cap) - the one caller, `handle`, already clamps to it, and the banner
/// is well under.
fn con_write_raw(bytes: &[u8]) {
    syscall4(syscall_abi::CON_WRITE, bytes.as_ptr() as u64, bytes.len() as u64, 0, 0);
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
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

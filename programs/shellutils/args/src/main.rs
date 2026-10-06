//! The argv proof program (network... no - the `/bin`/PATH arc's Stage 1):
//! prints the argument vector it was spawned with, then exits. It exists to
//! prove the new argv ABI end to end - `exec /EFI/ORBS/ARGS.BIN a b c` should
//! print `argc=4` and each of `argv[0]=/EFI/ORBS/ARGS.BIN` .. `argv[3]=c`.
//! Nothing passed arguments to a spawned program before this milestone; this
//! is the first program that reads them (via `GET_ARGC`/`GET_ARG`).
//!
//! Built exactly like `hello/` (`aarch64-unknown-none`, release-only, the
//! shared `programs/linker.ld`), staged by the Makefile as `\EFI\ORBS\ARGS.BIN`.

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
    let target = syscall(syscall_abi::STDOUT_TARGET, 0);
    let argc = syscall(syscall_abi::GET_ARGC, 0);

    // "argc=<n>\r\n"
    let mut line = [0u8; 640];
    let mut n = 0;
    emit(&mut line, &mut n, b"argc=");
    emit_dec(&mut line, &mut n, argc);
    emit(&mut line, &mut n, b"\r\n");
    write_out(target, &line[..n]);

    // "argv[<i>] = <bytes>\r\n" for each argument.
    let mut i = 0;
    while i < argc {
        let mut abuf = [0u8; 512];
        let alen = syscall4(syscall_abi::GET_ARG, i, abuf.as_mut_ptr() as u64, abuf.len() as u64, 0);
        let mut line = [0u8; 640];
        let mut n = 0;
        emit(&mut line, &mut n, b"argv[");
        emit_dec(&mut line, &mut n, i);
        emit(&mut line, &mut n, b"] = ");
        if alen != syscall_abi::NO_ARG {
            let take = (alen as usize).min(abuf.len()).min(line.len() - n - 2);
            line[n..n + take].copy_from_slice(&abuf[..take]);
            n += take;
        }
        emit(&mut line, &mut n, b"\r\n");
        write_out(target, &line[..n]);
        i += 1;
    }

    // If piped/captured (not the console), signal end-of-stream.
    if target != syscall_abi::CON_TASK {
        let dummy = [0u8; 1];
        let deadline = get_ticks() + 150;
        loop {
            let r = syscall4(syscall_abi::MSG_SEND, target, dummy.as_ptr() as u64, 0, 0);
            let transient = r == syscall_abi::MSG_ERR_FULL || r == syscall_abi::MSG_ERR_DENIED;
            if r == 0 || !transient || get_ticks() > deadline {
                break;
            }
        }
    }
    task_exit(0);
    loop {
        core::hint::spin_loop();
    }
}

/// Append `s` to `buf` at `*n`, advancing `*n` (bounded).
fn emit(buf: &mut [u8], n: &mut usize, s: &[u8]) {
    for &b in s {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

/// Append the decimal form of `v` to `buf` (hand-rolled - no `core::fmt`,
/// the relocation-safe idiom this project's userland programs use).
fn emit_dec(buf: &mut [u8], n: &mut usize, v: u64) {
    let mut d = [0u8; 20];
    let mut i = 20;
    let mut x = v;
    loop {
        i -= 1;
        d[i] = b'0' + (x % 10) as u8;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    emit(buf, n, &d[i..]);
}

fn write_out(target: u64, bytes: &[u8]) {
    if target == syscall_abi::CON_TASK {
        con_write(bytes);
    } else {
        pipe_out(target, bytes);
    }
}

fn pipe_out(target: u64, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::MSG_MAX_LEN as usize);
        let chunk = &bytes[off..off + n];
        let deadline = get_ticks() + 150;
        loop {
            let r = syscall4(syscall_abi::MSG_SEND, target, chunk.as_ptr() as u64, n as u64, 0);
            if r == 0 {
                break;
            }
            let transient = r == syscall_abi::MSG_ERR_FULL || r == syscall_abi::MSG_ERR_DENIED;
            if !transient || get_ticks() > deadline {
                return;
            }
        }
        off += n;
    }
}

fn con_write(bytes: &[u8]) {
    let payload_off = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; ninep_abi::NP_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&ninep_abi::NP_WRITE_FILE.to_le_bytes());
        // tree (a8) and path_len (a16) stay 0; data_len at a1 (offset 24).
        req[24..32].copy_from_slice(&(n as u64).to_le_bytes());
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

fn task_exit(code: u64) {
    syscall(syscall_abi::EXIT, code);
}

fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
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

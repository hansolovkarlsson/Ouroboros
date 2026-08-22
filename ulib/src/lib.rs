//! `ulib` - the shared userland support library for Ouroboros command
//! programs (standalone-binaries arc, Stage 4). It factors out the
//! boilerplate every `/bin` program repeats: the `svc` syscall wrappers,
//! reading argv (`GET_ARGC`/`GET_ARG`), routing output to the right place
//! (the console server, or - when the shell spawned us as a pipe producer or
//! `> file` source - back to the shell), a hand-rolled decimal formatter, and
//! `exit`. A command program is then just `_start` + its own logic over these
//! helpers.
//!
//! Not a full libc, and deliberately tiny: no allocator, no `core::fmt` in
//! the crash-prone paths (see `docs/processes.md`'s relocation notes), fixed
//! buffers only. Each program still provides its own `_start` (the entry, at
//! `.text.start`); `ulib` provides the one `#[panic_handler]` the binary
//! needs, so command crates don't repeat it.
//!
//! Built for `aarch64-unknown-none` as an ordinary dependency; it links into
//! each command binary under that binary's PIE flags (the shared
//! `shell/linker.ld`), so it needs no linker script of its own.

#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

/// A syscall with one argument (the rest zeroed).
#[inline(always)]
pub fn syscall(number: u64, arg0: u64) -> u64 {
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

/// A syscall with up to four arguments.
#[inline(always)]
pub fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
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

/// The number of arguments this program was spawned with (`argv[0]` is the
/// program name).
pub fn argc() -> u64 {
    syscall(syscall_abi::GET_ARGC, 0)
}

/// Copy argument `index` into `buf`, returning the bytes written (capped at
/// `buf.len()`), or `None` if there's no such argument.
pub fn arg(index: u64, buf: &mut [u8]) -> Option<usize> {
    let n = syscall4(
        syscall_abi::GET_ARG,
        index,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    );
    if n == syscall_abi::NO_ARG {
        None
    } else {
        Some((n as usize).min(buf.len()))
    }
}

/// Where this program's output should go: the console server by default, or -
/// when the shell spawned us as a pipe producer or `exec … > file` source -
/// the shell's own task, which relays or captures it.
pub fn stdout_target() -> u64 {
    syscall(syscall_abi::STDOUT_TARGET, 0)
}

/// The preemption tick count since boot (`uptime`'s source).
pub fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

/// Write `bytes` to this program's stdout target (console or the relaying
/// shell). The one output call a command should use.
pub fn write_out(target: u64, bytes: &[u8]) {
    if target == syscall_abi::CON_TASK {
        con_write(bytes);
    } else {
        pipe_out(target, bytes);
    }
}

/// Route output through the console server (a batched `DSPOP_WRITE` message),
/// falling back to the kernel console (`PUTC`) if there's no server this boot.
pub fn con_write(bytes: &[u8]) {
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

/// Send `bytes` as raw `MSG_MAX_LEN`-chunked messages to `target` (the shell,
/// when we're a pipe producer / capture source), with the same bounded retry
/// on a full mailbox or a not-yet-delegated send as `hello`/`args`.
pub fn pipe_out(target: u64, bytes: &[u8]) {
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

/// If our output is piped/captured (target isn't the console), signal
/// end-of-stream with an empty message so the reading task knows we're done.
/// A no-op when writing straight to the console.
pub fn end_of_stream(target: u64) {
    if target == syscall_abi::CON_TASK {
        return;
    }
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

/// Append `s` to `buf` at `*n`, advancing `*n` (bounded, never overruns).
pub fn emit(buf: &mut [u8], n: &mut usize, s: &[u8]) {
    for &b in s {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

/// Append the decimal form of `v` to `buf` - hand-rolled, no `core::fmt`
/// (the relocation-safe idiom this project's userland uses).
pub fn emit_dec(buf: &mut [u8], n: &mut usize, v: u64) {
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

/// End this task with `code` (never returns). A program that's somehow
/// unkillable (task 0/1) just parks quietly instead.
pub fn exit(code: u64) -> ! {
    syscall(syscall_abi::EXIT, code);
    loop {
        core::hint::spin_loop();
    }
}

/// The one panic handler the command binary needs - provided here so each
/// command crate doesn't repeat it. Parks; there's nowhere to report to.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

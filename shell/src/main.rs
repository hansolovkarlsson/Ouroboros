//! Ouroboros's default shell — a real, separately-loaded userland program,
//! not kernel code. This is the first thing the kernel loads and runs: see
//! `kernel/src/loader.rs` for how a binary gets from the ESP's filesystem
//! into memory, and `kernel/src/tasks.rs` for how it gets turned into a
//! running EL0 task. The kernel picks *which* binary to load from a config
//! file, so this program can be replaced without touching kernel code -
//! see `docs/processes.md` for the full mechanism and a guide to writing a
//! replacement.
//!
//! Everything here used to live at EL1, in the kernel's own `shell.rs`
//! (buffer, backspace handling, echo), driven by a dedicated `shell_input`
//! syscall from a trivial EL0 poll loop. That syscall is gone now - this
//! program calls `try_read_char`/`putc` directly and does its own line
//! editing, which is what "the shell is a separate process" actually means
//! in practice, not just in theory.
//!
//! Deliberately has no global mutable state (the input buffer is a local
//! in `main`'s stack frame, not a `static`): `linker.ld` defines but
//! asserts empty `.data`/`.bss`, since there's no crt0 here to zero a real
//! `.bss` before `main` runs, and the loader only copies exactly the
//! file's bytes - a nonzero `.bss` would just be missing from memory
//! entirely. Fine for now (a line buffer plus a length counter is all this
//! program needs); a future program that wants real global state needs
//! that crt0 written first.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const BUFFER_SIZE: usize = 128;
const BACKSPACE: u8 = 0x08;
const DEL: u8 = 0x7f;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

// Mirrors syscall.rs::NO_CHAR exactly - kept in sync by hand for now (no
// shared ABI crate yet; see docs/processes.md's "known rough edges").
const NO_CHAR: u64 = u64::MAX;

const SYS_TRY_READ_CHAR: u64 = 3;
const SYS_PUTC: u64 = 4;

/// Placed first in `.text` by `linker.ld` (`KEEP(*(.text.start))`) so it
/// lands at file/VA offset 0 - `tasks.rs` sets a loaded program's
/// `elr_el1` to exactly the load base, no symbol table involved.
#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    for &b in b"Ouroboros userland shell\r\n" {
        putc(b);
    }

    let mut buf = [0u8; BUFFER_SIZE];
    let mut len = 0usize;
    print_prompt();
    loop {
        match try_read_char() {
            Some(byte) => on_byte(byte, &mut buf, &mut len),
            None => wfe(),
        }
    }
}

fn print_prompt() {
    putc(b'$');
    putc(b' ');
}

/// Same shape as the kernel's old `shell.rs::on_byte`: CR/LF submits the
/// line (echoed back, then a fresh prompt), backspace/DEL erases via the
/// standard destructive-backspace sequence, anything else is appended and
/// echoed immediately. Phase 2 (commands) replaces "echo the line" with
/// real parsing - nothing else here should need to change shape for that.
fn on_byte(byte: u8, buf: &mut [u8; BUFFER_SIZE], len: &mut usize) {
    match byte {
        CR | LF => {
            putc(CR);
            putc(LF);
            for &b in &buf[..*len] {
                putc(b);
            }
            putc(CR);
            putc(LF);
            *len = 0;
            print_prompt();
        }
        BACKSPACE | DEL => {
            if *len > 0 {
                *len -= 1;
                putc(BACKSPACE);
                putc(b' ');
                putc(BACKSPACE);
            }
        }
        byte => {
            if *len < BUFFER_SIZE {
                buf[*len] = byte;
                *len += 1;
                putc(byte);
            }
            // Buffer full: silently drop further bytes, same as before.
        }
    }
}

#[inline(always)]
fn syscall(number: u64, arg0: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

fn try_read_char() -> Option<u8> {
    match syscall(SYS_TRY_READ_CHAR, 0) {
        NO_CHAR => None,
        byte => Some(byte as u8),
    }
}

fn putc(byte: u8) {
    syscall(SYS_PUTC, byte as u64);
}

fn wfe() {
    unsafe {
        asm!("wfe", options(nomem, nostack, preserves_flags));
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        wfe();
    }
}

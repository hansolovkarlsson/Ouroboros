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
//!
//! ## Phase 2: commands
//!
//! [`on_byte`] no longer just echoes the completed line - [`run_line`]
//! tokenizes it (whitespace-split, no quoting) and dispatches to a small
//! builtin table. `uptime` is the first builtin that needs real kernel
//! state (`get_ticks`, syscall 6) rather than being another echo demo -
//! this program can no longer just read `exceptions.rs`'s statics
//! directly the way the kernel-resident line editor it replaced could, so
//! exposing that state needed a new syscall. See `docs/processes.md` for
//! the full syscall table.

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
const SYS_GET_TICKS: u64 = 6;

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
/// line (parsed and dispatched via [`run_line`], then a fresh prompt),
/// backspace/DEL erases via the standard destructive-backspace sequence,
/// anything else is appended and echoed immediately.
fn on_byte(byte: u8, buf: &mut [u8; BUFFER_SIZE], len: &mut usize) {
    match byte {
        CR | LF => {
            putc(CR);
            putc(LF);
            // buf[..*len] is whatever bytes try_read_char returned,
            // completely unfiltered (see the `byte` arm below) - not
            // guaranteed valid UTF-8 (e.g. a pasted multi-byte sequence
            // split across separate reads, or a stray high byte), so this
            // has to be checked, not assumed.
            match core::str::from_utf8(&buf[..*len]) {
                Ok(line) => run_line(line),
                Err(_) => print_line("input wasn't valid UTF-8"),
            }
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

/// Tokenizes on whitespace (no quoting - `echo "a b"` sees two words, not
/// one) and dispatches to a builtin by name. An empty line (just Enter,
/// or a line of only spaces) does nothing, same as a real shell.
fn run_line(line: &str) {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else { return };

    match command {
        "help" => print_line("commands: help, echo, uptime, clear"),
        "echo" => {
            let mut first = true;
            for word in words {
                if !first {
                    putc(b' ');
                }
                for b in word.bytes() {
                    putc(b);
                }
                first = false;
            }
            putc(CR);
            putc(LF);
        }
        "uptime" => {
            print_u64_decimal(get_ticks());
            print_line(" ticks since boot");
        }
        "clear" => {
            // ANSI clear-screen + cursor-home - the shell's own escape
            // sequence, not a syscall; the console itself has no notion
            // of a screen, just a byte stream.
            for &b in b"\x1b[2J\x1b[H" {
                putc(b);
            }
        }
        _ => {
            print_str("unknown command: ");
            print_line(command);
        }
    }
}

fn print_str(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

fn print_line(s: &str) {
    print_str(s);
    putc(CR);
    putc(LF);
}

/// Hand-rolled rather than `write!`/`core::fmt::Arguments`: that machinery
/// builds its per-argument dispatch out of *data* (an array of function
/// pointers, one per formatted argument) rather than direct `bl` calls -
/// fine under a real relocating loader, but this one applies none (see
/// `linker.ld`'s doc comment and `docs/processes.md`). A binary linked for
/// base `0x0` but loaded somewhere else (always, in practice - see
/// `loader.rs`) has no way to know those embedded pointer values need
/// correcting, so they point at whatever the link-time address `0x0`
/// would have meant - resulting in exactly the crash this replaced
/// (`ELR_EL1` landing on a tiny near-null address instead of real code,
/// confirmed directly by trying `write!` here first). Direct calls
/// (`putc`, `print_str`, this function) compile to PC-relative `bl` and
/// have no such problem - so the fix is avoiding `core::fmt` entirely for
/// anything a loaded program formats, not just here.
fn print_u64_decimal(mut n: u64) {
    if n == 0 {
        putc(b'0');
        return;
    }
    let mut digits = [0u8; 20]; // u64::MAX has 20 decimal digits
    let mut count = 0;
    while n > 0 {
        digits[count] = b'0' + (n % 10) as u8;
        n /= 10;
        count += 1;
    }
    while count > 0 {
        count -= 1;
        putc(digits[count]);
    }
}

fn get_ticks() -> u64 {
    syscall(SYS_GET_TICKS, 0)
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

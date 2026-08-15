//! Phase 1 of "get to a shell": a line editor that buffers input a byte at
//! a time and echoes the completed line back on Enter. Runs entirely at
//! EL1, driven by `syscall.rs`'s `shell_input` handler — the EL0 task that
//! feeds it (`tasks.rs`'s task 0) is a trivial poll loop with no editing
//! logic of its own; see that module's doc comment for why the split is
//! there.
//!
//! Phase 2 (commands) replaces the "echo the whole line" step in [`on_byte`]
//! with real parsing/dispatch — nothing else here should need to change
//! shape for that.

use core::cell::UnsafeCell;

use crate::console;

const BUFFER_SIZE: usize = 128;
const BACKSPACE: u8 = 0x08;
const DEL: u8 = 0x7f;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

struct LineBuffer {
    bytes: UnsafeCell<[u8; BUFFER_SIZE]>,
    len: UnsafeCell<usize>,
}

// SAFETY: single-core; only ever touched from `on_byte`, which is only
// ever reached via the SVC dispatch path - exception entry masks IRQs, so
// two calls can never overlap.
unsafe impl Sync for LineBuffer {}

static BUFFER: LineBuffer = LineBuffer { bytes: UnsafeCell::new([0; BUFFER_SIZE]), len: UnsafeCell::new(0) };

/// Handles one byte of input from the shell task's poll loop. CR and LF are
/// both treated as "end of line" (whichever a given terminal sends);
/// backspace and DEL both erase, via the standard destructive-backspace
/// sequence (`\b`, space, `\b`) so the erased character actually disappears
/// from the terminal rather than just moving the cursor over it.
pub fn on_byte(byte: u8) {
    match byte {
        CR | LF => {
            console::putc(CR);
            console::putc(LF);

            let len = unsafe { *BUFFER.len.get() };
            let bytes = unsafe { &*BUFFER.bytes.get() };
            match core::str::from_utf8(&bytes[..len]) {
                Ok(line) => console::println!("Ouroboros kernel: you typed: {line}"),
                Err(_) => console::println!("Ouroboros kernel: you typed {len} byte(s) (not valid UTF-8)"),
            }
            unsafe { *BUFFER.len.get() = 0 };
        }
        BACKSPACE | DEL => {
            let len = unsafe { &mut *BUFFER.len.get() };
            if *len > 0 {
                *len -= 1;
                console::putc(BACKSPACE);
                console::putc(b' ');
                console::putc(BACKSPACE);
            }
        }
        byte => {
            let len = unsafe { &mut *BUFFER.len.get() };
            if *len < BUFFER_SIZE {
                unsafe { (*BUFFER.bytes.get())[*len] = byte };
                *len += 1;
                console::putc(byte);
            }
            // Buffer full: silently drop further bytes until Enter/erase -
            // good enough for phase 1, no line-too-long feedback yet.
        }
    }
}

//! `clear` - externalized command (standalone-binaries Stage 4). Sends the
//! ANSI clear-screen + cursor-home sequence to the console, since 2026-10-11
//! after normal video and the cursor shown (`CSI 0 m`, `CSI ? 25 h`): a
//! program that exits with the console inverted or its cursor hidden leaves
//! it so, as on Unix, and `clear` is the way back. No filesystem, no cwd -
//! part of the first externalized batch. (The console framebuffer backend
//! acts on every one of these; a byte-stream console passes them through to
//! the terminal.)

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    ulib::write_out(target, b"\x1b[0m\x1b[?25h\x1b[2J\x1b[H");
    ulib::end_of_stream(target);
    ulib::exit(0);
}

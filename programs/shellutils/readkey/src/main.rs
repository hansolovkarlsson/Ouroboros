//! `readkey` - a tiny interactive keyboard diagnostic, and the proof that a
//! `/bin` program can read the keyboard. Echoes each key you press as its
//! character and decimal byte value; **q** quits, and **Ctrl+C** terminates it
//! (the kernel kills the foreground program). Before the keyboard-ownership
//! work an interactive program like this *had* to be a shell builtin - only
//! the shell owned the keyboard. Now the shell hands a foreground command the
//! keyboard at spawn, so this reads input with `ulib::read_char` like any
//! ordinary program. The same mechanism a future editor or REPL would use.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::con_write(b"readkey: press keys (q to quit, Ctrl+C to abort)\r\n");
    loop {
        let c = ulib::read_char();
        if c == b'q' {
            break;
        }
        // Show the key: the printable character (or a placeholder), then its
        // decimal byte value, one per line.
        ulib::con_write(b"  key: ");
        if (0x20..0x7f).contains(&c) {
            ulib::con_write(&[c]);
        } else {
            ulib::con_write(b"?");
        }
        ulib::con_write(b"  (");
        let mut buf = [0u8; 8];
        let mut n = 0usize;
        ulib::emit_dec(&mut buf, &mut n, c as u64);
        ulib::con_write(&buf[..n]);
        ulib::con_write(b")\r\n");
    }
    ulib::con_write(b"readkey: bye\r\n");
    ulib::exit(0);
}

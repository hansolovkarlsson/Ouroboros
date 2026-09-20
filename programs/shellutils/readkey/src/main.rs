//! `readkey` - a tiny interactive keyboard diagnostic, and the proof that a
//! `/bin` program can read the keyboard. Echoes each key you press as its
//! character and decimal byte value; **q** quits, and **Ctrl+C** terminates it
//! (the kernel kills the foreground program). Before the keyboard-ownership
//! work an interactive program like this *had* to be a shell builtin - only
//! the shell owned the keyboard. Now the shell hands a foreground command the
//! keyboard at spawn, so this reads input with `ulib::read_char` like any
//! ordinary program. The same mechanism a future editor or REPL would use.
//!
//! `readkey poll` spins on `try_read_char` instead of blocking. It exists as
//! the observer for the keyboard-owner gate: run it in the background
//! (`exec /bin/readkey poll`) and type at the shell. If a non-owner could
//! consume keystrokes, the spinner would echo them and the shell would see
//! nothing; with the gate, the spinner answers only when it owns the keyboard.
//! It spins (a syscall per iteration, no yield exists yet), so it takes every
//! timeslice it is given; it gives up on its own after a tick budget, and is
//! an observer to run for the witness and kill after regardless.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: readkey [poll]  (poll: spin on try_read_char instead of blocking; an observer for the keyboard-owner gate, kill it when done)\r\n");
    let mut mode = [0u8; 8];
    let poll = match ulib::arg(1, &mut mode) {
        None => false,
        Some(n) if &mode[..n] == b"poll" => true,
        Some(_) => {
            ulib::con_write(b"readkey: unknown mode (the only one is `poll`)\r\n");
            ulib::exit(1);
        }
    };
    if poll {
        ulib::con_write(b"readkey: polling (in the foreground: q to quit, Ctrl+C to abort; in the background it never owns the keyboard, so kill it from the shell)\r\n");
    } else {
        ulib::con_write(b"readkey: press keys (q to quit, Ctrl+C to abort)\r\n");
    }
    // Poll mode spins (a syscall per iteration, no yield exists yet), so it
    // is bounded: it gives up after POLL_TICKS ticks, which is long enough
    // for the witness and short enough that a forgotten one does not cost
    // the rest of the session.
    const POLL_TICKS: u64 = 3000;
    let started = ulib::get_ticks();
    loop {
        let c = if poll {
            if ulib::get_ticks().wrapping_sub(started) > POLL_TICKS {
                ulib::con_write(b"readkey: poll budget spent, exiting\r\n");
                ulib::exit(0);
            }
            match ulib::try_read_char() {
                Some(c) => c,
                None => continue,
            }
        } else {
            ulib::read_char()
        };
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

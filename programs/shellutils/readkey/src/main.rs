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
//!
//! `readkey spin [ticks] [io|keep]` is the observer for the kernel's keyboard
//! queue (`docs/roadmap/roadmap-ctrl-c.md`, step 0): it prints `readkey:
//! spinning`, runs for `ticks` ticks (default `SPIN_TICKS`) without reading
//! the keyboard, then reads everything waiting and prints it on one line,
//! `readkey: got <byte> <byte> ...` in decimal. Keys pressed during the spin
//! reach it only if the kernel kept the bytes it read meanwhile; until
//! 2026-10-07 it threw them away. With `io` it writes a dot to the console
//! every five ticks while it spins, so it is mostly blocked in the console
//! server's call, as an editor redrawing is; with `keep` it exits without
//! reading, saying `readkey: kept`, and leaves what was typed to the shell.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: readkey [poll|spin]  (poll: spin on try_read_char instead of blocking, an observer for the keyboard-owner gate, kill it when done; spin [ticks] [io|keep]: run three seconds without reading, then print every byte typed meanwhile)\r\n");
    let mut mode = [0u8; 8];
    let poll = match ulib::arg(1, &mut mode) {
        None => false,
        Some(n) if &mode[..n] == b"poll" => true,
        Some(n) if &mode[..n] == b"spin" => spin_then_drain(),
        Some(_) => {
            ulib::con_write(b"readkey: unknown mode (`poll` or `spin`)\r\n");
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

/// Ticks `readkey spin` runs without reading unless told otherwise
/// (`readkey spin <ticks>`): three seconds at the 20 ms tick, time for a rig
/// to press a few keys.
const SPIN_TICKS: u64 = 150;

/// `readkey spin [ticks] [io|keep]`: see the module doc. Never returns.
fn spin_then_drain() -> ! {
    let mut arg = [0u8; 8];
    let ticks = ulib::arg(2, &mut arg)
        .and_then(|n| core::str::from_utf8(&arg[..n]).ok())
        .and_then(ulib::parse_u64)
        .unwrap_or(SPIN_TICKS);
    let mut how = [0u8; 8];
    let how_len = ulib::arg(3, &mut how).unwrap_or(0);
    let io = &how[..how_len] == b"io";
    let keep = &how[..how_len] == b"keep";
    ulib::con_write(b"readkey: spinning\r\n");
    let started = ulib::get_ticks();
    let mut last_dot = started;
    while ulib::get_ticks().wrapping_sub(started) < ticks {
        // `io`: a dot every five ticks, so the spin spends its time blocked
        // in con_write's call to the console server, the wait an editor
        // redrawing is in.
        if io && ulib::get_ticks().wrapping_sub(last_dot) >= 5 {
            ulib::con_write(b".");
            last_dot = ulib::get_ticks();
        }
        core::hint::spin_loop();
    }
    if keep {
        // `keep`: leave what was typed for the shell.
        ulib::con_write(b"\r\nreadkey: kept\r\n");
        ulib::exit(0);
    }
    // Everything first, then the printing: printing is con_write, and a read
    // between two of those must not depend on what a wait does meanwhile.
    let mut got = [0u8; 128];
    let mut n_got = 0usize;
    while let Some(c) = ulib::try_read_char() {
        if n_got < got.len() {
            got[n_got] = c;
            n_got += 1;
        }
    }
    ulib::con_write(b"\r\nreadkey: got");
    let mut buf = [0u8; 8];
    for &c in &got[..n_got] {
        let mut n = 0usize;
        ulib::emit_dec(&mut buf, &mut n, c as u64);
        ulib::con_write(b" ");
        ulib::con_write(&buf[..n]);
    }
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

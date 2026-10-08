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
//! `readkey spin [ticks] [keep|msg]` is the observer for the kernel's keyboard
//! queue (`docs/roadmap/roadmap-ctrl-c.md`, step 0): it prints `readkey:
//! spinning`, runs for `ticks` ticks (default `SPIN_TICKS`) without reading
//! the keyboard, then reads everything waiting and prints it on one line,
//! `readkey: got <byte> <byte> ...` in decimal. Keys pressed during the spin
//! reach it only if the kernel kept the bytes it read meanwhile; until
//! 2026-10-07 it threw them away. With `keep` it exits without reading,
//! writing `readkey: kept` to its stdout, and leaves what was typed to the
//! shell; with `msg` it then blocks in `MSG_RECV` until a message comes, so
//! `readkey spin 150 keep | readkey spin 0 msg` holds the keyboard owner (a
//! pipeline's last stage) in a message wait for three seconds.
//!
//! `readkey spin [ticks] raw` spins and drains in raw mode, so a Ctrl+C typed
//! during the spin comes back as 3; `readkey spin [ticks] rawkeep` is `keep`
//! in raw mode: a Ctrl+C typed during the spin is queued as a byte and left
//! unread when it exits.
//!
//! `readkey raw` is the observer for the keyboard modes
//! (`docs/roadmap/roadmap-ctrl-c.md`, step 1): it puts itself in raw mode
//! (`KBD_MODE`) and then echoes keys as the default mode does, so Ctrl+C
//! shows as `(3)` instead of ending it; Ctrl+\ still ends it, in every
//! mode. `readkey mode` prints the mode this task starts in, `readkey: mode
//! cooked in slot N` or `readkey: mode raw in slot N`, and exits: run after a
//! `readkey raw` ended in the same slot (it prints its slot too), it shows the
//! mode did not outlive the task.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: readkey [poll|spin|raw|mode]  (poll: spin on try_read_char instead of blocking, an observer for the keyboard-owner gate, kill it when done; spin [ticks] [keep|msg|raw|rawkeep]: run three seconds without reading, then print every byte typed meanwhile; raw: Ctrl+C is a key, Ctrl+\\ ends it; mode: print this task's keyboard mode)\r\n");
    let mut mode = [0u8; 8];
    let mut raw = false;
    let poll = match ulib::arg(1, &mut mode) {
        None => false,
        Some(n) if &mode[..n] == b"poll" => true,
        Some(n) if &mode[..n] == b"spin" => spin_then_drain(),
        Some(n) if &mode[..n] == b"mode" => print_mode(),
        Some(n) if &mode[..n] == b"raw" => {
            if ulib::kbd_mode(syscall_abi::KBD_RAW) != syscall_abi::KBD_RAW {
                ulib::con_write(b"readkey: the kernel refused raw mode\r\n");
                ulib::exit(1);
            }
            raw = true;
            false
        }
        Some(_) => {
            ulib::con_write(b"readkey: unknown mode (`poll`, `spin`, `raw` or `mode`)\r\n");
            ulib::exit(1);
        }
    };
    if poll {
        ulib::con_write(b"readkey: polling (in the foreground: q to quit, Ctrl+C to abort; in the background it never owns the keyboard, so kill it from the shell)\r\n");
    } else if raw {
        ulib::con_write(b"readkey: raw in slot ");
        write_dec(ulib::self_task());
        ulib::con_write(b", press keys (q to quit, Ctrl+\\ to abort; Ctrl+C is a key)\r\n");
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
        write_dec(c as u64);
        ulib::con_write(b")\r\n");
    }
    ulib::con_write(b"readkey: bye\r\n");
    ulib::exit(0);
}

/// `readkey mode`: print the keyboard mode this task is in, and exit.
/// The slot is printed so a rig can tell that a later `readkey mode` ran
/// where a `readkey raw` did: only then does "starts cooked" show the mode
/// ending with the task rather than a slot that was never raw.
fn print_mode() -> ! {
    match ulib::kbd_mode(syscall_abi::KBD_QUERY) {
        syscall_abi::KBD_RAW => ulib::con_write(b"readkey: mode raw in slot "),
        syscall_abi::KBD_COOKED => ulib::con_write(b"readkey: mode cooked in slot "),
        _ => ulib::con_write(b"readkey: mode unknown in slot "),
    }
    write_dec(ulib::self_task());
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

/// `v` in decimal, to the console.
fn write_dec(v: u64) {
    let mut buf = [0u8; 20];
    let mut n = 0usize;
    ulib::emit_dec(&mut buf, &mut n, v);
    ulib::con_write(&buf[..n]);
}

/// Ticks `readkey spin` runs without reading unless told otherwise
/// (`readkey spin <ticks>`): three seconds at the 20 ms tick, time for a rig
/// to press a few keys.
const SPIN_TICKS: u64 = 150;

/// `readkey spin [ticks] [keep|msg]`: see the module doc. Never returns.
fn spin_then_drain() -> ! {
    let mut arg = [0u8; 8];
    let ticks = ulib::arg(2, &mut arg)
        .and_then(|n| core::str::from_utf8(&arg[..n]).ok())
        .and_then(ulib::parse_u64)
        .unwrap_or(SPIN_TICKS);
    let mut how = [0u8; 8];
    let how_len = ulib::arg(3, &mut how).unwrap_or(0);
    let msg = &how[..how_len] == b"msg";
    // `rawkeep`: `keep`, in raw mode, so a Ctrl+C typed during the spin is
    // queued as a byte and left unread when it exits: the case where the
    // keyboard reverts to the shell with a raw owner's Ctrl+C still queued.
    let rawkeep = &how[..how_len] == b"rawkeep";
    // `raw`: the spin and the drain in raw mode, so a Ctrl+C typed during the
    // spin must come back as 3: read ahead by the tick, queued as a byte.
    let raw = &how[..how_len] == b"raw";
    if (rawkeep || raw) && ulib::kbd_mode(syscall_abi::KBD_RAW) != syscall_abi::KBD_RAW {
        ulib::con_write(b"readkey: the kernel refused raw mode\r\n");
        ulib::exit(1);
    }
    let keep = rawkeep || &how[..how_len] == b"keep";
    ulib::con_write(b"readkey: spinning\r\n");
    let started = ulib::get_ticks();
    while ulib::get_ticks().wrapping_sub(started) < ticks {
        core::hint::spin_loop();
    }
    if keep {
        // `keep`: leave what was typed for the shell. To stdout, so that a
        // `msg` stage after it in a pipeline wakes on it.
        let target = ulib::stdout_target();
        ulib::write_out(target, b"readkey: kept\r\n");
        ulib::end_of_stream(target);
        ulib::exit(0);
    }
    if msg {
        // `msg`: block in MSG_RECV until the stage before it in a pipeline
        // writes, so the keyboard owner (a pipeline's last stage) sits in a
        // message wait the whole time, the wait a program blocked on a
        // server is in.
        let mut m = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        ulib::syscall4(syscall_abi::MSG_RECV, m.as_mut_ptr() as u64, m.len() as u64, 0, 0);
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
    for &c in &got[..n_got] {
        ulib::con_write(b" ");
        write_dec(c as u64);
    }
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}


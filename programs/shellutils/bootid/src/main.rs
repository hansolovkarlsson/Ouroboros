//! `bootid` - this boot's identity, as the kernel established it before
//! `ExitBootServices` (step 3 of `docs/roadmap/roadmap-session-auth.md`): the
//! boot counter, which stores held it from before this boot, and how many
//! bytes of boot entropy the firmware gave.
//!
//! It also asks for the entropy itself and reports the answer, which must be a
//! refusal: the kernel hands those bytes to the network server alone. So every
//! run is a check of that gate, and exits 2 if it ever lets this program read.
//! Otherwise it exits 0 with a counter and 1 without one.

#![no_std]
#![no_main]

use syscall_abi::{BOOT_ID_STORE_FILE, BOOT_ID_STORE_VARIABLE};

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut buf = [0u8; 160];
    let mut n = 0;

    let counter = ulib::boot_id();
    match counter {
        Some(c) => {
            ulib::emit(&mut buf, &mut n, b"boot ");
            ulib::emit_dec(&mut buf, &mut n, c);
            ulib::emit(&mut buf, &mut n, b"\r\n");
        }
        None => ulib::emit(&mut buf, &mut n, b"no boot counter this boot (sessions will not be keyed)\r\n"),
    }
    ulib::write_out(target, &buf[..n]);

    let stores = ulib::boot_id_stores();
    n = 0;
    ulib::emit(&mut buf, &mut n, b"  held from before this boot: variable ");
    ulib::emit(&mut buf, &mut n, if stores & BOOT_ID_STORE_VARIABLE != 0 { b"yes" } else { b"no" });
    ulib::emit(&mut buf, &mut n, b", file ");
    ulib::emit(&mut buf, &mut n, if stores & BOOT_ID_STORE_FILE != 0 { b"yes" } else { b"no" });
    ulib::emit(&mut buf, &mut n, b"\r\n  boot entropy: ");
    ulib::emit_dec(&mut buf, &mut n, ulib::boot_entropy_len() as u64);
    ulib::emit(&mut buf, &mut n, b" bytes\r\n");
    ulib::write_out(target, &buf[..n]);

    let mut probe = [0u8; 64];
    let leaked = ulib::boot_entropy(&mut probe).is_some();
    let msg: &[u8] = if leaked {
        b"  LEAK: the kernel gave the boot entropy to a task that is not the network server\r\n"
    } else {
        b"  boot entropy withheld from this program (the network server only)\r\n"
    };
    ulib::write_out(target, msg);
    ulib::end_of_stream(target);
    ulib::exit(if leaked {
        2
    } else if counter.is_some() {
        0
    } else {
        1
    });
}

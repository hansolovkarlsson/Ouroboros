//! `pwd` - print the working directory. Externalized from a shell builtin to a
//! real `/bin/pwd`: the shell delivers each spawned program its cwd at spawn
//! (`CWD_STAGE`), which `ulib::cwd` (`GET_CWD`) reads back, so a program prints
//! the same directory the builtin did - behaviour-identical, just no longer
//! compiled into the shell.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    let mut buf = [0u8; ulib::PATH_MAX];
    let n = ulib::cwd(&mut buf);
    if n == 0 {
        // No cwd delivered (spawned without one) - the root, like the shell's
        // own default.
        ulib::write_out(target, b"/");
    } else {
        ulib::write_out(target, &buf[..n]);
    }
    ulib::write_out(target, b"\r\n");
    ulib::end_of_stream(target);
    ulib::exit(0);
}

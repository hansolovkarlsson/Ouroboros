//! `id` - print this task's user identity as `uid=N gid=M`.
//!
//! Reads the identity the kernel carries per task (`GET_ID` via
//! `ulib::getuid`/`getgid`). Numeric only - there is no `/etc/passwd` yet, so
//! names aren't resolved (that arrives with the login/users step). Because a
//! spawned command inherits the shell's identity, running `id` after `su`
//! reports the new user - the proof that identity is inherited across spawn.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: id  (print this session's uid and gid)\r\n");
    let target = ulib::stdout_target();

    let uid = ulib::getuid();
    let gid = ulib::getgid();

    let mut line = [0u8; 48];
    let mut w = 0usize;
    append(&mut line, &mut w, b"uid=");
    ulib::emit_dec(&mut line, &mut w, uid as u64);
    append(&mut line, &mut w, b" gid=");
    ulib::emit_dec(&mut line, &mut w, gid as u64);
    append(&mut line, &mut w, b"\r\n");

    ulib::write_out(target, &line[..w]);
    ulib::end_of_stream(target);
    ulib::exit(0);
}

fn append(buf: &mut [u8], n: &mut usize, src: &[u8]) {
    for &b in src {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

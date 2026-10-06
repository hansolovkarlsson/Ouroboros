//! `printenv` - print the environment this program inherited from the shell,
//! one `NAME=VALUE` per line. The consumer that proves the environment-export
//! ABI end to end: the shell serializes its env into an `ENV_STAGE` blob at
//! spawn, the kernel delivers it per-task, and a program reads it back via
//! `GET_ENVC`/`GET_ENV` (here through `ulib::env_count`/`env_at`). A pipeline
//! stage like any other, so `printenv | grep PATH` works.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: printenv   (print the inherited environment, one NAME=VALUE per line)\r\n",
    );
    let target = ulib::stdout_target();
    let n = ulib::env_count();
    // One entry (NAME=VALUE) at a time: 256 bytes holds any entry the shell
    // makes (GET_ENV would accept up to ENV_MAX, 2048).
    let mut buf = [0u8; 256];
    let mut i = 0;
    while i < n {
        if let Some(len) = ulib::env_at(i, &mut buf) {
            ulib::write_out(target, &buf[..len]);
            ulib::write_out(target, b"\r\n");
        }
        i += 1;
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

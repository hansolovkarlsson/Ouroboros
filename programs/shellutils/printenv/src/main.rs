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
    // One whole entry (NAME=VALUE) at a time, env_at's buffer being the
    // store's size: until 2026-10-06 this was 256 bytes, and a longer entry
    // (any spawner can stage one) printed as a shorter one.
    let mut buf = [0u8; ulib::ENV_ENTRY_BUF];
    let mut i = 0;
    while i < n {
        // The entries are contiguous, so the first one missing ends them.
        let Some(entry) = ulib::env_at(i, &mut buf) else {
            break;
        };
        ulib::write_out(target, entry);
        ulib::write_out(target, b"\r\n");
        i += 1;
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

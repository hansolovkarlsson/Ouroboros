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
    // One entry (NAME=VALUE) at a time, into a buffer the size of the whole
    // store, so no entry is printed cut: until 2026-10-06 this was 256 bytes,
    // and a longer entry (any spawner can stage one) printed as a shorter one.
    let mut buf = [0u8; syscall_abi::ENV_MAX as usize];
    let mut i = 0;
    while i < n {
        if let Some(len) = ulib::env_at(i, &mut buf) {
            // Never longer than the store; a cut entry is left out, not printed.
            if let Some(entry) = buf.get(..len) {
                ulib::write_out(target, entry);
                ulib::write_out(target, b"\r\n");
            }
        }
        i += 1;
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

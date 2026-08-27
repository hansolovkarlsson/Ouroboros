//! `send <task> <words...>` - send an IPC message (`MSG_SEND`) to a task,
//! joining the words with single spaces. Externalized from a shell builtin: it
//! only calls a syscall, needs no shell state, so it's an ordinary `/bin`
//! program. The IPC test companion to `recv`.

#![no_std]
#![no_main]

const USAGE: &[u8] = b"usage: send <task number> <words...>  (send an IPC message to a task)\r\n";

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let mut a1 = [0u8; 32];
    let Some(l1) = ulib::arg(1, &mut a1) else {
        ulib::con_write(USAGE);
        ulib::exit(1);
    };
    if &a1[..l1] == b"-?" {
        ulib::con_write(USAGE);
        ulib::exit(0);
    }
    let task_str = core::str::from_utf8(&a1[..l1]).unwrap_or("");
    let Some(dest) = ulib::parse_u64(task_str) else {
        ulib::con_write(USAGE);
        ulib::exit(1);
    };

    // Join argv[2..] with single spaces.
    let mut msg = [0u8; 64];
    let mut len = 0usize;
    let argc = ulib::argc();
    let mut i = 2u64;
    let mut first = true;
    let mut word = [0u8; 64];
    while i < argc {
        if let Some(wl) = ulib::arg(i, &mut word) {
            if !first && len < msg.len() {
                msg[len] = b' ';
                len += 1;
            }
            for &b in &word[..wl] {
                if len < msg.len() {
                    msg[len] = b;
                    len += 1;
                }
            }
            first = false;
        }
        i += 1;
    }
    if len == 0 {
        ulib::con_write(USAGE);
        ulib::exit(1);
    }

    let r = ulib::syscall4(syscall_abi::MSG_SEND, dest, msg.as_ptr() as u64, len as u64, 0);
    if ulib::is_fs_error(r) {
        ulib::fs_error("send", r);
        ulib::exit(1);
    }
    ulib::exit(0);
}

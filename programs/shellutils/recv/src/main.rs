//! `recv` - block until an IPC message arrives (`MSG_RECV`) and print it as
//! `task N: <message>`. Ctrl+C aborts. Externalized from a shell builtin (it
//! only calls a syscall); the IPC test companion to `send`. As a `/bin` program
//! it receives into its own (spawned) slot - run it, note its slot in `ps`, and
//! `send <slot> <words>` from elsewhere.

#![no_std]
#![no_main]

const USAGE: &[u8] = b"usage: recv  (block until an IPC message arrives, then print it)\r\n";

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let mut a1 = [0u8; 8];
    if let Some(l1) = ulib::arg(1, &mut a1) {
        if &a1[..l1] == b"-?" {
            ulib::con_write(USAGE);
            ulib::exit(0);
        }
    }

    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = ulib::syscall4(
        syscall_abi::MSG_RECV,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
        0,
    );
    if packed == syscall_abi::RECV_INTERRUPTED {
        ulib::con_write(b"recv: interrupted\r\n");
        ulib::exit(1);
    }
    if ulib::is_fs_error(packed) {
        ulib::fs_error("recv", packed);
        ulib::exit(1);
    }

    let sender = packed >> 32;
    let len = ((packed & 0xffff_ffff) as usize).min(buf.len());
    ulib::con_write(b"task ");
    let mut nb = [0u8; 20];
    let mut n = 0usize;
    ulib::emit_dec(&mut nb, &mut n, sender);
    ulib::con_write(&nb[..n]);
    ulib::con_write(b": ");
    ulib::con_write(&buf[..len]);
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

//! `mountprobe` - the `MOUNT` syscall from an ordinary task, for
//! `make test-mount-gate` (`scripts/test-mount-gate.py`). A probe, not a tool:
//! it asks the kernel to rescan the USB ports and install a found stick in
//! place of the disk (replace set), the swap that only the filesystem server
//! may ask for, and prints the answer by name:
//!
//! ```text
//! mountprobe: denied        BLOCK_ERR_DENIED, the gate (the expected answer)
//! mountprobe: installed     0: the disk was swapped under whatever is mounted
//! mountprobe: no device     MOUNT_NO_DEVICE
//! mountprobe: already       MOUNT_ALREADY
//! mountprobe: other <n>     anything else
//! ```
//!
//! Exits 0 when denied, 1 otherwise.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: mountprobe  (MOUNT with replace set, from this task; prints the kernel's answer)\r\n");
    let r = ulib::syscall(syscall_abi::MOUNT, 1);
    // One table: the name, or None for "other", which prints the number.
    let name: Option<&[u8]> = match r {
        syscall_abi::BLOCK_ERR_DENIED => Some(b"denied"),
        0 => Some(b"installed"),
        syscall_abi::MOUNT_NO_DEVICE => Some(b"no device"),
        syscall_abi::MOUNT_ALREADY => Some(b"already"),
        _ => None,
    };
    ulib::con_write(b"mountprobe: ");
    match name {
        Some(name) => ulib::con_write(name),
        None => {
            let mut line = [0u8; 32];
            let mut n = 0;
            ulib::emit(&mut line, &mut n, b"other ");
            ulib::emit_dec(&mut line, &mut n, r);
            ulib::con_write(&line[..n]);
        }
    }
    ulib::con_write(b"\r\n");
    ulib::exit(if r == syscall_abi::BLOCK_ERR_DENIED { 0 } else { 1 });
}

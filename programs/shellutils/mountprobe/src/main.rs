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
    let name: &[u8] = match r {
        syscall_abi::BLOCK_ERR_DENIED => b"denied",
        0 => b"installed",
        syscall_abi::MOUNT_NO_DEVICE => b"no device",
        syscall_abi::MOUNT_ALREADY => b"already",
        _ => b"other",
    };
    ulib::con_write(b"mountprobe: ");
    ulib::con_write(name);
    if !matches!(r, syscall_abi::BLOCK_ERR_DENIED | 0 | syscall_abi::MOUNT_NO_DEVICE | syscall_abi::MOUNT_ALREADY) {
        let mut digits = [0u8; 20];
        let mut n = r;
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        ulib::con_write(b" ");
        ulib::con_write(&digits[i..]);
    }
    ulib::con_write(b"\r\n");
    ulib::exit(if r == syscall_abi::BLOCK_ERR_DENIED { 0 } else { 1 });
}

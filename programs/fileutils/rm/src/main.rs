//! `rm` - externalized file removal (standalone-binaries Stage 4). Removes the
//! file named by `argv[1]` (not a directory - use `rmdir` for those), resolved
//! against the cwd the shell delivered at spawn. A single `FSOP_RM` via
//! `ulib::fs_op_path`.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: rm <file>  (remove a file)\r\n");
    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(1, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };
    if arg.is_empty() {
        ulib::con_write(b"rm: missing file argument\r\n");
        ulib::exit(1);
    }

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"rm: path too long\r\n");
            ulib::exit(1);
        }
    };

    let code = ulib::fs_op_path(syscall_abi::FSOP_RM, path);
    if ulib::is_fs_error(code) {
        ulib::fs_error("rm", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

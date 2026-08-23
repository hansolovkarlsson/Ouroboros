//! `rmdir` - externalized empty-directory removal (standalone-binaries Stage 4).
//! Removes the empty directory named by `argv[1]`, resolved against the cwd the
//! shell delivered at spawn. A single `FSOP_RMDIR` via `ulib::fs_op_path`.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(1, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };
    if arg.is_empty() {
        ulib::con_write(b"rmdir: missing directory argument\r\n");
        ulib::exit(1);
    }

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"rmdir: path too long\r\n");
            ulib::exit(1);
        }
    };

    let code = ulib::fs_op_path(syscall_abi::FSOP_RMDIR, path);
    if ulib::is_fs_error(code) {
        ulib::fs_error("rmdir", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

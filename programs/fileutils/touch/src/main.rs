//! `touch` - externalized empty-file creation (standalone-binaries Stage 4).
//! Creates the file named by `argv[1]` (a no-op if it already exists),
//! resolved against the cwd the shell delivered at spawn. A single `FSOP_TOUCH`
//! via `ulib::fs_op_path`.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: touch <file>  (create an empty file if absent)\r\n");
    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(1, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };
    if arg.is_empty() {
        ulib::con_write(b"touch: missing file argument\r\n");
        ulib::exit(1);
    }

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"touch: path too long\r\n");
            ulib::exit(1);
        }
    };

    let code = ulib::fs_op_path(syscall_abi::FSOP_TOUCH, path);
    if ulib::is_fs_error(code) {
        ulib::fs_error("touch", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

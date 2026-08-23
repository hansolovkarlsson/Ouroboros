//! `ls` - externalized directory listing (standalone-binaries Stage 4, the
//! first filesystem command out of the shell). Lists `argv[1]` (or, with no
//! argument, the current directory) as `name`/`name/` lines. Resolves its
//! path against the cwd the shell delivered at spawn (`ulib::cwd` / `GET_CWD`),
//! the mechanism that lets a spawned command handle relative paths and a bare
//! `ls`. Talks to the filesystem server via `ulib`'s `fs_list_dir`.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    // argv[1] is the path; absent means "the current directory" (empty arg,
    // which `resolve` maps to cwd itself).
    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(1, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"ls: path too long\r\n");
            ulib::exit(1);
        }
    };

    let mut listing = [0u8; 512];
    let n = ulib::fs_list_dir(path, &mut listing);
    if ulib::is_fs_error(n) {
        ulib::fs_error("ls", n);
    } else {
        ulib::write_out(target, &listing[..n as usize]);
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

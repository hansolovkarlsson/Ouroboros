//! `cat` - externalized file printer (standalone-binaries Stage 4). Streams
//! `argv[1]` in `SAFECOPY_MAX` chunks via the grant/safecopy bulk-read path
//! (`ulib::fs_read_bulk`), so it prints a file of any size without holding the
//! whole thing. Resolves its path against the cwd delivered at spawn.

#![no_std]
#![no_main]

const LF: u8 = b'\n';

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();

    let mut argbuf = [0u8; ulib::PATH_MAX];
    let arg = match ulib::arg(1, &mut argbuf) {
        Some(len) => core::str::from_utf8(&argbuf[..len]).unwrap_or(""),
        None => "",
    };
    if arg.is_empty() {
        ulib::con_write(b"cat: missing file argument\r\n");
        ulib::exit(1);
    }

    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");

    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let path = match ulib::resolve(cwd, arg, &mut pathbuf) {
        Some(plen) => core::str::from_utf8(&pathbuf[..plen]).unwrap_or(""),
        None => {
            ulib::con_write(b"cat: path too long\r\n");
            ulib::exit(1);
        }
    };

    // Stream in SAFECOPY_MAX chunks until a genuine 0 (EOF).
    let mut chunk = [0u8; syscall_abi::SAFECOPY_MAX as usize];
    let mut offset: u64 = 0;
    let mut wrote_any = false;
    let mut last_byte = 0u8;
    loop {
        let n = ulib::fs_read_bulk(path, offset, &mut chunk);
        if ulib::is_fs_error(n) {
            ulib::fs_error("cat", n);
            ulib::end_of_stream(target);
            ulib::exit(1);
        }
        if n == 0 {
            break;
        }
        let n = (n as usize).min(chunk.len());
        ulib::write_out(target, &chunk[..n]);
        wrote_any = true;
        last_byte = chunk[n - 1];
        offset += n as u64;
    }
    // Tidy trailing newline, console-only (so `cat a > b` copies bytes
    // exactly): add one if the content didn't end in a newline (or was empty).
    if target == syscall_abi::CON_TASK && (!wrote_any || last_byte != LF) {
        ulib::write_out(target, b"\r\n");
    }
    ulib::end_of_stream(target);
    ulib::exit(0);
}

//! `id` - print this task's user identity as `uid=N(name) gid=M(name)`.
//!
//! Reads the identity the kernel carries per task (`GET_ID` via
//! `ulib::getuid`/`getgid`), then resolves the uid -> name via `/etc/passwd` and
//! the gid -> name via `/etc/group` using the shared [`accounts`] lookups (the
//! same parser `login` uses). If a file is missing or the id isn't listed, the
//! name is simply omitted (numeric only) - so `id` still works on a disk with no
//! account files. Because a spawned command inherits the shell's identity,
//! running `id` after `su` reports the new user.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: id  (print this session's uid and gid, with names)\r\n");
    let target = ulib::stdout_target();

    let uid = ulib::getuid();
    let gid = ulib::getgid();

    let mut pbuf = [0u8; 512];
    let plen = read_file("/etc/passwd", &mut pbuf);
    let mut gbuf = [0u8; 512];
    let glen = read_file("/etc/group", &mut gbuf);

    let uname = accounts::find_user_by_uid(&pbuf[..plen], uid).map(|a| a.name);
    let gname = accounts::find_group_by_gid(&gbuf[..glen], gid).map(|g| g.name);

    let mut line = [0u8; 96];
    let mut w = 0usize;
    append(&mut line, &mut w, b"uid=");
    ulib::emit_dec(&mut line, &mut w, uid as u64);
    append_name(&mut line, &mut w, uname);
    append(&mut line, &mut w, b" gid=");
    ulib::emit_dec(&mut line, &mut w, gid as u64);
    append_name(&mut line, &mut w, gname);
    append(&mut line, &mut w, b"\r\n");

    ulib::write_out(target, &line[..w]);
    ulib::end_of_stream(target);
    ulib::exit(0);
}

/// Read a whole small file into `buf`, returning its length, or `0` on any error
/// (missing file / no filesystem) - id degrades to numeric-only.
fn read_file(path: &str, buf: &mut [u8]) -> usize {
    let r = ulib::fs_read_file(path, buf);
    if r < syscall_abi::FS_ERR_MIN {
        (r as usize).min(buf.len())
    } else {
        0
    }
}

/// Append `(name)` if resolved, nothing otherwise.
fn append_name(buf: &mut [u8], n: &mut usize, name: Option<&[u8]>) {
    if let Some(name) = name {
        append(buf, n, b"(");
        append(buf, n, name);
        append(buf, n, b")");
    }
}

fn append(buf: &mut [u8], n: &mut usize, src: &[u8]) {
    for &b in src {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

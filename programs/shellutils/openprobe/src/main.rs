//! `openprobe` - one raw `NP_OPEN` to fsd's boot mount, with flags no client
//! builds, for `make test-cerrno` (`scripts/test-cerrno.py`). A probe, not a
//! tool: libc sends `OPEN_EXCL` only beside `OPEN_CREATE` and never a flag
//! outside `OPEN_FLAGS`, so fsd's refusals of both, and `exists_answer`'s
//! answer to a caller who may not search the name's directory, were graded by
//! no boot until this (2026-10-29).
//!
//! ```text
//! openprobe <flags> <path>
//! ```
//!
//! `<flags>` is the request's flag word in decimal, sent as it is. Prints
//! `openprobe: <path> <flags> <answer>`, the answer by name:
//!
//! ```text
//! opened                  a fid (clunked again at once)
//! FS_ERR_ALREADY_EXISTS   EEXIST
//! FS_ERR_PERM             EACCES
//! FS_ERR_NOT_FOUND        ENOENT
//! FS_ERROR                the malformed-request answer
//! status <n>              anything else
//! ```
//!
//! Exits 0 when an answer came, 1 on a refused argument.

#![no_std]
#![no_main]

const PATH_MAX: usize = 128;

fn usage() -> ! {
    ulib::con_write(b"usage: openprobe <flags> <path>\r\n");
    ulib::exit(1);
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: openprobe <flags> <path>  (one raw NP_OPEN; prints fsd's answer)\r\n");
    let mut flags_buf = [0u8; 24];
    let mut path = [0u8; PATH_MAX];
    let (Some(fl), Some(pl)) = (ulib::arg(1, &mut flags_buf), ulib::arg(2, &mut path)) else {
        usage();
    };
    let Some(flags) = core::str::from_utf8(&flags_buf[..fl]).ok().and_then(ulib::parse_u64) else {
        usage();
    };
    // NP_OPEN's a0 is the flags and a1 the path length; tree 0, the boot mount.
    let r = ulib::np_call(ninep_abi::NP_OPEN, 0, [flags, pl as u64, 0, 0], &path[..pl], &[], &mut []);
    let name: Option<&[u8]> = match r {
        syscall_abi::FS_ERR_ALREADY_EXISTS => Some(b"FS_ERR_ALREADY_EXISTS"),
        syscall_abi::FS_ERR_PERM => Some(b"FS_ERR_PERM"),
        syscall_abi::FS_ERR_NOT_FOUND => Some(b"FS_ERR_NOT_FOUND"),
        syscall_abi::FS_ERROR => Some(b"FS_ERROR"),
        r if r < syscall_abi::FS_ERR_MIN => {
            ulib::np_call(ninep_abi::NP_CLUNK, 0, [r, 0, 0, 0], &[], &[], &mut []);
            Some(b"opened")
        }
        _ => None,
    };
    let mut line = [0u8; 64 + PATH_MAX];
    let mut n = 0;
    ulib::emit(&mut line, &mut n, b"openprobe: ");
    ulib::emit(&mut line, &mut n, &path[..pl]);
    ulib::emit(&mut line, &mut n, b" ");
    ulib::emit_dec(&mut line, &mut n, flags);
    ulib::emit(&mut line, &mut n, b" ");
    match name {
        Some(name) => ulib::emit(&mut line, &mut n, name),
        None => {
            ulib::emit(&mut line, &mut n, b"status ");
            ulib::emit_dec(&mut line, &mut n, r);
        }
    }
    ulib::emit(&mut line, &mut n, b"\r\n");
    ulib::con_write(&line[..n]);
    ulib::exit(0);
}

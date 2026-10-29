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
//! `<flags>` is the request's flag word in decimal, sent as it is. `<path>`
//! must be absolute: it goes to tree 0 as typed, through no namespace, so a
//! bound path (`/mnt/f`) names the boot mount's own. Prints
//! `openprobe: <path> <flags> <answer>`, the answer by name:
//!
//! ```text
//! opened                  a fid, clunked again at once
//! opened, clunk <n>       a fid whose clunk fsd answered with <n>, not 0
//! opened, clunk no answer a fid whose clunk fsd never answered
//! FS_ERR_ALREADY_EXISTS   EEXIST
//! FS_ERR_PERM             EACCES
//! FS_ERR_NOT_FOUND        ENOENT
//! FS_ERR_NOT_A_FILE       EISDIR
//! FS_ERROR                fsd's own malformed-request answer
//! status <n>              any other status fsd answered
//! no answer <n>           MSG_CALL itself failed (no fsd, or <n> its error)
//! short reply <n>         a reply of <n> bytes, too short for a status
//! ```
//!
//! The call is made here, not through ulib's `fs_call`/`np_call`, which give
//! `FS_ERROR` for a call fsd never answered too: a refusal must be told from
//! an fsd that died on the request (the high review of #369). Exits 0 when fsd
//! answered the open, and the clunk with 0 if it opened; 1 otherwise or on a
//! refused argument.

#![no_std]
#![no_main]

/// fsd's `FID_PATH_MAX` (programs/servers/fsd/src/main.rs, private there): a
/// longer path is refused FS_ERROR when the fid is made, which would read as
/// the flag refusals this probe grades.
const PATH_MAX: usize = 96;
/// The digits of `u64::MAX`.
const FLAGS_MAX: usize = 20;

fn usage() -> ! {
    ulib::con_write(b"usage: openprobe <flags> <path>  (path absolute, at most 96 bytes)\r\n");
    ulib::exit(1);
}

/// What one round trip to fsd came back with.
enum Answer {
    Status(u64),
    NoAnswer(u64),
    Short(u64),
}

/// One `verb` to fsd's tree 0: the header ninep-abi defines (verb, tree,
/// four params), then `payload` at `NP_REQ_PAYLOAD`. A copy of the encoding in
/// ulib's private `np_call`, kept here so ulib has no public call past the
/// namespace (the high review of #369). A drift that malformed the request
/// cannot pass the rig: the plain read open must answer `opened` and the
/// searchable `O_EXCL` open EEXIST.
fn call(verb: u64, params: [u64; 4], payload: &[u8]) -> Answer {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&verb.to_le_bytes());
    let mut i = 0;
    while i < 4 {
        req[16 + i * 8..24 + i * 8].copy_from_slice(&params[i].to_le_bytes());
        i += 1;
    }
    req[HDR..HDR + payload.len()].copy_from_slice(payload);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = ulib::syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::FSD_TASK,
        req.as_ptr() as u64,
        (HDR + payload.len()) as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed >= syscall_abi::FS_ERR_MIN {
        return Answer::NoAnswer(packed);
    }
    let len = packed & 0xffff_ffff;
    if len < 8 {
        return Answer::Short(len);
    }
    let mut status = [0u8; 8];
    status.copy_from_slice(&reply[0..8]);
    Answer::Status(u64::from_le_bytes(status))
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: openprobe <flags> <path>  (one raw NP_OPEN; prints fsd's answer)\r\n");
    // Each a byte more than an argument may be, so a longer one is seen and
    // refused rather than cut short and sent (`ulib::arg` truncates).
    let mut flags_buf = [0u8; FLAGS_MAX + 1];
    let mut path = [0u8; PATH_MAX + 1];
    let (Some(fl), Some(pl)) = (ulib::arg(1, &mut flags_buf), ulib::arg(2, &mut path)) else {
        usage();
    };
    if fl > FLAGS_MAX || pl == 0 || pl > PATH_MAX || path[0] != b'/' {
        usage();
    }
    let Some(flags) = core::str::from_utf8(&flags_buf[..fl]).ok().and_then(ulib::parse_u64) else {
        usage();
    };
    // NP_OPEN's a0 is the flags and a1 the path length.
    let answer = call(ninep_abi::NP_OPEN, [flags, pl as u64, 0, 0], &path[..pl]);
    let mut line = [0u8; 96 + PATH_MAX];
    let mut n = 0;
    ulib::emit(&mut line, &mut n, b"openprobe: ");
    ulib::emit(&mut line, &mut n, &path[..pl]);
    ulib::emit(&mut line, &mut n, b" ");
    ulib::emit_dec(&mut line, &mut n, flags);
    ulib::emit(&mut line, &mut n, b" ");
    let mut clunked = true;
    let answered = match answer {
        Answer::Status(r) => {
            let name: Option<&[u8]> = match r {
                syscall_abi::FS_ERR_ALREADY_EXISTS => Some(b"FS_ERR_ALREADY_EXISTS"),
                syscall_abi::FS_ERR_PERM => Some(b"FS_ERR_PERM"),
                syscall_abi::FS_ERR_NOT_FOUND => Some(b"FS_ERR_NOT_FOUND"),
                syscall_abi::FS_ERR_NOT_A_FILE => Some(b"FS_ERR_NOT_A_FILE"),
                syscall_abi::FS_ERROR => Some(b"FS_ERROR"),
                _ => None,
            };
            match name {
                Some(name) => ulib::emit(&mut line, &mut n, name),
                None if r < syscall_abi::FS_ERR_MIN => {
                    ulib::emit(&mut line, &mut n, b"opened");
                    match call(ninep_abi::NP_CLUNK, [r, 0, 0, 0], &[]) {
                        Answer::Status(0) => {}
                        Answer::Status(c) => {
                            ulib::emit(&mut line, &mut n, b", clunk ");
                            ulib::emit_dec(&mut line, &mut n, c);
                            clunked = false;
                        }
                        Answer::NoAnswer(_) | Answer::Short(_) => {
                            ulib::emit(&mut line, &mut n, b", clunk no answer");
                            clunked = false;
                        }
                    }
                }
                None => {
                    ulib::emit(&mut line, &mut n, b"status ");
                    ulib::emit_dec(&mut line, &mut n, r);
                }
            }
            clunked
        }
        Answer::NoAnswer(e) => {
            ulib::emit(&mut line, &mut n, b"no answer ");
            ulib::emit_dec(&mut line, &mut n, e);
            false
        }
        Answer::Short(len) => {
            ulib::emit(&mut line, &mut n, b"short reply ");
            ulib::emit_dec(&mut line, &mut n, len);
            false
        }
    };
    ulib::emit(&mut line, &mut n, b"\r\n");
    ulib::con_write(&line[..n]);
    ulib::exit(if answered { 0 } else { 1 });
}

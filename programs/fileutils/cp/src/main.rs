//! `cp` - externalized file copy (standalone-binaries Stage 4). Copies `argv[1]`
//! to `argv[2]`, both resolved against the cwd the shell delivered at spawn.
//! Streams the source into the destination one `SAFECOPY_MAX` chunk at a time
//! via the grant/safecopy bulk path (`fs_read_bulk` -> `fs_write_at`), so a
//! file of any size copies without holding the whole thing. Ported from the
//! shell's `cmd_cp` - same self-copy guard and read-before-truncate ordering.
//!
//! An existing destination is REFUSED unless `-f` is given, matching `mv`.
//! Deliberately a refusal and NOT a prompt: asking needs the keyboard, and `cp`
//! has none as a pipeline stage or under `cpu` on another machine. A guard that
//! works only in the interactive case is the wrong guard.

#![no_std]
#![no_main]

use ulib::PATH_MAX;

/// Returns `(force, index_of_first_operand)`. Only `-f` is accepted, and only
/// before the operands; any other `-...` word is rejected outright rather than
/// taken as a filename.
fn leading_force_flag() -> (bool, u64) {
    let mut buf = [0u8; 16];
    match ulib::arg(1, &mut buf) {
        Some(n) if n >= 1 && buf[0] == b'-' => {
            if &buf[..n] == b"-f" {
                (true, 2)
            } else {
                ulib::con_write(b"cp: unknown option (only -f is accepted)\r\n");
                ulib::exit(1);
            }
        }
        _ => (false, 1),
    }
}

fn arg_or_die(index: u64, buf: &mut [u8], msg: &[u8]) -> usize {
    match ulib::arg(index, buf) {
        Some(len) if len > 0 => len,
        _ => {
            ulib::con_write(msg);
            ulib::exit(1);
        }
    }
}

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(
        b"usage: cp [-f] <src> <dst>  (copy a file; -f overwrites an existing destination)\r\n",
    );
    let (force, first) = leading_force_flag();
    let mut src_arg_buf = [0u8; PATH_MAX];
    let src_arg_len = arg_or_die(first, &mut src_arg_buf, b"cp: missing source file argument\r\n");
    let src_arg = core::str::from_utf8(&src_arg_buf[..src_arg_len]).unwrap_or("");

    let mut dst_arg_buf = [0u8; PATH_MAX];
    let dst_arg_len =
        arg_or_die(first + 1, &mut dst_arg_buf, b"cp: missing destination file argument\r\n");
    let dst_arg = core::str::from_utf8(&dst_arg_buf[..dst_arg_len]).unwrap_or("");

    let mut cwd_buf = [0u8; PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwd_buf);
    let cwd = core::str::from_utf8(&cwd_buf[..cwd_len]).unwrap_or("/");

    let mut src_path_buf = [0u8; PATH_MAX];
    let Some(src_path_len) = ulib::resolve(cwd, src_arg, &mut src_path_buf) else {
        ulib::con_write(b"cp: path too long\r\n");
        ulib::exit(1);
    };
    let mut dst_path_buf = [0u8; PATH_MAX];
    let Some(dst_path_len) = ulib::resolve(cwd, dst_arg, &mut dst_path_buf) else {
        ulib::con_write(b"cp: path too long\r\n");
        ulib::exit(1);
    };

    // Self-copy guard: streaming cp truncates dst *first*, so `cp a a` (however
    // spelled after resolution) would destroy the source before reading it.
    // Byte equality of two runtime buffers - relocation-safe.
    if src_path_buf[..src_path_len] == dst_path_buf[..dst_path_len] {
        ulib::con_write(b"cp: source and destination are the same\r\n");
        ulib::exit(1);
    }

    let src_path = core::str::from_utf8(&src_path_buf[..src_path_len]).unwrap_or("");
    let dst_path = core::str::from_utf8(&dst_path_buf[..dst_path_len]).unwrap_or("");

    // Before the truncating write below, not after: cp destroys the
    // destination's contents as its FIRST act, so a refusal that came later
    // would arrive after the damage.
    if !force && ulib::fs_exists(dst_path) {
        ulib::con_write(b"cp: ");
        ulib::con_write(dst_path.as_bytes());
        ulib::con_write(b" exists (use -f to overwrite)\r\n");
        ulib::exit(1);
    }

    // Confirm the source exists (and is a file, not a directory) *before*
    // touching dst - a one-byte read is the cheapest existence/kind check.
    let mut probe = [0u8; 1];
    let code = ulib::fs_read_file(src_path, &mut probe);
    if ulib::is_fs_error(code) {
        ulib::fs_error("cp", code);
        ulib::exit(1);
    }

    // Truncate/create dst empty, then stream src into it one chunk at a time.
    // Non-atomic: an interrupted copy leaves dst truncated (a partial copy is a
    // wrong copy).
    let code = ulib::fs_write_bulk(dst_path, &[]);
    if ulib::is_fs_error(code) {
        ulib::fs_error("cp", code);
        ulib::exit(1);
    }

    let mut chunk = [0u8; syscall_abi::SAFECOPY_MAX as usize];
    let mut offset: u64 = 0;
    loop {
        let n = ulib::fs_read_bulk(src_path, offset, &mut chunk);
        if ulib::is_fs_error(n) {
            ulib::fs_error("cp", n);
            ulib::exit(1);
        }
        if n == 0 {
            break;
        }
        let n = (n as usize).min(chunk.len());
        let code = ulib::fs_write_at(dst_path, offset, &chunk[..n]);
        if ulib::is_fs_error(code) {
            ulib::fs_error("cp", code);
            ulib::exit(1);
        }
        offset += n as u64;
    }
    ulib::exit(0);
}

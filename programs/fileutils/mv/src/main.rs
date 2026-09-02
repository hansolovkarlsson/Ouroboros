//! `mv` - externalized rename/move (standalone-binaries Stage 4). Renames or
//! moves `argv[1]` to `argv[2]`, both resolved against the cwd the shell
//! delivered at spawn. A single `FSOP_MV` (the server relinks the entry - no
//! content moves), plus the `mv file dir` convenience: if the destination is an
//! existing directory, the source moves *into* it keeping its basename. Ported
//! from the shell's `cmd_mv` - same self-move guard.
//!
//! An existing destination is REFUSED unless `-f` is given. `fsd` will happily
//! replace it - that is POSIX `rename`, and it is the right behaviour for a
//! protocol verb with no user to consult - so the caution lives here, in the
//! command a person types. Deliberately a refusal and NOT a prompt: asking
//! needs the keyboard, and `mv` has none when it runs as a pipeline stage, or
//! under `cpu` on another machine, or when the request arrives from a 9P peer.
//! A guard that works only in the interactive case is the wrong guard.

#![no_std]
#![no_main]

use ulib::PATH_MAX;

/// Returns `(force, index_of_first_operand)`. Only `-f` is accepted, and only
/// before the operands; any other `-...` word is rejected outright.
fn leading_force_flag() -> (bool, u64) {
    let mut buf = [0u8; 16];
    match ulib::arg(1, &mut buf) {
        Some(n) if n >= 1 && buf[0] == b'-' => {
            if &buf[..n] == b"-f" {
                (true, 2)
            } else {
                ulib::con_write(b"mv: unknown option (only -f is accepted)\r\n");
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
        b"usage: mv [-f] <src> <dst>  (rename/move a file; -f replaces an existing destination)\r\n",
    );
    // `-f` may lead; everything after it is an operand. An UNKNOWN `-x` is an
    // error rather than a path: for a destructive command, silently treating a
    // mistyped flag as the source is the wrong way to be permissive.
    let (force, first) = leading_force_flag();
    let mut src_arg_buf = [0u8; PATH_MAX];
    let src_arg_len = arg_or_die(first, &mut src_arg_buf, b"mv: missing source argument\r\n");
    let src_arg = core::str::from_utf8(&src_arg_buf[..src_arg_len]).unwrap_or("");

    let mut dst_arg_buf = [0u8; PATH_MAX];
    let dst_arg_len = arg_or_die(first + 1, &mut dst_arg_buf, b"mv: missing destination argument\r\n");
    let dst_arg = core::str::from_utf8(&dst_arg_buf[..dst_arg_len]).unwrap_or("");

    let mut cwd_buf = [0u8; PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwd_buf);
    let cwd = core::str::from_utf8(&cwd_buf[..cwd_len]).unwrap_or("/");

    let mut src_path_buf = [0u8; PATH_MAX];
    let Some(src_path_len) = ulib::resolve(cwd, src_arg, &mut src_path_buf) else {
        ulib::con_write(b"mv: path too long\r\n");
        ulib::exit(1);
    };
    let mut dst_path_buf = [0u8; PATH_MAX];
    let Some(dst_path_len) = ulib::resolve(cwd, dst_arg, &mut dst_path_buf) else {
        ulib::con_write(b"mv: path too long\r\n");
        ulib::exit(1);
    };

    // Self-move guard (`mv a a`, however spelled after resolution): without it,
    // the into-directory shortcut below would turn `mv dir dir` into "move dir
    // inside itself." Byte equality of two runtime buffers - relocation-safe.
    if src_path_buf[..src_path_len] == dst_path_buf[..dst_path_len] {
        ulib::con_write(b"mv: source and destination are the same\r\n");
        ulib::exit(1);
    }

    let src_path = core::str::from_utf8(&src_path_buf[..src_path_len]).unwrap_or("");

    // `mv file dir` moves *into* an existing directory, keeping the source's
    // basename. Probe with fs_list_dir: a successful listing means dst is a
    // directory, so the real destination becomes dst/<basename of src>; any
    // error just means "not a directory" and dst is used as-is.
    let mut final_dst_buf = [0u8; PATH_MAX];
    final_dst_buf[..dst_path_len].copy_from_slice(&dst_path_buf[..dst_path_len]);
    let mut final_dst_len = dst_path_len;
    let mut scratch = [0u8; 8];
    let list = ulib::fs_list_dir(
        core::str::from_utf8(&dst_path_buf[..dst_path_len]).unwrap_or(""),
        &mut scratch,
    );
    if list == syscall_abi::NO_FS {
        ulib::fs_error("mv", list);
        ulib::exit(1);
    }
    if !ulib::is_fs_error(list) {
        // dst is an existing directory: append "/" + basename(src).
        let src_bytes = &src_path_buf[..src_path_len];
        let base_start = src_bytes
            .iter()
            .rposition(|&b| b == b'/')
            .map(|i| i + 1)
            .unwrap_or(0);
        let base = &src_bytes[base_start..];
        // Root ("/") already ends in the separator - don't double it.
        if !(final_dst_len == 1 && final_dst_buf[0] == b'/') {
            if final_dst_len >= final_dst_buf.len() {
                ulib::con_write(b"mv: path too long\r\n");
                ulib::exit(1);
            }
            final_dst_buf[final_dst_len] = b'/';
            final_dst_len += 1;
        }
        if final_dst_len + base.len() > final_dst_buf.len() {
            ulib::con_write(b"mv: path too long\r\n");
            ulib::exit(1);
        }
        final_dst_buf[final_dst_len..final_dst_len + base.len()].copy_from_slice(base);
        final_dst_len += base.len();
    }
    let final_dst = core::str::from_utf8(&final_dst_buf[..final_dst_len]).unwrap_or("");

    // Checked AFTER the into-a-directory rewrite above, so `mv a dir/` asks
    // about `dir/a` - the name that would actually be replaced - and not about
    // `dir`, which of course exists.
    if !force && ulib::fs_exists(final_dst) {
        ulib::con_write(b"mv: ");
        ulib::con_write(final_dst.as_bytes());
        ulib::con_write(b" exists (use -f to replace)\r\n");
        ulib::exit(1);
    }

    let code = ulib::fs_mv(src_path, final_dst);
    if ulib::is_fs_error(code) {
        ulib::fs_error("mv", code);
        ulib::exit(1);
    }
    ulib::exit(0);
}

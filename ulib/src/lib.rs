//! `ulib` - the shared userland support library for Ouroboros command
//! programs (standalone-binaries arc, Stage 4). It factors out the
//! boilerplate every `/bin` program repeats: the `svc` syscall wrappers,
//! reading argv (`GET_ARGC`/`GET_ARG`), routing output to the right place
//! (the console server, or - when the shell spawned us as a pipe producer or
//! `> file` source - back to the shell), a hand-rolled decimal formatter, and
//! `exit`. A command program is then just `_start` + its own logic over these
//! helpers.
//!
//! Not a full libc, and deliberately tiny: no allocator, no `core::fmt` in
//! the crash-prone paths (see `docs/processes.md`'s relocation notes), fixed
//! buffers only. Each program still provides its own `_start` (the entry, at
//! `.text.start`); `ulib` provides the one `#[panic_handler]` the binary
//! needs, so command crates don't repeat it.
//!
//! Built for `aarch64-unknown-none` as an ordinary dependency; it links into
//! each command binary under that binary's PIE flags (the shared
//! `shell/linker.ld`), so it needs no linker script of its own.

#![no_std]

use core::arch::asm;
use core::panic::PanicInfo;

/// A syscall with one argument (the rest zeroed).
#[inline(always)]
pub fn syscall(number: u64, arg0: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") 0u64,
            in("x2") 0u64,
            in("x3") 0u64,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

/// A syscall with up to four arguments.
#[inline(always)]
pub fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

/// The number of arguments this program was spawned with (`argv[0]` is the
/// program name).
pub fn argc() -> u64 {
    syscall(syscall_abi::GET_ARGC, 0)
}

/// Copy argument `index` into `buf`, returning the bytes written (capped at
/// `buf.len()`), or `None` if there's no such argument.
pub fn arg(index: u64, buf: &mut [u8]) -> Option<usize> {
    let n = syscall4(
        syscall_abi::GET_ARG,
        index,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    );
    if n == syscall_abi::NO_ARG {
        None
    } else {
        Some((n as usize).min(buf.len()))
    }
}

/// Where this program's output should go: the console server by default, or -
/// when the shell spawned us as a pipe producer or `exec … > file` source -
/// the shell's own task, which relays or captures it.
pub fn stdout_target() -> u64 {
    syscall(syscall_abi::STDOUT_TARGET, 0)
}

/// The preemption tick count since boot (`uptime`'s source).
pub fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

/// Write `bytes` to this program's stdout target (console or the relaying
/// shell). The one output call a command should use.
pub fn write_out(target: u64, bytes: &[u8]) {
    if target == syscall_abi::CON_TASK {
        con_write(bytes);
    } else {
        pipe_out(target, bytes);
    }
}

/// Route output through the console server (a batched `DSPOP_WRITE` message),
/// falling back to the kernel console (`PUTC`) if there's no server this boot.
pub fn con_write(bytes: &[u8]) {
    let payload_off = syscall_abi::FS_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; syscall_abi::FS_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&syscall_abi::DSPOP_WRITE.to_le_bytes());
        req[8..16].copy_from_slice(&(n as u64).to_le_bytes());
        req[payload_off..payload_off + n].copy_from_slice(&bytes[off..off + n]);
        let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        let r = syscall4(
            syscall_abi::MSG_CALL,
            syscall_abi::CON_TASK,
            req.as_ptr() as u64,
            (payload_off + n) as u64,
            reply.as_mut_ptr() as u64,
        );
        if r >= syscall_abi::FS_ERR_MIN {
            for &b in &bytes[off..off + n] {
                syscall(syscall_abi::PUTC, b as u64);
            }
        }
        off += n;
    }
}

/// Send `bytes` as raw `MSG_MAX_LEN`-chunked messages to `target` (the shell,
/// when we're a pipe producer / capture source), with the same bounded retry
/// on a full mailbox or a not-yet-delegated send as `hello`/`args`.
pub fn pipe_out(target: u64, bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::MSG_MAX_LEN as usize);
        let chunk = &bytes[off..off + n];
        let deadline = get_ticks() + 150;
        loop {
            let r = syscall4(syscall_abi::MSG_SEND, target, chunk.as_ptr() as u64, n as u64, 0);
            if r == 0 {
                break;
            }
            let transient = r == syscall_abi::MSG_ERR_FULL || r == syscall_abi::MSG_ERR_DENIED;
            if !transient || get_ticks() > deadline {
                return;
            }
        }
        off += n;
    }
}

/// Receive one pipeline-input message into `buf` (a filter's stdin). Returns
/// the byte count; `0` means end-of-stream - the empty message that marks the
/// end of a pipe, or an unexpected error - either way the filter should stop.
/// The read half of the filter shape whose write half is [`write_out`].
pub fn pipe_recv(buf: &mut [u8]) -> usize {
    let packed = syscall4(
        syscall_abi::MSG_RECV,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
        0,
    );
    if packed >= syscall_abi::FS_ERR_MIN {
        return 0;
    }
    ((packed & 0xffff_ffff) as usize).min(buf.len())
}

/// If our output is piped/captured (target isn't the console), signal
/// end-of-stream with an empty message so the reading task knows we're done.
/// A no-op when writing straight to the console.
pub fn end_of_stream(target: u64) {
    if target == syscall_abi::CON_TASK {
        return;
    }
    let dummy = [0u8; 1];
    let deadline = get_ticks() + 150;
    loop {
        let r = syscall4(syscall_abi::MSG_SEND, target, dummy.as_ptr() as u64, 0, 0);
        let transient = r == syscall_abi::MSG_ERR_FULL || r == syscall_abi::MSG_ERR_DENIED;
        if r == 0 || !transient || get_ticks() > deadline {
            break;
        }
    }
}

/// Append `s` to `buf` at `*n`, advancing `*n` (bounded, never overruns).
pub fn emit(buf: &mut [u8], n: &mut usize, s: &[u8]) {
    for &b in s {
        if *n < buf.len() {
            buf[*n] = b;
            *n += 1;
        }
    }
}

/// Append the decimal form of `v` to `buf` - hand-rolled, no `core::fmt`
/// (the relocation-safe idiom this project's userland uses).
pub fn emit_dec(buf: &mut [u8], n: &mut usize, v: u64) {
    let mut d = [0u8; 20];
    let mut i = 20;
    let mut x = v;
    loop {
        i -= 1;
        d[i] = b'0' + (x % 10) as u8;
        x /= 10;
        if x == 0 {
            break;
        }
    }
    emit(buf, n, &d[i..]);
}

/// End this task with `code` (never returns). A program that's somehow
/// unkillable (task 0/1) just parks quietly instead.
pub fn exit(code: u64) -> ! {
    syscall(syscall_abi::EXIT, code);
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// Working directory + path resolution
// ---------------------------------------------------------------------------

/// Longest path a command handles - the shell's own `PATH_SIZE`.
pub const PATH_MAX: usize = syscall_abi::CWD_MAX as usize;
/// Max path components `resolve` collapses to (the shell's `MAX_COMPONENTS`).
pub const MAX_COMPONENTS: usize = 16;

/// Copy this task's working directory (delivered at spawn) into `buf`,
/// returning its length - `0` if it was spawned without one.
pub fn cwd(buf: &mut [u8]) -> usize {
    let n = syscall4(syscall_abi::GET_CWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
    (n as usize).min(buf.len())
}

fn is_root(b: &[u8]) -> bool {
    b.len() == 1 && b[0] == b'/'
}
fn is_dot(s: &str) -> bool {
    s.len() == 1 && s.as_bytes()[0] == b'.'
}
fn is_dotdot(s: &str) -> bool {
    s.len() == 2 && s.as_bytes()[0] == b'.' && s.as_bytes()[1] == b'.'
}

/// Resolve `arg` against `cwd` into `out`, returning the length - absolute if
/// `arg` starts with `/`, else joined onto `cwd`, then `.`/`..` collapsed.
/// An empty `arg` resolves to `cwd` itself. `None` if it doesn't fit / is too
/// deep. Ported from the shell's `resolve_path`; scalar comparisons only, no
/// `core::fmt` - relocation-safe.
pub fn resolve(cwd: &str, arg: &str, out: &mut [u8]) -> Option<usize> {
    let mut raw = [0u8; PATH_MAX];
    let raw_len = concat_path(cwd, arg, &mut raw)?;
    let raw_str = core::str::from_utf8(&raw[..raw_len]).ok()?;
    normalize_path(raw_str, out)
}

fn concat_path(cwd: &str, comp: &str, out: &mut [u8]) -> Option<usize> {
    if comp.is_empty() {
        let b = cwd.as_bytes();
        if b.len() > out.len() {
            return None;
        }
        out[..b.len()].copy_from_slice(b);
        return Some(b.len());
    }
    if comp.as_bytes()[0] == b'/' {
        let b = comp.as_bytes();
        if b.len() > out.len() {
            return None;
        }
        out[..b.len()].copy_from_slice(b);
        return Some(b.len());
    }
    let mut len = 0;
    let cb = cwd.as_bytes();
    if cb.len() > out.len() {
        return None;
    }
    out[..cb.len()].copy_from_slice(cb);
    len += cb.len();
    if !is_root(cb) {
        if len >= out.len() {
            return None;
        }
        out[len] = b'/';
        len += 1;
    }
    let pb = comp.as_bytes();
    if len + pb.len() > out.len() {
        return None;
    }
    out[len..len + pb.len()].copy_from_slice(pb);
    Some(len + pb.len())
}

fn normalize_path(path: &str, out: &mut [u8]) -> Option<usize> {
    let mut stack: [&str; MAX_COMPONENTS] = [""; MAX_COMPONENTS];
    let mut depth = 0usize;
    for component in path.split('/').filter(|c| !c.is_empty()) {
        if is_dot(component) {
            continue;
        }
        if is_dotdot(component) {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth >= MAX_COMPONENTS {
            return None;
        }
        stack[depth] = component;
        depth += 1;
    }
    if out.is_empty() {
        return None;
    }
    let mut len = 1;
    out[0] = b'/';
    for (i, comp) in stack[..depth].iter().enumerate() {
        let b = comp.as_bytes();
        if i > 0 {
            if len >= out.len() {
                return None;
            }
            out[len] = b'/';
            len += 1;
        }
        if len + b.len() > out.len() {
            return None;
        }
        out[len..len + b.len()].copy_from_slice(b);
        len += b.len();
    }
    Some(len)
}

// ---------------------------------------------------------------------------
// Filesystem IPC (to the fsd server) - ported from the shell's `fs_*`
// ---------------------------------------------------------------------------

/// One filesystem-server round trip (FSOP v2): op + four u64 params + up to
/// two inline payloads, reply is a status u64 + inline result. Returns the
/// status, [`NO_FS`] if no filesystem server, or [`FS_ERROR`] on a malformed
/// round trip. See the shell's `fs_call` - this is that, verbatim.
pub fn fs_call(op: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = syscall_abi::FS_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&op.to_le_bytes());
    let mut i = 0;
    while i < 4 {
        let at = 8 + i * 8;
        req[at..at + 8].copy_from_slice(&params[i].to_le_bytes());
        i += 1;
    }
    let p1_end = HDR + payload1.len();
    let p2_end = p1_end + payload2.len();
    if p2_end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[HDR..p1_end].copy_from_slice(payload1);
    req[p1_end..p2_end].copy_from_slice(payload2);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::FSD_TASK,
        req.as_ptr() as u64,
        p2_end as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed == syscall_abi::TASK_ERR_NO_SUCH_TASK {
        return syscall_abi::NO_FS;
    }
    if packed >= syscall_abi::FS_ERR_MIN {
        return syscall_abi::FS_ERROR;
    }
    let reply_len = ((packed & 0xffff_ffff) as usize).min(reply.len());
    if reply_len < 8 {
        return syscall_abi::FS_ERROR;
    }
    let status = u64::from_le_bytes([
        reply[0], reply[1], reply[2], reply[3], reply[4], reply[5], reply[6], reply[7],
    ]);
    let data_len = (reply_len - 8).min(result.len());
    result[..data_len].copy_from_slice(&reply[8..8 + data_len]);
    status
}

/// List `path`'s entries into `buf` as `name\n`/`name/\n`. Returns a byte
/// count, [`NO_FS`], or a specific `FS_ERR_*` code.
pub fn fs_list_dir(path: &str, buf: &mut [u8]) -> u64 {
    fs_call(
        syscall_abi::FSOP_LIST_DIR,
        [path.len() as u64, buf.len() as u64, 0, 0],
        path.as_bytes(),
        &[],
        buf,
    )
}

/// Invoke a single path-only filesystem op (`FSOP_MKDIR`/`RMDIR`/`TOUCH`/`RM`):
/// the path is the only input, the reply is a bare status (`0` on success, or
/// an `FS_ERR_*`/`NO_FS` code). The shape `mkdir`/`rmdir`/`touch`/`rm` share.
pub fn fs_op_path(op: u64, path: &str) -> u64 {
    fs_call(op, [path.len() as u64, 0, 0, 0], path.as_bytes(), &[], &mut [])
}

/// Read up to `buf.len()` bytes of `path` from `offset` into `buf` via the
/// grant/safecopy bulk path (the server SAFECOPYs straight into `buf`).
/// Returns the byte count (0 at EOF), [`NO_FS`], or an `FS_ERR_*` code.
pub fn fs_read_bulk(path: &str, offset: u64, buf: &mut [u8]) -> u64 {
    let want = buf.len() as u64;
    let granted = syscall4(
        syscall_abi::GRANT,
        syscall_abi::FSD_TASK,
        buf.as_mut_ptr() as u64,
        want,
        syscall_abi::GRANT_WRITE,
    );
    if granted != 0 {
        return syscall_abi::FS_ERROR;
    }
    fs_call(
        syscall_abi::FSOP_READ_BULK,
        [path.len() as u64, offset, want, 0],
        path.as_bytes(),
        &[],
        &mut [],
    )
}

/// Read up to `buf.len()` bytes of `path` inline (the reply carries the
/// bytes, capped at 512 by the message limit) - returns the real byte count,
/// [`NO_FS`], or an `FS_ERR_*` code. A one-byte `buf` is the cheapest
/// existence/kind probe (a directory returns `FS_ERR_NOT_A_FILE`).
pub fn fs_read_file(path: &str, buf: &mut [u8]) -> u64 {
    fs_call(
        syscall_abi::FSOP_READ_FILE,
        [path.len() as u64, buf.len() as u64, 0, 0],
        path.as_bytes(),
        &[],
        buf,
    )
}

/// Create or fully overwrite `path` with `data` via the grant/safecopy bulk
/// path (`GRANT_READ`). Returns `0`, [`NO_FS`], or an `FS_ERR_*` code.
/// `data.len()` must be `<= SAFECOPY_MAX`; empty `data` truncates to empty and
/// skips the grant.
pub fn fs_write_bulk(path: &str, data: &[u8]) -> u64 {
    if !data.is_empty() {
        let granted = syscall4(
            syscall_abi::GRANT,
            syscall_abi::FSD_TASK,
            data.as_ptr() as u64,
            data.len() as u64,
            syscall_abi::GRANT_READ,
        );
        if granted != 0 {
            return syscall_abi::FS_ERROR;
        }
    }
    fs_call(
        syscall_abi::FSOP_WRITE_FILE,
        [path.len() as u64, data.len() as u64, 0, 0],
        path.as_bytes(),
        &[],
        &mut [],
    )
}

/// Write `data` at byte `offset` in `path`, extending the file without
/// rewriting the bytes before `offset` (the FAT32 offset-write primitive), via
/// the grant/safecopy bulk path (`GRANT_READ`). Returns `0`, [`NO_FS`], or an
/// `FS_ERR_*` code. `data.len()` must be `<= SAFECOPY_MAX`. Loop with a rising
/// `offset` to write a file of any size one chunk at a time. Empty `data` is a
/// no-op.
pub fn fs_write_at(path: &str, offset: u64, data: &[u8]) -> u64 {
    if data.is_empty() {
        return 0;
    }
    let granted = syscall4(
        syscall_abi::GRANT,
        syscall_abi::FSD_TASK,
        data.as_ptr() as u64,
        data.len() as u64,
        syscall_abi::GRANT_READ,
    );
    if granted != 0 {
        return syscall_abi::FS_ERROR;
    }
    fs_call(
        syscall_abi::FSOP_WRITE_AT,
        [path.len() as u64, offset, data.len() as u64, 0],
        path.as_bytes(),
        &[],
        &mut [],
    )
}

/// Rename or move the file/directory at `src` to `dst` - a single op taking
/// two paths (the server relinks the entry, no content moves). Returns `0`,
/// [`NO_FS`], or an `FS_ERR_*` code.
pub fn fs_mv(src: &str, dst: &str) -> u64 {
    fs_call(
        syscall_abi::FSOP_MV,
        [src.len() as u64, dst.len() as u64, 0, 0],
        src.as_bytes(),
        dst.as_bytes(),
        &mut [],
    )
}

/// Parse a base-10 `u64`, returning `None` on empty input or any non-digit.
/// Relocation-safe (scalar byte comparisons, no `str::parse`).
pub fn parse_u64(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut value: u64 = 0;
    for &b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(value)
}

/// Whether a status code is a failure (`>= FS_ERR_MIN`, which covers every
/// `FS_ERR_*`, `NO_FS`, and `FS_ERROR`). A real byte count never reaches this.
pub fn is_fs_error(code: u64) -> bool {
    code >= syscall_abi::FS_ERR_MIN
}

/// Print `cmd: <message>` for an fs status code to the console (errors go to
/// the console regardless of a command's stdout target - the stderr split).
/// Ported from the shell's `print_fs_error`/`print_no_fs`.
pub fn fs_error(cmd: &str, code: u64) {
    con_write(cmd.as_bytes());
    con_write(b": ");
    let msg: &[u8] = match code {
        syscall_abi::NO_FS => b"no filesystem mounted this boot",
        syscall_abi::FS_ERR_NOT_FOUND => b"no such file or directory",
        syscall_abi::FS_ERR_NOT_A_FILE => b"is a directory",
        syscall_abi::FS_ERR_NOT_A_DIRECTORY => b"not a directory",
        syscall_abi::FS_ERR_INVALID_NAME => b"invalid name",
        syscall_abi::FS_ERR_ALREADY_EXISTS => b"already exists",
        syscall_abi::FS_ERR_NOT_EMPTY => b"directory not empty",
        syscall_abi::FS_ERR_IS_ROOT => b"can't remove the root directory",
        syscall_abi::FS_ERR_DISK_FULL => b"disk full",
        syscall_abi::FS_ERR_IO => b"device I/O error",
        _ => b"failed",
    };
    con_write(msg);
    con_write(b"\r\n");
}

// ---------------------------------------------------------------------------
// Network server client
// ---------------------------------------------------------------------------

/// `MSG_CALL` the network server ([`NET_TASK`](syscall_abi::NET_TASK)) with the
/// request bytes in `req`, receiving the reply into `reply`. Returns the packed
/// `MSG_CALL` result (the reply length in the low 32 bits) on success, or an
/// error/sentinel ([`TASK_ERR_NO_SUCH_TASK`](syscall_abi::TASK_ERR_NO_SUCH_TASK)
/// when there's no netd this boot, or a code `>= FS_ERR_MIN`).
///
/// Reaching netd needs the `TO_NET` send-capability, which a spawnable slot does
/// *not* hold statically - the shell delegates it (`DELEGATE`) to a command it
/// spawns. A tick can let this program run in the window before that delegation
/// lands, so a transient `MSG_ERR_DENIED` is retried briefly (the same bounded
/// wait `pipe_out` uses), rather than surfaced as a failure.
pub fn net_call(req: &[u8], reply: &mut [u8]) -> u64 {
    let deadline = get_ticks() + 150;
    loop {
        let packed = syscall4(
            syscall_abi::MSG_CALL,
            syscall_abi::NET_TASK,
            req.as_ptr() as u64,
            req.len() as u64,
            reply.as_mut_ptr() as u64,
        );
        if packed == syscall_abi::MSG_ERR_DENIED && get_ticks() <= deadline {
            continue;
        }
        return packed;
    }
}

/// The one panic handler the command binary needs - provided here so each
/// command crate doesn't repeat it. Parks; there's nowhere to report to.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

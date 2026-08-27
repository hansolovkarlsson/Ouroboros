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
//! `programs/linker.ld`), so it needs no linker script of its own.

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

/// Block until a keyboard byte is available, and return it (`READ_CHAR`). A
/// program only receives keystrokes while it *owns* the keyboard - the shell
/// hands a foreground command that ownership at spawn, so an interactive `/bin`
/// program (an editor, a REPL) can read input here; Ctrl+C terminates it (the
/// kernel kills the foreground owner). A background/piped program that calls
/// this just blocks forever (it never owns the keyboard).
pub fn read_char() -> u8 {
    syscall(syscall_abi::READ_CHAR, 0) as u8
}

/// Route output through the console server as a batched write over the uniform
/// verb set (`ninep-abi`): an `NP_WRITE_FILE` whose inline data is the text -
/// a write to the console "file" (cond ignores the tree/path). Falls back to
/// the kernel console (`PUTC`) if there's no server this boot.
pub fn con_write(bytes: &[u8]) {
    let payload_off = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; ninep_abi::NP_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&ninep_abi::NP_WRITE_FILE.to_le_bytes());
        // tree (a8) and path_len (a16) stay 0; data_len at a1 (offset 24).
        req[24..32].copy_from_slice(&(n as u64).to_le_bytes());
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

/// One filesystem-server round trip over the uniform verb set ([`ninep-abi`],
/// the Phase 0 cluster protocol) — `fs_call`'s sibling, with the `tree` mount
/// selector at offset 8 and the payload at [`ninep_abi::NP_REQ_PAYLOAD`] (48).
/// `tree` is `0` for now (a single implicit mount); the per-task namespace
/// resolves it to a real mount in a later step. The reply shape (status u64 +
/// inline result) is identical to `fs_call`'s, so callers are unchanged.
fn np_call(verb: u64, tree: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&verb.to_le_bytes());
    req[8..16].copy_from_slice(&tree.to_le_bytes());
    let mut i = 0;
    while i < 4 {
        let at = 16 + i * 8;
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

/// Largest resolved filesystem path (a `bind` target can be longer than the
/// prefix it replaces, so the resolved path can exceed the input).
const FSP_MAX: usize = 256;

/// Read the current task's namespace blob (set at spawn via `NS_STAGE`) into
/// `buf`, returning its length (0 = none - the identity default). The blob is a
/// sequence of `[tree:u8][prefix_len:u8][target_len:u8][prefix][target]`
/// bindings - see [`resolve_ns`].
pub fn get_ns(buf: &mut [u8]) -> usize {
    let n = syscall4(syscall_abi::GET_NS, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
    (n as usize).min(buf.len())
}

/// A resolved destination for a client path: which server task services it,
/// how to select the mount there, and the server-side path. The one structural
/// change Phase 1c makes to the fs-helper layer - a resolution can now name a
/// *remote* machine (via `netd`), not just a local `fsd` mount.
struct Resolved {
    /// The server this op goes to: [`syscall_abi::FSD_TASK`] for a local mount,
    /// [`syscall_abi::NET_TASK`] for a remote one (reached over TCP by `netd`).
    server: u64,
    /// Local mount selector (the binding's `tree`); for a remote resolution this
    /// is `0` - the request's `tree` field the remote export ignores (it serves
    /// its own boot mount).
    tree: u64,
    /// `[ip:4][port:2 LE]` of the remote export listener; valid iff
    /// `server == NET_TASK`.
    endpoint: [u8; ninep_abi::NS_ENDPOINT_LEN],
    /// Length of the resolved server-side path written to the caller's buffer.
    len: usize,
}

/// Resolve an absolute client `path` through the namespace `ns`: the longest
/// component-aligned prefix binding wins and its `target` replaces the matched
/// prefix. A binding's `tree` selects a local mount, *except* the sentinel
/// [`ninep_abi::NS_REMOTE_TREE`] (`0xFF`), whose `target` begins with a 6-byte
/// endpoint (`[ip:4][port:2]`) and a remote-side root - such a match resolves to
/// [`syscall_abi::NET_TASK`] (routed over TCP). No match - an empty namespace,
/// or a relative path (bindings are absolute) - is identity to the local boot
/// mount (tree 0), so an unbound task is unchanged. The server-side path bytes
/// are written to `out`. Bounded, scalar-only (relocation-safe, like
/// [`normalize_path`]).
fn resolve_ns(ns: &[u8], path: &str, out: &mut [u8]) -> Resolved {
    // The resolution logic is shared (`ninep_abi::resolve_ns`, the single source
    // of truth used by ulib, the shell, and netd's export). Map its task-neutral
    // `NsTarget` to this layer's concrete server/tree/endpoint.
    let r = ninep_abi::resolve_ns(ns, path.as_bytes(), out);
    let zero = [0u8; ninep_abi::NS_ENDPOINT_LEN];
    match r.target {
        ninep_abi::NsTarget::Fsd(tree) => Resolved { server: syscall_abi::FSD_TASK, tree: tree as u64, endpoint: zero, len: r.len },
        ninep_abi::NsTarget::Console => Resolved { server: syscall_abi::CON_TASK, tree: 0, endpoint: zero, len: r.len },
        ninep_abi::NsTarget::NetLocal => Resolved { server: syscall_abi::NET_TASK, tree: 0, endpoint: zero, len: r.len },
        ninep_abi::NsTarget::Remote(ep) => Resolved { server: syscall_abi::NET_TASK, tree: 0, endpoint: ep, len: r.len },
    }
}

/// Resolve `path` through this task's namespace, writing the server-side path to
/// `out`. Reads the namespace via `GET_NS` each call - cheap (the blob is small
/// and every fs op is already an IPC round trip). An empty namespace yields the
/// local boot mount unchanged.
fn mount_resolve(path: &str, out: &mut [u8]) -> Resolved {
    let mut ns = [0u8; syscall_abi::NS_MAX as usize];
    let nlen = get_ns(&mut ns);
    resolve_ns(&ns[..nlen], path, out)
}

/// Route one verb to its resolved destination: a local mount goes straight to
/// `fsd` ([`np_call`]); a remote mount is wrapped in an `NETOP_RMOUNT` request
/// to `netd` ([`np_remote`]), which carries it over TCP. The reply shape
/// (status u64 + inline result) is identical either way, so callers are
/// unchanged. Bulk (grant/safecopy) ops handle the remote case themselves (no
/// grant crosses a machine) - see [`fs_read_bulk`].
fn np_dispatch(r: &Resolved, verb: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    if r.server == syscall_abi::CON_TASK {
        // The console is write-only; reads/dir-ops/path-ops don't apply. (Writes
        // to /dev/cons are handled in the fs_write_* helpers, which con_write.)
        syscall_abi::FS_ERROR
    } else if is_local_net(r) {
        np_netlocal(verb, params, payload1, payload2, result)
    } else if r.server == syscall_abi::NET_TASK {
        np_remote(&r.endpoint, verb, params, payload1, payload2, result)
    } else {
        np_call(verb, r.tree, params, payload1, payload2, result)
    }
}

/// Whether a resolution is the *local* `/net` netd-fs (cluster Phase 3) rather
/// than a remote mount: `NET_TASK` with a zero endpoint (a remote mount always
/// carries a real endpoint).
fn is_local_net(r: &Resolved) -> bool {
    r.server == syscall_abi::NET_TASK && r.endpoint == [0u8; ninep_abi::NS_ENDPOINT_LEN]
}

/// One direct NP verb round trip to `netd` for the local `/net` filesystem: like
/// [`np_call`] but addressed to `NET_TASK` (which serves `/net` read verbs and
/// the `/net/tcp` dial-out connection files). The inline payload is
/// `payload1 ++ payload2` (path, then any write data - the ctl/data writes to
/// `/net/tcp`; empty `payload2` for a read). Reply shape `[status:u64][data]`.
fn np_netlocal(verb: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&verb.to_le_bytes());
    // tree (offset 8) stays 0.
    let mut i = 0;
    while i < 4 {
        let at = 16 + i * 8;
        req[at..at + 8].copy_from_slice(&params[i].to_le_bytes());
        i += 1;
    }
    let end = HDR + payload1.len() + payload2.len();
    if end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[HDR..HDR + payload1.len()].copy_from_slice(payload1);
    req[HDR + payload1.len()..end].copy_from_slice(payload2);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::NET_TASK,
        req.as_ptr() as u64,
        end as u64,
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

/// One remote verb round trip: build an `NETOP_RMOUNT` request
/// (`[op][ip:4][port:2][pad:2][NP message]`) to `netd`, which frames the NP
/// message onto a TCP connection to `endpoint`'s 9P export gateway and returns
/// the NP reply body. The embedded NP request's `tree` is `0` (the remote export
/// serves its own boot mount). The reply is `[status:u64][data]`, decoded like
/// [`np_call`]'s. Bounded by [`syscall_abi::MSG_MAX_LEN`] both ways.
fn np_remote(endpoint: &[u8; ninep_abi::NS_ENDPOINT_LEN], verb: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize; // 48
    let base = syscall_abi::NETOP_RMOUNT_MSG; // 16
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&syscall_abi::NETOP_RMOUNT.to_le_bytes());
    // Endpoint at NETOP_RMOUNT_ENDPOINT (8..14); bytes 14..16 stay zero pad.
    req[syscall_abi::NETOP_RMOUNT_ENDPOINT..syscall_abi::NETOP_RMOUNT_ENDPOINT + ninep_abi::NS_ENDPOINT_LEN]
        .copy_from_slice(&endpoint[..]);
    // The NP message starts at `base`. Header: verb, tree(0), a0..a3.
    req[base..base + 8].copy_from_slice(&verb.to_le_bytes());
    req[base + 8..base + 16].copy_from_slice(&0u64.to_le_bytes());
    let mut i = 0;
    while i < 4 {
        let at = base + 16 + i * 8;
        req[at..at + 8].copy_from_slice(&params[i].to_le_bytes());
        i += 1;
    }
    let p1_start = base + HDR;
    let p1_end = p1_start + payload1.len();
    let p2_end = p1_end + payload2.len();
    if p2_end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[p1_start..p1_end].copy_from_slice(payload1);
    req[p1_end..p2_end].copy_from_slice(payload2);

    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::NET_TASK,
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
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    // The remote export caps an inline listing at FS_DATA_MAX (512); the local
    // path is bounded by the message reply either way.
    let want = if r.server == syscall_abi::NET_TASK {
        buf.len().min(ninep_abi::NP_REMOTE_CHUNK) as u64
    } else {
        buf.len() as u64
    };
    np_dispatch(
        &r,
        ninep_abi::NP_READDIR,
        [r.len as u64, want, 0, 0],
        &fsp[..r.len],
        &[],
        buf,
    )
}

/// Invoke a single path-only filesystem op (`FSOP_MKDIR`/`RMDIR`/`TOUCH`/`RM`):
/// the path is the only input, the reply is a bare status (`0` on success, or
/// an `FS_ERR_*`/`NO_FS` code). The shape `mkdir`/`rmdir`/`touch`/`rm` share.
pub fn fs_op_path(op: u64, path: &str) -> u64 {
    // Callers still pass the `FSOP_*` op they always did (so `/bin` mkdir/rmdir/
    // touch/rm are unchanged); map it to the uniform verb here. Unknown ops pass
    // through untranslated (there is no such caller today).
    let verb = match op {
        syscall_abi::FSOP_MKDIR => ninep_abi::NP_MKDIR,
        syscall_abi::FSOP_RMDIR => ninep_abi::NP_RMDIR,
        syscall_abi::FSOP_TOUCH => ninep_abi::NP_TOUCH,
        syscall_abi::FSOP_RM => ninep_abi::NP_RM,
        other => other,
    };
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    np_dispatch(&r, verb, [r.len as u64, 0, 0, 0], &fsp[..r.len], &[], &mut [])
}

/// Read up to `buf.len()` bytes of `path` from `offset` into `buf` via the
/// grant/safecopy bulk path (the server SAFECOPYs straight into `buf`).
/// Returns the byte count (0 at EOF), [`NO_FS`], or an `FS_ERR_*` code.
pub fn fs_read_bulk(path: &str, offset: u64, buf: &mut [u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    if r.server == syscall_abi::CON_TASK {
        return syscall_abi::FS_ERROR; // the console is write-only
    }
    if is_local_net(&r) {
        // Local /net: data comes inline (no grant), like the remote case.
        let want = buf.len().min(ninep_abi::NP_REMOTE_CHUNK) as u64;
        return np_netlocal(ninep_abi::NP_READ, [r.len as u64, offset, want, 0], &fsp[..r.len], &[], buf);
    }
    // Remote: no grant crosses a machine boundary. The export delivers the bytes
    // *inline* in the reply, so ask for a chunk that fits one message and copy
    // the returned data straight into `buf` (the caller loops with a rising
    // offset for a large file, exactly as it does locally).
    if r.server == syscall_abi::NET_TASK {
        let want = buf.len().min(ninep_abi::NP_REMOTE_CHUNK) as u64;
        return np_remote(
            &r.endpoint,
            ninep_abi::NP_READ,
            [r.len as u64, offset, want, 0],
            &fsp[..r.len],
            &[],
            buf,
        );
    }
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
    np_call(
        ninep_abi::NP_READ,
        r.tree,
        [r.len as u64, offset, want, 0],
        &fsp[..r.len],
        &[],
        &mut [],
    )
}

/// Read up to `buf.len()` bytes of `path` inline (the reply carries the
/// bytes, capped at 512 by the message limit) - returns the real byte count,
/// [`NO_FS`], or an `FS_ERR_*` code. A one-byte `buf` is the cheapest
/// existence/kind probe (a directory returns `FS_ERR_NOT_A_FILE`).
pub fn fs_read_file(path: &str, buf: &mut [u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    let want = if r.server == syscall_abi::NET_TASK {
        buf.len().min(ninep_abi::NP_REMOTE_CHUNK) as u64
    } else {
        buf.len() as u64
    };
    np_dispatch(
        &r,
        ninep_abi::NP_READ_FILE,
        [r.len as u64, want, 0, 0],
        &fsp[..r.len],
        &[],
        buf,
    )
}

/// Stat `path`, filling `info` (which must be [`ninep_abi::STAT_INFO_LEN`]
/// bytes) with the fixed metadata record. Returns [`ninep_abi::STAT_INFO_LEN`]
/// on success, or [`NO_FS`]/an `FS_ERR_*` code (test with [`is_fs_error`]). The
/// record is decoded with the `stat_*` accessors below.
pub fn fs_stat(path: &str, info: &mut [u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    np_dispatch(
        &r,
        ninep_abi::NP_STAT,
        [r.len as u64, 0, 0, 0],
        &fsp[..r.len],
        &[],
        info,
    )
}

/// The size field of a stat record (see [`fs_stat`]).
pub fn stat_size(info: &[u8]) -> u64 {
    u64::from_le_bytes([
        info[0], info[1], info[2], info[3], info[4], info[5], info[6], info[7],
    ])
}

/// Whether a stat record's entry is a directory.
pub fn stat_is_dir(info: &[u8]) -> bool {
    let flags = u32::from_le_bytes([
        info[ninep_abi::STAT_FLAGS_OFF],
        info[ninep_abi::STAT_FLAGS_OFF + 1],
        info[ninep_abi::STAT_FLAGS_OFF + 2],
        info[ninep_abi::STAT_FLAGS_OFF + 3],
    ]);
    flags & ninep_abi::STAT_FLAG_DIR != 0
}

/// The modified time of a stat record as `(year, month, day, hour, min, sec)`,
/// or `None` if the filesystem didn't surface one (`time_valid` == 0).
pub fn stat_time(info: &[u8]) -> Option<(u16, u8, u8, u8, u8, u8)> {
    if info[ninep_abi::STAT_TIMEVALID_OFF] == 0 {
        return None;
    }
    let year = u16::from_le_bytes([info[ninep_abi::STAT_YEAR_OFF], info[ninep_abi::STAT_YEAR_OFF + 1]]);
    Some((
        year,
        info[ninep_abi::STAT_MONTH_OFF],
        info[ninep_abi::STAT_DAY_OFF],
        info[ninep_abi::STAT_HOUR_OFF],
        info[ninep_abi::STAT_MIN_OFF],
        info[ninep_abi::STAT_SEC_OFF],
    ))
}

/// Create or fully overwrite `path` with `data` via the grant/safecopy bulk
/// path (`GRANT_READ`). Returns `0`, [`NO_FS`], or an `FS_ERR_*` code.
/// `data.len()` must be `<= SAFECOPY_MAX`; empty `data` truncates to empty and
/// skips the grant.
pub fn fs_write_bulk(path: &str, data: &[u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    if r.server == syscall_abi::CON_TASK {
        con_write(data); // /dev/cons: the bytes go to the console
        return 0;
    }
    if is_local_net(&r) {
        return syscall_abi::FS_ERROR; // /net is read-only
    }
    // Remote: no grant crosses a machine, so the data rides inline in the
    // request (bounded by NP_REMOTE_CHUNK). The Phase 1 export is read-only, so
    // this returns FS_ERROR today; the shape is ready for a writable export.
    if r.server == syscall_abi::NET_TASK {
        // Remote full overwrite: no grant crosses a machine, and the inline cap
        // bounds one request - so truncate-and-write the first chunk (NP_WRITE),
        // then stream the rest at rising offsets (NP_WRITE_AT). Empty data =
        // NP_WRITE with 0 bytes = truncate-to-empty, the loop then no-ops.
        let first = data.len().min(ninep_abi::NP_REMOTE_CHUNK);
        let st = np_remote(
            &r.endpoint,
            ninep_abi::NP_WRITE,
            [r.len as u64, first as u64, 0, 0],
            &fsp[..r.len],
            &data[..first],
            &mut [],
        );
        if st != 0 {
            return st;
        }
        let mut off = first;
        while off < data.len() {
            let end = (off + ninep_abi::NP_REMOTE_CHUNK).min(data.len());
            let st = np_remote(
                &r.endpoint,
                ninep_abi::NP_WRITE_AT,
                [r.len as u64, off as u64, (end - off) as u64, 0],
                &fsp[..r.len],
                &data[off..end],
                &mut [],
            );
            if st != 0 {
                return st;
            }
            off = end;
        }
        return 0;
    }
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
    np_call(
        ninep_abi::NP_WRITE,
        r.tree,
        [r.len as u64, data.len() as u64, 0, 0],
        &fsp[..r.len],
        &[],
        &mut [],
    )
}

/// Write `data` to `path` with the data carried **inline** in the request
/// (`NP_WRITE_FILE`, bounded by [`syscall_abi::FS_DATA_MAX`]), routed through the
/// namespace like any fs op. Unlike [`fs_write_bulk`] (grant/safecopy, and it
/// refuses `/net`), this reaches the `/net/tcp` dial-out connection files - a
/// `ctl` "connect …"/"close" or a `data` send - locally (`NsTarget::NetLocal`)
/// and through a remote mount (`NsTarget::Remote`) alike. Returns the op's
/// status (for `/net/tcp/N/data` that's the number of bytes accepted).
pub fn fs_write_inline(path: &str, data: &[u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    let d = &data[..data.len().min(syscall_abi::FS_DATA_MAX as usize)];
    np_dispatch(
        &r,
        ninep_abi::NP_WRITE_FILE,
        [r.len as u64, d.len() as u64, 0, 0],
        &fsp[..r.len],
        d,
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
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    if r.server == syscall_abi::CON_TASK {
        con_write(data); // /dev/cons: append to the console (offset ignored)
        return 0;
    }
    if is_local_net(&r) {
        return syscall_abi::FS_ERROR; // /net is read-only
    }
    if r.server == syscall_abi::NET_TASK {
        // Remote: chunk to the inline cap (no grant crosses a machine), one
        // NP_WRITE_AT round trip per <=NP_REMOTE_CHUNK bytes at rising offsets,
        // so a caller's larger (e.g. SAFECOPY_MAX) buffer still writes whole.
        let mut off = 0usize;
        while off < data.len() {
            let end = (off + ninep_abi::NP_REMOTE_CHUNK).min(data.len());
            let st = np_remote(
                &r.endpoint,
                ninep_abi::NP_WRITE_AT,
                [r.len as u64, offset + off as u64, (end - off) as u64, 0],
                &fsp[..r.len],
                &data[off..end],
                &mut [],
            );
            if st != 0 {
                return st;
            }
            off = end;
        }
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
    np_call(
        ninep_abi::NP_WRITE_AT,
        r.tree,
        [r.len as u64, offset, data.len() as u64, 0],
        &fsp[..r.len],
        &[],
        &mut [],
    )
}

/// Rename or move the file/directory at `src` to `dst` - a single op taking
/// two paths (the server relinks the entry, no content moves). Returns `0`,
/// [`NO_FS`], or an `FS_ERR_*` code.
pub fn fs_mv(src: &str, dst: &str) -> u64 {
    // Both paths resolve through the namespace; in Phase 0 every binding is
    // tree 0, so a cross-tree move can't arise yet (a later phase concern).
    let mut fsrc = [0u8; FSP_MAX];
    let mut fdst = [0u8; FSP_MAX];
    let rs = mount_resolve(src, &mut fsrc);
    let rd = mount_resolve(dst, &mut fdst);
    np_dispatch(
        &rs,
        ninep_abi::NP_MV,
        [rs.len as u64, rd.len as u64, 0, 0],
        &fsrc[..rs.len],
        &fdst[..rd.len],
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
        syscall_abi::FS_ERR_READ_ONLY => b"read-only filesystem",
        syscall_abi::FS_ERR_IO => b"device I/O error",
        syscall_abi::FS_ERR_AUTH => b"cluster authentication failed (wrong or missing cluster key)",
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

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

/// True if any argument is `-?` - the uniform usage-help flag. A program checks
/// this (usually via [`usage_if_requested`]) and prints its one-line usage.
pub fn help_requested() -> bool {
    let n = argc();
    let mut i = 1u64;
    let mut buf = [0u8; 4];
    while i < n {
        if let Some(l) = arg(i, &mut buf) {
            if &buf[..l] == b"-?" {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// If any argument is `-?`, print `usage` to the console and `exit(0)`. Call it
/// at the top of `_start` - the uniform "add `-?` for usage" convention across
/// `/bin` programs. Does nothing (returns) when no `-?` is present.
pub fn usage_if_requested(usage: &[u8]) {
    if help_requested() {
        con_write(usage);
        exit(0);
    }
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

/// Number of environment variables this program inherited from its spawner
/// (`GET_ENVC`). `0` for a program not spawned by the shell (or spawned with no
/// environment).
pub fn env_count() -> u64 {
    syscall(syscall_abi::GET_ENVC, 0)
}

/// Copy the `index`-th inherited environment entry - a `NAME=VALUE` string -
/// into `buf`, returning its true length (which may exceed `buf.len()`), or
/// `None` if `index` is out of range. Mirrors [`arg`]; the entry point for
/// iterating the environment (e.g. `printenv`).
pub fn env_at(index: u64, buf: &mut [u8]) -> Option<usize> {
    let n = syscall4(
        syscall_abi::GET_ENV,
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

/// Look up an inherited environment variable by `name`, copying its value into
/// `buf` and returning the value's true length, or `None` if it isn't set.
/// Scans the environment (`env_count`/`env_at`), splitting each entry on its
/// first `=`. A `getenv`-shaped helper for programs that want one variable.
pub fn getenv(name: &[u8], buf: &mut [u8]) -> Option<usize> {
    // One entry at a time. 256 bytes holds any NAME=VALUE (a name plus a
    // 128-byte value) and stays under the syscall's MAX_USER_LEN out-capacity
    // (512) - the whole-blob ENV_MAX (2048) would be rejected by the range check.
    let mut entry = [0u8; 256];
    let n = env_count();
    for i in 0..n {
        let Some(len) = env_at(i, &mut entry) else {
            continue;
        };
        // Split NAME=VALUE on the first '='.
        let mut eq = None;
        for (j, &b) in entry[..len].iter().enumerate() {
            if b == b'=' {
                eq = Some(j);
                break;
            }
        }
        if let Some(eq) = eq {
            if &entry[..eq] == name {
                let val = &entry[eq + 1..len];
                let m = val.len().min(buf.len());
                buf[..m].copy_from_slice(&val[..m]);
                return Some(val.len());
            }
        }
    }
    None
}

/// Where this program's output should go: the console server by default, or -
/// when the shell spawned us as a pipe producer or `exec … > file` source -
/// the shell's own task, which relays or captures it.
pub fn stdout_target() -> u64 {
    syscall(syscall_abi::STDOUT_TARGET, 0)
}

/// This task's own scheduler-slot index (`SELF`).
pub fn self_task() -> u64 {
    syscall(syscall_abi::SELF, 0)
}

/// Set the calling task's user identity - uid, gid, **and** the supplementary
/// group list, which change together in one call. Returns `0`, or
/// [`syscall_abi::SET_ID_DENIED`].
///
/// `gids` must be passed explicitly, including the empty slice, because the
/// group list is not optional at the ABI: the kernel reads a (pointer, count)
/// pair, so omitting it does not mean "leave the groups alone" - it means
/// **clear them**. A two-argument wrapper hid that behind a default and let a
/// caller silently drop a user's memberships while believing it had only
/// changed the uid; `su <uid>:<gid>` did exactly that. Making the parameter
/// mandatory is the point of this signature.
///
/// Only root may change identity, and only root may set a NON-empty list
/// (membership is a privilege grant). Children inherit both at spawn.
pub fn set_id(uid: u32, gid: u32, gids: &[u32]) -> u64 {
    syscall4(
        syscall_abi::SET_ID,
        uid as u64,
        gid as u64,
        gids.as_ptr() as u64,
        gids.len() as u64,
    )
}

/// The packed `(gid << 32) | uid` of task `task`, or [`syscall_abi::GET_ID_ERR`]
/// for an out-of-range index. Use [`getuid`]/[`getgid`] for this task's own.
pub fn task_id(task: u64) -> u64 {
    syscall(syscall_abi::GET_ID, task)
}

/// The packed `(gid << 32) | uid` of whoever sent the message this task most
/// recently received - **bound by the kernel at send time**, not read back off
/// the sender's slot afterwards. [`syscall_abi::GET_ID_ERR`] if no message has
/// been received.
///
/// This is the call a server authorizing a request wants, and [`task_id`] is
/// not: by the time a server drains its mailbox the sender may have exited and
/// its slot been re-spawned, so `task_id(sender)` can report the *new*
/// occupant's identity - root, if that is what landed there. See
/// [`syscall_abi::SENDER_ID`].
pub fn sender_id() -> u64 {
    syscall(syscall_abi::SENDER_ID, 0)
}

/// The supplementary group list captured alongside [`sender_id`]. Returns how
/// many gids the sender actually had (which may exceed `out`), or `None` if no
/// message has been received.
pub fn sender_groups(out: &mut [u32]) -> Option<usize> {
    let n = syscall4(
        syscall_abi::SENDER_GROUPS,
        out.as_mut_ptr() as u64,
        out.len() as u64,
        0,
        0,
    );
    if n == syscall_abi::GET_ID_ERR {
        None
    } else {
        Some(n as usize)
    }
}

/// This task's uid (the user it runs as; `0` = root).
pub fn getuid() -> u32 {
    task_id(self_task()) as u32
}

/// This task's gid.
pub fn getgid() -> u32 {
    (task_id(self_task()) >> 32) as u32
}

/// The preemption tick count since boot (`uptime`'s source).
pub fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

/// Voluntarily give up the rest of this task's time slice (`YIELD`), letting
/// another runnable task run before this one is resumed. Used to hand the CPU
/// to a pipe consumer when its mailbox is momentarily full, rather than
/// busy-spinning until the next tick - see [`pipe_out`].
pub fn yield_now() {
    syscall(syscall_abi::YIELD, 0);
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

/// This program's heap area as a mutable byte slice (its region's fixed raw
/// heap, reported by `HEAP_INFO` - see the shell's own use). Space far larger
/// than the stack, for data a fixed stack buffer can't hold (a pager buffering
/// a file, say). Not a `GlobalAlloc` heap - just the program's own
/// EL0-accessible scratch area; empty (`&mut []`) if the region is too small to
/// have one.
pub fn heap() -> &'static mut [u8] {
    let base = syscall(syscall_abi::HEAP_INFO, syscall_abi::HEAP_INFO_BASE);
    let size = syscall(syscall_abi::HEAP_INFO, syscall_abi::HEAP_INFO_SIZE);
    if base == 0 || size == 0 {
        return &mut [];
    }
    // SAFETY: HEAP_INFO reports this task's own reserved heap area, EL0-writable
    // for the whole run and not aliased by anything else.
    unsafe { core::slice::from_raw_parts_mut(base as *mut u8, size as usize) }
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

/// A microsecond-resolution monotonic timestamp since boot (`MONOTONIC_US`).
/// The same clock netd's RTT estimator uses; the account tools fall back to it
/// as (weak, clock-derived) salt entropy when there is no hardware RNG — see
/// [`random_bytes8`] and `accounts::salt_from`.
pub fn monotonic_us() -> u64 {
    syscall(syscall_abi::MONOTONIC_US, 0)
}

/// Read `task`'s supplementary gids into `out`, returning the task's true count
/// (which may exceed what fitted). Pass [`self_task`] for one's own.
pub fn groups_of(task: u64, out: &mut [u32]) -> usize {
    let r = syscall4(syscall_abi::GET_GROUPS, task, out.as_mut_ptr() as u64, out.len() as u64, 0);
    if r == syscall_abi::GET_ID_ERR {
        0
    } else {
        r as usize
    }
}

/// Fill `buf` with hardware entropy (`RANDOM`), returning how many bytes were
/// written — `0` when this machine has no entropy device, which is the ordinary
/// case rather than an error (only a QEMU run with `-device virtio-rng-device`
/// has one). `buf` is bounded by `MAX_USER_LEN` (512) like every user pointer.
///
/// The device may legitimately return fewer bytes than asked for, so a caller
/// needing exactly N must check the count — see [`random_bytes8`].
pub fn random(buf: &mut [u8]) -> usize {
    if buf.is_empty() {
        return 0;
    }
    // Ask for at most what a user pointer may carry (MAX_USER_LEN, 512). The
    // syscall rejects anything larger with RANDOM_UNAVAILABLE, which would read
    // as "this machine has no entropy device" - the one distinction the ABI
    // says matters. Clamping keeps a big request a *short* read instead.
    let want = buf.len().min(512);
    let r = syscall4(syscall_abi::RANDOM, buf.as_mut_ptr() as u64, want as u64, 0, 0);
    if r == syscall_abi::RANDOM_UNAVAILABLE {
        0
    } else {
        (r as usize).min(want)
    }
}

/// Eight bytes of hardware entropy, or `None` when there is no entropy device
/// (or it returned short). `None` is a normal answer: the caller is expected to
/// degrade *loudly* — `accounts::salt_from` takes exactly this `Option` and
/// reports whether the salt it built is strong.
pub fn random_bytes8() -> Option<[u8; 8]> {
    let mut b = [0u8; 8];
    if random(&mut b) == 8 {
        Some(b)
    } else {
        None
    }
}

/// Read one line of keyboard input into `buf` (up to its length), returning the
/// count. Submits on CR/LF, supports destructive backspace, and ignores other
/// control bytes. When `echo` is set each byte is echoed to the console (a
/// username); a password is read silently (`echo == false`). The interactive
/// `/bin` account tools (`passwd`/`useradd`) use this — they only receive
/// keystrokes while foreground (the shell hands a spawned command the keyboard,
/// like the pager). Mirrors the shell's own `login::read_field`.
pub fn read_line(buf: &mut [u8], echo: bool) -> usize {
    const CR: u8 = 13;
    const LF: u8 = 10;
    const BS: u8 = 8;
    const DEL: u8 = 127;
    let mut len = 0usize;
    loop {
        let b = read_char();
        match b {
            CR | LF => return len,
            BS | DEL => {
                if len > 0 {
                    len -= 1;
                    if echo {
                        con_write(&[BS, b' ', BS]);
                    }
                }
            }
            _ if b < 0x20 => {}
            _ => {
                if len < buf.len() {
                    buf[len] = b;
                    len += 1;
                    if echo {
                        con_write(&[b]);
                    }
                }
            }
        }
    }
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
///
/// On a **full mailbox** (`MSG_ERR_FULL`) the producer can make no progress
/// until the consumer drains, so it **yields** rather than busy-spinning: the
/// consumer runs (draining, or exiting early like `head`, after which the next
/// send fails fast with `TASK_ERR_NO_SUCH_TASK` and this returns). Without the
/// yield the producer would spin re-sending until the next tick preempted it -
/// up to a full second on hardware. A `MSG_ERR_DENIED` (the brief
/// delegation-not-yet-applied window) just retries, no yield.
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
            if r == syscall_abi::MSG_ERR_FULL {
                yield_now(); // let the consumer drain (or exit) before retrying
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
        if r == syscall_abi::MSG_ERR_FULL {
            yield_now(); // as in pipe_out: let the consumer drain before retrying
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

/// Set `path`'s permission bits (the low 12 of the POSIX mode). Returns `0`,
/// [`NO_FS`], or an `FS_ERR_*` code - notably [`FS_ERR_NOT_SUPPORTED`] on a
/// filesystem that can't model a mode (FAT32/exFAT/`/proc`). Backs `chmod`.
///
/// [`NO_FS`]: syscall_abi::NO_FS
/// [`FS_ERR_NOT_SUPPORTED`]: syscall_abi::FS_ERR_NOT_SUPPORTED
pub fn fs_chmod(path: &str, mode: u16) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    np_dispatch(&r, ninep_abi::NP_CHMOD, [r.len as u64, mode as u64, 0, 0], &fsp[..r.len], &[], &mut [])
}

/// Set `path`'s owner uid and/or gid; `None` leaves that field unchanged (so
/// `chown user`, `chown :group`, and `chown user:group` all go through here).
/// Returns `0`, [`NO_FS`], or an `FS_ERR_*` code ([`FS_ERR_NOT_SUPPORTED`] off
/// ext2). The wire encodes "unchanged" as `u64::MAX`. Backs `chown`.
///
/// [`NO_FS`]: syscall_abi::NO_FS
/// [`FS_ERR_NOT_SUPPORTED`]: syscall_abi::FS_ERR_NOT_SUPPORTED
pub fn fs_chown(path: &str, uid: Option<u16>, gid: Option<u16>) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    let uid = uid.map_or(u64::MAX, |u| u as u64);
    let gid = gid.map_or(u64::MAX, |g| g as u64);
    np_dispatch(&r, ninep_abi::NP_CHOWN, [r.len as u64, uid, gid, 0], &fsp[..r.len], &[], &mut [])
}

/// Read a whole small file into `buf`, returning its length, or `0` on any error
/// (missing file / no filesystem). A convenience over [`fs_read_bulk`] for
/// config-sized files like `/etc/passwd` / `/etc/group` (the account tools use
/// it); `buf` should be `<= SAFECOPY_MAX` so one bulk read covers it.
/// Read a whole small file, distinguishing **"could not read it"** from
/// **"it is empty or absent"**: `None` on a read error or a file that did not
/// fit `buf`, `Some(len)` otherwise (`Some(0)` for a genuinely missing file).
///
/// [`read_file_all`] collapses every one of those into `0`, which is fine for a
/// reader that just degrades - but catastrophic for a **read-modify-write** of
/// the account database. There, a `0` from a transient error or a file larger
/// than the buffer means the rewrite is built from *nothing* and replaces the
/// real file with a single line, silently discarding every other account. Any
/// caller that writes the file back must use this and refuse on `None`.
pub fn read_file_checked(path: &str, buf: &mut [u8]) -> Option<usize> {
    let r = fs_read_bulk(path, 0, buf);
    if r >= syscall_abi::FS_ERR_MIN {
        // A missing file is a legitimate empty; anything else is a real failure
        // and must not be mistaken for one.
        return if r == syscall_abi::FS_ERR_NOT_FOUND { Some(0) } else { None };
    }
    let n = r as usize;
    // Exactly filling the buffer means the file may be longer than we can see,
    // so a rewrite from it would truncate the tail. Refuse rather than guess.
    if n >= buf.len() {
        return None;
    }
    Some(n)
}

pub fn read_file_all(path: &str, buf: &mut [u8]) -> usize {
    let r = fs_read_bulk(path, 0, buf);
    if r < syscall_abi::FS_ERR_MIN {
        (r as usize).min(buf.len())
    } else {
        0
    }
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

/// What [`fs_presence`] could determine about a path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Presence {
    /// The server said there is no such file.
    Absent,
    /// The server returned a stat record.
    Present,
    /// The server answered something else - `NO_FS` while `fsd` is restarting,
    /// a permission refusal, a backend that cannot stat. We do not know.
    Unknown,
}

/// Whether `path` names something, as far as the server will say.
///
/// The check behind `mv`/`cp`'s refusal to replace a destination without `-f`.
/// It lives here rather than in each command so the two cannot drift into
/// disagreeing about what "already there" means - they are the same question,
/// asked about the same destructive act.
///
/// THE THIRD STATE IS THE POINT. An earlier version returned a bare `bool` and
/// folded every error into "no", reasoning that the operation would then go
/// ahead and fail on its own merits at the server. That reasoning was true of
/// the server as it used to be and is FALSE of the one this guard was written
/// for: `fsd` no longer refuses an existing destination, it REPLACES it. So an
/// `NP_STAT` that failed for any reason other than absence would have silently
/// switched the guard off and destroyed the file it exists to protect. The
/// callers now fail closed on `Unknown` and say which it was.
pub fn fs_presence(path: &str) -> Presence {
    let mut info = [0u8; ninep_abi::STAT_INFO_LEN];
    let r = fs_stat(path, &mut info);
    if !is_fs_error(r) {
        Presence::Present
    } else if r == syscall_abi::FS_ERR_NOT_FOUND {
        Presence::Absent
    } else {
        Presence::Unknown
    }
}

/// Leading `-f` / `--` for `mv` and `cp`: returns `(force, first_operand_index)`,
/// or `None` for an unrecognised option (the caller prints its own message).
///
/// `--` ends the options, so a file whose name begins with `-` stays reachable
/// (`mv -- -odd.txt new.txt`) - it was not, briefly, which is why this exists.
/// An unknown `-word` is refused rather than taken as a filename: for a command
/// that destroys data, reading a mistyped flag as the source is the wrong way
/// to be forgiving.
pub fn parse_force_opts() -> Option<(bool, u64)> {
    let mut force = false;
    let mut i = 1u64;
    loop {
        // Three bytes distinguishes `-f` and `--` (two) from anything longer,
        // which `arg` would silently truncate into looking like one of them.
        let mut buf = [0u8; 3];
        match arg(i, &mut buf) {
            Some(2) if &buf[..2] == b"--" => return Some((force, i + 1)),
            Some(2) if &buf[..2] == b"-f" => {
                force = true;
                i += 1;
            }
            Some(n) if n >= 1 && buf[0] == b'-' => return None,
            _ => return Some((force, i)),
        }
    }
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

/// The POSIX `(mode, uid, gid)` of a stat record - `mode` is the `S_IFMT` type
/// nibble plus the 12 permission bits - or `None` when the filesystem can't
/// model an owner/permissions (`mode_valid` == 0: FAT32/exFAT/`/proc`).
pub fn stat_mode(info: &[u8]) -> Option<(u16, u16, u16)> {
    if info[ninep_abi::STAT_MODEVALID_OFF] == 0 {
        return None;
    }
    let mode = u16::from_le_bytes([info[ninep_abi::STAT_MODE_OFF], info[ninep_abi::STAT_MODE_OFF + 1]]);
    let uid = u16::from_le_bytes([info[ninep_abi::STAT_UID_OFF], info[ninep_abi::STAT_UID_OFF + 1]]);
    let gid = u16::from_le_bytes([info[ninep_abi::STAT_GID_OFF], info[ninep_abi::STAT_GID_OFF + 1]]);
    Some((mode, uid, gid))
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

/// Write `data` to `path` as a **private** file (mode `0600`), creating it if
/// absent and asserting the mode *before* any content lands.
///
/// The ordering is the whole point, and it is easy to get subtly wrong in three
/// different ways - which is why this exists once instead of at each call site:
///
/// * `fsd` creates a new file world-readable (ext2's `NEW_FILE_MODE` is 0644),
///   so create-then-write-then-`chmod` publishes the secrets for the width of
///   two IPC round trips.
/// * An *existing* file's mode must be re-asserted too. An interrupted earlier
///   run, an admin's `touch`, or a tool that once got the order wrong can leave
///   a 0644 file that no later write would ever have repaired - the content
///   overwrite preserves the mode it finds, including a wrong one.
/// * Re-asserting it must not TRUNCATE: an unconditional empty write to fix a
///   mode would stake the whole database on the calls after it succeeding.
///   `chmod` needs no truncation, so the file is only ever created empty when
///   it genuinely does not exist yet.
///
/// A filesystem that models no mode at all (FAT32, exFAT) answers
/// [`syscall_abi::FS_ERR_NOT_SUPPORTED`], which is expected rather than a
/// failure - there is no privacy to be had there and `login` says so at the
/// prompt. Any other refusal is real and is returned.
///
/// Returns `0` on success, or an `FS_ERR_*` code (see [`is_fs_error`]).
pub fn write_private_file(path: &str, data: &[u8]) -> u64 {
    // 1. Make sure it exists - but only create when it is genuinely absent, so
    //    this never truncates a database it is about to rewrite from a buffer.
    let mut info = [0u8; ninep_abi::STAT_INFO_LEN];
    if is_fs_error(fs_stat(path, &mut info)) {
        let code = fs_write_bulk(path, &[]);
        if is_fs_error(code) {
            return code;
        }
    }
    // 2. Restrict it while it is still empty (or still holds only the old
    //    content), never after the new secrets have landed.
    let code = fs_chmod(path, 0o600);
    if is_fs_error(code) && code != syscall_abi::FS_ERR_NOT_SUPPORTED {
        return code;
    }
    // 3. Only now the content.
    fs_write_bulk(path, data)
}

/// Write `data` at `offset` into an **existing** private file, asserting mode
/// 0600 first. The partner of [`write_private_file`], for the case where only
/// part of a file changed.
///
/// The difference that matters is that this **never truncates**. A whole-file
/// write is truncate-then-write - `ext2`'s overwrite branch frees the old blocks
/// before the new ones land - so an `fsd` restart or a power loss in that window
/// leaves a credential database EMPTY and every account, root included, unable
/// to log in. Writing only the bytes that changed cannot do that: the worst an
/// interruption leaves behind is a damaged copy of the one entry being
/// rewritten, with every other account's line untouched.
///
/// Use [`accounts::changed_span`]-style logic to find the range; this only does
/// the mode + write half, so it stays free of any notion of what the file holds.
pub fn write_private_at(path: &str, offset: u64, data: &[u8]) -> u64 {
    // The mode goes first, as in write_private_file: an existing file that is
    // somehow world-readable is repaired before more secrets land in it, never
    // after. FS_ERR_NOT_SUPPORTED means the filesystem models no mode at all.
    let code = fs_chmod(path, 0o600);
    if is_fs_error(code) && code != syscall_abi::FS_ERR_NOT_SUPPORTED {
        return code;
    }
    fs_write_at(path, offset, data)
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
        syscall_abi::FS_ERR_NOT_SUPPORTED => b"not supported by this filesystem (mode/owner need ext2)",
        syscall_abi::FS_ERR_PERM => b"permission denied",
        syscall_abi::FS_ERR_IO => b"device I/O error",
        syscall_abi::FS_ERR_AUTH => b"cluster authentication failed (peer not authorized, or bad key/signature)",
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

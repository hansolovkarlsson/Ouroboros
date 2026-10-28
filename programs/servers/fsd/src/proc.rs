//! `/proc` - a synthetic, read-only process-table filesystem (cluster Phase 3;
//! see `docs/roadmap/roadmap-cluster-phase3.md`). It holds no disk: every listing and
//! file is generated on demand from the kernel's scheduler via the ungated
//! `TASK_STATE` syscall, so the machine's live task table *is* a file tree.
//!
//! It is a fourth arm of `vfs::Filesystem`, dispatched by the reserved
//! `PROC_TREE` mount index (auto-mounted at `fsd` boot), so the whole verb/reply
//! path - local *and* exported over 9P/TCP - works above it unchanged. That is
//! how `ls /mnt/a/proc` on one machine lists *another* machine's processes: the
//! export gateway routes `/proc` paths here, and this arm answers from that
//! machine's own kernel.
//!
//! Layout:
//! ```text
//!   /              ->  cpus 0/ 1/ 2/ ... the cores file, then one dir per scheduler slot
//!   /cpus          ->  "4\n"             how many cores take tasks (CORE_INFO_UP; since 2026-10-11)
//!   /<n>           ->  state core        the slot's files
//!   /<n>/state     ->  "runnable\n" | "blocked\n" | "zombie\n" | "unused\n"
//!   /<n>/core      ->  "1\n" | "none\n"   the core executing the task now (CORE_INFO_TASK_CORE; since 2026-10-11)
//! ```
//!
//! Every file here is generated at the instant it is read, and its size in a
//! listing or a `stat` is that instant's too: `state` is 8 or 9 bytes and
//! `core` 2 to 5 as the task moves, so a reader that sizes first and then
//! reads a moment later can read short. Read to the short read, as `cat` does.
//!
//! Read-only: every write/mutate method returns [`Error::ReadOnly`]. A slot
//! exposes its scheduler state and the core executing it; a `name` file could
//! join them, since the kernel's `TASK_NAME` answers for any slot (the shell's
//! `ps` reads it), and nobody has asked for one.
//! `/cpus` is the first top-level entry with a name rather than a slot number:
//! the boot core and every secondary that came up and takes tasks, which until
//! it existed a user could read only off the boot log or `coreprobe`'s report.
//!
//! `no_std`, no heap, and PIE-relocation-safe like the rest of userland: path
//! parsing is byte scanning (no `str` range-indexing / char-pattern splits, which
//! pull in a core lookup table and an `R_AARCH64_ABS64` - see docs/processes.md).

// No unsafe code: a disk handle conjured from nothing (`zeroed`, a
// `transmute`) or an `svc` of its own would reach the disk around the
// `Disk` this module was handed (disk.rs). `make check-disk-handle`.
#![forbid(unsafe_code)]

use core::ops::ControlFlow;

use crate::fat32::Error;

/// The synthetic process filesystem. Zero-sized - all state lives in the kernel.
pub struct Fs;

/// What a `/proc` path names.
enum Node {
    /// `/` - the set of task slots.
    Root,
    /// `/<n>` - one task slot's directory. `parse` has checked the slot
    /// exists, and every slot's directory lists the same two files, so the
    /// number is not kept.
    Task,
    /// `/<n>/state` - the slot's state file.
    State(u64),
    /// `/<n>/core` - the core executing the slot's task now, or `none`.
    Core(u64),
    /// `/cpus` - how many cores take tasks.
    Cpus,
    /// A path beneath one of the files (`/cpus/x`, `/<n>/state/x`,
    /// `/<n>/core/x`): not a directory, as a path under a file answers on the
    /// disk arms (the high reviews of #270 and #271).
    UnderFile,
    /// A path that resolves to nothing here.
    NotFound,
}

impl Fs {
    pub fn new() -> Self {
        Fs
    }

    /// No backing partition (synthetic) - reported as LBA 0 in the mount info.
    pub fn partition_lba(&self) -> u32 {
        0
    }

    pub fn list_dir(
        &mut self,
        path: &str,
        mut f: impl FnMut(&str, bool) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        match parse(path) {
            Node::Root => {
                // The cores file first, then one directory per valid scheduler
                // slot (0.. until the kernel reports no such slot). Includes
                // unused slots - their `state` file just reads "unused", the
                // same as `ps` shows.
                if f("cpus", false).is_break() {
                    return Ok(());
                }
                let mut i = 0u64;
                loop {
                    if crate::syscall4::<{ syscall_abi::TASK_STATE }>(i, 0, 0, 0)
                        == syscall_abi::TASK_STATE_INVALID
                    {
                        break;
                    }
                    let mut buf = [0u8; 20];
                    let n = u64_decimal(i, &mut buf);
                    if let Ok(name) = core::str::from_utf8(&buf[..n]) {
                        if f(name, true).is_break() {
                            return Ok(());
                        }
                    }
                    i += 1;
                }
                Ok(())
            }
            Node::Task => {
                // A slot's directory holds `state` and `core`.
                if f("state", false).is_continue() {
                    let _ = f("core", false);
                }
                Ok(())
            }
            Node::State(_) | Node::Core(_) | Node::Cpus | Node::UnderFile => Err(Error::NotADirectory),
            Node::NotFound => Err(Error::NotFound),
        }
    }

    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        self.read_at(path, 0, buf)
    }

    /// [`read_at`](Self::read_at) a piece at a time, as the disk arms do. A
    /// /proc file is made anew for every read and is one line, so it is read
    /// by path each piece: there is nothing on a disk to resolve once.
    pub fn read_each(
        &mut self,
        path: &str,
        offset: u64,
        want: u64,
        buf: &mut [u8],
        sink: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<(u64, bool), Error> {
        crate::vfs::read_pieces(offset, want, buf, |o, b| self.read_at(path, o, b), sink)
    }

    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        match parse(path) {
            Node::Root | Node::Task => Err(Error::NotAFile),
            Node::UnderFile => Err(Error::NotADirectory),
            Node::NotFound => Err(Error::NotFound),
            file => {
                let mut line = [0u8; LINE_MAX];
                let len = file_bytes(&file, &mut line);
                Ok(copy_out(&line[..len], offset, buf))
            }
        }
    }

    /// Metadata for one path: type and size. Synthetic, so there's no
    /// timestamp (`time: None`). Backs `ls -l`.
    pub fn stat(&mut self, path: &str) -> Result<crate::vfs::Stat, Error> {
        let (size, is_dir) = match parse(path) {
            Node::Root | Node::Task => (0u64, true),
            Node::UnderFile => return Err(Error::NotADirectory),
            Node::NotFound => return Err(Error::NotFound),
            file => {
                let mut line = [0u8; LINE_MAX];
                (file_bytes(&file, &mut line) as u64, false)
            }
        };
        Ok(crate::vfs::Stat {
            size,
            is_dir,
            time: None,
            mode: None, // synthetic: no owner/permission model
        })
    }

    // /proc is read-only: every mutate op is refused.
    pub fn write_file(&mut self, _path: &str, _data: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn write_at(&mut self, _path: &str, _offset: u64, _data: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mkdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rmdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn touch(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn rm(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
    pub fn mv(&mut self, _src: &str, _dst: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }
}

/// The bytes of one of the files, formatted into `buf` at the instant of
/// the read: the one place a file's content is made, so a listing's size,
/// a `stat` and a read cannot disagree on how it is spelled, and the next
/// file (`name` is a candidate) is one arm here and nowhere else.
fn file_bytes(node: &Node, buf: &mut [u8; LINE_MAX]) -> usize {
    match node {
        Node::State(n) => {
            let bytes = state_bytes(*n);
            buf[..bytes.len()].copy_from_slice(bytes);
            bytes.len()
        }
        Node::Core(n) => core_bytes(*n, buf),
        Node::Cpus => cpus_bytes(buf),
        // Not files; the callers' matches keep them out.
        Node::Root | Node::Task | Node::UnderFile | Node::NotFound => 0,
    }
}

/// The state string for slot `n`, from the `TASK_STATE` syscall. `parse`
/// refuses a slot past the scheduler's count, so this never sees one.
fn state_bytes(n: u64) -> &'static [u8] {
    match crate::syscall4::<{ syscall_abi::TASK_STATE }>(n, 0, 0, 0) {
        syscall_abi::TASK_STATE_RUNNABLE => b"runnable\n",
        syscall_abi::TASK_STATE_BLOCKED => b"blocked\n",
        syscall_abi::TASK_STATE_ZOMBIE => b"zombie\n",
        _ => b"unused\n",
    }
}

/// The most any file here holds: twenty digits and a newline for the
/// decimal lines (`/cpus`, `/<n>/core`), more than `state`'s longest word.
/// The one buffer size every file is formatted into.
const LINE_MAX: usize = 21;

/// The bytes of `/cpus`: the cores that take tasks, from the kernel's
/// `CORE_INFO_UP` (the boot core and every secondary that came up), as one
/// decimal line. Asked on every read, so it is what the kernel says now.
fn cpus_bytes(buf: &mut [u8; LINE_MAX]) -> usize {
    decimal_line(crate::syscall4::<{ syscall_abi::CORE_INFO }>(syscall_abi::CORE_INFO_UP, 0, 0, 0), buf)
}

/// `v` as a decimal line, `"4\n"`, into `buf`; the length written. The one
/// spelling of a synthetic file's number (`/cpus`, `/<n>/core`).
fn decimal_line(v: u64, buf: &mut [u8; LINE_MAX]) -> usize {
    let n = u64_decimal(v, &mut buf[..LINE_MAX - 1]);
    buf[n] = b'\n';
    n + 1
}

/// The bytes of `/<n>/core`: the index of the core executing slot `n` at
/// this instant, from the kernel's `CORE_INFO_TASK_CORE`, or `none` when no
/// core is (runnable but waiting its turn, blocked, empty, or the idle slot).
/// `fsd` itself is on a core while it answers, so its own slot reads a core.
fn core_bytes(n: u64, buf: &mut [u8; LINE_MAX]) -> usize {
    // `core_of` is the one spelling of the test, shared with the shell's `ps`.
    match syscall_abi::core_of(crate::syscall4::<{ syscall_abi::CORE_INFO }>(syscall_abi::CORE_INFO_TASK_CORE, n, 0, 0)) {
        Some(core) => decimal_line(core, buf),
        None => {
            buf[..5].copy_from_slice(b"none\n");
            5
        }
    }
}

/// Copy `src[offset..]` into `buf`, returning the byte count (0 at/past the end).
fn copy_out(src: &[u8], offset: u64, buf: &mut [u8]) -> u32 {
    let off = offset as usize;
    if off >= src.len() {
        return 0;
    }
    let n = (src.len() - off).min(buf.len());
    buf[..n].copy_from_slice(&src[off..off + n]);
    n as u32
}

/// Parse a `/proc`-relative path (byte scanning; relocation-safe). Accepts `/`,
/// `/cpus`, `/<digits>`, `/<digits>/state` and `/<digits>/core` for a slot the
/// scheduler has; a path below any of the files is [`Node::UnderFile`] and
/// anything else [`Node::NotFound`].
fn parse(path: &str) -> Node {
    let b = path.as_bytes();
    let mut i = 0;
    // Skip a leading '/'.
    if i < b.len() && b[i] == b'/' {
        i += 1;
    }
    if i >= b.len() {
        return Node::Root;
    }
    // The one named entry, a file: nothing lives under it, and a path that
    // goes on below it is "not a directory", as under a file on a disk.
    if &b[i..] == b"cpus" {
        return Node::Cpus;
    }
    if b[i..].starts_with(b"cpus/") {
        return Node::UnderFile;
    }
    // First component: the slot number (all digits).
    let start = i;
    let mut n = 0u64;
    while i < b.len() && b[i] != b'/' {
        let c = b[i];
        if !c.is_ascii_digit() {
            return Node::NotFound;
        }
        n = n.wrapping_mul(10).wrapping_add((c - b'0') as u64);
        i += 1;
    }
    if i == start {
        return Node::NotFound; // empty component (e.g. "//")
    }
    // A slot the scheduler does not have is not here either: the root
    // listing never offers it, and before 2026-10-11 `/99/state` read
    // "unused" and `/99/core` would have read "none", a plausible file for
    // a slot that does not exist (the third high review of #271).
    if crate::syscall4::<{ syscall_abi::TASK_STATE }>(n, 0, 0, 0) == syscall_abi::TASK_STATE_INVALID {
        return Node::NotFound;
    }
    if i >= b.len() {
        return Node::Task; // "/<n>"
    }
    // There is a second component after the '/'.
    i += 1; // skip '/'
    let rest = &b[i..];
    if rest == b"state" {
        Node::State(n)
    } else if rest == b"core" {
        Node::Core(n)
    } else if rest.starts_with(b"state/") || rest.starts_with(b"core/") {
        Node::UnderFile
    } else {
        Node::NotFound
    }
}

/// Format `v` as decimal into `buf`, returning the byte length. Manual digit loop
/// (no `core::fmt`), relocation-safe.
fn u64_decimal(v: u64, buf: &mut [u8]) -> usize {
    if v == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut n = v;
    let mut i = 0;
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    // Reverse into buf.
    for j in 0..i {
        buf[j] = tmp[i - 1 - j];
    }
    i
}

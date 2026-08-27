//! `/proc` - a synthetic, read-only process-table filesystem (cluster Phase 3;
//! see `docs/roadmap-cluster-phase3.md`). It holds no disk: every listing and
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
//!   /              ->  0/ 1/ 2/ ...      one dir per scheduler slot
//!   /<n>           ->  state             the slot's files
//!   /<n>/state     ->  "runnable\n" | "blocked\n" | "zombie\n" | "unused\n"
//! ```
//!
//! Read-only: every write/mutate method returns [`Error::ReadOnly`]. Only per-slot
//! *state* is exposed - there is no cross-task argv/name accessor in the kernel
//! yet (`GET_ARG*` returns the *caller's* own), so `/<n>` has just `state`.
//!
//! `no_std`, no heap, and PIE-relocation-safe like the rest of userland: path
//! parsing is byte scanning (no `str` range-indexing / char-pattern splits, which
//! pull in a core lookup table and an `R_AARCH64_ABS64` - see docs/processes.md).

use crate::fat32::Error;

/// The synthetic process filesystem. Zero-sized - all state lives in the kernel.
pub struct Fs;

/// What a `/proc` path names.
enum Node {
    /// `/` - the set of task slots.
    Root,
    /// `/<n>` - one task slot's directory.
    Task(u64),
    /// `/<n>/state` - the slot's state file.
    State(u64),
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

    pub fn list_dir(&mut self, path: &str, mut f: impl FnMut(&str, bool, u32)) -> Result<(), Error> {
        match parse(path) {
            Node::Root => {
                // One directory per valid scheduler slot (0.. until the kernel
                // reports no such slot). Includes unused slots - their `state`
                // file just reads "unused", the same as `ps` shows.
                let mut i = 0u64;
                loop {
                    if crate::syscall4(syscall_abi::TASK_STATE, i, 0, 0, 0)
                        == syscall_abi::TASK_STATE_INVALID
                    {
                        break;
                    }
                    let mut buf = [0u8; 20];
                    let n = u64_decimal(i, &mut buf);
                    if let Ok(name) = core::str::from_utf8(&buf[..n]) {
                        f(name, true, 0);
                    }
                    i += 1;
                }
                Ok(())
            }
            Node::Task(n) => {
                // A slot's directory holds just `state`.
                f("state", false, state_bytes(n).len() as u32);
                Ok(())
            }
            Node::State(_) => Err(Error::NotADirectory),
            Node::NotFound => Err(Error::NotFound),
        }
    }

    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        match parse(path) {
            Node::State(n) => Ok(copy_out(state_bytes(n), 0, buf)),
            Node::Root | Node::Task(_) => Err(Error::NotAFile),
            Node::NotFound => Err(Error::NotFound),
        }
    }

    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        match parse(path) {
            Node::State(n) => Ok(copy_out(state_bytes(n), offset, buf)),
            Node::Root | Node::Task(_) => Err(Error::NotAFile),
            Node::NotFound => Err(Error::NotFound),
        }
    }

    /// Metadata for one path: type and size. Synthetic, so there's no
    /// timestamp (`time: None`). Backs `ls -l`.
    pub fn stat(&mut self, path: &str) -> Result<crate::vfs::Stat, Error> {
        let (size, is_dir) = match parse(path) {
            Node::Root | Node::Task(_) => (0u64, true),
            Node::State(n) => (state_bytes(n).len() as u64, false),
            Node::NotFound => return Err(Error::NotFound),
        };
        Ok(crate::vfs::Stat {
            size,
            is_dir,
            time: None,
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

/// The state string for slot `n`, from the `TASK_STATE` syscall. A slot past the
/// scheduler's count reads "unused" (harmless; the listing never offers one).
fn state_bytes(n: u64) -> &'static [u8] {
    match crate::syscall4(syscall_abi::TASK_STATE, n, 0, 0, 0) {
        syscall_abi::TASK_STATE_RUNNABLE => b"runnable\n",
        syscall_abi::TASK_STATE_BLOCKED => b"blocked\n",
        syscall_abi::TASK_STATE_ZOMBIE => b"zombie\n",
        _ => b"unused\n",
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
/// `/<digits>`, and `/<digits>/state`; anything else is [`Node::NotFound`].
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
    if i >= b.len() {
        return Node::Task(n); // "/<n>"
    }
    // There is a second component after the '/'.
    i += 1; // skip '/'
    let rest = &b[i..];
    if rest == b"state" {
        Node::State(n)
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

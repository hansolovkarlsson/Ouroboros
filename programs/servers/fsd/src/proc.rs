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
//! Read-only: every write/mutate method returns [`Error::ReadOnly`]. Only per-slot
//! *state* is exposed - there is no cross-task argv/name accessor in the kernel
//! yet (`GET_ARG*` returns the *caller's* own), so `/<n>` has just `state`.
//! `/cpus` is the first top-level entry with a name rather than a slot number:
//! the boot core and every secondary that came up and takes tasks, which until
//! it existed a user could read only off the boot log or `coreprobe`'s report.
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
    /// `/<n>/core` - the core executing the slot's task now, or `none`.
    Core(u64),
    /// `/cpus` - how many cores take tasks.
    Cpus,
    /// `/cpus/...` - a path beneath the file: not a directory, as a path under
    /// a file answers on the disk arms (the high review of #270).
    UnderCpus,
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
                // The cores file first, then one directory per valid scheduler
                // slot (0.. until the kernel reports no such slot). Includes
                // unused slots - their `state` file just reads "unused", the
                // same as `ps` shows.
                let mut cpus = [0u8; CPUS_MAX];
                f("cpus", false, cpus_bytes(&mut cpus) as u32);
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
                // A slot's directory holds `state` and `core`.
                f("state", false, state_bytes(n).len() as u32);
                let mut core = [0u8; CPUS_MAX];
                f("core", false, core_bytes(n, &mut core) as u32);
                Ok(())
            }
            Node::State(_) | Node::Core(_) | Node::Cpus | Node::UnderCpus => Err(Error::NotADirectory),
            Node::NotFound => Err(Error::NotFound),
        }
    }

    pub fn read_file(&mut self, path: &str, buf: &mut [u8]) -> Result<u32, Error> {
        self.read_at(path, 0, buf)
    }

    pub fn read_at(&mut self, path: &str, offset: u64, buf: &mut [u8]) -> Result<u32, Error> {
        match parse(path) {
            Node::State(n) => Ok(copy_out(state_bytes(n), offset, buf)),
            Node::Core(n) => {
                let mut core = [0u8; CPUS_MAX];
                let len = core_bytes(n, &mut core);
                Ok(copy_out(&core[..len], offset, buf))
            }
            Node::Cpus => {
                let mut cpus = [0u8; CPUS_MAX];
                let n = cpus_bytes(&mut cpus);
                Ok(copy_out(&cpus[..n], offset, buf))
            }
            Node::Root | Node::Task(_) => Err(Error::NotAFile),
            Node::UnderCpus => Err(Error::NotADirectory),
            Node::NotFound => Err(Error::NotFound),
        }
    }

    /// Metadata for one path: type and size. Synthetic, so there's no
    /// timestamp (`time: None`). Backs `ls -l`.
    pub fn stat(&mut self, path: &str) -> Result<crate::vfs::Stat, Error> {
        let (size, is_dir) = match parse(path) {
            Node::Root | Node::Task(_) => (0u64, true),
            Node::State(n) => (state_bytes(n).len() as u64, false),
            Node::Core(n) => {
                let mut core = [0u8; CPUS_MAX];
                (core_bytes(n, &mut core) as u64, false)
            }
            Node::Cpus => {
                let mut cpus = [0u8; CPUS_MAX];
                (cpus_bytes(&mut cpus) as u64, false)
            }
            Node::UnderCpus => return Err(Error::NotADirectory),
            Node::NotFound => return Err(Error::NotFound),
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

/// The most `/cpus` holds: twenty decimal digits and a newline.
const CPUS_MAX: usize = 21;

/// The bytes of `/cpus`: the cores that take tasks, from the kernel's
/// `CORE_INFO_UP` (the boot core and every secondary that came up), as one
/// decimal line. Asked on every read, so it is what the kernel says now.
fn cpus_bytes(buf: &mut [u8; CPUS_MAX]) -> usize {
    let up = crate::syscall4(syscall_abi::CORE_INFO, syscall_abi::CORE_INFO_UP, 0, 0, 0);
    let n = u64_decimal(up, &mut buf[..CPUS_MAX - 1]);
    buf[n] = b'\n';
    n + 1
}

/// The bytes of `/<n>/core`: the index of the core executing slot `n` at
/// this instant, from the kernel's `CORE_INFO_TASK_CORE`, or `none` when no
/// core is (runnable but waiting its turn, blocked, empty, or the idle slot).
/// `fsd` itself is on a core while it answers, so its own slot reads a core.
fn core_bytes(n: u64, buf: &mut [u8; CPUS_MAX]) -> usize {
    let core = crate::syscall4(syscall_abi::CORE_INFO, syscall_abi::CORE_INFO_TASK_CORE, n, 0, 0);
    if core == syscall_abi::CORE_INFO_NO_CORE || core == syscall_abi::CORE_INFO_BAD_OP {
        buf[..5].copy_from_slice(b"none\n");
        return 5;
    }
    let len = u64_decimal(core, &mut buf[..CPUS_MAX - 1]);
    buf[len] = b'\n';
    len + 1
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
/// `/cpus`, `/<digits>`, `/<digits>/state` and `/<digits>/core`; `/cpus/...` is
/// [`Node::UnderCpus`] and anything else [`Node::NotFound`].
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
        return Node::UnderCpus;
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
    } else if rest == b"core" {
        Node::Core(n)
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

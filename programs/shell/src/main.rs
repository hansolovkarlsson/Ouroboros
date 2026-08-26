//! Ouroboros's default shell — a real, separately-loaded userland program,
//! not kernel code. This is the first thing the kernel loads and runs: see
//! `kernel/src/loader.rs` for how a binary gets from the ESP's filesystem
//! into memory, and `kernel/src/tasks.rs` for how it gets turned into a
//! running EL0 task. The kernel picks *which* binary to load from a config
//! file, so this program can be replaced without touching kernel code -
//! see `docs/processes.md` for the full mechanism and a guide to writing a
//! replacement.
//!
//! Everything here used to live at EL1, in the kernel's own `shell.rs`
//! (buffer, backspace handling, echo), driven by a dedicated `shell_input`
//! syscall from a trivial EL0 poll loop. That syscall is gone now - this
//! program calls `try_read_char`/`putc` directly and does its own line
//! editing, which is what "the shell is a separate process" actually means
//! in practice, not just in theory.
//!
//! Deliberately has no global mutable state (the input buffer, current
//! working directory, etc. are all locals in `main`'s stack frame, passed
//! down by `&mut` reference): `linker.ld` defines but asserts empty
//! `.data`/`.bss`, since there's no crt0 here to zero a real `.bss` before
//! `main` runs, and the loader only copies exactly the file's bytes - a
//! nonzero `.bss` would just be missing from memory entirely.
//!
//! ## Phase 2: commands
//!
//! [`on_byte`] no longer just echoes the completed line - [`run_line`]
//! tokenizes it (whitespace-split, no quoting) and dispatches to a small
//! builtin table. `uptime` is the first builtin that needs real kernel
//! state (`get_ticks`, `syscall_abi::GET_TICKS`) rather than being
//! another echo demo - this program can no longer just read
//! `exceptions.rs`'s statics directly the way the kernel-resident line
//! editor it replaced could, so exposing that state needed a new
//! syscall.
//!
//! ## Phase 3c: disk commands, and why path resolution needed no ".."/"."
//! ## special-casing
//!
//! `ls`/`cat`/`cd` call two new syscalls (`fs_list_dir`/`fs_read_file`,
//! `kernel/src/syscall.rs`) that only ever take a real kernel state -
//! there's no direct hardware access from EL0, same reasoning as
//! `try_read_char`/`putc`. Both syscalls now need more than one argument
//! (a path pointer/length, a buffer pointer/length), which is why the
//! syscall ABI itself grew from 1 argument to 4 - see `syscall`/`syscall4`
//! below and `kernel/src/exceptions.rs`'s module doc comment.
//!
//! `cd` needs relative-path support (`cd BOOT`, not just `cd /EFI/BOOT`) -
//! [`resolve_path`] just concatenates `cwd` and the given argument, with
//! **no special handling for `.` or `..`**. That's not a missing feature:
//! real FAT32 subdirectories contain genuine `.`/`..` directory entries
//! pointing at their own and their parent's cluster (confirmed directly
//! while testing phase 3b, listing `\EFI\BOOT` - `.` and `..` showed up as
//! ordinary entries), so the kernel-side `fat32.rs::Fs::find` already
//! resolves `..` correctly just by walking it like any other path
//! component. Only the root directory has no `..` (it has no parent) -
//! `cd ..` from `/` fails with "no such directory" rather than silently
//! staying put, a minor, acceptable rough edge for this phase.
//!
//! See `docs/processes.md` for the full syscall table, and
//! `docs/shell-commands.md` for a user-facing reference of every builtin
//! command below (syntax, behavior, known limitations).

#![no_std]
#![no_main]

use core::arch::asm;
use core::fmt::Write as _;
use core::panic::PanicInfo;

const BUFFER_SIZE: usize = 128;
const CWD_SIZE: usize = 128;
const PATH_SIZE: usize = 128;
// Max argv entries passed to a spawned program (`exec prog a b c ...`). The
// 128-byte input line can't hold more real tokens than this in practice.
const MAX_ARGS: usize = 16;

// Max stages in a pipeline (`a | b | c | ...`). Generous; the real ceiling is
// spawnable task slots (five: 5..NUM_TASKS), so a chain longer than that fails
// at spawn with "no free task slot" rather than here - see cmd_pipeline.
const MAX_STAGES: usize = 8;

// Where an unknown command is looked up as a program: a `:`-separated list of
// directories searched in order. A constant for now; Stage 3 makes it a real
// `PATH` env var. `/bin` matches the uppercase on-disk `\BIN\` case-
// insensitively (fsd's `find`), so a lowercase-typed command works.
const DEFAULT_PATH: &str = "/bin";

// The shell environment: a small fixed table of NAME=VALUE variables, held
// stack-local in `main` and threaded by `&mut` (like `cwd`), since userland
// has no static mutable state. `PATH` lives here (drives command lookup);
// `$VAR` in a line expands from here. Shell-local only - not exported into
// child programs yet (that's a later, argv-like ABI). Sizes bound by the
// 128-byte input line (`PATH` is the longest value).
const MAX_ENV_VARS: usize = 16;
const ENV_NAME_SIZE: usize = 24;
const ENV_VALUE_SIZE: usize = 128;
// Buffer for a line after `$VAR` expansion (can grow past the 128-byte input
// when a variable's value is long).
const EXPAND_SIZE: usize = 256;

struct Env {
    names: [[u8; ENV_NAME_SIZE]; MAX_ENV_VARS],
    name_lens: [usize; MAX_ENV_VARS],
    vals: [[u8; ENV_VALUE_SIZE]; MAX_ENV_VARS],
    val_lens: [usize; MAX_ENV_VARS],
    count: usize,
}

impl Env {
    fn new() -> Self {
        let mut e = Env {
            names: [[0; ENV_NAME_SIZE]; MAX_ENV_VARS],
            name_lens: [0; MAX_ENV_VARS],
            vals: [[0; ENV_VALUE_SIZE]; MAX_ENV_VARS],
            val_lens: [0; MAX_ENV_VARS],
            count: 0,
        };
        e.set("PATH", DEFAULT_PATH.as_bytes());
        e
    }

    fn index_of(&self, name: &[u8]) -> Option<usize> {
        (0..self.count).find(|&i| self.names[i][..self.name_lens[i]] == *name)
    }

    fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.index_of(name).map(|i| &self.vals[i][..self.val_lens[i]])
    }

    /// Set (or replace) `name`; `false` if the name/value is too long or the
    /// table is full.
    fn set(&mut self, name: &str, value: &[u8]) -> bool {
        let nb = name.as_bytes();
        if nb.is_empty() || nb.len() > ENV_NAME_SIZE || value.len() > ENV_VALUE_SIZE {
            return false;
        }
        let i = match self.index_of(nb) {
            Some(i) => i,
            None => {
                if self.count >= MAX_ENV_VARS {
                    return false;
                }
                self.count += 1;
                self.count - 1
            }
        };
        self.names[i][..nb.len()].copy_from_slice(nb);
        self.name_lens[i] = nb.len();
        self.vals[i][..value.len()].copy_from_slice(value);
        self.val_lens[i] = value.len();
        true
    }

    /// Remove `name` (swap-remove); `false` if it wasn't set.
    fn unset(&mut self, name: &[u8]) -> bool {
        let Some(i) = self.index_of(name) else {
            return false;
        };
        let last = self.count - 1;
        if i != last {
            self.names[i] = self.names[last];
            self.name_lens[i] = self.name_lens[last];
            self.vals[i] = self.vals[last];
            self.val_lens[i] = self.val_lens[last];
        }
        self.count -= 1;
        true
    }
}

/// Expand `$VAR` references in `line` into `out`, returning the expanded
/// string. A `$` followed by `[A-Za-z0-9_]+` is replaced by that variable's
/// value (nothing if unset); a `$` not followed by a name is a literal `$`.
/// Hand-rolled scalar scan - no `core::fmt`, no slice-vs-literal comparisons.
fn expand_vars<'a>(line: &str, env: &Env, out: &'a mut [u8]) -> &'a str {
    let bytes = line.as_bytes();
    let mut n = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > start {
                if let Some(val) = env.get(&bytes[start..j]) {
                    for &b in val {
                        if n < out.len() {
                            out[n] = b;
                            n += 1;
                        }
                    }
                }
                i = j;
                continue;
            }
        }
        if n < out.len() {
            out[n] = bytes[i];
            n += 1;
        }
        i += 1;
    }
    core::str::from_utf8(&out[..n]).unwrap_or("")
}
// (`LIST_BUFFER_SIZE` used to live here for the built-in `ls`'s listing
// buffer; `ls` is a /bin program now, so its listing buffer lives in the
// `ls` crate over `ulib`.)
// The redirect/pipe capture buffer used to be a fixed stack array
// (`CAPTURE_SIZE`, 1024 bytes); it's the program's 256KB heap region now
// (`get_heap` / `Output::Capture`), so a large capture like
// `cat big > file` fits and is written to disk in `SAFECOPY_MAX` chunks
// (`write_all`) rather than refused. Output larger than the heap still
// refuses-not-truncates.

const BACKSPACE: u8 = 0x08;
const DEL: u8 = 0x7f;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

// Syscall numbers and sentinel values come from the shared `syscall-abi`
// crate now, not hand-duplicated local consts - see its doc comment and
// `kernel/src/syscall.rs`'s dispatch table, the other side of this ABI.
use syscall_abi::{EXIT_DENIED, FS_ERR_MIN, FS_ERR_NOT_FOUND, MOUNT_ALREADY, MOUNT_NO_DEVICE, NO_FS, RECV_INTERRUPTED, TASK_KILLED_STATUS, TASK_STATE_BLOCKED, TASK_STATE_INVALID, TASK_STATE_RUNNABLE, TASK_STATE_UNUSED, TASK_STATE_ZOMBIE, WAIT_INTERRUPTED};

/// Placed first in `.text` by `linker.ld` (`KEEP(*(.text.start))`) so it
/// lands at file/VA offset 0 - `tasks.rs` sets a loaded program's
/// `elr_el1` to exactly the load base, no symbol table involved.
#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    for &b in b"Ouroboros userland shell\r\n" {
        putc(b);
    }

    let mut buf = [0u8; BUFFER_SIZE];
    let mut len = 0usize;
    let mut cwd = [0u8; CWD_SIZE];
    cwd[0] = b'/';
    let mut cwd_len = 1usize;
    let mut env = Env::new();

    print_prompt();
    loop {
        // Genuinely blocks now, rather than busy-polling `try_read_char`
        // every iteration - `READ_CHAR` (kernel/src/syscall.rs) suspends
        // this task at the scheduler level and switches to another
        // runnable one (task 1's idle loop, today) until a byte is
        // actually available, then resumes here with it already in hand.
        // Not `wfe()`: real Parallels hardware has a confirmed,
        // unresolved hang when an EL0 task executes `wfe` (see
        // `tasks.rs`'s module doc comment) - this loop never does,
        // deliberately, and never has to worry about whether a tick
        // source exists this boot the way the old busy-poll comment here
        // used to (before real MADT/GICv3 discovery made that concern
        // moot anyway - see CLAUDE.md's "MADT/GICv3" section). The
        // kernel-side blocking mechanism is what changed; this loop just
        // calls a syscall that happens to take longer to return now.
        let byte = read_char();
        on_byte(byte, &mut buf, &mut len, &mut cwd, &mut cwd_len, &mut env);
    }
}

fn print_prompt() {
    putc(b'$');
    putc(b' ');
}

/// Same shape as the kernel's old `shell.rs::on_byte`: CR/LF submits the
/// line (parsed and dispatched via [`run_line`], then a fresh prompt),
/// backspace/DEL erases via the standard destructive-backspace sequence,
/// anything else is appended and echoed immediately.
fn on_byte(byte: u8, buf: &mut [u8; BUFFER_SIZE], len: &mut usize, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize, env: &mut Env) {
    match byte {
        CR | LF => {
            putc(CR);
            putc(LF);
            // buf[..*len] is whatever bytes read_char returned,
            // completely unfiltered (see the `byte` arm below) - not
            // guaranteed valid UTF-8 (e.g. a pasted multi-byte sequence
            // split across separate reads, or a stray high byte), so this
            // has to be checked, not assumed.
            match core::str::from_utf8(&buf[..*len]) {
                Ok(line) => run_line(line, cwd, cwd_len, env),
                Err(_) => print_line("input wasn't valid UTF-8"),
            }
            *len = 0;
            print_prompt();
        }
        BACKSPACE | DEL => {
            if *len > 0 {
                *len -= 1;
                putc(BACKSPACE);
                putc(b' ');
                putc(BACKSPACE);
            }
        }
        byte => {
            // Unhandled C0 control bytes (anything below space that
            // isn't the CR/LF/backspace cases above) are ignored rather
            // than appended - they'd sit invisibly in the buffer and
            // corrupt the eventual command. The case that made this
            // real: a Ctrl+C typed while *this* shell owns the keyboard
            // arrives here as an ordinary 0x03 (the kernel only
            // intercepts it to reclaim the keyboard from a
            // *foregrounded* task - see `cmd_fg`), and should be a
            // clean no-op, not a hidden byte.
            if byte < 0x20 {
                return;
            }
            if *len < BUFFER_SIZE {
                buf[*len] = byte;
                *len += 1;
                putc(byte);
            }
            // Buffer full: silently drop further bytes, same as before.
        }
    }
}

/// Entry point for a submitted line: peels off a trailing `> file` /
/// `>> file` redirect if one is present (see [`parse_redirect`]), sets up
/// the matching [`Output`] sink, dispatches the rest of the line via
/// [`dispatch_line`], then - for a redirect - writes the captured output
/// to the target file ([`finish_redirect`]).
/// This program's heap region as a mutable byte slice (see the `heap_info`
/// syscall): a 256KB raw buffer the shell uses to hold a redirect/pipe
/// capture far larger than its 16KB stack. Not an allocator - just this
/// program's own EL0-accessible heap area.
///
/// # Safety contract
/// Returns a `&'static mut` over a fixed region, so the caller must not
/// keep two of these alive at once (they'd alias). The shell only ever
/// captures one command at a time (a redirect *or* a pipeline, never
/// nested), so each call's slice is used and dropped within one command -
/// no aliasing in practice, the same single-address-space discipline the
/// shell already uses for its syscall buffers.
fn get_heap() -> &'static mut [u8] {
    let base = syscall(syscall_abi::HEAP_INFO, syscall_abi::HEAP_INFO_BASE);
    let size = syscall(syscall_abi::HEAP_INFO, syscall_abi::HEAP_INFO_SIZE) as usize;
    if base == 0 || size == 0 {
        return &mut [];
    }
    unsafe { core::slice::from_raw_parts_mut(base as *mut u8, size) }
}

fn run_line(line: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize, env: &mut Env) {
    // Expand `$VAR` references first, using the current environment. The
    // expanded text lives in `ebuf` (a local) for the rest of this call; env
    // is only read here, so it's free to be borrowed mutably below.
    let mut ebuf = [0u8; EXPAND_SIZE];
    let line = expand_vars(line, env, &mut ebuf);

    // Pipelines first: standalone `|` tokens split the line into N stages.
    // The first stage may be a builtin (its output captured and streamed) or a
    // program; every later stage is a program reading its predecessor's output
    // as stdin and writing to the next (or the console, for the last) - see
    // cmd_pipeline. Combining with `>`/`>>` is refused: the last stage writes
    // straight to the console, so there's no capture of *its* output to
    // redirect.
    if line.split_whitespace().any(|t| t == "|") {
        if line.split_whitespace().any(|t| t == ">" || t == ">>") {
            print_line("pipe: can't combine | with output redirection");
            return;
        }
        let mut stages: [&str; MAX_STAGES] = [""; MAX_STAGES];
        match split_pipeline(line, &mut stages) {
            Ok(n) => cmd_pipeline(&stages[..n], cwd, cwd_len, env),
            Err(msg) => print_line(msg),
        }
        return;
    }
    match parse_redirect(line) {
        RedirectParse::NoRedirect => {
            let mut out = Output::Console;
            dispatch_line(line, cwd, cwd_len, env, &mut out);
        }
        RedirectParse::Malformed(msg) => print_line(msg),
        RedirectParse::Redirect { cmd, target, append } => {
            let capture = get_heap();
            let mut out = Output::Capture { buf: capture, len: 0, overflowed: false };
            dispatch_line(cmd, cwd, cwd_len, env, &mut out);
            let (buf, len, overflowed) = match out {
                Output::Capture { buf, len, overflowed } => (buf, len, overflowed),
                // Never constructed on this path; written out rather than
                // `unreachable!()` so a logic error here degrades to a
                // silently-skipped write, not a panic-handler hang.
                Output::Console => return,
            };
            finish_redirect(&buf[..len], overflowed, target, append, cwd, *cwd_len);
        }
    }
}

/// Split `line` into its `|`-separated stages (trimmed), filling `stages` and
/// returning the count. Each `|` must be its own whitespace token (same
/// no-quoting rule as `parse_redirect`); an empty stage or overflowing
/// `stages` is an error. Only called when a `|` token is present, so the count
/// is always >= 2.
fn split_pipeline<'a>(line: &'a str, stages: &mut [&'a str]) -> Result<usize, &'static str> {
    let base = line.as_ptr() as usize;
    let mut count = 0;
    let mut seg_start = 0usize;
    for token in line.split_whitespace() {
        if token == "|" {
            let tok_off = token.as_ptr() as usize - base;
            let seg = line.get(seg_start..tok_off).unwrap_or("").trim();
            if seg.is_empty() {
                return Err("pipe: empty pipeline stage (a `|` with no command beside it)");
            }
            if count >= stages.len() {
                return Err("pipe: too many pipeline stages");
            }
            stages[count] = seg;
            count += 1;
            seg_start = tok_off + 1;
        }
    }
    let seg = line.get(seg_start..).unwrap_or("").trim();
    if seg.is_empty() {
        return Err("pipe: missing command after |");
    }
    if count >= stages.len() {
        return Err("pipe: too many pipeline stages");
    }
    stages[count] = seg;
    Ok(count + 1)
}

/// Whether `cmd` is a shell builtin (so a pipeline's first stage can be one,
/// captured and streamed - a later stage can't, it must be a program that
/// reads its stdin). The list mirrors `dispatch_line`'s arms.
fn is_builtin(cmd: &str) -> bool {
    matches!(
        cmd,
        "help" | "cd" | "bind" | "pwd" | "write" | "mount" | "unmount" | "erase" | "partition" | "format"
            | "exec" | "exit" | "ps" | "kill" | "fg" | "wait" | "send" | "recv" | "selftest"
            | "env" | "set" | "unset" | "cpu"
    )
}

/// Resolve a pipeline stage's command to a program path `spawn_path` can load:
/// a `/`- or `.`-containing token is used as-is (spawn_path resolves it against
/// the cwd), a bare name is looked up on `$PATH` (the `run_path_command` probe).
/// Returns the path written into `buf`, or `None` (not a program - a builtin,
/// a typo, or no filesystem this boot).
fn resolve_command<'a>(cmd: &str, env: &Env, buf: &'a mut [u8; PATH_SIZE]) -> Option<&'a str> {
    if cmd.as_bytes().first() == Some(&b'/') || cmd.as_bytes().contains(&b'/') {
        let b = cmd.as_bytes();
        if b.is_empty() || b.len() > buf.len() {
            return None;
        }
        buf[..b.len()].copy_from_slice(b);
        return core::str::from_utf8(&buf[..b.len()]).ok();
    }
    let path = env
        .get(b"PATH")
        .and_then(|v| core::str::from_utf8(v).ok())
        .unwrap_or(DEFAULT_PATH);
    for dir in path.split(':') {
        let mut c = 0;
        for &x in dir.as_bytes() {
            if c < buf.len() {
                buf[c] = x;
                c += 1;
            }
        }
        if (c == 0 || buf[c - 1] != b'/') && c < buf.len() {
            buf[c] = b'/';
            c += 1;
        }
        for &x in cmd.as_bytes() {
            if c < buf.len() {
                buf[c] = x;
                c += 1;
            }
        }
        let Ok(cand) = core::str::from_utf8(&buf[..c]) else {
            continue;
        };
        let mut probe = [0u8; 1];
        let r = fs_read_file(cand, &mut probe);
        if r == NO_FS {
            return None; // no filesystem this boot - nothing to find
        }
        if r < FS_ERR_MIN {
            return core::str::from_utf8(&buf[..c]).ok();
        }
    }
    None
}

/// Spawn one program pipeline stage: tokenize `stage` into argv, resolve
/// `argv[0]` to a program path, and `spawn_path` it with `stdout_target`.
/// `Err(0)` = not a program / path too long; any other `Err` is a real spawn
/// status ([`NO_FS`] or an `FS_ERR_*`/`SPAWN_ERR_*` code).
fn spawn_stage(stage: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, env: &Env, stdout_target: u64) -> Result<u64, u64> {
    let mut argv_buf: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut n = 0;
    for w in stage.split_whitespace() {
        if n >= MAX_ARGS {
            break;
        }
        argv_buf[n] = w;
        n += 1;
    }
    if n == 0 {
        return Err(0);
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path) = resolve_command(argv_buf[0], env, &mut path_buf) else {
        return Err(0);
    };
    // `path` borrows `path_buf`; copy it out so spawn_path can take it while
    // argv (which borrows `stage`, a different buffer) stays valid.
    let mut owned = [0u8; PATH_SIZE];
    let plen = path.len();
    owned[..plen].copy_from_slice(path.as_bytes());
    let Ok(path) = core::str::from_utf8(&owned[..plen]) else {
        return Err(0);
    };
    spawn_path(path, &argv_buf[..n], cwd, cwd_len, stdout_target)
}

/// Run an N-stage pipeline (`a | b | c`, N >= 2). The first stage may be a
/// builtin (captured and streamed to stage 2) or a program; every later stage
/// is a program reading its predecessor's output as stdin (`MSG_RECV`) and
/// writing to the next stage, or to the console for the last one.
///
/// Program stages are spawned right-to-left so each producer has a live
/// consumer to point its stdout at; each adjacent producer->consumer link is
/// authorized with one `DELEGATE` (a spawnable slot's static mask reaches the
/// console and the shell, but not a sibling). A linear chain needs only the
/// existing one-target-per-task delegation - each stage delegates to exactly
/// one successor. The shell waits on (and reaps) every program stage.
fn cmd_pipeline(stages: &[&str], cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize, env: &mut Env) {
    let head_cmd = stages[0].split_whitespace().next().unwrap_or("");
    let mut head_buf = [0u8; PATH_SIZE];
    let head_is_program = resolve_command(head_cmd, env, &mut head_buf).is_some();
    let builtin_head = !head_is_program;
    if builtin_head && !is_builtin(head_cmd) {
        print_str("unknown command: ");
        print_line(head_cmd);
        return;
    }

    // The stages spawned as a program chain (all but a builtin head).
    let prog_stages: &[&str] = if builtin_head { &stages[1..] } else { stages };
    if prog_stages.is_empty() {
        print_line("pipe: missing program after |");
        return;
    }

    // Spawn right-to-left: each stage's stdout is the next stage's slot, and
    // the last stage writes to the console.
    let mut slots = [0u64; MAX_STAGES];
    let mut next_target = syscall_abi::CON_TASK;
    for i in (0..prog_stages.len()).rev() {
        match spawn_stage(prog_stages[i], cwd, *cwd_len, env, next_target) {
            Ok(slot) => {
                slots[i] = slot;
                next_target = slot;
            }
            Err(code) => {
                for s in &slots[i + 1..prog_stages.len()] {
                    syscall(syscall_abi::KILL, *s);
                }
                let cmd = prog_stages[i].split_whitespace().next().unwrap_or("");
                match code {
                    0 => {
                        print_str("pipe: ");
                        print_str(cmd);
                        print_line(": not found (a pipeline stage must be a program)");
                    }
                    NO_FS => print_no_fs(),
                    _ => print_fs_error("pipe", code),
                }
                return;
            }
        }
    }

    // Authorize each adjacent producer->consumer link.
    for i in 0..prog_stages.len() - 1 {
        if syscall4(syscall_abi::DELEGATE, slots[i], slots[i + 1], 0, 0) != 0 {
            for s in &slots[..prog_stages.len()] {
                syscall(syscall_abi::KILL, *s);
            }
            print_line("pipe: could not authorize the stream");
            return;
        }
    }

    // A builtin head: capture its output and stream it to the first program
    // stage (the shell is the byte path only for this one hop). A program head
    // streams itself, directly to stage 2 via its delegated stdout.
    if builtin_head {
        let capture = get_heap();
        let mut out = Output::Capture { buf: capture, len: 0, overflowed: false };
        dispatch_line(stages[0], cwd, cwd_len, env, &mut out);
        let (buf, len, overflowed) = match out {
            Output::Capture { buf, len, overflowed } => (buf, len, overflowed),
            Output::Console => {
                for s in &slots[..prog_stages.len()] {
                    syscall(syscall_abi::KILL, *s);
                }
                return;
            }
        };
        if overflowed {
            print_line("pipe: left command's output exceeds the capture buffer - nothing was piped");
            for s in &slots[..prog_stages.len()] {
                syscall(syscall_abi::KILL, *s);
            }
            return;
        }
        let mut sent = 0usize;
        loop {
            let chunk_len = (len - sent).min(syscall_abi::MSG_MAX_LEN as usize);
            if !pipe_send(slots[0], unsafe { buf.as_ptr().add(sent) }, chunk_len as u64) {
                for s in &slots[..prog_stages.len()] {
                    syscall(syscall_abi::KILL, *s);
                }
                return;
            }
            if chunk_len == 0 {
                break; // the empty message was the end-of-stream marker
            }
            sent += chunk_len;
        }
    }

    // Reap every program stage (in order, so a producer that streamed and
    // exited is collected before we block on its consumer).
    for i in 0..prog_stages.len() {
        let cmd = prog_stages[i].split_whitespace().next().unwrap_or("stage");
        wait_pipe_stage(cmd, slots[i]);
    }
}

/// Wait for one stage of a program-to-program pipe and report how it ended.
fn wait_pipe_stage(label: &str, slot: u64) {
    match syscall(syscall_abi::WAIT, slot) {
        0 => {}
        WAIT_INTERRUPTED => {
            print_str("pipe: ");
            print_str(label);
            print_line(" wait interrupted (it may still be running - see ps)");
        }
        TASK_KILLED_STATUS => {
            print_str("pipe: ");
            print_str(label);
            print_line(" was killed");
        }
        code if code >= FS_ERR_MIN => {
            print_str("pipe: ");
            print_str(label);
            print_line(" did not exit cleanly");
        }
        status => {
            print_str("pipe: ");
            print_str(label);
            print_str(" exited with code ");
            print_u64(status);
            print_line("");
        }
    }
}

/// One pipeline send with full-mailbox retry: a program that reads
/// slowly just makes this loop until space opens up (the tick
/// preempts the shell, the program drains, the retry succeeds), but a
/// program that never reads at all would hang the shell forever - so
/// the retry is bounded by real ticks (~3 seconds), after which the
/// program is killed and the pipe reports why. Returns whether the
/// send succeeded.
fn pipe_send(slot: u64, ptr: *const u8, len: u64) -> bool {
    let deadline = syscall(syscall_abi::GET_TICKS, 0) + 150;
    loop {
        match syscall4(syscall_abi::MSG_SEND, slot, ptr as u64, len, 0) {
            0 => return true,
            syscall_abi::MSG_ERR_FULL => {
                if syscall(syscall_abi::GET_TICKS, 0) > deadline {
                    print_line("pipe: program stopped reading its input - killing it");
                    syscall(syscall_abi::KILL, slot);
                    return false;
                }
            }
            syscall_abi::TASK_ERR_NO_SUCH_TASK => {
                // The program exited (or crashed) before reading
                // everything - a legitimate early exit for a filter;
                // stop streaming and let the caller's wait sort out
                // (and report) how it ended.
                return true;
            }
            _ => {
                print_line("pipe: send failed");
                return false;
            }
        }
    }
}

/// What [`parse_redirect`] found on a submitted line.
enum RedirectParse<'a> {
    /// No `>`/`>>` token anywhere - dispatch the whole line as-is.
    NoRedirect,
    /// A well-formed trailing redirect: dispatch `cmd` with a capture
    /// sink, then write the capture to `target` (append on `>>`).
    Redirect { cmd: &'a str, target: &'a str, append: bool },
    /// An operator was present but the rest of the line was wrong;
    /// the message says how. Nothing is dispatched.
    Malformed(&'static str),
}

/// Scans the line's whitespace tokens for the first standalone `>` or
/// `>>` and splits the line there. The operator must be its own token -
/// `echo hi>f` is one token, not a redirect, the same no-quoting/
/// no-glued-forms limitation the tokenizer already has everywhere else
/// (documented in `docs/shell-commands.md`, not treated as a bug).
///
/// The offset arithmetic relies on `split_whitespace` yielding subslices
/// of `line` itself, so `token.as_ptr() - line.as_ptr()` is the token's
/// byte offset - guaranteed by `str::split_whitespace`'s contract, not
/// an implementation accident. Operator detection uses scalar
/// length/byte checks ([`is_dot`]/[`is_dotdot`] style) rather than
/// `token == ">"` - literal comparisons are safe now that the loader
/// relocates (see [`cmd_selftest`]), this just keeps the file on one
/// idiom.
fn parse_redirect(line: &str) -> RedirectParse<'_> {
    for token in line.split_whitespace() {
        let bytes = token.as_bytes();
        let is_overwrite = bytes.len() == 1 && bytes[0] == b'>';
        let is_append = bytes.len() == 2 && bytes[0] == b'>' && bytes[1] == b'>';
        if !is_overwrite && !is_append {
            continue;
        }

        let offset = token.as_ptr() as usize - line.as_ptr() as usize;
        // `.get()` rather than `&line[..offset]`: both offsets are
        // guaranteed char boundaries (the token is a whitespace-split
        // subslice of `line`), but indexing's can't-happen failure path
        // (`str::slice_error_fail`) formats the offending string with
        // enough of `core::fmt` to drag prebuilt non-PIC libcore
        // objects into the link, which fails outright under this
        // crate's PIE model (the same "release-only builds" constraint
        // documented in CLAUDE.md's relocating-loader section, hit here
        // even in release). The Option path pulls none of that in.
        let Some(cmd) = line.get(..offset) else { return RedirectParse::NoRedirect };
        let Some(rest) = line.get(offset + token.len()..) else { return RedirectParse::NoRedirect };
        let mut rest_words = rest.split_whitespace();
        let Some(target) = rest_words.next() else {
            return RedirectParse::Malformed("redirect: missing target file");
        };
        if rest_words.next().is_some() {
            return RedirectParse::Malformed("redirect: unexpected token after target file");
        }
        return RedirectParse::Redirect { cmd, target, append: is_append };
    }
    RedirectParse::NoRedirect
}

/// Writes `captured` (a command's redirected output) to `target` -
/// full replace for `>`, read-concatenate-rewrite for `>>`. The read
/// completes in full before the write starts, same
/// safe-by-construction ordering as `cp`. Like `sh`, the target is
/// still created/truncated even if the command itself printed nothing
/// (or only printed errors, which never enter the capture - see
/// [`Output`]); `> f` with no command at all legitimately creates an
/// empty file.
fn finish_redirect(captured: &[u8], overflowed: bool, target: &str, append: bool, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    if overflowed {
        print_line("redirect: output too large to capture - nothing written");
        return;
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), target, &mut path_buf) else {
        print_line("redirect: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("redirect: path too long");
        return;
    };

    let result = if append {
        // Append at the current end of file via the offset-write
        // primitive - no read-back of the existing content, so `>>` works
        // regardless of how large the target already is. Stat the size
        // first: a missing file is created (write from offset 0), an
        // existing one is appended to at its EOF.
        let mut probe = [0u8; 1];
        match fs_read_file(path, &mut probe) {
            // No such file: `>>` creates it, standard sh semantics.
            FS_ERR_NOT_FOUND => write_all(path, captured, 0, true),
            code if code >= FS_ERR_MIN => code, // NO_FS, is-a-directory, etc.
            size => write_all(path, captured, size, false),
        }
    } else {
        // `>`: full replace (truncate to empty, then write from offset 0).
        write_all(path, captured, 0, true)
    };
    match result {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("redirect", code),
        _ => {}
    }
}

/// Write `data` to `path` starting at `start_offset`, chunked at
/// `SAFECOPY_MAX` (a single `fs_write_*` can't exceed it, but a heap-backed
/// capture can be far larger). With `truncate`, the file is first replaced
/// with empty content (for `>` and for `>>` creating a new file), then
/// written from `start_offset`; without it (appending to an existing file)
/// the existing content is left in place. Returns `0` on success or the
/// first error code.
fn write_all(path: &str, data: &[u8], start_offset: u64, truncate: bool) -> u64 {
    if truncate {
        let r = fs_write_bulk(path, &[]); // create/truncate to empty
        if r >= FS_ERR_MIN {
            return r;
        }
    }
    let chunk = syscall_abi::SAFECOPY_MAX as usize;
    let mut off = 0usize;
    while off < data.len() {
        let n = (data.len() - off).min(chunk);
        let r = fs_write_at(path, start_offset + off as u64, &data[off..off + n]);
        if r >= FS_ERR_MIN {
            return r;
        }
        off += n;
    }
    0
}

/// Tokenizes on whitespace (no quoting - `echo "a b"` sees two words, not
/// one) and dispatches to a builtin by name. An empty line (just Enter,
/// or a line of only spaces) does nothing, same as a real shell. Command
/// *output* goes to `out` (so a redirect can capture it); command *error*
/// messages go straight to the console via [`print_line`] regardless -
/// see [`Output`]'s doc comment for the stdout/stderr reasoning.
fn dispatch_line(line: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize, env: &mut Env, out: &mut Output) {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else { return };
    let arg = words.next().unwrap_or("");

    match command {
        "help" => out.put_line("commands: help, echo, uptime, clear, ls, cat, cd, pwd, mkdir, rmdir, touch, rm, write, writeat, cp, mv, mount, unmount, erase, partition, format, ping, resolve, fetch, exec, exit, ps, kill, fg, wait, send, recv, selftest, env, set, unset (a bare unknown command is looked up on $PATH; $VAR expands; append `> file`/`>> file` to redirect, or `| /path/to/program` to pipe)"),
        // echo, uptime, clear are externalized: they're /bin programs now
        // (found via PATH by the unknown-command arm), not builtins. See
        // "Standalone binaries, Stage 4".
        // ping, resolve, and fetch are externalized to /bin now (Stage 4) -
        // spawned programs that reach netd via the TO_NET capability the shell
        // delegates at spawn (see run_path_command / delegate_net).
        "pwd" => out.put_line(cwd_str(cwd, *cwd_len)),
        // ls, cat, mkdir, rmdir, touch, rm, cp, mv, and writeat are externalized
        // to /bin now (Stage 4) - they run as spawned programs that inherit the
        // shell's cwd via GET_CWD. Only `write` stays builtin (its content is
        // the raw command line, bounded by the input buffer, so it never needs
        // argv or the bulk path).
        "cd" => cmd_cd(arg, cwd, cwd_len),
        "bind" => cmd_bind(line, cwd, *cwd_len, out),
        "write" => cmd_write(line, cwd, *cwd_len),
        "exec" => cmd_exec(line, cwd, *cwd_len, out),
        "exit" => cmd_exit(),
        "ps" => cmd_ps(out),
        "kill" => cmd_kill(arg),
        "fg" => cmd_fg(arg),
        "wait" => cmd_wait(arg),
        "mount" => cmd_mount(line, arg, cwd, *cwd_len, out),
        "cpu" => cmd_cpu(line, out),
        "unmount" => cmd_unmount(),
        "erase" => cmd_erase(arg),
        "partition" => cmd_partition(arg),
        "format" => cmd_format(arg),
        "send" => cmd_send(line),
        "recv" => cmd_recv(),
        "selftest" => cmd_selftest(out),
        "env" => cmd_env(env, out),
        "set" | "export" => cmd_set(arg, env),
        "unset" => cmd_unset(arg, env),
        _ => {
            // Not a builtin: try to run it as a program found on PATH
            // (`$PATH/<command>`). Only if that finds nothing is it "unknown".
            if !run_path_command(command, line, cwd, *cwd_len, env, out) {
                print_str("unknown command: ");
                print_line(command);
            }
        }
    }
}

fn cwd_str(cwd: &[u8; CWD_SIZE], cwd_len: usize) -> &str {
    core::str::from_utf8(&cwd[..cwd_len]).unwrap_or("/")
}

/// Resolves `component` against `cwd`, then normalizes (collapses `.`/
/// `..`), into `out`, returning the resolved length. An empty `component`
/// resolves to `cwd` itself (so `ls` with no argument lists the current
/// directory); a leading `/` is treated as already absolute.
///
/// **Deliberately no slice/array-literal equality comparisons** anywhere
/// in this function or [`normalize_path`] (e.g. `cwd_bytes != b"/"`, or
/// `component == ".."`) - a real, confirmed second instance of the same
/// class of bug `print_u64_decimal`'s doc comment already documents for
/// `core::fmt`. Comparing a `[u8]`/`&str` against a `b"..."`/`"..."`
/// literal crashed here (`ELR_EL1` inside this function, `FAR_EL1` a
/// small, build-layout-dependent address - the signature of a data
/// reference computed for this binary's link-time base of `0x0`, wrong
/// once loaded anywhere else), even though comparing individual `u8`
/// values never does. [`is_root`]/[`is_dot`]/[`is_dotdot`] use only
/// scalar (`len()`, indexed-byte) comparisons for exactly this reason.
///
/// **Normalization matters for more than cosmetics - found by testing,
/// not by inspection.** Without it, `cwd` accumulates a literal `..` for
/// every `cd ..` (`/EFI/BOOT` -> `/EFI/BOOT/..` -> `/EFI/BOOT/../..`,
/// unboundedly) rather than shrinking, which both looks wrong in `pwd`
/// and means every subsequent lookup re-walks the same already-visited
/// directories for no reason. Worse, it also means a `cd ..` that
/// resolves to a directory whose real on-disk `..` entry is cluster `0`
/// (the FAT32 convention for "this is root" - see `fat32.rs::Fs::find`'s
/// doc comment) always gets exercised on the *next* `cd ..` on top of an
/// already-unresolved one, doubly so. Collapsing here avoids re-walking
/// `..` more than once per real step.
fn resolve_path(cwd: &str, component: &str, out: &mut [u8; PATH_SIZE]) -> Option<usize> {
    let mut raw = [0u8; PATH_SIZE];
    let raw_len = concat_path(cwd, component, &mut raw)?;
    let raw_str = core::str::from_utf8(&raw[..raw_len]).ok()?;
    normalize_path(raw_str, out)
}

fn is_root(bytes: &[u8]) -> bool {
    bytes.len() == 1 && bytes[0] == b'/'
}

fn is_dot(s: &str) -> bool {
    s.len() == 1 && s.as_bytes()[0] == b'.'
}

fn is_dotdot(s: &str) -> bool {
    s.len() == 2 && s.as_bytes()[0] == b'.' && s.as_bytes()[1] == b'.'
}

/// The old `resolve_path` body, unchanged: plain string concatenation, no
/// `.`/`..` interpretation - [`normalize_path`] handles that afterward.
fn concat_path(cwd: &str, component: &str, out: &mut [u8; PATH_SIZE]) -> Option<usize> {
    if component.is_empty() {
        let bytes = cwd.as_bytes();
        if bytes.len() > out.len() {
            return None;
        }
        out[..bytes.len()].copy_from_slice(bytes);
        return Some(bytes.len());
    }
    if component.as_bytes()[0] == b'/' {
        let bytes = component.as_bytes();
        if bytes.len() > out.len() {
            return None;
        }
        out[..bytes.len()].copy_from_slice(bytes);
        return Some(bytes.len());
    }

    let mut len = 0;
    let cwd_bytes = cwd.as_bytes();
    if cwd_bytes.len() > out.len() {
        return None;
    }
    out[..cwd_bytes.len()].copy_from_slice(cwd_bytes);
    len += cwd_bytes.len();
    if !is_root(cwd_bytes) {
        if len >= out.len() {
            return None;
        }
        out[len] = b'/';
        len += 1;
    }
    let comp_bytes = component.as_bytes();
    if len + comp_bytes.len() > out.len() {
        return None;
    }
    out[len..len + comp_bytes.len()].copy_from_slice(comp_bytes);
    len += comp_bytes.len();
    Some(len)
}

/// Collapses `.` and `..` path components (`..` past the root is simply
/// dropped, same as a real shell - `cd ..` at `/` stays at `/`). At most
/// [`MAX_COMPONENTS`] path components deep; deeper paths fail rather than
/// silently truncating.
const MAX_COMPONENTS: usize = 16;

fn normalize_path(path: &str, out: &mut [u8; PATH_SIZE]) -> Option<usize> {
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

    let mut len = 1;
    out[0] = b'/';
    for (i, comp) in stack[..depth].iter().enumerate() {
        let bytes = comp.as_bytes();
        if i > 0 {
            if len >= out.len() {
                return None;
            }
            out[len] = b'/';
            len += 1;
        }
        if len + bytes.len() > out.len() {
            return None;
        }
        out[len..len + bytes.len()].copy_from_slice(bytes);
        len += bytes.len();
    }
    Some(len)
}

/// Printed by every disk command whenever the kernel reports [`NO_FS`] -
/// distinct from a command-specific "not found"/"failed" message, since
/// no mounted filesystem means *no* path could ever resolve this boot,
/// not that this particular one was wrong. Added after real user
/// confusion testing `make run` (vvfat's disk is FAT16, not FAT32 - see
/// `fat32.rs`): every disk command failed with a generic error that read
/// exactly like a broken path or a corrupt disk, and the actual cause
/// (no FAT32 partition found at boot) was only ever visible in the
/// kernel's own boot log, never in the shell itself.
fn print_no_fs() {
    print_line("no filesystem mounted this boot (see the kernel boot log - `make run`'s disk is FAT16, not FAT32; use `make run-image` for disk commands)");
}

/// Prints `"<cmd>: <one specific reason>"` for a kernel `FS_ERR_*` code -
/// the payoff of splitting the old single collapsed `FS_ERROR` sentinel:
/// no more guess-list error messages. Integer `match` plus `print_str`
/// of literals only - the two long-established relocation-safe patterns
/// (see [`resolve_path`]'s doc comment for the history; direct literal
/// comparisons are safe too since the relocating loader, this just stays
/// on one idiom).
fn print_fs_error(cmd: &str, code: u64) {
    print_str(cmd);
    print_str(": ");
    print_line(match code {
        syscall_abi::FS_ERR_NOT_FOUND => "no such file or directory",
        syscall_abi::FS_ERR_NOT_A_FILE => "is a directory",
        syscall_abi::FS_ERR_NOT_A_DIRECTORY => "not a directory",
        syscall_abi::FS_ERR_INVALID_NAME => "invalid name (must fit this kernel's 8.3 short-name subset)",
        syscall_abi::FS_ERR_ALREADY_EXISTS => "already exists",
        syscall_abi::FS_ERR_NOT_EMPTY => "directory not empty",
        syscall_abi::FS_ERR_IS_ROOT => "can't remove the root directory",
        syscall_abi::FS_ERR_DISK_FULL => "disk full",
        syscall_abi::FS_ERR_IO => "device I/O error",
        syscall_abi::MSG_ERR_FULL => "mailbox full",
        syscall_abi::MSG_ERR_TOO_BIG => "message too big (64-byte limit)",
        syscall_abi::MSG_ERR_DENIED => "permission denied (the IPC capability policy doesn't permit reaching that task)",
        syscall_abi::SPAWN_ERR_BAD_ELF => "not a loadable program (bad ELF)",
        syscall_abi::SPAWN_ERR_TOO_LARGE => "program too large for the kernel's staging buffer (or empty)",
        syscall_abi::SPAWN_ERR_NO_FREE_SLOT => "no free task slot",
        syscall_abi::TASK_ERR_NO_SUCH_TASK => "no such task (see ps)",
        syscall_abi::TASK_ERR_PROTECTED => "that task is protected (the boot shell, idle, and the filesystem server are permanent)",
        _ => "failed",
    });
}

// ls, cat, mkdir, rmdir, touch, rm, cp, mv, and writeat are externalized to
// `/bin` now (Stage 4) - their logic moved into per-command crates over `ulib`,
// resolving paths against the cwd delivered at spawn (GET_CWD). The shared
// helpers that survive here (fs_list_dir, resolve_path, fs_write_file/at) stay
// because `cd` and `write` still use them.

fn cmd_cd(arg: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize) {
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, *cwd_len), arg, &mut path_buf) else {
        print_line("cd: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("cd: path too long");
        return;
    };

    // No dedicated "does this directory exist" syscall - listing it (into
    // a throwaway buffer, contents unused) both validates the path and
    // confirms it's a directory, reusing fs_list_dir rather than adding a
    // syscall just for this.
    let mut scratch = [0u8; 8];
    match fs_list_dir(path, &mut scratch) {
        NO_FS => {
            print_no_fs();
            return;
        }
        code if code >= FS_ERR_MIN => {
            print_fs_error("cd", code);
            return;
        }
        _ => {}
    }
    if path_len > cwd.len() {
        print_line("cd: path too long");
        return;
    }
    cwd[..path_len].copy_from_slice(&path_buf[..path_len]);
    *cwd_len = path_len;
}

/// Drop one trailing `/` (except from root itself), so a `bind` target is
/// stored in the no-trailing-slash form `resolve_ns` expects.
fn strip_trailing_slash(p: &[u8]) -> &[u8] {
    if p.len() > 1 && p[p.len() - 1] == b'/' {
        &p[..p.len() - 1]
    } else {
        p
    }
}

/// `bind <newpath> <oldpath>` - map `newpath` in this shell's namespace onto the
/// existing subtree `oldpath` (both resolved against the cwd), so any path under
/// `newpath` resolves as if it were under `oldpath` (Plan 9 `bind`, cluster
/// Phase 0). **Per-task:** only this shell and the commands it spawns (which
/// inherit the namespace) see it. In Phase 0 every mount is tree 0, so `bind`
/// remaps within the one filesystem; multi-mount (a later step) will let
/// `newpath` point at a different disk. Appends one entry to this task's
/// namespace via `GET_NS`/`NS_SET`.
fn cmd_bind(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // skip "bind"
    let (Some(new_arg), Some(old_arg)) = (words.next(), words.next()) else {
        out.put_line("usage: bind <newpath> <oldpath>");
        return;
    };
    let mut newp = [0u8; PATH_SIZE];
    let mut oldp = [0u8; PATH_SIZE];
    let (Some(nl), Some(ol)) = (
        resolve_path(cwd_str(cwd, cwd_len), new_arg, &mut newp),
        resolve_path(cwd_str(cwd, cwd_len), old_arg, &mut oldp),
    ) else {
        out.put_line("bind: path too long");
        return;
    };
    let prefix = strip_trailing_slash(&newp[..nl]);
    let target = strip_trailing_slash(&oldp[..ol]);
    // The new path must be a real absolute subpath, not root itself (root is the
    // implicit identity default and can't be usefully rebound in Phase 0).
    if prefix.len() < 2 || prefix[0] != b'/' {
        out.put_line("bind: <newpath> must be a path below /");
        return;
    }
    // Bind within the current mount: tree 0, target = the existing subtree.
    if !ns_add(prefix, target, 0) {
        out.put_line("bind: path too long or namespace full");
    }
}

/// Loads and starts `arg` as a new, independent task alongside whatever is
/// already running - a real `tasks::spawn`, not a POSIX exec-replaces-
/// current-process. The new task keeps running after this command returns;
/// there's no way yet to wait for it, stop it, or free its memory once
/// started (see `tasks.rs`'s module doc comment).
fn cmd_exec(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    // argv = the tokens after "exec": [program path, args...]. argv[0] is both
    // the path to load and the program's own argv[0].
    let mut argv_buf: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut n = 0;
    for w in line.split_whitespace().skip(1) {
        if n >= MAX_ARGS {
            break;
        }
        argv_buf[n] = w;
        n += 1;
    }
    if n == 0 {
        print_line("exec: missing program argument");
        return;
    }
    let argv = &argv_buf[..n];
    let path = argv[0];
    if out.is_console() {
        // Plain `exec prog [args]`: fire-and-forget, output straight to the
        // console.
        match spawn_path(path, argv, cwd, cwd_len, syscall_abi::CON_TASK) {
            Ok(_slot) => {}
            Err(0) => print_line("exec: path too long"),
            Err(NO_FS) => print_no_fs(),
            Err(code) => print_fs_error("exec", code),
        }
        return;
    }
    // `exec prog [args] > file`: route the program's output back to this shell
    // and capture it into the redirect sink, then wait for the program - the
    // caller (`run_line`) writes the capture to the file (`finish_redirect`).
    match spawn_path(path, argv, cwd, cwd_len, self_task()) {
        Ok(slot) => capture_program_output(slot, out),
        Err(0) => print_line("exec: path too long"),
        Err(NO_FS) => print_no_fs(),
        Err(code) => print_fs_error("exec", code),
    }
}

/// `env`: list every variable as `NAME=VALUE`.
fn cmd_env(env: &Env, out: &mut Output) {
    for i in 0..env.count {
        for &b in &env.names[i][..env.name_lens[i]] {
            out.put(b);
        }
        out.put(b'=');
        for &b in &env.vals[i][..env.val_lens[i]] {
            out.put(b);
        }
        out.put(CR);
        out.put(LF);
    }
}

/// `set NAME=VALUE` / `export NAME=VALUE`: set (or replace) a variable.
/// Value is a single token (no quoting - `set X=a b` sets `X=a`), same as
/// `echo`'s limitation. Not exported into child programs (shell-local).
fn cmd_set(arg: &str, env: &mut Env) {
    let Some((name, value)) = arg.split_once('=') else {
        print_line("usage: set NAME=VALUE");
        return;
    };
    if name.is_empty() {
        print_line("set: empty variable name");
        return;
    }
    if !env.set(name, value.as_bytes()) {
        print_line("set: name/value too long, or too many variables");
    }
}

/// `unset NAME`: remove a variable (silent if it wasn't set).
fn cmd_unset(arg: &str, env: &mut Env) {
    if arg.is_empty() {
        print_line("usage: unset NAME");
        return;
    }
    env.unset(arg.as_bytes());
}

/// Try to run an unknown command as a program found on `$PATH`:
/// for each `:`-separated directory, probe `<dir>/<command>` and, on the
/// first hit, spawn it with the whole line as its argv and run it in the
/// **foreground** - wait for it (which also reaps its slot, unlike `exec`'s
/// fire-and-forget). Returns `false` if no PATH directory has the command
/// (the caller then prints "unknown command"). A `>`/`>>` redirect routes
/// Delegate this shell's `TO_NET` send-capability to a freshly spawned command
/// (`slot`), so a network command (`ping`, and later `resolve`/`fetch`) can
/// reach the network server - a spawnable slot doesn't hold `TO_NET` statically
/// (`tasks.rs::caps_for_slot` gives it `TO_SHELL | TO_FSD | TO_CON`). The shell
/// holds it, so it alone can grant it, the same `DELEGATE` mechanism the
/// program-to-program pipe uses. Best-effort: granting it to every foreground
/// command is harmless (a command that never calls netd never uses it, and the
/// delegation is cleared when the task dies); a command with no network to
/// reach just reports its own "no network server" path if the grant somehow
/// failed. The cost of not knowing, from the shell, which `/bin` program needs
/// the network.
fn delegate_net(slot: u64) {
    let _ = syscall4(syscall_abi::DELEGATE, slot, syscall_abi::NET_TASK, 0, 0);
}

/// the program's output into the capture sink, exactly as `cmd_exec` does.
fn run_path_command(command: &str, line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, env: &Env, out: &mut Output) -> bool {
    // argv = every token on the line (argv[0] = the command as typed).
    let mut argv_buf: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut n = 0;
    for w in line.split_whitespace() {
        if n >= MAX_ARGS {
            break;
        }
        argv_buf[n] = w;
        n += 1;
    }
    let argv = &argv_buf[..n];

    // Search directories from the PATH env var (falling back to the default
    // if unset or not valid UTF-8).
    let path = env
        .get(b"PATH")
        .and_then(|v| core::str::from_utf8(v).ok())
        .unwrap_or(DEFAULT_PATH);
    for dir in path.split(':') {
        // Build "<dir>/<command>" into a fixed buffer (no allocation).
        let mut cand = [0u8; PATH_SIZE];
        let mut c = 0;
        for &b in dir.as_bytes() {
            if c < PATH_SIZE {
                cand[c] = b;
                c += 1;
            }
        }
        if (c == 0 || cand[c - 1] != b'/') && c < PATH_SIZE {
            cand[c] = b'/';
            c += 1;
        }
        for &b in command.as_bytes() {
            if c < PATH_SIZE {
                cand[c] = b;
                c += 1;
            }
        }
        let Ok(candidate) = core::str::from_utf8(&cand[..c]) else {
            continue;
        };

        // Probe: does this candidate exist as a file? (A one-byte read - the
        // pattern cmd_cp uses; a real size, including 0, means it's there.)
        let mut probe = [0u8; 1];
        let r = fs_read_file(candidate, &mut probe);
        if r == NO_FS {
            return false; // no filesystem this boot - nothing to find
        }
        if r >= FS_ERR_MIN {
            continue; // not in this directory - try the next
        }

        // Found it. Run it - foreground (console) or captured (redirect).
        if out.is_console() {
            match spawn_path(candidate, argv, cwd, cwd_len, syscall_abi::CON_TASK) {
                Ok(slot) => {
                    delegate_net(slot);
                    // Foreground: wait for it (also reaps the slot). Ctrl+C
                    // interrupts the wait and leaves it running in the
                    // background (see `ps`).
                    if syscall(syscall_abi::WAIT, slot) == WAIT_INTERRUPTED {
                        print_line("interrupted (the program keeps running - see ps)");
                    }
                }
                Err(0) => print_line("command path too long"),
                Err(NO_FS) => print_no_fs(),
                Err(code) => print_fs_error(command, code),
            }
        } else {
            match spawn_path(candidate, argv, cwd, cwd_len, self_task()) {
                Ok(slot) => {
                    delegate_net(slot);
                    capture_program_output(slot, out);
                }
                Err(0) => print_line("command path too long"),
                Err(NO_FS) => print_no_fs(),
                Err(code) => print_fs_error(command, code),
            }
        }
        return true;
    }
    false
}

/// Relay-capture a program's output (routed back to this shell as a raw
/// byte stream) into `out`, until its empty end-of-stream message, then
/// reap it. On Ctrl+C or a relay error the program is killed and the
/// capture is marked overflowed so the caller's `finish_redirect` refuses
/// to write a partial file.
fn capture_program_output(slot: u64, out: &mut Output) {
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed == RECV_INTERRUPTED || packed >= FS_ERR_MIN {
            print_line(if packed == RECV_INTERRUPTED {
                "exec: interrupted - killing the program, nothing written"
            } else {
                "exec: capture failed, nothing written"
            });
            syscall(syscall_abi::KILL, slot);
            if let Output::Capture { overflowed, .. } = out {
                *overflowed = true;
            }
            return;
        }
        let len = (packed & 0xffff_ffff) as usize;
        if len == 0 {
            break; // end of stream
        }
        for &b in &buf[..len] {
            out.put(b);
        }
    }
    let _ = syscall(syscall_abi::WAIT, slot);
}

/// Resolves `arg` against the cwd and runs the two-step spawn flow
/// (the kernel has no filesystem to read a path with: read the program
/// via the filesystem server in 512-byte chunks - the per-buffer cap -
/// feed each into the kernel's staging buffer via SPAWN_STAGE, then
/// SPAWN the staged total, which returns the new task's slot index).
/// Shared by `exec` and the pipeline flow (`left | program`), which
/// needs the slot to stream input to and wait on. `Err(0)` means the
/// path didn't resolve/encode (too long); any other `Err` is a real
/// code for [`print_fs_error`] (or [`NO_FS`]).
/// Encode `argv` into the `ARGS_STAGE` blob format (`[argc: u32 LE]` then
/// `[len: u32 LE][bytes]` per arg) and stage it in the kernel for the next
/// `SPAWN` to attach. Returns the blob length (SPAWN's arg2), or `Err(())`
/// if it doesn't fit `ARGV_MAX` (unreachable with a 128-byte input line).
fn stage_argv(argv: &[&str]) -> Result<u64, ()> {
    const CAP: usize = syscall_abi::ARGV_MAX as usize;
    let mut blob = [0u8; CAP];
    blob[0..4].copy_from_slice(&(argv.len() as u32).to_le_bytes());
    let mut off = 4usize;
    for a in argv {
        let bytes = a.as_bytes();
        if off + 4 + bytes.len() > CAP {
            return Err(());
        }
        blob[off..off + 4].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
        off += 4;
        blob[off..off + bytes.len()].copy_from_slice(bytes);
        off += bytes.len();
    }
    if syscall4(syscall_abi::ARGS_STAGE, blob.as_ptr() as u64, off as u64, 0, 0) != 0 {
        return Err(());
    }
    Ok(off as u64)
}

fn spawn_path(path: &str, argv: &[&str], cwd: &[u8; CWD_SIZE], cwd_len: usize, stdout_target: u64) -> Result<u64, u64> {
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), path, &mut path_buf) else {
        return Err(0);
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        return Err(0);
    };

    // A short read (or 0 for an empty file) ends the chunk loop.
    let mut offset: u64 = 0;
    let mut chunk = [0u8; 512];
    loop {
        let n = fs_read_at(path, offset, &mut chunk);
        if n >= FS_ERR_MIN {
            return Err(n);
        }
        if n == 0 {
            break;
        }
        if syscall4(syscall_abi::SPAWN_STAGE, offset, chunk.as_ptr() as u64, n, 0) != 0 {
            // Only reachable by staging past the kernel's 128KB buffer
            // - the same too-large refusal SPAWN itself would give.
            return Err(syscall_abi::SPAWN_ERR_TOO_LARGE);
        }
        offset += n;
        if n < chunk.len() as u64 {
            break;
        }
    }
    // arg1 is the spawned program's stdout target: CON_TASK for a plain
    // `exec` (output straight to the console), or the shell's own task
    // index for a pipe/redirect producer (so its output routes back here to
    // be relayed/captured). Passed explicitly (not via the 1-arg `syscall`
    // helper, which would leave x1 as 0 - task 0 - and misroute the spawn).
    // Stage the argv blob (attached to this SPAWN via its arg2 = blob length;
    // the child reads it via GET_ARGC/GET_ARG). argv[0] is the program name.
    let argv_len = match stage_argv(argv) {
        Ok(n) => n,
        Err(()) => return Err(0),
    };
    // Stage the cwd (arg3 = its length; the child reads it via GET_CWD), so a
    // spawned command inherits this shell's working directory and can resolve
    // relative paths / default to it.
    let cwd_stage_len = if cwd_len > 0 {
        if syscall4(syscall_abi::CWD_STAGE, cwd.as_ptr() as u64, cwd_len as u64, 0, 0) != 0 {
            return Err(0);
        }
        cwd_len as u64
    } else {
        0
    };
    match syscall4(syscall_abi::SPAWN, offset, stdout_target, argv_len, cwd_stage_len) {
        code if code >= FS_ERR_MIN => Err(code),
        slot => Ok(slot),
    }
}

/// This shell's own task slot index (the boot shell is task 0, but a
/// foreground-spawned shell is a higher slot) - needed as a pipe
/// producer's stdout target so its output routes back here. See `SELF`.
fn self_task() -> u64 {
    syscall(syscall_abi::SELF, 0)
}

/// `write <file> <words...>` - joins every word after the filename with a
/// single space (same join style as `echo`) and writes the result as the
/// file's *entire* contents, replacing whatever was there. Takes `line`
/// (not just the first argument, unlike every other command here) because
/// it needs both the filename and the rest of the line as separate
/// pieces - `run_line` already tokenized `arg` down to one word, which
/// isn't enough here.
fn cmd_write(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    let mut words = line.split_whitespace();
    words.next(); // "write" itself
    let Some(filename) = words.next() else {
        print_line("write: missing file argument");
        return;
    };

    let mut content = [0u8; BUFFER_SIZE];
    let mut len = 0usize;
    let mut first = true;
    for word in words {
        if !first && len < content.len() {
            content[len] = b' ';
            len += 1;
        }
        for b in word.bytes() {
            if len < content.len() {
                content[len] = b;
                len += 1;
            }
        }
        first = false;
    }

    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), filename, &mut path_buf) else {
        print_line("write: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("write: path too long");
        return;
    };

    // `write`'s content is bounded by the shell's input line
    // (BUFFER_SIZE, 128) - always well under the inline 512-byte cap -
    // so it stays on the cheap inline path rather than paying a GRANT
    // per write. cp/redirect, which genuinely exceed 512, use the bulk
    // path (fs_write_bulk).
    match fs_write_file(path, &content[..len]) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("write", code),
        _ => {}
    }
}

/// Where a command's *output* goes: the console (the only choice before
/// output redirection existed) or a capture buffer that a `>`/`>>`
/// redirect writes to a file once the command returns. Passed down to
/// command handlers by `&mut` reference, exactly like `cwd` already is -
/// a module-level "current sink" static is impossible here, since this
/// program is deliberately built with no static mutable state at all
/// (`linker.ld` asserts `.data`/`.bss` empty - see the module doc
/// comment).
///
/// Only real output goes through this; **error messages deliberately
/// don't** - they keep printing straight to the console via
/// [`print_line`], the POSIX stdout/stderr split (a redirect takes
/// stdout, errors stay visible on the terminal) without needing a second
/// sink to represent stderr.
enum Output<'a> {
    Console,
    /// Captured for a pending redirect or pipe. `buf` is the program's
    /// heap region (see `get_heap` - 256KB, far larger than the stack), so
    /// a large capture like `cat big > file` fits. `len` counts stored
    /// bytes; once the buffer is full, further bytes are discarded (not
    /// wrapped) and `overflowed` is set so the redirect can refuse to
    /// write a silently-incomplete file - `cp`'s "a partial copy is a
    /// wrong copy" reasoning, not `cat`'s truncate-and-note.
    Capture { buf: &'a mut [u8], len: usize, overflowed: bool },
}

impl Output<'_> {
    fn put(&mut self, byte: u8) {
        match self {
            Output::Console => putc(byte),
            Output::Capture { buf, len, overflowed } => {
                if *len < buf.len() {
                    buf[*len] = byte;
                    *len += 1;
                } else {
                    *overflowed = true;
                }
            }
        }
    }

    fn put_str(&mut self, s: &str) {
        for b in s.bytes() {
            self.put(b);
        }
    }

    fn put_line(&mut self, s: &str) {
        self.put_str(s);
        self.put(CR);
        self.put(LF);
    }

    /// Hand-rolled decimal formatting. Historical note, kept because it
    /// explains why [`cmd_selftest`] exists and why this still
    /// hand-rolls rather than switching to `write!` itself: under the
    /// *old*, non-relocating flat-binary loader, `write!`/
    /// `core::fmt::Arguments` crashed here. That machinery builds its
    /// per-argument dispatch out of *data* (an array of function
    /// pointers, one per formatted argument) rather than direct `bl`
    /// calls - a binary linked for base `0x0` but loaded somewhere else
    /// (always, in practice) had no way to know those embedded pointer
    /// values needed correcting, so they pointed at whatever link-time
    /// address `0x0` would have meant (`ELR_EL1` landing on a tiny
    /// near-null address instead of real code, confirmed directly by
    /// trying `write!` here first). **This is now fixed**, since
    /// `loader.rs` processes real `R_AARCH64_RELATIVE` relocations
    /// against the actual runtime load address (see CLAUDE.md's
    /// "relocating loader" milestone and [`cmd_selftest`], which proves
    /// it) - but this is left as hand-rolled decimal formatting anyway,
    /// simply because it was already written, works, and doesn't need
    /// `core::fmt`'s machinery to do something this simple.
    fn put_u64_decimal(&mut self, mut n: u64) {
        if n == 0 {
            self.put(b'0');
            return;
        }
        let mut digits = [0u8; 20]; // u64::MAX has 20 decimal digits
        let mut count = 0;
        while n > 0 {
            digits[count] = b'0' + (n % 10) as u8;
            n /= 10;
            count += 1;
        }
        while count > 0 {
            count -= 1;
            self.put(digits[count]);
        }
    }

    /// `cat` uses this to keep its display-only trailing-newline nicety
    /// off the captured byte stream, so `cat a > b` copies `a`'s bytes
    /// exactly (see [`cmd_cat`]).
    fn is_console(&self) -> bool {
        matches!(self, Output::Console)
    }
}

fn print_str(s: &str) {
    con_write(s.as_bytes());
}

fn print_line(s: &str) {
    print_str(s);
    putc(CR);
    putc(LF);
}

/// A `core::fmt::Write` target over an [`Output`] sink - lets
/// [`cmd_selftest`] use real `write!`/`format_args!` without pulling in
/// `alloc`, and lets its output participate in redirection like any
/// other command's.
struct Writer<'a, 'b>(&'a mut Output<'b>);

impl core::fmt::Write for Writer<'_, '_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.put_str(s);
        Ok(())
    }
}

/// Exercises the exact two patterns that used to crash under the old,
/// non-relocating flat-binary loader (see [`print_u64_decimal`]'s doc
/// comment and [`resolve_path`]'s) - `write!`/`core::fmt::Write`, and a
/// slice/string comparison against a literal - and confirms both now
/// produce correct output. Kept as a real, permanent regression check
/// rather than a throwaway test: this is the actual acceptance criterion
/// for the whole relocating-loader milestone (CLAUDE.md) - these patterns
/// need to be ordinary, safe Rust again, not something a program author
/// has to keep avoiding by hand-discipline.
fn cmd_selftest(out: &mut Output) {
    let mut w = Writer(out);

    // write!/core::fmt: n is computed at runtime (not a compile-time
    // constant folded away), so this genuinely exercises the formatting
    // machinery's argument dispatch, not just a literal string.
    let n = 6 * 7;
    let _ = write!(w, "write!/core::fmt: {n} (expect 42)\r\n");

    // Slice-vs-literal comparison: `probe` is a real runtime value (not
    // itself a literal), compared against a b"..." literal with `==` -
    // the exact shape that crashed as `cwd_bytes != b"/"` in cmd_cd's old
    // path-resolution code (see resolve_path's doc comment).
    let probe: [u8; 1] = *b"/";
    let slice_ok = probe.as_slice() == b"/";
    let _ = write!(w, "slice-vs-literal comparison: {slice_ok} (expect true)\r\n");

    // &str-vs-literal comparison: same shape as the old `component == ".."`
    // crash - `word` is built at runtime, not itself a literal.
    let word_bytes = *b"hi";
    let word = core::str::from_utf8(&word_bytes).unwrap_or("");
    let str_ok = word == "hi";
    let _ = write!(w, "str-vs-literal comparison: {str_ok} (expect true)\r\n");
}

/// One filesystem-server round trip, v2 protocol (fully self-contained - see
/// the protocol section in `syscall-abi`): builds a request from a header (op +
/// four params) plus up to two inline payload chunks (path, then data for the
/// ops that carry it), `MSG_CALL`s it to the server's fixed slot
/// ([`syscall_abi::FSD_TASK`]), and unpacks the reply - a status u64 carrying
/// exactly the old fs_* syscalls' return-value semantics, plus an inline result
/// payload copied out into `result`. No pointer of this task's ever crosses to
/// the server; the kernel's message machinery moves the bytes both ways, which
/// is what makes the protocol work under per-task page tables. The two
/// call-layer failures fold into the same status space: no server this boot
/// becomes [`NO_FS`] - literally true - and anything else (Ctrl+C mid-call, a
/// full server mailbox) becomes the generic [`FS_ERROR`].
fn fs_call(op: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
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
        return NO_FS;
    }
    if packed >= FS_ERR_MIN {
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
/// the Phase 0 cluster protocol) - [`fs_call`]'s sibling with the `tree` mount
/// selector at offset 8 and the payload at [`ninep_abi::NP_REQ_PAYLOAD`] (48).
/// `tree` is `0` for now (a single implicit mount); the per-task namespace
/// resolves it to a real mount in a later step. The reply shape (status u64 +
/// inline result) is identical to [`fs_call`]'s, so the wrappers below and
/// their callers are unchanged. The shell's *admin* ops (mount/format/…) keep
/// using [`fs_call`]/`FSOP_*` - they are `fsd`-specific control, not file verbs.
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
        return NO_FS;
    }
    if packed >= FS_ERR_MIN {
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

/// Lists `path`'s directory entries into `buf` as `name\n`/`name/\n` -
/// same format and truncation behavior as ever (the server implements
/// the old kernel handler verbatim, now into its own reply payload).
/// Returns a byte count on success, [`NO_FS`], or a specific
/// `FS_ERR_*` code (anything `>= FS_ERR_MIN`) - callers match on this
/// directly (see [`cmd_ls`] and [`print_fs_error`] - so every failure
/// reason gets its own accurate message). Every wrapper below is one
/// [`fs_call`] round trip to the filesystem server: the shell's "libc
/// layer" over IPC; the contracts are unchanged.
/// Largest resolved filesystem path (a `bind` target can be longer than the
/// prefix it replaces).
const FSP_MAX: usize = 256;

/// Read the shell's own namespace (set by `bind` via `NS_SET`, inherited by the
/// commands it spawns) into `buf`; returns its length (0 = none). See
/// [`resolve_ns`]. The shell reads its own namespace via `GET_NS` exactly as
/// `/bin` programs do (via `ulib`), so `cd`/`write` into a bound path resolve
/// the same way an `ls`/`cat` of it does.
fn get_ns(buf: &mut [u8]) -> usize {
    let n = syscall4(syscall_abi::GET_NS, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
    (n as usize).min(buf.len())
}

/// A resolved destination for a client path: which server task services it, the
/// mount selector there, and (for a remote mount) the endpoint. A duplicate of
/// `ulib::Resolved` - the shell keeps its own fs layer.
struct Resolved {
    server: u64,
    tree: u64,
    endpoint: [u8; ninep_abi::NS_ENDPOINT_LEN],
    len: usize,
}

/// Resolve an absolute `path` through the namespace `ns`: longest
/// component-aligned prefix `bind` wins, its target replacing the prefix. A
/// binding's `tree` selects a local mount, except the sentinel
/// [`ninep_abi::NS_REMOTE_TREE`] (`0xFF`), whose target is `[ip:4][port:2]` +
/// remote root and resolves to [`syscall_abi::NET_TASK`] (a remote mount over
/// TCP - cluster Phase 1c). No match is identity to the local boot mount. A
/// duplicate of `ulib::resolve_ns`; scalar-only, relocation-safe.
fn resolve_ns(ns: &[u8], path: &str, out: &mut [u8]) -> Resolved {
    // Shared resolution logic (`ninep_abi::resolve_ns` - one source of truth for
    // ulib, the shell, and netd's export); map its `NsTarget` to server/tree/ep.
    let r = ninep_abi::resolve_ns(ns, path.as_bytes(), out);
    let zero = [0u8; ninep_abi::NS_ENDPOINT_LEN];
    match r.target {
        ninep_abi::NsTarget::Fsd(tree) => Resolved { server: syscall_abi::FSD_TASK, tree: tree as u64, endpoint: zero, len: r.len },
        ninep_abi::NsTarget::Console => Resolved { server: syscall_abi::CON_TASK, tree: 0, endpoint: zero, len: r.len },
        ninep_abi::NsTarget::NetLocal => Resolved { server: syscall_abi::NET_TASK, tree: 0, endpoint: zero, len: r.len },
        ninep_abi::NsTarget::Remote(ep) => Resolved { server: syscall_abi::NET_TASK, tree: 0, endpoint: ep, len: r.len },
    }
}

/// Resolve `path` through the shell's namespace.
fn mount_resolve(path: &str, out: &mut [u8]) -> Resolved {
    let mut ns = [0u8; syscall_abi::NS_MAX as usize];
    let nlen = get_ns(&mut ns);
    resolve_ns(&ns[..nlen], path, out)
}

/// Route a verb to its resolved destination - local `fsd` ([`np_call`]) or a
/// remote mount over TCP via `netd` ([`np_remote`]). A duplicate of
/// `ulib::np_dispatch`.
fn np_dispatch(r: &Resolved, verb: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    if r.server == syscall_abi::CON_TASK {
        syscall_abi::FS_ERROR // the console is write-only (writes con_write below)
    } else if is_local_net(r) {
        np_netlocal(verb, params, payload1, result)
    } else if r.server == syscall_abi::NET_TASK {
        np_remote(&r.endpoint, verb, params, payload1, payload2, result)
    } else {
        np_call(verb, r.tree, params, payload1, payload2, result)
    }
}

/// The local `/net` netd-fs (cluster Phase 3): `NET_TASK` with a zero endpoint (a
/// remote mount always has a real endpoint). A duplicate of `ulib::is_local_net`.
fn is_local_net(r: &Resolved) -> bool {
    r.server == syscall_abi::NET_TASK && r.endpoint == [0u8; ninep_abi::NS_ENDPOINT_LEN]
}

/// A direct NP read to `NET_TASK` for the local `/net` filesystem - like
/// `np_call` but addressed to `netd` (read-only, inline data). A duplicate of
/// `ulib::np_netlocal`.
fn np_netlocal(verb: u64, params: [u64; 4], payload1: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&verb.to_le_bytes());
    let mut i = 0;
    while i < 4 {
        let at = 16 + i * 8;
        req[at..at + 8].copy_from_slice(&params[i].to_le_bytes());
        i += 1;
    }
    let end = HDR + payload1.len();
    if end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[HDR..end].copy_from_slice(payload1);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::NET_TASK,
        req.as_ptr() as u64,
        end as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed == syscall_abi::TASK_ERR_NO_SUCH_TASK {
        return NO_FS;
    }
    if packed >= FS_ERR_MIN {
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

/// One remote verb round trip via `netd`'s `NETOP_RMOUNT`. A duplicate of
/// `ulib::np_remote` (the shell keeps its own fs layer). The embedded NP
/// request's `tree` is `0` (the remote export serves its own boot mount).
fn np_remote(endpoint: &[u8; ninep_abi::NS_ENDPOINT_LEN], verb: u64, params: [u64; 4], payload1: &[u8], payload2: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let base = syscall_abi::NETOP_RMOUNT_MSG;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&syscall_abi::NETOP_RMOUNT.to_le_bytes());
    req[syscall_abi::NETOP_RMOUNT_ENDPOINT..syscall_abi::NETOP_RMOUNT_ENDPOINT + ninep_abi::NS_ENDPOINT_LEN]
        .copy_from_slice(&endpoint[..]);
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
        return NO_FS;
    }
    if packed >= FS_ERR_MIN {
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

/// Append one binding `[tree][prefix_len][target_len][prefix][target]` to this
/// shell's namespace (read via `GET_NS`, written back via `NS_SET`). Returns
/// `false` if a path is too long or the namespace is full. Shared by `bind`
/// (tree 0, target = an existing subtree) and `mount` (a fresh tree from
/// `FSOP_MOUNT_AT`, target = `/` - the mounted volume's root).
fn ns_add(prefix: &[u8], target: &[u8], tree: u8) -> bool {
    if prefix.len() > 255 || target.len() > 255 {
        return false;
    }
    let mut ns = [0u8; syscall_abi::NS_MAX as usize];
    let nlen = get_ns(&mut ns);
    let entry = 3 + prefix.len() + target.len();
    if nlen + entry > ns.len() {
        return false;
    }
    ns[nlen] = tree;
    ns[nlen + 1] = prefix.len() as u8;
    ns[nlen + 2] = target.len() as u8;
    ns[nlen + 3..nlen + 3 + prefix.len()].copy_from_slice(prefix);
    ns[nlen + 3 + prefix.len()..nlen + entry].copy_from_slice(target);
    syscall4(syscall_abi::NS_SET, ns.as_ptr() as u64, (nlen + entry) as u64, 0, 0) == 0
}

fn fs_list_dir(path: &str, buf: &mut [u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
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

/// Reads `path`'s contents into `buf`. Returns the file's *real* size
/// on success (which may exceed `buf.len()` - compare to detect
/// truncation), [`NO_FS`], or a specific `FS_ERR_*` code - same
/// contract as ever, same reasoning as [`fs_list_dir`].
fn fs_read_file(path: &str, buf: &mut [u8]) -> u64 {
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

/// Reads up to `buf.len()` bytes of `path` starting at byte `offset`,
/// returning how many were copied (`0` at/past end of file) - the
/// chunked-read primitive [`cmd_exec`]'s two-step spawn flow loops
/// over. Same error space as [`fs_read_file`].
fn fs_read_at(path: &str, offset: u64, buf: &mut [u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    let want = if r.server == syscall_abi::NET_TASK {
        buf.len().min(ninep_abi::NP_REMOTE_CHUNK) as u64
    } else {
        buf.len() as u64
    };
    np_dispatch(
        &r,
        ninep_abi::NP_READ_AT,
        [r.len as u64, offset, want, 0],
        &fsp[..r.len],
        &[],
        buf,
    )
}


/// Creates or fully overwrites `path` with `data` via the grant/safecopy
/// bulk path (rather than inline in the request, which [`fs_write_file`]'s
/// 512-byte cap bounds): grants `data` to the filesystem server as a
/// `GRANT_READ` buffer - the server `SAFECOPY`s it out during the call -
/// then issues the bulk write. Returns `0` on success, [`NO_FS`], or a
/// specific `FS_ERR_*` code. `data.len()` must be `<= SAFECOPY_MAX`.
/// Zero-length `data` is valid (truncate-to-empty) and skips the grant.
fn fs_write_bulk(path: &str, data: &[u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    if r.server == syscall_abi::CON_TASK {
        con_write(data); // /dev/cons
        return 0;
    }
    if is_local_net(&r) {
        return syscall_abi::FS_ERROR; // /net is read-only
    }
    if r.server == syscall_abi::NET_TASK {
        // Remote full overwrite: truncate-and-write the first chunk (NP_WRITE),
        // then stream the rest (NP_WRITE_AT). See ulib::fs_write_bulk.
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

/// Writes `data` at byte `offset` in `path`, extending the file without
/// rewriting the bytes before `offset` (the FAT32 offset-write
/// primitive), via the grant/safecopy bulk path (`GRANT_READ`). Returns
/// `0`, [`NO_FS`], or an `FS_ERR_*` code. `data.len()` must be
/// `<= SAFECOPY_MAX`. Loop with a rising `offset` to write a file of any
/// size one chunk at a time - see [`cmd_cp`]. Empty `data` is a no-op.
fn fs_write_at(path: &str, offset: u64, data: &[u8]) -> u64 {
    if data.is_empty() {
        return 0;
    }
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    if r.server == syscall_abi::CON_TASK {
        con_write(data); // /dev/cons (offset ignored - the console is a stream)
        return 0;
    }
    if is_local_net(&r) {
        return syscall_abi::FS_ERROR; // /net is read-only
    }
    if r.server == syscall_abi::NET_TASK {
        // Remote: chunk to the inline cap, one NP_WRITE_AT per <=NP_REMOTE_CHUNK
        // at rising offsets. See ulib::fs_write_at.
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

/// The status contract every path-op fs helper shares: `0` on success,
/// [`NO_FS`] if no filesystem is mounted, or a specific `FS_ERR_*` code for the
/// real failure reason (already exists, invalid 8.3 name, parent missing, disk
/// full, ... - see [`print_fs_error`]).
///
/// Creates or fully overwrites the file at `path` with `data`.
fn fs_write_file(path: &str, data: &[u8]) -> u64 {
    let mut fsp = [0u8; FSP_MAX];
    let r = mount_resolve(path, &mut fsp);
    if r.server == syscall_abi::CON_TASK {
        con_write(data); // /dev/cons
        return 0;
    }
    np_dispatch(
        &r,
        ninep_abi::NP_WRITE_FILE,
        [r.len as u64, data.len() as u64, 0, 0],
        &fsp[..r.len],
        data,
        &mut [],
    )
}

/// Asks the kernel to destroy this task (`EXIT` syscall). Never returns
/// for a task that's allowed to exit; for this shell - always task 0,
/// the designated keyboard owner - it always comes back [`EXIT_DENIED`]
/// instead (see [`cmd_exit`]).
fn task_exit(code: u64) -> u64 {
    syscall(syscall_abi::EXIT, code)
}

/// Task `i`'s scheduler state (`TASK_STATE` syscall) - see [`cmd_ps`].
fn task_state(i: u64) -> u64 {
    syscall(syscall_abi::TASK_STATE, i)
}

/// `ps` builtin: one line per scheduler slot, probing indices upward
/// until the kernel answers [`TASK_STATE_INVALID`] (how the slot count
/// is discovered without it leaking into the ABI as a constant). The
/// caller can't tell "running right now" from "runnable, waiting its
/// turn" - it is, by definition, the one running at the moment it asks -
/// so both print as "runnable". Real output, so it goes through the
/// sink (redirectable like any other command's).
fn cmd_ps(out: &mut Output) {
    let mut i = 0u64;
    loop {
        let state = task_state(i);
        if state == TASK_STATE_INVALID {
            break;
        }
        out.put_str("task ");
        out.put_u64_decimal(i);
        out.put_line(match state {
            TASK_STATE_UNUSED => ": unused",
            TASK_STATE_RUNNABLE => ": runnable",
            TASK_STATE_BLOCKED => ": blocked (waiting)",
            TASK_STATE_ZOMBIE => ": exited - `wait` to collect its status",
            _ => ": ?",
        });
        i += 1;
    }
}

/// Hand-rolled decimal parse (the shell's first numeric argument) - a
/// digit loop, no `core::fmt`, `None` for empty/non-digit input.
fn parse_u64(s: &str) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add((b - b'0') as u64)?;
    }
    Some(n)
}

/// `kill <n>` - destroys another task (the `KILL` syscall). The kernel
/// refuses tasks 0/1 and empty slots; see [`print_fs_error`]'s task
/// arms for the messages.
fn cmd_kill(arg: &str) {
    let Some(n) = parse_u64(arg) else {
        print_line("kill: usage: kill <task number> (see ps)");
        return;
    };
    match syscall(syscall_abi::KILL, n) {
        code if code >= FS_ERR_MIN => print_fs_error("kill", code),
        _ => {}
    }
}

/// `fg <n>` - hands the keyboard to task `n` (the `FG` syscall). This
/// shell's own next read then waits until that task exits or is killed
/// (ownership reverts to task 0 automatically on the owner's death).
/// Ctrl+C is the escape hatch: the kernel intercepts it whenever a
/// task other than the boot shell owns the keyboard, reverting
/// ownership to task 0 (the foregrounded task keeps running in the
/// background - nothing is delivered to it; `kill` it if it should
/// die too).
fn cmd_fg(arg: &str) {
    let Some(n) = parse_u64(arg) else {
        print_line("fg: usage: fg <task number> (see ps)");
        return;
    };
    match syscall(syscall_abi::FG, n) {
        code if code >= FS_ERR_MIN => print_fs_error("fg", code),
        _ => {}
    }
}

/// `wait <n>` - blocks until task `n` dies, then reports its collected
/// exit status (which is also what reaps it: an un-waited exited task
/// holds its slot as a zombie - see `ps`). Ctrl+C interrupts the wait
/// (the task keeps running); any other typing during a wait is
/// discarded, same spirit as typing at a busy foreground job in `sh`.
fn cmd_wait(arg: &str) {
    let Some(n) = parse_u64(arg) else {
        print_line("wait: usage: wait <task number> (see ps)");
        return;
    };
    match syscall(syscall_abi::WAIT, n) {
        WAIT_INTERRUPTED => print_line("wait: interrupted (the task keeps running)"),
        TASK_KILLED_STATUS => {
            print_str("task ");
            print_u64(n);
            print_line(" was killed");
        }
        code if code >= FS_ERR_MIN => print_fs_error("wait", code),
        status => {
            print_str("task ");
            print_u64(n);
            print_str(" exited with code ");
            print_u64(status);
            print_line("");
        }
    }
}

/// Console-side decimal print (the [`Output`] sink has its own
/// `put_u64_decimal`; this is the same digit loop for the handful of
/// console-only messages that need a number).
fn print_u64(n: u64) {
    let mut out = Output::Console;
    out.put_u64_decimal(n);
}

/// `mount` (no arg) reports what's mounted; `mount -a` performs the
/// mount action. The disk-tools arc (milestone 1) repurposed the bare
/// command to *list*, Unix-style - the mounting action moved to `-a`.
fn cmd_mount(line: &str, arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    match arg {
        "" => mount_info(out),
        "-a" => mount_disk(),
        "-r" => cmd_mount_remote(line, cwd, cwd_len, out),
        "-p" => cmd_mount_proc(line, cwd, cwd_len, out),
        "-c" => cmd_mount_con(line, cwd, cwd_len, out),
        "-n" => cmd_mount_net(line, cwd, cwd_len, out),
        _ => cmd_mount_at(line, cwd, cwd_len, out),
    }
}

/// `mount -n <path>` - bind the network server's synthetic `/net` (this machine's
/// network identity as read-only files, cluster Phase 3) at `<path>`. Then `ls
/// <path>` shows `ip`/`mac` and `cat <path>/ip` reads this machine's address. A
/// remote machine's `/net` needs no bind - `cat <remote-mount>/net/ip` reads it
/// (netd's export routes `/net`). Usually `mount -n /net`.
fn cmd_mount_net(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // "mount"
    words.next(); // "-n"
    let Some(path_arg) = words.next() else {
        out.put_line("mount: usage: mount -n <path>  (e.g. mount -n /net)");
        return;
    };
    let mut pbuf = [0u8; PATH_SIZE];
    let Some(pl) = resolve_path(cwd_str(cwd, cwd_len), path_arg, &mut pbuf) else {
        out.put_line("mount: path too long");
        return;
    };
    let prefix = strip_trailing_slash(&pbuf[..pl]);
    if prefix.len() < 2 || prefix[0] != b'/' {
        out.put_line("mount: <path> must be a path below /");
        return;
    }
    if ns_add(prefix, b"/", ninep_abi::NS_NET_TREE) {
        out.put_str("net mounted at ");
        if let Ok(p) = core::str::from_utf8(prefix) {
            out.put_line(p);
        } else {
            out.put_line("");
        }
    } else {
        out.put_line("mount: path too long or namespace full");
    }
}

/// `mount -c <path>` - bind the console (`cond`, `CON_TASK`) as a writable file at
/// `<path>` (cluster Phase 3 `/dev/cons`). A write to `<path>` then renders on the
/// console; reads are refused (write-only). Usually `mount -c /dev/cons`. A remote
/// machine's console needs no bind here - write `<remote-mount>/dev/cons` and
/// netd's export routes it (so `echo hi > /mnt/a/dev/cons` prints on A's screen).
fn cmd_mount_con(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // "mount"
    words.next(); // "-c"
    let Some(path_arg) = words.next() else {
        out.put_line("mount: usage: mount -c <path>  (e.g. mount -c /dev/cons)");
        return;
    };
    let mut pbuf = [0u8; PATH_SIZE];
    let Some(pl) = resolve_path(cwd_str(cwd, cwd_len), path_arg, &mut pbuf) else {
        out.put_line("mount: path too long");
        return;
    };
    let prefix = strip_trailing_slash(&pbuf[..pl]);
    if prefix.len() < 2 || prefix[0] != b'/' {
        out.put_line("mount: <path> must be a path below /");
        return;
    }
    if ns_add(prefix, b"/", ninep_abi::NS_CON_TREE) {
        out.put_str("console mounted at ");
        if let Ok(p) = core::str::from_utf8(prefix) {
            out.put_line(p);
        } else {
            out.put_line("");
        }
    } else {
        out.put_line("mount: path too long or namespace full");
    }
}

/// `mount -p <path>` - bind the synthetic `/proc` filesystem (the process table
/// as files, cluster Phase 3) at `<path>` in this shell's namespace. `fsd`
/// auto-mounts `/proc` at the reserved `NS_PROC_TREE` at boot, so this is just a
/// namespace bind (like `mount <n> <path>` for a disk tree). Then `ls <path>`
/// lists one dir per task slot and `cat <path>/<n>/state` reads its scheduler
/// state. A remote machine's `/proc` needs no bind here - read it at
/// `<remote-mount>/proc` (netd's export routes it). Usually `mount -p /proc`.
fn cmd_mount_proc(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // "mount"
    words.next(); // "-p"
    let Some(path_arg) = words.next() else {
        out.put_line("mount: usage: mount -p <path>  (e.g. mount -p /proc)");
        return;
    };
    let mut pbuf = [0u8; PATH_SIZE];
    let Some(pl) = resolve_path(cwd_str(cwd, cwd_len), path_arg, &mut pbuf) else {
        out.put_line("mount: path too long");
        return;
    };
    let prefix = strip_trailing_slash(&pbuf[..pl]);
    if prefix.len() < 2 || prefix[0] != b'/' {
        out.put_line("mount: <path> must be a path below /");
        return;
    }
    if ns_add(prefix, b"/", ninep_abi::NS_PROC_TREE) {
        out.put_str("proc mounted at ");
        if let Ok(p) = core::str::from_utf8(prefix) {
            out.put_line(p);
        } else {
            out.put_line("");
        }
    } else {
        out.put_line("mount: path too long or namespace full");
    }
}

/// `mount -r <host:port> <path>` - remote-mount another machine's 9P export
/// (cluster Phase 1c) into this shell's namespace at `<path>`. A path under
/// `<path>` then resolves to a `NETOP_RMOUNT` round trip through `netd` to
/// `host:port`, so `ls <path>` / `cat <path>/file` read the *remote* machine's
/// disk over TCP - the pivot to distributed. The binding is per-task and
/// inherited by spawned commands, exactly like every other namespace change.
///
/// **Trusted-LAN, no authentication** (the roadmap's stated Phase 1 posture):
/// any peer that connects to an export is served; who-may-mount-what is a later
/// hardening phase. `host` is a dotted-quad IPv4 (name resolution is a later
/// nicety); `port` defaults to 564 ([`ninep_abi::NP_NET_PORT`]) if omitted.
fn cmd_mount_remote(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // "mount"
    words.next(); // "-r"
    let (Some(hostport), Some(path_arg)) = (words.next(), words.next()) else {
        out.put_line("mount: usage: mount -r <host:port> <path>  (trusted LAN, no auth)");
        return;
    };
    // Split host[:port] on the ':' byte; default port NP_NET_PORT (564). Byte
    // scanning (not `split`/`rsplit_once`) keeps the PIE relocation-safe - a
    // char-pattern search emits an R_AARCH64_ABS64 against a core lookup table
    // (see docs/processes.md).
    let hb = hostport.as_bytes();
    let colon = {
        let mut idx = None;
        let mut i = 0;
        while i < hb.len() {
            if hb[i] == b':' {
                idx = Some(i);
            }
            i += 1;
        }
        idx
    };
    let (host, port) = match colon {
        Some(c) => {
            // Byte slices (not `&hostport[..c]`): str range-indexing inserts a
            // UTF-8 char-boundary check whose panic path pulls in core's
            // formatting tables and breaks the PIE link (R_AARCH64_ABS64) - the
            // same relocation trap docs/processes.md warns about.
            let h = core::str::from_utf8(&hb[..c]).unwrap_or("");
            let p = core::str::from_utf8(&hb[c + 1..]).unwrap_or("");
            match parse_u64(p) {
                Some(pn) if pn <= u16::MAX as u64 => (h, pn as u16),
                _ => {
                    out.put_line("mount: bad port");
                    return;
                }
            }
        }
        None => (hostport, ninep_abi::NP_NET_PORT),
    };
    let Some(ip) = parse_ipv4(host) else {
        out.put_line("mount: <host> must be a dotted-quad IPv4 (e.g. 10.0.2.2)");
        return;
    };
    let mut pbuf = [0u8; PATH_SIZE];
    let Some(pl) = resolve_path(cwd_str(cwd, cwd_len), path_arg, &mut pbuf) else {
        out.put_line("mount: path too long");
        return;
    };
    let prefix = strip_trailing_slash(&pbuf[..pl]);
    if prefix.len() < 2 || prefix[0] != b'/' {
        out.put_line("mount: <path> must be a path below /");
        return;
    }
    // Remote binding target: [ip:4][port:2 LE][remote-root]. The root is "/"
    // (the remote export serves its whole boot mount).
    let mut target = [0u8; ninep_abi::NS_ENDPOINT_LEN + 1];
    target[0..4].copy_from_slice(&ip);
    target[4..6].copy_from_slice(&port.to_le_bytes());
    target[6] = b'/';
    if ns_add(prefix, &target, ninep_abi::NS_REMOTE_TREE) {
        out.put_str("remote-mounted (trusted, no auth) at ");
        if let Ok(p) = core::str::from_utf8(prefix) {
            out.put_line(p);
        } else {
            out.put_line("");
        }
    } else {
        out.put_line("mount: path too long or namespace full");
    }
}

/// Parse a dotted-quad IPv4 (`a.b.c.d`, each octet 0-255) - no names, no
/// shorthand. Byte scanning on '.' (not `split`) keeps it PIE relocation-safe,
/// like [`parse_u64`] and the host:port split above.
fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let b = s.as_bytes();
    let mut octets = [0u8; 4];
    let mut n = 0;
    let mut start = 0;
    let mut i = 0;
    loop {
        let at_end = i == b.len();
        if at_end || b[i] == b'.' {
            if n >= 4 {
                return None;
            }
            let part = core::str::from_utf8(&b[start..i]).ok()?;
            let v = parse_u64(part)?;
            if v > 255 {
                return None;
            }
            octets[n] = v as u8;
            n += 1;
            start = i + 1;
            if at_end {
                break;
            }
        }
        i += 1;
    }
    if n == 4 {
        Some(octets)
    } else {
        None
    }
}

/// `cpu <host:port> <command...>` - remote execution (cluster Phase 4a, the Plan
/// 9 `cpu` model): run `<command>` on the remote machine and print its output
/// here. In 4a the command runs on the remote's CPU using the *remote's*
/// resources (its `/bin`, its disk); a later step imports this machine's
/// namespace so it reads *our* files. Trusted-LAN, no auth. Output is bounded to
/// one message for now (small commands). E.g. `cpu 10.0.2.10:564 ls /`.
fn cmd_cpu(line: &str, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // "cpu"
    let Some(hostport) = words.next() else {
        out.put_line("cpu: usage: cpu <host:port> <command...>  (e.g. cpu 10.0.2.10:564 ls /)");
        return;
    };
    // Split host[:port] on the ':' byte (PIE-safe byte scan, see cmd_mount_remote).
    let hb = hostport.as_bytes();
    let mut colon = None;
    let mut i = 0;
    while i < hb.len() {
        if hb[i] == b':' {
            colon = Some(i);
        }
        i += 1;
    }
    let (host, port) = match colon {
        Some(c) => {
            let h = core::str::from_utf8(&hb[..c]).unwrap_or("");
            let p = core::str::from_utf8(&hb[c + 1..]).unwrap_or("");
            match parse_u64(p) {
                Some(pn) if pn <= u16::MAX as u64 => (h, pn as u16),
                _ => {
                    out.put_line("cpu: bad port");
                    return;
                }
            }
        }
        None => (hostport, ninep_abi::NP_NET_PORT),
    };
    let Some(ip) = parse_ipv4(host) else {
        out.put_line("cpu: <host> must be a dotted-quad IPv4 (e.g. 10.0.2.10)");
        return;
    };
    // The command = the remaining words joined by single spaces (spacing is
    // irrelevant; the remote re-splits on spaces into argv).
    let mut cmd = [0u8; 256];
    let mut cl = 0usize;
    for w in words {
        if cl > 0 && cl < cmd.len() {
            cmd[cl] = b' ';
            cl += 1;
        }
        for &b in w.as_bytes() {
            if cl < cmd.len() {
                cmd[cl] = b;
                cl += 1;
            }
        }
    }
    if cl == 0 {
        out.put_line("cpu: usage: cpu <host:port> <command...>");
        return;
    }
    // NETOP_RUN request: [op][ip:4][port:2 LE][pad:2][command line]. Sent to the
    // local network server, which does the remote round trip and returns the
    // command's output.
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&syscall_abi::NETOP_RUN.to_le_bytes());
    req[8..12].copy_from_slice(&ip);
    req[12..14].copy_from_slice(&port.to_le_bytes());
    let base = 16usize;
    let n = cl.min(req.len() - base);
    req[base..base + n].copy_from_slice(&cmd[..n]);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::NET_TASK,
        req.as_ptr() as u64,
        (base + n) as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed >= FS_ERR_MIN {
        out.put_line("cpu: could not reach the network server");
        return;
    }
    let rlen = ((packed & 0xffff_ffff) as usize).min(reply.len());
    for &b in &reply[..rlen] {
        out.put(b);
    }
}

/// `mount <partition> <path>` - mount the disk's Nth partition and make it
/// visible at `<path>` in this shell's namespace (cluster Phase 0 multi-mount).
/// `fsd` mounts the partition into a fresh tree (`FSOP_MOUNT_AT`) and returns
/// its tree id; we `bind` `<path>` onto that tree's root. A path under `<path>`
/// then resolves to that filesystem - a **second disk visible alongside** the
/// boot mount at `/`, which the old single-mount model physically couldn't
/// express. Per-task, like every namespace change.
fn cmd_mount_at(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    words.next(); // "mount"
    let (Some(idx_arg), Some(path_arg)) = (words.next(), words.next()) else {
        out.put_line("mount: usage: `mount` | `mount -a` | `mount <partition> <path>` | `mount -r <host:port> <path>`");
        return;
    };
    let Some(index) = parse_u64(idx_arg) else {
        out.put_line("mount: <partition> must be a number (0, 1, ...)");
        return;
    };
    let mut pbuf = [0u8; PATH_SIZE];
    let Some(pl) = resolve_path(cwd_str(cwd, cwd_len), path_arg, &mut pbuf) else {
        out.put_line("mount: path too long");
        return;
    };
    let prefix = strip_trailing_slash(&pbuf[..pl]);
    if prefix.len() < 2 || prefix[0] != b'/' {
        out.put_line("mount: <path> must be a path below /");
        return;
    }
    // fsd mounts the partition into a tree and returns the tree id (a small
    // number < FS_ERR_MIN; an error is >= FS_ERR_MIN).
    let tree = fs_call(syscall_abi::FSOP_MOUNT_AT, [index, 0, 0, 0], &[], &[], &mut []);
    if tree >= FS_ERR_MIN {
        print_fs_error("mount", tree);
        return;
    }
    if !ns_add(prefix, b"/", tree as u8) {
        out.put_line("mount: path too long or namespace full");
    }
}

/// `mount` with no argument: ask the filesystem server what's mounted
/// (FSOP_MOUNT_INFO) and print the format, its partition's first sector,
/// and the disk's capacity - or that nothing is mounted.
fn mount_info(out: &mut Output) {
    // Reply payload: partition_lba (u64), capacity_sectors (u64), then the
    // format name as ASCII to the end. Zero-init so the name's end is a 0.
    let mut info = [0u8; 32];
    match fs_call(syscall_abi::FSOP_MOUNT_INFO, [0; 4], &[], &[], &mut info) {
        NO_FS => print_line("nothing mounted (run `mount -a` to mount the disk)"),
        0 => {
            let part_lba = u64::from_le_bytes([
                info[0], info[1], info[2], info[3], info[4], info[5], info[6], info[7],
            ]);
            let capacity = u64::from_le_bytes([
                info[8], info[9], info[10], info[11], info[12], info[13], info[14], info[15],
            ]);
            let name_end = info[16..].iter().position(|&b| b == 0).unwrap_or(16) + 16;
            let name = core::str::from_utf8(&info[16..name_end]).unwrap_or("?");
            out.put_str(name);
            out.put_str(" mounted at partition LBA ");
            out.put_u64_decimal(part_lba);
            // 512-byte sectors -> MiB is sectors / 2048.
            out.put_str(" (disk ");
            out.put_u64_decimal(capacity);
            out.put_str(" sectors, ");
            out.put_u64_decimal(capacity / 2048);
            out.put_line(" MiB)");
        }
        _ => print_line("mount: unexpected return"),
    }
}

/// `mount -a` - rescans the USB ports for a storage device that attached
/// after boot and mounts its filesystem. The Parallels workflow:
/// a passed-through stick appears a few seconds after the VM starts,
/// later than the kernel's boot-time scan - boot, wait a moment, type
/// `mount -a`, and the disk commands come alive.
fn mount_disk() {
    // Server-first, then the device: ask the filesystem server to
    // mount whatever block device the kernel already holds
    // (FSOP_MOUNT). Only if that can't produce a filesystem - nothing
    // mounted and the current device (if any) isn't mountable - ask
    // the kernel to rescan the USB ports and install a found stick as
    // a *replacement* device (MOUNT with the replace flag, safe
    // exactly because the server just confirmed nothing is mounted),
    // then ask the server again. This preserves the old
    // first-MOUNTED-wins behavior: an unmountable boot-time device
    // (e.g. `make run`'s FAT16 vvfat disk) never blocks a later USB
    // stick from becoming the disk.
    match fs_call(syscall_abi::FSOP_MOUNT, [0; 4], &[], &[], &mut []) {
        0 => {
            print_line("mounted - disk commands available");
            return;
        }
        MOUNT_ALREADY => {
            print_line("mount: a filesystem is already mounted");
            return;
        }
        NO_FS => {}
        _ => {
            print_line("mount: unexpected return");
            return;
        }
    }
    match syscall(syscall_abi::MOUNT, 1) {
        0 => {}
        MOUNT_NO_DEVICE => {
            print_line("mount: no USB storage device found (see the kernel log; is the stick attached to the VM?)");
            return;
        }
        _ => {
            print_line("mount: unexpected return");
            return;
        }
    }
    match fs_call(syscall_abi::FSOP_MOUNT, [0; 4], &[], &[], &mut []) {
        0 => print_line("mounted - disk commands available"),
        NO_FS => print_line("mount: device found, but no mountable filesystem on it (see the server's log line)"),
        _ => print_line("mount: unexpected return"),
    }
}

/// `unmount` - drops the filesystem server's mounted volume (FSOP_UNMOUNT)
/// so the disk can be reformatted or a different volume mounted. The
/// kernel's block device is untouched; `mount -a` re-probes and remounts
/// it. The disk-tools arc, milestone 1.
fn cmd_unmount() {
    match fs_call(syscall_abi::FSOP_UNMOUNT, [0; 4], &[], &[], &mut []) {
        0 => print_line("unmounted"),
        NO_FS => print_line("unmount: nothing was mounted"),
        _ => print_line("unmount: unexpected return"),
    }
}

/// `erase disk` - zeroes the disk's first sectors (`FSOP_ERASE`), wiping the
/// partition table and any filesystem metadata near the start so the disk can
/// be freshly partitioned. Requires the literal argument `disk` as a guard
/// against an accidental bare `erase`, and refuses while a filesystem is
/// mounted. Disk-tools milestone 2. **Destructive.** Must be a builtin, not a
/// `/bin` program: it runs when nothing is mounted, which is exactly when
/// `/bin` can't be read to load a program.
fn cmd_erase(arg: &str) {
    if arg != "disk" {
        print_line("erase: usage: `erase disk` (wipes the start of the disk; unmount first; destructive)");
        return;
    }
    match fs_call(syscall_abi::FSOP_ERASE, [0; 4], &[], &[], &mut []) {
        0 => print_line("erased - disk start wiped; run `partition` next"),
        MOUNT_ALREADY => print_line("erase: a filesystem is mounted - run `unmount` first"),
        MOUNT_NO_DEVICE => print_line("erase: no disk device"),
        _ => print_line("erase: I/O error"),
    }
}

/// `partition [fat32|exfat|ext2]` - writes a single-partition MBR spanning the
/// disk (`FSOP_PARTITION`), tagging the partition with the given type byte
/// (default fat32). Refuses while a filesystem is mounted. The partition is
/// left unformatted - `format` (a later milestone) lays a filesystem into it.
/// Disk-tools milestone 2. **Destructive** (overwrites the partition table).
/// A builtin for the same reason as `erase`.
fn cmd_partition(arg: &str) {
    let type_byte: u64 = match arg {
        "" | "fat32" => 0x0C,
        "exfat" => 0x07,
        "ext2" | "linux" => 0x83,
        _ => {
            print_line("partition: usage: `partition [fat32|exfat|ext2]` (default fat32; unmount first)");
            return;
        }
    };
    match fs_call(syscall_abi::FSOP_PARTITION, [type_byte, 0, 0, 0], &[], &[], &mut []) {
        0 => print_line("partitioned - one MBR partition spanning the disk (unformatted; use `format` next)"),
        MOUNT_ALREADY => print_line("partition: a filesystem is mounted - run `unmount` first"),
        MOUNT_NO_DEVICE => print_line("partition: no disk device"),
        _ => print_line("partition: disk too small or I/O error"),
    }
}

/// `format [fat32|exfat|ext2]` - lays a fresh filesystem into the disk's first
/// partition (`FSOP_FORMAT`); FAT32, exFAT, and ext2 are all supported.
/// Refuses while a filesystem is mounted; the partition must already exist
/// (`partition` first). Disk-tools milestone 3. **Destructive.** A builtin for
/// the same reason as `erase`/`partition`.
fn cmd_format(arg: &str) {
    let fstype: u64 = match arg {
        "" | "fat32" => syscall_abi::FMT_FAT32,
        "exfat" => syscall_abi::FMT_EXFAT,
        "ext2" => syscall_abi::FMT_EXT2,
        _ => {
            print_line("format: usage: `format [fat32|exfat|ext2]` (unmount first)");
            return;
        }
    };
    match fs_call(syscall_abi::FSOP_FORMAT, [fstype, 0, 0, 0], &[], &[], &mut []) {
        0 => print_line("formatted - run `mount -a`, then `ls`"),
        MOUNT_ALREADY => print_line("format: a filesystem is mounted - run `unmount` first"),
        MOUNT_NO_DEVICE => print_line("format: no disk device"),
        FS_ERR_NOT_FOUND => print_line("format: no partition - run `partition` first"),
        _ => print_line("format: the partition is too small, or an I/O error occurred"),
    }
}

/// `send <task> <words...>` - joins the words like `write` does and
/// sends them as one IPC message (`MSG_SEND`) to the given task.
fn cmd_send(line: &str) {
    let mut words = line.split_whitespace();
    words.next(); // "send" itself
    let Some(task_arg) = words.next() else {
        print_line("send: usage: send <task number> <words...>");
        return;
    };
    let Some(dest) = parse_u64(task_arg) else {
        print_line("send: usage: send <task number> <words...>");
        return;
    };

    let mut msg = [0u8; 64];
    let mut len = 0usize;
    let mut first = true;
    for word in words {
        if !first && len < msg.len() {
            msg[len] = b' ';
            len += 1;
        }
        for b in word.bytes() {
            if len < msg.len() {
                msg[len] = b;
                len += 1;
            }
        }
        first = false;
    }
    if len == 0 {
        print_line("send: usage: send <task number> <words...>");
        return;
    }

    match syscall4(syscall_abi::MSG_SEND, dest, msg.as_ptr() as u64, len as u64, 0) {
        code if code >= FS_ERR_MIN => print_fs_error("send", code),
        _ => {}
    }
}

/// `recv` - blocks until a message arrives (`MSG_RECV`) and prints it
/// as `task N: <message>`. Ctrl+C interrupts, same as `wait`.
fn cmd_recv() {
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    match syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) {
        RECV_INTERRUPTED => print_line("recv: interrupted"),
        code if code >= FS_ERR_MIN => print_fs_error("recv", code),
        packed => {
            let sender = packed >> 32;
            let len = (packed & 0xffff_ffff) as usize;
            print_str("task ");
            print_u64(sender);
            print_str(": ");
            if let Ok(text) = core::str::from_utf8(&buf[..len.min(buf.len())]) {
                print_line(text);
            } else {
                print_line("(non-UTF-8 message)");
            }
        }
    }
}

/// `exit` builtin. This boot-loaded shell is task 0, which the kernel
/// refuses to let exit (it's the one task that ever receives keyboard
/// input - if it died, nothing could type again this boot), so for this
/// program the command always reports the refusal. The builtin exists
/// anyway because a *replacement* program someone writes and spawns
/// (see docs/processes.md) genuinely can exit - and the syscall wrapper
/// here is the reference for how.
fn cmd_exit() {
    match task_exit(0) {
        EXIT_DENIED => print_line("exit: refused - the boot shell can't exit (nothing would own the keyboard)"),
        _ => print_line("exit: unexpected return"),
    }
}

/// The 1-argument syscalls this program used before phase 3c - a thin
/// wrapper over [`syscall4`] with the unused arguments zeroed.
#[inline(always)]
fn syscall(number: u64, arg0: u64) -> u64 {
    syscall4(number, arg0, 0, 0, 0)
}

#[inline(always)]
fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
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

/// Blocks until a byte is available - see `main`'s loop for why this
/// replaced a busy-poll around the non-blocking `TRY_READ_CHAR` syscall
/// (still available in `syscall-abi` for any caller that genuinely wants
/// non-blocking semantics; this program just isn't one anymore).
fn read_char() -> u8 {
    syscall(syscall_abi::READ_CHAR, 0) as u8
}

fn putc(byte: u8) {
    con_write(&[byte]);
}

/// Route output to the console server (task `CON_TASK`) as batched
/// `DSPOP_WRITE` messages, rather than one `PUTC` syscall per byte. The
/// console server owns the steady-state console (it renders to a
/// framebuffer, or forwards to the kernel's byte-stream console); this
/// is the client half of moving the console out of the kernel.
///
/// Falls back to the kernel's own console (`PUTC`) whenever the call
/// fails - most importantly when there's no console server this boot
/// (`TASK_ERR_NO_SUCH_TASK`), but also on any other reserved-band error -
/// so output always reaches *a* console. Longer strings are split into
/// `FS_DATA_MAX` chunks; each carries a 768-byte reply buffer (the
/// `MSG_CALL` ABI's fixed reply size), the one real stack cost here.
fn con_write(bytes: &[u8]) {
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

fn wfe() {
    unsafe {
        asm!("wfe", options(nomem, nostack, preserves_flags));
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        wfe();
    }
}

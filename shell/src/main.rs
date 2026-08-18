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
// Both sized to the kernel's per-syscall buffer cap (`MAX_USER_LEN`,
// 512 - see kernel/src/syscall.rs): any larger buffer would be rejected
// at the syscall boundary anyway, so this is the ceiling, not a choice.
const LIST_BUFFER_SIZE: usize = 512;
const CAT_BUFFER_SIZE: usize = 512;
/// Redirected-output capture buffer ([`Output::Capture`]). Exactly
/// [`CAT_BUFFER_SIZE`]/[`LIST_BUFFER_SIZE`] (512, the syscall cap) -
/// the largest output any single builtin can produce fits with zero
/// slack (`cat`'s display-only trailing newline and truncation notice
/// deliberately bypass the capture, so a full 512-byte read captures
/// exactly 512 bytes). Kept at the cap rather than larger because it
/// lives on `run_line`'s stack frame and this program's stack is a
/// fixed, unguarded 8KB (see `docs/processes.md`).
const CAPTURE_SIZE: usize = 512;

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
        on_byte(byte, &mut buf, &mut len, &mut cwd, &mut cwd_len);
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
fn on_byte(byte: u8, buf: &mut [u8; BUFFER_SIZE], len: &mut usize, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize) {
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
                Ok(line) => run_line(line, cwd, cwd_len),
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
fn run_line(line: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize) {
    // Pipelines first: a standalone `|` splits the line into a builtin
    // (left, output captured) and a program path (right, spawned and
    // fed the capture as IPC messages) - see cmd_pipeline. Combining
    // with `>`/`>>` is refused: the right side is a real separate task
    // writing straight to the console, so there's no capture of *its*
    // output to redirect.
    match parse_pipe(line) {
        PipeParse::NoPipe => {}
        PipeParse::Malformed(msg) => {
            print_line(msg);
            return;
        }
        PipeParse::Pipe { left, right } => {
            cmd_pipeline(left, right, cwd, cwd_len);
            return;
        }
    }
    match parse_redirect(line) {
        RedirectParse::NoRedirect => {
            let mut out = Output::Console;
            dispatch_line(line, cwd, cwd_len, &mut out);
        }
        RedirectParse::Malformed(msg) => print_line(msg),
        RedirectParse::Redirect { cmd, target, append } => {
            let mut capture = [0u8; CAPTURE_SIZE];
            let mut out = Output::Capture { buf: &mut capture, len: 0, overflowed: false };
            dispatch_line(cmd, cwd, cwd_len, &mut out);
            let (len, overflowed) = match out {
                Output::Capture { len, overflowed, .. } => (len, overflowed),
                // Never constructed on this path; written out rather than
                // `unreachable!()` so a logic error here degrades to a
                // silently-skipped write, not a panic-handler hang.
                Output::Console => return,
            };
            finish_redirect(&capture[..len], overflowed, target, append, cwd, *cwd_len);
        }
    }
}

/// What [`parse_pipe`] found on a submitted line.
enum PipeParse<'a> {
    /// No standalone `|` token - not a pipeline.
    NoPipe,
    /// `left | right`: run `left` as a builtin with a capture sink,
    /// spawn `right` as a program, stream the capture to it.
    Pipe { left: &'a str, right: &'a str },
    /// A `|` was present but the line was wrong; the message says how.
    Malformed(&'static str),
}

/// Scans the line's whitespace tokens for the first standalone `|`
/// and splits there - the operator must be its own token (`a|b` is one
/// word), same no-quoting tokenization rule as `parse_redirect`. The
/// right side must be exactly one token (a program path - programs
/// receive no arguments), and mixing `|` with `>`/`>>` is refused.
fn parse_pipe(line: &str) -> PipeParse<'_> {
    for token in line.split_whitespace() {
        // SAFETY-free pointer math: token is a subslice of line, so
        // this offset arithmetic can't go out of range.
        let token_start = token.as_ptr() as usize - line.as_ptr() as usize;
        if token == "|" {
            let left = line.get(..token_start).unwrap_or("").trim();
            let rest = line.get(token_start + 1..).unwrap_or("").trim();
            if left.is_empty() {
                return PipeParse::Malformed("pipe: missing command before |");
            }
            if rest.is_empty() {
                return PipeParse::Malformed("pipe: missing program after |");
            }
            let mut right_tokens = rest.split_whitespace();
            let right = right_tokens.next().unwrap_or("");
            if right_tokens.next().is_some() {
                return PipeParse::Malformed(
                    "pipe: exactly one program path may follow | (programs take no arguments)",
                );
            }
            if right == "|" || left.split_whitespace().any(|t| t == ">" || t == ">>") {
                return PipeParse::Malformed("pipe: only a single `left | program` pipeline is supported");
            }
            if right == ">" || right == ">>" {
                return PipeParse::Malformed("pipe: can't combine | with output redirection");
            }
            return PipeParse::Pipe { left, right };
        }
        if token == ">" || token == ">>" {
            // A redirect before any pipe operator: if a `|` appears
            // later it belongs to the redirect's target text - refuse
            // the combination outright rather than guessing.
            if line.split_whitespace().any(|t| t == "|") {
                return PipeParse::Malformed("pipe: can't combine | with output redirection");
            }
            return PipeParse::NoPipe;
        }
    }
    PipeParse::NoPipe
}

/// `left | program`: runs the builtin `left` with a capture sink (the
/// redirection machinery's), spawns `program` (see [`spawn_path`]),
/// streams the capture to it as IPC messages (1-64 bytes each,
/// [`syscall_abi::MSG_ERR_FULL`] retried with a real-tick timeout so a
/// program that never reads can't hang the shell), sends the empty
/// end-of-stream message, and waits for the program to exit. The
/// program's own output goes straight to the console as it runs -
/// there is no capture of a *task's* output, which is also why `|`
/// can't combine with `>`.
fn cmd_pipeline(left: &str, right: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize) {
    let mut capture = [0u8; CAPTURE_SIZE];
    let mut out = Output::Capture { buf: &mut capture, len: 0, overflowed: false };
    dispatch_line(left, cwd, cwd_len, &mut out);
    let (len, overflowed) = match out {
        Output::Capture { len, overflowed, .. } => (len, overflowed),
        Output::Console => return,
    };
    if overflowed {
        print_line("pipe: left command's output exceeds the 512-byte capture buffer - nothing was piped");
        return;
    }

    let slot = match spawn_path(right, cwd, *cwd_len) {
        Ok(slot) => slot,
        Err(0) => {
            print_line("pipe: path too long");
            return;
        }
        Err(NO_FS) => {
            print_no_fs();
            return;
        }
        Err(code) => {
            print_fs_error("pipe", code);
            return;
        }
    };

    // Stream the capture in message-sized chunks, then the empty
    // end-of-stream message (a still-non-null pointer with length 0 -
    // see MSG_SEND's doc in syscall-abi).
    let mut sent = 0usize;
    loop {
        let chunk_len = (len - sent).min(syscall_abi::MSG_MAX_LEN as usize);
        if !pipe_send(slot, unsafe { capture.as_ptr().add(sent) }, chunk_len as u64) {
            return; // pipe_send printed why, and killed the task if needed
        }
        if chunk_len == 0 {
            break; // the empty message was the end-of-stream marker
        }
        sent += chunk_len;
    }

    match syscall(syscall_abi::WAIT, slot) {
        WAIT_INTERRUPTED => print_line("pipe: interrupted (the program keeps running - see ps)"),
        TASK_KILLED_STATUS => print_line("pipe: program was killed"),
        code if code >= FS_ERR_MIN => {
            // Most likely the program crashed (a fault reaps the slot
            // immediately, so wait answers no-such-task) - the kernel
            // already reported the fault itself.
            print_line("pipe: program did not exit cleanly");
        }
        0 => {}
        status => {
            print_str("pipe: program exited with code ");
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

/// Holds an existing file's content plus the new capture during a `>>`
/// append ([`finish_redirect`]): the kernel's `fs_write_file` is
/// full-replace only (no append/offset writes at the FAT32 layer - a
/// known, documented gap), so append is read-everything, concatenate,
/// write-everything-back, all shell-side - the same
/// compose-two-existing-syscalls approach `cp` established. Anything
/// that doesn't fit is refused outright rather than partially written.
///
/// **Must not exceed 512** - the kernel bounds every userland
/// `(pointer, length)` syscall argument at `MAX_USER_LEN` (512,
/// `kernel/src/syscall.rs`), and an over-long buffer isn't a partial
/// success but a validation *failure* indistinguishable from
/// `FS_ERROR`. Found by testing, not review: a first cut used 1024
/// here, and the read-back's rejection looked exactly like
/// "no such file" - so `>>` silently behaved like `>`, discarding the
/// existing content instead of appending to it.
const APPEND_BUFFER_SIZE: usize = 512;

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

    let mut combined = [0u8; APPEND_BUFFER_SIZE];
    let data: &[u8] = if append {
        let existing = match fs_read_file(path, &mut combined) {
            NO_FS => {
                print_no_fs();
                return;
            }
            // No such file: `>>` creates it, standard sh semantics.
            FS_ERR_NOT_FOUND => 0,
            // Any other error (target is a directory, I/O failure, ...)
            // is now distinguishable from "missing" and reported here,
            // instead of limping into a write that would fail anyway.
            code if code >= FS_ERR_MIN => {
                print_fs_error("redirect", code);
                return;
            }
            // fs_read_file returns the file's *real* size, which may
            // exceed the buffer (the read was truncated at buffer
            // length) - so this one check refuses both "existing
            // content alone doesn't fit" and "existing + capture
            // doesn't fit". captured.len() <= CAPTURE_SIZE ==
            // APPEND_BUFFER_SIZE, so the subtraction can't underflow
            // (it can legitimately reach 0, refusing any append to a
            // nonempty file when the capture alone fills the buffer).
            size => {
                if size as usize > combined.len() - captured.len() {
                    print_line("redirect: file too large to append to - nothing written");
                    return;
                }
                size as usize
            }
        };
        combined[existing..existing + captured.len()].copy_from_slice(captured);
        &combined[..existing + captured.len()]
    } else {
        captured
    };

    match fs_write_file(path, data) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("redirect", code),
        _ => {}
    }
}

/// Tokenizes on whitespace (no quoting - `echo "a b"` sees two words, not
/// one) and dispatches to a builtin by name. An empty line (just Enter,
/// or a line of only spaces) does nothing, same as a real shell. Command
/// *output* goes to `out` (so a redirect can capture it); command *error*
/// messages go straight to the console via [`print_line`] regardless -
/// see [`Output`]'s doc comment for the stdout/stderr reasoning.
fn dispatch_line(line: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize, out: &mut Output) {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else { return };
    let arg = words.next().unwrap_or("");

    match command {
        "help" => out.put_line("commands: help, echo, uptime, clear, ls, cat, cd, pwd, mkdir, rmdir, touch, rm, write, cp, mv, mount, exec, exit, ps, kill, fg, wait, send, recv, selftest (append `> file`/`>> file` to redirect output, or `| /path/to/program` to pipe it into a spawned program)"),
        "echo" => {
            let mut first = true;
            for word in line.split_whitespace().skip(1) {
                if !first {
                    out.put(b' ');
                }
                for b in word.bytes() {
                    out.put(b);
                }
                first = false;
            }
            out.put(CR);
            out.put(LF);
        }
        "uptime" => {
            out.put_u64_decimal(get_ticks());
            out.put_line(" ticks since boot");
        }
        "clear" => {
            // ANSI clear-screen + cursor-home - the shell's own escape
            // sequence, not a syscall; the console itself has no notion
            // of a screen, just a byte stream. (Redirecting this writes
            // the raw escape bytes to the file - pointless but harmless,
            // not special-cased.)
            for &b in b"\x1b[2J\x1b[H" {
                out.put(b);
            }
        }
        "pwd" => out.put_line(cwd_str(cwd, *cwd_len)),
        "ls" => cmd_ls(arg, cwd, *cwd_len, out),
        "cat" => cmd_cat(arg, cwd, *cwd_len, out),
        "cd" => cmd_cd(arg, cwd, cwd_len),
        "mkdir" => cmd_mkdir(arg, cwd, *cwd_len),
        "rmdir" => cmd_rmdir(arg, cwd, *cwd_len),
        "touch" => cmd_touch(arg, cwd, *cwd_len),
        "rm" => cmd_rm(arg, cwd, *cwd_len),
        "write" => cmd_write(line, cwd, *cwd_len),
        "cp" => cmd_cp(line, cwd, *cwd_len),
        "mv" => cmd_mv(line, cwd, *cwd_len),
        "exec" => cmd_exec(arg, cwd, *cwd_len),
        "exit" => cmd_exit(),
        "ps" => cmd_ps(out),
        "kill" => cmd_kill(arg),
        "fg" => cmd_fg(arg),
        "wait" => cmd_wait(arg),
        "mount" => cmd_mount(),
        "send" => cmd_send(line),
        "recv" => cmd_recv(),
        "selftest" => cmd_selftest(out),
        _ => {
            print_str("unknown command: ");
            print_line(command);
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
        syscall_abi::SPAWN_ERR_BAD_ELF => "not a loadable program (bad ELF)",
        syscall_abi::SPAWN_ERR_TOO_LARGE => "program too large for the kernel's staging buffer (or empty)",
        syscall_abi::SPAWN_ERR_NO_FREE_SLOT => "no free task slot",
        syscall_abi::TASK_ERR_NO_SUCH_TASK => "no such task (see ps)",
        syscall_abi::TASK_ERR_PROTECTED => "that task is protected (the boot shell, idle, and the filesystem server are permanent)",
        _ => "failed",
    });
}

fn cmd_ls(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
        print_line("ls: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("ls: path too long");
        return;
    };

    let mut listing = [0u8; LIST_BUFFER_SIZE];
    match fs_list_dir(path, &mut listing) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("ls", code),
        n => {
            for &b in &listing[..n as usize] {
                out.put(b);
            }
        }
    }
}

fn cmd_cat(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize, out: &mut Output) {
    if arg.is_empty() {
        print_line("cat: missing file argument");
        return;
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
        print_line("cat: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("cat: path too long");
        return;
    };

    let mut file_buf = [0u8; CAT_BUFFER_SIZE];
    match fs_read_file(path, &mut file_buf) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("cat", code),
        size => {
            let n = (size as usize).min(file_buf.len());
            for &b in &file_buf[..n] {
                out.put(b);
            }
            // The tidy-terminal trailing newline is a display nicety,
            // console-only: keeping it out of a capture means
            // `cat a > b` copies a's bytes exactly (bounded by the read
            // buffer), rather than appending bytes a never contained.
            if out.is_console() && !file_buf[..n].ends_with(b"\n") {
                out.put(CR);
                out.put(LF);
            }
            // The truncation notice is an error-ish diagnostic, not
            // output - console always, never captured into the file
            // (which would corrupt a redirected copy with notice text).
            if size as usize > file_buf.len() {
                print_line("cat: (truncated - file is larger than this shell's read buffer)");
            }
        }
    }
}

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

fn cmd_mkdir(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    if arg.is_empty() {
        print_line("mkdir: missing directory argument");
        return;
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
        print_line("mkdir: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("mkdir: path too long");
        return;
    };

    match fs_mkdir(path) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("mkdir", code),
        _ => {}
    }
}

/// Loads and starts `arg` as a new, independent task alongside whatever is
/// already running - a real `tasks::spawn`, not a POSIX exec-replaces-
/// current-process. The new task keeps running after this command returns;
/// there's no way yet to wait for it, stop it, or free its memory once
/// started (see `tasks.rs`'s module doc comment).
fn cmd_exec(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    if arg.is_empty() {
        print_line("exec: missing program argument");
        return;
    }
    match spawn_path(arg, cwd, cwd_len) {
        Ok(_slot) => {}
        Err(0) => print_line("exec: path too long"),
        Err(NO_FS) => print_no_fs(),
        Err(code) => print_fs_error("exec", code),
    }
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
fn spawn_path(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) -> Result<u64, u64> {
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
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
    match syscall(syscall_abi::SPAWN, offset) {
        code if code >= FS_ERR_MIN => Err(code),
        slot => Ok(slot),
    }
}

fn cmd_rmdir(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    if arg.is_empty() {
        print_line("rmdir: missing directory argument");
        return;
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
        print_line("rmdir: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("rmdir: path too long");
        return;
    };

    match fs_rmdir(path) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("rmdir", code),
        _ => {}
    }
}

fn cmd_touch(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    if arg.is_empty() {
        print_line("touch: missing file argument");
        return;
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
        print_line("touch: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("touch: path too long");
        return;
    };

    match fs_touch(path) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("touch", code),
        _ => {}
    }
}

fn cmd_rm(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    if arg.is_empty() {
        print_line("rm: missing file argument");
        return;
    }
    let mut path_buf = [0u8; PATH_SIZE];
    let Some(path_len) = resolve_path(cwd_str(cwd, cwd_len), arg, &mut path_buf) else {
        print_line("rm: path too long");
        return;
    };
    let Ok(path) = core::str::from_utf8(&path_buf[..path_len]) else {
        print_line("rm: path too long");
        return;
    };

    match fs_rm(path) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("rm", code),
        _ => {}
    }
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

    match fs_write_file(path, &content[..len]) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("write", code),
        _ => {}
    }
}

/// `cp <src> <dst>` - reads `src`'s entire content into a local buffer,
/// then writes it to `dst` via [`fs_write_file`] (creating `dst` if it
/// doesn't exist, replacing it if it does - same semantics as `write`).
/// Needs no new syscall: this is pure shell-side plumbing over the two
/// that already exist (`fs_read_file`/`fs_write_file`). The read
/// completes in full, into this shell's own stack buffer, before the
/// write ever starts - copying a file onto itself is therefore safe by
/// construction, not a special case this handles explicitly.
fn cmd_cp(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    let mut words = line.split_whitespace();
    words.next(); // "cp" itself
    let Some(src_arg) = words.next() else {
        print_line("cp: missing source file argument");
        return;
    };
    let Some(dst_arg) = words.next() else {
        print_line("cp: missing destination file argument");
        return;
    };

    let mut src_path_buf = [0u8; PATH_SIZE];
    let Some(src_path_len) = resolve_path(cwd_str(cwd, cwd_len), src_arg, &mut src_path_buf) else {
        print_line("cp: path too long");
        return;
    };
    let Ok(src_path) = core::str::from_utf8(&src_path_buf[..src_path_len]) else {
        print_line("cp: path too long");
        return;
    };

    let mut content = [0u8; CAT_BUFFER_SIZE];
    let size = match fs_read_file(src_path, &mut content) {
        NO_FS => {
            print_no_fs();
            return;
        }
        code if code >= FS_ERR_MIN => {
            print_fs_error("cp", code);
            return;
        }
        n => n,
    };
    // Same truncation contract fs_read_file/cat already document: `size`
    // is the file's *real* size, which may exceed the buffer. Refusing
    // outright here (rather than silently copying a truncated prefix,
    // the way `cat` prints one with a notice) matches `cp`'s own
    // correctness expectation - a partial copy is a wrong copy, not
    // just an incomplete display.
    if size as usize > content.len() {
        print_line("cp: source file too large to copy (exceeds this shell's buffer)");
        return;
    }

    let mut dst_path_buf = [0u8; PATH_SIZE];
    let Some(dst_path_len) = resolve_path(cwd_str(cwd, cwd_len), dst_arg, &mut dst_path_buf) else {
        print_line("cp: path too long");
        return;
    };
    let Ok(dst_path) = core::str::from_utf8(&dst_path_buf[..dst_path_len]) else {
        print_line("cp: path too long");
        return;
    };

    match fs_write_file(dst_path, &content[..size as usize]) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("cp", code),
        _ => {}
    }
}

/// `mv <src> <dst>` - renames or moves a file or directory. Needs no new
/// buffer or content handling, unlike `cp`: it's a single syscall taking
/// two paths, since the kernel side just relinks the existing entry
/// rather than reading and rewriting any content.
fn cmd_mv(line: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
    let mut words = line.split_whitespace();
    words.next(); // "mv" itself
    let Some(src_arg) = words.next() else {
        print_line("mv: missing source argument");
        return;
    };
    let Some(dst_arg) = words.next() else {
        print_line("mv: missing destination argument");
        return;
    };

    let mut src_path_buf = [0u8; PATH_SIZE];
    let Some(src_path_len) = resolve_path(cwd_str(cwd, cwd_len), src_arg, &mut src_path_buf) else {
        print_line("mv: path too long");
        return;
    };
    let Ok(src_path) = core::str::from_utf8(&src_path_buf[..src_path_len]) else {
        print_line("mv: path too long");
        return;
    };

    let mut dst_path_buf = [0u8; PATH_SIZE];
    let Some(dst_path_len) = resolve_path(cwd_str(cwd, cwd_len), dst_arg, &mut dst_path_buf) else {
        print_line("mv: path too long");
        return;
    };
    let Ok(dst_path) = core::str::from_utf8(&dst_path_buf[..dst_path_len]) else {
        print_line("mv: path too long");
        return;
    };

    // Trivial self-move guard (`mv a a`, however spelled after path
    // resolution) - without it, the into-directory shortcut below would
    // turn `mv somedir somedir` into "move somedir *inside itself*",
    // the degenerate case of the known, documented no-cycle-detection
    // limitation, now one typo closer than it used to be. Byte equality
    // of two runtime buffers - not a comparison against a literal, so
    // relocation-safe by the same reasoning `selftest` proves.
    if src_path_buf[..src_path_len] == dst_path_buf[..dst_path_len] {
        print_line("mv: source and destination are the same");
        return;
    }

    // Real `mv`'s most common convenience beyond a plain rename:
    // `mv file dir` moves *into* an existing directory, keeping the
    // source's basename. Probed with the same fs_list_dir trick cmd_cd
    // uses (there's still no dedicated "does this exist" syscall): a
    // successful listing means dst is an existing directory, so the real
    // destination becomes dst/<basename of the resolved source>; any
    // error code just means "not a directory" and dst is used as-is.
    let mut scratch = [0u8; 8];
    let mut final_dst_buf = [0u8; PATH_SIZE];
    final_dst_buf[..dst_path_len].copy_from_slice(&dst_path_buf[..dst_path_len]);
    let mut final_dst_len = dst_path_len;
    match fs_list_dir(dst_path, &mut scratch) {
        NO_FS => {
            print_no_fs();
            return;
        }
        code if code >= FS_ERR_MIN => {}
        _ => {
            let src_bytes = &src_path_buf[..src_path_len];
            let base_start = src_bytes.iter().rposition(|&b| b == b'/').map(|i| i + 1).unwrap_or(0);
            let base = &src_bytes[base_start..];
            // Root ("/") already ends in the separator - don't double it.
            if !(final_dst_len == 1 && final_dst_buf[0] == b'/') {
                if final_dst_len >= final_dst_buf.len() {
                    print_line("mv: path too long");
                    return;
                }
                final_dst_buf[final_dst_len] = b'/';
                final_dst_len += 1;
            }
            if final_dst_len + base.len() > final_dst_buf.len() {
                print_line("mv: path too long");
                return;
            }
            final_dst_buf[final_dst_len..final_dst_len + base.len()].copy_from_slice(base);
            final_dst_len += base.len();
        }
    }
    let Ok(final_dst) = core::str::from_utf8(&final_dst_buf[..final_dst_len]) else {
        print_line("mv: path too long");
        return;
    };

    match fs_mv(src_path, final_dst) {
        NO_FS => print_no_fs(),
        code if code >= FS_ERR_MIN => print_fs_error("mv", code),
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
    /// Captured for a pending redirect. `len` counts stored bytes;
    /// once the buffer is full, further bytes are discarded (not
    /// wrapped) and `overflowed` is set so the redirect can refuse to
    /// write a silently-incomplete file - `cp`'s "a partial copy is a
    /// wrong copy" reasoning, not `cat`'s truncate-and-note.
    Capture { buf: &'a mut [u8; CAPTURE_SIZE], len: usize, overflowed: bool },
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
    for b in s.bytes() {
        putc(b);
    }
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

fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

/// One filesystem-server round trip: packs an `FSOP_*` request (op +
/// six little-endian u64 args - see the protocol section in
/// `syscall-abi`) and `MSG_CALL`s it to the server's fixed slot
/// ([`syscall_abi::FSD_TASK`]), returning the reply's u64 - which
/// carries exactly the old fs_* syscalls' return-value semantics, so
/// every caller (`cmd_ls`/`cmd_cat`/redirects/[`print_fs_error`])
/// works unchanged. The two call-layer failures are folded into that
/// same space: no server this boot (the call answers
/// `TASK_ERR_NO_SUCH_TASK`) becomes [`NO_FS`] - which is literally
/// true - and anything else (Ctrl+C mid-call, a full server mailbox)
/// becomes the generic [`FS_ERROR`].
fn fs_call(op: u64, args: [u64; 6]) -> u64 {
    let mut req = [0u8; 56];
    req[0..8].copy_from_slice(&op.to_le_bytes());
    let mut i = 0;
    while i < 6 {
        let at = 8 + i * 8;
        req[at..at + 8].copy_from_slice(&args[i].to_le_bytes());
        i += 1;
    }
    let mut reply = [0u8; 64];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::FSD_TASK,
        req.as_ptr() as u64,
        req.len() as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed == syscall_abi::TASK_ERR_NO_SUCH_TASK {
        return NO_FS;
    }
    if packed >= FS_ERR_MIN {
        return syscall_abi::FS_ERROR;
    }
    let len = (packed & 0xffff_ffff) as usize;
    if len < 8 {
        return syscall_abi::FS_ERROR;
    }
    u64::from_le_bytes([
        reply[0], reply[1], reply[2], reply[3], reply[4], reply[5], reply[6], reply[7],
    ])
}

/// Lists `path`'s directory entries into `buf` as `name\n`/`name/\n` -
/// same format and truncation behavior as ever (the server implements
/// the old kernel handler verbatim). Returns a byte count on success,
/// [`NO_FS`], or a specific `FS_ERR_*` code (anything `>= FS_ERR_MIN`)
/// (callers match on this directly - see [`cmd_ls`] and
/// [`print_fs_error`] - so every failure reason gets its own accurate
/// message). Every wrapper below is one [`fs_call`] round trip to the
/// filesystem server: the shell's "libc layer" over IPC now, not
/// syscalls; the contracts are unchanged.
fn fs_list_dir(path: &str, buf: &mut [u8]) -> u64 {
    fs_call(
        syscall_abi::FSOP_LIST_DIR,
        [path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0],
    )
}

/// Reads `path`'s contents into `buf`. Returns the file's *real* size
/// on success (which may exceed `buf.len()` - compare to detect
/// truncation), [`NO_FS`], or a specific `FS_ERR_*` code - same
/// contract as ever, same reasoning as [`fs_list_dir`].
fn fs_read_file(path: &str, buf: &mut [u8]) -> u64 {
    fs_call(
        syscall_abi::FSOP_READ_FILE,
        [path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0],
    )
}

/// Reads up to `buf.len()` bytes of `path` starting at byte `offset`,
/// returning how many were copied (`0` at/past end of file) - the
/// chunked-read primitive [`cmd_exec`]'s two-step spawn flow loops
/// over. Same error space as [`fs_read_file`].
fn fs_read_at(path: &str, offset: u64, buf: &mut [u8]) -> u64 {
    fs_call(
        syscall_abi::FSOP_READ_AT,
        [path.as_ptr() as u64, path.len() as u64, offset, buf.as_mut_ptr() as u64, buf.len() as u64, 0],
    )
}

/// Creates an empty directory at `path`. Returns `0` on success,
/// [`NO_FS`], or a specific `FS_ERR_*` code for the real failure
/// reason (already exists, invalid 8.3 name, parent missing, disk
/// full, ... - see [`print_fs_error`]).
fn fs_mkdir(path: &str) -> u64 {
    fs_call(syscall_abi::FSOP_MKDIR, [path.as_ptr() as u64, path.len() as u64, 0, 0, 0, 0])
}

/// Removes the empty directory at `path`. Same return contract as
/// [`fs_mkdir`].
fn fs_rmdir(path: &str) -> u64 {
    fs_call(syscall_abi::FSOP_RMDIR, [path.as_ptr() as u64, path.len() as u64, 0, 0, 0, 0])
}

/// Creates an empty file at `path`, or succeeds as a no-op if a file
/// already exists there. Same return contract as [`fs_mkdir`].
fn fs_touch(path: &str) -> u64 {
    fs_call(syscall_abi::FSOP_TOUCH, [path.as_ptr() as u64, path.len() as u64, 0, 0, 0, 0])
}

/// Removes the file at `path`. Same return contract as [`fs_mkdir`].
fn fs_rm(path: &str) -> u64 {
    fs_call(syscall_abi::FSOP_RM, [path.as_ptr() as u64, path.len() as u64, 0, 0, 0, 0])
}

/// Creates or fully overwrites the file at `path` with `data`. Same
/// return contract as [`fs_mkdir`].
fn fs_write_file(path: &str, data: &[u8]) -> u64 {
    fs_call(
        syscall_abi::FSOP_WRITE_FILE,
        [path.as_ptr() as u64, path.len() as u64, data.as_ptr() as u64, data.len() as u64, 0, 0],
    )
}

/// Renames or moves the file or directory at `src` to `dst`. Same
/// return contract as [`fs_mkdir`].
fn fs_mv(src: &str, dst: &str) -> u64 {
    fs_call(
        syscall_abi::FSOP_MV,
        [src.as_ptr() as u64, src.len() as u64, dst.as_ptr() as u64, dst.len() as u64, 0, 0],
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

/// `mount` - rescans the USB ports for a storage device that attached
/// after boot and mounts its FAT32 filesystem. The Parallels workflow:
/// a passed-through stick appears a few seconds after the VM starts,
/// later than the kernel's boot-time scan - boot, wait a moment, type
/// `mount`, and the disk commands come alive.
fn cmd_mount() {
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
    match fs_call(syscall_abi::FSOP_MOUNT, [0; 6]) {
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
    match fs_call(syscall_abi::FSOP_MOUNT, [0; 6]) {
        0 => print_line("mounted - disk commands available"),
        NO_FS => print_line("mount: device found, but no mountable FAT32 filesystem on it (see the server's log line)"),
        _ => print_line("mount: unexpected return"),
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
    let mut buf = [0u8; 64];
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
    syscall(syscall_abi::PUTC, byte as u64);
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

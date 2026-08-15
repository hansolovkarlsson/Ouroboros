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
//! See `docs/processes.md` for the full syscall table.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const BUFFER_SIZE: usize = 128;
const CWD_SIZE: usize = 128;
const PATH_SIZE: usize = 128;
const LIST_BUFFER_SIZE: usize = 256;
const CAT_BUFFER_SIZE: usize = 256;

const BACKSPACE: u8 = 0x08;
const DEL: u8 = 0x7f;
const CR: u8 = b'\r';
const LF: u8 = b'\n';

// Syscall numbers and sentinel values come from the shared `syscall-abi`
// crate now, not hand-duplicated local consts - see its doc comment and
// `kernel/src/syscall.rs`'s dispatch table, the other side of this ABI.
use syscall_abi::{FS_ERROR, NO_CHAR, NO_FS};

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
        match try_read_char() {
            Some(byte) => on_byte(byte, &mut buf, &mut len, &mut cwd, &mut cwd_len),
            None => wfe(),
        }
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
            // buf[..*len] is whatever bytes try_read_char returned,
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
            if *len < BUFFER_SIZE {
                buf[*len] = byte;
                *len += 1;
                putc(byte);
            }
            // Buffer full: silently drop further bytes, same as before.
        }
    }
}

/// Tokenizes on whitespace (no quoting - `echo "a b"` sees two words, not
/// one) and dispatches to a builtin by name. An empty line (just Enter,
/// or a line of only spaces) does nothing, same as a real shell.
fn run_line(line: &str, cwd: &mut [u8; CWD_SIZE], cwd_len: &mut usize) {
    let mut words = line.split_whitespace();
    let Some(command) = words.next() else { return };
    let arg = words.next().unwrap_or("");

    match command {
        "help" => print_line("commands: help, echo, uptime, clear, ls, cat, cd, pwd, mkdir, rmdir"),
        "echo" => {
            let mut first = true;
            for word in line.split_whitespace().skip(1) {
                if !first {
                    putc(b' ');
                }
                for b in word.bytes() {
                    putc(b);
                }
                first = false;
            }
            putc(CR);
            putc(LF);
        }
        "uptime" => {
            print_u64_decimal(get_ticks());
            print_line(" ticks since boot");
        }
        "clear" => {
            // ANSI clear-screen + cursor-home - the shell's own escape
            // sequence, not a syscall; the console itself has no notion
            // of a screen, just a byte stream.
            for &b in b"\x1b[2J\x1b[H" {
                putc(b);
            }
        }
        "pwd" => print_line(cwd_str(cwd, *cwd_len)),
        "ls" => cmd_ls(arg, cwd, *cwd_len),
        "cat" => cmd_cat(arg, cwd, *cwd_len),
        "cd" => cmd_cd(arg, cwd, cwd_len),
        "mkdir" => cmd_mkdir(arg, cwd, *cwd_len),
        "rmdir" => cmd_rmdir(arg, cwd, *cwd_len),
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

fn cmd_ls(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
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
        FS_ERROR => print_line("ls: no such directory"),
        n => {
            for &b in &listing[..n as usize] {
                putc(b);
            }
        }
    }
}

fn cmd_cat(arg: &str, cwd: &[u8; CWD_SIZE], cwd_len: usize) {
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
        FS_ERROR => print_line("cat: no such file"),
        size => {
            let n = (size as usize).min(file_buf.len());
            for &b in &file_buf[..n] {
                putc(b);
            }
            if !file_buf[..n].ends_with(b"\n") {
                putc(CR);
                putc(LF);
            }
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
        FS_ERROR => {
            print_line("cd: no such directory");
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
        FS_ERROR => print_line("mkdir: failed (already exists, bad name, parent missing, or disk full)"),
        _ => {}
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
        FS_ERROR => print_line("rmdir: failed (no such directory, not empty, or is root)"),
        _ => {}
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

/// Hand-rolled rather than `write!`/`core::fmt::Arguments`: that machinery
/// builds its per-argument dispatch out of *data* (an array of function
/// pointers, one per formatted argument) rather than direct `bl` calls -
/// fine under a real relocating loader, but this one applies none (see
/// `linker.ld`'s doc comment and `docs/processes.md`). A binary linked for
/// base `0x0` but loaded somewhere else (always, in practice - see
/// `loader.rs`) has no way to know those embedded pointer values need
/// correcting, so they point at whatever the link-time address `0x0`
/// would have meant - resulting in exactly the crash this replaced
/// (`ELR_EL1` landing on a tiny near-null address instead of real code,
/// confirmed directly by trying `write!` here first). Direct calls
/// (`putc`, `print_str`, this function) compile to PC-relative `bl` and
/// have no such problem - so the fix is avoiding `core::fmt` entirely for
/// anything a loaded program formats, not just here.
fn print_u64_decimal(mut n: u64) {
    if n == 0 {
        putc(b'0');
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
        putc(digits[count]);
    }
}

fn get_ticks() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

/// Lists `path`'s directory entries into `buf` as `name\n`/`name/\n` -
/// see `syscall.rs::fs_list_dir` for the exact format and truncation
/// behavior. Returns the raw syscall result: a byte count on success,
/// [`NO_FS`] if there's no mounted filesystem, or [`FS_ERROR`] if `path`
/// isn't a directory - callers match on this directly (see [`cmd_ls`])
/// rather than collapsing the two failure cases into one, so `ls`/`cd`
/// can tell "nothing's mounted" apart from "that path doesn't exist".
fn fs_list_dir(path: &str, buf: &mut [u8]) -> u64 {
    syscall4(syscall_abi::FS_LIST_DIR, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64)
}

/// Reads `path`'s contents into `buf`. Returns the raw syscall result:
/// the file's *real* size on success (which may exceed `buf.len()` -
/// compare to detect truncation, same contract as
/// `fat32::Fs::read_file`/`syscall.rs::fs_read_file`), [`NO_FS`], or
/// [`FS_ERROR`] - same reasoning as [`fs_list_dir`].
fn fs_read_file(path: &str, buf: &mut [u8]) -> u64 {
    syscall4(syscall_abi::FS_READ_FILE, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64)
}

/// Creates an empty directory at `path`. Returns the raw syscall result:
/// `0` on success, [`NO_FS`], or [`FS_ERROR`] - the kernel still
/// collapses every *non-`NO_FS`* failure reason (already exists, invalid
/// 8.3 name, parent missing, disk full, ...) into that one `FS_ERROR`
/// sentinel (see `syscall.rs::fs_mkdir`), so this program can't yet
/// report *why* beyond "no filesystem" vs. "some other failure".
fn fs_mkdir(path: &str) -> u64 {
    syscall4(syscall_abi::FS_MKDIR, path.as_ptr() as u64, path.len() as u64, 0, 0)
}

/// Removes the empty directory at `path`. Same return contract as
/// [`fs_mkdir`].
fn fs_rmdir(path: &str) -> u64 {
    syscall4(syscall_abi::FS_RMDIR, path.as_ptr() as u64, path.len() as u64, 0, 0)
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

fn try_read_char() -> Option<u8> {
    match syscall(syscall_abi::TRY_READ_CHAR, 0) {
        NO_CHAR => None,
        byte => Some(byte as u8),
    }
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

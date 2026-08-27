//! `write <file> [words...]` - create/overwrite `file` with the given words
//! joined by single spaces (no trailing newline). Externalized from a shell
//! builtin: the old builtin already word-split and rejoined with single spaces
//! (it never preserved raw spacing), so an argv-based `/bin/write` is
//! behaviour-identical. Resolves the path against the shell-delivered cwd and
//! writes via `ulib`'s bulk path (`fs_write_bulk`, `GRANT_READ`).
//!
//! For quick file creation; the content is bounded by the argv the shell stages
//! (`ARGV_MAX`) and the bulk-write cap (`SAFECOPY_MAX`). Larger or exact-byte
//! content is what `writeat`/redirection are for.

#![no_std]
#![no_main]

const MAX_CONTENT: usize = 512;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: write <file> [words...]  (write space-joined words to a file)\r\n");
    // argv[1] = the file; argv[2..] = the words to write.
    let mut namebuf = [0u8; ulib::PATH_MAX];
    let Some(name_len) = ulib::arg(1, &mut namebuf) else {
        ulib::con_write(b"write: missing file argument\r\n");
        ulib::exit(1);
    };
    let name = core::str::from_utf8(&namebuf[..name_len]).unwrap_or("");

    // Join argv[2..] with single spaces, then a newline.
    let mut content = [0u8; MAX_CONTENT];
    let mut len = 0usize;
    let argc = ulib::argc();
    let mut i = 2u64;
    let mut first = true;
    let mut word = [0u8; MAX_CONTENT];
    while i < argc {
        if let Some(wl) = ulib::arg(i, &mut word) {
            if !first && len < content.len() {
                content[len] = b' ';
                len += 1;
            }
            for &b in &word[..wl] {
                if len < content.len() {
                    content[len] = b;
                    len += 1;
                }
            }
            first = false;
        }
        i += 1;
    }
    // No trailing newline: the content is exactly the space-joined words,
    // byte-for-byte what the old shell builtin wrote (and `write <file>` with
    // no words truncates to empty - a valid case, not an error).

    // Resolve against the shell-delivered cwd.
    let mut cwdbuf = [0u8; ulib::PATH_MAX];
    let cwd_len = ulib::cwd(&mut cwdbuf);
    let cwd = core::str::from_utf8(&cwdbuf[..cwd_len]).unwrap_or("/");
    let mut pathbuf = [0u8; ulib::PATH_MAX];
    let Some(plen) = ulib::resolve(cwd, name, &mut pathbuf) else {
        ulib::con_write(b"write: path too long\r\n");
        ulib::exit(1);
    };
    let path = core::str::from_utf8(&pathbuf[..plen]).unwrap_or("");

    let r = ulib::fs_write_bulk(path, &content[..len]);
    if ulib::is_fs_error(r) {
        ulib::fs_error("write", r);
        ulib::exit(1);
    }
    ulib::exit(0);
}

//! `groupadd <name> [-g <gid>]` - create a group in `/etc/group` (root only).
//!
//! Appends a `name:gid:members` line (empty member list). The gid is the given
//! `-g` value or the next free gid at/above `accounts::FIRST_NORMAL_ID`.
//! **Root only** (the option-1 account-mutation model: admin tools refuse a
//! non-root caller; self-service is a later `accountd` tier). All parsing is
//! byte-only (PIE-safe). ext2 enforces the write's permission (root bypasses);
//! on FAT/exFAT the write is unrestricted.

#![no_std]
#![no_main]

const GROUP_FILE: &str = "/etc/group";
const BUF: usize = syscall_abi::SAFECOPY_MAX as usize;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    ulib::usage_if_requested(b"usage: groupadd <name> [-g <gid>]  (root only)\r\n");
    if ulib::getuid() != 0 {
        die(b"groupadd: only root may add groups\r\n");
    }

    let mut namebuf = [0u8; 33];
    let mut name_len = 0usize;
    let mut gid_opt: Option<u32> = None;
    let mut i = 1u64;
    let argc = ulib::argc();
    while i < argc {
        let mut a = [0u8; 33];
        let n = ulib::arg(i, &mut a).unwrap_or(0);
        let tok = &a[..n];
        if tok == b"-g" {
            i += 1;
            let mut g = [0u8; 12];
            let gn = ulib::arg(i, &mut g).unwrap_or(0);
            match accounts::parse_dec(&g[..gn]) {
                Some(v) => gid_opt = Some(v as u32),
                None => die(b"groupadd: -g needs a numeric gid\r\n"),
            }
        } else if name_len == 0 && !tok.is_empty() {
            namebuf[..n].copy_from_slice(tok);
            name_len = n;
        }
        i += 1;
    }
    if name_len == 0 {
        die(b"usage: groupadd <name> [-g <gid>]\r\n");
    }
    let name = &namebuf[..name_len];

    let mut src = [0u8; BUF];
    let Some(slen) = ulib::read_file_checked(GROUP_FILE, &mut src) else {
        // This buffer is rewritten back over /etc/group: a 0 from a transient
        // error or an over-long file would replace every group with one line.
        die(b"groupadd: could not read /etc/group - refusing to rewrite it\r\n");
    };
    if accounts::group_exists(&src[..slen], name) {
        die(b"groupadd: group already exists\r\n");
    }
    let gid = gid_opt.unwrap_or_else(|| accounts::next_free_gid(&src[..slen]));

    let mut line = [0u8; 64];
    let Some(llen) = accounts::format_group_line(&mut line, name, gid, b"") else {
        die(b"groupadd: name too long\r\n");
    };
    let mut out = [0u8; BUF];
    let Some(olen) = accounts::append_line(&src[..slen], &mut out, &line[..llen]) else {
        die(b"groupadd: /etc/group full (raise the account-file cap)\r\n");
    };
    let code = ulib::fs_write_bulk(GROUP_FILE, &out[..olen]);
    if ulib::is_fs_error(code) {
        ulib::fs_error("groupadd", code);
        ulib::exit(1);
    }
    ulib::con_write(b"groupadd: created group ");
    ulib::con_write(name);
    ulib::con_write(b"\r\n");
    ulib::exit(0);
}

fn die(msg: &[u8]) -> ! {
    ulib::con_write(msg);
    ulib::exit(1);
}

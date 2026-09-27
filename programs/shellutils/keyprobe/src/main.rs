//! `keyprobe` - try `netd`'s held-key ops as an ORDINARY user, and check each
//! is refused: step 4 of `docs/roadmap/roadmap-user-keys.md`.
//!
//! `netd` holds the cluster keys of the users logged in on this node, and every
//! spawned program holds a send right to `netd` (the shell grants it to all of
//! them, since it cannot tell which need the network). So the table's safety
//! rests on `netd`'s own checks of who is asking, and this is the task that
//! asks without being entitled to:
//!
//! - `NETOP_KEY_HOLD` must be `NET_KEY_DENIED` (only root fills the table);
//! - `NETOP_KEY_LIST` must be `NET_KEY_DENIED` (root only);
//! - `NETOP_KEY_DROP` of every handle must be `DENIED` or `NOT_HELD`, never
//!   `OK` (only the holder or root drops a key);
//! - `NETOP_KEY_DROP_MINE` must drop nothing (this task holds nothing).
//!
//! **It refuses to run as root**, where every one of these may legitimately
//! succeed: root may drop anyone's key, so a root run would destroy the state
//! it is meant to observe. One `[ok]`/`[FAIL]` line a check; the exit code is
//! the number of failures.

#![no_std]
#![no_main]

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    let target = ulib::stdout_target();
    if ulib::getuid() == 0 {
        ulib::write_out(target, b"keyprobe: run it as an ordinary user; root may drop any key\r\n");
        ulib::end_of_stream(target);
        ulib::exit(2);
    }
    let mut failures = 0u64;
    let mut check = |ok: bool, what: &[u8], status: u64| {
        ulib::write_out(target, if ok { b"[ok]   " } else { b"[FAIL] " });
        ulib::write_out(target, what);
        ulib::write_out(target, b": status ");
        let mut buf = [0u8; 24];
        let mut n = 0;
        ulib::emit_dec(&mut buf, &mut n, status);
        ulib::write_out(target, &buf[..n]);
        ulib::write_out(target, b"\r\n");
        if !ok {
            failures += 1;
        }
    };

    // A hold for my own uid, with a seed of no value.
    let mut req = [0u8; 48];
    req[..8].copy_from_slice(&syscall_abi::NETOP_KEY_HOLD.to_le_bytes());
    req[8..16].copy_from_slice(&(ulib::getuid() as u64).to_le_bytes());
    let st = call(&req);
    check(st == syscall_abi::NET_KEY_DENIED, b"a hold from a non-root task is refused", st);

    let st = call(&syscall_abi::NETOP_KEY_LIST.to_le_bytes());
    check(st == syscall_abi::NET_KEY_DENIED, b"a list from a non-root task is refused", st);

    for h in 0..syscall_abi::NET_KEY_MAX as u64 {
        let mut d = [0u8; 16];
        d[..8].copy_from_slice(&syscall_abi::NETOP_KEY_DROP.to_le_bytes());
        d[8..].copy_from_slice(&h.to_le_bytes());
        let st = call(&d);
        check(
            st == syscall_abi::NET_KEY_DENIED || st == syscall_abi::NET_KEY_NOT_HELD,
            b"a drop of another task's handle is refused",
            st,
        );
    }

    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let st = call_into(&syscall_abi::NETOP_KEY_DROP_MINE.to_le_bytes(), &mut reply);
    let n = u64::from_le_bytes(reply[8..16].try_into().unwrap_or([0xff; 8]));
    check(st == syscall_abi::NET_KEY_OK && n == 0, b"a drop of mine drops nothing I do not hold", n);

    ulib::end_of_stream(target);
    ulib::exit(failures);
}

/// Send `req` to `netd` and return the reply's status word (`u64::MAX` for no
/// answer).
fn call(req: &[u8]) -> u64 {
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    call_into(req, &mut reply)
}

/// Through `ulib::net_call`, as every call to `netd` from a spawned command must
/// be: the shell delegates `TO_NET` only after the spawn, and a call in that
/// window is refused `MSG_ERR_DENIED`, which `net_call` rides out. This used a
/// raw `MSG_CALL` at first, and every check failed with no answer at all.
/// `reply` is MSG_MAX_LEN, as MSG_CALL requires of every reply buffer; with
/// 16 bytes every call came back 0xffff_ffff_ffff_ffff.
fn call_into(req: &[u8], reply: &mut [u8; syscall_abi::MSG_MAX_LEN as usize]) -> u64 {
    let packed = ulib::net_call(req, reply);
    if packed >= syscall_abi::FS_ERR_MIN || (packed & 0xffff_ffff) < 8 {
        // Say what came back rather than a bare "no answer": a failed call and
        // a refusal are different findings.
        let target = ulib::stdout_target();
        ulib::write_out(target, b"keyprobe: MSG_CALL to netd returned 0x");
        let mut hex = [0u8; 16];
        for (i, h) in hex.iter_mut().enumerate() {
            let nib = (packed >> (60 - 4 * i)) & 0xf;
            *h = b"0123456789abcdef"[nib as usize];
        }
        ulib::write_out(target, &hex);
        ulib::write_out(target, b"\r\n");
        return u64::MAX;
    }
    u64::from_le_bytes(reply[..8].try_into().unwrap_or([0xff; 8]))
}

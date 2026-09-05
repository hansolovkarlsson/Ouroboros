//! Namespace resolution for the C fd path — the one piece of `ulib`'s routing
//! that a C program cannot reach.
//!
//! **Why this crate exists.** Step 3 of `docs/roadmap-fid-verbs.md`: a C
//! `open("/mnt/a/F")` today goes straight to `fsd`, which knows nothing about
//! `/mnt/a` — that is a *namespace* binding, and only `ninep_abi::resolve_ns`
//! knows how to read one. `resolve_ns` is deliberately the **single source** for
//! that logic, shared by `ulib` and `netd`, and C cannot call it. A third
//! hand-written copy in C would be *behaviour*, not constants, so
//! `scripts/check-wire-constants.py`'s precedent does not carry: nothing would
//! compare the copies.
//!
//! **The toolchain cost, stated because it is real.** The C programs link no
//! other Rust — they are clang + LLD against `programs/linker.ld` with `-pie`,
//! and the hard constraint of that link is **no `R_AARCH64_ABS64`**, the same
//! constraint that makes `alloc`'s collections unlinkable here. This crate was
//! therefore built as a **build gate first**: a trivial `x + 1`, linked and
//! relocation-checked, before any logic was written against the assumption that
//! it would work. The precedent is deliberate — a one-build gate proved `alloc`
//! could not be PIE-linked on stable before a week was spent on it
//! (`docs/capability-and-hardening-postmortem.md`).
//!
//! The gate then had to be made **representative**, which mattered more than
//! passing it: `x + 1` proves nothing about `resolve_ns`, and the ABS64 risk
//! lives in exactly the table-and-slice code the trivial version omitted. What
//! is checked is this file as it stands, calling the real resolver.
#![no_std]

/// `target` values written back to C. Deliberately small integers rather than a
/// repr(C) enum: the C side switches on them, and a plain scalar cannot grow a
/// relocation the way a pointer table can.
pub const NS_TARGET_FSD: u32 = 0;
pub const NS_TARGET_CONSOLE: u32 = 1;
pub const NS_TARGET_NETLOCAL: u32 = 2;
pub const NS_TARGET_REMOTE: u32 = 3;

#[inline(always)]
unsafe fn syscall3(number: u64, a0: u64, a1: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "svc #0",
        in("x8") number,
        inlateout("x0") a0 => ret,
        in("x1") a1,
        options(nostack),
    );
    ret
}

/// Resolve `path` through *this task's* namespace.
///
/// `path` / `path_len` is the absolute path; the server-side path is written to
/// `out` / `out_cap` and its length returned in `*out_len`. `*target` gets one
/// of the `NS_TARGET_*` values, and for a remote mount `endpoint` receives the
/// 6-byte `[ip:4][port:2 LE]`.
///
/// Returns 0 on success, or -1 if a buffer was too small. Every pointer is
/// checked for null, because the caller is C.
///
/// # Safety
/// The pointers must be valid for the lengths given, as usual for a C ABI.
#[no_mangle]
pub unsafe extern "C" fn ouro_ns_resolve(
    path: *const u8,
    path_len: usize,
    out: *mut u8,
    out_cap: usize,
    out_len: *mut usize,
    target: *mut u32,
    endpoint: *mut u8,
) -> i32 {
    if path.is_null() || out.is_null() || out_len.is_null() || target.is_null() {
        return -1;
    }
    // The namespace blob, fetched per call - the same choice `ulib`'s
    // `mount_resolve` makes, and for the same reason: the blob is small and
    // every fs op is already an IPC round trip, so caching it would only add a
    // way to be stale after a `bind`.
    let mut ns = [0u8; syscall_abi::NS_MAX as usize];
    let nlen = syscall3(
        syscall_abi::GET_NS,
        ns.as_mut_ptr() as u64,
        ns.len() as u64,
    ) as usize;
    if nlen > ns.len() {
        return -1;
    }

    let path_slice = core::slice::from_raw_parts(path, path_len);
    let out_slice = core::slice::from_raw_parts_mut(out, out_cap);
    let r = ninep_abi::resolve_ns(&ns[..nlen], path_slice, out_slice);
    if r.len > out_cap {
        return -1;
    }
    *out_len = r.len;
    match r.target {
        ninep_abi::NsTarget::Fsd(tree) => {
            *target = NS_TARGET_FSD;
            // The tree index rides in the high half - a C caller needs it to
            // address the right mount, and it has nowhere else to go without a
            // second out-parameter.
            *target |= (tree as u32) << 8;
        }
        ninep_abi::NsTarget::Console => *target = NS_TARGET_CONSOLE,
        ninep_abi::NsTarget::NetLocal => *target = NS_TARGET_NETLOCAL,
        ninep_abi::NsTarget::Remote(ep) => {
            *target = NS_TARGET_REMOTE;
            if endpoint.is_null() {
                return -1;
            }
            core::ptr::copy_nonoverlapping(ep.as_ptr(), endpoint, ep.len());
        }
    }
    0
}

/// A `staticlib` must carry its own, and these C programs link no other Rust,
/// so there is nothing for it to collide with (`ulib`'s is for Rust programs).
/// A trap is the right abort here: a C program has no unwinder, and this crate
/// has no console to report through.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("brk #1", options(nomem, nostack)) }
    }
}

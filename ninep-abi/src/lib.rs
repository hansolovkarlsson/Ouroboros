//! `ninep-abi` — the uniform, server-agnostic file-protocol verb set for the
//! distributed-cluster arc (Phase 0; see `docs/roadmap-cluster-phase0.md`).
//!
//! The whole cluster direction rests on one insight: *"remote" is just "the
//! same protocol, over TCP instead of local IPC."* Today each server speaks its
//! own bespoke protocol (`fsd`'s `FSOP_*`, `cond`'s `DSPOP_*`); this crate
//! defines the single verb set that replaces them, so mounting a remote
//! machine's service (a later phase) is the same operation as reaching a local
//! one — the transport changes, the model doesn't.
//!
//! This first step (0a+0b) defines the verbs and makes them the real in-use
//! path between clients (`ulib` → every `/bin` filesystem command) and `fsd`,
//! which speaks them alongside `FSOP_*` during the migration. No logic here,
//! just `pub const`s — safe under either build target, every value a scalar
//! inlined at the use site (the `syscall-abi` discipline).
//!
//! ## Wire format (request)
//!
//! A request is a header + inline payload, copied task-to-task by the kernel's
//! message machinery (no pointer crosses a task boundary), exactly like
//! `FSOP_*` — with one added field, the `tree` selector at offset 8 that the
//! whole design turns on:
//!
//! | offset | field            | meaning                                        |
//! |--------|------------------|------------------------------------------------|
//! | 0      | `verb: u64`      | one of the `NP_*` constants below              |
//! | 8      | `tree: u64`      | which mount (multi-mount key / remote handle)  |
//! | 16..48 | `a0..a3: u64`    | op params (path len, offset, want, 2nd-path…)  |
//! | 48..   | payload          | path bytes, then any inline data               |
//!
//! Reply: `status: u64` at offset 0, result payload from [`NP_REPLY_PAYLOAD`] —
//! identical to `FSOP_*`, and status codes are (for now) `syscall-abi`'s
//! existing `FS_ERR_*` / size conventions, so a verb returns byte-identical
//! results to the `FSOP_*` op it replaces. Bulk file data still moves by the
//! `grant`/`safecopy` primitive (cap `SAFECOPY_MAX`), never inline; the inline
//! payload cap stays `FS_DATA_MAX` (512), all within `MSG_MAX_LEN` (768).
#![no_std]

/// Request header size: `verb`(8) + `tree`(8) + four `u64` params(32). The
/// inline payload (path, then any data) starts here. One `u64` larger than
/// `FSOP_*`'s `FS_REQ_PAYLOAD` (40) — that extra word is the `tree` selector.
pub const NP_REQ_PAYLOAD: u64 = 48;

/// Reply header size: the status `u64` at offset 0; result payload follows.
/// Same as `FSOP_*`'s `FS_REPLY_PAYLOAD`.
pub const NP_REPLY_PAYLOAD: u64 = 8;

/// Base of the verb number space. Chosen well clear of `FSOP_*` (1..=18) and
/// `SYSOP_PING` (0xFFFF) so a server can dispatch on the `verb` field and speak
/// both protocols during the migration without any collision.
pub const NP_BASE: u64 = 0x100;

/// List a directory's entries into the reply (`name\n` / `name/\n`). Params:
/// `a0` = path length, `a1` = result-window size. Status = bytes written.
pub const NP_READDIR: u64 = NP_BASE;
/// Read a file inline: the reply carries the bytes (capped by the message
/// limit). Params: `a0` = path length, `a1` = want. Status = the file's *real*
/// size (a one-byte want is the cheapest existence/kind probe).
pub const NP_READ_FILE: u64 = NP_BASE + 1;
/// Read a file via the bulk `grant`/`safecopy` path (data delivered straight
/// into the client's `GRANT_WRITE` buffer, not the reply). Params: `a0` = path
/// length, `a1` = offset, `a2` = want. Status = bytes delivered (0 at EOF).
pub const NP_READ: u64 = NP_BASE + 2;
/// Create/overwrite a file from the client's `GRANT_READ` buffer via
/// `safecopy` (empty = truncate-to-empty, no grant). Params: `a0` = path
/// length, `a1` = data length. Status = 0.
pub const NP_WRITE: u64 = NP_BASE + 3;
/// Write at a byte offset, extending without rewriting the prefix (the streaming
/// primitive), from the client's `GRANT_READ` buffer. Params: `a0` = path
/// length, `a1` = offset, `a2` = data length. Status = 0.
pub const NP_WRITE_AT: u64 = NP_BASE + 4;
/// Create an empty file. Param: `a0` = path length. Status = 0.
pub const NP_TOUCH: u64 = NP_BASE + 5;
/// Create a directory. Param: `a0` = path length. Status = 0.
pub const NP_MKDIR: u64 = NP_BASE + 6;
/// Remove an empty directory. Param: `a0` = path length. Status = 0.
pub const NP_RMDIR: u64 = NP_BASE + 7;
/// Remove a file. Param: `a0` = path length. Status = 0.
pub const NP_RM: u64 = NP_BASE + 8;
/// Rename/move (relink; no content moves). Params: `a0` = src length, `a1` =
/// dst length; payload = src bytes then dst bytes. Status = 0.
pub const NP_MV: u64 = NP_BASE + 9;
/// Read a windowed slice of a file *inline* (the reply carries the bytes): the
/// chunked-read primitive an exec loader loops over. Params: `a0` = path length,
/// `a1` = offset, `a2` = want. Status = bytes copied (0 at/past EOF).
pub const NP_READ_AT: u64 = NP_BASE + 10;
/// Create/overwrite a file with data carried *inline* in the request (bounded
/// by `FS_DATA_MAX`, 512) rather than by grant/safecopy - the small-write path.
/// Params: `a0` = path length, `a1` = data length; payload = path then data.
/// Status = 0.
pub const NP_WRITE_FILE: u64 = NP_BASE + 11;

/// One past the last defined verb — a server dispatches the `[NP_BASE, NP_LIMIT)`
/// range to its verb handler and lets everything else (including `SYSOP_PING`)
/// fall through to its existing path.
pub const NP_LIMIT: u64 = NP_WRITE_FILE + 1;

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
//! | 16..40 | `a0..a2: u64`    | op params (path len, offset, want, 2nd-path…)  |
//! | 40     | `a3: u64`        | WHO the request is for (see `NP_ID_*`)         |
//! | 48..   | payload          | path bytes, then any inline data               |
//!
//! Reply: `status: u64` at offset 0, result payload from [`NP_REPLY_PAYLOAD`] —
//! identical to `FSOP_*`, and status codes are (for now) `syscall-abi`'s
//! existing `FS_ERR_*` / size conventions, so a verb returns byte-identical
//! results to the `FSOP_*` op it replaces. Bulk file data still moves by the
//! `grant`/`safecopy` primitive (cap `SAFECOPY_MAX`), never inline; the inline
//! payload cap stays `FS_DATA_MAX` (512), all within `MSG_MAX_LEN` (768).
#![no_std]

/// Request header size: `verb`(8) + `tree`(8) + four `u64` params(32) - the
/// last of which (`a3`) is the identity word, not an op param. The
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

/// **Stat** a single path: `a0` = path length; payload = the path. On success
/// the reply status is [`STAT_INFO_LEN`] and the result payload is a
/// [`StatInfo`](the `STAT_*` field layout below); on failure it's an `FS_ERR_*`
/// code. The per-file metadata behind `ls -l` (size, dir flag, modified time,
/// and — when the filesystem can model them — POSIX mode/owner).
pub const NP_STAT: u64 = NP_BASE + 12;

/// **Change mode** of a single path (the write side of `stat`'s mode field):
/// `a0` = path length, `a1` = the new POSIX permission bits (the low 12 of
/// `i_mode`; the `S_IFMT` type nibble is preserved by the server). Payload =
/// the path. Status = 0, or `FS_ERR_NOT_SUPPORTED` on a filesystem that can't
/// model a mode (FAT32/exFAT/`/proc`). Backs `chmod`.
pub const NP_CHMOD: u64 = NP_BASE + 13;

/// **Change owner** of a single path: `a0` = path length, `a1` = new uid,
/// `a2` = new gid (each `u16`; `u64::MAX` means "leave unchanged", so uid-only
/// and gid-only changes need no separate verb). Payload = the path. Status = 0,
/// or `FS_ERR_NOT_SUPPORTED` where owners can't be modeled. Backs `chown`.
pub const NP_CHOWN: u64 = NP_BASE + 14;

// --- fids: server-side open-file handles (a POSIX fd ≈ a 9P fid) ----------
// These coexist with the path-per-op verbs above; the C libc uses them, other
// clients (shell/ulib/cluster export) stay path-based. A fid is opened once
// (NP_OPEN authorizes the access against the file's mode/owner), then read/
// written by handle, and freed with NP_CLUNK. The cursor stays client-side and
// rides each PREAD/PWRITE offset (authentic 9P: reads/writes carry the offset).

/// **Open** a file, returning a fid (a small integer >= 3, usable directly as a
/// C fd). `a0` = the [`OPEN_*`] flag bits, `a1` = path length; payload = the
/// path. Status = the fid on success, or an `FS_ERR_*` code (permission is
/// checked here, once, per the flags). The fid remembers the file for this
/// client until [`NP_CLUNK`].
pub const NP_OPEN: u64 = NP_BASE + 15;

/// **Read** from a fid at an explicit offset: `a0` = fid, `a1` = offset,
/// `a2` = count. The reply carries the bytes inline; status = bytes read
/// (`0` at EOF). No per-op permission check - the fid was authorized at open.
pub const NP_PREAD: u64 = NP_BASE + 16;

/// **Write** to a fid at an explicit offset from the client's `GRANT_READ`
/// buffer: `a0` = fid, `a1` = offset, `a2` = count. Status = bytes written.
pub const NP_PWRITE: u64 = NP_BASE + 17;

/// **Stat** a fid (the [`NP_STAT`] record for the open file): `a0` = fid.
/// Status = [`STAT_INFO_LEN`], result = the record.
pub const NP_FSTAT: u64 = NP_BASE + 18;

/// **Close** a fid, freeing the server-side handle: `a0` = fid. Status = 0.
pub const NP_CLUNK: u64 = NP_BASE + 19;

/// [`NP_OPEN`] flag: open for reading (needs `r` on the file).
pub const OPEN_READ: u64 = 1;
/// [`NP_OPEN`] flag: open for writing (needs `w`).
pub const OPEN_WRITE: u64 = 2;
/// [`NP_OPEN`] flag: create the file if absent (needs `w` on the parent).
pub const OPEN_CREATE: u64 = 4;
/// [`NP_OPEN`] flag: truncate the file to empty on open.
pub const OPEN_TRUNC: u64 = 8;

/// One past the last defined verb — a server dispatches the `[NP_BASE, NP_LIMIT)`
/// range to its verb handler and lets everything else (including `SYSOP_PING`)
/// fall through to its existing path.
pub const NP_LIMIT: u64 = NP_CLUNK + 1;

// The `NP_STAT` result payload: a fixed 27-byte little-endian record. The time
// is a broken-down calendar (not an epoch) so no filesystem's differing epoch
// leaks into the ABI and the client formats it without date math; `time_valid`
// says whether the time fields are meaningful (a filesystem that doesn't yet
// surface a timestamp sets it 0). The trailing mode/uid/gid triple carries POSIX
// ownership+permission metadata, guarded by its own `mode_valid` byte: only ext2
// stores it, so FAT32/exFAT/`/proc` set the byte 0 and leave the triple zero
// (they can't model an owner or mode). The record grew from 20 to 27 bytes when
// the mode/owner surface landed; the mode fields sit *after* the original 20 so
// an old-length reader still decodes size/flags/time unchanged.
/// Total length of the `NP_STAT` result record.
pub const STAT_INFO_LEN: usize = 27;
/// `u64` file size in bytes (0 for a directory).
pub const STAT_SIZE_OFF: usize = 0;
/// `u32` flags; bit 0 ([`STAT_FLAG_DIR`]) = directory.
pub const STAT_FLAGS_OFF: usize = 8;
/// `u16` modified year (e.g. 2026).
pub const STAT_YEAR_OFF: usize = 12;
/// `u8` modified month (1-12).
pub const STAT_MONTH_OFF: usize = 14;
/// `u8` modified day (1-31).
pub const STAT_DAY_OFF: usize = 15;
/// `u8` modified hour (0-23).
pub const STAT_HOUR_OFF: usize = 16;
/// `u8` modified minute (0-59).
pub const STAT_MIN_OFF: usize = 17;
/// `u8` modified second (0-59).
pub const STAT_SEC_OFF: usize = 18;
/// `u8` non-zero if the year..second fields are meaningful.
pub const STAT_TIMEVALID_OFF: usize = 19;
/// `u16` POSIX mode: the `S_IFMT` type nibble plus the 12 permission bits
/// (`rwxrwxrwx` + setuid/setgid/sticky), exactly ext2's on-disk `i_mode`.
/// Meaningful only when [`STAT_MODEVALID_OFF`] is non-zero.
pub const STAT_MODE_OFF: usize = 20;
/// `u16` owning user id (`i_uid`). Meaningful only when [`STAT_MODEVALID_OFF`].
pub const STAT_UID_OFF: usize = 22;
/// `u16` owning group id (`i_gid`). Meaningful only when [`STAT_MODEVALID_OFF`].
pub const STAT_GID_OFF: usize = 24;
/// `u8` non-zero if the mode/uid/gid fields are real on-disk metadata (ext2);
/// zero when the filesystem can't model an owner or mode (FAT32/exFAT/`/proc`).
pub const STAT_MODEVALID_OFF: usize = 26;
/// [`STAT_FLAGS_OFF`] bit: the entry is a directory.
pub const STAT_FLAG_DIR: u32 = 1 << 0;

/// Remote-execution **run** request (cluster Phase 4a — the Plan 9 `cpu` model):
/// carried over the export connection in the same framed shape as an NP verb, but
/// its "reply" is the spawned command's **output stream** (bytes, then FIN), not a
/// single NP reply. `a0` = command-line length; the payload is the command line
/// (program name + space-separated args, e.g. `ls /` or `cat /net/ip`). The
/// export gateway spawns it on *this* machine (from `/bin`), captures its stdout,
/// and streams it back to the caller. Chosen well above `NP_LIMIT` so the fs-verb
/// dispatch never sees it — the cpu path handles it explicitly.
pub const NP_RUN: u64 = NP_BASE + 0x20;

// ---------------------------------------------------------------------------
// The verbs over TCP (cluster Phase 1: 9P-over-TCP). Locally a request is a
// kernel-copied `MSG_CALL` and bulk data moves by grant/safecopy; over a TCP
// stream there is no grant, so the same verb + params travel with data **inline
// in the stream**, length-delimited so the receiver knows where a message ends:
//
//   request:  [u32 len (LE)][verb:u64][tree:u64][a0..a3:u64][payload...]
//   reply:    [u32 len (LE)][status:u64][result...]
//
// `len` is the number of bytes that follow the 4-byte length field (the NP
// message itself: the 48-byte header + payload for a request, or the 8-byte
// status + result for a reply). A reader reads 4 bytes, then `len` more. The
// header is exactly the local NP wire; only the transport of bulk data changes
// (inline, not via a grant). See `docs/roadmap-cluster-phase1.md`.
// ---------------------------------------------------------------------------

/// The TCP port a machine's 9P export listener runs on (9P's registered port),
/// distinct from any HTTP server. A client remote-mounts `host:NP_NET_PORT`.
pub const NP_NET_PORT: u16 = 564;

/// The 4-byte length prefix on every framed message (little-endian `u32`).
pub const NP_NET_LEN_PREFIX: usize = 4;

/// Largest framed message body (the bytes after the length prefix): the 48-byte
/// header plus an inline data chunk. Sized to hold a bulk read/write chunk
/// (`SAFECOPY_MAX`) plus the header, and to fit comfortably in a couple of TCP
/// segments. A large file streams a chunk per round trip, as `cat` does locally.
pub const NP_NET_MAX: usize = 48 + 2048;

/// The namespace `tree` sentinel marking a **remote** binding (cluster Phase 1c).
/// A local binding's `tree` selects a mount *within* `fsd` (0 = the boot mount);
/// `0xFF` instead means the binding's `target` begins with a 6-byte endpoint
/// (`[ip:4][port:2 LE]`) followed by the remote-side root path, and a resolution
/// against it routes through `netd`'s [`crate::NP_NET_PORT`] export gateway
/// (via `NETOP_RMOUNT`) rather than the local `fsd`. Chosen at the top of the
/// `u8` tree space, clear of every real (small) local tree id.
pub const NS_REMOTE_TREE: u8 = 0xFF;

/// Bytes of endpoint (`[ip:4][port:2 LE]`) at the head of a remote binding's
/// `target`; the remote-side root path follows.
pub const NS_ENDPOINT_LEN: usize = 6;

/// The namespace `tree` sentinel marking a binding to the **network server's
/// `/net`** - a synthetic, read-only view of the machine's network identity
/// (`/net/ip`, `/net/mac`), served by `netd` itself (cluster Phase 3). Like the
/// console sentinel it routes to a non-fsd server (`NET_TASK`), but for *reads*:
/// `resolve_ns` returns `server = NET_TASK` with a zero endpoint (the marker that
/// distinguishes this local netd-fs from a remote mount, which always carries a
/// real endpoint). The shell's `mount -n /net` binds it; `netd`'s export
/// prefix-routes `/net` too, so `cat /mnt/a/net/ip` reads another machine's
/// address. Chosen just below [`NS_CON_TREE`].
pub const NS_NET_TREE: u8 = 0xFD;

/// The namespace `tree` sentinel marking a binding to the **console server**
/// (`cond`, `CON_TASK`) - cluster Phase 3's `/dev/cons`. Unlike a real fsd tree
/// (`/proc`'s [`NS_PROC_TREE`]), this routes to a *different server*: a write to a
/// path bound with this sentinel becomes an `NP_WRITE_FILE` to `CON_TASK` (the
/// console renders the inline bytes), and reads are refused (the console is
/// write-only). The shell's `mount -c /dev/cons` binds it; `netd`'s export
/// prefix-routes `/dev/cons` the same way, so another machine can write this
/// one's screen. Chosen just below [`NS_REMOTE_TREE`], clear of every real tree.
pub const NS_CON_TREE: u8 = 0xFE;

/// The reserved `fsd` mount-table index of the synthetic `/proc` filesystem
/// (cluster Phase 3): `fsd` auto-mounts `Filesystem::proc()` here at boot, so the
/// proc tree always exists alongside the boot disk (tree 0). A real tree number
/// (not a sentinel like [`NS_REMOTE_TREE`]) - the shell's `mount -p` binds
/// `/proc → (NS_PROC_TREE, "/")`, and `netd`'s export routes `/proc` paths to it.
/// `fsd`'s `MAX_MOUNTS` is sized to include it (`NS_PROC_TREE + 1`).
pub const NS_PROC_TREE: u8 = 4;

/// The most inline data a remote read/write chunk carries in one `NETOP_RMOUNT`
/// round trip. The NP reply body (`[status:u64][data]`) rides back in a single
/// `MSG_MAX_LEN` (768) message, so the data must leave room for the 8-byte
/// status (and a little slack). A client loops with a rising offset for a large
/// file, exactly as the local `cat` streams `SAFECOPY_MAX` chunks.
pub const NP_REMOTE_CHUNK: usize = 512;

// ---------------------------------------------------------------------------
// Cluster authentication (the export-hardening phase; see the security section
// of `docs/roadmap-cluster.md`). Every *inbound* framed export request carries
// an auth header in front of the NP message; the exporter verifies it before
// serving any verb (fs op *or* `NP_RUN`). The scheme is a **client-nonce MAC**:
//
//   framed request:  [u32 len][magic:8][nonce:16][mac:32][NP message...]
//
// where `mac = HMAC-SHA256(cluster_key, nonce || NP-message)`. The shared
// cluster secret never crosses the wire, and a peer without it cannot forge a
// request. `len` still counts every byte after the 4-byte prefix (now the auth
// header + the NP message).
//
// The *reply* is authenticated too (tier 2, mutual authentication):
//
//   framed reply:  [u32 len][mac:32][status:u64][result...]
//
// where `mac = HMAC-SHA256(cluster_key, request_nonce || [status][result])` -
// the reply is MAC'd against the SAME nonce the client signed its request with,
// which both proves the reply came from a holder of the key AND binds it to this
// specific request (a captured reply can't be replayed against another). No
// reply nonce field is needed - the request nonce is the shared per-transaction
// value both sides already hold. The client rejects a reply whose MAC doesn't
// verify (an injected/forged reply, or one from a peer without the key). This is
// integrity/authenticity, NOT confidentiality: bytes still cross in cleartext.
//
// Still out of scope (deferred to the leaving-a-trusted-network hardening,
// docs/roadmap-cluster.md): replay-of-observed-ops (a passive sniffer can replay
// a captured request verbatim; forgery of a *new* one it cannot), per-peer
// identity, transport encryption, and reply-auth for the `cpu`-run output
// *stream* (not a framed reply). See `programs/servers/netd/src/hmac.rs`.
// ---------------------------------------------------------------------------

/// Magic at the head of an authenticated framed request, distinguishing it from
/// an unauthenticated (legacy) frame. When the exporter has a key configured it
/// requires this; a frame without it is refused (fail-closed).
///
/// **Bumped to `02` when the requesting user's name joined the header** (the
/// per-user cluster identity arc). A deliberate flag day: an `01` peer's frame
/// is *refused* rather than misparsed, because the old layout's first 32
/// message bytes would otherwise be read as a username. Both nodes of a cluster
/// are built from one tree, so the cost is nil, and the alternative - a
/// silently misread identity - is not a trade worth making.
pub const NP_AUTH_MAGIC: u64 = 0x4155_5448_4E50_3032; // "AUTHNP02" (big-endian ASCII)

/// Bytes of nonce in the auth header (a fresh, non-repeating value per request:
/// the client's `MONOTONIC_US` clock, plus its packed IP for cross-machine
/// separation). Only freshness is required - not secrecy or unpredictability.
pub const NP_NONCE_LEN: usize = 16;

/// Bytes of MAC in the auth header (one HMAC-SHA256 digest).
pub const NP_MAC_LEN: usize = 32;

/// Bytes of **requesting user name** in the auth header, NUL-padded.
///
/// The shared key authenticates a *machine*; this says which of that machine's
/// users is asking, so the far side can apply its own permission model instead
/// of serving every remote request as root.
///
/// **A name, not a uid.** Two nodes have independent `/etc/passwd` files, so
/// uid 1000 need not be the same person on both - numeric identity (NFS's
/// `AUTH_SYS`) silently maps one user onto another whenever the numbering
/// differs. Names are what this account model already keys on (`su alice`,
/// `chown alice:staff`, `login`), and the far side resolves the name through
/// **its own** `/etc/passwd`, refusing a name it does not know.
///
/// 32 bytes matches the shell's `login` username field, so any name that can be
/// typed at a prompt can cross the cluster.
pub const NP_NAME_LEN: usize = 32;

/// The auth header size prepended to a framed request body: `magic`(8) +
/// `nonce`([`NP_NONCE_LEN`]) + `name`([`NP_NAME_LEN`]) + `mac`([`NP_MAC_LEN`]).
pub const NP_AUTH_HDR: usize = 8 + NP_NONCE_LEN + NP_NAME_LEN + NP_MAC_LEN;

/// Offset of the nonce within the auth header (right after the 8-byte magic).
pub const NP_AUTH_NONCE_OFF: usize = 8;
/// Offset of the requesting user's name within the auth header.
pub const NP_AUTH_NAME_OFF: usize = 8 + NP_NONCE_LEN;
/// Offset of the MAC within the auth header (after magic + nonce + name).
pub const NP_AUTH_MAC_OFF: usize = 8 + NP_NONCE_LEN + NP_NAME_LEN;

/// The bytes the request MAC covers *before* the NP message: `nonce` || `name`.
/// One constant because both sides must agree, and because it keeps the MAC a
/// two-part call rather than growing a three-part variant.
///
/// **The name is inside the MAC, which is the whole point.** It costs nothing -
/// the MAC was already there - and it means the claimed user cannot be edited
/// in flight by anything that does not hold the cluster key. A tamper attempt
/// fails verification exactly as a tampered path or offset already does.
pub const NP_MAC_PREFIX_LEN: usize = NP_NONCE_LEN + NP_NAME_LEN;

// --- the in-request identity word (`a3`) ----------------------------------
//
// WHO a request to `fsd` is made on behalf of, carried in the request itself at
// offset 40 (`a3`, the one param no verb uses). It exists because `netd`
// forwards remote requests: `fsd` would otherwise see `netd`'s own credential,
// which is root, and its root bypass would serve every remote request with no
// permission check at all. `netd` cannot `SET_ID` to the caller for this -
// that changes its own task identity, and it serves many connections from one
// task - so the identity has to travel as data.
//
// **In the request, not a latch.** An earlier attempt sent the identity as a
// separate message that `fsd` held until the next request arrived. Any other
// task's request interleaving between the two dropped it, and the fallback was
// `netd`'s root. A field of the request it authorizes cannot be separated from
// it by anything. See docs/unspellable-postmortem.md.
//
// **Who may set it.** `fsd` honours this field on a request from `NET_TASK` and
// from nowhere else, so no other task can claim an identity by writing to
// `a3`. That is a fact about the *slot*, not a credential: slots below
// `FIRST_SPAWNABLE` are protected boot servers and cannot be recycled, so
// `NET_TASK` is `netd` for the life of the boot.
//
// **And it is required.** A request from `NET_TASK` carrying [`NP_ID_NONE`] is
// REFUSED rather than served under `netd`'s own identity - the fallback that
// was the hole. Every path, including `netd`'s own reads, states which it is.

/// No identity stated. Legal from any task except `NET_TASK`, whose requests
/// are refused when they carry it (fail-closed - there is no "unstated means
/// netd's own" fallback, because that fallback is root).
pub const NP_ID_NONE: u64 = 0;

/// **`netd`'s own business**, stated explicitly: the cluster key, the
/// `/etc/passwd` lookups behind name resolution, the HTTP server's file reads.
/// Authorized against `netd`'s own kernel-bound credential, exactly as before
/// this field existed. Spelling it is what makes a forgotten export path a
/// compile error rather than a silent root escalation.
pub const NP_ID_FIRST_PARTY: u64 = 1;

/// Flag bit marking the word as a proxied identity; the low bits then carry
/// `(gid << 16) | uid`. `u16` halves match ext2's on-disk owner fields and
/// [`NP_CHOWN`]'s params; an id that does not fit cannot be proxied and the
/// request is refused rather than truncated onto a different account.
pub const NP_ID_PROXY: u64 = 1 << 63;

/// The uid *and* gid an **anonymous** request is authorized as - one that
/// arrived with no identity at all, which today means an HTTP request to
/// `netd`'s static-file server.
///
/// 65534 is the conventional `nobody`. The point is only that it is **not
/// root**: an anonymous reader gets the `other` triad and nothing else, so a
/// mode-0600 file is refused rather than served by the root bypass. Nothing
/// needs an account of this name to exist - `fsd` authorizes on the number, and
/// no name is ever resolved for it. (If a deployment does create an account with
/// uid 65534, anonymous HTTP inherits exactly that account's access, which is
/// why picking a normally-unused id matters.)
pub const NP_ID_ANON_ID: u32 = 65534;

/// The ready-made `a3` word for an anonymous request - [`NP_ID_ANON_ID`] as both
/// uid and gid. A constant rather than a `proxy_id` call so it cannot silently
/// become [`NP_ID_NONE`] at a call site that forgets to handle `None`.
pub const NP_ID_ANON: u64 =
    NP_ID_PROXY | ((NP_ID_ANON_ID as u64) << 16) | NP_ID_ANON_ID as u64;

/// Pack a resolved remote caller into the `a3` identity word. `None` when
/// either id is too large to carry, which the caller must treat as a refusal
/// (truncating would silently authorize a *different* account).
///
/// Note what is NOT carried: supplementary groups. One word has no room, and a
/// group list arriving out of step with the identity it belongs to is the exact
/// shape of bug this design exists to prevent. The cost is that a remote caller
/// is authorized on its uid and primary gid alone - which can only ever *deny*
/// access a local session would grant, never grant one it would deny.
#[inline]
pub fn proxy_id(uid: u32, gid: u32) -> Option<u64> {
    if uid > u16::MAX as u32 || gid > u16::MAX as u32 {
        return None;
    }
    Some(NP_ID_PROXY | ((gid as u64) << 16) | uid as u64)
}

/// The `(uid, gid)` a proxy identity word carries, or `None` if it is not one.
#[inline]
pub fn proxy_parts(word: u64) -> Option<(u32, u32)> {
    if word & NP_ID_PROXY == 0 {
        return None;
    }
    Some(((word & 0xffff) as u32, ((word >> 16) & 0xffff) as u32))
}

/// Largest fully-framed request buffer: the 4-byte length prefix + the auth
/// header + the biggest NP message. Buffers that build or receive a framed
/// export request are sized to this.
pub const NP_FRAME_MAX: usize = NP_NET_LEN_PREFIX + NP_AUTH_HDR + NP_NET_MAX;

// ---------------------------------------------------------------------------
// The shared namespace resolver. A namespace is a sequence of bindings
// `[tree:u8][prefix_len:u8][target_len:u8][prefix][target]`; resolving a path
// picks the longest component-aligned prefix binding and replaces the prefix
// with its target. The `tree` byte selects where the result lives - a real fsd
// mount index, or one of the sentinels above (remote / console / net). This one
// function is the single source of truth, used by `ulib`, the shell, *and*
// `netd`'s export gateway (so an exported request resolves through netd's own
// composed namespace, the Plan 9 model, instead of per-server prefix hacks).
// It is deliberately task-id-neutral: it returns a [`NsTarget`] the caller maps
// to a concrete server task, so this crate needs no dependency on `syscall-abi`.
// Byte-only and bounded (relocation-safe: no `str` range-indexing, no fmt).
// ---------------------------------------------------------------------------

/// Where a resolved path lives - the server-neutral result of [`resolve_ns`].
/// The caller maps this to a concrete task (fsd / cond / netd).
#[derive(Clone, Copy)]
pub enum NsTarget {
    /// A local `fsd` mount at this tree index (`0` = boot disk, [`NS_PROC_TREE`]
    /// = `/proc`, others = `mount`ed partitions).
    Fsd(u8),
    /// The console server (`/dev/cons`) - write-only.
    Console,
    /// The network server's local `/net` (read-only network identity).
    NetLocal,
    /// A remote mount: the `[ip:4][port:2 LE]` export endpoint to reach over TCP.
    Remote([u8; NS_ENDPOINT_LEN]),
}

/// A namespace resolution: where the path lives, and the length of the
/// server-side path bytes written to the caller's `out` buffer.
#[derive(Clone, Copy)]
pub struct NsResolved {
    pub target: NsTarget,
    pub len: usize,
}

/// Resolve absolute path `path` through namespace blob `ns`, writing the
/// server-side path to `out`. The longest component-aligned prefix binding wins;
/// its `target` replaces the matched prefix. No match (empty namespace, or a
/// relative path - bindings are absolute) is identity to the local boot mount
/// (`Fsd(0)`), so an unbound task is unchanged. A remote binding's `target`
/// begins with a 6-byte endpoint (stripped into [`NsTarget::Remote`]); the
/// console/net sentinels route to their servers. Bounded, byte-only.
pub fn resolve_ns(ns: &[u8], path: &[u8], out: &mut [u8]) -> NsResolved {
    let pbytes = path;
    let mut best_tree = 0u8;
    let mut best_plen = 0usize; // matched prefix length; 0 = no match
    let mut best_target: &[u8] = &[];
    let mut i = 0usize;
    while i + 3 <= ns.len() {
        let tree = ns[i];
        let plen = ns[i + 1] as usize;
        let tlen = ns[i + 2] as usize;
        let pstart = i + 3;
        let tstart = pstart + plen;
        let tend = tstart + tlen;
        if tend > ns.len() {
            break; // malformed blob - stop parsing
        }
        let prefix = &ns[pstart..tstart];
        let target = &ns[tstart..tend];
        i = tend;
        // Component-aligned: path == prefix, or path starts with prefix then '/'.
        let matches = pbytes.len() >= prefix.len()
            && &pbytes[..prefix.len()] == prefix
            && (pbytes.len() == prefix.len() || pbytes[prefix.len()] == b'/');
        if matches && prefix.len() > best_plen {
            best_tree = tree;
            best_plen = prefix.len();
            best_target = target;
        }
    }
    if best_plen == 0 {
        let n = pbytes.len().min(out.len());
        out[..n].copy_from_slice(&pbytes[..n]);
        return NsResolved { target: NsTarget::Fsd(0), len: n };
    }
    // A remote binding: [ip:4][port:2][remote-root]. Split the endpoint off; the
    // remote root plays the same role a local target does below.
    let remote = best_tree == NS_REMOTE_TREE && best_target.len() >= NS_ENDPOINT_LEN;
    let mut endpoint = [0u8; NS_ENDPOINT_LEN];
    let target = if remote {
        endpoint.copy_from_slice(&best_target[..NS_ENDPOINT_LEN]);
        &best_target[NS_ENDPOINT_LEN..]
    } else {
        best_target
    };
    // target ++ (path after the matched prefix). `after` is "" or starts '/'.
    let after = &pbytes[best_plen..];
    let mut n = 0usize;
    let target_is_root = target == b"/";
    if !(target_is_root && !after.is_empty()) {
        let t = target.len().min(out.len());
        out[..t].copy_from_slice(&target[..t]);
        n = t;
    }
    let a = after.len().min(out.len() - n);
    out[n..n + a].copy_from_slice(&after[..a]);
    n += a;
    if n == 0 && !out.is_empty() {
        out[0] = b'/'; // target "/" with empty after
        n = 1;
    }
    let target = if best_tree == NS_CON_TREE {
        NsTarget::Console
    } else if best_tree == NS_NET_TREE {
        NsTarget::NetLocal
    } else if remote {
        NsTarget::Remote(endpoint)
    } else {
        NsTarget::Fsd(best_tree)
    };
    NsResolved { target, len: n }
}

#[cfg(test)]
mod tests {
    //! Host tests for the pure parts of the ABI (`cargo test -p ninep-abi
    //! --target aarch64-apple-darwin`). No I/O, no syscalls - the cheapest
    //! foreign observer this project has.
    use super::*;

    #[test]
    fn proxy_id_round_trips() {
        let w = proxy_id(1000, 1000).unwrap();
        assert_eq!(proxy_parts(w), Some((1000, 1000)));
        let w = proxy_id(0, 0).unwrap();
        assert_eq!(proxy_parts(w), Some((0, 0)));
        let w = proxy_id(65535, 65535).unwrap();
        assert_eq!(proxy_parts(w), Some((65535, 65535)));
    }

    #[test]
    fn uid_and_gid_do_not_bleed_into_each_other() {
        // The bug this catches is a shift/mask slip putting the gid where the
        // uid is read - which authorizes the wrong account rather than failing.
        let w = proxy_id(1000, 0).unwrap();
        assert_eq!(proxy_parts(w), Some((1000, 0)));
        let w = proxy_id(0, 1000).unwrap();
        assert_eq!(proxy_parts(w), Some((0, 1000)));
    }

    #[test]
    fn root_proxy_is_not_none() {
        // THE load-bearing property of the flag bit. Root is uid 0 / gid 0, so
        // without a tag bit a proxied root would encode as 0 - indistinguishable
        // from NP_ID_NONE, which `fsd` refuses. Both must stay spellable and
        // distinct: the refusal must mean "stated nothing", never "stated root".
        assert_ne!(proxy_id(0, 0).unwrap(), NP_ID_NONE);
        assert_ne!(proxy_id(0, 0).unwrap(), NP_ID_FIRST_PARTY);
    }

    #[test]
    fn the_anonymous_word_is_a_proxy_and_is_not_root() {
        // The HTTP server serves with this. If it ever decoded as root, or as
        // NP_ID_NONE (refused), the static-file server would either bypass every
        // mode or stop working - and both have been true of it at some point.
        assert_eq!(proxy_parts(NP_ID_ANON), Some((NP_ID_ANON_ID, NP_ID_ANON_ID)));
        assert_ne!(NP_ID_ANON_ID, 0);
        assert_eq!(proxy_id(NP_ID_ANON_ID, NP_ID_ANON_ID), Some(NP_ID_ANON));
    }

    #[test]
    fn none_and_first_party_are_not_proxies() {
        // `fsd` reads the word as: FIRST_PARTY, else a proxy, else refuse. If
        // either sentinel parsed as a proxy it would authorize uid 0 - root.
        assert_eq!(proxy_parts(NP_ID_NONE), None);
        assert_eq!(proxy_parts(NP_ID_FIRST_PARTY), None);
    }

    #[test]
    fn oversized_ids_are_refused_not_truncated() {
        // Truncating would silently authorize a DIFFERENT account.
        assert_eq!(proxy_id(65536, 0), None);
        assert_eq!(proxy_id(0, 65536), None);
        assert_eq!(proxy_id(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn the_mac_covers_exactly_the_nonce_and_the_name() {
        // Both peers build the MAC input from NP_MAC_PREFIX_LEN and read the
        // fields by offset. If those disagreed, either the name would fall
        // outside the MAC (forgeable) or every verification would fail.
        assert_eq!(NP_AUTH_NAME_OFF, NP_AUTH_NONCE_OFF + NP_NONCE_LEN);
        assert_eq!(NP_AUTH_MAC_OFF, NP_AUTH_NAME_OFF + NP_NAME_LEN);
        assert_eq!(NP_MAC_PREFIX_LEN, NP_AUTH_MAC_OFF - NP_AUTH_NONCE_OFF);
        assert_eq!(NP_AUTH_HDR, 8 + NP_NONCE_LEN + NP_NAME_LEN + NP_MAC_LEN);
    }

    #[test]
    fn the_identity_word_sits_past_every_op_param() {
        // `a3` is the identity field, so the header must still have room for it
        // after a0..a2 - i.e. nothing may grow the params into offset 40.
        assert_eq!(NP_REQ_PAYLOAD, 48);
    }
}

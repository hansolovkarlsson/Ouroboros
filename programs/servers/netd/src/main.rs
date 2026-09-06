//! `netd` - the network server, the eighth userland program and the fourth
//! protected server (task slot [`syscall_abi::NET_TASK`], 4). It drives the
//! kernel's virtio-net NIC entirely from EL0 through the gated
//! `NET_SEND`/`NET_RECV`/`NET_MAC` syscalls - the DMA-owning driver stays in
//! the kernel (no IOMMU), the protocol stack lives here, the `fsd`/`BLOCK_*`
//! pattern.
//!
//! Stage 2b: real ARP + IPv4 + ICMP, exposed as a `NETOP_PING` request over
//! IPC (the `FSOP_*` shape). A client (the shell's `ping` command) sends a
//! target IPv4; `netd` ARP-resolves it, sends an ICMP echo request, waits
//! for the reply, and answers with a `NET_PING_*` status. Everything is
//! hand-rolled fixed-buffer (the ACPI/FAT32/virtio precedent) - no crates,
//! no heap.
//!
//! Scope, deliberate: **guest-initiated ping only**. Replying to a host's
//! ping needs an asynchronous receive loop (the poll/select gap), out of
//! scope here. The source IP `10.0.2.15` and the /24 assumption are the
//! QEMU user-net convention `init_net`'s old probe already used; real
//! DHCP/routing is future work.
//!
//! Built like every userland program: `aarch64-unknown-none`, release-only,
//! the shared `programs/linker.ld`, staged as `\EFI\ORBS\NETD.BIN`.

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;


/// Our IPv4, derived from the NIC's MAC (cluster Phase 1d) so two guests on a
/// shared L2 link get distinct addresses with no config channel: the last octet
/// is the MAC's last octet. The QEMU-default MAC (`…:56`) maps to `.15` -
/// SLIRP's DHCP lease - so every existing SLIRP-based run (ping/resolve/fetch,
/// and the export gateway's `hostfwd`, all of which target `.15`) is unchanged;
/// a two-VM socket-net target assigns distinct MACs (`…:0a`/`…:0b` -> `.10`/`.11`).
/// No NIC -> `.15` (the value is never used without a NIC anyway).
fn our_ip() -> [u8; 4] {
    let packed = syscall(syscall_abi::NET_MAC, 0);
    let last = if packed == syscall_abi::NET_ERROR {
        15
    } else {
        let b = (packed >> 40) as u8; // mac[5]
        if b == 0x56 {
            15
        } else {
            b
        }
    };
    [10, 0, 2, last]
}
/// QEMU user-net's built-in DNS proxy (forwards to the host's resolver).
const DNS_SERVER: [u8; 4] = [10, 0, 2, 3];
/// A fixed ephemeral UDP source port for DNS queries (one query at a time).
const DNS_SRC_PORT: u16 = 0x8000;
/// The DNS transaction id we send and match on the reply.
const DNS_ID: u16 = 0x4f42; // "OB"
/// QEMU user-net's gateway - the next hop for any off-subnet target.
const GATEWAY: [u8; 4] = [10, 0, 2, 2];
/// Base of our ephemeral TCP source-port range and the base initial sequence
/// number. A single client connection is live at a time, but the remote-mount
/// client (cluster Phase 1c) opens many *back to back* - one per verb - and a
/// reused (port, ISN) 4-tuple collides with the peer's lingering TIME_WAIT
/// socket (the SYN is dropped until it expires; observed as intermittent stalls).
/// So `next_src_port` rotates the source port per connection, giving each a
/// fresh 4-tuple. `TCP_SRC_PORT` remains the base of that range.
const TCP_SRC_PORT: u16 = 0xc000;
const TCP_ISN: u32 = 0x0000_1000;

/// Pick an ephemeral source port (0xc000..0xf000) that varies per connection, so
/// back-to-back client connections use distinct 4-tuples and never land on the
/// peer's lingering TIME_WAIT socket. Derived from the microsecond clock rather
/// than a counter (a zero-init `static` would need `.bss`, which the userland
/// loader doesn't support); successive `tcp_get`s are a full round trip apart, so
/// the clock has always advanced between them.
fn next_src_port() -> u16 {
    TCP_SRC_PORT.wrapping_add((now_us() % 0x3000) as u16)
}
/// TCP flag bits.
const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_PSH: u8 = 0x08;
const TCP_ACK: u8 = 0x10;
/// A fixed ICMP echo identifier - `netd` has no static state to count from,
/// and one outstanding ping at a time makes a fixed id/seq sufficient.
const ICMP_ID: u16 = 0x4f42; // "OB"
const ICMP_SEQ: u16 = 1;
/// ICMP echo payload length (bytes appended after the 8-byte ICMP header).
const PAYLOAD: usize = 32;

/// The TCP port `netd`'s HTTP server listens on, and a fixed server-side
/// initial sequence number (one server connection at a time, so fixed
/// suffices - the same reasoning as the client's `TCP_ISN`).
const SERVER_PORT: u16 = 80;
const SERVER_ISN: u32 = 0x0002_0000;
/// How many concurrent server connections `netd` can hold at once (a browser
/// opens several for one page). Bounded because each `TcpConn` carries a
/// ~2KB response-prefix buffer and they all live on `serve()`'s stack; a SYN
/// arriving with no free slot is dropped (the peer retransmits).
const MAX_CONNS: usize = 4;
/// Max SACK blocks parsed from one segment's TCP options (RFC 2018 allows up
/// to 4 in the 40-byte option area, 3 alongside timestamps).
const MAX_SACK: usize = 4;

/// Full responses for the two failure cases.
const RESP_404: &[u8] =
    b"HTTP/1.0 404 Not Found\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nNot Found\r\n";
// An anonymous reader may not have this file (it is served as `nobody`, never
// as root - see `As::Anonymous`). 403 rather than 404: the file is there.
const RESP_403: &[u8] =
    b"HTTP/1.0 403 Forbidden\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nForbidden\r\n";
const RESP_503: &[u8] = b"HTTP/1.0 503 Service Unavailable\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nNo filesystem mounted\r\n";
// Only GET and HEAD are implemented; any other method gets a 405 with an
// Allow header naming the two that work (RFC 7231 requires Allow on a 405).
const RESP_405: &[u8] = b"HTTP/1.0 405 Method Not Allowed\r\nAllow: GET, HEAD\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nMethod Not Allowed\r\n";

/// One file chunk / one TCP body segment - kept under the 1460 MSS so each
/// `NP_READ` (bulk) maps to at most one segment (a segment may be smaller when
/// the peer's remaining window is tighter - see `pump_send`).
const SERVE_CHUNK: usize = 1400;

/// TCP congestion control (Reno), all in bytes. The segment size used for
/// cwnd accounting is `SERVE_CHUNK`; the send window is `min(cwnd, peer
/// window)`. `cwnd` starts at `INIT_CWND` and grows per ACK - by ~`MSS` per
/// ACK in slow start (cwnd < ssthresh), by ~`MSS` per RTT in congestion
/// avoidance - and is cut on loss: halved to `ssthresh` on a fast
/// retransmit, dropped to one segment (back to slow start) on an RTO.
/// Capped at `MAX_CWND` (the 16-bit TCP window ceiling - no window scaling
/// is negotiated). On lossless SLIRP cwnd just ramps to the peer window and
/// stays, so the visible effect is the slow-start ramp at the start of a
/// transfer (the initial burst is `INIT_CWND`, not the whole peer window);
/// the reduction paths need injected loss to exercise.
const MSS: u32 = SERVE_CHUNK as u32;
const INIT_CWND: u32 = 4 * MSS;
const INIT_SSTHRESH: u32 = 0xFFFF;
const MIN_CWND: u32 = 2 * MSS;
const MAX_CWND: u32 = 0xFFFF;

/// TCP retransmit-timeout tuning, in `now()` ticks (a tick is 20ms). The RTO
/// is now *estimated per connection* from the measured round-trip time (RFC
/// 6298 - see `TcpConn::update_rtt`), not fixed: `RTO_INIT_TICKS` (~1s, the
/// RFC's initial value) is used until the first RTT sample, then the estimate
/// clamped to `[RTO_MIN_TICKS, RTO_MAX_TICKS]`. It still doubles per
/// consecutive firing up to a cap and gives up (RST + close) after
/// `RTO_MAX_RETRIES`. `RTO_POLL_MS` is how long `NET_WAIT` sleeps while data
/// is unacked, so netd wakes to check the timer even if the peer is silent.
/// `RTO_MIN_TICKS` matches `RTO_POLL_MS` so a minimum-RTO timer fires at the
/// next poll rather than sitting idle; `RTO_MAX_TICKS` (2s) stays under the
/// supervisor's ~2.5s wedge threshold.
const RTO_INIT_TICKS: u64 = 50; // ~1s, RFC 6298 initial RTO (pre-sample)
const RTO_MIN_TICKS: u64 = 10; // 200ms, == RTO_POLL_MS
const RTO_MAX_TICKS: u64 = 100; // 2s
const RTO_MAX_RETRIES: u8 = 5;
const RTO_POLL_MS: u64 = 200;
/// A tick in microseconds (`MONOTONIC_US`'s unit), for converting an
/// estimated RTO in µs to `now()` ticks.
const TICK_US: u64 = 20_000;
/// The RTO's clock-granularity floor `G` (RFC 6298's `max(G, 4*RTTVAR)`):
/// the 20ms tick the RTO deadline is measured in, so the variance term never
/// implies finer resolution than the timer actually has.
const RTT_G_US: u64 = TICK_US;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log(b"netd: network server ready\r\n");
    let packed_mac = syscall(syscall_abi::NET_MAC, 0);
    if packed_mac == syscall_abi::NET_ERROR {
        log(b"netd: no NIC this boot - ping will report no network\r\n");
    }
    serve(packed_mac);
}


/// Bytes of `/etc/cluster/authorized` netd keeps.
///
/// About fourteen peers at 72 bytes a line. It lives in `Auth`, on `serve`'s
/// frame, because that is where the rest of the auth config already lives and
/// this server has no heap - and 1 KB against a 32 KB stack that has hit its guard page five
/// times is a deliberate, measured choice rather than a comfortable one. A file
/// longer than this is REPORTED at boot rather than silently half-read: the
/// peers past the cut would simply not be authorized, which is safe and
/// invisible, and invisible is the half worth fixing.
const AUTHORIZED_MAX: usize = clusterkeys::AUTHORIZED_MAX;

/// The cluster authentication config, loaded once from disk at boot (see
/// `docs/roadmap-cluster-keys.md`). A *per-machine keypair*: this machine holds
/// its own private key and a list of the peer PUBLIC keys it accepts, so a
/// member is revoked by deleting a line rather than by re-keying everyone. A
/// machine with neither can neither serve nor dial. Read-only after startup; no mutable statics exist in
/// userland (`.bss` asserted empty), so this rides `serve`'s stack frame and is
/// threaded (`&Auth`) through the event loop to the two leaves that need it -
/// the inbound gate (`handle_9p`) and the outbound signer (`handle_rmount`/
/// `handle_run`).
struct Auth {
    /// The no-exec lever: a machine may share its disk (mounts allowed) while
    /// refusing remote code execution (`NP_RUN`). Set by a `\NOEXEC` flag file.
    noexec: bool,
    /// This machine's own signing key, expanded once at boot.
    ///
    /// `SigningKey` rather than the raw seed: expanding derives the public key
    /// with a scalar multiplication, and doing that per frame would double the
    /// cost of every signature for a value that never changes. `None` means this
    /// machine has no identity, and since the flag day there is no second format
    /// to fall back to: it can neither sign an outbound request nor sign a reply,
    /// so it neither dials nor serves.
    signing: Option<ed25519::SigningKey>,
    /// `/etc/cluster/authorized`: the peers this machine accepts, by public key.
    /// Empty means no peer can be verified at all, which closes the export -
    /// fail-closed, and with the MAC'd format gone there is nothing it leaves
    /// unaffected.
    authorized: [u8; AUTHORIZED_MAX],
    authorized_len: usize,
}

impl Auth {
    /// Whether this machine can serve an export request.
    ///
    /// Two conditions, and the second is the one that is easy to forget: it
    /// needs peers to verify against, AND an identity of its own, because a
    /// request gets a SIGNED reply - a machine that cannot sign refuses rather
    /// than answering unauthenticated (see `seal_reply`).
    ///
    /// Until the flag day this sat beside `accepts_mac`, and both were reached
    /// through a `can_verify` that meant "either". There is only one format
    /// now, so the three collapsed into this.
    fn accepts_signed(&self) -> bool {
        // PARSEABLE PEERS, not merely bytes. `clusterkeys` documents commenting
        // a line out as a supported revocation, so a file with its last peer
        // commented out is non-empty and authorizes nobody.
        self.signing.is_some() && count_peers(self.authorized()) > 0
    }

    /// Whether this machine can authenticate an **outbound** request, i.e. sign
    /// it. Separate from [`Auth::accepts_signed`] because a machine that lists
    /// no peers can still make requests of machines that list IT.
    fn can_send(&self) -> bool {
        self.signing.is_some()
    }

    /// The peers this machine accepts, as `clusterkeys` expects them.
    fn authorized(&self) -> &[u8] {
        &self.authorized[..self.authorized_len]
    }

}

/// The disk path (FAT 8.3-legal) of the no-exec flag.
const NOEXEC_PATH: &[u8] = b"/NOEXEC";
/// The peers this machine accepts, by public key (see
/// docs/roadmap-cluster-keys.md). Absent means it verifies no signatures, which
/// closes the export.
const AUTHORIZED_PATH: &[u8] = b"/etc/cluster/authorized";
/// This machine's own private key. Its presence is what lets netd sign its
/// outbound requests at all; without it, it can neither serve nor dial.
const ID_PATH: &[u8] = b"/etc/cluster/id";

/// Whether an `fsd` reply is a TRANSIENT failure that a boot-time read should
/// retry, rather than a real answer.
///
/// The list is of codes that mean **the server could not answer**, which is a
/// different question from "was this an error":
///
/// - `NO_FS` — the disk is not mounted yet. `netd` and `fsd` start together, so
///   the first reads race the mount.
/// - `TASK_ERR_NO_SUCH_TASK` — `fsd` is being restarted underneath us
///   (`tasks::fail_calls_to` fails every in-flight `MSG_CALL` to a slot being
///   torn down); the supervisor reinstalls it in the same handler.
/// - `FS_ERROR` — how `read_file_chunk` reports a refused `GRANT`. It grants
///   BEFORE it calls, and the kernel refuses a grant to a non-existent task, so
///   this — not `TASK_ERR_NO_SUCH_TASK` — is what the restart window actually
///   produces. Two earlier versions of this function missed it by enumerating
///   the codes they expected rather than the ones the path emits.
/// - `MSG_ERR_FULL` — `fsd`'s mailbox was momentarily full.
///
/// WHY NOT "EVERYTHING EXCEPT A DEFINITIVE ANSWER", which this briefly was:
/// because a PERMANENT error is then retried forever. `mkdir /NOEXEC` — a
/// plausible way to reach for the flag — makes `read_at` return
/// `FS_ERR_NOT_A_FILE`, and this read is netd's first, so the whole shared
/// budget is spent sleeping in `NET_WAIT` with the mailbox undrained, for a
/// condition waiting cannot change. That starves the two reads that configure
/// the cluster AND leaves the supervisor's 160ms health ping unacked, which is
/// the restart loop `testing-parallels.md` Risk #1 is about. Fail-safe applies
/// to the DECISION (see `noexec` below), not to how long to wait for one.
fn transient_fs_failure(r: u64) -> bool {
    matches!(
        r,
        syscall_abi::NO_FS
            | syscall_abi::TASK_ERR_NO_SUCH_TASK
            | syscall_abi::FS_ERROR
            | syscall_abi::MSG_ERR_FULL
    )
}

/// Load the cluster auth config from disk via `fsd` at boot.
///
/// Retries while `fsd` reports `NO_FS` (its disk isn't mounted yet - a
/// boot-order race, since netd and fsd start together), but stops immediately
/// on `FS_ERR_NOT_FOUND` (the file is definitively absent = run unconfigured).
fn load_auth() -> Auth {
    let mut auth = Auth {
        noexec: false,
        signing: None,
        authorized: [0u8; AUTHORIZED_MAX],
        authorized_len: 0,
    };
    // ONE retry budget across all three reads, not one each.
    //
    // Each waits for the same event - the disk mounting - so three separate 2s
    // budgets do not make the race three times as winnable, they make a
    // DISKLESS boot spend three times as long not draining netd's mailbox.
    // Parallels with no USB stick, and a Pi with no card, are both supported
    // configurations where nothing ever mounts; at several seconds of silence
    // the supervisor health ping (160ms) times out often enough to burn all
    // three restarts and disable netd for the boot. Up to ~2s total, at 40ms
    // a try.
    //
    // THE BUDGET GATES THE RETRY, NEVER THE FIRST ATTEMPT: written first as
    // `while budget > 0`, which meant that exhausting it on the first read left
    // the others never attempted ONCE, so a disk that mounted late gave a node
    // with no identity and no peers for the whole boot.
    let mut budget = 50u32;
    // The no-exec flag is presence-only: any successful read (even 0 bytes)
    // means the file exists; FS_ERR_* means it doesn't.
    //
    // RETRIED ON A TRANSIENT FAILURE LIKE THE TWO READS BELOW, AND THIS ONE
    // MATTERS MOST,
    // BECAUSE IT IS THE ONLY ONE THAT FAILS *OPEN*. An unread id or peer list
    // closes the export; an unread `\NOEXEC` leaves `noexec` false, which is
    // indistinguishable from "the operator did not ask for it" - and
    // `handle_cpu_run`'s `if auth.noexec` is the only gate on remote execution
    // past authentication, so a node configured to share its disk while
    // refusing `cpu` would spawn remote commands for the whole boot, silently
    // (the boot line prints only inside the true branch).
    //
    // Until step 10b this read was third, behind the retried `CLUSTER.KEY`
    // read, whose loop absorbed the mount race on its behalf; deleting that
    // read promoted this one to netd's FIRST `fsd` call and left it bare. The
    // race is real wherever the disk is slower than QEMU's virtio-blk - USB-MSD
    // on Parallels and the Pi, which is to say the platforms least likely to be
    // the ones you test on.
    let mut one = [0u8; 1];
    let m;
    loop {
        let r = read_file_chunk(As::FirstParty, NOEXEC_PATH, 0, 0, &mut one);
        if transient_fs_failure(r) && budget > 0 {
            budget -= 1;
            syscall(syscall_abi::NET_WAIT, 40);
            continue;
        }
        m = r;
        break;
    }
    // FAIL CLOSED ON "I DON'T KNOW". `m < FS_ERR_MIN` asked "did the read
    // succeed", which folds "the disk never mounted" in with "the operator did
    // not ask for no-exec" - so a node that ran out of retry budget on a slow
    // disk came up with remote execution ENABLED, which is the one outcome this
    // probe must never produce by accident. Only a definitive `FS_ERR_NOT_FOUND`
    // clears the lever now; every other outcome, success or unanswered, sets it.
    auth.noexec = m != syscall_abi::FS_ERR_NOT_FOUND;

    // This machine's own key. Absent is not an error either - it means netd
    // signs nothing, which since the flag day means it can neither serve nor dial.
    let mut idbuf = [0u8; 128];
    // Retried on NO_FS like the two reads around it, sharing their budget - but
    // ALWAYS ATTEMPTED, even with the budget spent. This file is now the whole
    // of this machine's cluster identity - unread means it neither serves nor
    // dials - so "never read at all" is the one outcome this must not have.
    let mut idn;
    loop {
        idn = read_file_chunk(As::FirstParty, ID_PATH, 0, 0, &mut idbuf);
        if transient_fs_failure(idn) && budget > 0 {
            budget -= 1;
            syscall(syscall_abi::NET_WAIT, 40);
            continue;
        }
        break;
    }
    if idn >= syscall_abi::FS_ERR_MIN && idn != syscall_abi::FS_ERR_NOT_FOUND {
        log(b"netd: WARNING could not read /etc/cluster/id; not signing\r\n");
    } else if idn == 0 {
        log(b"netd: WARNING /etc/cluster/id is empty; not signing\r\n");
    }
    if idn < syscall_abi::FS_ERR_MIN && idn > 0 {
        let len = (idn as usize).min(idbuf.len());
        match clusterkeys::parse_key_file(&idbuf[..len]) {
            Some(seed) => {
                let key = ed25519::SigningKey::from_secret(&seed);
                log(b"netd: cluster identity: signing as ");
                log_hex(&key.public());
                log(b"\r\n");
                auth.signing = Some(key);
            }
            // A key file that exists and is not a key is a mistake worth
            // naming: the machine ends up with no identity at all, and so
            // neither serves nor dials, which looks identical to having no file.
            None => log(b"netd: WARNING /etc/cluster/id is not a key; not signing\r\n"),
        }
    }

    // The authorized-peer list. Absent is not an error: a machine with no peers
    // simply verifies no signatures, which is what a single-machine run wants.
    //
    // Retried on NO_FS like the two reads above, because this read can happen
    // before the disk is mounted - and unlike the identity, a silently-empty peer
    // list produces "authentication failed" for every signed frame, which is
    // indistinguishable from a wrong key.
    let mut n;
    loop {
        n = read_file_chunk(As::FirstParty, AUTHORIZED_PATH, 0, 0, &mut auth.authorized);
        if transient_fs_failure(n) && budget > 0 {
            budget -= 1;
            syscall(syscall_abi::NET_WAIT, 40);
            continue;
        }
        break;
    }
    if n < syscall_abi::FS_ERR_MIN {
        auth.authorized_len = (n as usize).min(AUTHORIZED_MAX);
        // EXACT truncation detection: a file that is exactly AUTHORIZED_MAX
        // bytes is fully read, and warning about it would be a false alarm. One
        // byte past the end answers the question the fill level cannot.
        let mut probe = [0u8; 1];
        // (tree, offset) - the offset is the SECOND of the pair. Passing it as
        // the tree made fsd reject every probe as an out-of-range mount, so this
        // warning could never fire: a check that could not fail, inside the fix
        // for a check that could not fail.
        let more = read_file_chunk(As::FirstParty, AUTHORIZED_PATH, 0, AUTHORIZED_MAX as u64, &mut probe);
        if more < syscall_abi::FS_ERR_MIN && more > 0 {
            log(b"netd: WARNING /etc/cluster/authorized is larger than netd can read (");
            log_dec(AUTHORIZED_MAX as u64);
            log(b" bytes);\r\n      peers past that point will NOT be authorized\r\n");
        }
        // ALWAYS reported, including zero. A machine that authorizes nobody
        // refuses every signed frame with the same error a wrong key gives, so
        // "0 peers" at boot is the difference between a five-minute diagnosis
        // and an afternoon.
        log(b"netd: cluster identity: ");
        log_dec(count_peers(auth.authorized()) as u64);
        log(b" authorized peer(s)\r\n");
    } else if n != syscall_abi::FS_ERR_NOT_FOUND {
        // Not "absent" - actually broken. Say which.
        log(b"netd: WARNING could not read /etc/cluster/authorized (error ");
        log_dec(u64::MAX - n);
        log(b" below the error ceiling); no peer can be verified by signature\r\n");
    }
    // REPORTED PER DIRECTION: they have different preconditions, and a machine
    // can be able to do one and not the other. Naming WHICH half is missing
    // matters more now that there is no fallback format to fall back to - the
    // two causes send you to different files.
    if auth.accepts_signed() {
        log(b"netd: export open, accepts signed requests\r\n");
        if auth.noexec {
            log(b"netd: no-exec set (remote-run refused; disk-share allowed)\r\n");
        }
    } else if auth.signing.is_none() {
        log(b"netd: export CLOSED - no /etc/cluster/id to sign replies with (fail-closed)\r\n");
    } else {
        log(b"netd: export CLOSED - no authorized peers in /etc/cluster/authorized (fail-closed)\r\n");
    }
    if !auth.can_send() {
        // Says nothing about the export: this machine can still SERVE. It just
        // cannot ask anyone else for anything, which is a different outage and
        // would otherwise be discovered one failed `mount -r` at a time.
        log(b"netd: no /etc/cluster/id - cannot make remote requests\r\n");
    }
    auth
}

/// The event loop: block in `NET_WAIT` until either a client message or an
/// incoming frame, then drain both. Client requests (`ping`/`resolve`/
/// `fetch`) are handled synchronously; incoming frames feed the ARP responder
/// and the TCP HTTP server. This is the async-receive model - `netd` no
/// longer busy-polls one source while starving the other, and it can now
/// *answer* the network (serve a page, reply to ARP), not just initiate.
///
/// Replying to every client message is also what acks the supervisor's
/// health-ping (its reply, addressed to the kernel's sentinel, is intercepted
/// as the ack) - and staying `Blocked` in `NET_WAIT` between bursts is what
/// keeps the passive heartbeat seeing a healthy server.
fn serve(packed_mac: u64) -> ! {
    let mac = if packed_mac == syscall_abi::NET_ERROR {
        [0u8; 6]
    } else {
        unpack_mac(packed_mac)
    };
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let mut frame = [0u8; 1600];
    // The cluster auth config, read once from disk (retries past the fsd
    // boot-order race). Rides this frame like `conns`, threaded through the
    // event loop to the inbound gate and outbound signer.
    let auth = load_auth();
    // Up to MAX_CONNS concurrent server connections, kept on this frame
    // because userland has no static mutable state (`.bss` asserted empty).
    // `serve` never returns, so the frame persists for the whole boot.
    let mut conns: [Option<TcpConn>; MAX_CONNS] = core::array::from_fn(|_| None);
    // Dial-out (/net/tcp) client connections - persist across the separate NP
    // round trips of a clone/connect/data/close sequence, driven by pump_dials.
    let mut dials: [Option<DialConn>; MAX_DIAL] = core::array::from_fn(|_| None);
    // The most recent cpu run's collected output, delivered to the shell in
    // chunks via NETOP_RUN_MORE (one pending run at a time). On this frame - no
    // mutable statics.
    let mut pending = PendingRun::new();
    loop {
        // Block until a client message or an incoming frame is pending (or
        // return immediately if either already is). While *any* connection has
        // data unacked (server or dial), use a timeout so we still wake to
        // service the retransmit timer even if a peer has gone silent (no
        // frames); otherwise block indefinitely (the health-ping still wakes us).
        let unacked = conns
            .iter()
            .any(|c| matches!(c, Some(c) if c.snd_nxt != c.snd_una))
            || dials.iter().any(|c| matches!(c, Some(d) if d.state == DialState::Connecting || d.inflight > 0 || (d.state == DialState::Closing && !d.fin_sent)));
        let timeout = if unacked { RTO_POLL_MS } else { 0 };
        syscall(syscall_abi::NET_WAIT, timeout);

        // 1. Client requests (ping/resolve/fetch, and the /net/tcp file ops),
        // handled synchronously; the supervisor health-ping is acked here too
        // (any reply acks it). Also where a cpu child's output is captured
        // (cluster Phase 4a) - routed to its connection by drain_client_messages.
        drain_client_messages(packed_mac, &mut buf, &mut conns, &mut dials, &auth, Some(&mut pending));

        // 2. Incoming frames: ARP replies, dial-out connections, and the TCP server.
        if packed_mac != syscall_abi::NET_ERROR {
            while let Some(n) = recv(&mut frame) {
                on_frame(&mac, &frame[..n], &mut conns, &mut dials, &auth);
            }
            // Drive the dial-out connections (SYN/data/FIN retransmits, idle GC).
            pump_dials(&mac, &mut dials);
            // 3. Per connection (by index, so the mailbox drain below can take
            // `&mut conns` without a borrow clash): service the retransmit timer,
            // then stream its response up to the window in bounded bursts,
            // draining the mailbox between them (acking the health-ping, and
            // capturing any cpu-child output) so no single stretch looks wedged.
            for i in 0..conns.len() {
                if conns[i].is_none() {
                    continue;
                }
                if service_rto(&mac, conns[i].as_mut().unwrap(), now()) {
                    conns[i] = None;
                    continue;
                }
                loop {
                    let c = conns[i].as_mut().unwrap();
                    if !c.responded {
                        break;
                    }
                    let before = c.snd_nxt;
                    pump_send(&mac, c); // `c` borrow ends here
                    drain_client_messages(packed_mac, &mut buf, &mut conns, &mut dials, &auth, Some(&mut pending));
                    match conns[i].as_ref() {
                        Some(c) if c.snd_nxt != before => {} // progress - continue
                        _ => break,
                    }
                }
            }
        }
    }
}

/// Drain and handle every queued message (and the supervisor health-ping, whose
/// reply is the ack). Non-blocking; returns when the mailbox is empty. A message
/// from a **cpu child** (cluster Phase 4a - a spawned remote-run command whose
/// stdout we capture) is routed to *its* connection's output buffer instead of
/// the normal client dispatch; everything else is a client request.
fn drain_client_messages(packed_mac: u64, buf: &mut [u8], conns: &mut [Option<TcpConn>; MAX_CONNS], dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth, mut pending: Option<&mut PendingRun>) {
    loop {
        let packed =
            syscall4(syscall_abi::MSG_TRY_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            return; // NO_MSG or an error: mailbox drained
        }
        let sender = packed >> 32;
        let len = ((packed & 0xffff_ffff) as usize).min(buf.len());
        if let Some(ci) = conns
            .iter()
            .position(|c| matches!(c, Some(c) if c.cpu_child == sender as u8))
        {
            // A cpu child talks to us for TWO things (cluster Phase 4a/4b): its
            // stdout (a raw MSG_SEND we capture) and - once its namespace imports
            // the caller's (4b) - its remote-fs access (a NETOP_RMOUNT MSG_CALL we
            // must service and reply to). Demux by the op field: a fs request
            // always carries NETOP_RMOUNT (op 4), so it's never mistaken for
            // output (which would deadlock the child); raw text output never
            // starts with that small value. A request goes to the normal client
            // dispatch (which replies); everything else is captured output.
            let is_request = len >= 8 && read_u64(buf, 0) == syscall_abi::NETOP_RMOUNT;
            if is_request {
                handle_client(packed_mac, sender, buf, len, conns, dials, auth, pending.as_deref_mut());
            } else {
                cpu_child_msg(conns[ci].as_mut().unwrap(), sender as u8, &buf[..len]);
            }
        } else {
            handle_client(packed_mac, sender, buf, len, conns, dials, auth, pending.as_deref_mut());
        }
    }
}

/// Route one message from a captured cpu child (cluster Phase 4a) to its
/// connection: append output bytes to the connection's `prefix` (bounded), or -
/// on the empty end-of-stream message - `WAIT`-reap the child and clear
/// `cpu_child`, which releases `pump_send` to stream the accumulated output then
/// FIN. Output beyond `PREFIX_MAX` is dropped (bounded for now; streaming the
/// child's output straight through is a later refinement).
fn cpu_child_msg(c: &mut TcpConn, child: u8, data: &[u8]) {
    if data.is_empty() {
        let _ = syscall(syscall_abi::WAIT, child as u64); // reap; ignore the status
        c.cpu_child = CPU_NONE;
    } else {
        let space = c.prefix.len().saturating_sub(c.prefix_len);
        let n = data.len().min(space);
        c.prefix[c.prefix_len..c.prefix_len + n].copy_from_slice(&data[..n]);
        c.prefix_len += n;
    }
}

/// Dispatch one client request (a `MSG_CALL` from the shell) by op, replying
/// with the op's status. The unknown-op arm also acks the supervisor ping.
/// `pending` is `Option` because `tcp_run`'s re-entrant drain (during a run)
/// must NOT process a `cpu` run op - the shell that started this run is blocked
/// in it, so no `NETOP_RUN`/`NETOP_RUN_MORE` can legitimately arrive there; it
/// passes `None`, and the main serve loop passes `Some`.
#[allow(clippy::too_many_arguments)]
fn handle_client(packed_mac: u64, sender: u64, buf: &[u8], len: usize, conns: &mut [Option<TcpConn>; MAX_CONNS], dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth, pending: Option<&mut PendingRun>) {
    // Too short to carry an op: `buf` is never cleared between messages, so
    // decoding a short one would dispatch the PREVIOUS request's bytes as this
    // sender's. Reachable since 2026-09-06: a remote-exec child orphaned by a
    // netd restart keeps its send right (grants aimed at a supervised slot
    // survive its restart), so its zero-length end-of-stream marker lands here
    // with no `cpu_child` record to route it. Answered exactly as an unknown
    // op is, not dropped: a caller that got here through a blocking MSG_CALL
    // must never be left waiting for a reply that does not come. For the
    // orphan's fire-and-forget send the reply is refused or ignored, which is
    // fine. The supervisor ping is eight bytes and never takes this path.
    if len < 8 {
        reply(sender, &0u64.to_le_bytes());
        return;
    }
    match read_u64(buf, 0) {
        syscall_abi::NETOP_PING => {
            let status = handle_ping(packed_mac, read_u64(buf, 8));
            reply(sender, &status.to_le_bytes());
        }
        syscall_abi::NETOP_RESOLVE => {
            let end = len.min(buf.len()).max(8);
            let (status, ip) = handle_resolve(packed_mac, &buf[8..end]);
            let mut r = [0u8; 16];
            r[0..8].copy_from_slice(&status.to_le_bytes());
            r[8..16].copy_from_slice(&ip.to_le_bytes());
            reply(sender, &r);
        }
        syscall_abi::NETOP_FETCH => {
            let end = len.min(buf.len()).max(8);
            let mut host = [0u8; 256];
            let hlen = (end - 8).min(host.len());
            host[..hlen].copy_from_slice(&buf[8..8 + hlen]);
            let mut r = [0u8; syscall_abi::MSG_MAX_LEN as usize];
            let rlen = handle_fetch(packed_mac, &host[..hlen], &mut r);
            reply(sender, &r[..rlen]);
        }
        syscall_abi::NETOP_RMOUNT => {
            // The remote-mount client (cluster Phase 1c): carry the embedded NP
            // request over TCP to the endpoint's 9P export gateway and reply with
            // the NP reply body (`[status:u64][data]`) verbatim.
            let mut r = [0u8; syscall_abi::MSG_MAX_LEN as usize];
            let rlen = handle_rmount(packed_mac, buf, len, &mut r, auth);
            reply(sender, &r[..rlen]);
        }
        syscall_abi::NETOP_RUN => {
            // The remote-execution client (cluster Phase 4a): frame an NP_RUN to
            // the endpoint's export, which spawns the command there and streams
            // its output back; reply that output to the shell's `cpu` builtin.
            // handle_run pumps our event loop while it waits (cluster Phase 4b),
            // so we keep serving the spawned command's imported-namespace
            // callbacks (its /host reads come back to *our* export) - passing
            // `conns` is what lets it do that.
            let mut r = [0u8; syscall_abi::MSG_MAX_LEN as usize];
            let rlen = match pending {
                Some(p) => handle_run(packed_mac, sender, buf, len, &mut r, conns, dials, auth, p),
                None => 0, // a nested run can't happen (the caller is blocked)
            };
            reply(sender, &r[..rlen]);
        }
        syscall_abi::NETOP_RUN_MORE => {
            // The shell pulls the next chunk of its last cpu run's output; an
            // empty reply = end of stream. Owner-checked inside next_chunk.
            let mut r = [0u8; syscall_abi::MSG_MAX_LEN as usize];
            let rlen = pending.map(|p| p.next_chunk(sender, &mut r)).unwrap_or(0);
            reply(sender, &r[..rlen]);
        }
        // An NP read verb from a *local* client: the /net synthetic filesystem
        // (cluster Phase 3), served by netd itself. The client resolved /net to a
        // stripped fspath (e.g. "/ip") and sent a direct NP request here (not
        // wrapped in NETOP_RMOUNT). Reply in the fsd shape: [status:u64][data].
        op if (ninep_abi::NP_BASE..ninep_abi::NP_LIMIT).contains(&op) => {
            const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
            let a0 = read_u64(buf, 16) as usize;
            let a1 = read_u64(buf, 24);
            let a2 = read_u64(buf, 32);
            let end = len.min(buf.len());
            let path = if HDR < end { &buf[HDR..(HDR + a0).min(end)] } else { &[][..] };
            let want = if op == ninep_abi::NP_READ || op == ninep_abi::NP_READ_AT {
                a2 as usize
            } else {
                a1 as usize
            };
            // A write's payload follows the path (a1 = data length for NP_WRITE/
            // NP_WRITE_FILE); reads carry none. Needed for /net/tcp ctl/data writes.
            let data_in: &[u8] = if op == ninep_abi::NP_WRITE || op == ninep_abi::NP_WRITE_FILE {
                let ds = HDR + a0;
                &buf[ds.min(end)..(ds + a1 as usize).min(end)]
            } else {
                &[]
            };
            let mac = if packed_mac == syscall_abi::NET_ERROR { [0u8; 6] } else { unpack_mac(packed_mac) };
            let mut r = [0u8; 8 + 544];
            let cap = (r.len() - 8).min(512);
            let (status, dlen) = net_op(op, path, a1, want.min(cap), data_in, &mut r[8..], dials, &mac);
            r[0..8].copy_from_slice(&status.to_le_bytes());
            reply(sender, &r[..8 + dlen]);
        }
        // Unknown op (including the supervisor's health-ping): any reply acks
        // it. The value is irrelevant to the ping sentinel.
        _ => reply(sender, &0u64.to_le_bytes()),
    }
}

/// Resolve the target via ARP, send an ICMP echo request, wait for the echo
/// reply. Returns a `NET_PING_*` code.
fn handle_ping(packed_mac: u64, target_packed: u64) -> u64 {
    if packed_mac == syscall_abi::NET_ERROR {
        return syscall_abi::NET_PING_NO_NIC;
    }
    let mac = unpack_mac(packed_mac);
    let target = [
        target_packed as u8,
        (target_packed >> 8) as u8,
        (target_packed >> 16) as u8,
        (target_packed >> 24) as u8,
    ];

    let Some(target_mac) = arp_resolve(&mac, &target) else {
        return syscall_abi::NET_PING_NO_ARP;
    };
    if icmp_echo(&mac, &target_mac, &target) {
        syscall_abi::NET_PING_OK
    } else {
        syscall_abi::NET_PING_TIMEOUT
    }
}

/// Broadcast an ARP request for `target` and poll for the reply (~500ms),
/// returning the target's MAC.
fn arp_resolve(mac: &[u8; 6], target: &[u8; 4]) -> Option<[u8; 6]> {
    let mut arp = [0u8; 42];
    arp[0..6].copy_from_slice(&[0xff; 6]); // eth dst: broadcast
    arp[6..12].copy_from_slice(mac); // eth src
    arp[12..14].copy_from_slice(&[0x08, 0x06]); // ethertype: ARP
    arp[14..16].copy_from_slice(&[0x00, 0x01]); // htype: Ethernet
    arp[16..18].copy_from_slice(&[0x08, 0x00]); // ptype: IPv4
    arp[18] = 6; // hlen
    arp[19] = 4; // plen
    arp[20..22].copy_from_slice(&[0x00, 0x01]); // oper: request
    arp[22..28].copy_from_slice(mac); // sha
    arp[28..32].copy_from_slice(&our_ip()); // spa
    arp[38..42].copy_from_slice(target); // tpa
    if send(&arp).is_err() {
        return None;
    }

    let mut beat = now();
    let deadline = now() + 25; // ~500ms
    let mut frame = [0u8; 1600];
    loop {
        if let Some(len) = recv(&mut frame) {
            if len >= 42
                && frame[12] == 0x08
                && frame[13] == 0x06 // ARP
                && frame[20] == 0x00
                && frame[21] == 0x02 // reply
                && &frame[28..32] == target
            // sender protocol address == target
            {
                let mut sha = [0u8; 6];
                sha.copy_from_slice(&frame[22..28]);
                return Some(sha);
            }
        }
        beat = beat_if_new_tick(now(), beat);
        if now() > deadline {
            return None;
        }
    }
}

/// Send an ICMP echo request to `target` (already ARP-resolved to
/// `target_mac`) and poll for the matching echo reply (~1s). Returns whether
/// a reply came back.
fn icmp_echo(mac: &[u8; 6], target_mac: &[u8; 6], target: &[u8; 4]) -> bool {
    const IP_LEN: usize = 20;
    const ICMP_LEN: usize = 8 + PAYLOAD;
    const FRAME_LEN: usize = 14 + IP_LEN + ICMP_LEN;
    let mut f = [0u8; FRAME_LEN];

    // Ethernet.
    f[0..6].copy_from_slice(target_mac);
    f[6..12].copy_from_slice(mac);
    f[12..14].copy_from_slice(&[0x08, 0x00]); // IPv4

    // IPv4 header (offset 14).
    let ip = &mut f[14..14 + IP_LEN];
    ip[0] = 0x45; // version 4, IHL 5
    ip[1] = 0; // DSCP/ECN
    let total = (IP_LEN + ICMP_LEN) as u16;
    ip[2..4].copy_from_slice(&total.to_be_bytes());
    ip[4..6].copy_from_slice(&0u16.to_be_bytes()); // id
    ip[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags/frag
    ip[8] = 64; // TTL
    ip[9] = 1; // protocol: ICMP
    // checksum (ip[10..12]) left 0 for the computation
    ip[12..16].copy_from_slice(&our_ip());
    ip[16..20].copy_from_slice(target);
    let csum = ip_checksum(&f[14..14 + IP_LEN]);
    f[24..26].copy_from_slice(&csum.to_be_bytes()); // 14 + 10

    // ICMP echo request (offset 14 + IP_LEN = 34).
    let icmp_off = 14 + IP_LEN;
    f[icmp_off] = 8; // type: echo request
    f[icmp_off + 1] = 0; // code
                         // checksum (icmp_off+2..+4) left 0 for the computation
    f[icmp_off + 4..icmp_off + 6].copy_from_slice(&ICMP_ID.to_be_bytes());
    f[icmp_off + 6..icmp_off + 8].copy_from_slice(&ICMP_SEQ.to_be_bytes());
    for (i, b) in f[icmp_off + 8..icmp_off + 8 + PAYLOAD].iter_mut().enumerate() {
        *b = i as u8;
    }
    let icmp_csum = ip_checksum(&f[icmp_off..icmp_off + ICMP_LEN]);
    f[icmp_off + 2..icmp_off + 4].copy_from_slice(&icmp_csum.to_be_bytes());

    if send(&f).is_err() {
        return false;
    }

    let mut beat = now();
    let deadline = now() + 50; // ~1s
    let mut frame = [0u8; 1600];
    loop {
        if let Some(len) = recv(&mut frame) {
            if is_echo_reply(&frame[..len], target) {
                return true;
            }
        }
        beat = beat_if_new_tick(now(), beat);
        if now() > deadline {
            return false;
        }
    }
}

/// Whether `frame` is an ICMP echo reply from `target` matching our id.
fn is_echo_reply(frame: &[u8], target: &[u8; 4]) -> bool {
    if frame.len() < 34 || frame[12] != 0x08 || frame[13] != 0x00 {
        return false; // not IPv4
    }
    if frame[23] != 1 {
        return false; // not ICMP (IP protocol byte = 14 + 9)
    }
    if &frame[26..30] != target {
        return false; // IP source (14 + 12) != target
    }
    let ihl = (frame[14] & 0x0f) as usize * 4;
    let icmp = 14 + ihl;
    if frame.len() < icmp + 6 {
        return false;
    }
    // type 0 (echo reply) and our id.
    frame[icmp] == 0 && frame[icmp + 4..icmp + 6] == ICMP_ID.to_be_bytes()
}

/// Resolve `host` to an IPv4 by sending a DNS A-record query over UDP to the
/// user-net DNS server and parsing the response. Returns
/// `(status, packed_ip)` - the `NET_RESOLVE_*` status, and the four resolved
/// octets packed little-endian when it's `NET_RESOLVE_OK`. The first UDP
/// application in the stack (Stage 3).
fn handle_resolve(packed_mac: u64, host: &[u8]) -> (u64, u64) {
    if packed_mac == syscall_abi::NET_ERROR {
        return (syscall_abi::NET_RESOLVE_NO_NIC, 0);
    }
    if host.is_empty() {
        return (syscall_abi::NET_RESOLVE_NXDOMAIN, 0);
    }
    match resolve_ip(&unpack_mac(packed_mac), host) {
        Ok(ip) => (syscall_abi::NET_RESOLVE_OK, pack_ip(&ip)),
        Err(code) => (code, 0),
    }
}

/// Resolve `host` to an IPv4 via a DNS A-query over UDP. `Err` is a
/// `NET_RESOLVE_*` status (`TIMEOUT` / `NXDOMAIN`). Shared by `resolve` and
/// `fetch` (which resolves the hostname before connecting).
fn resolve_ip(mac: &[u8; 6], host: &[u8]) -> Result<[u8; 4], u64> {
    let Some(server_mac) = arp_resolve(mac, &DNS_SERVER) else {
        return Err(syscall_abi::NET_RESOLVE_TIMEOUT);
    };
    let mut dns = [0u8; 300];
    let Some(dlen) = build_dns_query(host, &mut dns) else {
        return Err(syscall_abi::NET_RESOLVE_NXDOMAIN);
    };
    let mut frame = [0u8; 400];
    let Some(flen) = build_dns_frame(mac, &server_mac, &dns[..dlen], &mut frame) else {
        return Err(syscall_abi::NET_RESOLVE_NXDOMAIN);
    };
    if send(&frame[..flen]).is_err() {
        return Err(syscall_abi::NET_RESOLVE_TIMEOUT);
    }
    let mut beat = now();
    let deadline = now() + 40; // ~800ms (kept tight so a fetch's resolve+TCP
                               // total stays under the supervisor wedge threshold)
    let mut rx = [0u8; 1600];
    loop {
        if let Some(len) = recv(&mut rx) {
            if let Some(payload) = dns_payload(&rx[..len]) {
                return parse_dns_a(payload).ok_or(syscall_abi::NET_RESOLVE_NXDOMAIN);
            }
        }
        beat = beat_if_new_tick(now(), beat);
        if now() > deadline {
            return Err(syscall_abi::NET_RESOLVE_TIMEOUT);
        }
    }
}

fn pack_ip(ip: &[u8; 4]) -> u64 {
    ip[0] as u64 | (ip[1] as u64) << 8 | (ip[2] as u64) << 16 | (ip[3] as u64) << 24
}

/// The next hop for `target`: itself if on the QEMU user-net `10.0.2.0/24`
/// subnet, otherwise the gateway (`10.0.2.2`) - a minimal default route. The
/// first place off-subnet routing matters (`ping`/`resolve` only ever
/// targeted on-subnet addresses).
fn next_hop(target: &[u8; 4]) -> [u8; 4] {
    if target[0] == 10 && target[1] == 0 && target[2] == 2 {
        *target
    } else {
        GATEWAY
    }
}

/// A parsed TCP segment (the fields the minimal client needs).
struct TcpSeg {
    seq: u32,
    ack: u32,
    flags: u8,
    data_off: usize, // byte offset of the payload within the frame
    data_len: usize,
}

/// `resolve` + route + connect: a client HTTP GET over TCP. Fills `out` with
/// `[status: u64][total: u64][response bytes...]` and returns its length.
fn handle_fetch(packed_mac: u64, host: &[u8], out: &mut [u8]) -> usize {
    let finish = |out: &mut [u8], status: u64, resp: &[u8]| -> usize {
        out[0..8].copy_from_slice(&status.to_le_bytes());
        out[8..16].copy_from_slice(&(resp.len() as u64).to_le_bytes());
        let n = resp.len().min(out.len() - 16);
        out[16..16 + n].copy_from_slice(&resp[..n]);
        16 + n
    };
    if packed_mac == syscall_abi::NET_ERROR {
        return finish(out, syscall_abi::NET_FETCH_NO_NIC, &[]);
    }
    if host.is_empty() {
        return finish(out, syscall_abi::NET_FETCH_NO_ROUTE, &[]);
    }
    let mac = unpack_mac(packed_mac);
    let Ok(ip) = resolve_ip(&mac, host) else {
        return finish(out, syscall_abi::NET_FETCH_NO_ROUTE, &[]);
    };
    let Some(dst_mac) = arp_resolve(&mac, &next_hop(&ip)) else {
        return finish(out, syscall_abi::NET_FETCH_NO_ROUTE, &[]);
    };

    // Minimal HTTP/1.0 request with a Host header (so name-based virtual
    // hosts like Cloudflare's serve the right site). Connection: close makes
    // the server FIN after the response, ending our receive loop.
    let mut req = [0u8; 400];
    let Some(rlen) = build_http_get(host, &mut req) else {
        return finish(out, syscall_abi::NET_FETCH_NO_ROUTE, &[]);
    };

    let mut resp = [0u8; 2048];
    let (status, got) = tcp_get(&mac, &dst_mac, &ip, 80, &req[..rlen], &mut resp, false);
    finish(out, status, &resp[..got])
}

/// Build `GET / HTTP/1.0\r\nHost: <host>\r\nConnection: close\r\n\r\n`.
fn build_http_get(host: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut w = 0;
    let mut put = |bytes: &[u8]| -> Option<()> {
        if w + bytes.len() > out.len() {
            return None;
        }
        out[w..w + bytes.len()].copy_from_slice(bytes);
        w += bytes.len();
        Some(())
    };
    put(b"GET / HTTP/1.0\r\nHost: ")?;
    put(host)?;
    put(b"\r\nConnection: close\r\n\r\n")?;
    Some(w)
}

/// A minimal client-side TCP connection: SYN handshake, send `request`, read
/// the response (in-order reassembly), clean FIN teardown. One connection,
/// bounded by short timeouts (kept under the supervisor wedge threshold).
/// Returns `(NET_FETCH_* status, bytes copied into resp)`.
fn tcp_get(mac: &[u8; 6], dst_mac: &[u8; 6], target: &[u8; 4], dst_port: u16, request: &[u8], resp: &mut [u8], framed: bool) -> (u64, usize) {
    let mut frame = [0u8; 1600];
    let mut rx = [0u8; 1600];
    // A fresh source port (and a derived ISN) per connection, so back-to-back
    // remote-mount round trips never reuse a 4-tuple the peer still holds in
    // TIME_WAIT (which silently drops the SYN - see next_src_port).
    let src_port = next_src_port();
    let isn = TCP_ISN ^ ((src_port as u32) << 8);

    // SYN, retransmitted up to SYN_TRIES times. A single SYN with no retransmit
    // (the original) failed the whole op if that one packet was dropped - which a
    // freshly-connected QEMU socket link (the two-VM cluster) can do to the very
    // first frame. Each try resends the SYN and waits a short window for the
    // SYN-ACK; a dropped SYN or SYN-ACK is recovered within the same op.
    const SYN_TRIES: u32 = 4;
    const SYN_WAIT_TICKS: u64 = 12; // per try (~a few hundred ms under TCG)
    let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, isn, 0, TCP_SYN, true, &[], &mut frame) else {
        return (syscall_abi::NET_FETCH_TIMEOUT, 0);
    };
    let mut beat = now();
    let their_isn = 'handshake: {
        let mut tries = 0u32;
        loop {
            if send(&frame[..n]).is_err() {
                return (syscall_abi::NET_FETCH_TIMEOUT, 0);
            }
            tries += 1;
            let deadline = now() + SYN_WAIT_TICKS;
            loop {
                if let Some(len) = recv(&mut rx) {
                    if let Some(s) = parse_tcp(&rx[..len], target, dst_port, src_port) {
                        if s.flags & TCP_RST != 0 {
                            return (syscall_abi::NET_FETCH_REFUSED, 0);
                        }
                        if s.flags & (TCP_SYN | TCP_ACK) == (TCP_SYN | TCP_ACK) && s.ack == isn + 1 {
                            break 'handshake s.seq;
                        }
                    }
                }
                beat = beat_if_new_tick(now(), beat);
                if now() > deadline {
                    break; // this try timed out - resend the SYN (if tries left)
                }
            }
            if tries >= SYN_TRIES {
                return (syscall_abi::NET_FETCH_TIMEOUT, 0);
            }
        }
    };

    let snd_nxt = isn + 1;
    let mut rcv_nxt = their_isn.wrapping_add(1);
    // ACK the SYN-ACK, then send the request (PSH|ACK).
    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
        let _ = send(&frame[..n]);
    }
    let mut snd_nxt = snd_nxt;
    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_PSH | TCP_ACK, false, request, &mut frame) {
        let _ = send(&frame[..n]);
        snd_nxt = snd_nxt.wrapping_add(request.len() as u32);
    }

    // Receive the response until it is COMPLETE, then the peer's FIN, then a
    // deadline (~1s), in that order of preference.
    //
    // `framed` says the reply carries its own length (`[u32 len][body]`) - the
    // 9P export's shape. Then EOF is not needed to know the reply ended, and
    // waiting for one is merely a habit: this returns as soon as the framed
    // body is complete.
    //
    // NOT unconditional, because the other caller is the HTTP fetch on port 80,
    // whose response has no length prefix this code parses - `Connection: close`
    // IS its terminator. Making both length-aware would have ended every fetch
    // at four bytes of "HTTP".
    //
    // Why this matters beyond a small latency win: it is the prerequisite for
    // the export becoming a SESSION (decision 3, docs/roadmap-fid-verbs.md).
    // A session never FINs between requests, so a client that waits for one
    // stalls into the deadline on every single request. Landing it FIRST, and
    // separately, is deliberate - it is verifiable against today's
    // FIN-closing export, and it is needed under every option for the wire
    // signal that is still undecided.
    let mut got = 0usize;
    let mut fin = false;
    let mut deadline = now() + 50;
    loop {
        if let Some(len) = recv(&mut rx) {
            if let Some(s) = parse_tcp(&rx[..len], target, dst_port, src_port) {
                if s.flags & TCP_RST != 0 {
                    break;
                }
                if s.data_len > 0 && s.seq == rcv_nxt {
                    let take = s.data_len.min(resp.len() - got);
                    resp[got..got + take].copy_from_slice(&rx[s.data_off..s.data_off + take]);
                    got += take;
                    rcv_nxt = rcv_nxt.wrapping_add(s.data_len as u32);
                    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    // Complete framed reply in hand? CLOSE, then stop.
                    //
                    // Not a bare `break`. `pump_send` has MAX_BURST = 1, so the
                    // export's FIN is always a SEPARATE segment after the last
                    // data segment - which means breaking here leaves that FIN
                    // in flight, unacked, and this side never sends one either.
                    // The exporting node's slot then sits in `Closing` until
                    // `service_rto` gives up (five doublings, ~6-9s), and its
                    // pool is only MAX_CONNS deep. Ending a connection is this
                    // side's job the moment it stops listening to it.
                    if framed && got >= ninep_abi::NP_NET_LEN_PREFIX {
                        let flen = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]) as usize;
                        if got >= ninep_abi::NP_NET_LEN_PREFIX + flen {
                            // CONSUME A COALESCED FIN FIRST. This breaks past
                            // the FIN arm below, so a peer that piggybacks its
                            // FIN on the last data segment would otherwise get
                            // a FIN|ACK acking only the data - leaving its FIN
                            // unacked and its slot retransmitting into an
                            // ephemeral port we no longer track, which is the
                            // exact failure this early close exists to prevent.
                            //
                            // The in-tree exporter never coalesces (MAX_BURST
                            // = 1, so its FIN is always its own segment). A host
                            // TCP stack MAY - np9p_server.py does `sendall` then
                            // `shutdown(SHUT_WR)` with nothing between - but on
                            // an idle connection `sendall` hands the data to the
                            // kernel, which transmits it immediately, so the FIN
                            // almost always lands in its own segment there too.
                            //
                            // SO THIS ARM IS UNEXERCISED BY ANY RIG IN THE TREE,
                            // and that is stated rather than left for someone to
                            // discover: if the `+ 1` here were wrong, neither
                            // `make test` nor `run-image-9p-client` would
                            // notice. Covering it needs a peer that can be MADE
                            // to coalesce, which a Python peer cannot do
                            // reliably (it is the host kernel's choice, not the
                            // script's). Kept because the cost of being wrong -
                            // a stranded slot out of a pool of four - is worse
                            // than the cost of a branch that rarely runs.
                            if s.flags & TCP_FIN != 0 {
                                rcv_nxt = rcv_nxt.wrapping_add(1);
                            }
                            if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_FIN | TCP_ACK, false, &[], &mut frame) {
                                let _ = send(&frame[..n]);
                            }
                            // `fin` deliberately NOT set: it means "the peer
                            // FIN'd" everywhere else here, and the only thing
                            // it feeds is `got > 0 || fin`, which `got > 0`
                            // already satisfies on this path. A flag that stops
                            // meaning its name is how the next reader is misled.
                            break;
                        }
                    }
                    deadline = now() + 50; // extend while making progress
                }
                if s.flags & TCP_FIN != 0 {
                    rcv_nxt = rcv_nxt.wrapping_add(1); // FIN consumes one sequence number
                    // ACK the FIN, then send our own FIN to close cleanly.
                    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_FIN | TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    fin = true;
                    break;
                }
            }
        }
        beat = beat_if_new_tick(now(), beat);
        if got >= resp.len() || now() > deadline {
            break;
        }
    }

    if got > 0 || fin {
        (syscall_abi::NET_FETCH_OK, got)
    } else {
        (syscall_abi::NET_FETCH_TIMEOUT, 0)
    }
}

/// A client TCP connection for a remote *run* (cluster Phase 4b) that **pumps our
/// own event loop while it waits**: the SYN handshake + `NP_RUN` send like
/// `tcp_get`, but the receive loop, besides accumulating the run's output stream,
/// also (a) acks the supervisor health-ping and serves other client requests,
/// (b) feeds every non-run frame to `on_frame` - which is how the spawned
/// command's imported-namespace (`/host`) reads, arriving at *our* export as
/// frames, get served *during* the run - and (c) pumps the server connections so
/// those export replies actually go out. A plain blocking `tcp_get` here would
/// deadlock: it drops those frames while stuck waiting, the remote child blocks on
/// its read, and no output ever comes. Returns `(NET_FETCH_* status, output len)`.
#[allow(clippy::too_many_arguments)]
fn tcp_run(packed_mac: u64, mac: &[u8; 6], dst_mac: &[u8; 6], target: &[u8; 4], dst_port: u16, request: &[u8], resp: &mut [u8], conns: &mut [Option<TcpConn>; MAX_CONNS], dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth) -> (u64, usize) {
    let mut frame = [0u8; 1600];
    let mut rx = [0u8; 1600];
    let mut cbuf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let src_port = next_src_port();
    let isn = TCP_ISN ^ ((src_port as u32) << 8);

    // SYN handshake (identical to tcp_get's, with retransmit).
    const SYN_TRIES: u32 = 4;
    const SYN_WAIT_TICKS: u64 = 12;
    let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, isn, 0, TCP_SYN, true, &[], &mut frame) else {
        return (syscall_abi::NET_FETCH_TIMEOUT, 0);
    };
    let mut beat = now();
    let their_isn = 'handshake: {
        let mut tries = 0u32;
        loop {
            if send(&frame[..n]).is_err() {
                return (syscall_abi::NET_FETCH_TIMEOUT, 0);
            }
            tries += 1;
            let deadline = now() + SYN_WAIT_TICKS;
            loop {
                if let Some(len) = recv(&mut rx) {
                    if let Some(s) = parse_tcp(&rx[..len], target, dst_port, src_port) {
                        if s.flags & TCP_RST != 0 {
                            return (syscall_abi::NET_FETCH_REFUSED, 0);
                        }
                        if s.flags & (TCP_SYN | TCP_ACK) == (TCP_SYN | TCP_ACK) && s.ack == isn + 1 {
                            break 'handshake s.seq;
                        }
                    } else {
                        // A non-run frame arriving mid-handshake (an early export
                        // request, ARP): serve it so we don't drop it.
                        on_frame(mac, &rx[..len], conns, dials, auth);
                    }
                }
                beat = beat_if_new_tick(now(), beat);
                if now() > deadline {
                    break;
                }
            }
            if tries >= SYN_TRIES {
                return (syscall_abi::NET_FETCH_TIMEOUT, 0);
            }
        }
    };

    let snd_nxt = isn + 1;
    let mut rcv_nxt = their_isn.wrapping_add(1);
    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
        let _ = send(&frame[..n]);
    }
    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_PSH | TCP_ACK, false, request, &mut frame) {
        let _ = send(&frame[..n]);
    }

    // Receive loop, pumping the export the whole time. A long deadline (a run with
    // namespace-import callbacks does several TCP round trips), extended on any
    // progress - the run's output, *or* an export request we served.
    let mut got = 0usize;
    let mut fin = false;
    let mut deadline = now() + 300;
    loop {
        // Ack the health-ping / service other client requests (re-entrant-safe:
        // the shell is blocked in this very run, so no nested run can arrive).
        let before_deadline = deadline;
        drain_client_messages(packed_mac, &mut cbuf, conns, dials, auth, None);
        while let Some(len) = recv(&mut rx) {
            if let Some(s) = parse_tcp(&rx[..len], target, dst_port, src_port) {
                if s.flags & TCP_RST != 0 {
                    fin = true; // treat a reset as the end of the run
                    break;
                }
                if s.data_len > 0 && s.seq == rcv_nxt {
                    let take = s.data_len.min(resp.len() - got);
                    resp[got..got + take].copy_from_slice(&rx[s.data_off..s.data_off + take]);
                    got += take;
                    rcv_nxt = rcv_nxt.wrapping_add(s.data_len as u32);
                    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    deadline = now() + 300;
                }
                if s.flags & TCP_FIN != 0 {
                    rcv_nxt = rcv_nxt.wrapping_add(1);
                    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    if let Some(n) = build_tcp(mac, dst_mac, target, src_port, dst_port, snd_nxt, rcv_nxt, TCP_FIN | TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    fin = true;
                    break;
                }
            } else {
                // Not our run connection: an export request (the child's /host
                // read) or ARP - serve it, and count it as progress.
                on_frame(mac, &rx[..len], conns, dials, auth);
                deadline = now() + 300;
            }
        }
        // Pump the server connections so served export replies actually go out.
        pump_conns(mac, conns);
        // If an export callback was served this pass, `deadline` moved.
        let _ = before_deadline;
        beat = beat_if_new_tick(now(), beat);
        if fin || got >= resp.len() || now() > deadline {
            break;
        }
    }

    if got > 0 || fin {
        (syscall_abi::NET_FETCH_OK, got)
    } else {
        (syscall_abi::NET_FETCH_TIMEOUT, 0)
    }
}

/// Service every server connection once (retransmit timer + a bounded pump),
/// exactly the main serve loop's step 3 - factored so `tcp_run` can keep the
/// export flowing while it drives a remote run (cluster Phase 4b).
#[allow(clippy::needless_range_loop)] // index needed for the re-borrow after pump
fn pump_conns(mac: &[u8; 6], conns: &mut [Option<TcpConn>; MAX_CONNS]) {
    for i in 0..conns.len() {
        if conns[i].is_none() {
            continue;
        }
        if service_rto(mac, conns[i].as_mut().unwrap(), now()) {
            conns[i] = None;
            continue;
        }
        loop {
            let c = conns[i].as_mut().unwrap();
            if !c.responded {
                break;
            }
            let before = c.snd_nxt;
            pump_send(mac, c);
            match conns[i].as_ref() {
                Some(c) if c.snd_nxt != before => {}
                _ => break,
            }
        }
    }
}

/// Serve one `NETOP_RMOUNT` client request (cluster Phase 1c): carry the
/// embedded `ninep-abi` NP request over a TCP round trip to a remote machine's
/// 9P export gateway and hand the client the NP reply body verbatim.
///
/// `buf[..len]` is `[op:u64][ip:4][port:2 LE][pad:2][NP message...]`. We frame
/// the NP message with the 4-byte length prefix the export listener expects,
/// open a client connection to `ip:port` (via [`tcp_get`], which sends the
/// request and reads the reply until the peer's FIN - the export does one
/// request/reply then closes, like HTTP `Connection: close`), strip the reply's
/// length prefix, and copy `[status:u64][data]` into `out`. On any transport
/// failure (no NIC, no route, timeout, refused) the reply is a bare
/// [`syscall_abi::NO_FS`] status - a remote mount that can't be reached fails
/// *cleanly* (the roadmap's stated Phase 1 posture), never hangs or corrupts.
/// Returns the number of bytes written to `out`.
/// Serve one `NETOP_RUN` client request (cluster Phase 4a): frame an `NP_RUN` to
/// the endpoint's export gateway (which spawns the command there and streams its
/// stdout back), and return that output to the shell's `cpu` builtin. The reply
/// is the **raw** output bytes (the run response is a stream, not a framed NP
/// reply), bounded by the caller's `out` (one `MSG_MAX_LEN` for now - small
/// commands). Request layout is `NETOP_RMOUNT`'s (endpoint + payload), the
/// payload being the command line rather than an NP message.
#[allow(clippy::too_many_arguments)]
/// The collected output of the most recent `cpu` run, delivered to the shell one
/// `MSG_MAX_LEN` chunk at a time (the shell pulls with `NETOP_RUN_MORE`). netd
/// holds it here between pull calls - a single pending run at a time, on `serve`'s
/// frame (no mutable statics). Bounded by `RUN_OUT_MAX` (the remote's own send
/// buffer caps it too); truly unbounded streaming as the child produces is a
/// later refinement (see `docs/roadmap-cluster.md`). The `owner` check means only
/// the task that issued the run may pull its output.
const RUN_OUT_MAX: usize = 2048;
struct PendingRun {
    active: bool,
    owner: u64,
    len: usize,
    cursor: usize,
    buf: [u8; RUN_OUT_MAX],
}
impl PendingRun {
    fn new() -> Self {
        PendingRun { active: false, owner: 0, len: 0, cursor: 0, buf: [0; RUN_OUT_MAX] }
    }
    /// Copy the next chunk (up to `out.len()`, capped at `MSG_MAX_LEN`) for task
    /// `who` into `out`; returns its length, or 0 when exhausted / not the owner
    /// (0 = end of stream, which the shell's pull loop stops on).
    fn next_chunk(&mut self, who: u64, out: &mut [u8]) -> usize {
        if !self.active || self.owner != who || self.cursor >= self.len {
            return 0;
        }
        let cap = out.len().min(syscall_abi::MSG_MAX_LEN as usize);
        let chunk = (self.len - self.cursor).min(cap);
        out[..chunk].copy_from_slice(&self.buf[self.cursor..self.cursor + chunk]);
        self.cursor += chunk;
        if self.cursor >= self.len {
            self.active = false; // fully delivered
        }
        chunk
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_run(packed_mac: u64, sender: u64, buf: &[u8], len: usize, out: &mut [u8], conns: &mut [Option<TcpConn>; MAX_CONNS], dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth, pending: &mut PendingRun) -> usize {
    let fail = |out: &mut [u8], msg: &[u8]| -> usize {
        let n = msg.len().min(out.len());
        out[..n].copy_from_slice(&msg[..n]);
        n
    };
    if packed_mac == syscall_abi::NET_ERROR || len < syscall_abi::NETOP_RMOUNT_MSG {
        return fail(out, b"cpu: no network\r\n");
    }
    let ep = syscall_abi::NETOP_RMOUNT_ENDPOINT;
    let ip = [buf[ep], buf[ep + 1], buf[ep + 2], buf[ep + 3]];
    let port = u16::from_le_bytes([buf[ep + 4], buf[ep + 5]]);
    let cmdline = &buf[syscall_abi::NETOP_RMOUNT_MSG..len];
    let hdr = ninep_abi::NP_REQ_PAYLOAD as usize; // 48
    let clen = cmdline.len().min(ninep_abi::NP_NET_MAX - hdr);

    // Build the NP_RUN message: [NP_RUN][tree=0][a0=cmdlen][a1=our ip][a2=our
    // port][cmdline]. a1/a2 carry OUR endpoint (cluster Phase 4b): the remote
    // spawns the command with a /host mount back to us, so it reads *our* files
    // at /host/... while running there. This bare message is then framed + signed.
    let mut np = [0u8; ninep_abi::NP_NET_MAX];
    np[0..8].copy_from_slice(&ninep_abi::NP_RUN.to_le_bytes()); // verb
    np[16..24].copy_from_slice(&(clen as u64).to_le_bytes()); // a0 = cmdlen
    let our = our_ip();
    let our_ip_packed = (our[0] as u64) | ((our[1] as u64) << 8) | ((our[2] as u64) << 16) | ((our[3] as u64) << 24);
    np[24..32].copy_from_slice(&our_ip_packed.to_le_bytes()); // a1 = our ip
    np[32..40].copy_from_slice(&(ninep_abi::NP_NET_PORT as u64).to_le_bytes()); // a2 = our port
    np[hdr..hdr + clen].copy_from_slice(&cmdline[..clen]);

    // WHO is asking, resolved to a name FIRST - see the ordering note in
    // `handle_rmount`. A machine that cannot name its own caller does not get to
    // send an unnamed request; the far side would refuse it anyway, and failing
    // here reports the real reason rather than the peer's.
    let mut who = [0u8; ninep_abi::NP_NAME_LEN];
    match caller_name(&mut who) {
        LineScan::Found => {}
        // Transient (no disk mounted yet, or fsd restarting) vs settled (this
        // machine genuinely has no account for the calling uid). Same refusal,
        // different thing to go and fix.
        LineScan::Unreadable => {
            return fail(out, b"cpu: cannot read /etc/passwd yet to name the calling user (try again)\r\n")
        }
        LineScan::NoDatabase => {
            return fail(out, b"cpu: this machine has no readable /etc/passwd, so it cannot name the calling user\r\n")
        }
        LineScan::NotFound => {
            return fail(out, b"cpu: this machine has no account for the calling user\r\n")
        }
    }

    // Frame + sign it (the export-hardening phase). A no-key client can't sign.
    let mut req = [0u8; ninep_abi::NP_FRAME_MAX];
    // (cpu output is a stream, not a framed reply - reply-auth deferred, so the
    // nonce is unused here.)
    let (total, _nonce) = frame_signed(auth, &who, &np[..hdr + clen], &mut req);
    if total == 0 {
        return fail(out, b"cpu: cannot authenticate (no /etc/cluster/id to sign with, or the request does not fit)\r\n");
    }

    let mac = unpack_mac(packed_mac);
    let Some(dst_mac) = arp_resolve(&mac, &next_hop(&ip)) else {
        return fail(out, b"cpu: remote unreachable\r\n");
    };
    // Pump our own event loop while awaiting the run's output (cluster Phase 4b):
    // the spawned command's /host reads come back to *our* export as frames we
    // must serve *during* the run - a plain blocking tcp_get would deadlock (it
    // would drop those frames while stuck waiting). tcp_run drives the run's
    // client connection AND serves the export + acks the health-ping each pass.
    // Collect the run's output straight into the pending buffer (bounded by
    // RUN_OUT_MAX; the remote's own send buffer caps it too). Then hand the shell
    // the first chunk and let it pull the rest via NETOP_RUN_MORE.
    pending.active = false;
    let (status, got) = tcp_run(packed_mac, &mac, &dst_mac, &ip, port, &req[..total], &mut pending.buf, conns, dials, auth);
    if status != syscall_abi::NET_FETCH_OK {
        return fail(out, b"cpu: remote run failed\r\n");
    }
    pending.owner = sender;
    pending.len = got.min(RUN_OUT_MAX);
    pending.cursor = 0;
    pending.active = true;
    pending.next_chunk(sender, out)
}

fn handle_rmount(packed_mac: u64, buf: &[u8], len: usize, out: &mut [u8], auth: &Auth) -> usize {
    // A bare-status failure reply the shell surfaces cleanly. `st` distinguishes
    // an unreachable peer (NO_FS) from an auth failure (FS_ERR_AUTH).
    let fail = |out: &mut [u8], st: u64| -> usize {
        out[0..8].copy_from_slice(&st.to_le_bytes());
        8
    };
    if packed_mac == syscall_abi::NET_ERROR || len < syscall_abi::NETOP_RMOUNT_MSG {
        return fail(out, syscall_abi::NO_FS);
    }
    let ep = syscall_abi::NETOP_RMOUNT_ENDPOINT;
    let ip = [buf[ep], buf[ep + 1], buf[ep + 2], buf[ep + 3]];
    let port = u16::from_le_bytes([buf[ep + 4], buf[ep + 5]]);
    let msg = &buf[syscall_abi::NETOP_RMOUNT_MSG..len];

    // Frame + sign the NP message (the export-hardening phase): a no-key client
    // can't sign, so it fails with FS_ERR_AUTH rather than sending an unsigned
    // request the remote would reject anyway.
    //
    // WHO is asking, resolved to a name BEFORE any TCP work. Two reasons for the
    // ordering. The credential comes from `SENDER_ID`, which reports the sender
    // of the last message this task received - and the TCP path below re-enters
    // the event loop, which receives more. And the `/etc/passwd` lookup wants its
    // buffers off the stack before the frame and response buffers go live, which
    // is where netd's peak is.
    let mut who = [0u8; ninep_abi::NP_NAME_LEN];
    match caller_name(&mut who) {
        LineScan::Found => {}
        // No name for the caller means this machine cannot say who is asking, so
        // it refuses to send rather than send an unnamed request. The two causes
        // report differently: NO_FS for the transient one (the shell surfaces it
        // as "no filesystem", which is both true and worth retrying), AUTH for a
        // uid this machine has no account for.
        LineScan::Unreadable => return fail(out, syscall_abi::NO_FS),
        // NOT `NO_FS`: the shell renders that as "no filesystem", which reads as
        // "not mounted yet, try again" - wrong for a file that is never coming.
        LineScan::NoDatabase => return fail(out, syscall_abi::FS_ERR_NOT_FOUND),
        LineScan::NotFound => return fail(out, syscall_abi::FS_ERR_AUTH),
    }

    // WHOSE SIGNATURE WILL WE ACCEPT ON THE REPLY? Resolved BEFORE sending.
    //
    // This check used to run after `tcp_get`, which meant a stale or missing
    // line for the dialled address let an NP_WRITE / NP_MKDIR / NP_RM land on
    // the exporter and then reported FS_ERR_AUTH: the write happened, the shell
    // said it did not. That is the same failure shape the send gate below was
    // added for, on a different trigger - so it gets the same treatment. The
    // lookup is a pure function of the authorized file and the address, so
    // hoisting it costs nothing.
    let expect_key = if auth.signing.is_some() {
        match clusterkeys::find_by_ip(auth.authorized(), &ip) {
            Some(peer) => Some(peer.key),
            None => return fail(out, syscall_abi::FS_ERR_AUTH),
        }
    } else {
        None
    };

    let mut req = [0u8; ninep_abi::NP_FRAME_MAX];
    let np = &msg[..msg.len().min(ninep_abi::NP_NET_MAX)];
    let (total, nonce) = frame_signed(auth, &who, np, &mut req);
    if total == 0 {
        return fail(out, syscall_abi::FS_ERR_AUTH);
    }

    let mac = unpack_mac(packed_mac);
    let Some(dst_mac) = arp_resolve(&mac, &next_hop(&ip)) else {
        return fail(out, syscall_abi::NO_FS);
    };

    let mut resp = [0u8; ninep_abi::NP_NET_LEN_PREFIX + ninep_abi::NP_NET_MAX];
    let (status, got) = tcp_get(&mac, &dst_mac, &ip, port, &req[..total], &mut resp, true);
    if status != syscall_abi::NET_FETCH_OK || got < ninep_abi::NP_NET_LEN_PREFIX + 8 {
        return fail(out, syscall_abi::NO_FS);
    }
    // Strip the reply's 4-byte length prefix; the sealed body is
    // `[sig:64][status:u64][data]` (reply-auth). Verify the signature against
    // the nonce WE sent the request with before trusting a single byte - an
    // injected or forged reply fails here.
    let body_len = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]) as usize;
    let body_end = (ninep_abi::NP_NET_LEN_PREFIX + body_len).min(got);
    let body = &resp[ninep_abi::NP_NET_LEN_PREFIX..body_end];

    // The reply is SIGNED. There is no longer a second format to be told apart
    // from, but the length and the signature are still checked before a byte of
    // the reply is trusted.
    let reply_np = {
        let sl = ninep_abi::NP_SIG_LEN;
        if body.len() < sl + 8 {
            return fail(out, syscall_abi::FS_ERR_AUTH);
        }
        let mut sig = [0u8; ninep_abi::NP_SIG_LEN];
        sig.copy_from_slice(&body[..sl]);
        let reply_np = &body[sl..];

        // THE KEY WE EXPECT FOR THE ADDRESS WE DIALLED - not merely some key
        // this machine authorizes.
        //
        // This is the asymmetry the design doc calls Decision 3, and it is the
        // whole reason `authorized` carries an address as well as a key. An
        // exporter looks a client up BY KEY, because the client offers one and
        // the only question is whether it is allowed. A client has no key
        // offered to it: accepting any authorized signature here would
        // authenticate "some cluster member", when the claim that matters is
        // "the machine I asked". Any other member could otherwise answer for it.
        let Some(peer_key) = expect_key else {
            return fail(out, syscall_abi::FS_ERR_AUTH);
        };
        let mut pfx = [0u8; ninep_abi::SIG_DOMAIN_MAX + ninep_abi::NP_NONCE_LEN];
        let tag = ninep_abi::SIG_DOMAIN_REPLY;
        pfx[..tag.len()].copy_from_slice(tag);
        pfx[tag.len()..tag.len() + nonce.len()].copy_from_slice(&nonce);
        if !ed25519::verify_prefixed(&peer_key, &pfx[..tag.len() + nonce.len()], reply_np, &sig) {
            return fail(out, syscall_abi::FS_ERR_AUTH);
        }
        reply_np
    };
    let n = reply_np.len().min(out.len());
    out[..n].copy_from_slice(&reply_np[..n]);
    n
}

/// The state of the one in-flight server connection.
/// Longest request path the server will resolve.
const PATH_MAX: usize = 128;
/// Capacity of a connection's response prefix: a whole fixed response, a
/// file's 200 header, *or* a generated directory-listing page. Sized for the
/// last of these - fsd's inline listing is capped at ~512 bytes of names,
/// which expands into HTML with links; 2 KB holds a typical listing (a larger
/// one is truncated).
const PREFIX_MAX: usize = 2048;

enum ConnState {
    /// Sent SYN-ACK, waiting for the peer's ACK (or its request data).
    SynRcvd,
    /// Handshake complete; the response is being streamed (paced by the
    /// peer's window - see `pump_send`).
    Established,
    /// Our FIN sent, waiting for the peer to finish closing.
    Closing,
}

/// One server-side TCP connection. `netd` serves one at a time - a second
/// peer's SYN while a connection is active just replaces it (single-threaded,
/// no concurrency), the same "one at a time, fixed values suffice" stance the
/// client side takes.
struct TcpConn {
    peer_ip: [u8; 4],
    peer_mac: [u8; 6],
    peer_port: u16,
    /// The local port this connection is served on - 80 (HTTP) or `NP_NET_PORT`
    /// 564 (9P export). Replies go out from this source port, and it selects
    /// which handler an incoming request goes to (`start_response` vs `handle_9p`).
    local_port: u16,
    /// Our next send sequence number (the seq of the next byte we'll send).
    snd_nxt: u32,
    /// The oldest unacknowledged sequence number - advanced by the peer's
    /// ACKs. `snd_nxt - snd_una` is what's currently in flight.
    snd_una: u32,
    /// The next sequence number we expect from the peer.
    rcv_nxt: u32,
    /// The peer's advertised receive window (bytes it will accept beyond
    /// `snd_una`) - the flow-control limit on how much we may have in flight.
    window: u32,
    state: ConnState,
    /// Whether we've begun responding (guards a retransmitted request from
    /// restarting the response).
    responded: bool,
    // --- the response being streamed ---
    /// A prefix sent first: the whole response for `/`/404/503, or the 200
    /// header (with Content-Type/Content-Length) ahead of a file's body. An
    /// owned buffer, not a `'static` slice, so the 200 header can be built
    /// per-request.
    prefix: [u8; PREFIX_MAX],
    /// How many bytes of `prefix` are valid.
    prefix_len: usize,
    /// How much of `prefix` has been sent.
    prefix_off: usize,
    /// Whether a file body follows the prefix (streamed from `fsd`).
    file: bool,
    path: [u8; PATH_MAX],
    path_len: usize,
    /// Next file byte offset to read/send.
    read_off: u64,
    /// Whether the body is fully sent (EOF or a read error).
    eof: bool,
    /// Whether our FIN has been sent.
    fin_sent: bool,
    /// Consecutive duplicate ACKs seen at `snd_una` (with data outstanding).
    /// Three triggers a fast retransmit: the peer is telling us a segment was
    /// lost by re-acking the last in-order byte for each out-of-order segment
    /// it receives past the gap.
    dup_acks: u8,
    /// The `snd_una` we last fast-retransmitted at - guards against a second
    /// (wasteful) retransmit for the *same* gap when the leftover dup-ACKs from
    /// the original out-of-order burst keep arriving. Reset on real progress.
    last_rexmit_una: u32,
    /// Retransmit-timeout (RTO) timer: the tick (from `now()`) at which
    /// unacked data is presumed lost and resent; 0 = timer off (nothing
    /// outstanding). The fallback to fast retransmit for when the peer goes
    /// silent and no dup-ACKs arrive.
    rto_deadline: u64,
    /// The `snd_una` the RTO timer was last (re)armed at, to detect forward
    /// progress (an advancing ACK) between checks.
    rto_snd_una: u32,
    /// Consecutive RTO firings without progress - drives exponential backoff
    /// and the eventual give-up (a dead peer).
    rto_retries: u8,
    /// Smoothed round-trip time and its variation, in microseconds (RFC 6298;
    /// see `update_rtt`). `srtt_us == 0` means no sample taken yet.
    srtt_us: u64,
    rttvar_us: u64,
    /// The current estimated RTO, in `now()` ticks - `RTO_INIT_TICKS` until the
    /// first RTT sample, then derived from `srtt_us`/`rttvar_us`. `service_rto`
    /// arms the timer with this instead of a fixed base.
    rto_ticks: u64,
    /// Highest sequence number ever *newly* sent (the send high-water mark) -
    /// used to distinguish new data from a retransmit when starting an RTT
    /// sample (Karn's algorithm: only new data is timed).
    snd_max: u32,
    /// Whether an RTT sample is currently outstanding. Started when new data is
    /// sent (see `rtt_on_send`); completed when `rtt_seq` is acked; invalidated
    /// by any retransmit (`rewind_to`), per Karn.
    rtt_active: bool,
    /// The sequence number that, once acked, completes the outstanding RTT
    /// sample (the end seq of the timed segment).
    rtt_seq: u32,
    /// `MONOTONIC_US` reading when the timed segment was sent.
    rtt_start_us: u64,
    /// Congestion window and slow-start threshold, in bytes (TCP Reno; see the
    /// `INIT_CWND` etc. constants). The send window is `min(cwnd, peer
    /// window)`.
    cwnd: u32,
    ssthresh: u32,
    /// Remote-execution (cluster Phase 4a): the scheduler slot of the spawned
    /// `cpu` child whose stdout this connection is capturing, or `CPU_NONE`
    /// (0xFF) when this isn't a run connection. While set, `pump_send` holds off
    /// (the response isn't ready); the child's output messages accumulate into
    /// `prefix`, and its end-of-stream reaps it, clears this, and lets the
    /// accumulated output stream out then FIN.
    cpu_child: u8,
}

/// `TcpConn::cpu_child` sentinel: this connection is not a remote-run capture.
const CPU_NONE: u8 = 0xFF;

// ---------------------------------------------------------------------------
// Dial-out: /net/tcp connection files (the Plan 9 `/net/tcp` model, scoped).
// A client (local, or a remote machine through the export) opens a TCP
// connection *out of this machine's NIC* by reading /net/tcp/clone (-> a
// connection number N), writing "connect ip!port" to /net/tcp/N/ctl, then
// writing/reading /net/tcp/N/data. The connection handle lives in the PATH
// (N), so no protocol fids are needed - each op is an ordinary path-based NP
// verb addressing slot N (the Phase-0 path-based-verbs design, extended).
//
// Architecture: net_op NEVER blocks or drives recv - it only mutates DialConn
// state and moves bytes to/from the per-conn buffers. ALL the TCP work (send
// the SYN when Connecting, complete the handshake, ACK + buffer inbound data,
// retransmit, send the FIN) happens in the event loop: `pump_dials` each pass
// and `dial_on_segment` for inbound frames - exactly the model the server-side
// `conns` already use. Reliability is honestly scoped to **stop-and-wait**
// (one segment outstanding at a time, no cwnd/SACK) - adequate for the small
// request/response transactions this primitive targets, documented as such.
// ---------------------------------------------------------------------------

/// Concurrent dial-out connections. Bounded (no heap), like `MAX_CONNS`. Kept
/// small because each `DialConn` carries its own send+recv buffers and the whole
/// `[Option<DialConn>; MAX_DIAL]` array lives on `serve`'s (guard-paged) stack -
/// so these three sizes trade directly against netd's stack headroom.
// A listener + a couple of concurrent accepted connections ("small fan-out").
// Each DialConn carries its send+recv buffers and the whole array lives on
// serve()'s guard-paged (32 KB) stack, so this is capped tight - 4 overflowed.
const MAX_DIAL: usize = 3;
/// Per-connection send buffer: bytes the client has queued (via a /data write)
/// that are not yet sent-and-acked. One small request's worth (stop-and-wait).
const DIAL_SBUF: usize = 512;
/// Per-connection receive buffer: bytes received from the peer awaiting a
/// /data read. The peer is flow-controlled to it (we stop ACKing new data when
/// it's full); the client should read promptly. Modest to bound stack use.
const DIAL_RBUF: usize = 768;
/// Ticks (`now()`) before an idle dial connection is reaped (bounded table, so
/// an abandoned connection can't leak a slot forever).
const DIAL_IDLE_TICKS: u64 = 600;

#[derive(Clone, Copy, PartialEq)]
enum DialState {
    /// Slot is unallocated.
    Free,
    /// Allocated by a `clone` read, not yet connected (no `connect` ctl yet).
    Idle,
    /// `connect` issued; the SYN is being (re)sent and the SYN-ACK awaited
    /// (active open - dial-out).
    Connecting,
    /// `announce <port>` issued (dial-in): this slot is a passive **listener**,
    /// accepting inbound connections on `announce_port`. It has no peer of its
    /// own; each accepted connection is a *separate* slot (see `dial_accept`).
    Listening,
    /// A passively-accepted inbound connection: we received a SYN on a listener's
    /// port, sent the SYN-ACK, and await the peer's final ACK (passive open).
    Accepting,
    /// Handshake complete; data flows.
    Established,
    /// `close` issued (or the peer FIN'd); our FIN is being sent / we're
    /// draining. Buffered receive data is still readable until drained.
    Closing,
    /// Fully closed (peer refused, reset, or clean teardown done). A final
    /// `status` read reports this; the slot is freed on the next `clone`/GC.
    Closed,
}

/// One dial-out (client) TCP connection, addressed by its slot index N in the
/// path `/net/tcp/N/...`. Leaner than [`TcpConn`] (no file serving, no
/// congestion control) - stop-and-wait reliability with a send and a receive
/// buffer.
struct DialConn {
    state: DialState,
    peer_ip: [u8; 4],
    peer_mac: [u8; 6],
    peer_port: u16,
    /// Our ephemeral source port (the local half of the 4-tuple; inbound
    /// segments addressed here belong to this connection).
    src_port: u16,
    /// Our initial sequence number (for validating the SYN-ACK's ack).
    isn: u32,
    /// Next sequence number we'll send.
    snd_nxt: u32,
    /// Oldest unacked sequence number (peer ACKs advance it).
    snd_una: u32,
    /// Next sequence number expected from the peer.
    rcv_nxt: u32,
    /// Whether our FIN has been sent (in Closing).
    fin_sent: bool,
    /// Send buffer: `sbuf[..slen]` is queued client data; `[..inflight]` has
    /// been sent (awaiting ACK), `[inflight..slen]` is unsent. An ACK removes
    /// acked bytes from the front (compacting); an RTO resends `[..inflight]`.
    sbuf: [u8; DIAL_SBUF],
    slen: usize,
    inflight: usize,
    /// Receive buffer: `rbuf[..rlen]` is data received, awaiting a /data read.
    rbuf: [u8; DIAL_RBUF],
    rlen: usize,
    /// Retransmit timer (a `now()` tick; 0 = off) and try counter, for the SYN
    /// and for outstanding data (stop-and-wait, so one thing at a time).
    retx_deadline: u64,
    retries: u8,
    /// Last activity tick, for idle GC.
    last_activity: u64,
    /// Set once the peer's FIN has been received (so a drained read reports EOF
    /// / the connection can finish closing).
    peer_fin: bool,
    /// For a `Listening` slot (dial-in): the port it accepts inbound connections
    /// on (0 otherwise). `src_port` doubles as this too for an accepted conn -
    /// see `dial_accept`.
    announce_port: u16,
    /// For a passively-accepted connection: the slot index of the `Listening`
    /// conn that accepted it (`DIAL_NO_PARENT` for a dial-out / listener slot).
    /// `listen` on the parent hands out accepted conns whose `pending` is set.
    parent: u8,
    /// A freshly-accepted connection not yet handed to the client via a `listen`
    /// read. Cleared once `listen` returns its number.
    pending: bool,
}

/// `DialConn::parent` sentinel: this conn is not a passively-accepted one.
const DIAL_NO_PARENT: u8 = 0xFF;

/// A fresh `Idle` dial connection with the given local (source) port. Shared by
/// the `clone` allocation and the passive `dial_accept` path (which then fills
/// in the peer + sequence state).
fn new_dial(src_port: u16) -> DialConn {
    DialConn {
        state: DialState::Idle,
        peer_ip: [0; 4],
        peer_mac: [0; 6],
        peer_port: 0,
        src_port,
        isn: TCP_ISN ^ ((src_port as u32) << 8),
        snd_nxt: 0,
        snd_una: 0,
        rcv_nxt: 0,
        fin_sent: false,
        sbuf: [0; DIAL_SBUF],
        slen: 0,
        inflight: 0,
        rbuf: [0; DIAL_RBUF],
        rlen: 0,
        retx_deadline: 0,
        retries: 0,
        last_activity: now(),
        peer_fin: false,
        announce_port: 0,
        parent: DIAL_NO_PARENT,
        pending: false,
    }
}

/// Find a free dial slot (unused or `Free`), or `None` if the table is full.
fn alloc_dial_slot(dials: &[Option<DialConn>; MAX_DIAL]) -> Option<usize> {
    dials
        .iter()
        .position(|c| c.is_none() || matches!(c, Some(d) if d.state == DialState::Free))
}

/// A parsed inbound TCP segment addressed to our listen port.
struct TcpIn {
    src_ip: [u8; 4],
    src_mac: [u8; 6],
    src_port: u16,
    /// The local port the segment is addressed to - 80 (HTTP) or
    /// `NP_NET_PORT` 564 (9P export); the connection remembers it so replies
    /// go out from the right source port.
    dst_port: u16,
    seq: u32,
    ack: u32,
    window: u16,
    flags: u8,
    /// Byte offset of the TCP payload within the frame, and its length.
    data_off: usize,
    data_len: u32,
    /// SACK blocks `(left, right)` parsed from the segment's TCP options (RFC
    /// 2018), for sender-side selective retransmit. `sack_n` is how many are
    /// valid; empty on a segment carrying no SACK option.
    sack: [(u32, u32); MAX_SACK],
    sack_n: usize,
}

/// Dispatch one received frame: answer ARP requests for our IP, and feed TCP
/// segments to the HTTP server. Everything else (including our own client
/// ops' replies, which the synchronous handlers already consumed) is ignored.
fn on_frame(mac: &[u8; 6], frame: &[u8], conns: &mut [Option<TcpConn>; MAX_CONNS], dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = ((frame[12] as u16) << 8) | frame[13] as u16;
    match ethertype {
        0x0806 => handle_arp(mac, frame),
        0x0800 => {
            // A dial-out (client) connection's segment is addressed to our
            // ephemeral source port, not a listen port - try those first (and a
            // retransmitted SYN for an already-accepted dial-in conn matches here
            // too, so it never double-accepts below).
            if dial_on_segment(mac, frame, dials) {
                return;
            }
            // A fresh inbound SYN to an `announce`d port (dial-in) - accept it.
            if dial_accept(mac, frame, dials) {
                return;
            }
            if let Some(seg) = parse_tcp_in(frame) {
                handle_tcp(mac, frame, &seg, conns, dials, auth);
            }
        }
        _ => {}
    }
}

/// Answer an ARP request for our IP (so the gateway/host can resolve us before
/// forwarding a connection to the server). Ignores requests for anyone else.
fn handle_arp(mac: &[u8; 6], frame: &[u8]) {
    if frame.len() < 42 {
        return;
    }
    if frame[20] != 0x00 || frame[21] != 0x01 || frame[38..42] != our_ip() {
        return; // not an ARP request, or not for us
    }
    let mut req_mac = [0u8; 6];
    req_mac.copy_from_slice(&frame[22..28]);
    let mut req_ip = [0u8; 4];
    req_ip.copy_from_slice(&frame[28..32]);
    let mut arp = [0u8; 42];
    arp[0..6].copy_from_slice(&req_mac); // eth dst: the requester
    arp[6..12].copy_from_slice(mac); // eth src: us
    arp[12..14].copy_from_slice(&[0x08, 0x06]); // ethertype: ARP
    arp[14..16].copy_from_slice(&[0x00, 0x01]); // htype: Ethernet
    arp[16..18].copy_from_slice(&[0x08, 0x00]); // ptype: IPv4
    arp[18] = 6; // hlen
    arp[19] = 4; // plen
    arp[20..22].copy_from_slice(&[0x00, 0x02]); // oper: reply
    arp[22..28].copy_from_slice(mac); // sha: us
    arp[28..32].copy_from_slice(&our_ip()); // spa: us
    arp[32..38].copy_from_slice(&req_mac); // tha: the requester
    arp[38..42].copy_from_slice(&req_ip); // tpa: the requester
    let _ = send(&arp);
}

/// Parse an inbound frame as a TCP segment addressed to our listen port,
/// returning the peer's address and the segment's fields (using the IP
/// total-length field to find the true payload end, ignoring Ethernet
/// padding). `None` if it isn't IPv4/TCP to port 80.
fn parse_tcp_in(frame: &[u8]) -> Option<TcpIn> {
    if frame.len() < 34 || frame[12] != 0x08 || frame[13] != 0x00 {
        return None; // not IPv4
    }
    if frame[23] != 6 {
        return None; // not TCP
    }
    let ihl = (frame[14] & 0x0f) as usize * 4;
    let t = 14 + ihl;
    if frame.len() < t + 20 {
        return None;
    }
    let dst_port = u16be(frame, t + 2);
    if dst_port != SERVER_PORT && dst_port != ninep_abi::NP_NET_PORT {
        return None; // not to a listen port (HTTP 80 or 9P-export 564)
    }
    let data_off = t + ((frame[t + 12] >> 4) as usize) * 4;
    let ip_total = u16be(frame, 16) as usize;
    let seg_end = (14 + ip_total).min(frame.len());
    let data_len = seg_end.saturating_sub(data_off) as u32;

    // Walk the TCP options (between the fixed 20-byte header and the data) for
    // SACK blocks (kind 5). kind 0 ends the list, kind 1 is a one-byte NOP,
    // everything else is [kind, len, ...len-2 bytes]. A SACK option's body is
    // 8-byte (left, right) big-endian pairs.
    let mut sack = [(0u32, 0u32); MAX_SACK];
    let mut sack_n = 0usize;
    let opt_end = data_off.min(frame.len());
    let mut i = t + 20;
    while i < opt_end {
        match frame[i] {
            0 => break,
            1 => i += 1,
            _ => {
                if i + 1 >= opt_end {
                    break;
                }
                let len = frame[i + 1] as usize;
                if len < 2 || i + len > opt_end {
                    break;
                }
                if frame[i] == 5 {
                    let mut j = i + 2;
                    while j + 8 <= i + len && sack_n < MAX_SACK {
                        sack[sack_n] = (u32be(frame, j), u32be(frame, j + 4));
                        sack_n += 1;
                        j += 8;
                    }
                }
                i += len;
            }
        }
    }
    let mut src_ip = [0u8; 4];
    src_ip.copy_from_slice(&frame[26..30]);
    let mut src_mac = [0u8; 6];
    src_mac.copy_from_slice(&frame[6..12]);
    Some(TcpIn {
        src_ip,
        src_mac,
        src_port: u16be(frame, t),
        dst_port,
        seq: u32be(frame, t + 4),
        ack: u32be(frame, t + 8),
        window: u16be(frame, t + 14),
        flags: frame[t + 13],
        data_off,
        data_len,
        sack,
        sack_n,
    })
}

/// Route one inbound TCP segment to its connection, multiplexing up to
/// `MAX_CONNS` at once (keyed by the peer's IP+port). A SYN opens (or, for the
/// same peer, restarts) a connection in a free slot - or is dropped if all
/// slots are busy (the peer retransmits); every other segment is dispatched to
/// its matching connection via [`handle_conn_segment`], and a returned "close"
/// (the peer's FIN, or an RST) frees the slot.
fn handle_tcp(mac: &[u8; 6], frame: &[u8], seg: &TcpIn, conns: &mut [Option<TcpConn>; MAX_CONNS], dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth) {
    // RST tears down a matching connection.
    if seg.flags & TCP_RST != 0 {
        if let Some(i) = find_conn(conns, seg) {
            conns[i] = None;
        }
        return;
    }

    // SYN (without ACK): open the connection in this peer's existing slot (a
    // retransmitted/duplicate SYN) or a free one, and answer with SYN-ACK.
    if seg.flags & TCP_SYN != 0 && seg.flags & TCP_ACK == 0 {
        let Some(i) = find_conn(conns, seg).or_else(|| free_slot(conns)) else {
            return; // all slots busy - drop; the peer will retransmit
        };
        let mut c = TcpConn {
            peer_ip: seg.src_ip,
            peer_mac: seg.src_mac,
            peer_port: seg.src_port,
            local_port: seg.dst_port,
            snd_nxt: SERVER_ISN,
            snd_una: SERVER_ISN,
            rcv_nxt: seg.seq.wrapping_add(1), // SYN consumes a sequence number
            window: seg.window as u32,
            state: ConnState::SynRcvd,
            responded: false,
            prefix: [0; PREFIX_MAX],
            prefix_len: 0,
            prefix_off: 0,
            file: false,
            path: [0; PATH_MAX],
            path_len: 0,
            read_off: 0,
            eof: false,
            fin_sent: false,
            dup_acks: 0,
            last_rexmit_una: 0,
            rto_deadline: 0,
            rto_snd_una: SERVER_ISN,
            rto_retries: 0,
            srtt_us: 0,
            rttvar_us: 0,
            rto_ticks: RTO_INIT_TICKS,
            snd_max: SERVER_ISN,
            rtt_active: false,
            rtt_seq: 0,
            rtt_start_us: 0,
            cwnd: INIT_CWND,
            ssthresh: INIT_SSTHRESH,
            cpu_child: CPU_NONE,
        };
        send_seg(mac, &c, TCP_SYN | TCP_ACK, true, &[]);
        c.snd_nxt = c.snd_nxt.wrapping_add(1); // our SYN consumes one too
        c.snd_max = c.snd_nxt; // data starts here; the SYN itself isn't timed
        conns[i] = Some(c);
        return;
    }

    // Everything else is dispatched to its matching connection (if any).
    if let Some(i) = find_conn(conns, seg) {
        if handle_conn_segment(mac, frame, seg, conns[i].as_mut().unwrap(), dials, auth) {
            conns[i] = None;
        }
    }
}

/// The per-connection TCP state machine for one non-SYN segment: process its
/// ACK (flow-control window, duplicate-ACK fast retransmit), complete the
/// handshake, set up the response from the request, and handle the peer's FIN.
/// Returns `true` when the connection should be closed (the peer FIN'd). The
/// actual sending (`pump_send`) is *not* done here - it runs once per
/// event-loop wake so the health-ping stays promptly acked (see `serve`).
fn handle_conn_segment(mac: &[u8; 6], frame: &[u8], seg: &TcpIn, c: &mut TcpConn, dials: &mut [Option<DialConn>; MAX_DIAL], auth: &Auth) -> bool {
    // Track the peer's ACK (frees send window), count duplicate ACKs, and
    // update the advertised window.
    if seg.flags & TCP_ACK != 0 {
        if seq_gt(seg.ack, c.snd_una) {
            let acked = seg.ack.wrapping_sub(c.snd_una); // new bytes acknowledged
            c.snd_una = seg.ack; // forward progress
            c.dup_acks = 0;
            // Congestion control (Reno) grows the window on each new ACK: in
            // slow start (cwnd < ssthresh) by up to a segment per ACK
            // (exponential, ~doubles per RTT); in congestion avoidance by
            // ~MSS*MSS/cwnd per ACK (~one segment per RTT). Capped at MAX_CWND.
            if c.cwnd < c.ssthresh {
                c.cwnd = c.cwnd.saturating_add(acked.min(MSS));
            } else {
                c.cwnd = c.cwnd.saturating_add((MSS * MSS / c.cwnd).max(1));
            }
            c.cwnd = c.cwnd.min(MAX_CWND);
            // Complete an outstanding RTT sample if this ACK covers the timed
            // segment's end seq (and it wasn't invalidated by a retransmit -
            // Karn; see rewind_to). !seq_gt(rtt_seq, ack) == ack >= rtt_seq.
            if c.rtt_active && !seq_gt(c.rtt_seq, seg.ack) {
                let rtt = now_us().wrapping_sub(c.rtt_start_us);
                rtt_update(c, rtt);
                c.rtt_active = false;
            }
            // After a go-back-N rewind, the peer (which buffers out-of-order
            // segments) can ack *past* the rewound snd_nxt in one jump once the
            // retransmit fills the gap. Keep the invariant snd_nxt >= snd_una -
            // otherwise snd_nxt - snd_una wraps huge and the window looks full
            // forever. Fast-forward the send cursor to snd_una (nothing below
            // it is unsent-and-unacked, so there's nothing to resend there).
            if seq_gt(c.snd_una, c.snd_nxt) {
                rewind_to(c, c.snd_una);
            }
        } else if seg.ack == c.snd_una && c.snd_nxt != c.snd_una && seg.data_len == 0 {
            // A duplicate ACK with data still outstanding: the peer received an
            // out-of-order segment (a gap before it). Three of these => fast
            // retransmit - rewind the send cursor to snd_una and resend from
            // there (go-back-N); the next pump does the actual sending.
            c.dup_acks = c.dup_acks.saturating_add(1);
            if c.dup_acks >= 3 && c.last_rexmit_una != c.snd_una {
                // Congestion control: a fast retransmit is a moderate loss
                // signal - halve the window (multiplicative decrease) rather
                // than collapsing to slow start the way an RTO does.
                c.ssthresh = (c.cwnd / 2).max(MIN_CWND);
                c.cwnd = c.ssthresh;
                // With SACK blocks, resend only the hole (selective retransmit)
                // and leave the forward cursor alone; otherwise go-back-N.
                if seg.sack_n > 0 {
                    sack_retransmit(mac, c, seg);
                } else {
                    rewind_to(c, c.snd_una);
                }
                c.last_rexmit_una = c.snd_una;
                c.dup_acks = 0;
            }
        }
    }
    c.window = seg.window as u32;

    // A bare ACK completes the handshake.
    if matches!(c.state, ConnState::SynRcvd) && seg.flags & TCP_ACK != 0 {
        c.state = ConnState::Established;
    }

    // The request (may arrive with the handshake ACK, or just after) -> set up
    // the response. Only once; the streaming itself is driven by pump_send.
    if !c.responded && seg.data_len > 0 && seg.seq == c.rcv_nxt {
        c.rcv_nxt = c.rcv_nxt.wrapping_add(seg.data_len);
        c.responded = true;
        let end = (seg.data_off + seg.data_len as usize).min(frame.len());
        let request = &frame[seg.data_off..end];
        // Dispatch by the local listen port: 80 is the HTTP server, 564 the 9P
        // export gateway (cluster Phase 1). Both stage a response in `c.prefix`
        // for pump_send to stream.
        if c.local_port == ninep_abi::NP_NET_PORT {
            handle_9p(c, request, dials, mac, auth);
        } else {
            start_response(c, request);
        }
    }

    // Note: the actual sending (pump_send) is NOT done here. It runs once per
    // event-loop wake, *after* the whole frame batch is drained and the client
    // mailbox is serviced - so a supervisor health-ping is always acked
    // promptly and a burst can never block netd long enough to look wedged.
    // This ACK just updated snd_una/window above; the next pump uses it.

    // The peer's FIN: ack it (past any data it carried plus the FIN) and close.
    if seg.flags & TCP_FIN != 0 {
        c.rcv_nxt = seg.seq.wrapping_add(seg.data_len).wrapping_add(1);
        send_seg(mac, c, TCP_ACK, false, &[]);
        return true;
    }
    false
}

/// Index of the connection matching `seg`'s peer (IP + source port), if any.
fn find_conn(conns: &[Option<TcpConn>; MAX_CONNS], seg: &TcpIn) -> Option<usize> {
    conns.iter().position(|c| {
        matches!(c, Some(c) if c.peer_ip == seg.src_ip && c.peer_port == seg.src_port)
    })
}

/// Index of a free connection slot, if any.
fn free_slot(conns: &[Option<TcpConn>; MAX_CONNS]) -> Option<usize> {
    conns.iter().position(|c| c.is_none())
}

/// Wrapping "is `a` sequence-after `b`" - handles the u32 sequence-number
/// wraparound the raw `>` can't.
fn seq_gt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// Rewind the send cursor to sequence `seq` (for a fast retransmit), so the
/// next `pump_send` resends everything from there (go-back-N). The response is
/// a fixed byte stream - `[prefix][file body from offset 0][FIN]` starting at
/// `SERVER_ISN + 1` (our SYN consumed one seq) - so a sequence number maps
/// straight back to a byte offset: prefix bytes, then file bytes re-read from
/// fsd, then the FIN. Resetting `eof`/`fin_sent` lets pump re-derive the tail.
fn rewind_to(c: &mut TcpConn, seq: u32) {
    let data_base = SERVER_ISN.wrapping_add(1);
    let off = seq.wrapping_sub(data_base) as usize;
    c.snd_nxt = seq;
    c.prefix_off = off.min(c.prefix_len);
    c.read_off = off.saturating_sub(c.prefix_len) as u64;
    c.eof = false;
    c.fin_sent = false;
    // Karn's algorithm: a retransmit makes any outstanding RTT sample
    // ambiguous (was the ACK for the original or the resend?), so drop it.
    // The next genuinely-new send starts a fresh sample.
    c.rtt_active = false;
}

/// Service the retransmit timer, called once per event-loop wake. Manages the
/// RTO for the one connection: arm it while data is outstanding, restart it on
/// forward progress (an advancing ACK, resetting backoff), and on expiry -
/// which only happens when the peer has gone silent, since a live peer's
/// dup-ACKs would have triggered fast retransmit first - resend from `snd_una`
/// (go-back-N via `rewind_to`; the next pump does the sending) with
/// exponential backoff. Returns `true` when the peer is unresponsive past
/// `RTO_MAX_RETRIES` (a RST is sent and the caller drops the connection).
fn service_rto(mac: &[u8; 6], c: &mut TcpConn, now: u64) -> bool {
    let in_flight = c.snd_nxt.wrapping_sub(c.snd_una);
    if in_flight == 0 {
        c.rto_deadline = 0; // nothing outstanding - timer off
        c.rto_retries = 0;
        c.rto_snd_una = c.snd_una;
        return false;
    }
    if c.snd_una != c.rto_snd_una {
        // Forward progress since the last check: restart the timer, reset backoff.
        c.rto_snd_una = c.snd_una;
        c.rto_retries = 0;
        c.rto_deadline = now + c.rto_ticks;
        return false;
    }
    if c.rto_deadline == 0 {
        c.rto_deadline = now + c.rto_ticks; // just became outstanding - arm it (estimated RTO)
        return false;
    }
    if now >= c.rto_deadline {
        if c.rto_retries >= RTO_MAX_RETRIES {
            send_seg(mac, c, TCP_RST, false, &[]); // peer is dead - abort
            return true;
        }
        // Congestion control: a timeout is the strongest loss signal - halve
        // ssthresh and collapse cwnd to one segment, restarting slow start.
        c.ssthresh = (c.cwnd / 2).max(MIN_CWND);
        c.cwnd = MSS;
        rewind_to(c, c.snd_una); // presume loss, resend from snd_una
        c.rto_retries += 1;
        c.rto_deadline = now + (c.rto_ticks << c.rto_retries.min(4)); // exp. backoff, capped
    }
    false
}

/// Build and send one server-direction segment for connection `c`.
fn send_seg(mac: &[u8; 6], c: &TcpConn, flags: u8, with_mss: bool, payload: &[u8]) {
    let mut out = [0u8; 1600];
    if let Some(n) = build_tcp_srv(
        mac,
        &c.peer_mac,
        &c.peer_ip,
        c.peer_port,
        c.local_port,
        c.snd_nxt,
        c.rcv_nxt,
        flags,
        with_mss,
        payload,
        &mut out,
    ) {
        let _ = send(&out[..n]);
    }
}

/// Send a server segment at an explicit sequence number (not `c.snd_nxt`) -
/// used by selective retransmit to resend a specific gap without disturbing
/// the forward send cursor.
fn send_seg_at(mac: &[u8; 6], c: &TcpConn, seq: u32, flags: u8, payload: &[u8]) {
    let mut out = [0u8; 1600];
    if let Some(n) = build_tcp_srv(
        mac, &c.peer_mac, &c.peer_ip, c.peer_port, c.local_port, seq, c.rcv_nxt, flags, false, payload, &mut out,
    ) {
        let _ = send(&out[..n]);
    }
}

/// Retransmit one segment of the response stream at sequence `seq` (up to `n`
/// bytes), reading the bytes from the prefix or the file at the matching
/// offset - the same seq->offset mapping `rewind_to` uses, but as a one-off
/// resend that leaves `snd_nxt`/`prefix_off`/`read_off` untouched.
fn retransmit_one(mac: &[u8; 6], c: &mut TcpConn, seq: u32, n: usize) {
    let data_base = SERVER_ISN.wrapping_add(1);
    let off = seq.wrapping_sub(data_base) as usize;
    let n = n.min(SERVE_CHUNK);
    let mut buf = [0u8; SERVE_CHUNK];
    if off < c.prefix_len {
        // Within the prefix (the 200 header / fixed response). A retransmit
        // never straddles the prefix->body boundary here, since a lost segment
        // lies entirely in one or the other.
        let take = n.min(c.prefix_len - off);
        buf[..take].copy_from_slice(&c.prefix[off..off + take]);
        send_seg_at(mac, c, seq, TCP_PSH | TCP_ACK, &buf[..take]);
    } else if c.file {
        let foff = (off - c.prefix_len) as u64;
        let r = read_file_chunk(As::Anonymous, &c.path[..c.path_len], 0, foff, &mut buf[..n]);
        if r < syscall_abi::FS_ERR_MIN {
            let got = r as usize;
            send_seg_at(mac, c, seq, TCP_PSH | TCP_ACK, &buf[..got]);
        }
    }
}

/// Selective retransmit (RFC 2018 sender side): resend only the first hole -
/// `[snd_una, the lowest SACK left-edge above snd_una)` - the peer's SACK
/// blocks say it already holds everything above that, so unlike go-back-N this
/// doesn't rewind the forward cursor or resend the SACKed data. Bounded to a
/// few segments per event; a larger hole is finished by the next dup-ACK round
/// or the RTO. Falls back to go-back-N if no block sits above snd_una.
fn sack_retransmit(mac: &[u8; 6], c: &mut TcpConn, seg: &TcpIn) {
    let mut hole_end = 0u32;
    let mut found = false;
    for k in 0..seg.sack_n {
        let l = seg.sack[k].0;
        if seq_gt(l, c.snd_una) && (!found || seq_gt(hole_end, l)) {
            hole_end = l;
            found = true;
        }
    }
    if !found {
        rewind_to(c, c.snd_una); // no usable SACK block - go-back-N
        return;
    }
    let mut seq = c.snd_una;
    let mut guard = 0;
    while seq_gt(hole_end, seq) && guard < 8 {
        let n = (hole_end.wrapping_sub(seq) as usize).min(SERVE_CHUNK);
        retransmit_one(mac, c, seq, n);
        seq = seq.wrapping_add(n as u32);
        guard += 1;
    }
}

/// Decide the response for `request` and record it on the connection (the
/// bytes are then streamed by `pump_send`, paced by the window). A path that
/// resolves to a **file** is streamed from the filesystem server (`fsd`) with
/// a 200 header; a path that resolves to a **directory** (including `/`)
/// returns a generated HTML **index** of its entries, with links, so the
/// filesystem is browsable; anything else is a 404, and a missing/unmounted
/// filesystem a 503. netd is `fsd`'s first non-shell client. The file is
/// *stat*'d (one `NP_READ_FILE`, whose status is the real size) - which
/// checks existence and gives the size for `Content-Length`; its body then
/// streams from offset 0 (fsd reads are idempotent, offset-based).
/// Largest inline data chunk an exported bulk read returns per request - fits
/// the prefix buffer with the 12-byte frame header, and about one TCP segment.
const EXPORT_CHUNK: usize = 1400;

/// A framed NP message must hold a request header plus a full bulk chunk.
///
/// HERE RATHER THAN IN `ninep-abi` because this is the only place both real
/// constants are in scope. `ninep-abi` asserted it against a hand-copied
/// `SAFECOPY_MAX = 2048`, so raising `syscall_abi::SAFECOPY_MAX` — which its own
/// doc invites — left the assertion comparing against the stale copy and
/// passing, while export reads silently stopped fitting. There is zero slack
/// today (2096 == 48 + 2048), so this is not a theoretical margin.
const _: () = assert!(
    ninep_abi::NP_NET_MAX >= ninep_abi::NP_REQ_PAYLOAD as usize + syscall_abi::SAFECOPY_MAX as usize,
    "NP_NET_MAX cannot hold an NP request header plus a full SAFECOPY_MAX chunk"
);

/// The reply prefix must have room for the largest reply AND its signature.
///
/// A COMPILE-TIME ASSERTION, not a test, because it is a relationship between
/// three constants in two crates and the thing that breaks it is editing one of
/// them. `seal_reply_signed`'s no-room arm is correct but degrades the export to
/// refusing every large read with `FS_ERR_AUTH` - a failure that looks like a key
/// problem between two correctly-keyed machines - so it must stay unreachable,
/// and "unreachable by 572 bytes of slack nobody re-derives" is not a guarantee.
///
/// The largest `build_9p_reply` is `frame_reply`'s header (`NP_NET_LEN_PREFIX` +
/// an 8-byte status) plus a full `EXPORT_CHUNK` of file data.
const _: () = assert!(
    ninep_abi::NP_NET_LEN_PREFIX + 8 + EXPORT_CHUNK + ninep_abi::NP_SIG_LEN <= PREFIX_MAX,
    "PREFIX_MAX cannot hold a full EXPORT_CHUNK reply plus its signature"
);

/// The inline-reply cap fsd enforces on `readdir`/`read_file` (`FS_DATA_MAX`);
/// a larger `want` is rejected, so the export clamps to it for those verbs.
const DATA_INLINE: usize = syscall_abi::FS_DATA_MAX as usize;

/// Serve one framed NP request on a 9P-export (port 564) connection (cluster
/// Phase 1): decode the frame, run the verb against local `fsd`, and stage the
/// framed reply in `c.prefix` for `pump_send` to stream (then FIN - one
/// request/reply per connection, like the HTTP server's `Connection: close`).
/// **Read and write** (cluster Phase 2): reads (readdir/read/read_file/read_at)
/// plus the mutate verbs (touch/mkdir/rmdir/rm/mv/write/write_file/write_at),
/// each relayed to the local `fsd`. The request's `tree` field is ignored - the
/// export serves the local boot mount (tree 0). Single-writer, trusted-LAN (see
/// docs/roadmap-cluster-phase2.md).
fn handle_9p(c: &mut TcpConn, request: &[u8], dials: &mut [Option<DialConn>; MAX_DIAL], mac: &[u8; 6], auth: &Auth) {
    // Authenticate first: verify the client's Ed25519 signature over the request
    // and recover the bare NP message. A failure (an unauthorized key, a bad
    // signature, or an unconfigured export) is refused before any verb runs -
    // fail-closed. `authenticate` returns the NP message slice on success.
    let signed_request = request.len() >= ninep_abi::NP_NET_LEN_PREFIX + 8
        && read_u64(request, ninep_abi::NP_NET_LEN_PREFIX) == ninep_abi::NP_AUTH_MAGIC_SIGNED;
    // A machine with no identity cannot sign a reply, and a signed request must
    // not be answered unauthenticated. Refusing is the honest outcome: an export
    // that cannot prove who it is has nothing to serve with.
    if signed_request && !auth.accepts_signed() {
        // Refused WITHOUT LOGGING, like every other pre-authentication refusal.
        //
        // A log line here is reachable by anyone who can send eight bytes to
        // port 564: `log` is a blocking MSG_CALL to `cond`, which on the
        // framebuffer backend blits glyphs, so a host looping connect/send/close
        // would drive unbounded console output and blocking IPC inside netd's
        // frame-drain loop - an unusable screen, and plausibly a supervisor
        // wedge. The diagnostic is worth having and is worth having ONCE, so it
        // is emitted at boot instead (see load_auth), where the machine says its
        // own state without anyone else being able to ask.
        deny_9p(c, request);
        return;
    }
    let Some((msg, nonce, name)) = authenticate(auth, request) else {
        deny_9p(c, request);
        return;
    };
    // WHO is asking, mapped onto THIS machine's accounts - resolved BEFORE the
    // routing decision below, so that `cpu` (NP_RUN) cannot slip past it the way
    // it did when the check lived on the fs path only.
    //
    // REFUSE an unknown name. The alternative - serving it as netd, which is
    // root - is exactly the hole this closes, and a "map the stranger onto
    // nobody" fallback would just be a quieter way of guessing.
    //
    // The honest limit, stated where it is decided: a peer holding the cluster
    // key can claim ANY name. This defends against the *users* of a trusted
    // node, which is the real exposure - a node's own users are not all trusted
    // even when the node is. Defending against a compromised node needs
    // per-user keys, a further tier (docs/ROADMAP.md).
    let proxy = match map_user(&name) {
        Ok(p) => p,
        Err(cause) => {
            // NOT deny_9p: that reports a key failure, and the signature
            // verified fine - sending an operator after /etc/cluster/authorized
            // when the real cause is a missing account costs them the
            // afternoon. Naming the cause is safe HERE and only here, because
            // this peer has already proved it holds an authorized private key;
            // the pre-auth path stays deliberately ambiguous so an
            // unauthenticated stranger cannot enumerate accounts.
            deny_unknown_user(c, request, cause, auth, &nonce);
            return;
        }
    };
    // Route: a remote-run request (cluster Phase 4a) vs a normal fs verb. `msg`
    // is the NP message (verb at offset 0), framing + auth already stripped.
    let verb = if msg.len() >= 8 { read_u64(msg, 0) } else { 0 };
    if verb == ninep_abi::NP_RUN {
        handle_cpu_run(c, msg, auth, proxy);
        return;
    }
    let n = build_9p_reply(msg, &mut c.prefix, dials, mac, As::Remote(proxy));
    // Reply-auth (mutual authentication): sign the reply against the request
    // nonce, so the client can prove it came from the machine it dialled.
    let n = seal_reply(c, auth, &nonce, n);
    c.prefix_len = n;
    c.prefix_off = 0;
    c.file = false;
}

/// Log a 32-byte key as hex.
fn log_hex(key: &[u8; 32]) {
    let mut hex = [0u8; 64];
    clusterkeys::encode_key(key, &mut hex);
    log(&hex);
}

/// How many well-formed peers an authorized file lists.
fn count_peers(authorized: &[u8]) -> usize {
    let mut n = 0;
    for line in authorized.split(|&c| c == b'\n') {
        if clusterkeys::parse_line(line).is_some() {
            n += 1;
        }
    }
    n
}

/// Verify a framed export request and return the bare NP message on success
/// (see `ninep_abi`'s auth-frame constants).
///
/// The framed request is `[len:4][magic:8][nonce:16][name:32][pubkey:32]
/// [sig:64][NP message]`, signed over `domain-tag ‖ nonce ‖ name ‖ message`.
/// Returns `None` (refuse) when the frame is too short, the magic is not this
/// format, the offered public key is not one this machine authorizes, or the
/// signature does not verify. On success also returns a copy of the request
/// nonce, which the reply is signed against (reply-auth, mutual authentication
/// - see `seal_reply`), and the requesting user's name.
fn authenticate<'a>(
    auth: &Auth,
    request: &'a [u8],
) -> Option<(&'a [u8], [u8; ninep_abi::NP_NONCE_LEN], [u8; ninep_abi::NP_NAME_LEN])> {
    let p = ninep_abi::NP_NET_LEN_PREFIX; // 4
    if request.len() < p + 8 {
        return None;
    }
    // ONE FORMAT. The magic is still checked as a discriminator rather than
    // assumed, because a frame that is not this format must be REFUSED and not
    // misparsed - the header sizes differ, so reading a MAC'd frame as a signed
    // one would take the signature's first bytes for a public key.
    //
    // The MAC'd format (AUTHNP02) was accepted here until the flag day, guarded
    // by an explicit "do we hold the shared key" check. That check existed
    // because HMAC WITH A ZERO-LENGTH KEY IS A VALID HMAC: without it an
    // unconfigured export would have accepted requests computed under the empty
    // key, which every attacker also has. Deleting the format deletes the trap
    // with it - there is no longer a verification that succeeds by default.
    if read_u64(request, p) != ninep_abi::NP_AUTH_MAGIC_SIGNED {
        return None;
    }
    authenticate_signed(auth, request)
}

/// Where the NP message starts in a framed request, by format.
///
/// The two denial paths peek at the verb to choose a reply shape, and both used
/// the MAC'd header size unconditionally - so for a signed frame they read the
/// first eight bytes of the Ed25519 SIGNATURE as the verb. `is_run` was
/// therefore never true for a signed `cpu` request, and the caller got a binary
/// framed reply where the code exists to give it a readable line. One function
/// now, so the denial paths cannot drift from `authenticate` again.
fn np_offset(request: &[u8]) -> Option<usize> {
    let p = ninep_abi::NP_NET_LEN_PREFIX;
    if request.len() >= p + 8 && read_u64(request, p) == ninep_abi::NP_AUTH_MAGIC_SIGNED {
        Some(p + ninep_abi::NP_AUTH_HDR_SIGNED)
    } else {
        // NOT a guess. With the MAC'd format gone there is one header layout,
        // and a frame that is not it has no verb at any offset we know - so this
        // says so rather than returning the old MAC header size and reading
        // whatever bytes live there as a verb. The callers use it only to decide
        // whether a denial should read as text (a `cpu` run) or as a framed
        // reply, and "unknown" correctly means the framed one.
        None
    }
}

/// Verify a **signed** framed request: the per-machine-keypair format.
///
/// Three things must hold, in this order, and the order is the design:
///
/// 1. The frame is long enough to hold the header it claims.
/// 2. The offered public key is one this machine **authorizes**. Checked BEFORE
///    any signature verification - an unknown key is refused without spending a
///    scalar multiplication on it, and more importantly the question "is this
///    key allowed here" is separate from "does this signature check out". A
///    valid signature by a key nobody authorized is exactly as unwelcome as an
///    invalid one.
/// 3. The signature verifies over `SIG_DOMAIN_REQUEST ‖ nonce ‖ name ‖ message`.
///
/// Returns the bare NP message, the nonce (which the reply is signed against, so
/// the two are bound to one transaction), and the claimed user name.
fn authenticate_signed<'a>(
    auth: &Auth,
    request: &'a [u8],
) -> Option<(&'a [u8], [u8; ninep_abi::NP_NONCE_LEN], [u8; ninep_abi::NP_NAME_LEN])> {
    let p = ninep_abi::NP_NET_LEN_PREFIX;
    let need = p + ninep_abi::NP_AUTH_HDR_SIGNED;
    if request.len() < need {
        return None;
    }
    let noff = p + ninep_abi::NP_AUTH_NONCE_OFF;
    let woff = p + ninep_abi::NP_AUTH_NAME_OFF;
    let koff = p + ninep_abi::NP_AUTH_PUBKEY_OFF;
    let soff = p + ninep_abi::NP_AUTH_SIG_OFF;

    let mut offered = [0u8; ninep_abi::NP_PUBKEY_LEN];
    offered.copy_from_slice(&request[koff..koff + ninep_abi::NP_PUBKEY_LEN]);

    // Is this key allowed here at all? An unknown key never reaches the verifier.
    //
    // THE MATCHED PEER IS DELIBERATELY DISCARDED, and that is worth stating
    // because a lookup whose result is thrown away reads like a mistake. Two
    // reasons, both design:
    //
    //  - The peer's `name` is the MACHINE's name; the `name` in this frame is
    //    the requesting USER's. They are different namespaces, and the machine
    //    name is not consulted by anything downstream - `map_user` resolves the
    //    user through this machine's own /etc/passwd.
    //  - The peer's `ip` is not checked against this connection's source
    //    address either. Inbound authorizes BY KEY, outbound BY ADDRESS (see
    //    docs/roadmap-cluster-keys.md, decision 3): an exporter is offered a key
    //    and need only decide whether it is allowed, so pinning the address as
    //    well would refuse an authorized machine that dialled from a second
    //    interface without making any forgery harder - the key is the claim.
    clusterkeys::find_by_key(auth.authorized(), &offered)?;

    let np = &request[need..];
    // The signed bytes, after the domain tag: nonce ‖ name ‖ message. Built
    // into one buffer because
    // the signature is over a concatenation the frame does not store adjacently
    // - the prefix and the message are separated by the key and signature.
    // Domain-separated: the signed input is TAG ‖ nonce ‖ name, so a signature
    // made as a REPLY cannot be replayed as a request.
    let mut prefix = [0u8; ninep_abi::SIG_DOMAIN_MAX + ninep_abi::NP_SIG_PREFIX_LEN];
    let tag = ninep_abi::SIG_DOMAIN_REQUEST;
    prefix[..tag.len()].copy_from_slice(tag);
    prefix[tag.len()..tag.len() + ninep_abi::NP_SIG_PREFIX_LEN]
        .copy_from_slice(&request[noff..noff + ninep_abi::NP_SIG_PREFIX_LEN]);
    let prefix = &prefix[..tag.len() + ninep_abi::NP_SIG_PREFIX_LEN];

    let mut sig = [0u8; ninep_abi::NP_SIG_LEN];
    sig.copy_from_slice(&request[soff..soff + ninep_abi::NP_SIG_LEN]);

    // The crate verifies over `prefix ‖ message` without joining them, so netd
    // does not carry a second buffer the size of the largest message on the
    // frame of its most stack-constrained server.
    if !ed25519::verify_prefixed(&offered, prefix, np, &sig) {
        return None;
    }

    let mut nonce = [0u8; ninep_abi::NP_NONCE_LEN];
    nonce.copy_from_slice(&request[noff..noff + ninep_abi::NP_NONCE_LEN]);
    let mut who = [0u8; ninep_abi::NP_NAME_LEN];
    who.copy_from_slice(&request[woff..woff + ninep_abi::NP_NAME_LEN]);
    Some((np, nonce, who))
}

/// Seal a framed export reply — reply-auth, i.e. mutual authentication.
///
/// `c.prefix[..n]` holds the finished framed reply `[len:4][status:8][result…]`;
/// rewrite it to `[len':4][sig:64][status:8][result…]`, signed over
/// `request_nonce ‖ body`. Binding to the request's nonce proves the exporter
/// holds the private key AND ties this reply to THAT request, so a captured
/// reply cannot be replayed against another. Returns the new framed length.
///
/// (The `cpu`-run output *stream* is not framed and stays reply-unauthenticated
/// — the harder streaming case, still deferred.)
///
/// Took a `signed: bool` until the flag day, because the reply format followed
/// whichever format the request used. With one format there is nothing to
/// follow.
fn seal_reply(c: &mut TcpConn, auth: &Auth, nonce: &[u8], n: usize) -> usize {
    match auth.signing.as_ref() {
        Some(key) => seal_reply_signed(c, key, nonce, n),
        // Unreachable in practice: `accepts_signed` requires a signing key, so
        // a request that authenticated implies one exists. Left as a refusal
        // rather than an `expect` because an unsealed reply must never go out -
        // returning `n` here would send the BODY unauthenticated.
        None => refuse_reply(c),
    }
}

/// Stage a bare `FS_ERR_AUTH` in place of a reply this machine cannot sign.
///
/// A STATUS-ONLY FRAME, DELIBERATELY, and it is not the same thing as sending
/// nothing. Both arms of [`seal_reply`] used to return `0`, which leaves
/// `prefix_len` at zero, and `pump_send` then closes the connection with a bare
/// FIN carrying no payload. The client's `handle_rmount` rejects that on LENGTH
/// (`got < NP_NET_LEN_PREFIX + 8`) and reports `NO_FS`, so the shell prints "no
/// filesystem" - the message an unreachable peer gives - about a machine that is
/// up, listening, and simply unable to sign. `np9p_client.py` raises an
/// unhandled `RuntimeError` on the same reply.
///
/// Twelve bytes (`[len][status]`) instead: too short to carry a signature, which
/// is exactly what the client's next check tests, so it maps to `FS_ERR_AUTH`
/// and the operator is sent to the keys rather than to the network. This is the
/// same unsealed shape [`deny_9p`] already stages - a refusal carries no data
/// to authenticate, and an attacker who could forge it could equally drop the
/// connection.
fn refuse_reply(c: &mut TcpConn) -> usize {
    frame_reply(&mut c.prefix, syscall_abi::FS_ERR_AUTH, &[])
}

/// Sign a framed export reply with this machine's private key - the reply half
/// of the mutual authentication [`seal_reply`] dispatches to.
///
/// `[len][sig:64][status][result]`, signed over `request_nonce ‖ body`. Binding
/// to the request's nonce is what ties this reply to *that* request, so a
/// captured reply cannot be replayed against a different one - the same property
/// the MAC had, carried over unchanged.
fn seal_reply_signed(
    c: &mut TcpConn,
    key: &ed25519::SigningKey,
    nonce: &[u8],
    n: usize,
) -> usize {
    let sl = ninep_abi::NP_SIG_LEN; // 64
    let body_start = ninep_abi::NP_NET_LEN_PREFIX;
    if n < body_start || n + sl > c.prefix.len() {
        // NO ROOM: refuse, exactly as [`seal_reply`]'s keyless arm does.
        //
        // Returning `n` here - which it did until this was caught - stages the
        // body with no signature over it, and `handle_9p` then transmits it: an
        // authenticated request answered by an unauthenticated reply, which is
        // the one thing reply-signing exists to prevent. The caller sees
        // `FS_ERR_AUTH`, so the symptom is "authentication failed on large
        // reads" between two machines that are correctly keyed.
        //
        // Unreachable today, but only by a margin nothing asserts: `PREFIX_MAX`
        // is 2048 and the largest `build_9p_reply` is 12 + `EXPORT_CHUNK`
        // (1400) = 1412, leaving 572 bytes spare after the signature. Raising
        // `EXPORT_CHUNK` past 1972, shrinking `PREFIX_MAX`, or adding a verb
        // with a bigger reply would each reach it - so the margin is pinned by
        // a compile-time assertion beside `EXPORT_CHUNK` rather than left to
        // arithmetic nobody re-does.
        return refuse_reply(c);
    }
    let body_len = n - body_start;
    // Signed over nonce ‖ body via the two-part entry point, so the
    // concatenation is never built on this server's stack.
    let mut pfx = [0u8; ninep_abi::SIG_DOMAIN_MAX + ninep_abi::NP_NONCE_LEN];
    let tag = ninep_abi::SIG_DOMAIN_REPLY;
    pfx[..tag.len()].copy_from_slice(tag);
    pfx[tag.len()..tag.len() + nonce.len()].copy_from_slice(nonce);
    let sig = ed25519::sign_prefixed(key, &pfx[..tag.len() + nonce.len()], &c.prefix[body_start..n]);
    c.prefix.copy_within(body_start..n, body_start + sl);
    c.prefix[body_start..body_start + sl].copy_from_slice(&sig);
    let new_body = sl + body_len;
    c.prefix[0..4].copy_from_slice(&(new_body as u32).to_le_bytes());
    body_start + new_body
}

/// Stage an auth-denied response on the export connection.
///
/// For a remote-run (`cpu`) client - which reads the reply as a raw output
/// stream - a human-readable denial line prints on the caller's screen; for an
/// fs client (a remote mount) a framed `FS_ERR_AUTH` reply is staged, which the
/// shell surfaces as "authentication failed". The verb peek is only used to pick
/// the reply *format* (untrusted, but harmless either way).
///
/// ONE REFUSAL FOR EVERY PRE-AUTHENTICATION REASON, and the wording is the
/// security property, not a nicety. There used to be a second function,
/// `deny_no_identity`, for the case where THIS machine has no keypair or no
/// authorized peers; it staged the same `FS_ERR_AUTH` on the wire and differed
/// only in the text it printed to a `cpu` caller - "the remote machine cannot
/// serve signed requests (no identity of its own, or no authorized peers)".
///
/// That text answered a question the asker had not authenticated to ask. The
/// gate reaching it runs BEFORE `authenticate`, on nothing more than a magic
/// number, so anyone who could reach port 564 could send eight bytes and learn
/// whether a machine was configured - fingerprinting every node in a cluster
/// while holding no key at all. The caller's own gate says the rule it broke:
/// the diagnostic "is emitted at boot instead, where the machine says its own
/// state without anyone else being able to ask".
///
/// So the message names no machine's state. It points at the configuration both
/// ends share, which is where the answer is either way, and the node that is
/// actually misconfigured has already said so on its own console at boot.
fn deny_9p(c: &mut TcpConn, request: &[u8]) {
    let is_run = matches!(np_offset(request),
        Some(voff) if request.len() >= voff + 8 && read_u64(request, voff) == ninep_abi::NP_RUN);
    c.file = false;
    c.prefix_off = 0;
    c.cpu_child = CPU_NONE;
    if is_run {
        let m: &[u8] =
            b"cpu: authentication failed (check /etc/cluster/id and authorized on both machines)\r\n";
        let n = m.len().min(c.prefix.len());
        c.prefix[..n].copy_from_slice(&m[..n]);
        c.prefix_len = n;
    } else {
        c.prefix_len = frame_reply(&mut c.prefix, syscall_abi::FS_ERR_AUTH, &[]);
    }
}

/// Refuse a request whose MAC verified but whose *user* this machine has never
/// heard of. Distinct from [`deny_9p`] only in what it says: same `FS_ERR_AUTH`
/// on the wire (one refusal code, so a client needs no new case), an accurate
/// line for the `cpu` caller, who reads the reply as raw output.
fn deny_unknown_user(c: &mut TcpConn, request: &[u8], cause: LineScan, auth: &Auth, nonce: &[u8]) {
    let is_run = matches!(np_offset(request),
        Some(voff) if request.len() >= voff + 8 && read_u64(request, voff) == ninep_abi::NP_RUN);
    c.file = false;
    c.prefix_off = 0;
    c.cpu_child = CPU_NONE;
    // MATCHED, NOT COMPARED. This was `cause == LineScan::Unreadable`, so when
    // the scan gained a third answer the new one silently took the "no such
    // account" branch - telling a correctly-named caller it is a stranger
    // because THIS machine has no account database. A match makes a future
    // fourth state a compile error instead.
    if is_run {
        let m: &[u8] = match cause {
            LineScan::Unreadable => b"cpu: this host cannot read its account database right now\r\n",
            LineScan::NoDatabase => b"cpu: this host has no readable account database\r\n",
            _ => b"cpu: no account of that name on this host (the signature was accepted)\r\n",
        };
        let n = m.len().min(c.prefix.len());
        c.prefix[..n].copy_from_slice(&m[..n]);
        c.prefix_len = n;
    } else {
        // NO_FS only for the transient case: the shell surfaces it as "no
        // filesystem", which is true AND worth retrying. A missing database is
        // not worth retrying, and a real stranger is neither.
        let status = match cause {
            LineScan::Unreadable => syscall_abi::NO_FS,
            LineScan::NoDatabase => syscall_abi::FS_ERR_NOT_FOUND,
            _ => syscall_abi::FS_ERR_AUTH,
        };
        // SEALED, or the three statuses above are a distinction no client can
        // observe. An unsealed refusal is 12 bytes - `[len][status]` - and every
        // client checks the length before the status, precisely because an
        // unsigned reply must not be trusted; the Rust one returns a hard-coded
        // FS_ERR_AUTH and `np9p_client.py` documents doing the same. So a
        // transient failure on the exporter arrived as "authentication failed"
        // and sent the operator to the keys.
        //
        // Signing it is possible HERE and not on the pre-auth paths: this peer
        // has already proved it holds an authorized private key, which means
        // `accepts_signed()` was true, which means we hold one too. An
        // authenticated request gets an authenticated answer, refusal included.
        let n = frame_reply(&mut c.prefix, status, &[]);
        c.prefix_len = seal_reply(c, auth, nonce, n);
    }
}

/// Frame **and sign** an NP message for an outbound export request - the
/// client-side counterpart of [`authenticate`].
///
/// Writes the one format, `[len][magic][nonce][name][pubkey][sig][np]`, and
/// returns the total framed length (or `0` if this machine has no signing
/// identity, or `out` is too small) **and the nonce it used** - the client
/// verifies the reply's SIGNATURE against that same nonce (reply-auth). The
/// nonce is a fresh, non-repeating value: the `MONOTONIC_US` clock plus our
/// packed IP. It took an `auth` rather than a key because it once had two
/// formats to choose between; it keeps it for [`Auth::can_send`].
fn frame_signed(
    auth: &Auth,
    who: &[u8; ninep_abi::NP_NAME_LEN],
    np: &[u8],
    out: &mut [u8],
) -> (usize, [u8; ninep_abi::NP_NONCE_LEN]) {
    let zero_nonce = [0u8; ninep_abi::NP_NONCE_LEN];
    // Refuse only when this machine can authenticate an outbound request by
    // NEITHER route. It used to refuse whenever the shared key was absent, which
    // by step 8 meant a machine that could sign perfectly well was refused
    // because of a secret its signature does not use.
    //
    // The gate itself stays, and for the original reason: a request cannot be
    // taken back. A node that sends something the exporter ACCEPTS AND EXECUTES
    // - NP_WRITE, NP_MKDIR, NP_RM all land - and only then discovers it cannot
    // authenticate the exchange tells the user "authentication failed" about a
    // write that happened. Refusing before the send is the only honest answer.
    if !auth.can_send() {
        return (0, zero_nonce);
    }
    // ONE FORMAT: sign, or refuse. Until the flag day this fell back to a
    // shared-key MAC when the machine had no identity of its own, which is what
    // kept a keyless node working through the migration. There is nothing to
    // fall back to now, and a request this machine cannot authenticate must not
    // be sent at all - see the gate above.
    match auth.signing.as_ref() {
        Some(key) => frame_signed_ed25519(key, who, np, out),
        None => (0, zero_nonce),
    }
}

/// Frame and **sign** an outbound export request with this machine's private
/// key - the mirror of [`authenticate_signed`].
///
/// Writes `[len][magic][nonce][name][pubkey][sig][np]` and returns the framed
/// length and the nonce. The nonce is what binds the two halves of the exchange:
/// the exporter signs its reply over that same value, so a captured reply cannot
/// be replayed against a different request.
fn frame_signed_ed25519(
    key: &ed25519::SigningKey,
    who: &[u8; ninep_abi::NP_NAME_LEN],
    np: &[u8],
    out: &mut [u8],
) -> (usize, [u8; ninep_abi::NP_NONCE_LEN]) {
    let zero_nonce = [0u8; ninep_abi::NP_NONCE_LEN];
    let p = ninep_abi::NP_NET_LEN_PREFIX;
    let body = ninep_abi::NP_AUTH_HDR_SIGNED + np.len();
    let total = p + body;
    if total > out.len() || np.len() > ninep_abi::NP_NET_MAX {
        return (0, zero_nonce);
    }
    // Fresh nonce: [monotonic_us:8][our_ip_packed:8], as the MAC'd path uses.
    // Only freshness is required of it, not secrecy - and with a signature the
    // nonce is no longer what stops a forgery, only what ties a reply to its
    // request.
    let us = syscall(syscall_abi::MONOTONIC_US, 0);
    let ip = our_ip();
    let ipp = (ip[0] as u64) | ((ip[1] as u64) << 8) | ((ip[2] as u64) << 16) | ((ip[3] as u64) << 24);
    let mut nonce = [0u8; ninep_abi::NP_NONCE_LEN];
    nonce[0..8].copy_from_slice(&us.to_le_bytes());
    nonce[8..16].copy_from_slice(&ipp.to_le_bytes());

    out[0..4].copy_from_slice(&(body as u32).to_le_bytes());
    out[p..p + 8].copy_from_slice(&ninep_abi::NP_AUTH_MAGIC_SIGNED.to_le_bytes());
    let noff = p + ninep_abi::NP_AUTH_NONCE_OFF;
    out[noff..noff + ninep_abi::NP_NONCE_LEN].copy_from_slice(&nonce);
    let woff = p + ninep_abi::NP_AUTH_NAME_OFF;
    out[woff..woff + ninep_abi::NP_NAME_LEN].copy_from_slice(who);
    let koff = p + ninep_abi::NP_AUTH_PUBKEY_OFF;
    out[koff..koff + ninep_abi::NP_PUBKEY_LEN].copy_from_slice(&key.public());
    let npoff = p + ninep_abi::NP_AUTH_HDR_SIGNED;
    out[npoff..npoff + np.len()].copy_from_slice(np);

    // Signed over SIG_DOMAIN_REQUEST ‖ nonce ‖ name ‖ np - the same bytes the
    // retired MAC covered, after the tag - and via the two-part entry point, so
    // the concatenation is never built in memory.
    let mut prefix = [0u8; ninep_abi::SIG_DOMAIN_MAX + ninep_abi::NP_SIG_PREFIX_LEN];
    let tag = ninep_abi::SIG_DOMAIN_REQUEST;
    prefix[..tag.len()].copy_from_slice(tag);
    prefix[tag.len()..tag.len() + ninep_abi::NP_NONCE_LEN].copy_from_slice(&nonce);
    prefix[tag.len() + ninep_abi::NP_NONCE_LEN..tag.len() + ninep_abi::NP_SIG_PREFIX_LEN]
        .copy_from_slice(who);
    let sig = ed25519::sign_prefixed(key, &prefix[..tag.len() + ninep_abi::NP_SIG_PREFIX_LEN], np);
    let soff = p + ninep_abi::NP_AUTH_SIG_OFF;
    out[soff..soff + ninep_abi::NP_SIG_LEN].copy_from_slice(&sig);
    (total, nonce)
}

/// Serve a remote-run request (cluster Phase 4a - the Plan 9 `cpu` model): spawn
/// the named `/bin` command on *this* machine with its stdout piped back to us,
/// and set the connection to capture it. The command's output accumulates into
/// `c.prefix` as it runs (see the cpu-child routing in `drain_client_messages`);
/// its end-of-stream reaps it and releases the connection to stream the output
/// then FIN. On a spawn failure the connection streams an error line immediately.
fn handle_cpu_run(c: &mut TcpConn, msg: &[u8], auth: &Auth, who: Proxy) {
    c.file = false;
    c.prefix_off = 0;
    c.prefix_len = 0;
    c.cpu_child = CPU_NONE;
    // The no-exec lever (the export-hardening phase): a machine may authenticate
    // remote *mounts* while refusing remote code execution. Reject NP_RUN here -
    // after auth, so an unauthenticated peer can't even probe it - streaming a
    // denial line (the cpu client reads the reply as raw output).
    if auth.noexec {
        let m: &[u8] = b"cpu: remote execution disabled on this host (no-exec)\r\n";
        let n = m.len().min(c.prefix.len());
        c.prefix[..n].copy_from_slice(&m[..n]);
        c.prefix_len = n;
        return;
    }
    let msg_hdr = ninep_abi::NP_REQ_PAYLOAD as usize; // 48
    // a0 (command-line length) at msg offset 16; a1/a2 = the caller's endpoint
    // (ip, port) for the namespace import (cluster Phase 4b).
    let cmdlen = if msg.len() >= 24 { read_u64(msg, 16) as usize } else { 0 };
    let caller_ip_packed = if msg.len() >= 32 { read_u64(msg, 24) } else { 0 };
    let caller_port = if msg.len() >= 40 { read_u64(msg, 32) as u16 } else { 0 };
    let pstart = msg_hdr;
    let cmdline: &[u8] = if msg.len() > pstart {
        &msg[pstart..(pstart + cmdlen).min(msg.len())]
    } else {
        &[]
    };
    // Import the caller's namespace (cluster Phase 4b): bind /host -> a remote
    // mount back to the caller before SPAWN, so the spawned command reads the
    // caller's files at /host/... while running here. Set on *our* namespace; the
    // child inherits a copy at spawn (a later run overwrites it - netd never
    // resolves through its own namespace). A zero endpoint = a 4a-only client.
    if caller_ip_packed != 0 {
        let ip = [
            caller_ip_packed as u8,
            (caller_ip_packed >> 8) as u8,
            (caller_ip_packed >> 16) as u8,
            (caller_ip_packed >> 24) as u8,
        ];
        set_host_ns(ip, caller_port);
    }
    // Run the command AS the remote user, not as netd.
    //
    // The identity word carries the caller into `fsd` for a file verb, but a
    // spawned program is not our request - it makes its own, with its own task
    // identity, and a child inherits the SPAWNER's. Without this, `cpu A passwd
    // root` would reach A's account server with netd's root authority.
    //
    // So netd becomes the user for the length of the spawn, and the child
    // inherits that. Restoring is a `Drop`, not a line at the end: the spawn
    // path has several early returns, and "remember to change back" is the class
    // of invariant this whole rebuild exists to stop relying on. Safe against the
    // POSIX saved-uid rule (`SET_ID` back to root is allowed because root is
    // netd's saved id), while the CHILD's saved id is the user - so it can never
    // restore root. Nothing else is served inside the window: `cpu_spawn` is
    // synchronous and never re-enters the event loop.
    let Some(_as_user) = AsUser::enter(who) else {
        let m: &[u8] = b"cpu: cannot assume the calling user's identity\r\n";
        let n = m.len().min(c.prefix.len());
        c.prefix[..n].copy_from_slice(&m[..n]);
        c.prefix_len = n;
        return;
    };
    match cpu_spawn(cmdline) {
        Some(slot) => {
            // Capture: hold the response until the child's output completes.
            c.cpu_child = slot;
        }
        None => {
            // Nothing to capture - stream an error line and FIN (cpu_child stays
            // NONE, so pump_send sends c.prefix immediately).
            let msg: &[u8] = b"cpu: cannot run command (no such /bin program?)\r\n";
            let n = msg.len().min(c.prefix.len());
            c.prefix[..n].copy_from_slice(&msg[..n]);
            c.prefix_len = n;
            c.cpu_child = CPU_NONE;
        }
    }
}

/// Spawn a `/bin` command for a remote-run request (cluster Phase 4a): parse the
/// command line into argv, read the program from `/bin` on the local disk, and
/// `SPAWN` it with stdout piped to netd (`NET_TASK`) plus the delegated reply
/// capability so its output reaches us. Returns the child's scheduler slot, or
/// `None` on any failure (empty command, no such program, spawn error). Mirrors
/// the shell's `spawn_path`, in netd.
/// Set netd's namespace to a single binding `/host -> remote(ip:port)` (cluster
/// Phase 4b): a `NS_REMOTE_TREE` binding whose target is `[ip:4][port:2][/]`. A
/// child spawned right after inherits it, so its `/host/...` accesses become 9P
/// round trips back to the caller (through this netd), reading the *caller's*
/// files. netd only ever uses this namespace for that inheritance.
fn set_host_ns(ip: [u8; 4], port: u16) {
    // Binding: [tree][prefix_len=5][target_len=7]["/host"][ip:4][port:2]["/"].
    let mut ns = [0u8; 3 + 5 + 7];
    ns[0] = ninep_abi::NS_REMOTE_TREE;
    ns[1] = 5;
    ns[2] = (ninep_abi::NS_ENDPOINT_LEN + 1) as u8; // endpoint (6) + "/"
    ns[3..8].copy_from_slice(b"/host");
    ns[8..12].copy_from_slice(&ip);
    ns[12..14].copy_from_slice(&port.to_le_bytes());
    ns[14] = b'/';
    let _ = syscall4(syscall_abi::NS_SET, ns.as_ptr() as u64, ns.len() as u64, 0, 0);
}

/// netd running as a remote user for the length of one `cpu` spawn, so the
/// spawned child inherits that identity instead of netd's root.
///
/// A guard rather than a pair of calls: `Drop` restores root on every path out
/// of the spawn, including the early returns. If the restore itself ever failed
/// netd would be left as that user - which denies netd its own later reads
/// (fail-closed) rather than granting anything, so the failure direction is the
/// safe one; it is reported because a server quietly losing its authority is
/// worth seeing in the log.
struct AsUser;

impl AsUser {
    /// Become `who`. `None` if the kernel refuses, in which case the caller must
    /// NOT spawn - running the command as root is the thing being prevented.
    fn enter(who: Proxy) -> Option<Self> {
        // uid and gid are SEPARATE arguments (arg0/arg1), not the packed word
        // GET_ID/SENDER_ID return - the same shape `login` and `su` use. Passing
        // the packed word here set the uid correctly and silently left the gid
        // at 0, which a `cpu <host> id` reported as `uid=1000(user) gid=0(root)`.
        // Groups are cleared with the identity (the 0/0 group argument): none
        // travel over the cluster, and leaving netd's own behind would attach
        // them to a user they do not belong to.
        if syscall4(syscall_abi::SET_ID, who.uid as u64, who.gid as u64, 0, 0) != 0 {
            return None;
        }
        Some(AsUser)
    }
}

impl Drop for AsUser {
    fn drop(&mut self) {
        // Back to root. Permitted because root is netd's SAVED identity, which
        // is the POSIX saved-set-uid rule the kernel implements - and which the
        // child deliberately does not get (its saved id is the user's).
        if syscall4(syscall_abi::SET_ID, 0, 0, 0, 0) != 0 {
            log(b"netd: WARNING could not restore root identity after a cpu run\r\n");
        }
    }
}

fn cpu_spawn(cmdline: &[u8]) -> Option<u8> {
    const ARGV_CAP: usize = syscall_abi::ARGV_MAX as usize;
    let mut blob = [0u8; ARGV_CAP]; // [argc:u32][ (len:u32, bytes) ... ]
    let mut w = 4usize;
    let mut argc = 0u32;
    let mut path = [0u8; 5 + 96]; // "/bin/" + program name
    let mut plen = 0usize;

    // Tokenize on spaces (byte scanning, relocation-safe). argv[0] is the program.
    let mut i = 0usize;
    while i < cmdline.len() {
        while i < cmdline.len() && cmdline[i] == b' ' {
            i += 1;
        }
        if i >= cmdline.len() {
            break;
        }
        let start = i;
        while i < cmdline.len() && cmdline[i] != b' ' {
            i += 1;
        }
        let tok = &cmdline[start..i];
        if w + 4 + tok.len() > blob.len() {
            return None;
        }
        blob[w..w + 4].copy_from_slice(&(tok.len() as u32).to_le_bytes());
        w += 4;
        blob[w..w + tok.len()].copy_from_slice(tok);
        w += tok.len();
        if argc == 0 {
            const P: &[u8] = b"/bin/";
            if P.len() + tok.len() > path.len() {
                return None;
            }
            path[..P.len()].copy_from_slice(P);
            path[P.len()..P.len() + tok.len()].copy_from_slice(tok);
            plen = P.len() + tok.len();
        }
        argc += 1;
    }
    if argc == 0 {
        return None;
    }
    blob[0..4].copy_from_slice(&argc.to_le_bytes());

    // Read the ELF from /bin (disk, tree 0) and feed it to the kernel's spawn
    // staging buffer chunk by chunk, exactly as the shell's spawn_path does. The
    // chunk is 512 bytes - the kernel's per-syscall pointer cap (`MAX_USER_LEN`)
    // that both the read (into `chunk`) and the `SPAWN_STAGE` copy must fit.
    let mut chunk = [0u8; 512];
    let mut offset = 0u64;
    loop {
        // FirstParty = netd's own credential - which, for the length of a
        // `cpu` spawn, IS the remote user (see `AsUser`), so a user who may
        // not read the binary cannot run it.
        let n = read_file_chunk(As::FirstParty, &path[..plen], 0, offset, &mut chunk);
        if n >= syscall_abi::FS_ERR_MIN {
            return None; // no such program, or a read error
        }
        if n == 0 {
            break;
        }
        let n = (n as usize).min(chunk.len());
        if syscall4(syscall_abi::SPAWN_STAGE, offset, chunk.as_ptr() as u64, n as u64, 0) != 0 {
            return None; // staged past the kernel's buffer
        }
        offset += n as u64;
        if n < chunk.len() {
            break;
        }
    }
    if offset == 0 {
        return None; // empty file
    }
    // Stage argv, then spawn with stdout -> NET_TASK (us) and no cwd.
    if syscall4(syscall_abi::ARGS_STAGE, blob.as_ptr() as u64, w as u64, 0, 0) != 0 {
        return None;
    }
    let slot = syscall4(syscall_abi::SPAWN, offset, syscall_abi::NET_TASK, w as u64, 0);
    if slot >= syscall_abi::FS_ERR_MIN {
        return None;
    }
    // Delegate the reply capability so the child may pipe its output back to us.
    let _ = syscall4(syscall_abi::DELEGATE, slot, syscall_abi::NET_TASK, 0, 0);
    Some(slot as u8)
}

/// `netd`'s **export namespace** (cluster Phase 3 — the namespace-aware export):
/// the composed tree the export serves to remote clients. An exported request
/// path resolves through this with the *same* [`ninep_abi::resolve_ns`] a local
/// client uses on its own namespace — the Plan 9 model ("a server exports a
/// namespace"), replacing the per-server prefix special-cases the earlier steps
/// used. Unbound paths default to the boot disk (`fsd` tree 0); these three
/// bindings add the synthetic servers. A binding is
/// `[tree][prefix_len][target_len][prefix][target]`; every target here is `"/"`
/// (each server's own root). A fourth resource is now a fourth *binding*, not a
/// new code branch. A `const` blob lives in `.rodata` (no mutable statics).
const EXPORT_NS: &[u8] = &[
    ninep_abi::NS_PROC_TREE, 5, 1, b'/', b'p', b'r', b'o', b'c', b'/',
    ninep_abi::NS_CON_TREE, 9, 1, b'/', b'd', b'e', b'v', b'/', b'c', b'o', b'n', b's', b'/',
    ninep_abi::NS_NET_TREE, 4, 1, b'/', b'n', b'e', b't', b'/',
];

// --- dial-out (/net/tcp) plumbing ---

/// Max data bytes per dial segment (stop-and-wait), and the max a /data read
/// returns per NP round trip (fits the inline reply cap; the client loops).
const DIAL_MSS: usize = 1200;
const DIAL_READ_MAX: usize = 512;
const DIAL_SYN_TRIES: u8 = 6;
const DIAL_DATA_TRIES: u8 = 8;
const DIAL_RETX_WAIT: u64 = 12; // ticks per (re)try

/// Parse a decimal byte string into a `u64` (no sign, no overflow guard beyond
/// u64). `None` on empty or a non-digit.
fn parse_dec(b: &[u8]) -> Option<u64> {
    if b.is_empty() {
        return None;
    }
    let mut v = 0u64;
    for &d in b {
        if !d.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((d - b'0') as u64);
    }
    Some(v)
}

/// Parse a dotted-quad IPv4 from bytes (byte-scanning, PIE-safe - no str
/// range-indexing).
fn parse_ip4(b: &[u8]) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut n = 0usize;
    let mut start = 0usize;
    let mut i = 0usize;
    loop {
        let at_end = i == b.len();
        if at_end || b[i] == b'.' {
            let v = parse_dec(&b[start..i])?;
            if v > 255 || n >= 4 {
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

/// Parse a `connect <a.b.c.d>!<port>` ctl command into an endpoint.
fn parse_connect(data: &[u8]) -> Option<([u8; 4], u16)> {
    const PFX: &[u8] = b"connect ";
    if data.len() < PFX.len() || &data[..PFX.len()] != PFX {
        return None;
    }
    let mut rest = &data[PFX.len()..];
    // Trim trailing whitespace/newline.
    while !rest.is_empty() && matches!(rest[rest.len() - 1], b'\n' | b'\r' | b' ' | b'\t') {
        rest = &rest[..rest.len() - 1];
    }
    let bang = rest.iter().position(|&b| b == b'!')?;
    let ip = parse_ip4(&rest[..bang])?;
    let port = parse_dec(&rest[bang + 1..])?;
    if port > u16::MAX as u64 {
        return None;
    }
    Some((ip, port as u16))
}

/// Split a `/tcp/<N>/<leaf>` path into `(N, leaf)`. `sub` is the path *after*
/// `/tcp` (e.g. `/3/data`); returns `None` for `/clone` or a malformed path.
fn dial_path(sub: &[u8]) -> Option<(usize, &[u8])> {
    if sub.is_empty() || sub[0] != b'/' {
        return None;
    }
    let rest = &sub[1..]; // "<N>/<leaf>"
    let slash = rest.iter().position(|&b| b == b'/')?;
    let n = parse_dec(&rest[..slash])? as usize;
    Some((n, &rest[slash + 1..]))
}

/// The `/net/tcp` file model (Plan 9 connection files, handle-in-path). `sub` is
/// the path after `/tcp`. Pure state mutation - the event loop (`pump_dials` /
/// `dial_on_segment`) does the actual TCP. `data_in` is a write's payload.
fn dial_file_op(
    verb: u64,
    sub: &[u8],
    want: usize,
    data_in: &[u8],
    out: &mut [u8],
    dials: &mut [Option<DialConn>; MAX_DIAL],
    mac: &[u8; 6],
) -> (u64, usize) {
    let is_read = verb == ninep_abi::NP_READ
        || verb == ninep_abi::NP_READ_AT
        || verb == ninep_abi::NP_READ_FILE;
    let is_write = verb == ninep_abi::NP_WRITE || verb == ninep_abi::NP_WRITE_FILE;

    // /tcp (dir): readdir lists clone (per-conn dirs omitted - minimal).
    if sub.is_empty() || sub == b"/" {
        if verb == ninep_abi::NP_READDIR {
            let listing: &[u8] = b"clone\n";
            let m = listing.len().min(want).min(out.len());
            out[..m].copy_from_slice(&listing[..m]);
            return (m as u64, m);
        }
        return (syscall_abi::FS_ERR_NOT_A_FILE, 0);
    }

    // /tcp/clone: allocate a connection, return its number.
    if sub == b"/clone" {
        if !is_read {
            return (syscall_abi::FS_ERROR, 0);
        }
        let Some(i) = alloc_dial_slot(dials) else {
            return (syscall_abi::FS_ERR_DISK_FULL, 0); // no free connection slot
        };
        dials[i] = Some(new_dial(next_src_port()));
        let mut num = [0u8; 8];
        let nlen = u64_decimal(i as u64, &mut num);
        num[nlen] = b'\n';
        let total = nlen + 1;
        let m = total.min(want).min(out.len());
        out[..m].copy_from_slice(&num[..m]);
        return (total as u64, m);
    }

    // /tcp/<N>/<leaf>
    let Some((n, leaf)) = dial_path(sub) else {
        return (syscall_abi::FS_ERR_NOT_FOUND, 0);
    };
    if n >= dials.len() || dials[n].is_none() {
        return (syscall_abi::FS_ERR_NOT_FOUND, 0);
    }

    if leaf == b"ctl" && is_write {
        // "connect a.b.c.d!port" or "close".
        if data_in.starts_with(b"connect ") {
            let Some((ip, port)) = parse_connect(data_in) else {
                return (syscall_abi::FS_ERR_INVALID_NAME, 0);
            };
            let Some(dst_mac) = arp_resolve(mac, &next_hop(&ip)) else {
                if let Some(c) = dials[n].as_mut() {
                    c.state = DialState::Closed;
                }
                return (syscall_abi::NO_FS, 0); // unreachable
            };
            let c = dials[n].as_mut().unwrap();
            c.peer_ip = ip;
            c.peer_port = port;
            c.peer_mac = dst_mac;
            c.snd_nxt = c.isn;
            c.snd_una = c.isn;
            c.state = DialState::Connecting;
            c.retx_deadline = 0;
            c.retries = 0;
            c.last_activity = now();
            return (0, 0);
        }
        if data_in.starts_with(b"announce ") {
            // "announce <port>" (dial-in): make this slot a passive listener.
            let mut rest = &data_in[b"announce ".len()..];
            while !rest.is_empty() && matches!(rest[rest.len() - 1], b'\n' | b'\r' | b' ' | b'\t') {
                rest = &rest[..rest.len() - 1];
            }
            let Some(port) = parse_dec(rest).filter(|p| *p <= u16::MAX as u64) else {
                return (syscall_abi::FS_ERR_INVALID_NAME, 0);
            };
            let c = dials[n].as_mut().unwrap();
            c.announce_port = port as u16;
            c.state = DialState::Listening;
            c.last_activity = now();
            return (0, 0);
        }
        if data_in.starts_with(b"close") {
            let c = dials[n].as_mut().unwrap();
            if c.state == DialState::Established {
                c.state = DialState::Closing;
            } else {
                c.state = DialState::Closed;
            }
            c.last_activity = now();
            return (0, 0);
        }
        return (syscall_abi::FS_ERR_INVALID_NAME, 0);
    }

    // /tcp/<N>/listen (dial-in): hand out the next connection accepted on this
    // listener (its number), or nothing if none is pending yet. Non-blocking -
    // the client polls, like /data.
    if leaf == b"listen" && is_read {
        let found = dials
            .iter()
            .position(|c| matches!(c, Some(d) if d.parent == n as u8 && d.pending));
        let Some(m) = found else {
            return (0, 0); // nothing accepted yet
        };
        dials[m].as_mut().unwrap().pending = false;
        let mut num = [0u8; 8];
        let nlen = u64_decimal(m as u64, &mut num);
        num[nlen] = b'\n';
        let total = nlen + 1;
        let k = total.min(want).min(out.len());
        out[..k].copy_from_slice(&num[..k]);
        return (total as u64, k);
    }

    if leaf == b"status" && is_read {
        let c = dials[n].as_ref().unwrap();
        let s: &[u8] = match c.state {
            DialState::Free => b"Free\n",
            DialState::Idle => b"Idle\n",
            DialState::Connecting => b"Connecting\n",
            DialState::Listening => b"Listening\n",
            DialState::Accepting => b"Accepting\n",
            DialState::Established => b"Established\n",
            DialState::Closing => b"Closing\n",
            DialState::Closed => b"Closed\n",
        };
        let m = s.len().min(want).min(out.len());
        out[..m].copy_from_slice(&s[..m]);
        return (s.len() as u64, m);
    }

    if leaf == b"data" {
        let c = dials[n].as_mut().unwrap();
        c.last_activity = now();
        if is_write {
            // Append to the send buffer (bounded); return bytes accepted.
            let room = c.sbuf.len() - c.slen;
            let take = data_in.len().min(room);
            c.sbuf[c.slen..c.slen + take].copy_from_slice(&data_in[..take]);
            c.slen += take;
            return (take as u64, 0);
        }
        if is_read {
            // Drain the receive buffer (bounded to DIAL_READ_MAX). 0 = nothing
            // buffered right now (the client should poll /status to tell "not
            // yet" from EOF).
            let take = c.rlen.min(want).min(DIAL_READ_MAX).min(out.len());
            out[..take].copy_from_slice(&c.rbuf[..take]);
            // Compact the receive buffer.
            c.rbuf.copy_within(take..c.rlen, 0);
            c.rlen -= take;
            return (take as u64, take);
        }
    }

    (syscall_abi::FS_ERR_NOT_FOUND, 0)
}

/// Event-loop step: drive every active dial connection - (re)send its SYN while
/// Connecting, stream queued send data (stop-and-wait) and retransmit it, send
/// the FIN while Closing, and reap idle slots. Called each `serve` pass.
#[allow(clippy::needless_range_loop)] // index needed for the reap re-borrow
fn pump_dials(mac: &[u8; 6], dials: &mut [Option<DialConn>; MAX_DIAL]) {
    let mut frame = [0u8; 1600];
    let t = now();
    for i in 0..dials.len() {
        if dials[i].is_none() {
            continue;
        }
        let reap = {
            let c = dials[i].as_mut().unwrap();
            match c.state {
                DialState::Connecting => {
                    if c.retx_deadline == 0 || t >= c.retx_deadline {
                        if c.retries >= DIAL_SYN_TRIES {
                            c.state = DialState::Closed;
                        } else if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.isn, 0, TCP_SYN, true, &[], &mut frame) {
                            let _ = send(&frame[..nn]);
                            c.retries += 1;
                            c.retx_deadline = t + DIAL_RETX_WAIT;
                        }
                    }
                }
                // Data flows the same whether we're staying open (Established) or
                // draining (Closing) - queued send data must FLUSH before the FIN,
                // or a response written just before `close` would be stranded.
                DialState::Established | DialState::Closing => {
                    if c.inflight == 0 && c.slen > 0 {
                        let chunk = c.slen.min(DIAL_MSS);
                        if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_nxt, c.rcv_nxt, TCP_PSH | TCP_ACK, false, &c.sbuf[..chunk], &mut frame) {
                            let _ = send(&frame[..nn]);
                        }
                        c.inflight = chunk;
                        c.snd_nxt = c.snd_nxt.wrapping_add(chunk as u32);
                        c.retx_deadline = t + DIAL_RETX_WAIT;
                        c.retries = 0;
                    } else if c.inflight > 0 && t >= c.retx_deadline {
                        if c.retries >= DIAL_DATA_TRIES {
                            c.state = DialState::Closed;
                        } else if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_una, c.rcv_nxt, TCP_PSH | TCP_ACK, false, &c.sbuf[..c.inflight], &mut frame) {
                            let _ = send(&frame[..nn]);
                            c.retries += 1;
                            c.retx_deadline = t + DIAL_RETX_WAIT;
                        }
                    } else if c.state == DialState::Closing && !c.fin_sent && c.inflight == 0 && c.slen == 0 {
                        // All queued data sent + acked - now the FIN.
                        if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_nxt, c.rcv_nxt, TCP_FIN | TCP_ACK, false, &[], &mut frame) {
                            let _ = send(&frame[..nn]);
                        }
                        c.fin_sent = true;
                        c.snd_nxt = c.snd_nxt.wrapping_add(1);
                        c.retx_deadline = t + DIAL_RETX_WAIT;
                    } else if c.state == DialState::Closing && c.fin_sent && t >= c.retx_deadline {
                        c.state = DialState::Closed; // give up waiting for the FIN-ACK
                    }
                }
                // Passive open (dial-in): retransmit the SYN-ACK until the peer's
                // final ACK arrives (or give up).
                DialState::Accepting if t >= c.retx_deadline => {
                    if c.retries >= DIAL_SYN_TRIES {
                        c.state = DialState::Closed;
                    } else if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.isn, c.rcv_nxt, TCP_SYN | TCP_ACK, true, &[], &mut frame) {
                        let _ = send(&frame[..nn]);
                        c.retries += 1;
                        c.retx_deadline = t + DIAL_RETX_WAIT;
                    }
                }
                _ => {}
            }
            // Reap a slot only once it's Closed AND its receive buffer is drained
            // (so the client can still read the last bytes), or after a long idle -
            // but never idle-reap a Listening slot (a listener has no traffic of
            // its own yet must persist until an explicit close).
            (c.state == DialState::Closed && c.rlen == 0)
                || (c.state != DialState::Listening && t.saturating_sub(c.last_activity) > DIAL_IDLE_TICKS)
        };
        if reap {
            dials[i] = None;
        }
    }
}

/// Route an inbound TCP segment to a dial connection (matched by 4-tuple).
/// Returns true if it belonged to one (handshake completion, ACK, buffered
/// data + our ACK, or the peer's FIN). Called from `on_frame` before the
/// server-connection path.
#[allow(clippy::needless_range_loop)] // index needed for as_ref/as_mut re-borrow
fn dial_on_segment(mac: &[u8; 6], frame: &[u8], dials: &mut [Option<DialConn>; MAX_DIAL]) -> bool {
    let mut out = [0u8; 1600];
    let t = now();
    for i in 0..dials.len() {
        let (peer, pport, sport) = match dials[i].as_ref() {
            Some(c) if c.state != DialState::Free && c.state != DialState::Idle => (c.peer_ip, c.peer_port, c.src_port),
            _ => continue,
        };
        let Some(s) = parse_tcp(frame, &peer, pport, sport) else {
            continue;
        };
        let c = dials[i].as_mut().unwrap();
        c.last_activity = t;
        if s.flags & TCP_RST != 0 {
            c.state = DialState::Closed;
            return true;
        }
        // Passive open (dial-in): the peer's final ACK completes the handshake.
        // Do this before the data match so an ACK piggybacking the first request
        // bytes both establishes the conn and buffers the data in one pass.
        if c.state == DialState::Accepting && s.flags & TCP_ACK != 0 && s.ack == c.isn.wrapping_add(1) {
            c.snd_una = c.isn.wrapping_add(1);
            c.state = DialState::Established;
            c.retx_deadline = 0;
            c.retries = 0;
        }
        match c.state {
            DialState::Connecting => {
                if s.flags & (TCP_SYN | TCP_ACK) == (TCP_SYN | TCP_ACK) && s.ack == c.isn.wrapping_add(1) {
                    c.snd_nxt = c.isn.wrapping_add(1);
                    c.snd_una = c.snd_nxt;
                    c.rcv_nxt = s.seq.wrapping_add(1);
                    c.state = DialState::Established;
                    c.retx_deadline = 0;
                    c.retries = 0;
                    if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_nxt, c.rcv_nxt, TCP_ACK, false, &[], &mut out) {
                        let _ = send(&out[..nn]);
                    }
                }
            }
            DialState::Established | DialState::Closing => {
                // ACK processing: advance snd_una, drop acked send data.
                if seq_gt(s.ack, c.snd_una) {
                    let acked = s.ack.wrapping_sub(c.snd_una) as usize;
                    let data_acked = acked.min(c.inflight);
                    if data_acked > 0 {
                        c.sbuf.copy_within(data_acked..c.slen, 0);
                        c.slen -= data_acked;
                        c.inflight -= data_acked;
                    }
                    c.snd_una = s.ack;
                    c.retries = 0;
                    if c.inflight == 0 {
                        c.retx_deadline = 0;
                    }
                }
                // In-order data: buffer it (flow-controlled to rbuf) and ACK.
                if s.data_len > 0 && s.seq == c.rcv_nxt {
                    let room = c.rbuf.len() - c.rlen;
                    let take = s.data_len.min(room).min(frame.len().saturating_sub(s.data_off));
                    if take > 0 {
                        c.rbuf[c.rlen..c.rlen + take].copy_from_slice(&frame[s.data_off..s.data_off + take]);
                        c.rlen += take;
                        c.rcv_nxt = c.rcv_nxt.wrapping_add(take as u32);
                        if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_nxt, c.rcv_nxt, TCP_ACK, false, &[], &mut out) {
                            let _ = send(&out[..nn]);
                        }
                    }
                }
                // Peer FIN, in order (after any data in this segment): ACK it,
                // send our FIN if we haven't, and close (rbuf stays drainable).
                if s.flags & TCP_FIN != 0 && s.seq.wrapping_add(s.data_len as u32) == c.rcv_nxt {
                    c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                    c.peer_fin = true;
                    if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_nxt, c.rcv_nxt, TCP_ACK, false, &[], &mut out) {
                        let _ = send(&out[..nn]);
                    }
                    if !c.fin_sent {
                        c.fin_sent = true;
                        if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.snd_nxt, c.rcv_nxt, TCP_FIN | TCP_ACK, false, &[], &mut out) {
                            let _ = send(&out[..nn]);
                        }
                        c.snd_nxt = c.snd_nxt.wrapping_add(1);
                    }
                    c.state = DialState::Closed;
                }
            }
            _ => {}
        }
        return true;
    }
    false
}

/// Passive open (dial-in): if `frame` is a fresh inbound SYN to a port some slot
/// has `announce`d, accept it — allocate a new `DialConn` for the connection,
/// send the SYN-ACK, and mark it `pending` for the listener's `listen` read.
/// Returns true if the SYN was for an announced port (accepted, or dropped
/// because the table is full — either way it isn't a server-conn segment).
/// Called from `on_frame` after `dial_on_segment` (so a *retransmitted* SYN for
/// an already-accepted conn matches there first and never double-accepts).
fn dial_accept(mac: &[u8; 6], frame: &[u8], dials: &mut [Option<DialConn>; MAX_DIAL]) -> bool {
    // Minimal parse: IPv4 + TCP, a pure SYN (no ACK).
    if frame.len() < 34 || frame[12] != 0x08 || frame[13] != 0x00 || frame[23] != 6 {
        return false;
    }
    let ihl = (frame[14] & 0x0f) as usize * 4;
    let t = 14 + ihl;
    if frame.len() < t + 20 {
        return false;
    }
    let flags = frame[t + 13];
    if flags & TCP_SYN == 0 || flags & TCP_ACK != 0 {
        return false; // only a bare SYN opens a new accepted connection
    }
    let dst_port = u16be(frame, t + 2);
    // Is dst_port a port some slot announced?
    let Some(li) = dials
        .iter()
        .position(|c| matches!(c, Some(d) if d.state == DialState::Listening && d.announce_port == dst_port))
    else {
        return false;
    };
    let Some(m) = alloc_dial_slot(dials) else {
        return true; // table full - drop the SYN; the client will retransmit
    };
    let src_ip = [frame[26], frame[27], frame[28], frame[29]];
    let src_mac = [frame[6], frame[7], frame[8], frame[9], frame[10], frame[11]];
    let src_port = u16be(frame, t);
    let their_seq = u32be(frame, t + 4);

    // Build the accepted conn. Its local (source) port is the announced port -
    // replies go out FROM there, TO the client - and a fresh, rotating ISN keeps
    // back-to-back accepts on the same port distinct.
    let mut c = new_dial(dst_port);
    let isn = TCP_ISN ^ ((next_src_port() as u32) << 8);
    c.isn = isn;
    c.snd_una = isn;
    c.snd_nxt = isn;
    c.peer_ip = src_ip;
    c.peer_mac = src_mac;
    c.peer_port = src_port;
    c.rcv_nxt = their_seq.wrapping_add(1);
    c.state = DialState::Accepting;
    c.parent = li as u8;
    c.pending = true;
    c.last_activity = now();

    // Send the SYN-ACK (seq = our ISN, ack = their SYN + 1); it consumes one seq.
    let mut out = [0u8; 1600];
    if let Some(nn) = build_tcp(mac, &c.peer_mac, &c.peer_ip, c.src_port, c.peer_port, c.isn, c.rcv_nxt, TCP_SYN | TCP_ACK, true, &[], &mut out) {
        let _ = send(&out[..nn]);
    }
    c.snd_nxt = isn.wrapping_add(1);
    c.retx_deadline = now() + DIAL_RETX_WAIT;
    dials[m] = Some(c);
    true
}

/// The `/net` server, served by `netd` itself: the read-only identity files
/// (`/ip` dotted-quad, `/mac` colon-hex) AND the read-write `/net/tcp` dial-out
/// connection files (see `dial_file_op`). `data_in` is a write's payload; the
/// dial table + our MAC let the tcp subtree drive real connections. Shared by
/// the export gateway (remote) and the local client path (`handle_client`).
#[allow(clippy::too_many_arguments)]
fn net_op(verb: u64, fspath: &[u8], offset: u64, want: usize, data_in: &[u8], out: &mut [u8], dials: &mut [Option<DialConn>; MAX_DIAL], mac: &[u8; 6]) -> (u64, usize) {
    // The dial-out connection files (Plan 9 /net/tcp), a read-WRITE subtree.
    if fspath.starts_with(b"/tcp") {
        return dial_file_op(verb, &fspath[4..], want, data_in, out, dials, mac);
    }
    // The file contents (tiny), rebuilt each call from the live NIC state.
    let mut content = [0u8; 24];
    let clen = net_content(fspath, &mut content);

    if verb == ninep_abi::NP_READDIR {
        if fspath == b"/" {
            let listing: &[u8] = b"ip\nmac\n";
            let n = listing.len().min(want).min(out.len());
            out[..n].copy_from_slice(&listing[..n]);
            return (n as u64, n);
        }
        return (syscall_abi::FS_ERR_NOT_A_DIRECTORY, 0);
    }
    // Stat: `/` is the directory, `/ip`/`/mac` are files sized by their content.
    // No timestamp (time_valid stays 0). Lets `ls -l /net` show real sizes.
    if verb == ninep_abi::NP_STAT {
        let (size, is_dir) = if fspath == b"/" {
            (0u64, true)
        } else {
            match clen {
                Some(c) => (c as u64, false),
                None => return (syscall_abi::FS_ERR_NOT_FOUND, 0),
            }
        };
        let mut info = [0u8; ninep_abi::STAT_INFO_LEN];
        info[..8].copy_from_slice(&size.to_le_bytes());
        let flags: u32 = if is_dir { ninep_abi::STAT_FLAG_DIR } else { 0 };
        info[ninep_abi::STAT_FLAGS_OFF..ninep_abi::STAT_FLAGS_OFF + 4]
            .copy_from_slice(&flags.to_le_bytes());
        let n = info.len().min(out.len());
        out[..n].copy_from_slice(&info[..n]);
        return (ninep_abi::STAT_INFO_LEN as u64, n);
    }
    let Some(clen) = clen else {
        return (syscall_abi::FS_ERR_NOT_FOUND, 0);
    };
    match verb {
        v if v == ninep_abi::NP_READ_FILE => {
            // Status = the file's real size; data = its first min(size, want) bytes.
            let n = clen.min(want).min(out.len());
            out[..n].copy_from_slice(&content[..n]);
            (clen as u64, n)
        }
        v if v == ninep_abi::NP_READ || v == ninep_abi::NP_READ_AT => {
            let off = offset as usize;
            if off >= clen {
                return (0, 0); // at/past EOF
            }
            let n = (clen - off).min(want).min(out.len());
            out[..n].copy_from_slice(&content[off..off + n]);
            (n as u64, n)
        }
        // /net serves only the read verbs above. Anything else - a write, or
        // one of the fid verbs - has no arm here, and saying so beats the
        // generic error a client renders as "no such file or directory".
        other => {
            log_no_arm(b"/net", other);
            (syscall_abi::FS_ERR_NO_SUCH_VERB, 0)
        }
    }
}

/// The content of a `/net` file into `buf`: `/ip` -> dotted IPv4, `/mac` ->
/// colon-hex MAC. `None` if `fspath` isn't a `/net` file.
fn net_content(fspath: &[u8], buf: &mut [u8]) -> Option<usize> {
    if fspath == b"/ip" {
        let ip = our_ip();
        let mut n = 0;
        for (i, octet) in ip.iter().enumerate() {
            if i > 0 {
                buf[n] = b'.';
                n += 1;
            }
            n += u64_decimal(*octet as u64, &mut buf[n..]);
        }
        buf[n] = b'\n';
        Some(n + 1)
    } else if fspath == b"/mac" {
        let mac = unpack_mac(syscall(syscall_abi::NET_MAC, 0));
        let mut n = 0;
        for (i, b) in mac.iter().enumerate() {
            if i > 0 {
                buf[n] = b':';
                n += 1;
            }
            buf[n] = hex_digit(b >> 4);
            buf[n + 1] = hex_digit(b & 0xf);
            n += 2;
        }
        buf[n] = b'\n';
        Some(n + 1)
    } else {
        None
    }
}

fn hex_digit(v: u8) -> u8 {
    if v < 10 {
        b'0' + v
    } else {
        b'a' + (v - 10)
    }
}

/// The `len` bytes of `payload` starting at `off`, clamped to what is actually
/// there — the one way this server slices a wire-supplied range.
///
/// Both numbers come straight off the network. `off` is a `usize` and `len` a
/// `u64`, so the obvious spellings are both wrong in release builds, in
/// different ways:
///
/// - `&payload[off..off + n]` does not clamp the range **START**. A frame with
///   `a0 = 0xFFFF` against a 100-byte payload panics on "range start index out
///   of range" — and a panic here kills `netd`, tearing down every live TCP
///   connection, dial slot and export session; repeat it and the per-boot
///   restart cap is gone, taking the network stack down for the boot.
/// - `(off + len as usize).min(payload.len())` **wraps** rather than
///   saturating, so a large `off` can produce an `end` *below* `start`, which
///   panics on a slice whose start clamp looked sufficient.
///
/// Computing the start first and the length from what remains makes
/// `start + n <= payload.len()` true by construction, with no arithmetic that
/// can overflow. Reaching this at all needs a valid MAC, so it is a
/// trusted-peer fault — but a truncated or mis-built frame gets here by
/// accident, which is the likelier way to meet it.
fn wire_slice(payload: &[u8], off: usize, len: u64) -> &[u8] {
    let start = off.min(payload.len());
    let n = (len as usize).min(payload.len() - start);
    &payload[start..start + n]
}

/// Decode a framed NP request and build its framed reply into `out`. See
/// `ninep_abi`'s frame doc: request = `[u32 len][verb][tree][a0..a3][payload]`.
fn build_9p_reply(msg: &[u8], out: &mut [u8; PREFIX_MAX], dials: &mut [Option<DialConn>; MAX_DIAL], mac: &[u8; 6], who: As) -> usize {
    let hdr = ninep_abi::NP_REQ_PAYLOAD as usize; // 48
    // `msg` is the bare NP message (verb at offset 0); framing + auth were
    // stripped by handle_9p/authenticate. Reject a runt.
    if msg.len() < hdr {
        return frame_reply(out, syscall_abi::FS_ERROR, &[]);
    }
    let verb = read_u64(msg, 0);
    let p0 = read_u64(msg, 16) as usize;
    let p1 = read_u64(msg, 24);
    let p2 = read_u64(msg, 32);
    let payload = &msg[hdr..];
    let path = &payload[..p0.min(payload.len())];
    // Resolve the incoming path through netd's export namespace with the shared
    // resolver (cluster Phase 3 - the namespace-aware export): one code path, no
    // per-server prefix special-cases. Unbound -> the boot disk (fsd tree 0); the
    // EXPORT_NS bindings add /proc, /dev/cons, and /net. This is exactly how a
    // local client resolves through *its* namespace.
    let mut pbuf = [0u8; 256];
    let resolved = ninep_abi::resolve_ns(EXPORT_NS, path, &mut pbuf);
    let fspath = &pbuf[..resolved.len];
    let tree: u64 = match resolved.target {
        // The console (/dev/cons) is a different server (CON_TASK): a write verb
        // emits the inline bytes to the console, reads are refused (write-only).
        //
        // `who` is deliberately not consulted here, and the boundary is worth
        // stating rather than leaving as an omission: the console has no
        // permission model on this machine either, so a remote user gets exactly
        // what a local one has - the ability to print. Nothing here reaches the
        // filesystem, which is where identity decides anything.
        ninep_abi::NsTarget::Console => {
            let status = match verb {
                v if v == ninep_abi::NP_WRITE || v == ninep_abi::NP_WRITE_FILE => {
                    log(wire_slice(payload, p0, p1));
                    0
                }
                v if v == ninep_abi::NP_WRITE_AT => {
                    log(wire_slice(payload, p0, p2));
                    0
                }
                // Read verbs land here too, and "the console has no arm for
                // reading" is exactly what this code says - so it is the right
                // answer for both, not a stretch to cover the write-only case.
                other => {
                    log_no_arm(b"/dev/cons", other);
                    syscall_abi::FS_ERR_NO_SUCH_VERB
                }
            };
            return frame_reply(out, status, &[]);
        }
        // /net: this machine's network identity (read-only) AND /net/tcp dial-out
        // (the connection files - read-write). Served by netd itself.
        //
        // Same boundary as the console arm above: `who` is not consulted because
        // dialling is not permission-modelled for local users either. A remote
        // user can therefore open a connection out of this machine's NIC exactly
        // as a local one can. If /net ever grows a per-user policy, it belongs
        // here - and the identity is already in hand for it.
        ninep_abi::NsTarget::NetLocal => {
            let mut ndata = [0u8; 544];
            // readdir/read_file take the result window in a1; read/read_at in a2.
            let want = if verb == ninep_abi::NP_READ || verb == ninep_abi::NP_READ_AT {
                p2 as usize
            } else {
                p1 as usize
            };
            // A write's payload follows the path (p1 = data length for the inline
            // write verbs) - the /net/tcp ctl/data writes.
            let data_in: &[u8] = if verb == ninep_abi::NP_WRITE || verb == ninep_abi::NP_WRITE_FILE {
                wire_slice(payload, p0, p1)
            } else {
                &[]
            };
            let (status, dlen) = net_op(verb, fspath, p1, want.min(ndata.len()), data_in, &mut ndata, dials, mac);
            return frame_reply(out, status, &ndata[..dlen]);
        }
        // A remote binding in the export namespace would be transitive mounting -
        // not bound today, so this can't occur; refuse defensively.
        ninep_abi::NsTarget::Remote(_) => {
            return frame_reply(out, syscall_abi::FS_ERR_NO_SUCH_VERB, &[]);
        }
        // A local fsd tree (the boot disk, /proc, or a mounted partition).
        ninep_abi::NsTarget::Fsd(t) => t as u64,
    };
    let mut buf = [0u8; EXPORT_CHUNK];

    // Data slice for a status that is a byte count (0..FS_ERR_MIN); empty on an
    // error status.
    fn ok_data(status: u64, buf: &[u8]) -> &[u8] {
        if status < syscall_abi::FS_ERR_MIN {
            &buf[..(status as usize).min(buf.len())]
        } else {
            &[]
        }
    }

    match verb {
        v if v == ninep_abi::NP_READDIR => {
            // The inline verbs (readdir / read_file) are capped at fsd's inline
            // reply size (FS_DATA_MAX 512); fsd rejects a larger `want`.
            let want = (p1 as usize).min(DATA_INLINE);
            let status = list_dir(who, fspath, tree, &mut buf[..want]);
            frame_reply(out, status, ok_data(status, &buf))
        }
        v if v == ninep_abi::NP_READ || v == ninep_abi::NP_READ_AT => {
            let want = (p2 as usize).min(buf.len());
            let status = read_file_chunk(who, fspath, tree, p1, &mut buf[..want]);
            frame_reply(out, status, ok_data(status, &buf))
        }
        v if v == ninep_abi::NP_READ_FILE => {
            // Status = the file's real size; data = its first min(size, want) bytes.
            let want = (p1 as usize).min(DATA_INLINE);
            let size = stat_size(who, fspath, tree);
            if size >= syscall_abi::FS_ERR_MIN {
                return frame_reply(out, size, &[]);
            }
            let n = read_file_chunk(who, fspath, tree, 0, &mut buf[..want]);
            frame_reply(out, size, ok_data(n, &buf))
        }
        // Stat: relay to fsd; status = STAT_INFO_LEN, data = the fixed record
        // (size/dir-flag/mtime, plus mode/owner when the remote fs models it).
        // Lets `ls -l` work across a remote mount, not just on the local disk.
        v if v == ninep_abi::NP_STAT => {
            let status = fsd_call(
                who,
                ninep_abi::NP_STAT,
                tree,
                fspath.len() as u64,
                0,
                fspath,
                &mut buf[..ninep_abi::STAT_INFO_LEN],
            );
            frame_reply(out, status, ok_data(status, &buf))
        }
        // --- write / mutate verbs (cluster Phase 2: read+write) ---
        // Path-only ops: the path is the whole payload, no data. (A write under
        // /proc routes to the read-only proc tree and is refused there.)
        v if v == ninep_abi::NP_TOUCH
            || v == ninep_abi::NP_MKDIR
            || v == ninep_abi::NP_RMDIR
            || v == ninep_abi::NP_RM =>
        {
            let status = fsd_call(who, v, tree, fspath.len() as u64, 0, fspath, &mut []);
            frame_reply(out, status, &[])
        }
        // chmod: a1 = mode. chown: a1 = uid, a2 = gid (u64::MAX = unchanged).
        // Path-only scalars, so relay straight through (fsd degrades to
        // FS_ERR_NOT_SUPPORTED on a non-ext2 tree, same as locally).
        v if v == ninep_abi::NP_CHMOD => {
            let status = fsd_call(who, v, tree, fspath.len() as u64, p1, fspath, &mut []);
            frame_reply(out, status, &[])
        }
        v if v == ninep_abi::NP_CHOWN => {
            let status = fsd_call3(who, v, tree, fspath.len() as u64, p1, p2, fspath, &mut []);
            frame_reply(out, status, &[])
        }
        // Rename: payload is src (p0 bytes) then dst (p1 bytes), inline; fsd's
        // NP_MV takes exactly that. Only the disk (tree 0) mounts are writable, so
        // the payload (with its original, unstripped paths) is passed as-is.
        v if v == ninep_abi::NP_MV => {
            let both = p0.saturating_add(p1 as usize).min(payload.len());
            let status = fsd_call(who, ninep_abi::NP_MV, tree, p0 as u64, p1, &payload[..both], &mut []);
            frame_reply(out, status, &[])
        }
        // Full create/overwrite: wire payload is path (p0) then data (p1),
        // inline. Relay as fsd's inline NP_WRITE_FILE - same create/overwrite
        // semantics for a <=512 chunk, no grant needed (0 data = truncate).
        v if v == ninep_abi::NP_WRITE || v == ninep_abi::NP_WRITE_FILE => {
            let both = p0.saturating_add(p1 as usize).min(payload.len());
            let status = fsd_call(who, ninep_abi::NP_WRITE_FILE, tree, p0 as u64, p1, &payload[..both], &mut []);
            frame_reply(out, status, &[])
        }
        // Offset write: wire payload is path (p0) then data (p2 bytes) at offset
        // p1. fsd's NP_WRITE_AT is grant-only, so bridge wire-inline -> a local
        // GRANT_READ buffer (the mirror of read_file_chunk's GRANT_WRITE).
        v if v == ninep_abi::NP_WRITE_AT => {
            let data = wire_slice(payload, p0, p2);
            let status = fsd_write_at(who, fspath, tree, p1, data);
            frame_reply(out, status, &[])
        }
        // A verb this export has no arm for - today the five fid verbs
        // (NP_OPEN/NP_PREAD/NP_PWRITE/NP_FSTAT/NP_CLUNK), which fsd implements
        // and no export does.
        //
        // NOT `FS_ERROR`, which is what this answered until 2026-09-05. Clients
        // render `FS_ERROR` as "no such file or directory" - a message about a
        // path, for a request whose path was fine - and that has already cost
        // one debugging session (a missing NP_STAT was recorded in the roadmap
        // for days as a guest-side path-resolution bug). The code says THAT a
        // verb is missing; only this log says WHICH, so it is not optional.
        v => {
            log_no_arm(b"export", v);
            frame_reply(out, syscall_abi::FS_ERR_NO_SUCH_VERB, &[])
        }
    }
}

/// Relay an offset-write to fsd, bridging the wire's inline data to fsd's
/// grant-based `NP_WRITE_AT` (cluster Phase 2): copy `data` into a local buffer,
/// `GRANT_READ` it to fsd, then issue `NP_WRITE_AT(pathlen, offset, datalen)` -
/// the exact mirror of `read_file_chunk`'s `GRANT_WRITE` read bridge, the other
/// way. `data.len()` is bounded by `NP_REMOTE_CHUNK` (the client's inline cap).
/// Returns fsd's status (0, or an `FS_ERR_*`/`TASK_ERR_*` code).
fn fsd_write_at(who: As, path: &[u8], tree: u64, offset: u64, data: &[u8]) -> u64 {
    if data.is_empty() {
        return 0;
    }
    let mut dbuf = [0u8; ninep_abi::NP_REMOTE_CHUNK];
    let dlen = data.len().min(dbuf.len());
    dbuf[..dlen].copy_from_slice(&data[..dlen]);
    let granted = syscall4(
        syscall_abi::GRANT,
        syscall_abi::FSD_TASK,
        dbuf.as_ptr() as u64,
        dlen as u64,
        syscall_abi::GRANT_READ,
    );
    if granted != 0 {
        return syscall_abi::FS_ERROR;
    }
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&ninep_abi::NP_WRITE_AT.to_le_bytes());
    req[8..16].copy_from_slice(&tree.to_le_bytes()); // 0 = disk; NS_PROC_TREE = /proc
    req[16..24].copy_from_slice(&(path.len() as u64).to_le_bytes()); // p0: path len
    req[24..32].copy_from_slice(&offset.to_le_bytes()); // p1: offset
    req[32..40].copy_from_slice(&(dlen as u64).to_le_bytes()); // p2: data len
    req[40..48].copy_from_slice(&who.word().to_le_bytes()); // a3: WHO this is for
    let end = HDR + path.len();
    if end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[HDR..end].copy_from_slice(path);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::FSD_TASK,
        req.as_ptr() as u64,
        end as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed >= syscall_abi::FS_ERR_MIN {
        return packed;
    }
    if (packed & 0xffff_ffff) < 8 {
        return syscall_abi::FS_ERROR;
    }
    read_u64(&reply, 0)
}

/// Frame a reply into `out`: `[u32 len (LE)][status:u64][data]`, `len` = the
/// bytes after the prefix (8 + data). Returns the total byte count written.
fn frame_reply(out: &mut [u8], status: u64, data: &[u8]) -> usize {
    let cap = out.len().saturating_sub(ninep_abi::NP_NET_LEN_PREFIX + 8);
    let dlen = data.len().min(cap);
    let body = 8 + dlen;
    out[0..4].copy_from_slice(&(body as u32).to_le_bytes());
    out[4..12].copy_from_slice(&status.to_le_bytes());
    out[12..12 + dlen].copy_from_slice(&data[..dlen]);
    12 + dlen
}

fn start_response(c: &mut TcpConn, request: &[u8]) {
    let head = is_head(request);

    c.prefix_off = 0;
    c.file = false;
    c.read_off = 0;
    c.eof = false;
    c.fin_sent = false;

    // Only GET and HEAD are supported; any other method -> 405 (with the
    // Allow header those two require). Checked before touching the path, so an
    // unsupported method never reaches fsd. 405 has a body, so - unlike the
    // other early responses - the HEAD-trim below doesn't apply (an
    // unsupported method is by definition not HEAD).
    if !head && !request.starts_with(b"GET ") {
        set_prefix(c, RESP_405);
        return;
    }

    let mut pathbuf = [0u8; PATH_MAX];
    let path = parse_path(request, &mut pathbuf);
    let path: &[u8] = if path.is_empty() { b"/" } else { path };

    // A file? (stat succeeds and returns a size.)
    // ANONYMOUS, not netd. An HTTP request carries no identity, and the
    // authority it gets must reflect that: `nobody` sees the `other` triad, so a
    // root-owned 0600 file is refused here rather than served by fsd's root
    // bypass. (Unchanged on FAT32/exFAT, which model no mode at all.)
    let size = stat_size(As::Anonymous, path, 0);
    if size < syscall_abi::FS_ERR_MIN {
        // 200 header (Content-Type by extension, Content-Length = size), then
        // the body streamed from offset 0.
        c.prefix_len = build_200_header(&mut c.prefix, content_type(path), size);
        c.file = true;
        c.path_len = path.len().min(PATH_MAX);
        c.path[..c.path_len].copy_from_slice(&path[..c.path_len]);
    } else if size == syscall_abi::TASK_ERR_NO_SUCH_TASK || size == syscall_abi::NO_FS {
        // No filesystem / no server -> 503 (don't bother trying a listing).
        set_prefix(c, RESP_503);
    } else {
        // A directory? (list succeeds.) Build a browsable HTML index.
        let mut names = [0u8; syscall_abi::FS_DATA_MAX as usize];
        let listed = list_dir(As::Anonymous, path, 0, &mut names);
        if listed < syscall_abi::FS_ERR_MIN {
            c.prefix_len = build_listing(path, &names[..listed as usize], &mut c.prefix);
        } else if size == syscall_abi::FS_ERR_PERM || listed == syscall_abi::FS_ERR_PERM {
            // It exists; anonymous just may not have it. 403, not 404 - saying
            // "not found" about a file that is there is the kind of small lie
            // that costs an operator an hour.
            set_prefix(c, RESP_403);
        } else {
            // Neither a file nor a directory.
            set_prefix(c, RESP_404);
        }
    }

    // HEAD: identical headers to the GET, no body. Trim the prefix to just the
    // header block (through the first CRLFCRLF - true for every response here,
    // file 200 / listing / 404 / 503) and stream no file body. Correct for all
    // response kinds, since a listing/error keeps its body in the prefix.
    if head {
        c.prefix_len = header_end(&c.prefix[..c.prefix_len]);
        c.file = false;
    }
}

/// Whether the request is an HTTP `HEAD` (headers only, no body). Methods are
/// uppercase; a non-GET/HEAD method gets a 405 (see `start_response`).
fn is_head(request: &[u8]) -> bool {
    request.starts_with(b"HEAD ")
}

/// Byte offset just past the end of the HTTP header block (the first
/// `\r\n\r\n`), or the whole length if none is found (shouldn't happen - every
/// response this server builds ends its headers that way).
fn header_end(buf: &[u8]) -> usize {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i] == b'\r' && buf[i + 1] == b'\n' && buf[i + 2] == b'\r' && buf[i + 3] == b'\n' {
            return i + 4;
        }
        i += 1;
    }
    buf.len()
}

/// Copy a complete fixed response (404/503) into the connection's prefix
/// buffer.
fn set_prefix(c: &mut TcpConn, bytes: &[u8]) {
    let n = bytes.len().min(PREFIX_MAX);
    c.prefix[..n].copy_from_slice(&bytes[..n]);
    c.prefix_len = n;
}

// --- naming the caller ------------------------------------------------------
//
// Both directions of the cluster's per-user identity bottom out in the account
// database, and both go through the shared `accounts` crate so a cluster caller
// is resolved by exactly the rules `login`, `su` and `id` use:
//
// - **Outbound** (this machine is asking): `caller_name` turns the local
//   sender's uid into the name that goes in the auth header.
// - **Inbound** (this machine is serving): `map_user` turns a name from the
//   header into ids in *our* `/etc/passwd`, or refuses.

/// Feed every line of `path` to `f`, stopping early when it returns `true`
/// (which this then returns).
///
/// STREAMED, not read whole. A whole-file read reports `0` on overflow, so a
/// database larger than the buffer would silently read as empty - which for
/// `/etc/shadow` once meant every account, root included, locked out. The same
/// trap is one buffer away here. Streaming also keeps netd's stack small: this
/// is the task whose 32 KB has hit the guard page five times, so a 512-byte
/// chunk plus a 128-byte line beats a 2 KB whole-file buffer - in a frame that
/// finishes before the request and response buffers go live.
///
/// A line too long for the buffer is skipped rather than truncated: a prefix of
/// an account line could match the wrong account.
#[inline(never)]
fn for_each_line(path: &[u8], mut f: impl FnMut(&[u8]) -> bool) -> LineScan {
    const CHUNK: usize = 512;
    let mut chunk = [0u8; CHUNK];
    let mut line = [0u8; 128];
    let mut ll = 0usize;
    let mut overlong = false;
    let mut off = 0u64;
    loop {
        // FirstParty: reading the account database is netd's own business, not
        // the caller's - the caller is precisely what it is being read to learn.
        let r = read_file_chunk(As::FirstParty, path, 0, off, &mut chunk);
        if r >= syscall_abi::FS_ERR_MIN {
            // A read failure is NOT "no such user", and the two KINDS of failure
            // are not the same either. `NO_FS` and a `TASK_ERR_*` from an `fsd`
            // restart clear on their own, so reporting them as a refusal sends
            // the operator after the cluster identity for something that would
            // have succeeded a moment later. An absent `/etc/passwd` never
            // clears, so inviting a retry for THAT is the mirror mistake.
            // `transient_fs_failure` is the same classifier `load_auth` retries
            // on, so the two cannot drift apart.
            return if transient_fs_failure(r) {
                LineScan::Unreadable
            } else {
                LineScan::NoDatabase
            };
        }
        let n = (r as usize).min(CHUNK);
        off += n as u64;
        for &b in &chunk[..n] {
            match b {
                b'\n' => {
                    if !overlong && ll > 0 && f(&line[..ll]) {
                        return LineScan::Found;
                    }
                    ll = 0;
                    overlong = false;
                }
                b'\r' => {}
                _ if ll < line.len() => {
                    line[ll] = b;
                    ll += 1;
                }
                _ => overlong = true,
            }
        }
        if n < CHUNK {
            break;
        }
    }
    if !overlong && ll > 0 && f(&line[..ll]) {
        LineScan::Found
    } else {
        LineScan::NotFound
    }
}

/// What a scan of an account file actually concluded. Three outcomes, because
/// two of them are refusals for opposite reasons: `NotFound` is an answer (this
/// machine has no such account, refuse and mean it), `Unreadable` is the absence
/// of one (ask again).
#[derive(Clone, Copy, PartialEq)]
enum LineScan {
    /// The callback accepted a line.
    Found,
    /// Every line was read; none matched.
    NotFound,
    /// The file could not be read at all, and RETRYING MAY HELP - no filesystem
    /// mounted yet, or `fsd` restarted mid-scan.
    Unreadable,
    /// The file could not be read at all, and retrying will NOT help - it is
    /// absent, or is a directory, or the read was refused.
    ///
    /// Split out because folding it into [`LineScan::Unreadable`] made every
    /// caller invite a retry that could never succeed. A machine with no
    /// `/etc/passwd` is a SUPPORTED state (`login` documents its root-session
    /// fallback for exactly that), so this is reachable by configuration, not
    /// only by breakage: the far side was told "no filesystem, try again" about
    /// a disk that is mounted and a file that will never appear.
    NoDatabase,
}

/// Strip a fixed-width header name's NUL padding.
fn trim_nul(b: &[u8]) -> &[u8] {
    let mut n = b.len();
    while n > 0 && b[n - 1] == 0 {
        n -= 1;
    }
    &b[..n]
}

/// The name of the user whose request we are forwarding, from **this** machine's
/// `/etc/passwd`. `false` if there is no name to be had.
///
/// The uid comes from `SENDER_ID` - the credential the kernel bound when the
/// client sent its request, not whoever occupies that slot by now. Reading it is
/// safe anywhere before the TCP exchange, because only an *unfiltered* receive
/// replaces the captured credential and every call this makes to `fsd` is a
/// `MSG_CALL` (a filtered one). It is NOT safe after the exchange begins, which
/// is why both callers resolve the name first.
#[inline(never)]
fn caller_name(out: &mut [u8; ninep_abi::NP_NAME_LEN]) -> LineScan {
    let packed = syscall4(syscall_abi::SENDER_ID, 0, 0, 0, 0);
    if packed == syscall_abi::GET_ID_ERR {
        return LineScan::NotFound;
    }
    let uid = packed as u32;
    for_each_line(b"/etc/passwd", |line| match accounts::find_user_by_uid(line, uid) {
        // A name too long for the header cannot cross the cluster, and must not
        // cross TRUNCATED - a prefix could name a different account over there.
        Some(a) if !a.name.is_empty() && a.name.len() <= out.len() => {
            out[..a.name.len()].copy_from_slice(a.name);
            true
        }
        _ => false,
    })
}

/// Resolve a remote caller's **name** through this machine's `/etc/passwd`.
/// `None` when there is no such account here, which the caller must treat as a
/// refusal.
///
/// This is where "name, not number" earns itself: the two nodes have independent
/// databases, so uid 1000 need not be the same person on both. Resolving the
/// name locally means our permission model applies to *our* idea of who that
/// is, and an account we have never heard of gets nothing rather than silently
/// landing on whoever holds that number here.
///
/// No cache: this costs a few local IPC round trips per remote operation,
/// against a network round trip that dominates them. A cache would also have to
/// answer when it goes stale relative to `useradd`/`usermod`, and a wrong answer
/// there is a misauthorization. If the cost ever matters, that is the question
/// to answer first.
#[inline(never)]
fn map_user(who: &[u8; ninep_abi::NP_NAME_LEN]) -> Result<Proxy, LineScan> {
    let name = trim_nul(who);
    if name.is_empty() {
        return Err(LineScan::NotFound);
    }
    let mut found: Option<Proxy> = None;
    let scan = for_each_line(b"/etc/passwd", |line| match accounts::find_user_by_name(line, name) {
        Some(a) => {
            found = Some(Proxy { uid: a.uid, gid: a.gid });
            true
        }
        None => false,
    });
    // An id too large for the identity word cannot be carried, and truncating it
    // would authorize a different account. Refuse instead.
    match found.filter(|p| ninep_abi::proxy_id(p.uid, p.gid).is_some()) {
        Some(p) => Ok(p),
        // A scan that could not read the file has not established that the
        // account is absent - only that we cannot tell, and there are two ways
        // of not telling. PROPAGATED, not flattened: this arm was
        // `scan == LineScan::Unreadable`, so a third answer collapsed into
        // "no such account" one level ABOVE the code that distinguishes them,
        // and a correctly-named caller was called a stranger because THIS
        // machine has no account database. Found by running it against a node
        // with no `/etc/passwd`, not by reading it.
        None if scan != LineScan::Found => Err(scan),
        None => Err(LineScan::NotFound),
    }
}

/// WHO a call to `fsd` is made on behalf of - a **required parameter** of every
/// one of the helpers below, and the reason a remote request can no longer
/// reach `fsd` with `netd`'s own root authority.
///
/// It is a parameter rather than a wrapper function, and that is the whole
/// design. An earlier attempt attached identity with opt-in `_as` twins and
/// wrote down the invariant *"is every export-path call proxied? - one grep."*
/// One path (`fsd_write_at`) was simply forgotten, so remote `cp`, `>>` and
/// `writeat` ran as root, and the grep never saw it because it searched for the
/// helper names that were known rather than for the property. A required
/// parameter cannot be forgotten: a new export verb that calls `fsd` does not
/// compile until it says who it is for. See docs/unspellable-postmortem.md.
#[derive(Clone, Copy)]
enum As {
    /// `netd`'s own business: its cluster identity and authorized-peer files, the
    /// `/etc/passwd` lookups behind name resolution, the HTTP server's file reads. Authorized against
    /// `netd`'s own kernel-bound credential, exactly as before this existed.
    FirstParty,
    /// A remote caller, already resolved through THIS machine's `/etc/passwd`.
    Remote(Proxy),
    /// An **anonymous** request - one that arrived over the network with no
    /// identity at all, which today means the HTTP static-file server. Served
    /// as `nobody` ([`NP_ID_ANON`](ninep_abi::NP_ID_ANON)), NOT as netd.
    ///
    /// This is the other listener, and it needs saying: HTTP has no user model,
    /// so before this it read files with netd's own root credential and `fsd`'s
    /// root bypass served them - an unauthenticated `GET /etc/shadow` on port 80
    /// returned the hashes, no signature and no username involved. That is the
    /// same exposure the cluster identity work closes, reachable through the
    /// door next to it. Anonymous means `other`-triad access and nothing more.
    Anonymous,
}

impl As {
    /// The `a3` identity word this puts in the request. A [`Proxy`] whose ids
    /// cannot be packed yields [`NP_ID_NONE`](ninep_abi::NP_ID_NONE), which
    /// `fsd` refuses - the safe direction, since the alternative would be
    /// truncating onto some other account.
    fn word(self) -> u64 {
        match self {
            As::FirstParty => ninep_abi::NP_ID_FIRST_PARTY,
            As::Remote(p) => ninep_abi::proxy_id(p.uid, p.gid).unwrap_or(ninep_abi::NP_ID_NONE),
            As::Anonymous => ninep_abi::NP_ID_ANON,
        }
    }
}

/// A remote caller mapped onto **this** machine's account model.
#[derive(Clone, Copy)]
struct Proxy {
    uid: u32,
    gid: u32,
}

/// One request/response round trip to the filesystem server over the uniform
/// verb set ([`ninep-abi`], the Phase 0 cluster protocol): build a request
/// (verb + `tree` selector + two u64 params + the path payload), `MSG_CALL`
/// `fsd`, and return its status - copying any inline reply payload into
/// `result`. Returns the status (a byte count / size on success, or an
/// `FS_ERR_*`/`TASK_ERR_*` code `>= FS_ERR_MIN`). The shell's `np_call`, pared
/// to netd's two callers. `tree` is `0` for now (a single implicit mount).
fn fsd_call(who: As, verb: u64, tree: u64, p0: u64, p1: u64, path: &[u8], result: &mut [u8]) -> u64 {
    fsd_call3(who, verb, tree, p0, p1, 0, path, result)
}

/// Like [`fsd_call`] but also forwards the third param (`a2`) - the one verb
/// that needs it across the export is `NP_CHOWN` (`a1` = uid, `a2` = gid).
// One over clippy's argument limit, and the one over is `who` - the parameter
// whose whole purpose is that it cannot be omitted. Bundling params into a
// struct to satisfy the lint would make the call sites shorter and the omission
// spellable again.
#[allow(clippy::too_many_arguments)]
fn fsd_call3(who: As, verb: u64, tree: u64, p0: u64, p1: u64, p2: u64, path: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&verb.to_le_bytes());
    req[8..16].copy_from_slice(&tree.to_le_bytes()); // 0 = disk; NS_PROC_TREE = /proc
    req[16..24].copy_from_slice(&p0.to_le_bytes());
    req[24..32].copy_from_slice(&p1.to_le_bytes());
    req[32..40].copy_from_slice(&p2.to_le_bytes());
    req[40..48].copy_from_slice(&who.word().to_le_bytes()); // a3: WHO this is for
    let end = HDR + path.len();
    if end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[HDR..end].copy_from_slice(path);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::FSD_TASK,
        req.as_ptr() as u64,
        end as u64,
        reply.as_mut_ptr() as u64,
    );
    if packed >= syscall_abi::FS_ERR_MIN {
        return packed; // MSG_CALL failed (no fsd task, denied, interrupted)
    }
    let reply_len = (packed & 0xffff_ffff) as usize;
    if reply_len < 8 {
        return syscall_abi::FS_ERROR;
    }
    let n = (reply_len - 8).min(result.len());
    result[..n].copy_from_slice(&reply[8..8 + n]);
    read_u64(&reply, 0) // fsd's status
}

/// Stat a file: `NP_READ_FILE`'s status is the file's *real* size regardless
/// of how much was copied, so a `want` of 1 (the smallest fsd accepts - it
/// rejects 0) gets the size in one call; the returned byte is ignored.
fn stat_size(who: As, path: &[u8], tree: u64) -> u64 {
    fsd_call(who, ninep_abi::NP_READ_FILE, tree, path.len() as u64, 1, path, &mut [])
}

/// List a directory's entries into `out` as newline-separated names (dirs
/// suffixed `/`, the same format the shell's `ls` uses); status is the byte
/// count, or an error code. Bounded by fsd's inline reply cap.
fn list_dir(who: As, path: &[u8], tree: u64, out: &mut [u8]) -> u64 {
    fsd_call(
        who,
        ninep_abi::NP_READDIR,
        tree,
        path.len() as u64,
        out.len() as u64,
        path,
        out,
    )
}

/// Build a browsable HTML directory index for `dir_path` from fsd's
/// newline-separated `entries` (dirs suffixed `/`) into `out`, returning its
/// length. Each entry is a link resolved against `dir_path` so a browser can
/// navigate into subdirectories and open files. No `Content-Length` (the body
/// is generated; `Connection: close` delimits it); truncated if it exceeds
/// `out`.
fn build_listing(dir_path: &[u8], entries: &[u8], out: &mut [u8]) -> usize {
    // The directory path without a trailing slash (except root itself), for
    // joining hrefs cleanly.
    let base: &[u8] = if dir_path.len() > 1 && *dir_path.last().unwrap() == b'/' {
        &dir_path[..dir_path.len() - 1]
    } else {
        dir_path
    };

    let mut w = 0usize;
    let mut put = |bytes: &[u8], w: &mut usize| {
        let n = bytes.len().min(out.len().saturating_sub(*w));
        out[*w..*w + n].copy_from_slice(&bytes[..n]);
        *w += n;
    };

    put(b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n", &mut w);
    put(b"<!DOCTYPE html><html><head><title>Index of ", &mut w);
    put(base, &mut w);
    put(b"</title></head><body><h1>Index of ", &mut w);
    put(base, &mut w);
    put(b"</h1><ul>", &mut w);

    for line in entries.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let is_dir = *line.last().unwrap() == b'/';
        let name = if is_dir { &line[..line.len() - 1] } else { line };
        if name == b"." {
            continue; // skip the self-link; keep ".." for parent navigation
        }
        put(b"<li><a href=\"", &mut w);
        // href = base + "/" + name  (base is "/" for root -> avoid "//")
        if base != b"/" {
            put(base, &mut w);
        }
        put(b"/", &mut w);
        put(name, &mut w);
        if is_dir {
            put(b"/", &mut w);
        }
        put(b"\">", &mut w);
        put(line, &mut w); // display name (keeps the trailing / for dirs)
        put(b"</a></li>", &mut w);
    }

    put(b"</ul></body></html>", &mut w);
    w
}

/// Pick a Content-Type from the request path's extension (case-insensitive).
/// A small, common set; everything else is served as a generic binary stream.
fn content_type(path: &[u8]) -> &'static [u8] {
    // Find the extension (bytes after the last '.').
    let mut dot = None;
    for (i, &b) in path.iter().enumerate() {
        if b == b'.' {
            dot = Some(i);
        }
    }
    let Some(d) = dot else {
        return b"application/octet-stream";
    };
    let ext = &path[d + 1..];
    // Lowercase into a small fixed buffer for comparison.
    let mut lc = [0u8; 8];
    if ext.is_empty() || ext.len() > lc.len() {
        return b"application/octet-stream";
    }
    for (i, &b) in ext.iter().enumerate() {
        lc[i] = b.to_ascii_lowercase();
    }
    let e = &lc[..ext.len()];
    match e {
        b"html" | b"htm" => b"text/html",
        b"txt" | b"cfg" | b"md" | b"log" => b"text/plain",
        b"css" => b"text/css",
        b"js" => b"text/javascript",
        b"json" => b"application/json",
        b"png" => b"image/png",
        b"jpg" | b"jpeg" => b"image/jpeg",
        b"gif" => b"image/gif",
        _ => b"application/octet-stream",
    }
}

/// Build a `200 OK` header with `Content-Type` and `Content-Length` into
/// `buf`, returning its length.
fn build_200_header(buf: &mut [u8], ct: &[u8], size: u64) -> usize {
    let mut w = 0;
    let mut put = |bytes: &[u8], w: &mut usize| {
        let n = bytes.len().min(buf.len() - *w);
        buf[*w..*w + n].copy_from_slice(&bytes[..n]);
        *w += n;
    };
    put(b"HTTP/1.0 200 OK\r\nContent-Type: ", &mut w);
    put(ct, &mut w);
    put(b"\r\nContent-Length: ", &mut w);
    let mut digits = [0u8; 20];
    let dn = u64_decimal(size, &mut digits);
    put(&digits[..dn], &mut w);
    put(b"\r\nConnection: close\r\n\r\n", &mut w);
    w
}

/// Format `v` as decimal into `buf`, returning the digit count. Hand-rolled
/// (netd builds all its bytes manually; no `core::fmt`).
fn u64_decimal(v: u64, buf: &mut [u8]) -> usize {
    if v == 0 {
        buf[0] = b'0';
        return 1;
    }
    let mut tmp = [0u8; 20];
    let mut n = 0;
    let mut x = v;
    while x > 0 {
        tmp[n] = b'0' + (x % 10) as u8;
        x /= 10;
        n += 1;
    }
    for i in 0..n {
        buf[i] = tmp[n - 1 - i];
    }
    n
}

/// Send up to `MAX_BURST` segments of the response - as much as the peer's
/// window currently allows - advancing `snd_nxt`, then stop. Runs once per
/// event-loop wake; the peer's ACKs (which free window) drive the next call.
/// Two bounds work together: the **window** (`snd_nxt - snd_una` never exceeds
/// it) is flow control - it's what lets a file of any size stream paced by the
/// client's consumption rather than overrunning its receive buffer; the
/// **burst cap** keeps any single pump short so netd returns to service its
/// mailbox (and ack the supervisor health-ping) promptly - a whole-window
/// blast per call once looked wedged and got netd restarted mid-transfer.
fn pump_send(mac: &[u8; 6], c: &mut TcpConn) {
    // Remote-run capture (cluster Phase 4a): while a cpu child is still running,
    // its output isn't complete - hold off streaming until end-of-stream clears
    // this (see the cpu-child routing in `drain_client_messages`).
    if c.cpu_child != CPU_NONE {
        return;
    }
    /// Segments per pump call. One: the caller (`serve`) loops pump + a
    /// mailbox drain, so it still flushes a *full window* per wake, but the
    /// supervisor health-ping is acked after **every** segment (~1 fsd read +
    /// send, a few ms) - never letting one uninterrupted stretch approach the
    /// ~160 ms ping timeout. Larger bursts (16, 48) were both measured to
    /// occasionally overrun it under QEMU's variable-latency TCG and get netd
    /// restarted mid-transfer; draining per segment removes that entirely at
    /// negligible cost (one extra non-blocking syscall per segment, dwarfed by
    /// the disk read itself).
    const MAX_BURST: usize = 1;
    let mut sent = 0usize;
    loop {
        if sent >= MAX_BURST {
            return; // yield - the next wake (driven by an ACK) continues
        }
        let in_flight = c.snd_nxt.wrapping_sub(c.snd_una);
        // Send window: the smaller of the congestion window and the peer's
        // advertised flow-control window, minus what's already in flight.
        let avail = c.cwnd.min(c.window).saturating_sub(in_flight);
        if avail == 0 {
            return; // window full - wait for an ACK
        }

        // 1. The prefix (a whole fixed response, or the file's 200 header).
        if c.prefix_off < c.prefix_len {
            let n = (c.prefix_len - c.prefix_off)
                .min(SERVE_CHUNK)
                .min(avail as usize);
            // Copy out of the conn's own buffer so send_seg can borrow c.
            let mut seg = [0u8; SERVE_CHUNK];
            seg[..n].copy_from_slice(&c.prefix[c.prefix_off..c.prefix_off + n]);
            let seg_start = c.snd_nxt;
            send_seg(mac, c, TCP_PSH | TCP_ACK, false, &seg[..n]);
            c.snd_nxt = c.snd_nxt.wrapping_add(n as u32);
            rtt_on_send(c, seg_start);
            c.prefix_off += n;
            sent += 1;
            continue;
        }

        // 2. The file body, one window-and-MSS-bounded chunk at a time.
        if c.file && !c.eof {
            let want = SERVE_CHUNK.min(avail as usize);
            let mut chunk = [0u8; SERVE_CHUNK];
            let r = read_file_chunk(As::Anonymous, &c.path[..c.path_len], 0, c.read_off, &mut chunk[..want]);
            if r >= syscall_abi::FS_ERR_MIN {
                c.eof = true; // a mid-stream read error just ends the body
                continue;
            }
            let got = r as usize;
            if got == 0 {
                c.eof = true; // real end of file
                continue;
            }
            let seg_start = c.snd_nxt;
            send_seg(mac, c, TCP_PSH | TCP_ACK, false, &chunk[..got]);
            c.snd_nxt = c.snd_nxt.wrapping_add(got as u32);
            rtt_on_send(c, seg_start);
            c.read_off += got as u64;
            sent += 1;
            continue;
        }

        // 3. Body fully sent -> FIN (once). avail >= 1 here, so it fits.
        if !c.fin_sent {
            send_seg(mac, c, TCP_FIN | TCP_ACK, false, &[]);
            c.snd_nxt = c.snd_nxt.wrapping_add(1); // FIN consumes a sequence number
            c.fin_sent = true;
            c.state = ConnState::Closing;
        }
        return;
    }
}

/// Extract the request-target path from an HTTP request line
/// (`GET /path HTTP/1.1`) into `buf`, stripping any query string. No
/// %-decoding (paths here are simple). Returns the path slice (empty if the
/// line is malformed - treated as `/`).
fn parse_path<'a>(request: &[u8], buf: &'a mut [u8]) -> &'a [u8] {
    // Skip the method to the first space.
    let mut i = 0;
    while i < request.len() && request[i] != b' ' {
        i += 1;
    }
    i += 1; // past the space
    let start = i;
    while i < request.len()
        && request[i] != b' '
        && request[i] != b'?'
        && request[i] != b'\r'
        && request[i] != b'\n'
    {
        i += 1;
    }
    let end = i.min(request.len());
    if start >= end {
        return &buf[..0];
    }
    let n = (end - start).min(buf.len());
    buf[..n].copy_from_slice(&request[start..start + n]);
    &buf[..n]
}

/// Read up to `buf.len()` bytes of the file at `path` (starting at `offset`)
/// from the filesystem server via the grant/safecopy bulk path - the same
/// mechanism the shell's `cat` uses, ported here so netd can serve files.
/// Returns the byte count copied into `buf` (`0` at/past EOF), or an
/// `FS_ERR_*`/`TASK_ERR_*` code (`>= FS_ERR_MIN`). `buf.len()` must be
/// `<= SAFECOPY_MAX`.
fn read_file_chunk(who: As, path: &[u8], tree: u64, offset: u64, buf: &mut [u8]) -> u64 {
    let want = buf.len() as u64;
    // Grant the buffer to fsd (GRANT_WRITE - the server writes file bytes into
    // it via SAFECOPY during the call).
    let granted = syscall4(
        syscall_abi::GRANT,
        syscall_abi::FSD_TASK,
        buf.as_mut_ptr() as u64,
        want,
        syscall_abi::GRANT_WRITE,
    );
    if granted != 0 {
        return syscall_abi::FS_ERROR;
    }
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&ninep_abi::NP_READ.to_le_bytes());
    req[8..16].copy_from_slice(&tree.to_le_bytes()); // 0 = disk; NS_PROC_TREE = /proc
    req[16..24].copy_from_slice(&(path.len() as u64).to_le_bytes()); // param0: path len
    req[24..32].copy_from_slice(&offset.to_le_bytes()); // param1: offset
    req[32..40].copy_from_slice(&want.to_le_bytes()); // param2: want
    req[40..48].copy_from_slice(&who.word().to_le_bytes()); // a3: WHO this is for
    let end = HDR + path.len();
    if end > req.len() {
        return syscall_abi::FS_ERROR;
    }
    req[HDR..end].copy_from_slice(path);
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let packed = syscall4(
        syscall_abi::MSG_CALL,
        syscall_abi::FSD_TASK,
        req.as_ptr() as u64,
        end as u64,
        reply.as_mut_ptr() as u64,
    );
    // A missing fsd (or a denied/interrupted call) is an error; pass the code
    // through so the caller can map TASK_ERR_NO_SUCH_TASK -> 503.
    if packed >= syscall_abi::FS_ERR_MIN {
        return packed;
    }
    let reply_len = (packed & 0xffff_ffff) as usize;
    if reply_len < 8 {
        return syscall_abi::FS_ERROR;
    }
    // Reply status = bytes delivered (already SAFECOPY'd into `buf`), or an
    // FS_ERR_* code.
    read_u64(&reply, 0)
}

/// Build an Ethernet+IPv4+TCP segment into `out`, returning its length.
/// `with_mss` adds a 4-byte MSS option (for the SYN). The TCP checksum
/// covers the IPv4 pseudo-header.
#[allow(clippy::too_many_arguments)]
fn build_tcp(
    mac: &[u8; 6],
    dst_mac: &[u8; 6],
    dst_ip: &[u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    with_mss: bool,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    // Client direction: our (rotating) ephemeral source port to the peer's port
    // (80 for an HTTP fetch, NP_NET_PORT 564 for a 9P remote-mount round trip).
    // We don't advertise SACK-permitted as a client (our receiver is
    // in-order-only; SACK is the server's sender-side win - see build_tcp_srv).
    build_tcp_generic(
        mac, dst_mac, dst_ip, src_port, dst_port, seq, ack, flags, with_mss, false, payload,
        out,
    )
}

/// Server direction: our port 80 to the peer's ephemeral port. Same framing
/// as [`build_tcp`], just the ports reversed - both delegate to
/// [`build_tcp_generic`].
#[allow(clippy::too_many_arguments)]
fn build_tcp_srv(
    mac: &[u8; 6],
    peer_mac: &[u8; 6],
    peer_ip: &[u8; 4],
    peer_port: u16,
    src_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    with_mss: bool,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    // Server direction: source port is the connection's local listen port (80
    // HTTP or 564 9P export). Advertise SACK-permitted on our SYN-ACK (the only
    // with_mss server segment), so the peer sends SACK blocks we can use for
    // selective retransmit (see sack_retransmit).
    build_tcp_generic(
        mac, peer_mac, peer_ip, src_port, peer_port, seq, ack, flags, with_mss, with_mss, payload,
        out,
    )
}

/// Build an Ethernet+IPv4+TCP segment (source IP always ours). The TCP
/// checksum covers the IPv4 pseudo-header; `with_mss` adds the SYN's MSS
/// option. Shared by the client ([`build_tcp`]) and server ([`build_tcp_srv`])
/// directions, which differ only in the source/destination ports.
#[allow(clippy::too_many_arguments)]
fn build_tcp_generic(
    mac: &[u8; 6],
    dst_mac: &[u8; 6],
    dst_ip: &[u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    with_mss: bool,
    sack_perm: bool,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    // SYN options: MSS (4 bytes) plus, on the server's SYN-ACK, SACK-permitted
    // (kind 4, len 2) padded with two NOPs to a 4-byte boundary - 8 bytes.
    let opt_len = if with_mss {
        if sack_perm { 8 } else { 4 }
    } else {
        0
    };
    let tcp_hdr = 20 + opt_len;
    const IP_LEN: usize = 20;
    let total = 14 + IP_LEN + tcp_hdr + payload.len();
    if out.len() < total {
        return None;
    }
    // Ethernet.
    out[0..6].copy_from_slice(dst_mac);
    out[6..12].copy_from_slice(mac);
    out[12..14].copy_from_slice(&[0x08, 0x00]);
    // IPv4.
    let ip = 14;
    out[ip] = 0x45;
    out[ip + 1] = 0;
    out[ip + 2..ip + 4].copy_from_slice(&((IP_LEN + tcp_hdr + payload.len()) as u16).to_be_bytes());
    out[ip + 4..ip + 8].copy_from_slice(&[0u8; 4]);
    out[ip + 8] = 64;
    out[ip + 9] = 6; // protocol: TCP
    out[ip + 10..ip + 12].copy_from_slice(&[0, 0]);
    out[ip + 12..ip + 16].copy_from_slice(&our_ip());
    out[ip + 16..ip + 20].copy_from_slice(dst_ip);
    let ipc = ip_checksum(&out[ip..ip + IP_LEN]);
    out[ip + 10..ip + 12].copy_from_slice(&ipc.to_be_bytes());
    // TCP.
    let t = ip + IP_LEN;
    out[t..t + 2].copy_from_slice(&src_port.to_be_bytes());
    out[t + 2..t + 4].copy_from_slice(&dst_port.to_be_bytes());
    out[t + 4..t + 8].copy_from_slice(&seq.to_be_bytes());
    out[t + 8..t + 12].copy_from_slice(&ack.to_be_bytes());
    out[t + 12] = ((tcp_hdr / 4) as u8) << 4; // data offset in 32-bit words
    out[t + 13] = flags;
    out[t + 14..t + 16].copy_from_slice(&64240u16.to_be_bytes()); // window
    out[t + 16..t + 18].copy_from_slice(&[0, 0]); // checksum placeholder
    out[t + 18..t + 20].copy_from_slice(&[0, 0]); // urgent pointer
    if with_mss {
        out[t + 20..t + 24].copy_from_slice(&[2, 4, 0x05, 0xb4]); // MSS 1460
        if sack_perm {
            // SACK-permitted (kind 4, len 2) + two NOPs (kind 1) to pad to 4.
            out[t + 24..t + 28].copy_from_slice(&[4, 2, 1, 1]);
        }
    }
    out[t + tcp_hdr..t + tcp_hdr + payload.len()].copy_from_slice(payload);
    let seg_len = tcp_hdr + payload.len();
    let csum = tcp_checksum(&our_ip(), dst_ip, &out[t..t + seg_len]);
    out[t + 16..t + 18].copy_from_slice(&csum.to_be_bytes());
    Some(total)
}

/// Parse a received frame as a TCP segment from `peer`:`peer_port` to us. Uses
/// the IP total-length field to find the true end of the TCP data (ignoring any
/// Ethernet padding). `peer_port` is the remote port this client connected to -
/// 80 for an HTTP fetch, `NP_NET_PORT` 564 (or whatever `mount -r` named) for a
/// 9P remote-mount round trip - so a reply is only accepted from that port.
fn parse_tcp(frame: &[u8], peer: &[u8; 4], peer_port: u16, our_port: u16) -> Option<TcpSeg> {
    if frame.len() < 34 || frame[12] != 0x08 || frame[13] != 0x00 {
        return None; // not IPv4
    }
    if frame[23] != 6 || frame[26..30] != *peer {
        return None; // not TCP, or not from the peer
    }
    let ihl = (frame[14] & 0x0f) as usize * 4;
    let t = 14 + ihl;
    if frame.len() < t + 20 {
        return None;
    }
    if u16be(frame, t) != peer_port || u16be(frame, t + 2) != our_port {
        return None; // wrong ports (peer's source, and our rotating ephemeral)
    }
    let data_off = t + ((frame[t + 12] >> 4) as usize) * 4;
    let ip_total = u16be(frame, 16) as usize;
    let seg_end = (14 + ip_total).min(frame.len());
    let data_len = seg_end.saturating_sub(data_off);
    Some(TcpSeg {
        seq: u32be(frame, t + 4),
        ack: u32be(frame, t + 8),
        flags: frame[t + 13],
        data_off,
        data_len,
    })
}

/// One's-complement Internet checksum over the IPv4 pseudo-header plus a TCP
/// segment (TCP requires the pseudo-header; UDP/ICMP/IP don't).
fn tcp_checksum(src: &[u8; 4], dst: &[u8; 4], seg: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for w in [
        ((src[0] as u32) << 8) | src[1] as u32,
        ((src[2] as u32) << 8) | src[3] as u32,
        ((dst[0] as u32) << 8) | dst[1] as u32,
        ((dst[2] as u32) << 8) | dst[3] as u32,
        6u32,             // protocol
        seg.len() as u32, // TCP length
    ] {
        sum += w;
    }
    let mut i = 0;
    while i + 1 < seg.len() {
        sum += ((seg[i] as u32) << 8) | seg[i + 1] as u32;
        i += 2;
    }
    if i < seg.len() {
        sum += (seg[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn u16be(b: &[u8], o: usize) -> u16 {
    ((b[o] as u16) << 8) | b[o + 1] as u16
}
fn u32be(b: &[u8], o: usize) -> u32 {
    ((b[o] as u32) << 24) | ((b[o + 1] as u32) << 16) | ((b[o + 2] as u32) << 8) | b[o + 3] as u32
}

/// Build a DNS standard query (A record, recursion desired) for `host` into
/// `out`, returning its length. Encodes the hostname as length-prefixed
/// labels (hand-rolled, no `str::split`).
fn build_dns_query(host: &[u8], out: &mut [u8]) -> Option<usize> {
    if out.len() < 12 {
        return None;
    }
    out[0..2].copy_from_slice(&DNS_ID.to_be_bytes());
    out[2..4].copy_from_slice(&0x0100u16.to_be_bytes()); // flags: recursion desired
    out[4..6].copy_from_slice(&1u16.to_be_bytes()); // qdcount
    out[6..12].copy_from_slice(&[0u8; 6]); // an/ns/ar count

    let mut w = 12;
    let mut label_start = 0;
    let mut i = 0;
    while i <= host.len() {
        if i == host.len() || host[i] == b'.' {
            let ll = i - label_start;
            if ll == 0 || ll > 63 || w + 1 + ll >= out.len() {
                return None;
            }
            out[w] = ll as u8;
            w += 1;
            out[w..w + ll].copy_from_slice(&host[label_start..i]);
            w += ll;
            label_start = i + 1;
        }
        i += 1;
    }
    if w + 5 > out.len() {
        return None;
    }
    out[w] = 0; // root label
    w += 1;
    out[w..w + 2].copy_from_slice(&1u16.to_be_bytes()); // QTYPE A
    out[w + 2..w + 4].copy_from_slice(&1u16.to_be_bytes()); // QCLASS IN
    Some(w + 4)
}

/// Wrap a DNS message in a UDP datagram, IPv4 packet, and Ethernet frame to
/// the DNS server. UDP checksum 0 (optional for IPv4, and SLIRP accepts it).
fn build_dns_frame(mac: &[u8; 6], server_mac: &[u8; 6], dns: &[u8], out: &mut [u8]) -> Option<usize> {
    const IP_LEN: usize = 20;
    const UDP_LEN: usize = 8;
    let total = 14 + IP_LEN + UDP_LEN + dns.len();
    if out.len() < total {
        return None;
    }
    // Ethernet.
    out[0..6].copy_from_slice(server_mac);
    out[6..12].copy_from_slice(mac);
    out[12..14].copy_from_slice(&[0x08, 0x00]);
    // IPv4.
    let ip = 14;
    out[ip] = 0x45;
    out[ip + 1] = 0;
    out[ip + 2..ip + 4].copy_from_slice(&((IP_LEN + UDP_LEN + dns.len()) as u16).to_be_bytes());
    out[ip + 4..ip + 8].copy_from_slice(&[0u8; 4]); // id, flags/frag
    out[ip + 8] = 64; // TTL
    out[ip + 9] = 17; // protocol: UDP
    out[ip + 10..ip + 12].copy_from_slice(&[0, 0]); // checksum placeholder
    out[ip + 12..ip + 16].copy_from_slice(&our_ip());
    out[ip + 16..ip + 20].copy_from_slice(&DNS_SERVER);
    let ipc = ip_checksum(&out[ip..ip + IP_LEN]);
    out[ip + 10..ip + 12].copy_from_slice(&ipc.to_be_bytes());
    // UDP.
    let udp = ip + IP_LEN;
    out[udp..udp + 2].copy_from_slice(&DNS_SRC_PORT.to_be_bytes());
    out[udp + 2..udp + 4].copy_from_slice(&53u16.to_be_bytes());
    out[udp + 4..udp + 6].copy_from_slice(&((UDP_LEN + dns.len()) as u16).to_be_bytes());
    out[udp + 6..udp + 8].copy_from_slice(&[0, 0]); // checksum 0 (optional on IPv4)
    out[udp + 8..udp + 8 + dns.len()].copy_from_slice(dns);
    Some(total)
}

/// If `frame` is a UDP datagram from the DNS server (port 53) carrying our
/// transaction id, return the DNS message payload.
fn dns_payload(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() < 34 || frame[12] != 0x08 || frame[13] != 0x00 {
        return None; // not IPv4
    }
    if frame[23] != 17 {
        return None; // not UDP (IP protocol = 14 + 9)
    }
    if frame[26..30] != DNS_SERVER {
        return None; // source IP != DNS server
    }
    let ihl = (frame[14] & 0x0f) as usize * 4;
    let udp = 14 + ihl;
    if frame.len() < udp + 8 {
        return None;
    }
    if frame[udp..udp + 2] != 53u16.to_be_bytes() {
        return None; // UDP source port != 53
    }
    let dns = udp + 8;
    if frame.len() < dns + 12 || frame[dns..dns + 2] != DNS_ID.to_be_bytes() {
        return None; // truncated, or not our query id
    }
    Some(&frame[dns..])
}

/// Find the first A record's IPv4 in a DNS response, handling name
/// compression pointers. `None` if there's no usable A record.
fn parse_dns_a(resp: &[u8]) -> Option<[u8; 4]> {
    if resp.len() < 12 {
        return None;
    }
    let qdcount = ((resp[4] as usize) << 8) | resp[5] as usize;
    let ancount = ((resp[6] as usize) << 8) | resp[7] as usize;
    if ancount == 0 {
        return None;
    }
    let mut pos = 12;
    for _ in 0..qdcount {
        pos = skip_name(resp, pos)?;
        pos += 4; // qtype + qclass
    }
    for _ in 0..ancount {
        pos = skip_name(resp, pos)?;
        if pos + 10 > resp.len() {
            return None;
        }
        let rtype = ((resp[pos] as usize) << 8) | resp[pos + 1] as usize;
        let rdlength = ((resp[pos + 8] as usize) << 8) | resp[pos + 9] as usize;
        let rdata = pos + 10;
        if rtype == 1 && rdlength == 4 && rdata + 4 <= resp.len() {
            return Some([resp[rdata], resp[rdata + 1], resp[rdata + 2], resp[rdata + 3]]);
        }
        pos = rdata + rdlength;
    }
    None
}

/// Skip a DNS name at `pos`, returning the position just after it. A
/// compression pointer (top two bits set) is a 2-byte terminal.
fn skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        if pos >= buf.len() {
            return None;
        }
        let len = buf[pos] as usize;
        if len == 0 {
            return Some(pos + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(pos + 2); // compression pointer ends the name
        }
        pos += 1 + len;
    }
}

/// One's-complement Internet checksum over 16-bit big-endian words (the IPv4
/// header and ICMP message both use it).
fn ip_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += ((data[i] as u32) << 8) | data[i + 1] as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn unpack_mac(packed: u64) -> [u8; 6] {
    [
        packed as u8,
        (packed >> 8) as u8,
        (packed >> 16) as u8,
        (packed >> 24) as u8,
        (packed >> 32) as u8,
        (packed >> 40) as u8,
    ]
}

fn read_u64(buf: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&buf[off..off + 8]);
    u64::from_le_bytes(b)
}

fn now() -> u64 {
    syscall(syscall_abi::GET_TICKS, 0)
}

/// Tell the supervisor we are alive, without being asked.
///
/// Every wait below BUSY-POLLS a non-blocking `recv`, so this task stays
/// `Runnable` for the whole of a network round trip. `supervisor.rs`'s passive
/// detector restarts a server observed continuously `Runnable` for
/// `WEDGE_TICKS` (2.56s), and its active ping only pokes a *Blocked* server -
/// so a busy one is never asked whether it is alive, and was simply killed for
/// taking too long. A remote `cat` of a 1960-byte file is five sequential round
/// trips; against a peer that signs in Python (~0.6s each) that is 3.0s, and it
/// failed 100% of the time with the supervisor restarting `netd` underneath it.
/// Traced 2026-09-03 - see docs/ROADMAP.md frontier item 3, where the packet
/// capture showed every TCP connection healthy and the fault here instead.
///
/// The kernel intercepts a `MSG_SEND` to `KERNEL_SENDER` as an ack before any
/// argument validation (syscall.rs), so this needs no buffer and cannot fail;
/// `note_ack` is a no-op for a task the supervisor does not manage.
fn heartbeat() {
    syscall4(syscall_abi::MSG_SEND, syscall_abi::KERNEL_SENDER, 0, 0, 0);
}

/// A wait loop's per-tick heartbeat: beats at most once per tick (20ms) rather
/// than once per spin, which would be a syscall per iteration of a tight loop.
/// Returns the new tick stamp to carry into the next iteration.
fn beat_if_new_tick(t: u64, last: u64) -> u64 {
    if t != last {
        heartbeat();
        return t;
    }
    last
}

/// Microseconds since boot - a high-resolution monotonic clock (unlike
/// `now()`'s 20ms tick), used for RTT estimation where a real elapsed
/// duration matters. Only meaningful as a difference of two readings.
fn now_us() -> u64 {
    syscall(syscall_abi::MONOTONIC_US, 0)
}

/// Fold a fresh round-trip-time measurement `r_us` into a connection's
/// smoothed RTT and variance and recompute its RTO (RFC 6298). First sample:
/// `SRTT = R`, `RTTVAR = R/2`. Later: `RTTVAR = 3/4 RTTVAR + 1/4 |SRTT - R|`,
/// `SRTT = 7/8 SRTT + 1/8 R`. Then `RTO = SRTT + max(G, 4 RTTVAR)`, converted
/// to `now()` ticks and clamped to `[RTO_MIN_TICKS, RTO_MAX_TICKS]`.
fn rtt_update(c: &mut TcpConn, r_us: u64) {
    if c.srtt_us == 0 {
        c.srtt_us = r_us;
        c.rttvar_us = r_us / 2;
    } else {
        let delta = c.srtt_us.abs_diff(r_us);
        c.rttvar_us = (3 * c.rttvar_us + delta) / 4;
        c.srtt_us = (7 * c.srtt_us + r_us) / 8;
    }
    let rto_us = c.srtt_us + (4 * c.rttvar_us).max(RTT_G_US);
    c.rto_ticks = (rto_us / TICK_US).clamp(RTO_MIN_TICKS, RTO_MAX_TICKS);
}

/// Called after a data segment starting at `seg_start` is sent. If it's new
/// data (at or past the send high-water mark, not a retransmit) and no RTT
/// sample is already outstanding, start timing it: the sample completes when
/// `snd_nxt` (its end seq) is acked. Karn's algorithm - retransmits are never
/// timed, and an outstanding sample is invalidated if the segment is later
/// retransmitted (see `rewind_to`).
fn rtt_on_send(c: &mut TcpConn, seg_start: u32) {
    if seq_gt(c.snd_max, seg_start) {
        return; // seg_start < snd_max: a retransmit, not new data - don't time
    }
    c.snd_max = c.snd_nxt;
    if !c.rtt_active {
        c.rtt_active = true;
        c.rtt_seq = c.snd_nxt;
        c.rtt_start_us = now_us();
    }
}

/// Transmit one frame. `Ok(())` on success.
fn send(frame: &[u8]) -> Result<(), ()> {
    if syscall4(syscall_abi::NET_SEND, frame.as_ptr() as u64, frame.len() as u64, 0, 0) == 0 {
        Ok(())
    } else {
        Err(())
    }
}

/// Non-blocking receive of one frame into `buf`; `Some(len)` if one was
/// waiting.
fn recv(buf: &mut [u8]) -> Option<usize> {
    let r = syscall4(syscall_abi::NET_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
    if r == syscall_abi::NET_NO_FRAME || r == syscall_abi::NET_ERROR {
        None
    } else {
        Some(r as usize)
    }
}

/// Reply to a client `MSG_CALL` (or ack the supervisor ping) with `data`.
fn reply(sender: u64, data: &[u8]) {
    syscall4(syscall_abi::MSG_SEND, sender, data.as_ptr() as u64, data.len() as u64, 0);
}

/// Log a decimal number.
/// Report a verb this server has no arm for, as ONE console round trip.
///
/// In hex, because every other artifact in this arc names verbs in hex -
/// `ninep-abi` defines them as `NP_BASE + n` off `0x100`, and both Python peers
/// print `0x10f`. A decimal `271` makes the reader convert before they can
/// cross-reference, in the one output whose entire purpose is legibility.
///
/// One `log()` call, not three: `log` is a synchronous `MSG_CALL` to the console
/// server, and this is reachable from a remote peer, so each extra call is
/// another IPC round trip an authorized peer can drive. It is NOT rate-limited,
/// and that is a deliberate, bounded choice rather than an oversight: netd holds
/// no static mutable state (the PIE linker script asserts `.data`/`.bss` empty),
/// so a per-boot cap has to be threaded from the main loop through five
/// signatures - too wide a change for a diagnostic line. The exposure is also
/// smaller than it looks: reaching this needs a *signature-verified, authorized*
/// peer, and the platform where a console flood actually hurts (Parallels, whose
/// framebuffer is the only console) HAS NO NETWORKING, so no remote peer can
/// reach netd there at all. The cap belongs with step 4 of
/// `docs/roadmap-fid-verbs.md`, which threads netd-owned per-connection state
/// for the fid table anyway.
fn log_no_arm(what: &[u8], verb: u64) {
    let mut line = [0u8; 64];
    let mut n = 0usize;
    for &b in b"netd: " {
        line[n] = b;
        n += 1;
    }
    for &b in what.iter().take(16) {
        line[n] = b;
        n += 1;
    }
    for &b in b": no arm for verb 0x" {
        line[n] = b;
        n += 1;
    }
    let mut started = false;
    for shift in (0..16).rev() {
        let nib = ((verb >> (shift * 4)) & 0xf) as u8;
        if nib != 0 || started || shift == 0 {
            started = true;
            line[n] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
            n += 1;
        }
    }
    line[n] = b'\r';
    line[n + 1] = b'\n';
    log(&line[..n + 2]);
}

fn log_dec(v: u64) {
    let mut buf = [0u8; 20];
    let mut n = 0usize;
    if v == 0 {
        log(b"0");
        return;
    }
    let mut x = v;
    while x > 0 {
        buf[n] = b'0' + (x % 10) as u8;
        n += 1;
        x /= 10;
    }
    let mut out = [0u8; 20];
    for i in 0..n {
        out[i] = buf[n - 1 - i];
    }
    log(&out[..n]);
}

/// Route a log line through the console server as a batched `DSPOP_WRITE`,
/// falling back to `PUTC` if there's no server this boot.
fn log(bytes: &[u8]) {
    let payload_off = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; ninep_abi::NP_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&ninep_abi::NP_WRITE_FILE.to_le_bytes());
        // tree (a8) and path_len (a16) stay 0; data_len at a1 (offset 24).
        req[24..32].copy_from_slice(&(n as u64).to_le_bytes());
        req[payload_off..payload_off + n].copy_from_slice(&bytes[off..off + n]);
        let mut r = [0u8; syscall_abi::MSG_MAX_LEN as usize];
        let ret = syscall4(
            syscall_abi::MSG_CALL,
            syscall_abi::CON_TASK,
            req.as_ptr() as u64,
            (payload_off + n) as u64,
            r.as_mut_ptr() as u64,
        );
        if ret >= syscall_abi::FS_ERR_MIN {
            for &b in &bytes[off..off + n] {
                syscall(syscall_abi::PUTC, b as u64);
            }
        }
        off += n;
    }
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

#[inline(always)]
fn syscall(number: u64, arg0: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") 0u64,
            in("x2") 0u64,
            in("x3") 0u64,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

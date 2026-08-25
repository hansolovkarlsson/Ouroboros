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

/// Our assumed IPv4 (QEMU user-net gives the guest `10.0.2.15` by default).
const OUR_IP: [u8; 4] = [10, 0, 2, 15];
/// QEMU user-net's built-in DNS proxy (forwards to the host's resolver).
const DNS_SERVER: [u8; 4] = [10, 0, 2, 3];
/// A fixed ephemeral UDP source port for DNS queries (one query at a time).
const DNS_SRC_PORT: u16 = 0x8000;
/// The DNS transaction id we send and match on the reply.
const DNS_ID: u16 = 0x4f42; // "OB"
/// QEMU user-net's gateway - the next hop for any off-subnet target.
const GATEWAY: [u8; 4] = [10, 0, 2, 2];
/// Our fixed ephemeral TCP source port and initial sequence number (one
/// connection at a time, so fixed values suffice).
const TCP_SRC_PORT: u16 = 0xc000;
const TCP_ISN: u32 = 0x0000_1000;
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
    // Up to MAX_CONNS concurrent server connections, kept on this frame
    // because userland has no static mutable state (`.bss` asserted empty).
    // `serve` never returns, so the frame persists for the whole boot.
    let mut conns: [Option<TcpConn>; MAX_CONNS] = core::array::from_fn(|_| None);
    loop {
        // Block until a client message or an incoming frame is pending (or
        // return immediately if either already is). While *any* connection has
        // data unacked, use a timeout so we still wake to service the
        // retransmit timer even if a peer has gone silent (no frames);
        // otherwise block indefinitely (the health-ping still wakes us).
        let unacked = conns
            .iter()
            .any(|c| matches!(c, Some(c) if c.snd_nxt != c.snd_una));
        let timeout = if unacked { RTO_POLL_MS } else { 0 };
        syscall(syscall_abi::NET_WAIT, timeout);

        // 1. Client requests (ping/resolve/fetch), handled synchronously; the
        // supervisor health-ping is acked here too (any reply acks it).
        drain_client_messages(packed_mac, &mut buf);

        // 2. Incoming frames: ARP replies for our IP, and the TCP server.
        if packed_mac != syscall_abi::NET_ERROR {
            while let Some(n) = recv(&mut frame) {
                on_frame(&mac, &frame[..n], &mut conns);
            }
            // 3. Per connection: service the retransmit timer (retransmit on
            // timeout, or give up on a dead peer), then stream its response up
            // to the current window - in bounded bursts, draining the mailbox
            // (acking the health-ping) between them so no single stretch runs
            // long enough to look wedged. Stops a connection's pump when a
            // burst makes no progress (window full, or the response is done).
            for slot in conns.iter_mut() {
                if let Some(c) = slot.as_mut() {
                    if service_rto(&mac, c, now()) {
                        *slot = None;
                        continue;
                    }
                }
                while let Some(c) = slot.as_mut() {
                    if !c.responded {
                        break;
                    }
                    let before = c.snd_nxt;
                    pump_send(&mac, c);
                    drain_client_messages(packed_mac, &mut buf);
                    if c.snd_nxt == before {
                        break;
                    }
                }
            }
        }
    }
}

/// Drain and handle every queued client message (and the supervisor
/// health-ping, whose reply is the ack). Non-blocking; returns when the
/// mailbox is empty.
fn drain_client_messages(packed_mac: u64, buf: &mut [u8]) {
    loop {
        let packed =
            syscall4(syscall_abi::MSG_TRY_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            return; // NO_MSG or an error: mailbox drained
        }
        let sender = packed >> 32;
        let len = (packed & 0xffff_ffff) as usize;
        handle_client(packed_mac, sender, buf, len);
    }
}

/// Dispatch one client request (a `MSG_CALL` from the shell) by op, replying
/// with the op's status. The unknown-op arm also acks the supervisor ping.
fn handle_client(packed_mac: u64, sender: u64, buf: &[u8], len: usize) {
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
    arp[28..32].copy_from_slice(&OUR_IP); // spa
    arp[38..42].copy_from_slice(target); // tpa
    if send(&arp).is_err() {
        return None;
    }

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
    ip[12..16].copy_from_slice(&OUR_IP);
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

    let deadline = now() + 50; // ~1s
    let mut frame = [0u8; 1600];
    loop {
        if let Some(len) = recv(&mut frame) {
            if is_echo_reply(&frame[..len], target) {
                return true;
            }
        }
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
    let deadline = now() + 40; // ~800ms (kept tight so a fetch's resolve+TCP
                               // total stays under the supervisor wedge threshold)
    let mut rx = [0u8; 1600];
    loop {
        if let Some(len) = recv(&mut rx) {
            if let Some(payload) = dns_payload(&rx[..len]) {
                return parse_dns_a(payload).ok_or(syscall_abi::NET_RESOLVE_NXDOMAIN);
            }
        }
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
    let (status, got) = tcp_get(&mac, &dst_mac, &ip, &req[..rlen], &mut resp);
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
fn tcp_get(mac: &[u8; 6], dst_mac: &[u8; 6], target: &[u8; 4], request: &[u8], resp: &mut [u8]) -> (u64, usize) {
    let mut frame = [0u8; 1600];
    let mut rx = [0u8; 1600];

    // SYN.
    let Some(n) = build_tcp(mac, dst_mac, target, TCP_ISN, 0, TCP_SYN, true, &[], &mut frame) else {
        return (syscall_abi::NET_FETCH_TIMEOUT, 0);
    };
    if send(&frame[..n]).is_err() {
        return (syscall_abi::NET_FETCH_TIMEOUT, 0);
    }

    // Wait for SYN-ACK (~700ms).
    let their_isn = {
        let deadline = now() + 35;
        loop {
            if let Some(len) = recv(&mut rx) {
                if let Some(s) = parse_tcp(&rx[..len], target) {
                    if s.flags & TCP_RST != 0 {
                        return (syscall_abi::NET_FETCH_REFUSED, 0);
                    }
                    if s.flags & (TCP_SYN | TCP_ACK) == (TCP_SYN | TCP_ACK) && s.ack == TCP_ISN + 1 {
                        break s.seq;
                    }
                }
            }
            if now() > deadline {
                return (syscall_abi::NET_FETCH_TIMEOUT, 0);
            }
        }
    };

    let snd_nxt = TCP_ISN + 1;
    let mut rcv_nxt = their_isn.wrapping_add(1);
    // ACK the SYN-ACK, then send the request (PSH|ACK).
    if let Some(n) = build_tcp(mac, dst_mac, target, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
        let _ = send(&frame[..n]);
    }
    let mut snd_nxt = snd_nxt;
    if let Some(n) = build_tcp(mac, dst_mac, target, snd_nxt, rcv_nxt, TCP_PSH | TCP_ACK, false, request, &mut frame) {
        let _ = send(&frame[..n]);
        snd_nxt = snd_nxt.wrapping_add(request.len() as u32);
    }

    // Receive the response until the peer's FIN or a deadline (~1s).
    let mut got = 0usize;
    let mut fin = false;
    let mut deadline = now() + 50;
    loop {
        if let Some(len) = recv(&mut rx) {
            if let Some(s) = parse_tcp(&rx[..len], target) {
                if s.flags & TCP_RST != 0 {
                    break;
                }
                if s.data_len > 0 && s.seq == rcv_nxt {
                    let take = s.data_len.min(resp.len() - got);
                    resp[got..got + take].copy_from_slice(&rx[s.data_off..s.data_off + take]);
                    got += take;
                    rcv_nxt = rcv_nxt.wrapping_add(s.data_len as u32);
                    if let Some(n) = build_tcp(mac, dst_mac, target, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    deadline = now() + 50; // extend while making progress
                }
                if s.flags & TCP_FIN != 0 {
                    rcv_nxt = rcv_nxt.wrapping_add(1); // FIN consumes one sequence number
                    // ACK the FIN, then send our own FIN to close cleanly.
                    if let Some(n) = build_tcp(mac, dst_mac, target, snd_nxt, rcv_nxt, TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    if let Some(n) = build_tcp(mac, dst_mac, target, snd_nxt, rcv_nxt, TCP_FIN | TCP_ACK, false, &[], &mut frame) {
                        let _ = send(&frame[..n]);
                    }
                    fin = true;
                    break;
                }
            }
        }
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
}

/// A parsed inbound TCP segment addressed to our listen port.
struct TcpIn {
    src_ip: [u8; 4],
    src_mac: [u8; 6],
    src_port: u16,
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
fn on_frame(mac: &[u8; 6], frame: &[u8], conns: &mut [Option<TcpConn>; MAX_CONNS]) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = ((frame[12] as u16) << 8) | frame[13] as u16;
    match ethertype {
        0x0806 => handle_arp(mac, frame),
        0x0800 => {
            if let Some(seg) = parse_tcp_in(frame) {
                handle_tcp(mac, frame, &seg, conns);
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
    if frame[20] != 0x00 || frame[21] != 0x01 || frame[38..42] != OUR_IP {
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
    arp[28..32].copy_from_slice(&OUR_IP); // spa: us
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
    if u16be(frame, t + 2) != SERVER_PORT {
        return None; // not to our listen port
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
fn handle_tcp(mac: &[u8; 6], frame: &[u8], seg: &TcpIn, conns: &mut [Option<TcpConn>; MAX_CONNS]) {
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
        };
        send_seg(mac, &c, TCP_SYN | TCP_ACK, true, &[]);
        c.snd_nxt = c.snd_nxt.wrapping_add(1); // our SYN consumes one too
        c.snd_max = c.snd_nxt; // data starts here; the SYN itself isn't timed
        conns[i] = Some(c);
        return;
    }

    // Everything else is dispatched to its matching connection (if any).
    if let Some(i) = find_conn(conns, seg) {
        if handle_conn_segment(mac, frame, seg, conns[i].as_mut().unwrap()) {
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
fn handle_conn_segment(mac: &[u8; 6], frame: &[u8], seg: &TcpIn, c: &mut TcpConn) -> bool {
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
        start_response(c, request);
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
        mac, &c.peer_mac, &c.peer_ip, c.peer_port, seq, c.rcv_nxt, flags, false, payload, &mut out,
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
        let r = read_file_chunk(&c.path[..c.path_len], foff, &mut buf[..n]);
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
    let size = stat_size(path);
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
        let listed = list_dir(path, &mut names);
        if listed < syscall_abi::FS_ERR_MIN {
            c.prefix_len = build_listing(path, &names[..listed as usize], &mut c.prefix);
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

/// One request/response round trip to the filesystem server over the uniform
/// verb set ([`ninep-abi`], the Phase 0 cluster protocol): build a request
/// (verb + `tree` selector + two u64 params + the path payload), `MSG_CALL`
/// `fsd`, and return its status - copying any inline reply payload into
/// `result`. Returns the status (a byte count / size on success, or an
/// `FS_ERR_*`/`TASK_ERR_*` code `>= FS_ERR_MIN`). The shell's `np_call`, pared
/// to netd's two callers. `tree` is `0` for now (a single implicit mount).
fn fsd_call(verb: u64, p0: u64, p1: u64, path: &[u8], result: &mut [u8]) -> u64 {
    const HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;
    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    req[0..8].copy_from_slice(&verb.to_le_bytes());
    req[8..16].copy_from_slice(&0u64.to_le_bytes()); // tree 0
    req[16..24].copy_from_slice(&p0.to_le_bytes());
    req[24..32].copy_from_slice(&p1.to_le_bytes());
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
fn stat_size(path: &[u8]) -> u64 {
    fsd_call(ninep_abi::NP_READ_FILE, path.len() as u64, 1, path, &mut [])
}

/// List a directory's entries into `out` as newline-separated names (dirs
/// suffixed `/`, the same format the shell's `ls` uses); status is the byte
/// count, or an error code. Bounded by fsd's inline reply cap.
fn list_dir(path: &[u8], out: &mut [u8]) -> u64 {
    fsd_call(
        ninep_abi::NP_READDIR,
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
            let r = read_file_chunk(&c.path[..c.path_len], c.read_off, &mut chunk[..want]);
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
fn read_file_chunk(path: &[u8], offset: u64, buf: &mut [u8]) -> u64 {
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
    req[8..16].copy_from_slice(&0u64.to_le_bytes()); // tree 0
    req[16..24].copy_from_slice(&(path.len() as u64).to_le_bytes()); // param0: path len
    req[24..32].copy_from_slice(&offset.to_le_bytes()); // param1: offset
    req[32..40].copy_from_slice(&want.to_le_bytes()); // param2: want
                                                      // param3 (0) already zeroed
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
    seq: u32,
    ack: u32,
    flags: u8,
    with_mss: bool,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    // Client direction: our ephemeral source port to the peer's port 80. We
    // don't advertise SACK-permitted as a client (our fetch receiver is
    // in-order-only; SACK is the server's sender-side win - see build_tcp_srv).
    build_tcp_generic(
        mac, dst_mac, dst_ip, TCP_SRC_PORT, 80, seq, ack, flags, with_mss, false, payload, out,
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
    seq: u32,
    ack: u32,
    flags: u8,
    with_mss: bool,
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    // Server direction: advertise SACK-permitted on our SYN-ACK (the only
    // with_mss server segment), so the peer sends SACK blocks we can use for
    // selective retransmit (see sack_retransmit).
    build_tcp_generic(
        mac, peer_mac, peer_ip, SERVER_PORT, peer_port, seq, ack, flags, with_mss, with_mss, payload,
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
    out[ip + 12..ip + 16].copy_from_slice(&OUR_IP);
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
    let csum = tcp_checksum(&OUR_IP, dst_ip, &out[t..t + seg_len]);
    out[t + 16..t + 18].copy_from_slice(&csum.to_be_bytes());
    Some(total)
}

/// Parse a received frame as a TCP segment from `peer`:80 to us. Uses the IP
/// total-length field to find the true end of the TCP data (ignoring any
/// Ethernet padding).
fn parse_tcp(frame: &[u8], peer: &[u8; 4]) -> Option<TcpSeg> {
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
    if u16be(frame, t) != 80 || u16be(frame, t + 2) != TCP_SRC_PORT {
        return None; // wrong ports
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
    out[ip + 12..ip + 16].copy_from_slice(&OUR_IP);
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

/// Route a log line through the console server as a batched `DSPOP_WRITE`,
/// falling back to `PUTC` if there's no server this boot.
fn log(bytes: &[u8]) {
    let payload_off = syscall_abi::FS_REQ_PAYLOAD as usize;
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(syscall_abi::FS_DATA_MAX as usize);
        let mut req = [0u8; syscall_abi::FS_REQ_PAYLOAD as usize + syscall_abi::FS_DATA_MAX as usize];
        req[0..8].copy_from_slice(&syscall_abi::DSPOP_WRITE.to_le_bytes());
        req[8..16].copy_from_slice(&(n as u64).to_le_bytes());
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

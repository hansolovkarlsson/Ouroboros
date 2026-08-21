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
//! the shared `shell/linker.ld`, staged as `\EFI\ORBS\NETD.BIN`.

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

/// The fixed page the server serves to every request. Small enough to fit one
/// TCP segment (well under the 1460 MSS), so the response is a single
/// `PSH|ACK`. `Connection: close` + our FIN delimit the body for HTTP/1.0.
const HTTP_RESPONSE: &[u8] = b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<!DOCTYPE html><html><head><title>Ouroboros</title></head><body><h1>Hello from Ouroboros</h1><p>Served by a from-scratch ARM64 microkernel's userland network server over a hand-rolled TCP/IP stack.</p></body></html>";

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
    // The one in-flight server connection's state - kept on this frame
    // because userland has no static mutable state (`.bss` asserted empty).
    // `serve` never returns, so the frame persists for the whole boot.
    let mut conn: Option<TcpConn> = None;
    loop {
        // Block until a client message or an incoming frame is pending (or
        // return immediately if either already is - the kernel checks both
        // up front, so this never sleeps through waiting input).
        syscall(syscall_abi::NET_WAIT, 0);

        // 1. Client requests. Handled synchronously: during a `fetch`/`ping`
        // its own receive loop consumes frames, so an unsolicited server
        // frame arriving mid-request is dropped (single-threaded, one thing
        // at a time) - acceptable, the client retransmits.
        loop {
            let packed =
                syscall4(syscall_abi::MSG_TRY_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
            if packed >= syscall_abi::FS_ERR_MIN {
                break; // NO_MSG or an error: mailbox drained
            }
            let sender = packed >> 32;
            let len = (packed & 0xffff_ffff) as usize;
            handle_client(packed_mac, sender, &buf, len);
        }

        // 2. Incoming frames: ARP replies for our IP, and the TCP server.
        if packed_mac != syscall_abi::NET_ERROR {
            while let Some(n) = recv(&mut frame) {
                on_frame(&mac, &frame[..n], &mut conn);
            }
        }
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
enum ConnState {
    /// Sent SYN-ACK, waiting for the peer's ACK (or its request data).
    SynRcvd,
    /// Handshake complete, waiting for the HTTP request.
    Established,
    /// Response + our FIN sent, waiting for the peer to finish closing.
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
    /// Our next send sequence number.
    snd_nxt: u32,
    /// The next sequence number we expect from the peer.
    rcv_nxt: u32,
    state: ConnState,
    /// Whether we've already sent the response (guards against a
    /// retransmitted request re-triggering it).
    responded: bool,
}

/// A parsed inbound TCP segment addressed to our listen port.
struct TcpIn {
    src_ip: [u8; 4],
    src_mac: [u8; 6],
    src_port: u16,
    seq: u32,
    flags: u8,
    data_len: u32,
}

/// Dispatch one received frame: answer ARP requests for our IP, and feed TCP
/// segments to the HTTP server. Everything else (including our own client
/// ops' replies, which the synchronous handlers already consumed) is ignored.
fn on_frame(mac: &[u8; 6], frame: &[u8], conn: &mut Option<TcpConn>) {
    if frame.len() < 14 {
        return;
    }
    let ethertype = ((frame[12] as u16) << 8) | frame[13] as u16;
    match ethertype {
        0x0806 => handle_arp(mac, frame),
        0x0800 => {
            if let Some(seg) = parse_tcp_in(frame) {
                handle_tcp(mac, &seg, conn);
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
    let mut src_ip = [0u8; 4];
    src_ip.copy_from_slice(&frame[26..30]);
    let mut src_mac = [0u8; 6];
    src_mac.copy_from_slice(&frame[6..12]);
    Some(TcpIn {
        src_ip,
        src_mac,
        src_port: u16be(frame, t),
        seq: u32be(frame, t + 4),
        flags: frame[t + 13],
        data_len,
    })
}

/// The server TCP state machine for one segment. Minimal but real: SYN ->
/// SYN-ACK, request data -> response + FIN, then ack the peer's FIN and close.
/// One connection at a time; a SYN always (re)starts it.
fn handle_tcp(mac: &[u8; 6], seg: &TcpIn, conn: &mut Option<TcpConn>) {
    // RST tears down a matching connection.
    if seg.flags & TCP_RST != 0 {
        if seg_matches(conn, seg) {
            *conn = None;
        }
        return;
    }

    // SYN (without ACK): start or restart the connection, answer with SYN-ACK.
    if seg.flags & TCP_SYN != 0 && seg.flags & TCP_ACK == 0 {
        let mut c = TcpConn {
            peer_ip: seg.src_ip,
            peer_mac: seg.src_mac,
            peer_port: seg.src_port,
            snd_nxt: SERVER_ISN,
            rcv_nxt: seg.seq.wrapping_add(1), // SYN consumes a sequence number
            state: ConnState::SynRcvd,
            responded: false,
        };
        send_seg(mac, &c, TCP_SYN | TCP_ACK, true, &[]);
        c.snd_nxt = c.snd_nxt.wrapping_add(1); // our SYN consumes one too
        *conn = Some(c);
        return;
    }

    // Everything else must belong to the active connection.
    if !seg_matches(conn, seg) {
        return;
    }
    let Some(c) = conn.as_mut() else { return };

    // A bare ACK completes the handshake.
    if matches!(c.state, ConnState::SynRcvd) && seg.flags & TCP_ACK != 0 {
        c.state = ConnState::Established;
    }

    // The request data (may arrive with the handshake ACK, or just after) ->
    // send the response and immediately FIN. Only once.
    if !c.responded && seg.data_len > 0 && seg.seq == c.rcv_nxt {
        c.rcv_nxt = c.rcv_nxt.wrapping_add(seg.data_len);
        send_seg(mac, c, TCP_PSH | TCP_ACK, false, HTTP_RESPONSE);
        c.snd_nxt = c.snd_nxt.wrapping_add(HTTP_RESPONSE.len() as u32);
        send_seg(mac, c, TCP_FIN | TCP_ACK, false, &[]);
        c.snd_nxt = c.snd_nxt.wrapping_add(1); // FIN consumes a sequence number
        c.responded = true;
        c.state = ConnState::Closing;
    }

    // The peer's FIN: ack it (past any data it carried plus the FIN) and close.
    if seg.flags & TCP_FIN != 0 {
        c.rcv_nxt = seg.seq.wrapping_add(seg.data_len).wrapping_add(1);
        send_seg(mac, c, TCP_ACK, false, &[]);
        *conn = None;
    }
}

/// Whether `seg` belongs to the active connection (same peer address+port).
fn seg_matches(conn: &Option<TcpConn>, seg: &TcpIn) -> bool {
    match conn {
        Some(c) => c.peer_ip == seg.src_ip && c.peer_port == seg.src_port,
        None => false,
    }
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
    // Client direction: our ephemeral source port to the peer's port 80.
    build_tcp_generic(
        mac, dst_mac, dst_ip, TCP_SRC_PORT, 80, seq, ack, flags, with_mss, payload, out,
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
    build_tcp_generic(
        mac, peer_mac, peer_ip, SERVER_PORT, peer_port, seq, ack, flags, with_mss, payload, out,
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
    payload: &[u8],
    out: &mut [u8],
) -> Option<usize> {
    let tcp_hdr = 20 + if with_mss { 4 } else { 0 };
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

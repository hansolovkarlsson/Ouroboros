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
/// A fixed ICMP echo identifier - `netd` has no static state to count from,
/// and one outstanding ping at a time makes a fixed id/seq sufficient.
const ICMP_ID: u16 = 0x4f42; // "OB"
const ICMP_SEQ: u16 = 1;
/// ICMP echo payload length (bytes appended after the 8-byte ICMP header).
const PAYLOAD: usize = 32;

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

/// The request loop: block on `MSG_RECV`, dispatch by op, reply with a
/// single-u64 status. Replying to every message is also what acks the
/// supervisor's health-ping (its reply, addressed to the kernel's sentinel,
/// is intercepted as the ack).
fn serve(packed_mac: u64) -> ! {
    let mut buf = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            continue; // interrupted or error - wait again
        }
        let sender = packed >> 32;
        let len = (packed & 0xffff_ffff) as usize;
        match read_u64(&buf, 0) {
            syscall_abi::NETOP_PING => {
                let status = handle_ping(packed_mac, read_u64(&buf, 8));
                reply(sender, &status.to_le_bytes());
            }
            syscall_abi::NETOP_RESOLVE => {
                // The hostname fills the message after the 8-byte op.
                let end = len.min(buf.len()).max(8);
                let (status, ip) = handle_resolve(packed_mac, &buf[8..end]);
                let mut r = [0u8; 16];
                r[0..8].copy_from_slice(&status.to_le_bytes());
                r[8..16].copy_from_slice(&ip.to_le_bytes());
                reply(sender, &r);
            }
            // Unknown op (including the supervisor's health-ping): any reply
            // acks it. The value is irrelevant to the ping sentinel.
            _ => reply(sender, &0u64.to_le_bytes()),
        }
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
    let mac = unpack_mac(packed_mac);
    let Some(server_mac) = arp_resolve(&mac, &DNS_SERVER) else {
        return (syscall_abi::NET_RESOLVE_TIMEOUT, 0);
    };

    let mut dns = [0u8; 300];
    let Some(dlen) = build_dns_query(host, &mut dns) else {
        return (syscall_abi::NET_RESOLVE_NXDOMAIN, 0);
    };
    let mut frame = [0u8; 400];
    let Some(flen) = build_dns_frame(&mac, &server_mac, &dns[..dlen], &mut frame) else {
        return (syscall_abi::NET_RESOLVE_NXDOMAIN, 0);
    };
    if send(&frame[..flen]).is_err() {
        return (syscall_abi::NET_RESOLVE_TIMEOUT, 0);
    }

    let deadline = now() + 75; // ~1.5s
    let mut rx = [0u8; 1600];
    loop {
        if let Some(len) = recv(&mut rx) {
            if let Some(payload) = dns_payload(&rx[..len]) {
                return match parse_dns_a(payload) {
                    Some(ip) => (
                        syscall_abi::NET_RESOLVE_OK,
                        ip[0] as u64 | (ip[1] as u64) << 8 | (ip[2] as u64) << 16 | (ip[3] as u64) << 24,
                    ),
                    None => (syscall_abi::NET_RESOLVE_NXDOMAIN, 0),
                };
            }
        }
        if now() > deadline {
            return (syscall_abi::NET_RESOLVE_TIMEOUT, 0);
        }
    }
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

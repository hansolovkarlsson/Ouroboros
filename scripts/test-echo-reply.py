#!/usr/bin/env python3
"""`netd` answers an ICMP echo request, and only a good one addressed to it.

Until 2026-11-19 `netd` sent echo requests (`ping`) and answered none, so
nothing could ping a node: not the Mac the Pi is cabled to, not another
node of the cluster. `answer_echo` in `netd` is the reply.

One boot of the FAT32 image, its NIC on a QEMU socket netdev whose other
end is this script: no SLIRP, nothing between the guest and the wire. The
script is the other host on the segment, at 10.0.2.2, the default
gateway's address, and builds and reads every frame itself, its checksums
computed here and not by `netd`'s code: the foreign observer.

The checks:

1. The boot reaches `netd` and a login, and `netd` answers an ARP request
   for its address (the guest's MAC, and the proof the wire works before
   any echo is judged).
2. Echo requests answered, each reply checked whole: Ethernet and IP
   addresses swapped, type 0 code 0, the identifier, sequence number and
   data unchanged, both checksums good by this script's arithmetic, the
   IP total length the request's. Data of 56 bytes, of 37 (an odd length,
   the checksum's last byte alone), of 1472 (a 1500-byte packet), and of 0
   with 30 bytes of junk after the packet (a 72-byte frame), whose reply
   must carry neither the junk nor a checksum over it.
3. Requests that must go unanswered, each watched for `QUIET_S` for any
   ICMP frame from the guest (a reply parsed at the wrong offset carries
   no sequence number where one is looked for): a bad ICMP checksum, a bad IP
   header checksum, an IP header with options, ICMP code 1, an IP total
   length too short for an echo, one longer than the frame (which, taken
   on trust, panicked `netd` on the copy), another host's address, the
   subnet's broadcast address, our address under a broadcast MAC, a
   fragment with more to come and one past the first, an IP source a reply
   cannot go to (255.255.255.255, the subnet's network and broadcast
   addresses, 0.0.0.0, 127.0.0.1, the guest's own), a multicast Ethernet
   source, and an echo reply (type 0).
4. `netd` still answers after them, and answers a burst of `BURST`
   requests sent back to back, every one.
4b. **Answered while `netd` waits for something else.** Each synchronous op
   polls the NIC for its own answer and, before 2026-11-20, dropped every
   other frame. Logged in, the guest's shell starts each op, the script
   sees the op's own frame on the wire (so `netd` is in that loop now) and
   withholds its answer, and sends an echo request and an ARP request into
   the wait; both must be answered: `ping 10.0.2.98` (an ARP resolve no
   one answers), `ping 10.0.2.2` (the echo reply withheld), `resolve
   foo.test` (the DNS answer withheld), and `fetch foo.test` twice over,
   its DNS answered with 10.0.2.2, once at its SYN (the SYN-ACK withheld)
   and once at its request (the response withheld, then a RST).
5. No server called wedged or restarted (a `netd` that panicked on a
   frame is restarted, and its next request may still be answered), and no
   fault line in QEMU's trace.

Controlled 2026-11-19: `main` (`cde340c4`), which answers no echo request,
fails every check in 2 and 4 and passes 3. And `answer_echo` with each of
nineteen guards removed in turn fails the check for that guard and no
other: each checksum, the options test, the code, the type, a total too
short, a total past the frame (`netd` panics on the copy and is restarted),
the length taken from the frame and not the IP header, the fragment test
for more-to-come and for an offset, the destination address and the
broadcast MAC, the Ethernet source, and each of the six source tests
(multicast and above, the subnet's network and broadcast, 0.x, 127.x, our
own). Not reached by any case: the 42-byte floor, which the later checks
mask, and `ECHO_MAX_FRAME`, which no caller's buffer can exceed.

Usage:  python3 scripts/test-echo-reply.py      (make test-echo-reply builds the image)
Exit status: 0, 1 for a failed check, drive-qemu's INCONCLUSIVE for a boot
the firmware never started.
"""
import importlib.util
import os
import queue
import re
import socket
import struct
import sys
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
PORT = 5671
PEER_MAC = bytes.fromhex("525400123502")
PEER_IP = bytes([10, 0, 2, 2])
# netd's DNS server with no /etc/net, answered for ARP like PEER_IP.
DNS_IP = bytes([10, 0, 2, 3])
# An address nobody on the wire answers ARP for.
NOBODY_IP = bytes([10, 0, 2, 98])
PEER_ISN = 0x10000000
OTHER_IP = bytes([10, 0, 2, 99])
BCAST_IP = bytes([10, 0, 2, 255])
# How long a request that must not be answered is watched for a reply.
QUIET_S = 1.5
REPLY_S = 3.0
BURST = 40
ECHO_ID = 0x5151
# IP options whose words sum to zero in one's complement (0x0800 + 0xF7FF),
# so the first 20 bytes' checksum holds too, and whose first byte is 8: a
# parser that took the ICMP message at offset 20 would find an echo
# request with a good checksum there, and answer it.
OPTIONS = bytes.fromhex("0800f7ff")


def csum(data):
    """The Internet checksum (RFC 1071), this script's own."""
    if len(data) % 2:
        data += b"\0"
    s = sum(struct.unpack(f">{len(data) // 2}H", data))
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return ~s & 0xFFFF


class Wire:
    """The socket netdev's far end: QEMU frames each packet with a 4-byte
    big-endian length. Received frames go to a queue; the guest's ARP
    requests for PEER_IP are answered here, so `netd` can reach us."""

    def __init__(self, conn):
        self.conn = conn
        self.frames = queue.Queue()
        self.guest_ip = None
        threading.Thread(target=self._reader, daemon=True).start()

    def send(self, frame):
        self.conn.sendall(struct.pack(">I", len(frame)) + frame)

    def _recv_exact(self, n):
        buf = b""
        while len(buf) < n:
            chunk = self.conn.recv(n - len(buf))
            if not chunk:
                raise EOFError
            buf += chunk
        return buf

    def _reader(self):
        try:
            while True:
                (n,) = struct.unpack(">I", self._recv_exact(4))
                frame = self._recv_exact(n)
                if frame[12:14] == b"\x08\x06" and frame[20:22] == b"\x00\x01" and frame[38:42] in (PEER_IP, DNS_IP):
                    self.send(arp(2, PEER_MAC, frame[38:42], frame[22:28], frame[28:32], frame[22:28]))
                    continue
                self.frames.put(frame)
        except (EOFError, OSError):
            pass

    def drain(self):
        while not self.frames.empty():
            self.frames.get_nowait()

    def wait(self, match, timeout):
        """The first frame `match` accepts within `timeout`, others dropped."""
        end = time.monotonic() + timeout
        while True:
            left = end - time.monotonic()
            if left <= 0:
                return None
            try:
                f = self.frames.get(timeout=left)
            except queue.Empty:
                return None
            if match(f):
                return f


def arp(op, sha, spa, tha, tpa, dst):
    return (dst + PEER_MAC + b"\x08\x06" + b"\x00\x01\x08\x00\x06\x04"
            + struct.pack(">H", op) + sha + spa + tha + tpa)


def echo(guest_mac, guest_ip, seq, data, *, icmp_type=8, code=0, src_ip=PEER_IP, dst_ip=None,
         eth_src=PEER_MAC, eth_dst=None, frag=0, bad_icmp=False, bad_ip=False, pad=b"",
         options=b"", total=None, bare=False):
    """An ICMP echo request frame from the peer, every field overridable.
    `options` (a multiple of 4 bytes) lengthen the IP header; `total`
    replaces the IP total length, the header's checksum still good.
    `bare` is an ICMP message of its 4-byte header alone, checksum good,
    `data` and `pad` then trailing it outside the packet."""
    if bare:
        icmp = struct.pack(">BBH", icmp_type, code, 0)
    else:
        icmp = struct.pack(">BBHHH", icmp_type, code, 0, ECHO_ID, seq) + data
    c = csum(icmp) ^ (0x0101 if bad_icmp else 0)
    icmp = icmp[:2] + struct.pack(">H", c) + icmp[4:]
    hlen = 20 + len(options)
    ip = struct.pack(">BBHHHBBH4s4s", 0x40 | hlen // 4, 0, total or hlen + len(icmp), 0x1234, frag, 64, 1, 0,
                     src_ip, dst_ip or guest_ip) + options
    c = csum(ip) ^ (0x0101 if bad_ip else 0)
    ip = ip[:10] + struct.pack(">H", c) + ip[12:]
    return (eth_dst or guest_mac) + eth_src + b"\x08\x00" + ip + icmp + (data if bare else b"") + pad


def is_icmp_from_guest(f):
    # 34 bytes, not 42: a reply to the short-total case would be 38.
    return len(f) >= 34 and f[12:14] == b"\x08\x00" and f[23] == 1


def judge_reply(req, rep, guest_mac, guest_ip):
    """'' when `rep` is `req`'s echo reply in every field, else what is wrong."""
    req_total = struct.unpack(">H", req[16:18])[0]
    if rep[0:6] != PEER_MAC or rep[6:12] != guest_mac:
        return f"Ethernet addresses {rep[0:6].hex()} <- {rep[6:12].hex()}"
    if rep[14] != 0x45:
        return f"IP version/IHL byte {rep[14]:#x}"
    total = struct.unpack(">H", rep[16:18])[0]
    if total != req_total:
        return f"IP total length {total}, the request's {req_total}"
    if len(rep) < 14 + total:
        return f"frame of {len(rep)} bytes, shorter than its IP length {total}"
    if len(rep) > 14 + total and len(rep) > 60:
        return f"frame of {len(rep)} bytes carries {len(rep) - 14 - total} past its IP length"
    if csum(rep[14:34]) != 0:
        return "IP header checksum"
    if rep[22] == 0:
        return "TTL 0"
    if rep[26:30] != guest_ip or rep[30:34] != PEER_IP:
        return f"IP addresses {rep[26:30].hex()} -> {rep[30:34].hex()}"
    if rep[34] != 0 or rep[35] != 0:
        return f"ICMP type {rep[34]} code {rep[35]}"
    if csum(rep[34:14 + total]) != 0:
        return "ICMP checksum"
    if rep[38:14 + total] != req[38:14 + req_total]:
        return "identifier, sequence or data changed"
    return ""


def tcp_frame(guest_mac, guest_ip, sport, dport, seq, ack, flags):
    """A TCP segment with no data from PEER_IP:sport to the guest, its
    checksum over the pseudo-header computed here."""
    tcp = struct.pack(">HHIIBBHHH", sport, dport, seq, ack, 5 << 4, flags, 8192, 0, 0)
    pseudo = PEER_IP + guest_ip + struct.pack(">BBH", 0, 6, len(tcp))
    tcp = tcp[:16] + struct.pack(">H", csum(pseudo + tcp)) + tcp[18:]
    ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(tcp), 0, 0, 64, 6, 0, PEER_IP, guest_ip)
    ip = ip[:10] + struct.pack(">H", csum(ip)) + ip[12:]
    return guest_mac + PEER_MAC + b"\x08\x00" + ip + tcp


def dns_answer(guest_mac, guest_ip, query, addr):
    """The guest's DNS `query` frame answered with one A record, `addr`."""
    ihl = (query[14] & 0x0F) * 4
    udp = 14 + ihl
    sport = struct.unpack(">H", query[udp:udp + 2])[0]
    q = query[udp + 8:14 + struct.unpack(">H", query[16:18])[0]]
    dns = q[0:2] + b"\x81\x80" + b"\x00\x01\x00\x01\x00\x00\x00\x00" + q[12:]
    dns += b"\xc0\x0c" + struct.pack(">HHIH", 1, 1, 60, 4) + addr
    u = struct.pack(">HHHH", 53, sport, 8 + len(dns), 0) + dns
    ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(u), 0, 0, 64, 17, 0, DNS_IP, guest_ip)
    ip = ip[:10] + struct.pack(">H", csum(ip)) + ip[12:]
    return guest_mac + PEER_MAC + b"\x08\x00" + ip + u


def guest_tcp(f):
    """(sport, dport, seq, flags, data length) of a TCP frame from the guest, or None."""
    if len(f) < 34 or f[12:14] != b"\x08\x00" or f[23] != 6:
        return None
    ihl = (f[14] & 0x0F) * 4
    t = 14 + ihl
    total = struct.unpack(">H", f[16:18])[0]
    doff = (f[t + 12] >> 4) * 4
    sport, dport, seq = struct.unpack(">HHI", f[t:t + 8])
    return sport, dport, seq, f[t + 13], total - ihl - doff


def main():
    checks = []

    def check(ok, what):
        checks.append((ok, what))
        print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)

    lst = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lst.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        lst.bind(("127.0.0.1", PORT))
    except OSError as err:
        print(f"[FAIL] the rig's port 127.0.0.1:{PORT} is free ({err})", flush=True)
        return drive_qemu.verdict("test-echo-reply", 1, 0)
    lst.listen(1)
    lst.settimeout(30)
    g = drive_qemu.Guest(IMAGE, extra_args=(
        "-netdev", f"socket,id=net0,connect=127.0.0.1:{PORT}",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[echo-reply] ")
    booted = False
    try:
        conn, _ = lst.accept()
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        wire = Wire(conn)
        deadline = time.time() + 120
        while time.time() < deadline and not all(x in g.transcript() for x in ["netd: network server ready", "login:"]):
            time.sleep(0.2)
        text = g.transcript()
        m = re.search(r"netd: address (\d+)\.(\d+)\.(\d+)\.(\d+)/", text)
        booted = "netd: network server ready" in text and "login:" in text and m is not None
        if booted:
            check(True, "the boot reaches netd and a login")
            guest_ip = bytes(int(x) for x in m.groups())
            mark = len(g.transcript())
            wire.drain()
            wire.send(arp(1, PEER_MAC, PEER_IP, b"\0" * 6, guest_ip, b"\xff" * 6))
            a = wire.wait(lambda f: f[12:14] == b"\x08\x06" and f[20:22] == b"\x00\x02" and f[28:32] == guest_ip, REPLY_S)
            check(a is not None, f"netd answers an ARP request for {'.'.join(map(str, guest_ip))}")
            guest_mac = a[22:28] if a else b"\xff" * 6

            def answered(req, what):
                wire.drain()
                wire.send(req)
                seq = req[40:42]
                rep = wire.wait(lambda f: is_icmp_from_guest(f) and f[34] == 0 and f[40:42] == seq, REPLY_S)
                if rep is None:
                    check(False, f"{what}: answered (no reply in {REPLY_S:.0f} s)")
                    return
                wrong = judge_reply(req, rep, guest_mac, guest_ip)
                check(not wrong, f"{what}: answered, the reply right in every field" + (f" (wrong: {wrong})" if wrong else ""))

            def unanswered(req, what):
                wire.drain()
                wire.send(req)
                # Any ICMP frame, not this request's sequence number: a reply
                # built at the wrong offset (the options case) carries none
                # where it would be looked for. Every request before this one
                # was answered or not before it was sent, so none is late.
                rep = wire.wait(is_icmp_from_guest, QUIET_S)
                check(rep is None, f"{what}: not answered" + (f" (got {rep.hex()})" if rep else ""))

            if a:
                e = lambda seq, data=b"", **kw: echo(guest_mac, guest_ip, seq, data, **kw)
                answered(e(1, bytes(range(56))), "a request with 56 bytes of data")
                answered(e(2, bytes(range(37))), "a request with 37 bytes of data (an odd length)")
                answered(e(3, bytes(i & 0xFF for i in range(1472))), "a request with 1472 bytes of data (a 1500-byte packet)")
                answered(e(4, pad=b"\xa5" * 30), "a request with no data and 30 bytes of junk after it (a 72-byte frame)")

                unanswered(e(10, b"x" * 8, bad_icmp=True), "a request whose ICMP checksum is wrong")
                unanswered(e(11, b"x" * 8, bad_ip=True), "a request whose IP header checksum is wrong")
                unanswered(e(23, b"x" * 8, options=OPTIONS), "a request whose IP header has options")
                unanswered(e(24, b"x" * 8, code=1), "a request with ICMP code 1")
                unanswered(e(25, b"x" * 8, total=24, bare=True), "a request whose IP total length is too short for an echo")
                unanswered(e(26, b"x" * 8, total=1400), "a request whose IP total length is longer than its frame")
                unanswered(e(12, b"x" * 8, dst_ip=OTHER_IP), "a request for another host's address")
                unanswered(e(27, b"x" * 8, eth_dst=b"\xff" * 6), "a request for our address under a broadcast MAC")
                unanswered(e(13, b"x" * 8, dst_ip=BCAST_IP, eth_dst=b"\xff" * 6), "a request to the subnet's broadcast address")
                unanswered(e(14, b"x" * 8, frag=0x2000), "a request that is a fragment (more fragments set)")
                unanswered(e(28, b"x" * 8, frag=0x0001), "a request that is a fragment (an offset past the first)")
                unanswered(e(15, b"x" * 8, src_ip=b"\xff\xff\xff\xff"), "a request from the broadcast address")
                unanswered(e(18, b"x" * 8, src_ip=BCAST_IP), "a request from the subnet's broadcast address")
                unanswered(e(29, b"x" * 8, src_ip=bytes([10, 0, 2, 0])), "a request from the subnet's network address")
                unanswered(e(19, b"x" * 8, src_ip=bytes(4)), "a request from 0.0.0.0")
                unanswered(e(21, b"x" * 8, src_ip=bytes([127, 0, 0, 1])), "a request from 127.0.0.1")
                unanswered(e(22, b"x" * 8, src_ip=guest_ip), "a request from the guest's own address")
                unanswered(e(16, b"x" * 8, eth_src=bytes.fromhex("010203040506")), "a request from a multicast Ethernet address")
                unanswered(e(17, b"x" * 8, icmp_type=0), "an echo reply nobody asked for")

                answered(e(20, b"after"), "a request after those")
                wire.drain()
                for i in range(BURST):
                    wire.send(e(100 + i, b"burst"))
                got = set()
                end = time.monotonic() + REPLY_S * 2
                while len(got) < BURST and time.monotonic() < end:
                    f = wire.wait(is_icmp_from_guest, max(0.01, end - time.monotonic()))
                    if f and f[34] == 0:
                        got.add(struct.unpack(">H", f[40:42])[0])
                check(len(got) == BURST, f"a burst of {BURST} requests sent back to back: every one answered ({len(got)})")

                # 4b: requests sent while netd polls for something else.
                logged_in = g.run([("login:", "root"), ("assword", "root"), ("# ", "")])
                check(logged_in, "the guest's shell logs in")
                probe_seq = [200]

                def probe(what):
                    """An echo request and an ARP request into netd's wait, now; both
                    must be answered."""
                    probe_seq[0] += 1
                    seq = struct.pack(">H", probe_seq[0])
                    wire.send(e(probe_seq[0], b"waiting"))
                    wire.send(arp(1, PEER_MAC, PEER_IP, b"\0" * 6, guest_ip, b"\xff" * 6))
                    want = {"echo", "arp"}
                    end = time.monotonic() + REPLY_S
                    while want and time.monotonic() < end:
                        f = wire.wait(lambda f: True, max(0.01, end - time.monotonic()))
                        if f is None:
                            break
                        if is_icmp_from_guest(f) and f[34] == 0 and f[40:42] == seq:
                            want.discard("echo")
                        elif f[12:14] == b"\x08\x06" and f[20:22] == b"\x00\x02" and f[28:32] == guest_ip:
                            want.discard("arp")
                    check(not want, f"while netd waits on {what}: an echo request and an ARP request both answered"
                          + (f" (not answered: {', '.join(sorted(want))})" if want else ""))

                def start(command, seen, what):
                    """Type `command` and wait for its op's frame (`seen`) on the wire."""
                    wire.drain()
                    g.type_line(command)
                    f = wire.wait(seen, 15)
                    check(f is not None, f"`{command}` puts {what} on the wire")
                    return f

                def back_at_prompt():
                    g.wait_for("# ", timeout=20)

                is_arp_for = lambda ip: (lambda f: f[12:14] == b"\x08\x06" and f[20:22] == b"\x00\x01" and f[38:42] == ip)
                if logged_in:
                    if start("ping 10.0.2.98", is_arp_for(NOBODY_IP), "an ARP request for 10.0.2.98"):
                        probe("an ARP reply (arp_resolve)")
                    back_at_prompt()
                    if start("ping 10.0.2.2", lambda f: is_icmp_from_guest(f) and f[34] == 8 and f[30:34] == PEER_IP, "an echo request to 10.0.2.2"):
                        probe("an echo reply (icmp_echo)")
                    back_at_prompt()
                    is_dns = lambda f: len(f) >= 42 and f[12:14] == b"\x08\x00" and f[23] == 17 and f[30:34] == DNS_IP
                    if start("resolve foo.test", is_dns, "a DNS query"):
                        probe("a DNS answer (resolve_ip)")
                    back_at_prompt()
                    q = start("fetch foo.test", is_dns, "a DNS query")
                    if q:
                        wire.send(dns_answer(guest_mac, guest_ip, q, PEER_IP))
                        syn = wire.wait(lambda f: (t := guest_tcp(f)) is not None and t[1] == 80 and t[3] & 0x02, 15)
                        check(syn is not None, "`fetch foo.test`, answered 10.0.2.2, sends a SYN to port 80")
                        if syn:
                            probe("a SYN-ACK (tcp_get's handshake)")
                            sport, _, their, _, _ = guest_tcp(syn)
                            wire.drain()
                            wire.send(tcp_frame(guest_mac, guest_ip, 80, sport, PEER_ISN, their + 1, 0x12))
                            req = wire.wait(lambda f: (t := guest_tcp(f)) is not None and t[0] == sport and t[4] > 0, 15)
                            check(req is not None, "`fetch` sends its request after the SYN-ACK")
                            if req:
                                probe("the response (tcp_get's data loop)")
                                _, _, rseq, _, rlen = guest_tcp(req)
                                wire.send(tcp_frame(guest_mac, guest_ip, 80, sport, PEER_ISN + 1, rseq + rlen, 0x14))
                    back_at_prompt()
            during = g.transcript()[mark:]
            wedged, restarted = during.count("wedged"), during.count("restarted")
            check(wedged == 0 and restarted == 0, f"no server called wedged or restarted ({wedged} wedged, {restarted} restarted)")
            check(g.proc.poll() is None, "the guest is alive at the end")
        out = g.transcript()
        faults = g.aborts()
    except socket.timeout:
        out, faults = g.transcript(), g.aborts()
        print("[echo-reply] QEMU never connected to the socket netdev", flush=True)
    finally:
        g.stop()
        lst.close()
    if drive_qemu.boot_stalled("test-echo-reply", "boot", out, faults):
        return drive_qemu.verdict("test-echo-reply", 0, 1)
    if not booted:
        check(False, "the boot reaches netd and a login")
    check(faults == 0, f"no fault lines in QEMU's trace ({faults})")
    return drive_qemu.verdict("test-echo-reply", sum(1 for ok, _ in checks if not ok), 0)


if __name__ == "__main__":
    sys.exit(main())

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
   with the frame padded to 60 bytes by junk, whose reply must carry
   neither the padding nor a checksum over it.
3. Requests that must go unanswered, each watched for `QUIET_S`: a bad
   ICMP checksum, a bad IP header checksum, another host's address, the
   subnet's broadcast address, a fragment, a broadcast IP source, a
   multicast Ethernet source, and an echo reply (type 0).
4. `netd` still answers after them, and answers a burst of `BURST`
   requests sent back to back, every one.
5. No server called wedged or restarted, and no fault line in QEMU's trace.

Controlled 2026-11-19: `main` (`cde340c4`), which answers no echo request,
fails every check in 2 and 4 and passes 3.

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
OTHER_IP = bytes([10, 0, 2, 99])
BCAST_IP = bytes([10, 0, 2, 255])
# How long a request that must not be answered is watched for a reply.
QUIET_S = 1.5
REPLY_S = 3.0
BURST = 40
ECHO_ID = 0x5151


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
                if frame[12:14] == b"\x08\x06" and frame[20:22] == b"\x00\x01" and frame[38:42] == PEER_IP:
                    self.send(arp(2, PEER_MAC, PEER_IP, frame[22:28], frame[28:32], frame[22:28]))
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


def echo(guest_mac, guest_ip, seq, data, *, icmp_type=8, src_ip=PEER_IP, dst_ip=None,
         eth_src=PEER_MAC, eth_dst=None, frag=0, bad_icmp=False, bad_ip=False, pad=b""):
    """An ICMP echo request frame from the peer, every field overridable."""
    icmp = struct.pack(">BBHHH", icmp_type, 0, 0, ECHO_ID, seq) + data
    c = csum(icmp) ^ (0x0101 if bad_icmp else 0)
    icmp = icmp[:2] + struct.pack(">H", c) + icmp[4:]
    ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(icmp), 0x1234, frag, 64, 1, 0,
                     src_ip, dst_ip or guest_ip)
    c = csum(ip) ^ (0x0101 if bad_ip else 0)
    ip = ip[:10] + struct.pack(">H", c) + ip[12:]
    return (eth_dst or guest_mac) + eth_src + b"\x08\x00" + ip + icmp + pad


def is_icmp_from_guest(f):
    return len(f) >= 42 and f[12:14] == b"\x08\x00" and f[23] == 1


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


def main():
    checks = []

    def check(ok, what):
        checks.append((ok, what))
        print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)

    lst = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lst.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    lst.bind(("127.0.0.1", PORT))
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
                rep = wire.wait(is_icmp_from_guest, QUIET_S)
                check(rep is None, f"{what}: not answered" + (f" (got {rep.hex()})" if rep else ""))

            if a:
                e = lambda seq, data=b"", **kw: echo(guest_mac, guest_ip, seq, data, **kw)
                answered(e(1, bytes(range(56))), "a request with 56 bytes of data")
                answered(e(2, bytes(range(37))), "a request with 37 bytes of data (an odd length)")
                answered(e(3, bytes(i & 0xFF for i in range(1472))), "a request with 1472 bytes of data (a 1500-byte packet)")
                answered(e(4, pad=b"\xa5" * 18), "a request with no data, its frame padded to 60 bytes with junk")

                unanswered(e(10, b"x" * 8, bad_icmp=True), "a request whose ICMP checksum is wrong")
                unanswered(e(11, b"x" * 8, bad_ip=True), "a request whose IP header checksum is wrong")
                unanswered(e(12, b"x" * 8, dst_ip=OTHER_IP), "a request for another host's address")
                unanswered(e(13, b"x" * 8, dst_ip=BCAST_IP, eth_dst=b"\xff" * 6), "a request to the subnet's broadcast address")
                unanswered(e(14, b"x" * 8, frag=0x2000), "a request that is a fragment (more fragments set)")
                unanswered(e(15, b"x" * 8, src_ip=b"\xff\xff\xff\xff"), "a request from the broadcast address")
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

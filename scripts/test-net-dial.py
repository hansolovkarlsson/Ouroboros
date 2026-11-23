#!/usr/bin/env python3
"""A `/net/tcp` `connect` on the wire, and the ones `netd` must refuse.

Until 2026-11-22 no rig sent a `connect` to `/net/tcp/N/ctl`: the dial-out
side of `netd` (`dial_file_op`, `parse_connect`, the dial engine) was read
and never run, so the address and port it parses (`netaddr::parse_ip`,
`accounts::parse_dec`, #490) were graded on the host alone.

One boot of the FAT32 image, its NIC on a QEMU socket netdev whose other
end is this script, at 10.0.2.2: the peer `dial` connects to, which builds
and reads every frame itself (the foreign observer). The guest's shell runs
`dial /net <ip> <port> <words>`, which reads `/net/tcp/clone`, writes
`connect ip!port` to the connection's `ctl`, waits for `Established`,
writes the words to `data` and prints what comes back until the peer
closes.

The checks:

1. The boot reaches `netd` and a login, and `mount -n /net` binds `/net`.
2. **A connect.** `dial /net 10.0.2.2 8000 hello`: the guest's SYN goes to
   10.0.2.2 port 8000 exactly; the script answers it, and the request
   arrives whole (`hello` and the CRLF CRLF `dial` ends it with); the
   script's answer, sent with a FIN, is printed by `dial`, and the prompt
   comes back.
3. **Refused, with nothing on the wire.** Each of these `connect`s is
   answered `dial: connect refused`, and no SYN leaves the guest in
   `QUIET_S`: a port past 65535 (65536, 99999), a port that is not a
   number (`80x`), an address part past 255 (`256.0.2.2`), one of four
   digits (`0010.0.2.2`, refused since #490), three parts and five. A
   20-digit port, which wrapped to 80 before #490, does not fit `dial`'s
   8-byte port argument and is not sent: `accounts`'s host test holds it.
4. **Unreachable.** `dial /net 10.0.2.98 8000`: an ARP request for
   10.0.2.98, which nobody answers, no SYN, and `dial` says it was refused.
   Run before the refusals of 3: its ARP wait is the slowest, and 5's
   control needs the last three failures quick.
5. **The slots come back.** After those eight refusals, more than
   `MAX_DIAL` (3) connections' worth, a second good `dial` (port 8001)
   connects and is answered as in 2.
   Only while the last three failures and that clone fall within
   `LEAK_WINDOW_S` of each other, or a leaking `dial`'s slots would be
   idle-reaped in time: a slower run is INCONCLUSIVE.
6. **A closed slot is its client's until it closes it.** `dialprobe
   10.0.2.98!8000`, in one process: clone slot A, `connect` to its `ctl`
   fails (ARP unanswered, netd sets A `Closed`, `NO_FS`), a second
   `connect` to it is refused (`FS_ERROR`: a slot connects once, from
   `Idle`), and the next clone must not be A; after `close` to A's `ctl`,
   the next clone is A again. Before
   the review of #504 netd freed A on `Closed`, so a client's late `close`
   could shut the slot's next owner.
7. **A client that closes before `listen` is read is still handed out.**
   `dialprobe accept 9200 3000` announces and sleeps before reading
   `listen`; in that time the script connects and sends its request with a
   FIN. The read must hand the connection out, request whole: before the
   third review of #504 netd freed it on `Closed` with the request unread.
   While the probe holds that connection `Closed`, the script resends its
   first SYN: it must get no SYN-ACK (before the fifth review that late
   copy was accepted, a connection with no client). Then the client
   reconnects from
   the same port, and the probe's second `listen` read must hand the new
   one out: before the fourth review a bare SYN on a `Closed` slot's
   4-tuple was used up there. Each client's FIN must reach netd before the
   `listen` read it is about (netd's own FIN, sent on taking it, shows it),
   or the run is INCONCLUSIVE.
8. **`serve` gives its listener back.** `MAX_DIAL` (3) runs of `serve`
   that nobody connects to each time out, and a third good `dial` (port
   8002) after them connects: a `Listening` slot is never idle-reaped, so
   before 2026-11-23, when `serve` closed it only on success, the three held
   every slot for the rest of the boot.
9. No server called wedged or restarted, and no fault line in QEMU's
   trace.

Exit status: 0, 1 for a failed check, drive-qemu's INCONCLUSIVE for a boot
the firmware never started or a run too slow for check 5's control.
"""
import importlib.util
import os
import re
import socket
import struct
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


drive_qemu = load("drive_qemu", "drive-qemu.py")
# The socket netdev's far end, its ARP answers and the frame helpers are
# test-echo-reply's, taken from it rather than copied.
echo_reply = load("echo_reply", "test-echo-reply.py")
Wire, arp, csum = echo_reply.Wire, echo_reply.arp, echo_reply.csum
PEER_MAC, PEER_IP, NOBODY_IP = echo_reply.PEER_MAC, echo_reply.PEER_IP, echo_reply.NOBODY_IP
QUIET_S = echo_reply.QUIET_S

RIG = "test-net-dial"
IMAGE = os.path.join(os.path.dirname(HERE), "build", "esp.img")
PORT = 5672
# netd idle-reaps a slot DIAL_IDLE_TICKS (600 ticks, 12 s) after its last
# activity. The last three failed dials must clone within this of the second
# good dial's clone for a leak to hold all MAX_DIAL slots there. The window is
# timed from before the first of the three is typed to the good dial's SYN on
# the wire, so it holds both clones and overstates the time between them by
# the first one's typing; 1 s of margin below the 12.
LEAK_WINDOW_S = 11
# The client the early-close check opens, what it sends with its FIN, and how
# long dialprobe waits before its `listen` read: time for the handshake, the
# request and the pump passes that freed the connection before the fix.
CLIENT_EARLY = 43010
EARLY_REQUEST = b"early-request\r\n"
AGAIN_REQUEST = b"again-request\r\n"
EARLY_WAIT_MS = 3000
# netd's /net/tcp slots, and how long a `serve` nobody connects to may take to
# give up (its 6000 listen polls).
MAX_DIAL = 3
SERVE_TIMEOUT_S = 120
PEER_ISN = 0x20000000
REQUEST = b"hello\r\n\r\n"
ANSWER = b"dial-answer-%d\n"
SYN, ACK, FIN, PSH, RST = 0x02, 0x10, 0x01, 0x08, 0x04


def segment(guest_mac, guest_ip, sport, dport, seq, ack, flags, data=b""):
    """A TCP segment from PEER_IP:sport to the guest carrying `data`, its
    checksum over the pseudo-header computed here."""
    tcp = struct.pack(">HHIIBBHHH", sport, dport, seq, ack, 5 << 4, flags, 8192, 0, 0) + data
    pseudo = PEER_IP + guest_ip + struct.pack(">BBH", 0, 6, len(tcp))
    tcp = tcp[:16] + struct.pack(">H", csum(pseudo + tcp)) + tcp[18:]
    ip = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(tcp), 0, 0, 64, 6, 0, PEER_IP, guest_ip)
    ip = ip[:10] + struct.pack(">H", csum(ip)) + ip[12:]
    return guest_mac + PEER_MAC + b"\x08\x00" + ip + tcp


def tcp_of(f):
    """(dst ip, sport, dport, seq, ack, flags, payload) of a TCP frame from
    the guest, or None."""
    if len(f) < 54 or f[12:14] != b"\x08\x00" or f[23] != 6:
        return None
    ihl = (f[14] & 0x0F) * 4
    t = 14 + ihl
    total = struct.unpack(">H", f[16:18])[0]
    doff = (f[t + 12] >> 4) * 4
    sport, dport, seq, ack = struct.unpack(">HHII", f[t:t + 12])
    return f[30:34], sport, dport, seq, ack, f[t + 13], f[t + doff:14 + total]


def said(out):
    """The guest's lines in `out` after the command line itself, the
    escape sequences gone, for a failure's message."""
    lines = [ln.strip() for ln in re.sub(r"\x1b\[[0-9;?=]*[A-Za-z]", "", out).splitlines()]
    return " / ".join(ln for ln in lines[1:] if ln and ln != "#")[:200]


def is_syn(f):
    t = tcp_of(f)
    return t is not None and t[5] & (SYN | ACK) == SYN


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
        return drive_qemu.verdict(RIG, 1, 0)
    lst.listen(1)
    lst.settimeout(30)
    g = drive_qemu.Guest(IMAGE, extra_args=(
        "-netdev", f"socket,id=net0,connect=127.0.0.1:{PORT}",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[net-dial] ")
    booted = False
    slow = None
    unforced = []
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
            a = wire.wait(lambda f: f[12:14] == b"\x08\x06" and f[20:22] == b"\x00\x02" and f[28:32] == guest_ip, 3.0)
            guest_mac = a[22:28] if a else b"\xff" * 6
            logged_in = a is not None and g.run([("login:", "root"), ("assword", "root"), ("# ", "")])
            check(logged_in, "netd answers ARP and the guest's shell logs in")
            if logged_in:
                # `/net` is bound by hand, as the manual has it (`mount -n /net`).
                g.type_line("mount -n /net")
                logged_in = g.wait_for("# ", timeout=20)
                check(logged_in, "`mount -n /net` binds this machine's /net")

            def prompt(command):
                check(g.wait_for("# ", timeout=20), f"`{command}` ends and the shell prompts again")

            def good_dial(port, n, window=None):
                """`dial` to the peer's `port`, served by this script: SYN-ACK,
                the request read, the answer sent with a FIN. With `window`,
                its "end" is the moment the SYN is seen: the clone is done by
                then."""
                command = f"dial /net 10.0.2.2 {port} hello"
                wire.drain()
                before = len(g.transcript())
                g.type_line(command)
                syn = wire.wait(is_syn, 15)
                if window is not None:
                    window["end"] = time.monotonic()
                t = tcp_of(syn) if syn else None
                check(t is not None and t[0] == PEER_IP and t[2] == port,
                      f"`{command}`: the guest's SYN goes to 10.0.2.2 port {port}"
                      + (f" (went to {'.'.join(map(str, t[0]))} port {t[2]})" if t and (t[0] != PEER_IP or t[2] != port)
                         else "" if t else " (no SYN)"))
                if t is None or t[2] != port:
                    prompt(command)
                    print(f"      it said: {said(g.transcript()[before:])}", flush=True)
                    return
                gport, their = t[1], t[3]
                wire.send(segment(guest_mac, guest_ip, port, gport, PEER_ISN, their + 1, SYN | ACK))
                got, end = b"", time.monotonic() + 15
                while len(got) < len(REQUEST) and time.monotonic() < end:
                    f = wire.wait(lambda f: (x := tcp_of(f)) is not None and x[1] == gport and len(x[6]) > 0,
                                  max(0.01, end - time.monotonic()))
                    if f is None:
                        break
                    got += tcp_of(f)[6]
                check(got == REQUEST, f"`{command}`: the request arrives whole ({got!r})")
                answer = ANSWER % n
                wire.send(segment(guest_mac, guest_ip, port, gport, PEER_ISN + 1, their + 1 + len(got),
                                  PSH | ACK | FIN, answer))
                prompt(command)
                out = g.transcript()[before:]
                check(answer.decode().strip() in out, f"`{command}`: dial prints the peer's answer")
                # The guest's FIN, answered, so the slot closes cleanly.
                fin = wire.wait(lambda f: (x := tcp_of(f)) is not None and x[1] == gport and x[5] & FIN, 3.0)
                if fin:
                    x = tcp_of(fin)
                    wire.send(segment(guest_mac, guest_ip, port, gport, PEER_ISN + 2 + len(answer), x[3] + 1, ACK))

            def refused(ip, port, what):
                """`dial` whose `connect` netd must refuse before anything goes
                on the wire."""
                command = f"dial /net {ip} {port}"
                wire.drain()
                before = len(g.transcript())
                g.type_line(command)
                prompt(command)
                out = g.transcript()[before:]
                syn = wire.wait(is_syn, QUIET_S)
                check("dial: connect refused" in out and syn is None,
                      f"`{command}` ({what}): refused, and no SYN on the wire"
                      + ("" if "dial: connect refused" in out else f" (not refused; it said: {said(out)})")
                      + (f" (a SYN to port {tcp_of(syn)[2]})" if syn else ""))

            def run(command):
                """Type `command` and return what the guest said before the prompt."""
                before = len(g.transcript())
                g.type_line(command)
                prompt(command)
                return g.transcript()[before:]

            if logged_in:
                good_dial(8000, 1)
                # The leak control below needs the last three failed dials to
                # hold their slots together when good_dial(8001) clones: under
                # netd's DIAL_IDLE_TICKS (12 s) of each other, or a leaking
                # dial's slots are reaped in time and the control cannot fail.
                window = {}
                # Unreachable: the ARP request goes out, nobody answers it.
                command = "dial /net 10.0.2.98 8000"
                wire.drain()
                before = len(g.transcript())
                g.type_line(command)
                asked = wire.wait(lambda f: f[12:14] == b"\x08\x06" and f[20:22] == b"\x00\x01" and f[38:42] == NOBODY_IP, 10)
                prompt(command)
                out = g.transcript()[before:]
                syn = wire.wait(is_syn, QUIET_S)
                check(asked is not None and syn is None and "dial: connect refused" in out,
                      f"`{command}` (nobody answers ARP): an ARP request, no SYN, and refused"
                      + ("" if asked else " (no ARP request)") + (" (a SYN)" if syn else "")
                      + ("" if "dial: connect refused" in out else f" (not refused; it said: {said(out)})"))
                refused("10.0.2.2", "65536", "a port past 65535")
                refused("10.0.2.2", "99999", "a port of five digits past 65535")
                refused("10.0.2.2", "80x", "a port that is not a number")
                refused("256.0.2.2", "8000", "an address part past 255")
                window["start"] = time.monotonic()
                refused("0010.0.2.2", "8000", "an address part of four digits")
                refused("10.0.2", "8000", "an address of three parts")
                refused("10.0.2.2.5", "8000", "an address of five parts")
                good_dial(8001, 2, window)
                slow = window.get("end", time.monotonic()) - window["start"]
                print(f"[net-dial] the last three failed dials took {slow:.1f} s (the window is {LEAK_WINDOW_S} s)", flush=True)
                # A slot netd has closed is held until its client writes
                # `close`: the review of #504 found that freeing it on `Closed`
                # alone let the next clone reuse it, and the first client's
                # late `close` then shut the new connection.
                out = run("dialprobe 10.0.2.98!8000")
                m = re.search(r"dialprobe: (\S) connect (\S+) again (\S+) (\S) (\S)", out)
                a, status, again, b, c = m.groups() if m else ("?",) * 5
                check(m is not None and a.isdigit() and status == "NO_FS" and b.isdigit() and b != a,
                      f"a slot netd closed (connect to 10.0.2.98, unanswered ARP) is not cloned again before its client closes it"
                      + (f" (slot {a}, connect answered {status}, then slot {b})" if m else f" (it said: {said(out)})"))
                check(m is not None and again == "FS_ERROR",
                      f"a second `connect` on that closed slot is refused at once (FS_ERROR, answered {again})")
                check(m is not None and c == a, f"its client's `close` frees it for the next clone (slot {a}, then {c})")
                # A client that connects, sends its request and closes before
                # `listen` is read is still handed out, request and all: the
                # third review of #504 found netd freeing such a connection on
                # `Closed` with the request unread. Then, while dialprobe holds
                # that connection Closed, the client reconnects from the same
                # port: the fourth review found its SYN used up by the Closed
                # slot and never accepted.
                command = f"dialprobe accept 9200 {EARLY_WAIT_MS}"
                wire.drain()
                before = len(g.transcript())
                g.type_line(command)
                listening = g.wait_for("dialprobe: listening", timeout=20)

                def connect_early(isn, request):
                    """SYN, then the request with a FIN once the SYN-ACK is in:
                    the SYN-ACK, and the guest's FIN when it came before
                    dialprobe's next `listen` read printed."""
                    wire.send(segment(guest_mac, guest_ip, CLIENT_EARLY, 9200, isn, 0, SYN))
                    sa = wire.wait(lambda f: (x := tcp_of(f)) is not None and x[1] == 9200 and x[2] == CLIENT_EARLY
                                   and x[5] & (SYN | ACK) == SYN | ACK, 1.5)
                    if not sa:
                        return None, None
                    sa = tcp_of(sa)
                    wire.send(segment(guest_mac, guest_ip, CLIENT_EARLY, 9200, isn + 1, sa[3] + 1, PSH | ACK | FIN, request))
                    shown = len(g.transcript())
                    fin = wire.wait(lambda f: (x := tcp_of(f)) is not None and x[1] == 9200 and x[2] == CLIENT_EARLY
                                    and x[5] & FIN, 1.5)
                    if fin:
                        x = tcp_of(fin)
                        wire.send(segment(guest_mac, guest_ip, CLIENT_EARLY, 9200, isn + 2 + len(request), x[3] + 1, ACK))
                    # netd sends its FIN on taking the client's, so a FIN seen
                    # before the probe printed its next line was taken before
                    # the `listen` read: what each check is about.
                    in_time = fin is not None and "dialprobe: " not in g.transcript()[shown:]
                    return sa, in_time

                early, early_in_time = connect_early(PEER_ISN, EARLY_REQUEST) if listening else (None, None)
                first = g.wait_for("dialprobe: accepted", timeout=10)
                # A late copy of the first connection's SYN, while that
                # connection is held Closed, is not a new one: the fifth
                # review of #504 found it accepted, a connection with no
                # client behind it holding a slot.
                if first:
                    wire.send(segment(guest_mac, guest_ip, CLIENT_EARLY, 9200, PEER_ISN, 0, SYN))
                    stale = wire.wait(lambda f: (x := tcp_of(f)) is not None and x[1] == 9200 and x[2] == CLIENT_EARLY
                                      and x[5] & (SYN | ACK) == SYN | ACK, 1.0)
                again, again_in_time = connect_early(PEER_ISN + 0x1000, AGAIN_REQUEST) if first else (None, None)
                prompt(command)
                out = g.transcript()[before:]
                handed = re.search(r"dialprobe: accepted (\d) (\S*)", out)
                check(listening and early is not None and handed is not None
                      and handed.group(2) == EARLY_REQUEST.decode().strip(),
                      f"a client that sends its request and FIN before `listen` is read is handed out, request whole"
                      + ("" if listening and early else " (no SYN-ACK)" if listening else " (never listening)")
                      + ("" if handed else f" (it said: {said(out)})"))
                rehanded = re.search(r"dialprobe: again (\d) (\S*)", out)
                check(again is not None and rehanded is not None and rehanded.group(2) == AGAIN_REQUEST.decode().strip(),
                      f"the client reconnecting from that port while its first connection is held Closed is accepted and handed out"
                      + ("" if again else " (no SYN-ACK)") + ("" if rehanded else f" (it said: {said(out)})"))
                check(first and not stale,
                      "a late copy of the first connection's SYN, while that connection is held Closed, gets no SYN-ACK"
                      + ("" if first else " (the first connection was never handed out)"))
                for ok, what in ((early_in_time, "the first client's FIN"), (again_in_time, "the reconnected client's FIN")):
                    if ok is False:
                        unforced.append(f"netd took {what} after dialprobe's `listen` read, so its check did not test the close before the read")
                # `serve` timing out with no connection closes its listener: a
                # `Listening` slot is never idle-reaped, so before 2026-11-23
                # each such run held its slot for the rest of the boot.
                for k in range(MAX_DIAL):
                    command = f"serve /net {9000 + k}"
                    before = len(g.transcript())
                    started = time.monotonic()
                    g.type_line(command)
                    ended = g.wait_for("# ", timeout=SERVE_TIMEOUT_S)
                    out = g.transcript()[before:]
                    check(ended and "serve: no connection accepted (timed out)" in out,
                          f"`{command}`, nobody connecting, times out ({time.monotonic() - started:.1f} s)"
                          + ("" if ended and "timed out" in out else f" (it said: {said(out)})"))
                good_dial(8002, 3)
            during = g.transcript()[mark:]
            wedged, restarted = during.count("wedged"), during.count("restarted")
            check(wedged == 0 and restarted == 0, f"no server called wedged or restarted ({wedged} wedged, {restarted} restarted)")
            check(g.proc.poll() is None, "the guest is alive at the end")
        out = g.transcript()
        faults = g.aborts()
        with open(os.path.join(os.path.dirname(HERE), "build", "net-dial.txt"), "w") as fh:
            fh.write(out)
    except socket.timeout:
        out, faults = g.transcript(), g.aborts()
        print("[net-dial] QEMU never connected to the socket netdev", flush=True)
    finally:
        g.stop()
        lst.close()
    if drive_qemu.boot_stalled(RIG, "boot", out, faults):
        return drive_qemu.verdict(RIG, 0, 1)
    if not booted:
        check(False, "the boot reaches netd and a login")
    check(faults == 0, f"no fault lines in QEMU's trace ({faults})")
    failed = sum(1 for ok, _ in checks if not ok)
    if slow is not None and slow > LEAK_WINDOW_S:
        unforced.append(f"the last three failed dials took {slow:.1f} s, past {LEAK_WINDOW_S} s: "
                        "a leaking dial's slots could be reaped in time, so the leak control could not fail")
    if not failed and unforced:
        for why in unforced:
            print(f"{RIG}: INCONCLUSIVE, {why}")
        print(f"{RIG}: run it again")
        return drive_qemu.INCONCLUSIVE
    return drive_qemu.verdict(RIG, failed, 0)


if __name__ == "__main__":
    sys.exit(main())

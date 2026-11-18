#!/usr/bin/env python3
"""`netd` lives through a flood of frames, and serves after it.

Pi net step 7's left-open item, "netd's drain loop has no bound and no
heartbeat" (`docs/roadmap/roadmap-pi-net.md`): `serve`'s drain took frames
for as long as any waited, so under a flood a pass never ended, `netd` never
came back to `NET_WAIT`, and the supervisor, which saw it `Runnable` for
`WEDGE_TICKS` with no progress, called it wedged. On the Pi a 15 s UDP flood
from the Mac had it restarted three times and given up on for the boot
(boot 42, 2026-11-17). The fix is two halves, each needed: a pass takes at
most `DRAIN_BUDGET` frames, and `NET_WAIT` notes progress for the supervisor
as `MSG_RECV` does (under a flood it returns at once, so `netd` is never
seen blocked).

One boot of the FAT32 image. The host floods the guest's port 9 with small
UDP datagrams through a forwarded port for `FLOOD_S` seconds, the cheapest
frame `netd` can be sent (it serves nothing there and drops each), while QEMU
dumps every frame to a pcap. The checks:

1. The boot reaches `login:` and `netd` answers HTTP before the flood.
2. **The flood reached the guest:** the pcap holds at least `MIN_FLOOD`
   datagrams to port 9. Without it the checks below pass on a flood that
   never arrived.
3. No server is called wedged, and none restarted, during the flood or the
   `SETTLE_S` seconds after.
4. `netd` answers HTTP after it, and the guest is alive.

Controlled 2026-11-17: `main` before the fix (`5f43c890`) had netd called
wedged four times and given up on in the same flood, and HTTP timed out
after; on the fix with either half removed (the drain unbounded, or the
note at `NET_WAIT` taken out) it failed check 3 as well.

Usage:  python3 scripts/test-net-flood.py      (make test-net-flood builds the image)
Exit status: 0, 1 for a failed check, drive-qemu's INCONCLUSIVE for a boot
the firmware never started.
"""
import importlib.util
import os
import socket
import struct
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
PCAP = os.path.join(ROOT, "build", "net-flood.pcap")
PORT_UDP, PORT_HTTP = 5665, 5666
# Six times WEDGE_TICKS (2.56 s): on `main` the first wedge came within it.
FLOOD_S = 15.0
SETTLE_S = 5.0
# Far fewer than a flood delivers (QEMU's user networking passed several
# hundred thousand in FLOOD_S here), and far more than `netd` drains in
# 2.56 s, which it did not on `main`.
MIN_FLOOD = 50_000


def http():
    """Whether the guest's HTTP server answers through the forwarded port."""
    try:
        with urllib.request.urlopen(f"http://localhost:{PORT_HTTP}/", timeout=10) as r:
            return r.status == 200
    except Exception:
        return False


def flood(secs):
    """Datagrams sent to the forwarded UDP port for `secs` seconds."""
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sent = 0
    end = time.monotonic() + secs
    while time.monotonic() < end:
        for _ in range(256):
            try:
                s.sendto(b"x" * 18, ("127.0.0.1", PORT_UDP))
                sent += 1
            except OSError:
                pass  # the host's own buffer full; the next try goes
    s.close()
    return sent


def port9_datagrams(path):
    """The pcap's IPv4 UDP frames to port 9: what reached the guest's NIC.
    QEMU's filter-dump writes the classic format, little-endian, Ethernet."""
    data = open(path, "rb").read()
    if len(data) < 24 or struct.unpack_from("<I", data, 0)[0] != 0xA1B2C3D4:
        return -1
    n, at = 0, 24
    while at + 16 <= len(data):
        incl = struct.unpack_from("<I", data, at + 8)[0]
        frame = data[at + 16:at + 16 + incl]
        at += 16 + incl
        if len(frame) < 38 or frame[12:14] != b"\x08\x00" or frame[23] != 17:
            continue
        ihl = (frame[14] & 0x0F) * 4
        if len(frame) >= 14 + ihl + 4 and struct.unpack_from(">H", frame, 14 + ihl + 2)[0] == 9:
            n += 1
    return n


def main():
    failed = 0

    def check(ok, what):
        nonlocal failed
        print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)
        failed += 0 if ok else 1

    if os.path.exists(PCAP):
        os.unlink(PCAP)
    g = drive_qemu.Guest(IMAGE, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=udp::{PORT_UDP}-:9,hostfwd=tcp::{PORT_HTTP}-:80",
        "-device", "virtio-net-device,netdev=net0",
        "-object", f"filter-dump,id=f0,netdev=net0,file={PCAP}",
    ), label="[net-flood] ")
    stalled = 0
    try:
        deadline = time.time() + 120
        while time.time() < deadline and not all(x in g.transcript() for x in ["netd: network server ready", "login:"]):
            time.sleep(0.2)
        text = g.transcript()
        if "login:" not in text and drive_qemu.firmware_stalled(text, 0):
            stalled = 1
            return drive_qemu.verdict("test-net-flood", 0, stalled)
        check("netd: network server ready" in text and "login:" in text, "the boot reaches netd and a login")
        check(http(), "netd answers HTTP before the flood")
        before = len(g.transcript())
        sent = flood(FLOOD_S)
        time.sleep(SETTLE_S)
        during = g.transcript()[before:]
        print(f"[net-flood] sent {sent} datagrams in {FLOOD_S:.0f} s", flush=True)
        check(http(), "netd answers HTTP after the flood")
        check(g.proc.poll() is None, "the guest is alive at the end")
        wedged = during.count("wedged")
        restarted = during.count("restarted")
        check(wedged == 0 and restarted == 0,
              f"no server called wedged or restarted during the flood ({wedged} wedged, {restarted} restarted)")
        if wedged or restarted:
            lines = [l for l in during.splitlines() if "slot" in l or l.startswith("netd:")]
            print("[net-flood] the guest's lines:\n  " + "\n  ".join(lines[-12:]), flush=True)
    finally:
        g.stop()
    reached = port9_datagrams(PCAP)
    check(reached >= MIN_FLOOD, f"the flood reached the guest: {reached} datagrams to port 9 in the pcap, at least {MIN_FLOOD}")
    return drive_qemu.verdict("test-net-flood", failed, stalled)


if __name__ == "__main__":
    sys.exit(main())

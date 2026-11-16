#!/usr/bin/env python3
"""`netd`'s address comes from `/etc/net`, and from nowhere else when the file is bad.

Pi net step 8 (`docs/roadmap/roadmap-pi-net.md`): `netd` reads its address,
mask, gateway and DNS server from `/etc/net` on the root (`netconf` parses
it), and uses QEMU user networking's values when there is no file. Three
boots of the ext2 image, each a copy with `/etc/net` put in by debugfs (from
e2fsprogs) or left out:

1. **A file naming 10.0.2.20.** `netd` says the address is from the file,
   the host reaches its HTTP server at 10.0.2.20 through a port forwarded
   there, and does NOT reach one at 10.0.2.15, the address it would have had.
   `ping 10.0.2.2` from the guest answers, so frames it sends carry an address
   SLIRP will answer to, and a ping of an address off the subnet is sent by
   way of the gateway (it is not refused for want of an ARP reply).
2. **A file with a line that is not understood.** `netd` says the file is
   refused and which line, then that it uses the defaults; the host reaches it
   at 10.0.2.15 and not at 10.0.2.20, the address the bad file named. Nothing
   of a refused file is used.
3. **No file**, the control: the defaults with no warning, reached at
   10.0.2.15.

The two "not reached" checks are what make the "reached" ones mean
something: SLIRP forwards a port to whatever address it is told, so a guest
answering at both would pass a check that only looked for an answer.

Usage:  python3 scripts/test-net-config.py      (make test-net-config builds the image)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import shutil
import subprocess
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "espext2.img")
COPY = os.path.join(ROOT, "build", "netconf.img")
PART = os.path.join(ROOT, "build", "netconf-part.img")
CONF = os.path.join(ROOT, "build", "netconf-etc-net.txt")
# The ext2 root's place in espext2.img: mkext2.py's layout (partition 1 at LBA
# 2048, 49152 sectors), as test-no-identity.py reads it. Checked below by
# finding /etc/passwd there.
OFF, SIZE = 2048 * 512, 49152 * 512
OLD, NEW = "10.0.2.15", "10.0.2.20"
PORT_OLD, PORT_NEW = 5661, 5662
# An address off the guest's subnet (TEST-NET-3), for the next hop.
FAR = "203.0.113.9"

GOOD = f"# test-net-config\naddress {NEW}\nmask 255.255.255.0\ngateway 10.0.2.2\ndns 10.0.2.3\n"
# Line 3 is the one not understood. The address line before it is a good one,
# so a parser that kept what it had read so far would come up at NEW.
BAD = f"address {NEW}\nmask 255.255.255.0\nrouter 10.0.2.2\n"


def debugfs():
    prefix = subprocess.run(["brew", "--prefix", "e2fsprogs"], capture_output=True, text=True).stdout.strip()
    tool = os.path.join(prefix, "sbin", "debugfs")
    if not prefix or not os.access(tool, os.X_OK):
        sys.exit("test-net-config: needs e2fsprogs' debugfs (brew install e2fsprogs)")
    return tool


def make_copy(conf):
    """A copy of the image, with `conf` as /etc/net when it is not None."""
    tool = debugfs()
    shutil.copyfile(IMAGE, COPY)
    with open(COPY, "rb") as f:
        f.seek(OFF)
        open(PART, "wb").write(f.read(SIZE))
    def stat(path):
        return subprocess.run([tool, "-R", f"stat {path}", PART], capture_output=True, text=True).stdout.find("Inode:") >= 0
    if not stat("/etc/passwd"):
        sys.exit("test-net-config: /etc/passwd not found where the ext2 root should be; the layout moved")
    if stat("/etc/net"):
        sys.exit("test-net-config: the image already holds /etc/net, so 'no file' would not be the control")
    if conf is not None:
        open(CONF, "w").write(conf)
        subprocess.run([tool, "-w", "-R", f"write {CONF} /etc/net", PART], capture_output=True, check=True)
        os.unlink(CONF)
        if not stat("/etc/net"):
            sys.exit("test-net-config: debugfs did not write /etc/net")
    with open(COPY, "r+b") as f:
        f.seek(OFF)
        f.write(open(PART, "rb").read())
    os.unlink(PART)


def http(port):
    """Whether the guest's HTTP server answers through the forwarded port."""
    try:
        with urllib.request.urlopen(f"http://localhost:{port}/etc/passwd", timeout=8) as r:
            return b"root" in r.read()
    except Exception:
        return False


def boot(label, conf, want_lines, reached, not_reached, ping):
    failed = 0

    def check(ok, what):
        nonlocal failed
        print(f"[{'PASS' if ok else 'FAIL'}] {label}: {what}", flush=True)
        failed += 0 if ok else 1

    try:
        make_copy(conf)
    except BaseException:
        for leftover in (COPY, PART, CONF):
            if os.path.exists(leftover):
                os.unlink(leftover)
        raise
    g = drive_qemu.Guest(COPY, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT_OLD}-{OLD}:80,hostfwd=tcp::{PORT_NEW}-{NEW}:80",
        "-device", "virtio-net-device,netdev=net0",
    ), label=f"[{label}] ")
    try:
        # The whole transcript is asked, not `wait_for` a line at a time:
        # `wait_for` moves its mark to the end of what has arrived, so of two
        # lines printed together it finds the first and has passed the second.
        deadline = time.time() + 90
        while time.time() < deadline and not all(x in g.transcript() for x in [*want_lines, "login:"]):
            time.sleep(0.2)
        text = g.transcript()
        for line in want_lines:
            check(line in text, f"netd says {line!r}")
        check("login:" in text, "the boot reaches a login")
        warned = "netd: WARNING /etc/net" in text
        check(warned == any("WARNING" in line for line in want_lines), f"a warning about /etc/net: {warned}")
        port = {OLD: PORT_OLD, NEW: PORT_NEW}
        check(http(port[reached]), f"the host reaches its HTTP server at {reached}")
        check(not http(port[not_reached]), f"and does not at {not_reached}")
        if ping:
            g.type_line("root")
            g.run([("assword", "root"), ("# ", "ping 10.0.2.2"), ("# ", f"ping {FAR}"), ("# ", "")])
            text = g.transcript()
            check("reply from 10.0.2.2" in text, "ping 10.0.2.2 from the guest answers")
            # Off the subnet the frame goes to the gateway, which SLIRP
            # answers ARP for; nobody answers ARP for FAR itself. So "no
            # ARP reply" is a ping that asked the wire for the far address,
            # as `handle_ping` did until 2026-11-15. Whether the echo is
            # answered is SLIRP's and the host's business, and not asked.
            went = f"reply from {FAR}" in text
            check(went and f"{FAR} is unreachable (no ARP reply)" not in text,
                  f"ping {FAR}, off the subnet, is sent by way of the gateway")
        check(g.proc.poll() is None, "the guest is alive at the end")
        if failed:
            # What the guest said, for the checks above that did not find it.
            lines = [l for l in g.transcript().splitlines() if l.startswith("netd:") or "ping" in l or "reply" in l or "unreach" in l or l.startswith("#")]
            print(f"[{label}] the guest's lines:\n  " + "\n  ".join(lines[-25:]), flush=True)
    finally:
        g.stop()
        for leftover in (COPY, PART, CONF):
            if os.path.exists(leftover):
                os.unlink(leftover)
    return failed


def main():
    failed = 0
    failed += boot("file", GOOD,
                   [f"netd: address {NEW}/24, gateway 10.0.2.2, dns 10.0.2.3 (from /etc/net)"],
                   reached=NEW, not_reached=OLD, ping=True)
    failed += boot("bad file", BAD,
                   ["netd: WARNING /etc/net is refused: a key that is not address, mask, gateway or dns, at line 3",
                    f"netd: address {OLD}/24, gateway 10.0.2.2, dns 10.0.2.3 (the defaults)"],
                   reached=OLD, not_reached=NEW, ping=False)
    failed += boot("no file", None,
                   [f"netd: address {OLD}/24, gateway 10.0.2.2, dns 10.0.2.3 (no /etc/net: the defaults)"],
                   reached=OLD, not_reached=NEW, ping=False)
    print(f"test-net-config: {failed} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

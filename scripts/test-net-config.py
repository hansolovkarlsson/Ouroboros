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
4. **A file on a root that mounts late** (since 2026-11-22): the ESP on a
   virtio vvfat disk, which fsd cannot mount, as `make run` boots it, and
   the copy with `/etc/net` naming 10.0.2.20 on a USB stick, which the
   kernel takes only when `mount -a` rescans: the Parallels workflow for a
   stick passed through after the VM started. `netd` must first say nothing
   was mounted and use the defaults; after `mount -a` it must say the root
   mounted after it loaded, then that its address is from the file, and the
   host must reach it at 10.0.2.20 and not at 10.0.2.15. Before 2026-11-22
   nothing read `/etc/net` again, so the guest stayed at 10.0.2.15.

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
# The staged ESP directory, booted through vvfat in the late boot.
ESP = os.path.join(ROOT, "build", "esp")
COPY = os.path.join(ROOT, "build", "netconf.img")
PART = os.path.join(ROOT, "build", "netconf-part.img")
CONF = os.path.join(ROOT, "build", "netconf-etc-net.txt")
# The ext2 root's place in espext2.img: mkext2.py's layout (partition 1 at LBA
# 2048, 49152 sectors), as test-no-identity.py reads it. Checked below by
# finding /etc/passwd there.
OFF, SIZE = 2048 * 512, 49152 * 512
OLD, NEW = "10.0.2.15", "10.0.2.20"
PORT_OLD, PORT_NEW = 5661, 5662
# netd's last line of a load with no identity (`load_auth`), at start and
# after a reload: the late boot types once it has seen it.
SETTLED = "netd: no /etc/cluster/id - cannot make remote requests"
# netd's line when the root mounts after its load found nothing mounted.
RELOAD = "netd: the root mounted after netd loaded: reading /etc/cluster and /etc/net again"
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


def boot(label, conf, want_lines, reached, not_reached, ping, late=None):
    """One boot of a copy of the image with `conf` as /etc/net. With `late`,
    the lines netd must print before `mount -a`, the copy is a USB stick
    and the boot disk the ESP through vvfat, which fsd cannot mount; the
    shell types `mount -a` once netd has loaded, and `want_lines` are the
    lines after it."""
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
    net = (
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT_OLD}-{OLD}:80,hostfwd=tcp::{PORT_NEW}-{NEW}:80",
        "-device", "virtio-net-device,netdev=net0",
    )
    if late is None:
        g = drive_qemu.Guest(COPY, extra_args=net, label=f"[{label}] ")
    else:
        # vvfat read-only (`fat:` and `readonly=on`, not `fat:rw:`), so
        # nothing is written to the staged ESP; the stick is the copy.
        g = drive_qemu.Guest(os.path.join(ESP, "boot"), virtio_disk=False, stamp_from=ESP, label=f"[{label}] ", extra_args=(
            "-drive", f"file=fat:{ESP},format=raw,media=disk,if=none,id=hd0,readonly=on",
            "-device", "virtio-blk-device,drive=hd0",
            "-device", "qemu-xhci,id=xhci0",
            "-drive", f"file={COPY},format=raw,if=none,id=usbstick",
            "-device", "usb-storage,drive=usbstick,bus=xhci0.0,port=1",
            *net,
        ))
    try:
        mark = 0
        if late is not None:
            ok = g.wait_for(drive_qemu.re.escape(SETTLED), timeout=90)
            text = g.transcript()
            for line in late:
                check(line in text, f"before mount -a, netd says {line!r}")
            check(not http(PORT_NEW) and not http(PORT_OLD), "before mount -a, the host reaches no HTTP server")
            # What netd says after `mount -a` is read from here on, so the
            # lines of its first load cannot answer for it.
            mark = len(g.transcript())
            ok = ok and g.run([("", drive_qemu.ENTER), ("# ", "mount -a")])
            check(ok and g.wait_for("disk commands available", timeout=30), "mount -a mounts the stick")
        # The whole transcript is asked, not `wait_for` a line at a time,
        # so the lines may come in any order. (This rig's first run is how
        # `wait_for` was found passing over the second of two lines printed
        # together; it moves its mark to the end of the match since
        # 2026-11-21.)
        deadline = time.time() + 90
        # After a reload, the export's line: the stick's root may hold an
        # identity, so SETTLED need not come.
        settled = ["netd: export"] if late is not None else ["login:"]
        while time.time() < deadline and not all(x in g.transcript()[mark:] for x in [*want_lines, *settled]):
            time.sleep(0.2)
        text = g.transcript()[mark:]
        for line in want_lines:
            check(line in text, f"netd says {line!r}")
        if late is None:
            check("login:" in text, "the boot reaches a login")
            # fsd's notice of its own start's mount reaches every boot; a
            # netd whose load found the disk must not read it as a late one.
            check(RELOAD not in g.transcript(), "netd does not read its files again (its load found the disk)")
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
    failed += boot("late", GOOD,
                   [RELOAD,
                    f"netd: address {NEW}/24, gateway 10.0.2.2, dns 10.0.2.3 (from /etc/net)"],
                   reached=NEW, not_reached=OLD, ping=False,
                   late=[f"netd: address {OLD}/24, gateway 10.0.2.2, dns 10.0.2.3 (nothing mounted to read /etc/net from: the defaults)"])
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

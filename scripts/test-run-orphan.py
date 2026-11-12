#!/usr/bin/env python3
"""A `cpu` run's command dies with the run's connection.

The executing machine's `netd` spawns a run's command and holds the reply
until the command ends. Until 2026-11-10 a peer that closed mid-run left the
command running: its connection was freed, nothing reaped the child, and it
held a task slot until someone typed `wait N` (docs/ROADMAP.md, the
receive-path ledger). Since per-user keys a logout on the dispatching machine
closes its user's runs, so a routine event reached it.

One boot of a copy of the ext2 image, the guest as the executing machine. The
host dispatches `cpu recv`, a command that blocks for good, sees it in the
guest's `ps`, closes the connection, and the command must be gone from `ps`
with the kernel's kill line for its slot. Once with a FIN and once with a RST,
the two closes a peer can make. Around them, `cpu id` runs to its end and
kills nothing: a run that ends in the ordinary way is collected when its
command has exited, with no kill line.

Then the other way round: the host dispatches `cpu recv` and keeps its
connection open, and the command is killed on the executing machine (`kill N`
typed there). The run must end: the host must be sent the reply's end (a FIN)
and the connection freed. Until 2026-11-12 a run ended only on the command's
own empty end-of-stream message, which a killed command never sends, so the
caller waited for good on a connection the idle reap exempts.

Usage:  python3 scripts/test-run-orphan.py      (make test-run-orphan builds the image)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import shutil
import socket
import struct
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
client = load("np9p_client", "np9p_client.py")
# What the client's own main() derives before any request: the dev key the
# image authorizes. Imported as a module here, so the rig holds the socket and
# chooses how it closes.
client.SIGN_KEY = client.dev_seed(client.SIGN_LABEL)

IMAGE = os.path.join(ROOT, "build", "espext2.img")
COPY = os.path.join(ROOT, "build", "run-orphan.img")
PORT = 5652  # its own, so the other rigs' boots can coexist
COMMAND = "recv"  # blocks in MSG_RECV until a message nobody sends
# The kill's own line, one a kill: `task N killed`, or `task N killed (running
# on core C, which ends it)` for a task on another core, whose second line
# (`task N ended on core C`) is deliberately not matched, so one kill counts
# once on any number of cores.
KILLED = r"Ouroboros kernel: task (\d+) killed"
PCAP = os.path.join(ROOT, "build", "run-orphan.pcap")
TCP_FIN, TCP_RST = 0x01, 0x04

failed = 0
checks = 0


def check(ok, what):
    global failed, checks
    checks += 1
    failed += 0 if ok else 1
    print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)
    return ok


def ps(g):
    """The slots of the tasks named COMMAND in a fresh `ps`, or None when the
    listing did not come."""
    mark = len(g.transcript())
    g.type_line("ps")
    if not g.wait_for(r"task \d+: [^\r\n]*\r\n(?:[^#]*)# ", timeout=20):
        return None
    out = g.transcript()[mark:]
    return [int(n) for n in re.findall(rf"task (\d+): [^\r\n]*  {COMMAND}\r", out)]


def closes():
    """(FINs, RSTs) the guest's export port has been sent so far, read from
    QEMU's capture of the guest's NIC. The rig's own observer of WHICH close
    reached the guest: SLIRP sits between the host's socket and the guest,
    and nothing says it relays a reset as a reset. Without this the RST round
    could be a second FIN round and every check still pass."""
    data = open(PCAP, "rb").read()
    fins = rsts = 0
    off = 24  # the pcap file header
    while off + 16 <= len(data):
        incl = struct.unpack_from("<I", data, off + 8)[0]
        f = data[off + 16:off + 16 + incl]
        off += 16 + incl
        # Ethernet, IPv4, TCP, to the export's port.
        if len(f) < 34 or f[12:14] != b"\x08\x00" or f[23] != 6:
            continue
        tcp = 14 + (f[14] & 0x0f) * 4
        if len(f) < tcp + 14 or struct.unpack_from(">H", f, tcp + 2)[0] != 564:
            continue
        flags = f[tcp + 13]
        fins += 1 if flags & TCP_FIN else 0
        rsts += 1 if flags & TCP_RST else 0
    return fins, rsts


def until(g, want, seconds):
    """Poll `ps` until `want(slots)` holds; the last listing either way."""
    deadline = time.time() + seconds
    slots = ps(g)
    while not (slots is not None and want(slots)) and time.time() < deadline:
        time.sleep(1)
        slots = ps(g)
    return slots


def run_to_end(command):
    return subprocess.run(
        [sys.executable, os.path.join(HERE, "np9p_client.py"), "localhost", str(PORT), "run", command],
        capture_output=True, timeout=60).stdout.decode("latin-1")


def cut_short(g, how):
    """Dispatch COMMAND, see it running, close the connection `how`, and
    check the command is ended."""
    cmd = COMMAND.encode()
    frame, _nonce = client.request_frame(client.build_frame(client.NP_RUN, 0, [len(cmd), 0, 0, 0], cmd))
    s = socket.create_connection(("localhost", PORT), timeout=10)
    try:
        s.sendall(frame)
        running = until(g, lambda slots: len(slots) == 1, 20)
        if not check(running is not None and len(running) == 1,
                     f"{how}: the run's command is a task on the executing machine: `{COMMAND}` in slots {running}"):
            return
        before = len(g.transcript())
        fins0, rsts0 = closes()
        if how == "RST":
            # SO_LINGER with a zero timeout: the close of a peer that died.
            s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        s.close()
        gone = until(g, lambda slots: slots == [], 20)
        check(gone == [], f"{how}: the command is gone once its connection is closed: `{COMMAND}` in slots {gone}")
        lines = [int(n) for n in re.findall(KILLED, g.transcript()[before:])]
        check(lines == running, f"{how}: the kernel ended that task and no other: kill lines for {lines}, want {running}")
        fins, rsts = closes()
        fins, rsts = fins - fins0, rsts - rsts0
        ok = (fins >= 1 and rsts == 0) if how == "FIN" else (rsts >= 1 and fins == 0)
        check(ok, f"{how}: that close is what reached the guest: {fins} FIN(s) and {rsts} RST(s) to its export port")
    finally:
        s.close()


def killed_here(g):
    """Dispatch COMMAND, see it running, kill it on the executing machine,
    and check the run ends for the caller, whose connection stays open."""
    cmd = COMMAND.encode()
    frame, _nonce = client.request_frame(client.build_frame(client.NP_RUN, 0, [len(cmd), 0, 0, 0], cmd))
    s = socket.create_connection(("localhost", PORT), timeout=10)
    try:
        s.sendall(frame)
        running = until(g, lambda slots: len(slots) == 1, 20)
        if not check(running is not None and len(running) == 1,
                     f"killed here: the run's command is a task on the executing machine: `{COMMAND}` in slots {running}"):
            return
        g.type_line(f"kill {running[0]}")
        gone = until(g, lambda slots: slots == [], 20)
        check(gone == [], f"killed here: the command is gone: `{COMMAND}` in slots {gone}")
        # The reply's end: bytes or none, then the peer's FIN (recv of b"").
        s.settimeout(20)
        ended = False
        try:
            while not ended:
                ended = s.recv(4096) == b""
        except (socket.timeout, ConnectionResetError):
            pass
        check(ended, "killed here: the caller is sent the run's end (a FIN) with its connection still open")
    finally:
        s.close()


def main():
    shutil.copyfile(IMAGE, COPY)
    g = drive_qemu.Guest(COPY, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT}-:564",
        "-device", "virtio-net-device,netdev=net0",
        "-object", f"filter-dump,id=f0,netdev=net0,file={PCAP}",
    ), label="[run-orphan] ")
    try:
        if not check(g.run([("login:", "root"), ("assword", "root"), ("# ", "")]), "the guest boots to root's shell"):
            return failed
        out = run_to_end("id")
        check("uid=" in out, f"before: `cpu id` runs to its end: {out.strip()[:80]!r}")
        cut_short(g, "FIN")
        cut_short(g, "RST")
        killed_here(g)
        out = run_to_end("id")
        check("uid=" in out, f"after: `cpu id` still runs to its end: {out.strip()[:80]!r}")
        text = g.transcript()
        kills = len(re.findall(KILLED, text))
        check(kills == 3, f"three tasks were killed in the whole boot, the two cut short and the one killed by hand: {kills}")
        restarts = len(re.findall(r"restarted \(attempt", text))
        check(restarts == 0, f"no server was restarted: {restarts} restart line(s)")
        check(g.proc.poll() is None, "the guest is alive at the end")
    finally:
        transcript = g.transcript()
        g.stop()
        with open(os.path.join(ROOT, "build", "run-orphan.txt"), "w") as f:
            f.write(transcript)
        os.unlink(COPY)
        if os.path.exists(PCAP) and not failed:
            os.unlink(PCAP)
    want = 17
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-run-orphan: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

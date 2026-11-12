#!/usr/bin/env python3
"""A `cpu` run's command ends when its caller goes silent, and not before.

While a run's command runs neither side of its connection speaks, so until
2026-11-12 the executing machine exempted the connection from its idle reap,
and a caller that vanished with no FIN and no RST (a machine powered off, a
cable pulled) left the command running for good, holding a task slot and a
connection slot (docs/ROADMAP.md, the receive-path ledger, from the review of
#437). The executing `netd` now probes a quiet run's caller every five
seconds with a TCP keepalive, a live caller's answers keep the connection,
and a connection silent for thirty seconds is reaped, which ends the command.

Two parts, since the two kinds of caller answer by different code.

**A host caller**, one boot of a copy of node A's ext2 image: the host dispatches
`cpu recv` through QEMU's user network and holds the connection open, doing
nothing. That caller is alive (QEMU's own TCP answers for it), so after
forty-five seconds, past the reap's thirty, the command must still be in the
guest's `ps`. Then forty-five seconds more with the guest's shell pinging an
address nothing answers at, one ping after another: each keeps `netd` in a
wait that reads frames off the NIC and drops all but its own reply, a probe's
answer among them, and time `netd` could not hear must not count as the
caller's silence (`credit_deaf`), so the command must still be there.

**A guest caller**, the two-node ext2 pair on a socket link: node B's shell
runs `cpu 10.0.2.10:564 recv` against node A. The probes are answered by
node B's `netd`, its dial side, which until the same day answered nothing to
one, and by then had closed its side anyway: the export did not acknowledge
a run's request until its reply, so node B retransmitted it for four and a
half seconds, gave up, and told its user "cpu: remote run failed" while the
command ran on (measured on main: 5.9 s). So twenty seconds in, node B must
not have said that. A dispatching `netd` gives a run up itself after thirty seconds without
output (`RUN_DEADLINE_TICKS`, and a run's output comes only at its end), so
"kept for forty-five seconds" cannot be asked of this caller: whichever side
ends the run, `ps` shows the same. What is asked, twenty seconds in, is read
from QEMU's capture of node A's NIC: node A sent probes (bare ACKs one
sequence number back) and node B answered them. Then node B's QEMU is killed
outright, before its own deadline, so nothing of B's says goodbye: the rig
checks its own clock for that (killed within 26 s of the `cpu` being typed,
or it says it was too slow, which is not the keepalive's failure), and the
capture must hold no FIN and no RST to the export's port. Within forty-five
seconds more the command must be gone from A's `ps`, with the kernel's kill
line for its slot and no other, no server restarted, and A still answering.
Both nodes boot copies of their images, so node B's disk, killed with its
root mounted, is not what the next run or a hand-run pair boots.

Usage:  make test-run-silent     (builds the node pair)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import shutil
import socket
import struct
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
client.SIGN_KEY = client.dev_seed(client.SIGN_LABEL)

BUILD = os.path.join(ROOT, "build")
NODE_A = os.path.join(BUILD, "espext2-a.img")
NODE_B = os.path.join(BUILD, "espext2-b.img")
COPY_A = os.path.join(BUILD, "run-silent-a.img")
COPY_B = os.path.join(BUILD, "run-silent-b.img")
PORT = 5654  # the host caller's forward, its own
# The pair's link, its own: the Makefile's rigs have 12340 and 12341,
# drive-2vm.py 12342, test-cred-client.py 12343.
LINK_PORT = 12346
COMMAND = "recv"  # blocks in MSG_RECV until a message nobody sends
KILLED = r"Ouroboros kernel: task (\d+) killed"
QUIET = 45  # seconds: past the idle reap's 30, with room for a slow boot's ticks
PROBED = 18  # seconds into a guest caller's run when the capture is read: three probes due
SEEN_BY = 10  # seconds the run's command may take to show in node A's `ps`
# Node B gives a run up 30 s after sending it (RUN_DEADLINE_TICKS) and then
# closes with a FIN, which ends the command too. Node B must be killed before
# that for the silent path to be the one tested; past this many seconds the
# rig says it was too slow and does not grade the keepalive.
KILL_BY = 26
PCAP = os.path.join(BUILD, "run-silent-a.pcap")
LOGIN = [("login:", "root"), ("assword", "root"), ("# ", "")]

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


def until(g, want, seconds):
    deadline = time.time() + seconds
    slots = ps(g)
    while not (slots is not None and want(slots)) and time.time() < deadline:
        time.sleep(1)
        slots = ps(g)
    return slots


def probes(path):
    """(probes node A sent, answers node B gave after the first of them, FINs
    to the export's port, RSTs to it), from the capture of node A's NIC. A probe is a segment from the export's
    port with no payload and only ACK set, one sequence number below the
    highest the export has used; an answer is a segment to that port with no
    payload and only ACK set, captured after the first probe."""
    data = open(path, "rb").read()
    segs = []
    off = 24  # the pcap file header
    while off + 16 <= len(data):
        incl = struct.unpack_from("<I", data, off + 8)[0]
        f = data[off + 16:off + 16 + incl]
        off += 16 + incl
        if len(f) < 34 or f[12:14] != b"\x08\x00" or f[23] != 6:
            continue
        ip = 14 + (f[14] & 0x0f) * 4
        total = struct.unpack_from(">H", f, 16)[0]
        if len(f) < ip + 20:
            continue
        sport, dport, seq = struct.unpack_from(">HHI", f, ip)
        payload = 14 + total - (ip + (f[ip + 12] >> 4) * 4)
        segs.append((sport, dport, seq, f[ip + 13], payload))
    top = max((seq for sport, _, seq, _, _ in segs if sport == 564), default=0)
    sent = answered = fins = rsts = 0
    for sport, dport, seq, flags, payload in segs:
        bare = payload == 0 and flags == 0x10
        if sport == 564 and bare and seq == (top - 1) & 0xffffffff:
            sent += 1
        elif dport == 564 and bare and sent:
            answered += 1
        if dport == 564:
            fins += 1 if flags & 0x01 else 0
            rsts += 1 if flags & 0x04 else 0
    return sent, answered, fins, rsts


def host_caller():
    """A caller on the host, alive and saying nothing: its run is kept."""
    copy = os.path.join(BUILD, "run-silent.img")
    shutil.copyfile(NODE_A, copy)
    g = drive_qemu.Guest(copy, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT}-:564",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[run-silent host] ")
    s = None
    try:
        if not check(g.run(LOGIN), "host caller: the guest boots to root's shell"):
            return
        cmd = COMMAND.encode()
        frame, _nonce = client.request_frame(client.build_frame(client.NP_RUN, 0, [len(cmd), 0, 0, 0], cmd))
        s = socket.create_connection(("localhost", PORT), timeout=10)
        s.sendall(frame)
        running = until(g, lambda slots: len(slots) == 1, 20)
        if not check(running is not None and len(running) == 1,
                     f"host caller: the run's command is a task: `{COMMAND}` in slots {running}"):
            return
        time.sleep(QUIET)
        still = ps(g)
        check(still == running,
              f"host caller: after {QUIET} s of a live caller's silence the command still runs: slots {still}, want {running}")
        kills = re.findall(KILLED, g.transcript())
        check(kills == [], f"host caller: nothing was killed: {kills}")
        # netd kept deaf: back-to-back pings of an address with no host.
        pings = 0
        deadline = time.time() + QUIET
        while time.time() < deadline:
            g.type_line("ping 10.0.2.99")
            if not g.wait_for(r"ping[^\r\n]*\r\n(?:[^#]*)# ", timeout=20):
                break
            pings += 1
        still = ps(g)
        kills = re.findall(KILLED, g.transcript())
        check(pings >= 10 and still == running and kills == [],
              f"host caller: after {QUIET} s more with netd kept in {pings} pings of a dead address the command still runs: "
              f"slots {still}, killed {kills}")
    finally:
        if s is not None:
            s.close()
        transcript = g.transcript()
        g.stop()
        with open(os.path.join(BUILD, "run-silent-host.txt"), "w") as f:
            f.write(transcript)
        os.unlink(copy)


def guest_caller():
    """A caller that is another node: kept while it lives, and its command
    ended once it is gone without a word."""
    a = b = None
    try:
        if os.path.exists(PCAP):
            os.remove(PCAP)  # never last run's capture
        shutil.copyfile(NODE_A, COPY_A)
        shutil.copyfile(NODE_B, COPY_B)
        a = drive_qemu.Guest(COPY_A, extra_args=[
            "-netdev", f"socket,id=net0,listen=127.0.0.1:{LINK_PORT}",
            "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a",
            "-object", f"filter-dump,id=f0,netdev=net0,file={PCAP}",
        ], intlog=os.path.join(BUILD, "run-silent-int-a.log"), label="[run-silent A] ")
        if not check(a.run(LOGIN), "guest caller: node A boots to root's shell"):
            return
        b = drive_qemu.Guest(COPY_B, extra_args=[
            "-netdev", f"socket,id=net0,connect=127.0.0.1:{LINK_PORT}",
            "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0b",
        ], intlog=os.path.join(BUILD, "run-silent-int-b.log"), label="[run-silent B] ")
        if not check(b.run(LOGIN), "guest caller: node B boots to root's shell"):
            return
        typed = time.time()
        b.type_line(f"cpu 10.0.2.10:564 {COMMAND}")
        running = until(a, lambda slots: len(slots) == 1, SEEN_BY)
        if not check(running is not None and len(running) == 1,
                     f"guest caller: the run's command is a task on node A within {SEEN_BY} s: `{COMMAND}` in slots {running}"):
            return
        time.sleep(max(0, typed + PROBED - time.time()))
        still = ps(a)
        check(still == running and re.findall(KILLED, a.transcript()) == [],
              f"guest caller: {PROBED} s in, with node B alive, the command still runs and nothing was killed: slots {still}")
        said = re.findall(r"cpu: [^\r\n]*", b.transcript())
        check(said == [], f"guest caller: {PROBED} s in, node B's `cpu` has not given the run up: it said {said}")
        sent, answered, _, _ = probes(PCAP)
        check(sent >= 2, f"guest caller: node A probed its quiet caller: {sent} probe(s) in its NIC's capture")
        check(answered >= sent - 1 and answered >= 1,
              f"guest caller: node B's netd answered them: {answered} answer(s) to {sent} probe(s)")
        # Gone without a word: QEMU killed outright, so node B's guest sends
        # nothing and the link carries no FIN and no RST.
        before = len(a.transcript())
        b.proc.kill()
        b.proc.wait()
        took = time.time() - typed
        check(took < KILL_BY,
              f"the rig killed node B {took:.0f} s after its `cpu` was typed, before node B's own 30 s deadline "
              f"(over {KILL_BY} s is the rig too slow, and what follows is then not the keepalive's doing)")
        gone = until(a, lambda slots: slots == [], QUIET)
        check(gone == [], f"guest caller: with node B gone the command is ended within {QUIET} s: `{COMMAND}` in slots {gone}")
        lines = [int(n) for n in re.findall(KILLED, a.transcript()[before:])]
        check(lines == running, f"guest caller: the kernel ended that task and no other: kill lines for {lines}, want {running}")
        _, _, fins, rsts = probes(PCAP)
        check(fins == 0 and rsts == 0,
              f"guest caller: node B said nothing on its way out: {fins} FIN(s) and {rsts} RST(s) to the export's port in the capture")
        text = a.transcript()
        restarts = len(re.findall(r"restarted \(attempt", text))
        check(restarts == 0, f"guest caller: no server was restarted on node A: {restarts} restart line(s)")
        check(a.proc.poll() is None and ps(a) is not None, "guest caller: node A is alive and answers at the end")
    finally:
        for g, name in ((a, "a"), (b, "b")):
            if g is not None:
                transcript = g.transcript()
                g.stop()
                with open(os.path.join(BUILD, f"run-silent-{name}.txt"), "w") as f:
                    f.write(transcript)
        for copy in (COPY_A, COPY_B):
            if os.path.exists(copy):
                os.unlink(copy)


def main():
    host_caller()
    guest_caller()
    want = 5 + 13
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-run-silent: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

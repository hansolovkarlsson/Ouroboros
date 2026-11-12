#!/usr/bin/env python3
"""A restarted server's children end with it.

`netd` spawns a `cpu` run's command and kills it when the run's connection
goes. A crash or a wedge takes `netd`'s table with no such step, and until
2026-11-12 the supervisor restarted `netd` and left every running command
behind, with nothing to answer to and nothing to collect it (docs/ROADMAP.md,
the receive-path ledger, from the review of #437). The kernel now ends the
torn-down server's children before the fresh one goes in
(`tasks::end_children_of_last_occupant`, called by `supervisor::restart`).

No request makes the shipped `netd` fault or spin, so the boot runs `netd`'s
`restart-rig` build, staged on a copy of esp.img: a ping of 192.0.2.250
faults it and of 192.0.2.251 spins it, the two ways the supervisor comes to
restart a server. One boot, three rounds. In each the host dispatches a
command through the export and keeps its connection open, the command is
seen in the guest's `ps`, the guest's shell pings the address, and then:

- the kernel must say it restarted server slot 4, and for which reason;
- it must say it ended the command's task, naming that slot and no other;
- the command must be gone from `ps`;
- a fresh `cpu id` from the host must run to its end on the new `netd`.

The rounds: `recv` (blocked for good) with a fault, `recv` with a spin, and
`coreprobe hold` (running, never blocked) with a fault, which on one core is
a child that may be the task the kernel returns into, and with `SMP=4` one
running on another core.

Usage:  make test-server-children      (builds the image and the rig's netd)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import shutil
import socket
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
client.SIGN_KEY = client.dev_seed(client.SIGN_LABEL)

IMAGE = os.path.join(ROOT, "build", "esp.img")
COPY = os.path.join(ROOT, "build", "server-children.img")
# The rig's netd, named by the Makefile, which rebuilds it first.
RIG_NETD = os.environ.get("NETD_RESTART_BIN")
RIG_LINE = "netd: the restart rig's build"
PORT = 5653  # its own, so the other rigs' boots can coexist
FAULT, SPIN = "192.0.2.250", "192.0.2.251"
ENDED = r"Ouroboros kernel: task (\d+) (?:ended|ends at this core's next entry|had exited, collected)[^\r\n]*server slot 4, which spawned it, is gone"
RESTARTED = r"server slot 4 restarted \(attempt (\d+)"

failed = 0
checks = 0


def check(ok, what):
    global failed, checks
    checks += 1
    failed += 0 if ok else 1
    print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)
    return ok


def stage():
    """COPY: esp.img with \\EFI\\ORBS\\NETD.BIN the rig's build, the one file
    changed (what macOS leaves on a volume it mounts is removed, as the
    Makefile's image recipes do)."""
    if not RIG_NETD or not os.path.exists(RIG_NETD):
        sys.exit("test-server-children is run by `make test-server-children`, which builds the rig's netd and names it")
    shutil.copyfile(IMAGE, COPY)
    mp = drive_qemu.attach(COPY, readonly=False)
    try:
        shutil.copyfile(RIG_NETD, os.path.join(mp, "EFI", "ORBS", "NETD.BIN"))
        subprocess.run(["find", mp, "-name", "._*", "-delete"], check=True)
        for d in (".fseventsd", ".Trashes", ".Spotlight-V100"):
            shutil.rmtree(os.path.join(mp, d), ignore_errors=True)
    finally:
        drive_qemu.detach(mp)


def ps(g, name):
    """The slots of the tasks named `name` in a fresh `ps`, or None when the
    listing did not come."""
    mark = len(g.transcript())
    g.type_line("ps")
    if not g.wait_for(r"task \d+: [^\r\n]*\r\n(?:[^#]*)# ", timeout=20):
        return None
    out = g.transcript()[mark:]
    return [int(n) for n in re.findall(rf"task (\d+): [^\r\n]*  {re.escape(name)}\r", out)]


def until(g, name, want, seconds):
    deadline = time.time() + seconds
    slots = ps(g, name)
    while not (slots is not None and want(slots)) and time.time() < deadline:
        time.sleep(1)
        slots = ps(g, name)
    return slots


def run_to_end(command):
    try:
        return subprocess.run(
            [sys.executable, os.path.join(HERE, "np9p_client.py"), "localhost", str(PORT), "run", command],
            capture_output=True, timeout=60).stdout.decode("latin-1")
    except subprocess.TimeoutExpired:
        return ""


def round_(g, label, command, address, reason):
    """Dispatch `command`, see it running, have netd restarted by a ping of
    `address`, and check the command went with the old netd."""
    name = command.split()[0]
    cmd = command.encode()
    frame, _nonce = client.request_frame(client.build_frame(client.NP_RUN, 0, [len(cmd), 0, 0, 0], cmd))
    s = socket.create_connection(("localhost", PORT), timeout=10)
    try:
        s.sendall(frame)
        running = until(g, name, lambda slots: len(slots) == 1, 20)
        if not check(running is not None and len(running) == 1,
                     f"{label}: the run's command is a task: `{name}` in slots {running}"):
            return
        before = len(g.transcript())
        g.type_line(f"ping {address}")
        ok = g.wait_for(RESTARTED, timeout=40)
        text = g.transcript()[before:]
        check(ok and reason in text, f"{label}: the kernel restarted server slot 4 ({reason!r} in its lines)")
        g.wait_for("netd: network server ready", timeout=30)
        g.type_line("")
        gone = until(g, name, lambda slots: slots == [], 20)
        text = g.transcript()[before:]
        ended = [int(n) for n in re.findall(ENDED, text)]
        check(ended == running, f"{label}: the kernel ended that task and no other with the server: lines for {ended}, want {running}")
        check(gone == [], f"{label}: the command is gone from `ps`: `{name}` in slots {gone}")
    finally:
        s.close()
    out = run_to_end("id")
    check("uid=" in out, f"{label}: `cpu id` runs to its end on the fresh netd: {out.strip()[:60]!r}")


def main():
    stage()
    g = drive_qemu.Guest(COPY, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT}-:564",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[server-children] ")
    try:
        if not check(g.run([("login:", "root"), ("assword", "root"), ("# ", "")]), "the guest boots to root's shell"):
            return failed
        check(RIG_LINE in g.transcript(), "netd is the restart rig's build")
        out = run_to_end("id")
        check("uid=" in out, f"before: `cpu id` runs to its end: {out.strip()[:60]!r}")
        round_(g, "fault, blocked", "recv", FAULT, "EL0 FAULT task=4")
        round_(g, "spin, blocked", "recv", SPIN, "slot 4 wedged")
        round_(g, "fault, running", "coreprobe hold", FAULT, "EL0 FAULT task=4")
        text = g.transcript()
        ended = len(re.findall(ENDED, text))
        check(ended == 3, f"three tasks were ended with a server in the whole boot, one a round: {ended}")
        restarts = [int(n) for n in re.findall(RESTARTED, text)]
        check(restarts == [1, 2, 3], f"netd was restarted three times, once a round: attempts {restarts}")
        check(g.proc.poll() is None, "the guest is alive at the end")
        faults = g.aborts()
        check(faults is not None, f"QEMU's trace was read ({faults} abort line(s), netd's two deliberate faults among them)")
    finally:
        transcript = g.transcript()
        g.stop()
        with open(os.path.join(ROOT, "build", "server-children.txt"), "w") as f:
            f.write(transcript)
        if os.path.exists(COPY):
            os.unlink(COPY)
    want = 3 + 3 * 5 + 4
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-server-children: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

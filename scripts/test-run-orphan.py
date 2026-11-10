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
kills nothing: a run that ends in the ordinary way is reaped by its `WAIT`.

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
KILLED = r"Ouroboros kernel: task (\d+) (?:killed|ended on core)"

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
        if how == "RST":
            # SO_LINGER with a zero timeout: the close of a peer that died.
            s.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        s.close()
        gone = until(g, lambda slots: slots == [], 20)
        check(gone == [], f"{how}: the command is gone once its connection is closed: `{COMMAND}` in slots {gone}")
        lines = [int(n) for n in re.findall(KILLED, g.transcript()[before:])]
        check(lines == running, f"{how}: the kernel ended that task and no other: kill lines for {lines}, want {running}")
    finally:
        s.close()


def main():
    shutil.copyfile(IMAGE, COPY)
    g = drive_qemu.Guest(COPY, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT}-:564",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[run-orphan] ")
    try:
        if not check(g.run([("login:", "root"), ("assword", "root"), ("# ", "")]), "the guest boots to root's shell"):
            return failed
        out = run_to_end("id")
        check("uid=" in out, f"before: `cpu id` runs to its end: {out.strip()[:80]!r}")
        cut_short(g, "FIN")
        cut_short(g, "RST")
        out = run_to_end("id")
        check("uid=" in out, f"after: `cpu id` still runs to its end: {out.strip()[:80]!r}")
        text = g.transcript()
        kills = len(re.findall(KILLED, text))
        check(kills == 2, f"two tasks were ended in the whole boot, the two cut short: {kills}")
        restarts = len(re.findall(r"restarted \(attempt", text))
        check(restarts == 0, f"no server was restarted: {restarts} restart line(s)")
        check(g.proc.poll() is None, "the guest is alive at the end")
    finally:
        transcript = g.transcript()
        g.stop()
        with open(os.path.join(ROOT, "build", "run-orphan.txt"), "w") as f:
            f.write(transcript)
        os.unlink(COPY)
    want = 12
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-run-orphan: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

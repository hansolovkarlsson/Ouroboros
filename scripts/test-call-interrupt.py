#!/usr/bin/env python3
"""An interrupted call's late answer goes nowhere, not to the next call.

    python3 scripts/test-call-interrupt.py    (or `make test-call-interrupt`, which builds the image)

The boot shell's Ctrl+C and Ctrl+\\ interrupt its call to a server
(`MSG_CALL`), but the server still has the request and answers it later.
Until 2026-10-09 that answer arrived while the shell waited in its NEXT call
to the same server and was taken as that call's answer. Now a server answers
with `MSG_REPLY` to the call it names (`SENDER_CALL`), and the kernel
delivers it only while the caller is still in that call (`tasks::
reply_target`). One boot with SLIRP, against a host 9P peer
(scripts/np9p_server.py) that holds every reply for 4.5 s, mounted at /mnt/s,
and a second such peer at /mnt/b (a peer serves one connection at a time,
so a request to the first would queue behind the one it holds):

1. `cd /mnt/s/SUB`, a builtin whose directory check is a call to netd, held
   open by the peer; Ctrl+C cuts it short, and the prompt is back at once.
2. `cd /mnt/b/NOPE`, a call to netd sent before SUB's answer comes, must
   fail on its own answer (no such directory on the peer), not succeed on
   SUB's "exists", which arrives while it waits.
3. `cd /mnt/s/SUB` and Ctrl+\\ again: the second interrupt key cuts a call
   short too.
4. `pwd` prints `/`: no late answer moved the shell.

Controls, each failing it: a reply delivered on "the caller is blocked
calling me" alone, the call number not compared (the bug: check 2's `cd`
takes SUB's "exists" and `pwd` prints `/mnt/b/NOPE`), and a call's wait not interruptible (checks 1 and 3).
The prompt checks read a snapshot 0.8 s after the key, far under the peer's
hold, and require the command's own echo before the prompt, so a wait the
key did not end cannot pass them. QEMU's own trace must hold no fault line.
About a minute. Run it whenever the message waits, the boot shell's
interrupt, MSG_CALL, MSG_REPLY or SENDER_CALL changes.
"""
import importlib.util
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-call-interrupt.txt")
PEER_PORTS = ("5642", "5643")
PEER_DELAY = 4.5     # under netd's 5 s REMOTE_DEADLINE_TICKS, so every request is answered
ETX, FS = b"\x03", b"\x1c"
PROMPT = "# "
# How soon after an interrupt key the prompt must be back: far under the hold.
PROMPT_BACK = 0.8


def main() -> int:
    if not os.path.exists(IMAGE):
        print(f"test-call-interrupt: {IMAGE} missing - run make image")
        return 2
    # Guest refuses a stale image too, but by then the peers are running; ask
    # first (scripts/srcid.py).
    drive_qemu.srcid.require_current(IMAGE, "test-call-interrupt: ")
    peer_logs = [open(os.path.join(ROOT, "build", f"np9p-{p}.log"), "w") for p in PEER_PORTS]
    peers = [
        subprocess.Popen(
            [sys.executable, os.path.join(HERE, "np9p_server.py"), p, "--delay", str(PEER_DELAY)],
            cwd=ROOT, stdout=log, stderr=subprocess.STDOUT,
        )
        for p, log in zip(PEER_PORTS, peer_logs)
    ]
    time.sleep(3)
    guest = drive_qemu.Guest(
        IMAGE, label="test-call-interrupt: ",
        extra_args=["-netdev", "user,id=net0", "-device", "virtio-net-device,netdev=net0"],
    )
    out = {}

    def interrupted(name, command, key):
        """Type `command`, then `key` while its call waits; whether the prompt
        is back PROMPT_BACK seconds later, read as a snapshot."""
        start = len(guest.transcript())
        guest.type_line(command)
        time.sleep(0.6)              # netd has the request, the peer holds it
        guest.type_raw(key)
        time.sleep(PROMPT_BACK)
        snap = guest.transcript()[start:]
        out[name] = snap
        # The command's echo must be there, and a prompt after it: a split
        # that found no echo would search the whole snapshot.
        back = command in snap and PROMPT in snap.split(command, 1)[1]
        return guest.wait_for(PROMPT, timeout=30), back

    def plain(name, command):
        start = len(guest.transcript())
        guest.type_line(command)
        ok = guest.wait_for(r"\n[^\n]*" + PROMPT, timeout=30)
        out[name] = guest.transcript()[start:]
        return ok

    results = []
    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        for port, mnt in zip(PEER_PORTS, ("/mnt/s", "/mnt/b")):
            if ok:
                guest.type_line(f"mount -r 10.0.2.2:{port} {mnt}")
                ok = guest.wait_for(PROMPT, timeout=30)
        if ok:
            ok, back = interrupted("sub", "cd /mnt/s/SUB", ETX)
            results.append(("Ctrl+C cuts the held call short", back))
        if ok:
            ok = plain("nope", "cd /mnt/b/NOPE")
            results.append(("the next call fails on its own answer, the late one refused",
                            "no such file" in out["nope"]))
        if ok:
            ok, back = interrupted("sub2", "cd /mnt/s/SUB", FS)
            results.append(("Ctrl+\\ cuts a held call short too", back))
        if ok:
            ok = plain("pwd", "pwd")
            lines = [l.strip() for l in out["pwd"].replace("\r", "").split("\n")]
            results.append(("pwd is /, not the NOPE a late answer would give", "/" in lines))
        time.sleep(drive_qemu.LINGER)
    finally:
        guest.stop()
        for peer, log in zip(peers, peer_logs):
            peer.kill()
            log.close()
    with open(TRANSCRIPT, "w") as f:
        f.write(guest.transcript())
    faults = drive_qemu.fault_line(guest)
    results.append((f"QEMU's trace: {faults}", "0 fault lines" in faults))
    if not ok:
        results.append(("the session reached its last step", False))
    fail = 0
    for name, good in results:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        fail |= not good
    if fail:
        print(f"     transcript in {TRANSCRIPT}")
        for label, text in out.items():
            print(f"     -- {label}: {text.strip()!r}")
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())

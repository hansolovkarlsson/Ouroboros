#!/usr/bin/env python3
"""An interrupted call's late answer is dropped, not taken by the next call.

    python3 scripts/test-call-interrupt.py    (or `make test-call-interrupt`, which builds the image)

The boot shell's Ctrl+C and Ctrl+\\ interrupt its call to a server
(`MSG_CALL`), but the server still has the request and answers it later.
Until 2026-10-09 that answer arrived while the shell waited in its NEXT call
to the same server and was taken as that call's answer. Now the kernel
records the abandoned call (`tasks::ABANDONED`); the next call to that
server waits, unsent, for the old answer (`WaitReason::Drain`), which is
dropped, and then runs. One boot with SLIRP, against a host 9P peer
(scripts/np9p_server.py) that holds every reply for 4.5 s, mounted at /mnt/s:

1. `cd /mnt/s/SUB`, a builtin whose directory check is a call to netd, held
   open by the peer; Ctrl+C cuts it short, and the prompt is back at once.
2. `cd /mnt/s/NOPE`, sent before SUB's answer comes, must fail on its own
   answer (no such directory on the peer), not succeed on SUB's "exists".
3. `cd /mnt/s/SUB` and Ctrl+C again, then `cd /mnt/s/NOPE`, which waits to
   drain SUB's answer, and Ctrl+\\ cuts that wait short too: the prompt is
   back at once.
4. `pwd` prints `/`: no late answer moved the shell.

Controls, each failing it: the abandoned call not recorded (the bug: check 2's
`cd` takes SUB's "exists"); the drain wait not interruptible (check 3's
prompt comes back only after SUB's answer); and the old answer not dropped
(refused instead, so check 2's `cd` waits for good). A first version of
this rig interrupted the drain before the plain `cd`, whose answer was then
the interrupted NOPE's, identical to its own, and the first control passed. The prompt checks read a snapshot one second after the key, far under
the peer's hold, so a wait the key did not end cannot pass them. QEMU's own
trace must hold no fault line. About a minute. Run it whenever the message
waits, the boot shell's interrupt, MSG_CALL or MSG_SEND changes.
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
PEER_PORT = "5642"
PEER_DELAY = 4.5     # under netd's 5 s REMOTE_DEADLINE_TICKS, so every request is answered
ETX, FS = b"\x03", b"\x1c"
PROMPT = "# "
# How soon after an interrupt key the prompt must be back: the drain check's
# snapshot falls about 3.8 s into SUB's 4.5 s hold.
PROMPT_BACK = 0.8


def main() -> int:
    for k in ("target/aarch64-unknown-none/release/netd.bin", "build/esp/EFI/BOOT/BOOTAA64.EFI"):
        k = os.path.join(ROOT, k)
        if os.path.exists(k) and os.path.getmtime(k) > os.path.getmtime(IMAGE):
            print(f"test-call-interrupt: {IMAGE} is older than {k} - run make image")
            return 2
    peer_log = open(os.path.join(ROOT, "build", f"np9p-{PEER_PORT}.log"), "w")
    peer = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "np9p_server.py"), PEER_PORT, "--delay", str(PEER_DELAY)],
        cwd=ROOT, stdout=peer_log, stderr=subprocess.STDOUT,
    )
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
        back = PROMPT in snap.split(command, 1)[-1]
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
        if ok:
            guest.type_line(f"mount -r 10.0.2.2:{PEER_PORT} /mnt/s")
            ok = guest.wait_for(PROMPT, timeout=30)
        if ok:
            ok, back = interrupted("sub", "cd /mnt/s/SUB", ETX)
            results.append(("Ctrl+C cuts the held call short", back))
        if ok:
            ok = plain("nope", "cd /mnt/s/NOPE")
            results.append(("the next call waits out the old answer and fails on its own",
                            "no such file" in out["nope"]))
        if ok:
            ok, _ = interrupted("sub2", "cd /mnt/s/SUB", ETX)
        if ok:
            ok, back = interrupted("drain", "cd /mnt/s/NOPE", FS)
            results.append(("Ctrl+\\ cuts the wait to drain the abandoned answer short", back))
        if ok:
            ok = plain("pwd", "pwd")
            lines = [l.strip() for l in out["pwd"].replace("\r", "").split("\n")]
            results.append(("pwd is /, not the NOPE a late answer would give", "/" in lines))
        time.sleep(drive_qemu.LINGER)
    finally:
        guest.stop()
        peer.kill()
        peer_log.close()
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

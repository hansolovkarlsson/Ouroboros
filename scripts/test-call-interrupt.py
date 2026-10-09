#!/usr/bin/env python3
"""A call's reply wait is not interrupted, so a late reply answers nobody.

    python3 scripts/test-call-interrupt.py    (or `make test-call-interrupt`, which builds the image)

The boot shell's Ctrl+C and Ctrl+\\ interrupt its waits for a child and for a
message, but since 2026-10-09 not a call's reply wait (`MSG_CALL`): the server
is still working on the request, and its reply, sent once the shell had moved
on, was delivered as the answer to the shell's NEXT call to that server. One
boot with SLIRP, against a host 9P peer (scripts/np9p_server.py) that holds
every reply for 3.5 s, mounted at /mnt/s:

1. `cd /mnt/s/SUB`, a builtin whose directory check is a call to netd, held
   open by the peer; Ctrl+C and Ctrl+\\ are typed on the serial line, and
   `echo ` and twelve `k`s on QEMU's USB keyboard (monitor `sendkey`), while
   it waits.
   The call must finish anyway: no interrupt message, and the prompt comes
   back only once the peer has answered.
2. The keys typed meanwhile were read ahead and kept, the two interrupt
   bytes among them ignored by the line editor: `q` and Enter after the
   prompt run `echo kkkkkkkkkkkkq`.
3. `cd /mnt/s/NOPE` must fail (no such directory on the peer).
4. `pwd` must print `/mnt/s/SUB`.

With the interrupt back on a call's reply wait (the mutation control: the
`from.is_some()` arm in `tasks.rs`'s `WaitReason::Message` poll removed), the
first `cd` is cut short, SUB's late "exists" answers the NOPE call, and `pwd`
prints `/mnt/s/NOPE`. With the call's wait not reading the keyboard at all
(the second control: that arm reduced to `None`), check 2 fails: QEMU's USB
keyboard queues 16 events, and 17 keys pressed while nothing reads it
overflow it (with six keys the control passed).
(Typed on the serial line they would survive, since QEMU holds a serial byte
until the guest reads it, and the control passed that way.) QEMU's own trace must hold no fault line. About a
minute. Run it whenever the message wait's poll, the boot shell's interrupt
or `MSG_CALL` changes.
"""
import importlib.util
import os
import re
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
# In build/, not a temp directory: macOS caps a socket path at 104 bytes.
MONITOR = os.path.join(ROOT, "build", "test-call-interrupt.monitor")
# test-kbd-queue's fast pace: 17 keys inside the peer's hold, with room.
KEY_DELAY = 0.12
WORD = "k" * 12
PEER_PORT = "5642"
PEER_DELAY = "3.5"   # under netd's 5 s REMOTE_DEADLINE_TICKS, so the call is answered
ETX, FS = b"\x03", b"\x1c"
PROMPT = "# "


def main() -> int:
    for k in ("target/aarch64-unknown-none/release/netd.bin", "build/esp/EFI/BOOT/BOOTAA64.EFI"):
        k = os.path.join(ROOT, k)
        if os.path.exists(k) and os.path.getmtime(k) > os.path.getmtime(IMAGE):
            print(f"test-call-interrupt: {IMAGE} is older than {k} - run make image")
            return 2
    peer_log = open(os.path.join(ROOT, "build", f"np9p-{PEER_PORT}.log"), "w")
    peer = subprocess.Popen(
        [sys.executable, os.path.join(HERE, "np9p_server.py"), PEER_PORT, "--delay", PEER_DELAY],
        cwd=ROOT, stdout=peer_log, stderr=subprocess.STDOUT,
    )
    time.sleep(3)
    if os.path.exists(MONITOR):
        os.remove(MONITOR)
    guest = drive_qemu.Guest(
        IMAGE, label="test-call-interrupt: ",
        extra_args=[
            "-netdev", "user,id=net0", "-device", "virtio-net-device,netdev=net0",
            "-device", "qemu-xhci,id=xhci0",
            "-device", "usb-kbd,bus=xhci0.0",
            "-monitor", f"unix:{MONITOR},server,nowait",
        ],
    )
    results = []
    try:
        ok = guest.wait_for("login:", timeout=120)
        ok = ok and guest.run([("", "root"), ("assword", "root")]) and guest.wait_for(PROMPT)
        if ok:
            guest.type_line(f"mount -r 10.0.2.2:{PEER_PORT} /mnt/s")
            ok = guest.wait_for(PROMPT, timeout=30)
        cd_start = len(guest.transcript())
        if ok:
            guest.type_line("cd /mnt/s/SUB")
            sent = time.time()
            time.sleep(0.6)          # netd has the request, the peer holds it
            guest.type_raw(ETX)
            time.sleep(0.3)
            guest.type_raw(FS)
            drive_qemu.sendkeys(MONITOR, ["e", "c", "h", "o", "spc", *WORD], KEY_DELAY, hold=10)
            ok = guest.wait_for(PROMPT, timeout=30)
            waited = time.time() - sent
        cd_out = guest.transcript()[cd_start:]
        kept_start = len(guest.transcript())
        if ok:
            guest.type_raw(b"q\n")
            ok = guest.wait_for(r"\n[^\n]*" + PROMPT, timeout=30)
        kept_out = guest.transcript()[kept_start:]
        nope_start = len(guest.transcript())
        if ok:
            guest.type_line("cd /mnt/s/NOPE")
            ok = guest.wait_for(PROMPT, timeout=30)
        nope_out = guest.transcript()[nope_start:]
        pwd_start = len(guest.transcript())
        if ok:
            guest.type_line("pwd")
            ok = guest.wait_for(r"\n[^\n]*" + PROMPT, timeout=30)
        pwd_out = guest.transcript()[pwd_start:]
        time.sleep(drive_qemu.LINGER)
    finally:
        guest.stop()
        peer.kill()
        peer_log.close()
    transcript = guest.transcript()
    with open(TRANSCRIPT, "w") as f:
        f.write(transcript)
    faults = drive_qemu.fault_line(guest)

    if not ok:
        print(f"FAIL the session did not reach its last step ({TRANSCRIPT})")
        return 1
    # The prompt after the first cd must wait for the peer: a cut-short call
    # returns at once, seconds before the 3.5 s hold ends.
    results.append(("the call ran to the peer's answer, Ctrl+C and Ctrl+\\ queued as bytes",
                    waited >= float(PEER_DELAY) - 1.0 and not re.search(r"interrupt|cd:", cd_out)))
    kept = [l.strip() for l in kept_out.replace("\r", "").split("\n")]
    results.append((f"keys typed during the call kept: `echo {WORD}`, then `q`, ran whole", WORD + "q" in kept))
    results.append(("cd to a missing remote directory fails", "cd:" in nope_out))
    lines = [l.strip() for l in pwd_out.replace("\r", "").split("\n")]
    results.append(("pwd is /mnt/s/SUB, not the NOPE a late reply would give", "/mnt/s/SUB" in lines))
    results.append((f"QEMU's trace: {faults}", "0 fault lines" in faults))
    fail = 0
    for name, good in results:
        print(f"{'ok  ' if good else 'FAIL'} {name}")
        fail |= not good
    if fail:
        print(f"     first cd waited {waited:.1f}s; transcript in {TRANSCRIPT}")
        for label, out in (("cd SUB", cd_out), ("kept", kept_out), ("cd NOPE", nope_out), ("pwd", pwd_out)):
            print(f"     -- {label}: {out.strip()!r}")
    return 1 if fail else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Drive the Ouroboros guest shell unattended, over QEMU's -nographic console.

    python3 scripts/drive-qemu.py <disk-image> 'WAIT@@TYPE' ['WAIT@@TYPE' ...]

Each argument is a step: wait for WAIT (a regex) to appear in *new* guest
output, then type TYPE followed by Enter. An empty TYPE just waits. Example:

    python3 scripts/drive-qemu.py build/espext2.img \\
        'login@@root' 'assword@@root' '# @@id' '# @@cat /etc/shadow'

Prints the full transcript, then a count of abort lines from QEMU's own `-d int`
trace - the health bar that is independent of anything the guest prints.

WHY THIS EXISTS, AND WHY IT IS FUSSY

The guest reads its console through a PL011, which has **no RX FIFO**: a byte
arriving while the guest is not looking is not queued, it is GONE. Piping a
script straight into QEMU therefore loses most of it. Two rules follow, and both
are load-bearing rather than defensive:

1. **Type one character at a time, with a delay** (TYPE_DELAY). A burst is
   dropped after the first byte.
2. **Wait for the prompt, and only match output produced SINCE the last step.**
   Searching the whole buffer matches a *previous* prompt and starts typing
   while the guest is still printing - which silently eats the first characters
   of the command, so `cat /etc/shadow` arrives as `t /etc/shadow` and the test
   appears to fail for a reason that has nothing to do with the code. This
   version tracks a high-water mark for exactly that reason.

A settle delay after each match (SETTLE) covers the same hazard for the tail of
a prompt that is still draining.

See docs/testing-qemu.md, and the memory note on QEMU stdin driving the guest
shell. The technique is what makes `cpu`, login, and permission behaviour
testable without a human at the keyboard.
"""
import os
import re
import subprocess
import sys
import threading
import time

TYPE_DELAY = 0.02   # per character; below this the PL011 drops input
SETTLE = 0.6        # after a prompt matches, before typing
TIMEOUT = 90        # per step
LINGER = 4          # after the last step, to capture trailing output


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    image = sys.argv[1]
    steps = []
    for a in sys.argv[2:]:
        wait, _, text = a.partition("@@")
        steps.append((wait, text))

    prefix = subprocess.run(
        ["brew", "--prefix", "qemu"], capture_output=True, text=True
    ).stdout.strip()
    ovmf = os.path.join(prefix, "share/qemu/edk2-aarch64-code.fd")
    intlog = os.path.join(os.path.dirname(os.path.abspath(image)), "qemu-int.log")

    cmd = [
        "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a72",
        "-m", "512M", "-bios", ovmf,
        "-drive", f"file={image},format=raw,if=none,id=hd0",
        "-device", "virtio-blk-device,drive=hd0",
        "-device", "virtio-rng-device",
        "-global", "virtio-mmio.force-legacy=false",
        "-nographic", "-d", "int", "-D", intlog,
    ]

    proc = subprocess.Popen(
        cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, bufsize=0,
    )
    buf = bytearray()
    lock = threading.Lock()

    def reader():
        while True:
            b = proc.stdout.read(1)
            if not b:
                break
            with lock:
                buf.extend(b)

    threading.Thread(target=reader, daemon=True).start()

    seen = 0  # high-water mark: never match output from a previous step

    def wait_for(pattern):
        nonlocal seen
        rx = re.compile(pattern.encode())
        deadline = time.time() + TIMEOUT
        while time.time() < deadline:
            with lock:
                cur = bytes(buf)
            if rx.search(cur, seen):
                time.sleep(SETTLE)
                with lock:
                    seen = len(buf)
                return True
            time.sleep(0.15)
        return False

    def type_line(text):
        for ch in text.encode() + b"\n":
            proc.stdin.write(bytes([ch]))
            proc.stdin.flush()
            time.sleep(TYPE_DELAY)

    ok = True
    for pattern, text in steps:
        if pattern and not wait_for(pattern):
            print(f"\n!!! TIMEOUT waiting for {pattern!r}\n", file=sys.stderr)
            ok = False
            break
        if text:
            type_line(text)

    time.sleep(LINGER)
    proc.kill()
    proc.wait()

    print(bytes(buf).decode("utf-8", "replace"))
    if os.path.exists(intlog):
        with open(intlog, errors="replace") as fh:
            aborts = sum(1 for line in fh if "Abort" in line or "SError" in line)
        print(f"\n--- qemu -d int: {aborts} abort lines ---")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

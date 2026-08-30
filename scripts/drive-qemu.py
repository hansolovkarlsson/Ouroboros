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


class Guest:
    """One QEMU guest, driven over its -nographic console.

    The paced typing and the "match only NEW output" high-water mark are the
    load-bearing parts (see the module docstring); anything driving a guest
    should reuse this rather than copy them.
    """

    def __init__(self, image, extra_args=(), intlog=None, label=""):
        prefix = subprocess.run(
            ["brew", "--prefix", "qemu"], capture_output=True, text=True
        ).stdout.strip()
        ovmf = os.path.join(prefix, "share/qemu/edk2-aarch64-code.fd")
        self.label = label
        self.intlog = intlog or os.path.join(
            os.path.dirname(os.path.abspath(image)), "qemu-int.log"
        )
        cmd = [
            "qemu-system-aarch64", "-machine", "virt", "-cpu", "cortex-a72",
            "-m", "512M", "-bios", ovmf,
            "-drive", f"file={image},format=raw,if=none,id=hd0",
            "-device", "virtio-blk-device,drive=hd0",
            "-device", "virtio-rng-device",
            "-global", "virtio-mmio.force-legacy=false",
            *extra_args,
            "-nographic", "-d", "int", "-D", self.intlog,
        ]
        self.proc = subprocess.Popen(
            cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, bufsize=0,
        )
        self.buf = bytearray()
        self.lock = threading.Lock()
        self.seen = 0
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        while True:
            b = self.proc.stdout.read(1)
            if not b:
                break
            with self.lock:
                self.buf.extend(b)

    def wait_for(self, pattern, timeout=TIMEOUT):
        rx = re.compile(pattern.encode())
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self.lock:
                cur = bytes(self.buf)
            if rx.search(cur, self.seen):
                time.sleep(SETTLE)
                with self.lock:
                    self.seen = len(self.buf)
                return True
            time.sleep(0.15)
        return False

    def type_line(self, text):
        for ch in text.encode() + b"\n":
            self.proc.stdin.write(bytes([ch]))
            self.proc.stdin.flush()
            time.sleep(TYPE_DELAY)

    def run(self, steps):
        """Each step is (wait_pattern, text_to_type). Returns True if all matched."""
        for pattern, text in steps:
            if pattern and not self.wait_for(pattern):
                self.report(f"!!! TIMEOUT waiting for {pattern!r}")
                return False
            if text:
                self.type_line(text)
        return True

    def report(self, msg):
        # stderr: a diagnostic interleaved into the transcript on stdout is
        # indistinguishable from guest output when the transcript is read later.
        print(f"{self.label}{msg}" if self.label else msg, file=sys.stderr)

    def transcript(self):
        with self.lock:
            return bytes(self.buf).decode(errors="replace")

    def aborts(self):
        """Count fault LINES in QEMU's own -d int trace, or None if there is no
        trace to read.

        Three properties, each of which this lost once in a refactor and each of
        which makes the number mean less when it is missing:

        - **`SError` counts.** It is the asynchronous fault class a DMA or MMU
          bug produces - including a server overrunning its guard page - so
          dropping it turns exactly the runs worth catching into clean ones.
          docs/testing-qemu.md defines the bar as
          `Data Abort|Prefetch Abort|SError`, and this must not disagree with it.
        - **Lines, not substring occurrences.** Two faults reported on one line
          are two faults, and one line mentioning `Abort` twice is not.
        - **A missing log is `None`, not `0`.** No trace is "I did not check",
          which reads identically to "I checked and it was clean" if both are 0 -
          and that is the single signal a two-node run has.
        """
        try:
            with open(self.intlog, "rb") as fh:
                return sum(
                    1 for line in fh
                    if b"Abort" in line or b"SError" in line
                )
        except OSError:
            return None

    def stop(self):
        self.proc.kill()
        self.proc.wait()


def fault_line(guest) -> str:
    """The health bar, worded so a missing trace never reads as a clean run."""
    n = guest.aborts()
    if n is None:
        return "NO TRACE (health bar unavailable - this is not a pass)"
    return f"{n} fault lines (Abort/SError)"


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    image = sys.argv[1]
    steps = []
    for a in sys.argv[2:]:
        wait, _, text = a.partition("@@")
        steps.append((wait, text))

    g = Guest(image)
    try:
        ok = g.run(steps)
        time.sleep(LINGER)
        transcript = g.transcript()
    finally:
        # BEFORE reading the trace (QEMU buffers it) and before printing, which
        # can raise BrokenPipeError under `| head` and would otherwise leak a
        # live QEMU still holding the image's write lock.
        g.stop()
    print(transcript)
    print(f"\n--- qemu -d int: {fault_line(g)} ---")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

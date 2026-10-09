#!/usr/bin/env python3
"""Drive the Ouroboros guest shell unattended, over QEMU's -nographic console.

    python3 scripts/drive-qemu.py [--slirp] <disk-image> 'WAIT@@TYPE' [...]

Each argument is a step: wait for WAIT (a regex) to appear in *new* guest
output, then type TYPE followed by Enter. An empty TYPE just waits; a TYPE of
`<ENTER>` types an empty line (a bare Enter), which is how a prompt that must
REFUSE an empty answer gets tested. Example:

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

See docs/testing/testing-qemu.md, and the memory note on QEMU stdin driving the guest
shell. The technique is what makes `cpu`, login, and permission behaviour
testable without a human at the keyboard.
"""
import os
import re
import subprocess
import sys
import threading
import time
import importlib.util

# The stale-image guard (scripts/srcid.py), loaded by path: the rigs load this
# file by path too, so scripts/ need not be on sys.path.
_spec = importlib.util.spec_from_file_location(
    "srcid", os.path.join(os.path.dirname(os.path.abspath(__file__)), "srcid.py"))
srcid = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(srcid)

ENTER = "<ENTER>"   # a step whose TYPE is this types an EMPTY line (bare Enter)
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

    def __init__(self, image, extra_args=(), intlog=None, label="", virtio_disk=True, machine="virt", stamp_from=None):
        """`virtio_disk=False` leaves `image` off the virtio-blk bus, for a
        caller that attaches it some other way (test-usb-hub.py boots it
        from a USB stick); `image` still names where the QEMU trace goes.
        `machine` is QEMU's -machine value: `virt,virtualization=on` makes
        the firmware hand the kernel off at EL2, as the Raspberry Pi's does
        (test-early-fault.py --el2, the EL1 drop's rig). `stamp_from` is what
        the stale-image guard reads when `image` is not what boots: the ESP
        directory a caller attaches through vvfat (test-early-fault.py)."""
        # Every boot of an image goes through here, so this is the one place
        # that refuses an image not built from the tree as it is now.
        srcid.require_current(stamp_from or image, label)
        prefix = subprocess.run(
            ["brew", "--prefix", "qemu"], capture_output=True, text=True
        ).stdout.strip()
        ovmf = os.path.join(prefix, "share/qemu/edk2-aarch64-code.fd")
        self.label = label
        self.intlog = intlog or os.path.join(
            os.path.dirname(os.path.abspath(image)), "qemu-int.log"
        )
        cmd = [
            "qemu-system-aarch64", "-machine", machine, "-cpu", "cortex-a72",
            "-m", "512M", "-bios", ovmf,
            *([
                "-drive", f"file={image},format=raw,if=none,id=hd0",
                "-device", "virtio-blk-device,drive=hd0",
            ] if virtio_disk else []),
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
        self.type_raw(text.encode() + b"\n")

    def type_raw(self, data):
        """Bytes on the serial line, paced at TYPE_DELAY, no Enter added: the
        one copy of the pacing every rig types with (six rigs had their own
        until 2026-10-08, the fourth high review of #240)."""
        for ch in data:
            self.proc.stdin.write(bytes([ch]))
            self.proc.stdin.flush()
            time.sleep(TYPE_DELAY)

    def run(self, steps):
        """Each step is (wait_pattern, text_to_type). Returns True if all matched.

        An empty TYPE waits without typing. To type an EMPTY LINE - a bare
        Enter - use the literal `<ENTER>`, which is what makes "the tool must
        refuse an empty answer" testable at all; without it every
        empty-input rule (passwd's and useradd's empty-password refusal) was
        unreachable from this harness, so the check guarding it could not fail.
        """
        for pattern, text in steps:
            if pattern and not self.wait_for(pattern):
                self.report(f"!!! TIMEOUT waiting for {pattern!r}")
                return False
            if text == ENTER:
                self.type_line("")
            elif text:
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
          docs/testing/testing-qemu.md defines the bar as
          `Data Abort|Prefetch Abort|SError`, and this must not disagree with it.
        - **Lines, not substring occurrences.** Two faults reported on one line
          are two faults, and one line mentioning `Abort` twice is not.
        - **A missing log is `None`, not `0`.** No trace is "I did not check",
          which reads identically to "I checked and it was clean" if both are 0 -
          and that is the single signal a two-node run has.

        It STOPS the guest first. QEMU buffers the trace, so a count read while
        it runs can miss the last faults, and every rig used to choose its own
        order (several read it before stopping). Stopping here makes the early
        read impossible to write (the third review of #226); `stop()` is
        idempotent and `transcript()` still works after it.
        """
        self.stop()
        try:
            with open(self.intlog, "rb") as fh:
                return sum(
                    1 for line in fh
                    if b"Abort" in line or b"SError" in line
                )
        except OSError:
            return None

    def stop(self):
        """Shut QEMU down, gracefully first: on SIGTERM it exits through its
        own shutdown, which flushes the -d int log, where SIGKILL (all this
        did until 2026-10-06) drops whatever the log still had buffered, a
        fault in the last command among it. Killed only if it does not go
        within five seconds. Safe to call more than once."""
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()


def sendkeys(monitor, names, delay, hold=None):
    """Keys on QEMU's USB keyboard: each name through the monitor's `sendkey`
    on the unix socket at `monitor`, `delay` seconds apart; `hold` is
    sendkey's hold time in ms (QEMU's default is 100). The one copy of the
    monitor loop the USB-keyboard rigs share."""
    import socket
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as mon:
        mon.connect(monitor)
        for name in names:
            mon.sendall(f"sendkey {name}{'' if hold is None else f' {hold}'}\n".encode())
            time.sleep(delay)


def fault_line(guest) -> str:
    """The health bar, worded so a missing trace never reads as a clean run."""
    return fault_text(guest.aborts())


def fault_text(n) -> str:
    """`fault_line` for a count already read, so a caller that needs the
    number too reads the trace once."""
    if n is None:
        return "NO TRACE (health bar unavailable - this is not a pass)"
    return f"{n} fault lines (Abort/SError)"


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    argv = sys.argv[1:]

    # `--slirp` gives the guest QEMU user-mode networking, so it can reach a
    # server on the host at 10.0.2.2 - which is how the guest-signs /
    # host-verifies half of the cluster is tested. Without it this harness has no
    # networking at all, which is why that half was never run unattended.
    # `--hostfwd=tcp::5640-:564` (repeatable) forwards a host port into the
    # guest, so a host-side peer can reach the guest's export or HTTP server
    # while the harness drives the shell - the two-party checks (a remote `cpu`
    # run from the host, then a command typed in the guest) need both at once.
    # Implies `--slirp`.
    extra = ()
    hostfwd = [a[len("--hostfwd="):] for a in argv if a.startswith("--hostfwd=")]
    argv = [a for a in argv if not a.startswith("--hostfwd=")]
    if "--slirp" in argv or hostfwd:
        if "--slirp" in argv:
            argv.remove("--slirp")
        netdev = "user,id=net0" + "".join(f",hostfwd={h}" for h in hostfwd)
        extra = (
            "-netdev", netdev,
            "-device", "virtio-net-device,netdev=net0",
        )

    image = argv[0]
    steps = []
    for a in argv[1:]:
        wait, _, text = a.partition("@@")
        steps.append((wait, text))

    g = Guest(image, extra_args=extra)
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

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
import gzip
import os
import re
import shutil
import subprocess
import sys
import tempfile
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
        # `SMP=N` in the environment (`make test-kbd-mode SMP=4`) boots every
        # rig on N cores unless the rig names its own -smp: the keyboard,
        # kill and call rigs are the checks that would see a race between
        # cores (multi-core step 4(d), docs/roadmap/roadmap-smp.md).
        smp = os.environ.get("SMP")
        if smp and "-smp" not in extra_args:
            extra_args = [*extra_args, "-smp", smp]
            self.report(f"booting with -smp {smp} (SMP={smp})")
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
                count = sum(
                    1 for line in fh
                    if b"Abort" in line or b"SError" in line
                )
        except OSError:
            return None
        # Meant to name, in every rig's output, a boot the firmware never
        # handed over. It cannot: the guest is stopped above, so the
        # transcript's last line is QEMU's "terminating on signal 15", which
        # no STALL_LINES pattern matches, and this never prints. A rig that
        # grades stalls reads the transcript before it stops the guest
        # (boot_stalled); the rest say nothing. docs/ROADMAP.md, "Guest.aborts()
        # cannot name a firmware stall".
        if firmware_stalled(self.transcript(), count):
            self.report("QEMU's firmware never started the kernel (drive-qemu.py's firmware_stalled)")
        return count

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


# A boot QEMU's own firmware never handed to the kernel. The checks would fail
# for a kernel that never ran, so such a boot is INCONCLUSIVE: not a failed
# check and not a pass. test-usb-hub met the first stall on 2026-10-04 (the
# firmware stopped in its USB boot); the second, on 2026-10-21, stopped after
# `Image type X64 can't be loaded`, on NVMe boots and on a virtio one, where a
# boot that goes on prints `BdsDxe: loading` next. What causes either is
# unknown, so every boot is logged with its verdict and a stall reads as a
# rate: a change that makes it likelier shows as a rising count, which a retry
# until green would hide. One copy for every rig (docs/ROADMAP.md, "The rigs
# booting from NVMe grade a firmware stall as failed checks").
INCONCLUSIVE = 100   # a rig's exit status when a boot stalled and no check failed
_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
STALL_LOG = os.path.join(_ROOT, "scratch", "firmware-stall-runs.log")
STALL_DIR = os.path.join(_ROOT, "scratch", "firmware-stalls")
STALL_LINES = ("UsbBootExecCmd", "Image type X64 can't be loaded on AARCH64 UEFI system.")
_ESCAPE = re.compile(r"\x1b\[[0-9;=?]*[A-Za-z]")


def firmware_stalled(out, aborts):
    """True only for a boot QEMU's firmware never handed over: the kernel
    printed nothing, the fault trace was read and is clean (`aborts` is 0,
    not None), and the transcript's last line, the firmware's screen clears
    taken out, is one of STALL_LINES. Anything else, a kernel hanging before
    its first line included, is for the checks to fail."""
    if "Ouroboros kernel" in out or aborts != 0:
        return False
    lines = [line for line in _ESCAPE.sub("", out).splitlines() if line.strip()]
    return bool(lines) and any(stall in lines[-1] for stall in STALL_LINES)


def record_run(label, verdict, kept=None, log=None):
    """Appends this boot to `log` (STALL_LOG unless named) and returns
    (INCONCLUSIVE boots, all boots) for `label`, which holds no space."""
    log = log or STALL_LOG
    os.makedirs(os.path.dirname(log), exist_ok=True)
    line = f"{time.strftime('%Y-%m-%d %H:%M:%S')} {label} {verdict}"
    with open(log, "a") as fh:
        fh.write(line + (f" {os.path.relpath(kept, _ROOT)}" if kept else "") + "\n")
    with open(log) as fh:
        mine = [entry.split() for entry in fh if entry.split()[2:3] == [label]]
    return sum(1 for entry in mine if entry[3:4] == ["INCONCLUSIVE"]), len(mine)


def boot_stalled(rig, boot, out, aborts, log=None, kept_dir=None, label=None, qemu_log=None):
    """The verdict for one boot of `rig`: logs it, and for a stall keeps the
    transcript (a rerun overwrites the one in build/), prints the verdict with
    this boot's rate, and answers True, the caller then grading none of the
    boot's guest checks. A boot that stalled on each of its last
    PERSISTENT runs, this one included, is no random stall but a hang every
    run meets (a firmware stopped by the rig's own disk, say): it answers
    False, and the checks fail. `log`, `kept_dir` and `label` default to
    STALL_LOG, STALL_DIR and `rig:boot`; test-usb-hub keeps its own. When
    `qemu_log` is given, a stall keeps it beside the transcript, gzipped, with
    .log.gz for .txt: the trace of what the firmware did, which a rerun
    overwrites."""
    log, kept_dir = log or STALL_LOG, kept_dir or STALL_DIR
    label = label or f"{rig}:{boot}"
    if not firmware_stalled(out, aborts):
        record_run(label, "booted", log=log)
        return False
    os.makedirs(kept_dir, exist_ok=True)
    kept = os.path.join(kept_dir, f"{time.strftime('%Y%m%d-%H%M%S')}-{rig}-{boot}.txt")
    with open(kept, "w") as fh:
        fh.write(out)
    if qemu_log and os.path.exists(qemu_log):
        # Compressed: a disk-trace rig's log is megabytes of trace lines, and
        # kept stalls are never cleaned up.
        with open(qemu_log, "rb") as src, gzip.open(kept[:-len(".txt")] + ".log.gz", "wb") as dst:
            shutil.copyfileobj(src, dst)
    stalled, runs = record_run(label, "INCONCLUSIVE", kept, log=log)
    rate = (f"{stalled} of {runs} {label} boot(s) in {os.path.relpath(log, _ROOT)} stalled; "
            f"this one kept as {os.path.relpath(kept, _ROOT)}")
    if last_verdicts(label, PERSISTENT, log) == ["INCONCLUSIVE"] * PERSISTENT:
        print(f"FAIL {boot}: QEMU's firmware never started the kernel, on each of this boot's "
              f"last {PERSISTENT} runs: a hang every run meets, not a random stall, so its checks are graded")
        print(f"  {rate}")
        return False
    print(f"INCONCLUSIVE {boot}: QEMU's firmware never started the kernel; not a pass, run it again")
    print(f"  {rate}")
    return True


# Runs in a row a boot may stall before the stall is taken for a hang. The
# worst rate seen, test-usb-hub's usb-boot on 2026-10-04, was one in three, so
# three in a row by chance is one in 27 there and far rarer elsewhere.
PERSISTENT = 3


def last_verdicts(label, count, log=None):
    """The verdicts of `label`'s last `count` runs in `log`, oldest first."""
    try:
        with open(log or STALL_LOG) as fh:
            mine = [entry.split()[3] for entry in fh if entry.split()[2:3] == [label] and len(entry.split()) > 3]
    except OSError:
        return []
    return mine[-count:]


def verdict(rig, failed, stalled):
    """A rig's exit status: 1 for a failed check, INCONCLUSIVE for a stalled
    boot and none failed, else 0, with a line saying which."""
    if failed:
        print(f"{rig}: {failed} check(s) FAILED" + (f", and {stalled} boot(s) INCONCLUSIVE" if stalled else ""))
        return 1
    if stalled:
        print(f"{rig}: {stalled} boot(s) INCONCLUSIVE (QEMU's firmware never started the kernel) "
              "and no check failed: not a pass, run it again")
        return INCONCLUSIVE
    print(f"{rig}: all checks passed")
    return 0


# The 2026-10-04 stall (build/usb-hub-usb-boot.txt, 667 bytes, since
# overwritten by a rerun; transcribed from the session that saw it), its last
# line with the screen-control escapes it was captured with. The firmware's
# lines up to `Image type X64` are what every boot prints first.
FIRMWARE_HEAD = """UEFI firmware (version edk2-stable202408-prebuilt.qemu.org built at 16:28:50 on Sep 12 2024)
ArmTrngLib could not be correctly initialized.
Error: Image at 0005FDB6000 start failed: 00000001
Error: Image at 0005FD6D000 start failed: Not Found
Error: Image at 0005FCBA000 start failed: Unsupported
Error: Image at 0005FC3F000 start failed: Not Found
Error: Image at 0005FB65000 start failed: Aborted
Tpm2SubmitCommand - Tcg2 - Not Found
Tpm2GetCapabilityPcrs fail!
Tpm2SubmitCommand - Tcg2 - Not Found
Image type X64 can't be loaded on AARCH64 UEFI system.
"""
CLEARS = "\x1b[2J\x1b[01;01H\x1b[=3h\x1b[2J\x1b[01;01H\x1b[2J\x1b[01;01H\x1b[=3h\x1b[2J\x1b[01;01H"
USB_STALLED = FIRMWARE_HEAD + CLEARS + "UsbBootExecCmd: Success to Exec 0x0 Cmd (Result = 1)\n"
# The 2026-10-21 stall: 11 lines, the head and its screen clears, nothing
# after (the transcripts were overwritten by reruns; this is their shape as
# the roadmap records it, with a passing boot's own clears).
X64_STALLED = FIRMWARE_HEAD + CLEARS
# What a passing boot prints after the head, before the kernel's first line:
# a kernel that hung there would end the transcript on BdsDxe.
BDS_STARTED = (
    CLEARS + 'BdsDxe: loading Boot0001 "UEFI QEMU NVMe Ctrl boot 1" from PciRoot(0x0)/Pci(0x2,0x0)/NVMe(0x1,00-00-00-00-00-00-00-00)\n'
    "ConvertPages: failed to find range 140000000 - 14045FFFF\n"
    'BdsDxe: starting Boot0001 "UEFI QEMU NVMe Ctrl boot 1" from PciRoot(0x0)/Pci(0x2,0x0)/NVMe(0x1,00-00-00-00-00-00-00-00)\n'
)


def self_test():
    """The classifier on fixed transcripts: the two stalls are INCONCLUSIVE,
    and nothing that could be a kernel's own failure is."""
    cases = [
        ("the USB stall as captured", USB_STALLED, 0, True),
        ("the USB stall with trailing blank lines", USB_STALLED + "\n\n", 0, True),
        ("the X64 stall, its screen clears last", X64_STALLED, 0, True),
        ("the X64 stall, CR LF line ends", X64_STALLED.replace("\n", "\r\n"), 0, True),
        ("the X64 stall without the clears", FIRMWARE_HEAD, 0, True),
        ("the stall, but the fault trace has a fault", X64_STALLED, 1, False),
        ("the stall, but no fault trace to read", USB_STALLED, None, False),
        ("the kernel ran, then the firmware's line", USB_STALLED + "Ouroboros kernel: UEFI stage alive\n", 0, False),
        ("a kernel line, firmware line last", "Ouroboros kernel: UEFI stage alive\n" + X64_STALLED, 0, False),
        ("no kernel, a different last line", USB_STALLED + "Synchronous Exception at 0x5C5480EC\n", 0, False),
        ("the firmware started the kernel, which printed nothing", FIRMWARE_HEAD + BDS_STARTED, 0, False),
        ("the USB boot went on to start the kernel, which printed nothing", USB_STALLED + BDS_STARTED, 0, False),
        ("the firmware stopped partway through its head", FIRMWARE_HEAD.split("Tpm2GetCapabilityPcrs")[0], 0, False),
        ("an empty transcript", "", 0, False),
    ]
    failed = 0
    for name, out, aborts, want in cases:
        ok = firmware_stalled(out, aborts) == want
        print(f"{'ok  ' if ok else 'FAIL'} {name}: {'INCONCLUSIVE' if want else 'graded'}")
        failed += not ok
    # The persistent rule, on a log of its own: stalls answer INCONCLUSIVE
    # until PERSISTENT in a row, a boot between them starts the count again.
    import contextlib, io
    with tempfile.TemporaryDirectory() as tmp:
        log = os.path.join(tmp, "runs.log")
        def run(out):
            with contextlib.redirect_stdout(io.StringIO()):
                return boot_stalled("self", "test", out, 0, log=log, kept_dir=tmp)
        got = [run(X64_STALLED) for _ in range(PERSISTENT - 1)] + [run(FIRMWARE_HEAD + BDS_STARTED)]
        got += [run(X64_STALLED) for _ in range(PERSISTENT)] + [run(X64_STALLED)]
        want = [True] * (PERSISTENT - 1) + [False] + [True] * (PERSISTENT - 1) + [False, False]
        ok = got == want
        print(f"{'ok  ' if ok else 'FAIL'} a stall on each of the last {PERSISTENT} runs is graded, a boot between them resets the count")
        failed += not ok
    return failed


def attach(image, readonly, prefix="ouroboros-image."):
    """Mount a FAT32 image on the host; returns the mount point. The one
    copy the rigs that read or write an image's files share (test-cpp,
    test-edit, test-spawn-size), with `detach` below."""
    mp = tempfile.mkdtemp(prefix=prefix)
    args = ["hdiutil", "attach", "-nobrowse", "-mountpoint", mp, image]
    if readonly:
        args.insert(2, "-readonly")
    subprocess.run(args, check=True, capture_output=True)
    return mp


def detach(mp):
    """Detach, forcing it if macOS holds the volume (Spotlight, fseventsd), as
    the Makefile's image-stall does; the mount point is removed either way."""
    r = subprocess.run(["hdiutil", "detach", mp], capture_output=True)
    if r.returncode != 0:
        subprocess.run(["hdiutil", "detach", "-force", mp], check=True, capture_output=True)
    os.rmdir(mp)


def clang_objects(paths):
    """How many of the object files at `paths` clang made: each object clang
    writes carries its `clang version` string, and none that DevTools's cc and
    as write does (19 of 19 against 0 of 19 for cpp's and Edit's, 2026-10-16).
    The linked program keeps no trace of it, so the objects a link took are
    where a rig can see which compiler made a program, without taking the
    Makefile's word for it (the review of #304)."""
    n = 0
    for p in paths:
        with open(p, "rb") as fh:
            n += b"clang version" in fh.read()
    return n


def required_env(name, rig):
    """A value only the Makefile gives a rig: exit naming the target if it is
    missing, rather than guess one (a guess compares against the wrong build)."""
    value = os.environ.get(name)
    if not value:
        sys.exit(f"{rig}: {name} is not set - run it as `make {rig}`, which passes it")
    return value


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2

    argv = sys.argv[1:]
    if argv == ["--self-test"]:
        return self_test()

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

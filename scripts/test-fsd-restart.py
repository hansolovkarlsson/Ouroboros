#!/usr/bin/env python3
"""fsd is restarted by the supervisor, and serves again after.

The kernel keeps every supervised server's image from the boot so it can
restart one that faults or wedges without a filesystem to load it from
(`kernel/src/supervisor.rs`); fsd is the server that matters most, since it
is the filesystem. Until 2026-11-16 nothing exercised fsd's restart: its
image had outgrown the 192 KB slot the supervisor kept it in, so on every
boot of the Pi it ran unsupervised, said by one warning and nothing else
(and once before, at 137 KB against 128 KB, the review-and-split
postmortem). `scripts/check-supervised-images.py` now holds the built images
to the supervisor's arena at every `make esp`; this rig is the other half,
the restart itself, end to end.

No request makes the shipped fsd fault or spin, so the boot runs fsd's
`restart-rig` build, staged on a copy of esp.img: any request whose path
ends in `restart-rig-fault` faults it and `restart-rig-spin` spins it, the two ways
the supervisor comes to restart a server. One boot, two rounds. In each:

- the kernel must say it restarted server slot 2, and for which reason;
- a fresh fsd must say it is ready;
- a listing of `/EFI/ORBS` must name FSD.BIN again, from the fresh fsd.

And over the boot: no line saying an image was too large to keep, the
restarts numbered 1 and 2, and the guest alive at the end.

The control, run by hand on 2026-11-16 (the PR records it): the same rig
with the old per-image limit put back in `supervisor::register` (192 KB,
which fsd's 197 KB passed), a mutation the build's check does not see. Eight
of the then thirteen checks failed: the image-too-large line, no restart in
either round, no fresh fsd, and no listing after (the shell could not find
`ls`, the filesystem being gone). A thirteenth check, that the kernel never
said it had no kept image, passed in the control too (an unregistered server
never reaches that line), so it could not fail and was removed.

Usage:  make test-fsd-restart      (builds the image and the rig's fsd)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")

IMAGE = os.path.join(ROOT, "build", "esp.img")
COPY = os.path.join(ROOT, "build", "fsd-restart.img")
# The rig's fsd, named by the Makefile, which rebuilds it first.
RIG_FSD = os.environ.get("FSD_RESTART_BIN")
RIG_LINE = "fsd: the restart rig's build"
READY = "fsd: filesystem server ready"
RESTARTED = r"server slot 2 restarted \(attempt (\d+)"
TOO_LARGE = "image too large to keep for crash recovery"

failed = 0
checks = 0


def check(ok, what):
    global failed, checks
    checks += 1
    failed += 0 if ok else 1
    print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)
    return ok


def stage():
    """COPY: esp.img with \\EFI\\ORBS\\FSD.BIN the rig's build, the one file
    changed (what macOS leaves on a volume it mounts is removed, as the
    Makefile's image recipes do)."""
    if not RIG_FSD or not os.path.exists(RIG_FSD):
        sys.exit("test-fsd-restart is run by `make test-fsd-restart`, which builds the rig's fsd and names it")
    shutil.copyfile(IMAGE, COPY)
    mp = drive_qemu.attach(COPY, readonly=False)
    try:
        shutil.copyfile(RIG_FSD, os.path.join(mp, "EFI", "ORBS", "FSD.BIN"))
        subprocess.run(["find", mp, "-name", "._*", "-delete"], check=True)
        for d in (".fseventsd", ".Trashes", ".Spotlight-V100"):
            shutil.rmtree(os.path.join(mp, d), ignore_errors=True)
    finally:
        drive_qemu.detach(mp)


def lists_fsd(g, label):
    """A listing of /EFI/ORBS, which must name FSD.BIN: fsd answering."""
    mark = len(g.transcript())
    g.type_line("ls /EFI/ORBS")
    g.wait_for(r"\r\n# ", timeout=30)
    out = g.transcript()[mark:]
    return check("FSD.BIN" in out, f"{label}: `ls /EFI/ORBS` names FSD.BIN: {out.strip()[-80:]!r}")


def round_(g, label, path, reason):
    """Open `path`, have fsd restarted, and check a fresh one serves."""
    before = len(g.transcript())
    g.type_line(f"cat {path}")
    ok = g.wait_for(RESTARTED, timeout=40)
    text = g.transcript()[before:]
    check(ok and reason in text, f"{label}: the kernel restarted server slot 2 ({reason!r} in its lines)")
    # The fresh fsd's line may have come with the restart's: read the whole
    # transcript since the round began, not only what arrives next.
    if READY not in g.transcript()[before:]:
        g.wait_for(READY, timeout=30)
    check(READY in g.transcript()[before:], f"{label}: a fresh fsd says it is ready")
    g.type_line("")
    g.wait_for(r"\r\n# ", timeout=20)
    lists_fsd(g, label)


def main():
    stage()
    g = drive_qemu.Guest(COPY, label="[fsd-restart] ")
    try:
        if not check(g.run([("login:", "root"), ("assword", "root"), ("# ", "")]), "the guest boots to root's shell"):
            return failed
        text = g.transcript()
        check(RIG_LINE in text, "fsd is the restart rig's build")
        check(TOO_LARGE not in text, "no supervised image was too large to keep")
        lists_fsd(g, "before")
        round_(g, "fault", "/restart-rig-fault", "EL0 FAULT task=2")
        round_(g, "spin", "/restart-rig-spin", "slot 2 wedged")
        text = g.transcript()
        restarts = [int(n) for n in re.findall(RESTARTED, text)]
        check(restarts == [1, 2], f"fsd was restarted twice, once a round: attempts {restarts}")
        check(g.proc.poll() is None, "the guest is alive at the end")
    finally:
        transcript = g.transcript()
        g.stop()
        with open(os.path.join(ROOT, "build", "fsd-restart.txt"), "w") as f:
            f.write(transcript)
        if os.path.exists(COPY):
            os.unlink(COPY)
    want = 4 + 2 * 3 + 2
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-fsd-restart: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

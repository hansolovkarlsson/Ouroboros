#!/usr/bin/env python3
"""A supervised server is restarted by the supervisor, and serves again after.

The kernel keeps every supervised server's image from the boot so it can
restart one that faults or wedges without a filesystem to load it from
(`kernel/src/supervisor.rs`). That is the reason the drivers are servers at
all. Until 2026-11-16 nothing exercised it for fsd, whose image had outgrown
the slot it was kept in, so on every boot of the Pi it ran unsupervised, said
by one warning (and once before, at 137 KB against 128 KB, the
review-and-split postmortem). `scripts/check-supervised-images.py` holds the
built images to the supervisor's arena at every `make esp`; this rig is the
other half, the restart itself, end to end, one server a run:

    python3 scripts/test-server-restart.py fsd|cond|accountd

(`make test-fsd-restart`, `test-cond-restart`, `test-accountd-restart`, which
build the server's rig and name it in RIG_BIN.) netd's restart is
`test-server-children`, which checks the children it ends as well.

No request makes a shipped server fault or spin, so the boot runs the
server's `restart-rig` build, staged on a copy of esp.img, each with a
trigger only it receives:

- fsd: any request whose path ends in `restart-rig-fault` or
  `restart-rig-spin` (`cat /restart-rig-fault`);
- cond: a write that is the word and a line end. Typing it would reach cond
  a key at a time through the shell's echo, so three files are staged on the
  image's root and `cat`ed: RIGFAULT.TXT, RIGSPIN.TXT, and RIGECHO.TXT, whose
  `restart-rig-echo` cond answers with a line of its own (the serving check
  below);
- accountd: a request naming the account (`passwd restart-rig-fault` as
  root, after its two prompts).

One boot, two rounds, a fault and then a spin. In each:

- the kernel must say it restarted the server's slot, and why (`EL0 FAULT
  task=N`, `slot N wedged`);
- a fresh server must say it is ready;
- the server must answer a request only it can answer: fsd a listing of
  `/EFI/ORBS` naming FSD.BIN, cond a line of its own for a third staged
  file (`restart-rig-echo`: any other text is printed by the kernel when
  cond is gone), accountd `passwd`'s `no such user` for an account that does
  not exist.

And over the boot: no line saying an image was too large to keep, the
restarts numbered 1 and 2, and the guest alive at the end.

The control, run by hand on 2026-11-16 against fsd (#473 records it): the old
192 KB per-image limit put back in `supervisor::register`, a mutation the
build's check does not see. Eight of the then thirteen checks failed: the
image-too-large line, no restart in either round, no fresh fsd, and no
listing after (the shell could not find `ls`, the filesystem being gone). A
thirteenth check, that the kernel never said it had no kept image, passed in
the control too (an unregistered server never reaches that line), so it
could not fail and was removed. cond's and accountd's controls, the same day,
refused their slot in `register`: 7 of the then 12 checks failed for each. cond's
first control failed only 5: its check then, that an `echo` printed, passed
with cond dead, which is why cond now answers a word of its own.

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
TOO_LARGE = "image too large to keep for crash recovery"
PROMPT = r"\r\n# "


def cat(path):
    def go(g):
        g.type_line(f"cat {path}")
    return go


def passwd(name):
    """`passwd NAME` as root: two prompts, then the request to accountd. A
    prompt that does not come is its own failed check, so a trigger that
    never reached accountd is not reported as a restart that did not
    happen; nothing more is typed into a shell that is not asking."""
    def go(g):
        g.type_line(f"passwd {name}")
        for prompt in ("New password", "Retype"):
            if not check(g.wait_for(prompt, timeout=20), f"`passwd {name}` prompts {prompt!r}"):
                return False
            g.type_line("rigpass")
        return True
    return go


def lists_fsd(g):
    mark = len(g.transcript())
    g.type_line("ls /EFI/ORBS")
    g.wait_for(PROMPT, timeout=30)
    out = g.transcript()[mark:]
    return "FSD.BIN" in out, f"`ls /EFI/ORBS` names FSD.BIN: {out.strip()[-80:]!r}"


def answers(g):
    # Any text comes out whether cond is alive or not: `ulib::con_write`
    # prints a failed call's bytes through the kernel's PUTC (an `echo` did,
    # in the control, with cond gone). So the rig's cond answers a file
    # holding `restart-rig-echo` with a line only it writes.
    mark = len(g.transcript())
    g.type_line("cat /RIGECHO.TXT")
    g.wait_for(PROMPT, timeout=30)
    out = g.transcript()[mark:]
    return "cond: restart-rig-echo seen" in out, f"cond answers restart-rig-echo: {out.strip()[-80:]!r}"


def no_such_user(g):
    mark = len(g.transcript())
    if not passwd("nosuchaccount")(g):
        return False, "`passwd nosuchaccount` did not prompt"
    g.wait_for(PROMPT, timeout=30)
    out = g.transcript()[mark:]
    return "passwd: no such user" in out, f"`passwd nosuchaccount` says no such user: {out.strip()[-80:]!r}"


SERVERS = {
    "fsd": dict(
        slot=2, file="FSD.BIN", ready="fsd: filesystem server ready",
        fault=cat("/restart-rig-fault"), spin=cat("/restart-rig-spin"), serves=lists_fsd, files={},
    ),
    "cond": dict(
        slot=3, file="COND.BIN", ready="cond: console server ready",
        fault=cat("/RIGFAULT.TXT"), spin=cat("/RIGSPIN.TXT"), serves=answers,
        files={
            "RIGFAULT.TXT": b"restart-rig-fault\r\n",
            "RIGSPIN.TXT": b"restart-rig-spin\r\n",
            "RIGECHO.TXT": b"restart-rig-echo\r\n",
        },
    ),
    "accountd": dict(
        slot=5, file="ACCOUNTD.BIN", ready="accountd: account server ready",
        fault=passwd("restart-rig-fault"), spin=passwd("restart-rig-spin"), serves=no_such_user, files={},
    ),
}

failed = 0
checks = 0


def restarted(slot):
    """The kernel's line for a supervised restart of `slot`, its attempt
    number captured."""
    return rf"server slot {slot} restarted \(attempt (\d+)"


def check(ok, what):
    global failed, checks
    checks += 1
    failed += 0 if ok else 1
    print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)
    return ok


def stage(server, s, copy, rig_bin):
    """`copy`: esp.img with the server's image the rig's build, and the
    files its trigger needs (what macOS leaves on a volume it mounts is
    removed, as the Makefile's image recipes do)."""
    if not os.path.exists(rig_bin):
        sys.exit(f"test-{server}-restart: RIG_BIN {rig_bin} does not exist - `make test-{server}-restart` builds it")
    shutil.copyfile(IMAGE, copy)
    mp = drive_qemu.attach(copy, readonly=False)
    try:
        shutil.copyfile(rig_bin, os.path.join(mp, "EFI", "ORBS", s["file"]))
        for name, data in s["files"].items():
            with open(os.path.join(mp, name), "wb") as f:
                f.write(data)
        subprocess.run(["find", mp, "-name", "._*", "-delete"], check=True)
        for d in (".fseventsd", ".Trashes", ".Spotlight-V100"):
            shutil.rmtree(os.path.join(mp, d), ignore_errors=True)
    finally:
        drive_qemu.detach(mp)


def round_(g, server, s, label, trigger, reason):
    """Pull `trigger`, have the server restarted, and check a fresh one
    serves."""
    before = len(g.transcript())
    trigger(g)
    ok = g.wait_for(restarted(s["slot"]), timeout=40)
    text = g.transcript()[before:]
    check(ok and reason in text, f"{label}: the kernel restarted server slot {s['slot']} ({reason!r} in its lines)")
    # The fresh server's line may have come with the restart's: read the
    # whole transcript since the round began, not only what arrives next.
    if s["ready"] not in g.transcript()[before:]:
        g.wait_for(s["ready"], timeout=30)
    check(s["ready"] in g.transcript()[before:], f"{label}: a fresh {server} says it is ready")
    g.type_line("")
    g.wait_for(PROMPT, timeout=20)
    ok, what = s["serves"](g)
    check(ok, f"{label}: {what}")


def main(server):
    s = SERVERS[server]
    copy = os.path.join(ROOT, "build", f"{server}-restart.img")
    g = None
    try:
        # Inside the try, so a staging that fails part way still removes
        # its copy of the image.
        stage(server, s, copy, drive_qemu.required_env("RIG_BIN", f"test-{server}-restart"))
        g = drive_qemu.Guest(copy, label=f"[{server}-restart] ")
        if not check(g.run([("login:", "root"), ("assword", "root"), ("# ", "")]), "the guest boots to root's shell"):
            return failed
        text = g.transcript()
        check(f"{server}: the restart rig's build" in text, f"{server} is the restart rig's build")
        check(TOO_LARGE not in text, "no supervised image was too large to keep")
        ok, what = s["serves"](g)
        check(ok, f"before: {what}")
        round_(g, server, s, "fault", s["fault"], f"EL0 FAULT task={s['slot']}")
        round_(g, server, s, "spin", s["spin"], f"slot {s['slot']} wedged")
        text = g.transcript()
        restarts = [int(n) for n in re.findall(restarted(s["slot"]), text)]
        check(restarts == [1, 2], f"{server} was restarted twice, once a round: attempts {restarts}")
        check(g.proc.poll() is None, "the guest is alive at the end")
    finally:
        if g is not None:
            transcript = g.transcript()
            g.stop()
            with open(os.path.join(ROOT, "build", f"{server}-restart.txt"), "w") as f:
                f.write(transcript)
        if os.path.exists(copy):
            os.unlink(copy)
    # passwd's two prompt checks, each time it runs: before, and twice a round.
    want = 4 + 2 * 3 + 2 + (2 * 5 if server == "accountd" else 0)
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-{server}-restart: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    if len(sys.argv) != 2 or sys.argv[1] not in SERVERS:
        sys.exit(f"usage: test-server-restart.py {'|'.join(SERVERS)}")
    sys.exit(main(sys.argv[1]))

#!/usr/bin/env python3
"""The held-key checks of step 4 of docs/roadmap/roadmap-user-keys.md, driven.

`login` derives a user's cluster key and hands it to `netd`, which holds it
while the user is logged in. This boots the FAT32 image once per scenario,
drives the guest's console with drive-qemu.py's `Guest` (the paced typing is
load-bearing; see that file), and reads `netd`'s table FROM THE HOST between
steps: `np9p_client.py run "clusterkey held"` runs the diagnostic on the guest
as root through the export, which needs no keyboard. That is what lets a check
see the table while the boot shell sits at a login prompt, where nothing can be
typed.

The FAT32 image, because the nested shells are /EFI/ORBS/SH.BIN on the ESP,
which the ext2 image does not mount. Keys do not depend on file modes.

Usage:  python3 scripts/test-held-keys.py [scenario ...]   (default: all)
        make test-held-keys                                 (rebuilds the image first)

Exit status: the number of scenarios that failed.
"""
import importlib.util
import os
import re
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
PORT = 5647  # its own, so a hand-run `make run-image-9p` on 5640 can coexist
NESTED = "/EFI/ORBS/SH.BIN"


def held():
    """The uids netd holds keys for, sorted (a uid held twice appears twice),
    or None when the diagnostic did not answer."""
    out = subprocess.run(
        [sys.executable, os.path.join(HERE, "np9p_client.py"), "localhost", str(PORT), "run", "clusterkey held"],
        capture_output=True, text=True, timeout=60,
    ).stdout
    m = re.search(r"held keys: (\d+)(?: \(([^)]*)\))?", out)
    if not m:
        return None
    uids = sorted(int(u) for u in re.findall(r"uid (\d+)", m.group(2) or ""))
    return uids if len(uids) == int(m.group(1)) else None


def login(user, prompt):
    """Steps that answer a login prompt already on screen."""
    return [("s", "login:", user), ("s", "assword", user), ("s", prompt, "")]


def nested_login(slot, user, prompt):
    """Start a nested shell from the shell holding the keyboard, hand it the
    keyboard with `fg`, and log in there. The user name follows `fg` with no
    wait: the nested prompt was printed BEFORE `fg`, so waiting for it again
    would match nothing new."""
    return [
        ("s", "", f"exec {NESTED}"),
        ("s", "login:", f"fg {slot}"),
        ("s", "", user),
        ("s", "assword", user),
        ("s", prompt, ""),
    ]


# Each scenario: a list of ("s", wait, type) console steps, ("h", uids, what)
# host checks of the table, and ("t", regex, what) checks of the transcript.
SCENARIOS = {
    # The step's check: a login holds its user's key, a logout drops it.
    "login-logout": [
        *login("user", r"\$ "),
        ("h", [1000], "a login holds the user's key"),
        ("s", "", "exit"),
        ("s", "login:", ""),
        ("h", [], "a logout drops it"),
    ],
    # Two logins of one user, one logs out, the key is still held; then the
    # other login's shell is killed and loses its key (the liveness check).
    "two-logins-and-kill": [
        *login("root", "# "),
        *nested_login(6, "user", r"\$ "),
        ("s", "", "fg 0"),
        ("s", "", "<ENTER>"),
        ("s", "# ", "exit"),
        *login("user", r"\$ "),
        ("h", [1000, 1000], "two logins of one user hold two keys"),
        ("s", "", "exit"),
        ("s", "login:", ""),
        ("h", [1000], "one logs out, and the other's key is still held"),
        # The prompt was matched above so the check could run; answer it
        # without waiting for it again.
        ("s", "", "root"),
        ("s", "assword", "root"),
        ("s", "# ", ""),
        ("s", "", "kill 6"),
        ("s", "# ", ""),
        ("h", [0], "a killed login shell loses its key"),
    ],
    # The ordinary user's side: keyprobe's refusals, and a nested shell started
    # BY A USER, whose login's hold comes from a non-root task and is refused.
    "non-root": [
        *login("user", r"\$ "),
        ("s", "", "keyprobe"),
        ("s", r"\$ ", ""),
        # Its EXIT CODE, the number of its own failed checks: "it reached its
        # last line" passed with [FAIL] lines above it, so the scenario caught
        # a broken refusal only through the host check.
        ("t", r"\$ keyprobe\r?\n(?:[^\n]*\n)*?[^\n]*task \d+ exited \(code 0\)", "keyprobe's checks all pass (exit 0)"),
        *nested_login(6, "user", r"\$ "),
        ("h", [1000], "a nested login run by a non-root shell holds nothing"),
    ],
    # A node with no realm derives no key, holds nothing, and logs in as it
    # always did: the realm is read only AFTER the password has been checked.
    "no-realm": [
        *login("root", "# "),
        ("s", "", "rm /etc/cluster/realm"),
        ("s", "# ", "exit"),
        *login("user", r"\$ "),
        ("s", "", "id"),
        ("s", r"uid=1000", ""),
        ("h", [], "a login on a node with no realm holds nothing"),
    ],
    # A fifth hold with four live keys is refused, never evicting, and that
    # login still succeeds. Four root logins: the boot shell and three nested.
    "full-table": [
        *login("root", "# "),
        *[step for slot in (6, 7, 8) for step in nested_login(slot, "root", "# ")],
        ("h", [0, 0, 0, 0], "four logins hold four keys"),
        *nested_login(9, "root", "# "),
        ("t", r"login: note: the cluster key is not held", "the fifth login says so"),
        ("h", [0, 0, 0, 0], "the fifth hold evicted nothing"),
    ],
}


def run(name, plan):
    # A FRESH COPY per scenario: the guest writes to the image it boots, and
    # `no-realm` deletes /etc/cluster/realm, so a later scenario booting the
    # same file ran with no realm and held nothing. Found by `full-table`
    # failing only when run after it.
    image = os.path.join(ROOT, "build", f"held-{name}.img")
    shutil.copyfile(IMAGE, image)
    g = drive_qemu.Guest(image, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT}-:564",
        "-device", "virtio-net-device,netdev=net0",
    ), label=f"[{name}] ")
    failures = 0
    try:
        for step in plan:
            if step[0] == "s":
                _, wait, text = step
                if not g.run([(wait, text)]):
                    print(f"[FAIL] {name}: timed out waiting for {wait!r}", flush=True)
                    failures += 1
                    break
            elif step[0] == "h":
                _, want, what = step
                time.sleep(1)
                got = held()
                ok = got == want
                print(f"[{'PASS' if ok else 'FAIL'}] {name}: {what}: held uids {got}, want {want}", flush=True)
                failures += 0 if ok else 1
            else:
                _, rx, what = step
                ok = re.search(rx, g.transcript()) is not None
                print(f"[{'PASS' if ok else 'FAIL'}] {name}: {what}", flush=True)
                failures += 0 if ok else 1
        # A run whose guest died proves nothing either way.
        alive = g.proc.poll() is None
        if not alive:
            print(f"[FAIL] {name}: the guest died", flush=True)
            failures += 1
        restarts = len(re.findall(r"restarted \(attempt", g.transcript()))
        if restarts:
            print(f"[FAIL] {name}: {restarts} server restart line(s)", flush=True)
            failures += 1
    finally:
        transcript = g.transcript()
        g.stop()
    with open(os.path.join(ROOT, "build", f"held-{name}.txt"), "w") as f:
        f.write(transcript)
    os.unlink(image)
    return failures


def main():
    names = sys.argv[1:] or list(SCENARIOS)
    failed = 0
    for n in names:
        if n not in SCENARIOS:
            sys.exit(f"unknown scenario {n!r} (known: {', '.join(SCENARIOS)})")
        f = run(n, SCENARIOS[n])
        failed += 1 if f else 0
    print(f"test-held-keys: {failed} of {len(names)} scenario(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""An export with no identity runs nothing, in either signed format.

A node that has authorized peers but no `/etc/cluster/id` cannot sign a
reply, so `netd` refuses every signed request before any verb runs
(`handle_9p`'s pre-gate). Until the high review of user-keys step 7 that gate
tested `AUTHNP03`'s magic alone, and an `AUTHNP05` frame was authenticated,
admitted and dispatched: on such a node `cpu ... id` ran and printed
`uid=1001(guest)`, the unsignable reply refused only afterwards.

This boots a copy of the ext2 image with `/etc/cluster/id` removed from its
root (debugfs, from e2fsprogs) and sends `cpu id` as `guest` in both formats.
Each must be answered with the denial line and nothing else.

Usage:  python3 scripts/test-no-identity.py      (make test-no-identity builds the image)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(spec)
spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "espext2.img")
COPY = os.path.join(ROOT, "build", "noid.img")
PART = os.path.join(ROOT, "build", "noid-part.img")
PORT = 5649
# The ext2 root's place in espext2.img: mkext2.py's layout (partition 1 at LBA
# 2048, 49152 sectors). Read back below rather than trusted: debugfs must find
# the file there to remove it.
OFF, SIZE = 2048 * 512, 49152 * 512
DENIED = "cpu: authentication failed"


def debugfs():
    prefix = subprocess.run(["brew", "--prefix", "e2fsprogs"], capture_output=True, text=True).stdout.strip()
    tool = os.path.join(prefix, "sbin", "debugfs")
    if not prefix or not os.access(tool, os.X_OK):
        sys.exit("test-no-identity: needs e2fsprogs' debugfs (brew install e2fsprogs)")
    return tool


def strip_identity():
    tool = debugfs()  # before any copy, so a missing tool leaves nothing behind
    shutil.copyfile(IMAGE, COPY)
    with open(COPY, "rb") as f:
        f.seek(OFF)
        open(PART, "wb").write(f.read(SIZE))
    if subprocess.run([tool, "-R", "stat /etc/cluster/id", PART], capture_output=True, text=True).stdout.find("Inode:") < 0:
        sys.exit("test-no-identity: /etc/cluster/id not found where the ext2 root should be; the layout moved")
    subprocess.run([tool, "-w", "-R", "rm /etc/cluster/id", PART], capture_output=True, check=True)
    with open(COPY, "r+b") as f:
        f.seek(OFF)
        f.write(open(PART, "rb").read())
    os.unlink(PART)


def main():
    try:
        strip_identity()
    except BaseException:
        for leftover in (COPY, PART):
            if os.path.exists(leftover):
                os.unlink(leftover)
        raise
    failed = 0
    g = drive_qemu.Guest(COPY, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{PORT}-:564",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[noid] ")
    try:
        closed = g.wait_for("export CLOSED - no /etc/cluster/id", timeout=90)
        print(f"[{'PASS' if closed else 'FAIL'}] the node says its export is closed for want of an identity", flush=True)
        failed += 0 if closed else 1
        for label, extra in (("AUTHNP05", ["--password=guest"]), ("AUTHNP03", [])):
            out = subprocess.run(
                [sys.executable, os.path.join(HERE, "np9p_client.py"), "localhost", str(PORT), "run", "id",
                 "--user", "guest", *extra], capture_output=True, timeout=60).stdout.decode("latin-1")
            ok = out.startswith(DENIED) and "uid=" not in out
            print(f"[{'PASS' if ok else 'FAIL'}] {label}: `cpu id` as guest is denied, nothing run: {out.strip()[:100]!r}",
                  flush=True)
            failed += 0 if ok else 1
        alive = g.proc.poll() is None
        print(f"[{'PASS' if alive else 'FAIL'}] the guest is alive at the end", flush=True)
        failed += 0 if alive else 1
    finally:
        g.stop()
        os.unlink(COPY)
    print(f"test-no-identity: {failed} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

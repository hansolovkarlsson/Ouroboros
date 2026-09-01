#!/usr/bin/env python3
"""Drive a TWO-NODE Ouroboros cluster unattended, over both guests' consoles.

    python3 scripts/drive-2vm.py IMAGE_A IMAGE_B \
        --a 'WAIT@@TYPE' ... --b 'WAIT@@TYPE' ...

Node A listens on a QEMU socket netdev, node B connects to it — the same shared
L2 link `make run-image-2vm-*` sets up, with the same MAC-derived addresses
(`…:0a` → 10.0.2.10, `…:0b` → 10.0.2.11). A's steps run first (it is the
exporter, so it usually just needs to reach a shell), then B's run while A stays
alive. Both transcripts and both abort counts are printed at the end.

Use the EXT2 images for anything about permissions: FAT32 records no mode, so
`fsd` has nothing to enforce and every remote request looks permitted there
regardless of who sent it. `make image-ext2` stages CLUSTER.KEY for exactly this
reason — without it the export is fail-closed and the rig cannot come up.

The console-typing rules (pace the input, match only NEW output) are inherited
from drive-qemu.py's `Guest`, deliberately rather than copied: they are the
load-bearing part and a second copy would drift from the first.
"""
import os
import sys
import time

# drive-qemu.py has a dash in its name, so it needs an explicit spec load.
import importlib.util

_spec = importlib.util.spec_from_file_location(
    "drive_qemu", os.path.join(os.path.dirname(os.path.abspath(__file__)), "drive-qemu.py")
)
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
Guest = drive_qemu.Guest

# A DIFFERENT port from the Makefile's two-node targets (12340 for the FAT32
# pair, 12341 for the ext2 pair), so a scripted run and a hand-run rig can
# coexist instead of failing as an unrelated guest-side timeout.
LINK_PORT = 12342

# Where each node's traffic is captured, relative to the IMAGE's directory - not
# the cwd. QEMU exits at startup if filter-dump cannot create its file, and that
# surfaces as "[A] did not reach its prompt", which points at the guest rather
# than at the path. The sibling -d int log is derived the same way for the same
# reason.
#
# Captured on every run, because "did it use the format I think it used?" cannot
# be answered from a transcript that only shows the operation succeeding.


def parse(argv):
    """Split argv into two images and the two guests' step lists.

    Validated rather than trusted: without --a/--b every argument would be read
    as an image name, and the run would boot both guests, type NOTHING, and exit
    0 - a green run that verified precisely nothing. A test harness that can
    report success having done no work is worse than no harness.
    """
    images, a_steps, b_steps = [], [], []
    seen = set()
    target = None
    for arg in argv:
        if arg in ("--a", "--b"):
            if arg in seen:
                raise SystemExit(f"{arg} given twice - steps would silently merge")
            seen.add(arg)
            target = a_steps if arg == "--a" else b_steps
        elif target is None:
            images.append(arg)
        else:
            wait, _, text = arg.partition("@@")
            target.append((wait, text))
    if len(images) != 2:
        raise SystemExit("need exactly two images (A then B)")
    if "--a" not in seen or "--b" not in seen:
        raise SystemExit("both --a and --b are required (a run with no steps proves nothing)")
    if not a_steps:
        # Otherwise a.run([]) returns instantly and B boots with no barrier on A
        # at all - B would try to mount an exporter that is not up yet.
        raise SystemExit("--a needs at least one step: B must not start before A is up")
    if not b_steps:
        raise SystemExit("--b needs at least one step")
    return images, a_steps, b_steps


def main() -> int:
    images, a_steps, b_steps = parse(sys.argv[1:])
    img_a, img_b = images
    # BOTH captures land beside image A, so a run whose two images live in
    # different directories still puts them together - and `auth_census` below
    # looks in exactly one place rather than guessing per image.
    build = os.path.dirname(os.path.abspath(img_a))

    # Delete last run's captures FIRST. If QEMU fails to write one (a bad path,
    # a guest that never started), a stale file from a previous run is still
    # sitting there, and every reader of it - `auth_census`, a human with
    # tcpdump - gets a confident answer about a run that did not happen. That
    # is the same stale-evidence trap as a check that cannot fail, and it has
    # already produced one wrong reading in this project.
    for stale in ("net-2vm-a.pcap", "net-2vm-b.pcap"):
        try:
            os.remove(os.path.join(build, stale))
        except FileNotFoundError:
            pass

    a = b = None
    try:
        a = Guest(
            img_a,
            extra_args=[
                "-netdev", f"socket,id=net0,listen=127.0.0.1:{LINK_PORT}",
                "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a",
                # Capture the link. What crosses it is the only evidence of which
                # wire format was actually used: a run that succeeds proves the
                # cluster works, not that it did so the way you believe.
                "-object", f"filter-dump,id=f0,netdev=net0,file={os.path.join(build, 'net-2vm-a.pcap')}",
            ],
            intlog=os.path.join(build, "qemu-int-a.log"),
            label="[A] ",
        )
        ok_a = a.run(a_steps)
        if not ok_a:
            # Surfaced here rather than left to show up as a confusing B-side
            # timeout: if the exporter never came up, nothing B does means
            # anything.
            print("[A] did not reach its prompt - not starting B", file=sys.stderr)
            return 1

        b = Guest(
            img_b,
            extra_args=[
                "-netdev", f"socket,id=net0,connect=127.0.0.1:{LINK_PORT}",
                "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0b",
                "-object", f"filter-dump,id=f1,netdev=net0,file={os.path.join(build, 'net-2vm-b.pcap')}",
            ],
            intlog=os.path.join(build, "qemu-int-b.log"),
            label="[B] ",
        )
        ok_b = b.run(b_steps)
        time.sleep(drive_qemu.LINGER)
        ta, tb = a.transcript(), b.transcript()
    finally:
        # try/finally, because every path out of the block above leaves a live
        # QEMU holding both the image's write lock AND the link port - and the
        # next run then fails as an unrelated guest-side timeout. A guest panic
        # mid-`type_line`, a Popen failure constructing B, and Ctrl-C during a
        # 90-second wait all take this path.
        for g in (a, b):
            if g is not None:
                g.stop()

    print("=" * 70)
    print("NODE A (the exporter)")
    print("=" * 70)
    print(ta)
    print("=" * 70)
    print("NODE B (the client)")
    print("=" * 70)
    print(tb)
    print(f"\n--- A: {drive_qemu.fault_line(a)}")
    print(f"--- B: {drive_qemu.fault_line(b)}")
    print(f"--- A: {auth_census(os.path.join(build, 'net-2vm-a.pcap'))}")
    print(f"--- B: {auth_census(os.path.join(build, 'net-2vm-b.pcap'))}")
    return 0 if ok_b else 1


# The two export frame formats, as they appear ON THE WIRE. The magic is written
# as a little-endian u64 of the big-endian integer the tag spells, so the bytes
# arrive REVERSED - which is why searching a capture for "AUTHNP03" finds
# nothing and looks like "the cluster used neither format".
AUTH_FORMATS = (
    ("signed", b"AUTHNP03"[::-1]),
    ("MAC'd", b"AUTHNP02"[::-1]),
)


def auth_census(pcap) -> str:
    """Which export auth format this node's traffic actually used.

    The capture is taken on every run precisely because a transcript cannot
    answer this - and until this existed, nothing read it. A two-node run whose
    wire carried five SIGNED frames and zero MAC'd ones still printed
    "remote-mounted (cluster-key auth)", and the transcript looked perfect. The
    health bar says the run did not fault; this says what it did.
    """
    try:
        with open(pcap, "rb") as f:
            data = f.read()
    except OSError as e:
        return f"auth format: capture unreadable ({e.strerror})"
    counts = [(name, data.count(tag)) for name, tag in AUTH_FORMATS]
    seen = [f"{n} {name}" for name, n in counts if n]
    if not seen:
        # Not necessarily wrong - a node that was never asked for anything
        # exports nothing - but never silently "fine" either.
        return "auth format: NO export frames seen in the capture"
    return "auth format: " + ", ".join(seen) + " frame(s)"


if __name__ == "__main__":
    sys.exit(main())

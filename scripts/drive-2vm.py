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
    build = os.path.dirname(os.path.abspath(img_a))

    a = b = None
    try:
        a = Guest(
            img_a,
            extra_args=[
                "-netdev", f"socket,id=net0,listen=127.0.0.1:{LINK_PORT}",
                "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a",
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
    return 0 if ok_b else 1


if __name__ == "__main__":
    sys.exit(main())

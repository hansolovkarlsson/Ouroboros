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

LINK_PORT = 12340


def parse(argv):
    images, a_steps, b_steps = [], [], []
    target = None
    for arg in argv:
        if arg == "--a":
            target = a_steps
        elif arg == "--b":
            target = b_steps
        elif target is None:
            images.append(arg)
        else:
            wait, _, text = arg.partition("@@")
            target.append((wait, text))
    return images, a_steps, b_steps


def main() -> int:
    images, a_steps, b_steps = parse(sys.argv[1:])
    if len(images) != 2:
        print(__doc__)
        return 2
    img_a, img_b = images
    build = os.path.dirname(os.path.abspath(img_a))

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

    print("=" * 70)
    print("NODE A (the exporter)")
    print("=" * 70)
    print(a.transcript())
    print("=" * 70)
    print("NODE B (the client)")
    print("=" * 70)
    print(b.transcript())
    print(f"\n--- qemu -d int: A {a.aborts()} aborts, B {b.aborts()} aborts ---")
    a.stop()
    b.stop()
    return 0 if (ok_a and ok_b) else 1


if __name__ == "__main__":
    sys.exit(main())

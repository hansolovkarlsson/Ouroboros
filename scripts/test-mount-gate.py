#!/usr/bin/env python3
"""The MOUNT syscall answers the filesystem server alone.

    python3 scripts/test-mount-gate.py     (or `make test-mount-gate`, which builds the images)

`MOUNT` with replace set has the kernel rescan the USB ports and install a
found stick as the block device in place of the one there. Swapped under a
mounted filesystem, fsd's cached geometry describes the wrong disk. Until
2026-10-19 any task could call it, an ordinary user included (fsd's
FSOP_MOUNT is root-only): the shell called it on fsd's word that nothing was
mounted. Now the kernel answers it to the CAP_BLOCK holder alone, fsd, which
makes the rescan itself inside FSOP_MOUNT when its mount found nothing and no
disk filesystem is mounted.

One boot of a copy of build/esp.img on virtio, a copy of build/usbstick.img
(a FAT32 stick) 1 MiB longer on USB beside it, so it is told from the ESP by
its size; as root:

- `mountprobe` (/bin/MOUNTPROBE, MOUNT with replace set from an ordinary
  task) must print `mountprobe: denied`, and `ls /` after it must still list
  the ESP (EFI, BIN), not the stick;
- `unmount`, `erase disk` (the virtio copy's start zeroed, so it holds no
  filesystem), then `mount -a` must print `mounted` after the kernel's
  `usb-msd block device installed` line: fsd's own rescan put the stick in
  place. The stick has no /bin, so the builtin `mount` then reports the
  mounted disk, which must be the stick copy's size, not the ESP image's.

No fault line in QEMU's trace. Fails with the gate removed (`mountprobe:
installed`, and `ls /` reads the stick through the ESP's geometry) and with
fsd's rescan removed (`mount -a` finds nothing). About a minute. Run it
whenever the kernel's MOUNT arm, fsd's FSOP_MOUNT or the shell's `mount -a`
changes.
"""
import importlib.util
import os
import re
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
_spec = importlib.util.spec_from_file_location("test_usb_hub", os.path.join(HERE, "test-usb-hub.py"))
usb_hub = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(usb_hub)

COPY = os.path.join(ROOT, "build", "test-mount-gate.img")
STICK = os.path.join(ROOT, "build", "usbstick.img")
# The rig's own copy of the stick, 1 MiB longer than esp.img and usbstick.img
# (both 64 MiB, with the same partition table), so `mount` tells it by size.
STICK_COPY = os.path.join(ROOT, "build", "test-mount-gate-stick.img")
COMMANDS = ["mountprobe", "ls /", "unmount", "erase disk", "mount -a", "mount"]
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)


def boot():
    # No marker commands: after `unmount` nothing can be loaded from /bin, so
    # each command's output is read between the prompt lines instead.
    steps = [("login:", "root"), ("assword", "root")]
    for cmd in COMMANDS:
        steps += [("# ", cmd)]
    steps += [("# ", "")]
    guest = None
    try:
        shutil.copyfile(os.path.join(ROOT, "build", "esp.img"), COPY)
        shutil.copyfile(STICK, STICK_COPY)
        with open(STICK_COPY, "ab") as fh:
            fh.write(bytes(1024 * 1024))
        guest = drive_qemu.Guest(COPY, extra_args=usb_hub.layout(True, STICK_COPY), label="test-mount-gate: ")
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        if guest:
            guest.stop()
        for f in (COPY, STICK_COPY):
            if os.path.exists(f):
                os.remove(f)
    with open(os.path.join(ROOT, "build", "test-mount-gate.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def outputs(out):
    """[output of each command], in order: the lines after its prompt line
    (`# cmd`) up to the next prompt, the kernel's exit lines dropped."""
    got = []
    at = 0
    for cmd in COMMANDS:
        m = re.compile(rf"^# {re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            got.append(None)
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        block = out[m.end():nxt.start() if nxt else len(out)]
        at = nxt.start() if nxt else len(out)
        got.append(EXIT.sub("", block).strip())
    return got


def main():
    if not os.path.exists(STICK):
        sys.exit(f"{STICK} not found - run `make test-mount-gate`, which builds it")
    # The stick has no /bin, so after the swap only builtins run: `mount`
    # names the mounted disk by its size, which must be the stick's.
    stick = os.path.getsize(STICK) // 512 + 2048
    esp = os.path.getsize(os.path.join(ROOT, "build", "esp.img")) // 512
    driven, out, faults = boot()
    o = outputs(out)
    names = {w.rstrip("/").upper() for w in (o[1] or "").split()}
    checks = [
        ("driven to the end, every command's output found", driven and None not in o),
        ("the boot disk is the virtio copy, fsd mounted FAT32",
         re.search(r"^fsd: FAT32 mounted", out, re.M) is not None),
        (f"mountprobe denied (got {o[0]!r})", o[0] == "mountprobe: denied"),
        ("ls / after it lists the ESP, not the stick",
         {"EFI", "BIN"} <= names and "USBTEST.TXT" not in names),
        ("unmount", o[2] == "unmounted"),
        ("erase disk", (o[3] or "").startswith("erased")),
        (f"mount -a mounted through fsd's rescan, the stick installed (got {o[4]!r})",
         (o[4] or "").endswith("mounted - disk commands available")
         and "usb-msd block device installed" in (o[4] or "")),
        (f"mount reports the stick's disk ({stick} sectors; the ESP image has {esp}): {o[5]!r}",
         stick != esp and f"(disk {stick} sectors" in (o[5] or "")),
        ("no fault lines", faults == 0),
    ]
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    print(f"test-mount-gate: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""The USB hub check: a keyboard and a storage stick BEHIND a hub, driven.

Every USB 2.0 device on a Raspberry Pi 4 or 400 sits behind an on-board hub,
the Pi 400's own keyboard included (docs/testing/testing-pi4.md, section 1b),
and xhci.rs addresses only devices on a root port. This is the QEMU stand-in
for that layout: a usb-hub on root port 1, a usb-kbd and a usb-storage stick on
its ports 1 and 2, and a usb-tablet directly on root port 2.

It boots build/esp.img with drive-qemu.py's `Guest` (the paced typing is
load-bearing; see that file), logs in over the serial console, and then types a
command through QEMU's monitor `sendkey`. That reaches the guest ONLY as a USB
keyboard, never over serial, so the command's output line proves the keyboard
behind the hub works end to end.

Checks, each an outcome rather than a log line of the driver under test:

  direct device  the tablet on root port 2 is found: xHCI and the port scan ran
                 this boot, so a failure below is about the hub
  keyboard       xhci.rs reports "keyboard ready"
  storage        xhci.rs configures the stick's bulk endpoints
  typed          a line typed through the USB keyboard runs, and its output
                 appears

--usb-boot is the hub layout with build/esp.img itself as the stick behind the
hub and no other disk: firmware boots from it, the kernel takes the controller,
and the filesystem server must mount that stick THROUGH THE HUB before
/bin/echo can run at all. So `typed` there also proves bulk data moves through
the hub, and a fifth check requires the mount. It is the Raspberry Pi booted
entirely from one USB stick, less the high-speed hub.

--stall is --usb-boot from build/usb-hub-stall.img (`make image-stall`:
build/esp.img with the MSDSTALL boot flag file). The kernel makes QEMU's stick
stall two ways, which no ordinary QEMU run does: it corrupts the signature of
every seventh CBW (tag 3 mod 7), which usb-storage answers with a Bulk-OUT
Stall, recovered by usb_msd.rs's retry from a fresh CBW; and it reads the CSW
of every command with tag 5 mod 7 as 12 bytes, which usb-storage answers with
a Bulk-IN Stall while the CSW stays owed, recovered in place by clearing the
halt and reading the CSW again. On top of --usb-boot's checks it requires
that the kernel armed the fault, that QEMU itself reported the bad
signatures (the foreign observer: the OUT stalls happened), that the kernel
logged recovering CSW reads, and that every OUT Stall was recovered by its
first retry: exactly one `retry 1/3` line per bad signature, no `retry 2/`
line, no `giving up` (the kernel logs every recovery while the fault is
armed). A CSW recovery that failed would fall back to that retry and break
the count. QEMU logs nothing for the short reads, so their count is the
kernel's own. An injected Stall's own retry is never corrupted (its tag is
4 mod 7), so a second Stall in a row and running out of attempts are not
exercised. A Stall in the data stage is not exercised either: QEMU pads a
data phase that comes up short rather than stalling it (see
docs/ROADMAP.md). Measured 2026-10-01: the recovery that rewound to the
ring's start (before #184) fails it; the one that dequeues at the enqueue
position passes.

--direct puts the keyboard and the stick on root ports instead, with the same
checks. That is the control: it passes on a kernel with no hub support, which
shows the checks can pass, while the hub layout fails there (measured
2026-09-27, before any hub code), which shows they can fail.

QEMU's usb-hub is full-speed (USB 1.1). The Pi's hub is high-speed with a
slower keyboard behind it, which needs the transaction-translator fields of the
slot context; this rig cannot exercise those, the board is their first test.

Usage:  python3 scripts/test-usb-hub.py [--direct | --usb-boot | --stall]
        make test-usb-hub          (rebuilds the image and the stick first)
Exit status: the number of checks that failed.
"""
import importlib.util
import os
import re
import socket
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
STICK = os.path.join(ROOT, "build", "usbstick.img")
MONITOR = os.path.join(ROOT, "build", "usb-hub-monitor.sock")
STALL_IMAGE = os.path.join(ROOT, "build", "usb-hub-stall.img")
MIN_STALLS = 5       # a --stall boot measured 30; fewer means the fault barely ran
WORD = "usbhub"      # typed as `echo usbhub`; its output is this word alone
KEY_DELAY = 0.3      # between sendkeys; each key is held ~100 ms by QEMU


def layout(direct, stick_image):
    """QEMU arguments for the USB devices. The stick is a snapshot so the
    check never writes the image behind it."""
    kbd, stick = ("1", "3") if direct else ("1.1", "1.2")
    args = ["-device", "qemu-xhci,id=xhci0"]
    if not direct:
        args += ["-device", "usb-hub,bus=xhci0.0,port=1"]
    return args + [
        "-device", f"usb-kbd,bus=xhci0.0,port={kbd}",
        "-drive", f"file={stick_image},format=raw,if=none,id=usbstick,snapshot=on",
        "-device", f"usb-storage,drive=usbstick,bus=xhci0.0,port={stick}",
        "-device", "usb-tablet,bus=xhci0.0,port=2",
        "-monitor", f"unix:{MONITOR},server,nowait",
    ]


def sendkeys(text):
    """Types `text` and Enter through the monitor: USB keyboard only."""
    names = {" ": "spc", "\n": "ret"}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as mon:
        mon.connect(MONITOR)
        for ch in text + "\n":
            mon.sendall(f"sendkey {names.get(ch, ch)}\n".encode())
            time.sleep(KEY_DELAY)


def main():
    args = sys.argv[1:]
    unknown = [a for a in args if a not in ("--direct", "--usb-boot", "--stall")]
    if unknown:
        print(f"test-usb-hub: unknown argument {unknown[0]}; usage: [--direct | --usb-boot | --stall]")
        return 2
    direct = "--direct" in args
    stall = "--stall" in args
    usb_boot = "--usb-boot" in args or stall
    if direct and usb_boot:
        print("test-usb-hub: --direct and --usb-boot/--stall are separate runs")
        return 2
    # --usb-boot's stick is the image itself, so it needs no usbstick.img.
    needed = (STALL_IMAGE,) if stall else (IMAGE,) if usb_boot else (IMAGE, STICK)
    # What rebuilds each: image-stall rebuilds IMAGE first, then the copy.
    remedy = "make image-stall" if stall else "make image" if usb_boot else f"make image {os.path.relpath(STICK, ROOT)}"
    for path in needed:
        if not os.path.exists(path):
            print(f"test-usb-hub: {path} missing - run {remedy}")
            return 2
    # A stale image would grade the previous kernel (same guard as
    # test-keyboard-chain.sh).
    kernel = os.path.join(ROOT, "build", "esp", "EFI", "BOOT", "BOOTAA64.EFI")
    # Compared with the kernel, not with each other: the hub boot uses
    # IMAGE as its writable disk, so the guest's own writes make it newer.
    for path in needed:
        if path != STICK and os.path.exists(kernel) and os.path.getmtime(kernel) > os.path.getmtime(path):
            print(f"test-usb-hub: {path} is older than {kernel} - run {remedy}")
            return 2
    if os.path.exists(MONITOR):
        os.remove(MONITOR)

    label = "direct" if direct else "stall" if stall else "usb-boot" if usb_boot else "hub"
    print(f"test-usb-hub: {label} layout")
    boot_image = STALL_IMAGE if stall else IMAGE
    g = drive_qemu.Guest(
        boot_image,
        extra_args=layout(direct, boot_image if usb_boot else STICK),
        virtio_disk=not usb_boot,
    )
    typed = False
    try:
        if g.run([("login:", "root"), ("assword", "root"), ("# ", "")]):
            sendkeys(f"echo {WORD}")
            # The echoed command line is "echo usbhub"; its output is the word
            # on a line of its own.
            typed = g.wait_for(rf"(?m)^{WORD}\r?$", timeout=20)
    finally:
        out = g.transcript().replace("\r", "")
        g.stop()
    log = os.path.join(ROOT, "build", f"usb-hub-{label}.txt")
    with open(log, "w") as fh:
        fh.write(out)

    checks = [
        ("direct device", re.search(r"interface class=0x03 subclass=0x00 protocol=0x00", out)),
        ("keyboard", re.search(r"xhci: keyboard ready", out)),
        ("storage", re.search(r"xhci: storage bulk endpoints configured", out)),
        ("typed", typed),
    ]
    if usb_boot:
        checks.append(("mounted through the hub", re.search(r"FAT32 mounted", out)))
    if stall:
        stalls = len(re.findall(r"qemu-system-aarch64: usb-msd: Bad signature", out))
        csw = len(re.findall(r"Ouroboros kernel: usb-msd: CSW read stalled", out))
        first = len(re.findall(r"Ouroboros kernel: usb-msd: .*resetting bulk endpoints, retry 1/", out))
        again = len(re.findall(r"Ouroboros kernel: usb-msd: .*(retry [2-9]/|giving up)", out))
        checks += [
            ("fault armed", re.search(r"MSDSTALL armed", out)),
            (f"stalls injected ({stalls}, QEMU's count)", stalls >= MIN_STALLS),
            (f"CSW reads stalled ({csw}, the kernel's count)", csw >= MIN_STALLS),
            (f"each recovered at once ({first} first retries, {again} further)", first == stalls and again == 0),
        ]
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += not ok
    print(f"transcript: {os.path.relpath(log, ROOT)}; {drive_qemu.fault_line(g)}")
    return failed


if __name__ == "__main__":
    sys.exit(main())

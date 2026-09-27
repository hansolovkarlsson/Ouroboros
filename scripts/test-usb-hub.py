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

--direct puts the keyboard and the stick on root ports instead, with the same
checks. That is the control: it passes on a kernel with no hub support, which
shows the checks can pass, while the hub layout fails there (measured
2026-09-27, before any hub code), which shows they can fail.

QEMU's usb-hub is full-speed (USB 1.1). The Pi's hub is high-speed with a
slower keyboard behind it, which needs the transaction-translator fields of the
slot context; this rig cannot exercise those, the board is their first test.

Usage:  python3 scripts/test-usb-hub.py [--direct]
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
WORD = "usbhub"      # typed as `echo usbhub`; its output is this word alone
KEY_DELAY = 0.3      # between sendkeys; each key is held ~100 ms by QEMU


def layout(direct):
    """QEMU arguments for the USB devices. The stick is a snapshot so the
    check never writes the shared image."""
    kbd, stick = ("1", "3") if direct else ("1.1", "1.2")
    args = ["-device", "qemu-xhci,id=xhci0"]
    if not direct:
        args += ["-device", "usb-hub,bus=xhci0.0,port=1"]
    return args + [
        "-device", f"usb-kbd,bus=xhci0.0,port={kbd}",
        "-drive", f"file={STICK},format=raw,if=none,id=usbstick,snapshot=on",
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
    direct = "--direct" in sys.argv[1:]
    for path in (IMAGE, STICK):
        if not os.path.exists(path):
            print(f"test-usb-hub: {path} missing - run make image {os.path.relpath(STICK, ROOT)}")
            return 2
    # A stale image would grade the previous kernel (same guard as
    # test-keyboard-chain.sh).
    kernel = os.path.join(ROOT, "build", "esp", "EFI", "BOOT", "BOOTAA64.EFI")
    if os.path.exists(kernel) and os.path.getmtime(kernel) > os.path.getmtime(IMAGE):
        print(f"test-usb-hub: {IMAGE} is older than {kernel} - run make image")
        return 2
    if os.path.exists(MONITOR):
        os.remove(MONITOR)

    label = "direct" if direct else "hub"
    print(f"test-usb-hub: {label} layout")
    g = drive_qemu.Guest(IMAGE, extra_args=layout(direct))
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
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += not ok
    print(f"transcript: {os.path.relpath(log, ROOT)}; {drive_qemu.fault_line(g)}")
    return failed


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Name the kernel function at an address from a firmware exception report.

On real hardware without a serial cable, the firmware's own handler prints
only "Synchronous Exception at 0x<pc>" on HDMI. The kernel logs where the
firmware loaded it ("image @ <base>..<end>", repeated just before the xHCI
takeover), so the offset is <pc> - <base>, and the linker map turns an offset
into a function.

The map must come from the same source as the binary on the card. The kernel
build is deterministic apart from the PE timestamp and debug record (checked
2026-09-28), so relinking the same tree with a map is enough:

    cargo rustc -p ouroboros-kernel --target aarch64-unknown-uefi -- \\
        -C link-arg=/MAP:build/BOOTAA64.map
    scripts/efi-symbol.py build/BOOTAA64.map <base>..<end> <pc> [<pc> ...]

<base>..<end> is the range exactly as the `image @` line prints it.

An address outside the image is firmware code (a DXE driver); only the serial
dump, which names the module, places it.
"""
import re
import sys


def symbols(path):
    base, out = None, []
    for line in open(path, errors="replace"):
        m = re.search(r"Preferred load address is ([0-9a-fA-F]+)", line)
        if m:
            base = int(m.group(1), 16)
        m = re.match(r"\s*[0-9a-fA-F]{4}:[0-9a-fA-F]{8}\s+(\S+)\s+([0-9a-fA-F]{16})\s", line)
        if m:
            out.append((int(m.group(2), 16), m.group(1)))
    if base is None or not out:
        sys.exit(f"efi-symbol: {path} is not an lld-link map")
    out.sort()
    return base, out


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    preferred, syms = symbols(sys.argv[1])
    lo, sep, hi = sys.argv[2].partition("..")
    if not sep:
        sys.exit("efi-symbol: give the image range as <base>..<end>, as the `image @` line prints it")
    image, end = int(lo, 16), int(hi, 16)
    for arg in sys.argv[3:]:
        pc = int(arg, 16)
        off = pc - image
        if not image <= pc < end:
            print(f"{pc:#x}: outside the image {image:#x}..{end:#x} (firmware code; the serial dump names the module)")
            continue
        va = preferred + off
        best = None
        for a, name in syms:
            if a > va:
                break
            best = (a, name)
        if best is None:
            print(f"{pc:#x}: offset {off:#x}, before the first symbol")
        else:
            print(f"{pc:#x}: offset {off:#x} = {best[1]} + {va - best[0]:#x}")


main()

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
    scripts/efi-symbol.py build/BOOTAA64.map \\
        target/aarch64-unknown-uefi/debug/BOOTAA64.efi <base>..<end> <pc> [<pc> ...]

<base>..<end> is the range exactly as the `image @` line prints it. The .efi
is the one that link wrote beside the map (add --release to both for a
PROFILE=release card). Two properties are checked before any address is
named, since a misread digit or the wrong build still yields a confident
symbol: the map and the .efi carry the same link timestamp, and <end>-<base>
is the .efi's SizeOfImage. Either failing refuses the lookup.

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


def map_timestamp(path):
    for line in open(path, errors="replace"):
        m = re.search(r"Timestamp is ([0-9a-fA-F]+)", line)
        if m:
            return int(m.group(1), 16)
    sys.exit(f"efi-symbol: {path} has no link timestamp")


def pe_header(path):
    """(TimeDateStamp, SizeOfImage) from a PE/COFF image."""
    data = open(path, "rb").read()
    pe = int.from_bytes(data[0x3C:0x40], "little")
    if data[:2] != b"MZ" or data[pe:pe + 4] != b"PE\0\0":
        sys.exit(f"efi-symbol: {path} is not a PE image")
    stamp = int.from_bytes(data[pe + 8:pe + 12], "little")
    size = int.from_bytes(data[pe + 24 + 56:pe + 24 + 60], "little")
    return stamp, size


def main():
    if len(sys.argv) < 5:
        sys.exit(__doc__)
    preferred, syms = symbols(sys.argv[1])
    stamp, size_of_image = pe_header(sys.argv[2])
    if map_timestamp(sys.argv[1]) != stamp:
        sys.exit(f"efi-symbol: {sys.argv[1]} and {sys.argv[2]} are not from the same link (timestamps differ)")
    lo, sep, hi = sys.argv[3].partition("..")
    if not sep:
        sys.exit("efi-symbol: give the image range as <base>..<end>, as the `image @` line prints it")
    image, end = int(lo, 16), int(hi, 16)
    if end - image != size_of_image:
        sys.exit(
            f"efi-symbol: the range is {end - image:#x} bytes but the image is {size_of_image:#x}: "
            "a digit is misread, or the card holds a different build (profile, or tree)"
        )
    for arg in sys.argv[4:]:
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

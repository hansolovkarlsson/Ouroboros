#!/usr/bin/env python3
"""Check that the built kernel still carries the xHCI driver's two barriers.

The xHCI driver orders its DMA memory against the controller with two
barriers, and no rig can see either one: QEMU's controller is emulated in
the same process and sees guest memory as the CPU last wrote it, so every
QEMU test passes with or without them. Only the Raspberry Pi 4 (a
non-coherent PCIe controller behind a write buffer) needs them, and there a
missing one is a timing window, not a reliable failure. So the check is on
the image instead.

`kernel/src/xhci.rs` keeps each barrier and the store it orders as one naked
function, fixed instructions whatever the profile:

    mmio_write32:        dsb sy;    str w1, [x0]; ret                    (32-bit registers)
    mmio_write64:        dsb sy;    str w1, [x0]; str w2, [x0, #4]; ret  (64-bit registers)
    publish_cycle_word:  dmb oshst; str w1, [x0]; ret                    (a TRB batch's flip)

This script disassembles the kernel image (`llvm-objdump`, from the
`llvm-tools` rustup component the Makefile already needs) and requires each
sequence exactly once, and at least one call to each (`bl`, or `b` for a
tail call): present, and in use. The PE image carries no symbols, so the
sequences are found by their instructions, which is why they are fixed in
assembly rather than left to the compiler. Deleting a barrier, putting an
instruction between it and its store, or routing a register write (a
`reg::Whole32::write` that stores without calling `mmio_write32`) or the
flip around its function fails it. That holds because each function has ONE
caller in the source (`Whole32::write`, `Whole64::write`, `ring_publish`):
with a second caller, routing one around it leaves "at least one call" true
and the check green. The RsvdP writes (`Kept32::write`, `Kept64::write`) and
PORTSC's (`PortStatus::write`) go through the first two, which the doorbells,
DCBAAP and ERDP also call, so one of those routed around them is NOT seen
here.

What it cannot see: ONE new raw `write_volatile` to a register somewhere
else in `xhci.rs`. The functions are still called by everything else, so
the check stays green; that every register write goes through `xhci.rs`'s
`reg` handles, whose barrier stores are private to that module, is a
property of the source, not of this check. And "exactly
once" is deliberate: a second copy of the same instructions elsewhere
in the kernel fails it, loudly, rather than letting either copy stand in
for the other.

Usage: check-xhci-barriers.py [IMAGE] [LLVM_TOOLS_DIR]
  IMAGE defaults to the debug kernel; LLVM_TOOLS_DIR to the llvm-tools bin
  directory of the active toolchain (the Makefile passes the one it uses).
"""

import re
import subprocess
import sys
from pathlib import Path

DEFAULT_IMAGE = "target/aarch64-unknown-uefi/debug/BOOTAA64.efi"

SEQUENCES = {
    "mmio_write32 (dsb sy; str w1, [x0]; ret)": ["dsb sy", "str w1, [x0]", "ret"],
    "mmio_write64 (dsb sy; str w1, [x0]; str w2, [x0, #4]; ret)": ["dsb sy", "str w1, [x0]", "str w2, [x0, #0x4]", "ret"],
    "publish_cycle_word (dmb oshst; str w1, [x0]; ret)": ["dmb oshst", "str w1, [x0]", "ret"],
}

LINE = re.compile(r"^\s*([0-9a-f]+):\s+(\S+)(?:\s+(.*?))?\s*$")
CALL = re.compile(r"^(?:bl|b) 0x([0-9a-f]+)")


def objdump() -> str:
    if len(sys.argv) > 2:
        return str(Path(sys.argv[2]) / "llvm-objdump")
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True).stdout.strip()
    host = next(l.split(": ", 1)[1] for l in subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout.splitlines() if l.startswith("host: "))
    return str(Path(sysroot) / "lib" / "rustlib" / host / "bin" / "llvm-objdump")


def checked_objdump() -> str:
    tool = Path(objdump())
    if not tool.exists():
        sys.exit(f"check-xhci-barriers: {tool} not found; run `rustup component add llvm-tools`")
    return str(tool)


def instructions(image: str) -> list[tuple[int, str]]:
    out = subprocess.run([checked_objdump(), "-d", "--no-show-raw-insn", image], capture_output=True, text=True, check=True).stdout
    insns = []
    for line in out.splitlines():
        m = LINE.match(line)
        if m:
            text = m.group(2) + (" " + re.sub(r"\s+", " ", m.group(3)) if m.group(3) else "")
            insns.append((int(m.group(1), 16), text))
    return insns


def main() -> int:
    image = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_IMAGE
    if not Path(image).exists():
        sys.exit(f"check-xhci-barriers: {image} not found; build the kernel first")
    insns = instructions(image)
    texts = [t for _, t in insns]
    calls = {}
    for _, t in insns:
        m = CALL.match(t)
        if m:
            target = int(m.group(1), 16)
            calls[target] = calls.get(target, 0) + 1

    ok = True
    for name, seq in SEQUENCES.items():
        starts = [insns[i][0] for i in range(len(texts) - len(seq) + 1) if texts[i:i + len(seq)] == seq]
        if len(starts) != 1:
            print(f"FAIL {name}: found {len(starts)} times, want exactly 1")
            ok = False
            continue
        n = calls.get(starts[0], 0)
        if n == 0:
            print(f"FAIL {name}: at {starts[0]:#x}, but nothing calls it")
            ok = False
            continue
        print(f"ok   {name}: at {starts[0]:#x}, called from {n} site(s)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

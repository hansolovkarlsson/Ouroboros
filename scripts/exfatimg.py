"""exFAT on the host, for the rigs: the ONE copy of a directory's set walker
and of the fsck runner they share (test-long-set.py and test-grow-dir.py), so
a change to fsck's output or to how sets are walked is made once.

    for s in exfatimg.sets(dir_bytes): ...       # every File set, in order
    hit = exfatimg.find(dir_bytes, "VEND")       # by name, case folded as fsd does
    with exfatimg.attached(image) as dev: rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
"""
import contextlib
import subprocess
import sys
from collections import namedtuple

ENTRY = 32
FILE, STREAM, NAME = 0x85, 0xC0, 0xC1

Set = namedtuple("Set", "offset count name data")


def sets(d):
    """Every File set in directory bytes `d` up to its first 0x00 slot, as
    (byte offset, entry count, name, the set's bytes), counted by the File's
    SecondaryCount. A set cut off by the end of `d` is not yielded: the walk
    stops there, no IndexError."""
    i = 0
    while i + ENTRY <= len(d) and d[i]:
        if d[i] != FILE:
            i += ENTRY
            continue
        count = d[i + 1] + 1
        end = i + ENTRY * count
        if count < 2 or end > len(d):
            return
        s = d[i:end]
        name = ""
        if s[ENTRY] == STREAM:
            chars = s[ENTRY + 3]
            raw = b"".join(s[k + 2:k + ENTRY] for k in range(2 * ENTRY, len(s), ENTRY) if s[k] == NAME)
            # NameLength counts UTF-16 units, so the cut is in bytes: a
            # surrogate pair is one Python character and two units.
            name = raw[:2 * chars].decode("utf-16le", "replace")
        yield Set(i, count, name, bytes(s))
        i = end


def find(d, name):
    """The set named `name` in `d`, matched ignoring ASCII case as fsd's
    lookups do, or None."""
    for s in sets(d):
        if s.name.lower() == name.lower():
            return s
    return None


@contextlib.contextmanager
def attached(image):
    """`image` attached without mounting; yields its /dev/diskN, detached after."""
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", image],
                         capture_output=True, text=True)
    if att.returncode != 0:
        raise RuntimeError(f"hdiutil attach {image} failed: {att.stderr.strip()}")
    dev = att.stdout.split()[0]
    try:
        yield dev
    finally:
        if subprocess.run(["hdiutil", "detach", dev], capture_output=True).returncode != 0:
            print(f"exfatimg: hdiutil detach {dev} failed: the device is still attached", file=sys.stderr)


def fsck(dev, tool):
    """`tool -n dev` (fsck_exfat or fsck_msdos): (exit code, its lines)."""
    r = subprocess.run([tool, "-n", dev], capture_output=True, text=True)
    return r.returncode, (r.stdout + r.stderr).strip().splitlines()


def complaints(lines):
    """fsck_exfat's complaints: its indented lines."""
    return [ln for ln in lines if ln.startswith("   ")]


def unexpected(lines):
    """Lines that are none of fsck_exfat's stages, complaints or its `NO`
    to a repair."""
    return [ln for ln in lines if not (ln.startswith(("** ", "   ")) or ln.endswith("? NO"))]

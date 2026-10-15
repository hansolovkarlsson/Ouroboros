#!/usr/bin/env python3
"""The committed picolibc, pinned: its headers and its allocator.

    python3 scripts/check-picolibc.py      (or `make check-picolibc`; also run by `make test`)

Two things about third_party/picolibc-prebuilt that only a QEMU boot or
another project would otherwise notice:

- The include tree is what DevTools's cc compiles against, read straight from
  this working copy (its cc/Makefile's CC_OUROBOROS_SYSTEM_DIR). On 2026-10-15
  a regeneration with the cross file's flag probes fixed defined eleven
  _HAVE_ macros in picolibc.h and installed complex.h; nothing here failed,
  and DevTools found it when its cc refused <math.h>
  (docs/handoffs/closed/2026-10-15-from-devtools-picolibc-long-double.md).
  This hashes every file under include/, path and bytes, and fails when the
  hash is not HEADERS_PIN. Updating the pin is the moment to send DevTools a
  `notice` in its docs/handoffs/.
- The allocator libc.a carries: malloc, free and realloc from nano-malloc's
  objects (scripts/build-picolibc.sh says why). A regeneration that drops or
  adds -Dnewlib-nano-malloc changes them, and make test-malloc, the check that
  sees what the allocator does, is a minute-long boot outside make test.
"""
import hashlib
import os
import subprocess
import sys

HEADERS_PIN = "7a8283488e020fc3c6e773865d9e433d5d9d573294aa60e32d5b352b132f3f9d"
ALLOCATOR = {"malloc": "nano-malloc.c.o", "free": "nano-free.c.o", "realloc": "nano-realloc.c.o"}
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PREBUILT = os.path.join(ROOT, "third_party", "picolibc-prebuilt")


def headers_hash():
    include = os.path.join(PREBUILT, "include")
    h = hashlib.sha256()
    for dirpath, dirnames, filenames in os.walk(include):
        dirnames.sort()
        for name in sorted(filenames):
            path = os.path.join(dirpath, name)
            h.update(os.path.relpath(path, include).encode() + b"\0")
            with open(path, "rb") as f:
                h.update(f.read() + b"\0")
    return h.hexdigest()


def llvm_nm():
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], capture_output=True, text=True, check=True).stdout.strip()
    host = next(l.split()[1] for l in subprocess.run(["rustc", "-vV"], capture_output=True, text=True,
                                                       check=True).stdout.splitlines() if l.startswith("host:"))
    return os.path.join(sysroot, "lib", "rustlib", host, "bin", "llvm-nm")


def defining_objects():
    out = subprocess.run([llvm_nm(), "-A", "--defined-only", os.path.join(PREBUILT, "lib", "libc.a")],
                         capture_output=True, text=True, check=True).stdout
    found = {}
    for line in out.splitlines():
        parts = line.split()
        # libc.a:member.o: 0000000000000000 T name
        if len(parts) == 4 and parts[2] == "T" and parts[3] in ALLOCATOR:
            found[parts[3]] = parts[0].rstrip(":").split(":")[-1]
    return found


def main():
    failed = 0
    got = headers_hash()
    if got == HEADERS_PIN:
        print("check-picolibc: the include tree DevTools's cc reads is as pinned")
    else:
        print(f"check-picolibc: FAIL the include tree changed: sha256 {got}, pinned {HEADERS_PIN}.\n"
              "  DevTools's cc compiles against these headers from this working copy: send it a notice\n"
              "  (its docs/handoffs/) before updating HEADERS_PIN in scripts/check-picolibc.py.")
        failed += 1
    found = defining_objects()
    for name, member in ALLOCATOR.items():
        if found.get(name) == member:
            continue
        print(f"check-picolibc: FAIL {name} is defined in {found.get(name, 'nothing')}, not {member}:\n"
              "  libc.a's allocator is not the one scripts/build-picolibc.sh documents")
        failed += 1
    if not failed:
        print("check-picolibc: libc.a's malloc, free and realloc are nano-malloc's")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

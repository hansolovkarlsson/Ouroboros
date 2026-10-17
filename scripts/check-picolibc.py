#!/usr/bin/env python3
"""The committed C library, pinned: libc.a and the headers DevTools's cc reads.

    python3 scripts/check-picolibc.py      (or `make check-picolibc`; also run by `make test`)

Three hashes, each against a pin:

- third_party/picolibc-prebuilt/lib/libc.a, the whole archive. It rebuilds
  byte for byte from scripts/build-picolibc.sh, libc/picolibc-cross.txt and
  the compiler named in picolibc-prebuilt/COMPILER, so any change to it is a
  regeneration. The archive shipped until 2026-10-15 hung in realloc (built
  without -fno-builtin-free) and passed every check in make test; which
  objects defined malloc was the same in it as in the fixed one, so only the
  bytes tell them apart. A changed archive owes make test-malloc and the C
  rigs before LIB_PIN moves.
- third_party/picolibc-prebuilt/include and libc/pico/include, which
  DevTools's cc compiles against straight from this working copy (its
  cc/Makefile, ld/tests/pie/check.sh, cc/tests/elf/tools.sh). On 2026-10-15 a
  regeneration defined eleven _HAVE_ macros in picolibc.h and nothing here
  failed; DevTools found it
  (docs/handoffs/closed/2026-10-15-from-devtools-picolibc-long-double.md).
  A changed header tree owes DevTools a `notice` in its docs/handoffs/
  before its pin moves.

A tree is the files git would track there (`git ls-files --cached --others
--exclude-standard`), path and bytes: a new header counts before it is
committed, and an ignored file (Finder's .DS_Store) does not.
"""
import hashlib
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LIB = "third_party/picolibc-prebuilt/lib/libc.a"
LIB_PIN = "83f0897ee0a6be792f0d65984d6bb2ae41d47edd3983e42533889d71b07f842e"
TREES = {
    "third_party/picolibc-prebuilt/include": "c297c48071d13a5d0657a6dc427c1a696861b31c21cc399757386ecffd5419a2",
    "libc/pico/include": "19440640c0ac66d24f2a71018ff315fbe670b3b349063f2013a9252b5eb0c5a6",
}


def file_hash(rel):
    with open(os.path.join(ROOT, rel), "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def tree_hash(rel):
    names = subprocess.run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", rel],
                           cwd=ROOT, capture_output=True, check=True).stdout.split(b"\0")
    h = hashlib.sha256()
    for name in sorted(n for n in names if n):
        path = os.path.join(ROOT, os.fsdecode(name))
        if not os.path.isfile(path):
            continue  # deleted in the working copy: absent from the tree
        h.update(name + b"\0")
        with open(path, "rb") as f:
            h.update(f.read() + b"\0")
    return h.hexdigest()


def main():
    failed = 0
    got = file_hash(LIB)
    if got == LIB_PIN:
        print(f"check-picolibc: {LIB} is as pinned")
    else:
        print(f"check-picolibc: FAIL {LIB} changed: sha256 {got}, pinned {LIB_PIN}.\n"
              "  A regeneration: run make test-malloc and the C rigs on it before updating LIB_PIN\n"
              "  in scripts/check-picolibc.py.")
        failed += 1
    for rel, pin in TREES.items():
        got = tree_hash(rel)
        if got == pin:
            print(f"check-picolibc: {rel}, which DevTools's cc reads, is as pinned")
        else:
            print(f"check-picolibc: FAIL {rel} changed: sha256 {got}, pinned {pin}.\n"
                  "  DevTools's cc compiles against it from this working copy: send it a notice\n"
                  "  (its docs/handoffs/) before updating the pin in scripts/check-picolibc.py.")
            failed += 1
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

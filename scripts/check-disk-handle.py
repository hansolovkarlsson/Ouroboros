#!/usr/bin/env python3
"""No file in fsd but disk.rs can make a disk handle, checked by building.

    python3 scripts/check-disk-handle.py      (also run by `make test`)

exFAT's disk gate holds only if a filesystem arm writes through the `Disk` it
was handed and no other: since 2026-10-27 `disk::Disk` has a private field and
is not `Clone`, the one way to make one is `disk::Device::disk`, `Device` is
made only by `Device::claim`, and the `BLOCK_*` svc is disk.rs's private
`block_svc`. Nothing in a normal build fails if a later change adds a
`#[derive(Clone)]`, a `pub` field or a `pub fn new()`: the hole just reopens
(the lesson of docs/postmortems/cluster-keys-postmortem.md, a check that
cannot fail). So this builds a copy of fsd, outside the tree, once clean and
once with each spelling an arm might write appended to its exfat.rs, and
requires the clean one to check and each other to fail with the error named,
at the line it added.

It uses `cargo check`, so nothing links. The copy lives under
target/check-disk-handle/, which git ignores, so the working tree is never
touched. What it cannot see: an `svc` written out in another file's `asm!`,
and `syscall4`'s runtime refusal of the BLOCK_* numbers (main.rs).
"""
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
FSD = os.path.join(ROOT, "programs", "servers", "fsd")
WORK = os.path.join(ROOT, "target", "check-disk-handle")
COPY = os.path.join(WORK, "fsd")

# (name, code appended to exfat.rs, the error code it must fail with)
MUTANTS = [
    ("a Disk made by hand", "fn mutant() -> crate::disk::Disk { crate::disk::Disk(()) }", "E0603"),
    ("a Disk cloned", "fn mutant(d: &crate::disk::Disk) -> crate::disk::Disk { crate::disk::Disk::clone(d) }", "E0277"),
    ("a Device made by hand",
     "fn mutant() -> crate::disk::Device { crate::disk::Device { table_rewritten: core::cell::Cell::new(false) } }",
     "E0451"),
    ("disk.rs's svc called", "fn mutant() -> u64 { crate::disk::block_svc(0, 0, 0) }", "E0603"),
]


def make_copy():
    shutil.rmtree(COPY, ignore_errors=True)
    shutil.copytree(os.path.join(FSD, "src"), os.path.join(COPY, "src"))
    with open(os.path.join(FSD, "Cargo.toml")) as f:
        toml = f.read()
    # Path dependencies made absolute, and a workspace of its own so cargo
    # does not take the copy for a member of the repository's.
    toml = re.sub(r'path = "(\.\./[^"]+)"',
                  lambda m: 'path = "%s"' % os.path.normpath(os.path.join(FSD, m.group(1))), toml)
    with open(os.path.join(COPY, "Cargo.toml"), "w") as f:
        f.write(toml + "\n[workspace]\n")


def check(extra):
    exfat = os.path.join(COPY, "src", "exfat.rs")
    with open(os.path.join(FSD, "src", "exfat.rs")) as f:
        text = f.read()
    line = text.count("\n") + 2
    with open(exfat, "w") as f:
        f.write(text + ("\n#[allow(dead_code)]\n" + extra + "\n" if extra else ""))
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(WORK, "target"))
    r = subprocess.run(["cargo", "check", "--release", "--target", "aarch64-unknown-none"],
                       cwd=COPY, env=env, capture_output=True, text=True)
    return r.returncode, r.stderr, line + 1


def main():
    make_copy()
    failures = 0
    code, err, _ = check(None)
    if code != 0:
        print("check-disk-handle: FAIL the unchanged copy of fsd does not check:\n" + err[-2000:])
        return 1
    print("check-disk-handle: ok   the unchanged copy checks")
    for name, extra, want in MUTANTS:
        code, err, line = check(extra)
        at = re.search(r"error\[%s\][^\n]*\n\s*--> [^\n]*exfat\.rs:%d:" % (want, line), err)
        if code != 0 and at:
            print("check-disk-handle: ok   %s is refused (%s)" % (name, want))
        else:
            failures += 1
            print("check-disk-handle: FAIL %s %s" % (
                name, "builds" if code == 0 else "fails, but not with %s at exfat.rs:%d:\n%s" % (want, line, err[-1500:])))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

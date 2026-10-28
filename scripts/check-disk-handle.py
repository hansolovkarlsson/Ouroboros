#!/usr/bin/env python3
"""No file in fsd but disk.rs can reach the disk, checked by building.

    python3 scripts/check-disk-handle.py      (also run by `make test`)

exFAT's disk gate holds only if a filesystem arm writes through the `Disk` it
was handed and no other. Since 2026-10-27:
  - `disk::Disk` has a private field, is not `Clone` and has no constructor
    but `disk::Device::disk`;
  - `Device` has a private field, is not `Clone`, and is made only by
    `Device::claim`;
  - the `BLOCK_*` and `MOUNT` calls are made only by disk.rs's private
    `block_svc`: `syscall4` and `syscall5` take their number as a constant and
    assert it is none of them, so such a call does not compile.
Nothing in a normal build fails if a later change adds a `#[derive(Clone)]`,
a `pub` field, a `pub fn new()`, an `impl Default` or drops that assert: the
hole just reopens (the lesson of docs/postmortems/cluster-keys-postmortem.md,
a check that cannot fail). So this builds a copy of fsd, outside the tree,
once clean and once with each spelling an arm might write appended to its
exfat.rs, and requires the clean build to succeed and each other to fail
with the error named. A type error must be at the line added; the assert's
error is reported at the assert, so it is matched by the instance it names
(`syscall4::<28>`), the only one the clean build does not have.

It runs `cargo build`, not `cargo check`: the assert is evaluated when
`syscall4` is instantiated for a number, which `cargo check` never does. The
mutants calling it are `#[no_mangle]` so they are built though nothing calls
them (a dead caller is never instantiated, and never runs either). The copy,
with programs/linker.ld beside it for the link, lives under
target/check-disk-handle/, which git ignores, so the working tree is never
touched. What it cannot see: an `svc` written out in another file's `asm!`,
and a `Device::claim` that answers `Some` to a second caller (a run-time
promise).
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

DEVICE = "crate::disk::Device"
DISK = "crate::disk::Disk"
ABI = "syscall_abi"

# (name, code appended to exfat.rs, the error code, and for the assert the
# instance its error must name; None means the error is at the added line)
MUTANTS = [
    ("a Disk made by hand", "fn mutant() -> %s { %s(()) }" % (DISK, DISK), "E0603", None),
    ("a Disk cloned", "fn mutant(d: &%s) -> %s { %s::clone(d) }" % (DISK, DISK, DISK), "E0277", None),
    ("a Disk from a constructor", "fn mutant() -> %s { %s::new() }" % (DISK, DISK), "E0599", None),
    ("a Disk by default", "fn mutant() -> %s { <%s as Default>::default() }" % (DISK, DISK), "E0277", None),
    ("a Device made by hand",
     "fn mutant() -> %s { %s { table_rewritten: core::cell::Cell::new(false) } }" % (DEVICE, DEVICE),
     "E0451", None),
    ("a Device cloned", "fn mutant(d: &%s) -> %s { %s::clone(d) }" % (DEVICE, DEVICE, DEVICE), "E0277", None),
    ("disk.rs's svc called", "fn mutant() -> u64 { crate::disk::block_svc(0, 0, 0) }", "E0603", None),
    ("BLOCK_WRITE through syscall4",
     "#[no_mangle]\npub extern \"C\" fn fsd_mutant() -> u64 { crate::syscall4::<{ %s::BLOCK_WRITE }>(0, 0, 0, 0) }" % ABI,
     "E0080", "syscall4"),
    ("MOUNT through syscall4",
     "#[no_mangle]\npub extern \"C\" fn fsd_mutant() -> u64 { crate::syscall4::<{ %s::MOUNT }>(1, 0, 0, 0) }" % ABI,
     "E0080", "syscall4"),
    ("BLOCK_READ through syscall5",
     "#[no_mangle]\npub extern \"C\" fn fsd_mutant() -> u64 { crate::syscall5::<{ %s::BLOCK_READ }>(0, 0, 0, 0, 0) }" % ABI,
     "E0080", "syscall5"),
]


def make_copy():
    shutil.rmtree(COPY, ignore_errors=True)
    shutil.copytree(os.path.join(FSD, "src"), os.path.join(COPY, "src"))
    # The link argument in .cargo/config.toml names programs/linker.ld from
    # where cargo runs.
    os.makedirs(os.path.join(COPY, "programs"))
    shutil.copy(os.path.join(ROOT, "programs", "linker.ld"), os.path.join(COPY, "programs"))
    with open(os.path.join(FSD, "Cargo.toml")) as f:
        toml = f.read()
    # Path dependencies made absolute, and a workspace of its own so cargo
    # does not take the copy for a member of the repository's.
    toml = re.sub(r'path = "(\.\./[^"]+)"',
                  lambda m: 'path = "%s"' % os.path.normpath(os.path.join(FSD, m.group(1))), toml)
    with open(os.path.join(COPY, "Cargo.toml"), "w") as f:
        f.write(toml + "\n[workspace]\n")


def build(extra):
    """Build the copy with `extra` appended to exfat.rs: (exit code, stderr,
    the line `extra` starts at)."""
    exfat = os.path.join(COPY, "src", "exfat.rs")
    with open(os.path.join(FSD, "src", "exfat.rs")) as f:
        text = f.read()
    tail = "\n#[allow(dead_code)]\n" + extra + "\n" if extra else ""
    with open(exfat, "w") as f:
        f.write(text + tail)
    line = text.count("\n") + 3
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(WORK, "target"))
    r = subprocess.run(["cargo", "build", "--release", "--target", "aarch64-unknown-none"],
                       cwd=COPY, env=env, capture_output=True, text=True)
    return r.returncode, r.stderr, line


def main():
    make_copy()
    failures = 0
    code, err, _ = build(None)
    if code != 0:
        print("check-disk-handle: FAIL the unchanged copy of fsd does not build:\n" + err[-2000:])
        return 1
    print("check-disk-handle: ok   the unchanged copy builds")
    for name, extra, want, instance in MUTANTS:
        code, err, line = build(extra)
        if instance:
            where = "naming %s::<N>" % instance
            seen = re.search(r"error\[%s\].*\n(?:.*\n)*?.*evaluation of `%s::<\d+>::" % (want, instance), err)
        else:
            where = "at exfat.rs:%d" % line
            seen = re.search(r"error\[%s\][^\n]*\n\s*--> [^\n]*exfat\.rs:%d:" % (want, line), err)
        if code != 0 and seen:
            print("check-disk-handle: ok   %s is refused (%s)" % (name, want))
        else:
            failures += 1
            print("check-disk-handle: FAIL %s %s" % (
                name, "builds" if code == 0 else "fails, but not with %s %s:\n%s" % (want, where, err[-1500:])))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

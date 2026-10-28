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
    assert it is none of them, so such a call does not compile;
  - every fsd module but main.rs and disk.rs forbids `unsafe` code, so none
    can make a `Disk` from nothing (`zeroed`) or write an `svc` of its own.
Nothing in a normal build fails if a later change adds a `#[derive(Clone)]`,
a `pub` field, a `pub fn new()`, an `impl Default`, drops that assert, leaves
a new `BLOCK_*` call out of `disk::is_device_call` or lifts a module's
`forbid`: the hole just reopens (the lesson of
docs/postmortems/cluster-keys-postmortem.md, a check that cannot fail). So
this builds a copy of fsd, outside the tree, once clean and once with each
spelling an arm might write appended to one of its files, and requires the
clean build to succeed and each other to fail as named: a type error or the
`unsafe` lint at the line added, the assert by its own message, which this
tree writes (rustc reports it at the assert, not the caller).

Every `BLOCK_*` call number in syscall-abi is tried through `syscall4`, read
from the crate rather than listed here, so a call added there later is
refused or this fails. A `BLOCK_*` constant that is not a call is told by its
name (`BLOCK_ERR_*`, `BLOCK_SECTOR_SIZE`); a new one of those fails here
until NOT_CALLS names it, which is the safe direction.

It runs `cargo build`, not `cargo check`: the assert is evaluated when
`syscall4` is instantiated for a number, which `cargo check` never does. The
mutants calling it are held by a `#[used]` static so they are built though
nothing calls them (a dead caller is never instantiated, and never runs
either). The copy,
with the workspace's `[profile.*]` tables and programs/linker.ld beside it
for the link, lives under target/check-disk-handle/, which git ignores, so
the working tree is never touched. What it cannot see: a `Device::claim`
that answers `Some` to a second caller (a run-time promise), and `unsafe`
code in main.rs or disk.rs, the two files that may hold it. Two things no
build case can see are read from the source instead (text_checks): that only
`Device::disk` returns a `Disk`, and that no arm names `Device`.
"""
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
FSD = os.path.join(ROOT, "programs", "servers", "fsd")
ABI_SRC = os.path.join(ROOT, "syscall-abi", "src", "lib.rs")
WORK = os.path.join(ROOT, "target", "check-disk-handle")
COPY = os.path.join(WORK, "fsd")

# The files that may hold `unsafe`: main.rs (the syscall wrappers) and disk.rs
# (its own svc). Every other file in fsd's src/ must forbid it.
MAY_BE_UNSAFE = {"main", "disk"}
# The one module besides main.rs that may name `Device`: vfs.rs mounts with
# it. A filesystem arm holding one could make as many `Disk`s as it liked.
MAY_NAME_DEVICE = {"main", "disk", "vfs"}
# The BLOCK_* constants in syscall-abi that are not call numbers.
NOT_CALLS = {"BLOCK_SECTOR_SIZE"}
# The assert's message in syscall4 and syscall5.
ASSERT_MESSAGE = "a device call is disk.rs's alone"

DEVICE = "crate::disk::Device"
DISK = "crate::disk::Disk"
# A caller kept live: a `#[used]` static holding it is built though nothing
# calls it, so `syscall4` is instantiated for its number (`#[no_mangle]` would
# do it too, but the filesystem modules' `forbid(unsafe_code)` refuses that).
LIVE = "fn mutant() -> u64 { %s }\n#[used]\nstatic MUTANT: fn() -> u64 = mutant;"


def block_calls():
    """Every BLOCK_* call number syscall-abi defines, by name."""
    with open(ABI_SRC) as f:
        names = re.findall(r"^pub const (BLOCK_[A-Z_0-9]+): u64", f.read(), re.M)
    return [n for n in names if not n.startswith("BLOCK_ERR_") and n not in NOT_CALLS]


def fs_modules():
    """Every module in fsd's src/ but the two that may hold `unsafe`, read from
    the directory, so a module added later is checked without a word here."""
    names = sorted(n[:-3] for n in os.listdir(os.path.join(FSD, "src")) if n.endswith(".rs"))
    return [n for n in names if n not in MAY_BE_UNSAFE]


def code_of(module):
    """`module`.rs with its comments removed (line comments, doc comments
    among them), for the text checks."""
    with open(os.path.join(FSD, "src", module + ".rs")) as f:
        return re.sub(r"//[^\n]*", "", f.read())


def device_literal():
    """`Device` built by hand with every field it has, each `unimplemented!()`,
    read from disk.rs, so the literal fails on privacy alone whatever the
    fields are called."""
    body = re.search(r"pub struct Device \{(.*?)\n\}", code_of("disk"), re.S).group(1)
    fields = re.findall(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(\w+)\s*:", body, re.M)
    return "%s { %s }" % (DEVICE, ", ".join("%s: unimplemented!()" % f for f in fields))


def text_checks():
    """What no build can see, read from the source: in disk.rs one function
    returns a `Disk`, `Device::disk`; and no module but MAY_NAME_DEVICE names
    `Device`, so no arm takes one (a new `pub fn` returning a `Disk`, or an
    arm's `mount_at` given a `&Device`, compiles and passes every build
    case). Text, not types: a check of last resort. Returns failures."""
    failures = []
    returning = re.findall(r"fn\s+(\w+)[^{;]*->\s*(?:Option<|Result<)?\s*Disk\b", code_of("disk"))
    if returning != ["disk"]:
        failures.append("disk.rs: the functions returning a Disk are %s, not only `disk`" % returning)
    for module in fs_modules():
        if module not in MAY_NAME_DEVICE and re.search(r"\bDevice\b", code_of(module)):
            failures.append("%s.rs names `Device`, which only %s may" % (module, ", ".join(sorted(MAY_NAME_DEVICE))))
    return failures


def mutants():
    """(name, file, code appended, how it must fail): ("code", E....) and
    ("unsafe",) at the added line, ("assert",) by its message."""
    m = [
        ("a Disk made by hand", "exfat", "fn mutant() -> %s { %s(()) }" % (DISK, DISK), ("code", "E0603")),
        ("a Disk cloned", "exfat", "fn mutant(d: &%s) -> %s { %s::clone(d) }" % (DISK, DISK, DISK), ("code", "E0277")),
        ("a Disk from a constructor", "exfat", "fn mutant() -> %s { %s::new() }" % (DISK, DISK), ("code", "E0599")),
        ("a Disk by default", "exfat", "fn mutant() -> %s { <%s as Default>::default() }" % (DISK, DISK),
         ("code", "E0277")),
        ("a Device made by hand", "exfat", "fn mutant() -> %s { %s }" % (DEVICE, device_literal()),
         ("code", "E0451")),
        ("a Device cloned", "exfat", "fn mutant(d: &%s) -> %s { %s::clone(d) }" % (DEVICE, DEVICE, DEVICE),
         ("code", "E0277")),
        ("disk.rs's svc called", "exfat", "fn mutant() -> u64 { crate::disk::block_svc(0, 0, 0) }",
         ("code", "E0603")),
        ("MOUNT through syscall4", "exfat",
         LIVE % "crate::syscall4::<{ syscall_abi::MOUNT }>(1, 0, 0, 0)", ("assert",)),
        ("BLOCK_READ through syscall5", "exfat",
         LIVE % "crate::syscall5::<{ syscall_abi::BLOCK_READ }>(0, 0, 0, 0, 0)", ("assert",)),
        ("an svc of exfat.rs's own", "exfat",
         'fn mutant() -> u64 { let r: u64; unsafe { core::arch::asm!("svc #0", out("x0") r) }; r }',
         ("unsafe",)),
    ]
    for call in block_calls():
        m.append(("%s through syscall4" % call, "exfat",
                  LIVE % ("crate::syscall4::<{ syscall_abi::%s }>(0, 0, 0, 0)" % call), ("assert",)))
    for module in fs_modules():
        m.append(("a Disk zeroed in %s.rs" % module, module,
                  "fn mutant() -> %s { unsafe { core::mem::zeroed() } }" % DISK, ("unsafe",)))
    return m


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
    # does not take the copy for a member of the repository's, with the
    # repository's profiles, so the copy builds as fsd ships.
    toml = re.sub(r'path = "(\.\./[^"]+)"',
                  lambda m: 'path = "%s"' % os.path.normpath(os.path.join(FSD, m.group(1))), toml)
    with open(os.path.join(ROOT, "Cargo.toml")) as f:
        profiles = re.findall(r"^\[profile\.[^\n]*\n(?:(?!\[)[^\n]*\n?)*", f.read(), re.M)
    with open(os.path.join(COPY, "Cargo.toml"), "w") as f:
        f.write(toml + "\n[workspace]\n\n" + "\n".join(p.strip() + "\n" for p in profiles))


def build(module=None, extra=None):
    """Build the copy with `extra` appended to `module`.rs: (exit code,
    stderr, the line `extra` starts at). The file is put back afterwards."""
    line = 0
    if module:
        source = os.path.join(FSD, "src", module + ".rs")
        with open(source) as f:
            text = f.read()
        before = (text if text.endswith("\n") else text + "\n") + "\n#[allow(dead_code)]\n"
        with open(os.path.join(COPY, "src", module + ".rs"), "w") as f:
            f.write(before + extra + "\n")
        line = before.count("\n") + 1
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(WORK, "target"))
    r = subprocess.run(["cargo", "build", "--release", "--target", "aarch64-unknown-none"],
                       cwd=COPY, env=env, capture_output=True, text=True)
    if module:
        shutil.copy(source, os.path.join(COPY, "src", module + ".rs"))
    return r.returncode, r.stderr, line


def main():
    failures = 0
    for problem in text_checks():
        failures += 1
        print("check-disk-handle: FAIL " + problem)
    if not failures:
        print("check-disk-handle: ok   only Device::disk returns a Disk, and no arm names Device")
    make_copy()
    code, err, _ = build()
    if code != 0:
        print("check-disk-handle: FAIL the unchanged copy of fsd does not build:\n" + err[-2000:])
        return 1
    print("check-disk-handle: ok   the unchanged copy builds")
    for name, module, extra, how in mutants():
        code, err, line = build(module, extra)
        at = r"\s*--> [^\n]*%s\.rs:%d:" % (module, line)
        if how[0] == "code":
            want = "%s at %s.rs:%d" % (how[1], module, line)
            seen = re.search(r"error\[%s\][^\n]*\n%s" % (how[1], at), err)
        elif how[0] == "unsafe":
            want = "the unsafe_code lint at %s.rs:%d" % (module, line)
            seen = re.search(r"error: usage of an `unsafe` block[^\n]*\n%s" % at, err)
        else:
            # Anywhere in the output: rustc has moved the message between the
            # error line and a note before, and only the assert prints it.
            want = "E0080 with the assert's message"
            seen = "error[E0080]" in err and ASSERT_MESSAGE in err
        if code != 0 and seen:
            print("check-disk-handle: ok   %s is refused (%s)" % (name, want))
        else:
            failures += 1
            print("check-disk-handle: FAIL %s %s" % (
                name, "builds" if code == 0 else "fails, but not with %s:\n%s" % (want, err[-1500:])))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())

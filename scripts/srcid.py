#!/usr/bin/env python3
r"""The source identity of a build: what an image was staged from.

    python3 scripts/srcid.py begin <file> <dir>...          # `make esp`, before the build
    python3 scripts/srcid.py write <stamp> <file> <dir>...  # `make esp`, after it
    python3 scripts/srcid.py check <image>                  # exit 0 when the image is current
    python3 scripts/srcid.py require <image>                # exit 2 unless it is (run-guest.sh)

WHY THIS EXISTS

The rigs used to refuse a stale image by comparing modification times: the
image against the kernel or netd binary it was staged from. That cannot see the
case it most needs to. A mutation control edits a source, rebuilds the image,
runs the rig and expects a failure; `git checkout` then restores the source.
The binaries and the image are still the mutant's, and every one of them is
NEWER than anything the guard compares, so the next direct run of a rig boots
the mutant and grades it as the tree. On 2026-10-09 that cost a hang and six
debug runs (docs/work-journal/2026-10-09.md, the stale reply's second design).

So the stamp records content, not times. `make esp` stages
`\EFI\ORBS\SRCID.TXT`: the git tree id of the working tree (tracked files with
their uncommitted changes, plus untracked files git does not ignore, hashed
into a throwaway copy of the index so the real one is never touched). The tree
is hashed before the programs are built and again after, and a difference
fails the build: a stamp taken only at the end would name an edit made during
the build that the binaries never saw. `check` recomputes the identity and
refuses on any difference. Reverting a mutation changes the tree, so the stamp
no longer matches, whatever the times say.

The stamp lives INSIDE the ESP, so it is in every image built from it and in
every copy a rig makes, and `check` reads it from the image's raw bytes (each
filesystem here stores a small file's data unencoded), or from an ESP
directory a rig boots through vvfat. A stamp beside the image would not do:
`make images-2vm` stages the ESP twice, and a rig's copy would have none. An
ESP staged again without the image being rebuilt changes nothing in the image,
so it cannot make a stale image look current. The ext2 and exFAT payloads
carry a copy as `/etc/srcid`, so an image whose data partition was built from
an older ESP holds two different stamps, and that is refused too.

What is left out, and why: `docs/` and the rigs themselves (`scripts/test-*`)
feed no image, and editing a rig between runs of it is the normal way to build
one. Anything else that changed refuses, including a change no image depends
on; the cost of that is one `make image`, and the cost of the opposite mistake
was an afternoon.

The stamp also names the directories an image takes programs from in
another project (DevTools's cpp and Edit, when present): exactly the ones the
Makefile compiles, hashed file by file and read only, so nothing is written
into that project and its own tests and docs changing refuses nothing here.

The identity is computed once per process, so a rig that boots several times
is judged against the tree at its first boot.

OUROBOROS_STALE_OK=1 boots anyway and says so on stderr, for driving an old
image on purpose (comparing builds by hand). A rig run that way is not a
result.
"""
import hashlib
import mmap
import re
import os
import subprocess
import sys
import tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
# The stamp's framing, found by a scan of the image's raw bytes.
HEAD = b"ouroboros-srcid v1\n"
STAMP_RX = re.compile(re.escape(HEAD) + rb"((?:[^\n\0]*\t[0-9a-z:]+\n){1,8})end\n")
# Pathspecs left out of this repository's identity; see the module doc.
EXCLUDE = ("docs", "scripts/test-*")


def git(cwd, *args, env=None):
    return subprocess.run(
        ["git", *args], cwd=cwd, env=env, check=True, capture_output=True, text=True,
    ).stdout.strip()


def repo_tree_id():
    """The git tree id of this repository's working tree as it is on disk.

    A copy of the real index is the starting point, so unchanged files are not
    rehashed; `add -A` brings in every change and every untracked, unignored
    file, and EXCLUDE is dropped before writing. The real index is never
    touched; the objects go into this repository's own store, as any `git add`
    would, and `git gc` takes them."""
    index = git(ROOT, "rev-parse", "--path-format=absolute", "--git-path", "index")
    with tempfile.TemporaryDirectory() as tmp:
        tmp_index = os.path.join(tmp, "index")
        if os.path.exists(index):
            with open(index, "rb") as src, open(tmp_index, "wb") as dst:
                dst.write(src.read())
        env = dict(os.environ, GIT_INDEX_FILE=tmp_index)
        git(ROOT, "add", "-A", "--", ".", env=env)
        git(ROOT, "rm", "-r", "-q", "--cached", "--ignore-unmatch", "--", *EXCLUDE, env=env)
        return git(ROOT, "write-tree", env=env)


def files_hash(path):
    """A hash of every regular file below `path`, names and contents, read only.

    For the directories another project's sources are compiled from: nothing is
    written there, not even into its repository's object store (a project
    changes only its own tree, ~/.claude/CLAUDE.md), and only the directories
    the Makefile compiles from are named, so that project's own tests and docs
    changing does not refuse a boot here. A missing directory hashes as such."""
    h = hashlib.sha256()
    if not os.path.isdir(path):
        return "missing"
    for d, dirs, files in os.walk(path):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(d, f)
            if not os.path.isfile(p):
                continue
            h.update(os.path.relpath(p, path).encode() + b"\0")
            with open(p, "rb") as fh:
                h.update(hashlib.sha256(fh.read()).digest())
    return h.hexdigest()


_identity = {}


def identity(dirs):
    """One line per tree: this repository first, then each other directory.

    Computed once per process for a given list: a rig that boots several times
    is judged against the tree as it was at its first boot, so an edit made
    while it runs neither aborts it part way nor costs a rehash per boot."""
    key = tuple(dirs)
    if key not in _identity:
        lines = [f".\t{repo_tree_id()}"]
        for d in dirs:
            lines.append(f"{d}\tfiles:{files_hash(os.path.join(ROOT, d))}")
        _identity[key] = "\n".join(lines) + "\n"
    return _identity[key]


def begin(path, dirs):
    """Record the tree as the build starts (`make esp`'s first prerequisite)."""
    with open(path, "w") as f:
        f.write(identity(dirs))


def write(stamp, begun, dirs):
    """Write the stamp, unless the tree changed while the programs were built.

    The stamp must name the sources the binaries were compiled from. Hashed at
    the end alone, an edit made during the build would be recorded as what the
    image came from while the binaries predate it; so the tree is hashed at
    both ends and a difference fails the build rather than stamping it."""
    with open(begun) as f:
        then = f.read()
    now = identity(dirs)
    if now != then:
        print("srcid: the source changed while the programs were being built, so "
              "no stamp can say what they were built from - run the build again",
              file=sys.stderr)
        return 1
    with open(stamp, "wb") as f:
        f.write(HEAD + now.encode() + b"end\n")
    return 0


def stamps_in(path):
    """Every distinct stamp body in an image's raw bytes, or in an ESP
    directory's \\EFI\\ORBS\\SRCID.TXT (test-early-fault.py boots one)."""
    if os.path.isdir(path):
        path = os.path.join(path, "EFI", "ORBS", "SRCID.TXT")
        if not os.path.exists(path):
            return set()
    with open(path, "rb") as f:
        if os.fstat(f.fileno()).st_size == 0:
            return set()
        with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
            return {match.group(1).decode() for match in STAMP_RX.finditer(m)}


def check(image):
    """None when `image` (a disk image or an ESP directory) is current, else
    the reason it is not."""
    if not os.path.exists(image):
        return f"there is nothing at {image} to check"
    found = stamps_in(image)
    if not found:
        return f"{image} carries no source stamp (\\EFI\\ORBS\\SRCID.TXT), so nothing says what it was built from"
    if len(found) > 1:
        return f"{image} carries {len(found)} different source stamps, so parts of it were built at different times"
    recorded = found.pop()
    dirs = [line.split("\t", 1)[0] for line in recorded.splitlines()[1:]]
    if identity(dirs) != recorded:
        return f"the source has changed since {image} was staged (a reverted mutation looks like this)"
    return None


def require_current(image, label=""):
    """Exit 2 unless `image` was built from the tree as it is now: the guard
    every guest boot goes through (drive-qemu.py's Guest)."""
    why = check(image)
    if why is None:
        return
    if os.environ.get("OUROBOROS_STALE_OK") == "1":
        print(f"{label}OUROBOROS_STALE_OK: booting anyway, NOT A RESULT: {why}", file=sys.stderr)
        return
    print(f"{label}refusing to boot {image}: {why} - run make image (or the rig's make target)",
          file=sys.stderr)
    sys.exit(2)


def main(argv):
    if len(argv) >= 2 and argv[0] == "begin":
        begin(argv[1], argv[2:])
        return 0
    if len(argv) >= 3 and argv[0] == "write":
        return write(argv[1], argv[2], argv[3:])
    if len(argv) == 2 and argv[0] == "check":
        why = check(argv[1])
        if why:
            print(f"srcid: {why}", file=sys.stderr)
            return 1
        return 0
    if len(argv) == 2 and argv[0] == "require":
        require_current(argv[1], "srcid: ")
        return 0
    print(__doc__.split("\n\n")[1], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

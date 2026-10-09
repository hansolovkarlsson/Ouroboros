#!/usr/bin/env python3
r"""The source identity of a build: what an image was staged from.

    python3 scripts/srcid.py write <stamp> <dir>...   # `make esp` stages the stamp
    python3 scripts/srcid.py check <image>            # exit 0 when the image is current

WHY THIS EXISTS

The rigs used to refuse a stale image by comparing modification times: the
image against the kernel or netd binary it was staged from. That cannot see the
case it most needs to. A mutation control edits a source, rebuilds the image,
runs the rig and expects a failure; `git checkout` then restores the source.
The binaries and the image are still the mutant's, and every one of them is
NEWER than anything the guard compares, so the next direct run of a rig boots
the mutant and grades it as the tree. On 2026-10-09 that cost a hang and six
debug runs (docs/work-journal/2026-10-09.md, the stale reply's second design).

So the stamp records content, not times. `make esp` stages, after every program
is built, `\EFI\ORBS\SRCID.TXT`: the git tree id of the working tree as it is
then (tracked files with their uncommitted changes, plus untracked files git
does not ignore, hashed into a throwaway index so the real one is never
touched). `check` recomputes it and refuses on any difference. Reverting a
mutation changes the tree, so the stamp no longer matches, whatever the times
say.

The stamp lives INSIDE the ESP, so it is in every image built from it and in
every copy a rig makes, and `check` reads it from the image's raw bytes (each
filesystem here stores a small file's data unencoded). A stamp beside the image
would not do: `make images-2vm` stages the ESP twice, and a rig's copy would
have none. An ESP staged again without the image being rebuilt changes nothing
in the image, so it cannot make a stale image look current. An image holding
two different stamps (a partition built from an older ESP) is refused too.

What is left out, and why: `docs/` and the rigs themselves (`scripts/test-*`)
feed no image, and editing a rig between runs of it is the normal way to build
one. Anything else that changed refuses, including a change no image depends
on; the cost of that is one `make image`, and the cost of the opposite mistake
was an afternoon.

The stamp also names the other trees an image takes programs from (DevTools's
cpp and Edit, when present), each identified the same way within its own
repository, so `check` recomputes exactly what `write` hashed.

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


def tree_id(path, exclude=()):
    """The git tree id of `path` as it is on disk, in its own repository.

    A copy of the real index is the starting point, so unchanged files are not
    rehashed; `add -A` brings in every change and every untracked, unignored
    file below `path`, and the excluded pathspecs are dropped before writing.
    A directory outside any repository is hashed file by file instead."""
    path = os.path.abspath(path)
    try:
        top = git(path, "rev-parse", "--show-toplevel")
    except (subprocess.CalledProcessError, FileNotFoundError):
        return "files:" + files_hash(path)
    rel = os.path.relpath(path, top)
    index = git(top, "rev-parse", "--path-format=absolute", "--git-path", "index")
    with tempfile.TemporaryDirectory() as tmp:
        tmp_index = os.path.join(tmp, "index")
        if os.path.exists(index):
            with open(index, "rb") as src, open(tmp_index, "wb") as dst:
                dst.write(src.read())
        env = dict(os.environ, GIT_INDEX_FILE=tmp_index)
        git(top, "add", "-A", "--", rel, env=env)
        if exclude:
            git(top, "rm", "-r", "-q", "--cached", "--ignore-unmatch", "--", *exclude, env=env)
        if rel == ".":
            return git(top, "write-tree", env=env)
        return git(top, "write-tree", f"--prefix={rel}/", env=env)


def files_hash(path):
    h = hashlib.sha256()
    for d, dirs, files in os.walk(path):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(d, f)
            h.update(os.path.relpath(p, path).encode() + b"\0")
            with open(p, "rb") as fh:
                h.update(hashlib.sha256(fh.read()).digest())
    return h.hexdigest()


def identity(dirs):
    """One line per tree: this repository first, then each other directory."""
    lines = [f".\t{tree_id(ROOT, EXCLUDE)}"]
    for d in dirs:
        lines.append(f"{d}\t{tree_id(os.path.join(ROOT, d))}")
    return "\n".join(lines) + "\n"


def write(stamp, dirs):
    with open(stamp, "wb") as f:
        f.write(HEAD + identity(dirs).encode() + b"end\n")


def stamps_in(image):
    """Every distinct stamp body in the image's raw bytes."""
    with open(image, "rb") as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as m:
        return {match.group(1).decode() for match in STAMP_RX.finditer(m)}


def check(image):
    """None when `image` is current, else the reason it is not."""
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
    if len(argv) >= 2 and argv[0] == "write":
        write(argv[1], argv[2:])
        return 0
    if len(argv) == 2 and argv[0] == "check":
        why = check(argv[1])
        if why:
            print(f"srcid: {why}", file=sys.stderr)
            return 1
        return 0
    print(__doc__.split("\n\n")[1], file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

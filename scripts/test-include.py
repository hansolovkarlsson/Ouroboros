#!/usr/bin/env python3
"""The C headers on the disk, on every image, against the host's staging tree.

    python3 scripts/test-include.py      (or `make test-include`, which builds the images)

Step 6 of docs/roadmap/roadmap-c-hosting.md: `make esp` stages picolibc's
headers and a generated target.h in /include, and clang's freestanding headers
in /include/clang (the accepted handoff 2026-10-01-from-proem-system-dirs.md),
and the ext2 and exFAT images copy the same tree. One boot of each image
(FAT32 build/esp.img, ext2 build/espext2.img, exFAT build/espexfat.img):

- every staged file, by path: `ls -l` with file operands stats each one
  (no directory listing involved), and the size must be the host's. This
  proves existence and size only: `ls` echoes an operand as typed, and FAT32
  and exFAT look names up regardless of case;
- the listing, `ls -l` of each directory: names with their case, `__` and `_`
  prefixes, subdirectories. One small enough to come back in one NP_READDIR
  reply (LISTING_MAX) must list exactly the host's names and sizes; a larger
  one is cut at that size with no sign of it (a gap of its own in
  docs/ROADMAP.md), so every name it does list must be a staged one, in its
  case, with its size;
- `cat` of three files, a nested one, a `__` one and target.h, must give the
  host's bytes;
- names the guest's fsd creates keep their case: a lowercase file, a mixed
  case one, a directory, and a case-only `mv`, read back by `ls /` (on FAT32
  a lowercase 8.3 name is stored with byte 12's case flags, a mixed one as a
  long name).

No fault line in QEMU's trace. About five minutes. Run it whenever the
header staging in the Makefile, or a filesystem's directory listing,
changes.
"""
import importlib.util
import os
import re
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

STAGED = os.path.join(ROOT, "build", "esp", "include")
IMAGES = [("fat32", "esp.img"), ("ext2", "espext2.img"), ("exfat", "espexfat.img")]
CATS = ["sys/_types.h", "clang/__stddef_null.h", "target.h"]
# Names made on the guest, then `ls /`: each must list exactly so, and its
# uppercase form must not. CASE.TXT is renamed to case.txt, a case-only mv:
# with -f, since on FAT32 and exFAT the destination answers "present" (it is
# the source), and mv will not replace one without it (a gap in ROADMAP.md).
MADE = ["touch /lower.txt", "touch /Mixed.Txt", "mkdir /lowdir", "touch /CASE.TXT",
        "mv -f /CASE.TXT /case.txt"]
MADE_NAMES = ["lower.txt", "Mixed.Txt", "lowdir", "case.txt"]
FENCE = "echo ==fence=="
# mode, owner, group, size, then a date and time or a lone `-` (ext2 records
# none here), then the name.
# What one `ls` of a directory can show: the 512-byte listing buffer, filled
# with `name\n` or `name/\n` per entry by NP_READDIR, which stops at the first
# name that does not fit.
LISTING_MAX = 512
# The shell's input line and word limits, for the file-operand commands.
LINE_MAX = 120
WORDS_MAX = 14
LS_LINE = re.compile(r"^([d-][rwx-]{9})\s+\S+\s+\S+\s+(\d+)\s+(?:\d{4}-\d\d-\d\d\s+\d\d:\d\d|-)\s+(\S+?)\r?$", re.M)


def host_dirs():
    """{guest dir: {name: size or None for a directory}} for the staged tree."""
    out = {}
    for dirpath, dirnames, filenames in os.walk(STAGED):
        rel = os.path.relpath(dirpath, STAGED)
        guest = "/include" if rel == "." else "/include/" + rel
        entries = {d: None for d in dirnames}
        entries.update({f: os.path.getsize(os.path.join(dirpath, f)) for f in filenames})
        out[guest] = entries
    return out


def guest_listing(block):
    """{name: size or None} from one `ls -l` block."""
    got = {}
    for perm, size, name in LS_LINE.findall(block):
        name = name.rstrip("/")
        got[name] = None if perm.startswith("d") else int(size)
    return got


def listing_bytes(entries):
    return sum(len(n) + (2 if size is None else 1) for n, size in entries.items())


def operand_commands(want):
    """`cd DIR` then `ls -l` commands naming every staged file, each within the
    shell's line and word limits: [(dir, command, [names])]."""
    out = []
    for d in sorted(want):
        files = sorted(n for n, size in want[d].items() if size is not None)
        chunk = []
        for n in files + [None]:
            cmd = "ls -l " + " ".join(chunk + ([n] if n else []))
            if n is None or len(cmd) > LINE_MAX or len(chunk) + 1 > WORDS_MAX:
                if chunk:
                    out.append((d, "ls -l " + " ".join(chunk), chunk))
                chunk = [n] if n else []
            else:
                chunk.append(n)
    return out


def boot(name, image, dirs, operands):
    copy = os.path.join(ROOT, "build", f"test-include-{name}.img")
    shutil.copyfile(os.path.join(ROOT, "build", image), copy)
    steps = [("login:", "root"), ("assword", "root")]
    for d in dirs:
        steps += [("# ", FENCE), ("# ", f"ls -l {d}")]
    here = None
    for d, cmd, _ in operands:
        if d != here:
            steps += [("# ", f"cd {d}")]
            here = d
        steps += [("# ", FENCE), ("# ", cmd)]
    for f in CATS:
        steps += [("# ", FENCE), ("# ", f"cat /include/{f}")]
    steps += [("# ", "cd /"), ("# ", FENCE)]
    steps += [("# ", c) for c in MADE]
    steps += [("# ", FENCE), ("# ", "ls /")]
    steps += [("# ", FENCE), ("# ", "")]
    guest = drive_qemu.Guest(copy, label=f"test-include {name}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
    finally:
        guest.stop()
        os.remove(copy)
    # After stop: QEMU buffers its -d int trace, so a fault in the last
    # command is only in the file once QEMU is gone (drive-qemu.py's main).
    faults = guest.aborts()
    with open(os.path.join(ROOT, "build", f"test-include-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def cat_text(block, command):
    """What `cat` printed: the lines after its command echo's own line ending,
    up to the exit line. Only that one line ending is dropped, so a file that
    begins with a blank line still compares (the review of #225)."""
    after = block.split(command, 1)[-1].replace("\r\n", "\n")
    if after.startswith("\n"):
        after = after[1:]
    return after.split("Ouroboros kernel: task", 1)[0]


def main() -> int:
    want = host_dirs()
    dirs = sorted(want)
    whole = {d for d in dirs if listing_bytes(want[d]) <= LISTING_MAX}
    operands = operand_commands(want)
    nfiles = sum(len(names) for _, _, names in operands)
    nblocks = len(dirs) + len(operands) + len(CATS) + 2
    print(f"     {nfiles} staged files; whole listings compared for {', '.join(sorted(whole))}")
    checks = [("the staged tree has /include and /include/clang",
               "/include" in want and "/include/clang" in want),
              ("some directory's listing fits one reply, so whole listings are compared at all",
               len(whole) > 0)]
    for name, image in IMAGES:
        driven, out, faults = boot(name, image, dirs, operands)
        blocks = re.split(r"^==fence==\r?$", out, flags=re.M)[1:]
        checks.append((f"{name}: driven to the end", driven))
        # Every fenced block came back, so no check below is dropped by a
        # short zip (the review of #225).
        checks.append((f"{name}: all {nblocks} fenced blocks came back", len(blocks) >= nblocks))
        stated, wrong = 0, []
        for (d, _, names), block in zip(operands, blocks[len(dirs):]):
            got = guest_listing(block)
            for n in names:
                if got.get(n) == want[d][n]:
                    stated += 1
                else:
                    wrong.append(f"{d}/{n}")
        checks.append((f"{name}: every staged file exists by path with its size ({stated}/{nfiles})"
                       + (f", not {wrong[:4]}" if wrong else ""), stated == nfiles))
        for d, block in zip(dirs, blocks):
            got = guest_listing(block)
            missing = sorted(set(want[d]) - set(got))
            extra = sorted(set(got) - set(want[d]))
            wrong = sorted(n for n in want[d] if n in got and got[n] != want[d][n])
            if d in whole:
                ok = not (missing or extra or wrong)
                what = f"{len(want[d])} names and sizes as staged"
            else:
                # Cut at one reply: what it lists must be staged, as staged.
                ok = bool(got) and not (extra or wrong)
                what = f"{len(got)} of {len(want[d])} names listed (one reply), each staged, in its case"
            detail = "" if ok else f" (missing {missing[:3]}, extra {extra[:3]}, wrong size {wrong[:3]})"
            checks.append((f"{name}: ls -l {d}: {what}{detail}", ok))
        for f, block in zip(CATS, blocks[len(dirs) + len(operands):]):
            with open(os.path.join(STAGED, f)) as fh:
                host = fh.read()
            checks.append((f"{name}: cat /include/{f} gives the staged bytes",
                           cat_text(block, f"cat /include/{f}") == host))
        root = blocks[len(dirs) + len(operands) + len(CATS) + 1] if len(blocks) >= nblocks else ""
        listed = [w.rstrip("/") for w in root.split("ls /", 1)[-1].split("Ouroboros kernel", 1)[0].split()]
        for n in MADE_NAMES:
            checks.append((f"{name}: a name made on the guest keeps its case: {n}",
                           n in listed and (n == n.upper() or n.upper() not in listed)))
        checks.append((f"{name}: no fault lines", faults == 0))
    failed = 0
    for what, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {what}")
        failed += 0 if ok else 1
    print("transcripts: build/test-include-{fat32,ext2,exfat}.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

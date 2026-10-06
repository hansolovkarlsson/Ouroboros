#!/usr/bin/env python3
"""The C headers on the disk, on every image, against the host's staging tree.

    python3 scripts/test-include.py      (or `make test-include`, which builds the images)

Step 6 of docs/roadmap/roadmap-c-hosting.md: `make esp` stages picolibc's
headers and a generated target.h in /include, and clang's freestanding headers
in /include/clang (the accepted handoff 2026-10-01-from-proem-system-dirs.md),
and the ext2 and exFAT images copy the same tree. One boot of each image
(FAT32 build/esp.img, ext2 build/espext2.img, exFAT build/espexfat.img):

- every staged file, by path: `ls -l` with file operands stats each one
  (no directory listing involved), and the size must be the host's;
- the whole listing, `ls -l` of a directory, for each directory small enough
  to come back in one NP_READDIR reply (LISTING_MAX): names with their case,
  `__` and `_` prefixes, subdirectories, exactly the host's. A larger one is
  cut at that size with no sign of it, a gap of its own in docs/ROADMAP.md,
  so its listing is not compared;
- `cat` of three files, a nested one, a `__` one and target.h, must give the
  host's bytes.

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
    steps += [("# ", "cd /")]
    steps += [("# ", FENCE), ("# ", "")]
    guest = drive_qemu.Guest(copy, label=f"test-include {name}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(copy)
    with open(os.path.join(ROOT, "build", f"test-include-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def cat_text(block, command):
    """What `cat` printed: the lines after its command echo, up to the exit line."""
    after = block.split(command, 1)[-1]
    body = after.split("Ouroboros kernel: task", 1)[0]
    return body.replace("\r\n", "\n").lstrip("\n")


def main() -> int:
    want = host_dirs()
    dirs = sorted(d for d in want if listing_bytes(want[d]) <= LISTING_MAX)
    operands = operand_commands(want)
    nfiles = sum(len(names) for _, _, names in operands)
    print(f"     {nfiles} staged files; whole listings compared for {', '.join(dirs)}")
    checks = [("the staged tree has /include and /include/clang",
               "/include" in want and "/include/clang" in want),
              ("some directory's listing fits one reply, so listings are compared at all",
               len(dirs) > 0)]
    for name, image in IMAGES:
        driven, out, faults = boot(name, image, dirs, operands)
        blocks = re.split(r"^==fence==\r?$", out, flags=re.M)[1:]
        checks.append((f"{name}: driven to the end", driven))
        stated, wrong = 0, []
        for (d, _, names), block in zip(operands, blocks[len(dirs):]):
            got = guest_listing(block)
            for n in names:
                if got.get(n) == want[d][n]:
                    stated += 1
                else:
                    wrong.append(f"{d}/{n}")
        checks.append((f"{name}: every staged file is there by path with its size ({stated}/{nfiles})"
                       + (f", not {wrong[:4]}" if wrong else ""), stated == nfiles))
        for d, block in zip(dirs, blocks):
            got = guest_listing(block)
            missing = sorted(set(want[d]) - set(got))
            extra = sorted(set(got) - set(want[d]))
            wrong = sorted(n for n in want[d] if n in got and got[n] != want[d][n])
            ok = not (missing or extra or wrong)
            detail = "" if ok else f" (missing {missing[:3]}, extra {extra[:3]}, wrong size {wrong[:3]})"
            checks.append((f"{name}: ls -l {d}: {len(want[d])} names and sizes as staged{detail}", ok))
        for f, block in zip(CATS, blocks[len(dirs) + len(operands):]):
            with open(os.path.join(STAGED, f)) as fh:
                host = fh.read()
            checks.append((f"{name}: cat /include/{f} gives the staged bytes",
                           cat_text(block, f"cat /include/{f}") == host))
        checks.append((f"{name}: no fault lines", faults == 0))
    failed = 0
    for what, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {what}")
        failed += 0 if ok else 1
    print("transcripts: build/test-include-{fat32,ext2,exfat}.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

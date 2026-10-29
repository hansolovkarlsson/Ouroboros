#!/usr/bin/env python3
"""errno from the C file layer, on FAT32 and on ext2.

    python3 scripts/test-cerrno.py      (or `make test-cerrno`, which builds both images)

Step 2 of docs/roadmap/roadmap-c-hosting.md. `/bin/CERRNO` (libc/cerrno.c,
picolibc) provokes every failure a C program can reach in the file layer and
checks the errno POSIX names for it: a missing file, a path through a file, a
full fd table, a bad fd, an fd used against its mode, a bad lseek, a directory
read, and a directory opened to write, read-write, create or truncate (EISDIR
at the open, since 2026-10-29); and stat(path) (step 4): a missing path, a path through a file, an
empty path, a null struct, a directory, a file against fstat's record, lstat,
a full fd table, a trailing `/` or `/.` (a directory, or ENOTDIR), the console
binding and a path below it, and a /net path (ENOSYS, not answered yet), after
`mount -c /dev/cons` and `mount -n /net`. Two boots, each of a COPY of the image:

- FAT32 (build/esp.img), as root.
- ext2 (build/espext2.img), as root, then as the ordinary `user`, which must
  get EACCES opening /etc/shadow (mode 0600) and yet stat it, as POSIX's stat
  needs no read permission, and stat must give the mode and owner debugfs
  reads off the image for two files (KNOWN_PATHS below): ext2 is the one image
  whose modes fsd enforces. Then, as `user` (/bin/CCREAT, since the medium
  review of #364): /etc/passwd (0644, not `user`'s) opened O_RDONLY|O_CREAT
  must read, since O_CREAT of an existing file asks for no write permission,
  a write through such an fd must be EBADF, since the fd is read-only, and a
  missing /etc/newf must be EACCES at the open, since creating it needs `w`
  on /etc (fsd's `check_access` and the fid's write check, with
  ninep_abi::open_changes). And O_WRONLY|O_CREAT|O_EXCL (`ccreat -x`, since
  2026-10-28, when libc stopped dropping O_EXCL): a new /Users/user/LOCK is
  made and a second open of it is EEXIST, as is one of /Users (a directory)
  and of /etc/shadow, which `user` may not read (EEXIST comes before the
  file's own permission, as in Linux), while a missing /etc/newx is EACCES.
  The FAT32 boot, as root, makes /LOCK and is refused it the second time.
  Then, as `user` (/bin/OPENPROBE, since 2026-10-29), the three `NP_OPEN`
  refusals no libc flag word reaches, each sent raw: O_CREAT|O_EXCL of
  /Users/user/hid/f answers EEXIST while `user` may search hid and EACCES
  once hid is mode 0600, as does a missing name beside it, so the answer
  tells nothing of what is in the directory (fsd's `exists_answer`);
  OPEN_EXCL without OPEN_CREATE answers FS_ERROR for an existing and a
  missing file; and a flag outside OPEN_FLAGS (32, and bit 63) answers
  FS_ERROR, where a plain read open of the same file opens. The probe
  makes its own call, so fsd never answering prints `no answer`, not
  FS_ERROR, and `ls -l` must show hid at drw------- (the chmod took).

Graded on cerrno's own summary line (`cerrno: N checks, 0 failed`, matched
whole, with N exactly ROOT_CHECKS below: fewer is a run cut short, and more
means cerrno.c gained a check this count must be raised for), and
no fault line in QEMU's trace. About two minutes. Run it whenever
libc/src/file.c's error paths, or fsd's answers to them, change.
"""
import importlib.util
import os
import re
import shutil
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

# How many checks cerrno runs as root. A summary with fewer is a run that
# skipped some, which must not pass.
ROOT_CHECKS = 45
SUMMARY = r"cerrno: (\d+) checks, (\d+) failed\r?\n"
# Files cerrno checks stat against, as `<path> <octal mode> <uid> <gid>`, the
# values read from the ext2 partition itself by debugfs (owner_on_image), not
# through anything this library or the guest does. `mke2fs -d` carries the
# host's mode AND owner, so /etc/shadow is owned by whoever built the image
# (501:20 on the usual host, not root: the carried "ext2 uid 501" item), and
# its uid and gid differ, so a swap of the two is caught. PRIVATE.TXT's owner
# is set by debugfs at build time (1000:1000).
KNOWN_PATHS = ["/etc/shadow", "/Users/user/PRIVATE.TXT"]
EXT2_PART = os.path.join(ROOT, "build", "ext2part.img")
# As `user`: open refused, stat allowed, then one check per known file.
USER_CHECKS = 2 + len(KNOWN_PATHS)
# As `user`, O_RDONLY|O_CREAT (/bin/CCREAT): each command and the one line it
# must print, `read` the bytes one read of up to 64 gives of /etc/passwd's
# size on the image (creat_lines).
EBADF, EACCES, EEXIST = 9, 13, 17  # picolibc's sys/errno.h


def excl_lines(lock):
    """O_CREAT|O_EXCL of `lock`, missing: made, and then refused."""
    return [
        (f"ccreat -x {lock}", f"ccreat: {lock} made 1"),
        (f"ccreat -x {lock}", f"ccreat: {lock} open errno {EEXIST}"),
    ]


def creat_lines(size):
    return [
        ("ccreat /etc/passwd", f"ccreat: /etc/passwd read {min(size, 64)}"),
        ("ccreat -w /etc/passwd", f"ccreat: /etc/passwd write errno {EBADF}"),
        ("ccreat /etc/newf", f"ccreat: /etc/newf open errno {EACCES}"),
    ] + excl_lines("/Users/user/LOCK") + [
        ("ccreat -x /Users", f"ccreat: /Users open errno {EEXIST}"),
        ("ccreat -x /etc/shadow", f"ccreat: /etc/shadow open errno {EEXIST}"),
        ("ccreat -x /etc/newx", f"ccreat: /etc/newx open errno {EACCES}"),
    ]


def creat_steps(lines, wait=None):
    """Driver steps for `lines`: each command typed once the one before it
    has printed its line."""
    wait = wait or creat_wait
    return ([(wait(before), cmd) for (before, _), (cmd, _) in zip(lines, lines[1:])]
            + [(wait(lines[-1][0]), "")])


def creat_checks(name, prompt, text, lines):
    """One check per distinct command: the lines it printed, in order,
    against the lines `lines` expects of it (a command typed twice, as the
    lock is, prints twice)."""
    checks = []
    for cmd in dict.fromkeys(cmd for cmd, _ in lines):
        want = [line for c, line in lines if c == cmd]
        got = re.findall(prompt + re.escape(cmd) + r"\r?\n(.*?)\r?$", text, re.M)
        checks.append((f"{name}: `{cmd}` prints {want} ({got})", got == want))
    return checks


def creat_wait(cmd):
    """The line `cmd` prints: the next command is typed only once it is
    there, not on a `$ ` that may be the prompt printed before it."""
    return r"ccreat: " + re.escape(cmd.split()[-1]) + r" [^\n]*\n"


# As `user`, raw NP_OPENs (/bin/OPENPROBE): `hid` is made, searchable, with a
# file in it; the probe lines before CHMOD run with it so, those after with
# hid at mode 0600, which `user` owns and may no longer search.
HID = "/Users/user/hid"
READ, EXCL_CREATE_READ, EXCL_READ = 1, 1 | 4 | 16, 1 | 16
WRITE, TRUNC_READ, CREATE_READ = 2, 1 | 8, 1 | 4  # ninep_abi's OPEN_*
CHMOD = f"chmod 600 {HID}"
# The chmod's own result, so a chmod that did not take is not read as fsd
# answering EEXIST where it should refuse: hid's line in its parent's listing.
LS = "ls -l /Users/user"
HID_MODE = r"^drw------- .* hid/\r?$"
# The kernel's line for a program's exit: `ls` has printed its whole listing,
# whatever order its names sort in (`hid/` sorts first, the review of #369).
EXITED = r"task \d+ exited \(code \d+\)\r?\n"


def probe(flags, path, answer):
    return (f"openprobe {flags} {path}", f"openprobe: {path} {flags} {answer}")


# A directory opened to write, truncate or create is EISDIR to a caller who
# may stat it, before the mode check (`user` may not write /etc) and EACCES
# to one who may not (hid/d once hid is 0600), as EEXIST is (since
# 2026-10-29, the review of #370).
PROBE_BEFORE = [probe(EXCL_CREATE_READ, f"{HID}/f", "FS_ERR_ALREADY_EXISTS"),
                probe(WRITE, f"{HID}/d", "FS_ERR_NOT_A_FILE")]
PROBE_AFTER = [
    probe(EXCL_CREATE_READ, f"{HID}/f", "FS_ERR_PERM"),
    probe(EXCL_CREATE_READ, f"{HID}/g", "FS_ERR_PERM"),
    probe(READ, "/etc/passwd", "opened"),
    probe(EXCL_READ, "/etc/passwd", "FS_ERROR"),
    probe(EXCL_READ, "/etc/nofile", "FS_ERROR"),
    probe(READ | 32, "/etc/passwd", "FS_ERROR"),
    probe(READ | 1 << 63, "/etc/passwd", "FS_ERROR"),
    probe(WRITE, f"{HID}/d", "FS_ERR_PERM"),
    probe(WRITE, "/etc", "FS_ERR_NOT_A_FILE"),
    probe(TRUNC_READ, "/etc", "FS_ERR_NOT_A_FILE"),
    probe(CREATE_READ, "/etc", "FS_ERR_NOT_A_FILE"),
    probe(READ, "/etc", "opened"),
]


def probe_wait(cmd):
    """The line `cmd` prints, as creat_wait."""
    _, flags, path = cmd.split()
    return r"openprobe: " + re.escape(path) + " " + flags + r" [^\n]*\n"


def probe_steps(after):
    """Once the pattern `after` has matched: hid and its file made, the probe
    before the chmod, the chmod, then the rest. The match that `after` waits
    for must be a line, not a prompt: the driver's settle takes the prompt
    after a matched line with it, so a `$ ` wait typed there waits for none."""
    return ([(after, f"mkdir {HID}"), (r"\$ ", f"touch {HID}/f"), (r"\$ ", f"mkdir {HID}/d"),
             (r"\$ ", PROBE_BEFORE[0][0])]
            + creat_steps(PROBE_BEFORE, probe_wait)[:-1]
            + [(probe_wait(PROBE_BEFORE[-1][0]), CHMOD), (r"\$ ", LS), (EXITED, PROBE_AFTER[0][0])]
            + creat_steps(PROBE_AFTER, probe_wait))


def debugfs_tool():
    """The Makefile passes its DEBUGFS; run by hand, ask brew as it does."""
    if os.environ.get("DEBUGFS"):
        return os.environ["DEBUGFS"]
    prefix = subprocess.run(["brew", "--prefix", "e2fsprogs"], capture_output=True, text=True)
    return os.path.join(prefix.stdout.strip(), "sbin", "debugfs")


def size_on_image(path):
    """`path`'s size on the ext2 partition, as debugfs reads the inode, or
    None if it could not."""
    r = subprocess.run([debugfs_tool(), "-R", f"stat {path}", EXT2_PART],
                       capture_output=True, text=True)
    size = re.search(r"Size:\s+(\d+)", r.stdout)
    return int(size.group(1)) if size else None


def owner_on_image(path):
    """[mode (octal, permission bits), uid, gid] of `path` on the ext2
    partition, as debugfs reads the inode, or None if it could not."""
    r = subprocess.run([debugfs_tool(), "-R", f"stat {path}", EXT2_PART],
                       capture_output=True, text=True)
    mode = re.search(r"Mode:\s+0?([0-7]+)", r.stdout)
    owner = re.search(r"User:\s+(\d+)\s+Group:\s+(\d+)", r.stdout)
    if not (mode and owner):
        return None
    return [format(int(mode.group(1), 8) & 0o7777, "o"), owner.group(1), owner.group(2)]


def boot(name, source, steps):
    image = os.path.join(ROOT, "build", f"test-cerrno-{name}.img")
    shutil.copyfile(os.path.join(ROOT, "build", source), image)
    guest = drive_qemu.Guest(image, label=f"test-cerrno {name}: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(image)
    with open(os.path.join(ROOT, "build", f"test-cerrno-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def summaries(out):
    return [(int(n), int(f)) for n, f in re.findall(r"^cerrno: (\d+) checks, (\d+) failed\r?$", out, re.M)]


def main() -> int:
    # The console and /net bindings first: stat of each is checked (the
    # targets open refuses).
    root_steps = [("login:", "root"), ("assword", "root"), ("# ", "mount -c /dev/cons"),
                  ("# ", "mount -n /net"), ("# ", "cerrno")]
    checks = []

    lock = excl_lines("/LOCK")
    d, out, f = boot("fat32", "esp.img", root_steps + [(SUMMARY, lock[0][0])] + creat_steps(lock))
    got = summaries(out)
    checks += [
        ("fat32: driven to the end", d),
        (f"fat32: root, {ROOT_CHECKS} checks, 0 failed", got == [(ROOT_CHECKS, 0)]),
        ("fat32: no fault lines", f == 0),
    ] + creat_checks("fat32: root", r"# ", out.split("cerrno: ", 1)[-1], lock)
    fat32 = out

    known = []
    for path in KNOWN_PATHS:
        values = owner_on_image(path)
        checks.append((f"debugfs reads {path}'s mode and owner off the image", values is not None))
        known += [path] + (values or ["0", "0", "0"])
    print(f"     ext2: expected from the image: {' '.join(known)}")
    passwd_size = size_on_image("/etc/passwd")
    checks.append(("debugfs reads /etc/passwd's size off the image", passwd_size is not None))
    creat = creat_lines(passwd_size or 0)
    d, out, f = boot("ext2", "espext2.img", root_steps + [
        (SUMMARY, "exit"),
        ("login:", "user"), ("assword", "user"), (r"\$ ", f"cerrno /etc/shadow {' '.join(known)}"),
        (SUMMARY, creat[0][0]),
    ] + creat_steps(creat)[:-1] + probe_steps(creat_wait(creat[-1][0])))
    got = summaries(out)
    checks += [
        ("ext2: driven to the end", d),
        (f"ext2: root, {ROOT_CHECKS} checks, 0 failed", got[:1] == [(ROOT_CHECKS, 0)]),
        ("ext2: user, /etc/shadow refused with EACCES, stat of it allowed, and the known modes and owners",
         got[1:2] == [(USER_CHECKS, 0)]),
        ("ext2: no fault lines", f == 0),
    ]
    as_user = out.split("cerrno /etc/shadow", 1)[-1]
    checks += creat_checks("ext2: user", r"\$ ", as_user, creat + PROBE_BEFORE + PROBE_AFTER)
    # No listing is a failure, not the transcript before it read as one.
    listing = as_user.split(LS, 1)[1].split("openprobe", 1)[0] if LS in as_user else ""
    checks.append((f"ext2: user: `{CHMOD}` took, hid listed drw-------",
                   re.search(HID_MODE, listing, re.M) is not None))
    for name, text in (("fat32", fat32), ("ext2", out)):
        for line in re.findall(r"^cerrno: (?:ok|FAIL).*$", text, re.M):
            print(f"     {name}: {line.rstrip()}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcripts: build/test-cerrno-fat32.txt, build/test-cerrno-ext2.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

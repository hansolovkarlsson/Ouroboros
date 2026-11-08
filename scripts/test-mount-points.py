#!/usr/bin/env python3
"""Mount points seen from their parents, and bare `mount` listing every mount.

    python3 scripts/test-mount-points.py   (or `make test-mount-points`, which builds the disk)

One boot of a copy of `build/disk.img` (the FAT32 ESP as partition 1, the ext2
root as partition 2), which has no `/mnt` and no `/dev`. Pi item 1 in
`docs/ROADMAP.md`: on the Pi on 2026-11-07, `mount 0 /mnt/boot` worked, but
`ls /mnt` said "no such file or directory" and bare `mount` showed `/` alone,
since a mount is a binding in the namespace and not a directory entry.

The CONTROL comes first: before anything is bound, `ls /mnt` must be refused,
so the image really lacks `/mnt` and what lists it afterwards is the union
(`ninep_abi::MountPoints`), not the disk. Then `mount 0 /mnt/boot`,
`mount -c /dev/cons`, a `bind` of a file (`/readme`) and a deep `bind`
(`/deep/a/b`), the root's own partition at `/mnt/root` (tree 0), a `bind`
over a file the disk lists (`/HELLO.TXT`), and a remote mount of a peer that
is not there (no NIC this boot), and the checks: bare `mount` names each with
its path, the root's partition by its format and not as `bind of /`; `ls /`
gains `mnt/`, `dev/` and `deep/` once each, `readme` as a file and
`HELLO.TXT/` in place of the disk's file, beside the disk's own entries; `ls
/mnt` is `boot/`, `root/` and `r/`, the dead peer's mount a directory by its
binding, with no stat sent to it; `ls /dev` is `cons`, a file,
since the console is one (a binding ending at a name is statted); `ls /deep/a`
is `b/`; the shell's own walk agrees (`echo /mnt/*`); `cd /mnt` works and
`pwd` says so; a path with nothing bound below it is still missing to `ls`
and `cd`; and after `unmount`, bare `mount` says nothing is mounted and still
lists the bindings that are left, and `cd` and the glob still reach the
mounts' parents with `/` unmounted. About fifty seconds. Run it whenever
`MountPoints`, `ulib::fs_list_dir`/`fs_stat`, the shell's `for_each_dir_entry`,
`cmd_cd` or `mount_info`, or `FSOP_MOUNT_INFO` changes. Since 2026-11-08 also
`write`, `mkdir`, `touch`, `writeat`, `cp` and a redirect into `/mnt`, each refused
as a directory of mount points only (`FS_ERR_MOUNTS_ONLY`), and a missing
directory inside a mount still "no such file": run it too whenever
`ninep_abi::mounts_parent`, `mounts_only` or a create path in `ulib` or the shell
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

SOURCE = os.path.join(ROOT, "build", "disk.img")
# A copy, so the boot counter this boot writes leaves build/disk.img as made.
IMAGE = os.path.join(ROOT, "build", "test-mount-points.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-mount-points.txt")
PROMPT = "# "
MOUNTS_ONLY = "its directory holds only mount points, so nothing can be made in it"

COMMANDS = [
    "ls /mnt",                      # 0: the control, before any binding
    "mount 0 /mnt/boot",            # 1
    "mount -c /dev/cons",           # 2
    "bind /readme /README.TXT",     # 3
    "bind /deep/a/b /etc",          # 4
    "mount 1 /mnt/root",            # 5: the root's own partition, tree 0
    "bind /HELLO.TXT /etc",         # 6: a binding over a file the disk lists
    "mount -r 10.0.2.99:564 /mnt/r",  # 7: a peer that is not there (no NIC)
    "mount",                        # 8
    "ls /",                         # 9
    "ls /mnt",                      # 10
    "ls /dev",                      # 11
    "ls /deep/a",                   # 12
    "echo /mnt/*",                  # 13
    "cd /mnt",                      # 14
    "pwd",                          # 15
    "cd /",                         # 16
    "ls /nope",                     # 17
    "cd /nope",                     # 18
    "ls /mnt/boot/EFI",             # 19
    # Making something in /mnt, a directory of mount points only: refused,
    # and saying so (Hans on the Pi, 2026-11-08: the old "no such file or
    # directory" read as if /mnt were missing). One per create path.
    "write /mnt/test something",    # 20: ulib's fs_write_bulk
    "mkdir /mnt/new",               # 21: fs_op_path's NP_MKDIR
    "touch /mnt/t",                 # 22: fs_op_path's NP_TOUCH
    "writeat /mnt/w 0 x",           # 23: fs_write_at
    "echo hi > /mnt/e",             # 24: the shell's own redirect
    "cp /README.TXT /mnt/c",        # 25: cp, past its overwrite guard
    "write /mnt/boot/nope/x y",     # 26: the control, inside a mount: still missing
    "writeat /nofile 0 x",          # 27: the control in a real directory with mounts below
    "unmount",                      # 28
    "mount",                        # 29
    # With / unmounted /bin is gone, so builtins only: an empty answer from
    # `cd` is success, and the control below shows it is not always empty.
    "cd /deep/a",                   # 30
    "cd /de*",                      # 31: the shell's glob expands it, to /dev
    "cd /nope",                     # 32: the control, still refused
    "cd /",                         # 33
    "mount -a",                     # 34
    "cd /etc",                      # 35
    "echo /b*",                     # 36: a glob of the root from below it
    "cd /",                         # 37
]


def answers(out):
    """Each command's output lines, in order, kernel lines dropped. A chunk is
    what follows one prompt up to the next; its first line is the echo."""
    chunks = re.split(r"\r?\n# ", out)
    got = []
    want = 0
    for chunk in chunks:
        lines = chunk.replace("\r", "").split("\n")
        if want < len(COMMANDS) and lines[0].strip() == COMMANDS[want]:
            got.append([l for l in lines[1:] if l and not l.startswith("Ouroboros kernel:")])
            want += 1
    return got


def names(lines):
    return [w for l in lines for w in l.split()]


def main() -> int:
    shutil.copyfile(SOURCE, IMAGE)
    guest = drive_qemu.Guest(IMAGE, label="test-mount-points: ")
    try:
        steps = [("login:", "root"), ("assword", "root")] + [(PROMPT, c) for c in COMMANDS] + [(PROMPT, "")]
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)
    a = answers(out)
    if len(a) < len(COMMANDS):
        print(f"FAIL every command answered (got {len(a)} of {len(COMMANDS)})")
        print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}")
        return 1
    root = names(a[9])
    mount = a[8]
    checks = [
        ("driven to the end", driven),
        ("control: before any binding, `ls /mnt` is no such file", any("no such file" in l for l in a[0])),
        ("`mount` names the root first, ending ' on /'", bool(mount) and mount[0].startswith("ext2 mounted at partition LBA ") and mount[0].endswith(" on /")),
        ("`mount` names the partition at its path", "FAT32 mounted at partition LBA 2048 on /mnt/boot" in mount),
        ("`mount` names the console at its path", "the console on /dev/cons" in mount),
        ("`mount` names each bind at its path", "bind of /README.TXT on /readme" in mount and "bind of /etc on /deep/a/b" in mount),
        ("`mount` names the root's own partition as ext2, not `bind of /`", "ext2 mounted at partition LBA 133120 on /mnt/root" in mount),
        ("`mount` names the remote mount", "remote 10.0.2.99:564/ on /mnt/r" in mount),
        ("`mount` has one line per mount", len(mount) == 8),
        ("`ls /` gains mnt/, dev/ and deep/, once each", all(root.count(n) == 1 for n in ("mnt/", "dev/", "deep/"))),
        ("`ls /` lists the bound file as a file", "readme" in root and "readme/" not in root),
        ("`ls /` lists a bound name over a disk file as the mount, once", root.count("HELLO.TXT/") == 1 and "HELLO.TXT" not in root),
        ("`ls /` still lists the disk's own entries", "bin/" in root and "README.TXT" in root and "etc/" in root),
        ("`ls /mnt` is boot/, root/ and r/: a dead peer's mount listed as a directory, unasked", sorted(names(a[10])) == ["boot/", "r/", "root/"]),
        ("`ls /dev` is cons, a file", names(a[11]) == ["cons"]),
        ("`ls /deep/a` is b/", names(a[12]) == ["b/"]),
        ("the shell's glob agrees: `echo /mnt/*`", a[13] == ["/mnt/boot /mnt/root /mnt/r"]),
        ("`cd /mnt` works", a[14] == []),
        ("`pwd` after it says /mnt", a[15] == ["/mnt"]),
        ("`ls /nope` is still no such file", any("no such file" in l for l in a[17])),
        ("`cd /nope` is still no such file", any("no such file" in l for l in a[18])),
        ("the mount itself still lists", "BOOT/" in names(a[19])),
        ("`write` in /mnt says it holds only mount points", a[20] == ["write: " + MOUNTS_ONLY]),
        ("`mkdir` in /mnt says so", a[21] == ["mkdir: " + MOUNTS_ONLY]),
        ("`touch` in /mnt says so", a[22] == ["touch: " + MOUNTS_ONLY]),
        ("`writeat` in /mnt says so", a[23] == ["writeat: " + MOUNTS_ONLY]),
        ("a redirect into /mnt says so", a[24] == ["redirect: " + MOUNTS_ONLY]),
        ("`cp` into /mnt says so", a[25] == ["cp: " + MOUNTS_ONLY]),
        ("control: a missing directory inside a mount is still no such file", a[26] == ["write: no such file or directory"]),
        ("control: a missing file in `/`, which has mounts below it, is still no such file", a[27] == ["writeat: no such file or directory"]),
        ("after `unmount`, `mount` says nothing is mounted", bool(a[29]) and a[29][0].startswith("nothing mounted")),
        ("after `unmount`, `mount` keeps the bindings left", "the console on /dev/cons" in a[29] and "bind of /etc on /deep/a/b" in a[29]),
        ("with / unmounted, `cd /deep/a` works", a[30] == []),
        ("with / unmounted, the glob finds /dev: `cd /de*` works", a[31] == []),
        ("with / unmounted, control: `cd /nope` is still refused", bool(a[32])),
        ("from /etc, `echo /b*` globs the root: /bin", a[36] == ["/bin"]),
        ("no fault lines", faults == 0),
    ]
    print(f"     mount -> {mount}")
    print(f"     ls / -> {root}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

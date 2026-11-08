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
(`/deep/a/b`), and the checks: bare `mount` names each with its path; `ls /`
gains `mnt/`, `dev/` and `deep/` once each and `readme` as a file, beside the
disk's own entries; `ls /mnt` is `boot/` alone; `ls /dev` is `cons`, a file,
since the console is one (a binding ending at a name is statted); `ls /deep/a`
is `b/`; the shell's own walk agrees (`echo /mnt/*`); `cd /mnt` works and
`pwd` says so; a path with nothing bound below it is still missing to `ls`
and `cd`; and after `unmount`, bare `mount` says nothing is mounted and still
lists the bindings that are left. About forty seconds. Run it whenever
`MountPoints`, `ulib::fs_list_dir`/`fs_stat`, the shell's `for_each_dir_entry`,
`cmd_cd` or `mount_info`, or `FSOP_MOUNT_INFO` changes.
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

COMMANDS = [
    "ls /mnt",                      # 0: the control, before any binding
    "mount 0 /mnt/boot",            # 1
    "mount -c /dev/cons",           # 2
    "bind /readme /README.TXT",     # 3
    "bind /deep/a/b /etc",          # 4
    "mount",                        # 5
    "ls /",                         # 6
    "ls /mnt",                      # 7
    "ls /dev",                      # 8
    "ls /deep/a",                   # 9
    "echo /mnt/*",                  # 10
    "cd /mnt",                      # 11
    "pwd",                          # 12
    "cd /",                         # 13
    "ls /nope",                     # 14
    "cd /nope",                     # 15
    "ls /mnt/boot/EFI",             # 16
    "unmount",                      # 17
    "mount",                        # 18
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
    root = names(a[6])
    mount = a[5]
    checks = [
        ("driven to the end", driven),
        ("control: before any binding, `ls /mnt` is no such file", any("no such file" in l for l in a[0])),
        ("`mount` names the root first, ending ' on /'", bool(mount) and mount[0].startswith("ext2 mounted at partition LBA ") and mount[0].endswith(" on /")),
        ("`mount` names the partition at its path", "FAT32 mounted at partition LBA 2048 on /mnt/boot" in mount),
        ("`mount` names the console at its path", "the console on /dev/cons" in mount),
        ("`mount` names each bind at its path", "bind of /README.TXT on /readme" in mount and "bind of /etc on /deep/a/b" in mount),
        ("`mount` has one line per mount", len(mount) == 5),
        ("`ls /` gains mnt/, dev/ and deep/, once each", all(root.count(n) == 1 for n in ("mnt/", "dev/", "deep/"))),
        ("`ls /` lists the bound file as a file", "readme" in root and "readme/" not in root),
        ("`ls /` still lists the disk's own entries", "bin/" in root and "README.TXT" in root and "etc/" in root),
        ("`ls /mnt` is boot/ alone", names(a[7]) == ["boot/"]),
        ("`ls /dev` is cons, a file", names(a[8]) == ["cons"]),
        ("`ls /deep/a` is b/", names(a[9]) == ["b/"]),
        ("the shell's glob agrees: `echo /mnt/*`", a[10] == ["/mnt/boot"]),
        ("`cd /mnt` works", a[11] == []),
        ("`pwd` after it says /mnt", a[12] == ["/mnt"]),
        ("`ls /nope` is still no such file", any("no such file" in l for l in a[14])),
        ("`cd /nope` is still no such file", any("no such file" in l for l in a[15])),
        ("the mount itself still lists", "BOOT/" in names(a[16])),
        ("after `unmount`, `mount` says nothing is mounted", bool(a[18]) and a[18][0].startswith("nothing mounted")),
        ("after `unmount`, `mount` keeps the bindings left", "the console on /dev/cons" in a[18] and "bind of /etc on /deep/a/b" in a[18]),
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

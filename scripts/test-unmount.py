#!/usr/bin/env python3
"""`unmount` with a partition mount and an open file, on the ext2 image.

    python3 scripts/test-unmount.py     (or `make test-unmount`, which builds the image)

One boot of a COPY of build/espext2.img (it ends with `erase disk`):

1. `mount 1 /mnt/f` mounts the FAT32 ESP as a second tree.
2. `exec /bin/cfidhold` holds /etc/passwd open (libc/cfidhold.c).
3. `unmount` must clear both trees: cfidhold reports its read failing.
4. `mount -a` brings `/` back: cfidhold must get a NEW descriptor, the old
   one still failing and closing cleanly (`cfidhold: ok`). Freeing the fid
   at unmount handed its number to the next opener.
5. `ls /mnt/f` must find nothing: the shell drops its own partition bindings
   at `unmount`, so the stale path cannot reach a partition a later mount
   puts in the same slot.
6. `unmount` then `erase disk` must erase: the disk tools are not refused by
   a partition tree `unmount` used to leave behind.

Run it whenever FSOP_UNMOUNT, the fid table or the shell's mount and namespace
code changes. About a minute and a half.
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

SOURCE = os.path.join(ROOT, "build", "espext2.img")
IMAGE = os.path.join(ROOT, "build", "test-unmount.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-unmount.txt")


def main() -> int:
    shutil.copyfile(SOURCE, IMAGE)
    guest = drive_qemu.Guest(IMAGE, label="test-unmount: ")
    try:
        driven = guest.run([
            ("login:", "root"), ("assword", "root"),
            ("# ", "mount 1 /mnt/f"),
            ("# ", "ls /mnt/f"),
            ("EFI", "exec /bin/cfidhold"),
            ("waiting for unmount", "unmount"),
            ("fails after unmount", "mount -a"),
            # `exec` gave the prompt back before cfidhold's last line, so no
            # new prompt follows it: type on the line itself.
            (r"cfidhold: (ok|FAIL)", "ls /mnt/f"),
            ("# ", "unmount"),
            ("unmounted", "erase disk"),
            ("# ", ""),
        ])
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(IMAGE)
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)

    after_mount = out.split("cfidhold: ok", 1)[1] if "cfidhold: ok" in out else ""
    # Typed on cfidhold's own line, so no prompt precedes it.
    ls_after = after_mount.split("ls /mnt/f", 1)[1].split("# ", 1)[0] if "ls /mnt/f" in after_mount else ""
    checks = [
        ("driven to the end", driven),
        ("the partition mounted (EFI listed)", "EFI/" in out.split("exec /bin/cfidhold", 1)[0]),
        ("cfidhold ok", "cfidhold: ok" in out and "cfidhold: FAIL" not in out),
        # Not found under `/`: the binding is gone. A binding left behind
        # answers "no filesystem mounted" (its tree is empty) or, once a
        # later mount fills the slot, lists that partition.
        ("/mnt/f gone after the remount", "no such file or directory" in ls_after),
        ("erase allowed after unmount", "erased - disk start wiped" in out),
        ("no fault lines", faults == 0),
    ]
    for line in re.findall(r"^cfidhold: .*$", out, re.M):
        print(f"     {line.rstrip()}")
    print(f"     ls /mnt/f after the remount: {ls_after.strip()!r}")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

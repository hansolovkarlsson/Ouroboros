#!/usr/bin/env python3
"""unlink, rename and fstat in the C port, on both disk formats: /bin/CRENAME
and /bin/CFSTAT.

    python3 scripts/test-crename.py     (or `make test-crename`, which builds both images)

Two boots, each of a COPY of the image:

- FAT32 (build/esp.img): `crename` (libc/crename.c) must print `crename: ok`:
  rename over an existing file gives 0 and leaves the temp's text and no temp,
  a rename of a missing file and a second remove give -1 with ENOENT. Then
  `ls /` must show no file of its: `remove` really removed them.
- ext2 (build/espext2.img), after `mount 1 /mnt/f` binds the FAT32 ESP as a
  second tree: the same, and a rename into /mnt/f must answer EXDEV.

Each boot then runs `cfstat` (libc/cfstat.c): `fstat` on /etc/passwd must leave
every field it does not fill zero, `st_dev` and `st_ino` included (Proem's
fstat note), and report a regular file: mode 100644 on ext2, which records it,
and 100000, the type alone, on FAT32.

These are the checks Proem's unlink and fstat notes and Edit's rename note
name. Run it whenever libc/src/file.c's path resolution, unlink, rename or
fstat, or fsd's NP_RM, NP_MV or NP_FSTAT changes. About two minutes.
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


def boot(name, source, steps):
    image = os.path.join(ROOT, "build", f"test-crename-{name}.img")
    shutil.copyfile(os.path.join(ROOT, "build", source), image)
    guest = drive_qemu.Guest(image, label=f"test-crename {name}: ")
    try:
        # The last step waits for cfstat's summary: the prompt after it can
        # arrive in the same read, so a further wait for a NEW prompt would
        # find nothing.
        driven = guest.run([("login:", "root"), ("assword", "root")] + steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(image)
    with open(os.path.join(ROOT, "build", f"test-crename-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def owner_from_ls(out):
    """(uid, gid) from `ls -l /etc/passwd`'s columns, or None (FAT32 prints
    `-` for both, since it records no owner)."""
    m = re.search(r"^\S+\s+(\S+)\s+(\S+)\s+\d+.*/etc/passwd\s*$", out, re.M)
    if not m or not m.group(1).isdigit():
        return None
    return int(m.group(1)), int(m.group(2))


def verdicts(name, driven, out, faults, want_exdev, want_mode):
    lines = re.findall(r"^crename: .*$", out, re.M)
    # The summary is the line `crename: ok` or `crename: FAIL` alone; every
    # check line also starts `crename: ok  ...`, so matching a substring would
    # pass a run cut short after its first check (the review of #215).
    summary = re.findall(r"^crename: (ok|FAIL)\r?$", out, re.M)
    after = out.split("crename: " + (summary[0] if summary else "x"), 1)[-1]
    # The `ls /` output only, up to the next prompt (later steps' output
    # must not count, the review of #216).
    listing = after.split("ls /", 1)[1].split("# ", 1)[0] if "ls /" in after else ""
    checks = [
        (f"{name}: driven to the end", driven),
        (f"{name}: crename ok (its summary line)", summary == ["ok"]),
        (f"{name}: no crename file left in /", listing != "" and "crename" not in listing.lower()),
        (f"{name}: no fault lines", faults == 0),
    ]
    cfstat = re.findall(r"^cfstat: (ok|FAIL)\r?$", out, re.M)
    checks.append((f"{name}: cfstat ok (its summary line)", cfstat == ["ok"]))
    got = re.search(r"^cfstat: fstat 0, size \d+, mode (\d+), uid (\d+), gid (\d+),", out, re.M)
    checks.append((f"{name}: fstat mode {want_mode}", got is not None and got.group(1) == want_mode))
    # uid and gid against `ls -l`, which reads them through a different path
    # (NP_STAT in the shell's ls, not libc), so the check can fail: before
    # the review of #216 they were compared with themselves.
    owner = owner_from_ls(out)
    want_owner = owner if owner is not None else (0, 0)
    checks.append((f"{name}: fstat uid/gid {want_owner} as ls -l shows",
                   got is not None and (int(got.group(2)), int(got.group(3))) == want_owner))
    for line in re.findall(r"^cfstat: .*$", out, re.M):
        print(f"     {name}: {line.rstrip()}")
    if want_exdev:
        checks.append((f"{name}: a rename into /mnt/f answers EXDEV", "(EXDEV)" in out))
    for line in lines:
        print(f"     {name}: {line.rstrip()}")
    return checks


def main() -> int:
    checks = []
    d, out, f = boot("fat32", "esp.img", [("# ", "mkdir /crename.d"), ("# ", "mkdir /crename.d/sub"), ("# ", "crename"), (r"crename: (ok|FAIL)\r?\n", "ls /"), ("# ", "ls -l /etc/passwd"), ("# ", "cfstat"), (r"cfstat: (ok|FAIL)\r?\n", "")])
    checks += verdicts("fat32", d, out, f, want_exdev=False, want_mode="100666")
    d, out, f = boot("ext2", "espext2.img", [("# ", "mount 1 /mnt/f"), ("# ", "mkdir /crename.d"), ("# ", "mkdir /crename.d/sub"), ("# ", "crename"), (r"crename: (ok|FAIL)\r?\n", "ls /"), ("# ", "ls -l /etc/passwd"), ("# ", "cfstat"), (r"cfstat: (ok|FAIL)\r?\n", "")])
    checks += verdicts("ext2", d, out, f, want_exdev=True, want_mode="100644")
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcripts: build/test-crename-fat32.txt, build/test-crename-ext2.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

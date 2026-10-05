#!/usr/bin/env python3
"""unlink and rename in the C port, on both disk formats: /bin/CRENAME.

    python3 scripts/test-crename.py     (or `make test-crename`, which builds both images)

Two boots, each of a COPY of the image:

- FAT32 (build/esp.img): `crename` (libc/crename.c) must print `crename: ok`:
  rename over an existing file gives 0 and leaves the temp's text and no temp,
  a rename of a missing file and a second remove give -1 with ENOENT. Then
  `ls /` must show no file of its: `remove` really removed them.
- ext2 (build/espext2.img), after `mount 1 /mnt/f` binds the FAT32 ESP as a
  second tree: the same, and a rename into /mnt/f must answer EXDEV.

These are the checks Proem's unlink note and Edit's rename note name. Run it
whenever libc/src/file.c's path resolution, unlink or rename, or fsd's NP_RM or
NP_MV changes. About two minutes.
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
        driven = guest.run([("login:", "root"), ("assword", "root")] + steps + [("# ", "")])
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
        os.remove(image)
    with open(os.path.join(ROOT, "build", f"test-crename-{name}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def verdicts(name, driven, out, faults, want_exdev):
    lines = re.findall(r"^crename: .*$", out, re.M)
    # The summary is the line `crename: ok` or `crename: FAIL` alone; every
    # check line also starts `crename: ok  ...`, so matching a substring would
    # pass a run cut short after its first check (the review of #215).
    summary = re.findall(r"^crename: (ok|FAIL)\r?$", out, re.M)
    listing = out.split("ls /", 1)[1] if "ls /" in out.split("crename: " + (summary[0] if summary else "x"), 1)[-1] else ""
    checks = [
        (f"{name}: driven to the end", driven),
        (f"{name}: crename ok (its summary line)", summary == ["ok"]),
        (f"{name}: no crename file left in /", listing != "" and "crename" not in listing.lower()),
        (f"{name}: no fault lines", faults == 0),
    ]
    if want_exdev:
        checks.append((f"{name}: a rename into /mnt/f answers EXDEV", "(EXDEV)" in out))
    for line in lines:
        print(f"     {name}: {line.rstrip()}")
    return checks


def main() -> int:
    checks = []
    d, out, f = boot("fat32", "esp.img", [("# ", "mkdir /crename.d"), ("# ", "mkdir /crename.d/sub"), ("# ", "crename"), (r"crename: (ok|FAIL)\r?\n", "ls /")])
    checks += verdicts("fat32", d, out, f, want_exdev=False)
    d, out, f = boot("ext2", "espext2.img", [("# ", "mount 1 /mnt/f"), ("# ", "mkdir /crename.d"), ("# ", "mkdir /crename.d/sub"), ("# ", "crename"), (r"crename: (ok|FAIL)\r?\n", "ls /")])
    checks += verdicts("ext2", d, out, f, want_exdev=True)
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcripts: build/test-crename-fat32.txt, build/test-crename-ext2.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

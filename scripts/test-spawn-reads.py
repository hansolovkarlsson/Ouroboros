#!/usr/bin/env python3
"""Spawning a program resolves its path once a read request, exactly.

    python3 scripts/test-spawn-reads.py    (or `make test-spawn-reads`, which builds the images)

The shell spawns a program by reading its file through fsd and staging it in
the kernel. Until 2026-10-17 it read 512 bytes a request (`NP_READ_AT`), and
fsd resolved the path from the root for each one: one run of `ls` read 1,201
sectors on FAT32 (1,032 on ext2, 1,942 on exFAT), against 199, 187 and 290
since. Now the shell reads `SPAWN_READ` (8 KiB) a request through `NP_READ`,
and fsd resolves the path once a request and copies the file to the shell's
grant a `SAFECOPY_MAX` (2 KiB) piece at a time (`vfs::read_pieces`). No
program's output can show either half; only the disk can, so this rig counts
QEMU's virtio-blk reads (scripts/disktrace.py).

On a copy of each image (FAT32 build/esp.img, ext2 build/espext2.img, exFAT
build/espexfat.img) the host puts two copies of /bin/ARGS in the root:
/SMALL as it is, and /BIG zero-padded to BIG bytes (the loader reads only
what the program headers name, so both run the same). Two boots, each
logging in and spawning `/SMALL w` once to warm fsd's caches, then RUNS
spawns of `/SMALL a` in one boot and of `/BIG a` in the other; each must
print `argv[1] = a`.

The check is exact. Every resolution of `/SMALL` or `/BIG` reads the root
directory's first sector once, and everything else a run does is the same
in both boots, so per run that sector's count differs between the boots by
the difference in requests: ceil(size / SPAWN_READ) + 1 each (the last read
of 0 ends the loop). It must equal that. Resolving once a 2 KiB piece (fsd's
half undone) makes it about four times as much, and once a 512 bytes (the
shell's half undone, as on main before 2026-10-17) about sixteen times.
The root directory's first sector is found on the host: from the FAT32 or
exFAT boot sector, and on ext2 by debugfs. Reported too: the sectors one
spawn of each reads.

About three minutes; needs e2fsprogs, as the ext2 image does.
"""
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import bootstick  # noqa: E402  (its e2fsprogs lookup, `tool`)
import disktrace  # noqa: E402
import mbrdisk  # noqa: E402  (its MBR reader, `entries`)

SECTOR = 512
RUNS = 2
SPAWN_READ = 8 * 1024  # the shell's SPAWN_READ
BIG = 200 * 1024
IMAGES = [("fat32", "esp.img", 0x0B), ("ext2", "espext2.img", 0x83), ("exfat", "espexfat.img", 0x07)]


def requests(size):
    """The NP_READ requests spawn_path makes for a file of `size` bytes."""
    return math.ceil(size / SPAWN_READ) + 1


def partition(image, ptype):
    """(start LBA, sectors) of the one partition of MBR type `ptype`."""
    with open(image, "rb") as fh:
        parts = mbrdisk.entries(fh.read(SECTOR)) or []
    found = [p for p in parts if p[1] == ptype]
    if len(found) != 1:
        raise SystemExit(f"{image}: not one partition of type {ptype:#x}: {parts}")
    return found[0][2], found[0][3]


def place_mounted(image, files):
    """Write `files` ({name: bytes}) to the root of partition 1 of `image`
    (FAT32 or exFAT), attached without mounting and mounted alone, since
    the exFAT image has the ESP beside it."""
    out = subprocess.run(["hdiutil", "attach", "-nobrowse", "-nomount", image],
                         check=True, capture_output=True, text=True).stdout
    disk = out.split()[0]
    mp = tempfile.mkdtemp(prefix="test-spawn-reads.")
    try:
        subprocess.run(["diskutil", "mount", "-mountPoint", mp, f"{disk}s1"], check=True, capture_output=True)
        for name, data in files.items():
            with open(os.path.join(mp, name), "wb") as fh:
                fh.write(data)
        for f in os.listdir(mp):
            if f.startswith("._"):
                os.remove(os.path.join(mp, f))
        subprocess.run(["diskutil", "unmount", mp], check=True, capture_output=True)
    finally:
        subprocess.run(["hdiutil", "detach", disk], capture_output=True)
        os.rmdir(mp)


def place_ext2(image, files):
    """Write `files` to the root of `image`'s ext2 partition with debugfs,
    whose complaints are read (it exits 0 whatever a command did), and
    return the root directory's first block's LBA."""
    start, count = partition(image, 0x83)
    try:
        debugfs, e2fsck = bootstick.tool("debugfs"), bootstick.tool("e2fsck")
    except bootstick.Refused as e:
        raise SystemExit(str(e))
    with tempfile.TemporaryDirectory() as tmp:
        part, cmds = os.path.join(tmp, "part.img"), os.path.join(tmp, "cmds")
        with open(image, "rb") as src, open(part, "wb") as dst:
            src.seek(start * SECTOR)
            dst.write(src.read(count * SECTOR))
        with open(cmds, "w") as fh:
            for name, data in files.items():
                host = os.path.join(tmp, name)
                with open(host, "wb") as f:
                    f.write(data)
                fh.write(f"write {host} {name}\n")
        out = subprocess.run([debugfs, "-w", "-f", cmds, part], capture_output=True, text=True)
        errors = [ln for ln in out.stderr.splitlines() if ln.strip() and not ln.startswith("debugfs ")]
        if out.returncode != 0 or errors:
            raise SystemExit("writing the ext2 root failed: " + "; ".join(errors[:5]))
        if subprocess.run([e2fsck, "-fn", part], capture_output=True).returncode != 0:
            raise SystemExit("the ext2 root does not check clean after the writes (e2fsck -fn)")
        blocks = subprocess.run([debugfs, "-R", "blocks /", part], capture_output=True, text=True).stdout.split()
        bsize = re.search(r"Block size:\s+(\d+)", subprocess.run(
            [debugfs, "-R", "stats", part], capture_output=True, text=True).stdout)
        with open(part, "rb") as src, open(image, "r+b") as dst:
            dst.seek(start * SECTOR)
            dst.write(src.read())
    return start + int(blocks[0]) * int(bsize.group(1)) // SECTOR


def root_fat32(image):
    start, _ = partition(image, 0x0B)
    with open(image, "rb") as fh:
        fh.seek(start * SECTOR)
        b = fh.read(SECTOR)
    per_clus, reserved, nfats = b[13], struct.unpack_from("<H", b, 14)[0], b[16]
    fatsz, root = struct.unpack_from("<I", b, 36)[0], struct.unpack_from("<I", b, 44)[0]
    return start + reserved + nfats * fatsz + (root - 2) * per_clus


def root_exfat(image):
    start, _ = partition(image, 0x07)
    with open(image, "rb") as fh:
        fh.seek(start * SECTOR)
        b = fh.read(SECTOR)
    heap, root = struct.unpack_from("<I", b, 88)[0], struct.unpack_from("<I", b, 96)[0]
    return start + heap + ((root - 2) << b[109])


def boot(name, prepared, image, program):
    steps = [("login:", "root"), ("assword", "root"), ("# ", "/SMALL w")]
    steps += [("# ", f"{program} a")] * RUNS + [("# ", "")]
    problems, lines, count = disktrace.boot(f"test-spawn-reads-{name}", prepared, image, steps)
    said = sum(1 for ln in lines if ln.strip() == "argv[1] = a")
    if said < RUNS:
        problems.append(f"`{program} a` printed argv[1] = a {said} times, not {RUNS}")
    return problems, count


def main() -> int:
    with open(os.path.join(ROOT, "build", "esp", "bin", "ARGS"), "rb") as fh:
        small = fh.read()
    files = {"SMALL": small, "BIG": small + bytes(BIG - len(small))}
    want = requests(BIG) - requests(len(small))
    failed = 0
    for name, image, ptype in IMAGES:
        prepared = os.path.join(ROOT, "build", f"test-spawn-reads-{name}-prepared.img")
        shutil.copyfile(os.path.join(ROOT, "build", image), prepared)
        try:
            if ptype == 0x83:
                root = place_ext2(prepared, files)
            else:
                place_mounted(prepared, files)
                root = root_fat32(prepared) if ptype == 0x0B else root_exfat(prepared)
            p0, base = boot(name, prepared, image, "/SMALL")
            p1, more = boot(name, prepared, image, "/BIG")
        finally:
            os.remove(prepared)
        problems = p0 + p1
        delta = (more[root] - base[root]) / RUNS
        if not problems and delta != want:
            problems.append(f"the root directory's first sector (LBA {root}) is read {delta:g} more "
                            f"times a spawn of /BIG than of /SMALL, not {want}: one resolution "
                            "a request is the rule")
        verdict = "FAIL" if problems else "ok  "
        print(f"{verdict} {name}: /BIG ({BIG} bytes) resolves {delta:g} more times than /SMALL "
              f"({len(small)}), {want} more requests; the root sector is LBA {root}")
        for p in problems:
            print(f"     {p}")
        failed += bool(problems)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

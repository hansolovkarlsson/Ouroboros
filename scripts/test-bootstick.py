#!/usr/bin/env python3
"""`make bootstick` against a disk image file: one-disk step 5's rig.

    python3 scripts/test-bootstick.py   (or `make test-bootstick`, which builds the images)

scripts/bootstick.py writes the one-disk layout to a whole disk and carries
the old disk's /etc, /Users, boot counter and signature over. A file target
is attached with hdiutil and written through the same /dev/diskN path a real
stick is, so this exercises the guard, the write and the read-back as a
stick would see them; only `sudo dd` (a device the user cannot open) is not
reached.

On the host, before any boot, every refusal must leave its disk's bytes as
they were: a FAT volume holding someone's file, a Pi card, a one-disk layout
whose ext2 root lacks /etc/srcid, a disk too small, a partition's node. Then
a fresh file is written, and must hold build/esp.img's ESP and
build/ext2part.img's root as they are, and a signature of its own.

Three boots of that file on virtio:
  first     log in as root, note `bootid`'s counter, `useradd ann` and put a
            file in her home, power off
  restaged  after bootstick writes the file again: `ann` logs in with her
            password, reads her file, and the counter is above the first
            boot's, so neither the accounts nor the counter went back
  reset     after `CARRY=0 bootstick`: `ann` is refused, the counter is above
            the restaged boot's still. This boot is the control: the first
            two pass on a carry that works and this one shows that a disk
            written without it is told apart.

A mutation of bootstick.py needs OUROBOROS_STALE_OK=1: the writer is in the
tree's source hash (scripts/srcid.py) but not in the image, so the guard
reads a mutated writer as a stale image. The image booted is the one built
before the mutation.

About two and a half minutes. Run it whenever scripts/bootstick.py,
mbrdisk.py, disksig.py or the root's recipe changes.
"""
import hashlib
import importlib.util
import os
import re
import shutil
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
sys.path.insert(0, HERE)
import bootstick  # noqa: E402
import disksig  # noqa: E402
import mbrdisk  # noqa: E402

BUILD = os.path.join(ROOT, "build")
WORK = os.path.join(BUILD, "test-bootstick")
STICK = os.path.join(WORK, "stick.img")
TRANSCRIPT = os.path.join(BUILD, "test-bootstick-{}.txt")
NAME = "test-bootstick: "
PASSWORD = "annpass1"
HOME_TEXT = "carried-home"


def bootstick_run(target, carry=True):
    env = dict(os.environ, CARRY="1" if carry else "0")
    out = subprocess.run([sys.executable, os.path.join(HERE, "bootstick.py"), target],
                         capture_output=True, text=True, env=env)
    return out.returncode, out.stdout + out.stderr


def digest(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def fat_volume(name, files):
    """A one-partition MBR FAT32 disk image holding `files` (name -> text)."""
    src = os.path.join(WORK, name + "-src")
    shutil.rmtree(src, ignore_errors=True)
    os.makedirs(src)
    for rel, text in files.items():
        os.makedirs(os.path.dirname(os.path.join(src, rel)), exist_ok=True)
        with open(os.path.join(src, rel), "w") as fh:
            fh.write(text)
    cdr = os.path.join(WORK, name + ".cdr")
    subprocess.run(["hdiutil", "create", "-size", "128m", "-fs", "FAT32", "-layout", "MBRSPUD", "-volname", "STICK",
                    "-srcfolder", src, "-format", "UDTO", "-ov", cdr], capture_output=True, check=True)
    path = os.path.join(WORK, name + ".img")
    os.replace(cdr, path)
    shutil.rmtree(src)
    return path


def blank(name, size):
    path = os.path.join(WORK, name + ".img")
    with open(path, "wb") as fh:
        fh.truncate(size)
    return path


def refusals():
    """Each target bootstick must refuse, and the bytes it must not touch."""
    checks = []
    foreign_root = os.path.join(WORK, "foreign-root.img")
    shutil.copyfile(os.path.join(BUILD, "disk.img"), foreign_root)
    root_start = struct.unpack_from("<I", open(foreign_root, "rb").read(512), 446 + 16 + 8)[0]
    debugfs = bootstick.tool("debugfs")
    subprocess.run([debugfs, "-w", "-R", "rm /etc/srcid", f"{foreign_root}?offset={root_start * 512}"],
                   capture_output=True, check=True)
    cases = [
        ("a FAT volume holding someone's file", fat_volume("foreign", {"IMG_0001.JPG": "photo"}),
         "neither empty nor ours"),
        ("a Pi card", fat_volume("card", {"RPI_EFI.fd": "fw"}), "a Pi card"),
        ("an ext2 partition 2 without /etc/srcid", foreign_root, "not a disk this wrote"),
        ("a disk too small", blank("small", 32 << 20), "the image needs"),
    ]
    for what, path, reason in cases:
        before = digest(path)
        code, out = bootstick_run(path)
        checks.append((f"refused, untouched: {what}", code == 1 and reason in out and digest(path) == before))
        if not checks[-1][1]:
            print(out)
    code, out = bootstick_run("/dev/disk0s1")
    checks.append(("refused: a partition's node", code == 1 and "not a whole disk" in out))
    return checks


def boot(label, steps):
    guest = drive_qemu.Guest(STICK, label=NAME + f"[{label}] ")
    try:
        reached = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT.format(label), "w") as fh:
        fh.write(out)
    return reached, out, faults


def counter(out):
    found = re.findall(r"^boot (\d+)\r?$", out, re.M)
    return int(found[-1]) if found else None


def root_bytes(path):
    with open(path, "rb") as fh:
        sector = fh.read(512)
    _, _, start, sectors = bootstick.parse_mbr(sector)[1]
    with open(path, "rb") as fh:
        fh.seek(start * 512)
        return fh.read(sectors * 512)


def main():
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    checks = refusals()

    blank("stick", 128 << 20)
    code, out = bootstick_run(STICK)
    print(out, end="")
    with open(STICK, "rb") as fh:
        sector = fh.read(512)
    parts = bootstick.parse_mbr(sector) or []
    esp = mbrdisk.esp_payload(os.path.join(BUILD, "esp.img"))
    with open(STICK, "rb") as fh:
        fh.seek(parts[0][2] * 512 if parts else 0)
        written_esp = fh.read(len(esp))
    sig = disksig.read_signature(STICK) if code == 0 else 0
    checks += [
        ("a fresh file is written", code == 0),
        ("partition 1 is the ESP, partition 2 the ext2 root", bootstick.is_one_disk(parts) is not None),
        ("the ESP is esp.img's as built", written_esp == esp),
        ("the root is ext2part.img's as built", parts and root_bytes(STICK)[:os.path.getsize(os.path.join(BUILD, "ext2part.img"))]
         == open(os.path.join(BUILD, "ext2part.img"), "rb").read()),
        ("the disk has a signature, not disk.img's", sig != 0 and sig != disksig.read_signature(os.path.join(BUILD, "disk.img"))),
    ]

    first_ok, first, first_faults = boot("first", [
        ("login:", "root"), ("assword", "root"), ("# ", "bootid"), ("# ", "useradd ann"),
        ("assword", PASSWORD), ("assword", PASSWORD), ("# ", f"echo {HOME_TEXT} > /Users/ann/n.txt"),
        ("# ", "cat /Users/ann/n.txt"), ("# ", "shutdown"),
    ])
    c1 = counter(first)
    checks += [
        ("first: driven to the end", first_ok),
        ("first: fsd mounted the ext2 root", "ext2 mounted" in first),
        ("first: the home file was written", re.search(rf"^{HOME_TEXT}\r?$", first, re.M) is not None),
        ("first: a boot counter", c1 is not None),
        ("first: no fault lines", first_faults == 0),
    ]

    code, out = bootstick_run(STICK)
    print(out, end="")
    checks += [
        ("re-staged: written, the old disk taken as ours", code == 0 and "ours (one-disk)" in out),
        ("re-staged: the signature kept", disksig.read_signature(STICK) == sig),
    ]
    login_ann = [("login:", "ann"), ("assword", PASSWORD), (r"\$ |Login incorrect", "")]
    second_ok, second, second_faults = boot("restaged", login_ann + [
        ("", "cat /Users/ann/n.txt"), (r"\$ ", "bootid"), (r"\$ ", "logout"), ("login:", "root"),
        ("assword", "root"), ("# ", "shutdown"),
    ])
    c2 = counter(second)
    checks += [
        ("restaged: driven to the end", second_ok),
        ("restaged: ann logs in with her password", "Login incorrect" not in second and "$ " in second),
        ("restaged: her file is there", re.search(rf"^{HOME_TEXT}\r?$", second, re.M) is not None),
        ("restaged: the counter went on from the first boot's", c1 is not None and c2 is not None and c2 > c1),
        ("restaged: no fault lines", second_faults == 0),
    ]

    code, out = bootstick_run(STICK, carry=False)
    print(out, end="")
    checks.append(("reset: CARRY=0 written", code == 0 and "not carried" in out))
    # The refusal and the next prompt in one wait: they arrive together, and a
    # wait for "login:" after one for the refusal alone finds nothing new.
    third_ok, third, third_faults = boot("reset", [
        ("login:", "ann"), ("assword", PASSWORD), (r"Login incorrect[\s\S]*login:", "root"),
        ("assword", "root"), ("# ", "bootid"), ("# ", "shutdown"),
    ])
    c3 = counter(third)
    checks += [
        ("reset: driven to the end", third_ok),
        ("reset: ann is refused (the control: a disk written without the carry is told apart)",
         "Login incorrect" in third),
        ("reset: the counter still went on", c2 is not None and c3 is not None and c3 > c2),
        ("reset: no fault lines", third_faults == 0),
    ]

    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {NAME}{name}")
        failed += 0 if ok else 1
    print(f"{NAME}{len(checks) - failed} of {len(checks)} checks passed; transcripts in "
          f"{TRANSCRIPT.format('*')}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

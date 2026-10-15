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
whose ext2 root lacks /etc/srcid, a disk too small, a partition's node.
Then the Pi's stick (step 5b), on the host but for one boot: the pinned
firmware on a fresh one, its settings and config.txt carried by a re-stage,
FIRMWARE=1 resetting the settings and keeping config.txt, PI=0 dropping it. Then
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


def bootstick_run(target, carry=True, **extra):
    env = dict(os.environ, CARRY="1" if carry else "0", **extra)
    for name in ("PI", "FIRMWARE"):
        if name not in extra:
            env.pop(name, None)
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


def ext2(path, request, write=False):
    """debugfs's stdout for one request on the root of the one-disk image at `path`."""
    start = bootstick.parse_mbr(open(path, "rb").read(512))[1][2]
    args = [bootstick.tool("debugfs")] + (["-w"] if write else []) + ["-R", request, f"{path}?offset={start * 512}"]
    return subprocess.run(args, capture_output=True, text=True).stdout


def counter_record(path):
    """The boot counter record in partition 1 of the image at `path`."""
    start = bootstick.parse_mbr(open(path, "rb").read(512))[0][2] * 512
    with open(path, "rb") as fh:
        def read(o, n):
            fh.seek(start + o)
            return fh.read(n)
        return bootstick.esp_counter(read)


def sources():
    """The other disks bootstick takes as ours, and what each must carry
    (the review of #293): a `make stick` FAT volume's homes owned by the
    passwd entry whose home they are, in this project's passwd format; a
    written esp.img's boot counter; and a dd copy of disk.img, whose
    signature every such copy shares, given one of its own."""
    checks = []
    passwd = open(os.path.join(BUILD, "esp", "etc", "passwd")).read() + "ann:1002:1002:/Users/ann\n"
    fat = fat_volume("make-stick", {".ouroboros-stick": "", "etc/passwd": passwd, "Users/ann/n.txt": "hi\n"})
    code, out = bootstick_run(fat)
    listing = ext2(fat, "ls -p /Users/ann") if code == 0 else ""
    checks.append(("a `make stick` stick: written, ann's home and file owned by 1002",
                   code == 0 and "/040755/1002/1002/./" in listing and "/100644/1002/1002/n.txt/" in listing))
    esp_stick = fat_volume("esp-stick", {"EFI/ORBS/INIT.CFG": "\\EFI\\ORBS\\SH.BIN\n",
                                         "EFI/ORBS/BOOTID.TXT": "00000000000000000042\n"})
    code, out = bootstick_run(esp_stick)
    checks.append(("a written esp.img: its boot counter carried",
                   code == 0 and counter_record(esp_stick) == b"00000000000000000042\n"))
    copy = os.path.join(WORK, "dd-copy.img")
    shutil.copyfile(os.path.join(BUILD, "disk.img"), copy)
    with open(copy, "ab") as fh:
        fh.truncate(128 << 20)
    code, out = bootstick_run(copy)
    image_sig = disksig.read_signature(os.path.join(BUILD, "disk.img"))
    checks.append(("a dd copy of disk.img: taken as ours, given a signature of its own",
                   code == 0 and "ours (one-disk)" in out and disksig.read_signature(copy) not in (0, image_sig)))
    return checks


def refusals():
    """Each target bootstick must refuse, and the bytes it must not touch."""
    checks = []
    foreign_root = os.path.join(WORK, "foreign-root.img")
    shutil.copyfile(os.path.join(BUILD, "disk.img"), foreign_root)
    root_start = bootstick.parse_mbr(open(foreign_root, "rb").read(512))[1][2]
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
        # No kept image either: a refusal before the write leaves the disk as
        # it was, so an image left would read as a failed write's next run.
        checks.append((f"refused, untouched, no image kept: {what}", code == 1 and reason in out
                       and digest(path) == before and not os.path.exists(bootstick.KEPT_IMG)))
        if not checks[-1][1]:
            print(out)
    code, out = bootstick_run("/dev/disk0s1")
    checks.append(("refused: a partition's node", code == 1 and "not a whole disk" in out))
    # A kept image left by a write that did not finish is the only copy of
    # that disk's /etc and /Users: the next run must not rebuild it in place
    # (the second review of #293). main() stops before this if a real one is
    # there, so the one made here is the rig's own.
    with open(bootstick.KEPT_IMG, "wb") as fh:
        fh.write(b"the only copy")
    target = blank("after-failure", 128 << 20)
    before = digest(target)
    code, out = bootstick_run(target)
    # Gone is a failure to grade, not a crash: a run that went ahead deletes
    # the image when its write succeeds.
    kept = open(bootstick.KEPT_IMG, "rb").read() if os.path.exists(bootstick.KEPT_IMG) else None
    if kept is not None:
        os.remove(bootstick.KEPT_IMG)
    checks.append(("refused, untouched, the kept image too: a kept image left from a failed write",
                   code == 1 and "did not finish" in out and digest(target) == before and kept == b"the only copy"))
    if not checks[-1][1]:
        print(out)
    return checks


def boot(label, steps, image=STICK):
    guest = drive_qemu.Guest(image, label=NAME + f"[{label}] ")
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


def srcid_of_build():
    return subprocess.run([bootstick.tool("debugfs"), "-R", "cat /etc/srcid", os.path.join(BUILD, "ext2part.img")],
                          capture_output=True, text=True).stdout


class Esp:
    """Partition 1 of a written image, mounted for the length of a `with`."""

    def __init__(self, path, write=False):
        self.path, self.write = path, write

    count = 0

    def __enter__(self):
        # A mountpoint of its own each time, so a volume that would not
        # detach is never mounted over (the review of #294).
        Esp.count += 1
        self.mount = os.path.join(WORK, f"esp-mount-{Esp.count}")
        os.makedirs(self.mount)
        subprocess.run(["hdiutil", "attach", "-nobrowse", *([] if self.write else ["-readonly"]),
                        "-mountpoint", self.mount, self.path], capture_output=True, check=True)
        return self

    def read(self, name):
        path = os.path.join(self.mount, name)
        return open(path, "rb").read() if os.path.isfile(path) else None

    def append(self, name, data):
        with open(os.path.join(self.mount, name), "r+b") as fh:
            fh.seek(-len(data), os.SEEK_END)
            fh.write(data)

    def __exit__(self, *exc):
        # Checked: bootstick then writes the image's raw node, which must not
        # happen under a FAT mounted read-write (bootstick.detach's rule).
        bootstick.detach(self.mount)


def pi_stick():
    """Step 5b, the Pi's stick: the firmware at the root of partition 1 on a
    fresh disk, carried whole on a re-stage, reinstalled by FIRMWARE=1 with
    config.txt kept, dropped by PI=0. QEMU cannot run the Pi's firmware, so
    what the Pi's boot ROM would read is checked on the host, and one boot
    shows the ESP and root still boot with the firmware beside them."""
    checks = []
    zip_path, _, release_names = bootstick.pftf_zip()
    # Every file the release ships, the readme aside: the Pi's boot ROM needs
    # files no QEMU boot can show missing (the review of #294).
    members = [n for n in subprocess.run(["unzip", "-Z1", zip_path], capture_output=True, text=True).stdout.split()
               if not n.endswith("/") and n != "Readme.md"]
    shipped = {n: subprocess.run(["unzip", "-p", zip_path, n], capture_output=True).stdout for n in members}
    pi = blank("pi", 128 << 20)
    code, out = bootstick_run(pi, PI="1")
    with Esp(pi) as esp:
        names = os.listdir(esp.mount)
        checks.append((f"pi: a fresh stick gets the pinned firmware, all {len(shipped)} files, byte for byte",
                       code == 0 and all(esp.read(n) == data for n, data in shipped.items())))
        manifest = bootstick.read_manifest(esp.mount)
        checks.append(("pi: its manifest names exactly the release's files", manifest == set(release_names)))
        checks.append(("pi: beside the ESP's tree, without the readme or macOS's sidecars",
                       esp.read("EFI/BOOT/BOOTAA64.EFI") is not None and "Readme.md" not in names
                       and not any(n.startswith("._") for n in names)))
    ok, out_boot, faults = boot("pi", [("login:", "root"), ("assword", "root"), ("# ", "shutdown")], image=pi)
    checks.append(("pi: it boots on QEMU to a root shell, the ext2 root mounted, no faults",
                   ok and "ext2 mounted" in out_boot and faults == 0))
    settings, overlay = b"SETTINGS", b"# set by hand\n"
    with Esp(pi, write=True) as esp:
        esp.append("RPI_EFI.fd", settings)
        with open(os.path.join(esp.mount, "config.txt"), "ab") as fh:
            fh.write(overlay)
        # A boot flag set to bisect a hang, and a directory left by hand: not
        # the firmware's, so neither may outlive the re-stage.
        open(os.path.join(esp.mount, "NOXHCI"), "w").close()
        os.makedirs(os.path.join(esp.mount, "OLDSTUFF"))
        # A file only the stick's own release shipped, named by its manifest
        # and by no current release: the re-stage must carry it, by the
        # manifest, with no fetch (Hans, 2026-10-14).
        open(os.path.join(esp.mount, "OLDPIN.BIN"), "w").close()
        with open(os.path.join(esp.mount, *bootstick.MANIFEST), "a") as fh:
            fh.write("OLDPIN.BIN\n")
    code, out = bootstick_run(pi)
    with Esp(pi) as esp:
        fd, config = esp.read("RPI_EFI.fd"), esp.read("config.txt")
        names = os.listdir(esp.mount)
        manifest = bootstick.read_manifest(esp.mount) or set()
    checks.append(("pi: a re-stage carries a file only its manifest names, by the manifest, and lists it again",
                   code == 0 and "OLDPIN.BIN" in names and "by its manifest" in out and "OLDPIN.BIN" in manifest))
    checks.append(("pi: a re-stage keeps it a Pi stick and carries RPI_EFI.fd and config.txt",
                   code == 0 and "a Pi stick" in out and fd and fd.endswith(settings)
                   and config and config.endswith(overlay)))
    checks.append(("pi: a boot flag and a stray directory at the root are not carried, and are named",
                   code == 0 and "NOXHCI" not in names and "OLDSTUFF" not in names
                   and "not carried from its root: NOXHCI, OLDSTUFF" in out))
    # A stick from before the manifest: the pinned release's names, and a
    # manifest written for the next time.
    with Esp(pi, write=True) as esp:
        listed = os.path.join(esp.mount, *bootstick.MANIFEST)
        if os.path.exists(listed):  # absent is for the checks to grade, not a crash
            os.remove(listed)
    code, out = bootstick_run(pi)
    with Esp(pi) as esp:
        fd = esp.read("RPI_EFI.fd")
        manifest = bootstick.read_manifest(esp.mount)
    checks.append(("pi: with no manifest, the release's names carry it, and a manifest is written",
                   code == 0 and "no manifest" in out and fd and fd.endswith(settings)
                   and manifest is not None and "RPI_EFI.fd" in manifest))
    code, out = bootstick_run(pi, FIRMWARE="1")
    with Esp(pi) as esp:
        fd, config = esp.read("RPI_EFI.fd"), esp.read("config.txt")
    checks.append(("pi: FIRMWARE=1 puts the pinned RPI_EFI.fd back and keeps config.txt",
                   code == 0 and fd == shipped["RPI_EFI.fd"] and config and config.endswith(overlay)))
    code, out = bootstick_run(pi, PI="0")
    with Esp(pi) as esp:
        gone = esp.read("RPI_EFI.fd") is None and esp.read("start4.elf") is None
    checks.append(("pi: PI=0 drops the firmware, and says so", code == 0 and gone and "dropped" in out))
    before = digest(pi)
    code, out = bootstick_run(pi, FIRMWARE="1")
    checks.append(("pi: FIRMWARE=1 on a stick that is not a Pi's is refused, untouched",
                   code == 1 and "not a Pi stick" in out and digest(pi) == before))
    code, out = bootstick_run(pi, PI="true")
    checks.append(("an option that is not 1 or 0 (PI=true) is refused, untouched",
                   code == 1 and "give PI=1 or PI=0" in out and digest(pi) == before))
    # An ESP that will not mount: its firmware cannot be told, so the default
    # refuses, and PI=0 re-stages it, which is how a damaged ESP is repaired.
    esp_start = bootstick.parse_mbr(open(pi, "rb").read(512))[0][2]
    with open(pi, "r+b") as fh:
        fh.seek(esp_start * 512)
        fh.write(bytes(512))
    before = digest(pi)
    code, out = bootstick_run(pi)
    checks.append(("an ESP that will not mount is refused by default, untouched",
                   code == 1 and "does not mount" in out and digest(pi) == before))
    code, out = bootstick_run(pi, PI="0")
    try:
        with Esp(pi) as esp:
            repaired = esp.read("EFI/BOOT/BOOTAA64.EFI") is not None
    except subprocess.CalledProcessError:
        repaired = False  # still unmountable: graded here, not a crash
    checks.append(("PI=0 re-stages a disk whose ESP will not mount, and the ESP mounts after", code == 0 and repaired))
    return checks


def main():
    if os.path.exists(bootstick.KEPT_IMG):
        sys.exit(f"{NAME}{bootstick.KEPT_IMG} exists: a real write left it, the only copy of a disk's /etc and "
                 f"/Users; move it away before running this rig, which makes and deletes its own")
    shutil.rmtree(WORK, ignore_errors=True)
    os.makedirs(WORK)
    checks = refusals()
    checks += pi_stick()

    checks += sources()

    # Not a whole number of MiB, with a backup GPT header at its last LBA:
    # the zeroing must reach the very end (the review of #293).
    blank("stick", (128 << 20) + 3 * 512)
    with open(STICK, "r+b") as fh:
        fh.seek(-512, os.SEEK_END)
        fh.write(b"EFI PART")
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
        ("the root's /etc/passwd and /etc/srcid are the image's, /etc/disksig the signature",
         code == 0 and ext2(STICK, "cat /etc/passwd") == open(os.path.join(BUILD, "esp", "etc", "passwd")).read()
         and ext2(STICK, "cat /etc/srcid") == srcid_of_build()
         and ext2(STICK, "cat /etc/disksig").strip() == f"{disksig.read_signature(STICK):08x}"),
        ("the last LBA's old GPT header is gone", open(STICK, "rb").read()[-512:] == bytes(512)),
        ("the disk has a signature, not disk.img's", sig != 0 and sig != disksig.read_signature(os.path.join(BUILD, "disk.img"))),
    ]

    # A disk that ends inside the MiB after the image: the zeroing of the last
    # MiB used to be skipped whole then, leaving the backup GPT header at its
    # last LBA (the second review of #293).
    image_size = (parts[1][2] + parts[1][3]) * 512 if len(parts) > 1 else 0
    tight = blank("tight", image_size + 3 * 512)
    with open(tight, "r+b") as fh:
        fh.seek(-512, os.SEEK_END)
        fh.write(b"EFI PART")
    code, out = bootstick_run(tight)
    checks.append(("a disk 3 sectors larger than the image: written, its last LBA's old GPT header gone",
                   image_size > 0 and code == 0 and open(tight, "rb").read()[-512:] == bytes(512)))
    if not checks[-1][1]:
        print(out)
    os.remove(tight)

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

    # The old root's build stamp as an earlier build's: it must not carry, or
    # the restaged boot meets two stamps and the stale-image guard refuses it.
    stamp = os.path.join(WORK, "old-srcid")
    with open(stamp, "w") as fh:
        fh.write("an earlier build\n")
    ext2(STICK, "rm /etc/srcid", write=True)
    ext2(STICK, f"write {stamp} /etc/srcid", write=True)
    code, out = bootstick_run(STICK)
    print(out, end="")
    checks += [
        ("re-staged: the build stamp is the new image's, not the old root's",
         ext2(STICK, "cat /etc/srcid") == srcid_of_build()),
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

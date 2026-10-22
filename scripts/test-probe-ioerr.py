#!/usr/bin/env python3
"""A read error inside a partition's probe is an I/O error, not "no filesystem".

    python3 scripts/test-probe-ioerr.py     (or `make test-probe-ioerr`, which builds the images)

fsd probes each partition as FAT32, exFAT and ext2. Until 2026-10-20 a read
that failed inside a probe read the same as a partition holding none of them
(`vfs::Filesystem::probe` answered `None` for both), so on a disk that was
there and failing, `mount -a` found "no filesystem" and had the kernel swap
in whatever USB stick was attached, putting the stick at `/` with the real
disk out of reach until a reboot, and `format` took the partition it could
not read as empty and laid a new filesystem over it. Only a read error in the
partition table was told apart (#324).

Five boots, each on fresh copies. In each the firmware boots esp.img from
NVMe, which the kernel cannot drive, so the virtio disk goes in as the
fallback block device, and the boot partition is not known to fsd. That disk
is a copy of build/espexfat.img (exFAT, then the ESP) or build/espext2.img
(ext2, then the ESP), signed again (scripts/disksig.py) so it is not taken
for the boot disk, attached through QEMU's blkdebug driver with every read of
one sector failing with EIO. A USB stick, a copy of build/usbstick.img, is
beside it.

- MOUNT: the exFAT copy with the ESP's entry zeroed (one partition), its
  root directory's first sector failing (read by the exFAT probe's
  `locate_bitmap`). fsd's boot line must say the mount failed with a disk I/O
  error; `mount -a` must answer that the disk gave an I/O error and was not
  replaced, with no `usb-msd block device installed` line; `mount` must say
  nothing is mounted.
- FORMAT: the same disk; `format ext2` must be refused, and after the boot
  the partition must be as it was, every byte. The bytes are the check: on
  main before 2026-10-20 the format wrote ext2 over the partition and then
  failed on the sector that would not read, printing the same refusal.
- BESIDE: the exFAT copy whole, the same sector failing. The ESP mounts, and
  the boot line must say a partition before it would not read; `mount 0
  /mnt/x` (the exFAT partition) must answer `device I/O error`; after
  `unmount`, `format ext2` must be refused and both partitions be as they
  were: with the boot partition not known both are the root's candidates,
  and the format took the ESP when the exFAT before it would not read (the
  review of #329).
- TABLE (since 2026-10-22): a copy of build/espgpt.img, its GPT header
  (sector 1) failing, #324's own case: the partition table will not read,
  so fsd's boot line and `mount -a` must answer as in MOUNT, and no stick be
  installed. The kernel reads sector 0 alone before it installs a disk, and
  takes one whose sector 1 will not read as not the boot disk (D6), so this
  disk goes in as the fallback. A failing sector 0 cannot be the case: the
  kernel drops such a disk and installs the stick itself (the roadmap).
  TABLE rests on parttable's rule that sector 1 unread is `Unreadable`, not
  a reason to read the backup header: if that rule changes, the disk would
  mount from its backup and TABLE must fail some other way.
- EXT2: the ext2 copy with the ESP's entry zeroed, the partition's first
  sector failing: FAT32's and exFAT's probes read it, ext2's reads its
  superblock two sectors on. ext2 must mount, as it did before the change:
  the probe tries every arm before answering a read's error (the review of
  #329 found the first version stopping at FAT32's).

In each but TABLE, the failing sector must be past sectors 0 and 1, the partition
table's reads, and is checked before the boot: a precondition, not an
observation, since blkdebug says nothing of which reads it failed. BESIDE is
the evidence that the table reads through it (the ESP mounts), and the
controls below that the error reaches the probe. No fault line in QEMU's
trace. Fails with the probe answering `None` for a read error, as on main
before 2026-10-20 (MOUNT: the stick installed and mounted; FORMAT: the
partition overwritten), with the probe stopping at its first arm's read
error (EXT2: nothing mounts), with `format` going on past a candidate that
would not read (BESIDE: the ESP formatted), with `mount N` answering
"no filesystem" (BESIDE), and with an unreadable partition table read as
one holding no partitions (TABLE: the stick installed and mounted). That the stick can be swapped in at all is
test-mount-gate's check. About four minutes. Run it whenever fsd's probe,
mount, format, `mount N` or `mount -a`'s rescan changes.
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
BUILD = os.path.join(ROOT, "build")
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

ESP = os.path.join(BUILD, "esp.img")
EXFAT = os.path.join(BUILD, "espexfat.img")
EXT2 = os.path.join(BUILD, "espext2.img")
GPT = os.path.join(BUILD, "espgpt.img")
STICK = os.path.join(BUILD, "usbstick.img")
COPY = os.path.join(BUILD, "test-probe-ioerr.img")
STICK_COPY = os.path.join(BUILD, "test-probe-ioerr-stick.img")
BLKDEBUG = os.path.join(BUILD, "test-probe-ioerr.blkdebug")
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)

# name: (source image, keep the ESP's entry, which sector fails, commands)
BOOTS = {
    "MOUNT": (EXFAT, False, "exfat-root", ["mount -a", "mount"]),
    "FORMAT": (EXFAT, False, "exfat-root", ["format ext2"]),
    "BESIDE": (EXFAT, True, "exfat-root", ["mount 0 /mnt/x", "unmount", "format ext2"]),
    "EXT2": (EXT2, False, "first", ["mount"]),
    "TABLE": (GPT, True, "gpt-header", ["mount -a", "mount"]),
    "SECTOR0": (EXFAT, True, "sector0", ["mount -a", "mount"]),
}
# The boots with no USB stick beside the disk.
NO_STICK = {"SECTOR0"}


def partitions(image):
    """[(type, start, sectors)] of the MBR's used entries, read on the host."""
    with open(image, "rb") as fh:
        mbr = fh.read(512)
    assert mbr[510:512] == b"\x55\xaa", f"{image}: no MBR"
    found = []
    for i in range(4):
        e = mbr[0x1BE + 16 * i:0x1CE + 16 * i]
        if e[4]:
            found.append((e[4], int.from_bytes(e[8:12], "little"), int.from_bytes(e[12:16], "little")))
    return found


def prepare(source, keep_esp, rule):
    """The virtio copy and the sector that fails: the exFAT root directory's
    first (from the volume's boot sector), the GPT header (sector 1), the
    MBR (sector 0), or partition 1's first."""
    shutil.copyfile(source, COPY)
    _, start, _ = partitions(COPY)[0]
    with open(COPY, "r+b") as fh:
        if not keep_esp:
            mbr = bytearray(fh.read(512))
            mbr[0x1CE:0x1DE] = bytes(16)  # the ESP's entry: one partition left
            fh.seek(0)
            fh.write(mbr)
        fh.seek(start * 512)
        vbr = fh.read(512)
    if rule == "exfat-root":
        assert vbr[3:11] == b"EXFAT   ", f"{source}: partition 1 holds no exFAT"
        heap = int.from_bytes(vbr[88:92], "little")
        root = int.from_bytes(vbr[96:100], "little")
        failing = start + heap + ((root - 2) << vbr[109])
    elif rule == "gpt-header":
        assert partitions(COPY)[0][0] == 0xEE, f"{source}: no protective MBR"
        # The sector holding the primary header's signature, found rather
        # than assumed, so the precondition that it is sector 1 can fail.
        with open(COPY, "rb") as fh:
            head = fh.read(34 * 512)
        failing = next((i for i in range(34) if head[i * 512:i * 512 + 8] == b"EFI PART"), -1)
    elif rule == "sector0":
        failing = 0
    else:
        failing = start
    # A GPT disk takes no MBR signature, and needs none: the boot partition
    # (esp.img's) is an MBR one, which no GPT disk matches.
    if rule != "gpt-header":
        subprocess.run([sys.executable, os.path.join(HERE, "disksig.py"), "set", COPY], check=True,
                       stdout=subprocess.DEVNULL)
    return failing


def digests(image):
    """Each partition's SHA-256, every byte of it."""
    out = []
    with open(image, "rb") as fh:
        for _, start, sectors in partitions(image):
            fh.seek(start * 512)
            out.append(hashlib.sha256(fh.read(sectors * 512)).hexdigest())
    return out


def boot(name):
    """One boot on fresh copies: (driven, transcript, faults, partitions
    unchanged, failing sector, partition 1's start)."""
    source, keep_esp, rule, commands = BOOTS[name]
    steps = [("# ", cmd) for cmd in commands] + [("# ", "")]
    if name in ("BESIDE", "EXT2"):
        # A filesystem mounts, with accounts on it: login as root first.
        steps = [("login:", "root"), ("assword", "root")] + steps
    guest = None
    try:
        failing = prepare(source, keep_esp, rule)
        start = partitions(COPY)[0][1]
        shutil.copyfile(STICK, STICK_COPY)
        before = digests(COPY)
        with open(BLKDEBUG, "w") as fh:
            fh.write(f'[inject-error]\nevent = "read_aio"\nerrno = "5"\nsector = "{failing}"\n')
        extra = [
            # bootindex=0 pins the firmware to the NVMe disk, which it boots
            # and the kernel cannot drive (test-boot-partition's fallback case).
            "-drive", f"file={ESP},format=raw,if=none,id=nvboot,snapshot=on",
            "-device", "nvme,serial=boot,drive=nvboot,bootindex=0",
            # The failing disk: every read of the one sector answers EIO.
            "-drive", f"file=blkdebug:{BLKDEBUG}:{COPY},format=raw,if=none,id=hd0,rerror=report",
            "-device", "virtio-blk-device,drive=hd0",
        ]
        if name not in NO_STICK:
            extra += [
                "-device", "qemu-xhci,id=xhci0",
                "-drive", f"file={STICK_COPY},format=raw,if=none,id=usbstick",
                "-device", "usb-storage,drive=usbstick,bus=xhci0.0,port=1",
            ]
        guest = drive_qemu.Guest(COPY, extra_args=extra, virtio_disk=False, stamp_from=ESP,
                                 label=f"test-probe-ioerr[{name}]: ")
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
        guest.stop()
        guest = None
        after = digests(COPY)
    finally:
        if guest:
            guest.stop()
        for f in (COPY, STICK_COPY, BLKDEBUG):
            if os.path.exists(f):
                os.remove(f)
    with open(os.path.join(BUILD, f"test-probe-ioerr-{name.lower()}.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults, before == after, failing, start


def outputs(out, commands):
    """[output of each command], in order: the lines after its prompt line
    (`# cmd`) up to the next prompt, the kernel's exit lines dropped. netd's
    boot lines can land after a prompt, so a command may be echoed on a line
    of its own."""
    got = []
    at = 0
    for cmd in commands:
        m = re.compile(rf"^(?:# )?{re.escape(cmd)}\r?$", re.M).search(out, at)
        if not m:
            got.append(None)
            continue
        nxt = re.compile(r"^# ", re.M).search(out, m.end())
        block = out[m.end():nxt.start() if nxt else len(out)]
        at = nxt.start() if nxt else len(out)
        got.append(EXIT.sub("", block).strip())
    return got


def checks_of(name):
    commands = BOOTS[name][3]
    driven, out, faults, untouched, failing, start = boot(name)
    # A precondition: the table's reads are sectors 0 and 1 (MBR, GPT
    # header), so the error must land past them, except in TABLE, where it
    # is the GPT header's. The host's own check, graded even when the
    # firmware never started the kernel.
    pre = [(f"{name}: the failing sector, {failing}, is the GPT header's", failing == 1) if name == "TABLE" else
           (f"{name}: the failing sector, {failing}, is the MBR's", failing == 0) if name == "SECTOR0" else
           (f"{name}: the failing sector, {failing}, is past the table's (partition 1 at {start})",
            failing >= start > 1)]
    if drive_qemu.boot_stalled("test-probe-ioerr", name, out, faults):
        return pre, True
    o = outputs(out, commands)
    boot_log = out.split(f"# {commands[0]}")[0]
    checks = [
        ("driven to the end, every command's output found", driven and None not in o),
    ]
    # One write since 2026-10-20 (it was three, and another task's line
    # landed inside it), so the whole line, before the first command.
    io_at_boot = "fsd: mount failed (disk I/O error) - disk commands won't work\r\n" in boot_log
    if name in ("MOUNT", "FORMAT", "TABLE", "SECTOR0"):
        checks.append(("fsd's boot mount failed with a disk I/O error", io_at_boot))
    if name == "SECTOR0":
        checks += [
            ("the kernel held the disk, sector 0 unreadable twice",
             "virtio-blk read failed twice" in boot_log and "held as not the boot disk" in boot_log),
            ("and installed it as the fallback, no stick attached",
             "virtio-blk installed as the fallback: no USB stick is attached" in boot_log),
        ]
    if name in ("MOUNT", "TABLE", "SECTOR0"):
        checks += [
            (f"mount -a answered the I/O error, the disk not replaced (got {o[0]!r})",
             (o[0] or "").endswith("mount: the disk answered with an I/O error; it was not replaced by a USB stick")),
            ("no stick installed", "usb-msd block device installed" not in out),
            (f"mount after it: nothing mounted (got {o[1]!r})", (o[1] or "").startswith("nothing mounted")),
        ]
    if name == "FORMAT":
        checks += [
            (f"format ext2 refused (got {o[0]!r})",
             o[0] == "format: the partition is too small, or an I/O error occurred"),
            ("the partition as it was, every byte", untouched),
        ]
    if name == "BESIDE":
        checks += [
            ("the ESP mounted, the line saying a partition before it would not read",
             "fsd: a partition before it would not read (disk I/O error)\r\nfsd: FAT32 mounted" in boot_log),
            (f"mount 0 /mnt/x answered the I/O error (got {o[0]!r})", o[0] == "mount: device I/O error"),
            (f"unmount (got {o[1]!r})", o[1] == "unmounted"),
            (f"format ext2 refused (got {o[2]!r})",
             o[2] == "format: the partition is too small, or an I/O error occurred"),
            ("both partitions as they were, every byte", untouched),
        ]
    if name == "EXT2":
        checks += [
            ("ext2 mounted at boot", "fsd: ext2 mounted, disk commands available" in boot_log),
            (f"mount reports ext2 at partition 1 (got {o[0]!r})",
             (o[0] or "").startswith(f"ext2 mounted at partition LBA {start} ")),
        ]
    checks.append(("no fault lines", faults == 0))
    return pre + [(f"{name}: {label}", ok) for label, ok in checks], False


def main():
    for f in (ESP, EXFAT, EXT2, GPT, STICK):
        if not os.path.exists(f):
            sys.exit(f"{f} not found - run `make test-probe-ioerr`, which builds it")
    checks = []
    stalled = 0
    for name in BOOTS:
        got, stall = checks_of(name)
        checks += got
        stalled += stall
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    return drive_qemu.verdict("test-probe-ioerr", failed, stalled)


if __name__ == "__main__":
    sys.exit(main())

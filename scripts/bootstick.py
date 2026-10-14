#!/usr/bin/env python3
"""Write the one-disk layout to a whole disk: one-disk step 5, `make bootstick`.

    python3 scripts/bootstick.py /dev/diskN        a stick, by its whole disk
    python3 scripts/bootstick.py FILE              a disk image file, attached
                                                   with hdiutil and written as
                                                   a stick is
    python3 scripts/bootstick.py --self-test       the pure rules (`make test`)

Options, in the environment: CARRY=0 writes the image's /etc and /Users,
carrying none of the old disk's (the boot counter and the signature are
carried still); EJECT=1 ejects a device when done. PI=1 makes the Raspberry
Pi 4's stick, the pftf firmware at the root of partition 1 beside the ESP
tree (step 5b), and a Pi stick re-staged stays one unless PI=0. FIRMWARE=1
reinstalls the pinned firmware on a Pi stick, resetting its settings and
keeping its config.txt.

What it writes is build/disk.img's layout (scripts/mkdisk.py): the FAT32 ESP
as partition 1 and the ext2 root as partition 2, built here from build/esp.img
and build/ext2part.img with the old disk's state carried over. Decision D2
(Hans, 2026-10-13) is the image written, `/etc` carried from the old root; on
2026-10-14 he added `/Users`, so a re-stage loses nothing made on the stick,
and left the root at the image's size (growing it is its own roadmap item).
docs/roadmap/roadmap-one-disk.md, step 5, is the plan.

Carried over from the old disk, when it is ours:
  /etc and /Users  from the old root: each file's bytes, mode, owner and
                   group, and symbolic links. Laid over the new root, the old
                   disk's entry winning where both have one; an entry only the
                   new image has (a file a newer tree introduced) stays. A
                   home the old disk has replaces the image's whole. Never
                   /etc/srcid, the build's stamp, or /etc/disksig, below.
  the disk signature  the old disk's, since it is the same disk (the
                   signature is a disk's name, scripts/disksig.py), when the
                   root's /etc/disksig says this script gave it. Any other
                   disk gets a random one, never the image's: two sticks
                   written from one build, or copied from disk.img with dd,
                   must differ.
  the boot counter  \\EFI\\ORBS\\BOOTID.TXT, the larger of the old and the
                   new: rolling the file back would let a boot hand out a
                   counter it handed out before, on a machine whose variable
                   store does not keep it (kernel/src/bootid.rs). From a
                   written esp.img too, read off its volume.

The old disk's root is ext2 for a disk this script wrote, so it is read with
e2fsprogs' debugfs, never mounted. A stick staged by `make stick` (FAT32 or
exFAT, `.ouroboros-stick` at its root) or a written esp.img (`EFI/ORBS/
INIT.CFG`) has no modes or owners: its files take the new image's mode for
the same path, else 0644 and 0755, and a home directory and everything under
it takes the owner whose home it is in the carried /etc/passwd (this
project's `name:uid:gid:home`), else root.

The guard, since this replaces a whole disk. The target must be a whole disk
(`/dev/diskN`, not a partition), external or removable, not the disk the Mac
booted from, and large enough. And it must be empty or ours:
  ours     an MBR with a FAT ESP as partition 1 and an ext2 partition 2
           holding /etc/srcid (a disk this wrote), or a FAT32 or exFAT
           volume holding `.ouroboros-stick` or `EFI/ORBS/INIT.CFG`;
  empty    no partition table and nothing in its first MiB, or every volume
           on it empty but for what macOS leaves on a fresh one.
A Pi card (RPI_EFI.fd on a plain FAT volume) is refused: the kernel cannot
read the card once it runs, so the Pi's one disk is a stick.

The Pi's stick (PI=1) is the same disk with the firmware the Pi's boot ROM
reads from partition 1: the pinned pftf release (scripts/pftf.env, shared
with make sdcard) on a fresh stick, and on a re-stage the stick's own
firmware files carried whole, since RPI_EFI.fd is both the firmware and the
store of its settings and UEFI variables. Anything this cannot read is refused rather than
guessed at. A device it cannot open is read and written through `sudo dd`;
an attached image file belongs to the user and needs no sudo.

The old root is checked for /etc/srcid in place, on the device, before any
of it is copied. The disk as it will be written is built at
build/bootstick-written.img and kept until the disk reads it back, since
once the write starts it is the only copy of the old /etc and /Users. After
the write the bytes are read back and compared, and the disk's last MiB, up
to its last LBA, is zeroed, so a backup GPT left by a stick's old table
cannot be found behind the new MBR.
"""
import contextlib
import hashlib
import io
import os
import plistlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import disksig  # noqa: E402
import mbrdisk  # noqa: E402

ME = "bootstick"
SECT = 512
MIB = 1 << 20
ESP_IMG = os.path.join(ROOT, "build", "esp.img")
KEPT_IMG = os.path.join(ROOT, "build", "bootstick-written.img")
ESP_TREE = os.path.join(ROOT, "build", "esp")
CACHE = os.path.join(ROOT, "build", "cache")
PFTF_ENV = os.path.join(HERE, "pftf.env")
# What marks a Pi's boot partition: the pftf firmware, which also holds its
# settings and every UEFI variable (no NVRAM on the Pi; scripts/sdcard.sh).
FIRMWARE_MARK = "RPI_EFI.fd"
ROOT_IMG = os.path.join(ROOT, "build", "ext2part.img")
ESP_TYPES = (0x0B, 0x0C, 0xEF)
EXT2_TYPE = mbrdisk.EXT2_TYPE
CARRIED = ("etc", "Users")
# Under /etc but the build's or this script's, never the old disk's: the
# build stamp (scripts/srcid.py; an old one beside a new ESP's is two stamps,
# which the stale-image guard refuses), and the signature this script gave
# the disk, which is how a re-stage knows the signature is one it chose.
NEVER_CARRIED = ("etc/srcid", "etc/disksig")
SIG_FILE = "etc/disksig"
# What macOS leaves on a fresh FAT volume, the same set scripts/sdcard.sh
# does not count as contents.
LITTER = re.compile(r"^(\._.*|\.DS_Store|\.Spotlight-V100|\.fseventsd|\.Trashes|\.TemporaryItems|"
                    r"\.VolumeIcon\.icns|\.metadata_never_index|\.apdisk|System Volume Information)$")
BOOTID_PATH = ("EFI", "ORBS", "BOOTID.TXT")
BOOTID_RECORD = re.compile(rb"^\d{20}\n$")


class Refused(Exception):
    """A reason not to write, said as it is."""


def say(msg):
    print(f"{ME}: {msg}", flush=True)


def tool(name):
    prefix = subprocess.run(["brew", "--prefix", "e2fsprogs"], capture_output=True, text=True).stdout.strip()
    path = os.path.join(prefix, "sbin", name)
    if not os.access(path, os.X_OK):
        raise Refused(f"{name} not found - run: brew install e2fsprogs")
    return path


# --- Pure rules: no device, no tool. `--self-test` covers each. -------------

parse_mbr = mbrdisk.entries


def is_one_disk(parts):
    """The layout this script writes: a FAT ESP as partition 1 and an ext2
    partition 2. Its root still has to show it is ours (/etc/srcid)."""
    if not parts:
        return None
    by_number = {p[0]: p for p in parts}
    esp, root = by_number.get(1), by_number.get(2)
    if esp and root and esp[1] in ESP_TYPES and root[1] == EXT2_TYPE:
        return esp, root
    return None


def volume_kind(names):
    """What the top-level names of a mounted volume say it is: 'card',
    'ours', 'empty', or None for anything else."""
    names = [n for n in names if not LITTER.match(n)]
    if "RPI_EFI.fd" in names:
        return "card"
    if ".ouroboros-stick" in names:
        return "ours"
    if not names:
        return "empty"
    return None


def homes_from_passwd(text):
    """{home directory name under /Users: (uid, gid)} from a passwd file in
    this project's format, `name:uid:gid:home` (accounts/src/lib.rs,
    parse_account; scripts/mkpasswd.py), not Unix's seven fields."""
    homes = {}
    for line in text.splitlines():
        fields = line.split(":")
        if len(fields) < 4:
            continue
        try:
            uid, gid = int(fields[1]), int(fields[2])
        except ValueError:
            continue
        home = fields[3].rstrip("/")
        if home.startswith("/Users/") and "/" not in home[len("/Users/"):]:
            homes[home[len("/Users/"):]] = (uid, gid)
    return homes


def fat_metadata(rel, kind, new_entries, homes):
    """(mode, uid, gid) for an entry carried from a volume that records none:
    the new image's mode for the same path, else 0644 or 0755; the owner of
    the home it is in, else root."""
    if rel in new_entries and new_entries[rel]["kind"] == kind:
        perm = new_entries[rel]["mode"] & 0o7777
    else:
        perm = 0o755 if kind == "d" else 0o644
    mode = (0o040000 if kind == "d" else 0o100000) | perm
    parts = rel.split("/")
    uid, gid = (0, 0)
    if len(parts) >= 2 and parts[0] == "Users" and parts[1] in homes:
        uid, gid = homes[parts[1]]
    return mode, uid, gid


def quote(path):
    """A path as one debugfs argument. Its parser takes double quotes and no
    escapes, so a name holding one cannot be said, and is refused."""
    if '"' in path or "\n" in path:
        raise Refused(f"cannot carry {path!r}: debugfs cannot name a path with a quote or a newline")
    return f'"{path}"'


def plan_merge(old, new, whole=()):
    """The debugfs commands that lay `old` over `new`. Each is a dict of
    relative path -> entry {kind: 'd'|'f'|'l', mode, uid, gid, src}, where
    src is a host file for 'f' and the target for 'l'. Old wins where both
    have the path; what only new has stays, except under a path in `whole`
    that old has, which old replaces outright (a home: a file deleted from it
    on the stick must not come back from the image). Pure, so the self-test
    can check the order: a path is cleared before it is made, a directory
    before what is in it, and nothing is removed from a directory before it
    is emptied."""
    cmds = []

    def remove(rel):
        # Children first, deepest first, then the entry itself.
        below = sorted((p for p in new if p.startswith(rel + "/")), key=lambda p: -p.count("/"))
        for p in below:
            cmds.append(f"{'rmdir' if new[p]['kind'] == 'd' else 'rm'} {quote('/' + p)}")
        cmds.append(f"{'rmdir' if new[rel]['kind'] == 'd' else 'rm'} {quote('/' + rel)}")
        for p in below + [rel]:
            gone.add(p)

    gone = set()
    for rel in sorted(old, key=lambda p: (p.count("/"), p)):
        o = old[rel]
        n = new.get(rel) if rel not in gone and not any(rel.startswith(g + "/") for g in gone) else None
        path = quote("/" + rel)
        if o["kind"] == "d":
            if n and (n["kind"] != "d" or rel in whole):
                remove(rel)
                n = None
            if not n:
                cmds.append(f"mkdir {path}")
        else:
            if n:
                remove(rel)
            if o["kind"] == "f":
                cmds.append(f"write {quote(o['src'])} {path}")
            else:
                cmds.append(f"symlink {path} {quote(o['src'])}")
        if o["kind"] != "l":
            cmds.append(f"sif {path} mode 0{o['mode']:o}")
        cmds.append(f"sif {path} uid {o['uid']}")
        cmds.append(f"sif {path} gid {o['gid']}")
    return cmds


def fat32_find(read, path):
    """(byte offset of the file's first cluster, size) of `path`, a tuple of
    8.3 names, in the FAT32 filesystem `read(offset, length)` reads, or None.
    Short names only: every name this looks for is one. Enough for the boot
    counter's file, and no more."""
    bpb = read(0, SECT)
    bps = struct.unpack_from("<H", bpb, 11)[0]
    spc = bpb[13]
    reserved = struct.unpack_from("<H", bpb, 14)[0]
    fats = bpb[16]
    fat_size, root_cluster = struct.unpack_from("<I", bpb, 36)[0], struct.unpack_from("<I", bpb, 44)[0]
    if bpb[510:512] != b"\x55\xaa" or bps not in (512, 1024, 2048, 4096) or not spc or not fat_size:
        return None
    data = (reserved + fats * fat_size) * bps
    cluster_bytes = spc * bps

    def short(name):
        base, _, ext = name.upper().partition(".")
        return (base.ljust(8) + ext.ljust(3)).encode()

    def chain(cluster):
        seen = set()
        while 2 <= cluster < 0x0FFFFFF8 and cluster not in seen:
            seen.add(cluster)
            yield data + (cluster - 2) * cluster_bytes
            cluster = struct.unpack("<I", read(reserved * bps + cluster * 4, 4))[0] & 0x0FFFFFFF

    cluster = root_cluster
    for depth, name in enumerate(path):
        want, found = short(name), None
        for off in chain(cluster):
            block = read(off, cluster_bytes)
            for i in range(0, cluster_bytes, 32):
                e = block[i:i + 32]
                if e[0] == 0:
                    break
                if e[0] == 0xE5 or e[11] & 0x0F == 0x0F or e[11] & 0x08:
                    continue
                if e[:11] == want:
                    found = e
                    break
            if found or (block and any(block[i] == 0 for i in range(0, cluster_bytes, 32))):
                break
        if not found:
            return None
        first = (struct.unpack_from("<H", found, 20)[0] << 16) | struct.unpack_from("<H", found, 26)[0]
        is_dir = bool(found[11] & 0x10)
        if depth < len(path) - 1:
            if not is_dir:
                return None
            cluster = first
        else:
            if is_dir or first < 2:
                return None
            return data + (first - 2) * cluster_bytes, struct.unpack_from("<I", found, 28)[0]
    return None


def bootid_value(record):
    """The counter in a boot counter record, or None if it is not one."""
    return int(record[:20]) if record and BOOTID_RECORD.match(record) else None


# --- The disk. ---------------------------------------------------------------

def diskutil(*args):
    plist = args[0] in ("info", "list")
    out = subprocess.run(["diskutil", args[0], "-plist", *args[1:]] if plist else ["diskutil", *args],
                         capture_output=True)
    if out.returncode != 0:
        raise Refused(f"diskutil {' '.join(args)} failed: {out.stderr.decode().strip() or out.stdout.decode().strip()}")
    return plistlib.loads(out.stdout) if plist else out.stdout.decode()


class Disk:
    """A whole disk, read and written through its raw node, by `sudo dd` when
    this user cannot open it."""

    def __init__(self, dev):
        self.dev = dev
        self.raw = "/dev/r" + dev[len("/dev/"):]
        self.info = diskutil("info", dev)
        self.size = self.info.get("Size") or self.info.get("TotalSize") or 0
        self._sudo_read = None

    def read(self, offset, length):
        """`length` bytes at any `offset`: the raw node takes whole sectors,
        so the read is widened to them and cut back."""
        lead = offset % SECT
        if lead:
            return self.read(offset - lead, length + lead)[lead:]
        padded = (length + SECT - 1) // SECT * SECT
        if self._sudo_read is not True:
            try:
                with open(self.raw, "rb", buffering=0) as fh:
                    fh.seek(offset)
                    data = bytearray()
                    while len(data) < padded:
                        chunk = fh.read(min(MIB, padded - len(data)))
                        if not chunk:
                            break
                        data += chunk
                    self._sudo_read = False
                    return bytes(data[:length])
            except PermissionError:
                self._sudo_read = True
                say(f"{self.raw} is not readable as you; reading through sudo dd")
        out = subprocess.run(["sudo", "dd", f"if={self.raw}", "bs=512", f"skip={offset // SECT}",
                              f"count={padded // SECT}"], capture_output=True)
        if out.returncode != 0:
            raise Refused(f"sudo dd could not read {self.raw}: {out.stderr.decode().strip()}")
        return out.stdout[:length]

    def write_file(self, path, offset=0):
        try:
            with open(path, "rb") as src, open(self.raw, "r+b", buffering=0) as dst:
                dst.seek(offset)
                while True:
                    chunk = src.read(MIB)
                    if not chunk:
                        break
                    if len(chunk) % SECT:
                        chunk += bytes(SECT - len(chunk) % SECT)
                    dst.write(chunk)
            return
        except PermissionError:
            say(f"{self.raw} is not writable as you; writing through sudo dd")
        bs = MIB if offset % MIB == 0 else SECT
        out = subprocess.run(["sudo", "dd", f"if={path}", f"of={self.raw}", f"bs={bs}", f"oseek={offset // bs}",
                              "conv=sync"], capture_output=True)
        if out.returncode != 0:
            raise Refused(f"sudo dd could not write {self.raw}: {out.stderr.decode().strip()}")


def check_target(disk, need):
    info = disk.info
    if not info.get("WholeDisk"):
        raise Refused(f"{disk.dev} is a partition; name the whole disk (/dev/diskN)")
    if info.get("Internal") and not info.get("RemovableMedia"):
        raise Refused(f"{disk.dev} is internal, fixed media; refusing")
    if disk.dev in system_disks():
        raise Refused(f"{disk.dev} holds the Mac's own system volume; refusing")
    if info.get("Writable") is False:
        raise Refused(f"{disk.dev} is read-only (a locked card or stick?)")
    if disk.size < need:
        raise Refused(f"{disk.dev} holds {disk.size} bytes; the image needs {need}")


def system_disks():
    """The whole disks the Mac's system volume lives on: on APFS the
    container diskutil names for `/` is synthesized, so the physical stores
    behind it are looked up too (the review of #293)."""
    boot = diskutil("info", "/")
    disks = {f"/dev/{boot.get('ParentWholeDisk')}", f"/dev/{boot.get('DeviceIdentifier')}"}
    for store in boot.get("APFSPhysicalStores") or []:
        ident = store.get("APFSPhysicalStore")
        if ident:
            disks.add(f"/dev/{diskutil('info', ident).get('ParentWholeDisk')}")
    return disks


def volumes(disk):
    """[(identifier, mount point or None)] of the partitions macOS sees on
    the disk, or of the whole disk when it carries a volume of its own."""
    listing = diskutil("list", disk.dev)
    entries = []
    for whole in listing.get("AllDisksAndPartitions", []):
        if f"/dev/{whole.get('DeviceIdentifier')}" != disk.dev:
            continue
        for part in whole.get("Partitions", []):
            entries.append((part["DeviceIdentifier"], part.get("MountPoint")))
        if not whole.get("Partitions") and whole.get("Content"):
            entries.append((whole["DeviceIdentifier"], whole.get("MountPoint")))
    return entries


def debugfs_read(debugfs, spec, request):
    """debugfs's answer to one read-only request on `spec`, through sudo
    when the device cannot be opened as this user."""
    out = subprocess.run([debugfs, "-R", request, spec], capture_output=True, text=True)
    if "Permission denied" in out.stderr:
        out = subprocess.run(["sudo", debugfs, "-R", request, spec], capture_output=True, text=True)
    return out


def ext2_root_is_ours(debugfs, spec):
    """Whether the ext2 filesystem at `spec` (an image, or a device with
    `?offset=`) holds /etc/srcid, read in place: a Linux stick's 30 GB root
    is refused without being copied first (the review of #293)."""
    out = debugfs_read(debugfs, spec, "stat /etc/srcid")
    return "Inode:" in out.stdout


def ext2_text(debugfs, image, path):
    out = subprocess.run([debugfs, "-R", f"cat {quote('/' + path)}", image], capture_output=True, text=True)
    return out.stdout if "File not found" not in out.stderr else None


def esp_counter(read):
    """The boot counter record in the FAT32 ESP `read(offset, length)`
    reads, or None."""
    try:
        at = fat32_find(read, BOOTID_PATH)
    except (struct.error, IndexError):
        return None
    return read(at[0], 21) if at and at[1] == 21 else None


def classify(disk, debugfs, tmp):
    """('one-disk', root image, old counter record), ('fat', mount point,
    old counter record), or ('empty', None, None); refuses anything else."""
    sector0 = disk.read(0, SECT)
    parts = parse_mbr(sector0)
    layout = is_one_disk(parts)
    if layout:
        esp, root = layout
        if not ext2_root_is_ours(debugfs, f"{disk.dev}?offset={root[2] * SECT}"):
            raise Refused(f"{disk.dev} has an ext2 partition 2 without /etc/srcid: not a disk this wrote; refusing")
        root_img = os.path.join(tmp, "old-root.img")
        with open(root_img, "wb") as fh:
            fh.write(disk.read(root[2] * SECT, root[3] * SECT))
        return "one-disk", root_img, esp_counter(lambda o, n: disk.read(esp[2] * SECT + o, n))
    found = volumes(disk)
    if not found:
        if any(disk.read(0, MIB)):
            raise Refused(f"{disk.dev} has no volume macOS can name, and its first MiB is not empty; refusing")
        return "empty", None, None
    kinds = []
    for ident, mount in found:
        if not mount:
            try:
                diskutil("mount", "readOnly", ident)
            except Refused:
                raise Refused(f"{disk.dev}: {ident} does not mount, so what it holds cannot be told; refusing")
            mount = diskutil("info", ident).get("MountPoint")
        names = os.listdir(mount)
        kind = volume_kind(names)
        if kind is None and os.path.isfile(os.path.join(mount, "EFI", "ORBS", "INIT.CFG")):
            kind = "ours"
        if kind == "card":
            raise Refused(f"{mount} holds RPI_EFI.fd: a Pi card, which the kernel cannot read once it runs; "
                          f"write the Pi's one disk to a USB stick (PI=1), and stage a card with make sdcard")
        if kind is None:
            shown = ", ".join(n for n in names if not LITTER.match(n))[:200]
            raise Refused(f"{mount} ({ident}) is neither empty nor ours, and holds: {shown}; refusing")
        kinds.append((kind, mount))
    ours = [m for k, m in kinds if k == "ours"]
    if len(ours) > 1:
        raise Refused(f"{disk.dev} holds more than one volume of ours ({', '.join(ours)}); refusing to choose")
    if ours:
        counter = os.path.join(ours[0], *BOOTID_PATH)
        record = open(counter, "rb").read() if os.path.isfile(counter) else None
        return "fat", ours[0], record
    return "empty", None, None


# --- The roots: read an ext2 tree, lay one over another. -------------------

def ext2_tree(debugfs, image, top):
    """{relative path: entry} of `top` and everything under it, from
    debugfs's `ls -p`, which prints /inode/mode/uid/gid/name/size/ a line.
    Empty when the image has no `top`."""
    entries = {}
    pending = [top]
    while pending:
        rel = pending.pop()
        out = subprocess.run([debugfs, "-R", f"ls -p {quote('/' + rel)}", image], capture_output=True, text=True)
        lines = [ln for ln in out.stdout.splitlines() if ln.startswith("/")]
        if not lines:
            # Absent only when debugfs says so: any other failure to list a
            # tree would carry nothing and reset the stick's accounts without
            # a word (the review of #293).
            if rel == top and "File not found by ext2_lookup" in out.stderr:
                return {}
            raise Refused(f"cannot list /{rel} in {image}: {out.stderr.strip() or 'no entries'}")
        for line in lines:
            fields = line.split("/")
            if len(fields) < 7:
                raise Refused(f"cannot read debugfs's line {line!r}")
            mode, uid, gid, name = int(fields[2], 8), int(fields[3]), int(fields[4]), fields[5]
            if name == "..":
                continue
            path = rel if name == "." else f"{rel}/{name}"
            if name == "." and rel != top:
                continue
            fmt = mode & 0o170000
            kind = {0o040000: "d", 0o100000: "f", 0o120000: "l"}.get(fmt)
            if kind is None:
                raise Refused(f"/{path} in the old root is neither a file, a directory nor a link (mode {mode:o}); refusing to drop it")
            entries[path] = {"kind": kind, "mode": mode, "uid": uid, "gid": gid, "src": None}
            if kind == "d" and name != ".":
                pending.append(path)
    return entries


def ext2_fetch(debugfs, image, entries, dest):
    """Each file's bytes into `dest`, each link's target into its entry."""
    cmds = []
    for rel, e in entries.items():
        if e["kind"] == "f":
            e["src"] = os.path.join(dest, f"{len(cmds):06d}")
            cmds.append(f"dump {quote('/' + rel)} {quote(e['src'])}")
        elif e["kind"] == "l":
            out = subprocess.run([debugfs, "-R", f"stat {quote('/' + rel)}", image], capture_output=True, text=True)
            m = re.search(r'Fast link dest: "(.*)"', out.stdout)
            if not m:
                raise Refused(f"cannot read the link /{rel} in the old root")
            e["src"] = m.group(1)
    if cmds:
        script = os.path.join(dest, "dump.cmds")
        with open(script, "w") as fh:
            fh.write("\n".join(cmds) + "\n")
        subprocess.run([debugfs, "-f", script, image], capture_output=True, check=True)
        for e in entries.values():
            if e["kind"] == "f" and not os.path.isfile(e["src"]):
                raise Refused("debugfs did not dump every file of the old root")


def fat_tree(mount, top, new_entries, homes):
    entries = {}
    base = os.path.join(mount, top)
    if not os.path.isdir(base):
        return {}
    for dirpath, dirnames, filenames in os.walk(base):
        dirnames[:] = [d for d in dirnames if not LITTER.match(d)]
        rel_dir = os.path.relpath(dirpath, mount)
        mode, uid, gid = fat_metadata(rel_dir, "d", new_entries, homes)
        entries[rel_dir] = {"kind": "d", "mode": mode, "uid": uid, "gid": gid, "src": None}
        for f in filenames:
            if LITTER.match(f):
                continue
            rel = f"{rel_dir}/{f}"
            mode, uid, gid = fat_metadata(rel, "f", new_entries, homes)
            entries[rel] = {"kind": "f", "mode": mode, "uid": uid, "gid": gid, "src": os.path.join(dirpath, f)}
    return entries


def build_root(debugfs, e2fsck, kind, source, tmp):
    """A copy of build/ext2part.img with the old disk's /etc and /Users laid
    over it, checked by e2fsck. Returns its path and what was carried."""
    new_root = os.path.join(tmp, "root.img")
    shutil.copyfile(ROOT_IMG, new_root)
    if kind == "empty":
        return new_root, 0
    old = {}
    new = {}
    for top in CARRIED:
        new.update(ext2_tree(debugfs, new_root, top))
    if kind == "one-disk":
        for top in CARRIED:
            old.update(ext2_tree(debugfs, source, top))
        files = os.path.join(tmp, "files")
        os.makedirs(files)
        ext2_fetch(debugfs, source, old, files)
    else:
        passwd = os.path.join(source, "etc", "passwd")
        homes = homes_from_passwd(open(passwd, errors="replace").read()) if os.path.isfile(passwd) else {}
        for top in CARRIED:
            old.update(fat_tree(source, top, new, homes))
    old = {p: e for p, e in old.items()
           if not any(p == n or p.startswith(n + "/") for n in NEVER_CARRIED)}
    if not old:
        return new_root, 0
    script = os.path.join(tmp, "carry.cmds")
    # /etc file by file, so a file a newer tree introduced is added; /Users
    # home by home, as `make stick` keeps them (scripts/sdcard.sh).
    homes_old = {p for p in old if p.count("/") == 1 and p.startswith("Users/")}
    with open(script, "w") as fh:
        fh.write("\n".join(plan_merge(old, new, whole=homes_old)) + "\n")
    out = subprocess.run([debugfs, "-w", "-f", script, new_root], capture_output=True, text=True)
    # debugfs exits 0 whatever a command did, so its complaints are read: a
    # command that failed prints a line naming itself, the commands do not.
    errors = [ln for ln in out.stderr.splitlines() if ln.strip() and not ln.startswith("debugfs ")]
    if out.returncode != 0 or errors:
        raise Refused("laying the old root over the new one failed: " + "; ".join(errors[:5]))
    check = subprocess.run([e2fsck, "-fn", new_root], capture_output=True, text=True)
    if check.returncode != 0:
        raise Refused(f"the carried root does not check clean (e2fsck -fn):\n{check.stdout}")
    # Every carried file must read back as it was, mode and owner included.
    after = {}
    for top in CARRIED:
        after.update(ext2_tree(debugfs, new_root, top))
    for rel, e in old.items():
        a = after.get(rel)
        if not a or a["kind"] != e["kind"] or (e["kind"] != "l" and a["mode"] != e["mode"]) \
                or a["uid"] != e["uid"] or a["gid"] != e["gid"]:
            raise Refused(f"/{rel} did not carry over as it was: {e} became {a}")
    return new_root, sum(1 for e in old.values() if e["kind"] != "d")


def pftf_zip():
    """The pinned pftf release's zip (scripts/pftf.env), fetched into
    build/cache once and checked against its SHA-256 on every use, as
    scripts/sdcard.sh does. Returns its path and version."""
    pin = {}
    for line in open(PFTF_ENV):
        line = line.strip()
        if line and not line.startswith("#"):
            name, _, value = line.partition("=")
            pin[name] = value
    version, want = pin["FW_VERSION"], pin["FW_SHA256"]
    name = f"RPi4_UEFI_Firmware_{version}.zip"
    path = os.path.join(CACHE, name)
    if not os.path.isfile(path):
        url = f"https://github.com/pftf/RPi4/releases/download/{version}/{name}"
        say(f"fetching {url}")
        os.makedirs(CACHE, exist_ok=True)
        subprocess.run(["curl", "-fsSL", "-o", path + ".part", url], check=True)
        os.replace(path + ".part", path)
    got = hashlib.sha256(open(path, "rb").read()).hexdigest()
    if got != want:
        raise Refused(f"{path} has SHA-256 {got}, expected {want}; delete it and retry, or check the release")
    return path, version


def old_firmware(disk, kind):
    """The mount point of the old disk's ESP when it holds the Pi's firmware
    (a one-disk Pi stick), else None. Mounted read-only if macOS has not."""
    if kind != "one-disk":
        return None
    ident = disk.dev[len("/dev/"):] + "s1"
    mount = diskutil("info", ident).get("MountPoint")
    if not mount:
        try:
            diskutil("mount", "readOnly", ident)
        except Refused:
            raise Refused(f"{disk.dev}: its ESP ({ident}) does not mount, so whether it holds the Pi's firmware "
                          f"cannot be told; refusing")
        mount = diskutil("info", ident).get("MountPoint")
    return mount if os.path.isfile(os.path.join(mount, FIRMWARE_MARK)) else None


def pi_esp(tmp, old_fw, reinstall):
    """A copy of build/esp.img with the Pi's firmware at its root: the old
    disk's own firmware files, RPI_EFI.fd with them, when it had them and
    FIRMWARE=1 was not asked (its settings and variables live in that file,
    and it is the firmware itself, so it is carried whole, as make sdcard
    installs it once); otherwise the pinned release, keeping the old disk's
    config.txt, where the UART overlay is set by hand. What is the firmware's
    on the old disk is everything at its root that build/esp does not own.
    Returns the copy's path and what went on it."""
    copy = os.path.join(tmp, "esp-pi.img")
    shutil.copyfile(ESP_IMG, copy)
    mount = os.path.join(tmp, "esp-pi")
    os.makedirs(mount)
    out = subprocess.run(["hdiutil", "attach", "-nobrowse", "-mountpoint", mount, copy], capture_output=True, text=True)
    if out.returncode != 0:
        raise Refused(f"hdiutil could not attach a copy of {ESP_IMG}: {out.stderr.strip()}")
    try:
        owned = set(os.listdir(ESP_TREE))
        if old_fw and not reinstall:
            for name in sorted(os.listdir(old_fw)):
                if name in owned or LITTER.match(name):
                    continue
                subprocess.run(["cp", "-R", "-X", os.path.join(old_fw, name), mount], check=True)
            note = "the old disk's, RPI_EFI.fd and its settings with it"
        else:
            zip_path, version = pftf_zip()
            # Names exactly as shipped: the pftf readme says renaming them
            # breaks boot. The readme itself is not wanted (sdcard.sh).
            subprocess.run(["unzip", "-o", "-q", zip_path, "-x", "Readme.md", "-d", mount], check=True)
            note = f"pftf {version}" + (" (FIRMWARE=1: the old settings reset)" if old_fw else "")
            if old_fw and os.path.isfile(os.path.join(old_fw, "config.txt")):
                subprocess.run(["cp", "-X", os.path.join(old_fw, "config.txt"), mount], check=True)
                note += ", the old config.txt kept"
        subprocess.run(["find", mount, "-name", "._*", "-delete"], check=True)
        for name in (".fseventsd", ".Trashes", ".Spotlight-V100"):
            shutil.rmtree(os.path.join(mount, name), ignore_errors=True)
    except subprocess.CalledProcessError as e:
        raise Refused(f"putting the Pi's firmware on the ESP failed ({e.cmd[0]}): is the ESP full?")
    finally:
        subprocess.run(["hdiutil", "detach", mount], capture_output=True)
    return copy, note


def esp_with_bootid(kind, old_record, esp_img=ESP_IMG):
    """The ESP of `esp_img`, its boot counter raised to the old disk's when
    that is larger. The record is fixed width and written in place."""
    esp = bytearray(mbrdisk.esp_payload(esp_img))
    new_at = fat32_find(lambda o, n: bytes(esp[o:o + n]), BOOTID_PATH)
    if not new_at:
        raise Refused(f"{esp_img} has no {'/'.join(BOOTID_PATH)}")
    if kind == "empty":
        return bytes(esp), None
    old_value = bootid_value(old_record)
    if old_value is None:
        say("the old disk's boot counter could not be read; the new disk starts from the image's")
        return bytes(esp), None
    new_value = bootid_value(bytes(esp[new_at[0]:new_at[0] + 21]))
    if new_value is None or old_value > new_value:
        esp[new_at[0]:new_at[0] + 21] = old_record
    return bytes(esp), old_value


def write(target):
    debugfs, e2fsck = tool("debugfs"), tool("e2fsck")
    for f in (ESP_IMG, ROOT_IMG):
        if not os.path.isfile(f):
            raise Refused(f"{f} not found - run `make disk` (make bootstick builds it)")
    carry = os.environ.get("CARRY", "1") != "0"
    attached = None
    if not target.startswith("/dev/"):
        if not os.path.isfile(target):
            raise Refused(f"{target} is neither /dev/diskN nor an existing file (make one: mkfile -n 128m {target})")
        out = subprocess.run(["hdiutil", "attach", "-imagekey", "diskimage-class=CRawDiskImage", "-nomount", target],
                             capture_output=True, text=True)
        if out.returncode != 0:
            raise Refused(f"hdiutil could not attach {target}: {out.stderr.strip()}")
        attached = out.stdout.split()[0]
        say(f"{target} attached as {attached}")
    elif not re.fullmatch(r"/dev/disk\d+", target):
        raise Refused(f"{target} is not a whole disk's node (/dev/diskN)")
    try:
        disk = Disk(attached or target)
        check_target(disk, 0)
        with tempfile.TemporaryDirectory() as tmp:
            kind, source, old_record = classify(disk, debugfs, tmp)
            old_fw = old_firmware(disk, kind)
            say(f"{disk.dev} is {'empty' if kind == 'empty' else 'ours (' + kind + ')'}"
                f"{', a Pi stick' if old_fw else ''}")
            # A Pi stick stays one unless PI=0 says otherwise: dropping its
            # firmware loses the firmware's settings with it.
            pi = {"1": True, "0": False}.get(os.environ.get("PI", ""), bool(old_fw))
            reinstall = os.environ.get("FIRMWARE") == "1"
            if reinstall and not pi:
                raise Refused("FIRMWARE=1 reinstalls the Pi's firmware, and this is not a Pi stick (PI=1 makes one)")
            if old_fw and not pi:
                say("PI=0: the Pi's firmware and its settings on this stick are dropped")
            # The old signature is kept only when this script gave it, which
            # the root's /etc/disksig says: a stick that is a dd copy of
            # disk.img carries the image's, which every such copy shares, so
            # it gets a new one (the review of #293).
            old_sig = None
            if kind == "one-disk":
                on_disk = struct.unpack_from("<I", disk.read(0, SECT), disksig.SIG_OFFSET)[0]
                if on_disk and (ext2_text(debugfs, source, SIG_FILE) or "").strip() == f"{on_disk:08x}":
                    old_sig = on_disk
            sig = old_sig or (int.from_bytes(os.urandom(4), "little") or 1)
            root_kind = kind
            if not carry and kind != "empty":
                # Only the root's state: the counter and the disk's name are
                # never rolled back (the module doc says why).
                say("CARRY=0: /etc and /Users not carried; accounts and homes made on this disk are reset")
                root_kind, source = "empty", None
            root, carried = build_root(debugfs, e2fsck, root_kind, source, tmp)
            sig_file = os.path.join(tmp, "disksig")
            with open(sig_file, "w") as fh:
                fh.write(f"{sig:08x}\n")
            out = subprocess.run([debugfs, "-w", "-R", f"write {quote(sig_file)} /{SIG_FILE}", root],
                                 capture_output=True, text=True)
            if "Allocated inode" not in out.stdout + out.stderr:
                raise Refused(f"could not write /{SIG_FILE} into the new root: {out.stderr.strip()}")
            esp_img, firmware = pi_esp(tmp, old_fw, reinstall) if pi else (ESP_IMG, None)
            esp, counter = esp_with_bootid(kind, old_record, esp_img)
            # Kept outside the temporary directory until the disk reads back
            # whole: once the write starts it is the only copy of the old
            # disk's /etc and /Users (the review of #293).
            image = KEPT_IMG
            # assemble signs from the bytes and says so; the signature is
            # replaced below, so its line would name one the disk never gets.
            with contextlib.redirect_stdout(io.StringIO()):
                mbrdisk.assemble(image, mbrdisk.one_disk(esp, mbrdisk.ext2_payload(root)))
            disksig.put_signature(image, sig)
            size = os.path.getsize(image)
            check_target(disk, size)
            diskutil("unmountDisk", disk.dev)
            say(f"writing {size // MIB} MiB to {disk.dev}")
            try:
                disk.write_file(image)
                want = hashlib.sha256(open(image, "rb").read()).hexdigest()
                same = hashlib.sha256(disk.read(0, size)).hexdigest() == want
            except (Refused, OSError) as e:
                raise Refused(f"writing {disk.dev} failed: {e}. The disk as it should be, the old disk's "
                              f"/etc and /Users in it, is {image}; write it with dd once the disk works")
            if not same:
                raise Refused(f"{disk.dev} does not read back what was written; do not boot it. The disk "
                              f"as it should be, the old disk's /etc and /Users in it, is {image}")
            # The last MiB up to the very end, where a backup GPT's header is
            # (the last LBA), whatever the disk's size is a multiple of.
            tail = disk.size - MIB
            if tail >= size:
                zeros = os.path.join(tmp, "zeros")
                with open(zeros, "wb") as fh:
                    fh.write(bytes(MIB))
                disk.write_file(zeros, tail)
            say(f"wrote and read back {disk.dev}: ESP partition 1, ext2 root partition 2, "
                f"disk signature {sig:#010x}{' (kept)' if old_sig else ' (new)'}")
            say(f"carried {carried} files from the old disk's /etc and /Users" if carried
                else "carried nothing: /etc and /Users are the image's")
            if counter is not None:
                say(f"boot counter carried: {counter}")
            if firmware:
                say(f"the Pi's firmware on partition 1: {firmware}")
        os.remove(KEPT_IMG)
    finally:
        if attached:
            subprocess.run(["hdiutil", "detach", attached], capture_output=True)
    if not attached:
        if os.environ.get("EJECT") == "1":
            diskutil("eject", target)
        else:
            say(f"not ejected; run diskutil eject {target} (or pass EJECT=1)")


# --- The self-test: each pure rule on synthetic input. ----------------------

def self_test():
    results = []

    def mbr(*entries):
        s = bytearray(SECT)
        s[510:512] = b"\x55\xaa"
        for i, (ptype, start, n) in enumerate(entries):
            e = 0x1BE + i * 16
            s[e + 4] = ptype
            struct.pack_into("<II", s, e + 8, start, n)
        return bytes(s)

    results.append(("no MBR is no table", parse_mbr(bytes(SECT)) is None))
    results.append(("a protective MBR is no table", parse_mbr(mbr((0xEE, 1, 100))) is None))
    one = parse_mbr(mbr((0x0C, 2048, 100), (0x83, 4096, 100)))
    results.append(("the written layout is recognised", is_one_disk(one) is not None))
    results.append(("the root first is not it", is_one_disk(parse_mbr(mbr((0x83, 2048, 100), (0x0C, 4096, 100)))) is None))
    results.append(("an ESP alone is not it", is_one_disk(parse_mbr(mbr((0x0C, 2048, 100)))) is None))
    results.append(("a card is a card", volume_kind(["RPI_EFI.fd", "start4.elf"]) == "card"))
    results.append(("macOS's litter is empty", volume_kind([".fseventsd", "._x", ".Spotlight-V100"]) == "empty"))
    results.append(("a stick is ours", volume_kind([".ouroboros-stick", "bin"]) == "ours"))
    results.append(("anything else is not", volume_kind(["photos"]) is None))
    homes = homes_from_passwd("root:0:0:/\nann:1002:1002:/Users/ann:00ab:11cd\nbad:z:1:/Users/bad\n")
    results.append(("homes come from passwd", homes == {"ann": (1002, 1002)}))
    new = {"etc/shadow": {"kind": "f", "mode": 0o100600, "uid": 501, "gid": 20}}
    results.append(("a FAT file takes the image's mode", fat_metadata("etc/shadow", "f", new, homes) == (0o100600, 0, 0)))
    results.append(("a home's file takes its owner", fat_metadata("Users/ann/x", "f", new, homes) == (0o100644, 1002, 1002)))
    results.append(("anything else is root's", fat_metadata("Users/zed/x", "f", new, homes) == (0o100644, 0, 0)))

    def entry(kind, mode=0o644, src=None):
        return {"kind": kind, "mode": (0o040000 if kind == "d" else 0o100000) | mode, "uid": 0, "gid": 0, "src": src}

    new = {"etc": entry("d"), "etc/passwd": entry("f"), "etc/new": entry("f"),
           "Users": entry("d"), "Users/ann": entry("d"), "Users/ann/a": entry("f"), "Users/ann/sub": entry("d"),
           "Users/ann/sub/b": entry("f")}
    old = {"etc": entry("d"), "etc/passwd": entry("f", src="/h/p"), "Users": entry("d"),
           "Users/ann": entry("f", src="/h/ann")}
    cmds = plan_merge(old, new)
    results.append(("an old file replaces the new one", 'rm "/etc/passwd"' in cmds
                    and cmds.index('rm "/etc/passwd"') < cmds.index('write "/h/p" "/etc/passwd"')))
    results.append(("what only the new image has stays", not any("/etc/new" in c for c in cmds)))
    order = [c for c in cmds if "Users/ann" in c]
    results.append(("a directory replaced by a file is emptied deepest first",
                    order[:4] == ['rm "/Users/ann/sub/b"', 'rmdir "/Users/ann/sub"', 'rm "/Users/ann/a"', 'rmdir "/Users/ann"']
                    or order[:4] == ['rm "/Users/ann/sub/b"', 'rm "/Users/ann/a"', 'rmdir "/Users/ann/sub"', 'rmdir "/Users/ann"']))
    results.append(("then the file is written and owned", 'write "/h/ann" "/Users/ann"' in cmds
                    and cmds.index('rmdir "/Users/ann"') < cmds.index('write "/h/ann" "/Users/ann"')))
    old2 = {"Users": entry("d"), "Users/bob": entry("d"), "Users/bob/f": entry("f", src="/h/f")}
    cmds2 = plan_merge(old2, {"Users": entry("d")})
    results.append(("a missing directory is made before its file",
                    cmds2.index('mkdir "/Users/bob"') < cmds2.index('write "/h/f" "/Users/bob/f"')))
    home_new = {"Users": entry("d"), "Users/ann": entry("d"), "Users/ann/gone": entry("f"),
                "Users/ann/kept": entry("f"), "Users/zed": entry("d")}
    home_old = {"Users": entry("d"), "Users/ann": entry("d"), "Users/ann/kept": entry("f", src="/h/k")}
    cmds3 = plan_merge(home_old, home_new, whole={"Users/ann"})
    results.append(("a home is replaced whole: what the stick deleted stays deleted",
                    'rm "/Users/ann/gone"' in cmds3 and 'rmdir "/Users/ann"' in cmds3
                    and cmds3.index('rmdir "/Users/ann"') < cmds3.index('mkdir "/Users/ann"')
                    < cmds3.index('write "/h/k" "/Users/ann/kept"')))
    results.append(("a home only the image has stays", not any("zed" in c for c in cmds3)))
    results.append(("without `whole`, the image's file in a home stays",
                    not any("gone" in c for c in plan_merge(home_old, home_new))))
    try:
        quote('a"b')
        results.append(("a quote in a name is refused", False))
    except Refused:
        results.append(("a quote in a name is refused", True))
    results.append(("a counter record reads", bootid_value(b"00000000000000000042\n") == 42))
    results.append(("anything else is no counter", bootid_value(b"42\n") is None and bootid_value(None) is None))

    # A FAT32 volume of one cluster per directory: the boot counter found by
    # its path, and a path that is not there not found.
    bps, spc, reserved, fat_size = 512, 1, 32, 8
    fs = bytearray((reserved + fat_size + 8) * bps)
    fs[11:13], fs[13], fs[14:16], fs[16] = struct.pack("<H", bps), spc, struct.pack("<H", reserved), 1
    struct.pack_into("<I", fs, 36, fat_size)
    struct.pack_into("<I", fs, 44, 2)
    fs[510:512] = b"\x55\xaa"
    data = (reserved + fat_size) * bps
    for c in (2, 3, 4, 5):
        struct.pack_into("<I", fs, reserved * bps + c * 4, 0x0FFFFFFF)

    def dirent(cluster, slot, name, attr, first, size=0):
        off = data + (cluster - 2) * bps + slot * 32
        fs[off:off + 11] = name
        fs[off + 11] = attr
        struct.pack_into("<H", fs, off + 20, first >> 16)
        struct.pack_into("<H", fs, off + 26, first & 0xFFFF)
        struct.pack_into("<I", fs, off + 28, size)

    dirent(2, 0, b"\x41b\x00o\x00o\x00t\x00i\x00", 0x0F, 0)  # an LFN entry, skipped
    dirent(2, 1, b"EFI        ", 0x10, 3)
    dirent(3, 0, b"ORBS       ", 0x10, 4)
    dirent(4, 0, b"\xe5OOTID  TXT", 0x20, 9, 21)  # deleted, skipped
    dirent(4, 1, b"BOOTID  TXT", 0x20, 5, 21)
    found = fat32_find(lambda o, n: bytes(fs[o:o + n]), BOOTID_PATH)
    results.append(("the boot counter is found by its path", found == (data + 3 * bps, 21)))
    results.append(("a path not there is not found", fat32_find(lambda o, n: bytes(fs[o:o + n]), ("EFI", "X.TXT")) is None))

    failed = 0
    for name, ok in results:
        print(f"{'ok  ' if ok else 'FAIL'} bootstick: {name}")
        failed += 0 if ok else 1
    return 1 if failed else 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if len(argv) != 1:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    try:
        write(argv[0])
    except Refused as e:
        say(str(e))
        return 1
    except subprocess.CalledProcessError as e:
        say(f"{e.cmd[0]} failed: {e}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

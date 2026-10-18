#!/usr/bin/env python3
"""A directory grown past its first cluster lists and finds every entry.

    python3 scripts/test-grow-dir.py    (or `make test-grow-dir`, which builds the image)

When a directory's clusters are full, fsd links another to its chain
(`grow_dir`) and writes the new entry there. FAT32 has no size for a
directory: its chain is its extent. exFAT has one, the DataLength in the
directory's stream extension, and a reader stops there (fsd's own walk_dir
does, and so does every other driver), so a grown directory whose DataLength
stays at its first cluster has entries no reader sees.

The image is assembled here from the same build/exfatpart.img and
build/esp.img as build/espexfat.img. One boot runs /bin/DIRPROBE
(programs/shellutils/dirprobe) with COUNT files:

- on the exFAT root, 4 KiB clusters: 128 entries a cluster, three to each
  file's entry set, so COUNT files grow the directory twice;
- on the FAT32 ESP at /mnt/f, 512-byte clusters: the control, a FAT32
  directory grown six times.

The probe makes the files, finds each by its path, lists the directory and
touches every file again (a no-op where fsd finds the file, a second entry
where it does not). After the boot the host reads each directory off the
disk and checks that the probe's directory grew (the check that the rig
tested anything), that every file's entry is there once, and on exFAT that
the directory's DataLength and ValidDataLength cover its whole chain; then
it runs fsck_exfat -n and fsck_msdos -n on the two partitions, the foreign
observer. KEEP_IMAGE=1 keeps build/test-grow-dir.img for a look. One boot,
about a minute.
"""
import importlib.util
import os
import re
import struct
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import mbrdisk  # noqa: E402


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
# The host's reader of a FAT32 or exFAT partition (chains, listings, the bitmap).
Volume = load("test_stale_cursor", "test-stale-cursor.py").Volume

BUILD = os.path.join(ROOT, "build")
IMAGE = os.path.join(BUILD, "test-grow-dir.img")
PART = os.path.join(BUILD, "test-grow-dir-x.part")
W_BYTES = bytes(0x20 + (i * 7) % 0x5F for i in range(4096))  # every byte's high bit clear
COUNT = 100
NAMES = [f"F{i:03d}" for i in range(COUNT)]


def mac_dir():
    """PART: a copy of build/exfatpart.img with an empty directory MAC and a
    file W after it, both written by macOS, which makes a directory one
    contiguous (NoFatChain) cluster, as it made /SUB, /Users and /etc."""
    with open(os.path.join(BUILD, "exfatpart.img"), "rb") as src, open(PART, "wb") as dst:
        dst.write(src.read())
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", PART],
                         capture_output=True, text=True)
    if att.returncode != 0:
        sys.exit(f"test-grow-dir: hdiutil attach failed: {att.stderr.strip()}")
    dev = att.stdout.split()[0]
    try:
        if subprocess.run(["diskutil", "mount", dev], capture_output=True).returncode != 0:
            sys.exit(f"test-grow-dir: diskutil mount {dev} failed")
        info = subprocess.run(["diskutil", "info", dev], capture_output=True, text=True).stdout
        mp = re.search(r"Mount Point:\s*(\S.*)$", info, re.M).group(1)
        try:
            os.mkdir(os.path.join(mp, "MAC"))
            with open(os.path.join(mp, "W"), "wb") as fh:
                fh.write(W_BYTES)
            subprocess.run(["find", mp, "-name", "._*", "-delete"], capture_output=True)
            for junk in (".fseventsd", ".Trashes", ".Spotlight-V100", "MAC/.DS_Store", ".DS_Store"):
                subprocess.run(["rm", "-rf", os.path.join(mp, junk)], capture_output=True)
        finally:
            subprocess.run(["diskutil", "unmount", dev], capture_output=True)
    finally:
        subprocess.run(["hdiutil", "detach", dev], capture_output=True)


def exfat_dir(vol, name):
    """The root's entry `name` on exFAT: (first cluster, DataLength,
    ValidDataLength, chained), or None."""
    d = vol.data(vol.chain(vol.root))
    i = 0
    while i < len(d) and d[i]:
        count = d[i + 1] + 1 if d[i] == 0x85 else 1
        if d[i] == 0x85:
            stream = d[i + 32:i + 64]
            got = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            if got.decode("utf-16le")[:stream[3]].upper() == name.upper():
                return (struct.unpack_from("<I", stream, 20)[0], struct.unpack_from("<Q", stream, 24)[0],
                        struct.unpack_from("<Q", stream, 8)[0], not stream[1] & 0x02)
        i += 32 * count
    return None


def exfat_names(d):
    """Every live file name in the entry sets of directory bytes `d`, in
    order, a name twice if two sets carry it; a set cut off by the end of
    `d` is not read."""
    out = []
    i = 0
    while i + 32 <= len(d) and d[i]:
        count = d[i + 1] + 1 if d[i] == 0x85 else 1
        if i + 32 * count > len(d):
            break
        if d[i] == 0x85:
            stream = d[i + 32:i + 64]
            got = b"".join(d[i + 32 * k + 2:i + 32 * k + 32] for k in range(2, count))
            out.append(got.decode("utf-16le")[:stream[3]])
        i += 32 * count
    return out


def fat32_names(d):
    """Every live short name in FAT32 directory bytes `d`, `.` and `..` left out."""
    out = []
    for j in range(0, len(d), 32):
        e = d[j:j + 32]
        if e[0] == 0:
            break
        if e[0] != 0xE5 and e[11] != 0x0F and e[0] != ord("."):
            base, ext = e[:8].decode("latin1").rstrip(), e[8:11].decode("latin1").rstrip()
            out.append(base + ("." + ext if ext else ""))
    return out


def names_check(where, names):
    """The listing on the disk holds every file once and nothing else."""
    dup = sorted({n for n in names if names.count(n) > 1})
    missing = [n for n in NAMES if n not in names]
    extra = sorted(set(names) - set(NAMES))
    for what, which in (("missing", missing), ("twice", dup), ("not the probe's", extra)):
        if which:
            print(f"     {where}: on the disk, {what}: {which[:8]}{' ...' if len(which) > 8 else ''}")
    return (f"{where}: on the disk the directory holds each of the {COUNT} files once", not (dup or missing or extra))


def mac_checks(exf, before):
    """MAC grown and listed as /grow is, and W's bytes as macOS wrote them."""
    checks = []
    first, size, valid, chained = exfat_dir(exf, "MAC")
    chain = exf.chain(first) if chained else [first]
    whole = len(chain) * exf.cluster
    print(f"     exfat /MAC: first cluster {first}, {'chained' if chained else 'NoFatChain'}, "
          f"chain of {len(chain)} clusters, DataLength {size}, ValidDataLength {valid}")
    checks.append((f"exfat /MAC: grew past its first cluster ({COUNT} files), FAT-chained", chained and len(chain) >= 3))
    checks.append(("exfat /MAC: DataLength and ValidDataLength are its whole chain", size == whole and valid == whole))
    checks.append(names_check("exfat /MAC", exfat_names(exf.data(chain)[:size])))
    w = exf.listing(exf.root).get("W")
    checks.append(("exfat /W: macOS's bytes unchanged, in the cluster after MAC's",
                   w is not None and w[0] == before["W"][0] and exf.data([w[0]]) == W_BYTES))
    return checks


def disk_checks(vols, before):
    exf, esp = vols
    checks = mac_checks(exf, before)
    found = exfat_dir(exf, "grow")
    if not found:
        return [("exfat root: /grow is on the disk", False)]
    first, size, valid, chained = found
    chain = exf.chain(first) if chained else [first]
    whole = len(chain) * exf.cluster
    print(f"     exfat root: /grow: chain of {len(chain)} clusters ({whole} bytes), "
          f"DataLength {size}, ValidDataLength {valid}")
    checks.append((f"exfat root: /grow grew past its first cluster ({COUNT} files)", len(chain) >= 3))
    checks.append(("exfat root: /grow's DataLength and ValidDataLength are its whole chain",
                   size == whole and valid == whole))
    # Read as a driver reads it: no further than DataLength.
    checks.append(names_check("exfat root", exfat_names(exf.data(chain)[:size])))

    names = {k.upper(): v for k, v in esp.listing(esp.root).items()}
    if "GROW" not in names:
        return checks + [("fat32 /mnt/f: /GROW is on the disk", False)]
    chain = esp.chain(names["GROW"][0])
    print(f"     fat32 /mnt/f: /GROW: chain of {len(chain)} clusters")
    checks.append((f"fat32 /mnt/f: /GROW grew past its first cluster ({COUNT} files)", len(chain) >= 3))
    checks.append(names_check("fat32 /mnt/f", fat32_names(esp.data(chain))))
    return checks


def fsck():
    """fsck_exfat -n and fsck_msdos -n on the image's two partitions."""
    att = subprocess.run(["hdiutil", "attach", "-nomount", "-imagekey", "diskimage-class=CRawDiskImage", IMAGE],
                         capture_output=True, text=True)
    if att.returncode != 0:
        return [("the image attaches for fsck", False)], [att.stderr.strip()]
    dev = att.stdout.split()[0]
    checks, notes = [], []
    try:
        for part, tool, where in (("s1", "fsck_exfat", "exfat root"), ("s2", "fsck_msdos", "fat32 /mnt/f")):
            r = subprocess.run([tool, "-n", dev + part], capture_output=True, text=True)
            checks.append((f"{where}: {tool} -n finds nothing to repair", r.returncode == 0))
            if r.returncode != 0:
                notes += [f"{where}: {ln}" for ln in (r.stdout + r.stderr).strip().splitlines()]
    finally:
        subprocess.run(["hdiutil", "detach", dev], capture_output=True)
    return checks, notes


def main() -> int:
    try:
        mac_dir()
        parts = mbrdisk.assemble(IMAGE, [
            (0x07, mbrdisk.exfat_payload(PART), False),
            (0x0B, mbrdisk.esp_payload(os.path.join(BUILD, "esp.img")), True),
        ])
    finally:
        if os.path.exists(PART):
            os.remove(PART)
    try:
        exf = Volume(IMAGE, parts[0][0])
        before = exf.listing(exf.root)
        mac, w = before.get("MAC"), before.get("W")
        # The case is built only if MAC is one NoFatChain cluster with W's right after it.
        if not (mac and w and not mac[2] and mac[1] == exf.cluster and w[0] == mac[0] + 1):
            sys.exit(f"test-grow-dir: macOS did not lay out MAC and W as the rig needs: MAC {mac}, W {w}")
        runs = [("exfat root", "/grow"), ("exfat /MAC", "/MAC"), ("fat32 /mnt/f", "/mnt/f/grow")]
        steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 1 /mnt/f")]
        prev = "# "
        for _, d in runs:
            steps.append((prev, f"dirprobe {d} {COUNT}"))
            prev = r"dirprobe: (ok|FAIL)\r?\n"
        steps.append((prev, ""))
        guest = drive_qemu.Guest(IMAGE, label="test-grow-dir: ")
        try:
            driven = guest.run(steps)
            out = guest.transcript().replace("\r\n", "\n")
            faults = guest.aborts()
        finally:
            guest.stop()
        with open(os.path.join(BUILD, "test-grow-dir.txt"), "w") as fh:
            fh.write(out)

        checks = [("driven to the end", driven), ("no fault lines", faults == 0)]
        pieces = re.split(r"dirprobe (/\S+) \d+\n", out)
        by_dir = dict(zip(pieces[1::2], pieces[2::2]))
        for where, d in runs:
            lines = [ln.rstrip() for ln in re.findall(r"^dirprobe: .*$", by_dir.get(d, ""), re.M)]
            for ln in lines:
                print(f"     {where}: {ln}")
            for step in ("create", "find", "list", "retouch"):
                checks.append((f"{where}: ok {step}", f"dirprobe: ok {step}" in lines))
            checks.append((f"{where}: the probe's summary is ok", lines[-1:] == ["dirprobe: ok"]))
        checks += disk_checks([Volume(IMAGE, start) for start, _ in parts], before)
        more, notes = fsck()
        checks += more
        for n in notes:
            print(f"     {n}")
    finally:
        if os.environ.get("KEEP_IMAGE") != "1":
            os.remove(IMAGE)
    failed = 0
    for name, ok in checks:
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print("transcript: build/test-grow-dir.txt")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

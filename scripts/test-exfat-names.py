#!/usr/bin/env python3
"""exFAT names outside ASCII are names of their own, not one `?` rendering.

    python3 scripts/test-exfat-names.py     (or `make test-exfat-names`, which builds the image)

Until 2026-10-21 fsd rendered each exFAT name to ASCII with `?` for any other
UTF-16 unit, and looked names up by that rendering, so `café` and `cafè`
(which macOS writes) were both `caf?`, and the one lookup (`entry_in`) took
whichever set came first: a read, a write, a removal or a rename of one acted
on the other, and a create said the other existed. Now a lookup compares the
set's UTF-16 units, up-cased by the arm's one rule (ASCII), and a listing
renders them UTF-8, which a lookup takes back. One boot of a copy of
build/espexfat.img (exFAT root, partition 1). The host plants three files on
the root, each a contiguous cluster of its own: `café` (U+00E9) holding
`acute`, then `cafè` (U+00E8) holding `grave`, and `caf` with U+1F600 (a
surrogate pair, five units) holding `astral`; before the boot each set must
read back and fsck_exfat -n pass (the observer's own check). Then, typed as
UTF-8 on the serial line:

- `ls /` must list all three by their UTF-8 names;
- `cat /cafè` must print `grave`, the second set, not the first;
- `cat /CAF` + U+1F600 must print `astral` (ASCII case still folded beside a
  pair);
- `echo new > /cafë` (U+00EB) must make a new file, `cat` of it `new` and
  `cat /café` still `acute`, then `rm /cafë`;
- `mv /cafè /grave` and `ls /` must list `grave` and `café`, not `cafè`.

After the boot, on the host: `grave`'s set names `cafè`'s cluster, `café`'s
set and cluster are as planted (its bytes `acute`), no set is left named
`cafè` or `cafë`, and fsck_exfat -n passes. No fault lines in QEMU's trace.
Fails with main's exfat.rs before 2026-10-21 (`cat /cafè` prints `acute`, the
pair is not found, `echo` overwrites `café`, `mv` moves it). A name fsd makes
outside ASCII carries a NameHash up-cased by ASCII alone, which fsck_exfat
rejects ("Invalid file name"; the volume's up-case table is not read, a
roadmap item), so the rig removes `cafë` in the boot that makes it. About a
minute. Run it whenever exFAT's walk_dir name assembly, entry_in,
units_match, names_match, utf8_name or list_dir changes.
"""
import importlib.util
import os
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import exfatimg  # noqa: E402
import mbrdisk  # noqa: E402


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")
stale_cursor = load("test_stale_cursor", "test-stale-cursor.py")
Volume, set_checksum = stale_cursor.Volume, stale_cursor.set_checksum

IMAGE = os.path.join(ROOT, "build", "test-exfat-names.img")
ACUTE, GRAVE, UMLAUT = "café", "cafè", "cafë"
ASTRAL = "caf\U0001F600"
FILES = [(ACUTE, b"acute\r\n"), (GRAVE, b"grave\r\n"), (ASTRAL, b"astral\r\n")]


def name_set(name, first, size):
    """`name`'s entry set, a contiguous file of `size` bytes at `first`, its
    NameLength in UTF-16 units and its NameHash up-cased as the volume's
    table (macOS's) up-cases, which Python's upper() matches for these."""
    units = name.encode("utf-16le")
    count = len(units) // 2
    k = -(-count // 15)
    s = bytearray(32 * (2 + k))
    s[0], s[1] = 0x85, 1 + k
    struct.pack_into("<H", s, 4, 0x20)  # archive
    s[32], s[33], s[35] = 0xC0, 0x03, count  # AllocationPossible, NoFatChain
    h = 0
    for b in name.upper().encode("utf-16le"):
        h = (((h << 15) | (h >> 1)) + b) & 0xFFFF
    struct.pack_into("<H", s, 36, h)
    struct.pack_into("<Q", s, 40, size)
    struct.pack_into("<I", s, 52, first)
    struct.pack_into("<Q", s, 56, size)
    for e in range(k):
        chunk = units[30 * e:30 * e + 30]
        s[64 + 32 * e] = 0xC1
        s[64 + 32 * e + 2:64 + 32 * e + 2 + len(chunk)] = chunk
    struct.pack_into("<H", s, 2, set_checksum(s))
    return bytes(s)


def find(d, name):
    """The set named exactly `name` (no folding: the names differ only past
    ASCII), or None."""
    return next((s for s in exfatimg.sets(d) if s.name == name), None)


def first_cluster(s):
    return struct.unpack_from("<I", s.data, 52)[0]


def plant():
    shutil.copyfile(os.path.join(ROOT, "build", "espexfat.img"), IMAGE)
    with open(IMAGE, "rb") as fh:
        start = [p for p in mbrdisk.entries(fh.read(512)) if p[1] == 0x07][0][2]
    vol = Volume(IMAGE, start)
    chain = vol.chain(vol.root)
    d = bytearray(vol.data(chain))
    end = 0
    while end < len(d) and d[end]:
        end += 32
    free = [c for c in range(2, vol.count + 2) if not vol.used(c)][:len(FILES)]
    sets = b"".join(name_set(n, c, len(data)) for (n, data), c in zip(FILES, free))
    if end + len(sets) + 32 > len(d):
        sys.exit("test-exfat-names: no room for the planted sets in the exFAT root")
    d[end:end + len(sets)] = sets
    with open(IMAGE, "r+b") as fh:
        for k, c in enumerate(chain):
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(d[k * vol.cluster:(k + 1) * vol.cluster])
        for (_, data), c in zip(FILES, free):
            fh.seek(vol.heap + (vol.bitmap - 2) * vol.cluster + (c - 2) // 8)
            b = fh.read(1)[0]
            fh.seek(-1, 1)
            fh.write(bytes([b | 1 << (c - 2) % 8]))
            fh.seek(vol.heap + (c - 2) * vol.cluster)
            fh.write(data + bytes(vol.cluster - len(data)))
    return start, dict(zip((n for n, _ in FILES), free))


def fsck():
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_exfat")
    ok = (rc == 0 and "** Checking file system hierarchy." in lines
          and not exfatimg.complaints(lines) and not exfatimg.unexpected(lines))
    return ok, lines


def answer(out, cmd, last=False):
    """What the guest printed for `cmd` (its first run, or its `last`): the
    text after its echoed line, up to the next prompt."""
    marker = "# " + cmd + "\r"
    if marker not in out:
        return None
    after = out.rsplit(marker, 1)[1] if last else out.split(marker, 1)[1]
    return after.split("# ", 1)[0].strip()


def run():
    start, clusters = plant()
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    ok, lines = fsck()
    checks = [
        ("before: the three sets read back by their exact names, each on its own planted cluster",
         all(find(root, n) is not None and first_cluster(find(root, n)) == clusters[n] for n, _ in FILES)),
        ("before: fsck_exfat -n passes", ok),
    ]
    notes = [f"before: {ln}" for ln in lines] if not ok else []
    if not all(c for _, c in checks):
        return checks, notes

    upper_astral = "CAF\U0001F600"
    cmds = ["ls /", f"cat /{GRAVE}", f"cat /{upper_astral}", f"echo new > /{UMLAUT}",
            f"cat /{UMLAUT}", f"cat /{ACUTE}", f"rm /{UMLAUT}", f"mv /{GRAVE} /grave", "ls /"]
    steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in cmds] + [("# ", "")]
    guest = drive_qemu.Guest(IMAGE, label="test-exfat-names: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", "test-exfat-names.txt"), "w") as fh:
        fh.write(out)

    listing = (answer(out, "ls /") or "").split()
    last = (answer(out, "ls /", last=True) or "").split()
    vol = Volume(IMAGE, start)
    root = vol.data(vol.chain(vol.root))
    grave, acute = find(root, "grave"), find(root, ACUTE)
    ok, lines = fsck()
    if not ok:
        notes += [f"after: {ln}" for ln in lines]
    checks += [
        ("driven to the end, fsd mounted exFAT", driven and "fsd: exFAT mounted" in out),
        (f"ls / lists all three by their UTF-8 names ({[n for n in listing if n.startswith('caf')]})",
         all(n in listing for n, _ in FILES)),
        (f"cat /{GRAVE} prints grave, the second set ({answer(out, f'cat /{GRAVE}')!r})",
         answer(out, f"cat /{GRAVE}") == "grave"),
        (f"cat /{upper_astral} prints astral ({answer(out, f'cat /{upper_astral}')!r})",
         answer(out, f"cat /{upper_astral}") == "astral"),
        (f"echo new > /{UMLAUT} makes a file of its own ({answer(out, f'cat /{UMLAUT}')!r})",
         answer(out, f"cat /{UMLAUT}") == "new"),
        (f"cat /{ACUTE} after it still prints acute ({answer(out, f'cat /{ACUTE}')!r})",
         answer(out, f"cat /{ACUTE}") == "acute"),
        (f"ls / after the mv lists grave, {ACUTE} and {ASTRAL}, not {GRAVE}",
         "grave" in last and ACUTE in last and ASTRAL in last and GRAVE not in last),
        (f"grave's set names {GRAVE}'s cluster",
         grave is not None and first_cluster(grave) == clusters[GRAVE]),
        (f"{ACUTE}'s set and cluster as planted, holding acute",
         acute is not None and first_cluster(acute) == clusters[ACUTE]
         and vol.data([clusters[ACUTE]])[:7] == b"acute\r\n"),
        (f"no set is left named {GRAVE} or {UMLAUT}", find(root, GRAVE) is None and find(root, UMLAUT) is None),
        ("fsck_exfat -n passes after", ok),
        (f"no fault lines in QEMU's trace ({faults})", faults == 0),
    ]
    return checks, notes


def main():
    try:
        checks, notes = run()
    finally:
        if os.path.exists(IMAGE) and not os.environ.get("KEEP"):
            os.remove(IMAGE)
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    for n in notes:
        print("      " + n)
    print(f"test-exfat-names: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

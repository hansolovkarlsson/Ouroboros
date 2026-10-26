#!/usr/bin/env python3
"""FAT32 long names outside ASCII are names of their own, not one `?` rendering.

    python3 scripts/test-fat32-names.py     (or `make test-fat32-names`, which builds the image)

Until 2026-10-26 fsd read each FAT32 long name (LFN) to ASCII with `?` for
any other UTF-16 unit (`lfn_chars`), and looked names up by that rendering,
so `café` and `cafè` were both `caf?` and the first answered for both: the
collision exFAT had until 2026-10-21 (test-exfat-names.py). And fsd wrote a
long name's UTF-8 bytes a byte to a unit (`put_lfn_chars`), so a name it
made outside ASCII went to disk as other characters (`café` as five units,
`é`'s two bytes as two) and was found by no lookup of it. Now the walk keeps
the units and renders them as exFAT does (`utf16::utf8_name`: UTF-8, or
ASCII with `?` for each unit outside it when the name holds an unpaired
surrogate or its UTF-8 passes 255 bytes), a lookup compares that rendering
ignoring ASCII case, and a create writes the name's UTF-16 units.

One boot of a copy of build/esp.img (FAT32, partition 1). The host plants
five files on the root, each a long name (LFN entries, checksummed to a
short alias of its own) on a cluster of its own: `café` (U+00E9) holding
`acute`, `cafè` (U+00E8) holding `grave`, `caf` with U+1F600 (a surrogate
pair) holding `astral`, 100 of U+4E2D (300 bytes of UTF-8) holding `long`,
and `lone` with an unpaired surrogate holding `lone`; before the boot each
must read back by its exact name and fsck_msdos -n pass. Then, typed as
UTF-8 on the serial line:

- `ls /` lists the first three by their UTF-8 names, the overlong one as
  100 `?`s and the unpaired one as `lone?`, and `echo /*` the overlong one
  so too;
- `cat /cafè` prints `grave` (the second name, not the first);
- `cat /CAF` + U+1F600 prints `astral` (ASCII case folded beside a pair);
- `echo new > /cafë` (U+00EB) makes a file of its own, `cat` of it `new`,
  `cat /café` still `acute`, then `rm /cafë`;
- `mv /cafè /grave`, and `ls /` after lists `grave`, `café` and the astral
  name, not `cafè`;
- `cat` of the overlong name's and `lone?`'s listings print their bytes,
  and `touch /lone?` makes no second file;
- `cp /grave` to a name with a pair (U+1F601) makes it, and `cat` of it
  prints `grave`.

After the boot, on the host: `grave` names `cafè`'s cluster, `café`'s entry
and bytes are as planted, no long name `cafè` or `cafë` is left, the copy's
long name is exactly `new` + U+1F601 in five UTF-16 units (the write side),
one long name `lone` + a lone surrogate, and fsck_msdos -n passes. No fault
lines. Fails with main's fat32.rs before 2026-10-26 (no UTF-8 name found,
the listing `caf?`, the copy's name written as its UTF-8 bytes) and with
the read side alone fixed (the copy found by no lookup). About a minute.
Run it whenever FAT32's lfn_chars, put_lfn_chars, build_name_entries,
walk_dir's long-name assembly, insert_named_entry or names_match changes,
or utf16.rs does.
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
import fat32img  # noqa: E402
import mbrdisk  # noqa: E402


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = load("drive_qemu", "drive-qemu.py")

IMAGE = os.path.join(ROOT, "build", "test-fat32-names.img")
ACUTE, GRAVE, UMLAUT = "café", "cafè", "cafë"
ASTRAL = "caf\U0001F600"
OVERLONG = "中" * 100  # 300 bytes of UTF-8, past the 255 a listing shows
LONE = "lone\ud800"  # an unpaired surrogate
MADE = "new\U0001F601"  # a pair, made by the guest
FILES = [(ACUTE, b"acute\r\n"), (GRAVE, b"grave\r\n"), (ASTRAL, b"astral\r\n"),
         (OVERLONG, b"long\r\n"), (LONE, b"lone\r\n")]
SHOWN = {OVERLONG: "?" * 100, LONE: "lone?"}  # as a listing shows them
LFN_POS = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]


def units(name):
    raw = name.encode("utf-16le", "surrogatepass")
    return [struct.unpack_from("<H", raw, i)[0] for i in range(0, len(raw), 2)]


def checksum(short):
    s = 0
    for b in short:
        s = (((s & 1) << 7) + (s >> 1) + b) & 0xFF
    return s


def name_entries(name, short, cluster, size):
    """`name`'s LFN entries (highest sequence first) and its short entry
    `short` (11 bytes) on `cluster`, `size` bytes, as fsd's
    build_name_entries lays them out."""
    u = units(name)
    n = -(-len(u) // 13)
    out = b""
    for idx in range(n):
        seq = n - idx
        e = bytearray(32)
        e[0] = seq | (0x40 if idx == 0 else 0)
        e[11], e[13] = 0x0F, checksum(short)
        for k, p in enumerate(LFN_POS):
            ci = (seq - 1) * 13 + k
            v = u[ci] if ci < len(u) else (0 if ci == len(u) else 0xFFFF)
            struct.pack_into("<H", e, p, v)
        out += bytes(e)
    s = bytearray(32)
    s[:11], s[11] = short, 0x20
    struct.pack_into("<H", s, 20, cluster >> 16)
    struct.pack_into("<H", s, 26, cluster & 0xFFFF)
    struct.pack_into("<I", s, 28, size)
    return out + bytes(s)


def long_names(vol, first):
    """[(long name or None, short name, first cluster, size, LFN units)] of
    the directory at `first`, a run of LFN entries taken only when its
    checksum is its short entry's, as fsd takes it. A lone surrogate is read
    back as U+FFFD in the name; the units are as on disk."""
    out, pending, sums = [], {}, set()
    for c in vol.chain(first):
        vol.fh.seek(vol.lba(c) * fat32img.SECTOR)
        d = vol.fh.read(vol.cluster_bytes)
        for j in range(0, len(d), 32):
            e = d[j:j + 32]
            if e[0] == 0:
                return out
            if e[0] == 0xE5:
                pending, sums = {}, set()
                continue
            if e[11] == 0x0F:
                seq = e[0] & 0x1F
                pending[seq] = [struct.unpack_from("<H", e, p)[0] for p in LFN_POS]
                sums.add(e[13])
                continue
            if e[11] & 0x08:
                pending, sums = {}, set()
                continue
            u = []
            if pending and sums == {checksum(e[:11])}:
                for seq in sorted(pending):
                    u += pending[seq]
                if 0 in u:
                    u = u[:u.index(0)]
                u = [x for x in u if x != 0xFFFF]
            raw = b"".join(struct.pack("<H", x) for x in u)
            name = raw.decode("utf-16le", "replace") if u else None
            first_c = (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0]
            out.append((name, e[:11].decode("latin1"), first_c, struct.unpack_from("<I", e, 28)[0], u))
            pending, sums = {}, set()
    return out


def find(entries, name):
    """The entry whose long name is exactly `name` (a lone surrogate as
    U+FFFD), or None."""
    name = name.encode("utf-16le", "surrogatepass").decode("utf-16le", "replace")
    return next((e for e in entries if e[0] == name), None)


def by_short(entries, name):
    """The entry with no long name whose short name is `name` (an 8.3 name
    fsd made, case aside), or None."""
    base, _, ext = name.upper().partition(".")
    want = base.ljust(8) + ext.ljust(3)
    return next((e for e in entries if e[0] is None and e[1] == want), None)


def plant():
    shutil.copyfile(os.path.join(ROOT, "build", "esp.img"), IMAGE)
    with open(IMAGE, "rb") as fh:
        start = mbrdisk.entries(fh.read(512))[0][2]
    with open(IMAGE, "r+b") as fh:
        vol = fat32img.Fat32(fh, start)
        free = [c for c in range(2, vol.cluster_end) if vol.entry(c) == 0][:len(FILES)]
        planted = b"".join(name_entries(n, f"FNAME~{k + 1}".encode().ljust(8) + b"TXT", c, len(data))
                           for k, ((n, data), c) in enumerate(zip(FILES, free)))
        # The image's root is a cluster or two: zeroed free clusters are
        # linked after its last until the names fit, as a driver grows it.
        while True:
            chain = vol.chain(vol.root)
            d = bytearray(vol.read(chain, len(chain) * vol.cluster_bytes))
            end = 0
            while end < len(d) and d[end]:
                end += 32
            if end + len(planted) + 32 <= len(d):
                break
            grow = next(c for c in range(2, vol.cluster_end) if vol.entry(c) == 0 and c not in free)
            vol.set_entry(chain[-1], grow)
            vol.set_entry(grow, 0x0FFFFFFF)
            fh.seek(vol.lba(grow) * fat32img.SECTOR)
            fh.write(bytes(vol.cluster_bytes))
        d[end:end + len(planted)] = planted
        for k, c in enumerate(chain):
            fh.seek(vol.lba(c) * fat32img.SECTOR)
            fh.write(d[k * vol.cluster_bytes:(k + 1) * vol.cluster_bytes])
        for (_, data), c in zip(FILES, free):
            vol.set_entry(c, 0x0FFFFFFF)
            fh.seek(vol.lba(c) * fat32img.SECTOR)
            fh.write(data + bytes(vol.cluster_bytes - len(data)))
    return start, dict(zip((n for n, _ in FILES), free))


def fsck():
    with exfatimg.attached(IMAGE) as dev:
        rc, lines = exfatimg.fsck(dev + "s1", "fsck_msdos")
    return rc == 0 and not exfatimg.complaints(lines), lines


def answer(out, cmd, last=False):
    """What the guest printed for `cmd` (its first run, or its `last`): the
    text after its echoed line, up to the next prompt, without the kernel's
    own lines (a program's exit)."""
    marker = "# " + cmd + "\r"
    if marker not in out:
        return None
    after = out.rsplit(marker, 1)[1] if last else out.split(marker, 1)[1]
    lines = after.split("# ", 1)[0].splitlines()
    return "\n".join(ln for ln in lines if not ln.startswith("Ouroboros kernel:")).strip()


def entries_of(start):
    with open(IMAGE, "rb") as fh:
        vol = fat32img.Fat32(fh, start)
        return long_names(vol, vol.root), vol


def run():
    start, clusters = plant()
    entries, _ = entries_of(start)
    ok, lines = fsck()
    checks = [
        ("before: the five long names read back exactly, each on its own planted cluster",
         all(find(entries, n) is not None and find(entries, n)[2] == clusters[n] for n, _ in FILES)),
        ("before: fsck_msdos -n passes", ok),
    ]
    notes = [f"before: {ln}" for ln in lines] if not ok else []
    if not all(c for _, c in checks):
        return checks, notes

    upper_astral = "CAF\U0001F600"
    cmds = ["ls /", "echo /*", f"cat /{GRAVE}", f"cat /{upper_astral}", f"echo new > /{UMLAUT}",
            f"cat /{UMLAUT}", f"cat /{ACUTE}", f"rm /{UMLAUT}", f"mv /{GRAVE} /grave",
            f"cat /{SHOWN[OVERLONG]}", f"cat /{SHOWN[LONE]}", f"touch /{SHOWN[LONE]}",
            f"cp /grave /{MADE}", f"cat /{MADE}", "ls /"]
    steps = [("login:", "root"), ("assword", "root")] + [("# ", c) for c in cmds] + [("# ", "")]
    guest = drive_qemu.Guest(IMAGE, label="test-fat32-names: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", "test-fat32-names.txt"), "w") as fh:
        fh.write(out)
    if drive_qemu.boot_stalled("test-fat32-names", "boot", out, faults):
        return checks, notes + ["the boot stalled in the firmware"]

    listing = (answer(out, "ls /") or "").split()
    last = (answer(out, "ls /", last=True) or "").split()
    entries, _ = entries_of(start)
    grave, acute, made = by_short(entries, "grave"), find(entries, ACUTE), find(entries, MADE)
    with open(IMAGE, "rb") as fh:
        vol = fat32img.Fat32(fh, start)
        acute_bytes = vol.read([clusters[ACUTE]], 7)
    ok, lines = fsck()
    if not ok:
        notes += [f"after: {ln}" for ln in lines]
    echo = (answer(out, "echo /*") or "").split()
    checks += [
        ("driven to the end, fsd mounted FAT32", driven and "fsd: FAT32 mounted" in out),
        (f"ls / lists the three by their UTF-8 names ({[n for n in listing if n.lower().startswith('caf')]})",
         all(n in listing for n, _ in FILES[:3])),
        (f"ls / lists the unpaired surrogate's name as {SHOWN[LONE]} and the overlong one as 100 ?s, "
         "before and after", SHOWN[LONE] in listing and SHOWN[OVERLONG] in listing and SHOWN[OVERLONG] in last),
        ("echo /* gives the overlong name as 100 ?s, and the names after it",
         f"/{SHOWN[OVERLONG]}" in echo and "/EFI" in echo),
        (f"cat /{GRAVE} prints grave, the second name ({answer(out, f'cat /{GRAVE}')!r})",
         answer(out, f"cat /{GRAVE}") == "grave"),
        (f"cat /{upper_astral} prints astral ({answer(out, f'cat /{upper_astral}')!r})",
         answer(out, f"cat /{upper_astral}") == "astral"),
        (f"echo new > /{UMLAUT} makes a file of its own ({answer(out, f'cat /{UMLAUT}')!r})",
         answer(out, f"cat /{UMLAUT}") == "new"),
        (f"cat /{ACUTE} after it still prints acute ({answer(out, f'cat /{ACUTE}')!r})",
         answer(out, f"cat /{ACUTE}") == "acute"),
        (f"cat of the overlong name's listing prints long ({answer(out, f'cat /{SHOWN[OVERLONG]}')!r})",
         answer(out, f"cat /{SHOWN[OVERLONG]}") == "long"),
        (f"cat /{SHOWN[LONE]} prints lone ({answer(out, f'cat /{SHOWN[LONE]}')!r})",
         answer(out, f"cat /{SHOWN[LONE]}") == "lone"),
        (f"cp /grave /{MADE} makes a name with a pair, cat of it grave ({answer(out, f'cat /{MADE}')!r})",
         answer(out, f"cat /{MADE}") == "grave"),
        (f"ls / after the mv lists grave, {ACUTE} and {ASTRAL}, not {GRAVE}",
         "grave" in last and ACUTE in last and ASTRAL in last and GRAVE not in last),
        (f"grave names {GRAVE}'s cluster", grave is not None and grave[2] == clusters[GRAVE]),
        (f"{ACUTE}'s entry and cluster as planted, holding acute ({acute_bytes!r})",
         acute is not None and acute[2] == clusters[ACUTE] and acute_bytes == b"acute\r\n"),
        (f"no long name {GRAVE} or {UMLAUT} is left", find(entries, GRAVE) is None and find(entries, UMLAUT) is None),
        (f"the copy's long name on disk is {MADE!r} in five UTF-16 units ({made[4] if made else None})",
         made is not None and made[4] == units(MADE)),
        (f"touch /{SHOWN[LONE]} made no second file (one long name lone with a lone surrogate)",
         sum(1 for e in entries if e[0] == "lone�") == 1),
        ("fsck_msdos -n passes after", ok),
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
    print(f"test-fat32-names: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""FAT32 never reaches past its volume, by a link or by a first cluster.

    python3 scripts/test-fat-link.py     (or `make test-fat-link`, which builds the images)

Until 2026-10-19 FAT32 took any cluster number it was given to the disk: a
link below end of chain was followed, and a directory entry's first cluster
was used as it stood. A damaged one, a cluster past the volume's last, sent a
read or a write to the sectors after the volume, and `rm`'s free zeroed its
FAT entry far past the FAT, in every copy. Now `cluster_to_lba`, `fat_entry`
and `write_fat_entry` refuse a cluster outside `2..cluster_end`, a link to one
ends the chain, and `free_chain` frees nothing for a first cluster outside it.

The rig builds a disk of its own, so a stray write has somewhere to land that
the host can see: the ESP of build/esp.img as partition 1, a WITNESS
partition of 0xFF bytes right after it, and build/ext2part.img as the root
(which fsd mounts by its type, 0x83). On the ESP the host puts eight files and
plants damage in each, in the FAT (both copies) or in its directory entry:

- RD, two clusters: its first cluster links to a cluster in the witness.
  `cat /mnt/f/RD | wc` must count one cluster (a chain shorter than the size
  reads as the file's end, as it always has), not the witness's bytes too;
- APP, one cluster: links to another cluster in the witness. `echo tail >>
  /mnt/f/APP` must extend the chain inside the volume and read back;
- RMF: links to a cluster whose FAT entry, in each copy, lies in the witness
  (0xFF bytes, end of chain). `rm` must free its cluster and nothing past it;
- BADM: links to the bad-cluster mark 0x0FFFFFF7, whose FAT entry lies about
  1 GiB on, past the disk. `rm` must succeed and free its cluster;
- ONE: links to the reserved cluster 1. `rm` must free its cluster and leave
  FAT entry 1 (the volume's clean-shutdown bits) as it was in both copies;
- HEADR, one cluster: its directory entry names a first cluster in the
  witness. `cat /mnt/f/HEADR | wc` must count nothing, the read refused;
- HEADW, 100 bytes: the same, another witness cluster. `echo tail >>` must
  write nothing there and leave its size 100;
- HEADM: its first cluster's FAT entry lies in the witness. `rm` must remove
  the entry and write no FAT sector.

The commands run after `mount 0 /mnt/f`, and `ls /mnt/f` must list the files
(a mount that failed any way at all fails there, not later). After the boot
the witness must be every byte 0xFF, and no fault line in QEMU's trace. Before
the boot the host reads back what it planted and checks each planted cluster
lands where this says (the observer's own checks). Fails with the bounds
removed (the main tree before 2026-10-19): the witness written by APP's
append, RMF's and HEADM's frees in both copies, and HEADW's append; RD read as
two clusters and HEADR as one; BADM's rm an I/O error; FAT entry 1 zeroed.
One boot, about a minute. Run it whenever FAT32's next_cluster, fat_entry,
write_fat_entry, cluster_to_lba, free_chain, write_at, read path or
cluster_end changes.
"""
import importlib.util
import os
import re
import shutil
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import exfatimg  # noqa: E402  (its `place_mounted`)
import fat32img  # noqa: E402
import mbrdisk  # noqa: E402

_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

SECTOR = 512
WITNESS_TYPE = 0xDA  # "non-filesystem data": nothing mounts it
WITNESS_MIN = 8 * 1024 * 1024
ESP_COPY = os.path.join(ROOT, "build", "test-fat-link-esp.img")
DISK = os.path.join(ROOT, "build", "test-fat-link.img")
# Sectors into the witness where each planted cluster's data or FAT entry
# lies, apart so a written sector names the case that wrote it.
AT_APP, AT_RD, AT_HEADR, AT_HEADW, AT_RMF, AT_HEADM = 64, 128, 256, 512, 2048, 2056
COMMANDS = [
    "ls /mnt/f",
    "cat /mnt/f/RD | wc",
    "echo tail >> /mnt/f/APP",
    "rm /mnt/f/RMF",
    "rm /mnt/f/BADM",
    "rm /mnt/f/ONE",
    "cat /mnt/f/HEADR | wc",
    "echo tail >> /mnt/f/HEADW",
    "rm /mnt/f/HEADM",
]
EXIT = re.compile(r"^Ouroboros kernel: task \d+ exited \(code (\d+)\)\r?$", re.M)


def build():
    """Plant the files and the damage; return the plan and the observer's checks."""
    try:
        shutil.copyfile(os.path.join(ROOT, "build", "esp.img"), ESP_COPY)
        with open(ESP_COPY, "rb") as fh:
            cb = fat32img.Fat32(fh, mbrdisk.entries(fh.read(SECTOR))[0][2]).cluster_bytes
        one = {n: n[0].encode() * cb for n in ("APP", "RMF", "BADM", "ONE", "HEADR", "HEADM")}
        exfatimg.place_mounted(ESP_COPY, {**one, "RD": b"R" * (2 * cb), "HEADW": b"W" * 100})
        esp = mbrdisk.esp_payload(ESP_COPY)
    finally:
        if os.path.exists(ESP_COPY):
            os.remove(ESP_COPY)
    root = mbrdisk.ext2_payload(os.path.join(ROOT, "build", "ext2part.img"))
    # The witness holds the planted FAT sectors in every copy, so it is at
    # least as long as the FATs past a margin.
    fatsz, nfats = struct.unpack_from("<I", esp, 36)[0], esp[16]
    witness = b"\xff" * max(WITNESS_MIN, (nfats * fatsz + 4096) * SECTOR)
    layout = mbrdisk.assemble(DISK, [(mbrdisk.ESP_TYPE, esp, True), (WITNESS_TYPE, witness, False),
                                     (mbrdisk.EXT2_TYPE, root, False)])
    (esp_start, _), (wit_start, wit_sectors), _ = layout
    plan = {"witness": (wit_start, wit_sectors), "esp_start": esp_start, "cb": cb}
    checks = []
    with open(DISK, "r+b") as fh:
        v = fat32img.Fat32(fh, esp_start)
        files = {name: (first, size) for _, name, first, size in v.listing(v.root)}
        names = ("RD", "APP", "RMF", "BADM", "ONE", "HEADR", "HEADW", "HEADM")
        checks.append(("plant: the eight files on the ESP", all(n in files for n in names)))
        checks.append(("plant: the ESP has two FAT copies", v.nfats == 2))
        if not all(ok for _, ok in checks):
            return plan, checks
        wit_end = wit_start + wit_sectors

        def data_at(sector):  # a cluster whose data starts `sector` into the witness
            return 2 + -(-(wit_start + sector - v.data) // v.spc)

        def fat_at(sector):  # a cluster whose entry, copy 0, is `sector` into the witness
            return (wit_start + sector - v.fat) * (SECTOR // 4)

        def in_wit(c):  # all of cluster c's data in the witness
            return wit_start <= v.lba(c) and v.lba(c) + v.spc <= wit_end

        def fat_in_wit(c):
            return all(wit_start <= v.fat_lba(c, k) < wit_end for k in range(v.nfats))

        first = {n: files[n][0] for n in names}
        links = {"RD": data_at(AT_RD), "APP": data_at(AT_APP), "RMF": fat_at(AT_RMF),
                 "BADM": fat32img.BAD_CLUSTER, "ONE": 1}
        heads = {"HEADR": data_at(AT_HEADR), "HEADW": data_at(AT_HEADW), "HEADM": fat_at(AT_HEADM)}
        plan["fat1"] = [v.entry(1, k) for k in range(v.nfats)]
        for n, c in links.items():
            v.set_entry(first[n], c)
        for n, c in heads.items():
            v.set_first(v.locate(v.root, n), c)
        plan.update(first=first, links=links, heads=heads)
        after = {name: f for _, name, f, _ in v.listing(v.root)}
        checks += [
            ("plant: RD and APP link to clusters wholly in the witness",
             all(v.entry(first[n]) == links[n] and not v.in_heap(links[n]) and in_wit(links[n])
                 for n in ("RD", "APP"))),
            ("plant: RMF links to a cluster whose entry is in the witness in both copies",
             v.entry(first["RMF"]) == links["RMF"] and not v.in_heap(links["RMF"])
             and links["RMF"] < fat32img.BAD_CLUSTER and fat_in_wit(links["RMF"])),
            ("plant: BADM links to the bad-cluster mark, ONE to cluster 1",
             v.entry(first["BADM"]) == fat32img.BAD_CLUSTER and v.entry(first["ONE"]) == 1),
            ("plant: HEADR and HEADW name first clusters wholly in the witness",
             all(after[n] == heads[n] and not v.in_heap(heads[n]) and in_wit(heads[n]) for n in ("HEADR", "HEADW"))),
            ("plant: HEADM names a first cluster whose entry is in the witness in both copies",
             after["HEADM"] == heads["HEADM"] and not v.in_heap(heads["HEADM"]) and fat_in_wit(heads["HEADM"])),
            ("plant: no two planted sectors share a witness sector",
             len({AT_APP, AT_RD, AT_HEADR, AT_HEADW, AT_RMF, AT_HEADM, AT_RMF + v.fatsz, AT_HEADM + v.fatsz}) == 8
             and v.spc <= 8),
        ]
    return plan, checks


def boot():
    steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 0 /mnt/f")]
    for i, cmd in enumerate(COMMANDS):
        steps += [("# ", f"echo ==m{i}=="), ("# ", cmd)]
    steps += [("# ", "echo ==end=="), ("==end==", "")]
    guest = drive_qemu.Guest(DISK, label="test-fat-link: ")
    try:
        driven = guest.run(steps)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(os.path.join(ROOT, "build", "test-fat-link.txt"), "w") as fh:
        fh.write(out)
    return driven, out, faults


def outputs(out):
    """{command: its output}, from its marker's output line to the next
    marker's typed line, the command's own echoed line dropped."""
    got = {}
    for i, cmd in enumerate(COMMANDS):
        m = re.search(rf"^==m{i}==\r?$", out, re.M)
        if not m:
            continue
        rest = out[m.end():]
        stop = re.search(rf"^[^\n]*echo ==(m{i + 1}|end)==", rest, re.M)
        block = rest[:stop.start()] if stop else rest
        got[cmd] = block.split(cmd, 1)[1] if cmd in block else ""
    return got


def text(block):
    return EXIT.sub("", block).strip()


def wc_bytes(block):
    # The shell prefixes `pipe: ` to the last stage's output when an earlier
    # stage failed.
    m = re.findall(r"^(?:pipe: )?\s*(\d+)\s+(\d+)\s+(\d+)\s*$", text(block), re.M)
    return int(m[-1][2]) if m else None


def verdicts(plan, driven, out, faults):
    cb = plan["cb"]
    o = outputs(out)
    with open(DISK, "rb") as fh:
        wit_start, wit_sectors = plan["witness"]
        fh.seek(wit_start * SECTOR)
        witness = fh.read(wit_sectors * SECTOR)
        changed = [i // SECTOR for i in range(0, len(witness), SECTOR)
                   if witness[i:i + SECTOR] != b"\xff" * SECTOR]
        v = fat32img.Fat32(fh, plan["esp_start"])
        files = {name: (first, size) for _, name, first, size in v.listing(v.root)}
        fat1 = [v.entry(1, k) for k in range(v.nfats)]
        app = files.get("APP")
        app_chain = v.chain(app[0]) if app else []
        app_data = v.read(app_chain, app[1]) if app else b""
        first = plan["first"]
        freed = {n: v.entry(first[n]) == 0 for n in ("RMF", "BADM", "ONE")}
    listed = {w.rstrip("/").upper() for w in text(o.get("ls /mnt/f", "")).split()}
    return [
        ("driven to the end, every command's output found", driven and len(o) == len(COMMANDS)),
        ("fsd mounted ext2 as /", re.search(r"^fsd: ext2 mounted", out, re.M) is not None),
        ("ls /mnt/f lists the planted files (the ESP mounted)",
         {"RD", "APP", "RMF", "BADM", "ONE", "HEADR", "HEADW", "HEADM"} <= listed),
        (f"RD read as one cluster ({cb} bytes), got {wc_bytes(o.get('cat /mnt/f/RD | wc', ''))}",
         wc_bytes(o.get("cat /mnt/f/RD | wc", "")) == cb),
        (f"APP's chain its cluster and one of the volume's: {app_chain}",
         len(app_chain) == 2 and app_chain[0] == first["APP"] and v.in_heap(app_chain[1])),
        ("APP reads back as its As then tail", app_data[:cb] == b"A" * cb and app_data[cb:].startswith(b"tail")),
        ("RMF removed, its cluster free", "RMF" not in files and freed["RMF"]),
        ("BADM removed, its cluster free", "BADM" not in files and freed["BADM"]),
        ("ONE removed, its cluster free", "ONE" not in files and freed["ONE"]),
        (f"FAT entry 1 as it was in both copies ({fat1} vs {plan['fat1']})", fat1 == plan["fat1"]),
        (f"HEADR's read refused, nothing counted (got {wc_bytes(o.get('cat /mnt/f/HEADR | wc', ''))})",
         wc_bytes(o.get("cat /mnt/f/HEADR | wc", "")) == 0
         and "cat: device I/O error" in o.get("cat /mnt/f/HEADR | wc", "")),
        ("HEADW's append refused, its size still 100",
         files.get("HEADW", (0, None))[1] == 100 and text(o.get("echo tail >> /mnt/f/HEADW", "x")) != ""),
        ("HEADM removed", "HEADM" not in files),
        (f"the witness unchanged (changed sectors: {changed[:8]}{'...' if len(changed) > 8 else ''})",
         changed == []),
        ("no fault lines", faults == 0),
    ]


def main():
    plan, checks = build()
    failed = 0
    for label, ok in checks:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    if failed:
        print(f"test-fat-link: {failed} check(s) FAILED before the boot")
        if os.path.exists(DISK):
            os.remove(DISK)
        return 1
    try:
        driven, out, faults = boot()
        results = verdicts(plan, driven, out, faults)
    finally:
        if os.path.exists(DISK) and not os.environ.get("KEEP"):
            os.remove(DISK)
    for label, ok in results:
        print(("PASS  " if ok else "FAIL  ") + label)
        failed += not ok
    print(f"test-fat-link: {'all checks passed' if failed == 0 else f'{failed} check(s) FAILED'}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

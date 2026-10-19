#!/usr/bin/env python3
"""A FAT32 chain ends at a link outside the volume's clusters.

    python3 scripts/test-fat-link.py     (or `make test-fat-link`, which builds the images)

Until 2026-10-19 FAT32's `next_cluster` followed any link below end of
chain, so a damaged one, a cluster number past the volume's last, sent a walk
past the volume: a write to sectors beyond its end, a read of them, and `rm`'s
free reading and zeroing that cluster's FAT entry far past the FAT, in every
copy. exFAT's walks end there since 2026-10-19 (#320), and FAT32's now do.

The rig builds a disk of its own, so a stray write has somewhere to land that
the host can see: the ESP of build/esp.img as partition 1, a WITNESS
partition filled with 0xFF bytes right after it, and build/ext2part.img as the
root (which fsd mounts by its type, 0x83). On the ESP the host puts three
files, then plants in both FAT copies a link out of the volume on each
(clusters numbered past the volume's end, which no FAT32 driver may follow):

- RD, two clusters of `R`: its first cluster links to a cluster that lies in
  the witness. `cat /mnt/f/RD | wc` must count ONE cluster of bytes (the
  chain ends there, and FAT32's read takes a chain shorter than the size as
  the file's end, as it always has), not two (the witness's 0xFF bytes
  read as the file's second cluster);
- APP, one cluster of `A`: its cluster links to another cluster in the
  witness. `echo tail >> /mnt/f/APP` must extend the chain with a cluster of
  the volume's own, and the file must read back as the `A`s then `tail`;
- RMF, one cluster: its cluster links to a cluster whose FAT entry, in each
  FAT copy, lies in the witness (0xFF bytes: end of chain). `rm /mnt/f/RMF`
  must free its cluster and nothing past it.

After the boot the witness must be every byte 0xFF, APP's chain its cluster
and one more of the volume's, RMF gone with its cluster free, and no fault
line in QEMU's trace. Before the boot the host reads back what it planted and
checks each planted cluster lands where this says (the observer's own
checks). Fails with the bound removed from `next_cluster` (the witness
written by APP's append and by RMF's free, RD read as two clusters). One boot,
about a minute. Run it whenever FAT32's next_cluster, free_chain, write_at,
read path or cluster_end changes.
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
import mbrdisk  # noqa: E402

_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)
_spec = importlib.util.spec_from_file_location("spawn_reads", os.path.join(HERE, "test-spawn-reads.py"))
spawn_reads = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(spawn_reads)

SECTOR = 512
EOC_MIN = 0x0FFFFFF8
WITNESS_TYPE = 0xDA  # "non-filesystem data": nothing mounts it
WITNESS_MIN = 8 * 1024 * 1024
ESP_COPY = os.path.join(ROOT, "build", "test-fat-link-esp.img")
DISK = os.path.join(ROOT, "build", "test-fat-link.img")


class Esp:
    """The ESP inside a disk image, read and patched in place: geometry,
    FAT entries (every copy on a write), the root directory's short entries."""

    def __init__(self, fh, start):
        self.fh, self.start = fh, start
        fh.seek(start * SECTOR)
        b = fh.read(SECTOR)
        self.spc, reserved, self.nfats = b[13], struct.unpack_from("<H", b, 14)[0], b[16]
        self.fatsz = struct.unpack_from("<I", b, 36)[0]
        self.root = struct.unpack_from("<I", b, 44)[0]
        self.fat = start + reserved
        self.data = self.fat + self.nfats * self.fatsz
        total = struct.unpack_from("<H", b, 19)[0] or struct.unpack_from("<I", b, 32)[0]
        # fsd's cluster_end: data sectors over the cluster size, plus 2, never
        # past what one FAT copy describes.
        self.cluster_end = min((total - reserved - self.nfats * self.fatsz) // self.spc + 2,
                               self.fatsz * SECTOR // 4)
        self.cluster_bytes = self.spc * SECTOR

    def lba(self, cluster):
        return self.data + (cluster - 2) * self.spc

    def fat_lba(self, cluster, copy=0):
        return self.fat + copy * self.fatsz + cluster * 4 // SECTOR

    def entry(self, cluster):
        self.fh.seek(self.fat * SECTOR + cluster * 4)
        return struct.unpack("<I", self.fh.read(4))[0] & 0x0FFFFFFF

    def set_entry(self, cluster, value):
        for copy in range(self.nfats):
            at = (self.fat + copy * self.fatsz) * SECTOR + cluster * 4
            self.fh.seek(at)
            old = struct.unpack("<I", self.fh.read(4))[0]
            self.fh.seek(at)
            self.fh.write(struct.pack("<I", (old & 0xF0000000) | value))

    def chain(self, first):
        out = [first]
        while 2 <= self.entry(out[-1]) < min(EOC_MIN, self.cluster_end) and len(out) < 64:
            out.append(self.entry(out[-1]))
        return out

    def files(self):
        """{short name: (first cluster, size)} of the root directory."""
        out = {}
        for c in self.chain(self.root):
            self.fh.seek(self.lba(c) * SECTOR)
            d = self.fh.read(self.cluster_bytes)
            for j in range(0, len(d), 32):
                e = d[j:j + 32]
                if e[0] == 0:
                    return out
                if e[0] != 0xE5 and e[11] != 0x0F and not e[11] & 0x08:
                    name = (e[:8].decode("latin1").rstrip() + "." + e[8:11].decode("latin1").rstrip()).rstrip(".")
                    first = (struct.unpack_from("<H", e, 20)[0] << 16) | struct.unpack_from("<H", e, 26)[0]
                    out[name] = (first, struct.unpack_from("<I", e, 28)[0])
        return out

    def read(self, chain, size):
        out = b""
        for c in chain:
            self.fh.seek(self.lba(c) * SECTOR)
            out += self.fh.read(self.cluster_bytes)
        return out[:size]


def build():
    """Plant the files and links; return the plan and the observer's checks."""
    shutil.copyfile(os.path.join(ROOT, "build", "esp.img"), ESP_COPY)
    with open(ESP_COPY, "rb") as fh:
        start = mbrdisk.entries(fh.read(SECTOR))[0][2]
        cb = Esp(fh, start).cluster_bytes
    spawn_reads.place_mounted(ESP_COPY, {"RD": b"R" * (2 * cb), "APP": b"A" * cb, "RMF": b"M" * cb})
    esp = mbrdisk.esp_payload(ESP_COPY)
    os.remove(ESP_COPY)
    root = mbrdisk.ext2_payload(os.path.join(ROOT, "build", "ext2part.img"))
    # The witness must hold RMF's link's FAT sector in every copy, so it is
    # at least as long as the FATs past a margin.
    fatsz = struct.unpack_from("<I", esp, 36)[0]
    nfats = esp[16]
    witness = b"\xff" * max(WITNESS_MIN, (nfats * fatsz + 4096) * SECTOR)
    layout = mbrdisk.assemble(DISK, [(mbrdisk.ESP_TYPE, esp, True), (WITNESS_TYPE, witness, False),
                                     (mbrdisk.EXT2_TYPE, root, False)])
    (esp_start, _), (wit_start, wit_sectors), _ = layout
    plan = {"witness": (wit_start, wit_sectors)}
    checks = []
    with open(DISK, "r+b") as fh:
        e = Esp(fh, esp_start)
        files = e.files()
        checks.append(("plant: RD, APP and RMF on the ESP", all(n in files for n in ("RD", "APP", "RMF"))))
        if not checks[-1][1]:
            return plan, checks
        # A cluster whose data lies 64 sectors into the witness, and the next.
        xa = 2 + -(-(wit_start + 64 - e.data) // e.spc)
        xr_sector = wit_start + 2048  # RMF's link's FAT entry, copy 0
        xr = (xr_sector - e.fat) * (SECTOR // 4)
        rd, app, rmf = files["RD"][0], files["APP"][0], files["RMF"][0]
        e.set_entry(rd, xa + 1)
        e.set_entry(app, xa)
        e.set_entry(rmf, xr)
        plan.update(cb=e.cluster_bytes, rd=rd, app=app, rmf=rmf, xa=xa, xr=xr, esp_start=esp_start)
        wit_end = wit_start + wit_sectors
        in_wit = lambda s: wit_start <= s < wit_end  # noqa: E731
        checks += [
            ("plant: RD links to a cluster in the witness",
             e.entry(rd) == xa + 1 and xa + 1 >= e.cluster_end
             and in_wit(e.lba(xa + 1)) and in_wit(e.lba(xa + 1) + e.spc - 1)),
            ("plant: APP links to a cluster in the witness",
             e.entry(app) == xa and xa >= e.cluster_end and in_wit(e.lba(xa)) and in_wit(e.lba(xa) + e.spc - 1)),
            ("plant: RMF links to a cluster whose FAT entry is in the witness in every copy",
             e.entry(rmf) == xr and xr >= e.cluster_end and xr < EOC_MIN
             and all(in_wit(e.fat_lba(xr, c)) for c in range(e.nfats))),
            ("plant: the ESP has two FAT copies", e.nfats == 2),
        ]
    return plan, checks


def boot():
    steps = [("login:", "root"), ("assword", "root"), ("# ", "mount 0 /mnt/f"),
             ("# ", "echo ==rd=="), ("# ", "cat /mnt/f/RD | wc"),
             ("# ", "echo tail >> /mnt/f/APP"), ("# ", "rm /mnt/f/RMF"),
             ("# ", "echo ==end=="), ("==end==", "")]
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


def verdicts(plan, driven, out, faults):
    cb = plan["cb"]
    m = re.search(r"^==rd==\r?$(.*?)^[^\n]*echo tail", out, re.M | re.S)
    counts = re.findall(r"^\s*(\d+)\s+(\d+)\s+(\d+)\s*$", m.group(1), re.M) if m else []
    rd_bytes = int(counts[-1][2]) if counts else None
    with open(DISK, "rb") as fh:
        wit_start, wit_sectors = plan["witness"]
        fh.seek(wit_start * SECTOR)
        witness = fh.read(wit_sectors * SECTOR)
        changed = [i // SECTOR for i in range(0, len(witness), SECTOR)
                   if witness[i:i + SECTOR] != b"\xff" * SECTOR]
        e = Esp(fh, plan["esp_start"])
        files = e.files()
        app = files.get("APP")
        app_chain = e.chain(app[0]) if app else []
        app_data = e.read(app_chain, app[1]) if app else b""
        rmf_free = e.entry(plan["rmf"]) == 0
    return [
        ("driven to the end", driven),
        ("fsd mounted ext2 as / and the ESP at /mnt/f", re.search(r"^fsd: ext2 mounted", out, re.M) is not None
         and "mount 0 /mnt/f" in out and "no such" not in out.split("mount 0 /mnt/f", 1)[-1].split("==rd==")[0]),
        (f"RD read as one cluster ({cb} bytes), got {rd_bytes}", rd_bytes == cb),
        (f"the witness unchanged (changed sectors: {changed[:6]}{'...' if len(changed) > 6 else ''})",
         changed == []),
        (f"APP's chain its cluster and one of the volume's: {app_chain}",
         len(app_chain) == 2 and app_chain[0] == plan["app"] and 2 <= app_chain[1] < e.cluster_end),
        ("APP reads back as its As then tail", app_data[:cb] == b"A" * cb and app_data[cb:].startswith(b"tail")),
        ("RMF removed, its cluster free", "RMF" not in files and rmf_free),
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

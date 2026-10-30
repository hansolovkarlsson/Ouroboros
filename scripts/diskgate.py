"""fsd's `disk-gate-rig` build on a boot ESP, for the rigs' --disk-gate modes.

    import diskgate

`make test-disk-gate` (since 2026-10-29) boots read-only exFAT volumes on an
ESP whose \\EFI\\ORBS\\FSD.BIN is fsd's `disk-gate-rig` build, whose verb gate
does not ask an arm, so each refused write is exFAT's own disk gate's
(`gate::Gated`), which no boot of the shipped fsd reaches. test-long-set
--disk-gate boots its five read-only volumes so, and since 2026-10-30
test-volume-bound --disk-gate boots BESIDE, whose bitmap claims the next
partition's first sector. This module is the one copy of what both modes
share: the flag and the names it gives a run, the build's line, the ESP
carrying the build, and the check of which fsd a boot ran (a second copy in
test-volume-bound would drift; the medium review of #374 found the flag's
handling copied still).
"""
import os
import shutil
import subprocess
import sys

GATE_LINE = "fsd: the disk-gate rig's build: the verb gate does not ask an arm"
READY_LINE = "fsd: filesystem server ready"
# The rig's fsd, named by the Makefile (its FSD_GATE_BIN), which rebuilds it
# first: a direct run would stage whatever build was left there, and a path
# spelled here too could drift from the Makefile's (the review of #371).
FSD_GATE_BIN = os.environ.get("FSD_GATE_BIN")
FLAG = "--disk-gate"


def mode(argv, rig, gate_rig):
    """(gate, the run's name, argv's other arguments): whether `FLAG` was
    given, and `gate_rig` then, else `rig`. The name is the run's in its
    verdict, its stall log and its files, so the two modes never share a
    scratch image or a transcript (the review of #371)."""
    gate = FLAG in argv
    return gate, gate_rig if gate else rig, [a for a in argv if a != FLAG]


def esp(source, dest, drive_qemu):
    """`dest`: a copy of the FAT32 image `source` (esp.img) whose
    \\EFI\\ORBS\\FSD.BIN is FSD_GATE_BIN. Exits when the Makefile did not name
    the build or it is not there."""
    if not FSD_GATE_BIN:
        sys.exit("--disk-gate is run by `make test-disk-gate`, which builds the rig's fsd and names it")
    if not os.path.exists(FSD_GATE_BIN):
        sys.exit(f"{FSD_GATE_BIN} not found")
    shutil.copyfile(source, dest)
    try:
        mp = drive_qemu.attach(dest, readonly=False)
        try:
            shutil.copyfile(FSD_GATE_BIN, os.path.join(mp, "EFI", "ORBS", "FSD.BIN"))
            # What macOS leaves on a volume it mounts read-write, removed as
            # the Makefile's image recipes do, so the ESP holds esp.img's
            # files and no others, FSD.BIN the one changed (its FAT, FSInfo
            # and FSD.BIN's entry are macOS's writing, not esp.img's).
            subprocess.run(["find", mp, "-name", "._*", "-delete"], check=True)
            for d in (".fseventsd", ".Trashes", ".Spotlight-V100"):
                shutil.rmtree(os.path.join(mp, d), ignore_errors=True)
        finally:
            drive_qemu.detach(mp)
    except BaseException:
        # Half made: never left for a later run, KEEP or not.
        os.remove(dest)
        raise


def check(label, out, gate):
    """The fsd this boot ran: the rig's build when `gate`, the shipped one
    otherwise. Either way fsd must have said it is ready, so a boot that died
    before fsd spoke is not taken for the shipped build."""
    said, ready = GATE_LINE in out, READY_LINE in out
    if gate:
        return (f"{label}: fsd is the disk-gate rig's build ({GATE_LINE!r} printed)", said and ready)
    return (f"{label}: fsd is the shipped build ({READY_LINE!r} and no {GATE_LINE!r})", ready and not said)

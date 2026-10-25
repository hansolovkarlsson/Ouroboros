#!/usr/bin/env python3
"""The disk reads of a driven boot, counted by QEMU: the one copy, for the
rigs that grade what fsd reads (test-listing-walk.py, test-spawn-reads.py,
test-write-walk.py, test-dir-scan.py).

    python3 scripts/disktrace.py --self-test    (run by `make test`)

QEMU traces every virtio-blk read (`-trace enable=virtio_blk_handle_read`,
into the -d log drive-qemu.py already keeps), so the count is QEMU's, not
anything in the guest. A rig boots twice, the same steps but for the work it
grades, and takes the difference per sector: the boots are deterministic, the
same build reading the same sectors each time.

Every boot goes through drive-qemu.py's classifier (`boot_stalled`) under
the rig's name and a case name of its own, so each boot keeps its own stall
history. `boot` answers None for a boot QEMU's firmware never started (after
the classifier has said INCONCLUSIVE), and `settle` turns the answers of a
case's boots into its problems, or None when the case is INCONCLUSIVE: a
boot that ran and failed fails the case whatever its other boots did. A rig
ends with drive_qemu.verdict. Since 2026-10-25; until then a stall was
failed checks here, and for a day a stall dropped its whole pair, a failed
first boot with it.

Not a program but for the self-test: loaded by path, as the rigs load
drive-qemu.py.
"""
import collections
import contextlib
import importlib.util
import io
import os
import re
import shutil
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

READ = re.compile(rb"virtio_blk_handle_read .*?sector (\d+) nsectors (\d+)")
WRITE = re.compile(rb"virtio_blk_handle_write .*?sector (\d+) nsectors (\d+)")


def boot(rig, case, source, image, steps, inspect=None, writes=None):
    """Boot a copy of `source` (the stale-image guard reads build/`image`),
    drive `steps`, and return (problems, transcript lines, sectors read by
    sector number), or None when QEMU's firmware never started the kernel and
    the classifier did not grade the stall (a hang every run meets is graded,
    and then answers as a boot whose steps timed out). The copy and its QEMU
    log are build/`rig`-`case`, and `rig`:`case` is the boot's label in the
    stall log, so no two boots of a rig may share a case. When given,
    `inspect` is called with the copy's path after a boot whose steps all
    ran, before the copy is removed, to read what the guest wrote; what it
    raises is the caller's, and the boot is logged before it runs. When
    `writes` (a Counter) is given, QEMU traces writes too and it is filled
    with the sectors written, as the reads are returned."""
    tag = f"{rig}-{case}"
    copy = os.path.join(ROOT, "build", f"{tag}.img")
    log = os.path.join(ROOT, "build", f"{tag}.log")
    shutil.copyfile(source, copy)
    try:
        trace = ["-trace", "enable=virtio_blk_handle_read"]
        if writes is not None:
            trace += ["-trace", "enable=virtio_blk_handle_write"]
        guest = drive_qemu.Guest(copy, extra_args=trace,
                                 intlog=log, label=f"{tag}: ",
                                 stamp_from=os.path.join(ROOT, "build", image))
        driven = False
        try:
            driven = guest.run(steps)
            out = guest.transcript().replace("\r\n", "\n")
        finally:
            guest.stop()
        faults = guest.aborts()
        if drive_qemu.boot_stalled(rig, case, out, faults, qemu_log=log):
            return None
        if inspect and driven:
            inspect(copy)
    finally:
        os.remove(copy)
    problems = []
    if not driven:
        problems.append("a step timed out")
    if faults:
        problems.append("fault lines in QEMU's trace")
    with open(log, "rb") as fh:
        text = fh.read()
    count = collections.Counter()
    for sector, n in READ.findall(text):
        for s in range(int(n)):
            count[int(sector) + s] += 1
    if writes is not None:
        for sector, n in WRITE.findall(text):
            for s in range(int(n)):
                writes[int(sector) + s] += 1
    if not count:
        problems.append("no virtio_blk_handle_read events: the observer sees nothing")
    return problems, out.split("\n"), count


def settle(answers):
    """The problems of a case from its boots' answers, each `boot`'s (or a
    rig's wrapper's, whose first item is its problems) or None for a stall:
    None when a boot stalled and no boot that ran has a problem (the case is
    INCONCLUSIVE, which the classifier has already printed), else every
    problem, with a note when a boot stalled, so a case with a stall is never
    graded on its counts."""
    problems = [p for a in answers if a is not None for p in a[0]]
    stalls = sum(a is None for a in answers)
    if stalls and not problems:
        return None
    if stalls:
        problems.append(f"{stalls} of the case's {len(answers)} boots stalled in QEMU's firmware: "
                        "its counts are not compared")
    return problems


def per_run(base, more, runs):
    """The sectors `runs` more of the graded work read, per run, from the
    counts of two boots: (problems, {sector: reads a run}). A count that is
    not a whole number a run means the boots differ by more than that work."""
    diff = more - base
    if sum(base.values()) > sum(more.values()) or any(c % runs for c in diff.values()):
        return ["the two boots differ by more than the work graded"], {}
    return [], {s: c // runs for s, c in diff.items()}


def self_test():
    """`boot` and `settle` with a fake guest, on a scratch build/ and stall
    log: a stall answers None and is logged under rig:case with its QEMU log
    kept, a boot that ran is graded, a failed boot fails its case beside a
    stall, a stall on each of the last PERSISTENT runs is graded, and a boot
    is logged before its inspect runs."""
    global ROOT
    saved = ROOT, drive_qemu.Guest, drive_qemu.STALL_LOG, drive_qemu.STALL_DIR
    failed = 0

    def check(name, ok):
        nonlocal failed
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += not ok

    with tempfile.TemporaryDirectory() as tmp:
        ROOT = tmp
        os.makedirs(os.path.join(tmp, "build"))
        drive_qemu.STALL_LOG = os.path.join(tmp, "runs.log")
        drive_qemu.STALL_DIR = os.path.join(tmp, "stalls")
        source = os.path.join(tmp, "source.img")
        with open(source, "wb") as fh:
            fh.write(b"disk")
        plan = []

        class Fake:
            """A boot as the next entry of `plan` says: "ran", "timed out" or
            "stalled"; the trace holds one read of sector 5."""
            def __init__(self, copy, extra_args, intlog, label, stamp_from):
                self.kind = plan.pop(0)
                with open(intlog, "w") as fh:
                    fh.write("virtio_blk_handle_read vdev 0x0 sector 5 nsectors 1\n")

            def run(self, steps):
                return self.kind == "ran"

            def transcript(self):
                return drive_qemu.X64_STALLED if self.kind == "stalled" else "Ouroboros kernel: UEFI stage alive\n"

            def stop(self):
                pass

            def aborts(self):
                return 0

        drive_qemu.Guest = Fake

        def run(kind, case="a", inspect=None):
            plan.append(kind)
            with contextlib.redirect_stdout(io.StringIO()):
                return boot("self", case, source, "image", [], inspect)

        def logged():
            with open(drive_qemu.STALL_LOG) as fh:
                return [line.split()[2:4] for line in fh]

        try:
            check("a stall answers None", run("stalled") is None)
            check("and is logged as self:a INCONCLUSIVE", logged()[-1] == ["self:a", "INCONCLUSIVE"])
            kept = sorted(os.listdir(drive_qemu.STALL_DIR))
            check("with its transcript and its QEMU log kept",
                  [k[-4:] for k in kept] == [".log", ".txt"] and kept[0][:-4] == kept[1][:-4])
            ran = run("ran", "b")
            check("a boot that ran is graded, its trace counted",
                  ran is not None and ran[0] == [] and ran[2] == {5: 1})
            check("and logged as self:b booted", logged()[-1] == ["self:b", "booted"])
            timed_out = run("timed out", "c")
            check("settle: a failed boot beside a stall fails the case",
                  bool(settle([timed_out, None])) and "a step timed out" in settle([timed_out, None]))
            check("settle: a stall beside a passing boot is INCONCLUSIVE", settle([ran, None]) is None)
            check("settle: two passing boots have no problems", settle([ran, ran]) == [])
            got = [run("stalled", "d") for _ in range(drive_qemu.PERSISTENT)]
            check(f"a stall on each of the last {drive_qemu.PERSISTENT} runs of a case is graded",
                  got[:-1] == [None] * (drive_qemu.PERSISTENT - 1) and got[-1] is not None
                  and "a step timed out" in got[-1][0])

            def raises(copy):
                raise ValueError("inspect")
            try:
                run("ran", "e", raises)
            except ValueError:
                pass
            check("a boot is logged before its inspect runs, and its copy removed",
                  logged()[-1] == ["self:e", "booted"]
                  and not os.path.exists(os.path.join(tmp, "build", "self-e.img")))
        finally:
            ROOT, drive_qemu.Guest, drive_qemu.STALL_LOG, drive_qemu.STALL_DIR = saved
    return failed


if __name__ == "__main__":
    if sys.argv[1:] != ["--self-test"]:
        print(__doc__)
        sys.exit(2)
    sys.exit(1 if self_test() else 0)

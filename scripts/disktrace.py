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
the classifier has said INCONCLUSIVE), and otherwise every problem the rig
finds in that boot, on its disk or in its transcript, through the rig's
`inspect` and `check`. `settle` turns the answers of a case's boots into its
problems, or None when the case is INCONCLUSIVE: a boot that ran and failed
fails the case whatever its other boots did. A rig keeps a `Tally` of its
cases and ends with its verdict. Since 2026-10-25; until then a stall was
failed checks here.

Not a program but for the self-test: loaded by path, as the rigs load
drive-qemu.py.
"""
import collections
import contextlib
import gzip
import importlib.util
import io
import os
import re
import shutil
import struct
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

READ = re.compile(rb"virtio_blk_handle_read .*?sector (\d+) nsectors (\d+)")
WRITE = re.compile(rb"virtio_blk_handle_write .*?sector (\d+) nsectors (\d+)")


def boot(rig, case, source, image, steps, inspect=None, check=None, writes=None):
    """Boot a copy of `source` (the stale-image guard reads build/`image`),
    drive `steps`, and return the boot's answer, (problems, transcript lines,
    sectors read by sector number), or None when QEMU's firmware never
    started the kernel and the classifier did not grade the stall (a hang
    every run meets is graded, and then answers as a boot whose steps timed
    out). The copy and its QEMU log are build/`rig`-`case`, and
    `rig`:`case` is the boot's label in the stall log, so no two boots of a
    rig may share a case.

    Everything a rig grades in one boot is in its answer's problems, so that
    `settle` sees it whatever the case's other boots did: `inspect(copy)`,
    called after a boot whose steps all ran and before the copy is removed,
    returns the problems it finds on the guest's disk (one it raises reading
    the disk, a ValueError, IndexError or struct.error, is a problem too), and
    `check(lines, count)` returns the problems in the boot's transcript and
    reads. The boot is logged before either runs. When `writes` (a Counter)
    is given, QEMU traces writes too and it is filled with the sectors
    written, before `check` runs."""
    tag = f"{rig}-{case}"
    copy = os.path.join(ROOT, "build", f"{tag}.img")
    log = os.path.join(ROOT, "build", f"{tag}.log")
    shutil.copyfile(source, copy)
    problems = []
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
            try:
                problems += inspect(copy)
            except (ValueError, IndexError, struct.error) as e:
                problems.append(f"reading the guest's disk after the boot: {e}")
    finally:
        os.remove(copy)
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
    lines = out.split("\n")
    if check:
        problems += check(lines, count)
    return problems, lines, count


def settle(answers):
    """The problems of a case from its boots' answers (`boot`'s, or None for
    a stall): None when a boot stalled and no boot that ran has a problem
    (the case is INCONCLUSIVE, which the classifier has already printed),
    else every problem, with a note when a boot stalled, so a case with a
    stall is never graded on what its boots compare."""
    problems = [p for a in answers if a is not None for p in a[0]]
    stalls = sum(a is None for a in answers)
    if stalls and not problems:
        return None
    if stalls:
        problems.append(f"{stalls} of the case's {len(answers)} boots stalled in QEMU's firmware: "
                        "what they compare is not graded")
    return problems


class Tally:
    """A rig's cases, for drive_qemu.verdict: `settle` a case's answers,
    then `close` it with every problem it has, its comparison's included,
    unless settle answered None. The verdict counts the cases that failed
    and the boots that stalled, and refuses a case settled and never closed,
    so a rig cannot lose one by forgetting it."""

    def __init__(self, rig):
        self.rig, self.failed, self.stalled, self.open = rig, 0, 0, 0

    def settle(self, answers):
        self.stalled += sum(a is None for a in answers)
        problems = settle(answers)
        self.open += problems is not None
        return problems

    def close(self, problems):
        if not self.open:
            raise RuntimeError(f"{self.rig}: a case closed that was not settled")
        self.open -= 1
        self.failed += bool(problems)

    def verdict(self):
        if self.open:
            raise RuntimeError(f"{self.rig}: {self.open} case(s) settled and never closed")
        return drive_qemu.verdict(self.rig, self.failed, self.stalled)


def per_run(base, more, runs):
    """The sectors `runs` more of the graded work read, per run, from the
    counts of two boots: (problems, {sector: reads a run}). A count that is
    not a whole number a run means the boots differ by more than that work."""
    diff = more - base
    if sum(base.values()) > sum(more.values()) or any(c % runs for c in diff.values()):
        return ["the two boots differ by more than the work graded"], {}
    return [], {s: c // runs for s, c in diff.items()}


def self_test():
    """`boot`, `settle` and `Tally` with a fake guest, on a scratch build/
    and stall log. The fake holds the rig to its order as QEMU does: once it
    is stopped its transcript ends with QEMU's own signal line, which hides
    a stall from the classifier; and it can report fault lines."""
    global ROOT
    saved = ROOT, drive_qemu.Guest, drive_qemu.STALL_LOG, drive_qemu.STALL_DIR
    failed = 0

    def check(name, ok):
        nonlocal failed
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += not ok

    trace = ("virtio_blk_handle_read vdev 0x0 sector 5 nsectors 1\n"
             "virtio_blk_handle_write vdev 0x0 sector 9 nsectors 1\n")
    kernel = "Ouroboros kernel: UEFI stage alive\n"
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
            """A boot as the next entry of `plan` says: "ran", "timed out",
            "stalled", or "faulted" (ran, with a fault line in QEMU's trace),
            or "stalled, faulted" (the stall's transcript, a fault line)."""
            def __init__(self, copy, extra_args, intlog, label, stamp_from):
                self.kind, self.stopped = plan.pop(0), False
                with open(intlog, "w") as fh:
                    fh.write(trace)

            def run(self, steps):
                return self.kind in ("ran", "faulted")

            def transcript(self):
                out = drive_qemu.X64_STALLED if self.kind.startswith("stalled") else kernel
                return out + ("qemu-system-aarch64: terminating on signal 15 from pid 1\n" if self.stopped else "")

            def stop(self):
                self.stopped = True

            def aborts(self):
                return int("faulted" in self.kind)

        drive_qemu.Guest = Fake

        def run(kind, case, inspect=None, hook=None, writes=None):
            plan.append(kind)
            with contextlib.redirect_stdout(io.StringIO()):
                return boot("self", case, source, "image", [], inspect, hook, writes)

        def logged():
            with open(drive_qemu.STALL_LOG) as fh:
                return [line.split()[2:4] for line in fh]

        seen = []

        def inspect(copy):
            with open(copy, "rb") as fh:
                seen.append(fh.read())
            return ["inspected"]

        try:
            check("a stall answers None", run("stalled", "a", inspect) is None)
            check("and is logged as self:a INCONCLUSIVE", logged()[-1] == ["self:a", "INCONCLUSIVE"])
            kept = sorted(os.listdir(drive_qemu.STALL_DIR))
            texts = []
            for k in kept:
                with (gzip.open if k.endswith(".gz") else open)(os.path.join(drive_qemu.STALL_DIR, k), "rt") as fh:
                    texts.append(fh.read())
            check("with its transcript and its QEMU log, gzipped, kept, each as it was",
                  len(kept) == 2 and kept == [kept[1][:-4] + ".log.gz", kept[1]] and kept[1].endswith(".txt")
                  and texts == [trace, drive_qemu.X64_STALLED])
            check("and nothing of it inspected", seen == [])

            hooked, written = [], collections.Counter()

            def hook(lines, count):
                hooked.append((kernel.rstrip() in lines, dict(count), dict(written)))
                return ["checked"]

            ran = run("ran", "b", inspect, hook, written)
            check("a boot that ran is answered with its trace counted",
                  ran is not None and ran[2] == {5: 1} and kernel.rstrip() in ran[1])
            check("and logged as self:b booted", logged()[-1] == ["self:b", "booted"])
            check("its copy inspected before it is removed", seen == [b"disk"]
                  and not os.path.exists(os.path.join(tmp, "build", "self-b.img")))
            check("its check given the transcript and the reads, the writes counted first",
                  hooked == [(True, {5: 1}, {9: 1})])
            check("and what each found in its problems", ran is not None and ran[0] == ["inspected", "checked"])

            timed_out = run("timed out", "c", inspect, lambda lines, count: [])
            check("a boot whose steps timed out is not inspected, and says so",
                  len(seen) == 1 and timed_out is not None and timed_out[0] == ["a step timed out"])
            faulted = run("faulted", "d")
            check("a fault line in QEMU's trace is a problem",
                  faulted is not None and faulted[0] == ["fault lines in QEMU's trace"])
            graded = run("stalled, faulted", "e")
            check("a stall's transcript with a fault line is no stall, and is graded",
                  graded is not None and "fault lines in QEMU's trace" in graded[0])

            def raises(copy):
                raise IndexError("short sector")
            raised = run("ran", "f", raises)
            check("what an inspect raises reading the disk is a problem, the boot logged and its copy removed",
                  raised is not None and any("short sector" in p for p in raised[0])
                  and logged()[-1] == ["self:f", "booted"]
                  and not os.path.exists(os.path.join(tmp, "build", "self-f.img")))

            passed = run("ran", "g")
            for order in ([timed_out, None], [None, timed_out]):
                beside = settle(order) or []
                check(f"settle: a failed boot {'before' if order[0] else 'after'} a stall fails the case, "
                      "saying a boot stalled",
                      "a step timed out" in beside and any("stalled" in p for p in beside))
            check("settle: a stall beside a passing boot, either side, is INCONCLUSIVE",
                  settle([passed, None]) is None and settle([None, passed]) is None)
            check("settle: two passing boots have no problems", settle([passed, passed]) == [])

            tally = Tally("self")
            tally.close(tally.settle([timed_out, None]))
            tally.settle([None, passed])
            tally.close(tally.settle([passed, passed]))
            with contextlib.redirect_stdout(io.StringIO()):
                code = tally.verdict()
            check("Tally: a failed case fails the rig, counting the boots that stalled",
                  code == 1 and (tally.failed, tally.stalled) == (1, 2))
            tally = Tally("self")
            tally.settle([None, passed])
            with contextlib.redirect_stdout(io.StringIO()):
                code = tally.verdict()
            check("Tally: a stall alone is INCONCLUSIVE", code == drive_qemu.INCONCLUSIVE)
            tally = Tally("self")
            tally.settle([passed, passed])
            try:
                tally.verdict()
                refused = False
            except RuntimeError:
                refused = True
            check("Tally: a case settled and never closed is refused", refused)

            got = [run("stalled", "h") for _ in range(drive_qemu.PERSISTENT)]
            check(f"a stall on each of the last {drive_qemu.PERSISTENT} runs of a case is graded",
                  got[:-1] == [None] * (drive_qemu.PERSISTENT - 1) and got[-1] is not None
                  and "a step timed out" in got[-1][0])
        finally:
            ROOT, drive_qemu.Guest, drive_qemu.STALL_LOG, drive_qemu.STALL_DIR = saved
    return failed


if __name__ == "__main__":
    if sys.argv[1:] != ["--self-test"]:
        print(__doc__)
        sys.exit(2)
    sys.exit(1 if self_test() else 0)

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
the rig's name and a name of its own, so each boot keeps its own stall
history. A case is the boots a rig compares, two as a rule. `boot` answers None for a boot QEMU's firmware never started (after
the classifier has said INCONCLUSIVE), and otherwise every problem the rig
finds in that boot, on its disk or in its transcript, through the rig's
`inspect` and `check`. A rig keeps a `Tally` of its cases: `Tally.settle`
turns the answers of a case's boots into its problems, or None when the case
is INCONCLUSIVE (a boot that ran and failed fails the case whatever its other
boots did), counting the boots that stalled, and the rig ends with the
Tally's verdict. Since 2026-10-25; until then a stall was
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


def boot(rig, name, source, image, steps, inspect=None, check=None, writes=None):
    """Boot a copy of `source` (the stale-image guard reads build/`image`),
    drive `steps`, and return the boot's answer, (problems, transcript lines,
    sectors read by sector number), or None when QEMU's firmware never
    started the kernel and the classifier did not grade the stall (a hang
    every run meets is graded, and then answers as a boot whose steps timed
    out). `name` names this one boot within the rig: the copy and its QEMU
    log are build/`rig`-`name`, and `rig`:`name` is the boot's label in the
    stall log, which drive-qemu refuses to see twice in one run.

    Everything a rig grades in one boot is in its answer's problems, so that
    `Tally.settle` sees it whatever the case's other boots did: `inspect(copy,
    problems)`, called after a boot whose steps all ran and before the copy
    is removed, appends what it finds on the guest's disk (a SystemExit, or
    an error reading the image, ValueError, IndexError (a short read
    indexed), OSError or struct.error, is a
    problem too, beside what it appended first; any other exception is a
    bug in the rig and propagates), and `check(lines, count)`
    returns the problems in the boot's transcript and reads. The boot is
    logged before either runs. When `writes` (a Counter) is given, QEMU
    traces writes too and it is filled with the sectors written, before
    `check` runs."""
    tag = f"{rig}-{name}"
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
        if drive_qemu.boot_stalled(rig, name, out, faults, qemu_log=log):
            return None
        if inspect and driven:
            # SystemExit too: the rigs' image helpers refuse a host image
            # that way, and on the guest's disk that is a finding. A bug in
            # the rig's own model (a TypeError, say) keeps its traceback.
            try:
                inspect(copy, problems)
            except (ValueError, IndexError, OSError, struct.error, SystemExit) as e:
                kind = type(e).__qualname__ if type(e).__module__ == "builtins" else f"{type(e).__module__}.{type(e).__qualname__}"
                problems.append(f"inspecting the guest's disk after the boot raised {kind}: {e}")
    finally:
        os.remove(copy)
    if not driven:
        problems.append("a step timed out")
    text = b""
    if faults is None:
        problems.append("no fault trace to read: QEMU left no -d log")
    else:
        if faults:
            problems.append(drive_qemu.fault_text(faults))
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


def _settle(answers):
    """The problems of a case, the boots a rig compares, from the list of
    their answers (`boot`'s, or None for a stall): None when a boot stalled
    and no boot that ran has a problem (the case is INCONCLUSIVE, which the
    classifier has already printed), else every problem, with a note when a
    boot stalled, so a case with a stall is never graded on what its boots
    compare. Private: `Tally.settle` is the way in, since it also counts the
    stalls the verdict needs."""
    problems = [p for a in answers if a is not None for p in a[0]]
    stalls = sum(a is None for a in answers)
    if stalls and not problems:
        return None
    if stalls:
        problems.append(f"{stalls} of the case's {len(answers)} boots stalled in QEMU's firmware: "
                        "what they compare is not graded")
    return problems


class Case(list):
    """A case's problems as `Tally.settle` hands them out: a list the rig
    adds its comparison's problems to, and closes once."""


class Tally:
    """A rig's cases, for drive_qemu.verdict: `settle` a case's answers (any
    iterable, read once),
    which hands back its problems as a Case (or None, INCONCLUSIVE), add the
    comparison's problems to that Case, then `close` it. Only a Case settled
    here closes, and only once; the verdict counts the cases that failed and
    the boots that stalled, and refuses a case settled and never closed, so
    a rig cannot lose one by forgetting it or close the wrong one."""

    def __init__(self, rig):
        self.rig, self.failed, self.stalled, self.open = rig, 0, 0, []

    def settle(self, answers):
        answers = list(answers)
        self.stalled += sum(a is None for a in answers)
        problems = _settle(answers)
        if problems is None:
            return None
        case = Case(problems)
        self.open.append(case)
        return case

    def close(self, case):
        if not any(c is case for c in self.open):
            raise RuntimeError(f"{self.rig}: closing what is not a case settled and still open: {case!r}")
        self.open = [c for c in self.open if c is not case]
        self.failed += bool(case)

    def verdict(self):
        if self.open:
            raise RuntimeError(f"{self.rig}: {len(self.open)} case(s) settled and never closed")
        return drive_qemu.verdict(self.rig, self.failed, self.stalled)


def per_run(base, more, runs):
    """The sectors `runs` more of the graded work read, per run, from the
    counts of two boots: (problems, {sector: reads a run}), the counts None
    when the boots cannot be compared. A sector the graded boot read fewer
    times than the base, or a count that is not a whole number a run, means
    the boots differ by more than that work (Counter subtraction alone would
    drop the first as 0)."""
    diff = more - base
    if any(base[s] > more[s] for s in base) or any(c % runs for c in diff.values()):
        return ["the two boots differ by more than the work graded"], None
    return [], {s: c // runs for s, c in diff.items()}


def self_test():
    """`boot`, `Tally` and `per_run` with a fake guest, on a scratch build/
    and stall log. The fake behaves as drive-qemu's Guest does where boot
    depends on it: aborts() stops the guest, and once stopped the transcript
    ends with QEMU's own signal line; it must be handed the copy, never the
    source, with the trace flags asked for, and writes to the disk it boots
    (so a copy sharing the source's file, or taken again before inspect, is
    seen); it runs the steps it is given; its console's lines end CR LF; its
    trace reads and writes two sectors a request."""
    global ROOT
    saved = ROOT, drive_qemu.Guest, drive_qemu.STALL_LOG, drive_qemu.STALL_DIR
    failed = 0

    def check(name, ok):
        nonlocal failed
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += not ok

    trace = ("virtio_blk_handle_read vdev 0x0 sector 5 nsectors 2\n"
             "virtio_blk_handle_write vdev 0x0 sector 9 nsectors 2\n")
    kernel = "Ouroboros kernel: UEFI stage alive"
    with tempfile.TemporaryDirectory() as tmp:
        ROOT = tmp
        build = os.path.join(tmp, "build")
        os.makedirs(build)
        drive_qemu.STALL_LOG = os.path.join(tmp, "runs.log")
        drive_qemu.STALL_DIR = os.path.join(tmp, "stalls")
        source = os.path.join(tmp, "source.img")
        with open(source, "wb") as fh:
            fh.write(b"disk")
        plan, given, ran_steps = [], [], []

        class Fake:
            """A boot as the next entry of `plan` says: "ran", "timed out",
            "stalled", "faulted" (ran, a fault line in QEMU's trace),
            "stalled, faulted" (the stall's transcript, a fault line),
            "silent" (ran, a trace with no reads) or "stalled, no log"
            (the stall's transcript, and QEMU left no -d log)."""
            def __init__(self, image, extra_args, intlog, label, stamp_from):
                self.kind, self.stopped = plan.pop(0), False
                given.append((image, list(extra_args)))
                if not self.kind.startswith("stalled"):
                    with open(image, "ab") as fh:
                        fh.write(b"+")
                if self.kind != "stalled, no log":
                    with open(intlog, "w") as fh:
                        fh.write("" if self.kind == "silent" else trace)

            def run(self, steps):
                ran_steps.append(list(steps))
                return self.kind in ("ran", "faulted", "silent")

            def transcript(self):
                out = drive_qemu.X64_STALLED if self.kind.startswith("stalled") else kernel + "\r\n# "
                return out + ("qemu-system-aarch64: terminating on signal 15 from pid 1\n" if self.stopped else "")

            def stop(self):
                self.stopped = True

            def aborts(self):
                self.stop()
                if self.kind == "stalled, no log":
                    return None
                return int("faulted" in self.kind)

        drive_qemu.Guest = Fake

        def run(kind, name, inspect=None, hook=None, writes=None):
            plan.append(kind)
            with contextlib.redirect_stdout(io.StringIO()):
                return boot("self", name, source, "image", [("login:", "root"), ("# ", "")],
                            inspect, hook, writes)

        def logged():
            with open(drive_qemu.STALL_LOG) as fh:
                return [line.split()[2:4] for line in fh]

        seen = []

        def inspect(copy, problems):
            with open(copy, "rb") as fh:
                seen.append((copy, fh.read(), logged()[-1]))
            problems.append("inspected")

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
            check("and nothing of it inspected, its copy removed",
                  seen == [] and not os.path.exists(os.path.join(build, "self-a.img")))
            check("the guest is handed the copy, never the source, with the read trace",
                  given[-1] == (os.path.join(build, "self-a.img"), ["-trace", "enable=virtio_blk_handle_read"]))

            hooked, written = [], collections.Counter()

            def hook(lines, count):
                hooked.append((kernel in lines, dict(count), dict(written)))
                return ["checked"]

            ran = run("ran", "b", inspect, hook, written)
            check("a boot that ran is answered, every sector of a request counted",
                  ran is not None and ran[2] == {5: 1, 6: 1})
            check("and logged as self:b booted", logged()[-1] == ["self:b", "booted"])
            check("with writes asked for, the guest is handed the write trace too",
                  given[-1][1] == ["-trace", "enable=virtio_blk_handle_read", "-trace", "enable=virtio_blk_handle_write"])
            check("its copy inspected as the guest left it, after the boot is logged, and removed after",
                  seen == [(os.path.join(build, "self-b.img"), b"disk+", ["self:b", "booted"])]
                  and not os.path.exists(os.path.join(build, "self-b.img")))
            with open(source, "rb") as fh:
                check("the source as it was, the guest's write in the copy alone", fh.read() == b"disk")
            check("the guest given every step", ran_steps[-1] == [("login:", "root"), ("# ", "")])
            check("its check given the transcript's lines without CR, the reads, and the writes counted first",
                  hooked == [(True, {5: 1, 6: 1}, {9: 1, 10: 1})])
            check("and what each found in its problems", ran is not None and ran[0] == ["inspected", "checked"])

            timed_out = run("timed out", "c", inspect, hook)
            check("a boot whose steps timed out is not inspected, says so, and is checked",
                  len(seen) == 1 and timed_out is not None and timed_out[0] == ["a step timed out", "checked"])
            faulted = run("faulted", "d")
            check("a fault line in QEMU's trace is a problem, counted",
                  faulted is not None and faulted[0] == [drive_qemu.fault_text(1)])
            graded = run("stalled, faulted", "e")
            check("a stall's transcript with a fault line is no stall, and is graded",
                  graded is not None and drive_qemu.fault_text(1) in graded[0])
            nolog = run("stalled, no log", "e2")
            check("a stall's transcript with no fault trace to read is graded, saying so",
                  nolog is not None and any("no fault trace" in p for p in nolog[0]))
            silent = run("silent", "e3")
            check("a trace with no reads is a problem: the observer sees nothing",
                  silent is not None and any("observer sees nothing" in p for p in silent[0]))

            for exc in (struct.error("unpack requires a buffer of 4 bytes"), SystemExit("not one partition")):
                def raises(copy, problems, exc=exc):
                    problems.append("found first")
                    raise exc
                kind = "struct.error" if isinstance(exc, struct.error) else type(exc).__name__
                name = f"f-{kind}"
                raised = run("ran", name, raises)
                check(f"an inspect raising {kind} keeps what it found first, the error a problem",
                      raised is not None and raised[0][0] == "found first"
                      and f"raised {kind}: {exc}" in raised[0][1]
                      and logged()[-1] == [f"self:{name}", "booted"]
                      and not os.path.exists(os.path.join(build, f"self-{name}.img")))

            def buggy(copy, problems):
                return None + 1
            try:
                run("ran", "f-TypeError", buggy)
                propagated = False
            except TypeError:
                propagated = True
            check("an inspect raising anything else, a bug in the rig, propagates", propagated)

            passed = run("ran", "g")
            for order in ([timed_out, None], [None, timed_out]):
                beside = _settle(order) or []
                check(f"settle: a failed boot {'before' if order[0] else 'after'} a stall fails the case, "
                      "saying a boot stalled",
                      "a step timed out" in beside and any("stalled" in p for p in beside))
            check("settle: a stall beside a passing boot, either side, is INCONCLUSIVE",
                  _settle([passed, None]) is None and _settle([None, passed]) is None)
            check("settle: two passing boots have no problems", _settle([passed, passed]) == [])

            def verdict_of(tally):
                with contextlib.redirect_stdout(io.StringIO()):
                    return tally.verdict()

            def refused(fn):
                try:
                    fn()
                except RuntimeError:
                    return True
                return False

            tally = Tally("self")
            tally.close(tally.settle([timed_out, None]))
            tally.settle([None, None])
            tally.close(tally.settle([passed, passed]))
            check("Tally: a failed case fails the rig, counting every boot that stalled",
                  verdict_of(tally) == 1 and (tally.failed, tally.stalled) == (1, 3))
            tally = Tally("self")
            tally.settle([None, passed])
            check("Tally: a stall alone is INCONCLUSIVE", verdict_of(tally) == drive_qemu.INCONCLUSIVE)
            tally = Tally("self")
            tally.settle([passed, passed])
            check("Tally: a case settled and never closed is refused", refused(tally.verdict))
            tally = Tally("self")
            first, second = tally.settle([passed, passed]), tally.settle([timed_out, passed])
            tally.close(first)
            check("Tally: a case closed twice is refused", refused(lambda: tally.close(first)))
            check("Tally: closing None, or a list it did not settle, is refused",
                  refused(lambda: tally.close(None)) and refused(lambda: tally.close(["a step timed out"])))
            tally.close(second)
            check("Tally: the failed case still fails the rig", verdict_of(tally) == 1)
            tally = Tally("self")
            case = tally.settle(iter([None, timed_out]))
            tally.close(case)
            check("Tally: an iterator of answers is read whole, the stall first",
                  case is not None and len(case) == 3 and (tally.failed, tally.stalled) == (1, 1))
            tally = Tally("self")
            first, second = tally.settle([passed, passed]), tally.settle([passed, passed])
            tally.close(first)
            check("Tally: a case is closed by identity, not by an equal one still open",
                  first == second and refused(lambda: tally.close(first)))
            tally.close(second)
            check("Tally: and the one still open then closes", verdict_of(tally) == 0)

            check("per_run: the work's reads a run, from two boots",
                  per_run(collections.Counter({1: 1}), collections.Counter({1: 3, 2: 2}), 2) == ([], {1: 1, 2: 1}))
            check("per_run: a sector the graded boot read fewer times cannot be compared",
                  per_run(collections.Counter({1: 3, 2: 1}), collections.Counter({1: 1, 2: 5}), 2)[1] is None)
            check("per_run: nor a count that is not a whole number a run",
                  per_run(collections.Counter(), collections.Counter({1: 3}), 2)[1] is None)

            got = [run("stalled", f"h{i}") for i in range(drive_qemu.PERSISTENT)]
            check("each boot of a case keeps its own stall history", got == [None] * drive_qemu.PERSISTENT)
            got = []
            for _ in range(drive_qemu.PERSISTENT):
                drive_qemu._classified.discard((os.path.abspath(drive_qemu.STALL_LOG), "self:h"))
                got.append(run("stalled", "h"))
            check(f"a stall on each of the last {drive_qemu.PERSISTENT} runs of a boot is graded",
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

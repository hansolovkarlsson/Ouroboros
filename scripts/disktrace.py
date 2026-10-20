#!/usr/bin/env python3
"""The disk reads of a driven boot, counted by QEMU: the one copy, for the
rigs that grade what fsd reads (test-listing-walk.py, test-spawn-reads.py).

QEMU traces every virtio-blk read (`-trace enable=virtio_blk_handle_read`,
into the -d log drive-qemu.py already keeps), so the count is QEMU's, not
anything in the guest. A rig boots twice, the same steps but for the work it
grades, and takes the difference per sector: the boots are deterministic, the
same build reading the same sectors each time.

Not a program: loaded by path, as the rigs load drive-qemu.py.
"""
import collections
import importlib.util
import os
import re
import shutil

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

READ = re.compile(rb"virtio_blk_handle_read .*?sector (\d+) nsectors (\d+)")
WRITE = re.compile(rb"virtio_blk_handle_write .*?sector (\d+) nsectors (\d+)")


def boot(tag, source, image, steps, inspect=None, writes=None):
    """Boot a copy of `source` (the stale-image guard reads build/`image`),
    drive `steps`, and return (problems, transcript lines, sectors read by
    sector number). `tag` names the copy and its log under build/. When
    given, `inspect` is called with the copy's path after a boot whose steps
    all ran, before the copy is removed, to read what the guest wrote; what
    it raises is the caller's. When `writes` (a Counter) is given, QEMU traces
    writes too and it is filled with the sectors written, as the reads are
    returned."""
    copy = os.path.join(ROOT, "build", f"{tag}.img")
    log = os.path.join(ROOT, "build", f"{tag}.log")
    shutil.copyfile(source, copy)
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
        try:
            if inspect and driven:
                inspect(copy)
        finally:
            os.remove(copy)
    problems = []
    if not driven:
        problems.append("a step timed out")
    if guest.aborts():
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


def per_run(base, more, runs):
    """The sectors `runs` more of the graded work read, per run, from the
    counts of two boots: (problems, {sector: reads a run}). A count that is
    not a whole number a run means the boots differ by more than that work."""
    diff = more - base
    if sum(base.values()) > sum(more.values()) or any(c % runs for c in diff.values()):
        return ["the two boots differ by more than the work graded"], {}
    return [], {s: c // runs for s, c in diff.items()}

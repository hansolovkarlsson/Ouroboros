#!/usr/bin/env python3
"""A stack's never-written bytes, counted by the kernel.

The loader paints every program's stack before its first instruction, and
`TASK_STACK_FREE` counts the paint still standing at the stack's low end: the
task's low-water headroom, which `fsd` serves as `/proc/<n>/stackfree`. Until
2026-11-10 the one such measurement was a probe patched into `netd` on a
branch (`8429d38f`), whose own log lines broke the rig it was read from
(docs/ROADMAP.md).

One boot of the FAT32 image. `/bin/stackprobe <levels>` asks for its own
count, goes `levels` calls deep writing 1 KiB in each, asks again and prints
both:

- a fresh program has nearly its whole stack unwritten, and none of it more
  than the stack's size;
- 8 levels and 24 levels differ by 16 whole frames of 1 KiB and a few words
  each: the count moves by what was written, byte for byte;
- 24 levels of ZEROS count the same as 24 of any other byte, which a stack
  only zeroed at load could not say;
- a program loaded into the slot a deep one just left starts as high as the
  first did: the paint is laid at every load;
- a probe held at its depth reads the same from outside, by
  `cat /proc/<slot>/stackfree`, and a killed probe's slot reads `none`;
- `/proc/<n>` lists the file, the idle task's reads `none`, `netd`'s a count
  below the stack's size, and nothing lives under it.

Usage:  python3 scripts/test-stack-free.py      (make test-stack-free builds the image)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
_spec = importlib.util.spec_from_file_location("drive_qemu", os.path.join(HERE, "drive-qemu.py"))
drive_qemu = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(drive_qemu)

IMAGE = os.path.join(ROOT, "build", "esp.img")
TRANSCRIPT = os.path.join(ROOT, "build", "test-stack-free.txt")
PROMPT = "# "
CHUNK = 1024  # stackprobe's bytes a level (its CHUNK; a copy, and the frame check fails if they part)
FRAME_EXTRA = 128  # the most bytes a level's frame may add to its chunk
FRESH = 1024  # the most a program may have written before its first act
NETD = 4  # netd's slot (syscall-abi's NET_TASK)
IDLE = 1  # the idle task's (kernel/src/tasks.rs)
LINE = r"stackprobe: slot (\d+) size (\d+) free (\d+) then (\d+)"

failed = 0
checks = 0


def check(ok, what):
    global failed, checks
    checks += 1
    failed += 0 if ok else 1
    print(f"[{'PASS' if ok else 'FAIL'}] {what}", flush=True)
    return ok


def sh(g, line, wait=None):
    """Type `line` at a prompt and return what the guest printed up to the
    next prompt (or up to `wait`), or "" when that never came."""
    mark = len(g.transcript())
    g.type_line(line)
    if not g.wait_for(wait or PROMPT, timeout=30):
        return ""
    return g.transcript()[mark:]


def probe(g, command, wait=None):
    """One run of stackprobe: (slot, size, free before, free after), or None."""
    m = re.search(LINE, sh(g, command, wait))
    return tuple(int(x) for x in m.groups()) if m else None


def hold(g, levels):
    """A probe left blocked at its depth, by `exec`, which gives the prompt
    back at once: the probe's line and the prompt race, so wait for the
    line, then for a prompt of our own."""
    held = probe(g, f"exec /bin/stackprobe {levels} hold", wait=LINE)
    sh(g, "echo settled", wait=r"\nsettled\r?\n[\s\S]*?" + PROMPT)
    return held


def proc(g, path):
    """The one line `cat /proc/<path>` prints: a count, `none`, or the
    refusal's text."""
    out = sh(g, f"cat /proc/{path}")
    m = re.search(rf"cat /proc/{re.escape(path)}\r?\n([^\r\n]*)", out)
    return m.group(1) if m else None


def asked(g, slot):
    """The kernel's own answer about `slot`, asked by a program the shell
    runs as whoever is logged in: a count, `none` or `denied`."""
    m = re.search(rf"stackprobe: slot {slot} says (\w+)", sh(g, f"stackprobe of {slot}"))
    return m.group(1) if m else None


def ordinary_user(g, size):
    """Who may read the mark: root, the task itself, and nobody else, by the
    file and by the syscall both."""
    global PROMPT
    root = asked(g, NETD)
    check(root is not None and root.isdigit() and 0 < int(root) < size,
          f"root's program asks the kernel about netd and is answered: {root!r}")
    g.type_line("logout")
    PROMPT = r"\$ "
    if not check(g.run([("login:", "user"), ("assword", "user"), (PROMPT, "")]), "the ordinary `user` logs in"):
        return
    sh(g, "mount -p /proc")
    state = proc(g, f"{NETD}/state")
    check(state in ("runnable", "blocked"), f"user reads netd's state, a file for everyone: {state!r}")
    listing = sh(g, f"ls /proc/{NETD}")
    check(re.search(r"\bstackfree\b", listing) is not None, f"user's `ls /proc/{NETD}` lists stackfree")
    refused = proc(g, f"{NETD}/stackfree")
    check(refused is not None and "permission denied" in refused,
          f"user's read of /proc/{NETD}/stackfree is refused: {refused!r}")
    denied = asked(g, NETD)
    check(denied == "denied", f"user's program asks the kernel about netd and is refused: {denied!r}")
    shell = asked(g, 0)
    check(shell == "denied", f"and about the shell that ran it, another task of the same user: {shell!r}")
    own = probe(g, "stackprobe 8")
    check(own is not None and 0 < own[3] < own[2] <= size,
          f"user's program is still answered about its own stack: {own}")


def main():
    g = drive_qemu.Guest(IMAGE, label="[stack-free] ")
    try:
        if not check(g.run([("login:", "root"), ("assword", "root"), (PROMPT, "")]), "the guest boots to root's shell"):
            return failed
        first = probe(g, "stackprobe 0")
        if not check(first is not None, f"`stackprobe 0` prints its line: {first}"):
            return failed
        slot, size, fresh, _ = first
        check(size - FRESH <= fresh <= size,
              f"a fresh program has written under {FRESH} bytes of its {size}-byte stack: {fresh} never written")

        shallow, deep, zeros = probe(g, "stackprobe 8"), probe(g, "stackprobe 24"), probe(g, "stackprobe 24 zero")
        if check(None not in (shallow, deep, zeros), f"the three depths print their lines: {shallow}, {deep}, {zeros}"):
            moved = shallow[3] - deep[3]
            frame, rest = divmod(moved, 16)
            check(rest == 0 and CHUNK <= frame <= CHUNK + FRAME_EXTRA,
                  f"16 levels more write 16 frames more, each {CHUNK} to {CHUNK + FRAME_EXTRA} bytes: "
                  f"{shallow[3]} then {deep[3]}, {moved} bytes, {frame} a frame, {rest} over")
            check(deep[2] - deep[3] >= 24 * CHUNK,
                  f"24 levels fall the count by at least their {24 * CHUNK} bytes: {deep[2]} then {deep[3]}")
            check(zeros[3] == deep[3], f"zeros written count as any byte does: {zeros[3]}, want {deep[3]}")

        again = probe(g, "stackprobe 0")
        check(again is not None and again[0] == slot and again[2] == fresh,
              f"a program loaded into the slot a deep one left starts where the first did: {again}, want slot {slot} at {fresh}")

        check("proc mounted" in sh(g, "mount -p /proc"), "/proc mounts")
        # `exec` gives the prompt back at once, so the probe's line and the
        # prompt race: wait for the line, then for a prompt of our own.
        held = hold(g, 24)
        if check(held is not None, f"a held probe prints its line: {held}"):
            seen = proc(g, f"{held[0]}/stackfree")
            check(seen == str(held[3]), f"the same count read from outside: /proc/{held[0]}/stackfree says {seen!r}, the probe said {held[3]}")
        # An ended task's slot. The `cat` that reads it must not be loaded
        # into it, or it reads its own stack: two more probes held, both
        # killed, and `cat` takes the lower slot and reads the higher.
        lower, higher = hold(g, 8), hold(g, 8)
        if check(None not in (held, lower, higher) and held[0] < lower[0] < higher[0],
                 f"two more held probes take the next slots up: {lower}, {higher}"):
            sh(g, f"kill {lower[0]}")
            sh(g, f"kill {higher[0]}")
            gone = proc(g, f"{higher[0]}/stackfree")
            check(gone == "none", f"a killed task has no stack to count: /proc/{higher[0]}/stackfree says {gone!r}")
            sh(g, f"kill {held[0]}")

        listing = sh(g, f"ls /proc/{NETD}")
        check(re.search(r"\bstackfree\b", listing) is not None, f"`ls /proc/{NETD}` lists stackfree")
        idle = proc(g, f"{IDLE}/stackfree")
        check(idle == "none", f"the idle task has no stack to count: {idle!r}")
        netd = proc(g, f"{NETD}/stackfree")
        check(netd is not None and netd.isdigit() and 0 < int(netd) < size,
              f"netd's count is a number below the stack's size: {netd!r}")
        under = proc(g, f"{NETD}/stackfree/x")
        check(under is not None and "not a directory" in under, f"nothing lives under the file: {under!r}")
        check("after-stack" in sh(g, "echo after-stack"), "the shell answers after")
        ordinary_user(g, size)
        faults = g.aborts()
        check(faults == 0, f"no fault lines in QEMU's trace: {faults}")
    finally:
        transcript = g.transcript()
        g.stop()
        with open(TRANSCRIPT, "w") as f:
            f.write(transcript)
    want = 27
    if checks != want and not failed:
        print(f"[FAIL] {checks} checks ran, want {want}", flush=True)
        return 1
    print(f"test-stack-free: {failed} of {checks} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(main())

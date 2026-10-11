#!/usr/bin/env python3
"""The owner of the kernel's spawn staging: /bin/STAGEPROBE on a booted image.

    python3 scripts/test-spawn-owner.py      (or `make test-spawn-owner`, which builds the image)
    SMP=4 python3 scripts/test-spawn-owner.py (the same on four cores)

The staging buffers a spawn is fed through (`SPAWN_STAGE`, `ARGS_STAGE`,
`CWD_STAGE`, `ENV_STAGE`) are filled over many syscalls, and until 2026-10-11
belonged to no task: two tasks spawning at once interleaved their chunks and
one started the other's image. Now the first staging call claims the staging
for its task, every other task's staging call and `SPAWN` answer
`SPAWN_ERR_STAGING_BUSY` meanwhile, and the owner's `SPAWN` (any outcome,
including the total of 0 a stager gives up with) or the owner's end releases
it.

`stageprobe hold <ms>` claims the staging as a spawner does and holds it
without spawning; `stageprobe check <ms>` waits, then tries to claim until it
is refused and then until it succeeds. One boot, in this order:

0. `stageprobe over`: one chunk staged, then a `SPAWN` of a total of 4096
   must answer `SPAWN_ERROR` (the kernel hands a child no more than its
   owner staged, so the previous stager's bytes are never parsed).
1. `exec /bin/stageprobe check 6000`, then `exec /bin/stageprobe hold 12000` (both in
   the background). While the holder holds, `echo refused-one` must be
   refused by the shell with the code's message and print nothing; the
   checker must report a refusal with the code, and then, once the holder
   has exited, a claim (the release by exit). `echo ok-one` must then run.
2. `exec /bin/stageprobe hold 60000`, `echo refused-two` refused, `kill` of the
   holder, `echo ok-two` runs (the release by kill).
3. `exec /bin/stageprobe hold 1500 abandon`: after its abandon line, with the
   holder still alive for fifteen seconds more, `echo ok-three` must run
   before the holder's exiting line (the release by a SPAWN of total 0).

The four refusal checks fail on the tree with `claim_staging` answering Ok
to everyone (the state before this change); with `SPAWN` not releasing, the
boot shell keeps the staging from its first spawn and no holder can even
claim it, so eight fail (checked 2026-10-11). QEMU's own trace must hold no fault line. About two minutes.
Run it whenever claim_staging, release_staging_of, end_task or the shell's
and netd's staging flows change.
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
TRANSCRIPT = os.path.join(ROOT, "build", "test-spawn-owner.txt")
PROMPT = "# "
BUSY = (1 << 64) - 1 - 44      # syscall_abi::SPAWN_ERR_STAGING_BUSY
TOO_LARGE = (1 << 64) - 1 - 12  # syscall_abi::SPAWN_ERR_TOO_LARGE, a SPAWN of total 0
SPAWN_ERROR = (1 << 64) - 1     # syscall_abi::SPAWN_ERROR, a total past what the owner staged
REFUSAL = r"echo: another task is mid-spawn and holds the kernel's spawn staging \(try again\)"


def slot_of(out, line_rx):
    m = None
    for m in re.finditer(line_rx, out):
        pass
    return m.group(1) if m else None


def drive(guest):
    got = {}
    if not guest.run([("login:", "root"), ("assword", "root"), (PROMPT, "")]):
        return None
    # 0. a child gets no more than its owner staged: a SPAWN of a total past
    # the one chunk is SPAWN_ERROR, not the previous stager's image parsed.
    guest.type_line("/bin/stageprobe over")
    got["over"] = guest.wait_for(rf"spawn of a total past the staged chunk answered code {SPAWN_ERROR}[\s\S]*" + PROMPT, timeout=30)
    # 1. the checker first (it waits six seconds before it tries), then the holder.
    guest.type_line("exec /bin/stageprobe check 6000")
    got["checker"] = guest.wait_for(r"stageprobe \d+: checking after ms 6000", timeout=30)
    checker = slot_of(guest.transcript(), r"stageprobe (\d+): checking after ms 6000")
    guest.type_line("exec /bin/stageprobe hold 12000")
    got["holder1"] = guest.wait_for(r"stageprobe \d+: holding the spawn staging for ms 12000", timeout=30)
    holder1 = slot_of(guest.transcript(), r"stageprobe (\d+): holding the spawn staging for ms 12000")
    guest.type_line("echo refused-one")
    guest.wait_for(PROMPT, timeout=30)
    got["one_out"] = guest.transcript()
    # The checker's claim is in the future; the holder's exiting line comes
    # just before it (within one settle window, so one wait for both), and
    # the checker's refusal line is already in the transcript: read at the end.
    got["claimed"] = guest.wait_for(rf"stageprobe {checker}: claimed after refusals \d+", timeout=60)
    claimed_out = guest.transcript()
    exit1 = claimed_out.find(f"stageprobe {holder1}: exiting")
    got["holder1_exit"] = exit1 >= 0 and exit1 < claimed_out.find(f"stageprobe {checker}: claimed")
    guest.type_line("echo ok-one")
    got["ok_one"] = guest.wait_for(r"\nok-one\r?\n[\s\S]*" + PROMPT, timeout=30)
    # 2. the release by a kill.
    guest.type_line("exec /bin/stageprobe hold 60000")
    got["holder2"] = guest.wait_for(r"stageprobe \d+: holding the spawn staging for ms 60000", timeout=30)
    holder2 = slot_of(guest.transcript(), r"stageprobe (\d+): holding the spawn staging for ms 60000")
    guest.type_line("echo refused-two")
    guest.wait_for(PROMPT, timeout=30)
    got["two_out"] = guest.transcript()
    guest.type_line(f"kill {holder2}")
    guest.wait_for(PROMPT, timeout=30)
    guest.type_line("echo ok-two")
    got["ok_two"] = guest.wait_for(r"\nok-two\r?\n[\s\S]*" + PROMPT, timeout=30)
    # 3. the release by an abandon, the holder still alive.
    guest.type_line("exec /bin/stageprobe hold 1500 abandon")
    got["abandoned"] = guest.wait_for(rf"stageprobe \d+: abandoned the staging, spawn answered code {TOO_LARGE}", timeout=30)
    holder3 = slot_of(guest.transcript(), r"stageprobe (\d+): abandoned the staging")
    guest.type_line("echo ok-three")
    got["ok_three"] = guest.wait_for(r"\nok-three\r?\n[\s\S]*" + PROMPT, timeout=30)
    got["three_out"] = guest.transcript()
    got["holder3_exit"] = guest.wait_for(rf"stageprobe {holder3}: exiting", timeout=40)
    got["slots"] = (checker, holder1, holder2, holder3)
    return got


def checks(got, out, faults):
    checker, holder1, holder2, holder3 = got["slots"]
    one = got["one_out"]
    two = got["two_out"]
    three = got["three_out"]
    refused_line = rf"stageprobe {checker}: refused, the staging is held, code {BUSY}"
    ok3 = three.find("\nok-three")
    exit3 = three.find(f"stageprobe {holder3}: exiting")
    return [
        ("a SPAWN of a total past what the owner staged is SPAWN_ERROR", bool(got["over"])),
        ("the checker and the first holder started", bool(got["checker"] and got["holder1"]) and None not in (checker, holder1)),
        ("echo refused while a program held the staging (the message)", bool(re.search(REFUSAL, one))),
        ("echo printed nothing while refused", "\nrefused-one" not in one),
        ("the checker was refused with SPAWN_ERR_STAGING_BUSY", bool(re.search(refused_line, out))),
        ("the holder exited and the checker then claimed", bool(got["claimed"] and got["holder1_exit"])),
        ("echo ran after the holder's exit", bool(got["ok_one"])),
        ("the second holder started", bool(got["holder2"]) and holder2 is not None),
        ("echo refused again while it held", len(re.findall(REFUSAL, two)) == 2 and "\nrefused-two" not in two),
        ("echo ran after the holder's kill", bool(got["ok_two"])),
        ("the third holder abandoned its staging (SPAWN of 0 refused as empty)", bool(got["abandoned"]) and holder3 is not None),
        ("echo ran after the abandon, the holder still alive", bool(got["ok_three"]) and ok3 >= 0 and (exit3 < 0 or exit3 > ok3)),
        ("the third holder exited afterwards", bool(got["holder3_exit"])),
        ("no fault lines", faults == 0),
    ]


def main() -> int:
    guest = drive_qemu.Guest(IMAGE, label="test-spawn-owner: ")
    try:
        got = drive(guest)
        out = guest.transcript()
        faults = guest.aborts()
    finally:
        guest.stop()
    with open(TRANSCRIPT, "w") as fh:
        fh.write(out)
    if got is None:
        print("FAIL the login never answered")
        print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}")
        return 1
    for line in re.findall(r"^stageprobe \d+: .*$|^echo: .*$", out, re.M):
        print(f"     {line.rstrip()}")
    failed = 0
    for name, ok in checks(got, out, faults):
        print(f"{'ok  ' if ok else 'FAIL'} {name}")
        failed += 0 if ok else 1
    print(f"transcript: {os.path.relpath(TRANSCRIPT, ROOT)}; {drive_qemu.fault_text(faults)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Reproduce a remote-filesystem failure with a packet capture attached.

    python3 scripts/trace-remote-flake.py <out.pcap> [ops] [_] <peer-log>

Boots ONE guest with `-object filter-dump`, mounts the host 9P peer
(`np9p_server.py`, which must already be running on 5641), and runs a MIXED
cycle of remote ops, recording pass/fail per iteration. Written for roadmap
frontier item 3, whose standing instruction is "the fix wants a packet trace
first, not a guess" - and which that trace then answered by showing every TCP
connection healthy and the fault somewhere else entirely.

THREE OBSERVERS, so a failure can be located rather than guessed at:
  - the guest transcript : which iteration failed, and the kernel's own log
  - the peer's request log : whether the request ever ARRIVED
  - the pcap : what actually crossed the wire

WHY A MIXED CYCLE, not one op repeated: 20 identical `cat`s produced zero
failures, because the fault is a function of how long a single op keeps `netd`
runnable, and the ops differ in how many round trips they make. One repeated op
samples one point.

WHY THE PEER-LIVENESS ASSERTION IS NOT OPTIONAL: a run of this harness once
produced a beautifully regular "only cat fails" pattern that was ENTIRELY an
artifact of the host peer having been killed - every SYN got an RST, `cat`
exited 1, and `ls` printed an error and exited 0 (it always exits 0), so the
harness scored the `ls`es as passes. A dead peer looks exactly like the bug.
This refuses to report anything unless the peer's request count rose.

Reuses drive-qemu.py's `Guest` rather than copying it: the paced typing and the
match-only-NEW-output high-water mark are load-bearing, and a second copy would
drift. See docs/testing-qemu.md.
"""
import importlib.util, os, sys, time

HERE = "/Users/hans/Projects/Ouroboros"
spec = importlib.util.spec_from_file_location("dq", os.path.join(HERE, "scripts/drive-qemu.py"))
dq = importlib.util.module_from_spec(spec); spec.loader.exec_module(dq)

IMAGE = os.path.join(HERE, "build/esp.img")
PCAP  = sys.argv[1]
N     = int(sys.argv[2]) if len(sys.argv) > 2 else 20
# A MIXED cycle, matching the shape both observed failures appeared in: several
# different verbs in one boot, not one op repeated. `ls -l` alone opens four
# connections in rapid succession (stat, readdir, stat per entry).
CYCLE = ["ls -l /mnt/a", "cat /mnt/a/SUB/NOTE.TXT", "ls /mnt/a/SUB",
         "cat /mnt/a/HELLO.TXT", "ls /mnt/a/NOPE", "ls /mnt/a"]

extra = (
    "-netdev", "user,id=net0",
    "-device", "virtio-net-device,netdev=net0",
    "-object", f"filter-dump,id=f0,netdev=net0,file={PCAP}",
)
# THE INSTRUMENT MUST PROVE IT IS ALIVE. A previous run of this harness scored
# a perfectly regular failure pattern that was entirely an artifact of the host
# peer having been killed: every SYN got an RST, `cat` exited 1, and `ls` printed
# an error and exited 0 (see below), so the harness read it as "only cat fails".
# A dead peer looks exactly like the bug being hunted.
PEER_LOG = sys.argv[4] if len(sys.argv) > 4 else None
def peer_requests():
    if not PEER_LOG or not os.path.exists(PEER_LOG): return None
    return open(PEER_LOG).read().count("REQ ")
before = peer_requests()
if before is None:
    print("!!! no peer log given - cannot verify the peer is alive"); sys.exit(2)

g = dq.Guest(IMAGE, extra_args=extra)
results = []
try:
    for wait, text in (("login", "root"), ("assword", "root"),
                       ("# ", "mount -r 10.0.2.2:5641 /mnt/a")):
        if not g.wait_for(wait):
            print("!!! never reached", wait); sys.exit(1)
        g.type_line(text)
    if not g.wait_for("# "):
        print("!!! mount never returned"); sys.exit(1)

    for i in range(N):
        mark = len(g.buf)
        g.type_line(CYCLE[i % len(CYCLE)])
        ok = g.wait_for("# ")
        with g.lock:
            out = bytes(g.buf[mark:]).decode("utf-8", "replace")
        # `ls /mnt/a/NOPE` is SUPPOSED to fail - exclude the expected one, or
        # the harness reports a 1-in-6 failure rate that is entirely by design
        # and looks exactly like the bug being hunted.
        expected_fail = CYCLE[i % len(CYCLE)].endswith("NOPE")
        failed = (("failed" in out) or ("code 1" in out)) and not expected_fail
        results.append(not failed)
        print(f"  iter {i+1:2d} {CYCLE[i % len(CYCLE)]:24s}: {'FAIL' if failed else 'ok  '}  "
              f"{out.strip().splitlines()[-2] if len(out.strip().splitlines())>1 else ''}"[:100],
              flush=True)
    time.sleep(dq.LINGER)
finally:
    g.stop()

after = peer_requests()
print(f"\n  peer requests served during this run: {after - before}")
if after == before:
    print("  !!! THE PEER RECEIVED NOTHING - it is dead or unreachable.")
    print("  !!! Every result above is an artifact. Do not read them as data.")
    sys.exit(3)

n_ok = sum(results); n = len(results)
print(f"\n=== {n - n_ok} failure(s) in {n} ops ===")
print(f"  failing iterations: {[i+1 for i,ok in enumerate(results) if not ok] or 'none'}")
print(f"  {dq.fault_line(g)}")

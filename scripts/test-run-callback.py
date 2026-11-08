#!/usr/bin/env python3
"""Decision 7's checks (step 7 of docs/roadmap/roadmap-user-keys.md), driven.

A `cpu` child's `/host` reads come back to the dispatching node as requests
from the EXECUTING machine claiming the run's user, with no user credential.
netd's export serves such a claim while it has an `NP_RUN` in flight to that
machine as that user, and only then (ninep-abi's rule 2). This rig boots the
ext2 image as the dispatcher and plays the executing machine itself, on the
host: a stand-in that accepts the guest's `NP_RUN`, holds it open, and calls
back into the guest's export through a hostfwd, signing with whichever dev key
a check needs. Nothing on the host executes anything; the stand-in answers the
run with one line and a FIN, which is what ends the run on the guest.

The guest dials two addresses, each one an `authorized` line of the dev
cluster: 10.0.2.2, `host`, root-flagged (SLIRP's own address, which reaches
the host's loopback), and 10.0.2.20, `intruder`, unflagged (a SLIRP guestfwd
to the host). That gives every bound a check:

- served: `host`, the run's machine, claims the run's user during the run
  (a read of a file only `user` may read), one-shot and on a keyed session;
- the machine bound: `intruder` claims the same user during the same run,
  refused;
- the time bound: after the run ends, `host`'s one-shot claim is refused,
  and the keyed session opened under the run is closed at its next frame;
- the squash: a run as root to the UNFLAGGED machine, whose callback claims
  root, refused (a run never lifts the squash); to the flagged one, served.

And, since netd reads `/etc/cluster/users` per claim rather than once at
boot, the registry edited live from the guest's root shell: a broken line for
`user` refuses `user` even with a valid credential, while `guest`, whose line
is not the broken one, is still served; with the file removed, nobody is
registered and an uncredentialed `user` is served.

The guest's registry is the dev one, which lists `user`: without rule 2 every
callback claiming `user` would be refused for carrying no credential, which is
what makes the served checks evidence of the rule.

Usage:  python3 scripts/test-run-callback.py      (make test-run-callback builds the image)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import shutil
import socket
import struct
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, path))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = _load("drive_qemu", "drive-qemu.py")
c = _load("np9p_client", "np9p_client.py")

IMAGE = os.path.join(ROOT, "build", "espext2.img")
EXPORT = 5648        # the guest's export, forwarded from the host
FLAGGED = 5651       # the stand-in as `host`: the guest dials 10.0.2.2:5651
UNFLAGGED = 5650     # the stand-in as `intruder`: the guest dials 10.0.2.20:5650
HOST = c.dev_seed(c.DEV_PEER_LABELS["host"])
INTRUDER = c.dev_seed(c.DEV_PEER_LABELS["intruder"])
PRIVATE = b"/Users/user/PRIVATE.TXT"
PRIVATE_TEXT = b"only user may read this\n"

results = []


def check(what, ok, detail):
    results.append(ok)
    print(f"[{'PASS' if ok else 'FAIL'}] {what}: {detail}", flush=True)


def read(path, seed, user, password=None):
    """One signed read of `path` at the guest's export, as `user`, signed by
    `seed`, credentialed with `password` if given: (status, data), or (None,
    why) for a dead connection, which is never a refusal."""
    np_msg = c.build_frame(c.NP_READ, 0, [len(path), 0, 4096], path)
    if password is None:
        frame, nonce = c.signed_frame(np_msg, seed, user=user)
    else:
        frame, nonce = c.credentialed_frame(np_msg, seed, password, user=user)
    try:
        with socket.create_connection(("localhost", EXPORT), timeout=15) as s:
            s.sendall(frame)
            return c.recv_reply(s, nonce, peer_key=c.peer_key())
    except (RuntimeError, OSError) as e:
        return None, f"{type(e).__name__}: {e}"


def shown(st, data):
    if st is None:
        return f"no reply ({data})"
    return f"served, {len(data)} bytes" if c.served(st) else f"refused {c.status_name(st)}"


def keyed_as_user():
    """A keyed session opened by `host` as `user`: the Session, or why not."""
    c.SIGN_KEY = HOST
    s = c.Session("localhost", EXPORT)
    try:
        st, _ = s.open(keyed=True, user=b"user", password=None)
        if st != 0 or s.keys is None:
            s.close()
            return None, f"not keyed (status {st})"
        return s, "keyed"
    except (RuntimeError, OSError) as e:
        s.close()
        return None, f"{type(e).__name__}: {e}"


def stat_on(s):
    try:
        st, _ = s.op(c.stat_frame(PRIVATE))
        return st, None
    except (RuntimeError, OSError) as e:
        return None, f"{type(e).__name__}: {e}"


# Every check the rig records on a full run. A thread that died, or a phase
# that never ran, records fewer, and that is a failure: a rig that counts only
# the FAILs it saw passes when its checks never ran.
EXPECTED = 15


def stand_in(port, during, ready):
    """Accept ONE run on `port`, call `during(claimed)` while holding it open,
    then answer it and close, which ends the run on the guest. Any exception
    is a recorded failure, never a silently dead thread."""
    try:
        _stand_in(port, during, ready)
    except Exception as e:  # noqa: BLE001  recorded as the failure it is
        check(f"the stand-in on {port}", False, f"{type(e).__name__}: {e}")


def _recv_exact(conn, n):
    got = b""
    while len(got) < n:
        b = conn.recv(n - len(got))
        if not b:
            raise RuntimeError(f"the run's frame ended at {len(got)} of {n} bytes")
        got += b
    return got


def _stand_in(port, during, ready):
    ls = socket.socket()
    ls.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    ls.bind(("127.0.0.1", port))
    ls.listen(1)
    ready.set()
    ls.settimeout(120)
    try:
        conn, _ = ls.accept()
    except socket.timeout:
        check(f"the guest's run reached the stand-in on {port}", False, "no connection in 120 s")
        return
    finally:
        ls.close()
    with conn:
        conn.settimeout(15)
        (n,) = struct.unpack("<I", _recv_exact(conn, 4))
        body = _recv_exact(conn, n)
        # AUTHNP03: [magic:8][nonce:16][name:32]...; the guest's client sends no
        # credential until step 8.
        claimed = body[8 + c.NP_NONCE_LEN:8 + c.NP_NONCE_LEN + c.NP_NAME_LEN].split(b"\0", 1)[0]
        during(claimed)
        conn.sendall(b"stand-in: run answered\r\n")


def user_run():
    """Logged in as `user`, `cpu` to the flagged machine."""
    state = {}

    def during(claimed):
        check("the run carries the logged-in user", claimed == b"user", f"claimed {claimed!r}")
        st, data = read(PRIVATE, HOST, b"user")
        check("rule 2: the run's machine claims the run's user during the run, uncredentialed, served",
              st is not None and c.served(st) and data == PRIVATE_TEXT, shown(st, data))
        st, data = read(PRIVATE, INTRUDER, b"user")
        check("the machine bound: another machine claims the run's user during the run, refused",
              st == c.FS_ERR_AUTH, shown(st, data))
        s, how = keyed_as_user()
        state["session"] = s
        st, why = stat_on(s) if s else (None, how)
        check("rule 2: a keyed session opened by the run's machine as the run's user serves",
              s is not None and st is not None and c.served(st), why or f"status {st}")

    def after():
        st, data = read(PRIVATE, HOST, b"user")
        check("the time bound: after the run ends, the run's machine's claim is refused",
              st == c.FS_ERR_AUTH, shown(st, data))
        s = state.get("session")
        if s is None:
            check("the time bound: the session opened under the run closes when it ends", False,
                  "no session was opened")
            return
        st, why = stat_on(s)
        check("the time bound: the session opened under the run closes when it ends",
              st is None, why or f"served or answered: status {st}")
        s.close()

    return during, after


def root_run(flagged):
    def during(claimed):
        check("the run carries root", claimed == b"root", f"claimed {claimed!r}")
        seed = HOST if flagged else INTRUDER
        st, data = read(b"/etc/shadow", seed, b"root")
        if flagged:
            check("the squash: a run as root to a flagged machine, its callback claiming root, served",
                  st is not None and c.served(st) and data.startswith(b"root:"), shown(st, data))
        else:
            check("the squash: a run as root to an UNFLAGGED machine, its callback claiming root, refused",
                  st == c.FS_ERR_AUTH, shown(st, data))
    return during, lambda: None


def registry_checks(g, prompt):
    """Edit the guest's registry from its root shell and see each claim met
    as the edit says, at the very next request."""
    if not g.run([("", "echo user not-a-key > /etc/cluster/users"), (prompt, "")]):
        check("the registry edited", False, "timed out")
        return
    st, data = read(PRIVATE, INTRUDER, b"user", password=b"user")
    check("the registry: `user` with a valid credential, its line broken, refused",
          st == c.FS_ERR_AUTH, shown(st, data))
    st, data = read(b"/HELLO.TXT", INTRUDER, b"guest")
    check("the registry: `guest`, the broken line not its own, still served",
          st is not None and c.served(st), shown(st, data))
    if not g.run([("", "rm /etc/cluster/users"), (prompt, "")]):
        check("the registry removed", False, "timed out")
        return
    st, data = read(PRIVATE, INTRUDER, b"user")
    check("the registry: removed, nobody is registered, `user` uncredentialed served",
          st is not None and c.served(st) and data == PRIVATE_TEXT, shown(st, data))


def cpu(g, port_addr, prompt, what):
    """Type one `cpu` at the guest and wait for its prompt to come back."""
    if not g.run([("", f"cpu {port_addr} id"), (prompt, "")]):
        check(f"{what}: the guest's `cpu` returned", False, "timed out")


def main():
    image = os.path.join(ROOT, "build", "run-callback.img")
    shutil.copyfile(IMAGE, image)
    g = drive_qemu.Guest(image, extra_args=(
        "-netdev", f"user,id=net0,hostfwd=tcp::{EXPORT}-:564,"
                   # `cmd:`, one command per guest connection; the `tcp:` form
                   # connects ONCE, at QEMU's start, before any stand-in listens.
                   f"guestfwd=tcp:10.0.2.20:{UNFLAGGED}-cmd:nc 127.0.0.1 {UNFLAGGED}",
        "-device", "virtio-net-device,netdev=net0",
    ), label="[callback] ")
    try:
        for user, prompt, runs in (
            ("user", r"\$ ", [(FLAGGED, "10.0.2.2", user_run())]),
            ("root", "# ", [(UNFLAGGED, "10.0.2.20", root_run(False)),
                            (FLAGGED, "10.0.2.2", root_run(True))]),
        ):
            if not g.run([("login:", user), ("assword", user), (prompt, "")]):
                check(f"login as {user}", False, "timed out")
                break
            for port, addr, (during, after) in runs:
                ready = threading.Event()
                t = threading.Thread(target=stand_in, args=(port, during, ready), daemon=True)
                t.start()
                ready.wait(10)
                cpu(g, f"{addr}:{port}", prompt, f"{user} to {addr}")
                t.join(timeout=90)
                # AFTER the guest's prompt is back, not after a fixed wait:
                # `cpu` returns only once netd has answered its caller on the
                # stand-in's FIN, which is where the run's park is freed.
                after()
            if user == "root":
                registry_checks(g, prompt)
            g.run([("", "exit")])
        alive = g.proc.poll() is None
        check("the guest is alive at the end", alive, "alive" if alive else "died")
        restarts = len(re.findall(r"restarted \(attempt", g.transcript()))
        check("no server restarted", restarts == 0, f"{restarts} restart line(s)")
    finally:
        transcript = g.transcript()
        g.stop()
        with open(os.path.join(ROOT, "build", "run-callback.txt"), "w") as f:
            f.write(transcript)
        os.unlink(image)
    if len(results) != EXPECTED:
        check("every check ran", False, f"{len(results)} recorded, {EXPECTED} expected")
    failed = results.count(False)
    print(f"test-run-callback: {failed} of {len(results)} check(s) failed", flush=True)
    return failed


if __name__ == "__main__":
    sys.exit(min(main(), 125))

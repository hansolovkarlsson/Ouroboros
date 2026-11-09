#!/usr/bin/env python3
"""Step 8 of docs/roadmap/roadmap-user-keys.md, driven: netd's client sends a
user credential, on the two-node ext2 rig.

A user logged in on a node has a key held by that node's netd (step 4); since
step 8 every request netd sends for that user's uid carries the user's
credential (`AUTHNP05`), and the export on the other node (step 7) serves the
registered user `user` only with one. This boots the ext2 pair, A exporting
and B the client, drives B's console with drive-qemu.py's `Guest`, and grades
every step from B's console and B's capture (QEMU's filter-dump), the one
place that says which format a request used.

The checks, in the plan's words:

- served: `user`, logged in on B, reads a file only `user` may read on A by a
  path verb (`cat`) and through a keyed session (`cremote`); and `cpu A` reads
  B's copy back through `/host` (A runs the command as `user` on B's
  credential, and its `/host` reads come back under Decision 7);
- the key locked: once `user` has logged out, a request from uid 1000 on B
  (`su user` by root) is refused at A, by a path verb and by a fid verb;
- the session case: a keyed session opened by `user` is closed by B at the
  logout (the capture shows B's FIN on it, and the program holding its fid
  finds the fid dead), and the su'd request opens a NEW session, signed
  `AUTHNP03`, never riding the old one; the same with a nested login shell
  killed instead of logged out, the FIN within one idle period;
- under load from another uid: `user`'s reads are served while root, also
  logged in on B with a key held, floods netd with messages of its own
  (`cfidwait -n`). Written as the control for "signed for the uid captured at
  the park", it is NOT one: a mutation reading `SENDER_ID` at sign time still
  passes it, because the park and the sign happen in one drain of netd's
  mailbox and the flood never landed between them (measured, user-keys step
  8). It stays as a check that a second uid's traffic changes nothing.

It also prints the added cost of a credential, from the capture: the round
trip of a one-shot request and of a session's opening, `user` (credentialed)
against `guest` by `su guest` (no key held, so `AUTHNP03`; unregistered, so
served), the same file and verb. The capture sees the export's added verify,
not the client's added sign, which happens before the first byte is sent;
`edtest`'s "sign (one-shot)" is that half.

Usage:  python3 scripts/test-cred-client.py     (make test-cred-client builds the pair)
Exit status: the number of failed checks.
"""
import importlib.util
import os
import re
import shutil
import struct
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, path))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


drive_qemu = _load("drive_qemu", "drive-qemu.py")
c = _load("np9p_client", "np9p_client.py")

BUILD = os.path.join(ROOT, "build")
IMAGE_A = os.path.join(BUILD, "espext2-a.img")
IMAGE_B = os.path.join(BUILD, "espext2-b.img")
LINK_PORT = 12343    # its own: drive-2vm.py has 12342, the Makefile's rigs 12340/12341
PCAP_B = os.path.join(BUILD, "cred-client-b.pcap")
A_IP, B_IP = "10.0.2.10", "10.0.2.11"
EXPORT = f"{A_IP}:564"
PRIVATE = "/Users/user/PRIVATE.TXT"
PRIVATE_TEXT = "only user may read this"
HELLO = "/HELLO.TXT"
HOLD_S = 60          # cfidwait's hold: past the su'd checks that follow the logout
AUTH_REFUSED = "authentication failed"
USER_P, ROOT_P = r"\$ ", "# "

results = []


def check(what, ok, detail=""):
    results.append(bool(ok))
    print(f"[{'PASS' if ok else 'FAIL'}] {what}" + (f": {detail}" if detail else ""), flush=True)


def do(g, cmd, wait, timeout=90):
    """Type `cmd`, wait for `wait`, and return (matched, the output since)."""
    with g.lock:
        start = len(g.buf)
    g.type_line(cmd)
    ok = g.wait_for(wait, timeout)
    with g.lock:
        out = bytes(g.buf[start:]).decode(errors="replace")
    if not ok:
        g.report(f"!!! TIMEOUT waiting for {wait!r} after {cmd!r}")
    return ok, out


def login(g, user, prompt, at_prompt=False):
    """Log in. `at_prompt`: the `login:` prompt has already been matched (by
    the `exit` that led to it), so the name is typed without waiting for it
    again, which would wait for a prompt that never comes."""
    return g.run([("" if at_prompt else "login:", user), ("assword", user), (prompt, "")])


def first_line(out, rx):
    m = re.search(rx, out)
    return m.group(0) if m else out.strip().replace("\r", "").split("\n")[-2:]


# --- B's capture --------------------------------------------------------------

MAGICS = {c.NP_AUTH_MAGIC_SIGNED: "AUTHNP03", c.NP_AUTH_MAGIC_KEYED: "AUTHNP04", c.NP_AUTH_MAGIC_USER: "AUTHNP05"}
FIN, SYN = 0x01, 0x02


def packets(path):
    """(t, src, sport, dst, dport, flags, payload) for each TCP/IPv4 frame of a
    pcap (filter-dump writes the classic format, Ethernet)."""
    with open(path, "rb") as f:
        data = f.read()
    (magic,) = struct.unpack("<I", data[:4])
    endian = "<" if magic in (0xA1B2C3D4, 0xA1B23C4D) else ">"
    nano = magic in (0xA1B23C4D, 0x4D3CB2A1)
    off = 24
    while off + 16 <= len(data):
        sec, frac, incl, _ = struct.unpack(endian + "IIII", data[off:off + 16])
        frame = data[off + 16:off + 16 + incl]
        off += 16 + incl
        t = sec + frac / (1e9 if nano else 1e6)
        if len(frame) < 34 or frame[12:14] != b"\x08\x00" or frame[23] != 6:
            continue
        ihl = (frame[14] & 0x0F) * 4
        tcp = 14 + ihl
        total = struct.unpack(">H", frame[16:18])[0]
        src, dst = ".".join(map(str, frame[26:30])), ".".join(map(str, frame[30:34]))
        sport, dport = struct.unpack(">HH", frame[tcp:tcp + 4])
        doff = (frame[tcp + 12] >> 4) * 4
        flags = frame[tcp + 13]
        payload = frame[tcp + doff:14 + total]
        yield t, src, sport, dst, dport, flags, payload


def flows(path):
    """B's connections to A's export, by B's port: a dict of
    {syn, fin, first (magic name, verb, t), replies [t], frames [(t, magic)]}."""
    out = {}
    for t, src, sport, dst, dport, flags, payload in packets(path):
        if src == B_IP and dst == A_IP and dport == 564:
            f = out.setdefault(sport, {"syn": None, "fin": None, "first": None, "reply": None, "frames": []})
            if flags & SYN and f["syn"] is None:
                f["syn"] = t
            if flags & FIN and f["fin"] is None:
                f["fin"] = t
            if len(payload) >= 12:
                (magic,) = struct.unpack("<Q", payload[4:12])
                name = MAGICS.get(magic)
                if name:
                    f["frames"].append((t, name))
                    if f["first"] is None:
                        at = 4 + {"AUTHNP03": c.NP_AUTH_HDR_SIGNED,
                                  "AUTHNP05": c.NP_AUTH_HDR_SIGNED + c.NP_CRED_HDR_LEN + c.NP_CRED_USER_SIG_LEN,
                                  "AUTHNP04": c.NP_AUTH_HDR_KEYED}[name]
                        verb = struct.unpack("<Q", payload[at:at + 8])[0] if len(payload) >= at + 8 else None
                        f["first"] = (name, verb, t)
        elif src == A_IP and sport == 564 and dst == B_IP and payload:
            f = out.get(dport)
            if f is not None and f["first"] is not None and f["reply"] is None:
                f["reply"] = t
    return out


# --- the run ------------------------------------------------------------------

def main():
    for img in (IMAGE_A, IMAGE_B):
        if not os.path.exists(img):
            sys.exit(f"test-cred-client: {img} missing - run make images-2vm-ext2")
    try:
        os.remove(PCAP_B)
    except FileNotFoundError:
        pass
    # Copies, so a run's writes never reach the next one.
    img_a, img_b = os.path.join(BUILD, "cred-client-a.img"), os.path.join(BUILD, "cred-client-b.img")
    shutil.copyfile(IMAGE_A, img_a)
    shutil.copyfile(IMAGE_B, img_b)
    a = b = None
    marks = {}
    try:
        a = drive_qemu.Guest(img_a, extra_args=(
            "-netdev", f"socket,id=net0,listen=127.0.0.1:{LINK_PORT}",
            "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0a",
        ), label="[A] ")
        if not login(a, "root", ROOT_P):
            check("A reaches its prompt", False, "timed out")
            return 1
        b = drive_qemu.Guest(img_b, extra_args=(
            "-netdev", f"socket,id=net0,connect=127.0.0.1:{LINK_PORT}",
            "-device", "virtio-net-device,netdev=net0,mac=52:54:00:12:34:0b",
            "-object", f"filter-dump,id=f1,netdev=net0,file={PCAP_B}",
        ), label="[B] ")
        run_b(b, marks)
        alive = a.proc.poll() is None and b.proc.poll() is None
        check("both guests are alive at the end", alive)
        restarts = sum(len(re.findall(r"restarted \(attempt", g.transcript())) for g in (a, b))
        check("no server restarted on either node", restarts == 0, f"{restarts} restart line(s)")
    finally:
        for g, name in ((a, "a"), (b, "b")):
            if g is not None:
                transcript = g.transcript()
                g.stop()
                with open(os.path.join(BUILD, f"cred-client-{name}.txt"), "w") as f:
                    f.write(transcript)
        for img in (img_a, img_b):
            os.unlink(img)
    grade_capture(marks)
    failed = results.count(False)
    print(f"test-cred-client: {failed} of {len(results)} check(s) failed", flush=True)
    return failed


def run_b(b, marks):
    # 1. `user` logged in on B: served, by every route.
    if not login(b, "user", USER_P):
        check("B: login as user", False, "timed out")
        return
    do(b, f"mount -r {EXPORT} /mnt/a", USER_P)
    _, out = do(b, f"cat /mnt/a{PRIVATE}", USER_P)
    check("served: user on B reads a user-only file on A by a path verb", PRIVATE_TEXT in out,
          first_line(out, PRIVATE_TEXT + "|" + AUTH_REFUSED))
    _, out = do(b, f"cremote /mnt/a{PRIVATE}", USER_P)
    rx = re.escape(f"/mnt/a{PRIVATE}") + r": fd=\d+ size=\d+ first=" + PRIVATE_TEXT
    # cremote's first line is a LOCAL read of /EFI/ORBS/INIT.CFG, which the
    # ext2 root does not carry; only the remote file's line is graded.
    check("served: user on B reads it through a keyed session (cremote)", re.search(rx, out),
          first_line(out, rx + "|" + re.escape(f"/mnt/a{PRIVATE}") + r": [^\r\n]*"))
    _, out = do(b, f"cpu {EXPORT} cat /host{PRIVATE}", USER_P, timeout=120)
    check("served: `cpu A` as user runs on A and reads B's copy back through /host", PRIVATE_TEXT in out,
          first_line(out, PRIVATE_TEXT + "|cpu:[^\r\n]*|" + AUTH_REFUSED))
    # The cost baseline's credentialed half: three one-shots of a public file.
    for _ in range(3):
        do(b, f"cat /mnt/a{HELLO}", USER_P)
    do(b, f"cremote /mnt/a{HELLO}", USER_P)
    marks["user_cost_end"] = time.time()

    # 2. The session case: a fid held across the logout.
    marks["exec"] = time.time()
    ok, out = do(b, f"exec /bin/cfidwait /mnt/a{PRIVATE} {HOLD_S}", r"cfidwait: (held|open failed)[^\r\n]*")
    check("the session case: user's held fid is open (cfidwait)", "cfidwait: held" in out,
          first_line(out, r"cfidwait: [^\r\n]*"))
    marks["held"] = time.time()
    b.run([("", "<ENTER>"), (USER_P, "")])
    marks["logout"] = time.time()
    do(b, "exit", "login:")
    marks["login_prompt"] = time.time()

    # 3. The key locked: root on B, `su user`, uid 1000 with no key held.
    login(b, "root", ROOT_P, at_prompt=True)
    do(b, f"mount -r {EXPORT} /mnt/b", ROOT_P)
    do(b, "su user", USER_P)
    marks["su"] = time.time()
    _, out = do(b, f"cremote /mnt/b{PRIVATE}", USER_P)
    check("the key locked: uid 1000 after the logout opens a fid, refused at A",
          re.search(re.escape(f"/mnt/b{PRIVATE}") + ": open failed: " + AUTH_REFUSED, out),
          first_line(out, re.escape(f"/mnt/b{PRIVATE}") + r": [^\r\n]*"))
    marks["su_done"] = time.time()
    _, out = do(b, f"cat /mnt/b{PRIVATE}", USER_P)
    check("the key locked: uid 1000 after the logout reads by a path verb, refused at A",
          AUTH_REFUSED in out and PRIVATE_TEXT not in out, first_line(out, PRIVATE_TEXT + "|[^\r\n]*" + AUTH_REFUSED))
    ok = b.wait_for(r"cfidwait: second read (ok|failed[^\r\n]*)", timeout=HOLD_S + 30)
    tail = b.transcript()[-400:]
    m = re.findall(r"cfidwait: second read (ok|failed[^\r\n]*)", tail)
    check("the session case: the fid held across the logout is dead after it",
          ok and m and m[-1].startswith("failed"), m[-1] if m else "no second read seen")
    b.run([("", "<ENTER>"), (USER_P, "")])
    do(b, "exit", "login:")

    # 4. The cost baseline's uncredentialed half, and the flood: `guest`
    # (unregistered) by `su guest`, so no key is held and AUTHNP03 is served.
    login(b, "root", ROOT_P, at_prompt=True)
    do(b, f"mount -r {EXPORT} /mnt/a", ROOT_P)
    do(b, "su guest", USER_P)
    marks["guest_cost"] = time.time()
    for _ in range(3):
        do(b, f"cat /mnt/a{HELLO}", USER_P)
    do(b, f"cremote /mnt/a{HELLO}", USER_P)
    marks["guest_cost_end"] = time.time()
    do(b, "exit", "login:")

    # 5. The uid captured at the park, then the killed nested login. Root logs
    # in on the boot shell and holds a key too; a nested shell logs `user` in.
    login(b, "root", ROOT_P, at_prompt=True)
    _, out = do(b, "exec /bin/sh", "login:")
    _, out = do(b, "ps", ROOT_P)
    slots = [int(n) for n, st in re.findall(r"task (\d+): (blocked|runnable|running)", out) if int(n) >= 6]
    if not slots:
        check("the nested shell is running", False, "no task 6 or above in ps")
        return
    nested = max(slots)
    b.run([("", f"fg {nested}"), ("", "user"), ("assword", "user"), (USER_P, "")])
    do(b, f"mount -r {EXPORT} /mnt/c", USER_P)
    # The flood runs as root, from the boot shell: hand it the keyboard, start
    # it, and come back.
    b.run([("", "fg 0"), ("", "<ENTER>"), (ROOT_P, "")])
    do(b, "exec /bin/cfidwait -n 25", ROOT_P)
    b.run([("", f"fg {nested}"), ("", "<ENTER>"), (USER_P, "")])
    served = 0
    for _ in range(5):
        _, out = do(b, f"cat /mnt/c{PRIVATE}", USER_P)
        served += PRIVATE_TEXT in out
    nudged = b.wait_for(r"cfidwait: nudged netd \d+ times", timeout=60)
    m = re.findall(r"cfidwait: nudged netd (\d+) times", b.transcript())
    check("under load from another uid: user's five reads are served while root floods netd",
          served == 5 and nudged, f"{served} of 5 served, root sent {m[-1] if m else '?'} messages")
    # The killed login: a fid held by a program the nested shell started.
    marks["exec2"] = time.time()
    _, out = do(b, f"exec /bin/cfidwait /mnt/c{PRIVATE} {HOLD_S}", r"cfidwait: (held|open failed)[^\r\n]*")
    check("the killed login: user's held fid is open", "cfidwait: held" in out, first_line(out, r"cfidwait: [^\r\n]*"))
    marks["held2"] = time.time()
    b.run([("", "fg 0"), ("", "<ENTER>"), (ROOT_P, "")])
    marks["kill"] = time.time()
    do(b, f"kill {nested}", ROOT_P)
    marks["killed"] = time.time()
    ok = b.wait_for(r"cfidwait: second read (ok|failed[^\r\n]*)", timeout=HOLD_S + 30)
    m = re.findall(r"cfidwait: second read (ok|failed[^\r\n]*)", b.transcript())
    check("the killed login: the fid held under it is dead after the kill",
          ok and len(m) >= 2 and m[-1].startswith("failed"), m[-1] if len(m) >= 2 else "no second read seen")


def grade_capture(marks):
    try:
        fl = flows(PCAP_B)
    except OSError as e:
        check("B's capture is readable", False, str(e))
        return
    census = {}
    for f in fl.values():
        for _, name in f["frames"]:
            census[name] = census.get(name, 0) + 1
    print(f"--- B's capture: {', '.join(f'{n} {k}' for k, n in sorted(census.items())) or 'no export frames'}")
    check("the capture carries credentialed requests (AUTHNP05)", census.get("AUTHNP05", 0) > 0, str(census))
    by_syn = sorted((f for f in fl.values() if f["syn"] is not None), key=lambda f: f["syn"])
    s05 = [f for f in by_syn if f["first"] and f["first"][:2] == ("AUTHNP05", c.NP_SESSION)]
    s03 = [f for f in by_syn if f["first"] and f["first"][:2] == ("AUTHNP03", c.NP_SESSION)]
    # Capture time and host time differ by an unknown offset, so each check
    # compares DURATIONS: a gap between two frames of the capture against the
    # gap between the host's marks around the steps that sent them.
    #
    # The logout. Every session B opens before the su'd `cremote` is `user`'s,
    # credentialed, so the first AUTHNP03 session is the su'd request's, and
    # the held one is the last credentialed session before it.
    if "su_done" in marks and s03:
        new = s03[0]
        held = [f for f in s05 if f["syn"] < new["syn"]]
        h = held[-1] if held else None
        fin = h["fin"] - h["syn"] if h and h["fin"] else None
        # The FIN: after the logout was typed (its SYN went out before the fid
        # was reported open), and within one idle period of the login prompt.
        lo = marks["logout"] - marks["held"]
        hi = marks["login_prompt"] - marks["exec"] + 10
        check("the capture: B's FIN on the held session at the logout",
              fin is not None and lo - 1 <= fin <= hi,
              f"FIN {fin:.1f} s after its SYN, the logout window {lo:.1f} to {hi:.1f} s" if fin is not None
              else "the held session has no FIN")
        if h:
            keyed = [t for t, name in h["frames"] if name == "AUTHNP04" and h["fin"] and t > h["fin"]]
            check("the capture: no keyed frame rides the held session after its FIN", not keyed,
                  f"{len(keyed)} after it")
        # The su'd request's session: opened AFTER that FIN, and by the time
        # the su'd `cremote` had answered (not some later uncredentialed one).
        gap = new["syn"] - h["syn"] if h else None
        check("the capture: the su'd request opens a new session, uncredentialed (AUTHNP03)",
              h is not None and h["fin"] is not None and new["syn"] > h["fin"] and gap <= marks["su_done"] - marks["exec"] + 5,
              f"AUTHNP03 session {gap:.1f} s after the held one's SYN" if gap is not None else "no held session")
    else:
        check("the capture: B's FIN on the held session at the logout", False, "no AUTHNP03 session in the capture")
    # The kill: the last credentialed session B opens is the second holder's.
    if "killed" in marks and s05:
        k = s05[-1]
        fin = k["fin"] - k["syn"] if k["fin"] else None
        lo = marks["kill"] - marks["held2"]
        hi = marks["killed"] - marks["exec2"] + 15
        check("the capture: B's FIN on the held session within one idle period of the kill",
              fin is not None and lo - 1 <= fin <= hi,
              f"FIN {fin:.1f} s after its SYN, the kill window {lo:.1f} to {hi:.1f} s" if fin is not None
              else "the held session has no FIN")
    else:
        check("the capture: B's FIN on the held session within one idle period of the kill", False,
              "the run did not reach the kill")
    # The added cost, from the capture: request on the wire to reply on the
    # wire, A's side of the exchange, user (AUTHNP05) against guest (AUTHNP03).
    def rtts(magic, verb):
        return [(f["reply"] - f["first"][2]) * 1e6 for f in by_syn
                if f["first"] and f["first"][0] == magic and f["first"][1] == verb and f["reply"]]
    seen = sorted({(f["first"][0], hex(f["first"][1] or 0)) for f in by_syn if f["first"]})
    print(f"--- first frames seen: {seen}", flush=True)
    for verb, what in ((c.NP_READ_FILE, "one-shot read"), (c.NP_READ, "one-shot read (NP_READ)"), (c.NP_SESSION, "session open")):
        u, g = rtts("AUTHNP05", verb), rtts("AUTHNP03", verb)
        if u and g:
            mu, mg = sorted(u)[len(u) // 2], sorted(g)[len(g) // 2]
            print(f"--- measured, {what}: AUTHNP05 median {mu:.0f} us over {len(u)}, AUTHNP03 median {mg:.0f} us "
                  f"over {len(g)}, the export's added cost {mu - mg:.0f} us", flush=True)
        else:
            print(f"--- measured, {what}: not enough samples ({len(u)} AUTHNP05, {len(g)} AUTHNP03)", flush=True)


if __name__ == "__main__":
    sys.exit(min(main(), 125))

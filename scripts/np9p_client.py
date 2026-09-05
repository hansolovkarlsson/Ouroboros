#!/usr/bin/env python3
r"""A minimal host-side client for Ouroboros's 9P-over-TCP export (cluster
Phase 1). Speaks the length-delimited `ninep-abi` frame to a netd export
listener (default guest port 564, reached via a SLIRP hostfwd) so you can read a
guest's filesystem over TCP from the host - the "foreign observer" that verifies
the export gateway (docs/roadmap-cluster-phase1.md).

Usage:
    python3 scripts/np9p_client.py <host> <port> readdir <path>
    python3 scripts/np9p_client.py <host> <port> read    <path> [offset] [want]
    python3 scripts/np9p_client.py <host> <port> stat    <path>
    python3 scripts/np9p_client.py <host> <port> mv      <src> <dst>
    python3 scripts/np9p_client.py <host> <port> run     <command>
    python3 scripts/np9p_client.py <host> <port> noverb  [path] [verb]

Every request is SIGNED with a per-machine Ed25519 key: the auth header is
`[magic:8][nonce:16][name:32][pubkey:32][sig:64]` in front of the NP message,
signed over `domain-tag || nonce || name || np`, where `name` is the user the
request is made on behalf of. The exporter looks the offered public key up in
its `/etc/cluster/authorized` and refuses one it does not list.

The shared `\CLUSTER.KEY` this used to MAC with authenticates nothing any more.
`--legacy-mac` still builds a frame in that retired format, because proving the
guest refuses one requires being able to send one.

e.g. after `make run-image-9p`:
    python3 scripts/np9p_client.py localhost 5640 readdir /
    python3 scripts/np9p_client.py localhost 5640 read /EFI/ORBS/INIT.CFG
    python3 scripts/np9p_client.py localhost 5640 readdir / --sign=nobody      # -> refused (unauthorized key)
    python3 scripts/np9p_client.py localhost 5640 readdir / --legacy-mac       # -> refused (retired format)
    python3 scripts/np9p_client.py localhost 5640 read /etc/shadow --user user # -> refused (permissions)
    python3 scripts/np9p_client.py localhost 5640 noverb /                     # -> FS_ERR_NO_SUCH_VERB (NP_OPEN)
    python3 scripts/np9p_client.py localhost 5640 noverb / 0x10c               # -> served: NP_STAT (the control)
"""
import hashlib
import hmac
import os
import socket
import struct
import sys

# ninep-abi verb numbers (NP_BASE = 0x100).
NP_BASE = 0x100
NP_READDIR = NP_BASE + 0
NP_READ_FILE = NP_BASE + 1
NP_READ = NP_BASE + 2
NP_MV = NP_BASE + 9
NP_STAT = NP_BASE + 12
STAT_INFO_LEN = 27   # ninep-abi STAT_INFO_LEN; the NP_STAT result record
NP_WRITE_FILE = NP_BASE + 11
NP_WRITE_AT = NP_BASE + 4
NP_READ_AT = NP_BASE + 10
NP_OPEN = NP_BASE + 15  # the first of the five fid verbs no export implements
FS_ERR_MIN = (1 << 64) - 64  # errors are a small band just below u64::MAX
FS_ERR_AUTH = (1 << 64) - 1 - 30  # u64::MAX - 30
# The two statuses a SEALED post-authentication refusal can carry. They became
# worth naming when the exporter started signing that refusal: before, every
# client rejected the 12-byte unsealed denial on length and reported AUTH
# FAILED, so these values could not reach a caller at all.
NO_FS = (1 << 64) - 1 - 1  # u64::MAX - 1: no filesystem mounted (transient)
FS_ERR_NOT_FOUND = (1 << 64) - 1 - 2  # u64::MAX - 2: definitively absent

# NOT a wire value: what `recv_reply` returns when THIS CLIENT would not trust
# the reply it got - the exporter's signature did not verify against the key we
# expect for the host we dialled. Distinct from FS_ERR_AUTH because those are
# opposite failures, and reporting both as "AUTH FAILED" made this script agree
# with whatever the reader already believed. A negative control that cannot tell
# "the export refused me" from "I refused the export" proves neither.
# Deliberately ONE PAST the u64 range: it can never collide with a status the
# wire can carry, and it still compares `>= FS_ERR_MIN`, so every existing
# "is this an error?" branch in this script keeps treating it as one instead of
# falling through to the success path or raising a TypeError.
REPLY_UNVERIFIED = 1 << 64

# Cluster auth wire constants (ninep-abi).
#
# The RETIRED shared-key format, kept only so `--legacy-mac` can build a frame
# the guest must refuse. It is not a format this client speaks any more.
RETIRED_MAC_MAGIC = int.from_bytes(b"AUTHNP02", "big")
NP_AUTH_MAGIC_SIGNED = int.from_bytes(b"AUTHNP03", "big")  # the per-machine-keypair format
NP_RUN = 0x100 + 0x20  # ninep-abi's NP_BASE + 0x20 - remote execution (`cpu`)
NP_PUBKEY_LEN = 32
NP_SIG_LEN = 64
# Signature DOMAIN TAGS - must match ninep-abi's SIG_DOMAIN_* byte for byte.
# They keep a signature made in one role from verifying in the other: without
# them a captured reply signature is structurally a valid request signature from
# the same key.
SIG_DOMAIN_REQUEST = b"ouroboros-cluster-request-v1\0"
SIG_DOMAIN_REPLY = b"ouroboros-cluster-reply-v1\0"

NP_NONCE_LEN = 16
NP_NAME_LEN = 32  # requesting user's name, NUL-padded - ninep-abi NP_NAME_LEN

# The seed this client signs with. Signing is no longer opt-in: it is the only
# format the guest accepts.
#
# THIS SCRIPT IS THE FOREIGN OBSERVER. A signer that shares none of the guest's
# code - Python against Rust - agreeing about a format neither can quietly
# redefine, is the only thing that can show the verifier is right rather than
# merely self-consistent.
# The dev nodes, by the SHORT NAME an `authorized` line carries, mapped to the
# seed label `mkclusterkeys.py` actually derives that node's key from.
#
# This map exists because the short name is the one a person reaches for, and
# passing it raw produced a control THAT COULD NOT FAIL FOR ITS STATED REASON:
# `--peer=node-b` derived sha256(b"node-b"), a key belonging to no machine in the
# cluster, so the run printed REPLY NOT VERIFIED whether or not the client checks
# the key for the address it dialled - it would print the same thing against an
# exporter that accepted ANY authorized signature, which is exactly the bug the
# control is meant to catch. Only a real other node's key distinguishes "a key I
# authorize" from "the key for the host I asked".
DEV_PEER_LABELS = {
    "node-a": "ouroboros-dev-node-a",
    "node-b": "ouroboros-dev-node-b",
    "host": "ouroboros-dev-host-peer",
}

SIGN_LABEL = DEV_PEER_LABELS["host"]
SIGN_KEY = None  # derived in main() from SIGN_LABEL, or --sign=<label>

# The retired shared key, set ONLY by `--legacy-mac[=<key>]`. When set, this
# client sends an old-format frame instead of a signed one - the flag day's
# negative control, and nothing else.
LEGACY_MAC_KEY = None


def dev_seed(label):
    """The same fixed dev seeds scripts/mkclusterkeys.py derives its keys from."""
    return hashlib.sha256(label.encode()).digest()


_ED_REFERENCE = None


def load_ed_reference():
    """The Ed25519 reference, which asserts itself against RFC 8032 when loaded.

    Memoized: loading it re-runs those self-assertions, which are two full
    pure-Python signatures. Once is the point (it proves the reference before
    anything trusts it); once PER FRAME made `dial` and `serve`, which issue
    several ops each, needlessly slow.
    """
    global _ED_REFERENCE
    if _ED_REFERENCE is None:
        import importlib.util
        import os.path
        here = os.path.dirname(os.path.abspath(__file__))
        spec = importlib.util.spec_from_file_location("edref", os.path.join(here, "gen-sign-vectors.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        _ED_REFERENCE = mod
    return _ED_REFERENCE


# WHICH USER this client claims to be. The key authenticates the machine; the
# name says who on it is asking, and the guest resolves it through its OWN
# /etc/passwd - so a name it does not know is refused outright. Overridden with
# `--user <name>`; `root` keeps the pre-identity behaviour.
USER = b"root"

# The public key this client expects the EXPORTER to sign its replies with -
# i.e. the key for the host being dialled. Defaults to the dev node-a identity,
# which is what every image stages as its own; `--peer=<label>` picks another,
# including a wrong one, to prove the check bites.
#
# Held as a LABEL and derived on demand. Deriving it is a scalar multiplication
# in pure Python, and loading the reference runs its RFC 8032 self-check first -
# together about a second, which used to be paid by every invocation including
# `np9p_client.py` with no arguments at all. It is only ever needed to verify a
# SIGNED reply, so an unsigned run (and a usage error) now pays nothing.
PEER_LABEL = DEV_PEER_LABELS["node-a"]
_PEER_KEY = None



def peer_key():
    """The expected exporter public key, derived once, on first use."""
    global _PEER_KEY
    if _PEER_KEY is None:
        _PEER_KEY = load_ed_reference().public_key(dev_seed(PEER_LABEL))
    return _PEER_KEY


# Status codes worth NAMING in output. A probe that prints a bare
# 0xffffffffffffffd8 makes the reader do the arithmetic that the bug was about.
STATUS_NAMES = {
    (1 << 64) - 1: "FS_ERROR",
    (1 << 64) - 1 - 2: "FS_ERR_NOT_FOUND",
    (1 << 64) - 1 - 29: "FS_ERR_READ_ONLY",
    (1 << 64) - 1 - 30: "FS_ERR_AUTH",
    (1 << 64) - 1 - 32: "FS_ERR_PERM",
    (1 << 64) - 1 - 39: "FS_ERR_NO_SUCH_VERB",
}


def status_name(status):
    """`FS_ERR_NO_SUCH_VERB` for a known code, else the raw value."""
    return STATUS_NAMES.get(status, f"0x{status:x}")


def build_frame(verb, tree, params, payload):
    # The bare NP message: [verb u64][tree u64][a0..a3 u64][payload].
    hdr = struct.pack("<Q", verb) + struct.pack("<Q", tree)
    for i in range(4):
        hdr += struct.pack("<Q", params[i] if i < len(params) else 0)
    return hdr + payload


def signed_frame(np_msg, seed, user=None):
    """[u32 len][magic:8][nonce:16][name:32][pubkey:32][sig:64][np]

    The signature covers SIG_DOMAIN_REQUEST || nonce || name || np - the same
    bytes the retired MAC covered, after a domain tag that keeps a reply
    signature from being replayed as a request one. The public
    key travels in the frame the way SSH offers one; the exporter decides whether
    that key is authorized BEFORE it verifies anything.
    """
    ed = load_ed_reference()
    user = USER if user is None else user
    if len(user) > NP_NAME_LEN:
        sys.exit(f"user name too long (max {NP_NAME_LEN})")
    nonce = os.urandom(NP_NONCE_LEN)
    name = user.ljust(NP_NAME_LEN, b"\0")
    public = ed.public_key(seed)
    sig = ed.sign(seed, SIG_DOMAIN_REQUEST + nonce + name + np_msg)
    # ASSERTED, NOT ASSUMED. `NP_PUBKEY_LEN` and `NP_SIG_LEN` are two of the
    # names check-wire-constants.py compares against ninep-abi, and until these
    # lines existed this function built its header by concatenation and consulted
    # neither - so a reported disagreement could be "fixed" by editing a constant
    # no code reads, turning the check green over an unchanged frame.
    assert len(public) == NP_PUBKEY_LEN, "public key is not NP_PUBKEY_LEN bytes"
    assert len(sig) == NP_SIG_LEN, "signature is not NP_SIG_LEN bytes"
    body = struct.pack("<Q", NP_AUTH_MAGIC_SIGNED) + nonce + name + public + sig + np_msg
    return struct.pack("<I", len(body)) + body, nonce


def legacy_mac_frame(np_msg, key, user=None):
    """Build a frame in the RETIRED shared-key MAC format (`AUTHNP02`).

    Kept ONLY so the flag day has a negative control: the guest must refuse
    this, and a test that cannot produce an old frame cannot show that it does.
    Nothing here builds one by default.

    `[u32 len][magic:8][nonce:16][name:32][mac:32][np]`, where
    `mac = HMAC-SHA256(key, nonce || name || np)`.
    """
    nonce = os.urandom(NP_NONCE_LEN)
    user = USER if user is None else user
    if len(user) > NP_NAME_LEN:
        sys.exit(f"user name too long (max {NP_NAME_LEN})")
    name = user.ljust(NP_NAME_LEN, b"\0")
    mac = hmac.new(key, nonce + name + np_msg, hashlib.sha256).digest()
    auth = struct.pack("<Q", RETIRED_MAC_MAGIC) + nonce + name + mac
    body = auth + np_msg
    return struct.pack("<I", len(body)) + body, nonce


def recv_reply(sock, nonce, peer_key=None):
    # The server frames [u32 len][sig:64][status u64][data] then FINs
    # (reply-auth). Verify the signature against the nonce WE sent before
    # trusting a byte; a failure (tamper, or an unauthorized peer) -> refusal.
    buf = b""
    while True:
        chunk = sock.recv(4096)
        if not chunk:
            break
        buf += chunk
    if len(buf) < 4:
        raise RuntimeError(f"short reply ({len(buf)} bytes)")
    (flen,) = struct.unpack("<I", buf[:4])
    body = buf[4:4 + flen]
    # The reply is signed, and it must verify against the key expected for the
    # HOST WE DIALLED, not against any key we happen to authorize. Accepting the
    # latter would authenticate "some cluster member" rather than "the machine
    # I asked".
    #
    # A body too short to hold a signature is not a mangled reply, it is the
    # DENIAL shape: the export refuses an unauthorized request with a bare
    # `[len=8][FS_ERR_AUTH]` and no signature, because it has nothing to sign
    # with on behalf of a caller it just rejected. Confirmed on the wire: 12
    # bytes total. So report the export's refusal, not ours.
    #
    # A hostile middlebox could truncate a real reply into this shape and make
    # us print "refused" for something else. That costs a diagnostic, not a
    # trust decision - both paths refuse the reply either way.
    if len(body) < NP_SIG_LEN + 8:
        return FS_ERR_AUTH, b""
    sig, np = body[:NP_SIG_LEN], body[NP_SIG_LEN:]
    if peer_key is None or not load_ed_reference().verify(peer_key, SIG_DOMAIN_REPLY + nonce + np, sig):
        return REPLY_UNVERIFIED, b""
    (status,) = struct.unpack("<Q", np[:8])
    return status, np[8:]


def one_op(host, port, np_msg, timeout=10):
    """Run one authenticated NP op over a fresh export connection; return (status, data)."""
    if LEGACY_MAC_KEY is not None:
        # The negative control: a retired-format frame, which must be refused.
        frame, nonce = legacy_mac_frame(np_msg, LEGACY_MAC_KEY)
    else:
        frame, nonce = signed_frame(np_msg, SIGN_KEY)
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.sendall(frame)
        status, data = recv_reply(s, nonce, peer_key=peer_key())
    return status, data


def run_op(host, port, command, timeout=10):
    """Send an NP_RUN (`cpu`) frame and return the export's RAW reply bytes.

    Not `one_op`: a remote-run reply is an output STREAM, not a framed
    `[len][sig][status][data]`, so there is nothing to parse or verify - the
    caller gets exactly what crossed the wire. That is what makes this usable as
    a probe: a refusal on this path is a human-readable line, and comparing the
    line two different machines send is how you check that an unauthenticated
    caller cannot tell them apart.

    `--legacy-mac` is REFUSED here rather than supported. The export cannot peek
    the verb of a retired frame - it does not parse that format at all - so it
    answers with a *framed* reply whichever verb was inside, and this function
    would print those bytes raw, as binary. The retired-format control belongs
    on the fs path, where the reply is decoded and the refusal is legible.
    """
    if LEGACY_MAC_KEY is not None:
        sys.exit("run: --legacy-mac has no readable answer on this path (the "
                 "export replies to a retired frame with a framed status, not "
                 "text); use it with readdir/read/stat instead")
    cmd = command.encode()
    # a0 = command-line length; a1/a2 would be the caller's endpoint for the
    # /host namespace import (cluster Phase 4b), which a host peer does not
    # offer - it exports nothing back - so they stay zero and the command runs
    # with the remote's own namespace only.
    np_msg = build_frame(NP_RUN, 0, [len(cmd), 0, 0, 0], cmd)
    frame, _nonce = signed_frame(np_msg, SIGN_KEY)
    out = b""
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.sendall(frame)
        try:
            while True:
                b = s.recv(4096)
                if not b:
                    break
                out += b
        except socket.timeout:
            pass
    return out


def np_readfile(host, port, path, want=512):
    pb = path.encode()
    return one_op(host, port, build_frame(NP_READ_FILE, 0, [len(pb), want], pb))


def np_writefile(host, port, path, data):
    pb = path.encode()
    # NP_WRITE_FILE: a0 = path len, a1 = data len; payload = path ++ data.
    msg = build_frame(NP_WRITE_FILE, 0, [len(pb), len(data)], pb + data)
    return one_op(host, port, msg)


def do_dial(host, port, dst_ip, dst_port, request):
    """Drive the GUEST's /net/tcp to dial dst_ip:dst_port out of ITS nic, over the
    export - "use the guest's network from here". Prints the response bytes."""
    import time
    base = "/net/tcp"
    st, data = np_readfile(host, port, base + "/clone")
    if st == REPLY_UNVERIFIED:
        print("status: REPLY NOT VERIFIED"); sys.exit(1)
    if st == FS_ERR_AUTH:
        print("status: AUTH FAILED"); sys.exit(1)
    if st >= FS_ERR_MIN or not data.strip().isdigit():
        print(f"clone failed (status 0x{st:016x})"); sys.exit(1)
    n = int(data.strip())
    print(f"[clone] connection {n}")
    st, _ = np_writefile(host, port, f"{base}/{n}/ctl", f"connect {dst_ip}!{dst_port}".encode())
    if st >= FS_ERR_MIN:
        print(f"connect failed (status 0x{st:016x})"); sys.exit(1)
    # Poll status until Established.
    for _ in range(50):
        st, data = np_readfile(host, port, f"{base}/{n}/status", want=16)
        s = data.split(b"\n")[0].decode("latin1", "replace")
        if s.startswith("Established"):
            print(f"[status] {s}"); break
        if s.startswith("Closed"):
            print("[status] Closed - refused/unreachable"); sys.exit(1)
        time.sleep(0.1)
    else:
        print("connect timed out"); sys.exit(1)
    if request:
        np_writefile(host, port, f"{base}/{n}/data", request)
    print("[response]")
    got = b""
    empties = 0
    for _ in range(400):
        st, data = np_readfile(host, port, f"{base}/{n}/data", want=512)
        if st >= FS_ERR_MIN:
            break
        if data:
            got += data; empties = 0
            continue
        st, sdata = np_readfile(host, port, f"{base}/{n}/status", want=16)
        if sdata.split(b"\n")[0].startswith(b"Closed"):
            break
        empties += 1
        if empties > 40:
            break
        time.sleep(0.05)
    np_writefile(host, port, f"{base}/{n}/ctl", b"close")
    sys.stdout.buffer.write(got)
    sys.stdout.flush()
    print(f"\n[done] {len(got)} bytes received")


def do_serve(host, port, announce_port, extern_port, response):
    """Drive the GUEST's /net/tcp DIAL-IN over the export: announce a port on the
    guest's NIC, then a host socket connects to the guest at that port (via the
    hostfwd `extern_port`) as the external client; the guest accepts, we relay
    the request/response over the export, and the external client sees the reply.
    Proves "accept inbound on the guest's network, served from here.\""""
    import time
    base = "/net/tcp"
    st, data = np_readfile(host, port, base + "/clone")
    if st >= FS_ERR_MIN or not data.strip().isdigit():
        print(f"clone failed (0x{st:016x})"); sys.exit(1)
    n = int(data.strip())
    print(f"[clone] listener {n}")
    st, _ = np_writefile(host, port, f"{base}/{n}/ctl", f"announce {announce_port}".encode())
    if st >= FS_ERR_MIN:
        print(f"announce failed (0x{st:016x})"); sys.exit(1)
    print(f"[announce] listening on guest:{announce_port}")

    # The external client connects to the guest at announce_port (hostfwd).
    ext = socket.create_connection(("localhost", extern_port), timeout=10)
    req = b"PING-FROM-EXTERNAL-CLIENT\r\n"
    ext.sendall(req)
    print(f"[external] connected to localhost:{extern_port} (-> guest:{announce_port}), sent {len(req)} bytes")

    # Accept: poll listen for the accepted connection M.
    m = None
    for _ in range(50):
        st, data = np_readfile(host, port, f"{base}/{n}/listen", want=16)
        if st < FS_ERR_MIN and data.strip().isdigit():
            m = int(data.strip()); break
        time.sleep(0.1)
    if m is None:
        print("no connection accepted (timeout)"); sys.exit(1)
    print(f"[listen] accepted connection {m}")

    # Read the request the external client sent (relayed through the guest).
    got_req = b""
    for _ in range(50):
        st, data = np_readfile(host, port, f"{base}/{m}/data", want=512)
        if st < FS_ERR_MIN and data:
            got_req += data; break
        time.sleep(0.05)
    print(f"[request] guest relayed: {got_req!r}")

    # Respond, then close.
    np_writefile(host, port, f"{base}/{m}/data", response)
    np_writefile(host, port, f"{base}/{m}/ctl", b"close")

    # The external client should receive the response the guest relayed.
    ext.settimeout(10)
    got_resp = b""
    try:
        while True:
            chunk = ext.recv(4096)
            if not chunk:
                break
            got_resp += chunk
    except OSError:
        pass
    ext.close()
    np_writefile(host, port, f"{base}/{n}/ctl", b"close")  # stop listening
    print(f"[external] received: {got_resp!r}")
    if got_resp.strip() == response.strip():
        print("[OK] external client got the response served from the export side")
    else:
        print("[FAIL] response mismatch"); sys.exit(1)


def main():
    if "--help" in sys.argv or "-h" in sys.argv:
        print(__doc__)
        return
    args = sys.argv[1:]
    # `--legacy-mac[=<key>]`: send a frame in the RETIRED shared-key format,
    # which the guest must refuse. This is the flag day's negative control - a
    # test that cannot produce an old frame cannot show that one is rejected.
    global LEGACY_MAC_KEY
    for i, a in enumerate(list(args)):
        if a == "--legacy-mac":
            LEGACY_MAC_KEY = b"ouroboros-dev-cluster-key-v1"
            del args[i]
            break
        if a.startswith("--legacy-mac="):
            LEGACY_MAC_KEY = a[len("--legacy-mac="):].encode()
            del args[i]
            break
    # `--user <name>`: who this request claims to be (default root). The guest
    # applies ITS permission model to that name, so this is what makes an
    # unprivileged remote read testable from the host.
    # `--sign=<label>`: sign with a DIFFERENT seed - which is how a key the guest
    # does NOT authorize gets tested. Signing itself is no longer opt-in, so a
    # bare `--sign` is accepted and means the default identity.
    #
    # NOT `--sign <label>` with a heuristic: guessing whether the next token is a
    # label or a positional silently swallowed arguments. `read --sign FILE 0 100`
    # signed with the label "FILE" and read the path "0" - a test that would have
    # "proved" a refusal for entirely the wrong reason.
    global SIGN_LABEL, SIGN_KEY
    for i, a in enumerate(list(args)):
        if a == "--sign":
            # A bare `--sign` means the default identity. But `--sign nobody`
            # is the likeliest typo for the documented control, and it used to
            # leave "nobody" as an ignored positional and sign with the DEFAULT
            # key - so a run meant to prove an unauthorized key is refused
            # instead proved an authorized one is served, silently. The
            # `leftover` check below cannot catch it: the orphan is not a flag.
            # Refuse rather than guess which was meant.
            if i + 1 < len(args) and not args[i + 1].startswith("-"):
                sys.exit(f"--sign takes no separate argument; write "
                         f"--sign={args[i + 1]} (or a bare --sign for the "
                         f"default identity)")
            del args[i]
            break
        if a.startswith("--sign="):
            SIGN_LABEL = a[len("--sign="):]
            del args[i]
            break
    SIGN_KEY = dev_seed(SIGN_LABEL)
    # `--peer=<node>`: whose signature to expect on the reply. Recorded, not
    # derived - see `peer_key()`. The short dev node name is translated to the
    # seed label that node's key is really derived from.
    #
    # AN UNKNOWN NAME IS AN ERROR, not a fallback. It used to pass the raw
    # string through, so `--peer=nodeb` derived a key NO MACHINE HOLDS and the
    # run printed "REPLY NOT VERIFIED" - which it would also print against an
    # exporter that wrongly accepted any authorized signature. The control then
    # passes for the wrong reason, which is the exact defect
    # `check_dev_peer_labels` was added to prevent, reachable through a typo.
    # `--peer-label=` below is the escape hatch for a deliberately-nobody key,
    # where saying so explicitly is the point.
    global PEER_LABEL
    for i, a in enumerate(list(args)):
        if a.startswith("--peer="):
            want = a[len("--peer="):]
            if want not in DEV_PEER_LABELS:
                sys.exit(f"--peer={want}: unknown node (known: "
                         f"{', '.join(sorted(DEV_PEER_LABELS))}); "
                         f"use --peer-label=<seed> for a key no machine holds")
            PEER_LABEL = DEV_PEER_LABELS[want]
            del args[i]
            break
    for i, a in enumerate(list(args)):
        if a.startswith("--peer-label="):
            PEER_LABEL = a[len("--peer-label="):]
            del args[i]
            break
    if "--user" in args:
        global USER
        i = args.index("--user")
        if i + 1 >= len(args):
            sys.exit("--user needs a name")
        USER = args[i + 1].encode()
        del args[i:i + 2]
    # EVERY FLAG MUST HAVE BEEN CONSUMED BY NOW.
    #
    # An unrecognised `--flag` used to fall through as an ignored positional, so
    # the request went out with DEFAULT settings while the operator believed
    # they had changed something - a negative control that quietly becomes a
    # successful authenticated run. `--key wrong` was documented as a control
    # for exactly that until the flag day removed the flag, and it did not start
    # failing, it started SUCCEEDING. A typo (`--sing=nobody`) does the same,
    # and so does a repeated flag, since each loop above deletes one and breaks.
    # `dial` and `serve` forward args[5:] to the guest as arbitrary text, so a
    # `--`-prefixed word there is payload, not a flag. Everything up to the
    # fixed positionals is checked for every op.
    checked = args[:5] if (len(args) > 2 and args[2] in ("dial", "serve")) else args
    leftover = [a for a in checked if a.startswith("--")]
    if leftover:
        sys.exit(f"unknown or repeated option(s): {' '.join(leftover)}")
    if len(args) < 4:
        print(__doc__)
        sys.exit(2)
    host, port, op = args[0], int(args[1]), args[2]

    if op == "serve":
        # serve <host> <port> serve <announce_port> <extern_hostfwd_port> [response...]
        if len(args) < 5:
            print("usage: np9p_client.py <host> <port> serve <announce_port> <extern_port> [response...]")
            sys.exit(2)
        announce_port, extern_port = int(args[3]), int(args[4])
        response = ((" ".join(args[5:]) if len(args) > 5 else "HELLO-SERVED-VIA-GUEST") + "\r\n").encode()
        do_serve(host, port, announce_port, extern_port, response)
        return

    if op == "badwrite":
        # badwrite <host> <port> badwrite [path]
        #
        # A deliberately MALFORMED but correctly-SIGNED NP_WRITE_AT: a0 (the
        # path length) is 0xFFFF against a payload of a dozen bytes. The export
        # used to slice `&payload[a0..a0 + dlen]` without clamping the range
        # START, so this panicked netd - killing every live TCP connection,
        # dial slot and export session, and burning a supervisor restart.
        #
        # It needs a valid MAC, so it is a trusted-peer fault rather than an
        # open one - but a truncated or mis-built frame arrives here by
        # accident, which is the likelier way to meet it. Kept as a regression
        # probe: run it and the guest should answer an error and stay up.
        path = (args[3] if len(args) > 3 else "/HELLO.TXT").encode()
        np = build_frame(NP_WRITE_AT, 0, [0xFFFF, 0, 0, 0], path)
        try:
            status, data = one_op(host, port, np)
            print(f"guest answered status=0x{status:x} ({len(data)} bytes) - it survived")
        except Exception as exc:
            print(f"no usable answer: {exc!r}")
        return

    if op == "noverb":
        # noverb <host> <port> noverb [path] [verb]
        #
        # Send a correctly-signed request for a verb the export has NO ARM for -
        # NP_OPEN by default, the first of the five fid verbs (the open frontier
        # item; see docs/roadmap-fid-verbs.md). The point is the STATUS the
        # guest answers with, not the data.
        #
        # Until 2026-09-05 that status was the generic FS_ERROR, which every
        # client renders as "no such file or directory" - a message about a
        # path, for a request whose path was fine. It is now
        # FS_ERR_NO_SUCH_VERB, and netd LOGS the verb number on the guest
        # console (a status code cannot carry it).
        #
        # THE CONTROL IS THE SECOND ARGUMENT: pass a verb the export DOES serve
        # (0x10c = NP_STAT) and this must NOT answer FS_ERR_NO_SUCH_VERB. A
        # probe that reports "not implemented" for everything, including what is
        # implemented, proves nothing about either.
        path = (args[3] if len(args) > 3 else "/").encode()
        verb = int(args[4], 0) if len(args) > 4 else NP_OPEN
        np = build_frame(verb, 0, [len(path), 0, 0, 0], path)
        try:
            status, data = one_op(host, port, np)
            print(f"verb 0x{verb:x} path={path!r} -> {status_name(status)} "
                  f"({len(data)} bytes of data)")
        except Exception as exc:
            print(f"no usable answer: {exc!r}")
        return

    if op == "dial":
        # dial <host> <port> dial <dst_ip> <dst_port> [request words...]
        if len(args) < 5:
            print("usage: np9p_client.py <host> <port> dial <dst_ip> <dst_port> [request...]")
            sys.exit(2)
        dst_ip, dst_port = args[3], int(args[4])
        request = (" ".join(args[5:]) + "\r\n\r\n").encode() if len(args) > 5 else b""
        do_dial(host, port, dst_ip, dst_port, request)
        return

    path = args[3]
    pb = path.encode()

    if op == "run":
        # `<path>` is the command line here. The reply is raw text, so this
        # returns before any of the framed-reply handling below.
        sys.stdout.write(run_op(host, port, path).decode("utf-8", "replace"))
        return
    if op == "readdir":
        np_msg = build_frame(NP_READDIR, 0, [len(pb), 4096], pb)
    elif op == "read":
        offset = int(args[4]) if len(args) > 4 else 0
        want = int(args[5]) if len(args) > 5 else 4096
        np_msg = build_frame(NP_READ, 0, [len(pb), offset, want], pb)
    elif op == "mv":
        # Two paths in one payload, lengths in a0/a1. Present because the
        # guest's own /bin/mv guards `mv f f` before fsd ever sees it, so the
        # server-side guard - the one that protects THIS path, where paths
        # arrive raw - had no client that could reach it.
        if len(args) < 5:
            sys.exit("mv needs <src> <dst>")
        src = args[3].encode()
        dst = args[4].encode()
        np_msg = build_frame(NP_MV, 0, [len(src), len(dst)], src + dst)
    elif op == "stat":
        # NP_STAT, not NP_READ_FILE. This op sent NP_READ_FILE with want=1 and
        # printed its byte count as a "size", which is a plausible-looking
        # answer produced by a completely different verb: NP_STAT is the only
        # verb reached through `ancestors_searchable` rather than
        # `path_allows`, so the one arm this tool could not exercise was the
        # one that most needed a foreign observer.
        np_msg = build_frame(NP_STAT, 0, [len(pb)], pb)
    else:
        print(f"unknown op {op!r}", file=sys.stderr)
        sys.exit(2)
    # one_op signs, sends, reads the reply, and verifies the reply MAC (reply-auth).
    status, data = one_op(host, port, np_msg)

    if status == REPLY_UNVERIFIED:
        print("status: REPLY NOT VERIFIED (the export answered, but not with the "
              "signature we expect from the host we dialled)")
        sys.exit(1)
    if status == FS_ERR_AUTH:
        # The wire cannot say WHICH half failed, and deliberately so - telling a
        # caller "the key was fine, the name was wrong" would enumerate accounts.
        print("status: AUTH FAILED (export refused our key, our --user name, "
              "or the frame format)")
        sys.exit(1)
    if status == NO_FS:
        print("status: NO FILESYSTEM on the export (its disk is not mounted, or "
              "fsd is restarting) - this one is worth retrying")
        sys.exit(1)
    if status == FS_ERR_NOT_FOUND:
        print("status: NOT FOUND on the export - for a whole request this means "
              "it has no readable /etc/passwd to resolve our name, which will "
              "not clear by itself")
        sys.exit(1)
    if status >= FS_ERR_MIN:
        print(f"status: ERROR 0x{status:016x}")
        sys.exit(1)
    print(f"status: {status}")
    if op == "readdir":
        print("entries:")
        sys.stdout.write(data.decode("latin1"))
    elif op == "stat":
        # status is STAT_INFO_LEN on success; the record is in `data`.
        if len(data) < STAT_INFO_LEN:
            print(f"status: {status}; short stat record ({len(data)} bytes)")
            sys.exit(1)
        size = int.from_bytes(data[0:8], "little")
        flags = int.from_bytes(data[8:12], "little")
        kind = "dir" if flags & 1 else "file"
        line = f"{kind}  size={size}"
        if data[19]:
            year = int.from_bytes(data[12:14], "little")
            line += (f"  {year:04d}-{data[14]:02d}-{data[15]:02d} "
                     f"{data[16]:02d}:{data[17]:02d}:{data[18]:02d}")
        if data[26]:
            mode = int.from_bytes(data[20:22], "little")
            uid = int.from_bytes(data[22:24], "little")
            gid = int.from_bytes(data[24:26], "little")
            line += f"  mode={mode:04o} uid={uid} gid={gid}"
        else:
            line += "  (filesystem records no mode)"
        print(line)
    else:
        sys.stdout.buffer.write(data)
        sys.stdout.flush()


if __name__ == "__main__":
    main()

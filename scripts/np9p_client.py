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

The export now REQUIRES cluster authentication (the export-hardening phase):
every request carries an auth header `[magic:8][nonce:16][name:32][mac:32]` in
front of the NP message, where `mac = HMAC-SHA256(cluster_key, nonce || name ||
np)` and `name` is the user the request is made on behalf of. This client
signs with the shared dev key by default; pass `--key <k>` to use another (e.g.
to prove the export refuses a wrong key). The key must match the guest's
`\CLUSTER.KEY` (Makefile `CLUSTER_KEY`, default below).

e.g. after `make run-image-9p`:
    python3 scripts/np9p_client.py localhost 5640 readdir /
    python3 scripts/np9p_client.py localhost 5640 read /EFI/ORBS/INIT.CFG
    python3 scripts/np9p_client.py localhost 5640 readdir / --key wrong  # -> AUTH error
    python3 scripts/np9p_client.py localhost 5640 read /etc/shadow --user user  # -> refused
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
NP_WRITE_FILE = NP_BASE + 11
NP_WRITE_AT = NP_BASE + 4
NP_READ_AT = NP_BASE + 10
FS_ERR_MIN = (1 << 64) - 64  # errors are a small band just below u64::MAX
FS_ERR_AUTH = (1 << 64) - 1 - 30  # u64::MAX - 30

# Cluster auth wire constants (ninep-abi). Must match the guest's CLUSTER.KEY.
NP_AUTH_MAGIC = int.from_bytes(b"AUTHNP02", "big")  # ninep-abi NP_AUTH_MAGIC
NP_AUTH_MAGIC_SIGNED = int.from_bytes(b"AUTHNP03", "big")  # the per-machine-keypair format
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
DEFAULT_KEY = b"ouroboros-dev-cluster-key-v1"  # Makefile CLUSTER_KEY default

# WHICH MACHINE KEY this client signs with, when signing (--sign).
#
# THIS SCRIPT IS THE FOREIGN OBSERVER FOR STEP 7. The guest has no signing client
# yet, so the only thing that can prove its verifier works is a signer that
# shares none of its code - a Python one, against a Rust one, agreeing about a
# format neither of them can quietly redefine.
SIGN_KEY = None  # set by --sign; the dev seed for the "host" peer


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
PEER_KEY = None


def build_frame(verb, tree, params, payload):
    # The bare NP message: [verb u64][tree u64][a0..a3 u64][payload].
    hdr = struct.pack("<Q", verb) + struct.pack("<Q", tree)
    for i in range(4):
        hdr += struct.pack("<Q", params[i] if i < len(params) else 0)
    return hdr + payload


def signed_frame(np_msg, seed, user=None):
    """[u32 len][magic:8][nonce:16][name:32][pubkey:32][sig:64][np]

    The signature covers nonce || name || np - the same bytes the MAC covers, so
    the only thing that changed is who can produce the authenticator. The public
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
    body = struct.pack("<Q", NP_AUTH_MAGIC_SIGNED) + nonce + name + public + sig + np_msg
    return struct.pack("<I", len(body)) + body, nonce


def sign_frame(np_msg, key, user=None):
    # [u32 len][magic:8][nonce:16][name:32][mac:32][np]; len = bytes after the
    # prefix. mac = HMAC-SHA256(key, nonce || name || np) - the name is INSIDE
    # the MAC, so the guest can trust which user is asking. Returns (frame,
    # nonce); the caller verifies the reply's MAC against the same nonce
    # (reply-auth).
    nonce = os.urandom(NP_NONCE_LEN)
    user = USER if user is None else user
    if len(user) > NP_NAME_LEN:
        sys.exit(f"user name too long (max {NP_NAME_LEN})")
    name = user.ljust(NP_NAME_LEN, b"\0")
    mac = hmac.new(key, nonce + name + np_msg, hashlib.sha256).digest()
    auth = struct.pack("<Q", NP_AUTH_MAGIC) + nonce + name + mac
    body = auth + np_msg
    return struct.pack("<I", len(body)) + body, nonce


def recv_reply(sock, key, nonce, expect_signed=False, peer_key=None):
    # The server frames [u32 len][mac:32][status u64][data] then FINs (reply-auth).
    # Verify mac = HMAC(key, req_nonce || [status][data]) before trusting a byte;
    # a failure (tamper, or a wrong/denied key) -> FS_ERR_AUTH.
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
    if expect_signed:
        # We signed the request, so the reply is signed - and it must verify
        # against the key expected for the HOST WE DIALLED, not against any key
        # we happen to authorize. Accepting the latter would authenticate "some
        # cluster member" rather than "the machine I asked".
        if len(body) < NP_SIG_LEN + 8:
            return FS_ERR_AUTH, b""
        sig, np = body[:NP_SIG_LEN], body[NP_SIG_LEN:]
        if peer_key is None or not load_ed_reference().verify(peer_key, SIG_DOMAIN_REPLY + nonce + np, sig):
            return FS_ERR_AUTH, b""
        (status,) = struct.unpack("<Q", np[:8])
        return status, np[8:]
    if len(body) < 32 + 8:
        return FS_ERR_AUTH, b""  # too short to be a sealed reply (or a denial)
    reply_mac, np = body[:32], body[32:]
    if not hmac.compare_digest(reply_mac, hmac.new(key, nonce + np, hashlib.sha256).digest()):
        return FS_ERR_AUTH, b""  # reply not authenticated
    (status,) = struct.unpack("<Q", np[:8])
    data = np[8:]
    return status, data


def one_op(host, port, key, np_msg, timeout=10):
    """Run one authenticated NP op over a fresh export connection; return (status, data)."""
    if SIGN_KEY is not None:
        frame, nonce = signed_frame(np_msg, SIGN_KEY)
    else:
        frame, nonce = sign_frame(np_msg, key)
    with socket.create_connection((host, port), timeout=timeout) as s:
        s.sendall(frame)
        status, data = recv_reply(
            s, key, nonce,
            expect_signed=SIGN_KEY is not None,
            peer_key=PEER_KEY,
        )
    return status, data


def np_readfile(host, port, key, path, want=512):
    pb = path.encode()
    return one_op(host, port, key, build_frame(NP_READ_FILE, 0, [len(pb), want], pb))


def np_writefile(host, port, key, path, data):
    pb = path.encode()
    # NP_WRITE_FILE: a0 = path len, a1 = data len; payload = path ++ data.
    msg = build_frame(NP_WRITE_FILE, 0, [len(pb), len(data)], pb + data)
    return one_op(host, port, key, msg)


def do_dial(host, port, key, dst_ip, dst_port, request):
    """Drive the GUEST's /net/tcp to dial dst_ip:dst_port out of ITS nic, over the
    export - "use the guest's network from here". Prints the response bytes."""
    import time
    base = "/net/tcp"
    st, data = np_readfile(host, port, key, base + "/clone")
    if st == FS_ERR_AUTH:
        print("status: AUTH FAILED"); sys.exit(1)
    if st >= FS_ERR_MIN or not data.strip().isdigit():
        print(f"clone failed (status 0x{st:016x})"); sys.exit(1)
    n = int(data.strip())
    print(f"[clone] connection {n}")
    st, _ = np_writefile(host, port, key, f"{base}/{n}/ctl", f"connect {dst_ip}!{dst_port}".encode())
    if st >= FS_ERR_MIN:
        print(f"connect failed (status 0x{st:016x})"); sys.exit(1)
    # Poll status until Established.
    for _ in range(50):
        st, data = np_readfile(host, port, key, f"{base}/{n}/status", want=16)
        s = data.split(b"\n")[0].decode("latin1", "replace")
        if s.startswith("Established"):
            print(f"[status] {s}"); break
        if s.startswith("Closed"):
            print("[status] Closed - refused/unreachable"); sys.exit(1)
        time.sleep(0.1)
    else:
        print("connect timed out"); sys.exit(1)
    if request:
        np_writefile(host, port, key, f"{base}/{n}/data", request)
    print("[response]")
    got = b""
    empties = 0
    for _ in range(400):
        st, data = np_readfile(host, port, key, f"{base}/{n}/data", want=512)
        if st >= FS_ERR_MIN:
            break
        if data:
            got += data; empties = 0
            continue
        st, sdata = np_readfile(host, port, key, f"{base}/{n}/status", want=16)
        if sdata.split(b"\n")[0].startswith(b"Closed"):
            break
        empties += 1
        if empties > 40:
            break
        time.sleep(0.05)
    np_writefile(host, port, key, f"{base}/{n}/ctl", b"close")
    sys.stdout.buffer.write(got)
    sys.stdout.flush()
    print(f"\n[done] {len(got)} bytes received")


def do_serve(host, port, key, announce_port, extern_port, response):
    """Drive the GUEST's /net/tcp DIAL-IN over the export: announce a port on the
    guest's NIC, then a host socket connects to the guest at that port (via the
    hostfwd `extern_port`) as the external client; the guest accepts, we relay
    the request/response over the export, and the external client sees the reply.
    Proves "accept inbound on the guest's network, served from here.\""""
    import time
    base = "/net/tcp"
    st, data = np_readfile(host, port, key, base + "/clone")
    if st >= FS_ERR_MIN or not data.strip().isdigit():
        print(f"clone failed (0x{st:016x})"); sys.exit(1)
    n = int(data.strip())
    print(f"[clone] listener {n}")
    st, _ = np_writefile(host, port, key, f"{base}/{n}/ctl", f"announce {announce_port}".encode())
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
        st, data = np_readfile(host, port, key, f"{base}/{n}/listen", want=16)
        if st < FS_ERR_MIN and data.strip().isdigit():
            m = int(data.strip()); break
        time.sleep(0.1)
    if m is None:
        print("no connection accepted (timeout)"); sys.exit(1)
    print(f"[listen] accepted connection {m}")

    # Read the request the external client sent (relayed through the guest).
    got_req = b""
    for _ in range(50):
        st, data = np_readfile(host, port, key, f"{base}/{m}/data", want=512)
        if st < FS_ERR_MIN and data:
            got_req += data; break
        time.sleep(0.05)
    print(f"[request] guest relayed: {got_req!r}")

    # Respond, then close.
    np_writefile(host, port, key, f"{base}/{m}/data", response)
    np_writefile(host, port, key, f"{base}/{m}/ctl", b"close")

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
    np_writefile(host, port, key, f"{base}/{n}/ctl", b"close")  # stop listening
    print(f"[external] received: {got_resp!r}")
    if got_resp.strip() == response.strip():
        print("[OK] external client got the response served from the export side")
    else:
        print("[FAIL] response mismatch"); sys.exit(1)


def main():
    # Pull an optional `--key <k>` out of argv (anywhere after the fixed args).
    key = DEFAULT_KEY
    args = sys.argv[1:]
    if "--key" in args:
        i = args.index("--key")
        key = args[i + 1].encode()
        del args[i:i + 2]
    # `--user <name>`: who this request claims to be (default root). The guest
    # applies ITS permission model to that name, so this is what makes an
    # unprivileged remote read testable from the host.
    # `--sign [seed-label]`: use the SIGNED frame format (per-machine keypairs)
    # instead of the shared-key MAC. Defaults to the dev "host" identity, which
    # is what `mkclusterkeys.py` puts in every image's authorized file; pass a
    # label to sign with a key the guest does NOT authorize.
    # `--sign` alone uses the dev "host" identity; `--sign=<label>` uses another
    # seed label, which is how a key the guest does NOT authorize is tested.
    #
    # NOT `--sign <label>` with a heuristic: guessing whether the next token is a
    # label or a positional silently swallowed arguments. `read --sign FILE 0 100`
    # signed with the label "FILE" and read the path "0" - a test that would have
    # "proved" a refusal for entirely the wrong reason.
    global SIGN_KEY
    for i, a in enumerate(list(args)):
        if a == "--sign":
            SIGN_KEY = dev_seed("ouroboros-dev-host-peer")
            del args[i]
            break
        if a.startswith("--sign="):
            SIGN_KEY = dev_seed(a[len("--sign="):])
            del args[i]
            break
    # `--peer=<label>`: whose signature to expect on the reply.
    global PEER_KEY
    for i, a in enumerate(list(args)):
        if a.startswith("--peer="):
            PEER_KEY = load_ed_reference().public_key(dev_seed(a[len("--peer="):]))
            del args[i]
            break
    if PEER_KEY is None:
        PEER_KEY = load_ed_reference().public_key(dev_seed("ouroboros-dev-node-a"))
    if "--user" in args:
        global USER
        i = args.index("--user")
        USER = args[i + 1].encode()
        del args[i:i + 2]
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
        do_serve(host, port, key, announce_port, extern_port, response)
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
            status, data = one_op(host, port, key, np)
            print(f"guest answered status=0x{status:x} ({len(data)} bytes) - it survived")
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
        do_dial(host, port, key, dst_ip, dst_port, request)
        return

    path = args[3]
    pb = path.encode()

    if op == "readdir":
        np_msg = build_frame(NP_READDIR, 0, [len(pb), 4096], pb)
    elif op == "read":
        offset = int(args[4]) if len(args) > 4 else 0
        want = int(args[5]) if len(args) > 5 else 4096
        np_msg = build_frame(NP_READ, 0, [len(pb), offset, want], pb)
    elif op == "stat":
        np_msg = build_frame(NP_READ_FILE, 0, [len(pb), 1], pb)
    else:
        print(f"unknown op {op!r}", file=sys.stderr)
        sys.exit(2)
    # one_op signs, sends, reads the reply, and verifies the reply MAC (reply-auth).
    status, data = one_op(host, port, key, np_msg)

    if status == FS_ERR_AUTH:
        # The wire cannot say WHICH half failed, and deliberately so - telling a
        # caller "the key was fine, the name was wrong" would enumerate accounts.
        print("status: AUTH FAILED (export refused the cluster key or the --user name)")
        sys.exit(1)
    if status >= FS_ERR_MIN:
        print(f"status: ERROR 0x{status:016x}")
        sys.exit(1)
    print(f"status: {status}")
    if op == "readdir":
        print("entries:")
        sys.stdout.write(data.decode("latin1"))
    elif op == "stat":
        print(f"size: {status} bytes; first byte(s): {data!r}")
    else:
        sys.stdout.buffer.write(data)
        sys.stdout.flush()


if __name__ == "__main__":
    main()

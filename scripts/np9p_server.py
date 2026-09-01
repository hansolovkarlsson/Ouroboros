#!/usr/bin/env python3
"""A minimal host-side 9P-over-TCP *server* for testing Ouroboros's remote-mount
client (cluster Phase 1c). It's the mirror of `np9p_client.py`: that one lets the
host read a guest's exported filesystem; this one lets a *guest* remote-mount and
read a filesystem served by the host - the "foreign observer" that verifies the
client routing (netd's NETOP_RMOUNT -> TCP -> this server), per
docs/roadmap-cluster-phase1.md's step 1c.

It serves a small fixed in-memory tree over the length-delimited `ninep-abi`
frame, one request/reply per connection then FIN (matching the guest export
gateway's `Connection: close` shape, which the guest's `tcp_get`-based client
reads to EOF).

Usage (host):
    python3 scripts/np9p_server.py [port]        # default 5641

Then in the guest (SLIRP maps the host to 10.0.2.2):
    mount -r 10.0.2.2:5641 /mnt/a
    ls /mnt/a
    cat /mnt/a/HELLO.TXT
    ls /mnt/a/SUB
    cat /mnt/a/SUB/NOTE.TXT
"""
import hashlib
import os
import socket
import struct
import sys

NP_BASE = 0x100
NP_READDIR = NP_BASE + 0
NP_READ_FILE = NP_BASE + 1
NP_READ = NP_BASE + 2
NP_READ_AT = NP_BASE + 10

FS_ERROR = (1 << 64) - 1
NO_FS = (1 << 64) - 2
FS_ERR_AUTH = (1 << 64) - 1 - 30  # u64::MAX - 30
# A stand-in for fsd's FS_ERR_NOT_FOUND band; any value >= FS_ERR_MIN reads as an
# error to the client. Use FS_ERROR for "no such path".
HDR = 48  # NP_REQ_PAYLOAD

# Cluster auth: the guest SIGNS every request with its per-machine Ed25519 key,
# so this server verifies a signature and an authorized public key before
# serving anything. The shared \CLUSTER.KEY it used to verify a MAC against
# authenticates nothing since the flag day.
NP_AUTH_MAGIC_SIGNED = int.from_bytes(b"AUTHNP03", "big")  # per-machine keypairs
NP_NAME_LEN = 32  # requesting user's name, NUL-padded
NP_KEY_LEN = 32
NP_SIG_LEN = 64
# Signature DOMAIN TAGS - must match ninep-abi's SIG_DOMAIN_* byte for byte.
# They keep a signature made in one role from verifying in the other: without
# them a captured reply signature is structurally a valid request signature from
# the same key.
SIG_DOMAIN_REQUEST = b"ouroboros-cluster-request-v1\0"
SIG_DOMAIN_REPLY = b"ouroboros-cluster-reply-v1\0"

NP_AUTH_HDR_SIGNED = 8 + 16 + NP_NAME_LEN + NP_KEY_LEN + NP_SIG_LEN

# Public keys this server accepts, by hex. The dev cluster's three identities,
# derived the same way scripts/mkclusterkeys.py derives them - so a guest built
# from this tree is authorized here without any copying.
#
# THIS SERVER IS THE FOREIGN OBSERVER FOR THE CLIENT HALF. The guest signing its
# own requests is checked by something that shares none of its code; a guest
# whose signatures only its own exporter verifies is a closed loop.
_ED = None


def ed():
    """The Ed25519 reference, memoized. Asserts itself against RFC 8032 on load."""
    global _ED
    if _ED is None:
        import importlib.util
        here = os.path.dirname(os.path.abspath(__file__))
        spec = importlib.util.spec_from_file_location("edref", os.path.join(here, "gen-sign-vectors.py"))
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        _ED = mod
    return _ED


_AUTHORIZED = None


def dev_authorized():
    """The dev peers' public keys, as bytes. Memoized: deriving them is three
    scalar multiplications in a naive pure-Python reference, and doing that per
    request sits squarely inside the ~1s budget the guest allows for a reply -
    the budget this rig was just found to be marginal against."""
    global _AUTHORIZED
    if _AUTHORIZED is None:
        labels = ["ouroboros-dev-node-a", "ouroboros-dev-node-b", "ouroboros-dev-host-peer"]
        _AUTHORIZED = {ed().public_key(hashlib.sha256(l.encode()).digest()) for l in labels}
    return _AUTHORIZED


def verify_signed(body):
    """Verify an AUTHNP03 frame: [magic][nonce][name][pubkey][sig][np].

    The offered key must be one this server authorizes BEFORE the signature is
    checked - the same order netd uses, and for the same reason: a valid
    signature by a key nobody authorized is exactly as unwelcome as an invalid
    one.
    """
    if len(body) < NP_AUTH_HDR_SIGNED:
        return None
    nonce = body[8:24]
    name = body[24 : 24 + NP_NAME_LEN]
    koff = 24 + NP_NAME_LEN
    pub = body[koff : koff + NP_KEY_LEN]
    sig = body[koff + NP_KEY_LEN : NP_AUTH_HDR_SIGNED]
    np = body[NP_AUTH_HDR_SIGNED:]
    if pub not in dev_authorized():
        return None
    if not ed().verify(pub, SIG_DOMAIN_REQUEST + nonce + name + np, sig):
        return None
    return np, nonce


# The host peer's own identity - the "host" dev key, which every image's
# authorized file already lists, so a guest built from this tree accepts its
# signed replies without any copying.
def host_seed():
    return hashlib.sha256(b"ouroboros-dev-host-peer").digest()


def warm_up():
    """Do the slow Ed25519 work BEFORE accepting connections.

    This reference is deliberately naive - affine arithmetic with a modular
    inversion per point addition - which costs about 0.2s to sign, 0.3s to
    verify, and a further second to import (its RFC 8032 self-assertions are two
    full signatures). Paid lazily, that lands on the FIRST client request and
    pushes it past what the guest's `tcp_get` will wait for: the guest reports
    "no filesystem", which looks like a protocol failure and is a stopwatch.
    Paid here, the first request costs the same as every other.
    """
    seed = host_seed()
    sig = ed().sign(seed, b"warm")
    assert ed().verify(ed().public_key(seed), b"warm", sig), "reference disagrees with itself"
    return dev_authorized()


def verify(body):
    """Strip + verify the auth header; return (NP message, nonce), or None. The
    nonce is what the reply is signed against (reply-auth).

    ONE FORMAT. The retired shared-key MAC'd format (`AUTHNP02`) was accepted
    here until the flag day; a frame carrying it is now refused like any other
    unknown magic, which is what makes this peer able to show that the GUEST
    refuses one too rather than merely assuming so."""
    if len(body) < 8:
        return None
    (magic,) = struct.unpack("<Q", body[:8])
    if magic != NP_AUTH_MAGIC_SIGNED:
        return None
    return verify_signed(body)

# The served tree. Directories map a path to a list of (name, is_dir); files map
# a path to bytes. HELLO.TXT is deliberately > NP_REMOTE_CHUNK (512) so a guest
# `cat` exercises the multi-round-trip chunked-read loop over the network.
_HELLO = b"".join(
    (b"line %03d: hello from the host 9P server over TCP\n" % i) for i in range(40)
)
DIRS = {
    b"/": [(b"HELLO.TXT", False), (b"SUB", True)],
    b"/SUB": [(b"NOTE.TXT", False)],
}
FILES = {
    b"/HELLO.TXT": _HELLO,
    b"/SUB/NOTE.TXT": b"a nested file, read remotely\n",
}


def listing(path):
    """fsd's readdir format: `name\\n` for a file, `name/\\n` for a directory."""
    entries = DIRS.get(path)
    if entries is None:
        return None
    out = b""
    for name, is_dir in entries:
        out += name + (b"/\n" if is_dir else b"\n")
    return out


def read_frame(sock):
    """Read one [u32 len][body] frame; return body bytes (or None on EOF)."""
    hdr = b""
    while len(hdr) < 4:
        chunk = sock.recv(4 - len(hdr))
        if not chunk:
            return None
        hdr += chunk
    (flen,) = struct.unpack("<I", hdr)
    body = b""
    while len(body) < flen:
        chunk = sock.recv(flen - len(body))
        if not chunk:
            return None
        body += chunk
    return body


def frame_reply(status, data=b""):
    body = struct.pack("<Q", status) + data
    return struct.pack("<I", len(body)) + body


def serve_request(body):
    # Authenticate first: reject an unauthorized key, a bad signature or a
    # retired-format frame before serving any verb - the guest surfaces
    # FS_ERR_AUTH. A denial is unsealed (the client's reply-verify fails ->
    # auth error anyway).
    verified = verify(body)
    if verified is None:
        return frame_reply(FS_ERR_AUTH)
    body, nonce = verified

    # Every real reply is SEALED (reply-auth): [u32 len][sig:64][status][data],
    # signed over `domain-tag || request_nonce || [status][data]` with this
    # peer's own key, which every image's authorized file lists at 10.0.2.2.
    def sealed(status, data=b""):
        inner = struct.pack("<Q", status) + data
        seal = ed().sign(host_seed(), SIG_DOMAIN_REPLY + nonce + inner)
        framed = seal + inner
        return struct.pack("<I", len(framed)) + framed

    if len(body) < HDR:
        return sealed(FS_ERROR)
    verb, tree = struct.unpack("<QQ", body[:16])
    a0, a1, a2, a3 = struct.unpack("<QQQQ", body[16:48])
    payload = body[HDR:]
    path = payload[: min(a0, len(payload))]

    if verb == NP_READDIR:
        out = listing(path)
        if out is None:
            return sealed(FS_ERROR)
        want = a1
        out = out[:want]
        return sealed(len(out), out)

    if verb in (NP_READ, NP_READ_AT):
        data = FILES.get(path)
        if data is None:
            return sealed(FS_ERROR)
        offset, want = a1, a2
        chunk = data[offset : offset + want]
        return sealed(len(chunk), chunk)

    if verb == NP_READ_FILE:
        data = FILES.get(path)
        if data is None:
            return sealed(FS_ERROR)
        want = a1
        return sealed(len(data), data[:want])

    # Any mutate/unknown verb: the export is read-only.
    return sealed(FS_ERROR)


def main():
    args = sys.argv[1:]
    port = int(args[0]) if args else 5641
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", port))
    # Warm the signer BEFORE listening, not just before the first request: a
    # client connecting during the ~1s warm-up would otherwise wait it out.
    peers = warm_up()
    srv.listen(8)
    print(f"9P test server listening on 0.0.0.0:{port} "
          f"(guest reaches it at 10.0.2.2:{port} over SLIRP)")
    print(f"  signing replies as the dev 'host' identity; {len(peers)} peer key(s) authorized")
    while True:
        conn, addr = srv.accept()
        try:
            body = read_frame(conn)
            if body is None:
                continue
            reply = serve_request(body)
            conn.sendall(reply)
            # One request/reply per connection, then FIN - the guest client reads
            # to EOF (Connection: close shape).
            try:
                conn.shutdown(socket.SHUT_WR)
            except OSError:
                pass
        except (ConnectionError, OSError) as e:
            print(f"  [conn {addr}] {e}")
        finally:
            conn.close()


if __name__ == "__main__":
    main()

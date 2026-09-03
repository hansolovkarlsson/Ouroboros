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

VERBS SERVED, stated because the gap is otherwise invisible: NP_READDIR,
NP_STAT, NP_READ / NP_READ_AT, NP_READ_FILE. Every other verb - including all
of the mutating ones - gets FS_ERROR, since the export is read-only.

An unimplemented verb does NOT surface as "unsupported" at the guest. It
surfaces as whatever the *command* makes of FS_ERROR, which is usually "no such
file or directory" - a message about a path, for a request whose path was fine.
That cost a real debugging session: `ls` stats its target before listing it, so
a missing NP_STAT made every `ls` of a remote mount fail while `cat` under the
same mount worked, and the symptom was recorded in the roadmap for days as a
guest-side path-resolution bug. If a guest command fails here for a reason that
makes no sense, check this list before suspecting the guest.
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
NP_STAT = NP_BASE + 12

# `NP_STAT`'s reply: status = STAT_INFO_LEN, payload = a fixed 27-byte StatInfo.
# The offsets are `ninep-abi`'s STAT_* constants; keep them in step with it.
# `ls` STATS ITS TARGET BEFORE LISTING IT, so a peer without this verb makes
# every `ls` of a remote mount fail as "no such file or directory" while `cat`
# of a file under the same mount works - which is exactly how the bug this
# implements away was recorded, as a path-resolution fault in the guest.
STAT_INFO_LEN = 27
STAT_SIZE_OFF = 0
STAT_FLAGS_OFF = 8
STAT_TIMEVALID_OFF = 19
STAT_MODEVALID_OFF = 26
STAT_FLAG_DIR = 1 << 0

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
# The dev cluster's identities, by the short name an `authorized` line carries,
# mapped to the seed label `scripts/mkclusterkeys.py` derives that key from.
#
# SPELLED ONCE, AND CHECKED. These labels used to be a literal list inside
# `dev_authorized()` and a second literal inside `host_seed()`, neither of which
# `check-wire-constants.py` could see - it compared `mkclusterkeys.py` against
# `np9p_client.py` only. Renaming a dev identity therefore passed the check and
# broke this server silently, so `make run-image-9p-client` failed every request
# with a bare FS_ERR_AUTH that reads as a guest-side crypto bug.
DEV_PEER_LABELS = {
    "node-a": "ouroboros-dev-node-a",
    "node-b": "ouroboros-dev-node-b",
    "host": "ouroboros-dev-host-peer",
}

NP_NONCE_LEN = 16  # fresh per-request value the reply signature is bound to
NP_NAME_LEN = 32  # requesting user's name, NUL-padded
NP_PUBKEY_LEN = 32  # named as ninep-abi and np9p_client.py name it - see below
NP_SIG_LEN = 64
# Signature DOMAIN TAGS - must match ninep-abi's SIG_DOMAIN_* byte for byte.
# They keep a signature made in one role from verifying in the other: without
# them a captured reply signature is structurally a valid request signature from
# the same key.
SIG_DOMAIN_REQUEST = b"ouroboros-cluster-request-v1\0"
SIG_DOMAIN_REPLY = b"ouroboros-cluster-reply-v1\0"

# NAMES MATTER HERE, not just values: scripts/check-wire-constants.py compares
# these against ninep-abi BY NAME, so a constant this file spells differently is
# silently skipped rather than checked. The public-key length was spelled with a
# private name and the nonce as a bare literal, so the two fields that decide
# which guests are served were the two the cross-language check never looked at:
# changing that length to 33 shifted every slice below and the checker still
# reported "10 constants agree".
NP_AUTH_HDR_SIGNED = 8 + NP_NONCE_LEN + NP_NAME_LEN + NP_PUBKEY_LEN + NP_SIG_LEN

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
        _AUTHORIZED = {
            ed().public_key(hashlib.sha256(l.encode()).digest())
            for l in DEV_PEER_LABELS.values()
        }
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
    noff = 8 + NP_NONCE_LEN  # past the magic and the nonce
    # SLICED BY THE CONSTANT, not by 8:24. This was the one field still spelled
    # as literals, on the line above a comment asserting the parameterisation it
    # did not have - and it is the value fed straight into the signature and
    # reused for the reply. Change NP_NONCE_LEN everywhere and the header, name,
    # key and signature offsets all follow while this kept reading 16 bytes, so
    # every correctly-keyed guest would be refused and check-wire-constants.py
    # would still report agreement.
    nonce = body[8:noff]
    name = body[noff : noff + NP_NAME_LEN]
    koff = noff + NP_NAME_LEN
    pub = body[koff : koff + NP_PUBKEY_LEN]
    sig = body[koff + NP_PUBKEY_LEN : NP_AUTH_HDR_SIGNED]
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
    return hashlib.sha256(DEV_PEER_LABELS["host"].encode()).digest()


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


def stat_info(path):
    """A 27-byte StatInfo for a path in the served tree, or None if absent.

    `time` and `mode` are left INVALID (their valid-flag bytes stay 0), which
    is what `fsd` itself reports for a filesystem that cannot model them
    (FAT32, exFAT, `/proc`) - so `ls -l` shows a size and a type here and
    omits the columns this peer has no answer for, rather than inventing one.
    """
    if path in DIRS:
        size, is_dir = 0, True
    elif path in FILES:
        size, is_dir = len(FILES[path]), False
    else:
        return None
    info = bytearray(STAT_INFO_LEN)
    info[STAT_SIZE_OFF:STAT_SIZE_OFF + 8] = struct.pack("<Q", size)
    flags = STAT_FLAG_DIR if is_dir else 0
    info[STAT_FLAGS_OFF:STAT_FLAGS_OFF + 4] = struct.pack("<I", flags)
    info[STAT_TIMEVALID_OFF] = 0
    info[STAT_MODEVALID_OFF] = 0
    return bytes(info)


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

    if verb == NP_STAT:
        info = stat_info(path)
        if info is None:
            return sealed(FS_ERROR)
        return sealed(STAT_INFO_LEN, info)

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

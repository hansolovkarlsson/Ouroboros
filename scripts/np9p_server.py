#!/usr/bin/env python3
"""A minimal host-side 9P-over-TCP *server* for testing Ouroboros's remote-mount
client (cluster Phase 1c). It's the mirror of `np9p_client.py`: that one lets the
host read a guest's exported filesystem; this one lets a *guest* remote-mount and
read a filesystem served by the host - the "foreign observer" that verifies the
client routing (netd's NETOP_RMOUNT -> TCP -> this server), per
docs/roadmap/roadmap-cluster-phase1.md's step 1c.

It serves a small fixed in-memory tree over the length-delimited `ninep-abi`
frame, one request/reply per connection then FIN (matching the guest export
gateway's `Connection: close` shape, which the guest's `tcp_get`-based client
reads to EOF).

Usage (host):
    python3 scripts/np9p_server.py [port]
    python3 scripts/np9p_server.py --self-test [-v]   # verb dispatch + a fid round trip        # default 5641

Then in the guest (SLIRP maps the host to 10.0.2.2):
    mount -r 10.0.2.2:5641 /mnt/a
    ls /mnt/a
    cat /mnt/a/HELLO.TXT
    ls /mnt/a/SUB
    cat /mnt/a/SUB/NOTE.TXT

VERBS SERVED - the path verbs NP_READDIR, NP_STAT, NP_READ and NP_READ_AT (one
arm, same handling), NP_READ_FILE; and since 2026-09-05 the READ-CAPABLE FID
verbs NP_OPEN / NP_PREAD / NP_FSTAT / NP_CLUNK, so a C program's open/read/fstat
works over a remote mount pointed at this peer. A known verb refused on policy
gets FS_ERR_READ_ONLY (the mutating verbs, plus NP_PWRITE and an NP_OPEN asking
for write/create/truncate - this export is read-only); anything else gets
FS_ERR_NO_SUCH_VERB.

**netd's export serves NP_OPEN / NP_FSTAT / NP_CLUNK since step 5 and NP_PREAD
since step 6 (both 2026-09-12), on a session only, and NOT YET NP_PWRITE**
(step 7 of docs/roadmap/roadmap-fid-verbs.md). This peer served the read half
on any connection before the guest did, which is the right way round for a
foreign observer: the client was built and checked against something that
already answered. One difference is deliberate and worth
knowing when a result here and there disagree: this peer keys fids on nothing
(one global table, any connection may use any fid), where netd keys them on
the SESSION, so a fid opened on one connection is refused on another there
and served here. `np9p_client.py fid-gate` measures netd's rule.

DO NOT TRUST THIS PARAGRAPH. `--self-test` compares the dispatch chain against
SELF_TEST_VERBS on every `make test`; that table is the checked claim, this is
prose.

This list is hand-maintained prose and nothing compares it to the dispatch
chain; `serve_request`'s fallthrough is the authority, and it PRINTS the verb
number it refused.

An unimplemented verb DOES now surface as itself at the guest ("that server
does not implement this request"), since 2026-09-05. It used to surface as
whatever the *command* made of FS_ERROR, which is usually "no such file or
directory" - a message about a path, for a request whose path was fine. That
cost a real debugging session: `ls` stats a named operand before listing it, so
a missing NP_STAT made `ls /mnt/a` fail while `cat` under the same mount worked,
and the symptom was recorded in the roadmap for days as a guest-side
path-resolution bug. The status code says THAT a verb is missing and cannot say
WHICH, so the fallthrough still prints the number - read it before suspecting
the guest.
"""
import contextlib
import hashlib
import hmac
import io
import os
import socket
import time
import struct
import sys
import threading

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import x25519_ref  # noqa: E402  RFC 7748's ladder, asserted against its vectors on load

NP_BASE = 0x100
NP_READDIR = NP_BASE + 0
NP_READ_FILE = NP_BASE + 1
NP_READ = NP_BASE + 2
NP_READ_AT = NP_BASE + 10
NP_STAT = NP_BASE + 12
NP_CLUNK = NP_BASE + 19
NP_LIMIT = NP_CLUNK + 1  # ninep-abi: one past the last defined verb

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

# WHICH ERROR CODE, and why it is not all one value. `FS_ERROR` is the generic
# "something went wrong"; the guest renders it as "failed". `FS_ERR_NOT_FOUND`
# means DEFINITIVELY ABSENT, and `ulib::fs_presence` branches on exactly that
# value to answer Absent rather than Unknown - which is what `mv`/`cp`'s
# destructive-overwrite guard consumes. Answering FS_ERROR for an absent path
# therefore does not merely print a vaguer message: it switches that guard into
# its "could not tell" arm for a path this peer knows for certain is not there.
#
# This file used to answer FS_ERROR everywhere, with a note saying any value
# above FS_ERR_MIN reads as an error to the client. True, and it stopped being
# sufficient once a verb existed whose ABSENT answer is consumed as data rather
# than displayed - NP_STAT is that verb.
FS_ERROR = (1 << 64) - 1
NO_FS = (1 << 64) - 1 - 1  # u64::MAX - 1: no filesystem mounted (transient)
FS_ERR_NOT_FOUND = (1 << 64) - 1 - 2  # u64::MAX - 2: definitively absent
FS_ERR_NOT_A_FILE = (1 << 64) - 1 - 3  # u64::MAX - 3: it is a directory
FS_ERR_READ_ONLY = (1 << 64) - 1 - 29  # u64::MAX - 29
FS_ERR_AUTH = (1 << 64) - 1 - 30  # u64::MAX - 30
NP_SESSION = NP_BASE + 0x21  # open a session on this connection (Decision 3)
FS_ERR_NO_SUCH_VERB = (1 << 64) - 1 - 39  # u64::MAX - 39: this server has no arm for that verb

# The verbs this peer refuses ON POLICY (the export is read-only), as an
# EXPLICIT SET rather than "anything in [NP_BASE, NP_LIMIT)".
#
# The range test was wrong and said so confidently: NP_LIMIT is one past
# NP_CLUNK, so the five FID verbs (NP_OPEN 0x10f .. NP_CLUNK 0x113) are INSIDE
# it, and every one of them was answered "read-only filesystem" - a policy this
# peer does not have about them, for verbs it simply does not implement. The
# range only ever meant "a verb I have heard of", which is not the same
# question, and it silently absorbed each new verb ninep-abi defined.
# Fids: server-side open-file handles (ninep-abi's NP_OPEN..NP_CLUNK).
#
# THE PARAMETER LAYOUT IS NOT THE ONE EVERY OTHER PATH VERB USES, and that is
# the trap to carry into netd's export: for NP_OPEN, `a0` is the OPEN_* FLAGS
# and `a1` is the path length - the reverse of every other path-carrying verb,
# where `a0` is the path length. A generic "p0 is the path length" decode reads
# the flag word (1, 3, ...) as a length and resolves a 1-3 byte path, which
# lands somewhere plausible instead of failing.
#
# The other four carry NO PATH AT ALL - `a0` is the fid - so re-resolving a path
# per operation is not merely wasteful, there is nothing to resolve. The fid
# must remember what it was opened on.
FID_BASE = 3  # 0/1/2 stay clear of a C program's stdin/stdout/stderr
MAX_FIDS = 8  # ninep-abi MAX_FIDS, fsd's ceiling, so exhaustion behaves the same here
OPEN_READ = 1
OPEN_WRITE = 2
OPEN_CREATE = 4
OPEN_TRUNC = 8
# fid -> path. Flags are not kept: this export is read-only, so the only flag
# combination that ever gets a fid is a pure read.
FIDS = {}

MUTATING_VERBS = {
    NP_BASE + 3,   # NP_WRITE
    NP_BASE + 4,   # NP_WRITE_AT
    NP_BASE + 5,   # NP_TOUCH
    NP_BASE + 6,   # NP_MKDIR
    NP_BASE + 7,   # NP_RMDIR
    NP_BASE + 8,   # NP_RM
    NP_BASE + 9,   # NP_MV
    NP_BASE + 11,  # NP_WRITE_FILE
    NP_BASE + 13,  # NP_CHMOD
    NP_BASE + 14,  # NP_CHOWN
    NP_BASE + 17,  # NP_PWRITE - a fid write, refused for the same reason
}
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

# Keyed sessions (ninep-abi's normative block, "Keyed sessions"; step 4 of
# docs/roadmap/roadmap-session-auth.md). Declared here ahead of step 5, which
# uses them, so check-wire-constants.py pins them from the step that defines them.
NP_AUTH_MAGIC_KEYED = int.from_bytes(b"AUTHNP04", "big")
NP_SEQ_LEN = 8
NP_KEYED_TAG_LEN = 32
NP_AUTH_HDR_KEYED = 8 + NP_SEQ_LEN + NP_KEYED_TAG_LEN
NP_EPHEMERAL_LEN = 32
NP_SESSION_KEY_LEN = 32
SIG_DOMAIN_SESSION = b"ouroboros-cluster-session-v1\0"
SIG_DOMAIN_EPHEMERAL_C = b"ouroboros-cluster-eph-c-v1\0"
SIG_DOMAIN_EPHEMERAL_E = b"ouroboros-cluster-eph-e-v1\0"

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
    return np, nonce, name


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
    """Strip + verify the auth header; return (NP message, nonce, name), or
    None. The nonce is what the reply is signed against (reply-auth); the name
    is the user the request is made as, which a fid is bound to at open.

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
# ...and BIG.TXT is deliberately MUCH longer, ~28 round trips against ~5.
#
# HELLO.TXT is 1960 bytes, which `cat` reads as five remote round trips (four
# 512-byte reads at 0/512/1024/1536, then a terminating zero-length one). At the
# ~0.6 s per round trip this Python peer costs, that is ~3 s - and
# docs/ROADMAP.md already records it failing 7 of 7 for exactly that reason:
# netd stays continuously Runnable across a multi-chunk remote read, and 3 s is
# past supervisor.rs's WEDGE_TICKS (2.56 s), so the supervisor restarts it
# mid-read.
#
# So HELLO.TXT sits right ON that threshold, which makes it a poor probe: it
# fails for a timing reason that any change to per-round-trip cost can move
# either side of. BIG.TXT (13,600 bytes, 27 chunks, ~28 round trips, ~17 s) is
# far past it and stays there, so a test using it fails for a reason that does
# not depend on how fast the signing happens to be today.
_BIG = b"".join(
    (b"line %03d: a file long enough to need more connections than netd has\n" % i)
    for i in range(200)
)
DIRS = {
    b"/": [(b"HELLO.TXT", False), (b"BIG.TXT", False), (b"SUB", True)],
    b"/SUB": [(b"NOTE.TXT", False)],
}
FILES = {
    b"/HELLO.TXT": _HELLO,
    b"/BIG.TXT": _BIG,
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

    Only size and the dir flag are set. The time and mode valid-flags are left
    at the zero every other byte starts as, which is how `fsd` reports a
    filesystem that cannot model a field: `time: None` on exFAT, ext2 and
    `/proc` (FAT32 DOES decode an mtime), and `mode: None` on FAT32, exFAT and
    `/proc`. The two lists are different, which is why this says both rather
    than one triple for both halves.

    What the guest does with an absent field is NOT this peer's choice, and
    is worth knowing before reading `ls -l` output as evidence: `ls`
    SYNTHESIZES a mode string when it has none (`perm_string`, `None if is_dir
    => 0o755` else `0o644`), so a remote listing prints a plausible
    `-rw-r--r--` in the column ext2's real bits occupy. The dashes in the
    uid/gid/time columns are real absences; the mode column is not.

    The valid-flags are deliberately NOT written as an explicit `= 0`. That
    assignment cannot fail visibly - the bytearray is already zero, so 25 of
    the 27 possible offsets produce a byte-identical record - while an
    out-of-range offset raises IndexError, which `main`'s
    `except (ConnectionError, OSError)` does not catch, killing the accept
    loop. `fsd`'s own reference does `fill(0)` then only ever writes `= 1`.
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
    # Slice-assignment RESIZES a bytearray on a width mismatch rather than
    # raising, so a struct-format slip would ship a record whose length
    # disagrees with the status the caller is told - and nothing guest-side
    # compares the two (`ulib::fs_stat` returns the status without checking the
    # bytes delivered). Every sibling arm sends a MEASURED length; this is the
    # only one sending a constant, so it states the equivalence instead.
    assert len(info) == STAT_INFO_LEN, f"StatInfo width {len(info)} != {STAT_INFO_LEN}"
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


# How long a session may sit idle here before this peer closes it: a bound on
# a guest that died holding one, the same job netd's CONN_IDLE_TICKS does.
SESSION_IDLE_S = 60


def request_verb(body):
    """The NP verb inside a signed request frame, or None if it is not one."""
    if len(body) < NP_AUTH_HDR_SIGNED + 8:
        return None
    (magic,) = struct.unpack("<Q", body[:8])
    if magic != NP_AUTH_MAGIC_SIGNED:
        return None
    return struct.unpack("<Q", body[NP_AUTH_HDR_SIGNED:NP_AUTH_HDR_SIGNED + 8])[0]


def reply_status(reply):
    """The status word of a sealed `[len][sig][status][data]` reply (or of the
    unsealed 12-byte denial, whose status is FS_ERR_AUTH either way)."""
    body = reply[4:]
    if len(body) >= NP_SIG_LEN + 8:
        return struct.unpack("<Q", body[NP_SIG_LEN:NP_SIG_LEN + 8])[0]
    return struct.unpack("<Q", body[:8])[0] if len(body) >= 8 else FS_ERROR


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
    body, nonce, name = verified
    return serve_np(body, name, lambda status, data=b"": seal(nonce, status, data))


def seal(nonce, status, data=b""):
    """A SEALED reply (reply-auth): [u32 len][sig:64][status][data], signed over
    `domain-tag || request_nonce || [status][data]` with this peer's own key,
    which every image's authorized file lists at 10.0.2.2."""
    inner = struct.pack("<Q", status) + data
    sig = ed().sign(host_seed(), SIG_DOMAIN_REPLY + nonce + inner)
    framed = sig + inner
    return struct.pack("<I", len(framed)) + framed


def serve_np(body, name, sealed):
    """Serve one NP message as user `name`, answering through `sealed(status,
    data)`: a signed reply on an ordinary connection, a keyed one on a keyed
    session. One dispatch for both, so a verb cannot behave differently under
    the two formats."""
    if len(body) < HDR:
        return sealed(FS_ERROR)
    verb, tree = struct.unpack("<QQ", body[:16])
    a0, a1, a2, a3 = struct.unpack("<QQQQ", body[16:48])
    payload = body[HDR:]
    # A session: answer 0 and the connection loop in main() keeps reading
    # frames on this socket instead of shutting it down after the reply. No
    # budget here - a host peer has no MAX_CONNS - so the guest's FS_ERR_BUSY
    # arm has no counterpart in this server, and says so.
    if verb == NP_SESSION:
        print("  [session opened on this connection]", flush=True)
        return sealed(0)
    path = payload[: min(a0, len(payload))]

    if verb == NP_READDIR:
        out = listing(path)
        if out is None:
            return sealed(FS_ERR_NOT_FOUND)
        want = a1
        out = out[:want]
        return sealed(len(out), out)

    if verb == NP_STAT:
        info = stat_info(path)
        if info is None:
            return sealed(FS_ERR_NOT_FOUND)
        return sealed(STAT_INFO_LEN, info)

    if verb in (NP_READ, NP_READ_AT):
        data = FILES.get(path)
        if data is None:
            return sealed(FS_ERR_NOT_FOUND)
        offset, want = a1, a2
        chunk = data[offset : offset + want]
        return sealed(len(chunk), chunk)

    if verb == NP_READ_FILE:
        data = FILES.get(path)
        if data is None:
            return sealed(FS_ERR_NOT_FOUND)
        want = a1
        return sealed(len(data), data[:want])

    # --- fids ------------------------------------------------------------
    # NP_OPEN: a0 = OPEN_* flags, a1 = path length (see the note by FID_BASE).
    if verb == NP_BASE + 15:
        flags, plen = a0, a1
        fpath = body[HDR:HDR + plen]
        if flags & (OPEN_WRITE | OPEN_CREATE | OPEN_TRUNC):
            # Refused on POLICY, like the mutating verbs - not "no such verb".
            print(f"  [read-only: refusing NP_OPEN flags=0x{flags:x} "
                  f"path={fpath!r}]", flush=True)
            return sealed(FS_ERR_READ_ONLY)
        if fpath in DIRS:
            return sealed(FS_ERR_NOT_A_FILE)
        if fpath not in FILES:
            return sealed(FS_ERR_NOT_FOUND)
        free = next((n for n in range(FID_BASE, FID_BASE + MAX_FIDS)
                     if n not in FIDS), None)
        if free is None:
            # fsd reaps dead owners' fids first and only then fails; this peer
            # has no task table to ask, so it simply fails. A client that leaks
            # fids sees the ceiling here EARLIER than against fsd, which is the
            # safe direction for an observer.
            return sealed(FS_ERROR)
        # Bound to the USER as well as the number, as fsd binds a fid to the
        # (task, user) that opened it: a fid verb from another user answers
        # FS_ERR_PERM and the fid is kept (ninep-abi, NP_CLUNK). Before the
        # review of #130 this peer freed any known fid for any caller, so no
        # observer could see fsd's own clunk test fail.
        FIDS[free] = (fpath, name)
        return sealed(free)

    # The fid ops. a0 = fid, and there is no path.
    if verb in (NP_BASE + 16, NP_BASE + 18, NP_BASE + 19):
        fid = a0
        if fid not in FIDS:
            # MATCHES fsd, which answers a bare FS_ERROR for a bad or
            # not-yours fid. Deliberately not "improved" to a clearer code: an
            # observer that answers better than the server it observes hides
            # exactly the divergence it exists to find. That fsd's own answer
            # is the same over-generic sentinel this file just stopped using
            # for verbs is a real follow-up, recorded in
            # docs/roadmap/roadmap-fid-verbs.md, not a difference to introduce here.
            print(f"  [bad fid {fid} for verb 0x{verb:x}]", flush=True)
            return sealed(FS_ERROR)
        fpath, opener = FIDS[fid]
        if opener != name:
            print(f"  [fid {fid}: opened as {opener!r}, asked for as {name!r}]", flush=True)
            return sealed(FS_ERR_PERM)
        if verb == NP_BASE + 19:  # NP_CLUNK
            del FIDS[fid]
            return sealed(0)
        if verb == NP_BASE + 18:  # NP_FSTAT
            info = stat_info(fpath)
            if info is None:
                return sealed(FS_ERR_NOT_FOUND)
            return sealed(STAT_INFO_LEN, info)
        # NP_PREAD: a1 = offset, a2 = count. Status = bytes read, 0 at EOF.
        data = FILES[fpath]
        off, count = a1, a2
        if off >= len(data):
            return sealed(0)
        chunk = data[off:off + count]
        return sealed(len(chunk), chunk)

    # Anything else. SAY SO ON STDOUT: this line is why the bug that added the
    # NP_STAT arm above cost a debugging session. An unserved verb was
    # indistinguishable from an absent path at the guest (both FS_ERROR, which
    # `ls` renders as "no such file or directory"), so the only way to learn
    # that the verb number was 0x10c was to wrap this script in an ad-hoc
    # logger. Printing it costs one line and makes the next gap name itself.
    #
    # A prose list of served verbs sits in this file's docstring with nothing
    # comparing it to this dispatch chain, so treat THIS as the authority.
    if verb in MUTATING_VERBS:
        # A verb this peer knows of and refuses on policy, not one it failed to
        # recognise. netd copies the status through verbatim, so the guest
        # renders "read-only filesystem" - which is the actual reason, and
        # distinguishable from both "absent" and "no idea".
        print(f"  [read-only: refusing verb 0x{verb:x} path={path!r}]", flush=True)
        return sealed(FS_ERR_READ_ONLY)
    print(f"  [unserved verb 0x{verb:x} path={path!r}] -> FS_ERR_NO_SUCH_VERB",
          flush=True)
    # NOT FS_ERROR, which this answered until 2026-09-05. The guest rendered
    # that as "no such file or directory" - a message about a path, for a
    # request whose path was fine - which is precisely the confusion the print
    # above exists to work around. Now the guest says so itself, and the print
    # is what names WHICH verb, since one status code cannot.
    return sealed(FS_ERR_NO_SUCH_VERB)


# Every verb ninep-abi defines, and the group this peer's DISPATCH must put it
# in. Kept beside the dispatch on purpose: the docstring's prose list of served
# verbs had nothing comparing it to the chain, so when the read-only branch
# silently swallowed the five fid verbs the prose went on claiming otherwise -
# confidently, and for as long as nobody read both. `--self-test` is what turns
# that prose into a claim that can fail.
#
# EACH ENTRY CARRIES ITS OWN PARAMS, because a generic frame is wrong for
# NP_OPEN: its `a0` is the OPEN_* flags, not the path length. The first version
# of this test sent `a0 = len(path)`, so a 10-character path arrived as flags
# 10 = OPEN_WRITE|OPEN_TRUNC and was refused read-only - the exact trap
# documented by FID_BASE, reproduced by the harness meant to check it. `FID` is
# substituted with a fid this test opens first.
FID = "<fid>"
# "served" means a real answer, not an error in the reserved band.
SELF_TEST_VERBS = [
    #  name,           verb,          expected group,  (a0, a1, a2),        path
    ("NP_READDIR",    NP_BASE + 0,  "served",       ("PLEN", 4096, 0), b"/"),
    ("NP_READ_FILE",  NP_BASE + 1,  "served",       ("PLEN", 512, 0),  b"/HELLO.TXT"),
    ("NP_READ",       NP_BASE + 2,  "served",       ("PLEN", 0, 512),  b"/HELLO.TXT"),
    ("NP_WRITE",      NP_BASE + 3,  "read-only",    ("PLEN", 0, 0),    b"/HELLO.TXT"),
    ("NP_WRITE_AT",   NP_BASE + 4,  "read-only",    ("PLEN", 0, 0),    b"/HELLO.TXT"),
    ("NP_TOUCH",      NP_BASE + 5,  "read-only",    ("PLEN", 0, 0),    b"/NEW.TXT"),
    ("NP_MKDIR",      NP_BASE + 6,  "read-only",    ("PLEN", 0, 0),    b"/NEWDIR"),
    ("NP_RMDIR",      NP_BASE + 7,  "read-only",    ("PLEN", 0, 0),    b"/SUB"),
    ("NP_RM",         NP_BASE + 8,  "read-only",    ("PLEN", 0, 0),    b"/HELLO.TXT"),
    ("NP_MV",         NP_BASE + 9,  "read-only",    ("PLEN", 0, 0),    b"/HELLO.TXT"),
    ("NP_READ_AT",    NP_BASE + 10, "served",       ("PLEN", 0, 512),  b"/HELLO.TXT"),
    ("NP_WRITE_FILE", NP_BASE + 11, "read-only",    ("PLEN", 0, 0),    b"/HELLO.TXT"),
    ("NP_STAT",       NP_BASE + 12, "served",       ("PLEN", 0, 0),    b"/HELLO.TXT"),
    ("NP_CHMOD",      NP_BASE + 13, "read-only",    ("PLEN", 0o644, 0), b"/HELLO.TXT"),
    ("NP_CHOWN",      NP_BASE + 14, "read-only",    ("PLEN", 0, 0),    b"/HELLO.TXT"),
    # a0 = FLAGS here, not the path length.
    ("NP_OPEN(r)",    NP_BASE + 15, "served",       (OPEN_READ, "PLEN", 0), b"/HELLO.TXT"),
    ("NP_OPEN(w)",    NP_BASE + 15, "read-only",    (OPEN_WRITE, "PLEN", 0), b"/HELLO.TXT"),
    ("NP_OPEN(dir)",  NP_BASE + 15, "not-a-file",   (OPEN_READ, "PLEN", 0), b"/SUB"),
    ("NP_OPEN(gone)", NP_BASE + 15, "not-found",    (OPEN_READ, "PLEN", 0), b"/NOPE.TXT"),
    # a0 = a FID, and no path at all.
    ("NP_PREAD",      NP_BASE + 16, "served",       (FID, 0, 512),     b""),
    ("NP_PREAD(bad)", NP_BASE + 16, "generic-error", (999, 0, 512),    b""),
    # A count of 0 is served, not refused: the POSIX shape, and what fsd
    # answers since 2026-09-21 (it refused FS_ERROR before, the divergence
    # docs/roadmap/roadmap-fid-verbs.md's ledger recorded). The value, 0 with
    # no bytes, is checked in the round trip below; this row pins the class.
    ("NP_PREAD(zero)", NP_BASE + 16, "served",       (FID, 0, 0),       b""),
    ("NP_PWRITE",     NP_BASE + 17, "read-only",    (FID, 0, 4),       b""),
    ("NP_FSTAT",      NP_BASE + 18, "served",       (FID, 0, 0),       b""),
    ("NP_CLUNK",      NP_BASE + 19, "served",       (FID, 0, 0),       b""),
    ("NP_RUN",        NP_BASE + 0x20, "no-such-verb", ("PLEN", 0, 0),  b"/HELLO.TXT"),
    ("NP_SESSION",    NP_BASE + 0x21, "served",       (0, 0, 0),         b""),
]


def classify(status):
    if status == FS_ERR_READ_ONLY:
        return "read-only"
    if status == FS_ERR_NO_SUCH_VERB:
        return "no-such-verb"
    if status == FS_ERR_NOT_FOUND:
        return "not-found"
    if status == FS_ERR_NOT_A_FILE:
        return "not-a-file"
    if status == FS_ERROR:
        return "generic-error"
    if status >= (1 << 64) - 64:
        return f"error 0x{status:x}"
    return "served"


def _frame(verb, params, path, fid):
    """One NP request body: [verb][tree][a0][a1][a2][a3][path]."""
    vals = [len(path) if p == "PLEN" else (fid if p is FID else p) for p in params]
    return (struct.pack("<Q", verb) + b"\0" * 8
            + struct.pack("<QQQ", *vals) + b"\0" * (HDR - 40) + path)


def _call(body, quiet):
    """Drive one request through the dispatch; return (status, data)."""
    if quiet:
        with contextlib.redirect_stdout(io.StringIO()):
            out = serve_request(body)
    else:
        out = serve_request(body)
    inner = out[4 + NP_SIG_LEN:]
    return struct.unpack("<Q", inner[:8])[0], inner[8:]


def self_test(quiet=True):
    """Check the dispatch against the table above, then a real fid round trip.

    Auth is stubbed out - this tests DISPATCH, and a signature check in the way
    would only mean the test needs keys to answer a question about verb numbers.
    `verify` is restored afterwards so an in-process caller is not left with a
    server that authenticates nothing.
    """
    global verify
    real_verify = verify
    verify = lambda body: (body, b"\0" * NP_NONCE_LEN, b"self-test")  # noqa: E731
    bad = []
    try:
        FIDS.clear()
        # A fid for the entries that need one. If this fails everything after
        # it is meaningless, so say so rather than reporting 4 confusing
        # mismatches.
        st, _ = _call(_frame(NP_BASE + 15, (OPEN_READ, "PLEN", 0), b"/HELLO.TXT", 0), quiet)
        if st >= (1 << 64) - 64:
            print(f"np9p_server --self-test: could not open a fid (status 0x{st:x})")
            return 1
        fid = st

        for name, verb, want, params, path in SELF_TEST_VERBS:
            status, _ = _call(_frame(verb, params, path, fid), quiet)
            got = classify(status)
            if got != want:
                bad.append(f"  - {name} (0x{verb:x}): dispatch says {got}, "
                           f"this table says {want}")
            elif not quiet:
                print(f"  {name:14} 0x{verb:x} -> {got}")

        # The round trip Step 2 is actually for: open -> fstat -> pread (in two
        # chunks, so an offset that is ignored shows up) -> clunk -> the fid is
        # gone. Byte-compared against the file, not just "no error".
        FIDS.clear()
        want_bytes = FILES[b"/HELLO.TXT"]
        st, _ = _call(_frame(NP_BASE + 15, (OPEN_READ, "PLEN", 0), b"/HELLO.TXT", 0), quiet)
        fid = st
        st, info = _call(_frame(NP_BASE + 18, (FID, 0, 0), b"", fid), quiet)
        size = struct.unpack("<Q", info[STAT_SIZE_OFF:STAT_SIZE_OFF + 8])[0]
        if st != STAT_INFO_LEN or size != len(want_bytes):
            bad.append(f"  - fid round trip: NP_FSTAT gave status {st}, size "
                       f"{size}; expected {STAT_INFO_LEN}, {len(want_bytes)}")
        half = len(want_bytes) // 2
        got = b""
        for off, count in ((0, half), (half, len(want_bytes))):
            st, chunk = _call(_frame(NP_BASE + 16, (FID, off, count), b"", fid), quiet)
            if st != len(chunk):
                bad.append(f"  - fid round trip: NP_PREAD status {st} != "
                           f"{len(chunk)} bytes delivered")
            got += chunk
        # A zero-count read answers 0 and delivers nothing, at an offset where
        # the file HAS bytes, so EOF cannot be what answered. fsd answers the
        # same since 2026-09-21; the guest-side observer is the fid gate's
        # zero-count check in np9p_client.py.
        st, chunk = _call(_frame(NP_BASE + 16, (FID, 0, 0), b"", fid), quiet)
        if st != 0 or chunk:
            bad.append(f"  - fid round trip: NP_PREAD with count 0 gave status "
                       f"{st}, {len(chunk)} bytes; expected 0 and none")
        if got != want_bytes:
            # Report the FIRST DIVERGENCE, not both buffers. The file is ~2KB
            # and printing it twice buried the one byte that mattered under
            # 4000 characters of identical text - a failure nobody reads is not
            # much better than one that never fires.
            at = next((i for i in range(min(len(got), len(want_bytes)))
                       if got[i] != want_bytes[i]), min(len(got), len(want_bytes)))
            bad.append(f"  - fid round trip: read back {len(got)} bytes, file "
                       f"holds {len(want_bytes)}; first difference at byte {at}: "
                       f"got {got[at:at + 24]!r} want {want_bytes[at:at + 24]!r}")
        st, _ = _call(_frame(NP_BASE + 19, (FID, 0, 0), b"", fid), quiet)
        if st != 0:
            bad.append(f"  - fid round trip: NP_CLUNK status {st}, expected 0")
        st, _ = _call(_frame(NP_BASE + 16, (FID, 0, 16), b"", fid), quiet)
        if classify(st) != "generic-error":
            bad.append(f"  - fid round trip: NP_PREAD on a CLUNKED fid gave "
                       f"{classify(st)}; a freed handle must not still read")
    finally:
        verify = real_verify
        FIDS.clear()
    keyed_bad = keyed_self_test(quiet)
    if bad or keyed_bad:
        print("np9p_server --self-test: DISPATCH DISAGREES WITH THE TABLE")
        print("\n".join(bad + keyed_bad))
        return 1
    print(f"np9p_server: {len(SELF_TEST_VERBS)} verb(s) dispatch as documented, "
          f"and a fid round trip reads the file back byte-for-byte; a KEYED "
          f"session with np9p_client.py serves every verb, the client refuses "
          f"all {len(MISBEHAVE_MODES)} misbehaving replies, and the export "
          f"closes on a skipped seq and a tampered tag")
    return 0


def _client_module():
    """np9p_client.py, loaded as the OTHER implementation: its own ephemeral
    derivation and key schedule, signing as the dev "host" identity (which this
    server authorizes) and expecting this server's replies to be signed by it."""
    import importlib.util
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location("np9p_client", os.path.join(here, "np9p_client.py"))
    c = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(c)
    c.SIGN_KEY = c.dev_seed(c.DEV_PEER_LABELS["host"])
    c.PEER_LABEL = c.DEV_PEER_LABELS["host"]
    c._PEER_KEY = None
    return c


def _with_server(quiet, mode, client_side):
    """Serve ONE connection on a loopback port, with MISBEHAVE = `mode`, while
    `client_side(port)` runs against it. Returns (what it returns, how the
    server ended the connection): "refused", "closed", or "CRASHED: <error>"
    when serve_connection raised, which no check may count as a pass."""
    global MISBEHAVE
    MISBEHAVE = mode
    lsock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    lsock.bind(("127.0.0.1", 0))
    lsock.listen(1)
    port = lsock.getsockname()[1]

    ended = []

    def one():
        conn, addr = lsock.accept()
        try:
            ended.append(serve_connection(conn, addr))
        except Exception as e:  # noqa: BLE001  recorded, then judged by the caller
            ended.append(f"CRASHED: {type(e).__name__}: {e}")

    t = threading.Thread(target=one, daemon=True)
    t.start()
    try:
        result = client_side(port)
    finally:
        t.join(timeout=30)
        lsock.close()
        MISBEHAVE = None
    return result, (ended[0] if ended else "CRASHED: the server thread did not finish")


def keyed_self_test(quiet=True):
    """Step 5's check (docs/roadmap/roadmap-session-auth.md): a keyed session
    between this server and np9p_client.py, two implementations with separate
    key schedules, over a real socket. Returns a list of failures."""
    c = _client_module()
    bad = []
    sink = io.StringIO()

    def run(mode, fn):
        if quiet:
            with contextlib.redirect_stdout(sink):
                return _with_server(quiet, mode, fn)
        return _with_server(quiet, mode, fn)

    # 1. The honest server: the whole verb table under AUTHNP04.
    def honest(port):
        out = []
        s = c.Session("127.0.0.1", port)
        try:
            st, eph = s.open(keyed=True)
            if st != 0 or s.keys is None:
                return [f"  - keyed: NP_SESSION with a key was not keyed (status 0x{st:x}, "
                        f"{len(eph)}-byte result)"]
            FIDS.clear()
            st, _ = s.op(_frame(NP_BASE + 15, (OPEN_READ, "PLEN", 0), b"/HELLO.TXT", 0))
            if st >= (1 << 64) - 64:
                return [f"  - keyed: could not open a fid (status 0x{st:x})"]
            fid = st
            for name, verb, want, params, path in SELF_TEST_VERBS:
                st, _ = s.op(_frame(verb, params, path, fid))
                got = classify(st)
                if got != want:
                    out.append(f"  - keyed {name} (0x{verb:x}): got {got}, the table says {want}")
        except (RuntimeError, OSError) as e:
            out.append(f"  - keyed: the session broke: {e}")
        finally:
            s.close()
            FIDS.clear()
        return out

    out, ended = run(None, honest)
    bad += out
    if ended.startswith("CRASHED"):
        bad.append(f"  - keyed: the server {ended}")

    # 2. Each misbehaving reply must be REFUSED by the client: FS_ERR_AUTH.
    for mode in MISBEHAVE_MODES:
        def misbehaving(port):
            s = c.Session("127.0.0.1", port)
            try:
                st, _ = s.open(keyed=True)
                if st != 0 or s.keys is None:
                    return f"not keyed (0x{st:x})"
                st, _ = s.op(_frame(NP_BASE + 12, ("PLEN", 0, 0), b"/HELLO.TXT", 0))
                return st
            except (RuntimeError, OSError) as e:
                return f"broke: {e}"
            finally:
                s.close()
        got, ended = run(mode, misbehaving)
        if ended.startswith("CRASHED"):
            bad.append(f"  - keyed '{mode}': the server {ended}")
        if got != FS_ERR_AUTH:
            shown = got if isinstance(got, str) else classify(got)
            bad.append(f"  - keyed: the client did not refuse a '{mode}' reply ({shown})")

    # 3. The export must close, with no reply, on a request it cannot trust.
    for label, tamper in (("a skipped request seq", "seq"), ("a tampered request tag", "tag")):
        def refused(port, tamper=tamper):
            s = c.Session("127.0.0.1", port)
            try:
                st, _ = s.open(keyed=True)
                if st != 0 or s.keys is None:
                    return f"not keyed (0x{st:x})"
                if tamper == "seq":
                    s.seq += 1
                else:
                    k_c2s, k_s2c = s.keys
                    s.keys = (bytes([k_c2s[0] ^ 1]) + k_c2s[1:], k_s2c)
                st, _ = s.op(_frame(NP_BASE + 12, ("PLEN", 0, 0), b"/HELLO.TXT", 0))
                return f"answered (status 0x{st:x})"
            except (RuntimeError, OSError):
                return "closed"
            finally:
                s.close()
        got, ended = run(None, refused)
        # Both halves: the client saw the connection end, AND the server ended
        # it on purpose. A crash also ends it, and passed this check until the
        # review of #165.
        if got != "closed" or ended != "refused":
            bad.append(f"  - keyed: the export did not refuse {label} "
                       f"(client saw: {got}; server: {ended})")
    return bad


# ---------------------------------------------------------------------------
# KEYED SESSIONS (step 5 of docs/roadmap/roadmap-session-auth.md), transcribed
# from ninep-abi's normative block, "Keyed sessions". The key schedule and the
# ephemeral derivation are written HERE and again, separately, in
# np9p_client.py, on purpose: a shared helper would make the plan's control
# (drop one input from ONE side's derivation) impossible, since both sides would
# change together and still agree.

# This peer's stand-in for a boot ID: fixed for the process, never repeated
# across runs (the clock is in it). The spec's own words: a host peer with no
# boot ID uses its own non-repeating value in its place.
HOST_BOOT_ID = struct.pack("<Q", time.time_ns()) + os.urandom(8)

# `--misbehave <mode>`: send keyed replies a correct client must REFUSE, for the
# client-side controls (the self-test here, and step 7's guest client).
#   bad-tag     the tag with one bit flipped
#   wrong-key   tagged with k_c2s, the REQUEST direction's key
#   replay-seq  tagged over seq - 1, a replay of the previous reply's seq
#   skip-seq    tagged over seq + 1, a reply from the future
MISBEHAVE_MODES = ("bad-tag", "wrong-key", "replay-seq", "skip-seq")
MISBEHAVE = None


def export_ephemeral(request_nonce):
    """The export's ephemeral secret: SIG_DOMAIN_EPHEMERAL_E, the machine key,
    the boot ID, the request nonce, the clock and fresh entropy, hashed; X25519
    clamps it. Never stored past the handshake."""
    h = hashlib.sha512(SIG_DOMAIN_EPHEMERAL_E + host_seed() + HOST_BOOT_ID + request_nonce
                       + struct.pack("<Q", time.monotonic_ns() // 1000) + os.urandom(32))
    return h.digest()[:32]


def session_keys(shared, request_nonce, eph_client, eph_export):
    """K = SHA-512(SIG_DOMAIN_SESSION ‖ shared ‖ request_nonce ‖ eph_client ‖
    eph_export); k_c2s is the first half, k_s2c the second."""
    k = hashlib.sha512(SIG_DOMAIN_SESSION + shared + request_nonce + eph_client + eph_export).digest()
    return k[:NP_SESSION_KEY_LEN], k[NP_SESSION_KEY_LEN:2 * NP_SESSION_KEY_LEN]


def keyed_tag(key, seq, data):
    """HMAC-SHA-512(key, seq ‖ data), truncated to NP_KEYED_TAG_LEN."""
    return hmac.new(key, struct.pack("<Q", seq) + data, hashlib.sha512).digest()[:NP_KEYED_TAG_LEN]


class Keyed:
    """One keyed session's state: the two keys, the user its NP_SESSION was
    signed as (every verb runs as them), and the last seq accepted."""

    def __init__(self, k_c2s, k_s2c, name):
        self.k_c2s, self.k_s2c, self.name = k_c2s, k_s2c, name
        self.seq = 0

    def reply(self, seq, status, data=b""):
        """[u32 len][tag:32][status][result], tagged with k_s2c over the
        request's seq, or deliberately wrong under --misbehave."""
        inner = struct.pack("<Q", status) + data
        key, s = self.k_s2c, seq
        if MISBEHAVE == "wrong-key":
            key = self.k_c2s
        elif MISBEHAVE == "replay-seq":
            s = seq - 1
        elif MISBEHAVE == "skip-seq":
            s = seq + 1
        tag = bytearray(keyed_tag(key, s, inner))
        if MISBEHAVE == "bad-tag":
            tag[0] ^= 1
        framed = bytes(tag) + inner
        return struct.pack("<I", len(framed)) + framed

    def serve(self, body):
        """A keyed request's reply, or None: an AUTHNP03 frame, a bad tag or a
        seq that is not exactly one more than the last. None means CLOSE
        WITHOUT REPLYING, since an unauthenticated refusal is what an attacker
        would forge."""
        if len(body) < NP_AUTH_HDR_KEYED:
            return None
        (magic, seq) = struct.unpack("<QQ", body[:16])
        if magic != NP_AUTH_MAGIC_KEYED or seq != self.seq + 1:
            return None
        tag = body[16:NP_AUTH_HDR_KEYED]
        np_msg = body[NP_AUTH_HDR_KEYED:]
        if not hmac.compare_digest(tag, keyed_tag(self.k_c2s, seq, np_msg)):
            return None
        self.seq = seq
        if len(np_msg) >= 8 and struct.unpack("<Q", np_msg[:8])[0] == NP_SESSION:
            # No payload: today's idempotent 0. A key offer: no re-keying.
            if len(np_msg) > HDR:
                return None
            return self.reply(seq, 0)
        return serve_np(np_msg, self.name, lambda st, data=b"": self.reply(seq, st, data))


def offers_key(body):
    """Whether a signed frame is an NP_SESSION carrying an ephemeral key."""
    return (request_verb(body) == NP_SESSION
            and len(body) == NP_AUTH_HDR_SIGNED + HDR + NP_EPHEMERAL_LEN)


def open_keyed(body):
    """Key a session from a verified NP_SESSION offering `eph_client`: the
    reply (a SIGNED one, carrying `eph_export`) and the Keyed state, or None
    for a refusal to send as the unsigned denial, or "close" for an all-zero
    shared secret, which closes without a reply on both sides."""
    verified = verify(body)
    if verified is None:
        return None
    np_msg, nonce, name = verified
    eph_client = np_msg[HDR:HDR + NP_EPHEMERAL_LEN]
    secret = export_ephemeral(nonce)
    eph_export = x25519_ref.public(secret)
    shared = x25519_ref.shared(secret, eph_client)
    if shared is None:
        return "close"
    k_c2s, k_s2c = session_keys(shared, nonce, eph_client, eph_export)
    return seal(nonce, 0, eph_export), Keyed(k_c2s, k_s2c, name)


def abort(conn):
    """Close with an RST, the keyed session's answer to an auth failure."""
    try:
        conn.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
    except OSError:
        pass


def serve_connection(conn, addr, delay=0.0):
    """One accepted connection: a request and a reply then FIN, or a session
    (NP_SESSION answered 0) held until the peer closes, keyed when its first
    frame offered a key. Returns how it ended: "refused" when this peer closed
    it with an RST on purpose (a keyed-session refusal), "closed" otherwise.
    An exception other than a socket error is NOT caught: the self-test must
    be able to tell a deliberate refusal from a crash (review of #165)."""
    try:
        # A session (NP_SESSION answered 0) keeps this loop reading frames
        # on the same socket; anything else is one request, one reply, FIN.
        # Single-threaded, so a held session blocks the next accept until
        # it closes or idles out - fine for the gate this exists for, and
        # named in the roadmap for the step that makes the guest hold one.
        conn.settimeout(SESSION_IDLE_S)
        session = False
        keyed = None
        first = True
        while True:
            body = read_frame(conn)
            if body is None:
                break
            if keyed is not None:
                reply = keyed.serve(body)
                if reply is None:
                    print(f"  [conn {addr}] keyed session: refused frame, closing with RST", flush=True)
                    abort(conn)
                    return "refused"
            elif len(body) >= 8 and struct.unpack("<Q", body[:8])[0] == NP_AUTH_MAGIC_KEYED:
                # AUTHNP04 on a connection that was never keyed.
                reply = frame_reply(FS_ERR_AUTH)
            elif offers_key(body):
                if not first:
                    # Keying happens only on a fresh connection's first frame.
                    print(f"  [conn {addr}] key offered mid-stream: closing with RST", flush=True)
                    abort(conn)
                    return "refused"
                opened = open_keyed(body)
                if opened == "close":
                    print(f"  [conn {addr}] all-zero shared secret: closing", flush=True)
                    abort(conn)
                    return "refused"
                if opened is None:
                    reply = frame_reply(FS_ERR_AUTH)
                else:
                    reply, keyed = opened
                    session = True
                    print("  [keyed session opened on this connection]", flush=True)
            else:
                reply = serve_request(body)
                if request_verb(body) == NP_SESSION and reply_status(reply) == 0:
                    session = True
            first = False
            if delay:
                time.sleep(delay)
            conn.sendall(reply)
            if not session:
                break
        if session:
            return "closed"  # the peer closed it; nothing to shut down
        # One request/reply per connection, then FIN.
        #
        # The FIN is TIDY TEARDOWN, NOT A FRAMING SIGNAL - and it stopped
        # being one on 2026-09-05. This used to say "the guest client reads
        # to EOF", which was true when written and is now false: the guest
        # stops at `4 + len` from the frame's own header and closes first.
        # Left as a warning rather than deleted, because the next reader
        # would otherwise reasonably conclude that `shutdown(SHUT_WR)` is
        # load-bearing for correctness here. It is not.
        #
        # NP_RUN is the exception, and it is a real one: its reply is a raw
        # stream with NO length prefix, so for that verb EOF genuinely is
        # the terminator (see np9p_client.py's run_op).
        try:
            conn.shutdown(socket.SHUT_WR)
        except OSError:
            pass
        return "closed"
    except (ConnectionError, OSError) as e:
        print(f"  [conn {addr}] {e}")
        return "closed"
    finally:
        conn.close()


def main():
    args = sys.argv[1:]
    if "--self-test" in args:
        sys.exit(self_test(quiet="-v" not in args))
    # `--delay S` holds every reply for S seconds before sending it: a peer
    # that is slow, not dead, so a guest with its request PARKED (netd's async
    # remote mount, docs/roadmap/roadmap-async-rmount.md) can be killed and
    # its slot taken by a second caller before the first reply lands. That is
    # the identity check of the plan's step 0: the second caller must get its
    # OWN reply, never the first's. Longer than the guest's deadline and the
    # reply is simply too late, which is the other thing the rig measures.
    if "--delay" in args:
        di = args.index("--delay")
        if di + 1 >= len(args):
            print("np9p_server: --delay needs a value in seconds", file=sys.stderr)
            sys.exit(2)
        delay = float(args[di + 1])
    else:
        delay = 0.0
    args = [a for i, a in enumerate(args) if a != "--delay" and (i == 0 or args[i - 1] != "--delay")]
    global MISBEHAVE
    if "--misbehave" in args:
        mi = args.index("--misbehave")
        if mi + 1 >= len(args) or args[mi + 1] not in MISBEHAVE_MODES:
            print(f"np9p_server: --misbehave needs one of {', '.join(MISBEHAVE_MODES)}", file=sys.stderr)
            sys.exit(2)
        MISBEHAVE = args[mi + 1]
        args = args[:mi] + args[mi + 2:]
        print(f"np9p_server: MISBEHAVING on keyed replies: {MISBEHAVE}")
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
        serve_connection(conn, addr, delay)

if __name__ == "__main__":
    main()

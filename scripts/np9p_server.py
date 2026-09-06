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

**netd's export still implements NONE of the fid verbs** - that is steps 4-6 of
docs/roadmap-fid-verbs.md - so this peer is currently AHEAD of the guest, which
is the right way round for a foreign observer: the client can be built and
checked against something that already answers.

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
import io
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
MAX_FIDS = 8  # fsd's own ceiling, mirrored so exhaustion behaves the same here
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
        FIDS[free] = fpath
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
            # docs/roadmap-fid-verbs.md, not a difference to introduce here.
            print(f"  [bad fid {fid} for verb 0x{verb:x}]", flush=True)
            return sealed(FS_ERROR)
        fpath = FIDS[fid]
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
    ("NP_PWRITE",     NP_BASE + 17, "read-only",    (FID, 0, 4),       b""),
    ("NP_FSTAT",      NP_BASE + 18, "served",       (FID, 0, 0),       b""),
    ("NP_CLUNK",      NP_BASE + 19, "served",       (FID, 0, 0),       b""),
    ("NP_RUN",        NP_BASE + 0x20, "no-such-verb", ("PLEN", 0, 0),  b"/HELLO.TXT"),
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
    verify = lambda body: (body, b"\0" * NP_NONCE_LEN)  # noqa: E731
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
    if bad:
        print("np9p_server --self-test: DISPATCH DISAGREES WITH THE TABLE")
        print("\n".join(bad))
        return 1
    print(f"np9p_server: {len(SELF_TEST_VERBS)} verb(s) dispatch as documented, "
          f"and a fid round trip reads the file back byte-for-byte")
    return 0


def main():
    args = sys.argv[1:]
    if "--self-test" in args:
        sys.exit(self_test(quiet="-v" not in args))
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
        except (ConnectionError, OSError) as e:
            print(f"  [conn {addr}] {e}")
        finally:
            conn.close()


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
r"""A minimal host-side client for Ouroboros's 9P-over-TCP export (cluster
Phase 1). Speaks the length-delimited `ninep-abi` frame to a netd export
listener (default guest port 564, reached via a SLIRP hostfwd) so you can read a
guest's filesystem over TCP from the host - the "foreign observer" that verifies
the export gateway (docs/roadmap/roadmap-cluster-phase1.md).

Usage:
    python3 scripts/np9p_client.py <host> <port> readdir <path>
    python3 scripts/np9p_client.py <host> <port> read    <path> [offset] [want]
    python3 scripts/np9p_client.py <host> <port> stat    <path>
    python3 scripts/np9p_client.py <host> <port> mv      <src> <dst>
    python3 scripts/np9p_client.py <host> <port> run     <command>
    python3 scripts/np9p_client.py <host> <port> noverb  <path> [verb]
    python3 scripts/np9p_client.py <host> <port> session <path>          # readdir+stat+read on ONE connection
    python3 scripts/np9p_client.py <host> <port> session-gate [hold_s]  # the step-4 checks, PASS/FAIL each

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
NP_SESSION = NP_BASE + 0x21  # open a session on this connection (Decision 3, 2026-09-07)
# NOT syscall-abi's FS_ERR_MIN, and no longer named as if it were. This is a
# deliberately LOOSE host-side floor - 'anything in the top 64 is an error' -
# which is safe here and is what lets REPLY_UNVERIFIED (1 << 64) compare as an
# error. It carried the ABI name until 2026-09-05 and was invisible to
# check-wire-constants only because its spelling `(1 << 64) - 64` happens not
# to match the parser's `- 1 - N` idiom. That is luck, not design: the real
# FS_ERR_MIN is now a CHECKED constant, so a name collision here would compare
# a loose sentinel against the ABI and fail for a reason nobody would enjoy
# diagnosing. Renamed so it cannot.
ERR_BAND_FLOOR = (1 << 64) - 64
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
# wire can carry, and it still compares `>= ERR_BAND_FLOOR`, so every existing
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
# A sanity ceiling on a DECLARED reply length, so recv_reply cannot be made to
# allocate on a number the peer chose. Deliberately NOT a mirror of ninep-abi's
# NP_FRAME_MAX (~2.2 KB): a defensive bound only has to be finite and
# comfortably above real traffic, and mirroring the exact constant would add
# three more hand-copied ABI values - NP_NET_MAX, NP_AUTH_HDR_SIGNED and
# NP_FRAME_MAX are all defined as SUMS in Rust, which check-wire-constants
# cannot parse, so the copies would be pinned by nothing. A round 64 KiB
# couples to nothing and still refuses the 4 GiB case this exists for.
MAX_DECLARED_REPLY = 64 * 1024
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
#
# BUILT FROM THE MODULE CONSTANTS, not from repeated literals. `check-wire-
# constants.py` parses `^NAME = ...` lines and pins those against Rust; a literal
# inside a dict body is invisible to it, so a code that moved would be caught in
# the constant and NOT in the table - and this probe would then print a
# confidently wrong name for the very status it exists to make honest.
FS_ERROR = (1 << 64) - 1
FS_ERR_READ_ONLY = (1 << 64) - 1 - 29  # u64::MAX - 29
FS_ERR_PERM = (1 << 64) - 1 - 32  # u64::MAX - 32
FS_ERR_NO_SUCH_VERB = (1 << 64) - 1 - 39  # u64::MAX - 39: no arm for that verb
FS_ERR_BUSY = (1 << 64) - 1 - 40  # u64::MAX - 40: the export's session budget is spent
STATUS_NAMES = {
    FS_ERROR: "FS_ERROR",
    NO_FS: "NO_FS",
    FS_ERR_NOT_FOUND: "FS_ERR_NOT_FOUND",
    FS_ERR_READ_ONLY: "FS_ERR_READ_ONLY",
    FS_ERR_AUTH: "FS_ERR_AUTH",
    FS_ERR_PERM: "FS_ERR_PERM",
    FS_ERR_NO_SUCH_VERB: "FS_ERR_NO_SUCH_VERB",
    FS_ERR_BUSY: "FS_ERR_BUSY",
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
    # READ EXACTLY WHAT THE FRAME SAYS, rather than to EOF.
    #
    # The reply is `[u32 len][sig:64][status u64][data]`, so its length is on
    # the wire and the server's FIN was never NEEDED to know where it ends -
    # only convenient. Waiting for one is what makes this client unable to talk
    # to an export that keeps the connection open between requests, which is
    # what a fid SESSION is (decision 3, docs/roadmap/roadmap-fid-verbs.md): it would
    # block here forever on a reply it had already received in full.
    #
    # Landed on its own, and BEFORE any session exists, because it is a no-op
    # against today's FIN-closing export - the bytes and the parse are
    # identical, only the stopping condition changes - and it is required under
    # every option for the wire signal that is still undecided.
    # BOUNDED, because `flen` comes off the wire BEFORE any signature is
    # checked. Reading to EOF was inherently bounded by the peer closing;
    # reading a DECLARED length is not, and `sock.recv(n)` allocates n bytes -
    # so `len = 0xFFFFFFFF` would attempt a 4 GiB allocation on the first call.
    # This script is the foreign observer for a security boundary, and one that
    # the observed party can hang or exhaust is a poor witness.
    #
    # ONE bound, and the per-read `min(...)` below is loop shape rather than a
    # second one: the declared-length check runs first and uses the same 64 KiB
    # figure, so the per-call cap can never limit anything it has not already
    # limited. Calling that defence-in-depth would be describing a guard that
    # cannot fire.
    def read_exactly(n):
        out = b""
        while len(out) < n:
            chunk = sock.recv(min(n - len(out), 65536))
            if not chunk:
                break  # EOF - let the caller report a short reply
            out += chunk
        return out

    buf = read_exactly(4)
    if len(buf) < 4:
        raise RuntimeError(f"short reply ({len(buf)} bytes)")
    (flen,) = struct.unpack("<I", buf[:4])
    if flen > MAX_DECLARED_REPLY:
        raise RuntimeError(
            f"reply header declares {flen} bytes, far beyond anything this "
            f"protocol produces (cap {MAX_DECLARED_REPLY}) - refusing to read it")
    buf += read_exactly(flen)
    if len(buf) < 4 + flen:
        raise RuntimeError(
            f"truncated reply: header says {flen} bytes, got {len(buf) - 4}")
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


class Session:
    """One export connection held across requests: the step-4 shape.

    `open()` sends NP_SESSION; status 0 means every later `op()` on this object
    is served on the SAME TCP connection, which is what a fid's lifetime will
    hang off. Against an export that predates the verb, `open()` answers
    FS_ERR_NO_SUCH_VERB and the connection closes after it, as it always did -
    so the caller learns "no sessions here" from the one answer that cannot be
    mistaken for a key problem.
    """

    def __init__(self, host, port, timeout=10):
        self.sock = socket.create_connection((host, port), timeout=timeout)

    def op(self, np_msg, user=None, seed=None):
        frame, nonce = signed_frame(np_msg, SIGN_KEY if seed is None else seed, user=user)
        self.sock.sendall(frame)
        return recv_reply(self.sock, nonce, peer_key=peer_key())

    def open(self):
        return self.op(build_frame(NP_SESSION, 0, [], b""))

    def close(self):
        self.sock.close()

    def abort(self):
        """Close with a RST rather than a FIN: the shape of a peer that dies
        mid-session (SO_LINGER with a zero timeout)."""
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))
        self.sock.close()


# What the guest export budgets: SESSION_MAX in netd is MAX_CONNS - 1 = 3, so
# the fourth session on a fresh export must be refused with FS_ERR_BUSY. Spelled
# here as the EXPECTATION the gate checks, not as a mirror of the constant.
EXPECTED_SESSIONS = 3
# netd reaps a silent connection after CONN_IDLE_TICKS, which is 30 s of GUEST
# TICKS - and a guest tick is not a wall-clock 20 ms. Under TCG the timer fires
# at whatever rate the host affords (CLAUDE.md records ~37 ms once), so the same
# 1500 ticks can be 30 s or well over a minute of the host's time, varying with
# host load. A single fixed wait therefore measured the host's clock against the
# guest's and flaked: three runs of the same build gave 3/3, 2/3 and 0/3 reaped.
# So the check ESCALATES instead: wait, probe once, and if the session is still
# served wait longer, up to the last of these. It still fails if the reap never
# happens, which is the property under test; what it no longer does is fail
# because QEMU was slow. A probe RESETS the idle timer (an accepted segment
# refreshes `last_rx`), which is why each attempt opens its own session rather
# than re-probing one.
REAP_WAITS_S = (40, 75, 120)


def do_session(host, port, path):
    """readdir, stat and read `path` over ONE connection, and say so."""
    s = Session(host, port)
    st, _ = s.open()
    if st != 0:
        print(f"session refused: {status_name(st)}")
        s.close()
        sys.exit(1)
    pb = path.encode()
    st, data = s.op(build_frame(NP_READDIR, 0, [len(pb), 4096], pb))
    print(f"[same connection] readdir -> {status_name(st) if st >= ERR_BAND_FLOOR else f'{len(data)} bytes'}")
    st, data = s.op(build_frame(NP_STAT, 0, [len(pb)], pb))
    print(f"[same connection] stat    -> {status_name(st) if st >= ERR_BAND_FLOOR else f'record of {len(data)} bytes'}")
    st, data = s.op(build_frame(NP_READ, 0, [len(pb), 0, 64], pb))
    print(f"[same connection] read    -> {status_name(st) if st >= ERR_BAND_FLOOR else data[:64]!r}")
    s.close()


def do_session_gate(host, port, hold_s=10):
    """The step-4 session gate (docs/roadmap/roadmap-fid-verbs.md), run end to
    end against a live export, one PASS/FAIL line per check:

      1. a session idle for `hold_s` seconds still answers on the same
         connection (the guest transcript is the other half of this check: it
         must show no `server slot 4 ... restart` line), and NP_RUN on it is
         refused FRAMED with the session still in phase, three ways: a valid
         caller ("no arm"), an unknown user name, an unauthorized key;
      2. the budget: three sessions accepted, the fourth refused with
         FS_ERR_BUSY; a one-shot request still served beside them; none of the
         three evicted; with the table full a fifth connection is refused
         (reset or EOF), not hung; a session its peer aborts returns its slot
         at once;
      3. sessions abandoned without a FIN are reaped by the idle timeout, and
         their slots come back.

    Every check can fail against an export without the feature: 1 and 2 fail
    on FS_ERR_NO_SUCH_VERB at the first open, and 3 on the slots never coming
    back. Exit status is the number of failed checks.
    """
    import time
    failed = 0

    def check(name, ok, detail):
        nonlocal failed
        print(f"[{'PASS' if ok else 'FAIL'}] {name}: {detail}", flush=True)
        if not ok:
            failed += 1

    root = build_frame(NP_READDIR, 0, [1, 4096], b"/")

    def served(status):
        return status < ERR_BAND_FLOOR

    # 1. idle hold
    s = Session(host, port, timeout=hold_s + 20)
    st, _ = s.open()
    if st != 0:
        check("open a session", False, status_name(st))
        s.close()
        return failed
    # In a try: the failure this check EXISTS to detect - the session dying
    # during the hold - arrives as an exception, so bare calls made the one
    # event under test abort the whole run with a traceback and lose the ten
    # checks after it. The three below were written with the guard; this one,
    # first and oldest, was not (review of #124).
    try:
        st1, d1 = s.op(root)
        time.sleep(hold_s)
        st2, d2 = s.op(root)
        ok = served(st1) and served(st2) and d1 == d2
        detail = f"readdir before/after: {status_name(st1)}/{status_name(st2)}, {len(d1)}/{len(d2)} bytes"
    except (RuntimeError, OSError) as exc:
        st1, d1 = FS_ERROR, b""
        ok, detail = False, f"the session died during the hold ({exc.__class__.__name__})"
    check(f"a session idle {hold_s}s still answers on the same connection", ok, detail)
    # NP_RUN on a session is refused FRAMED, whether the refusal is "no arm on
    # this connection" (a valid caller) or an auth refusal (an unknown user):
    # the export's raw-text refusal has no FIN behind it on a session, so it
    # would leave every later reply out of phase (review of #123). The readdir
    # after each is what shows the session is still in phase.
    run = build_frame(NP_RUN, 0, [len(b"echo x"), 0, 0, 0], b"echo x")
    try:
        st_run, _ = s.op(run)
        st_after, d_after = s.op(root)
        detail = f"run -> {status_name(st_run)}, then readdir -> {status_name(st_after) if not served(st_after) else f'{len(d_after)} bytes'}"
        ok = st_run == FS_ERR_NO_SUCH_VERB and served(st_after) and d_after == d1
    except (RuntimeError, OSError) as exc:
        ok, detail = False, f"session lost ({exc.__class__.__name__})"
    check("NP_RUN on a session is refused framed and the session stays in phase", ok, detail)
    try:
        st_run, _ = s.op(run, user=b"nobody-here")
        st_after, d_after = s.op(root)
        detail = f"run as an unknown user -> {status_name(st_run)}, then readdir -> {status_name(st_after) if not served(st_after) else f'{len(d_after)} bytes'}"
        # The property is FRAMED-AND-IN-PHASE, so the refusal status is
        # accepted as a set: deny_unknown_user answers NO_FS or FS_ERR_NOT_FOUND
        # when the export cannot read its own /etc/passwd, and pinning
        # FS_ERR_AUTH made this fail for a reason it is not about (review #124).
        ok = st_run in (FS_ERR_AUTH, FS_ERR_NOT_FOUND, NO_FS) and served(st_after) and d_after == d1
    except (RuntimeError, OSError) as exc:
        ok, detail = False, f"session lost ({exc.__class__.__name__})"
    check("an auth-refused NP_RUN on a session is refused framed and the session stays in phase", ok, detail)
    # The check above reaches deny_unknown_user (a valid key, an unknown name).
    # deny_9p is the OTHER refusal path, reached by an unauthorized KEY, and it
    # had its own copy of the raw-vs-framed rule - so it needs its own check
    # that can fail (review of #124).
    try:
        st_run, _ = s.op(run, seed=dev_seed("nobody"))
        st_after, d_after = s.op(root)
        detail = f"run with an unauthorized key -> {status_name(st_run)}, then readdir -> {status_name(st_after) if not served(st_after) else f'{len(d_after)} bytes'}"
        ok = st_run == FS_ERR_AUTH and served(st_after) and d_after == d1
    except (RuntimeError, OSError) as exc:
        ok, detail = False, f"session lost ({exc.__class__.__name__})"
    check("an NP_RUN with an unauthorized key on a session is refused framed and the session stays in phase", ok, detail)
    s.close()
    time.sleep(0.5)

    # 2. budget
    held = []
    refusal = None
    for i in range(EXPECTED_SESSIONS + 1):
        s = Session(host, port)
        st, _ = s.open()
        if st == 0:
            held.append(s)
        else:
            refusal = (i + 1, st)
            s.close()
            break
    check(f"{EXPECTED_SESSIONS} sessions accepted, the next refused with FS_ERR_BUSY",
          len(held) == EXPECTED_SESSIONS and refusal is not None and refusal[1] == FS_ERR_BUSY,
          f"accepted {len(held)}, refusal " + (f"#{refusal[0]} {status_name(refusal[1])}" if refusal else "none"))
    try:
        st, _ = one_op(host, port, root)
        detail = "served" if served(st) else status_name(st)
    except (RuntimeError, OSError) as exc:
        st, detail = FS_ERROR, f"no reply ({exc.__class__.__name__})"
    check("a one-shot request is served beside the held sessions", served(st), detail)

    def answers(s):
        try:
            return served(s.op(root)[0])
        except (RuntimeError, OSError):
            return False
    alive = sum(1 for s in held if answers(s))
    check("no held session was evicted", alive == len(held), f"{alive}/{len(held)} still answer")
    # The table is full once a bare 4th connection is open (SYN only, no
    # request). A 5th must be refused: SLIRP accepts the host's connect on the
    # guest's behalf, so the refusal shows on the first read as a reset or EOF,
    # never as a served reply and never as a hang.
    # `bare` is the PRECONDITION - the fourth connection, which must SUCCEED to
    # fill the table - and `fifth` is the one under test. They are caught
    # separately: with both in one try, a refusal of `bare` (an earlier one-shot
    # still tearing down, so the table was already full) set the outcome and the
    # check reported the property proven from the refusal of the connection that
    # was supposed to establish it (review of #124).
    bare = None
    fifth = None
    outcome = None
    try:
        bare = socket.create_connection((host, port), timeout=10)
    except OSError as exc:
        outcome = f"precondition failed: the 4th connection was refused ({exc.__class__.__name__})"
    try:
        if bare is None:
            raise RuntimeError("no precondition")
        time.sleep(0.5)
        fifth = socket.create_connection((host, port), timeout=10)
        frame, _nonce = signed_frame(root, SIGN_KEY)
        fifth.settimeout(8)
        fifth.sendall(frame)
        got = fifth.recv(4)
        outcome = "EOF" if not got else f"served {len(got)}+ bytes"
    except socket.timeout:
        outcome = "hang (8s timeout)"
    except RuntimeError:
        pass  # the precondition already failed; its message is the outcome
    except OSError as exc:
        outcome = f"reset ({exc.__class__.__name__})"
    check("with the table full, a 5th connection is refused rather than hung",
          bare is not None and (outcome == "EOF" or outcome.startswith("reset")), outcome)
    for sk in (fifth, bare):
        if sk is not None:
            sk.close()
    time.sleep(0.5)
    # A peer that dies mid-session sends a RST (or its OS does); the slot must
    # be back immediately, not after the idle timeout.
    held.pop(0).abort()
    time.sleep(0.5)
    s = Session(host, port)
    st, _ = s.open()
    check("a session aborted by its peer (RST) returns its slot at once", st == 0, status_name(st) if st else "accepted")
    if st == 0:
        held.append(s)
    else:
        s.close()

    # 3. the idle reap, escalating (see REAP_WAITS_S for why it is not one
    # wait). The budget check's sessions are KEPT for it: a reap matters because
    # it returns a slot, so holding the table FULL is what lets the check after
    # this one ("the slots are back") fail for the reason it names.
    reaped_after = None
    detail = "no session held"
    for wait in REAP_WAITS_S:
        while len(held) < EXPECTED_SESSIONS:
            s = None
            try:
                s = Session(host, port, timeout=20)
                st, _ = s.open()
            except (RuntimeError, OSError):
                # CLOSED before breaking: the socket exists by then, so netd has
                # a slot for it, and abandoning it here held that slot through
                # the whole measurement window this loop is about to open
                # (review of #124).
                if s is not None:
                    s.close()
                break
            if st != 0:
                s.close()
                break
            held.append(s)
        if not held:
            detail = "could not hold a session to test"
            continue
        print(f"  holding {len(held)} session(s) silent for {wait}s ...", flush=True)
        time.sleep(wait)
        reaped = 0
        for s in held:
            try:
                s.sock.settimeout(15)
                # ANY REPLY, including a framed error, means the session is
                # ALIVE. Scoring this by `served()` counted every error status
                # as "reaped", so a session that answered FS_ERR_BUSY passed the
                # check while demonstrably answering (review of #124).
                s.op(root)
            except socket.timeout:
                pass  # neither answering nor refused: not evidence either way
            except (RuntimeError, OSError):
                reaped += 1  # EOF or reset: the export dropped it
        held_now = len(held)
        detail = f"{reaped}/{held_now} reaped after {wait}s (held {held_now} of {EXPECTED_SESSIONS})"
        for s in held:
            s.close()
        held = []
        # Against what was actually HELD, not the target: the refill above
        # breaks on a transient, and scoring 2 reaped out of 2 against a target
        # of 3 reported failure while printing the property satisfied on its own
        # evidence, then slept through the whole escalation (review of #124).
        # At least TWO, and all of them. Scoring `reaped == held_now` alone let
        # a run that could only hold one session pass on that single reap -
        # weakest exactly on the slow, loaded runs the escalation exists for
        # (review of #124).
        if held_now >= 2 and reaped == held_now:
            reaped_after = wait
            break
    check("sessions silent past the export's idle limit are reaped", reaped_after is not None, detail)
    time.sleep(1)
    # The slots come back - RETRIED, because teardown is asynchronous: the RST
    # for a reaped session and the FINs for the ones just closed are still in
    # flight, and a connection opened into that moment can be answered with a
    # bare close. That surfaced as an uncaught "short reply (0 bytes)" that
    # killed the whole run after every check had passed.
    fresh = []
    why_stopped = ""
    for attempt in range(6):
        while len(fresh) < EXPECTED_SESSIONS:
            # The CONNECT is inside the try as well: a SYN against a table that
            # has not finished tearing down is answered with a RST (by design
            # since #123), which SLIRP reports to the host as a refused
            # connection - from socket.create_connection, before any frame is
            # sent. Catching only the open() left that as an uncaught
            # ConnectionRefusedError.
            s = None
            try:
                s = Session(host, port)
                st, _ = s.open()
            except (RuntimeError, OSError) as exc:
                if s is not None:
                    s.close()  # see the reap loop: an abandoned socket holds a slot
                why_stopped = f"{exc.__class__.__name__} on open #{len(fresh) + 1}"
                break
            if st != 0:
                s.close()
                why_stopped = f"{status_name(st)} on open #{len(fresh) + 1}"
                break
            fresh.append(s)
        if len(fresh) == EXPECTED_SESSIONS:
            break
        for s in fresh:
            s.close()
        fresh = []
        time.sleep(2)
    check(f"the reaped slots are back: {EXPECTED_SESSIONS} fresh sessions accepted",
          len(fresh) == EXPECTED_SESSIONS,
          f"{len(fresh)} accepted" + (f" ({why_stopped})" if why_stopped and len(fresh) < EXPECTED_SESSIONS else ""))
    for s in fresh:
        s.close()
    print(f"session-gate: {failed} check(s) failed", flush=True)
    return failed


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
    if st >= ERR_BAND_FLOOR or not data.strip().isdigit():
        print(f"clone failed (status 0x{st:016x})"); sys.exit(1)
    n = int(data.strip())
    print(f"[clone] connection {n}")
    st, _ = np_writefile(host, port, f"{base}/{n}/ctl", f"connect {dst_ip}!{dst_port}".encode())
    if st >= ERR_BAND_FLOOR:
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
        if st >= ERR_BAND_FLOOR:
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
    if st >= ERR_BAND_FLOOR or not data.strip().isdigit():
        print(f"clone failed (0x{st:016x})"); sys.exit(1)
    n = int(data.strip())
    print(f"[clone] listener {n}")
    st, _ = np_writefile(host, port, f"{base}/{n}/ctl", f"announce {announce_port}".encode())
    if st >= ERR_BAND_FLOOR:
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
        if st < ERR_BAND_FLOOR and data.strip().isdigit():
            m = int(data.strip()); break
        time.sleep(0.1)
    if m is None:
        print("no connection accepted (timeout)"); sys.exit(1)
    print(f"[listen] accepted connection {m}")

    # Read the request the external client sent (relayed through the guest).
    got_req = b""
    for _ in range(50):
        st, data = np_readfile(host, port, f"{base}/{m}/data", want=512)
        if st < ERR_BAND_FLOOR and data:
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
    if len(args) < 4 and not (len(args) == 3 and args[2] == "session-gate"):
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
        # item; see docs/roadmap/roadmap-fid-verbs.md). The point is the STATUS the
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
        # `path` is REQUIRED, not defaulted: main() already exits on
        # `len(args) < 4`, so an `else "/"` default here could never be reached
        # and the usage line promised a bare `noverb` that only ever printed the
        # docstring.
        path = args[3].encode()
        verb = int(args[4], 0) if len(args) > 4 else NP_OPEN
        np = build_frame(verb, 0, [len(path), 0, 0, 0], path)
        try:
            status, data = one_op(host, port, np)
            print(f"verb 0x{verb:x} path={path!r} -> {status_name(status)} "
                  f"({len(data)} bytes of data)")
        except Exception as exc:
            print(f"no usable answer: {exc!r}")
        return

    if op == "session-gate":
        hold = int(args[3]) if len(args) > 3 else 10
        sys.exit(min(do_session_gate(host, port, hold), 125))

    if op == "session":
        do_session(host, port, args[3])
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
    if status >= ERR_BAND_FLOOR:
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

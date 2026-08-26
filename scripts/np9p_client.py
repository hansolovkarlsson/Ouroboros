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
every request carries an auth header `[magic:8][nonce:16][mac:32]` in front of
the NP message, where `mac = HMAC-SHA256(cluster_key, nonce || np)`. This client
signs with the shared dev key by default; pass `--key <k>` to use another (e.g.
to prove the export refuses a wrong key). The key must match the guest's
`\CLUSTER.KEY` (Makefile `CLUSTER_KEY`, default below).

e.g. after `make run-image-9p`:
    python3 scripts/np9p_client.py localhost 5640 readdir /
    python3 scripts/np9p_client.py localhost 5640 read /EFI/ORBS/INIT.CFG
    python3 scripts/np9p_client.py localhost 5640 readdir / --key wrong  # -> AUTH error
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
NP_READ_AT = NP_BASE + 10
FS_ERR_MIN = (1 << 64) - 64  # errors are a small band just below u64::MAX
FS_ERR_AUTH = (1 << 64) - 1 - 30  # u64::MAX - 30

# Cluster auth wire constants (ninep-abi). Must match the guest's CLUSTER.KEY.
NP_AUTH_MAGIC = int.from_bytes(b"AUTHNP01", "big")  # ninep-abi NP_AUTH_MAGIC
NP_NONCE_LEN = 16
DEFAULT_KEY = b"ouroboros-dev-cluster-key-v1"  # Makefile CLUSTER_KEY default


def build_frame(verb, tree, params, payload):
    # The bare NP message: [verb u64][tree u64][a0..a3 u64][payload].
    hdr = struct.pack("<Q", verb) + struct.pack("<Q", tree)
    for i in range(4):
        hdr += struct.pack("<Q", params[i] if i < len(params) else 0)
    return hdr + payload


def sign_frame(np_msg, key):
    # [u32 len][magic:8][nonce:16][mac:32][np]; len = bytes after the prefix.
    # mac = HMAC-SHA256(key, nonce || np). The nonce is arbitrary (fresh); the
    # export doesn't validate freshness in this tier.
    nonce = os.urandom(NP_NONCE_LEN)
    mac = hmac.new(key, nonce + np_msg, hashlib.sha256).digest()
    auth = struct.pack("<Q", NP_AUTH_MAGIC) + nonce + mac
    body = auth + np_msg
    return struct.pack("<I", len(body)) + body


def recv_reply(sock):
    # The server frames [u32 len][status u64][data] then FINs; read to EOF.
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
    if len(body) < 8:
        raise RuntimeError(f"reply body too short ({len(body)} bytes)")
    (status,) = struct.unpack("<Q", body[:8])
    data = body[8:]
    return status, data


def main():
    # Pull an optional `--key <k>` out of argv (anywhere after the fixed args).
    key = DEFAULT_KEY
    args = sys.argv[1:]
    if "--key" in args:
        i = args.index("--key")
        key = args[i + 1].encode()
        del args[i:i + 2]
    if len(args) < 4:
        print(__doc__)
        sys.exit(2)
    host, port, op, path = args[0], int(args[1]), args[2], args[3]
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
    frame = sign_frame(np_msg, key)

    with socket.create_connection((host, port), timeout=10) as s:
        s.sendall(frame)
        # Don't half-close the write side here: an immediate FIN could race the
        # request and tear the connection down before the server replies. The
        # server FINs after its framed reply; recv_reply reads to that EOF.
        status, data = recv_reply(s)

    if status == FS_ERR_AUTH:
        print("status: AUTH FAILED (export rejected the cluster key)")
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

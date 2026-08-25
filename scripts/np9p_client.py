#!/usr/bin/env python3
"""A minimal host-side client for Ouroboros's 9P-over-TCP export (cluster
Phase 1). Speaks the length-delimited `ninep-abi` frame to a netd export
listener (default guest port 564, reached via a SLIRP hostfwd) so you can read a
guest's filesystem over TCP from the host - the "foreign observer" that verifies
the export gateway (docs/roadmap-cluster-phase1.md).

Usage:
    python3 scripts/np9p_client.py <host> <port> readdir <path>
    python3 scripts/np9p_client.py <host> <port> read    <path> [offset] [want]
    python3 scripts/np9p_client.py <host> <port> stat    <path>

e.g. after `make run-image-9p`:
    python3 scripts/np9p_client.py localhost 5640 readdir /
    python3 scripts/np9p_client.py localhost 5640 read /EFI/ORBS/INIT.CFG
"""
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


def build_frame(verb, tree, params, payload):
    # [u32 len][verb u64][tree u64][a0..a3 u64][payload]; len = bytes after len.
    hdr = struct.pack("<Q", verb) + struct.pack("<Q", tree)
    for i in range(4):
        hdr += struct.pack("<Q", params[i] if i < len(params) else 0)
    msg = hdr + payload
    return struct.pack("<I", len(msg)) + msg


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
    if len(sys.argv) < 5:
        print(__doc__)
        sys.exit(2)
    host, port, op, path = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
    pb = path.encode()

    if op == "readdir":
        frame = build_frame(NP_READDIR, 0, [len(pb), 4096], pb)
    elif op == "read":
        offset = int(sys.argv[5]) if len(sys.argv) > 5 else 0
        want = int(sys.argv[6]) if len(sys.argv) > 6 else 4096
        frame = build_frame(NP_READ, 0, [len(pb), offset, want], pb)
    elif op == "stat":
        frame = build_frame(NP_READ_FILE, 0, [len(pb), 1], pb)
    else:
        print(f"unknown op {op!r}", file=sys.stderr)
        sys.exit(2)

    with socket.create_connection((host, port), timeout=10) as s:
        s.sendall(frame)
        # Don't half-close the write side here: an immediate FIN could race the
        # request and tear the connection down before the server replies. The
        # server FINs after its framed reply; recv_reply reads to that EOF.
        status, data = recv_reply(s)

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

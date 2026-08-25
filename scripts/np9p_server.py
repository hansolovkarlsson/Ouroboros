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
# A stand-in for fsd's FS_ERR_NOT_FOUND band; any value >= FS_ERR_MIN reads as an
# error to the client. Use FS_ERROR for "no such path".
HDR = 48  # NP_REQ_PAYLOAD

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
    if len(body) < HDR:
        return frame_reply(FS_ERROR)
    verb, tree = struct.unpack("<QQ", body[:16])
    a0, a1, a2, a3 = struct.unpack("<QQQQ", body[16:48])
    payload = body[HDR:]
    path = payload[: min(a0, len(payload))]

    if verb == NP_READDIR:
        out = listing(path)
        if out is None:
            return frame_reply(FS_ERROR)
        want = a1
        out = out[:want]
        return frame_reply(len(out), out)

    if verb in (NP_READ, NP_READ_AT):
        data = FILES.get(path)
        if data is None:
            return frame_reply(FS_ERROR)
        offset, want = a1, a2
        chunk = data[offset : offset + want]
        return frame_reply(len(chunk), chunk)

    if verb == NP_READ_FILE:
        data = FILES.get(path)
        if data is None:
            return frame_reply(FS_ERROR)
        want = a1
        return frame_reply(len(data), data[:want])

    # Any mutate/unknown verb: the export is read-only.
    return frame_reply(FS_ERROR)


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 5641
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", port))
    srv.listen(8)
    print(f"9P test server listening on 0.0.0.0:{port} "
          f"(guest reaches it at 10.0.2.2:{port} over SLIRP)")
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

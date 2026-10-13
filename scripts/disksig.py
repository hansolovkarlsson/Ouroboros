#!/usr/bin/env python3
"""The MBR disk signature of a built image, from the image's own bytes.

    python3 scripts/disksig.py set IMAGE...     write each image's signature
    python3 scripts/disksig.py check IMAGE...   each must carry its own, nonzero
    python3 scripts/disksig.py guid IMAGE LABEL print a GUID from IMAGE's bytes
    python3 scripts/disksig.py --self-test      (run by `make test`)

Step 2 of docs/roadmap/roadmap-one-disk.md. The kernel names the partition it
booted from by the firmware's `HardDrive` device-path node (bootdisk.rs), and
on an MBR disk that node carries the disk's 32-bit signature, bytes 440 to 443
of sector 0. Every image this project built until 2026-10-13 left those bytes
zero, so the node named all of them alike, and step 3, which picks the block
device by the node, could tell two of them apart only by where the boot
partition starts.

The signature is the first four bytes of SHA-256 over the whole image with
those four bytes counted as zero, so it is the image's own: the same bytes
give the same signature, and two images that differ in any byte (the node-a
and node-b pair, the stall image and the image it was copied from) get
different ones, with no counter or randomness to keep. hdiutil's FAT32 is not
byte-for-byte reproducible, so two builds of one tree differ too; nothing
relies on them matching. A hash of 0 becomes 1, since 0 means "none" to the
kernel. `check` recomputes it, so an image changed after it was signed (a
recipe that forgot to sign again after editing a copy) is caught, not just an
unsigned one.

A GPT disk takes no MBR signature (its protective MBR's is unused, by the
UEFI specification): its node carries the partition's GUID instead, which
`guid` derives from the payload in the same way, for scripts/mkgpt.py.
"""
import hashlib
import os
import struct
import sys
import tempfile
import uuid

SIG_OFFSET = 440
SIG_LEN = 4
CHUNK = 1 << 20


def content_hash(path, label=b""):
    """SHA-256 of the image with the signature bytes counted as zero, then
    `label`, read in chunks so a large image is never held whole."""
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        first = bytearray(fh.read(CHUNK))
        if len(first) >= SIG_OFFSET + SIG_LEN:
            first[SIG_OFFSET:SIG_OFFSET + SIG_LEN] = bytes(SIG_LEN)
        h.update(first)
        for chunk in iter(lambda: fh.read(CHUNK), b""):
            h.update(chunk)
    h.update(label)
    return h.digest()


def signature_for(path):
    sig = struct.unpack("<I", content_hash(path)[:SIG_LEN])[0]
    return sig or 1


def read_signature(path):
    with open(path, "rb") as fh:
        sector = fh.read(512)
    if len(sector) < 512 or sector[510:512] != b"\x55\xaa":
        raise ValueError(f"{path}: sector 0 is not an MBR")
    return struct.unpack_from("<I", sector, SIG_OFFSET)[0]


def is_gpt(path):
    with open(path, "rb") as fh:
        fh.seek(512)
        return fh.read(8) == b"EFI PART"


def set_signature(path):
    if is_gpt(path):
        raise ValueError(f"{path}: a GPT disk takes no MBR signature (scripts/mkgpt.py sets its GUIDs)")
    read_signature(path)
    sig = signature_for(path)
    with open(path, "r+b") as fh:
        fh.seek(SIG_OFFSET)
        fh.write(struct.pack("<I", sig))
    return sig


def check_signature(path):
    """None when `path` carries its own nonzero signature, else why not."""
    have = read_signature(path)
    if have == 0:
        return "no disk signature (bytes 440-443 are zero)"
    want = signature_for(path)
    if have != want:
        return f"signature {have:#010x} is not its own ({want:#010x}): the image changed after it was signed"
    return None


def guid_for(path, label):
    """A GUID from `path`'s bytes and `label`, in the on-disk (mixed-endian)
    byte order, shaped as an RFC 4122 version 4 GUID."""
    raw = bytearray(content_hash(path, label.encode())[:16])
    raw[6] = (raw[6] & 0x0F) | 0x40
    raw[8] = (raw[8] & 0x3F) | 0x80
    return uuid.UUID(bytes=bytes(raw)).bytes_le


def self_test():
    """Each rule on a synthetic MBR image, so the check can fail: unsigned,
    signed, changed after signing, and two different images."""
    results = []
    with tempfile.TemporaryDirectory() as tmp:
        def image(name, fill):
            path = os.path.join(tmp, name)
            disk = bytearray(4096)
            disk[510:512] = b"\x55\xaa"
            disk[1024:1024 + len(fill)] = fill
            with open(path, "wb") as fh:
                fh.write(disk)
            return path
        a = image("a.img", b"alpha")
        b = image("b.img", b"beta")
        results.append(("an unsigned image fails the check", check_signature(a) is not None))
        sig_a = set_signature(a)
        results.append(("a signed image passes", check_signature(a) is None and sig_a != 0))
        results.append(("signing is stable", set_signature(a) == sig_a))
        results.append(("two images get different signatures", set_signature(b) != sig_a))
        with open(a, "r+b") as fh:
            fh.seek(2048)
            fh.write(b"changed")
        results.append(("an image changed after signing fails", check_signature(a) is not None))
        results.append(("the GUIDs of one image differ by label", guid_for(b, "disk") != guid_for(b, "partition")))
        results.append(("a GUID is stable", guid_for(b, "disk") == guid_for(b, "disk")))
    failed = 0
    for name, ok in results:
        print(f"{'ok  ' if ok else 'FAIL'} disksig: {name}")
        failed += 0 if ok else 1
    return 1 if failed else 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if len(argv) >= 2 and argv[0] == "set":
        for path in argv[1:]:
            print(f"disksig: {path}: {set_signature(path):#010x}")
        return 0
    if len(argv) >= 2 and argv[0] == "check":
        failed = 0
        for path in argv[1:]:
            why = check_signature(path)
            print(f"{'ok  ' if why is None else 'FAIL'} disksig: {path}: {why or 'its own signature'}")
            failed += 0 if why is None else 1
        return 1 if failed else 0
    if len(argv) == 3 and argv[0] == "guid":
        print(uuid.UUID(bytes_le=guid_for(argv[1], argv[2])))
        return 0
    print(__doc__.split("\n\n")[1])
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

#!/usr/bin/env python3
r"""The MBR disk signature of a built image, from the image's own bytes.

    python3 scripts/disksig.py set IMAGE...     write each image's signature
    python3 scripts/disksig.py check IMAGE...   each MBR image must carry one, nonzero
    python3 scripts/disksig.py guid IMAGE LABEL print a GUID from IMAGE's bytes
    python3 scripts/disksig.py --self-test      (run by `make test`)

Step 2 of docs/roadmap/roadmap-one-disk.md. The kernel names the partition it
booted from by the firmware's `HardDrive` device-path node (bootdisk.rs), and
on an MBR disk that node carries the disk's 32-bit signature, bytes 440 to 443
of sector 0. Every image this project built until 2026-10-13 left those bytes
zero, so the node named all of them alike, and step 3, which picks the block
device by the node, could tell two of them apart only by where the boot
partition starts.

The signature is the first four bytes of SHA-256 over the whole image as
built, with those four bytes counted as zero, so two images built different
(the node-a and node-b pair, the stall image and the image it was copied
from, which its recipe signs again) get different ones, with no counter or
randomness to keep. hdiutil's FAT32 is not byte-for-byte reproducible, so two
builds of one tree differ too; nothing relies on them matching. A hash of 0
becomes 1, since 0 means "none" to the kernel.

It is the disk's name, not a checksum of it. Every boot writes the image (the
boot counter's `\EFI\ORBS\BOOTID.TXT`, and whatever a rig makes), so `check`
asks only that an MBR image carries a nonzero signature; one that tracked the
bytes refused every image that had been booted once (the review of #286). And
a copy of an image carries its source's signature, as a disk copied with `dd`
does: a rig that attaches a copy beside its source signs the copy again.

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


def _hash_image(chunks):
    """SHA-256 state over an image given as chunks, the signature bytes in
    the first chunk counted as zero."""
    h = hashlib.sha256()
    for i, chunk in enumerate(chunks):
        if i == 0 and len(chunk) >= SIG_OFFSET + SIG_LEN:
            chunk = bytearray(chunk)
            chunk[SIG_OFFSET:SIG_OFFSET + SIG_LEN] = bytes(SIG_LEN)
        h.update(chunk)
    return h


def content_hash(path):
    """The hash state of the image at `path`, read in chunks so a large image
    is never held whole."""
    with open(path, "rb") as fh:
        return _hash_image(iter(lambda: fh.read(CHUNK), b""))


def signature_from_digest(digest):
    """The first four bytes, little-endian; 0 means none, so it becomes 1."""
    return struct.unpack("<I", digest[:SIG_LEN])[0] or 1


def signature_for(path):
    return signature_from_digest(content_hash(path).digest())


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
    """None when `path` is a GPT disk or an MBR disk with a nonzero
    signature, else why not."""
    if is_gpt(path):
        return None
    if read_signature(path) == 0:
        return "no disk signature (bytes 440-443 are zero)"
    return None


def guid_from_state(state, label):
    """A GUID from an image's hash state and `label`, in the on-disk
    (mixed-endian) byte order, shaped as an RFC 4122 version 4 GUID."""
    h = state.copy()
    h.update(label.encode())
    raw = bytearray(h.digest()[:16])
    raw[6] = (raw[6] & 0x0F) | 0x40
    raw[8] = (raw[8] & 0x3F) | 0x80
    return uuid.UUID(bytes=bytes(raw)).bytes_le


def guid_for(path, label):
    return guid_from_state(content_hash(path), label)


def guids_for_bytes(data, labels):
    """One GUID per label from an image already in memory, hashed once
    (mkgpt.py holds esp.img whole)."""
    state = _hash_image([data])
    return [guid_from_state(state, label) for label in labels]


def self_test():
    """Each rule on synthetic images, so each check can fail: unsigned and
    signed, booted after signing, two different images, the 0 rule, GPT, and
    the GUID's shape."""
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
            fh.write(b"booted")
        results.append(("an image written after signing still passes", check_signature(a) is None))
        results.append(("a hash of 0 gives signature 1", signature_from_digest(bytes(32)) == 1))
        gpt = image("gpt.img", b"")
        with open(gpt, "r+b") as fh:
            fh.seek(512)
            fh.write(b"EFI PART")
        try:
            set_signature(gpt)
            refused = False
        except ValueError:
            refused = True
        results.append(("set refuses a GPT disk", refused))
        results.append(("check passes a GPT disk", check_signature(gpt) is None))
        guid = uuid.UUID(bytes_le=guid_for(b, "disk"))
        results.append(("a GUID is version 4, RFC 4122 variant", guid.version == 4 and guid.variant == uuid.RFC_4122))
        results.append(("the GUIDs of one image differ by label", guid_for(b, "disk") != guid_for(b, "partition")))
        with open(b, "rb") as fh:
            whole = fh.read()
        results.append(("in memory and from disk give one GUID", guids_for_bytes(whole, ["disk"])[0] == guid_for(b, "disk")))
    failed = 0
    for name, ok in results:
        print(f"{'ok  ' if ok else 'FAIL'} disksig: {name}")
        failed += 0 if ok else 1
    return 1 if failed else 0


def main(argv):
    try:
        return run(argv)
    except (OSError, ValueError) as e:
        print(f"disksig: {e}", file=sys.stderr)
        return 1


def run(argv):
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
            ok = why or ("a GPT disk, named by its partition GUID" if is_gpt(path) else "signed")
            print(f"{'ok  ' if why is None else 'FAIL'} disksig: {path}: {ok}")
            failed += 0 if why is None else 1
        return 1 if failed else 0
    if len(argv) == 3 and argv[0] == "guid":
        print(uuid.UUID(bytes_le=guid_for(argv[1], argv[2])))
        return 0
    print(__doc__.split("\n\n")[1])
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

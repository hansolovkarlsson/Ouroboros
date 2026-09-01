#!/usr/bin/env python3
"""Generate the per-machine cluster identity files staged onto every disk image.

Writes, into a directory given on the command line:

    id           this machine's PRIVATE key, 64 hex characters (mode 0600)
    id.pub       its public key
    authorized   one line per peer: <name> <ipv4> <pubkey-hex>

THE DEV KEYS ARE DETERMINISTIC, AND THAT IS DELIBERATE. `make image-ext2` builds
node A's disk and node B's disk in separate invocations, and both must end up
with the SAME `authorized` file or the two nodes cannot authenticate each other.
Random keys per build would produce a cluster that fails to talk to itself, with
a symptom (authentication refused) that looks nothing like the cause (the images
disagree about who the peers are). So the dev keypairs are derived from fixed
seed strings, exactly as `scripts/mkpasswd.py` uses fixed dev passwords.

WHICH MEANS THE DEV PRIVATE KEYS ARE IN THIS REPOSITORY, and anyone can sign as
these nodes. That is the same trade the dev passwords already make, and it is
fine for QEMU rigs; it is NOT fine for anything real. A deployment
generates on the device (`/bin/clusterkey`, which requires real entropy and
refuses without it) and distributes the PUBLIC halves by hand. `--random` here
writes one real identity for THIS node and an `authorized` naming only itself,
because a build machine cannot know other machines' keys - it does not, and
cannot, produce a whole working cluster on its own.

The Ed25519 maths is imported from gen-sign-vectors.py rather than copied: that
reference asserts itself against RFC 8032's published signatures when loaded, and
a third copy of a curve implementation is exactly the kind of drift this project
keeps finding in its own prose.
"""
import hashlib
import importlib.util
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("edref", os.path.join(_HERE, "gen-sign-vectors.py"))
edref = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(edref)  # this ASSERTS the reference against RFC 8032

# The dev cluster: the two nodes the two-VM rigs boot, plus the host-side Python
# peer that `run-image-9p` talks to. Addresses match netd's MAC-derived scheme
# (…:0a -> .10, …:0b -> .11) and SLIRP's fixed host address.
DEV_PEERS = [
    ("node-a", "10.0.2.10", "ouroboros-dev-node-a"),
    ("node-b", "10.0.2.11", "ouroboros-dev-node-b"),
    ("host", "10.0.2.2", "ouroboros-dev-host-peer"),
]


def seed_from(label):
    """A fixed 32-byte seed for a dev identity, from a printable label."""
    return hashlib.sha256(label.encode()).digest()


def keypair(seed):
    return seed, edref.public_key(seed)


def main():
    args = sys.argv[1:]
    random_keys = "--random" in args
    if random_keys:
        args.remove("--random")
    if len(args) != 2:
        print("usage: mkclusterkeys.py [--random] <out-dir> <this-node-name>", file=sys.stderr)
        print(f"  node names: {', '.join(n for n, _, _ in DEV_PEERS)}", file=sys.stderr)
        sys.exit(2)
    out_dir, me = args
    names = [n for n, _, _ in DEV_PEERS]
    if me not in names:
        print(f"mkclusterkeys.py: unknown node '{me}' (expected one of {names})", file=sys.stderr)
        sys.exit(2)

    keys = {}
    for name, ip, label in DEV_PEERS:
        seed = seed_from(label)
        keys[name] = (seed, edref.public_key(seed), ip)
    if random_keys:
        # --random replaces THIS NODE'S key with a real one, and nothing else.
        #
        # It used to draw a fresh seed for every peer while writing only this
        # node's `id`, so the other peers' private keys existed nowhere - and two
        # nodes built by separate invocations each got an `authorized` naming the
        # other's WRONG public key. A deployment path that cannot produce a
        # working cluster is worse than none, because it looks like one.
        my_ip = dict((n, i) for n, i, _ in DEV_PEERS)[me]
        seed = os.urandom(32)
        keys = {me: (seed, edref.public_key(seed), my_ip)}

    os.makedirs(out_dir, exist_ok=True)
    my_seed, my_pub, _ = keys[me]

    id_path = os.path.join(out_dir, "id")
    # Created 0600 rather than written and then chmod'd: the naive order leaves a
    # real secret world-readable for the length of the write. Immaterial for the
    # deterministic dev keys, which are public by design - but this is the same
    # code path a --random key takes, and that one is genuinely secret. The mode
    # is carried onto the guest by mke2fs -d.
    if os.path.exists(id_path):
        os.unlink(id_path)
    fd = os.open(id_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(my_seed.hex() + "\n")

    with open(os.path.join(out_dir, "id.pub"), "w") as f:
        f.write(my_pub.hex() + "\n")

    with open(os.path.join(out_dir, "authorized"), "w") as f:
        f.write("# Peers this machine accepts. One line per peer:\n")
        f.write("#   <name> <ipv4> <public-key-hex>\n")
        f.write("# Delete or comment out a line to revoke that peer.\n")
        if random_keys:
            f.write("# This key was generated randomly, so no other machine's public key is\n")
            f.write("# known here. Append one line per peer, copied from that machine's\n")
            f.write("# /etc/cluster/id.pub - see `clusterkey` on the device.\n")
        else:
            f.write("# DEV KEYS: derived from fixed seeds, so they are public. Not for real use.\n")
        for name, (_seed, pub, ip) in keys.items():
            f.write(f"{name} {ip} {pub.hex()}\n")

    print(f"mkclusterkeys: {out_dir} identity={me} peers={len(keys)}"
          f"{' (RANDOM keys)' if random_keys else ' (fixed dev keys)'}")


if __name__ == "__main__":
    main()

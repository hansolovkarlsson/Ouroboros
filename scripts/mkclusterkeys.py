#!/usr/bin/env python3
"""Generate the per-machine cluster identity files staged onto every disk image.

Writes, into a directory given on the command line:

    id           this machine's PRIVATE key, 64 hex characters (mode 0600)
    id.pub       its public key
    authorized   one line per peer: <name> <ipv4> <pubkey-hex> [root]
    realm        the cluster's realm (dev builds only: the public `ouroboros-dev`)
    users        the user-key registry: <name> <pubkey-hex> (mode 0600)

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
#
# `intruder` is NOT a machine. It is the authorized-but-hostile node that
# `np9p_client.py impersonate-gate` signs as (step 0 of
# docs/roadmap/roadmap-user-keys.md), kept apart from the identities the rigs
# use so that step 1 can trust those with root and leave this one untrusted,
# and the gate still sees the difference. Its address is one no dev machine
# holds: an export finds a peer by KEY and never reads the address, and a
# client looks one up only for a host it dials, which nobody dials this one.
# Sharing an address with a real peer would make that peer's lookup ambiguous,
# which `find_by_ip` refuses.
#
# The last field is the `root` flag (root squash, step 1 of the same plan): a
# peer may claim a user resolving to uid 0 or gid 0 only if its line carries
# it. Every identity the rigs drive the cluster as root with is flagged, so
# those rigs are unchanged; `intruder` is not, which is what the gate's row 1
# sees. `np9p_server.py` keeps its own copy of the flags (DEV_ROOT_PEERS), and
# `check-wire-constants.py` holds the two equal.
DEV_PEERS = [
    ("node-a", "10.0.2.10", "ouroboros-dev-node-a", True),
    ("node-b", "10.0.2.11", "ouroboros-dev-node-b", True),
    ("host", "10.0.2.2", "ouroboros-dev-host-peer", True),
    ("intruder", "10.0.2.20", "ouroboros-dev-intruder", False),
]


# THE DEV USERS' KEYS (step 3 of docs/roadmap/roadmap-user-keys.md): derived
# from each dev account's password and the dev realm, exactly as a node derives
# them at login, and written HERE INDEPENDENTLY of the Rust in
# clusterkeys/src/users.rs rather than through a shared helper. The two agree
# only if both follow Decision 2; a Rust host test pins the keys this prints
# (`--dev-user-keys`), and check-wire-constants.py holds that fixture equal to
# this script's output, so a disagreement surfaces on `make test`.
#
# The realm is PUBLIC and for dev rigs only: a realm must be unique to a cluster,
# because a dictionary precomputed for one serves every node in it.
DEV_REALM = b"ouroboros-dev"
USERKEY_DOMAIN = b"ouroboros-cluster-userkey-v1\0"
USERKEY_ITERATIONS = 210_000
# (name, password, registered). Passwords match mkpasswd.py's dev accounts.
# The registry lists `user` only (Decision 6): a root claim is decided by the
# `root` flag, and `guest` is the gate's row 5, a name with no registered key.
DEV_USERS = [
    ("root", b"root", False),
    ("user", b"user", True),
    ("guest", b"guest", False),
]


def user_seed(name, password, realm=DEV_REALM):
    """Decision 2: PBKDF2-HMAC-SHA-512 over the length-prefixed salt; the
    first 32 bytes are the Ed25519 seed."""
    n = name.encode()
    salt = USERKEY_DOMAIN + bytes([len(realm)]) + realm + bytes([len(n)]) + n
    return hashlib.pbkdf2_hmac("sha512", password, salt, USERKEY_ITERATIONS, 64)[:32]


def dev_user_keys():
    """(name, public key) for every dev user, registered or not."""
    return [(n, edref.public_key(user_seed(n, pw))) for n, pw, _ in DEV_USERS]


def seed_from(label):
    """A fixed 32-byte seed for a dev identity, from a printable label."""
    return hashlib.sha256(label.encode()).digest()


def keypair(seed):
    return seed, edref.public_key(seed)


def main():
    args = sys.argv[1:]
    if args == ["--dev-user-keys"]:
        # The fixture clusterkeys' host test pins, and check-wire-constants.py
        # compares against it: every dev user's public key, one per line.
        for name, pub in dev_user_keys():
            print(f"{name} {pub.hex()}")
        return
    random_keys = "--random" in args
    if random_keys:
        args.remove("--random")
    if len(args) != 2:
        print("usage: mkclusterkeys.py [--random] <out-dir> <this-node-name>", file=sys.stderr)
        print(f"  node names: {', '.join(n for n, _, _, _ in DEV_PEERS)}", file=sys.stderr)
        sys.exit(2)
    out_dir, me = args
    names = [n for n, _, _, _ in DEV_PEERS]
    if me not in names:
        print(f"mkclusterkeys.py: unknown node '{me}' (expected one of {names})", file=sys.stderr)
        sys.exit(2)

    keys = {}
    for name, ip, label, root in DEV_PEERS:
        seed = seed_from(label)
        keys[name] = (seed, edref.public_key(seed), ip, root)
    if random_keys:
        # --random replaces THIS NODE'S key with a real one, and nothing else.
        #
        # It used to draw a fresh seed for every peer while writing only this
        # node's `id`, so the other peers' private keys existed nowhere - and two
        # nodes built by separate invocations each got an `authorized` naming the
        # other's WRONG public key. A deployment path that cannot produce a
        # working cluster is worse than none, because it looks like one.
        # Its own line is not root-flagged: a machine's trust in ITSELF with
        # root is not what the flag is for, and granting it here would be a
        # default nobody chose. An operator adds ` root` to a peer's line by
        # hand when that machine should be trusted with root.
        my_ip = dict((n, i) for n, i, _, _ in DEV_PEERS)[me]
        seed = os.urandom(32)
        keys = {me: (seed, edref.public_key(seed), my_ip, False)}

    os.makedirs(out_dir, exist_ok=True)
    my_seed, my_pub, _, _ = keys[me]

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
        f.write("#   <name> <ipv4> <public-key-hex> [root]\n")
        f.write("# `root` trusts that peer to act as root (uid or gid 0) here.\n")
        f.write("# Delete or comment out a line to revoke that peer.\n")
        if random_keys:
            f.write("# This key was generated randomly, so no other machine's public key is\n")
            f.write("# known here. Append one line per peer, copied from that machine's\n")
            f.write("# /etc/cluster/id.pub - see `clusterkey` on the device.\n")
        else:
            f.write("# DEV KEYS: derived from fixed seeds, so they are public. Not for real use.\n")
        for name, (_seed, pub, ip, root) in keys.items():
            f.write(f"{name} {ip} {pub.hex()}{' root' if root else ''}\n")

    # The realm and the registry, for dev builds only. A --random identity is a
    # real node's: its realm comes from `clusterkey realm new` on a machine
    # with entropy and is copied to the others, and a dev registry would name
    # dev keys a real cluster must not accept.
    if not random_keys:
        with open(os.path.join(out_dir, "realm"), "w") as f:
            f.write(DEV_REALM.decode() + "\n")
        users_path = os.path.join(out_dir, "users")
        if os.path.exists(users_path):
            os.unlink(users_path)
        fd = os.open(users_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write("# Users whose remote claims need their own credential here. One line\n")
            f.write("# per user: <name> <public-key-hex>. A user not listed is served as\n")
            f.write("# today, on the machine's word. DEV KEYS: derived from the public dev\n")
            f.write("# passwords and realm. Not for real use.\n")
            registered = {n for n, _, r in DEV_USERS if r}
            for name, pub in dev_user_keys():
                if name in registered:
                    f.write(f"{name} {pub.hex()}\n")

    print(f"mkclusterkeys: {out_dir} identity={me} peers={len(keys)}"
          f"{' (RANDOM keys)' if random_keys else ' (fixed dev keys)'}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Generate Ouroboros's /etc/passwd, staged into the disk images at build time.

Format, one account per line:
    name:uid:gid:home:salt_hex:hash_hex
where hash_hex = SHA-256(salt || password), written to /etc/SHADOW. The
world-readable /etc/passwd carries only name:uid:gid:home. The salt is 8 random
bytes; the
guest's login (programs/shell/src/login.rs) only *verifies* (recomputes the
hash with the stored salt and compares), so it needs no runtime randomness.

DEV credentials, committed on purpose (this is a dev OS): a real deployment
would generate its own. Passwords are the same as the usernames.

    root / root   (uid 0)
    user / user   (uid 1000)

Home directories live under /Users (this project's chosen home base): root keeps
'/', a normal user gets '/Users/<name>' (the login sets it as the initial cwd and
exports HOME, so `~` expands to it). The image build stages /Users/<name> and, on
ext2, chowns it to the user. Usage: mkpasswd.py > /path/to/passwd
"""
import hashlib
import os
import sys

# (name, uid, gid, home, password)
ACCOUNTS = [
    ("root", 0, 0, "/", "root"),
    ("user", 1000, 1000, "/Users/user", "user"),
    # A second unprivileged account, and the one the impersonation gate's row 5
    # claims: a name with NO key in /etc/cluster/users (mkclusterkeys.py
    # registers `user` only), so a claim of it is served on the machine's word
    # (docs/roadmap/roadmap-user-keys.md, Decision 5).
    ("guest", 1001, 1001, "/Users/guest", "guest"),
]


def passwd_entry(name, uid, gid, home, password):
    """The public half: no secret. Every `id`, `ls -l` and `chown` reads this."""
    return f"{name}:{uid}:{gid}:{home}"


# VERSION 2 (docs/roadmap/roadmap-user-keys.md, Decision 10): PBKDF2-HMAC-SHA-512
# at the cluster's iteration count over a 7-byte salt, the first 32 bytes kept,
# the salt field marked `2$`. The same width as the version-1 line it replaces.
V2_ITERATIONS = 210_000


def shadow_entry(name, uid, gid, home, password):
    """The private half, for /etc/shadow (mode 0600, root-owned)."""
    salt = os.urandom(7)
    digest = hashlib.pbkdf2_hmac("sha512", password.encode(), salt, V2_ITERATIONS, 64)[:32]
    return f"{name}:2${salt.hex()}:{digest.hex()}"


def main():
    # `mkpasswd.py` writes /etc/passwd; `mkpasswd.py --shadow` writes
    # /etc/shadow. Two invocations rather than one writing two files, so the
    # Makefile keeps its plain stdout redirection - but that means the salts
    # differ between the two runs, which is FINE precisely because only the
    # shadow half is ever consulted for a password.
    shadow = "--shadow" in sys.argv
    fn = shadow_entry if shadow else passwd_entry
    lines = [fn(*acc) for acc in ACCOUNTS]
    sys.stdout.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()

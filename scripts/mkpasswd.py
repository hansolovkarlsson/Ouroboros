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
]


def passwd_entry(name, uid, gid, home, password):
    """The public half: no secret. Every `id`, `ls -l` and `chown` reads this."""
    return f"{name}:{uid}:{gid}:{home}"


def shadow_entry(name, uid, gid, home, password):
    """The private half, for /etc/shadow (mode 0600, root-owned)."""
    salt = os.urandom(8)
    digest = hashlib.sha256(salt + password.encode()).digest()
    return f"{name}:{salt.hex()}:{digest.hex()}"


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

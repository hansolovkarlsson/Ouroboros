#!/usr/bin/env python3
"""Generate Ouroboros's /etc/passwd, staged into the disk images at build time.

Format, one account per line:
    name:uid:gid:home:salt_hex:hash_hex
where hash_hex = SHA-256(salt || password). The salt is 8 random bytes; the
guest's login (programs/shell/src/login.rs) only *verifies* (recomputes the
hash with the stored salt and compares), so it needs no runtime randomness.

DEV credentials, committed on purpose (this is a dev OS): a real deployment
would generate its own. Passwords are the same as the usernames.

    root / root   (uid 0)
    user / user   (uid 1000)

Home is '/' for now - per-user /home directories are the next small step
(roadmap item 4, batch a). Usage: mkpasswd.py > /path/to/passwd
"""
import hashlib
import os
import sys

# (name, uid, gid, home, password)
ACCOUNTS = [
    ("root", 0, 0, "/", "root"),
    ("user", 1000, 1000, "/", "user"),
]


def entry(name, uid, gid, home, password):
    salt = os.urandom(8)
    digest = hashlib.sha256(salt + password.encode()).digest()
    return f"{name}:{uid}:{gid}:{home}:{salt.hex()}:{digest.hex()}"


def main():
    lines = [entry(*acc) for acc in ACCOUNTS]
    out = "\n".join(lines) + "\n"
    sys.stdout.write(out)


if __name__ == "__main__":
    main()

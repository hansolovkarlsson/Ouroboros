#!/usr/bin/env python3
"""Generate Ouroboros's /etc/group, staged into the disk images at build time.

Format, one group per line:
    name:gid:members
where `members` is a comma-separated list of usernames (may be empty). This is
this project's own format - it omits the traditional Unix `passwd` (`x`) field,
matching our /etc/passwd shape. See the `accounts` crate (parsing/formatting) and
the /bin group tools (groupadd/usermod).

Group *membership* is enforced by each task's single kernel-owned primary gid
(the /etc/passwd `gid` field), so the members list here is informational for now;
full supplementary-group membership is a deferred tier. The DEV groups mirror the
DEV accounts in mkpasswd.py (a user-private group per account, gid == uid).

Usage: mkgroup.py > /path/to/group
"""
import sys

# (name, gid, members)
GROUPS = [
    ("root", 0, ""),
    ("user", 1000, ""),
]


def main():
    out = "".join(f"{name}:{gid}:{members}\n" for name, gid, members in GROUPS)
    sys.stdout.write(out)


if __name__ == "__main__":
    main()

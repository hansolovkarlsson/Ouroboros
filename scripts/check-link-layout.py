#!/usr/bin/env python3
"""The layout DevTools's `ld` holds in its own code, pinned.

    python3 scripts/check-link-layout.py LDFLAGS    (or `make check-link-layout`)

DevTools's linker reads no linker script: its `--target=ouroboros` is
programs/linker.ld's layout and the C programs' link flags (the Makefile's
LDFLAGS_RUSTSHIM) written into its code, as of the note
docs/handoffs/closed/2026-10-14-from-devtools-link-with-ld.md. So the two are
a second copy, and a copy drifts without a word: `ld.lld` follows an edit to
the script, `make CLINK=devtools` keeps linking the old layout, and every
check here stays green. This hashes the script with its comments and spacing
taken out, so a comment-only edit passes, together with the flags, and fails
when the hash is not PIN. Updating PIN is the moment to send DevTools a
`notice` in its docs/handoffs/.
"""
import hashlib
import os
import re
import sys

PIN = "ff5ba04ed441a38f578d232d4df2b869e7e797a792c860a8eb255e6c431e3552"
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def layout_hash(ldflags):
    with open(os.path.join(ROOT, "programs", "linker.ld")) as f:
        script = f.read()
    script = re.sub(r"/\*.*?\*/", " ", script, flags=re.S)
    script = " ".join(script.split())
    flags = " ".join(ldflags.split())
    return hashlib.sha256(f"{script}\n{flags}\n".encode()).hexdigest()


def main(argv):
    if len(argv) != 1:
        sys.exit(__doc__)
    got = layout_hash(argv[0])
    if got != PIN:
        print("check-link-layout: programs/linker.ld or LDFLAGS_RUSTSHIM changed, "
              "and DevTools's ld (--target=ouroboros) holds a copy of both.\n"
              "  Send DevTools a `notice` in its docs/handoffs/ saying what changed,\n"
              f"  then set PIN in scripts/check-link-layout.py to {got}",
              file=sys.stderr)
        return 1
    print("check-link-layout: programs/linker.ld and LDFLAGS_RUSTSHIM as DevTools's ld was told")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

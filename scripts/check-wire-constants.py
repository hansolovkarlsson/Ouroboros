#!/usr/bin/env python3
"""Check that the wire constants agree between Rust and the Python peers.

`ninep-abi` and the two host peers (`np9p_client.py`, `np9p_server.py`) each
spell the same protocol constants independently — there is no shared header
across the language boundary, and there cannot be one. So they can disagree, and
when they do the symptom is a signature that does not verify, which reads as
"wrong key" or "the guest's crypto is broken" and sends you looking at the
implementation instead of at a constant.

This has already happened once in a near-miss: `SIG_DOMAIN_REQUEST` was written
with a RAW NUL byte in the Rust source rather than the `\0` escape. It was the
right byte, so everything worked — but it rendered as a trailing space, which is
one whitespace-stripping editor away from silently changing the signed bytes.

Checked here rather than in either language's tests because the property is that
the two AGREE; a test inside one of them can only pin what that one says.
"""
import re
import sys
import os

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def rust_consts(path):
    """Byte-string and integer constants from a Rust source file."""
    src = open(path).read()
    out = {}
    for name, body in re.findall(r'pub const (\w+): &\[u8\] = b"((?:[^"\\]|\\.)*)";', src):
        out[name] = body.encode().decode("unicode_escape").encode("latin1")
    for name, val in re.findall(r"pub const (\w+): usize = (\d+);", src):
        out[name] = int(val)
    return out


def py_consts(path):
    """The same, from a Python peer — evaluated literally, never exec'd."""
    src = open(path).read()
    out = {}
    for name, lit in re.findall(r'^(\w+) = (b"(?:[^"\\]|\\.)*")\s*$', src, re.M):
        out[name] = eval(lit)  # a bytes literal matched by the regex above
    # A trailing `# comment` is ordinary in these files, so it must not make a
    # constant invisible to this check - that failure mode is a check that
    # quietly stops looking at the thing it is named for.
    for name, val in re.findall(r"^(\w+) = (\d+)\s*(?:#.*)?$", src, re.M):
        out[name] = int(val)
    return out


# Only constants BOTH sides spell. A name one side does not have is not a
# disagreement — but a name neither has is a check that silently tests nothing,
# so every entry must be found at least once or this script fails.
CHECKED = [
    "SIG_DOMAIN_REQUEST",
    "SIG_DOMAIN_REPLY",
    "NP_NONCE_LEN",
    "NP_NAME_LEN",
    "NP_PUBKEY_LEN",
    "NP_SIG_LEN",
]

# NOT checked: NP_MAC_LEN. Neither peer names it - both write the literal 32 at
# each use - so there is nothing to compare. Listing it anyway would trip this
# script's own "dead entry" guard, which is the correct outcome for a name that
# does not exist on one side, and the wrong way to record a nit about the peers.


def main():
    rust = rust_consts(os.path.join(ROOT, "ninep-abi", "src", "lib.rs"))
    peers = {
        name: py_consts(os.path.join(HERE, name))
        for name in ("np9p_client.py", "np9p_server.py")
    }

    problems = []
    compared = 0
    per_peer = {name: 0 for name in peers}
    for const in CHECKED:
        if const not in rust:
            problems.append(f"{const}: not found in ninep-abi (renamed? then update this list)")
            continue
        seen_anywhere = False
        for peer, consts in peers.items():
            if const not in consts:
                continue
            seen_anywhere = True
            compared += 1
            per_peer[peer] += 1
            if consts[const] != rust[const]:
                problems.append(
                    f"{const}: ninep-abi has {rust[const]!r}, {peer} has {consts[const]!r}"
                )
        if not seen_anywhere:
            problems.append(f"{const}: no Python peer spells it (dead entry in this list)")

    # A check that compared nothing passes for the wrong reason. The regexes
    # above are the fragile part - a formatting change to either language could
    # make them match zero constants - so refuse to report success on silence.
    #
    # PER PEER, not just in total. A single total lets one peer drop out
    # entirely and still clear the bar on the other's matches: today the client
    # spells 6 of these names and the server 4, so a server that stopped parsing
    # would leave 6 >= 6 and this would report success while checking half of
    # what it is named for.
    for peer, n in per_peer.items():
        if n == 0:
            problems.append(f"{peer}: no constants matched at all - did its formatting change?")
    if compared < len(CHECKED):
        problems.append(f"only {compared} constant(s) compared, expected at least {len(CHECKED)}")

    if problems:
        print("check-wire-constants: DISAGREEMENT")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(f"check-wire-constants: {compared} constant(s) agree across Rust and both Python peers")
    return 0


if __name__ == "__main__":
    sys.exit(main())

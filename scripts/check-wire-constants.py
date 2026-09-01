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


# How many of CHECKED each peer is known to spell TODAY. A bare "more than
# zero" is not enough: breaking three of a peer's four declarations still leaves
# one match, and the total stays above the floor because the other peer covers
# the same names - so the script reports success while checking a quarter of
# what it is named for. Confirmed, which is why these are numbers and not a
# truthiness test. Raise a baseline when a peer learns a new constant.
PEER_BASELINE = {
    "np9p_client.py": 6,
    "np9p_server.py": 4,
}


def check_dev_peer_labels(problems):
    """`np9p_client.py`'s short-name map must match `mkclusterkeys.py`'s dev peers.

    Same shape of hazard as the constants above, in a place a reader would not
    look: `mkclusterkeys.py` derives each dev node's key from a seed LABEL
    ("ouroboros-dev-node-b"), while `--peer=` takes the short NAME ("node-b"),
    so the client carries a translation. If the two drift, `--peer=node-b`
    derives a key belonging to no machine, and the reply-verification control
    built on it PASSES FOR THE WRONG REASON - it reports "not verified" against
    a key nobody holds, which an exporter accepting any authorized signature
    would also fail. That is a check that cannot fail, and it is what this one
    exists to stop coming back.

    Parsed textually rather than imported: `mkclusterkeys.py` executes the
    Ed25519 reference at module level (an RFC 8032 self-check, about a second),
    which is the cost the client's own lazy loading was written to avoid.
    """
    mk = open(os.path.join(HERE, "mkclusterkeys.py")).read()
    cl = open(os.path.join(HERE, "np9p_client.py")).read()

    block = re.search(r"DEV_PEERS = \[(.*?)\]", mk, re.S)
    if not block:
        problems.append("mkclusterkeys.py: DEV_PEERS not found (renamed?)")
        return
    want = {n: l for n, _ip, l in re.findall(
        r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)', block.group(1))}

    block = re.search(r"DEV_PEER_LABELS = \{(.*?)\}", cl, re.S)
    if not block:
        problems.append("np9p_client.py: DEV_PEER_LABELS not found (renamed?)")
        return
    got = dict(re.findall(r'"([^"]+)"\s*:\s*"([^"]+)"', block.group(1)))

    if not want:
        problems.append("mkclusterkeys.py: DEV_PEERS parsed empty - a check over nothing")
        return
    if want != got:
        problems.append(
            f"dev peer labels disagree: mkclusterkeys.py has {want}, "
            f"np9p_client.py has {got}")


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
        want = PEER_BASELINE.get(peer, 1)
        if n < want:
            problems.append(
                f"{peer}: matched {n} constant(s), expected at least {want} - "
                "did its formatting change, or was a constant renamed?"
            )
    if compared < len(CHECKED):
        problems.append(f"only {compared} constant(s) compared, expected at least {len(CHECKED)}")

    check_dev_peer_labels(problems)

    if problems:
        print("check-wire-constants: DISAGREEMENT")
        for p in problems:
            print(f"  - {p}")
        return 1
    print(f"check-wire-constants: {compared} constant(s) agree across Rust and both Python peers, "
          "and the dev peer labels agree")
    return 0


if __name__ == "__main__":
    sys.exit(main())

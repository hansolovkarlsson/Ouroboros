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
    # Not an auth field, but the same hazard: a fixed-width record whose length
    # is hardcoded in all three implementations. A wrong value here does not
    # fail a signature, it silently misparses every `ls -l` over a remote mount.
    "STAT_INFO_LEN",
]

# NOT checked: NP_MAC_LEN. Neither peer names it - both write the literal 32 at
# each use - so there is nothing to compare. Listing it anyway would trip this
# script's own "dead entry" guard, which is the correct outcome for a name that
# does not exist on one side, and the wrong way to record a nit about the peers.
#
# NOT checked either: STAT_INFO_LEN's field OFFSETS (STAT_SIZE_OFF and the
# rest). Only np9p_server.py spells them - it is the only peer that BUILDS a
# StatInfo, the client just forwards the bytes - so there is no second
# declaration to compare, same as NP_MAC_LEN above. The record's LENGTH is
# checked, which is the value whose drift breaks parsing outright.


# How many of CHECKED each peer is known to spell TODAY. A bare "more than
# zero" is not enough: breaking three of a peer's four declarations still leaves
# one match, and the total stays above the floor because the other peer covers
# the same names - so the script reports success while checking a quarter of
# what it is named for. Confirmed, which is why these are numbers and not a
# truthiness test. Raise a baseline when a peer learns a new constant.
PEER_BASELINE = {
    "np9p_client.py": 7,
    # Was 4, which BAKED THE GAP IN AS CORRECT: the server spelled the
    # public-key length with a private name and the nonce as a bare literal, so
    # the two fields deciding which guests it serves were skipped - and the
    # reduced count was recorded as expected. Both now use the shared names; the
    # floor rises with them, or the rename could be undone without this
    # noticing.
    "np9p_server.py": 7,
}


def check_dev_peer_labels(problems):
    """BOTH peers' short-name maps must match `mkclusterkeys.py`'s dev peers.

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

    block = re.search(r"DEV_PEERS = \[(.*?)\]", mk, re.S)
    if not block:
        problems.append("mkclusterkeys.py: DEV_PEERS not found (renamed?)")
        return
    want = {n: l for n, _ip, l in re.findall(
        r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)', block.group(1))}
    if not want:
        problems.append("mkclusterkeys.py: DEV_PEERS parsed empty - a check over nothing")
        return

    # BOTH peers, not one. This compared only the client, so renaming a dev
    # identity passed the check and broke the SERVER silently - the half that
    # decides which guests `make run-image-9p-client` serves.
    for peer in ("np9p_client.py", "np9p_server.py"):
        src = open(os.path.join(HERE, peer)).read()
        block = re.search(r"DEV_PEER_LABELS = \{(.*?)\}", src, re.S)
        if not block:
            problems.append(f"{peer}: DEV_PEER_LABELS not found (renamed?)")
            continue
        got = dict(re.findall(r'"([^"]+)"\s*:\s*"([^"]+)"', block.group(1)))
        if want != got:
            problems.append(
                f"dev peer labels disagree: mkclusterkeys.py has {want}, "
                f"{peer} has {got}")
        # A REAL seed label spelled anywhere OUTSIDE the table is a copy the
        # table cannot govern - which is exactly how the server held two of
        # them. Matched against the labels `mkclusterkeys.py` actually derives
        # keys from, not an `ouroboros-dev-` prefix: the retired MAC key
        # `ouroboros-dev-cluster-key-v1` shares that prefix and is not a peer.
        #
        # THE TABLE REGION IS CUT OUT FIRST. Written at first as "a label not in
        # the table", which could not fail for its stated reason: the bug is a
        # label that IS in the table and is ALSO copied somewhere else, so the
        # condition excluded exactly the case it was written to catch. Found by
        # reverting `host_seed()` to its literal and watching this pass.
        outside = src.replace(block.group(0), "")
        stray = sorted({l for l in want.values()
                        if f'"{l}"' in outside or f'b"{l}"' in outside})
        if stray:
            problems.append(
                f"{peer}: dev seed label(s) spelled outside DEV_PEER_LABELS: "
                f"{', '.join(stray)}")


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

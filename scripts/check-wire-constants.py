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
    # The STATUS CODES are `u64` written as an offset from the top of the range
    # (`u64::MAX - 30`), not as a literal, and both Python peers hand-transcribe
    # them as `(1 << 64) - 1 - 30`. That arithmetic is done twice by hand in two
    # languages for values whose drift is silent - `ulib::fs_presence` branches
    # on FS_ERR_NOT_FOUND, so a wrong one switches a destructive-overwrite guard
    # rather than printing a wrong message. Parsed narrowly, matching only that
    # idiom: anything else stays invisible rather than being guessed at.
    for name, off in re.findall(r"pub const (\w+): u64 = u64::MAX - (\d+);", src):
        out[name] = (1 << 64) - 1 - int(off)
    for name in re.findall(r"pub const (\w+): u64 = u64::MAX;", src):
        out[name] = (1 << 64) - 1
    # FLAG BITS are `u32` written as a shift (`1 << 0`), which neither pattern
    # above sees: the integer one wants `usize` and a literal. STAT_FLAG_DIR
    # was invisible to BOTH languages for that reason, so the bit deciding
    # whether a remote entry lists as a directory was pinned by nothing while
    # the script reported agreement - a check that silently did not cover the
    # constant it was added alongside.
    for name, sh in re.findall(r"pub const (\w+): u32 = 1 << (\d+);", src):
        out[name] = 1 << int(sh)
    for name, val in re.findall(r"pub const (\w+): u32 = (\d+);", src):
        out[name] = int(val)
    return out


def py_consts(path):
    """The same, from a Python peer — evaluated literally, never exec'd."""
    src = open(path).read()
    out = {}
    # `(1 << 64) - 1 - 30`, the peers' spelling of `u64::MAX - 30`. Matched
    # before the bare-integer pattern below, which would not see it at all.
    for name, off in re.findall(
            r"^(\w+) = \(1 << 64\) - 1 - (\d+)\s*(?:#.*)?$", src, re.M):
        out[name] = (1 << 64) - 1 - int(off)
    for name in re.findall(r"^(\w+) = \(1 << 64\) - 1\s*(?:#.*)?$", src, re.M):
        out[name] = (1 << 64) - 1
    for name, lit in re.findall(r'^(\w+) = (b"(?:[^"\\]|\\.)*")\s*$', src, re.M):
        out[name] = eval(lit)  # a bytes literal matched by the regex above
    # A trailing `# comment` is ordinary in these files, so it must not make a
    # constant invisible to this check - that failure mode is a check that
    # quietly stops looking at the thing it is named for.
    for name, val in re.findall(r"^(\w+) = (\d+)\s*(?:#.*)?$", src, re.M):
        out[name] = int(val)
    # The peers' spelling of a flag bit. This also matches np9p_client.py's
    # `REPLY_UNVERIFIED = 1 << 64`, a deliberate out-of-range sentinel that is
    # not a wire constant - harmless, since only names listed in CHECKED are
    # ever compared, and it is not one.
    for name, sh in re.findall(r"^(\w+) = 1 << (\d+)\s*(?:#.*)?$", src, re.M):
        out[name] = 1 << int(sh)
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
    # Not auth fields, but the same hazard: a fixed-width record whose length
    # AND field offsets are hardcoded across implementations. A wrong value
    # here does not fail a signature - it silently misparses every `ls -l` over
    # a remote mount, which is a well-formed wrong answer rather than an error.
    "STAT_INFO_LEN",
    "STAT_SIZE_OFF",
    "STAT_FLAGS_OFF",
    "STAT_TIMEVALID_OFF",
    "STAT_MODEVALID_OFF",
    # Status codes (syscall-abi). FS_ERR_NOT_FOUND is the load-bearing one:
    # `ulib::fs_presence` branches on it to answer Absent rather than Unknown,
    # so a peer spelling it wrong does not print a vaguer message - it switches
    # `mv`/`cp`'s destructive-overwrite guard into its "could not tell" arm.
    "NO_FS",
    "FS_ERR_NOT_FOUND",
    "FS_ERR_AUTH",
    # Server-only: it is what the read-only export now refuses mutations with,
    # and netd copies a status through verbatim, so the value reaches a guest.
    "FS_ERR_READ_ONLY",
    # The dir bit of a StatInfo's flags word. Move it in ninep-abi while a peer
    # keeps writing bit 0 and every remote directory lists as a FILE, with no
    # error anywhere - the well-formed wrong answer this script exists to stop.
    "STAT_FLAG_DIR",
    # The generic failure sentinel. Server-only (the client reads statuses, it
    # does not send them), and free coverage once the `u64::MAX` pattern
    # existed - found by asking which names a peer and Rust BOTH spell that
    # this list does not mention, rather than by remembering to add it.
    "FS_ERROR",
    # "the server has no arm for that verb", reserved 2026-09-05. It replaced
    # FS_ERROR in the read-only server's fallthrough, so its VALUE now decides
    # whether a guest says "does not implement this request" or "no such file
    # or directory" - the whole point of reserving it. Reserving it also moved
    # FS_ERR_MIN (the band was full from MAX-1 to MAX-38), which is exactly the
    # kind of shift a hand-mirrored copy misses.
    "FS_ERR_NO_SUCH_VERB",
]

# NOT checked: NP_MAC_LEN. Neither peer names it - both write the literal 32 at
# each use - so there is nothing to compare. Listing it anyway would trip this
# script's own "dead entry" guard, which is the correct outcome for a name that
# does not exist on one side, and the wrong way to record a nit about the peers.
#
# The STAT_* OFFSETS above were first left out, with a note claiming they were
# NP_MAC_LEN's situation - "only np9p_server.py spells them, so there is no
# second declaration to compare". BOTH HALVES OF THAT WERE WRONG, and the
# reasoning is worth keeping because it is an easy mistake to repeat:
#
#   - This script does not compare the peers to EACH OTHER. It compares each
#     peer to `ninep-abi` (see `seen_anywhere` in main), so ONE peer spelling a
#     name is enough for that name to be checked. A server-only constant is
#     fully checkable.
#   - NP_MAC_LEN differs in kind, not degree: `ninep-abi` does not declare it
#     at all, so listing it would trip the "not found in ninep-abi" guard.
#     Being spelled by one peer is the normal case; being spelled by no Rust
#     is the excluding one.
#
# The stated risk model was wrong too: it said the LENGTH is "the value whose
# drift breaks parsing outright", implying the offsets were the safer omission.
# The opposite holds. A wrong length IS caught (np9p_client.py checks it),
# while a drifted STAT_FLAGS_OFF makes every remote directory render as a file
# with no error anywhere - and a well-formed wrong answer is strictly worse
# than a parse failure.
#
# STAT_FLAG_DIR was listed here as "genuinely unpinnable" until 2026-09-03: it
# is `u32` where the integer pattern wanted `usize`, and `1 << 0` where both
# wanted a literal, so it was invisible to BOTH languages and adding it to
# CHECKED failed with "not found in ninep-abi". The fix was to widen the
# patterns, not to keep describing the gap - the parser's reach was being
# treated as a property of the constants rather than of the parser.


# How many of CHECKED each peer is known to spell TODAY. A bare "more than
# zero" is not enough: breaking three of a peer's four declarations still leaves
# one match, and the total stays above the floor because the other peer covers
# the same names - so the script reports success while checking a quarter of
# what it is named for. Confirmed, which is why these are numbers and not a
# truthiness test. Raise a baseline when a peer learns a new constant.
PEER_BASELINE = {
    "np9p_client.py": 10,  # 6 auth + STAT_INFO_LEN + 3 status codes
    # Was 4, which BAKED THE GAP IN AS CORRECT: the server spelled the
    # public-key length with a private name and the nonce as a bare literal, so
    # the two fields deciding which guests it serves were skipped - and the
    # reduced count was recorded as expected. Both now use the shared names; the
    # floor rises with them, or the rename could be undone without this
    # noticing.
    "np9p_server.py": 18,  # + 4 STAT_* offsets, FS_ERR_READ_ONLY, STAT_FLAG_DIR, FS_ERROR, FS_ERR_NO_SUCH_VERB
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
    problems_early = []
    # BOTH crates: the verb/frame constants are ninep-abi's, the status codes
    # syscall-abi's, and the peers spell names from each. Checked for collisions
    # when this was added (none) - a name defined in both would silently take
    # whichever loaded last.
    rust, origin = {}, {}
    for crate in ("ninep-abi", "syscall-abi"):
        found = rust_consts(os.path.join(ROOT, crate, "src", "lib.rs"))
        dupes = set(rust) & set(found)
        if dupes:
            problems_early.append(
                f"{sorted(dupes)} declared in more than one crate - this script "
                "merges them, so one silently wins; rename or scope them")
        rust.update(found)
        # WHICH crate each name came from, so a disagreement names the file to
        # open. Reporting "ninep-abi has ..." for a syscall-abi constant sends
        # the reader to a file that does not contain it.
        origin.update({k: crate for k in found})
    peers = {
        name: py_consts(os.path.join(HERE, name))
        for name in ("np9p_client.py", "np9p_server.py")
    }

    problems = list(problems_early)
    compared = 0
    per_peer = {name: 0 for name in peers}
    for const in CHECKED:
        if const not in rust:
            problems.append(
                f"{const}: not found in ninep-abi or syscall-abi "
                "(renamed? then update this list)")
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
                    f"{const}: {origin[const]} has {rust[const]!r}, "
                    f"{peer} has {consts[const]!r}"
                )
        if not seen_anywhere:
            problems.append(f"{const}: no Python peer spells it (dead entry in this list)")

    # A check that compared nothing passes for the wrong reason. The regexes
    # above are the fragile part - a formatting change to either language could
    # make them match zero constants - so refuse to report success on silence.
    #
    # PER PEER, not just in total. A single total lets one peer drop out
    # entirely and still clear the bar on the other's matches: if the server
    # stopped parsing, the client's own matches would carry the total past a
    # combined floor and this would report success while checking half of what
    # it is named for.
    #
    # DELIBERATELY NO LONGER QUOTING THE COUNTS HERE. This comment used to say
    # "the client spells 6 of these names and the server 4"; the 4 was already
    # false when written (the baseline eight lines above had been raised to 6,
    # and its own note records why), and both numbers went stale again the next
    # time a constant was added. The numbers that must be right live in
    # PEER_BASELINE, where the script reads them - a prose copy beside them is
    # a restatement nothing checks.
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

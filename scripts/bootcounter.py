#!/usr/bin/env python3
"""The boot counter's record on the ESP, EFI/ORBS/BOOTID.TXT: the one host-side
copy of its rule.

The kernel (kernel/src/bootid.rs, read_file) trusts a file of exactly 21
bytes, twenty ASCII digits and a newline, whose value fits a u64; anything
else is no counter. Every host tool that carries a counter from an old disk
to a new one reads it here, so the rule is written once on the host:
scripts/bootstick.py imports `value`, and scripts/sdcard.sh runs `carry`.
Two copies of it had already drifted once (the review of #417: the shell
copy took a newline and twenty digits as a record).

    python3 scripts/bootcounter.py carry NEW OLD...
        NEW is the record about to be staged (the build's), each OLD a record
        the disk held before, read only if it exists. The largest OLD value
        is written into NEW when it is above NEW's, or NEW is no record;
        an OLD that exists and is no record is named. On the Pi the file is
        the counter's only store, so a staged record below the old one would
        let a boot hand out a number an earlier boot had. A NEW that is no
        record, with no OLD value to replace it, is refused (exit 1): the
        kernel leaves such a file as it is, so the count would never be
        kept, and the build that staged it is broken.
    python3 scripts/bootcounter.py --self-test
"""
import os
import re
import sys

RECORD_LEN = 21
RECORD = re.compile(rb"[0-9]{20}\n")
U64_MAX = 2**64 - 1


def value(record):
    """The counter in a record, or None if it is not one by the kernel's rule."""
    # fullmatch, not match: `$` would match before a final newline too, so a
    # 22-byte record passed (the second review of #293).
    if not record or not RECORD.fullmatch(record):
        return None
    v = int(record[:20])
    return v if v <= U64_MAX else None


def record(v):
    """The 21-byte record of `v`."""
    if not 0 <= v <= U64_MAX:
        raise ValueError(f"{v} is not a u64")
    return b"%020d\n" % v


def read(path):
    with open(path, "rb") as f:
        return f.read(RECORD_LEN + 1)


class Refused(Exception):
    pass


def carry(new, olds, say=print):
    """Raise the record at `new` to the largest value among `olds`, if that is
    larger. Returns the value written, or None if `new` was left as it was.
    Raises Refused when `new` is no record and no old value replaces it."""
    best = None
    for old in olds:
        if not os.path.exists(old):
            continue
        v = value(read(old))
        if v is None:
            say(f"the card's boot counter ({old}) is not a counter record; not carried")
            continue
        best = v if best is None else max(best, v)
    current = value(read(new)) if os.path.exists(new) else None
    if best is None:
        if current is None:
            raise Refused(f"the build's boot counter ({new}) is not a counter record")
        return None
    if current is not None and current >= best:
        return None
    with open(new, "wb") as f:
        f.write(record(best))
    say(f"boot counter kept at {best} (the card's)")
    return best


def self_test():
    results = []
    ok = lambda name, cond: results.append((name, bool(cond)))  # noqa: E731
    ok("a record reads", value(b"00000000000000000042\n") == 42)
    ok("too short is no counter", value(b"42\n") is None)
    ok("nothing is no counter", value(None) is None and value(b"") is None)
    ok("a byte after the record is no counter", value(b"00000000000000000042\n\n") is None)
    ok("a newline before twenty digits is no counter", value(b"\n00000000000000000006") is None)
    ok("no newline at the end is no counter", value(b"000000000000000000420") is None)
    # 21 bytes, so only the digit rule can refuse it (the second review of
    # #417: a two-byte Arabic-Indic digit made 22, and the length refused it).
    ok("a non-digit is no counter", value(b"0000000000000000004x\n") is None)
    ok("u64::MAX reads", value(b"18446744073709551615\n") == U64_MAX)
    ok("u64::MAX + 1 is no counter", value(b"18446744073709551616\n") is None)
    ok("twenty nines is no counter", value(b"99999999999999999999\n") is None)
    ok("a record round-trips", value(record(123456789)) == 123456789 and len(record(0)) == RECORD_LEN)
    try:
        record(U64_MAX + 1)
        ok("a value past u64 has no record", False)
    except ValueError:
        ok("a value past u64 has no record", True)

    import tempfile
    with tempfile.TemporaryDirectory() as d:
        def put(name, data):
            p = os.path.join(d, name)
            if data is None:
                if os.path.exists(p):
                    os.remove(p)
            else:
                with open(p, "wb") as f:
                    f.write(data)
            return p
        quiet = []
        say = quiet.append
        new = put("new", record(0))
        ok("the larger old value is carried", carry(new, [put("a", record(6))], say) == 6 and read(new) == record(6))
        new = put("new", record(9))
        ok("a larger new value stays", carry(new, [put("a", record(6))], say) is None and read(new) == record(9))
        new = put("new", record(6))
        ok("an equal value is not rewritten", carry(new, [put("a", record(6))], say) is None)
        new = put("new", record(0))
        ok("the largest of several olds", carry(new, [put("a", record(3)), put("b", record(7))], say) == 7)
        new = put("new", record(0))
        quiet.clear()
        ok("a bad old is named and not carried",
           carry(new, [put("a", b"6\n")], say) is None and read(new) == record(0) and quiet
           and "not a counter record" in quiet[0])
        new = put("new", b"garbage")
        ok("a bad new takes the old", carry(new, [put("a", record(6))], say) == 6 and read(new) == record(6))
        new = put("new", record(0))
        ok("a missing old is skipped", carry(new, [os.path.join(d, "none")], say) is None)
        for name, bad in (("a bad new with no old is refused", b"garbage"),
                          ("a missing new with no old is refused", None)):
            new = put("new", bad)
            try:
                carry(new, [os.path.join(d, "none")], say)
                ok(name, False)
            except Refused:
                ok(name, True)

    for name, passed in results:
        print(f"  {'ok  ' if passed else 'FAIL'} {name}")
    failed = [n for n, p in results if not p]
    print(f"bootcounter self-test: {len(results) - len(failed)} of {len(results)} passed")
    return 1 if failed else 0


def main(argv):
    if argv == ["--self-test"]:
        return self_test()
    if len(argv) >= 3 and argv[0] == "carry":
        try:
            carry(argv[1], argv[2:], say=lambda s: print(f"sdcard: {s}"))
        except Refused as e:
            print(f"sdcard: {e}; refusing to stage it", file=sys.stderr)
            return 1
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

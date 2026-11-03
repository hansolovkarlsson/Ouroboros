#!/usr/bin/env python3
"""Check that every pointer to a section of a document names one that exists.

A comment or a document says where the story behind it lives with a pointer
of one shape: a markdown file, an apostrophe-s and a quoted section name,

    see docs/CHANGELOG.md's "A real relocating loader" entry
    see `CLAUDE.md`'s "Driver isolation, part 3"

This finds every such pointer in the tracked source and documents and fails
when the file holds no heading (a `#` line outside fenced code) and no bold
lead-in (`**...**`) containing the quoted text, compared without case,
backticks or a trailing period, or when several different ones hold it and
none is it whole: a name that short would stay green when the section it
means is renamed.

WHY IT EXISTS. On 2026-11-03 thirty-four comments in the code, and about as
many pointers in the documents, named sections of CLAUDE.md that had long
since moved to the CHANGELOG, to docs/boot-history.md (#386), or nowhere. A
section is renamed or moved in one file and the pointer to it sits in another,
so no compiler, test or review of the edit sees it: the pointer rots
silently. PR #390 repointed the code; this makes the next rename fail.

WHAT IT CANNOT SEE. That the section named says what the pointer promises.
The review of #390 found five pointers whose heading existed and whose entry
did not hold the claim. Only reading catches that. This checks the name.

WHAT IT SKIPS. The dated records: docs/work-journal/, docs/archive/,
docs/handoffs/, docs/release-notes/ and docs/sessions/. Each says what was
true on its day and is not kept current, so a pointer there that has since
gone stale is a fact about that day. Everything else tracked, of the file
types below, is checked, the CHANGELOG and the postmortems included. This
file is skipped too, since its examples above include a dead pointer.

A pointer's file is resolved as written from the repository root, then
relative to the file holding the pointer, then by its name alone when exactly
one tracked markdown file has it. A pointer whose file cannot be resolved
fails too.

`--self-test` runs the matcher on fixed text, so the check is seen to fail.
"""

import os
import re
import subprocess
import sys

SKIP = (
    "third_party/",
    "docs/work-journal/",
    "docs/archive/",
    "docs/handoffs/",
    "docs/release-notes/",
    "docs/sessions/",
    # This file: its docstring shows a dead pointer as an example.
    "scripts/check-doc-pointers.py",
)
TYPES = re.compile(r"(\.(rs|py|sh|md|toml|ld|c|h|S|s|txt|html)|(^|/)Makefile)$")

# A pointer may wrap across comment lines; joining each line break and the
# comment marker after it into one space lets the pointer match whole.
# Markdown has no comment markers, and stripping one there would eat the
# first `*` of a bold lead-in or the `#` of a heading, so it joins on
# whitespace and blockquote markers alone.
JOIN = re.compile(r"[ \t]*\n[ \t]*(?:///?!?|#|\*|--)?[ \t]*")
JOIN_MD = re.compile(r"[ \t]*\n[ \t]*(?:>[ \t]*)*")
POINTER = re.compile(r"`?([\w./-]+\.md)`?['’]s\s+\"([^\"\n]{3,160})\"")
BOLD = re.compile(r"\*\*(.+?)\*\*")

# Fewer pointers than this means the extraction broke, not that the tree is
# clean: 150 were found on 2026-11-03, once source-map.md's "see ... above"
# references (to sections of the CLAUDE.md it was moved out of) were turned
# into pointers this can check.
FLOOR = 120


def norm(text):
    """Compared without case, backticks or a trailing period: prose quotes
    "known rough edges." for the heading "## Known rough edges"."""
    return text.replace("`", "").strip().rstrip(".").strip().lower()


FENCE = re.compile(r"^[ \t]*(```|~~~).*?^[ \t]*\1[^\n]*$", re.M | re.S)


def anchors(text):
    """The heading lines and bold lead-ins of one markdown text, normalised.
    Fenced code is dropped first: a `#[inline]` or a shell comment in a code
    block is not a heading."""
    text = FENCE.sub("", text)
    heads = [line.lstrip("#").strip() for line in text.split("\n") if line.startswith("#")]
    # Bold markers are paired a paragraph at a time, so one stray `**`
    # cannot shift the pairing of every bold span after it in the file.
    bold = [b for para in re.split(r"\n[ \t>]*\n", text) for b in BOLD.findall(JOIN_MD.sub(" ", para))]
    return [norm(a) for a in heads + bold]


def pointers(text, markdown=False):
    """(file, quoted name) for each pointer in one text."""
    joined = (JOIN_MD if markdown else JOIN).sub(" ", text)
    return [(m.group(1), m.group(2)) for m in POINTER.finditer(joined)]


def resolve(src, path, markdown):
    """A markdown file's references are relative to it, so for a .md source
    the relative reading comes first; code names paths from the root."""
    relative = os.path.normpath(os.path.join(os.path.dirname(src), path))
    order = (relative, path) if src.endswith(".md") else (path, relative)
    for candidate in order:
        if candidate in markdown:
            return candidate
    named = [f for f in markdown if f == path or f.endswith("/" + path)]
    return named[0] if len(named) == 1 else None


def holds(name, anchor_list):
    """None when the name picks out one section, else why not. A name is
    found in a heading or lead-in that contains it; when several different
    ones do, it must equal one of them whole. Otherwise renaming the section
    it means leaves the others to keep it green: "Phase 4" was satisfied by
    "Cluster Phase 4a" and "4b" alone."""
    want = norm(name)
    hits = set(a for a in anchor_list if want in a)
    if not hits:
        return "no heading or bold lead-in holds it"
    if len(hits) > 1 and want not in hits:
        return f"{len(hits)} headings or lead-ins hold it; quote enough of one to name it alone"
    return None


def check(root):
    tracked = subprocess.check_output(["git", "ls-files"], cwd=root, text=True).split("\n")
    tracked = [f for f in tracked if f and os.path.isfile(os.path.join(root, f))]
    markdown = set(f for f in tracked if f.endswith(".md"))
    cache = {}
    found, bad = 0, []
    for src in tracked:
        if src.startswith(SKIP) or not TYPES.search(src):
            continue
        with open(os.path.join(root, src), encoding="utf-8", errors="replace") as fh:
            text = fh.read()
        for path, name in pointers(text, src.endswith(".md")):
            found += 1
            target = resolve(src, path, markdown)
            if target is None:
                bad.append(f"{src}: {path}'s \"{name}\": no such document")
                continue
            if target not in cache:
                with open(os.path.join(root, target), encoding="utf-8", errors="replace") as fh:
                    cache[target] = anchors(fh.read())
            why = holds(name, cache[target])
            if why:
                bad.append(f"{src}: {path}'s \"{name}\": {why} in {target}")
    return found, bad


def self_test():
    doc = ("# Title\n\n## A real relocating loader - and more\n\nstray ** here\n\n"
           "**Network stack, Stage 1.** text\n\n## Phase 4: writes\n\n"
           "## Cluster Phase 4a: cpu\n\n## Cluster Phase 4b: import\n\n"
           "```rust\n#[inline(always)]\n```\n")
    a = anchors(doc)
    cases = [
        ("A real relocating loader", True),
        ("a real RELOCATING loader.", True),
        ("Network stack, Stage 1.", True),
        ("Network stack, Stage 2", False),
        ("Driver isolation, part 3", False),
        ("Phase 4: writes", True),
        ("Phase 4", False),  # three headings hold it and none is it
        ("inline(always)", False),  # a line in fenced code is not a heading
    ]
    failed = [n for n, want in cases if (holds(n, a) is None) != want]
    # With the one heading it means renamed, "Phase 4" must still fail.
    renamed = anchors(doc.replace("## Phase 4: writes", "## Writes"))
    if holds("Phase 4: writes", renamed) is None or holds("Phase 4", renamed) is None:
        failed.append("a renamed heading kept its pointer green")
    wrapped = "// see docs/CHANGELOG.md's \"A real\n    // relocating loader\" entry\n"
    got = pointers(wrapped)
    if got != [("docs/CHANGELOG.md", "A real relocating loader")]:
        failed.append(f"wrapped pointer read as {got}")
    if pointers("see `CLAUDE.md`'s \"Toolchain\" section") != [("CLAUDE.md", "Toolchain")]:
        failed.append("backticked pointer not found")
    if resolve("kernel/x.rs", "ROADMAP.md", {"docs/ROADMAP.md", "README.md"}) != "docs/ROADMAP.md":
        failed.append("bare file name not resolved")
    if resolve("x.rs", "README.md", {"docs/README.md", "README.md"}) != "README.md":
        failed.append("code's path not read from the root")
    if resolve("docs/foo.md", "README.md", {"docs/README.md", "README.md"}) != "docs/README.md":
        failed.append("markdown's path not read relative to it")
    if failed:
        print("check-doc-pointers self-test FAILED:", *failed, sep="\n  ")
        return 1
    print("check-doc-pointers self-test: ok")
    return 0


def main():
    if "--self-test" in sys.argv:
        return self_test()
    root = subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip()
    found, bad = check(root)
    if found < FLOOR:
        print(f"check-doc-pointers: only {found} pointers found, under the floor of {FLOOR}: the extraction is broken")
        return 1
    for line in bad:
        print(line)
    if bad:
        print(f"check-doc-pointers: {len(bad)} of {found} pointers name no section")
        return 1
    print(f"check-doc-pointers: {found} pointers, every one names a section")
    return 0


if __name__ == "__main__":
    sys.exit(main())

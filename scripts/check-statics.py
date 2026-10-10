#!/usr/bin/env python3
"""Every static in the kernel is in the multi-core plan's inventory, once.

    python3 scripts/check-statics.py      (also run by `make test`)

The kernel's mutable statics are sound today by one argument, stated once in
kernel/src/synccell.rs: one core, and never EL1 with interrupts unmasked.
Multi-core (docs/roadmap/roadmap-smp.md) retires that argument, and its plan
classifies every static by owner, since the class decides what the arc does
to it. A static added later, in another file on another day, is the one way
the inventory goes stale without anyone seeing it (the lesson of
docs/postmortems/true-when-written-postmortem.md), so this check lays the
tree beside the table on every `make test`.

What it computes: every `static NAME:` in kernel/src/*.rs (indented ones
too, so a static inside a module or function counts), as `module::NAME`.
What it reads: the inventory table in roadmap-smp.md, the rows that open
with a class letter (`| **A.` to `| **E.`), and every `module::NAME` written
in backticks on such a row. It fails, naming each one, when a static in the
tree is in no row, in two rows, or when a name in a row is no longer in the
tree. Nothing else in the plan is read: a `module::NAME` in the prose
outside the table is a mention, not a classification.
"""
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
KERNEL_SRC = os.path.join(ROOT, "kernel", "src")
PLAN = os.path.join(ROOT, "docs", "roadmap", "roadmap-smp.md")

STATIC_RE = re.compile(r"^\s*(?:pub(?:\(crate\))?\s+)?static\s+(?:mut\s+)?([A-Z_][A-Z_0-9]*)\s*:", re.M)
ROW_RE = re.compile(r"^\| \*\*([A-E])\.")
NAME_RE = re.compile(r"`([a-z_0-9]+)::([A-Z_][A-Z_0-9]*)`")


def tree_statics():
    found = {}
    for name in sorted(os.listdir(KERNEL_SRC)):
        if not name.endswith(".rs"):
            continue
        module = name[:-3]
        with open(os.path.join(KERNEL_SRC, name)) as fh:
            src = fh.read()
        for m in STATIC_RE.finditer(src):
            line = src.count("\n", 0, m.start()) + 1
            found[f"{module}::{m.group(1)}"] = line
    return found


def table_statics():
    """{qualified name: [class letters it appears under]}"""
    classified = {}
    rows = 0
    with open(PLAN) as fh:
        for line in fh:
            row = ROW_RE.match(line)
            if not row:
                continue
            rows += 1
            for module, name in NAME_RE.findall(line):
                classified.setdefault(f"{module}::{name}", []).append(row.group(1))
    return classified, rows


def main() -> int:
    tree = tree_statics()
    table, rows = table_statics()
    problems = []
    if rows == 0:
        problems.append(f"no class rows found in {os.path.relpath(PLAN, ROOT)}")
    for q, line in sorted(tree.items()):
        classes = table.get(q, [])
        if not classes:
            problems.append(f"{q} (kernel/src/{q.split('::')[0]}.rs:{line}) is in no class row")
        elif len(classes) > 1:
            problems.append(f"{q} is in rows {', '.join(classes)}: a static has one owner")
    for q in sorted(table):
        if q not in tree:
            problems.append(f"{q} is in row {table[q][0]} but not in the tree")
    per_class = {}
    for q, classes in table.items():
        if q in tree and len(classes) == 1:
            per_class[classes[0]] = per_class.get(classes[0], 0) + 1
    counts = ", ".join(f"{c} {per_class.get(c, 0)}" for c in "ABCDE")
    for p in problems:
        print(f"FAIL {p}")
    if problems:
        print(f"{len(problems)} problem(s): {len(tree)} statics in the tree, {len(table)} names in the table")
        return 1
    print(f"ok   {len(tree)} kernel statics, each in one class of the multi-core inventory ({counts})")
    return 0


if __name__ == "__main__":
    sys.exit(main())

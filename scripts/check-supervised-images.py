#!/usr/bin/env python3
"""Check that every supervised server's built image fits the supervisor's
arena, so each one can be restarted after a crash or a wedge.

The kernel keeps a copy of each supervised server's ELF image from the boot
(`kernel/src/supervisor.rs`, `IMAGE_ARENA`), so a dead server can be loaded
again without a filesystem, which matters because one of them is the
filesystem. An image that does not fit is not a boot failure: the loader logs
one warning ("image too large to keep for crash recovery") and the server
runs unsupervised for the whole boot. That warning scrolled past on every
boot of the Pi from at least `7512000f` to 2026-11-16, with fsd at 197 KB
against a fixed 192 KB slot, and once before that at 137 KB against 128 KB
(the review-and-split postmortem). Nothing failed either time.

So the property is computed, at the end of every `make esp`, from the files
the boot will read:

- the arena's size, from `const IMAGE_ARENA: usize = N * 1024;` in
  supervisor.rs (the line's comment says this script reads it);
- which images are supervised, from loader.rs: each `supervisor::register`
  call outside a `//` comment, and the image named by the warning it logs
  ("Ouroboros kernel: NAME.BIN - {} - ... won't be restartable"); a call in
  any other kernel source is refused, since this would not see its image;
- each image's size, from the staged ESP's `EFI/ORBS/`.

It fails if the number of register calls and named images differ, if one is
missing from the ESP, if there are more than MAX_SUPERVISED, or if the images
do not fit with the worst case of `register`'s 16-byte alignment (up to 15
bytes before each image after the first), which holds whatever order main.rs
loads them in.

    python3 scripts/check-supervised-images.py build/esp
    python3 scripts/check-supervised-images.py --self-test
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SUPERVISOR = os.path.join(ROOT, "kernel", "src", "supervisor.rs")
LOADER = os.path.join(ROOT, "kernel", "src", "loader.rs")

ARENA_RE = re.compile(r"^const IMAGE_ARENA: usize = (\d+) \* 1024;$", re.M)
MAX_RE = re.compile(r"^const MAX_SUPERVISED: usize = (\d+);$", re.M)
REGISTER_RE = re.compile(r"supervisor::register\(")
NAME_RE = re.compile(r'"Ouroboros kernel: ([A-Z0-9]+\.BIN) - \{\} - [^"]*restartable this boot"')
ALIGN = 16


class Refused(Exception):
    pass


def code(text):
    """`text` with `//` comments removed, line by line: what a register call
    is counted in, so a comment that mentions one counts for nothing. (No
    `//` inside a string literal in these files; a warning's text has none.)"""
    return "\n".join(line.split("//", 1)[0] for line in text.split("\n"))


def read_sources(supervisor_text, loader_text, others=()):
    """(arena bytes, max supervised, [image names]) from the two sources.
    `others` is (name, text) for every other kernel source: a register call
    in one of them is refused, since its image would be held to nothing."""
    for name, text in others:
        if REGISTER_RE.search(code(text)):
            raise Refused(
                f"{name}: a supervisor::register call outside loader.rs; this check reads the supervised images "
                "from loader.rs alone, so that image would never be held to the arena"
            )
    arena = ARENA_RE.findall(supervisor_text)
    if len(arena) != 1:
        raise Refused(f"supervisor.rs: expected one `const IMAGE_ARENA: usize = N * 1024;`, found {len(arena)}")
    most = MAX_RE.findall(supervisor_text)
    if len(most) != 1:
        raise Refused(f"supervisor.rs: expected one `const MAX_SUPERVISED: usize = N;`, found {len(most)}")
    calls = len(REGISTER_RE.findall(code(loader_text)))
    names = NAME_RE.findall(loader_text)
    if calls == 0:
        raise Refused("loader.rs: no supervisor::register call found")
    if calls != len(names):
        raise Refused(
            f"loader.rs: {calls} supervisor::register calls but {len(names)} images named by their warnings "
            f"({', '.join(names) or 'none'}): each register call must log the image it kept"
        )
    if len(set(names)) != len(names):
        raise Refused(f"loader.rs: an image is registered twice: {', '.join(names)}")
    return int(arena[0]) * 1024, int(most[0]), names


def fit(arena, most, sizes):
    """The problems with `sizes` ({name: bytes or None}) in an arena of
    `arena` bytes and at most `most` entries, and the bytes they need; no
    problems when they fit. `register` puts each image at the next 16-byte
    boundary, in the order main.rs loads the servers, so the padding depends
    on that order; this judges the worst case, up to 15 bytes before each
    image after the first, which fits in every order or is refused."""
    problems = []
    if len(sizes) > most:
        problems.append(f"{len(sizes)} supervised images, more than MAX_SUPERVISED ({most})")
    present = []
    for name, size in sizes.items():
        if size is None:
            problems.append(f"{name} is not in the ESP")
        elif size == 0:
            problems.append(f"{name} is empty")
        else:
            present.append(size)
    need = sum(present) + (ALIGN - 1) * max(len(present) - 1, 0)
    if need > arena:
        problems.append(
            f"the images need up to {need} bytes with their alignment, {need - arena} more than the arena's {arena}"
        )
    return problems, need


def others():
    """(name, text) of every kernel source but loader.rs and supervisor.rs."""
    src = os.path.dirname(LOADER)
    out = []
    for name in sorted(os.listdir(src)):
        path = os.path.join(src, name)
        if name.endswith(".rs") and path not in (LOADER, SUPERVISOR):
            with open(path) as f:
                out.append((name, f.read()))
    return out


def check(esp):
    with open(SUPERVISOR) as f, open(LOADER) as g:
        arena, most, names = read_sources(f.read(), g.read(), others())
    sizes = {}
    for name in names:
        path = os.path.join(esp, "EFI", "ORBS", name)
        sizes[name] = os.path.getsize(path) if os.path.isfile(path) else None
    problems, used = fit(arena, most, sizes)
    listed = ", ".join(f"{n} {s // 1024 if s else 0} KB" for n, s in sizes.items())
    if problems:
        for p in problems:
            print(f"check-supervised-images: FAIL: {p}")
        print(f"check-supervised-images: arena {arena // 1024} KB (supervisor.rs IMAGE_ARENA); images: {listed}")
        print("check-supervised-images: a server whose image is not kept cannot be restarted after a crash or a wedge")
        return 1
    print(f"check-supervised-images: ok: {len(sizes)} images, at most {used // 1024} KB of the {arena // 1024} KB arena ({listed})")
    return 0


def self_test():
    sup = "const IMAGE_ARENA: usize = 4 * 1024;\nconst MAX_SUPERVISED: usize = 2;\n"
    two = (
        'supervisor::register(A, &x)\n"Ouroboros kernel: A.BIN - {} - the server won\'t be restartable this boot"\n'
        'supervisor::register(B, &y)\n"Ouroboros kernel: B.BIN - {} - the b server won\'t be restartable this boot"\n'
    )
    assert read_sources(sup, two) == (4096, 2, ["A.BIN", "B.BIN"])
    # A register call whose warning names no image, and the reverse.
    for bad in (two + "supervisor::register(C, &z)\n", two.replace("A.BIN - {}", "A.BIN: {}")):
        try:
            read_sources(sup, bad)
            raise AssertionError("refused nothing")
        except Refused:
            pass
    for bad_sup in ("const MAX_SUPERVISED: usize = 2;\n", sup + sup, sup.replace("4 * 1024", "4096")):
        try:
            read_sources(bad_sup, two)
            raise AssertionError("refused nothing")
        except Refused:
            pass
    # Exactly full fits; one byte more does not; the worst case's padding
    # counts, whatever the sizes' own alignment.
    assert fit(4096, 2, {"A": 2048, "B": 2033})[0] == []
    assert fit(4096, 2, {"A": 2048, "B": 2034})[0] != []
    assert fit(4096, 2, {"A": 2047, "B": 2035})[0] != []
    assert fit(4096, 1, {"A": 4096})[0] == []
    # Each image under the arena but not their sum: the case a per-image
    # cap would have passed.
    assert fit(4096, 2, {"A": 3000, "B": 3000})[0] != []
    assert fit(4096, 1, {"A": 10, "B": 10})[0] != []
    assert fit(4096, 2, {"A": None})[0] != []
    assert fit(4096, 2, {"A": 0})[0] != []
    # The same sizes in either order: one answer.
    assert fit(4096, 2, {"A": 2047, "B": 2033})[0] == fit(4096, 2, {"B": 2033, "A": 2047})[0]
    # A comment naming a register call is not one; a call in another file
    # is refused.
    commented = two + "// supervisor::register(C, &z) is how a fifth would be added\n"
    assert read_sources(sup, commented) == (4096, 2, ["A.BIN", "B.BIN"])
    try:
        read_sources(sup, two, [("main.rs", "    supervisor::register(5, &img);\n")])
        raise AssertionError("refused nothing")
    except Refused:
        pass
    assert read_sources(sup, two, [("main.rs", "    // supervisor::register(5, &img);\n")])[2] == ["A.BIN", "B.BIN"]
    # The real sources parse.
    with open(SUPERVISOR) as f, open(LOADER) as g:
        arena, most, names = read_sources(f.read(), g.read(), others())
    assert "FSD.BIN" in names and len(names) <= most and arena > 0, (arena, most, names)
    print(f"check-supervised-images: self-test ok ({len(names)} images registered in loader.rs, arena {arena // 1024} KB)")
    return 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        sys.exit(self_test())
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    try:
        sys.exit(check(sys.argv[1]))
    except Refused as e:
        print(f"check-supervised-images: FAIL: {e}")
        sys.exit(1)

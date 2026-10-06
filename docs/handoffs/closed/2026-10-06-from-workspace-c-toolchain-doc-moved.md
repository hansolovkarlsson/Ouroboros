# The C toolchain plan moved into DevTools

- **From:** workspace
- **To:** Ouroboros
- **Date:** 2026-10-06
- **Kind:** notice
- **Status:** done

## What is asked

Nothing is required. The workspace document `~/Projects/docs/c-compiler-toolchain.md`
now lives at `~/Projects/DevTools/docs/c-compiler-toolchain.md`, at Hans's
request. The text and section anchors are unchanged, apart from the opening
note and two relative links inside it. The old path holds a short pointer to
the new one, so links here still resolve, but the pointer is meant to go once
the links that name it are updated.

## Why

`grep -rn c-compiler-toolchain docs/` in this repository finds the path in
`docs/ROADMAP.md` line 2221 and `docs/roadmap/roadmap-c-hosting.md` line 14. From a file in `docs/`, a link to the new place is
`../../DevTools/docs/c-compiler-toolchain.md`; from `docs/roadmap/`, one
level deeper.

## Reply

Noted 2026-10-06 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing to start, and the two links it names now point
at the new place.

Checked: `~/Projects/DevTools/docs/c-compiler-toolchain.md` exists, and the
old path holds the pointer. `docs/ROADMAP.md`'s toolchain paragraph and
`docs/roadmap/roadmap-c-hosting.md`'s "Why this exists" name
`~/Projects/DevTools/docs/c-compiler-toolchain.md`, kept as home-relative
paths as they were written, not links, since both point outside this
repository. Run here, `git grep -n 'Projects/docs/c-compiler-toolchain' --
docs ':!docs/handoffs'` prints nothing, so the pointer can go whenever the
workspace wants.

# Proem's requests now come from DevTools, and its tree has moved

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-06
- **Kind:** notice
- **Status:** done

## What is asked

Nothing new. This says who now owns Proem's notes here, and where Proem's
source is for anything here that builds or reads it.

Proem moved into DevTools (`~/Projects/DevTools`) on 2026-10-06, with its
code in `DevTools/proem/` (`lib/`, `driver/`, `tests/` and `Makefile`, laid
out as before) and its records merged into DevTools's `docs/`. Edit moved
the same way earlier the same day, and you have a notice for that,
`2026-10-06-from-devtools-edit-moved.md`.

- **The accepted `2026-10-01-from-proem-system-dirs.md` is DevTools's
  request now.** Replies and further notes about the preprocessor go to
  DevTools's `docs/handoffs/` and come from DevTools. The roadmap item it
  blocks, "Proem running on Ouroboros", is in
  `~/Projects/DevTools/docs/ROADMAP.md` under **Proem**. The four done
  notes from Proem in `closed/` stay as they are; DevTools has their
  outcome on record (Proem's completed entry 30).
- **Where Proem's source is.** `docs/roadmap/roadmap-c-hosting.md` line 111
  plans to build `libproem.a` and `driver/proem.c` from `$(PROEM_DIR)`,
  `../Proem` by default, and `docs/ROADMAP.md` line 2413 quotes the same.
  `~/Projects/Proem` is gone: later on 2026-10-06 it was moved to
  `~/Projects/archive/Proem`, frozen at `3cafbcb`, so `../Proem` now finds
  nothing. The current source is `../DevTools/proem`. Nothing in this tree's `Makefile` or `scripts/` names
  `../Proem` yet.
- **What reads this tree from DevTools.** `make pico-check`, `make
  mem-peak` and `make ouroboros-build` in `DevTools/proem/` read picolibc's
  headers and `CFLAGS_OS` from `../../Ouroboros` (from that directory, this
  tree) unless `OUROBOROS` says otherwise. They passed on 2026-10-06.

## Why

`git -C ~/Projects/DevTools log --oneline -1 b2b7d80` prints the merge that
brought Proem in. `~/Projects/DevTools/docs/ROADMAP.md`, "Bring Proem in",
records the move. `grep -n PROEM_DIR docs/roadmap/roadmap-c-hosting.md`
here finds line 111.

## Reply

Noted 2026-10-06 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing to start.

Checked: DevTools `b2b7d80` brings Proem in, and `~/Projects/Proem` is gone,
archived at `~/Projects/archive/Proem`. The later rename to cpp
(`2026-10-06-from-devtools-cpp-renamed.md`, triaged in the same commit)
supersedes the `DevTools/proem` paths this note gives, so the plan and the
roadmap name `../DevTools/cpp`. `2026-10-01-from-proem-system-dirs.md` stays
accepted, and its reply now records that it is DevTools's request.
`docs/ROADMAP.md`'s "CPP is now Proem" paragraph says Proem's notes are
DevTools's requests. The four closed notes from Proem stay as they are.
Nothing in this tree's `Makefile` or `scripts/` names `../Proem`, as the
note says.

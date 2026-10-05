# The C-hosting plan still builds `/bin/CPP`; the tool is now Proem

- **From:** workspace
- **To:** Ouroboros
- **Date:** 2026-10-01
- **Kind:** requirement
- **Status:** done
- **Blocks:** nothing yet. Step 8 of `roadmap-c-hosting.md` would build the
  wrong binary from a path that no longer exists.

## What is asked

Rename CPP to Proem in the C-hosting plan, so that step 8 builds and stages
`/bin/proem` from Proem's tree:

- `/bin/CPP` becomes `/bin/proem`, and the `cpp-bin` target `proem-bin`.
- `$(CPP_DIR)`, defaulting to `../CPP`, becomes `$(PROEM_DIR)`, defaulting to
  `../Proem`.
- The driver it compiles is `driver/proem.c`, not `driver/cpp.c`. The library
  is still `lib/*.c`, which Proem's own build archives as `libproem.a`.
- The finish line's command, `cpp -include /include/target.h ...`, becomes
  `proem -include /include/target.h ...`.
- The other mentions of CPP in the plan, and its index line in
  `docs/README.md`, say Proem. A mention that records what happened on
  2026-09-29 may keep the old name with "(now Proem)".

How far to carry the rename, and whether the plan merges first, is
Ouroboros's call.

## Why

CPP was renamed Proem on 2026-10-01. The folder is `~/Projects/Proem`, the
repository `github.com/hansolovkarlsson/Proem`, and `~/Projects/CPP` no
longer exists. Proem's tree at `444e184`:

- `ls ~/Projects/Proem/driver` prints `proem.c`.
- Its `Makefile` builds `bin/proem` (line 50) and `build/libproem.a`
  (line 18). Its `pico-check` target still finds this tree as
  `../Ouroboros` unless `OUROBOROS` says otherwise (line 109), so that side
  needs no change.

The plan exists only on the branch `docs/c-hosting` (`f060b8d`, also on
`origin`), not on `main`:

    git grep -nE 'CPP|cpp' docs/c-hosting -- docs/roadmap/roadmap-c-hosting.md

lists 23 lines. Step 8, at line 100, is the one that would build the wrong
binary: "**Build and stage `/bin/CPP`.** A `cpp-bin` target beside
`cpico-bin`, compiling CPP's `lib/*.c` and `driver/cpp.c` from
`$(CPP_DIR)` (`../CPP` by default ...". The same branch's `docs/README.md`
line 98 indexes the plan by the old name and path.

`main` needs nothing: `git grep -nE '\bCPP\b|Projects/CPP|bin/cpp' main`
finds only the rename notice in `docs/ROADMAP.md`.

## Done when

On whichever branch carries the plan:

    git grep -nE '/bin/CPP|cpp-bin|CPP_DIR|\.\./CPP\b|driver/cpp\.c' <branch> -- docs

prints nothing, and step 8 names `/bin/proem`, `proem-bin`, `PROEM_DIR` with
`../Proem` as its default, and `driver/proem.c`.

## Reply

Accepted 2026-10-01 by Ouroboros, for later: on `docs/ROADMAP.md` under "What
Proem asks of Ouroboros", not started.

Checked: the plan exists only on `docs/c-hosting` (`f060b8d`, on `origin`
too), and `git grep -nE 'CPP|cpp' origin/docs/c-hosting --
docs/roadmap/roadmap-c-hosting.md` lists 23 lines; `main` carries only the
rename notice in `docs/ROADMAP.md`. The rename will be made on that branch,
before the plan merges, so `main` never carries the old names.

Done 2026-10-04 by Ouroboros: `69653a8` on `docs/c-hosting`, pushed to
`origin` (the plan has still not merged, so `main` never carried the old
names). Step 8 now builds and stages `/bin/proem` through a `proem-bin`
target, from `$(PROEM_DIR)` with `../Proem` as its default, compiling
`lib/*.c` (archived by Proem's own build as `libproem.a`) and
`driver/proem.c`, and the finish line runs `proem -include
/include/target.h ...`. The plan's title and other mentions, its paragraph in
the branch's `docs/ROADMAP.md` and its index line in `docs/README.md` say
Proem; the two places that record what was read on 2026-09-29 say "CPP (now
Proem)". Your check, run on the branch:

    git grep -nE '/bin/CPP|cpp-bin|CPP_DIR|\.\./CPP\b|driver/cpp\.c' docs/c-hosting -- docs

prints nothing.

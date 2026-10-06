# Proem is cpp again: the names Ouroboros's plan uses for it change

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-06
- **Kind:** notice
- **Status:** done

## What is asked

Nothing new. This follows `2026-10-06-from-devtools-proem-moved.md`, still
open here: later the same day, inside DevTools, the preprocessor went back
to the name it had before 2026-10-01, cpp. Hans decided it: Proem was a name
for a project of its own, and in DevTools, beside a C compiler in `cc/`, the
Unix name says what the tool is.

What changed, everything at once, as the 2026-10-01 rename did the other
way:

| Before | Now |
| --- | --- |
| `~/Projects/DevTools/proem/` | `~/Projects/DevTools/cpp/` |
| `driver/proem.c` | `driver/cpp.c` |
| `build/libproem.a` | `build/libcpp.a` |
| `bin/proem` | `bin/cpp` |
| `-DPROEM_SYSTEM_DIRS=...` | `-DCPP_SYSTEM_DIRS=...` |
| the `proem_` and `PROEM_` prefixes | `cpp_` and `CPP_` |
| messages `proem: FILE: ...` | `cpp: FILE: ...` |

`lib/` and its file names are unchanged. On Ouroboros the command would
be `/bin/cpp`, which nothing there uses today.

Where this tree names the old ones:

- **The accepted `2026-10-01-from-proem-system-dirs.md`**, DevTools's request
  since the move note, asks for `proem` built from `driver/proem.c` with
  `-DPROEM_SYSTEM_DIRS`. The request is the same; the file and the macro are
  `driver/cpp.c` and `CPP_SYSTEM_DIRS`, and the command is `cpp hello.c`.
- **`docs/roadmap/roadmap-c-hosting.md`** step 8 plans a `proem-bin`
  target, `PROEM_DIR` (whose default the move note already said is now
  `../DevTools/proem`; it is `../DevTools/cpp` now) and `/bin/proem`. Lines
  5 to 7 and 16 say CPP was renamed Proem; that is history and reads
  correctly.
- `docs/ROADMAP.md` line 2413 quotes the same names.

`make pico-check`, `make mem-peak` and `make ouroboros-build` still read
this tree from DevTools, now from `DevTools/cpp/` with `../../Ouroboros` as
the default, and passed after the rename with the same numbers as before
it.

## Why

`git -C ~/Projects/DevTools show --stat HEAD` names the rename commit once
it is made; until then `ls ~/Projects/DevTools/cpp/driver` lists `cpp.c`,
and `grep -rn CPP_SYSTEM_DIRS ~/Projects/DevTools/cpp/driver/cpp.c` finds the
macro. `~/Projects/DevTools/docs/ROADMAP.md`, "Rename Proem back to cpp",
records the decision and the reasons weighed.

## Reply

Noted 2026-10-06 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing to start.

Checked: `~/Projects/DevTools/cpp/driver/cpp.c` exists and names
`CPP_SYSTEM_DIRS`; when this was triaged, the rename was staged in DevTools
and not yet committed, as the note foresaw. Updated here: the C-hosting
plan's opening (a dated note on the move and the name) and its step 8
(`/bin/cpp`, `cpp-bin`, `CPP_DIR` defaulting to `../DevTools/cpp`,
`build/libcpp.a`, `driver/cpp.c`, the finish line's `cpp` command, with the
old names in a dated aside); `docs/ROADMAP.md`'s "CPP is now Proem"
paragraph and its open item for the header stage (`cpp`,
`driver/cpp.c`, `CPP_SYSTEM_DIRS`); and the reply on
`2026-10-01-from-proem-system-dirs.md`. Left as written, being history: the
plan's lines on the 2026-10-01 rename, and `docs/ROADMAP.md`'s done item
"Rename CPP to Proem in the C-hosting plan" (the note's line 2413), which
records that change.

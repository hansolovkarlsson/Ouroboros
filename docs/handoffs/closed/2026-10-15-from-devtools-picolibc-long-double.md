# picolibc rebuilt with `_HAVE_LONG_DOUBLE` breaks `<math.h>` for `cc`

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-15
- **Kind:** finding
- **Status:** done
- **Blocks:** DevTools's `cc --target=ouroboros` compiling any program
  that includes `<math.h>` or `<wchar.h>`, once the rebuild reaches
  `main`; with it `make -C ld test` and `make -C cc elf-check` in
  DevTools

## What is asked

That the picolibc rebuilt for the nano-malloc note
(`docs/handoffs/2026-10-14-from-devtools-nano-malloc.md`) change only
the allocator in `third_party/picolibc-prebuilt/include/picolibc.h`, or,
if the other changes are wanted, that this note says so before they
reach `main`, so DevTools can plan for them. The one that matters to
DevTools today is `_HAVE_LONG_DOUBLE`. Whether the other ten matter
has not been checked.

## Why

This was seen in your working copy, not on `main`. On 2026-10-15 the
working copy was on branch `fix/binned-malloc` (at `9d9203a3`, the same
commit as `origin/main`), with `scripts/build-picolibc.sh` changed by
one option, `-Dnewlib-nano-malloc=false`, and `picolibc.h` and
`lib/libc.a` rebuilt and not committed. Besides `_NANO_MALLOC` now
undefined, `git diff third_party/picolibc-prebuilt/include/picolibc.h`
shows 11 macros that are `#undef` on `main` and `#define` in the
rebuild (counted with `grep -c '^+#define _HAVE'`):

    _HAVE_ALIAS_ATTRIBUTE  _HAVE_ALLOC_SIZE  _HAVE_ATTRIBUTE_ALWAYS_INLINE
    _HAVE_ATTRIBUTE_GNU_INLINE  _HAVE_BITFIELDS_IN_PACKED_STRUCTS
    _HAVE_BUILTIN_COMPLEX  _HAVE_COMPLEX  _HAVE_FORMAT_ATTRIBUTE
    _HAVE_LONG_DOUBLE  _HAVE_NO_BUILTIN_ATTRIBUTE  _HAVE_WEAK_ATTRIBUTE

The build script's change names none of these, so they seem to come
from picolibc's own configure probes. On `main`, `picolibc.h` was last
changed in `aae5311d`.

With `_HAVE_LONG_DOUBLE` defined, `<math.h>` declares `long double`
functions (line 277, `__fpclassifyl`) and `<wchar.h>` does too (line
245). DevTools's `cc` refuses `long double` for Ouroboros, where it is
binary128 in software, which is held on DevTools's roadmap. Against
your working copy, `cc` built in DevTools at `1a56a82`:

    $ printf '#include <math.h>\nint main(void) { return isnan(0.0); }\n' > m.c
    $ cc/bin/cc --target=ouroboros -o /dev/null m.c
    .../Ouroboros/third_party/picolibc-prebuilt/include/math.h:277:27: error:
      long double is not supported yet on this target, where it is binary128 in software
    $ printf '#include <wchar.h>\nint main(void) { return 0; }\n' > w.c
    $ cc/bin/cc --target=ouroboros -o /dev/null w.c
    .../Ouroboros/third_party/picolibc-prebuilt/include/wchar.h:245:1: error:
      long double is not supported yet on this target, where it is binary128 in software

DevTools's `make -C ld test` failed the same way on `math_compare` and
`wint_t_wchar`. Against an export of your `origin/main` at `9d9203a3`
it passes: 203 programs, the ELF oracle 203 and 17, and the PIE check.

## Done when

On your `main`, `third_party/picolibc-prebuilt/include/picolibc.h` has
`_HAVE_LONG_DOUBLE` undefined and DevTools's `make -C ld test` passes
against it, or the reply says it is to stay defined and when, so that
`long double` for Ouroboros moves up DevTools's roadmap.

## Reply

Done 2026-10-15 by Ouroboros, in `56c98079` on branch `fix/binned-malloc`
(on `main` once Hans merges it). Thank you for catching it in the working
copy: it would have reached `main` in this change.

`git diff 9d9203a3 56c98079 -- third_party/picolibc-prebuilt/include` is
one line, `-#define _NANO_MALLOC` / `+#undef _NANO_MALLOC`.
`_HAVE_LONG_DOUBLE` and the other ten stay undefined, and `complex.h` is
not installed. The rebuild you saw was the first of two; the second, with
the cross file fixed, is where the eleven came from.

**Why they appeared, and what holds them back.** They come from picolibc's
configure probes, which have all failed under our cross file since the
port began (2026-08-28): `-nostdlib` and `--ld-path` ride every compile,
clang warns them unused, and meson probes with
`-Werror=unused-command-line-argument`. So `long double`, `_Complex` and
the attributes have been undefined by accident, not by choice. The
allocator needed two of picolibc's flags, which the cross file now passes
by hand, and the probes stay broken, so the configuration you read is
unchanged. Fixing the probes outright is a new item in Ouroboros's
`ROADMAP.md` ("picolibc's optional flags, all 31 probed NO"), and it
**waits on** your `long double` for this target. Ouroboros will send a
`notice` before `picolibc.h` changes. If you would rather the macros
stayed undefined even after the probes are fixed, a note saying so would
settle it. `make -C ld test` passing against this tree is yours to run.

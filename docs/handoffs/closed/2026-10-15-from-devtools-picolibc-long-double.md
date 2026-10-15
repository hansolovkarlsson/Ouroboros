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

Done 2026-10-15 by Ouroboros, on branch `fix/binned-malloc` (#301, on
`main` once Hans merges it). Thank you for catching it in the working copy:
it would have reached `main` in this change.

`third_party/picolibc-prebuilt/include` on the branch is `main`'s byte for
byte: `_HAVE_LONG_DOUBLE` and the other ten stay undefined, and `complex.h`
is not installed. (The branch went back to nano-malloc after a review, so
not even `_NANO_MALLOC` differs now.) `scripts/check-picolibc.py`, run by
`make test`, hashes that tree against a pin, so a change to it fails here
until the pin is updated, which is the moment Ouroboros sends you a
`notice`.

**Where the eleven macros came from.** The rebuild you read had the cross
file's flag probes fixed. Those probes have all failed since the port
began (2026-08-28): the cross file's `--ld-path` rides every compile, clang
warns it unused, and meson probes with `-Werror=unused-command-line-argument`,
so 30 of picolibc's 31 optional flags probe NO and every compiler feature
picolibc probes for reads as missing. So `long double`, `_Complex` and the
attributes have been undefined by accident, not by choice. The probes stay
broken for now; the two flags picolibc's malloc needs are passed by hand.
Fixing the probes is an item in Ouroboros's `ROADMAP.md` ("picolibc's
optional flags"), and it **waits on** your `long double` for this target.
If you would rather the macros stayed undefined even after the probes are
fixed, a note saying so would settle it. `make -C ld test` passing against
this tree is yours to run.

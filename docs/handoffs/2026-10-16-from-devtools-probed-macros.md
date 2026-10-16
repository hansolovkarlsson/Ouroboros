# The eleven probed macros: bring each back on its own condition

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-16
- **Kind:** finding
- **Status:** accepted
- **Blocks:** nothing today; Ouroboros's plan for
  `libc/picolibc-patches/0002-undefine-probed-macros.patch`

This answers the question in the reply to
`docs/handoffs/closed/2026-10-15-from-devtools-picolibc-long-double.md`,
whether DevTools would rather the macros stayed undefined even after
the probes were fixed, and reports a defect in the plan written beside
the patch.

## What is asked

Do not keep the eleven macros undefined for good, and do not bring them
back together when DevTools's `cc` takes `long double`. Split
`0002-undefine-probed-macros.patch` by what each macro costs `cc`:

1. **Seven cost `cc` nothing**, and may be defined whenever Ouroboros
   wants: `_HAVE_ALIAS_ATTRIBUTE`, `_HAVE_BITFIELDS_IN_PACKED_STRUCTS`,
   `_HAVE_BUILTIN_COMPLEX`, `_HAVE_COMPLEX`, `_HAVE_FORMAT_ATTRIBUTE`,
   `_HAVE_NO_BUILTIN_ATTRIBUTE`, `_HAVE_WEAK_ATTRIBUTE`. (Whether
   `<complex.h>` is installed with `_HAVE_COMPLEX` is a separate
   question: `cc` refuses `_Complex`, and says so by
   `__STDC_NO_COMPLEX__`.)
2. **`_HAVE_ALLOC_SIZE`** stays undefined until DevTools sends a
   `notice` that `cc` reads `__alloc_size__`.
3. **`_HAVE_ATTRIBUTE_ALWAYS_INLINE` and `_HAVE_ATTRIBUTE_GNU_INLINE`**
   stay undefined until DevTools sends a `notice` that `cc` reads
   `__declare_extern_inline`.
4. **`_HAVE_LONG_DOUBLE`** stays undefined until DevTools sends a
   `notice` that `cc` takes `long double` for Ouroboros, as the patch's
   README already says.

DevTools's two items for 2 and 3 are small and on its roadmap now;
`long double` stays held there.

## Why

The README of `libc/picolibc-patches/` says the patch "goes, with a
notice to DevTools" when `cc` takes binary128, so all eleven come back
at once. Three of the four that hurt `cc` have nothing to do with
`long double`, so on that day `cc` would lose `stdlib.h`, `malloc.h`,
`ctype.h` and `fenv.h`.

Measured on 2026-10-16 against `origin/main` at `f76e1ad`, exported by
`sh scripts/ouroboros.sh export origin/main` in DevTools, with DevTools's
`cc` at `0455cd8`. Each of the 136 headers under
`third_party/picolibc-prebuilt/include` was compiled alone, `#include
<H>` and an empty `main`, by `cc --target=ouroboros -nostdinc -I
cc/include -I DIR`. DIR was the tree as shipped, then a copy with one
macro's `#undef` made `#define` in `picolibc.h`, for each macro:

| Defined | Headers `cc` then refuses, beyond the 26 it refuses as shipped |
|---|---|
| `_HAVE_LONG_DOUBLE` | `math.h`, `stdlib.h`, `wchar.h`, `fastmath.h`, `sys/iconvnls.h`: "long double is not supported yet on this target, where it is binary128 in software" |
| `_HAVE_ALLOC_SIZE` | `stdlib.h`, `malloc.h`: "cc does not know the attribute '__alloc_size__'" |
| `_HAVE_ATTRIBUTE_ALWAYS_INLINE` and `_HAVE_ATTRIBUTE_GNU_INLINE`, together | `ctype.h` (line 263), `machine/fenv-fp.h` (line 30) through `fenv.h`: "expected a type before '('" |
| each of the other seven | none |

Either inline macro alone changes nothing: `sys/cdefs.h:469` defines
`__declare_extern_inline` only under both, as `extern __inline type
__attribute((gnu_inline, always_inline))`. `cc` reads neither the
`__inline` keyword, the `__attribute` spelling, nor the two attributes.

The seven were checked for more than parsing. `_HAVE_BITFIELDS_IN_PACKED_STRUCTS`,
`_HAVE_COMPLEX` and `_HAVE_BUILTIN_COMPLEX` appear in no installed header
but `picolibc.h`, now that `complex.h` is removed, so they change no
layout or declaration a program sees. `_HAVE_ALIAS_ATTRIBUTE`,
`_HAVE_WEAK_ATTRIBUTE` and `_HAVE_NO_BUILTIN_ATTRIBUTE` only define
helper macros in `sys/cdefs.h` (`__strong_reference`,
`__weak_reference`, `__inhibit_new_builtin_calls`) that no other
installed header uses. `_HAVE_FORMAT_ATTRIBUTE` changes `stdio.h`, which
`cc` reads with it defined.

To reproduce, from DevTools, with `INC` naming a copy of the include
tree whose `picolibc.h` has the macros under test defined:

    for h in $(cd "$INC" && find . -name '*.h' | sed 's|^\./||'); do
        printf '#include <%s>\nint main(void) { return 0; }\n' "$h" > t.c
        cc/bin/cc --target=ouroboros -nostdinc -I cc/include -I "$INC" \
            -o /dev/null t.c 2>&1 | grep -m1 error && echo "  in $h"
    done

The cost on Ouroboros's side, of keeping them all undefined, is its own
clang programs' `long double` prototypes, `<complex.h>` and the format
and `alloc_size` warnings, for a limit that is DevTools's. That is why
the answer is not "keep them undefined".

## Done when

`libc/picolibc-patches/README.md` on Ouroboros's `main` gives each
macro its own condition, as in 1 to 4, and DevTools's `make test
OUROBOROS_REF=origin/main` passes against the headers `main` installs.

## Reply

Accepted 2026-10-16 by Ouroboros. It is an item in `docs/ROADMAP.md`'s
block "What DevTools asks of Ouroboros next", which cites this note, to
be built in a PR of its own, not alongside the cc-and-as work.

Checked before accepting, with the `cc` Ouroboros builds from your
`cc/` at `0455cd8` (`make CCOMP=devtools`), on copies of
`third_party/picolibc-prebuilt/include` as `main` installs it: with
`_HAVE_ALLOC_SIZE` defined, `cc --target=ouroboros -nostdinc -I
cc/include -I <copy>` refuses `#include <stdlib.h>` and still takes
`<stdio.h>`; with `_HAVE_FORMAT_ATTRIBUTE` defined, it takes both; as
shipped, both. That matches your table for the rows checked.

How it will be built, so you can object before it is:

- `0002-undefine-probed-macros.patch` is split by condition: one patch
  for `_HAVE_LONG_DOUBLE`, one for `_HAVE_ALLOC_SIZE`, one for the two
  inline macros, each removed when your `notice` for it arrives. The
  README names each macro and its condition, as your 1 to 4.
- The seven that cost `cc` nothing are defined, as the probes leave them,
  so this project's clang programs get their declarations and warnings
  back. `<complex.h>` stays removed while `cc` refuses `_Complex`.
- That changes `picolibc.h`, which your `cc` reads from this working
  copy, so it is sent to you as a `notice` before `check-picolibc`'s pin
  moves, and your `make test OUROBOROS_REF=origin/main` is the check
  after it merges.

# This project's patches to picolibc

`scripts/build-picolibc.sh` applies every `*.patch` here, in name order, to
the headers it copies into `third_party/picolibc-prebuilt/`, and stops if
one no longer applies. The committed prebuilt tree already carries them. A
change to picolibc's headers goes here as a patch, never only into the
prebuilt tree, where the next regeneration would drop it.

- `0001-math-comparison-builtins.patch`: `<math.h>` takes C11's comparison
  macros (`isgreater` and the rest) as builtins for a compiler that has
  them, as well as for one that says it is GNU. DevTools's `cc` is not GNU,
  has no statement expressions or `__typeof__`, and has the six builtins.
  Asked by DevTools on 2026-10-13
  (`docs/handoffs/closed/2026-10-13-from-devtools-picolibc-comparison-builtins.md`).

The next two keep `<picolibc.h>` from defining a macro DevTools's `cc`
cannot yet take. picolibc's probes define eleven `_HAVE_` macros since they
work (2026-10-16); four of them made `cc` refuse a header, and each patch
goes on its own condition, a `notice` from DevTools, never all at once (its
finding, `docs/handoffs/closed/2026-10-16-from-devtools-probed-macros.md`). A patch
removed is a header change, so it owes `check-picolibc`'s pin, and DevTools
a reply naming the commit. There were three: `0003`, which kept
`_HAVE_ALLOC_SIZE` undefined, was removed on 2026-10-17, when `cc` read
`__alloc_size__` (DevTools's notice,
`docs/handoffs/closed/2026-10-16-from-devtools-alloc-size-read.md`). The
number is not reused.

- `0002-undefine-long-double.patch`: `_HAVE_LONG_DOUBLE`, which makes `cc`
  refuse `math.h`, `stdlib.h`, `wchar.h`, `fastmath.h` and
  `sys/iconvnls.h` ("long double is not supported yet on this target").
  Goes when DevTools sends a notice that `cc` takes `long double` for
  Ouroboros
  (`docs/handoffs/closed/2026-10-15-from-devtools-picolibc-long-double.md`).
  Programs that use `long double` then need the quad-float helpers
  (`__addtf3` and the rest), which nothing here links yet.
- `0004-undefine-extern-inline.patch`: `_HAVE_ATTRIBUTE_ALWAYS_INLINE` and
  `_HAVE_ATTRIBUTE_GNU_INLINE`, which together make `sys/cdefs.h` define
  `__declare_extern_inline`, and `cc` then refuses four headers: `ctype.h`,
  `fenv.h` (through `machine/fenv-fp.h`), and `math.h` and `fastmath.h`
  (both through `machine/math.h`). Either alone changes nothing. Goes when
  DevTools sends a notice that `cc` reads `__declare_extern_inline`.
  DevTools has said that notice will not come for now: `cc` reads the
  macro since its `c06cc3b`, but picolibc's `sys/cdefs.h` drops `__inline`
  for a compiler that is not GNU, so the expansion is not inline under
  any such compiler, and `fenv.h`'s and `math.h`'s inline bodies are GNU
  `asm`. If DevTools goes further, it will ask for a change to
  `sys/cdefs.h` in a note of its own.

The other seven stay as the probes define them, since they cost `cc`
nothing: `_HAVE_ALIAS_ATTRIBUTE`, `_HAVE_BITFIELDS_IN_PACKED_STRUCTS`,
`_HAVE_BUILTIN_COMPLEX`, `_HAVE_COMPLEX`, `_HAVE_FORMAT_ATTRIBUTE`,
`_HAVE_NO_BUILTIN_ATTRIBUTE` and `_HAVE_WEAK_ATTRIBUTE`. Each of the tree's 136
headers compiled alone, `#include <H>` and an empty `main`, by `cc
--target=ouroboros -nostdinc -I <cc>/include -I
third_party/picolibc-prebuilt/include` (DevTools's reproduction): 26
refused with the seven defined, the same 26 as without (25 with `-I
libc/pico/include` added, as the Makefile passes for Edit and cpp, again
the same either way).
`<complex.h>` is still removed, by `scripts/build-picolibc.sh` rather than a
deletion hunk (which would hold the whole file): `cc` refuses `_Complex`,
and the complex functions in `libc.a` need helpers (`__muldc3` and the rest)
nothing here links.

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

The next three keep `<picolibc.h>` from defining a macro DevTools's `cc`
cannot yet take. picolibc's probes define eleven `_HAVE_` macros since they
work (2026-10-16); four of them make `cc` refuse a header, and each patch
goes on its own condition, a `notice` from DevTools, never all at once (its
finding, `docs/handoffs/closed/2026-10-16-from-devtools-probed-macros.md`). A patch
removed is a header change, so it owes `check-picolibc`'s pin, and DevTools
a reply naming the commit.

- `0002-undefine-long-double.patch`: `_HAVE_LONG_DOUBLE`, which makes `cc`
  refuse `math.h`, `stdlib.h`, `wchar.h`, `fastmath.h` and
  `sys/iconvnls.h` ("long double is not supported yet on this target").
  Goes when DevTools sends a notice that `cc` takes `long double` for
  Ouroboros
  (`docs/handoffs/closed/2026-10-15-from-devtools-picolibc-long-double.md`).
  Programs that use `long double` then need the quad-float helpers
  (`__addtf3` and the rest), which nothing here links yet.
- `0003-undefine-alloc-size.patch`: `_HAVE_ALLOC_SIZE`, which makes `cc`
  refuse `stdlib.h` and `malloc.h` ("cc does not know the attribute
  '__alloc_size__'"). Goes when DevTools sends a notice that `cc` reads
  `__alloc_size__`.
- `0004-undefine-extern-inline.patch`: `_HAVE_ATTRIBUTE_ALWAYS_INLINE` and
  `_HAVE_ATTRIBUTE_GNU_INLINE`, which together make `sys/cdefs.h` define
  `__declare_extern_inline`, and `cc` then refuses `ctype.h` and, through
  `fenv.h`, `machine/fenv-fp.h`. Either alone changes nothing. Goes when
  DevTools sends a notice that `cc` reads `__declare_extern_inline`.

The other seven stay as the probes define them, since they cost `cc`
nothing: `_HAVE_ALIAS_ATTRIBUTE`, `_HAVE_BITFIELDS_IN_PACKED_STRUCTS`,
`_HAVE_BUILTIN_COMPLEX`, `_HAVE_COMPLEX`, `_HAVE_FORMAT_ATTRIBUTE`,
`_HAVE_NO_BUILTIN_ATTRIBUTE` and `_HAVE_WEAK_ATTRIBUTE` (each header compiled
alone by `cc`: 26 of 136 refused with them defined, the same 26 as without).
`<complex.h>` is still removed, by `scripts/build-picolibc.sh` rather than a
deletion hunk (which would hold the whole file): `cc` refuses `_Complex`,
and the complex functions in `libc.a` need helpers (`__muldc3` and the rest)
nothing here links.

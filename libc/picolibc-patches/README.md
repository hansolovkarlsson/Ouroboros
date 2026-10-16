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
- `0002-undefine-probed-macros.patch`: `<picolibc.h>` leaves undefined the
  eleven `_HAVE_` macros that picolibc's probes define once they work:
  `_HAVE_ALIAS_ATTRIBUTE`, `_HAVE_ALLOC_SIZE`, `_HAVE_ATTRIBUTE_ALWAYS_INLINE`,
  `_HAVE_ATTRIBUTE_GNU_INLINE`, `_HAVE_BITFIELDS_IN_PACKED_STRUCTS`,
  `_HAVE_BUILTIN_COMPLEX`, `_HAVE_COMPLEX`, `_HAVE_FORMAT_ATTRIBUTE`,
  `_HAVE_LONG_DOUBLE`, `_HAVE_NO_BUILTIN_ATTRIBUTE` and
  `_HAVE_WEAK_ATTRIBUTE`. `scripts/build-picolibc.sh` removes `<complex.h>`
  beside it (a deletion hunk would hold the whole file). So programs see the headers they saw before the
  probes were fixed on 2026-10-16 while the library is built with them.
  DevTools's `cc` refuses `long double` here
  (`docs/handoffs/closed/2026-10-15-from-devtools-picolibc-long-double.md`);
  when it takes binary128, this patch goes, with a notice to DevTools, and
  programs that use `long double` need the quad-float helpers (`__addtf3`
  and the rest), which nothing here links yet.

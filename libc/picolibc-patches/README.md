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

# picolibc's comparison macros by builtin, where the compiler has them

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-13
- **Kind:** requirement
- **Status:** done
- **Blocks:** DevTools's C compiler, `cc`, compiling C11's comparison
  macros (`isgreater` and the rest, C11 7.12.14) for Ouroboros

## What is asked

One condition in the prebuilt picolibc's `include/math.h`, at line 406
of `third_party/picolibc-prebuilt/include/math.h`:

```c
#if __GNUC_PREREQ (2, 97) && !(defined(__riscv) && defined(__clang__))
#define isgreater(__x,__y)	(__builtin_isgreater (__x, __y))
```

to take the builtins also from a compiler that says it has them:

```c
#if (__GNUC_PREREQ (2, 97) && !(defined(__riscv) && defined(__clang__))) \
    || __has_builtin(__builtin_isgreater)
#define isgreater(__x,__y)	(__builtin_isgreater (__x, __y))
```

The six macros under it are unchanged. Nothing changes for clang or GCC,
which already take the first branch. picolibc's `<sys/cdefs.h>` defines
`__has_builtin(x)` as 0 for a compiler without it (`sys/cdefs.h:83`),
and `<math.h>` includes it, so the new test is safe anywhere.

How you make the change is yours: in the prebuilt tree, in the recipe
that builds it, or upstream.

## Why

`cc` is not GNU and does not say it is, so it takes the `#else` branch,
which writes each macro as a GNU statement expression with `__typeof__`:

```
$ printf '#include <math.h>\nint f(double x) { return isgreater(x, 1.0); }\n' > t.c
$ cc/bin/cc --target=ouroboros -o /dev/null t.c
t.c:2:35: error: expected an expression before '{'
```

`cc` has neither extension, and was asked not to grow them for this
(Hans, 2026-10-13). It has the six builtins instead, as of DevTools's
commit `06d7b3d`, since the macOS SDK writes the macros as builtins for
every compiler, and its preprocessor answers `__has_builtin` for each.

The change was tried in a copy of the headers, not in your tree: with
it, `cc --target=ouroboros` compiles a program using all six macros,
and under QEMU, linked with your picolibc, it prints `1 1 1 0 1 0` for
ordered operands and `0 0 0 0 0 1` with a NaN, as clang's builtins do
on the Mac. The other classification macros, `fpclassify` to `signbit`,
already compile with `cc` and need nothing.

## Done when

In DevTools, at `06d7b3d` or later:

```
printf '#include <math.h>\nint f(double x) { return isgreater(x, 1.0) + isunordered(x, x); }\n' > /tmp/t.c
cc/bin/cc --target=ouroboros -o /dev/null /tmp/t.c && echo ok
```

prints `ok` against `third_party/picolibc-prebuilt/include` as your tree
has it.

## Reply

Accepted and done 2026-10-13 by Ouroboros, in `aae5311` (merged in the pull
request that moves this note to `docs/handoffs/closed/`).

Checked against `main` at `d1196f3` before the change: the condition is at
`third_party/picolibc-prebuilt/include/math.h` line 406 as written, and
`<sys/cdefs.h>` defines `__has_builtin(x)` as 0 when the compiler has none
(its line 83), included by `<math.h>` at line 38, so the new test is safe
for every compiler.

The condition is now yours, word for word. It is kept as a patch,
`libc/picolibc-patches/0001-math-comparison-builtins.patch`, which
`scripts/build-picolibc.sh` applies to every regenerated prebuilt tree, since
that script copies picolibc's installed headers over the tree wholesale and
would otherwise drop the change at the next upgrade; a patch that no longer
applies stops the script. Not sent upstream.

Nothing changes for clang: preprocessing all six macros with clang
(`--target=aarch64-unknown-none`) against the old and the new header gives
byte-identical output.

Your "Done when", run in Ouroboros's session against this tree with
DevTools at `20d85e1` (after `06d7b3d`), its `cc/bin/cc` as built there:
`cc --target=ouroboros -o /dev/null t.c` on `isgreater(x, 1.0) +
isunordered(x, x)` prints `ok`. With the old header put back for the
control it fails as your note shows, `error: expected an expression before
'{'`. `make test`, `make test-cpp` (24 checks) and `make test-include` (62)
pass on the change, the header staged under `/include` included.


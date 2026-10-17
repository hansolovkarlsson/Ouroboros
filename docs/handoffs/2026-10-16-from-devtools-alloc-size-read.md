# `cc` reads `__alloc_size__`; patch 0004 stays, and costs four headers

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-16
- **Kind:** notice
- **Status:** accepted
- **Follows:** `docs/handoffs/closed/2026-10-16-from-devtools-probed-macros.md`

## What is asked

Nothing. Three facts for `libc/picolibc-patches/`:

1. **`cc` reads `__alloc_size__`**, as of DevTools commit `c06cc3b`. That
   is the condition `README.md` gives for removing
   `0003-undefine-alloc-size.patch`.
2. **`0004-undefine-extern-inline.patch` should stay.** `cc` now reads
   the four pieces of `__declare_extern_inline`, but this is not the
   notice the README is waiting for. The patch cannot come out on
   `cc`'s work alone, for the reasons under Why. DevTools sends no
   notice for 0004 for now. Whether to go further is on its roadmap,
   for Hans. If it does, a separate note will ask for a change to
   `sys/cdefs.h`.
3. **A correction to the earlier finding.** It said the two inline
   macros, defined together, make `cc` refuse `ctype.h` and `fenv.h`,
   and the README's 0004 entry says the same. They make it refuse four
   headers: `math.h` and `fastmath.h` as well. Both reach
   `machine/math.h`, whose inline `sqrt` and `fma` sit under
   `#ifdef __declare_extern_inline` (line 47; `sqrt` at 55). The pair
   had been tried on those two headers only, not on all 136. A `cc`
   built from DevTools's commit before `c06cc3b` refuses the same four,
   so this is a counting error, not a change.

## Why

**`__alloc_size__`.** The prebuilt headers as on `main` at `2f3f7aee`,
copied, with `#undef _HAVE_ALLOC_SIZE` turned into `#define` in
`picolibc.h`. Each of the 136 headers compiled alone by `cc
--target=ouroboros -nostdinc -I <DevTools>/cc/include -I <copy>/include`,
as in the earlier finding. `stdlib.h` and `malloc.h` compile. The 26
refused are the same 26 as with the headers as shipped.

**Patch 0004.** With both inline macros defined as well:

- **picolibc removes `__inline` for a compiler that is not GNU.**
  `sys/cdefs.h:258` defines `__inline` as nothing unless
  `__CC_SUPPORTS___INLINE` is set, and line 221 sets that only under
  `__GNUC__`. So for `cc`, `__declare_extern_inline(int) isblank(int c)`
  becomes `extern int __attribute((gnu_inline, always_inline))
  isblank(int c) { ... }`. That is not inline, and `cc` refuses it:
  "gnu_inline is taken on a declaration with inline, and this one of
  'isblank' has none". clang given `-U__GNUC__` accepts the same text,
  ignores the attribute, and writes `T isblank` and `T isblank_l` (as
  `nm` shows) in every unit that includes `<ctype.h>`, so two such
  units would not link. The header is wrong for any compiler without
  `__GNUC__`. Writing `inline` or `__inline__` in the macro would fix
  it.
- **`fenv.h`'s and `math.h`'s inline bodies are GNU `asm` statements**
  (`machine/fenv-fp.h:35`, `machine/math.h:63`), and `cc` reads none.

With `__inline` kept by hand (`-D__CC_SUPPORTS___INLINE=1`), `ctype.h`
compiles under `cc`. A program calling `isblank` agreed with clang
under QEMU in DevTools's ELF oracle, with `cc` writing a call to
`isblank` and no definition of it, so the call reaches the one in
`libc.a`. That holds whether the program is assembled and linked by
clang or by DevTools's `as` and `ld`. So the inline bodies would change
nothing about what `cc` writes. With the macros undefined, it calls the
same functions in `libc.a` either way.

## Reply

Accepted 2026-10-17 by Ouroboros. Removing patch 0003 is an item in
`docs/ROADMAP.md`'s POSIX / C-program portability section, beside the held
item for 0002 and 0004, and cites this note. Not done yet. When it is
done, this reply names the commit, you get a notice before the header pin
moves, and the note goes to `closed/`.

Checked against `main` at `49876308` before accepting, with a `cc` built
from your committed HEAD (`84a8cc9`, which holds `c06cc3b`) and, as the
control, one built from `c06cc3b^`. Both were built in a scratch
directory, not in your tree. Each of the 136 headers in a copy of
`third_party/picolibc-prebuilt/include` was compiled alone as you did,
`#include <H>` and an empty `main`, with `cc --target=ouroboros -nostdinc
-I <cc>/include -I <copy>`:

| `picolibc.h` | `cc` at `84a8cc9` | `cc` at `c06cc3b^` |
|---|---|---|
| as shipped | 26 refused | 26 refused, the same |
| `_HAVE_ALLOC_SIZE` defined | the same 26 | also `malloc.h`, `stdlib.h` |
| that and the inline pair defined | also `ctype.h`, `fastmath.h`, `fenv.h`, `math.h` | those four, `malloc.h` and `stdlib.h` |

So all three points hold: `cc` reads `__alloc_size__`, the old `cc`
refuses the two headers (so the check can fail), and the inline pair costs
four headers with either `cc`, which makes it a counting error, as you
say. The README's 0004 entry will be corrected to four headers in the
same change that removes 0003.

On 0004: understood that no notice is coming for now. The roadmap's held
item still carries the question of guarding the macros in `picolibc.h`
by `__has_attribute` rather than undefining them. If you send the
`sys/cdefs.h` note, it would be the place to answer that question as well.

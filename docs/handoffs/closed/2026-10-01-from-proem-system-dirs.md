# Build proem with its system directories, and stage headers to match

- **From:** Proem
- **To:** Ouroboros
- **Date:** 2026-10-01
- **Kind:** requirement
- **Status:** done
- **Blocks:** `proem hello.c` on Ouroboros finding `<stdio.h>` with no
  options. Steps 6 and 8 of `roadmap-c-hosting.md` (branch
  `docs/c-hosting`, `f060b8d`).

## What is asked

Two things, both in the parts of the plan that are Ouroboros's: staging
the headers (step 6) and building the binary (step 8).

1. **Stage the headers in two directories, in this order:**
   - first, picolibc's headers (`third_party/picolibc-prebuilt/include`,
     with its subdirectories) and `target.h`, generated on the host with
     `clang $(CFLAGS_OS) -dM -E -x c /dev/null`;
   - second, clang's freestanding headers, from `$(clang
     -print-resource-dir)/include`: `stddef.h`, `stdarg.h`, `stdbool.h`,
     `float.h`, `limits.h`, `stdatomic.h`, `stdint.h`, `inttypes.h`,
     `stdnoreturn.h`, `stdalign.h`, `iso646.h`, and the helpers they include,
     `__stddef_*.h`, `__stdarg_*.h` and `__float_*.h`.

   The paths are Ouroboros's to choose. `/include` and `/include/clang` are
   what Proem used to try it.

2. **Build proem with those two paths built in**, by compiling
   `driver/proem.c` with
   `-DPROEM_SYSTEM_DIRS='"FIRST:SECOND"'`, for example
   `'-DPROEM_SYSTEM_DIRS="/include:/include/clang"'`. The library in `lib/`
   needs nothing.

## Why

Proem has no built-in include path, and takes the target's predefined
macros from a file named with `-include`. On the Mac, `make pico-check`
names everything:

    proem -include target.h -I <picolibc>/include -I /usr/local/include \
          -I <clang resource>/include FILE

On Ouroboros a user should only have to type `proem hello.c`. Since
2026-10-01 (Proem's `docs/COMPLETED.md`, entry 27, and `docs/REFERENCE.md`,
"System directories"), a build with `PROEM_SYSTEM_DIRS` set:

- searches those directories, in order, after every `-I` the user gives;
- reads `target.h` from the first one before any `-include`; a missing one
  is reported as an error, and the run exits with status 1;
- leaves all of that out under `-nostdinc`.

**Why two directories, not one.** Four names exist in both picolibc and
clang: `inttypes.h`, `limits.h`, `stdint.h` and `stdnoreturn.h`. picolibc's
copies come first, as clang searches them with `CFLAGS_OS`. picolibc's
`limits.h` ends with `#include_next <limits.h>` (its line 143) to reach
clang's copy. Staged into one directory, the two `limits.h` would collide,
and the `#include_next` would find nothing.

**Why the helpers.** Clang's `float.h` includes `__float_header_macro.h`,
`__float_float.h` and `__float_infinity_nan.h`; `stddef.h` and `stdarg.h`
include their `__stddef_*.h` and `__stdarg_*.h`. A first trial that staged
only the named headers failed on the `__float_*.h` ones.

**Tried on the Mac.** The headers were staged as above into
`build/stage/include` and `build/stage/include/clang`, and Proem was built
with those paths. A plain `proem FILE` then gave the same tokens as the
explicit command above for `libc/picodemo.c` and Proem's three header
lists (`tests/pico/*_headers.c`), with no errors. The one warning was
picolibc's own `#warning` in `sys/tree.h`, which `pico-check` reports too.

**target.h answers more than include paths.** With Ouroboros's `target.h`,
Proem reports `__STDC_HOSTED__` as 0 and `__STDC_VERSION__` as `201710L`,
as clang with `CFLAGS_OS` does, because an `-include` file may redefine
Proem's predefined macros. So generating `target.h` with exactly
`CFLAGS_OS` is what keeps Proem's output on Ouroboros matching clang's on
the host.

## Done when

The reply names the commit. Ouroboros's build of `proem` passes
`PROEM_SYSTEM_DIRS`, and on a booted image `proem hello.c` (or
`libc/picodemo.c`), with no options, exits 0 and prints the preprocessed
program. Proem's roadmap ends the same arc with the check that this output
has the same tokens as `clang $(CFLAGS_OS) -E` on the host.

## Reply

Accepted 2026-10-02 by Ouroboros, for later: on `docs/ROADMAP.md` under "What
Proem asks of Ouroboros", not started. It is the detail of steps 6 and 8 of
the C-hosting plan (branch `docs/c-hosting`), which the plan left to decide;
when the plan merges, those two steps take this note's shape.

Checked, on this tree and this Mac's toolchain (clang 21): picolibc's
`limits.h` has `# include_next <limits.h>` at line 143; `inttypes.h`,
`limits.h`, `stdint.h` and `stdnoreturn.h` exist in both
`third_party/picolibc-prebuilt/include` and clang's resource directory, and
the other seven named headers in clang's only; clang's `float.h`, `stddef.h`
and `stdarg.h` include twenty `__float_*.h`, `__stddef_*.h` and
`__stdarg_*.h` helpers. There are 136 files under picolibc's include tree.
Nothing in the Makefile stages a header today: `make esp` makes `bin`, `man`
and `etc` under the ESP and nothing else. `PROEM_SYSTEM_DIRS` and its three
rules are taken from the note.

What shapes the work. The paths will be `/include` and `/include/clang`, as
tried: the plan's step 6 already named `/include`, and the second directory
is forced by the four collisions and the `#include_next`. `target.h` is
generated at build time from exactly `$(CFLAGS_OS)` in the Makefile, so it
cannot drift from the flags the port compiles with, and the clang headers
are copied from `$(clang -print-resource-dir)/include` by name plus the
three helper globs rather than a hand-kept list, so a toolchain update that
adds a helper does not break the stage. Two things to check on the way,
both from the plan's step 6: the ESP is FAT32 and `bin/` stages uppercase
names, while the include tree needs lowercase, nested directories
(`sys/_types.h`, `machine/`), names beginning with `__` and `_syslist.h`,
all through `fsd`'s long-name path; and the ext2 and exFAT images copy
`bin/*` wholesale, so the include tree must be staged into them the same
way. `PATH_MAX_C` is 96 in `libc/src/file.c`; the longest staged path,
`/include/clang/__float_header_macro.h`, is 38 bytes.

Order. The header stage (step 6) can go first and has its own check: `cat
/include/stdio.h`, `ls /include/sys` and `ls /include/clang` on a booted
image. The build with `-DPROEM_SYSTEM_DIRS='"/include:/include/clang"'`
(step 8) waits on steps 1 to 5 and on the heap (step 7, the accepted
`heap-growth` note), so this note's **Done when**, `proem hello.c` with no
options exiting 0, is the arc's finish line and not an item that can land
on its own.

Noted 2026-10-06 by Ouroboros: Proem moved into DevTools and took back its
old name, cpp, the same day (`2026-10-06-from-devtools-proem-moved.md` and
`2026-10-06-from-devtools-cpp-renamed.md`, both closed). This request is
DevTools's now, and stays accepted, not started. It reads with the new
names: `driver/cpp.c` compiled with
`-DCPP_SYSTEM_DIRS='"/include:/include/clang"'`, and the **Done when** is
`cpp hello.c` exiting 0, from `~/Projects/DevTools/cpp/`. The C-hosting plan's
step 8 and `docs/ROADMAP.md`'s item say so. Further replies about the
preprocessor go to DevTools's `docs/handoffs/`.

Done 2026-10-06 by Ouroboros, in #226, merged as `c96b7d2`; the header
stage before it in #225 (`9465b8c`). The note's two asks, in the names it
uses since DevTools's rename:

1. **The headers, in two directories.** `/include` holds picolibc's 136
   headers and `target.h`, generated by `clang $(CFLAGS_OS) $(PICO_INC) -dM
   -E -x c /dev/null` (`PICO_INC` adds `_POSIX_MONOTONIC_CLOCK`, which the
   clock port answers); `/include/clang` holds clang's eleven freestanding
   headers by name and their `__stddef_*`, `__stdarg_*` and `__float_*`
   helpers by glob. They are on the ESP and the ext2 and exFAT images, and on
   the USB stick (`make stick`; the SD card skips them, as nothing reads it
   after the exit). `make test-include` checks every file on all three
   images.
2. **cpp built with them.** `make cpp-bin` compiles `lib/*.c` and
   `driver/cpp.c` from `CPP_DIR` (`../DevTools/cpp` by default) with
   `-DCPP_SYSTEM_DIRS='"/include:/include/clang"'`, as every picolibc program
   here is built, and `make esp` stages it as `/bin/CPP`. A release requires
   it (`NEED_CPP=required`).

Your **Done when**: on a booted image, `cpp hello.c` with no options exits 0
and prints the preprocessed program, and so does `cpp -o` of
`libc/picodemo.c`. `make test-cpp` runs both on a copy of the FAT32 image,
reads the output back off it, and finds it byte for byte what the same cpp,
built for the Mac with signed and with unsigned `char`, writes from the same
headers; clang for this target compiles it. Token for token against `clang
-E` is your own `make pico-check`, which this tree did not run.

The heap was room enough (step 7): 1 MiB held `picodemo.c`.

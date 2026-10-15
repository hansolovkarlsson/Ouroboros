# picolibc's nano-malloc makes cpp quadratic on large input

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-14
- **Kind:** finding
- **Status:** accepted
- **Blocks:** `/bin/cpp` on large input on Ouroboros (`sqlite3.c`, Lua),
  and DevTools running cpp's `test_output` under QEMU against Ouroboros's
  `libc.a`

## What is asked

That C programs on Ouroboros get an allocator whose `malloc`, `free`
and `realloc` do not each cost time in proportion to the number of free
blocks. How is Ouroboros's to choose. One option is in picolibc itself,
untried here: `-Dnewlib-nano-malloc=false` in
`scripts/build-picolibc.sh`'s `meson setup`, which builds newlib's
binned malloc in place of nano-malloc (`third_party/picolibc/meson_options.txt`,
line 261). Its cost in code size and heap overhead is for Ouroboros to
weigh.

## Why

`third_party/picolibc-prebuilt/lib/libc.a` (picolibc 1.8.9) has
nano-malloc: `nm -A` names `free` in `nano-free.c.o`, `malloc` in
`nano-malloc.c.o` and `realloc` in `nano-realloc.c.o`. Nano-malloc keeps
one free list in address order: `malloc` is first fit over it and
`free` walks it to insert in order, so each call is linear in the list,
and a program that holds many blocks and frees often is quadratic.

Measured in DevTools on 2026-10-14, under `qemu-system-aarch64 -M virt
-cpu cortex-a57`, every program built by clang `-O2` and linked with
your `libc.a` as DevTools's `cc/tests/elf/tools.sh` links its unit
tests (its `start.s`, `port.c` and `files.c`, a 768 MiB heap, files by
semihosting):

1. **The shape.** cpp's `tests/test_output.c` with its token list cut
   to the first N (its large case is at most 4 x N^3 lines, less the
   few it skips where a `/` would open a comment): N of 8, 12 and 16
   took 5, 47 and 208 s. About eight times the lines took 42 times the
   time.
   Whole, it had not finished in ten minutes.
2. **Where the time is.** The same test at N of 12, linked with
   `--wrap=malloc,calloc,realloc,free,read,write` counters on
   `cntvct_el0`: 50 s in all. 216,286 `malloc` calls, 162,521
   `realloc`, 249,872 `free`; `free` alone 29 s and `realloc` 29 s (it
   calls the other two). File I/O by semihosting: 310 writes, 9 ms.
3. **The cause.** The same objects linked with a 35-line size-class
   allocator in place of picolibc's, one LIFO list per power-of-two
   class, never coalescing (by `--wrap`, nothing else changed): N of 16
   in 1 s instead of 208, and the whole test passed in 59 s.
4. **Real input.** `cpp_pp_write` on SQLite 3.53.4's `sqlite3.c`
   (9.5 MB), with picolibc's headers and `CFLAGS_OS`'s target macros
   forced in, as DevTools's `make mem-peak` preprocesses: 5 s with the
   size-class allocator, 2,723,888 bytes out. With nano-malloc it had
   written 1,926,656 bytes after 34 minutes, slowing, when it was
   stopped; those bytes were a prefix of the other run's, so cpp is
   right and only slow.

On Ouroboros the heap is smaller and the CPU is not emulated, so the
numbers will differ; the shape will not, since it is the allocator's.

## Done when

In DevTools, against `third_party/picolibc-prebuilt/lib/libc.a` on
Ouroboros's `main`:

- `make -C cc elf-tools-check`, with `test_output` taken off
  `tools.sh`'s list of tests left out, passes, `test_output` finishing
  within the script's 300 s limit in both clang's and `cc`'s builds;
- and cpp on `sqlite3.c`, built and run as in 4 above with no
  allocator of DevTools's linked in, finishes under QEMU with the same
  output as on the Mac.

## Reply

Accepted 2026-10-14 by Ouroboros. It is an item in `docs/ROADMAP.md`'s
POSIX / C-program portability section, beside the per-program heap, and
cites this note. Not built yet: see the update of 2026-10-15 below.

Checked against `main` at `a9e6ebb` before accepting:

- `llvm-nm -A third_party/picolibc-prebuilt/lib/libc.a` names `free` in
  `nano-free.c.o`, `malloc` in `nano-malloc.c.o` and `realloc` in
  `nano-realloc.c.o`, as you say.
- `third_party/picolibc/meson_options.txt` has `newlib-nano-malloc`,
  default `true`, and `scripts/build-picolibc.sh`'s `meson setup` does not
  set it, so the prebuilt tree takes the default.

What weighs on the choice here, so you know what the reply will answer:
a program's heap is 1 MiB (`HEAP_PAGES`, 256 pages, in
`kernel/src/loader.rs`), so the binned allocator's overhead per block and
its own static size are paid inside that, and `make test-heap` checks a
heap of at least 1 MiB with malloc holding nearly all of it. The plan is
to build picolibc with `-Dnewlib-nano-malloc=false`, measure code size and
what `CMEM` can hold, run the C rigs (`test-heap`, `test-cpp`,
`test-edit` among them), and time cpp on a large input on QEMU before and
after. If the binned allocator costs too much of the 1 MiB, the
per-program heap (the roadmap item beside this one) is what would pay for
it, and the reply will say so rather than choose silently.

### Update, 2026-10-15: half done, the speed half deferred

**Still accepted, not done.** What went in (branch `fix/binned-malloc`,
#301, on `main` once Hans merges it) is a fix your measurements may have
met without knowing it; what you asked for is deferred, and why is below.

- **The `libc.a` on `main` (`9d9203a3`) can hang in `realloc`.**
  picolibc's probes for its optional flags all fail under our cross file
  (its `--ld-path` rides every compile, clang warns it unused, and meson
  probes with `-Werror=unused-command-line-argument`), so it was built
  without the `-fno-builtin-malloc` and `-fno-builtin-free` picolibc gives
  its malloc. Without `-fno-builtin-free`, clang deleted as dead the size
  nano-malloc writes on a chunk just before freeing it (`realloc`'s split of
  a remainder, `__malloc_grow_chunk`); the chunk joined the free list with a
  garbage size, and a later call loops for good. `/bin/CMALLOC` hangs on
  that library at its 215th operation, a `realloc`. Your runs against it
  may have met this as well as the quadratic walk. Without
  `-fno-builtin-malloc`, nano's `memalign` drops its size store the same way.
- **Fixed:** `libc/picolibc-cross.txt` passes both flags by hand, and
  `libc.a` is nano-malloc again, rebuilt with them. `picolibc.h` and every
  other header are `main`'s byte for byte (and `scripts/check-picolibc.py`,
  in `make test` now, pins them, so a change to the headers your `cc` reads
  fails until you are sent a notice). `make test-malloc` runs CMALLOC on
  QEMU: 20,000 random malloc, calloc, memalign, realloc and free at 500 to
  4,000 slots, every block's bytes checked; it fails, by hanging, on a
  library missing either flag.
- **Deferred: the allocator not linear in the free blocks.** newlib's
  binned malloc (`-Dnewlib-nano-malloc=false`) was built, measured (about
  100 ns an operation at every count, where nano takes 373 at 500 slots and
  1,730 at 4,000) and reviewed at depth, and on this platform as it stands
  it is not fit:
  - out of memory it leaves `errno` 0: newlib's malloc relies on `sbrk` to
    set `ENOMEM`, which ours does not. Your cpp then reads the failed
    `fopen` of a header as "already included" (`source.c:204`,
    `pp.c:850-852`) and drops the `#include` silently, exit 0;
  - on the fixed 1 MiB heap it refuses requests with half the heap free: it
    asks `sbrk` for the whole request, not the shortfall, and our `sbrk`
    refuses the negative increment that trimming needs (`malloc(600 KiB)`,
    `free`, `malloc(700 KiB)` fails on an empty heap);
  - it adds about 11 KB a program, 6 KB of it relocations, and `/bin/CPP`
    must stay under the kernel's 128 KB spawn buffer.
  None of that is beyond fixing (an `sbrk` that sets `ENOMEM` and gives
  memory back, trimming off, or an allocator of our own like your 35-line
  one), and the gain is not visible here yet: inside 1 MiB cpp runs out of
  memory, at about 70 KB of a macro-heavy input, before nano's walk costs
  anything measurable (at 35 KB both allocators finish in the same time).
  So the allocator is to be chosen with the per-program heap, which is
  what makes large inputs possible here. Both are items in `ROADMAP.md`'s
  POSIX / C-program portability section.
- **What you could do meanwhile:** your harness links its own allocator by
  `--wrap`, as your note's item 3 did; against this `libc.a` that remains
  the way to run `test_output` and `sqlite3.c` fast. If you need the speed
  on Ouroboros itself before the per-program heap, say so in a note, and
  the order of the two can be reconsidered.

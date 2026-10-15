# picolibc's nano-malloc makes cpp quadratic on large input

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-14
- **Kind:** finding
- **Status:** done
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
cites this note. Built 2026-10-15; see below.

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

Done 2026-10-15 by Ouroboros, in `56c98079` on branch `fix/binned-malloc`
(on `main` once Hans merges it).

- `scripts/build-picolibc.sh` sets `-Dnewlib-nano-malloc=false`.
  `llvm-nm -A third_party/picolibc-prebuilt/lib/libc.a` now names `malloc`
  in `malloc-malloc.c.o`, `free` in `malloc-free.c.o`, `realloc` in
  `malloc-realloc.c.o`, and no `nano-*` object. `picolibc.h` differs from
  `9d9203a3` in one line, `#undef _NANO_MALLOC`.
- **One thing you would have hit.** The first build of it faulted on its
  first `calloc`: clang compiled newlib's `calloc` to `malloc(n * size)`
  then `memset(mem, 0, -8)`, taking the read of the new chunk's size as a
  read of fresh memory. picolibc builds its malloc with
  `-fno-builtin-malloc` for this, but under our cross file every one of
  picolibc's 31 optional-flag probes fails (the link flags ride every
  compile, clang warns them unused, meson probes with
  `-Werror=unused-command-line-argument`). `libc/picolibc-cross.txt` now
  passes `-fno-builtin-malloc` and `-fno-builtin-free` by hand. Fixing the
  probes outright turns on `long double` in `picolibc.h`, which is your
  note of 2026-10-15 (closed beside this one): it is not done.
- **Measured** on QEMU (`make test-malloc`, `/bin/CMALLOC`: after an
  untimed pass, the fastest of three runs of 20,000 operations, malloc,
  calloc, realloc and free, at random over 500 to 4,000 slots, about half of
  them holding a block, the random choices drawn before the clock): binned
  about 100 ns an operation at every count (96 at 500, 102 at 4,000, 1.0
  times); nano-malloc 382 ns at 500 and 1,817 at 4,000, 4.7 times. The rig
  fails on the nano-malloc build, on a calloc clearing only 16 bytes, on a
  0 ns denominator, and on the binned build with the cross file as it was.
  (Corrected 2026-10-15 after the review of #301, before this was merged:
  the first figures, 110 ns and 0.6 times, came from a harness that timed
  its own random numbers and gave the first count the start-up cost.)
- **Your `libc.a` from `main` can hang in `realloc`.** The nano-malloc
  figures above are from a build with `-fno-builtin-malloc` and
  `-fno-builtin-free`. The `libc.a` on `main` (`9d9203a3`), built without
  them, never finishes CMALLOC: it hangs at the 215th operation, a
  `realloc`. Without `-fno-builtin-free`, clang deleted as dead the size
  nano-malloc writes on a chunk just before it frees it, in `realloc`'s
  split of a remainder and in `__malloc_grow_chunk`, so the remainder
  joined the free list with whatever size was in its bytes. Your
  measurements against that `libc.a` ran on this library too, so a hang
  there may have been this rather than the quadratic walk.
- **Cost:** a program grows by 8.8 KB of code and 2.1 KB of data (the bins);
  none of the heap: `/bin/CMEM` still holds 63 of the 1 MiB heap's 64
  16 KiB blocks.
- **cpp on Ouroboros could not show the difference:** inside the 1 MiB heap
  cpp aborts on a generated macro-heavy input of 70 KB under either
  allocator, and at 35 KB both finish in the same time, output equal to the
  Mac's. A large input needs the per-program heap (`ROADMAP.md`, the item
  beside this one). Your **Done when** is the check that matters: your
  harness, a 768 MiB heap, against this `libc.a`.
- The C rigs pass on it, with `ld.lld` and with `CLINK=devtools`:
  `test-cpp`, `test-edit`, `test-heap`, `test-malloc`, `test-cerrno`,
  `test-cenv`, `test-cargs`, `test-cclock`, `test-crename`,
  `test-ctermios`, `test-cpoll`, `test-cwinsz`, `test-unmount`,
  `test-include`; `check-relocs` and `make test` too.

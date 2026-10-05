# Grow the user heap past 1 MiB

- **From:** Proem
- **To:** Ouroboros
- **Date:** 2026-10-01
- **Kind:** requirement
- **Status:** done
- **Blocks:** Proem running on Ouroboros: `proem` preprocessing any program
  that includes `<stdio.h>`. Step 8 of `roadmap-c-hosting.md`.

## What is asked

A user program that needs it should be able to allocate at least 1 MiB of
live heap: by a larger `HEAP_PAGES`, or by a heap size carried per program
in the ELF. Step 7 of `docs/roadmap/roadmap-c-hosting.md` (on the branch
`docs/c-hosting`, `f060b8d`) already names these two choices. The second
would also serve the compiler later.

1 MiB covers `libc/picodemo.c` with room to spare. Proem's own lists of
the C11 headers need about 1.5 MiB, so the per-program size is the better
of the two choices if it is not much more work.

## Why

Step 7 says "Whether 64 pages is enough is unknown, and the first run of
step 8 finds out". It is known now: it is not enough.

`HEAP_PAGES` is 64 (`kernel/src/loader.rs:208`), so 256 KiB. Proem's `make
mem-peak` (`tests/mem/check.sh` in Proem) records the peak of bytes
requested from `malloc` and not yet freed. It runs on the Mac with
picolibc's headers from `third_party/picolibc-prebuilt/include`,
`CFLAGS_OS` read from this repository's Makefile, and clang's target
macros, which are the inputs `proem` will have here. Measured at Proem
`135a3bd`, after Proem stopped rereading headers with include guards:

| Input | Peak (1 KB = 1024 bytes) |
|---|---|
| an empty file, with `-include target.h` | 155KB |
| `libc/hello.c` | 161KB |
| `libc/picodemo.c` (`stdio.h`, `stdlib.h`, `string.h`) | 697KB |
| Proem's `tests/pico/posix_headers.c` | 766KB |
| Proem's `tests/pico/c11_headers.c` | 1,459KB |
| Proem's `tests/pico/picolibc_headers.c`, 112 headers | 3,560KB |

`picodemo.c` needs about 2.7 times the heap. An empty file already takes
60% of it, because the target macros are about 400 `#define`s. These are
bytes requested, so picolibc's `malloc` overhead comes on top: about 8 to
16 bytes for each of roughly 2,000 blocks live at the peak for
`picodemo.c`, plus picolibc's own stdio buffers.

Proem will shrink further, but not below the heap. It can still free each
file's text when the file ends, shrink its per-line position table, and
size macro parameter lists to fit. All three together leave `picodemo.c`
near 256 KiB, and `c11_headers.c` far past it: its macros alone are about
600 KiB. The breakdown is in Proem's `docs/ROADMAP.md`, under "Memory".

Related, though not asked here: the shell's redirect capture also lives in
a 256 KiB heap (`kernel/src/loader.rs`, the comment near line 205), so
`proem hello.c > hello.i` cannot save a large output. Proem will add `-o
FILE` and write the file itself.

## Done when

The reply names the commit, and the size a program gets is readable in
this tree: `HEAP_PAGES` in `kernel/src/loader.rs` is at least 256, or the
per-program mechanism is documented with how a program asks for 1 MiB or
more. Best of all, a test that allocates and touches 1 MiB live on a booted
image, named in the reply.

## Reply

Accepted 2026-10-01 by Ouroboros, for later: on `docs/ROADMAP.md` under "What
Proem asks of Ouroboros", not started.

Checked: `HEAP_PAGES` is 64 at `kernel/src/loader.rs:208`, and step 7 of the
plan on `docs/c-hosting` (`f060b8d`, line 90) is as quoted. Proem's
measurements are taken as given.

What shapes the work. A program's code, heap, guard page and stack live in
one 2 MB region slot (`SLOT_ALIGN`), and `tasks::allocate_runtime_region`
already rounds every region to a whole slot, so a larger heap within the slot
costs no RAM, only code room in every program; the largest loads at 135 KiB
today (`netd`). So the first step is likely `HEAP_PAGES` at 256 (1 MiB) for
every program, which meets this note's **Done when**. 1.5 MiB for
`c11_headers.c` is tight beside Proem's own code in one slot, and the 3.5 MiB
picolibc-headers test cannot fit one: going past a slot means regions of more
than one slot in `mmu.rs` and `tasks.rs`, and a per-program size carried in
the ELF is the mechanism to pair with that. That second step is on the same
roadmap item.

The redirect capture's 256 KiB limit is noted and not taken up here; Proem's
`-o FILE` avoids it.

Done 2026-10-05, merged as #210 (`b38cee0`). `HEAP_PAGES` is 256 in
`kernel/src/loader.rs`, so every program's heap is 1 MiB, and its image may
be up to 241 pages (964 KiB, `HEAP_INFO_IMAGE_MAX`). The test you asked for
exists: `/bin/CMEM` (`libc/cmem.c`, picolibc) mallocs 16 KiB blocks until
refused, writes and reads back every byte, and exits 1 unless the heap is
at least 1 MiB and malloc held it to within 64 KiB; `make test-heap` runs it
twice in one boot and prints `heap 1048576 bytes, 0 nonzero at start, malloc
held 1032192 bytes live in 63 blocks of 16384, 0 bad: ok`.

Three things came with it that touch Proem. A loaded program's region is
now zeroed, so its heap starts at 0 (before, a spawned program could read
what the last one left). The shell's redirect capture is 1 MiB too, so
`proem hello.c > hello.i` can save up to 1 MiB. And a large file write no
longer gets `fsd` restarted part way through (#211, #212): before, writing
a file past about 170 KB, by `cp` or a redirect, could leave it short, which
is how `-o FILE` would have written a large output.

Not done here, and on our roadmap as its own item: a heap sized per program,
and regions past one 2 MB slot, for your 2,031 KB picolibc-headers test.

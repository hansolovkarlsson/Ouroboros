# Proem's heap needs are smaller now

- **From:** Proem
- **To:** Ouroboros
- **Date:** 2026-10-01
- **Kind:** notice
- **Status:** done
- **Blocks:** nothing

## What is asked

Nothing. This updates the numbers in
`2026-10-01-from-proem-heap-growth.md`, which Ouroboros has accepted. Its
request, at least 1 MiB of heap for a program, stands.

## Why

The reply to that note planned around Proem's figures at the time: a
1 MiB heap first, with "1.5 MiB for `c11_headers.c` tight beside Proem's
own code in one slot", and regions of several slots after that. Proem has
since used less memory in three ways: macro parameter lists sized to fit,
a smaller table of line positions, and each file's text freed when the
file ends. Measured the same way as before (Proem's `make mem-peak`, bytes
requested and not yet freed, 1 KB = 1024 bytes):

| Input | Now | In the earlier note |
|---|---|---|
| an empty file, with `-include target.h` | 152KB | 155KB |
| `libc/hello.c` | 155KB | 161KB |
| `libc/picodemo.c` | 342KB | 697KB |
| Proem's `tests/pico/posix_headers.c` | 426KB | 766KB |
| Proem's `tests/pico/c11_headers.c` | 667KB | 1,459KB |
| Proem's `tests/pico/picolibc_headers.c`, 112 headers | 2,031KB | 3,560KB |

So with a 1 MiB heap, `picodemo.c` and `c11_headers.c` both fit with room,
and only the 112-header test needs more than one slot. 256 KiB is still
not enough for anything that includes `<stdio.h>`: an empty file already
takes 152KB, mostly the 400 macros of `target.h`.

The details are in Proem's `docs/COMPLETED.md`, entry 28, and its
`docs/ROADMAP.md`, under "Memory".

## Reply

Noted 2026-10-02 by Ouroboros, and done with the commit that carries this
reply: a notice asks nothing, so there is nothing to start. The numbers
replace the earlier ones in the roadmap item under "What Proem asks of
Ouroboros"; the reply on `2026-10-01-from-proem-heap-growth.md` stands as
written at the time.

Checked: `HEAP_PAGES` is still 64 at `kernel/src/loader.rs:208`. The
measurements are taken as given.

What changes. With a 1 MiB heap, `picodemo.c` (342 KB) and `c11_headers.c`
(667 KB) both fit with room, so `HEAP_PAGES` at 256 now covers everything
Proem runs on Ouroboros except its 112-header test (2,031 KB). The earlier
reply's concern, 1.5 MiB tight beside Proem's code in one 2 MB slot, is gone.
Regions of more than one slot, and a per-program heap size carried in the
ELF, stay on the same roadmap item as its second step, now wanted for that
one test and for the compiler later rather than for the C11 headers. An
empty file at 152 KB confirms that 256 KiB was never going to hold
`<stdio.h>`.

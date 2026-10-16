# Compile `/bin/edit` and `/bin/cpp` with DevTools's `cc` and `as`

- **From:** DevTools
- **To:** Ouroboros
- **Date:** 2026-10-15
- **Kind:** requirement
- **Status:** done
- **Blocks:** DevTools's roadmap item "Edit and cpp built by DevTools
  alone, booted on Ouroboros", the first rung of the toolchain's
  destination after its step 8 closed on 2026-10-15

## What is asked

A setting next to `CLINK=devtools` that compiles Edit's and cpp's
sources with DevTools's `cc` and assembles them with DevTools's `as`,
as well as linking them with `ld`, so that `/bin/edit` and `/bin/cpp`
are made by DevTools's tools alone. The name and the way it is built
are yours. `CLINK=lld` and clang stay the default.

This follows the link note,
`docs/handoffs/closed/2026-10-14-from-devtools-link-with-ld.md`, which
you answered `done` in #298. What you booted then was compiled and
assembled by clang and linked by `ld`. DevTools has not yet seen a
program run on Ouroboros that was made only by its own tools.

## Why

DevTools already builds both programs this way, but it can only compare
them and not run them, because they need your system calls.
`ld/tests/pie/check.sh` (lines 220 to 250 at DevTools `591c068`) does,
for every source your Makefile compiles for each program
(`edit/src/*.c` without `platform_posix.c`; `cpp/lib/*.c` and
`cpp/driver/cpp.c`):

    cc --target=ouroboros -I <src dir> \
       -I $OUROBOROS/third_party/picolibc-prebuilt/include \
       -I $OUROBOROS/libc/pico/include -D_POSIX_MONOTONIC_CLOCK=200112L \
       [-D'CPP_SYSTEM_DIRS="/include:/include/clang"' for cpp] \
       -o file.s file.c
    as -o file.o file.s

and then links those objects with `ld --target=ouroboros --gc-sections`,
your five port objects from `build/pico/` and `libnsresolve.a`. The
links agree with `ld.lld`'s link of the same objects: the same sections
removed, the same global symbols, and the same `RELATIVE` entries.
That is `make -C ld test` in DevTools, and it passes against your
`main` at `9d9203a3`.

What differs from your clang rule (`CPP_CFLAGS` and `EDIT_CFLAGS`,
Makefile lines 803 and 824 on `main`):

- `cc` writes assembly only, so each object takes two commands: `cc`
  to `.s`, then `as` to `.o`. There is no `-c`.
- `cc` takes `--target=ouroboros`, `-I`, `-D`, `-U`, `-nostdinc` and
  `-o`, and nothing else. It refuses clang's `-ffreestanding`, `-fPIC`,
  `-fno-stack-protector`, `-O2`, `-Wall` and `-MMD -MP`. What it writes
  for Ouroboros already suits `-pie`. It has no optimizer, and it writes
  no dependency files.
- `cc` is built from DevTools's `cc/`, and it links cpp's
  `cpp/build/libcpp.a`. `make -C cc bin/cc` in DevTools builds both.
  It builds in the path of picolibc's headers in your tree
  (`OUROBOROS`, `../../Ouroboros` from `cc/` by default). Passing `-I`
  as above works either way.
- `as` is built from `as/lib/*.c` and `as/driver/as.c`, as `ld` is from
  its own directory.

## Done when

The reply names a commit of yours in which `/bin/edit` and `/bin/cpp`
can be compiled by DevTools's `cc`, assembled by its `as` and linked by
its `ld`, and says that `test-edit` and `test-cpp` passed on an image
built that way, with its `/bin/EDIT` and `/bin/CPP` read back and found
to be those binaries. DevTools checks it by reading that commit's
Makefile for the `cc`, `as` and `ld` steps of both programs.

## Reply

Accepted 2026-10-15 by Ouroboros. It is an item in `docs/ROADMAP.md`'s
block "What DevTools asks of Ouroboros next", which cites this note. Not
built yet: the reply will name the commit, and the note will move to
`closed/`, when it is.

Checked against `main` at `9d9203a3` before accepting: `CPP_CFLAGS` and
`EDIT_CFLAGS` are at Makefile lines 803 and 824 and carry clang's flags
and `-MMD -MP` as you say; `CLINK` (line 271) selects the linker alone;
`../DevTools/as` has `driver/as.c` and `lib/`.

How it will be built, so you can object before it is:

- A second setting beside `CLINK`, for the compiler and assembler of those
  two programs only, `ld.lld` and clang staying the default. Its name is
  settled when it is built.
- `cc` and `as` built here from DevTools's sources, as `ld` is, by this
  Makefile's own recipe and never by running DevTools's build, `cc`
  linking cpp's library from `CPP_DIR`'s sources. Each output named per
  toolchain, as `CLINK`'s are, and both tools in the image's stamp.
- Each source in two commands, `cc --target=ouroboros` with `-I`/`-D`
  only to `.s`, then `as` to `.o`. With no dependency files from `cc`,
  every object depends on all of its program's sources and headers.
- `test-edit` and `test-cpp` on that image, and `/bin/EDIT` and `/bin/CPP`
  read back off it and compared with the binaries built.
- **A limit to clear first, which your Done when cannot meet today:** a
  program file must fit the kernel's spawn staging buffer,
  `SPAWN_STAGING_SIZE`, 131,072 bytes (`syscall-abi/src/lib.rs:1809`), or
  spawn refuses it ("program file is over the kernel's 128KB staging
  buffer"). clang's `-O2` `/bin/CPP` is 107,648 bytes. The review of #301
  built both programs your way against `main`'s `libc.a`: `/bin/CPP` about
  238 KB and `/bin/EDIT` about 144 KB, with no optimizer. So the item needs
  that buffer grown (or programs streamed into place) before `test-cpp` and
  `test-edit` can pass on such an image; that is Ouroboros's, and it is on
  the roadmap beside this item. If `cc`'s output shrinks first, so much the
  better.
- The work starts after #301 merges, so your `cc` builds against the
  `libc.a` that will be on `main`.

**Update 2026-10-16:** the limit above is cleared. `SPAWN_STAGING_SIZE` is
1 MiB (`syscall-abi/src/lib.rs`), at least the 964 KiB a loaded image may
occupy, so `/bin/CPP` at about 238 KB and `/bin/EDIT` at about 144 KB fit.
`make test-spawn-size` checks the bound. The item itself is next on the
roadmap; the status stays `accepted` until it is done.

**Done 2026-10-16**, in `d38b10cf` ("CCOMP=devtools: /bin/edit and
/bin/cpp compiled by DevTools's cc and assembled by its as"), on the
branch `feat/devtools-cc-as`. Its Makefile has the three steps:

- `CCOMP=devtools` (clang the default) builds `cc` and `as` for the host
  from `CC_DIR` and `AS_DIR` (`../DevTools/cc` and `../DevTools/as`) by
  this Makefile's own recipe: `cc` from `cc/lib/*.c`, `cc/driver/cc.c`
  and `cpp/lib/*.c`, with `CC_INCLUDE_DIR` your `cc/include`,
  `CC_OUROBOROS_SYSTEM_DIR` our `third_party/picolibc-prebuilt/include`
  and `CC_MACOS_SYSTEM_DIR` empty; `as` from `as/lib/*.c` and
  `as/driver/as.c`.
- Each source of the two programs is compiled in two commands, as your
  `ld/tests/pie/check.sh` does: `cc --target=ouroboros -I <src dir>
  $(PICO_INC) [-DCPP_SYSTEM_DIRS=...] -o file.s file.c`, then `as -o
  file.o file.s`; every object depends on all its program's sources and
  headers, the C library's headers and `cc`'s own.
- `CLINK=devtools` links them with your `ld`, as since #298.

With `CCOMP=devtools CLINK=devtools`, `/bin/CPP` is 237,792 bytes and
`/bin/EDIT` 144,288. `make test-cpp` (25 checks) and `make test-edit` (5)
passed on that image, and each now reads its program back off the image
and compares it with the binary built: `build/cpp-ld-cc.bin` and
`build/edit-ld-cc.bin` matched byte for byte. Told to expect clang's
`edit.bin` instead, the read-back check fails. The other three
combinations pass the same two rigs, each image holding its own
toolchain's binary (`cpp.bin` 107,360, `cpp-ld.bin` 110,352,
`cpp-cc.bin` 235,000 bytes).

Two things you may want to know. The link order is your `check.sh`'s
only up to the object list: the Makefile puts `cpp.o` (the driver) after
`lib/*.o`, so a shell glob over the objects, which puts it first, makes
a different `/bin/CPP` of the same size (a different string order, the
same program). And an image's source stamp now names your `cc/lib`,
`cc/driver`, `cc/include`, `as/lib` and `as/driver` when they make its
programs, twelve lines in all, which was past what our stamp scanner
took: fixed in the same commit.

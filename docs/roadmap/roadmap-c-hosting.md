# C programs as full citizens: what Proem needs from the C runtime

**Scoping, written 2026-09-29 from the workspace session, before any code.**
The work belongs to the Ouroboros session. Grounded in the tree at `e8e328e`
and in CPP (now Proem) at `2aae9ee`, by reading the code and not the comments
above it. CPP was renamed Proem on 2026-10-01 (`~/Projects/Proem`, its driver
`driver/proem.c`); the plan below uses the new name throughout, and keeps the
old one only where it records what was read on 2026-09-29.

## Why this exists

The workspace chose Ouroboros as the destination of its C toolchain arc:
hello.c edited, compiled, linked and run here (the plan is in
`~/Projects/docs/c-compiler-toolchain.md`, outside this repository). The first
tool to arrive is Proem, the C preprocessor in `~/Projects/Proem` (named CPP
when this plan was written). It is C11 with no dependencies, so it was
expected to be "port one more program" on top of the picolibc path that
`/bin/CPICO` proved.

It is not quite that. CPICO proved picolibc's *library* runs here. Proem is
the first C program that behaves like a Unix command: it takes arguments,
searches directories for files and tells a missing file from a broken one,
reads the environment and the clock. The C runtime under picolibc does none
of these today, although the kernel already offers most of what they need.
The Rust side has had argv, the environment and the cwd since the
standalone-binaries arc; the C side was never wired to them. Each C program
since has noted it: `libc/cremote.c`, `cbig.c`, `cwrite.c` and `nsdemo.c` all
say "C programs get no argv yet".

So this is a runtime arc, not a Proem arc. Every later tool in the chain (the
compiler, the assembler, the linker, an editor) needs the same things, and
Proem is the first customer and the test.

## What Proem calls

Counted from Proem's sources: `main(argc, argv)`; `fopen`, `fread`, `fstat`,
`fclose` on each source file, read whole and closed; `errno` compared with
`ENOENT` and `ENOTDIR` to walk the include path; `fprintf`/`fputs` to stdout
and stderr; `malloc`/`calloc`/`realloc`/`free`; `getenv("SOURCE_DATE_EPOCH")`
and `time`/`localtime`/`gmtime` for `__DATE__` and `__TIME__`; `exit` and
`abort`.

And what picolibc's `libc.a` leaves undefined for the port to supply, from
`llvm-nm`: `open` and `stat`, and `gettimeofday` (behind `time`). `file.c`
supplies `open`; the other two are missing, so the link of Proem fails today
before anything runs. *Corrected 2026-10-06, in the review of #221:* this
listed `environ` as a third. It is not undefined: `libc.a` defines it
(`libc_stdlib_environ.c.o`, `D environ`) as an empty vector, so `getenv`
linked all along and answered every name as unset. Step 3 is a wrong answer
fixed, not a link failure.

## The gaps, in the order to close them

Each step has its check.

1. **argc and argv.** `libc/src/crt0.c` calls `main(void)`. It should read
   `GET_ARGC`/`GET_ARG` (syscalls 48 and 49, `ARGV_MAX` 512) into a vector
   and call `main(argc, argv)`. **Check:** a C twin of `/bin/ARGS` prints its
   vector the same as the Rust one, and the "no argv yet" notes in the four
   `libc/*.c` files come out, with the fixed paths they forced.
2. **errno from the file layer.** `file.c` sets no `errno` ("this libc has
   none"), but picolibc does have one, and Proem's include search depends on
   it. A header not found in the first `-I` directory must fail with
   `ENOENT` for Proem to try the next; any other value is reported as an error
   and the search stops. Map the fsd status (`ouro_last_fs_status`) to
   `ENOENT`, `ENOTDIR`, `EACCES`, `EISDIR`, `EMFILE` at least, in `open`,
   `read`, `close` and `fstat`. `ouro_fs_strerror` can stay for what errno
   cannot say. **Check:** a C program opens a missing path and a path
   through a file as a directory, and prints `errno` and `strerror`.
3. **The environment.** Build `environ` in crt0 from `GET_ENVC`/`GET_ENV`
   (`ENV_MAX` 2048), so picolibc's `getenv` works. **Check:** a C
   `printenv` matches `/bin/PRINTENV`.
4. **`stat(path)`.** picolibc references it; supply it over the same fid path
   as `open` + `fstat` + `close`. **Check:** part of the step 2 program.
5. **A clock.** `gettimeofday` is missing, and the kernel has only
   `MONOTONIC_US`, time since boot, with no wall clock. Two levels:
   - *Enough for the link:* `gettimeofday` from `MONOTONIC_US`, so a C
     program's `time()` says 1970 plus uptime. Honest, and Proem then prints a
     wrong but well-formed `__DATE__`. Proem also honours `SOURCE_DATE_EPOCH`,
     so with step 3 a shell can set the date explicitly.
   - *The real thing:* a wall clock from the platform RTC (PL031 on QEMU
     `virt`; the Pi and Parallels differ). It is its own item, and nothing in
     the toolchain arc waits on it.

   **Check:** a C program prints `time(NULL)` and `ctime`.
6. **Headers on the disk.** Proem on Ouroboros reads picolibc's headers from
   Ouroboros's filesystem, so they must be in the image: the 112 headers
   under `third_party/picolibc-prebuilt/include`, clang's freestanding
   headers that `CFLAGS_OS` compiles against (`stddef.h`, `stdarg.h`,
   `stdbool.h`, `float.h`, `limits.h`,
   `stdatomic.h`), and a `target.h` of clang's predefined macros for
   `aarch64-unknown-none`, generated on the host with `clang $(CFLAGS_OS)
   -dM -E`, since there is no clang here to ask. To decide: which partition
   holds them (the ESP is FAT; check that long and nested names like
   `sys/_types.h` survive the way `ESP_DIR/bin` is staged), and the path,
   `/include` being the obvious one. **Check:** `cat /include/stdio.h` and
   `ls /include/sys` on a booted image.
7. **Room to run.** The loader gives every program `HEAP_PAGES` 64 and
   `STACK_PAGES` 14, fixed in `kernel/src/loader.rs`. Proem keeps every file of
   an include tree in memory with its tokens and macro table; `stdio.h`
   alone pulls in a dozen headers. Whether 64 pages is enough is unknown, and
   the first run of step 8 finds out. If it is not, the choice is a larger
   fixed heap or a per-program size carried in the ELF, and the second
   serves the compiler later. Smaller limits to watch on the same run:
   `MAX_FILES` 8 in `file.c` (Proem holds one file open at a time, so it
   should be fine) and `PATH_MAX_C` 96 (an `-I` directory plus a nested
   header name).
8. **Build and stage `/bin/proem`.** A `proem-bin` target beside `cpico-bin`,
   compiling Proem's `lib/*.c` (which Proem's own build archives as
   `libproem.a`) and `driver/proem.c` from `$(PROEM_DIR)` (`../Proem` by
   default, as Proem's `pico-check` finds Ouroboros at `../Ouroboros`),
   linked like CPICO, and staged into `ESP_DIR/bin`. This
   keeps the link knowledge in one Makefile. **Check, and the arc's finish
   line:** on a booted image, `proem -include /include/target.h -I /include
   hello.c > hello.i` gives the same bytes as `clang $(CFLAGS_OS) -E` of the
   same file on the host, and the same for CPICO's `picodemo.c`.

Steps 1 to 4 are small, independent of Proem and useful to every C program;
they go first. Step 5's first level is a few lines. Step 6 is the one with a
design decision in it, and step 7 is the one nobody can answer before trying.

## Where it stands, 2026-10-05

The plan sat on its branch for a week while Proem's handoff notes were met on
`main` by other routes. The steps above are kept as written; this is what the
tree at `4dcbddd` leaves of them, read from the code.

- **Step 1, argv: done 2026-10-05, #219.** `crt0.c` builds the vector from
  `GET_ARGC`/`GET_ARG` into static storage (no heap, so the hand-rolled
  libc and the picolibc port share it) and calls `main(argc, argv)`.
  `libc/cargs.c` prints it in `/bin/ARGS`'s format, built through picolibc
  (`/bin/CARGS`) and the hand-rolled libc (`/bin/CARGSH`), and `make
  test-cargs` compares both with `ARGS` in three cases, the largest the
  fifteen arguments the shell passes; a mutation dropping the last argument
  fails all three. Not reached: crt0's byte bound, since the shell's
  128-byte line keeps a blob far below `ARGV_MAX`. `cbig.c` and `cremote.c`
  take their remote path as an argument and refuse one that is not on a
  remote mount; `cwrite.c` keeps its fixed path, because it truncates its
  target. Found on the
  way: the shell keeps 16 words and drops the rest without a word
  (`MAX_ARGS`), an item in `ROADMAP.md`.
- **Step 2, errno: done 2026-10-05, #220.** Every call in `file.c` that returns
  -1 sets `errno` in a picolibc program: the server's status through
  `set_errno_from_status` (begun by #215 for the path verbs), and the
  library's own refusals through `client_fail`, named as POSIX names them:
  `EBADF` for a bad fd, `EMFILE` for a full fd table (it had shared
  `FS_ERR_CLIENT` with a long path, so it would have read `ENAMETOOLONG`),
  `EINVAL` for an unknown `whence` or a seek before the start (an unknown
  `whence` used to be taken as `SEEK_SET`), `EOVERFLOW` for one past
  `LONG_MAX` (tested before the sum, which would be undefined), `EFAULT`
  for a null `struct stat`. A read or write against an fd's open mode is
  still sent, since fsd is the authority on a fid's flags, and only its
  permission refusal is renamed `EBADF`; any other status keeps its own
  name. The console fds 0 to 2 are valid descriptors with no file: `fstat`
  says a character device, `lseek` `ESPIPE`, `close` succeeds. `close`
  reports a refused `NP_CLUNK` as -1 with `errno`, the slot released
  either way. `/bin/CERRNO`
  (`libc/cerrno.c`) provokes each case; `make test-cerrno` runs it on FAT32
  and ext2 as root (27 checks each) and as `user` on ext2 for `EACCES`. fsd
  already answered `ENOTDIR` for a path through a file, on both. Controls:
  `main`'s `file.c` fails every check, and dropping the `EBADF` naming fails
  exactly the two mode checks, with `EACCES`.
- **Step 3, the environment: done 2026-10-05.** `crt0.c` builds `environ`
  from `GET_ENVC`/`GET_ENV` into static storage (`ENV_MAX`, 2048 bytes),
  through the same reader as argv, one entry per copy since the kernel
  refuses an out buffer over 512 bytes; an entry longer than that is left
  out rather than cut, since a cut `NAME=VALUE` is a different value.
  picolibc's `getenv`, which linked before and found nothing (picolibc's own
  `environ` is an empty vector), now answers what `set` made. crt0 declares
  `environ` and assigns it; picolibc defines it, and the hand-rolled libc
  weakly in `stdlib.c`, so a ported program that defines its own still
  links. `libc/cenv.c` prints `environ` in
  `/bin/PRINTENV`'s format, built through picolibc (`/bin/CENV`, with
  `getenv` answers) and the hand-rolled libc (`/bin/CENVH`); `make
  test-cenv` compares both with `printenv` before and after four `set`s,
  the last a 128-byte value (the shell's longest), and checks `getenv` for
  `SOURCE_DATE_EPOCH`, `PATH`, the long value, a set name and an unset one,
  against values the rig sets rather than printenv's. Control: an
  environment cut to one entry fails both comparisons and two `getenv`
  checks. Not reached by any rig: the skip of an entry over 512 bytes, which
  no shell spawn can stage; the kernel-side bound that removes it is its
  own PR.
- **Steps 4 and 5, `stat`, a clock: open.** Nothing in `libc/src` defines
  `stat` or `gettimeofday`.
- **`fstat` itself** was wrong before this plan touched it, and is fixed:
  #216 zeroes what it does not fill and builds the picolibc port against
  picolibc's headers, which had put the size in `st_dev`/`st_ino`. A real
  file identity is its own item in `ROADMAP.md`.
- **Step 7, room: half met.** `HEAP_PAGES` is 256 (1 MiB, #210), which
  holds Proem's `picodemo.c` and C11-header tests. Its 112-header picolibc
  test needs 2,031 KB, past the one 2 MB slot that holds a program's code,
  heap and stack, so step 8's second check waits on "a heap sized per
  program, and regions past one 2 MB slot" in `ROADMAP.md`. `MAX_FILES`
  (8) and `PATH_MAX_C` (96) are unchanged.
- **Steps 6 and 8** are not started; they are Proem's system-directories
  handoff (`docs/handoffs/2026-10-01-from-proem-system-dirs.md`, accepted).

So the order stands: steps 1 to 5 next, each small, then 6 and 8.

## What stays in Proem's session

Proem's side is recorded in Proem's own roadmap: that its sources build with
Ouroboros's flags and warnings clean, how the default include directory and
`target.h` are named on the command line (or built in), and that
`__STDC_VERSION__` stays `201112L` while `CFLAGS_OS` names no `-std` and
clang there reads gnu17. None of it blocks the steps above.

# C programs as full citizens: what Proem needs from the C runtime

**Scoping, written 2026-09-29 from the workspace session, before any code.**
The work belongs to the Ouroboros session. Grounded in the tree at `e8e328e`
and in CPP (now Proem) at `2aae9ee`, by reading the code and not the comments
above it. CPP was renamed Proem on 2026-10-01 (`~/Projects/Proem`, its driver
`driver/proem.c`); the plan below uses the new name throughout, and keeps the
old one only where it records what was read on 2026-09-29.

**On 2026-10-06 the preprocessor moved into DevTools and took the name cpp
back** (`~/Projects/DevTools/cpp/`, its driver `driver/cpp.c`, its library
`build/libcpp.a`, the macro `CPP_SYSTEM_DIRS`; `~/Projects/Proem` is archived
at `~/Projects/archive/Proem`). Step 8 below names the new build; elsewhere
"Proem" in this plan means that tool, as it did when written. Notes:
`handoffs/closed/2026-10-06-from-devtools-proem-moved.md` and
`handoffs/closed/2026-10-06-from-devtools-cpp-renamed.md`.

## Why this exists

The workspace chose Ouroboros as the destination of its C toolchain arc:
hello.c edited, compiled, linked and run here (the plan is in
`~/Projects/DevTools/docs/c-compiler-toolchain.md` since 2026-10-06, outside
this repository; the compiler is DevTools's too, written by hand there). The first
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
   *(Built 2026-10-06 over the path verb `NP_STAT` instead: see "Where it
   stands".)*
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
8. **Build and stage `/bin/cpp`.** A `cpp-bin` target beside `cpico-bin`,
   compiling the preprocessor's `lib/*.c` (which its own build archives as
   `build/libcpp.a`) and `driver/cpp.c` from `$(CPP_DIR)`
   (`../DevTools/cpp` by default, as its `pico-check` finds this tree at
   `../../Ouroboros`), linked like CPICO, and staged into `ESP_DIR/bin`.
   This keeps the link knowledge in one Makefile. *(Was `/bin/proem`,
   `proem-bin`, `PROEM_DIR` and `../Proem` until 2026-10-06.)* **Check, and
   the arc's finish line:** on a booted image, `cpp -include
   /include/target.h -I /include hello.c > hello.i` gives the same bytes as
   `clang $(CFLAGS_OS) -E` of the same file on the host, and the same for
   CPICO's `picodemo.c`.

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
- **Step 3, the environment: done 2026-10-05, #221.** `crt0.c` builds
  `environ` from `GET_ENVC`/`GET_ENV` into static storage (`ENV_MAX`, 2048
  bytes), through the same reader as argv, one entry per copy into the rest
  of the buffer: since 2026-10-06 (#222) the kernel's store reads accept any
  capacity and copy at most the entry, where `GET_ENV` refused anything over
  its blanket 512 bytes, which had forced a 512-byte window and a mirrored
  `USER_COPY_MAX` in crt0. picolibc's `getenv`, which linked before and
  found nothing (picolibc's own `environ` is an empty vector), now answers
  what `set` made. crt0 declares `environ` and assigns it; picolibc defines
  it, and the hand-rolled libc weakly in `stdlib.c`, so a ported program
  that defines its own still links. `libc/cenv.c` prints `environ` in
  `/bin/PRINTENV`'s format, built through picolibc (`/bin/CENV`, with
  `getenv` answers) and the hand-rolled libc (`/bin/CENVH`); `make
  test-cenv` compares both with `printenv` before and after four `set`s, the
  last a 128-byte value (the shell's longest), and checks `getenv` for
  `SOURCE_DATE_EPOCH`, `PATH`, the long value, a set name and an unset one,
  against values the rig sets rather than printenv's. Control: an
  environment cut to one entry fails both comparisons and two `getenv`
  checks. The kernel's rule has its own control in the same rig: crt0's
  first `GET_ENV` asks for 2047 bytes, so with the blanket refusal back
  every `getenv` answers unset, and `/bin/RDPROBE` reads all five stores
  through a 4 KiB buffer.
- **Step 4, `stat(path)`: done 2026-10-06, #223.** `file.c`'s `stat` sends one
  `NP_STAT`, the path verb `ls -l` uses, which fsd, `netd`'s export and the
  host peer all serve, and decodes the record through the same function as
  `fstat`. Not `open` + `fstat` + `close`, as step 4 first said: that asks
  for read permission on the file and an fd slot, and POSIX's `stat` needs
  neither. fsd authorizes `NP_STAT` by the ancestor walk alone, so `stat` of
  a file the caller may not read succeeds, a directory needs no open, and a
  full fd table does not stop it. A console binding is the character device
  `fstat(1)` reports, at the binding only; a path ending in `/` or `/.` must
  name a directory (`ENOTDIR` otherwise), checked against the path as given,
  since the library collapses it as text first. A `/net` binding answers
  `ENOSYS`, as `open` does: `netd` serves `NP_STAT` for `/`, `ip` and `mac`
  but not under `/tcp`, where existing files get `EISDIR`, `EIO` or
  `ENOENT` (an item in `ROADMAP.md`). `lstat` is `stat`, there being no
  symbolic links. `make test-cerrno` checks it: fourteen checks as root on
  FAT32 and ext2 (a missing path, a path through a file, an empty path, a
  null struct, a directory, a file against `fstat`'s record, `lstat`, every
  fd in use, a trailing slash on a directory and on a file, a trailing `/.`,
  the console binding, a path below it, a `/net` path), and as `user` on
  ext2 `stat` of `/etc/shadow` succeeding where `open` is refused, and the
  mode and owner of two files as `debugfs` reads them off the image
  (`/etc/shadow`'s owner is the host user who built the image, uid 501 gid
  20 here, since `mke2fs -d` carries it: the carried "ext2 uid 501" item,
  which this check found again; its uid and gid differ, so a swap is
  caught). Not observed: `stat` through a remote mount (the rig is one
  machine), a refusal for a directory above that is not searchable (the
  ext2 image has none), and the hand-rolled libc's `stat` (only `cerrno`'s
  picolibc build runs). `..` is lexical, as for every call in the library:
  `stat("/NOSUCH/../etc/passwd")` succeeds (an item in `ROADMAP.md`).
- **Step 5, a clock: done 2026-10-06, the first level, #224.**
  `libc/pico/clock.c`, in the picolibc port beside `builtins.c` (the
  hand-rolled libc has no `<time.h>`), gives `gettimeofday` and
  `clock_gettime` from `MONOTONIC_US`, so `time()` links and says 1970 plus
  uptime. `clock_gettime` answers every clock id picolibc's `<time.h>` names
  that is this clock: realtime and monotonic, and under `_GNU_SOURCE` the
  coarse, raw and boot-time ids; `gettimeofday` fills a `struct timezone`
  with UTC. picolibc's `features.h` defines `_POSIX_MONOTONIC_CLOCK` for
  RTEMS only, which hides the name `CLOCK_MONOTONIC`; the Makefile's
  `PICO_INC` defines it for every picolibc program here, since the clock
  exists, and `clock.c` refuses to build without it. `/bin/CCLOCK`
  (`libc/cclock.c`) runs twelve checks of its own, and `make test-cclock`
  runs it twice in one boot with a gap the host times: each `ctime` line
  must be Python's formatting of its second, each run must exit 0 (read from
  its own block), and the guest clock must keep the host's pace across a 20
  s gap within 0.3 s plus 1% (21.08 s against 21.09 s). Controls: a clock
  1000 times slow passes every check of `CCLOCK`'s own and fails only the
  pace, and so does one 5% fast (22.11 s against 21.08 s). Not ported:
  `times` (behind `clock()`), `clock_getres`, `nanosleep`. The second level,
  a wall clock from the platform's RTC, is an item in `ROADMAP.md`.
- **Step 6, headers on the disk: done 2026-10-06, #225.** `make esp` stages
  picolibc's 136 headers in `/include` with a `target.h` generated from
  `$(CC) $(CFLAGS_OS) $(PICO_INC) -dM -E`, the flags a picolibc program here
  is compiled with (so it carries `_POSIX_MONOTONIC_CLOCK`, step 5's flag),
  and clang's eleven freestanding headers and their 20 `__stddef_*`,
  `__stdarg_*` and `__float_*` helpers (by glob) in `/include/clang`: 168
  files, as the system-dirs note asked. The ext2 and exFAT images copy the
  same tree. Two things found on the way. FAT32: macOS writes a lowercase
  name that fits 8.3 as a short entry with byte 12's case flags and no long
  name, and fsd ignored the flags, so `stdio.h` listed as `STDIO.H` (lookup,
  being case-insensitive, was never affected); fsd now reads them, and
  writes them too: a name it creates keeps its case, all-lowercase halves
  flagged, a mixed-case name as a long name, so `touch foo.txt` and a
  case-only `mv -f` of a file no longer turn uppercase (the review of #225;
  a directory's case-only rename is still refused, an item in `ROADMAP.md`).
  And `ls` of `/include` (72 names) or `/include/clang` (31) shows only what
  fits one 512-byte `NP_READDIR` reply, with no sign of the rest, an older
  gap now in `ROADMAP.md`; it does not touch cpp, which opens headers by
  path. `make test-include` checks all three images: every staged file
  exists by path with its size (168 of 168, `ls -l` with file operands, no
  listing); the whole listing of each directory that fits one reply, names
  and case included (seven of nine), and for the other two that every name
  listed is a staged one in its case; three files `cat` to the host's bytes;
  and names made on the guest keep their case. The card (`make sdcard`)
  skips `/include`, which only the stick can serve.
- **Steps 7 and 8, room to run and `/bin/cpp`: done 2026-10-06, #226, the arc's
  finish line.** `make cpp-bin` compiles DevTools's cpp (`lib/*.c` and
  `driver/cpp.c`, read from `CPP_DIR`, `../DevTools/cpp` by default) as
  every picolibc program here is, with
  `-DCPP_SYSTEM_DIRS='"/include:/include/clang"'`, and links it like CPICO:
  it linked first time, 104 KB, no warnings, every call it makes met by
  steps 1 to 5. `make esp` stages it as `/bin/CPP` when `CPP_DIR` has the
  sources, and says so and goes on when it does not, so this tree alone
  still builds. On a booted image, `cpp hello.c` with no options prints the
  program, and `make test-cpp` checks the rest: `cpp -o` of `hello.c`,
  CPICO's `picodemo.c` (a dozen headers deep) and `high.c` (UTF-8, raw bytes
  past 0x7f, and `__DATE__`/`__TIME__`, so `SOURCE_DATE_EPOCH` is read) exit
  0; the host reads the `.i` files back off the image and each is byte for
  byte what the same cpp, built for the Mac from the same sources with
  signed and with unsigned `char`, writes from the same staged headers
  (paths mapped, `SOURCE_DATE_EPOCH` set on both); and clang for this target
  compiles all three. Step 7 needed nothing: the 1 MiB heap held
  `picodemo.c`. The finish line was written as "the same bytes as `clang
  $(CFLAGS_OS) -E`"; clang's spacing and line markers differ from cpp's, so
  the check compares with cpp on the host, and cpp against clang token for
  token is DevTools's own `make pico-check`.
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
  handoff (`docs/handoffs/closed/2026-10-01-from-proem-system-dirs.md`, done in
  #226).

So the order stands: steps 1 to 5 next, each small, then 6 and 8. *(As of
2026-10-06, all eight steps are done, #219 to #226: the arc is
finished.)*

## What stays in Proem's session

Proem's side is recorded in Proem's own roadmap: that its sources build with
Ouroboros's flags and warnings clean, how the default include directory and
`target.h` are named on the command line (or built in), and that
`__STDC_VERSION__` stays `201112L` while `CFLAGS_OS` names no `-std` and
clang there reads gnu17. None of it blocks the steps above.

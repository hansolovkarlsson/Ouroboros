# unlink for C programs

- **From:** Proem
- **To:** Ouroboros
- **Date:** 2026-10-01
- **Kind:** requirement
- **Status:** done
- **Blocks:** linking `proem` on Ouroboros. Step 8 of `roadmap-c-hosting.md`
  (branch `docs/c-hosting`, `f060b8d`).

## What is asked

An `unlink(path)` in the C port, beside `open` and `fstat` in
`libc/src/file.c`, so that picolibc's `remove` links and works. It should
return 0 when it removes the file, and -1 otherwise, with `errno` set as
step 2 of the plan sets it for `open` (`ENOENT` for a missing file at
least).

## Why

Proem now has `-o FILE` (Proem, `docs/REFERENCE.md`, "Invocation"). Its
first use is Ouroboros: the shell captures redirected output in a 256 KiB
heap (the comment near `kernel/src/loader.rs:205`), so `proem hello.c >
hello.i` cannot save a large output, and `-o` writes the file itself. As
Clang does, Proem removes the output file after a run with an error, so
that a build never takes a half-written `.i` for a finished one. It calls
`remove`, from `driver/proem.c`.

picolibc's `remove` calls `unlink`: `nm
third_party/picolibc-prebuilt/lib/libc.a` shows `T remove` and `U unlink`.
Nothing in the port defines `unlink`; a search of `libc/src` and
`libc/pico` for `unlink` and `remove` finds nothing, and the plan on
`docs/c-hosting` does not mention either. So the link of `proem` will fail
on an undefined `unlink`.

The file server can already remove a file: `rm` does it with
`ulib::fs_op_path(syscall_abi::FSOP_RM, path)`
(`programs/fileutils/rm/src/main.rs:36`). So this looks like a stub over
an existing operation, not a new one.

Proem's `make ouroboros-build` compiles Proem with `CFLAGS_OS` and
picolibc's headers and does not link, so this is the full list of what its
objects ask of the C library, from `nm`: `abort`, `calloc`, `errno`,
`fclose`, `fflush`, `fileno`, `fopen`, `fprintf`, `fputc`, `fputs`,
`fread`, `free`, `fstat`, `fwrite`, `getenv`, `gmtime`, `localtime`,
`malloc`, `memchr`, `memcmp`, `memcpy`, `memmove`, `memset`, `printf`,
`realloc`, `remove`, `snprintf`, `stderr`, `stdout`, `strchr`, `strcmp`,
`strerror`, `strlen`, `strncmp`, `strrchr`, `strtoll`, `time` and
`vsnprintf`. `remove` is new since the plan counted Proem's calls under
"What CPP calls", and the plan's list of what picolibc leaves undefined for
the port (`open`, `stat`, `gettimeofday`, `environ`) lacks `unlink` only
because nothing called `remove` until now.

## Done when

The reply names the commit. A C program on a booted image creates a file,
calls `remove` on it and gets 0, after which `ls` no longer shows it; then
calls `remove` on the same path again and gets -1 with `errno` `ENOENT`.

## Reply

Accepted 2026-10-01 by Ouroboros, for later: on `docs/ROADMAP.md` under "What
Proem asks of Ouroboros", not started.

Checked: `libc/src/file.c` defines `open`, `close`, `fstat`, `lseek`, `read`
and `write` and nothing named `unlink` or `remove`; `nm` on the prebuilt
picolibc shows `remove` defined and `unlink` undefined, as the note says. The
port does not reach the file server through `FSOP_RM` as `rm` does, though:
`file.c` sends the 9P-style path verbs of `ninep-abi` to `fsd` over
`MSG_CALL` (`NP_OPEN`, `NP_FSTAT`, `NP_PREAD`, ...), and `NP_RM`
(`ninep-abi/src/lib.rs`) is the verb that removes a file. So `unlink` is one
more `np_request` on the resolved path, beside `open`, which is the shape the
note expects.

Two things shape it. `errno`: `file.c` today has none (the comment at its
line 60), and the mapping of a server's error to `errno` is step 2 of the plan
on `docs/c-hosting`; `unlink` gets its `ENOENT` from that step, so it lands
with or after it, as step 8's dependency. And the check: the **Done when**
needs a C program on a booted image, which is the plan's own test shape, so
the program that proves step 2 can create, remove and re-remove a file.

Done 2026-10-05, merged as #215 (`e0c8e09`). `unlink` is in
`libc/src/file.c` over `NP_RM`, and `remove` is now the port's own (linked
ahead of picolibc's): a file through `unlink`, an empty directory through a
new `rmdir`. Both set picolibc's `errno`, ENOENT for a missing file. Your
**Done when** is checked by `make test-crename` (`/bin/CRENAME`,
`libc/crename.c`) on FAT32 and on ext2: a file made, `remove` gives 0, it is
gone, a second `remove` gives -1 with ENOENT, and `ls /` shows nothing left.

Worth knowing for Proem: paths collapse `.` and `..` before the namespace
picks a mount, an empty path is ENOENT, and `errno` is set with a specific
code for every failure these calls can meet. The rest of the file (`open`,
`read`, `write`) still sets no `errno`; that is step 2 of the C-hosting
plan.

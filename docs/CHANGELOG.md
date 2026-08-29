# Ouroboros changelog

Historical record of completed milestones, newest first. For
forward-looking plans, see [`roadmap.md`](roadmap.md); for the
debugging history and lessons behind each decision (what was tried,
what broke, how it was diagnosed), see the debugging postmortems under `docs/`; for *how* something
here actually works today, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md).

## Symbolic-mode `chmod`, `chown` by name (2026-08-29)

The two write halves of the mode/owner surface, finishing the "small polish"
the users/permissions arc left behind.

**`/bin/chmod` takes POSIX symbolic modes** as well as octal.

- **`[ugoa...][+-=][rwxXst...][,...]`**: `chmod u+x script`, `go-w`, `a=rx`,
  `u+rw,go+r` (comma clauses applied left to right, each seeing the last one's
  result), multi-op clauses (`u+x-w`), and the copy form `g=u` ("give group what
  the owner has"). `X` is the conditional execute — set only if the target is a
  directory or already carries some execute bit. `s`/`t` set setuid/setgid/sticky;
  those bits are **stored** (ext2 keeps the full low 12) but nothing acts on them
  yet — this system has no setuid execution, and the man page says so.
- **Naming no "who" means `a`.** There is no umask here, so a bare `+x` really is
  all three fields, where POSIX would mask it. Documented rather than silently
  divergent.
- **Symbolic is relative, so it stats first.** An octal mode is absolute and goes
  straight to `NP_CHMOD`; a symbolic one needs the current bits (and the
  directory flag for `X`), so `chmod` reads them with `fs_stat` and applies the
  expression to what it finds. A filesystem with no mode to build on (FAT32/
  exFAT/`/proc`, `mode_valid == 0`) gets the same "not supported by this
  filesystem" refusal the write itself would have given.
- **Tested twice over.** The parser is pure (bytes + current mode → new bits), so
  it was developed against a **host harness** — 27 cases including every operator,
  the copy form, `X` on a file vs a directory, clause chaining, and six malformed
  expressions that must be rejected — before being wired in (the same
  "host-harness the pure part" move the GPT-CRC work used). Then end to end on
  `run-image-ext2`: `644` → `u+x` → `744`, `a=rx` → `555`, `u+rw,go+r` + `g=u` →
  `775`, `a+X` leaves a plain file alone but takes a directory `644` → `755`, and
  `chmod q+z` / `chmod u` are rejected while `chmod 755` still works.

**`/bin/chown` takes names** as well as numeric ids — the twin gap the `chmod`
work surfaced (its module doc still claimed "there is no `/etc/passwd` yet",
written before the users arc shipped).

- **`chown alice:staff f`**, `chown alice f`, `chown :staff f` — each field
  resolved through `/etc/passwd`/`/etc/group` with the shared `accounts` lookups,
  the same parser `login`/`su`/`id` use. Numeric ids still work unchanged.
- **An all-digits field is always an id**, never a name lookup — the rule `su`
  and `useradd -g` already follow, so `chown 1000 f` can't be hijacked by an
  account literally named `1000`.
- **Both fields resolve before anything is written.** An unknown name exits with
  `no such user: bob` / `no such group: nogroup` *before* the single `NP_CHOWN`,
  so a half-applied `chown user:nogroup` (uid changed, group not) is impossible —
  the same prepare-then-commit shape as the `useradd` fix below.
- **One deliberate POSIX divergence, documented:** GNU's trailing colon
  (`chown alice: f`) also sets the group to alice's login group; here it leaves
  the group alone, matching what the numeric form has always done. The man page
  says so rather than diverging silently.
- **Verified** on `run-image-ext2`: `user` → `1000:0`, `user:staff` → `1000:1500`,
  `:0` → group only, `501:20` numeric, and `nobody` / `user:nogroup` / `:`
  rejected with the file left untouched.

## Small-gaps cleanup: `useradd` is atomic + `/etc/skel`, one account-file reader (2026-08-29)

Two of the roadmap's small open items, both code-review notes left by PR #22.

- **`useradd` no longer half-creates an account.** It writes three things —
  `/etc/group`, the home directory, `/etc/passwd` — with no multi-file
  transaction to lean on, and it used to write the *account line first*, then
  best-effort the group and home, reporting success either way. A failure after
  the passwd write left an account whose primary group or home never got made.
  The order is now chosen so the **`/etc/passwd` write is the single commit
  point**: group and home are prepared first, and if either fails nothing is
  committed and the tool exits non-zero; if the commit itself fails, the prep is
  **rolled back** (the group line removed via a new `accounts::remove_line`, a
  home directory we created removed). Two latent siblings of the same bug went
  with it: a user-private group whose name already existed left the account's gid
  naming *nothing* (now the existing group's gid is adopted), and an existing
  *file* where the home should go was silently `chmod`ed as if it were the home
  (now refused, `FS_ERR_NOT_A_DIRECTORY`).
- **`/etc/skel`.** If it exists, its **top-level files** are copied into a home
  `useradd` just created — each chowned to the new user and carrying the template
  file's mode (a `600` template lands as `600`). There is no `/etc/skel` by
  default and its absence is silent; subdirectories are reported and skipped (a
  first cut — recursion wants a path stack this fixed frame doesn't have); and a
  home that `useradd` merely *adopts* is left alone, since dropping files into a
  directory someone else made is not this tool's call. The copy runs **after** the
  commit and its failures are **warnings**: skel content is a convenience, the
  account/group/home are already complete without it, and keeping it post-commit
  leaves the rollback above a plain `rmdir` (which would fail on a directory we
  had already filled). The copy loop is `/bin/cp`'s shape — truncate, then
  `fs_read_bulk`/`fs_write_at` by chunk — repeated rather than shared, since a
  `/bin` program can't spawn another.
- **One account-file reader in the shell.** `login::read_passwd` and
  `read_account_file` were the same chunked `FS_DATA_MAX` loop, differing only in
  login's bounded `NO_FS` retry (fsd may still be mounting at boot). Merged into
  one `read_account_file` carrying the retry; `login`, `su`, and `id`'s name
  lookups all come through it. (`ulib::read_file_all` stays separate — it's in
  the `/bin` programs, and the shell has its own fs layer.)
- **Verified** on `run-image-ext2` over two unattended QEMU sessions (the
  paced-stdin recipe): `useradd -g nosuchgroup` refuses before writing anything;
  `useradd dave` with `/Users/dave` a file fails at the home step and leaves
  **neither** `/etc/passwd` nor `/etc/group` touched (the rollback); `useradd
  alice` still creates account + group + home owned `1001:1001`; `useradd -g devs
  bob` uses `devs` and invents no second group; `useradd carol` with an existing
  `carol` group adopts its gid (2000); `useradd erin` adopts an existing home and
  chowns it; and `login` + `su bob` + `id` confirm the merged reader. For
  `/etc/skel`: no skel dir → empty home, silently; with `README` (644) and
  `NOTES` (600) → both copied to `alice`, owned `1002:1002`, modes preserved,
  contents intact; a `sub/` directory reported and skipped; and `bob`, whose home
  already existed, left empty. Plus the
  `accounts` host tests (now 9 — `remove_line` round-trips with `append_line`).

## Account management: passwd/useradd/groupadd/usermod, names, groups, homes (users arc, step 4 — "closing the security arc")

The users/permissions arc's account layer above the kernel identity: tools to
**create and manage users and groups**, name resolution, and per-user home
directories. This is the "close the security update" pass — the model was
**option 1** (root-only admin tools) built on a **shared helper** so a later
self-service `passwd` (an `accountd` server or a setuid bit) slots in without a
rewrite.

- **New `accounts` crate** (repo-root, pure `no_std`, **host-unit-tested** —
  `cargo test -p accounts --target aarch64-apple-darwin`, 8 tests): the single
  home for `/etc/passwd` + `/etc/group` parsing/formatting, SHA-256 password
  hashing, salt derivation, name↔id lookups, and the `replace_line`/`append_line`
  file-rewrite helpers. `login` and every `/bin` account tool call it, so the
  logic exists once; it also absorbed the shell's duplicated `sha256.rs`.
- **`/bin/passwd [user]`** — set a password (root only): prompts twice (echo
  off), fresh clock-derived salt, rewrites the one account line.
- **`/bin/useradd <name> [-u uid] [-g group|gid]`** — create a user (root only):
  appends the account, prompts the initial password, creates a **user-private
  group** (`gid == uid`, the Linux default) when `-g` is absent, and makes +
  chowns the home directory `/Users/<name>`.
- **`/bin/groupadd <name> [-g gid]`** and **`/bin/usermod <user> -g <group|gid>`**
  — create a group / set a user's **primary group** (root only). The group model
  is **primary-gid** (a task carries one kernel gid); full supplementary-group
  membership stays a kernel-identity tier.
- **`su <username>`** — root drops to a named account (numeric `su 1000:1000`
  still works). A non-root `su` stays denied (the kernel escalation guard).
- **`id` shows names** — `uid=1000(user) gid=1000(user)`, resolved via
  `/etc/passwd` + `/etc/group`, numeric fallback if unlisted.
- **Per-user homes under `/Users`** — the login home for `user` is `/Users/user`;
  login exports `HOME`, and the shell expands a leading **`~`** to `$HOME`
  (`cd ~`, `cat ~/f`). The ext2 image chowns `/Users/user` to the user, so the
  permission-enforcement demo is real (a user writes in `~`, `/` is denied).
- **`/etc/group`** (`name:gid:members`) staged by `scripts/mkgroup.py`; the
  account files are read chunked into a 2 KB buffer (~20 accounts), matching the
  tools' write cap.
- **Salts are clock-derived** (`MONOTONIC_US` + hash) — documented as weak; a
  virtio-entropy `RANDOM` syscall is the noted upgrade. All tools are PIE-safe
  (byte-only parsing; `llvm-readobj -r` shows zero `R_AARCH64_ABS64`).
- **New files/dirs are owned by their creator** — a bug the arc *surfaced*: ext2
  `touch`/`write_file`/`mkdir` hardcoded `uid/gid = 0`, so a logged-in user's new
  file was root-owned and the follow-up data write was denied (a user couldn't
  use its own home). fsd now stamps the caller's identity onto the fs
  (`set_creator`, from `GET_ID(sender)`) before each op; ext2 creates with it
  (root → `(0,0)`, unchanged for boot/format). Verified: a `user` session writes
  `~/note.txt` and reads it back, while `touch /` stays `permission denied`.
- **Verified end to end** on `run-image-ext2` (login `user`: `id` names, home,
  `~`, home-write allowed, `/`-write denied; login `root`: `useradd`/`groupadd`/
  `usermod`/`su carol` → `uid=1001(carol) gid=1002(staff)`), plus 8 `accounts`
  host unit tests (`cargo test -p accounts --target aarch64-apple-darwin`).
- **Two code-review fixes before merge** (PR #22): ext2 `write_file`'s overwrite
  branch preserved the old inode's `mode`/`links` but not its owner, so with
  creator ownership live, *overwriting* an existing file re-owned it to the
  writer (POSIX overwrite never chowns) — now preserves `uid`/`gid` too; and
  `/bin/id` read `/etc/passwd` with the 512-byte inline read while
  `login`/`su`/`useradd` read the full ~2 KB, so it couldn't resolve names past
  ~5 accounts — switched to the bulk read. See
  `docs/account-management-postmortem.md`.

## picolibc: the real C library (libc arc, step 6)

Unmodified standard-C stdlib code now runs on Ouroboros through a **picolibc
port** — `%f`/`%e`/`%g` float formatting, `snprintf`, `qsort`, `malloc`,
`strtol`, the things the hand-rolled libc (steps 3–4) couldn't do. picolibc is
the C library; our `libc/src` stubs are its *porting layer*.

- **picolibc 1.8.9, built `-fPIC`** → it self-relocates under our existing PIE
  loader with **`R_AARCH64_RELATIVE` only, zero `ABS64`** (the recurring
  unloadable-relocation trap — checked with `llvm-objdump -R`). No loader or
  kernel change was needed; the whole port is a build-and-link exercise.
- **Its stdio bottoms out at our stubs.** Built with `-Dposix-console=true`,
  picolibc wires `stdin`/`stdout`/`stderr` to fd 0/1/2 — i.e. to the
  `write`/`read`/`open`/`close`/`lseek`/`fstat`/`sbrk`/`_exit` in `libc/src/`
  (`os.c`, `file.c`), the same stubs the hand-rolled libc used. The porting
  layer is compiled against picolibc's *own* headers so struct/ABI shapes
  (`struct stat`, flags) match.
- **Two compiler-rt builtins carried ourselves** (`libc/pico/builtins.c`):
  `__lshrti3`/`__ashlti3`, the 128-bit shifts picolibc's exact-float (ryu) path
  references. macOS ships compiler-rt only as Mach-O (unusable for our ELF
  link), so these two are hand-written (split-into-64-bit-halves, since clang
  lowers a variable 128-bit shift to these very calls).
- **`-Dnewlib-global-errno=true`, no TLS, no picocrt** (we use our own `crt0`),
  no semihosting/multilib/tests. See `libc/picolibc-cross.txt` (the meson
  cross-file: clang `-target aarch64-none-elf` + `--ld-path=…/ld.lld`, LLVM
  binutils from the Rust toolchain).
- **`make cpico-bin` → `/bin/CPICO`** (`libc/picodemo.c`): booted, logged in,
  ran it — `pi=3.14159  e=2.718e+00  g=1.23457e+06`, `qsort: 1 2 3 5 8 9`,
  `snprintf`/`malloc`/`strtol` all correct, exit code 0.
- **The prebuilt lib is committed** (`third_party/picolibc-prebuilt`, ~2.1 MB:
  a debug-stripped `libc.a` + headers), so a fresh checkout needs no meson/ninja
  to build. `scripts/build-picolibc.sh` regenerates it; the 39 MB source clone
  and build trees are gitignored.

Remaining in the arc: port a real application (SQLite, a small C compiler) — now
"port one more program," not "invent the mechanism."

## Fids: server-side open-file handles (libc arc, step 5)

`fsd` now has real **open-file handles** — a `fid`, which is exactly what a
POSIX fd is (and a 9P fid). The deferred-since-cluster-Phase-0 feature: *a POSIX
fd ≈ a 9P fid*, one mechanism serving both C portability and the proper 9P
model. The C libc's file I/O now uses them.

- **Five new fsd verbs** (`ninep-abi`): `NP_OPEN` (path + flags → a fid ≥ 3,
  usable directly as a C fd), `NP_PREAD`/`NP_PWRITE` (fid + offset), `NP_FSTAT`,
  `NP_CLUNK`. A small per-client fid table lives on `fsd`'s stack (`MAX_FIDS`,
  owner-checked); when full, `NP_OPEN` first reaps fids whose owner task has died
  (`TASK_STATE`) — the pragmatic answer to fsd not being told when a client dies.
- **Permission is checked once, at open** (POSIX semantics): `NP_OPEN` runs the
  `check_access` gate (R for a read-open, W for a write/create-open); the
  subsequent `NP_PREAD`/`NP_PWRITE` skip it — verified on ext2, where a
  non-root user's `open` of the root-owned `/` for create is denied.
- **They coexist** with the path-per-op verbs — no client migration. The C libc
  (`file.c`) switched to fids; the shell/`ulib`/cluster export stay path-based
  (`cat` reads the same file `cfile` wrote via a fid). The cursor stays
  client-side and rides each read/write offset (authentic 9P).
- **`NP_STAT`'s reply builder** was factored into a shared `write_stat_reply`
  used by both the path `NP_STAT` and the fid `NP_FSTAT`.

Verified: `cfile` writes/reads a file entirely through fids, `cat` confirms it's
on disk, `cfile | grep hello` pipes cleanly, and the ext2 permission-at-open
denial holds.

## C file I/O + pipe-aware output (libc arc, step 4)

C programs can now **read and write files**, and their output **flows through
pipes** — so a C program is a real participant in the shell, not just a
compute-and-print toy. `# cfile` writes `/CTEST.TXT`, stats it, and reads it
back (the shell's own `cat` confirms it's on disk); `cfile | grep hello` filters
its output.

- **`libc/src/file.c`**: `open`/`read`/`write`/`close`/`lseek`/`fstat` over
  `fsd`'s `NP_*` verbs (`MSG_CALL` to `FSD_TASK`), through a small fd table
  (fds ≥ 3 hold a path + cursor, since `fsd` is path-per-op). Reads use the
  inline `NP_READ_AT`; writes grant the buffer + `NP_WRITE_AT`; `fstat` reads
  `NP_STAT`. Relative paths resolve against the cwd (`GET_CWD`); tree 0 (the
  default disk mount) only, for now. New `<fcntl.h>`/`<sys/stat.h>` + `make
  cfile-bin` → `/bin/CFILE`.
- **`write(1|2)` is stdout-target-aware:** to the console (batched via `cond`),
  or — when the program is a pipe producer — to the consumer via `MSG_SEND`,
  with an end-of-stream marker on `exit`. C programs now work in pipelines.
- **Two bugs the pipe test caught and fixed:** `printf` was emitting *one
  `write` per character* (a flood of 1-byte pipe messages), and `pipe_write`
  gave up on a full consumer mailbox instead of yielding — together they dropped
  bytes and merged lines downstream. Fixed by **buffering** stdout (flush on
  newline / full / exit) and making the pipe send **yield + retry** on
  `MSG_ERR_FULL` (the same shape as `ulib::pipe_out`).

Next: file descriptors as real server handles (fids), then port picolibc/newlib.

## A minimal libc: C programs with printf/malloc/strings

The libc arc's third step — a real (if small) C standard library, so a C program
is written *normally* (`#include <stdio.h>`, call `printf`/`malloc`) instead of
hand-rolling syscalls. `# cdemo` prints formatted output, allocates memory, and
sums 1..100 = 5050.

- **`libc/`** now has standard-ish headers (`libc/include/{stdio,stdlib,string,
  unistd}.h`) and sources (`libc/src/`): `crt0` (`_start`), `os.c` (the syscall
  stubs — `write`→console via `PUTC`, `read`→keyboard via `READ_CHAR`,
  `sbrk`→the `HEAP_INFO` region, `_exit`→`EXIT`), a minimal `printf`
  (`%d`/`%i`/`%u`/`%x`/`%X`/`%c`/`%s`/`%%`), `malloc`/`free` (a bump allocator
  over `sbrk`; `free` a no-op), and the `string.h` essentials. `make cdemo-bin`,
  staged as `/bin/CDEMO`.
- **This is where the last two steps pay off:** `sbrk` keeps its break pointer in
  a `.bss` `static` (unlocked by the `.data`/`.bss` milestone), and the whole
  thing rides the C-runs-on-Ouroboros toolchain path.
- **`-fno-builtin` on the libc sources** so the optimizer doesn't rewrite
  `string.c`'s `memcpy`/`memset` loops into calls to themselves (a classic
  freestanding-libc self-recursion); user programs still get builtins, which
  resolve to the libc's own `memcpy`. Zero `ABS64`, no undefined symbols.
- Verified fresh-per-spawn (ran twice, identical output — the bump heap
  re-initializes each spawn). `libc/hello.c` stays the self-contained no-libc
  proof + `.data`/`.bss` regression test.

Still to grow: file I/O (`open`/`read`/`close` via `fsd`, an fd table), a
stdout-target-aware `write` (pipes/redirection), then picolibc/newlib.

## Userland `.data`/`.bss` support: programs can have globals/statics

The libc arc's second step, and the real unblock for non-trivial C: **userland
programs may now have mutable statics/globals.** A C global or file-scope array,
or a simple Rust `static mut`, lands in `.data` (initialized) or `.bss` (zeroed)
— previously the linker script `ASSERT`ed both empty.

- **The loader already had the mechanics.** `loader.rs`'s `copy_segments` copies
  each `PT_LOAD` segment's `p_filesz` file bytes *and* zeroes the `p_memsz -
  p_filesz` tail (a real `.bss`), and `elf_region_size` sizes the region to
  `p_memsz` — in *both* the boot-load and runtime-spawn paths. So the change was
  removing the two `ASSERT(SIZEOF(.data/.bss)==0)` guards in
  `programs/linker.ld` (a deliberate scope-gate, not a technical gap) and
  **verifying** it actually works. No kernel change.
- **Verified fresh-per-spawn.** `libc/hello.c` now exercises an initialized
  `.data` global (`g_data = 7`) and a `.bss` global (`g_bss`): it reads `data=7
  bss=0`, mutates both to `data=8 bss=5`, and — run twice — starts at `data=7`
  *both* times, proving each spawn loads a fresh copy of `.data` rather than
  sharing a mutated region. A global holding a pointer emits an
  `R_AARCH64_RELATIVE` the loader applies (confirmed: two RELATIVE, zero ABS64).
- **Caveat for Rust:** this does *not* lift the separate liballoc ceiling —
  `alloc`'s collections still fail to link on an `R_AARCH64_ABS64` in prebuilt
  liballoc. Plain scalar/array `static`s are now fine, though.

Next in the arc: a real libc (crt0 + POSIX syscall stubs), now that C programs
can hold state. See `docs/processes.md`'s "Writing a program in C".

## The first C program runs on Ouroboros (userland-libc arc begins)

The foundational milestone of the POSIX/C-portability arc: a **C program,
cross-compiled, runs on Ouroboros** through the same loader and syscall boundary
the Rust programs use. `# chello` → `hello from C on Ouroboros`, spawned and
reaped by the shell like any `/bin` program.

The point was to prove the **toolchain path**, which was the one real
uncertainty — the rest of the arc is growing a libc, not inventing a mechanism:

- **clang → our PIE format.** Apple's system `clang` targets
  `aarch64-unknown-none` (ELF), and Rust's bundled **LLD** (`ld.lld`) links the
  object against the *same* `programs/linker.ld` and PIE flags
  (`-fPIC`/`-pie`/`--no-dynamic-linker`) the Rust programs use, then
  `llvm-objcopy` strips it to the identical `.bin` shape. No extra cross-toolchain
  to install. `make chello-bin`.
- **The loader needed no changes.** The C ELF is `ET_DYN`, entry at offset 0
  (`_start` in `.text.start`), zero problematic relocations (no `R_AARCH64_ABS64`,
  no `.data`/`.bss` — the linker script's ASSERTs held), so the existing
  loader/spawn path loads and relocates it exactly like a Rust program.
- **`libc/hello.c`** is deliberately self-contained for now — its own `_start`
  plus inline `svc` syscall stubs (`putc`/`exit`) — because the milestone is the
  *path*, not the library. The real libc (crt0 + POSIX syscall stubs
  `_write`/`_read`/`_open`/`_sbrk`/`_exit`/…, then picolibc/newlib on top) grows
  from here. Constraints inherited from the loader (no static mutable state yet,
  so no globals/`.data`/`.bss`) are documented in `docs/processes.md`.

## Permission enforcement: the file mode/owner finally means something (arc step 3)

The last piece of the users/permissions arc. The three inputs existed — file
**mode/owner** (stat/chmod/chown), a per-task **identity** (login), and the IPC
layer telling `fsd` **who sent** each request — but nothing joined them. Now
`fsd` **checks** the caller against the target's owner + mode and refuses with
**`FS_ERR_PERM`** ("permission denied") when the classic Unix rule says no.

- **The check** (`fsd`'s `check_access`, gating every `NP_*` file verb): look up
  the caller's uid/gid (`GET_ID(sender)`), then — **root (uid 0) bypasses** —
  compare against the file's owner/mode via the owner→group→other triad. Reads
  need `r` on the target; writes need `w` (on the file, or the parent dir when
  creating); create/delete/rename need `w` on the parent dir; `chmod` is
  owner-or-root; `chown` is root-only.
- **ext2 only.** FAT32/exFAT/`/proc` return `mode: None` from `stat`, which the
  check treats as unrestricted — the same honest per-FS degradation as
  `chmod`/`chown`. So the FAT *boot* disk is unchanged; enforcement bites on an
  ext2 disk (`run-image-ext2`).
- **Deliberate first-cut scope:** the check covers the *object* of the operation
  (and the parent dir for namespace changes), but **not** the search (`x`) bit on
  every ancestor directory — that POSIX path-traversal check is a clean
  follow-up, kept out so the whole thing lives centrally in `fsd`'s dispatch
  rather than woven into each filesystem's path walk. Remote (cluster) requests
  come through `netd` (a root server) and stay machine-authenticated by the
  cluster key; per-user cluster identity remains a named future tier.
- **Root bypass is the safety valve** — a bug in the check can only ever
  *over*-restrict a non-root user, never lock root out.

Verified on QEMU (ext2): as root, set up `/secret` (mode 600) and a user-owned
`/u`, then drop to the user (`su 1000`) — `cat /secret` → "permission denied",
`touch /nope` (writing root-owned `/`) → "permission denied", but `touch /u/mine`
(a dir the user owns) succeeds and `ls /u` shows it; the user still runs `/bin`
programs (755, world-executable). The FAT boot image is unaffected (a user
`touch`es freely there). **This completes the users/permissions arc** — the
roadmap's #1 gap.

## Login: authenticated sessions (the login arc, step 2)

The shell now **gates every session on a login prompt** — username + password,
checked against `/etc/passwd` — and runs the session as that user. Step 2 of the
users/permissions arc, built on the identity primitive from step 1.

- **Saved-uid, not a separate login process.** The shell stays task 0 and uses a
  POSIX-style **saved identity**: root drops to the logged-in user, and on logout
  restores root (`SET_ID` back to the saved identity) to re-prompt. Chosen over a
  `login`-as-init process because that would have required rewiring the
  capability model's "slot 0 = the shell" assumption (a security-sensitive
  refactor); the saved-uid path delivers the same login → session → logout →
  re-login UX with no capability changes. The kernel gained `SAVED_IDS` +
  `apply_id`; a child's saved identity is its *own* uid (not the parent's), so a
  user's programs can never restore root.
- **Escalation is closed at the shell.** Because the shell's saved identity is
  root (for logout), a naive `SET_ID(0)` from a logged-in user would escalate —
  so `su` is now **root-only** (enforced in the shell, the login TCB). A user's
  only route to root is `logout` + logging in as root; child programs can't
  restore root at all. Verified: as `user`, `su 0` is denied and `id` stays 1000.
- **Auth.** `/etc/passwd` is `name:uid:gid:home:salt:hash` with
  `hash = SHA-256(salt‖password)` — salts/hashes precomputed at build time
  (`scripts/mkpasswd.py`), so login only *verifies* (no runtime RNG). SHA-256 is
  copied into the shell (`sha256.rs`, from netd's validated impl; a shared crate
  is a noted follow-up). Passwords are read with echo off; the check is
  constant-time. DEV accounts: `root`/`root`, `user`/`user`. No `/etc/passwd` →
  login falls back to a root session (the "no accounts configured" bootstrap).
- **`logout`** (and `exit`) end a session; the shell never exits (it's task 0).
  Each session starts clean (fresh env + cwd). New `man login`.

Verified on QEMU (FAT32 + ext2): boot shows `login:`; `user`/`user` → `$` prompt,
`id` → `uid=1000`; a wrong password → "Login incorrect"; `logout` → back to the
login prompt; `root`/`root` → `#` prompt, `id` → `uid=0`; and `su 0` as the user
is refused. (Per-user `/home` and `passwd`/`useradd` are the next small steps;
home is `/` for now.)

## User identity: a uid/gid per task (the login arc, step 1)

The first step of the users/permissions arc: **every task carries a user
identity**, the unforgeable foundation a permission check (a later step) will
build on. Login, passwords, `/etc/passwd`, and `/home` come next; this is just
the primitive plus the ability to see and change it.

- **Where it lives — the kernel, deliberately.** This *reverses* the roadmap's
  tentative "identity is probably a userland construct" note, on security
  grounds: the kernel is the only component that unforgeably knows an IPC
  message's real sender, so it is the right root of trust for the
  task→identity binding. One packed `(gid << 32) | uid` per task slot
  (`tasks.rs`'s `IDS`, mirroring `STDOUT_TARGET`), default `0` (root:root) at
  boot. Names/passwords/policy stay 100% userland — the kernel only knows
  uid/gid are numbers.
- **Two syscalls**: `SET_ID` (61, set the caller's uid+gid) and `GET_ID`
  (62, read any task's packed identity). The privilege rule is the whole model
  for now: **only root (uid 0) may change identity** — root can *drop* to any
  user (a login's mechanism), a non-root task can't change its uid at all
  (`SET_ID_DENIED`), so there's no escalation and `su`-back-to-root will need
  authentication (the login step).
- **Children inherit** their parent's identity at `SPAWN` (like the namespace),
  so a command runs as whoever started it; identity resets to root on task
  death alongside the other per-slot clears.
- **`/bin/id`** prints `uid=N gid=M` (numeric — no names yet). **`su`** is a new
  shell *builtin* (`su <uid>[:<gid>]`, chown-style, numeric) — a builtin because
  `SET_ID` acts on the caller, so it must change the shell's *own* identity, not
  a short-lived child's. The prompt now shows **`#` for root, `$` for a normal
  user** — the most visible sign of the primitive.

Verified on QEMU: boot prompt is `#`; `id` → `uid=0 gid=0`; `su 1000:1000` flips
the prompt to `$` and `id` (a spawned child) then reports `uid=1000 gid=1000` —
proving inheritance across spawn; `su 0` is refused ("only root may change
identity") and `id` still shows `1000` — proving no escalation.

## `chmod` / `chown`: the write side of the mode/owner surface

The `stat` surface made mode/owner *readable*; now they're *writable* too.
`chmod` and `chown` change an ext2 file's permission bits and owner — the write
twin of `ls -l`'s new columns. Read-only on every other filesystem (they can't
model it): **ext2 only**.

- Two new verbs — **`NP_CHMOD`** (`a1` = mode) and **`NP_CHOWN`** (`a1` = uid,
  `a2` = gid, `u64::MAX` = "leave unchanged", so uid-only/gid-only need no
  separate verb). `fsd`'s `Filesystem::chmod`/`chown` dispatch to the ext2 arm;
  the others return the new **`FS_ERR_NOT_SUPPORTED`** code (`ls -l`-style honest
  per-filesystem degradation, not a silent no-op).
- ext2 patches the inode field **in place** — a surgical read-modify-write of
  just the mode (offset 0) or uid/gid (offsets 2/24), *not* the existing
  `write_inode` (which zeroes the whole slot for a fresh inode and would wipe a
  pre-existing file's timestamps/flags). `chmod` preserves the `S_IFMT` type
  nibble, so it can't turn a directory into a file.
- **`/bin/chmod`** (`chmod <octal> <file>`) and **`/bin/chown`**
  (`chown <uid|uid:gid|:gid> <file>`) — numeric only (no symbolic `u+x`, no name
  lookup: there's no `/etc/passwd` yet). All arg parsing works in bytes, never
  slicing a `&str` by a runtime index (the PIE relocation trap).
- The 9P **export relays both verbs** (a new `fsd_call3` forwards `chown`'s third
  param), so a remote `chmod /mnt/a/f` works the same as local — mirroring the
  already-proven touch/mkdir/stat relays.

Verified on QEMU: `chmod 640` + `chown 7:8` on an ext2 file → `ls -l` shows
`-rw-r----- 7 8`; `chmod 700` then shows `-rwx------ 7 8` (owner preserved);
`chmod 2755` renders the setgid bit as `-rwxr-sr-x` (mode/owner each preserving
the other). The foreign observer confirms it: **e2fsck -fn is clean** after the
in-place edits, and macOS `debugfs` reads back `Mode: 0700 User: 7 Group: 8`
exactly. On FAT32, `chmod`/`chown` report "not supported by this filesystem
(mode/owner need ext2)". (Remote-over-cluster relays compile and mirror working
code but weren't two-node-booted this session — the 2vm images are FAT32.)

## `stat` mode/owner surface: `ls -l` shows permissions and owners

The per-file `stat` op (`NP_STAT`) reported size, a directory flag, and an
mtime. It now also carries **POSIX mode + owner** — the keystone the whole
users/permissions arc was waiting on (see `docs/gap-analysis.md`). Read-only:
*stored + surfaced, not enforced*, and no `chmod`/`chown` write path yet.

- The `NP_STAT` result record grew from **20 → 27 bytes**: a `u16` mode (ext2's
  `i_mode` — the `S_IFMT` type nibble + 12 permission bits), a `u16` uid, a
  `u16` gid, and a `mode_valid` byte. The triple sits *after* the original 20,
  so the record's size/flags/time fields are byte-for-byte unchanged.
- `fsd`'s `vfs::Stat` gains an `Option<FileMode>`. **ext2** fills it from the
  inode (`read_inode` now parses `i_uid`/`i_gid` alongside the mode it already
  read; created files stay root-owned, uid/gid `0`). **FAT32/exFAT/`/proc`**
  return `None` — they can't model an owner or mode.
- `ulib::stat_mode` decodes the triple (mirroring `stat_time`); `mode_valid`
  `0` → `None`.
- **`ls -l`** renders a full permission string (`drwxr-xr-x`, incl.
  setuid/setgid/sticky) plus numeric uid/gid columns. When the filesystem
  models mode/owner (ext2) they're real; otherwise `ls` synthesizes a
  conventional mode from the type and shows `-` for the owner (the way Linux
  presents a mode-less mount like vfat).
- `netd`'s export relays the record verbatim (buffers sized by `STAT_INFO_LEN`,
  so remote `ls -l` picked up the new columns for free).

Verified on QEMU: `run-image-ext2` shows `drwx------ 0 0 … lost+found/` (ext2's
`lost+found` is mode `0700`, root-owned) and real owners for host-created files;
`run-image` (FAT32) shows the synthesized `drwxr-xr-x`/`-rw-r--r--` with `-`
owners and the real mtime intact — no regression to the existing time field.

## Environment export: spawned commands inherit the shell's env

The shell's environment was **shell-local** — `set FOO=bar` and `$VAR`
expansion worked, but a spawned program couldn't read the variables. Now the
shell **exports its environment to every command it spawns**, an argv-shaped
second ABI:

- Three syscalls — **`ENV_STAGE` (58)**, **`GET_ENVC` (59)**, **`GET_ENV`
  (60)** — mirroring `ARGS_STAGE`/`GET_ARGC`/`GET_ARG`. The env blob uses the
  *same* `[count][len][…]` encoding as argv, where each entry is a
  `NAME=VALUE` string, so the kernel and child share the decoders.
- Because `SPAWN`'s four args are already full (offset/stdout/argv-len/cwd-len),
  the env has no length arg: `ENV_STAGE` **latches** the blob (`PENDING_ENV_LEN`)
  and the next `SPAWN` consumes it, attaching it to the new task's per-task env
  store (`tasks::set_env`/`env_blob`/`clear_env`, cleared on death like argv).
- The shell serializes its `Env` (`Env::serialize`) and stages it in
  `spawn_path`, threaded through `run_found_command`/`spawn_stage`/`cmd_exec`.
- `ulib` gains `env_count`/`env_at`/`getenv`; a new **`/bin/printenv`** prints
  the inherited environment (the consumer that proves the ABI). It's a normal
  pipeline stage, so `printenv | grep PATH` works.

One gotcha worth recording: `GET_ENV`'s out-buffer is validated like every user
pointer, so its capacity must be `<=` the syscall boundary's `MAX_USER_LEN`
(512) — read **one entry at a time** into a small buffer; the whole-blob
`ENV_MAX` (2048) is the store size, not a read cap. (A first attempt passed a
2048-byte buffer and `GET_ENV` silently returned "no such entry" — caught by a
`valid_user_range` debug print.)

Verified on `make run-image`: `printenv` shows `PATH=/bin`; after
`set FOO=bar; set GREETING=hello`, `printenv` shows all three; `printenv | grep
FOO` filters to `FOO=bar`. No faults.

## `/bin/sort` — the filter that can't stream

The one standard filter that had been deferred, because it breaks the shape
all the others share: it must read *every* line before it can emit one, so
it can't work from a fixed line buffer. `sort` reads all of stdin into this
program's 256KB heap (input bytes in the front), builds an in-place line
index (start+len per line) reinterpreted from the heap's 4-aligned tail, and
**heapsorts the index** (O(n log n), iterative, no scratch array — so its
*working* memory stays O(1) beyond the input it necessarily holds).

Flags (combinable, e.g. `-rn`): `-r` reverse, `-n` numeric (leading integer,
ties broken lexicographically), `-u` unique (drop lines equal to the previous
emitted one), `-f` fold case. Default is byte-lexicographic ascending. There's
**no per-line length limit** (unlike the streaming filters), and the "buffer
everything" constraint is handled with a **documented size cap**: an input
larger than the heap is truncated, sorted, and emitted, with a one-line
warning to the console (never into the sorted output).

Verified on `make run-image`: `cat f | sort` / `-r` / `-u` and `sort` vs
`sort -n` on numbers (`100 30 9` lexical → `9 30 100` numeric) all correct;
no faults. A new `/bin/SORT` crate + `man sort`.

## Shell: a builtin may appear anywhere in a pipeline

Only a pipeline's *first* stage could be a builtin (a later stage had to be a
`/bin` program that reads stdin), so `cat f | ps`, `ls | ps | grep x`, and the
like were refused. Now a **single** builtin may sit at **any** position.

The enabling observation: a builtin runs *in the shell*, not as a task, and no
builtin reads stdin — so a builtin can only ever be a pipeline's **source**,
never a stream transformer. A non-first builtin therefore means "run everything
upstream for its side effects, discard its output, and let the builtin be the
source of whatever follows." `cmd_pipeline` now classifies the stages, and when
the (single) builtin is at position `k > 0` it runs `stages[..k]` as a program
pipeline whose output is **drained** (a new no-size-limit discard path,
`drain_program_output`), then runs `stages[k..]` — the builtin as head, feeding
the program stages after it — through the same `run_head_pipeline` core the
first-stage-builtin case already used. A lone terminating builtin (`cat f | ps`)
is just run to the console (or the redirect capture, so `cat f | ps > out`
works). Two or more builtins are refused with a clear message — only one thing
in a pipeline can be the source.

The pipeline core was refactored to a `run_head_pipeline(stages, …, sink)` with
a `PipeSink` of `Console | Redirect(path,append) | Drain`, unifying the console,
`> file`, and drain destinations (the redirect path from the previous entry is
now one of the three sinks).

Verified on `make run-image`: `cat tf.txt | ps` (builtin last) prints the task
table; `ls | ps | grep runnable` (builtin middle) filters it; `ps | grep x`
(builtin first) still works; `cat tf.txt | ps > psout.txt` captures the table to
a file; `env | ps` reports "only one builtin per pipeline"; plain pipelines are
unchanged. No faults.

## Filters: `grep` flags, and a cooperative `YIELD` for prompt early-exit

Two small filter follow-ups from the open-gaps list.

**`grep` gained `-i`/`-v`/`-n`.** It was substring-only *and* case-sensitive;
now `-i` folds case (ASCII), `-v` inverts (print non-matching lines), and `-n`
prefixes each printed line with its 1-based input line number. Flags may be
separate (`-i -v`) or combined (`-in`), before the pattern. Matching is still a
plain substring — real regex is a separate, larger arc. (Verified on
`make run-image`: `grep -i cat` matched `CAT`, `grep -v P` printed exactly the
`P`-free rows, `grep -in pw` numbered the case-folded `PWD` match.)

**`head` no longer leans on the producer's send-timeout when it exits early.**
When `head N` has its N lines it exits; the upstream producer discovers this on
its next send. Two things make that prompt now:

- A send to the exited (zombie) consumer already fails *non-transiently*
  (`TASK_ERR_NO_SUCH_TASK`), so `pipe_out` returns at once — it never waits out
  its retry deadline. (This was already true; the old comment overstated it.)
- The remaining stall was the *busy-spin* while the consumer is still draining:
  a producer that fills the consumer's mailbox got `MSG_ERR_FULL` and re-sent in
  a tight loop until the next tick let the consumer run — up to a full second at
  a 1-second hardware tick, burning a CPU that could have been running the
  consumer. Fixed with a new **`YIELD` syscall** (57): on `MSG_ERR_FULL`,
  `pipe_out` yields instead of spinning, and the kernel switches to another
  runnable task — *preferring real work over the idle task* — so the consumer
  runs, drains (or exits, at which point the next send fails fast). `YIELD`
  saves the caller as still-runnable and switches like a block that doesn't
  block; `end_of_stream` yields the same way. Benefits every producer/consumer
  pair, not just `head`.

Verified on `make run-image`: `tree / | head 3` (a high-volume producer feeding
an early-exiting consumer) completes promptly with no hang and no fault; plain
pipelines are unchanged.

## Shell: `a | b > file` — pipelines compose with redirection

A pipeline and an output redirect can now be used together:
`ls /bin | grep C > matches.txt`, `ps | grep runnable >> log.txt`. Before,
this was refused (`can't combine | with output redirection`) because the
last pipeline stage wrote straight to the console, with nothing capturing
its output.

The fix reorders `run_line`: it **parses the trailing `> file`/`>> file`
redirect first**, leaving any `|` in the command part, then runs the
pipeline. When a redirect is present, the last stage's stdout is pointed at
the **shell itself** (`self_task()`) instead of `CON_TASK`, so the shell
captures its output with the same `capture_program_output` →
`finish_redirect` path a single `cmd > file` already uses (into the 256KB
heap capture, then written to the file — create/replace for `>`, append for
`>>`). Everything else about the pipeline is unchanged: stages still stream
directly to one another, a builtin first stage is still captured-and-streamed,
and the last stage just gets a different destination. No `FG` in the
redirected case (the output goes to a file, so the last stage doesn't own the
keyboard); on a capture error the producers are killed and reaped so nothing
hangs.

Verified on `make run-image`: `ls /bin | grep PW > pwout.txt` then
`>> pwout.txt` produced a two-line file (read back both by the guest and by
macOS mounting the image); `env | grep PATH > envout.txt` (a builtin-head
pipeline) captured `PATH=/bin`; plain pipelines still print to the console.
No faults.

## GPT: validate the CRC32s, fall back to the backup

`fsd`'s `partition::discover` used to *trust* a GPT: it keyed off the "EFI
PART" signature and read the partition-entry array without checking either
CRC32 the format carries. Now it **validates**, following the UEFI spec's
GPT rules, and recovers from a damaged primary:

- **Header CRC32** — over `HeaderSize` bytes with the 4-byte CRC field
  taken as zero — and **partition-entry-array CRC32** — over
  `NumberOfPartitionEntries * SizeOfPartitionEntry` bytes — are both
  recomputed and checked against the header's stored values. A table-free
  bitwise CRC-32 (reflected `0xEDB88320`, the same one `zlib.crc32`/GPT
  use) does it, folded incrementally so the array is CRC'd sector-by-sector
  with no large buffer. Bounded reads (array capped at 64 KiB) keep a
  corrupt header from running the scan away.
- **Backup fallback** — if the *primary* header or array fails validation,
  discovery reads the **backup** GPT at the last LBA (its own header +
  array) and uses that. Only if both copies fail does it report no
  partitions, rather than trusting a corrupt table.
- **Protective-MBR detection** — a GPT disk is now recognized by the
  "EFI PART" signature *or* a `0xEE` protective-MBR entry, so a disk whose
  primary header signature is itself corrupted is still recognized and
  recovered from the backup.

The clean path is unchanged: a valid primary validates and mounts, never
touching the backup (verified booting `make run-image-gpt` under QEMU — the
image `scripts/mkgpt.py` builds still mounts).

**Testing note worth keeping:** this *can't* be tested through a normal UEFI
boot — EDK2/OVMF validates the same CRCs and **auto-repairs a corrupt
primary from the backup on boot**, so by the time `fsd` reads the disk the
primary is healed (confirmed: a bit flipped in the primary header was back
to its original value on disk after one boot); and a disk with *both* copies
corrupt is rejected by the firmware (`CheckCrc32: Crc check failed` → EFI
shell), so the kernel never runs. So `fsd`'s own logic was validated with a
**host harness that runs the real `partition.rs` against a mock disk**: clean
→ mounts on primary; corrupt-primary-header and corrupt-primary-array → both
fall back to the backup; corrupt-both → zero partitions; plain-MBR → still
one partition (no regression). The CRC implementation was separately
cross-checked to produce byte-identical values to `zlib.crc32` and to the
CRCs `mkgpt.py` stored, for both the primary and backup headers.

## FAT32: long-filename (LFN) *write*

The guest can now **create** files and directories with long names, not
just read them. Before, `fsd`'s FAT32 arm only wrote 8.3 short entries
(`make_short_name`), so any name too long, with extra dots, spaces, or a
non-8.3 character was rejected with "invalid name"; the LFN *read* path
(reconstructing a long name from the entries a real formatter wrote) had
no write counterpart.

**What was added, all in `fat32.rs`:**

- **`insert_named_entry`** is the single funnel for a *named* create
  (`mkdir`/`touch`/`write_file`/`mv` all route through it). A name that
  fits 8.3 still becomes one plain short entry — **byte-for-byte the old
  behavior, uppercased, no LFN** — so nothing about existing short-name
  files changed. A name that doesn't fit gets the LFN treatment.
- **`generate_short_alias`** builds the `NAME~N` basis name every long
  name needs: a sanitized ≤8-char uppercase base + extension, with a `~1`,
  `~2`, … numeric tail incremented until the 11-byte short name is unique
  in that directory (`short_alias_exists` compares the raw 8.3 field, since
  two different long names can share an alias base). The base is trimmed so
  even `~999999` fits in 8 characters.
- **`build_name_entries`/`put_lfn_chars`** lay out the on-disk run — the
  LFN entries (highest sequence first, `0x40` on the last), each stamped
  with the alias's `lfn_checksum` and 13 UTF-16LE characters at the same
  field offsets the read path uses, then the short entry last.
- **`write_entry_run`/`place_entries`** find a *physically contiguous* run
  of that many free slots (spanning sectors/clusters via the chain,
  reusing `0xE5` slots and the `0x00` tail), extending the directory by a
  zeroed cluster if none exists, and write each affected sector once.
- **`free_entry_with_lfn`** frees a deleted entry's LFN run alongside its
  short entry, so `rm`/`rmdir`/`mv` no longer strand orphaned LFN entries
  (an LFN entry's first byte is a sequence number, not `0xE5`, so the old
  free-slot scan would never reclaim it — and `fsck` flags them). It
  matches the target by exact on-disk location, so a freshly inserted `mv`
  destination in the same directory is never mistaken for it, and frees the
  preceding run only if its checksum matches (the same orphan guard the
  read path uses).

**Validation** (the project's foreign-observer discipline): driven from
the guest shell over `make run-image` — created `longfilename1.txt` /
`longfilename2.txt` (whose aliases collide → `LONGFI~1.TXT` /
`LONGFI~2.TXT`), a long-named directory with a nested long-named file,
then exercised `rm` and `mv` of long-named entries. The **guest** read
every long name back correctly; **macOS's own FAT driver** then mounted the
guest-written image and listed all long names and file contents; and
**`fsck_msdos`** passed its directory-checking phase with no orphaned-LFN,
checksum, or duplicate-short-name errors. (Its lone FSInfo free-count
warning is pre-existing — `fsd` doesn't maintain the FSInfo hint on any
write — and advisory.) No kernel faults or guard-page overflows.

**Still open:** case-preservation for lowercase-but-8.3 names (e.g.
`File.txt` still stores/reads as `FILE.TXT`) — deliberately left as the
old behavior, since the gap this closes was specifically `>8.3` names.

## Shell: filename wildcards and tab completion

Two interactive shell improvements.

**Wildcards (globbing).** `*` and `?` in a command line now expand against the
filesystem before the command runs: `echo *.txt`, `cat /bin/L*`, `ls sub/*.log`.
`*` matches any run, `?` any single character, case-insensitively (FAT is);
dotfiles are matched only when the pattern starts with `.` (so `*` skips
`.`/`..`). A token that matches nothing is kept literal (bash-style), and
pipeline/redirect tokens have no wildcards so they pass through. Expansion
happens in `run_line` after `$VAR` expansion (skipped entirely when the line has
no wildcard). Each matching token can expand to several filenames, so it needs
commands that take several arguments — **`ls` was taught to** (below), and `echo`
already did.

**`ls` now takes any number of operands, and file operands too.** It used to
`readdir` its single argument, so a glob-expanded `ls *.txt` (which passes
*files*, not a directory) failed with "not a directory". Now each operand is
`stat`ed: file operands are listed together as a group, directory operands have
their contents listed with a `name:` header when there's more than one operand.
So `ls *.txt`, `ls -l *.log`, `ls file`, and `ls dir1 dir2` all work.

**Tab completion.** Pressing Tab completes the last word of the input as a
filename. A single match replaces the typed prefix with the full name (in the
filesystem's case) plus `/` for a directory or a space for a file; several
matches extend to their longest common prefix, and if that adds nothing, list
them and redraw the line. It handles a directory prefix (`cat sub/pre<Tab>`).

**A PIE trap worth recording:** both features were rewritten to work in **bytes**
after a link failure — slicing a `&str` by a runtime index emits `core`'s
*formatted* char-boundary panic path, which pulls in the `core::fmt` code the
`-pie` link can't resolve (`R_AARCH64_ABS64`). Byte slices use a lighter,
already-linked panic path; `from_utf8` converts back only where a `&str` is
needed. (See `manpages`/`selftest` lore on the same relocation limit.)

**Verified** in QEMU: `echo *` / `*.txt` / `/bin/L*` / `?anana.txt` expanded
correctly and `*.md` stayed literal; `cat ba<Tab>` completed to `BANANA.TXT` and
`echo ap<Tab>` listed `APPLE.TXT APRICOT.TXT` and extended to `AP`.

This shell-and-binaries branch as a whole — the keyboard-ownership arc, the
externalization scoping, and the two old traps that resurfaced (this PIE wall
and the pager's piped-read `MSG_MAX` bug) — is retrospected in
[`interactive-shell-postmortem.md`](interactive-shell-postmortem.md).

## `man` - manual pages

A `man <command>` that prints a full manual page, complementing the one-line
`-?` usage. Pages are **plain-text files** under `/man/` on disk (e.g.
`/man/ls`), read and printed by `/bin/man` (`\n`→`\r\n` for the console; pipe a
long page to `more`). The source pages live in `manpages/` and the build stages
them to `/man` on the ESP. Nearly 60 pages cover the `/bin` commands, the
filters, the net commands, and the builtins, each with the traditional
`NAME`/`SYNOPSIS`/`DESCRIPTION`/`OPTIONS`/`EXAMPLES`/`SEE ALSO` sections.

**No styling:** the framebuffer console (`cond`) understands only a couple of
ANSI sequences (clear/home) and silently drops the rest, so bold and colour
would render as nothing — the pages are plain text with UPPERCASE section
headers instead. **Verified** in QEMU: `man ls` (full page), `man partition`
(a >8-char name, read via FAT long-filename support), `man nope` (a clean "no
manual entry"), and `man` (usage).

## `-?` usage help on every command that takes arguments

A uniform convention: append `-?` to any command that takes arguments — builtin
or `/bin` — and it prints a one-line usage instead of running. `ls -?`, `grep -?`,
`mount -?`, `kill -?`, and so on.

For `/bin` programs it's `ulib::usage_if_requested(b"usage: ...")` as the first
line of `_start` (a new ulib helper that scans argv for `-?` and, if found,
prints the usage and exits) — added to every arg-taking program (`ls`, `cat`,
`mkdir`/`rmdir`/`touch`/`rm`, `cp`/`mv`, `writeat`, `tree`, `write`, `more`,
`grep`/`head`/`tail`, `ping`/`resolve`/`fetch`/`dial`/`serve`, `send`/`recv`).
For builtins it's a `builtin_usage(cmd)` table consulted in the shell's dispatch
before the command runs, covering the ones with arguments (`cd`, `bind`, `mount`,
`erase`/`partition`/`format`, `exec`, `cpu`, `kill`/`fg`/`wait`, `set`/`unset`);
option-less builtins (`help`/`exit`/`ps`/`unmount`/`shutdown`/`halt`) just run.
The argument-less filters (`wc`/`nl`/`rev`/`uniq`/`upper`) and `echo`/`args`
(which print their arguments) are deliberately left alone. **Verified** in QEMU
across both kinds, including `-?` mixed with other flags (`ls -l -?`).

## A minimal shell: `more`/`less`, `send`, `recv`, `selftest` move to `/bin`

Following the keyboard arc, the shell shed the commands that didn't need to be
built in, leaving a deliberately minimal builtin set: shell state (`cd`/`bind`/
`env`/`set`/`unset`), job control (`exec`/`ps`/`kill`/`fg`/`wait`), the
disk-management and power commands that must run with nothing mounted
(`erase`/`partition`/`format`/`shutdown`/`halt`), the mount commands, `cpu`, and
`help`/`exit`. Everything else is a `/bin` program.

- **`more`/`less` → `/bin`** — the flagship, and the payoff of the keyboard arc.
  A pager reads the keyboard while it runs, which a spawned program couldn't do
  before; now it can. `/bin/more` pages a file argument or stdin (a pipe), so
  both `more <file>` and `<cmd> | more` work. Making the pipe case work needed
  one more generalization: the shell now hands a **pipeline's last stage** the
  keyboard (`cmd_pipeline`), so an interactive consumer gets keystrokes — the
  same rule a single foreground command already gets. `less` is the same binary
  under a second name.
- **`send`/`recv` → `/bin`** — the IPC test pair; they only call a syscall.
- **`selftest` → `/bin`** — the relocating-loader regression check. Moving it out
  makes it test a *spawned* PIE binary (what actually matters), and it confirmed
  `core::fmt`/`write!` links and runs in a `/bin` program (zero relocations).

Two bugs found and fixed while moving `more`: a piped `more` read its stdin with
the full 256 KB heap slice as the buffer, but `MSG_RECV` rejects a length over
`MSG_MAX_LEN` (768) — fixed by reading one bounded chunk at a time into the heap;
and the pipeline-last-stage `FG` was needed or the piped pager never got the
keyboard. Added `ulib::heap()` (the program's heap area) and `ulib::read_char()`
for interactive `/bin` programs. **Verified** in QEMU: `more /L.TXT`, `ls -l /bin
| more` (paged, space-advanced), `send`, and `selftest` (all three checks pass).
Workspace is now 44 crates.

## Foreground programs own the keyboard — interactive `/bin` programs

The single thing that forced every interactive command (a pager, and later an
editor or a REPL) to be a shell **builtin** is gone. Keyboard input is routed to
one owner (`INPUT_OWNER`, the boot shell by default), and a spawned `/bin`
program was never made that owner — so it could never read a key. Now **the shell
hands a foreground command the keyboard at spawn** (a `FG` before the `WAIT` in
`run_found_command`), and the kernel already reverts ownership to the shell when
that command dies. So an interactive command is just an ordinary `/bin` program
that calls `read_char` — no kernel-of-the-shell required.

**Ctrl+C now terminates the foreground program** (it used to just yank the
keyboard back and leave the program running, stranded). The escape hatch
(`interrupt_key_check`) marks the foreground owner for death; `on_tick` performs
the kill at a safe point (the same teardown the `KILL` syscall uses, split on
whether the victim is the interrupted task) and the shell's `WAIT` wakes with
`TASK_KILLED_STATUS`. It catches Ctrl+C both from a program *blocked reading* (via
the wake-check keyboard poll) and from a *running compute loop* (a once-per-tick
poll of the foreground owner) — so a runaway program is still interruptible. It's
**terminate, not a signal**: nothing is delivered to the program to catch (an
editor can't yet offer "save first"); real signals, or a raw-input mode that
delivers Ctrl+C as a byte, are a later refinement.

Also new: **`ulib::read_char`** (the blocking key read) and **`/bin/READKEY`**, a
small keyboard-echo diagnostic that's the living proof of the arc — it reads keys
and prints each one, `q` quits, Ctrl+C aborts. The known caveat: a *running*
(not blocked-reading) foreground program can lose type-ahead to the once-per-tick
Ctrl+C poll — rare, since a program waiting for input is blocked, not running.

**Verified** in QEMU: `readkey` received `a`/`Z`/`5` (a `/bin` program reading the
keyboard — the whole point), `q` returned to the shell, and Ctrl+C terminated it
(`task 5 terminated`) and dropped back to the prompt. Normal commands, pipelines,
and the `more` builtin all still work. This unblocks the editor/BASIC direction:
those are now just `/bin` programs.

The design behind this — *interactivity is keyboard ownership, not a shell
property* — and the protected-server guard bug an ultrareview later caught in
`FG` are written up in
[`interactive-shell-postmortem.md`](interactive-shell-postmortem.md).

## `pwd` and `write` move out to `/bin`

Two more former builtins became real `/bin` programs, continuing the
externalization arc. `pwd` reads the cwd the shell delivers at spawn (`GET_CWD`)
and prints it; `write <file> [words...]` joins the words and writes them via
`ulib`'s bulk path. Both are **behaviour-identical** to the builtins they
replace — notably `write`, whose old builtin already word-split and rejoined with
single spaces (the "raw command line" rationale in the docs wasn't what the code
did), so nothing changed but where it lives.

These moved because they need *nothing* from the shell that a spawned program
lacks: the cwd is delivered at spawn, and file writes go through the same server
every other `/bin` file command uses. The ones that **stayed** builtin each fail
that test — `cd`/`bind` mutate the shell's *own* per-task cwd/namespace (a spawned
program would change only its own), `more`/`less` need the keyboard only the shell
owns, and `erase`/`partition`/`format` must run when nothing is mounted (exactly
when `/bin` can't be read). The workspace is now 39 crates.

**Verified** in QEMU: `pwd` at `/` printed `/`, `cd /d` then `pwd` printed `/d`
(the program correctly sees the shell's cwd), and `write hi.txt hello …` then
`cat hi.txt` round-tripped the content.

## `help` lists builtins only

`help` had drifted into listing a mix of shell builtins and `/bin` programs
(`echo`/`ls`/`cat`/`grep`/… were all shown as if builtin, a leftover from before
they were externalized). It now lists only the actual builtins — the commands
that run *inside* the shell — and points at `ls /bin` for the externalized
programs (a live list that changes as programs are added, so `help` shouldn't
try to enumerate it). No behavior change, just an honest command list.

## `more` - a pager (`less` is an alias)

A pager to read output a screen at a time: `more <file>` or `<command> | more`,
with **space** for the next screen, **Enter** for one more line, and **q** to
quit. `less` is an alias for the same forward-only pager.

It's a **builtin**, and that's forced by the design rather than chosen: a pager
must read the keyboard *while it runs*, but this kernel routes keyboard input to
exactly one owner (`INPUT_OWNER`, the boot shell at task 0), and a spawned `/bin`
program never becomes that owner. So a pager can only run inside the shell, which
already owns the keyboard. Content is captured into the shell's 256 KB heap
buffer — from a file (read in `FS_DATA_MAX` windows) or from a piped command
(`<command> | more` intercepts the trailing `| more`, dispatches the producer
with a capturing sink, then pages the buffer) — so output larger than the buffer
is refused rather than paged, and `| more` currently takes a single command, not
a multi-stage pipeline. The `--More--` prompt is erased with a `\r`-and-spaces
trick the console (`cond`) already handles.

**Verified** in QEMU (piped stdin driving the pager): `ls -l /bin | more` showed
23 lines then `--More--`, **space** revealed the rest, control returned to the
shell; `q` quit early mid-listing; `more /LIST.TXT` paged a file; and a short
`ls | more` printed one screen with no prompt.

## `ls`: columns, sort by name, and `ls -l` (size + date/time)

`ls` used to dump entries one-per-line in raw on-disk order. Now it's a real
listing: **sorted by name** (case-insensitive), **multi-column** by default (like
a terminal `ls`, directories suffixed `/`), with dotfiles (and `.`/`..`) hidden
unless **`-a`**. And **`ls -l`** is the long form — one entry per line with its
type, size, and modified `YYYY-MM-DD HH:MM`. Flags combine (`-la`).

The long form needed metadata the filesystem protocol didn't expose, so this adds
a **`stat`** verb (`NP_STAT`) — the reusable per-file primitive the gap-analysis
flagged as the keystone. Given a path, `fsd` returns a fixed 20-byte record:
size, a directory flag, and a **broken-down calendar** modified time (not an
epoch — so no filesystem's differing epoch leaks into the ABI, and the client
formats it with no date math). A `time_valid` byte says whether the time is
meaningful: **FAT32** now decodes its "write" date/time from the directory entry
(the primary/dev target), while exFAT/ext2/`/proc` return size and type with the
time unset (a follow-up). `ls -l` reads the directory (unchanged `readdir`) then
stats each entry (`ulib::fs_stat` + `stat_*` accessors), so no existing readdir
client was touched.

**Verified** in QEMU on the FAT32 disk: `ls /bin` printed sorted columns
(`ARGS  CAT  CLEAR …` down the columns, `/`-suffixed dirs), `ls -l /bin` showed
real sizes and `2026-08-27 08:54` timestamps, `-a` revealed `./`/`../`, and bare
`ls` at `/` listed `BIN/  CLUSTER.KEY  EFI/`.

## `shutdown` and `halt` - machine power control

Two new shell builtins to stop the machine: `shutdown` (alias `poweroff`) powers
it off; `halt` stops the CPU without cutting power.

Powering off is a privileged platform operation, so it goes through a new
**`POWER` syscall** (56) that runs at EL1: `POWER_OFF` issues **PSCI `SYSTEM_OFF`**
(the ARM firmware power-off call), and `POWER_HALT` masks interrupts and parks the
core in `wfi` forever. The one real subtlety is the PSCI **conduit** — whether the
firmware call is `hvc` or `smc` — which the new `power.rs` reads from **ACPI's
FADT** (`ARM_BOOT_ARCH` flags) at boot, in the same before-`exit_boot_services`
window as the MADT parse. If PSCI is absent or refuses, power-off falls back to a
halt so the machine at least stops.

They're **builtins, not `/bin` programs**, deliberately: you must be able to power
off even with no disk mounted (exactly when `/bin` can't be read), the same
reasoning as `erase`/`partition`/`format`. Not capability-gated — a single-user
machine lets its shell turn itself off.

**Verified** in QEMU: `shutdown` printed `powering off` and the VM **exited on its
own** (PSCI `SYSTEM_OFF` via the `hvc` conduit QEMU's ACPI advertises — no
fallback); `halt` printed `system halted` and a following `echo` produced nothing
(the machine really stopped — not even the timer tick resumes it).

## `tree` - a recursive directory listing in `/bin`

A new `/bin/TREE`: lists a directory recursively as an indented tree, then a
`N directories, M files` summary — the classic Unix `tree`. A standalone
`programs/fileutils/` crate over `ulib`, resolving its path against the
shell-delivered cwd like `ls`, and talking to fsd via `fs_list_dir`.

Two constraints shaped it. The framebuffer console renders **ASCII 0x20–0x7E
only**, so the branches are the ASCII forms (`|-- `, `` `-- ``, `|   `) rather
than the Unicode box-drawing glyphs. And a spawned `/bin` program gets a
**~32 KB stack**, so the recursion is **depth-capped at 16** (each frame holds a
512-byte listing plus the child's path and prefix, which the recursion borrows —
a deeper subtree just isn't descended). It skips the `.`/`..` entries FAT returns
(descending `..` would loop forever), and links with **zero relocations** (the
PIE requirement). The workspace is now 37 crates.

**Verified** in QEMU against a built nested tree:

```
/t
|-- SUB
|   `-- DEEP.TXT
|-- A.TXT
`-- B.TXT

1 directory, 3 files
```

plus `tree /efi` (the boot layout, correct `|   ` continuations and `2
directories, 10 files`) and `tree /nonexistent` (a clean `no such file or
directory`).

Entries are **sorted alphabetically** (case-insensitive, directories and files
interleaved) rather than shown in raw on-disk order: each directory's entries
are collected as offset+length records into the listing buffer (no name bytes
moved, no heap) and insertion-sorted before emitting — up to 64 entries per
directory (the sort array is live across the recursion, so it's a stack bound).
Verified with a deliberately out-of-order directory (`zebra`/`apple`/`mango`/…)
coming out `APPLE BERRY MANGO ZEBRA`, and `tree /` rendering the whole disk
sorted (`/bin` as `ARGS … WRITEAT`) with `6 directories, 44 files`.

## Fix: running a program by a relative path from `/`

A command containing a `/` was being fed through the `$PATH` search like a bare
name — each PATH directory prepended — so `bin/echo` from `/` became
`/bin/bin/echo` and failed with `unknown command: bin/echo`. It only *seemed*
to work from a subdirectory: `../bin/echo` became `/bin/../bin/echo`, which
normalizes back to `/bin/echo` by accident, regardless of the cwd.

The fix applies the standard shell rule at the top-level command dispatch (the
pipeline-stage resolver `resolve_command` already did this): a word containing a
`/` is a **pathname**, resolved against the cwd (or used as-is if absolute) and
**not** searched on `$PATH`; only a bare name searches `$PATH`. So `/bin/echo`,
`bin/echo` (from `/`), and `../bin/echo` (from a subdir) all run `/bin/echo`; a
missing pathname (`bin/nope`) still falls through to `unknown command`.
**Verified** in QEMU across all five cases.

## `ps` shows process names

`ps` printed only a slot number and state (`task 5: runnable`); now each line
carries the task's **name**. The data mostly already existed — the kernel stores
every spawned task's argv, and `argv[0]` *is* the command name — it just had no
way out: `GET_ARG` (49) only reads the *calling* task's own argv.

A new **`TASK_NAME`** syscall (54) closes that: `(task index, out ptr, out cap)`
copies task *i*'s `argv[0]` into the caller's buffer and returns its length (or 0
for a nameless slot), the exact bounds-checked copy-to-caller shape of
`GET_ARG`/`GET_CWD`, and the read-only companion to `TASK_STATE` (18) that `ps`
already probes. The one genuine gap was the **boot-loaded** tasks — the idle
task, `fsd`/`cond`/`netd`, and the init program are loaded, not `SPAWN`ed, so
their argv was empty. `tasks::set_name` synthesizes a single-argument argv blob
for them (`init` names slot 0 `shell`, 1 `idle`, and the three servers via a new
`server_name` helper); `supervisor::restart` re-applies the server name after a
crash (the teardown clears argv), so a restarted server doesn't go nameless.

Result:

```
task 0: runnable  shell
task 2: blocked (waiting)  fsd
task 3: blocked (waiting)  cond
task 4: blocked (waiting)  netd
task 5: blocked (waiting)  /bin/PONG
```

A spawned task shows the `argv[0]` it was launched with (a `/bin`-relative path
for `exec`); unused slots and reaped-pending zombies (whose argv was cleared on
exit) show no name — a zombie isn't running. **Verified** end to end in QEMU: the
boot tasks named as above, and `exec /bin/PONG` (a long-lived server) then `ps`
showed `task 5: blocked (waiting)  /bin/PONG`.

A follow-on: **`ps` now also shows a zombie's exit code.** A zombie already
carried its status (that's what `wait` collects), but `ps` only said `exited -
`wait` to collect its status`. A new **`TASK_EXIT_CODE`** syscall (55) *peeks*
that status without reaping it (unlike `wait`, which collects and frees the
slot), returning `TASK_NO_EXIT_CODE` for any non-zombie slot, so `ps` prints
`task 5: exited (code 1) - `wait` to collect`. Now you can see *why* a task is
holding its slot before deciding to `wait` on it. **Verified**: `exec /bin/CAT`
(no arg, exits 1) showed `code 1`, `exec /bin/ARGS` (exits 0) showed `code 0`,
each matching the kernel's own exit log.

## Four more `/bin` pipeline filters — `tail`, `nl`, `rev`, `uniq`

The `/bin` filter set (`upper`/`grep`/`wc`/`head`) grew four classic Unix
line filters, each a standalone `programs/textutils/` crate over `ulib` in
the exact `head`/`grep` shape (stdin = `pipe_recv`, stdout = `write_out`,
EOF out = `end_of_stream`):

- **`tail [N]`** — the last N lines (default 10), the complement of `head`.
  Unlike `head` it can't stop early (it must read to EOF to know which lines
  are last), so it keeps the newest lines in a fixed ring (`MAX_KEEP` = 64,
  a larger N capped to that) and flushes at EOF.
- **`nl`** — numbers every line, a right-aligned 6-column count + tab before
  each (the `cat -n` behavior). Streamed with a line-piece flush, so the
  number is emitted exactly once per logical line even when a long line is
  flushed in pieces.
- **`rev`** — reverses the byte order of each line, keeping the trailing
  newline at the end (in-place two-pointer swap).
- **`uniq`** — collapses runs of *adjacent* identical lines, comparing each
  completed line against the previously emitted one (Unix `uniq` semantics:
  only neighbours, so it pairs with a future `sort`; for now it de-dups
  already-adjacent repeats).

All four are bounded to a 256-byte line buffer with no heap — a longer line
is handled in pieces (`tail` truncates it), the shared fixed-buffer caveat.
Every binary links with **zero relocations** (fully position-independent, no
`R_AARCH64_ABS64` — the recurring PIE trap avoided by construction).

**Verified** end to end in QEMU (real FAT32 via `make run-image`, driven
through `-nographic` stdin against a multi-line `/TEST.TXT` fixture):
`nl` produced the numbered listing, `rev` turned `hi mom` into `mom ih`,
`uniq` collapsed 6 fruit lines to 4, `tail 2`/`head 3` gave the right ends,
and `cat TEST | uniq | wc` = `4 4 25` — confirming the whole chain. The
workspace is now 36 crates.

## `cpu` output past one message — chunked delivery to the shell

`cpu <host:port> <cmd>`'s output was capped at one IPC message (768 bytes) — the
`MSG_CALL` reply the shell got back. `cat` a file, `ls` a big dir, anything
chatty was silently truncated. Now the shell **pulls the output in chunks**, so a
command's full output (up to ~2 KB) comes back.

**The design, forced by the capability model.** netd does *not* hold `TO_SHELL`,
so it can't push a stream of messages to the shell — it can only reply via the
one-shot reply exemption. So the fix is a **pull loop**: `handle_run` collects the
run's output into a `PendingRun` buffer on `netd`'s frame (bounded by
`RUN_OUT_MAX`, which the remote's own send buffer also caps), returns the first
768-byte chunk, and the shell pulls the rest with a new `NETOP_RUN_MORE` op — an
empty reply signals end of stream. An owner check means only the task that issued
the run may pull its output; a single pending run at a time (the shell is blocked
in the run until it gets the first chunk, so there's never a nested one).

**Scope, honest:** this lifts the cap from 768 bytes to ~2 KB (the remote's
buffer), which covers realistic command output — but it's still **bounded**,
because netd collects the whole run before the shell pulls it. Truly unbounded
streaming (the remote sending as the child produces, the caller forwarding
incrementally) is a real arc, documented in `docs/roadmap-cluster.md` as a later
refinement to build if the need arises.

**Verified** end to end: a guest ran `cpu` against a host "export" that streamed
1500 bytes back; the guest printed all 33 lines (the full 1500) — past the old
768-byte cut — via the chunked pull. Zero EL0 faults, and the ~2 KB `PendingRun`
on netd's stack did *not* trip the guard page (the recurring trap). See
[`cpu-streaming-postmortem.md`](cpu-streaming-postmortem.md).

## Reply authentication — mutual auth on the export (auth tier 2, part 1)

v0.10.0 authenticated the export *request* (proving the client holds the cluster
key). This adds the other direction: the *reply* is now MAC'd too, so the server
proves *it* holds the key — **mutual authentication**. An active attacker who can
inject into the TCP stream can no longer feed a client forged data (wrong file
contents, a fake success); the client rejects any reply whose MAC doesn't verify.

**The mechanism** (free-riding on the symmetric key): the framed reply gains a
32-byte MAC prepended to its body —

```
reply:  [u32 len][mac:32][status:u64][result]
mac = HMAC-SHA256(cluster_key, request_nonce ‖ [status][result])
```

Binding to the **request nonce** (not a fresh reply nonce) does double duty for
free: both sides already hold that nonce, so no new field is needed, and it ties
the reply to its specific request (a captured reply can't be replayed against a
different one). `netd`'s `seal_reply` wraps every framed export reply
(`authenticate` now returns the nonce); the client (`handle_rmount`, and the
`/host` callback that rides it) verifies before trusting a byte, failing
`FS_ERR_AUTH` on a mismatch. No round trip, no state — the existing `hmac.rs`
applied to the other direction.

**Scope, honest:** this is **integrity/authenticity, not confidentiality** —
bytes still cross the wire in cleartext (a sniffer reads your files even with
perfect MACs). And it's **defense-in-depth under trusted-LAN**, not solving a
current threat: it's banked cheaply now, while the rest of the hardening (replay
protection, per-peer identity, transport encryption, and reply-auth for the
`cpu`-run output *stream*, which is not a framed reply) stays deferred behind an
explicit **"leaving a trusted network" trigger** on the roadmap.

**Verified** cross-implementation: the guest sealed each reply and the Python
foreign observer verified it (a correct-key `readdir`/`dial`/`serve` all pass, so
the guest's Rust `seal_reply` and the Python HMAC agree byte-for-byte); a tampered
reply — any flipped byte of data or MAC — is rejected. Zero EL0 faults. See the
tier-2 addendum in [`cluster-auth-postmortem.md`](cluster-auth-postmortem.md).

## Dial-in: /net/tcp accept — serve on another machine's network

The mirror of dial-out: a machine now **accepts inbound** TCP connections on
another machine's network presence. `serve /mnt/a/net 9000 …` announces port 9000
on **machine A's** NIC; a client that connects to A:9000 is answered by a program
running on the announcing machine. A is pure **ingress** (accept + relay bytes);
the server logic and its state live where `serve` runs. This completes the
`/net/tcp` model symmetrically (dial-out = active open, dial-in = passive open).

**The file model** (extends `/net/tcp`):
- write `announce <port>` to `/net/tcp/N/ctl` — make slot N a passive listener;
- read `/net/tcp/N/listen` → the number M of a newly accepted connection (empty
  if none pending yet — non-blocking, the client polls);
- `/net/tcp/M/data` / `/net/tcp/M/status` — the accepted connection, the same
  `DialConn` machinery as any other.

**Almost all reuse.** An accepted connection *is* a `DialConn` — its buffers,
retransmit (`pump_dials`), and inbound handling (`dial_on_segment`) are unchanged;
the relay needs no new relay code. The new bits: a `Listening` state + an
`Accepting` (passive-open) state; `dial_accept` (a fresh inbound SYN to an
announced port → allocate an accepted `DialConn`, send the SYN-ACK, mark it
`pending` for the listener); and a `listen` read that scans for a `pending`
accepted conn parented to that listener. The connection handle is still
path-encoded (`/net/tcp/M/…`), so still no fids. A `/bin/serve` program is the
consumer (accept one, respond, close).

**A real bug fixed en route:** the `Closing` state only sent the FIN, never
flushed queued send-buffer data — so a response written just before `close` would
have been stranded. `pump_dials` now flushes data in *both* `Established` and
`Closing`, sending the FIN only once the send buffer is drained.

**Scope, honest:** small fan-out (a listener + up to two concurrent accepted
connections — `MAX_DIAL=3`), TCP only, stop-and-wait. And dial-in is more
speculative on consumers than dial-out (no cluster feature demands inbound-through
-another-machine yet) — it completes the model and is a real capability, not a
waiting consumer.

**Verified** cross-implementation: the guest announced port 9000 on its NIC; a
host socket connected to it (via a hostfwd) as the external client; the guest
accepted the inbound connection (passive open), and the request/response relayed
through the export were byte-exact end to end (external client received the served
reply). Zero EL0 faults. The guard-page **stack overflow returned** (`MAX_DIAL=4`
overflowed `serve`'s 32 KB stack — the network arc's recurring trap, now the
fifth time) and was fixed by capping the fan-out. See
[`dial-in-postmortem.md`](dial-in-postmortem.md).

## Dial-out: /net/tcp connection files — use another machine's network

The last unshared cluster resource: a machine now dials TCP **out of another
machine's NIC**, exposed as Plan 9-style `/net/tcp` connection files. `dial
/mnt/a/net 93.184.216.34 80 GET / HTTP/1.0` opens the connection from **machine
A's** network — the "use another's network" half of the north star, alongside
the disk/`/proc`/`/dev/cons`/remote-exec already shared.

**The file model** (served by `netd` under `/net/tcp`, read-write):
- read `/net/tcp/clone` → a connection number `N`;
- write `connect <ip>!<port>` (or `close`) to `/net/tcp/N/ctl`;
- write/read `/net/tcp/N/data` to send/receive bytes;
- read `/net/tcp/N/status` → `Connecting`/`Established`/`Closed`.

**The design that avoids fids.** The connection handle lives in the *path* (`N`),
so each op is an ordinary path-based NP verb addressing a slot in `netd`'s
bounded `[Option<DialConn>; MAX_DIAL]` table — no protocol fids (the Phase-0
path-based-verbs design, extended). `net_op` only mutates dial state; **all the
TCP work happens in the event loop** — `pump_dials` (send the SYN while
Connecting, stream queued data, retransmit, send the FIN) and `dial_on_segment`
(complete the handshake, ACK + buffer inbound, handle the peer FIN), the same
model the server-side connections use. `connect` is asynchronous (the client
polls `status`), so `net_op` never blocks the loop. Reliability is honestly
scoped to **stop-and-wait** (one segment outstanding, no cwnd/SACK) — enough for
the small request/response transactions this targets.

**Routing is almost all reuse.** Locally (`mount -n /net`) a `/net/tcp` op
resolves to `NsTarget::NetLocal` → a direct NP verb to `NET_TASK`; through a
remote mount it resolves to `NsTarget::Remote` → the export → A's `net_op`, so
dialing out of A's NIC is just `/mnt/a/net/tcp/...`. `ulib` gained
`fs_write_inline` (an `NP_WRITE_FILE` routed through the namespace) and taught
`np_netlocal` to carry write data, since `/net` was read-only before. A
`/bin/dial` program is the consumer; `netd` grew nothing structural — a new arm
of its existing `/net` server.

**Scope, honest:** `cpu A fetch` already dials out for the run-a-program case;
`/net/tcp`'s distinct value is a *raw* connection the caller drives (no matching
program needed on A) composed as files. Deferred: inbound `listen`/`accept`
through A, UDP, and flow-control beyond a simple window.

**Verified** cross-implementation: the host drove the guest's `/net/tcp` over
the export to dial back out to a host TCP server (the foreign observer), which
saw the connection arrive from the guest's NIC and received its forwarded
request; the reply streamed back through the guest. Zero EL0 faults. A stack
overflow (the big per-conn buffers on `serve`'s guard-paged stack — the network
arc's recurring trap) was found and fixed by shrinking `MAX_DIAL`/buffers. See
[`dial-out-postmortem.md`](dial-out-postmortem.md).

## Cluster authentication: the export-hardening phase (v0.10.0)

The distributed cluster stops trusting the whole LAN. Phases 1–4 all shipped
**trusted-LAN, no auth**: any host that could reach a machine's 9P export (TCP
564) could read/write its disk (`mount -r`) and run arbitrary `/bin` programs on
it (`cpu`). Now every export request is authenticated with a **shared cluster
secret**, and an unauthenticated peer gets nothing — fail-closed.

**The scheme — a client-nonce MAC.** A framed export request now carries an auth
header in front of the NP message: `[u32 len][magic:8][nonce:16][mac:32][NP
message]`, where `mac = HMAC-SHA256(cluster_key, nonce ‖ NP-message)`. The secret
never crosses the wire, and it folds into the existing one-request-per-connection
model with **no extra round trip** (chosen over a server-nonce challenge-response
precisely to avoid taxing every `ls`/`cat` with an added RTT). The *reply* stays
unauthenticated in this first cut (the request is the capability); replay of an
observed request is out of scope (a sniffer can replay a captured frame but can't
forge a new one) — both documented, next-tier work.

**One gate, three call sites, one symmetric key.** The inbound gate is `netd`'s
`handle_9p` (`authenticate()` — verify or reply `FS_ERR_AUTH` + FIN, covering fs
verbs *and* `NP_RUN`). The outbound signer is `handle_rmount`/`handle_run`
(`frame_signed()` — a fresh `MONOTONIC_US`+IP nonce), covering `mount -r`, `cpu`,
**and** the `/host` reverse callback. Because the key is *symmetric* (cluster
membership, not per-peer identity), the bidirectional `cpu` case — where the
remote becomes a client of the caller's export — authenticates in both directions
with zero extra code.

**Config, fail-closed.** `netd` reads the secret from `\CLUSTER.KEY` on the boot
disk at startup, via its existing `fsd` client (no new config channel). No key =
export refuses all remote clients (safe for a single-machine run). A `\NOEXEC`
flag file is the one cheap safety lever: a machine may authenticate remote
*mounts* while refusing `NP_RUN` entirely (RCE ≫ disk-sharing in blast radius).

**Crypto, hand-rolled.** `programs/servers/netd/src/hmac.rs`: SHA-256 +
HMAC-SHA256, `no_std`, no heap, no crate (the FAT32/ACPI/virtio precedent),
validated against NIST SHA-256 and RFC 4231 HMAC known-answer vectors.

**Verified.** Cross-implementation against a real guest (`make run-image-9p` +
the auth-aware `scripts/np9p_client.py`, the foreign observer): a Python-signed
request with the correct key made the guest's Rust `authenticate()` serve its
real disk; a wrong key was refused; zero EL0 faults. The `.bss` no-mutable-statics
ceiling bit again (the key threads as `&Auth` through the whole event loop); a
sentinel collision (`FS_ERR_AUTH` first placed on a used `MAX-n` slot) was caught
by `rustc`'s `unreachable_patterns`; and a magic-byte transposition shared by both
Python peers was caught only by the independent Rust guest — see
[`cluster-auth-postmortem.md`](cluster-auth-postmortem.md).

## Cluster Phase 4b: namespace import — the full Plan 9 `cpu` model

The compute cluster completes: a remote command now runs on the remote's CPU but
reads **your** files, through your namespace imported at `/host`. `cpu
10.0.2.10:564 cat /host/BONLY.TXT` runs `cat` on machine A yet prints
`hello-from-B-imported` — a file that exists only on **B**, the caller — because
`/host` is B's namespace, mounted into the command's namespace on A. `ls /` on the
same command shows A's disk; `ls /host` shows B's. Data on B, computation on A, no
shared memory. **This is the Plan 9 `cpu` model, whole.**

**The import.** The `cpu` frame carries the caller's endpoint; before `SPAWN`,
the remote netd binds `/host → remote(caller)` on its own namespace (`NS_SET`),
which the child inherits at spawn. The child's `/host/…` accesses resolve to a 9P
round trip back to the caller (through the remote's own netd), reading the
*caller's* disk.

**Two problems the design predicted, both solved.** First, the cpu child now talks
to netd for *two* things — its stdout (`MSG_SEND`) and its `/host` fs access
(`NETOP_RMOUNT` `MSG_CALL`) — so netd **demuxes by the op field**: a fs request
always carries `NETOP_RMOUNT`, so it's never mistaken for output (which would
deadlock the child), while raw text output never collides with that small value.
Second — the real blocker — the caller's netd was servicing the run with a
*blocking* `tcp_get`, so it couldn't serve the child's `/host` callbacks arriving
at its own export (it dropped their frames): a mutual deadlock. Fixed by
**`tcp_run`**, a client connection that **pumps the event loop while it waits** —
each pass it accumulates the run's output, feeds every non-run frame to `on_frame`
(so the child's `/host` reads are served *during* the run), pumps the server
connections (so those replies go out), and acks the health-ping. The same "netd
must never block" rule that shaped 4a's capture, now on the caller.

**How it was proven.** Two VMs: a file written on B only, then from B `cpu
10.0.2.10:564 ls /` (A's root), `ls /host` (B's root, showing the B-only file),
and `cat /host/BONLY.TXT` (its contents) — the command on A reading B's disk
through the import. Zero `-d int` aborts on both nodes; the non-cpu paths (remote
mount/read) unregressed. Phase 4 (4a + 4b) is complete — the honest distributed
processing, done. See [`roadmap-cluster-phase4.md`](roadmap-cluster-phase4.md).

## Cluster Phase 4a: remote execution — run a program on another machine

The cluster becomes a *compute* cluster, not only a storage one. `cpu <host:port>
<command>` runs `<command>` on another machine and streams its output back —
**the program executes on the remote's CPU, using the remote's resources.** The
proof it's genuinely remote: on machine B, `cpu 10.0.2.10:564 ls /` shows
`RANHERE/`, a directory that exists only on **machine A's** disk — so the `ls` ran
on A. This is the transport half of the Plan 9 `cpu` model (step 4a; importing
the caller's namespace is 4b — see
[`roadmap-cluster-phase4.md`](roadmap-cluster-phase4.md)).

**netd is the spawner, non-blocking.** A `RUN` frame (a new `ninep_abi::NP_RUN`
verb) arrives on the export connection; netd reads the named `/bin` program off
its own disk (`read_file_chunk` + `SPAWN_STAGE`, in 512-byte chunks — the kernel's
`MAX_USER_LEN` per-syscall cap), stages argv, and `SPAWN`s it with `stdout_target
= NET_TASK`. The child runs on the remote's scheduler; **netd only relays.** Its
output arrives as ordinary messages to `NET_TASK`, which netd's event loop already
drains every wake — so the capture never blocks the supervised, health-pinged
server (unlike the shell's blocking `capture_program_output`). A message *from the
child's slot* is routed to its connection's output buffer; the child's empty
end-of-stream message `WAIT`-reaps it and releases the connection to stream the
output then FIN.

**A capability the model didn't have: a child talking back to its spawner.** The
child's output pipe (`stdout_target = NET_TASK`) needs it to *send to netd*, but
`DELEGATE` only hands out a send-cap the caller statically holds, and a `/bin`
slot holds `TO_FSD`/`TO_CON`, not `TO_NET`. Resolved minimally: `NET_TASK` now
holds `TO_NET` (a self-send bit) *solely* so it may `DELEGATE` the reply capability
to a child it spawns — which netd does right after `SPAWN`. So the remote-run
child can pipe its output back, and nothing else gains authority.

**The A side** is a `NETOP_RUN` client op + a `cpu` shell builtin: the shell hands
its netd `(endpoint, command line)`; netd frames the `NP_RUN`, does the TCP round
trip (reusing `tcp_get`), and returns the streamed output, which the shell prints.

**Scope (4a).** The command runs with the *remote's* namespace — `cpu B ls /`
lists **B's** disk — so it uses B's `/bin`, B's files. Importing the *caller's*
namespace (so the remote command reads *your* files) is **4b**. Output is bounded
to one message for now (small commands); trusted-LAN, no auth; the child is
reaped, and killable if the run is torn down.

**How it was proven.** Two VMs: `cpu 10.0.2.10:564 ls /` returns A's root
(including the A-only `RANHERE/` marker), `cpu … uptime` returns A's uptime, and a
bad command a clean error. The netd serve-loop restructure this needed (iterate
connections by index so the per-segment mailbox drain can take `&mut conns` and
route cpu output) was regression-tested against the full export matrix — remote
disk read/write, `/proc`, `/net`, `/dev/cons` all still work — with zero `-d int`
aborts on both nodes.

## Cluster: the namespace-aware export — retiring the per-server prefix hacks

A structural cleanup, not a new feature: the export gateway now serves **its own
composed namespace** through the *same* resolver a local client uses, replacing
the three per-server path-prefix special-cases (`/proc`, `/dev/cons`, `/net`) that
Phase 3 accumulated. This is the Plan 9 model stated properly — *a server exports
a namespace* — and it was the deferred step the third consumer finally justified.

**One resolver, three call sites.** The namespace-resolution logic (longest
component-aligned prefix wins; sentinels route to remote / console / net; the rest
to an fsd tree) moved into **`ninep-abi` as the single `resolve_ns`**, returning a
task-neutral `NsTarget` (so the crate needs no `syscall-abi` dependency). `ulib`
and the shell deleted their copies and now delegate to it (mapping `NsTarget` to
their concrete server/tree/endpoint), and — the point — **`netd`'s export uses it
too**, resolving each incoming request path against a small `EXPORT_NS` binding
blob (`/proc`→proc tree, `/dev/cons`→console, `/net`→netd's net-fs; everything
else the boot disk). `route_export` and its three hand-written prefix checks are
gone. A fourth exported resource is now a fourth *binding*, not a code branch.

**No behavior change — that's the point.** Every path resolves exactly as before;
the win is that client and export share one implementation instead of three that
had to be kept in agreement. Verified against the full matrix on QEMU and two VMs:
local disk / `/proc` / `/dev/cons` / `/net` all read/write as before, and remotely
`ls /mnt/a`, `cat` a file, `mkdir`+`write`+read-back (disk r/w), `cat
/mnt/a/proc/2/state`, `cat /mnt/a/net/ip`/`mac`, and `write /mnt/a/dev/cons` all
work through the shared resolver. Zero `-d int` aborts. (One incidental
observation, unrelated to the refactor: a remote read can still flake under heavy
back-to-back connection churn — the one-connection-per-op design's known
chattiness, which client-side caching is the eventual answer to; it reproduces on
neither light load nor the pre-refactor code.)

## Cluster Phase 3 (step 3): `/net` — a machine's network identity as files (Phase 3 done)

The third and last Phase 3 file server, and the first served by **`netd` itself**:
`/net`, a synthetic read-only view of the machine's network identity —
`/net/ip` (dotted-quad IPv4), `/net/mac` (colon-hex MAC). Locally `mount -n /net;
cat /net/ip`; remotely, **`cat /mnt/a/net/ip` reads machine A's address**. With it,
**Phase 3 is complete** — three resources (`/proc`, `/dev/cons`, `/net`), each a
file server, each remotely readable.

**Served by netd, both directions.** `/proc` lives in fsd and `/dev/cons` routes
to cond; `/net` is netd's *own* synthetic filesystem (netd owns the NIC, so it
knows the IP/MAC — `our_ip()` and `NET_MAC`). A shared `net_op` serves the read
verbs (readdir → `ip`/`mac`; read `/ip`/`/mac` with offset/want windowing) and is
driven from two places: the **export gateway** (`route_export` recognizes `/net`,
so a remote read is served straight from netd), and — new — netd's **local client
handler**, which now answers NP read verbs addressed to `NET_TASK` (a local
client's `/net` read is a direct NP call, not the `NETOP_RMOUNT` remote wrap).

**The local-vs-remote discriminator.** A `/net` binding and a remote mount both
resolve to `server = NET_TASK`, so they're told apart by the **endpoint**: `/net`
(local netd-fs) carries a zero endpoint, a remote mount a real one. `resolve_ns`
sets it, `is_local_net` checks it, and the fs helpers route a local `/net` read to
a new `np_netlocal` (a direct NP call to `NET_TASK`) instead of `np_remote`;
writes to `/net` are refused (read-only). Threaded through both `ulib` and the
shell. The shell's `mount -n /net` binds it (`NS_NET_TREE` sentinel).

**How it was proven.** Locally: `mount -n /net; ls /net` → `ip`/`mac`, `cat
/net/ip` → `10.0.2.15`, `cat /net/mac` → the MAC, `write /net/ip x` refused. Two
VMs: from B, `ls /mnt/a/net` → `ip`/`mac`, `cat /mnt/a/net/ip` → `10.0.2.10` (A's
MAC-derived address), `cat /mnt/a/net/mac` → `52:54:00:12:34:0a` — B reading A's
network identity over 9P/TCP. Zero `-d int` aborts on both nodes.

**Phase 3 complete → cut v0.8.0.** Scope, honestly: `/net` is the network-identity
slice (Plan 9's full `/net/tcp` connection files — using another machine's NIC to
*dial* — is a later, larger surface). And there are now **three** non-disk prefix
special-cases in the export, which is the real signal that the deferred
namespace-aware export (resolve incoming paths through a composed per-export
namespace, retiring the prefix hacks) has earned its place next. See
[`roadmap-cluster-phase3.md`](roadmap-cluster-phase3.md).

## Cluster Phase 3 (step 2): `/dev/cons` — write another machine's screen

The second Phase 3 file server, and the first that routes to a **non-fsd**
server: `/dev/cons`, the console as a writable file. Locally `echo hi >
/dev/cons` renders on the screen; across the network, **`write /mnt/a/dev/cons
…` prints on machine A's console** — remote console, the Plan 9 way.

**The routing insight.** `/proc` (step 1) lived *inside* fsd, so it reused the
disk path whole. The console is a different server (`cond`, `CON_TASK`), so
`/dev/cons` needed the namespace and the export to target it — the first time
either routed anywhere but fsd. It's done with a **console sentinel**
(`NS_CON_TREE`), mirroring the remote sentinel: locally the shell's `mount -c
/dev/cons` binds the path to it and `resolve_ns` returns `server = CON_TASK`, so
the fs write helpers (shell + `ulib`) emit the bytes with `con_write` (an
`NP_WRITE_FILE` to the console) instead of an fsd call; reads are refused (the
console is write-only). Remotely, `netd`'s export `route_export` recognizes
`/dev/cons` and `handle_9p` emits the write's inline bytes to `CON_TASK` (`netd`
already messages the console for its own logging) — so a wire write from another
machine lands on this one's screen with no client-side setup.

**Scope, honestly.** `/dev/cons` is the writable console file; `ls /dev` (a `/dev`
directory listing) isn't served, and the console isn't readable. Like `/proc`'s
prefix routing, the console sentinel is an explicit special-case, not yet a
fully namespace-aware export — but it's the *second* consumer, which is what a
future generalization would be built for.

**How it was proven.** Locally: `mount -c /dev/cons`, then `write /dev/cons
CONSOLE-WRITE-OK` and `echo redirect-to-cons > /dev/cons` both render on the
console, and `cat /dev/cons` returns an error (write-only). Between two VMs: from
B, `mount -r 10.0.2.10:564 /mnt/a` then `write /mnt/a/dev/cons
>>>HELLO-ON-A-FROM-B<<<` — and **machine A's console prints
`>>>HELLO-ON-A-FROM-B<<<`**, B writing A's screen over 9P/TCP. Zero `-d int`
aborts on both nodes. See [`roadmap-cluster-phase3.md`](roadmap-cluster-phase3.md).

## Cluster Phase 3 (step 1): `/proc` — a machine's process table as (remote) files

"Everything is a file" reaches across the network. `/proc` — a synthetic,
read-only view of the kernel's task table — is now a file tree, both locally and,
over the Phase-2 transport, **from another machine**: `ls /mnt/a/proc` on machine
B lists **machine A's** live tasks, and `cat /mnt/a/proc/2/state` reads A's
filesystem-server state. The Plan 9 idea — a resource is a file, a *remote*
resource is a *remote* file — made concrete.

**A synthetic `Filesystem` arm.** `fsd`'s `Filesystem` enum gains a fourth arm,
`Proc` (a new `proc.rs`) — the first **non-disk** arm, which proves the enum is a
real VFS and not just a format multiplexer. It holds no storage: every listing
and file is generated on demand from the ungated `TASK_STATE` syscall. Layout:
`/` → one dir per scheduler slot (`0/`…`9/`), `/<n>/state` →
`runnable`/`blocked`/`zombie`/`unused`. Read-only (every mutate method returns
`ReadOnly`). It is auto-mounted at boot into a **reserved mount-table index**
(`NS_PROC_TREE`; `MAX_MOUNTS` grew by one), so the proc tree always exists
alongside the boot disk at tree 0.

**Local access is a namespace bind.** A new shell `mount -p /proc` binds `/proc →
(NS_PROC_TREE, "/")` in the caller's namespace — exactly like `mount <n> <path>`
binds a disk partition's tree — so `ls /proc; cat /proc/0/state` work on one
machine, inherited by spawned commands like every namespace change.

**Remote access needs no bind — the export routes `/proc`.** `netd`'s export
gateway sent every incoming path to `fsd` tree 0 (the disk); it now **prefix-routes**:
a wire path under `/proc` goes to `NS_PROC_TREE` (with the `/proc` prefix stripped,
since the proc tree's root *is* the process table), everything else to the disk.
So a remote `ls /mnt/a/proc` — arriving at A as `/proc/…` — is served from A's proc
tree with zero client-side setup. (The `tree` selector is now threaded through the
export's `fsd_call`/`read_file_chunk`/`stat_size`/`list_dir` calls; the HTTP file
server passes tree 0, unchanged.) This is a deliberate, scoped prefix hack — the
export exposes exactly two things, A's disk and A's `/proc` — not yet a fully
namespace-aware export (deferred until a second synthetic tree needs it).

**How it was proven.** Locally: `mount -p /proc; ls /proc` → `0/`…`9/`, and the
states read true (`/proc/2` = fsd `runnable`, `/proc/4` = netd `blocked`, `/proc/9`
= `unused`, `/proc/0` = the shell `blocked` waiting on the foreground `cat`).
Between two VMs: from B, `mount -r 10.0.2.10:564 /mnt/a` then `ls /mnt/a/proc` lists
A's ten slots and `cat /mnt/a/proc/2/state`/`…/4/state` return A's fsd/netd states
— B reading A's live kernel table as files. Disk access (`ls /mnt/a`) still works
alongside. Zero `-d int` aborts on both nodes. Only per-slot *state* is exposed
(there is no cross-task argv/name accessor yet). Folds into the Phase 3 arc; more
resources-as-files (`/dev/cons`, `/net`) later. See
[`roadmap-cluster-phase3.md`](roadmap-cluster-phase3.md).

## Cluster Phase 2: two-node read+write — machine B writes machine A's disk

The cluster becomes a *shared disk*, not just a shared reader. Phase 1 let B read
A's filesystem over 9P/TCP; Phase 2 lets B **create and edit files on A's disk** —
`mkdir`/`write`/`cp`/`touch`/`rm`/`mv` against a remote mount all work, and A sees
the changes on its own disk. **The milestone that answers "is a shared-disk
cluster doable?" with a yes you can act on.**

**Write-side export verbs.** The export gateway (`handle_9p`) was read-only; it
now relays the mutate verbs to the local `fsd`, reusing netd's fsd-client calls.
Path-only ops (`touch`/`mkdir`/`rmdir`/`rm`) and `mv` (two inline paths) go
straight through `fsd_call`. A full write (`NP_WRITE`, ≤512 inline) is relayed as
fsd's inline `NP_WRITE_FILE` — same create/overwrite, no grant. An offset write
(`NP_WRITE_AT`) bridges the wire's inline data to fsd's **grant-based**
`NP_WRITE_AT`: netd copies the bytes into a local buffer, `GRANT_READ`s it to
fsd, and issues the call — the exact mirror of `read_file_chunk`'s `GRANT_WRITE`
read bridge, the other direction.

**Client chunks a large write.** A remote write's data rides inline (no grant
across a machine), bounded by `NP_REMOTE_CHUNK` (512), so the fs helpers loop:
`fs_write_at` splits a larger buffer into ≤512-byte `NP_WRITE_AT` round trips at
rising offsets, and `fs_write_bulk` truncates-then-streams. Every `/bin` writer
(`cp`, `writeat`, the shell `write`, `>>`) is unchanged — the chunking is
invisible above the helper, like Phase 1's inline-read fallback.

**Semantics: single-writer, clean-disconnect** (documented, not coordinated). One
writer per remote tree by convention, no distributed lock (fsd serializes all
disk access through one task, so a single writer never tears). A peer that drops
makes the next remote op **fail cleanly** — a distinct error, the mount goes
stale, nothing half-applied at the client, no hang, no corruption on the far
disk. Concurrent multi-writer coherence is explicitly out of scope.

**A robustness fix the two-VM link forced:** the client TCP `tcp_get` sent a
**single SYN with no retransmit**, so a dropped first packet on a
freshly-connected QEMU socket hub failed the whole op (seen as an intermittent
first-`ls`). Now the SYN is retransmitted up to a few times within the op, so a
dropped SYN/SYN-ACK is recovered — it helps HTTP fetch and remote reads too.

**How it was proven.** Two Ouroboros VMs on the shared L2 link
(`make run-image-2vm-a`/`-b`): from B, `mkdir /mnt/a/CL`, `write
/mnt/a/CL/NOTE.TXT …`, `cat` it back (through A), and `cp /BIN/LS /mnt/a/LSCOPY` (a
17 KB file → 34 chunked `NP_WRITE_AT` round trips). Then **A**, reading its *own*
disk, sees `CL/`, `LSCOPY`, and the note's exact text — B genuinely wrote A's
disk. Byte-exactness confirmed by a foreign observer: mounting A's disk image on
macOS, `LSCOPY` is `cmp`-identical to `/BIN/LS`. Clean-disconnect verified by
`SIGKILL`ing A mid-session — B's next op returns a clean error and B stays
responsive locally. Zero Data/Prefetch aborts on both VMs. **Phase 2 done → cut
v0.7.0.** See [`roadmap-cluster-phase2.md`](roadmap-cluster-phase2.md).

## Cluster Phase 1, step 1d: two-node integration — machine B reads machine A's disk (Phase 1 done)

**The aha, on real cross-machine wire.** Where 1a/1c were verified with one VM
and a host python peer, 1d puts **two Ouroboros VMs on a shared L2 link** (QEMU
socket networking — a virtual hub, no SLIRP, no gateway) and has one mount the
other: machine A exports its disk (netd's port-564 listener is always on), machine
B runs `mount -r 10.0.2.10:564 /mnt/a` and then `ls`/`cat`s A's real filesystem.

**Per-guest IP, derived from the MAC.** netd's `OUR_IP` was the hardcoded SLIRP
lease `10.0.2.15`; it's now `our_ip()`, deriving the last octet from the NIC's MAC
(read via `NET_MAC`). Two guests on the link get distinct MACs (`…:0a`/`…:0b`) →
distinct IPs (`.10`/`.11`) with **no new config channel**. The QEMU-default MAC
(`…:56`) maps back to `.15`, so every existing SLIRP run (ping/resolve/fetch, and
the export gateway's `hostfwd`, all of which target `.15`) is byte-for-byte
unchanged — the whole per-guest-IP change is invisible to the single-VM paths. No
mutable global was needed (userland has no `.bss`): `our_ip()` reads the MAC each
call. `next_hop` was already subnet-based (`10.0.2.0/24` on-link → direct), so two
on-link guests need no gateway; netd's existing ARP responder (Stage 4b) answers
for the derived IP.

**Testing infra** (`make run-image-2vm-a` / `run-image-2vm-b`): a QEMU
`-netdev socket,listen=`/`connect=` pair joins the two guests at L2, each with its
own disk copy (two QEMU write-locks can't share a file) and its own pcap. Run A
first (it listens). Verified end to end: from B, `mount -r 10.0.2.10:564 /mnt/a`
then `ls /mnt/a` → `BIN/ EFI/`, `cat /mnt/a/EFI/ORBS/INIT.CFG` → `\EFI\ORBS\SH.BIN`,
`ls /mnt/a/BIN` → A's whole `/bin` — **B reading A's disk, block for block, over a
protocol written from scratch.** Confirmed against the shared-link pcap (B ARPs A,
TCP to `:564`, the framed NP exchange, rotating source ports, clean FIN — no SLIRP
anywhere), and **zero Data/Prefetch aborts on both VMs** under `-d int`.

**This is the milestone that answers the years-long question with a yes.** Phase 1
is complete (1a export + 1c remote-mount client + 1d two-node); write-side
sharing is Phase 2. Ready to cut **v0.6.0**. See
[`roadmap-cluster.md`](roadmap-cluster.md) / [`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md).

## Cluster Phase 1, step 1c: the remote-mount client — a machine reads another's disk

The other half of the pivot: a machine now **remote-mounts** another's 9P export
and reads it. Where 1a made netd *serve* the filesystem over TCP, 1c makes it a
*client* — `mount -r <host:port> <path>` binds a remote endpoint into the shell's
namespace, and `ls <path>` / `cat <path>/file` then read the remote machine's
disk. **The aha: `ls`/`cat` are unchanged** — the same `/bin` programs, over the
same `ninep-abi` verbs; only the namespace resolution decides local-vs-remote.

**netd's outbound primitive.** A new `NETOP_RMOUNT(endpoint, NP-request)` client
op frames the NP message onto a TCP round trip to the endpoint's export gateway
(reusing `tcp_get`, generalized off its HTTP specifics) and returns the reply
body verbatim. A transport failure (no route, timeout, refused) replies a bare
`NO_FS` — a remote mount that can't be reached fails *cleanly* (the roadmap's
stated Phase 1 posture), never hangs or corrupts.

**A remote namespace binding.** The binding's `tree` sentinel
`NS_REMOTE_TREE` (`0xFF`) marks a remote entry whose `target` is
`[ip:4][port:2][remote-root]`. Resolution now returns a small `Resolved
{ server, tree, endpoint, len }` — a local match routes to `FSD_TASK` as before,
a remote match to `NET_TASK` via `NETOP_RMOUNT`. Threaded through **both** the
`ulib` fs layer and the shell's duplicate, plus the new `mount -r` builtin
(dotted-quad IPv4, port defaults to 564). No capability change: a `/bin` command
already holds `TO_FSD` statically *and* is delegated `TO_NET` at spawn. Bulk
reads (grant/safecopy locally) fall back to **inline** chunks over the wire
(`NP_REMOTE_CHUNK` 512, one message per round trip), so `cat` streams a remote
file exactly as it streams a local one.

**Two real bugs, both found by the packet trace, not the logs:**

- **`parse_tcp` hardwired the peer's source port to 80.** The client-side parser
  (shared with HTTP fetch) rejected the SYN-ACK from port 564/5641 outright, so
  the handshake stalled. Fixed by threading the connected port through.
- **A reused (source-port, ISN) 4-tuple collided with the peer's TIME_WAIT.**
  The remote-mount client opens many connections *back to back* (one per verb),
  and the fixed ephemeral port `0xc000` meant the second SYN landed on the peer's
  lingering socket and was silently dropped — observed as *intermittent* stalls
  (readdir would work, the next read wouldn't). Fixed with `next_src_port`, a
  per-connection rotating source port (and a derived ISN); derived from the
  microsecond clock, not a counter, since a zero-init `static` would need `.bss`
  the userland loader doesn't support.

One more relocation-class trap for the pile: `mount -r`'s host:port split first
used `&hostport[..c]` — str range-indexing inserts a UTF-8 char-boundary check
whose panic path pulls in core's formatting tables and breaks the PIE link
(`R_AARCH64_ABS64`). Byte slices + `from_utf8` instead (docs/processes.md).

**The staging reorder, recorded honestly:** the design (1a–1d) had 1a as the
outbound remote-client and 1b as the export gateway; the build did the *export*
first (labeled 1a, easier to verify with a host client), so this — the outbound
client + remote binding + `mount -r` — is the design's 1a+1c together, and the
outbound half is no longer ahead of us.

Verified with a foreign observer — a host-side python 9P *server*
(`scripts/np9p_server.py`) over `make run-image-9p-client` (the guest reaches the
host at `10.0.2.2` over SLIRP, no hostfwd needed): a guest `mount -r
10.0.2.2:5641 /mnt/a` then `ls /mnt/a` → `HELLO.TXT SUB/`, `cat
/mnt/a/SUB/NOTE.TXT` → the nested file's bytes, and `cat /mnt/a/HELLO.TXT` → a
~1960-byte file streamed correctly over four 512-byte round trips. Local `ls /` /
`cat /EFI/ORBS/INIT.CFG` unchanged (regression pass), the export gateway still
serves the host client (regression pass), and zero Data/Prefetch aborts across
the run (only the expected HVC/IRQ/SVC). One VM, host as server — no second VM
yet. Next: 1d (two-VM integration) → **v0.6.0**. See
[`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md).

## Cluster Phase 1, step 1a: the 9P export gateway — a machine serves its fs over TCP

The first step toward distributed: **netd exports the local `fsd` over TCP**, so
a client on the network can read the machine's filesystem with the same verb
set, reframed for the wire. This is the pivot the whole Phase 0 groundwork was
for — *remote is just this protocol over TCP instead of local IPC*.

**The reframing** (in `ninep-abi`): locally, bulk data moves by grant/safecopy;
over TCP there is no grant, so a verb travels as a **length-delimited frame with
data inline** — `[u32 len][verb][tree][a0..a3][payload]` request,
`[u32 len][status][result]` reply. `NP_NET_PORT` (564) and `NP_NET_MAX` are added;
no new verbs.

**netd gains a second listener.** Its inbound TCP was hardwired to the HTTP
server on port 80; now `parse_tcp_in` accepts port 564 too, `TcpConn` remembers
its `local_port` (so replies leave from the right source port — `build_tcp_srv`
threads it), and the first-data handler dispatches by port: 80 → the HTTP
`start_response`, 564 → the new **`handle_9p`**, which decodes the NP frame, runs
the verb against local `fsd` (reusing netd's existing `list_dir`/`stat_size`/
`read_file_chunk` fsd-client calls), and frames the reply back — one
request/reply per connection, like HTTP's `Connection: close`. **Read-side verbs
for now** (`readdir`/`read`/`read_file`/`read_at`); writes come later.

Verified with a foreign observer — a host-side python 9P client
(`scripts/np9p_client.py`) over a new `make run-image-9p` (SLIRP
`hostfwd tcp::5640-:564`): `readdir /` returns `BIN/ EFI/` (status 10), and
`read /EFI/ORBS/INIT.CFG` returns `\EFI\ORBS\SH.BIN` (status 16, the real size) —
the guest's disk, read over TCP from the host. The existing HTTP server still
serves (`curl :5555` → the HTML index), netd is not restarted, and zero `-d int`
aborts across repeated requests. One VM, host as client — no second VM needed
yet. Next: 1b (netd's outbound remote-client) + 1c (the remote-mount client:
`mount -r host:port /path`), then 1d (two-VM). See
[`roadmap-cluster-phase1.md`](roadmap-cluster-phase1.md).

## Cluster Phase 0, step 0e: cond on the verb set — the last bespoke protocol retired

The console server joins the uniform protocol, and `DSPOP_*` — the third and
last of the bespoke server protocols — is **deleted**. Writing the console is
now a write to the console "file": an `ninep_abi::NP_WRITE_FILE` message whose
inline data is the text. cond serves only the console, so it ignores the
`tree`/`path` fields and renders the data; every `con_write` client (the shared
`ulib` one plus the shell's, `fsd`'s, `netd`'s, and the `hello`/`pong`/`args`
demos' own copies — seven in all) now emits the verb instead of `DSPOP_WRITE`.
Console writes still address `CON_TASK` directly (no namespace resolution on the
per-character echo hot path — the concern the console-server postmortem raised);
making `/dev/cons` a namespace-mounted file is a Phase 3 concern, noted not built.

Verified on QEMU by an A/B (committed `main` on `DSPOP_WRITE` vs. this change on
`NP_WRITE_FILE`): the whole shell + `/bin` session — every character of which
flows through `con_write` → cond — is **byte-identical**, zero `-d int` aborts.
The framebuffer backend decodes the same `handle()`, so it's covered by
construction (real-hardware Parallels confirms the pixels, as ever).

**Phase 0 is complete.** The three hand-rolled protocols (`FSOP_*`, `DSPOP_*`,
`NETOP_*`) are unified onto one `ninep-abi` verb set for the servers that own
*files and the console*: `FSOP_*` is reduced to `fsd`'s disk-management control
ops, `DSPOP_*` is gone, and `NETOP_*` (the `ping`/`resolve`/`fetch` *client*
protocol) stays until `/net`-as-files lands in Phase 3. Combined with the
per-task namespace + `bind` (0c) and multi-mount (0d), the Plan 9 foundation of
the distributed vision is in place — a natural **v0.5.0**. Next: cut the release,
then Phase 1 (9P-over-TCP).

## Cluster Phase 0, step 0d: fsd multi-mount — two filesystems at once (the payoff)

**Two different filesystems mounted simultaneously at different paths** — the
concrete payoff the whole Phase 0 design pointed at, and something the old
single-`Option<Filesystem>` model physically could not express.

`fsd` grew from one mount to a **mount table** `[Option<vfs::Filesystem>;
MAX_MOUNTS]` (4), indexed by the `ninep-abi` `tree` selector: tree 0 is the boot
auto-mount, and `handle_ninep` now routes each request to `mounts[tree]` (an
unmounted tree replies `NO_FS`). A new admin op **`FSOP_MOUNT_AT`** (19) mounts a
*specific* partition (by discovery index) into the first free tree slot and
returns the tree id; `vfs::Filesystem::mount_partition(disk, index)` is the
partition-selecting sibling of `mount` (which takes the first that validates).
The shell's **`mount <partition> <path>`** calls it, then `bind`s `<path>` onto
the returned tree's root (a shared `ns_add(prefix, target, tree)` now backs both
`bind` — tree 0 — and `mount` — a fresh tree). No kernel change (0c's namespace
already carries the tree in each binding).

Verified on QEMU against the existing two-partition **`run-image-ext2`** disk
(ext2 at partition 0, the FAT32 ESP at partition 1 — so `fsd` auto-mounts ext2 at
tree 0 and the disk still UEFI-boots the FAT32 ESP): `ls /` lists the **ext2**
root (`README.TXT`/`HELLO.TXT`/`bin/`/`sub/`/`lost+found/`), then
`mount 1 /mnt/f` mounts the **FAT32 ESP** at tree 1 and `ls /mnt/f` lists *its*
root (`BIN/`/`EFI/`) — two different on-disk filesystems live at once, each read
over NP by its tree (`cat /README.TXT` from ext2 tree 0 and
`cat /mnt/f/EFI/ORBS/INIT.CFG` from FAT32 tree 1 both return their real
contents), zero `-d int` aborts. Single-mount regression (the plain image) is
byte-identical to before but for binary-size boot-log lines. **Phase 0's headline
feature is done.** Remaining: 0e (`cond` on the verbs, retiring `DSPOP_*`); then
Phase 0 complete = v0.5.0.

## Cluster Phase 0, step 0c: per-task namespaces + `bind` (the first kernel change)

The Plan 9 foundation of the distributed vision: each task composes its own
**namespace** — a table of `bind`ings mapping a path prefix to a mount subtree —
and a mount affects only the task that made it. This is the arc's **first kernel
change**, deliberately isolated from the multi-mount consumer (0d) to keep it
bisectable.

Modeled bolt-for-bolt on the existing per-task **CWD** store (`CWDS`/`GET_CWD`):
the kernel keeps a per-task `NAMESPACES: [_; NUM_TASKS]` blob of opaque bytes
(`kernel/src/tasks.rs`) and interprets nothing — userland resolves. Two
syscalls: **`NS_SET`** (52, set the caller's own namespace) and **`GET_NS`** (53,
read it). Inheritance is automatic and Plan-9-shaped: `spawn_staged` copies the
**spawning task's** namespace into the child, so every command the shell spawns
sees the shell's `bind`s. An **empty** namespace means identity-to-tree-0 (the
default), so a task that never binds behaves exactly as before — zero regression
risk.

Resolution (`resolve_ns`: longest **component-aligned** prefix wins, its target
replacing the matched prefix; `(tree, fs_path)` out) lives in userland, added to
**both** `ulib`'s fs client (for `/bin`) and the shell's own fs helpers (so
`cd`/`write` into a bound path resolve the same way `ls`/`cat` do — `cd`
validates via `fs_list_dir`, which now resolves). A `bind <new> <old>` shell
builtin appends one entry via `GET_NS`/`NS_SET`. Every binding is tree 0 in this
step (single mount), so **`fsd` is untouched** — `bind` remaps *within* the one
filesystem; multi-mount (0d) will point a prefix at a different disk.

Verified on QEMU two ways: (1) **regression** — the fs batch (`ls`/`cat`/64 KiB
read/`mkdir`/`write`/`writeat`/`cp`/`mv`/`rm`/`rmdir`) is **byte-identical** to a
pre-change baseline but for two boot artifacts (the kernel binary grew ~2.5 KB;
a print race), zero `-d int` aborts — an empty namespace really is identity; and
(2) the **`bind` feature** — `bind /mnt /EFI` then `ls /mnt` == `ls /EFI`,
`cat /mnt/ORBS/INIT.CFG` == `cat /EFI/ORBS/INIT.CFG`, and `cd /mnt; pwd` → `/mnt`
with `ls` there listing `/EFI` — proving a *spawned* `/bin` command resolves the
*inherited* namespace. Design note: the plan's spawn-time staging was replaced
with **direct `NS_SET` + automatic parent→child inheritance** once building it
showed the shell's own `cd` needs to resolve too — simpler, and no staging
buffer/pending-length. Next: **0d** — `fsd` multi-mount + a 3-partition disk =
two filesystems at once, the payoff.

## Cluster Phase 0: netd migrated, and the FSOP_* file-op arms retired

The last FSOP file-op client migrates, and the payoff: **`fsd`'s `FSOP_*`
file-op handlers are deleted** — the uniform verb set is now the *sole* file
protocol.

`netd` (the static-file HTTP server) was the one remaining client still reading
files from `fsd` over `FSOP_*` (`READ_FILE` to stat, `LIST_DIR`, `READ_BULK` to
stream). Its own `fsd` client (`fsd_call` + the manual bulk-read build) now
speaks the verbs (`NP_READ_FILE`/`NP_READDIR`/`NP_READ`, `tree = 0`).

With every client (`ulib`/`/bin`, the shell, `netd`) migrated, `fsd`'s twelve
`FSOP_*` file-op match arms (`LIST_DIR`/`READ_FILE`/`READ_AT`/`READ_BULK`/
`WRITE_FILE`/`WRITE_BULK`/`WRITE_AT`/`MKDIR`/`RMDIR`/`TOUCH`/`RM`/`MV`) were
**deleted** — ~210 lines. `handle()` now dispatches the NP range to
`handle_ninep` and keeps only the `FSOP_*` **disk-management control ops**
(mount/mount-info/unmount/erase/partition/format — `fsd`-specific, not uniform
file verbs); any stray op (or the supervisor's `SYSOP_PING`) gets a bare status
reply, which for the ping is the ack.

Verified two ways on QEMU: (1) the fs batch (shell + `/bin`, incl. a 64 KiB read)
— all outputs correct, **exactly one** "filesystem server ready" (the ping still
acks, so `fsd` is *not* supervisor-restarted after the deletion), zero `-d int`
aborts; (2) `netd` over the network (`run-image-server` + `curl`) — `GET
/BIG.TXT` returns **HTTP 200, 65536 bytes, byte-identical** to the on-disk file,
exercising `netd`'s stat + streaming reads end-to-end over NP.

The three bespoke protocols are now down to two: `FSOP_*` is **admin-only**, and
`DSPOP_*` (the console) remains until step 0e. Small cosmetic follow-up left: a
few `FSOP_*` file-op *constants* linger as `ulib`'s mkdir/rmdir/touch/rm
translation keys (`/bin` still passes them; `ulib` maps them to `NP_*`) — pointing
`/bin` at the verbs directly would let those constants be deleted too. The
load-bearing retirement (no FSOP file traffic; the handlers gone) is done.

## Cluster Phase 0: the shell migrates to the uniform verb set

Follow-on to step 0a+0b: the **shell's own filesystem helpers** now speak
`ninep-abi` too. 0a+0b moved `ulib` (every `/bin` command); the shell keeps a
*separate* set of fs helpers (it predates `ulib`), so it was still emitting
`FSOP_*` for file ops. Re-pointed all six — `fs_list_dir`/`fs_read_file`/
`fs_read_at`/`fs_write_bulk`/`fs_write_at`/`fs_write_file` — through a new shell
`np_call`, exactly as `ulib` was. The shell's **admin** ops (mount/mount-info/
unmount/erase/partition/format) keep using `fs_call`/`FSOP_*` — they are
`fsd`-specific control, not uniform file verbs.

Two verbs were added to complete the file-op cover the shell needs: `NP_READ_AT`
(windowed inline read — the exec loader's chunked `spawn_path` loop) and
`NP_WRITE_FILE` (small inline create/overwrite — the `write` builtin), each
mirroring its `FSOP_*` original into the same `vfs` call in `fsd`'s
`handle_ninep`. The verb set now fully covers `fsd`'s file ops.

Verified on QEMU with the same piped-stdin batch harness (an A/B: committed
`main` with the shell on `FSOP_*` vs. this change with it on `NP`): **byte-
identical** but for one boot-print interleaving line, zero `-d int` aborts. The
run exercises both new verbs directly — every `/bin` command's program is loaded
by the shell via `NP_READ_AT`, and `write` then `cat` round-trips through
`NP_WRITE_FILE`.

`FSOP_*` file-op arms **stay in `fsd`** for now: **`netd`** (the static-file HTTP
server) still reads files from `fsd` via `FSOP_READ_FILE`/`LIST_DIR`/`READ_BULK`.
Migrating `netd`'s `fsd` client is the next sub-step, after which the `FSOP_*`
file-op arms are deleted and only the admin ops remain.

## Cluster Phase 0, step 0a+0b: the `ninep-abi` uniform verb set, in use end-to-end

The first *build* step of the distributed-cluster arc (design:
[`roadmap-cluster-phase0.md`](roadmap-cluster-phase0.md)). The whole direction
rests on "remote is just this same protocol over TCP instead of local IPC," so
step one is to stop having three bespoke server protocols and define **one
uniform, server-agnostic verb set** — and make it the real, in-use path, not a
paper spec.

New crate **`ninep-abi`** (repo root, `#![no_std]`, consts only — the
`syscall-abi` shape): the verbs `NP_READDIR`/`NP_READ_FILE`/`NP_READ`/`NP_WRITE`/
`NP_WRITE_AT`/`NP_TOUCH`/`NP_MKDIR`/`NP_RMDIR`/`NP_RM`/`NP_MV` over a wire that
mirrors `FSOP_*` with one added field — the **`tree` selector at offset 8** (the
multi-mount key, and the Phase 1 remote-tree handle) — so the request header is
`verb`/`tree`/4 params, payload at `NP_REQ_PAYLOAD` (48). Bulk data still moves
by `grant`/`safecopy` untouched; status codes reuse `syscall-abi`'s existing set,
so a verb is byte-identical to the `FSOP_*` op it replaces. Verb numbers start at
`NP_BASE` (0x100), clear of `FSOP_*` (1..18) and `SYSOP_PING` (0xFFFF), so `fsd`
speaks both during the migration.

`fsd` gained `handle_ninep` (each arm calls the **same** `vfs` method + the same
grant/safecopy path as the `FSOP_*` arm it mirrors; `vfs` engine unchanged), and
`handle()` routes the `[NP_BASE, NP_LIMIT)` range to it. `ulib`'s fs client
(`fs_list_dir`/`fs_read_file`/`fs_read_bulk`/`fs_write_bulk`/`fs_write_at`/
`fs_op_path`/`fs_mv`, via a new `np_call`) now emits the verbs with `tree = 0` (a
single implicit mount; the per-task namespace resolves it later) — so **every
`/bin` filesystem command (`ls`/`cat`/`mkdir`/`touch`/`writeat`/`cp`/`mv`/`rm`/
`rmdir`) reaches `fsd` over the new protocol**, with **outer signatures
unchanged so no `/bin` source changed**. The shell keeps its own `FSOP_*`
helpers for now (its migration + retiring the `FSOP_*` file-op arms is the next
sub-step); `fsd` dual-speaks meanwhile. No kernel change.

Verified on QEMU with a piped-stdin batch harness driving the real shell + `/bin`
(`ls`/`cat` incl. a 64 KiB file/`mkdir`/`touch`/`write`/`writeat`/`cp`/`mv`/`rm`/
`rmdir`): the capture is **byte-identical** to a pre-change baseline except two
non-functional boot artifacts (the `fsd` binary is 8 KiB larger; its async
"mounted" line interleaves differently with the first prompt), **zero `-d int`
aborts**. Cross-protocol interop confirmed in passing — `write` (shell→`FSOP_*`)
then `cat` (`/bin`→`NP`) reads the same bytes back. Next: 0c (per-task namespace
+ `mount`/`bind` syscalls), 0d (`fsd` multi-mount — the payoff), 0e (`cond` on
the verbs, retiring `DSPOP_*`). Phase 0 complete = v0.5.0.

## Large-read fsd restart fixed: a sequential-read cursor in FAT32 (v0.4.1)

The one frontier item the real-hardware pass left open: a *large* multi-MB
`cat` (or any big sequential read — `netd` streaming a file over HTTP is the
other) got `fsd` **supervisor-restarted mid-read**, dropping the mount. Root
cause, found by reading the code rather than the (QEMU-invisible) symptom:
`fat32::read_at`'s seek walked the file's cluster chain **from its start**
every call, and `next_cluster` reads a FAT sector per step with no cache — so
a client reading a file in `SAFECOPY_MAX` (2 KiB) chunks re-walked an
ever-longer prefix each time. That's **O(n²) disk reads** over the whole file,
and a single late-offset request issuing hundreds-to-thousands of FAT reads in
*one uninterrupted `handle()` call*. On slow real hardware that lone request
runs past the supervisor's runnable-wedge threshold (`WEDGE_TICKS`, ~2.56 s)
and `fsd` is torn down and restarted mid-transfer.

This is deeper than the fix originally sketched in the roadmap ("ack the
health-ping during long reads"): that addresses only the *blocked*-wedge /
`PING_TIMEOUT` path, but the detector that actually fires here is the
*runnable*-wedge, which no ping touches. The real fix is to **bound the work
per request**: `fat32::Fs` gained a `read_cursor` that remembers where the
last `read_at` walk landed, so a forward/sequential read *resumes* from there
instead of re-walking. Each bulk request becomes O(chunk); `fsd` returns to
`msg_recv` between chunks (resetting the wedge counter and servicing the ping
promptly — the `netd` "small bursts, drain between each" pattern, reached
structurally rather than by threading a mailbox drain into the FS engine); and
the whole read is O(n). Only the chain *position* is cached — never data,
which is still read fresh every call — and it is invalidated at the single
choke point for all chain mutation (`write_fat_entry`), so a read can never
follow a stale chain (an in-place data overwrite, which leaves the chain
untouched, correctly keeps the cursor).

Verified on QEMU with a piped-stdin harness: a 64 KiB file (128 clusters, 512 B
each; 32 chunks, so the cursor resumes across a cluster boundary ~31 times)
`cat`s back with all 8192 monotonic per-line counters present, strictly
increasing, no gaps/dups/reorder; a 1 MiB `cat … | wc` reports the exact
`131072 131072 1048576`; zero `-d int` aborts. The A/B is decisive: the 1 MiB
read is **0.99 s with the cursor vs. did-not-finish-in-120 s without it** — the
runaway single-request runtime that trips the wedge on real hardware, made
visible even on QEMU's fast virtio-blk.

Deliberately **not** touched: the Mode A BOT retry (a real-hardware-only tuning
knob that can't be validated on QEMU and risks regressing the confirmed
contention fix — and cutting per-request FAT reads by orders of magnitude
already shrinks its retry exposure). Scope note: the cursor helps *sequential
read-only* workloads (`cat`, `netd` file serving — the reported bug); a large
`cp` still re-walks on the read side because its interleaved destination writes
invalidate the cursor each iteration (and `write_at` has its own start-walk),
and `exfat`/`ext2` share the analogous re-walk — all tracked as follow-ups, not
in this fix. Packaged as **v0.4.1** (an isolated fix on a released minor — no
new arc).

## xHCI keyboard ↔ USB-storage contention fixed (two bugs, one symptom) — real hardware

The real-Parallels bug where the xHCI stack couldn't serve the USB keyboard and
a USB stick at once turned out to be **two independent bugs**, each confirmed
fixed on real hardware:

- **Mode A — keyboard works, storage reads degrade to I/O errors shortly after
  mount** (boot from `.hdd`, stick attached late). `usb_msd.rs` had *no* BOT
  error recovery, so a single contention-induced bulk-endpoint stall stayed
  halted and every later command failed permanently. Added
  `xhci::reset_storage_endpoint` (Reset Endpoint + Set TR Dequeue Pointer on the
  storage bulk DCI — the same two-command shape as EP0's `recover_from_stall`,
  and *controller commands* rather than USB class requests, which Parallels'
  passthrough doesn't forward) and a bounded retry-with-recovery wrapper in
  `usb_msd::bot_command` (old body → `bot_command_once`). Software ring state is
  reset only after both commands succeed, so resetting a healthy endpoint
  no-ops without desyncing. Holds under realistic sustained reads + typing.

- **Mode B — storage works, the keyboard is never addressed** (stick present at
  boot, or booting from the stick). A boot-log capture (via a temporary
  `CNTPCT_EL0` screen-freeze, since removed) showed *only* the stick's port
  enumerated — "no boot-protocol keyboard among them". Root cause: the boot port
  scan broke its wait loop on the **first** connected port and enumerated
  immediately, so the fast SuperSpeed stick won the race and the scan finished
  before Parallels' slower synthetic keyboard settled — missed for the whole
  boot (no hot-plug). Replaced "break on first connected" with a
  **minimum-settle + debounce** scan (`SCAN_MIN_SETTLE_MS` 1500 /
  `SCAN_DEBOUNCE_MS` 400 / `SCAN_SETTLE_CAP_MS` 5000): wait at least 1.5s, then
  proceed once the connected-port set holds steady for 400ms (cap 5s). The
  keyboard, which settles within ~1s even alone, is reliably present by
  enumerate time. Confirmed: keyboard works with the stick present at boot
  **and** booting entirely from the stick (the harder case, firmware hammering
  the stick right up to handoff, then the kernel resets xHCI and re-enumerates
  both).

Lessons: an "inverse correlation across two configs" was two bugs wearing one
symptom, each needing its own fix; and QEMU hid both (its keyboard is synthetic,
its storage is virtio-blk, so they never share the xHCI bus — the contention and
the enumeration race are structurally impossible there). Verified on QEMU for
non-regression (keyboard + storage still co-enumerate, `keyboard ready`, zero
aborts) and end-to-end on real Parallels hardware. One frontier item stayed
open after this pass: a *large* multi-MB `cat` got fsd supervisor-restarted
mid-read (the long-read-vs-wedge-detector interaction) — separate from the
contention itself, which realistic reads no longer trip. **Now fixed** — see
the "Large-read fsd restart fixed" entry above (v0.4.1).

## Disk management tools, milestone 3 (step 3): `format` ext2 (mkfs) — the arc's finale

`format ext2` completes milestone 3 (and the whole disk-management arc):
`fsd` now mkfs's all three of its filesystems through the one `FSOP_FORMAT`
op. `ext2::Fs::format` is the inverse of `mount_at`, deliberately minimal but
`e2fsck`-clean:

- **Single block group, fixed 4 KiB blocks.** 4 KiB blocks put
  `s_first_data_block` at 0 (no 1 KiB boot-block special case), and one group
  keeps the layout flat — no backup superblock, and `sparse_super`/`resize`/
  `large_file` all off (the plain old-style ext2 e2fsck still fully validates).
  128-byte inodes; the `filetype` incompat feature (matching what `mount_at`
  reads). The cost is a 128 MiB cap (one group = `8 × 4096` blocks); a bigger
  partition is formatted to 128 MiB, remainder unused — multi-group mkfs is
  future work.
- **What it writes.** The superblock (all the free/count/geometry fields
  `mount_at` re-derives, a nonzero UUID, volume label `OUROBOROS`), the single
  block-group descriptor, the block + inode bitmaps (used bits *and* the
  padding bits for blocks/inodes that don't exist, which e2fsck checks), a
  zeroed inode table carrying the root (inode 2) and `lost+found` (inode 11)
  directories, and those two directories' data blocks (`.`/`..`/`lost+found`).
- **A real gotcha, caught on the first boot.** The formatter first used eight
  separate `[0u8; 4096]` structure buffers on the stack (~32 KiB), which
  overran `fsd`'s guard page — `EL0 FAULT … esr=0x9200004f` (data abort, DFSC
  permission fault level 3), the task cleanly killed and supervisor-restarted
  rather than corrupting anything. Refactored to one reused 4 KiB buffer
  (each step clears, fills, writes it), exactly the discipline the rest of the
  file already follows. The guard page earning its keep again.

Verified on QEMU end-to-end (`unmount`→`erase disk`→`partition ext2`→`format
ext2`→`mount -a`, then a `write` — `fsd` mounted and wrote a file into *its
own* mkfs output, exercising the superblock parse, root-inode read, root-dir
walk, inode + block allocation from the new bitmaps, and dirent insertion),
then on the host with the foreign checker: `e2fsck -fn` passes all five passes
clean (exit 0, "12/4032 files, 133/16128 blocks"), and `debugfs` reads back
the root (`.`/`..`/`lost+found`) plus the guest-written `GREETING.TXT` (inode
12, regular 0644, 42 bytes) byte-for-byte. No faults in the passing run. With
this, `fsd` can erase, partition, **and** format FAT32/exFAT/ext2 entirely
from within the guest — the disk-management arc is complete.

## Disk management tools, milestone 3 (step 2): `format` exFAT (mkfs)

`format exfat` now works alongside `format fat32`, through the same
`FSOP_FORMAT` op and `find_partition` targeting. `exfat::Fs::format` is the
mkfs — the inverse of `mount_at`, and a good deal more involved than FAT32's:

- **Boot regions.** The main 12-sector boot region (VBR + 8 extended boot
  sectors carrying the `0x0000AA55` ExtendedBootSignature + OEM/reserved +
  the boot checksum) plus an identical backup region. The boot checksum is the
  exFAT 32-bit rotate-add over sectors 0–10, excluding the VBR's VolumeFlags
  (bytes 106–107) and PercentInUse (byte 112).
- **Layout.** Microsoft's cluster-size table picks sectors-per-cluster; the FAT
  is sized from the cluster-count upper bound, the cluster heap is aligned to a
  cluster boundary, and the real cluster count is finalized.
- **System files** (contiguous, FAT-chained, from cluster 2): the allocation
  bitmap (with exactly the system clusters' bits set), a minimal compressed
  up-case table (ASCII `a–z` → `A–Z`, everything else identity, with its own
  32-bit checksum), and the root directory carrying the volume-label (`0x83`),
  allocation-bitmap (`0x81`, which the reader locates at mount), and up-case
  (`0x82`) entries.

Verified on QEMU end-to-end, then two ways on the host: macOS `fsck_exfat`
passes every phase — boot region, system files, up-case table, hierarchy, and
the active bitmap ("the volume OUROBOROS appears to be OK") — and macOS mounted
the volume and read back a file the guest wrote (`hi.txt` →
"exfat-from-ouroboros"). One debugging note worth recording: `fsck_exfat`
requires a real attached device (`hdiutil attach -nomount`), not a plain file —
on a plain file it bails on a block-size ioctl and *misreports the boot region
as invalid*, which briefly looked like a format bug but wasn't (the checksum
was self-consistent all along; a real device node validated clean). Zero `-d
int` aborts. Next: milestone 3's ext2 mkfs step.

## Disk management tools, milestone 3 (step 1): `format` FAT32 (mkfs)

The disk can now be *made* into a filesystem from inside the guest, closing the
loop: `erase disk` → `partition fat32` → `format fat32` → `mount -a` → `ls`. A
new `FSOP_FORMAT(fstype)` op and a shell `format [fat32]` builtin (a builtin for
the same load-from-unmounted-disk reason as `erase`/`partition`).

`format` targets the disk's first MBR partition (`find_partition`, reading LBA
0). `fat32::Fs::format` is the mkfs — the inverse of `mount_at`:

- Computes the layout with Microsoft's fatgen103 formulas: sectors-per-cluster
  from the volume-size table, then `FATSz32`. Refuses a partition too small for
  a valid FAT32 (< 65 525 clusters).
- Writes the boot sector (BPB with the `"FAT32   "` type string `mount_at`
  checks, label `OUROBOROS`) and FSInfo, plus their backup copies at reserved
  sectors 6 and 7.
- Zeroes both FATs and initializes the three reserved entries (FAT[0] media,
  FAT[1] EOC, FAT[2] EOC for the one-cluster root), then zeroes the root
  directory cluster.

Refused while a filesystem is mounted. **exFAT and ext2 formats are later
steps** (`format exfat`/`format ext2` return an "unsupported yet" error for
now).

Verified on QEMU end-to-end, then **two ways on the host**: macOS `fsck_msdos`
passes all three phases on the Ouroboros-formatted partition (and `file(1)`
decodes the BPB exactly as written — FAT32, 1000 sectors/FAT, 126 991 free
clusters, label "OUROBOROS"); and macOS *mounted* the volume and read back a
file the guest wrote onto its own fresh filesystem (`HELLO.TXT` →
"made-by-ouroboros"). One cosmetic `fsck` note — FSInfo's free-count hint goes
stale after writes — is a **pre-existing FAT32 write-engine trait** (the engine
never maintained FSInfo; the spec allows it to be stale, and `fsck` only warns),
not a format bug; a write-engine follow-up. Next: milestone 3's exFAT and ext2
mkfs steps — see `roadmap.md`.

## Disk management tools, milestone 2: `erase` + `partition` (MBR)

The guest can now prepare a blank disk itself, not just use one some other tool
partitioned. Two new raw-disk `FSOP_*` ops in `fsd` (the "filesystem server"
becoming a "storage server"), driven by two new shell builtins:

- **`erase disk`** (`FSOP_ERASE`) zeroes the disk's first 2048 sectors (1 MiB) —
  the partition table and any filesystem metadata near the start — via a
  `BLOCK_WRITE` loop. The literal `disk` argument is a guard against an
  accidental bare `erase`.
- **`partition [fat32|exfat|ext2]`** (`FSOP_PARTITION`) writes a fresh MBR with
  one primary partition spanning LBA 2048→end, tagged with the matching type
  byte (0x0C / 0x07 / 0x83; default fat32). Only LBA 0 is written; the partition
  is left unformatted for a later `format`.

Both **refuse while a filesystem is mounted** (`MOUNT_ALREADY` → "unmount
first") — which is why milestone 1's `unmount` came first.

**A real architectural constraint the plan hadn't foreseen: these must be shell
builtins, not `/bin` programs.** A `/bin` program is *read from the mounted
disk* to be loaded — but `erase`/`partition` run precisely when nothing is
mounted (you `unmount` first). The load-from-disk dependency and the
unmount-first requirement are mutually exclusive, so the tools live in the
shell, which is already resident. (The rest of the command surface stays in
`/bin`; only the disk-preparation tools that operate on an unmounted disk are
builtins.)

Verified on QEMU against a scratch copy of the boot disk: the full
`mount`→`unmount`→`erase disk`→`partition exfat` sequence, plus the
refuse-while-mounted and usage guards. The resulting MBR was read cleanly by
macOS's own `fdisk` (partition 1: type 0x07, start LBA 2048, 129024 sectors =
131072 − 2048), and a hexdump confirmed the wipe (the old FAT32 boot sector at
LBA 1 zeroed). Zero `-d int` aborts across the run. Next: milestone 3, `format`
(mkfs) per filesystem — see `roadmap.md`.

## Disk management tools, milestone 1: `mount`-info and `unmount`

The first step of the disk-management arc (managing disks from the running
system, not just using a disk some other tool prepared). `fsd` grows two new
`FSOP_*` ops, and the shell's `mount` command is repurposed Unix-style:

- **`mount` with no argument now *lists* what's mounted** (`FSOP_MOUNT_INFO`):
  the format, its partition's first sector (LBA), and the disk capacity — e.g.
  `exFAT mounted at partition LBA 2048 (disk 182272 sectors, 89 MiB)`, or
  `nothing mounted`. Each `Fs` (FAT32/exFAT/ext2) now records its
  `partition_lba`; `Filesystem` gained `name()`/`partition_lba()` accessors, and
  the whole-disk capacity comes from `BLOCK_INFO`.
- **`mount -a`** performs the old mounting action (the two-half server-then-USB
  rescan for the Parallels late-attach workflow).
- **`unmount`** (`FSOP_UNMOUNT`) drops the mounted filesystem (`fs = None`) so
  the disk can be reformatted or a different volume mounted; the kernel's block
  device is untouched, so `mount -a` re-probes and remounts it.

Foundational for the rest of the arc — you unmount before formatting, and
mount-info is how you confirm a format worked. Verified on QEMU across FAT32
(partition LBA 1) and exFAT (partition LBA 2048): the full mount-info → unmount →
"nothing mounted" → `mount -a` → remount cycle, `ls` after remount, and the
`mount xyz` usage guard, with zero `-d int` aborts. See `roadmap.md`'s "Disk
management tools" arc for milestones 2 (partition/erase) and 3 (format).

## More filesystems, step 5: ext2 read-write (the arc's finale)

The ext2 arm is now read-write, built in four staged commits mirroring the FAT
write arcs (narrowest-useful-first, one tested commit per stage). This completes
the whole "more filesystems" arc: `fsd` now reads *and writes* FAT32, exFAT, and
ext2, all through the unchanged `FSOP_*` protocol.

ext2 writing is the fiddliest of the three, because its consistency is spread
across more structures than FAT's single table:

- **Stage A - allocation + `touch`.** ext2 allocation is bitmap-based: each
  block group has a block bitmap and an inode bitmap, and every allocation keeps
  the free counts consistent in *both* the group descriptor and the superblock
  (plus `bg_used_dirs_count` for directories) - e2fsck checks all of them agree.
  `bitmap_alloc`/`alloc_block`/`alloc_inode`/`write_inode`/`insert_dirent` (the
  classic slack-split directory insert) are the machinery; `touch` exercises it
  with no data blocks.
- **Stage B - `write_file`/`write_at`.** Data-block allocation with direct +
  single/double indirect pointers (`ensure_block`); `write_file` writes new
  blocks then frees the old (write-new-before-free); `write_at` is the
  streaming/append primitive (`cp`/`>>`/`writeat`).
- **Stage C - `mkdir`/`rm`/`rmdir`.** The link-count bookkeeping ext2 needs:
  `mkdir` bumps the parent's link count (the new dir's `..`), `rmdir` decrements
  it; a freed inode gets links 0 + a deletion time.
- **Stage D - `mv`.** Re-point a dirent at the same inode then unlink the old;
  a cross-directory *directory* move also fixes the moved dir's `..` and moves
  the parent link-count contribution.

A real e2fsck finding, fixed: a freed inode's `i_dtime` must be a plausible
*timestamp*, not a small sentinel - e2fsck treats a links-0 inode whose
`i_dtime` is `< s_inodes_count` as sitting on the orphan list (with `i_dtime`
as the next-orphan pointer), so a sentinel of `1` produced a bogus "corrupted
orphan linked list". Fixed with a fixed large constant (no RTC).

Verified on QEMU at every stage (`make run-image-ext2`), zero `-d int` aborts:
create/overwrite/append, streaming `cp` of a 16 KiB binary (single-indirect
blocks), `mkdir` + writing inside it, `rm`/`rmdir` (non-empty refused), rename +
cross-directory file and directory moves - all persisted across reboots. And
validated against the reference tools throughout: **`e2fsck -fn` passes all five
passes completely clean** after every stage's churn (bitmaps, free counts, link
counts, directory connectivity all consistent), and `debugfs` reads a copied
binary back **byte-identical**. FAT32/exFAT unregressed.

**The "more filesystems" arc is complete** (steps 0-5): GPT/MBR discovery, the
VFS refactor, and FAT32 + exFAT + ext2, each read-write (FAT32/exFAT) or with
ext2 now read-write too. Deferred, as noted in `roadmap.md`: ext4 (a separate
large arc), and FAT long-filename *write*.

## More filesystems, step 4: ext2 read-only

The third filesystem arm - and the one that actually *tests* the `Filesystem`
abstraction. FAT32 and exFAT are the same shape (a partition, a FAT, a heap of
clusters, a flat list of directory entries), so a thin enum sufficed. ext2 is a
genuinely different model, and driving it through the **unchanged** `FSOP_*`
protocol - nothing above `fsd` touched - is the proof the abstraction is real,
not FAT-shaped in disguise.

New `fsd/src/ext2.rs`. What's structurally new versus FAT:

- **Inodes own the metadata; a directory entry is just `name -> inode number`.**
  Resolving a path reads the directory's inode, walks its data blocks for the
  name, gets an inode number, reads *that* inode, and repeats
  (`read_inode`/`find`). Mode, size, and block pointers live in the inode, not
  the directory entry.
- **Block groups.** A block group descriptor table (right after the superblock)
  records each group's inode table location; inode *N* is in group
  `(N-1)/inodes_per_group` at index `(N-1) % inodes_per_group`.
- **Direct + indirect block pointers.** An inode has 12 direct block pointers
  then single / double / triple indirect (pointer blocks of pointer blocks).
  `block_for` maps a file's logical block to a physical one, following one or
  two indirection levels (triple is beyond the sizes here, treated as EOF); a
  `0` pointer is a sparse hole, read as zeros.
- **Case-sensitive names** (Unix), unlike FAT/exFAT's case-fold - `find` matches
  exactly. Entries are variable-length `rec_len` records within the directory's
  data blocks.

Read-only (writes return `Error::ReadOnly`). The `FSOP_*` protocol is FAT-shaped
(no permissions, owners, or symlinks), so this presents files and directories
and ignores the Unix metadata it can't model; symlinks are reported but not
followed. Root is always inode 2.

Testing needed an ext2 disk that still boots. Same two-partition trick as exFAT
(new `scripts/mkext2.py` + `make run-image-ext2`): the **ext2 partition first**
(so `fsd` mounts it - the FAT32 *and* exFAT probes both fail, ext2 succeeds, the
enum reaching its third arm) and the **FAT32 ESP second** (UEFI boots
`BOOTAA64`). The ext2 image is built with Homebrew `e2fsprogs`' `mke2fs -d`
(block size forced to 1024 so the >12 KiB `/bin` binaries spill into
single-indirect blocks, exercising the indirection), with a **lowercase `/bin`**
- ext2 is case-sensitive and the shell probes `/bin/<command>` as typed, unlike
the FAT/exFAT images whose 8.3-heritage uppercase names only work because those
filesystems match case-insensitively.

Verified on QEMU, zero `-d int` aborts: `ls`/`cat`, subdirectories, `/bin`
running off ext2, a `cat /README.TXT | grep line | wc` pipeline (`3 11 57`),
**case-sensitivity** (`cat /CaseSensitive.txt` reads it, `/casesensitive.txt` is
"no such file or directory" - the behaviour that would differ on FAT/exFAT), and
`touch` refused "read-only filesystem". FAT32/exFAT unregressed.

Next in the arc (see `roadmap.md`): ext2 read-write - inode + block-bitmap
allocation and directory-record insertion, the highest corruption risk of the
set.

## Housekeeping: `programs/` + `build/` reorganization

Two structural cleanups once the flat top-level layout got unwieldy (done
between the exFAT and ext2 work). Purely moves — no behaviour change, all crate
*package* names unchanged.

- **All userland crates moved under `programs/`, grouped by role**:
  `programs/shell`, `programs/servers/{fsd,cond,netd}`,
  `programs/demos/{hello,pong}`, `programs/fileutils/*`, `programs/textutils/*`,
  `programs/netutils/*`, `programs/shellutils/*`. The kernel and the two shared
  libs (`ulib`, `syscall-abi`) stay at the repo root; the shared PIE linker
  script moved to `programs/linker.ld`. Done with `git mv` (history preserved).
  Because crates build by `-p <name>` and stage from `target/.../<name>`, the
  **Makefile needed no edits** — only each moved crate's `path = "../..."` deps
  deepened, the workspace `members` list, and `.cargo/config.toml`'s
  `-Tprograms/linker.ld`.
- **All generated artifacts moved under `build/`**: the `esp/` staging tree, the
  disk images (`esp.img`/`esp.hdd`/`espgpt.img`/`espexfat.img`/`espext2.img`/…),
  `net.pcap`, logs, and transient dirs — driven by one `BUILD_DIR` Makefile
  variable, so the repo root holds only source/docs/config. `.gitignore`
  collapsed to `/build/`; `make clean` is now `rm -rf build`.

Verified on QEMU (boot, `/bin` commands, filesystem ops, zero `-d int` aborts)
and later confirmed on real Parallels hardware (see the roadmap's real-hardware
pass). One carryover: a registered Parallels VM's Hard Disk must be re-pointed to
`build/esp.hdd`.

## More filesystems, step 3: exFAT read-write

The exFAT arm is now read-write, built in four staged commits mirroring FAT32's
own write arc (narrowest-useful-first, one tested commit per stage - the
scoping lever the arc calls out, since all the corruption risk lives in writes).

- **Stage A - allocation + `touch`.** The machinery, exercised by the
  lowest-risk op (touch allocates no clusters). Free clusters are tracked by an
  allocation *bitmap* (located at mount from root's `0x81` entry);
  `alloc_cluster`/`bitmap_set`/`free_data` manage it and keep it in sync with
  the FAT. Newly-created files/dirs are always FAT-*chained* (`NoFatChain = 0`),
  so allocation is the direct parallel of FAT32's `write_chain` (bitmap-set +
  FAT-link). `build_entry_set` assembles a File (`0x85`) + Stream-Extension
  (`0xC0`) + File-Name (`0xC1`) set with the two required checksums - the
  whole-set `SetChecksum` and the up-cased `NameHash` - and `create_entry`
  inserts it, growing the directory when full.
- **Stage B - `write_file`/`write_at`.** Data writing, allocation through the
  bitmap. `write_file` writes the new chain before touching the old (FAT32's
  ordering); `write_at` is the streaming/append primitive (`cp`/`>>`/`writeat`),
  a contiguous file first converted to a FAT chain so it can be extended.
  `patch_stream_ext` updates an existing set's stream extension and recomputes
  its `SetChecksum`.
- **Stage C - `mkdir`/`rm`/`rmdir`.** exFAT directories have *no* `.`/`..`
  entries (unlike FAT32), so an empty one is just a zeroed cluster. `delete_set`
  clears the in-use bit on every entry of a set (no `0xE5` tombstone).
- **Stage D - `mv`.** Re-points a new set at the same clusters (no data copy),
  preserving the `NoFatChain` layout; no `..` fixup needed. The write surface is
  complete, so the arm reports "exFAT" (not "read-only") and never returns
  `Error::ReadOnly`.

Verified on QEMU (`make run-image-exfat`) at every stage, zero `-d int` aborts:
create/overwrite/append, streaming `cp` of a 16 KB (four-cluster) binary,
`mkdir` + writing a file inside it, `rm`/`rmdir` (non-empty refused), rename +
cross-directory move + moving a whole directory - all read back correctly and
persisted across reboots. **Validated against a real driver throughout**: macOS
mounts the volume, the copied binary reads back *byte-identical* to the
original, and `fsck_exfat` reports the volume OK (active bitmap + file-system
hierarchy clean) after all the allocate/free/rename churn - proof the
`SetChecksum`, `NameHash`, bitmap, and FAT stay spec-correct. FAT32 via
`run-image` unregressed.

Next in the arc (see `roadmap.md`): ext2 read-only - the genuinely different
(inode-based) model that makes the `Filesystem` abstraction prove itself.

## More filesystems, step 2: exFAT read-only

The first real exercise of step 1's `Filesystem` enum — a *second* on-disk
format, driven through the same `FSOP_*` protocol clients already speak, so
nothing above `fsd` changed. Read-only first, the big scoping lever the arc
calls out: FAT32's own write support was phases 4–8, where every corruption
risk lived, so a new format lands read-only as one milestone and read-write as
a separate one.

New `fsd/src/exfat.rs`. exFAT is structurally a cluster filesystem like FAT (a
partition, a FAT, a heap of fixed-size clusters), so the read *machinery*
(cluster-to-LBA, chain walking, windowed `read_at`) mirrors `fat32.rs`. The
genuinely different parts, and how a read-only driver handles each:

- **Boot sector by `log2` shifts.** `BytesPerSectorShift`/`SectorsPerCluster
  Shift` instead of raw counts, and explicit `FatOffset`/`ClusterHeapOffset`
  sector counts (partition-relative — `mount_at` adds `partition_lba`, exactly
  as `partition.rs` hands it in).
- **Contiguous files skip the FAT entirely.** Each entry carries a `NoFatChain`
  flag; when set, its clusters are simply consecutive and the FAT is never
  consulted (`advance()` branches on it). This is why exFAT allocates large
  files without a long chain.
- **Directory *entry sets*, not one record.** A File entry (`0x85`, attributes
  + secondary count) then a Stream-Extension entry (`0xC0`, first cluster + data
  length + `NoFatChain`) then File-Name entries (`0xC1`, 15 UTF-16 chars each).
  `walk_dir` reassembles a set into one `DirEntry` — the exFAT analogue of
  FAT32's LFN reconstruction.
- **UTF-16 names up to 255 chars**, rendered ASCII (non-ASCII → `?`), exactly as
  `fat32.rs` renders its own long names — the whole userland here is ASCII.
- **The allocation bitmap (`0x81`) and up-case table (`0x82`) are ignored.** The
  bitmap is a *write* concern (finding free clusters); a read-only driver never
  allocates. The up-case table drives case-insensitive comparison per spec — we
  approximate with ASCII case-fold (`eq_ignore_ascii_case`, what `fat32.rs`
  already uses), correct for ASCII names.

Every write op (`write_file`/`write_at`/`mkdir`/`rmdir`/`touch`/`rm`/`mv`)
returns the new shared `Error::ReadOnly`, which `main.rs` maps to the new
`FS_ERR_READ_ONLY` ABI code, rendered by `ulib::fs_error` as "read-only
filesystem". `vfs::mount` now probes each partition **FAT32-then-exFAT** and
takes the first that validates; a new `Filesystem::name()` reports the mounted
format in `fsd`'s log.

Testing needed an exFAT disk that's still UEFI-bootable. UEFI can only boot
FAT, and `fsd` mounts the *first* partition it can, so a new two-partition disk
(new `scripts/mkexfat.py` + `make image-exfat`/`run-image-exfat`) puts the
**exFAT partition first** (so `fsd` mounts it — FAT32 probe fails, exFAT probe
succeeds, the real enum fallthrough) and the **FAT32 ESP second** (UEFI ignores
the exFAT partition it can't read and boots `BOOTAA64` from the FAT32 one). The
exFAT filesystem itself is built with macOS's `newfs_exfat` (`hdiutil` can't
make exFAT) and carries `/bin` (so the shell runs commands off exFAT) plus test
files. One block device, no slot-ordering ambiguity.

Verified on QEMU, zero `-d int` aborts: `fsd: exFAT (read-only) mounted`; `ls /`
listing subdirs and a long name; `cat` of files and of a long-named file; `cat
/README.TXT | grep line | wc` → `3 11 60` (a multi-stage pipeline reading from
exFAT); `ls /bin` (the commands themselves loaded from exFAT); `touch`/`mkdir`
refused with "read-only filesystem"; `cat /nope.txt` → "no such file or
directory". FAT32 via `run-image` unregressed (mounts as "FAT32", `touch`/`rm`
still work).

Next in the arc (see `roadmap.md`): exFAT read-write (bitmap allocation,
directory-entry-set writes), then ext2 read-only — the real VFS test, a
genuinely different (inode-based) model.

## More filesystems, step 0: GPT + multi-partition discovery

The prerequisite the rest of the arc needs: real disks (anything macOS/Linux
formats, especially large ones) use GPT, not MBR, and `fsd` read only the first
FAT32 *MBR* partition. Partition discovery is now its own concern, above any
one filesystem.

- New `fsd/src/partition.rs`: `discover(disk, out)` enumerates a disk's
  partition start LBAs — **GPT or MBR**. It detects GPT by the "EFI PART"
  signature at LBA 1 (a GPT disk also carries a protective MBR), parses the
  header (entry-array LBA, count, size) and the entry array (skipping zero-GUID
  entries); otherwise it reads the classic MBR table (skipping empty and the
  protective 0xEE entry). Bounded, fixed-buffer, no `alloc`.
- `fat32::Fs::mount` became `mount_at(disk, partition_lba)` — the BPB-reading
  half, no partition scan; it returns `NotFat32` if the sector isn't a FAT32
  BPB, so a caller can try the next partition. The MBR-scan and the
  FAT32-type-byte filter are gone.
- `vfs::Filesystem::mount` now discovers partitions and tries `mount_at` at each
  (first FAT32 wins), so it mounts a FAT32 partition wherever it sits — first or
  not, MBR or GPT — and a partition that isn't FAT32 is simply skipped (the hook
  a future exFAT/ext2 probe slots into).

Testing needed a GPT disk, which macOS has no tooling for (no `sgdisk`/`gdisk`;
`hdiutil` builds MBR). New `scripts/mkgpt.py` (+ `make image-gpt`/
`run-image-gpt`) wraps `esp.img`'s FAT32 partition in a **bootable** GPT
container — a protective MBR, primary + backup GPT headers with correct CRC32s
(UEFI validates them to boot), and an EFI-System-Partition entry at LBA 2048.

Verified on QEMU, zero `-d int` aborts, two ways: (1) `run-image` (the MBR
path) still mounts and the whole FS surface works — no regression; (2)
`run-image-gpt` boots from the GPT disk (capacity 133152 sectors, the GPT
container) and `fsd` mounts the FAT32 partition **via the GPT parser** — the
disk has no real MBR table, only the protective entry, so the GPT path is the
only way it was found. `ls /`, `cat /EFI/ORBS/INIT.CFG`, `ls /bin | wc`
(`22 22 110`), `mkdir`/`write`/`cat` all worked on the GPT-mounted partition.

## More filesystems, step 1: the VFS refactor (a `Filesystem` enum inside `fsd`)

First step of the "more filesystems" arc, and a pure refactor — no behaviour
change, proven byte-identical before any second filesystem exists (the
"refactor first, prove no change" pattern `block.rs`'s `BlockDevice` enum
already followed).

`fsd` *was* FAT32: the type was hardcoded (`fsd/src/fat32.rs`'s `Fs`), and
`main.rs` called it directly. The client-facing VFS already existed — clients
speak `FSOP_*` over IPC and never know the format — so what was missing was
*internal* multiplexing. New `fsd/src/vfs.rs` adds a `Filesystem` **enum**
(`Fat32` the only arm today) whose per-op methods (`list_dir`/`read_file`/
`read_at`/`write_file`/`write_at`/`mkdir`/`rmdir`/`touch`/`rm`/`mv`) forward to
the arm, plus a `mount` that detects the format (just FAT32 for now). An enum,
not `dyn Trait`, because `fsd` is `no_std` with no heap — the same reason
`block::BlockDevice` and `console::Console` are enums. `main.rs` now holds an
`Option<vfs::Filesystem>` and calls it exactly as it called `fat32::Fs`; a
second filesystem (exFAT, ext2) is a new arm plus a branch in `mount`.

Verified on QEMU (run-image), zero `-d int` aborts, the whole FS surface
byte-identical through the new dispatch: `ls`, `mkdir`/`cd`/`write`/`cat`,
`cp`, `mv`, `rm`, `rmdir` (correctly refusing a non-empty dir), and a
pipeline reading a file (`cat h.txt | grep works | wc` → `0 3 18`).

Next in the arc (see `roadmap.md`): GPT + multi-partition (so a second
filesystem on a real disk is reachable), then exFAT read-only, then ext2
read-only (the real test of the abstraction).

## Multi-stage pipelines, step 6: real `/bin` filters (`grep`/`wc`/`head`) — arc complete

The payoff, and the last step of the pipeline arc: three real filter programs so
pipelines are genuinely useful, not just a mechanism demo. Each is a small
`ulib` filter following the `upper` shape (stdin via a new `ulib::pipe_recv`,
output to the stdout target so it chains, end-of-stream + exit on EOF).

- `wc` — counts lines/words/bytes of its stdin (byte-streaming, no line buffer)
  and prints `<lines> <words> <bytes>`.
- `grep <pattern>` — prints the stdin lines containing `pattern` (a plain
  substring, hand-rolled `contains`, no regex); line-buffered, since stdin
  arrives in arbitrary chunks.
- `head [N]` — prints the first `N` lines (default 10), then signals
  end-of-stream and exits early (the upstream producer's next send fails
  harmlessly via `pipe_out`'s bounded retry).
- `ulib::pipe_recv` factors out the filter's stdin read (the read half of the
  shape whose write half is `write_out`).

Verified on QEMU (run-image), zero `-d int` aborts: `echo one two three | wc` →
`1 3 15`; `ls /bin | wc` → `22 22 110`; `ls /bin | head 3` → the first three
entries; `ls /bin | grep CAT` → `CAT`; **`ls /bin | grep C | wc` → `7 7 33`**
(three stages: list → filter → count); **`cat /g.txt | grep find | upper` →
`FINDME IN HERE`** (list → filter → transform); `grep nomatch` → nothing.

**The multi-stage-pipeline arc is complete:** a chainable filter shape, N-stage
parsing, argv and PATH on every stage, per-link delegation plumbing, and a real
set of filters. The classic `cat FILE | grep x | wc` works.

## Multi-stage pipelines: N-stage `a | b | c` with argv and PATH

Steps 2–5 of the pipeline arc, together — the two-stage pipe becomes a real
N-stage pipeline of standalone programs. `echo hello | upper | upper` and
`cat FILE | upper` now work: bare names, arguments on any stage, and a chain as
long as the spawnable slots allow (five).

- **N-stage parsing** (`split_pipeline`): the line splits on every standalone
  `|` token into up to `MAX_STAGES` (8) trimmed stages, not just the first.
- **argv on stages** (`spawn_stage`): each stage is tokenized into its own argv
  and spawned with it — the old "programs take no arguments in a pipe"
  restriction is gone, so a producer like `cat FILE | …` carries its file.
- **PATH resolution in pipes** (`resolve_command`): a stage's command resolves
  the same way a bare command does — `$PATH` for a bare name (the
  `run_path_command` probe, factored out), used as-is for a `/`-path. So
  `echo … | upper` works with bare names, not just explicit `/EFI/ORBS/…`.
- **N-stage plumbing** (`cmd_pipeline`, rewritten): program stages are spawned
  right-to-left (each producer gets a live consumer to aim stdout at), each
  adjacent producer→consumer link is authorized with one `DELEGATE`, and the
  last stage writes to the console. A linear chain needs only the existing
  one-target-per-task delegation — each stage delegates to exactly one
  successor — so no general/transitive delegation was required. The first stage
  may still be a **builtin** (captured and streamed to stage 2, the shell the
  byte path for that one hop, via `is_builtin`); a later stage must be a program
  (it reads stdin), and a non-program there reports "not found (a pipeline stage
  must be a program)".
- `upper` is now staged to `/bin/UPPER` too (it's a real command now), so it
  resolves as a bare `upper`.

The old two-path patchwork (`parse_pipe`/`PipeParse`/`cmd_pipeline_prog`, the
`/`-prefix program-vs-builtin heuristic, single-`|`-only) is gone. Combining a
pipe with `>`/`>>` is still refused (the last stage writes straight to the
console — there's no capture of its output to redirect).

Verified on QEMU (run-image), zero `-d int` aborts: `echo hello world | upper`
→ "HELLO WORLD"; `echo chained pipe | upper | upper` → "CHAINED PIPE" (three
tasks, the middle `upper` a genuine middle stage, EOF propagating down the
chain); `cat /pt.txt | upper` → the file's contents uppercased (argv producer);
`cat /pt.txt | upper | upper` likewise over three stages; `echo x | nosuchprog`
and `echo x | pwd` → the not-a-program error; `pwd | upper` → "/" (builtin
head). Only more `/bin` filters (`grep`/`wc`/`head`/`sort`) remain to make
pipelines broadly useful — see `roadmap.md`.

## Multi-stage pipelines, step 1: a chainable filter shape (`upper` over `ulib`)

First step of the multi-stage-pipeline arc (`a | b | c`), chosen after the
standalone-binaries arc completed. The foundational piece every future pipeline
stage depends on: a filter that can sit in the *middle* of a pipe, not only at
the end.

`upper` (the reference filter) was rewritten over `ulib`: it still reads stdin
via `MSG_RECV` and uppercases each byte, but it now writes to its **stdout
target** (`ulib::write_out`) instead of a hardcoded console, and propagates
end-of-stream downstream (`ulib::end_of_stream`) before exiting. When it's the
last stage its target is the console (unchanged behaviour); when it's a middle
stage its target is the next program, so `… | upper | …` will chain. This is
the shape every future filter (`grep`/`wc`/`head`/`sort`) copies. Dropping its
hand-rolled `con_write`/`syscall4`/panic handler for `ulib`'s shrank it by half.

Verified on QEMU (run-image), zero `-d int` aborts, no regression to the
existing two-stage pipes: `echo hello world | /EFI/ORBS/UPPER.BIN` → "HELLO
WORLD" (builtin-captured left), and `cat /pt.txt | /EFI/ORBS/UPPER.BIN` →
"PIPED THROUGH A FILTER" (program left, live relay).

The rest of the arc is staged in `roadmap.md`: N-stage parsing, argv on
pipeline stages (so `cat FILE | …` works), PATH resolution in pipes, the
N-stage spawn/delegate/wait plumbing (a linear chain needs only the existing
one-target-per-task delegation), then real `/bin` filters as the payoff.

## More spawnable task slots: `NUM_TASKS` 7 → 10 (five concurrent spawned tasks)

Standalone-binaries Stage 0, done after the rest of the arc rather than before
it (the fs/net command externalization got by on two spawnable slots because
each foreground command is waited-and-reaped before the next). Raising the
ceiling gives real headroom for concurrency: a foreground command + a
background task + a multi-stage pipeline.

- `NUM_TASKS` 7 → 10; `FIRST_SPAWNABLE` stays 5, so slots 5–9 are spawnable —
  five, up from two. Slots 0–4 remain the fixed roles (shell, idle, fsd, cond,
  netd).
- The per-task fixed arrays in `tasks.rs` (`TASKS`/`REGIONS`/`MAILBOXES`/
  `ARGVS`/`CWDS`/`GRANTS`) and the L0/L1/L2/L3 table pools in `mmu.rs` were
  converted from explicit N-element literals to `[const { … }; N]`, so they now
  auto-scale from the one constant (the wrapper types aren't `Copy`, hence the
  inline-const form). `mmu.rs`'s `MAX_EL0_REGIONS` must stay equal to
  `NUM_TASKS` (one table view per task); the boot EL0-regions array in `main.rs`
  is now built programmatically instead of a fixed literal. `STATES` stays a
  literal (its boot values aren't uniform), extended by the three new slots.
- The shell's send-mask to its spawnable children became a computed
  `TO_SPAWNABLE` (bits `FIRST_SPAWNABLE..NUM_TASKS`) instead of the hardcoded
  `TO_SPAWN_5 | TO_SPAWN_6`, so it widens automatically.

**One real gotcha, caught before it shipped:** the capabilities `u32` packs the
IPC send-mask in the low `NUM_TASKS` bits and the resource caps (`CAP_BLOCK`/
`CAP_CON`/`CAP_NET`) at bits 8/9/10. At `NUM_TASKS=7` the send-mask stopped at
bit 6 — no overlap — but at `NUM_TASKS=10` it reaches bit 9, which would have
aliased `CAP_CON` (and bit 8 `CAP_BLOCK`): a spawnable slot's send-mask bit
would have read as a device capability. The resource caps moved to bits 16/17/18,
clear of the send-mask for any `NUM_TASKS` up to 16.

Verified on QEMU (run-image), zero `-d int` aborts: `ps` shows ten slots; five
`pong` instances `exec`'d concurrently into slots 5–9 (all succeed — was two
max before); the sixth refused with "no free task slot"; a `builtin | /prog`
pipeline still works; `kill`/reap frees slots correctly. The boot log's EL0
region list now carries ten entries.

## Standalone binaries, Stage 4 (netd increment): `ping`/`resolve`/`fetch` externalized via capability delegation

Eighth step of the arc, and the first commands to reach a server a spawnable
slot can't statically talk to. A spawned task gets `TO_SHELL | TO_FSD | TO_CON`
(`tasks.rs::caps_for_slot`) — **not `TO_NET`** — so a spawned network client is
`MSG_ERR_DENIED`. The fix is runtime capability delegation, not a policy
widening: the shell holds `TO_NET` statically, so when it spawns a `/bin`
command it `DELEGATE`s `TO_NET` to the child (`delegate_net` in
`run_path_command`), exactly the mechanism the program-to-program pipe already
uses to authorize a producer→consumer send. The static policy stays tight; the
grant is per-task and cleared on death.

- `ulib::net_call(req, reply)` — one `MSG_CALL` to the network server, returning
  the packed result. It retries briefly on `MSG_ERR_DENIED` (a tick can let the
  child run in the window before the shell's delegation lands — the same bounded
  wait `pipe_out` uses), so the delegation race never surfaces as a failure.
- `ping <a.b.c.d>` (its own `parse_ipv4`), `resolve <hostname>` (DNS-over-UDP,
  IP formatted with `emit_dec`), and `fetch <hostname>` (HTTP GET, the response
  streamed to stdout with a truncation note) are new `/bin` programs over
  `ulib`. Each routes its success output to the stdout target (so `ping x >
  file` / `resolve x > file` capture) and errors to the console — the stderr
  split, with an end-of-stream on every exit so a capturing shell is never
  stranded.
- The three shell builtins (`cmd_ping`/`cmd_resolve`/`cmd_fetch`) and the
  now-unused `parse_ipv4` were removed; a pre-existing orphaned `fs_call` doc
  comment (stranded above `cmd_ping`) was reattached to `fs_call`.

Verified on QEMU (a real-FAT32 image + virtio-net + SLIRP), zero `-d int`
aborts: `ping 10.0.2.2` → "reply from 10.0.2.2" (a spawned-slot program reached
netd only because the shell delegated `TO_NET`); `resolve example.com` →
a real address; `resolve nonexistent.invalid` → "could not resolve" (exit 1);
`fetch example.com` → the full HTTP response with headers and body plus the
"… bytes total" truncation note; `resolve example.com > /res.txt` then `cat`
showed the captured line.

The three deliberately-builtin groups remain (see `roadmap.md`): `ps`/`kill`/
`wait`/`fg` (job control), and `mount`/`selftest`/`help` (shell-coupled). With
the network commands out, the externalization arc is effectively complete.

## Standalone binaries, Stage 4 (bulk-data increment): `cp`/`mv`/`writeat` externalized

Seventh step of the arc, and the last filesystem commands to leave the shell.
These are the involved ones — two operands and, for `cp`/`writeat`, bulk data
over grant/safecopy — but with `ulib`'s fs client layer already carrying the
read/write primitives, each is a faithful port of its old builtin.

- `ulib` gained the remaining fs helpers: `fs_read_file` (inline read / the
  cheapest existence-and-kind probe), `fs_write_bulk` (create/truncate),
  `fs_write_at` (the offset-write primitive), `fs_mv`, and `parse_u64`
  (relocation-safe, shared by `writeat`).
- `cp` resolves both operands against the cwd, guards against a self-copy
  (streaming truncates the destination first), probes the source exists, then
  streams it one `SAFECOPY_MAX` chunk at a time (`fs_read_bulk` → `fs_write_at`)
  so a file of any size copies.
- `mv` keeps the `mv file dir` convenience (an `fs_list_dir` probe decides
  whether the destination is a directory to move *into*, keeping the basename)
  and the self-move guard, then a single `FSOP_MV`.
- `writeat` joins `argv[3..]` with spaces and writes at the parsed offset via
  `fs_write_at` — interior overwrite in place, past-EOF writes zero-filling the
  gap. It does not create the file.
- The three shell builtins (`cmd_cp`/`cmd_mv`/`cmd_writeat`) and the now-unused
  `fs_mv`/`fs_read_bulk` helpers were removed. Only `write` stays builtin (its
  content is the raw command line, bounded by the input buffer, so it never
  needs argv or the bulk path).

Verified on QEMU (run-image), zero `-d int` aborts: `cp /src.txt /dst.txt`
then `cat /dst.txt` → the copied text; `cp a a` → "source and destination are
the same"; `mv /dst.txt /d` lands `DST.TXT` inside the directory; `mv` rename
then `cat` → the content; `writeat /src.txt 6 AAAAA` → "hello AAAAA" (interior);
`writeat /src.txt 20 END` past EOF → the gap zero-filled then "END"; a missing
source to any of the three → the specific error and exit code 1.

**The whole filesystem command surface now lives in `/bin`** (`ls`/`cat` +
`mkdir`/`rmdir`/`touch`/`rm` + `cp`/`mv`/`writeat`), with only `write`, `cd`,
and `pwd` left in the shell. The arc continues with the non-fs commands
(`ps`/`kill`/`wait`/`ping`/`resolve`/`fetch`/`mount`/`selftest`/`help`). See
`roadmap.md`.

## Standalone binaries, Stage 4 (write-command increment): `mkdir`/`rmdir`/`touch`/`rm` externalized

Sixth step of the arc. With the cwd-delivery ABI in place, the four
path-only write commands were the cheapest batch to externalize: each is just
"resolve `argv[1]` against the delivered cwd, send one `FSOP_*`, report the
status." They share a new `ulib::fs_op_path(op, path)` helper (the shape the
shell's old `fs_mkdir`/`fs_rmdir`/`fs_touch`/`fs_rm` had), so each program is
~40 lines of `_start` — argument check, cwd fetch, path resolve, one call.

- `mkdir` → `FSOP_MKDIR`, `rmdir` → `FSOP_RMDIR`, `touch` → `FSOP_TOUCH`,
  `rm` → `FSOP_RM`, staged as `/bin/MKDIR`/`RMDIR`/`TOUCH`/`RM`.
- The four shell builtins (`cmd_mkdir`/`cmd_rmdir`/`cmd_touch`/`cmd_rm`) and
  their now-unused `fs_*` helpers were removed; `fs_write_file`/`fs_mv` (still
  used by `write`/`cp`/`mv`) inherited the shared status-contract doc comment.
- On failure a command prints the specific reason (via `ulib::fs_error`) and
  exits non-zero — the stderr-to-console split holds.

Verified on QEMU (run-image), zero `-d int` aborts: `mkdir /d1` then `ls /`
shows it; `touch /d1/f1` (absolute) and, after `cd /d1`, `touch f2` / `rm f1`
(relative to the delivered cwd) all resolve; every error path is correct with
exit code 1 — `rmdir` on a non-empty dir → "directory not empty", `mkdir` on
an existing one → "already exists", `rm` on a directory → "is a directory",
`rmdir /nope` → "no such file or directory".

Still builtin (they need the shell's own state or move bulk data): `write`,
`writeat`, `cp`, `mv`. The arc continues with those, then the non-fs commands.
See `roadmap.md`.

## Standalone binaries, Stage 4 (filesystem increment): a cwd-delivery ABI, and `ls`/`cat` externalized

Fifth step of the arc, and the first filesystem commands to leave the shell.
Externalizing `ls`/`cat` needed the one thing the first increment
(`echo`/`uptime`/`clear`) deliberately dodged: a spawned program has no cwd, so
it can't resolve a relative path or default a bare `ls` to "the current
directory." This adds a **cwd-delivery mechanism**, mirroring the argv ABI, then
moves `ls` and `cat` out.

- **The cwd ABI** (`syscall-abi`): `CWD_STAGE` (50) stages the shell's cwd
  bytes into a kernel buffer before `SPAWN`, exactly as `ARGS_STAGE` does for
  argv; `SPAWN`'s `arg3` now carries the staged cwd length; the kernel copies
  it into a per-slot `CWDS` store (the `MAILBOXES`/`ARGVS` pattern, cleared in
  all three teardown paths); `GET_CWD` (51) lets the child copy it back out
  (`CWD_MAX` = 128). A spawn with no cwd staged yields `/`.
- **`ulib` grew the filesystem client layer** it didn't need before:
  `cwd()`/`GET_CWD`, `resolve`/`concat_path`/`normalize_path` (the shell's own
  path logic, relocation-safe), `fs_call`/`fs_list_dir`/`fs_read_bulk`, and
  `is_fs_error`/`fs_error` — so a command talks to the filesystem server over
  the same IPC the shell uses.
- **`ls` and `cat` became `/bin` programs.** `ls` lists `argv[1]` (or, absent,
  the cwd) via `fs_list_dir`; `cat` streams `argv[1]` in `SAFECOPY_MAX` chunks
  via the grant/safecopy bulk-read path — both resolving against the delivered
  cwd. Their shell builtins (and the now-dead `cmd_ls`/`cmd_cat`/`get_ticks`/
  `LIST_BUFFER_SIZE`) were removed; `cd`/`cp`/`mv` keep `resolve_path` and the
  fs wrappers they still use.

Verified on QEMU (run-image), zero `-d int` aborts: bare `ls` → the root
listing; `cd EFI` then bare `ls` → **EFI's** contents (proving the spawned
program received cwd `/EFI`, not the default); relative `ls ORBS` and
`cat ORBS/INIT.CFG` after `cd EFI` → resolved correctly; absolute
`cat /EFI/ORBS/INIT.CFG` → the file; `ls /bin` → the command binaries;
`cat /nope/missing` → a clean "no such file or directory" and exit code 1; the
remaining builtins unregressed.

The arc continues: the rest of the filesystem commands
(`mkdir`/`rmdir`/`touch`/`rm`/`cp`/`mv`/`writeat`) can now follow the same
cwd+argv pattern, then the non-fs ones (`ps`/`kill`/`wait`/`ping`/…). See
`roadmap.md`.

## Standalone binaries, Stage 4 (first increment): `ulib` + echo/uptime/clear externalized

Fourth step of the arc, and the first commands to actually leave the shell. A
new shared userland crate, `ulib`, factors out the boilerplate every command
program repeats — the `svc` wrappers, argv reading (`GET_ARGC`/`GET_ARG`),
output routing (`write_out`/`con_write`/`pipe_out` + end-of-stream), a decimal
formatter, `exit`, and the one `#[panic_handler]`. Then `echo`, `uptime`, and
`clear` became real `/bin` programs (dropping their shell builtins), each just
an `_start` over `ulib`.

Why these three first: they need **neither the filesystem nor the shell's
cwd**, so they externalize cleanly. The filesystem commands
(`ls`/`cat`/`mkdir`/…) resolve paths against the shell's cwd, which a spawned
program doesn't have — externalizing them needs a **cwd-delivery mechanism**
(a per-task cwd, mirroring argv), the next increment.

Because a bare command already resolves via PATH (Stage 2) and runs
foreground/reaped (Stage 3's PATH still holds), dropping the builtins means
`echo`/`uptime`/`clear` now run as `/bin/ECHO`/`/bin/UPTIME`/`/bin/CLEAR` — in
the console, in a `| program` pipe (the builtin-left path spawns the program
and captures it), and in a `> file` redirect, all unchanged from a user's
view.

The tradeoff, documented: an externalized command is unavailable on a boot
without `/bin` (e.g. `make run`'s unmountable FAT16, or real hardware with no
stick) — a bare `echo` there is "unknown command" rather than the builtin it
used to be. A minimal built-in fallback set is a deferred option (see
`roadmap.md`).

Verified on QEMU (run-image): `echo hello world` → `hello world` (via
`/bin/ECHO`, task 5, reaped); `uptime` → a real tick count; three invocations
back to back each reaped (no slot exhaustion); `echo … | /EFI/ORBS/UPPER.BIN`
→ uppercased through the pipe; `echo … > /echoout.txt` then `cat` showed the
captured text; `help`/`selftest`/`ls /bin` and the remaining builtins
unregressed; zero `-d int` aborts.

The arc continues: a cwd-delivery mechanism, then the filesystem commands
(`ls`/`cat`/…). See `roadmap.md`.

## Standalone binaries, Stage 3: a shell environment

Third step of the arc: the shell gains environment variables and `$VAR`
expansion, and `PATH` becomes a real variable driving command lookup
(replacing Stage 2's `/bin` constant).

- A stack-local env store (`Env` — a fixed 16-entry NAME=VALUE table),
  threaded by `&mut` through `on_byte`/`run_line`/`dispatch_line` like `cwd`,
  since userland has no static mutable state. Initialized with `PATH=/bin`.
- Commands: `env` (list all), `set NAME=VALUE` / `export NAME=VALUE` (set or
  replace), `unset NAME` (remove). Values are a single token (no quoting,
  same limitation as `echo`).
- `$VAR` expansion (`expand_vars`) rewrites the line before dispatch: `$NAME`
  (`[A-Za-z0-9_]+`) becomes the variable's value (nothing if unset), a bare
  `$` is literal. Hand-rolled scalar scan, relocation-safe.
- `run_path_command` reads `PATH` from the env now, so `set PATH=…` changes
  where bare commands are found.
- Shell-local only: env is not exported into child programs yet (that's a
  later, argv-like ABI — deferred).

Verified on QEMU (run-image): `env` shows `PATH=/bin`; `set GREETING=hello` +
`echo $GREETING` → `hello`; `echo pre-$GREETING-post` → `pre-hello-post`;
`echo $MISSING done` → `done` (unset expands empty); `env` lists both vars;
`unset GREETING` then `echo …$GREETING…` → empty; `set PATH=/nowhere` makes
`args` "unknown command", `set PATH=/bin` makes it work again; zero `-d int`
aborts.

Next (last of the arc): externalize the actual commands into `/bin`
(Stage 4). See `roadmap.md`.

## Standalone binaries, Stage 2: /bin + PATH lookup

Second step of the arc (Stage 1 gave spawned programs argv): an unknown
command is now looked up as a **program on a PATH** and run by bare name.
Type `args foo bar` and the shell finds `/bin/args`, spawns it with argv
`[args, foo, bar]`, and runs it in the foreground — the first commands that
aren't shell builtins.

- **Makefile**: a new top-level `/bin` directory on the ESP, holding command
  programs named uppercase and extension-less (8.3-legal: `ARGS`), staged
  alongside the existing `\EFI\ORBS\` binaries. (Just `args` for now — the
  standalone commands themselves are Stage 4.)
- **shell**: the unknown-command arm (`dispatch_line`) now calls
  `run_path_command`, which walks `DEFAULT_PATH` (`/bin`, a constant until
  Stage 3's env var), probes `<dir>/<command>` with a one-byte `fs_read_file`,
  and on the first hit spawns it with the whole line as argv. Case is handled
  by fsd's case-insensitive `find`, so a lowercase-typed `args` matches
  `\BIN\ARGS`. Only if no PATH directory has the command is it "unknown
  command".
- **Foreground, not fire-and-forget**: unlike `exec`, a PATH-run command is
  waited for (which also *reaps its slot*) — so running commands back to back
  reuses the slot instead of exhausting the two spawnable slots with zombies
  (the Stage 1 caveat). A `>`/`>>` redirect captures the program's output,
  exactly as `exec … > file` does; Ctrl+C interrupts the wait (the program
  keeps running — see `ps`).

Verified on QEMU (run-image, real FAT32): `args foo bar` → argc=3 with the
args; `args` alone → argc=1; four `args` invocations back to back each ran in
slot 5 and reaped (no slot exhaustion); a lowercase-typed command matched the
uppercase on-disk `/bin/ARGS`; `bogus nonsense` → "unknown command: bogus";
`echo …` and the other builtins unregressed; `args one two > /binout.txt`
then `cat` showed the captured argv; zero `-d int` aborts.

Next in the arc: a shell environment (Stage 3, makes PATH a real env var),
then externalizing the actual commands into `/bin` (Stage 4). See
`roadmap.md`.

## Standalone binaries, Stage 1: an argv ABI

First step of the roadmap's "standalone command binaries" arc: give spawned
programs an argument vector. Nothing passed arguments to a spawned program
before — `SPAWN`/`SPAWN_STAGE` carried only the program bytes plus a stdout
target, a new task's GPRs were all zeroed, and `_start()` took nothing — so
`exec /path a b c` ignored `a b c` entirely. This is the foundation the rest
of the arc (a `/bin` directory, PATH lookup, standalone `ls`/`cat`/`echo`)
stands on.

Delivered kernel-side and fetched, the same shape as the per-task stdout
target — so a program's start-up register/stack state is unchanged (`_start`
still takes nothing). The shell stages an argv blob; the kernel keeps it
per-task; the child reads it via new syscalls:

- `ARGS_STAGE` (47): stage the argv blob into a kernel buffer, attached to
  the next `SPAWN` (its new arg2 = the blob length; 0 = no args). Blob
  format: `[argc: u32 LE]` then `[len: u32 LE][bytes]` per arg.
- `GET_ARGC` (48) / `GET_ARG` (49): the child reads its argument count and
  copies each argument out (`NO_ARG` for an out-of-range index).
- A per-slot argv store in `tasks.rs` (the `MAILBOXES` pattern), filled at
  spawn from the staging buffer, cleared on task death alongside the
  mailbox/grant/delegate.

Shell: `spawn_path` gained the argv vector and stages it before `SPAWN`;
`cmd_exec` builds argv from the whole line (`exec prog a b c` → argv
`[prog, a, b, c]`); the pipeline callers pass a single-element argv (just the
program path). A new `args/` program (the ninth userland crate) prints its
argv — the proof.

Verified on QEMU (`run-image`, real FAT32): `exec /EFI/ORBS/ARGS.BIN alpha
beta gamma` printed `argc=4` and `argv[0]=/EFI/ORBS/ARGS.BIN` …
`argv[3]=gamma`; `exec …/ARGS.BIN` alone printed `argc=1`; `exec HELLO.BIN`,
a `echo hi there | UPPER.BIN` pipe (→ `HI THERE`), `selftest`, and the disk
surface all unregressed; zero `-d int` aborts. (Repeated fire-and-forget
`exec` still exhausts the two spawnable slots — a spawned program that exits
is a zombie holding its slot until waited; that's the existing 2-slot/reaping
limit the arc's Stage 0 addresses, not an argv issue.)

Still to come in the arc: `/bin` + PATH lookup (Stage 2), a shell environment
(Stage 3), and externalizing the commands (Stage 4). See `roadmap.md`.

## Network stack, Stage 4o: TCP SACK (sender-side selective retransmit)

The last open TCP item. Loss recovery was go-back-N (4h fast retransmit, 4i
RTO both `rewind_to(snd_una)` then resend everything forward). This adds
RFC 2018 SACK on the sender side: negotiate SACK-permitted, parse the peer's
SACK blocks, and on a fast retransmit resend only the missing hole instead
of the whole window.

Scope, decided deliberately: **sender-side only** (the server). The receiver
side (generating SACK blocks) is out of scope here — the server receives
only a tiny request, and the client `fetch` reassembles its response
in-order without buffering out-of-order data, so there's nothing to SACK.

Three pieces: the server's SYN-ACK now advertises SACK-permitted (option
kind 4, NOP-padded); `parse_tcp_in` walks the TCP options for SACK blocks
(kind 5) into `TcpIn`; and on a fast retransmit with SACK blocks present,
`sack_retransmit` resends only `[snd_una, lowest SACK left-edge)` via a new
explicit-sequence send (`send_seg_at`/`retransmit_one`) that leaves the
forward cursor untouched — the peer already holds the SACKed data. No SACK
blocks → the existing go-back-N `rewind_to` fallback.

A real environment limitation, confirmed and documented, not a bug: **QEMU
user-net (SLIRP) doesn't support SACK.** SLIRP terminates the hostfwd TCP
connection with its own minimal stack, whose SYN carries no SACK-permitted,
so the guest's peer never enables SACK and never sends SACK blocks — every
dup-ACK is a plain cumulative ACK. So the end-to-end selective-retransmit
path can't be exercised through user-net (the same class of environment
limit as "SLIRP can't lose packets" and "Parallels' virtio-net is PCI").

Verified on QEMU, given that: SACK-permitted is really advertised (`tcpdump`
shows `mss 1460,sackOK,nop,nop` on the SYN-ACK); the go-back-N fallback
recovers a real injected drop byte-complete (the actual SLIRP path, since
the peer sends no SACK); and — with temporary instrumentation (reverted) —
the parser extracted a hand-built SACK block correctly (`sack_n=1
block=1000:2000`), and a fabricated SACK block drove `sack_retransmit` to
resend **only** the dropped segment (seq 21004:22404) rather than the
go-back-N burst (21004 onward), the pcap confirming the hole-only resend
directly. A clean run streamed byte-complete with sackOK advertised, client
`ping`/`fetch` unregressed; zero `-d int` aborts.

This closes the TCP feature set (handshake, flow control, fast retransmit,
RTO, congestion control, concurrent connections, and now SACK on the
sender). Still coarse: sender-side only (no SACK-block generation on
receive); the selective retransmit handles the first hole per event, bounded
to a few segments (a larger hole finishes via the next dup-ACK round or the
RTO); and end-to-end verification needs a SACK-speaking peer (not available
through SLIRP).

## Network stack, Stage 4n: TCP congestion control (Reno)

The send window was bounded only by the peer's advertised flow-control
window. This adds a congestion window (`cwnd`) — TCP Reno — so the send rate
is `min(cwnd, peer window)`: it ramps up gently on a fresh connection and
backs off on loss.

Per-connection `cwnd`/`ssthresh` (bytes): `cwnd` starts at `INIT_CWND`
(4·MSS) and grows on each new ACK — by up to a segment per ACK in slow start
(cwnd < ssthresh, ~doubling per RTT), by ~MSS²/cwnd per ACK in congestion
avoidance (~one segment per RTT) — capped at the 16-bit window ceiling (no
window scaling negotiated). Loss cuts it: a fast retransmit (3 dup-ACKs)
halves cwnd to ssthresh (multiplicative decrease); an RTO drops cwnd to one
segment (back to slow start), the stronger response to the stronger loss
signal. All wired into the existing send-window computation and the
already-proven fast-retransmit / RTO branches — no new state machine.

Verified on QEMU with temporary instrumentation (reverted): a 60 KB download
showed textbook Reno in the cwnd log — slow-start growth 5601 → 26604 (+MSS
per ACK), then an injected drop (fast retransmit enabled) halved it to
ssthresh = 13302, after which congestion avoidance grew it ~+146/ACK
(MSS²/cwnd, the additive-increase phase, distinct from slow-start's +1400) —
the file downloading byte-complete throughout. The pcap showed the initial
burst metered by cwnd (≈4 segments) rather than the whole peer window. A
clean run streamed byte-complete with client `ping`/`fetch` unregressed;
zero `-d int` aborts.

On lossless SLIRP the reduction paths need injected loss to exercise (cwnd
otherwise just ramps to the peer window and stays); the visible everyday
effect is the slow-start ramp at the start of a transfer. Still QEMU-only;
this is Reno (no fast-recovery inflation, and go-back-N retransmit is
unchanged — SACK stays the one open TCP item).

## Network stack, Stage 4m: HTTP 405 for unsupported methods

The server treated every request like a GET (it only distinguished HEAD). A
request with any other method (POST, DELETE, …) now gets a proper `405
Method Not Allowed` with the `Allow: GET, HEAD` header RFC 7231 requires.
Pure `netd`, one check: `start_response` rejects anything but GET/HEAD up
front — before touching the path, so an unsupported method never reaches
`fsd` — reusing the same fixed-response prefix mechanism as 404/503.

Verified on QEMU: `curl -X POST /` and `curl -X DELETE /file` → `405 Method
Not Allowed` + `Allow: GET, HEAD` + a plain-text body; `GET` (200, correct
Content-Type/Content-Length) and `HEAD` (200 headers, no body) unregressed;
zero `-d int` aborts.

## Network stack, Stage 4l: RTT-estimated RTO

The retransmit timeout (Stage 4i) used a fixed 1 s base. This makes it
adaptive: an RFC 6298 estimator (`TcpConn`/`rtt_update`) measures each
connection's round-trip time and derives the RTO from a smoothed SRTT and
its variation (RTTVAR), replacing the fixed base — a fast peer gets a fast
RTO, a slow one a patient one.

Meaningful RTT estimation needs a finer clock than netd's `now()` (the 20 ms
preemption tick — a fetch RTT of tens of ms is 1–2 of those). A new kernel
syscall, `MONOTONIC_US` (46), exposes the ARM generic timer's free-running
counter as microseconds since boot (overflow-safe: whole seconds plus the
sub-second remainder, not a naive `ticks * 1_000_000` that overflows a u64
in days). Pure system-register reads, not gated — a monotonic clock is a
harmless read for any task.

The estimator, per RFC 6298: the first sample R sets SRTT = R, RTTVAR = R/2;
later samples fold in RTTVAR = ¾·RTTVAR + ¼·|SRTT−R|, SRTT = ⅞·SRTT + ⅛·R;
then RTO = SRTT + max(G, 4·RTTVAR), converted to `now()` ticks and clamped
to [200 ms, 2 s] (the floor matches the `NET_WAIT` poll so a minimum-RTO
timer fires at the next poll; the ceiling stays under the supervisor's
~2.5 s wedge threshold). `RTO_INIT_TICKS` (~1 s, the RFC's initial value) is
used until the first sample. `service_rto` arms the timer with this
per-connection estimate instead of a fixed base.

One RTT sample is timed at a time (Karn's algorithm): a sample starts when
genuinely new data is sent (tracked via a send high-water mark `snd_max`, so
a retransmit is never timed), completes when its end sequence is acked, and
is invalidated by any retransmit (`rewind_to` clears it — the ACK would be
ambiguous). The exponential backoff on repeated firings is unchanged.

Verified on QEMU with temporary instrumentation (reverted): the server's RTT
samples logged real values with correct RFC-6298 first-sample maths (srtt =
rtt, rttvar = rtt/2) and an adaptive RTO (10 and 47–49 ticks, not the fixed
50); loss recovery via the adaptive RTO was confirmed by injecting one
dropped segment with fast retransmit disabled — the file still downloaded
complete (59968 bytes), so the RTO fired, resent from `snd_una`, and
recovered. A clean run (no injection) streamed the file byte-complete with
no spurious retransmits, and client `ping`/`fetch` worked; zero `-d int`
aborts throughout.

A documented characteristic, not a bug: netd is single-threaded and
processes a segment's ACK only after pumping a window, so a busy transfer's
measured RTT includes netd's own send latency (~300 ms for a 60 KB file over
SLIRP, vs ~9 ms for a tiny one). This *over*-estimates the RTO, which is the
safe direction (a too-short RTO would retransmit spuriously) — it is not
pure wire RTT. Still QEMU-only (the whole stack is); the client `fetch` path
uses its own fixed deadlines, not the estimator (only the server has a
persistent per-connection RTO).

## Network stack, Stage 4k: IRQ-driven NIC receive

The NIC was the last driver still polled — `netd` woke on the timer tick's
wake-check (`net_has_frame()`), so a delivered frame waited up to one tick
(20 ms) to be noticed. This wires the virtio-net receive queue's interrupt
through the GIC so a frame wakes `netd` immediately. The tick-poll stays as
a fallback, so the interrupt is a latency optimization, not a correctness
dependency: a missed IRQ degrades to ≤1 tick of delay, never a lost frame.

The NIC's interrupt is a GIC **SPI**, and neither GIC backend routed SPIs
before — both only ever enabled the timer PPI. `gicv2::enable_interrupt`
now also sets `GICD_ITARGETSR` for an SPI (route it to CPU 0; an untargeted
SPI is delivered nowhere); `gicv3::enable_interrupt` gains a distributor
path for SPIs (`GICD_ISENABLER` + `GICD_IROUTER` affinity routing + Group 1
in `GICD_IGROUPR`, versus the redistributor path a PPI uses). The
slot→INTID mapping (QEMU `virt`: virtio-mmio slot *i* → SPI 16+*i* → GIC
INTID 48+*i*) was confirmed via a devicetree dump, like the transport
addresses themselves.

`virtio_net::init` suppresses *transmit* interrupts
(`VIRTQ_AVAIL_F_NO_INTERRUPT` on the TX ring — `send_frame` polls), so the
device's single interrupt line only ever means "a receive frame arrived"
and the handler needn't disambiguate queues. `rust_irq_handler` acks the
device (`InterruptStatus` → `InterruptACK`, required or the device won't
raise the next one) and calls a new `tasks::on_net_irq`, which wakes the
`NetInput`-blocked server and switches to it immediately — the same
frame-overwrite contract as `on_tick`. `netd` drains all frames and
re-posts buffers itself, as before. Enabled only when a NIC was actually
installed (QEMU only, behind `virtio_mmio_probe_safe`); the no-NIC path is
untouched.

Verified on QEMU, both GIC versions, with temporary IRQ instrumentation
(reverted): under default **GICv2**, the receive IRQ fired for every
inbound frame — 2 per `ping` (ARP + ICMP reply), 2 per `resolve`, 7 for a
`fetch` — with `ping`/`resolve`/`fetch` all working, `uptime` advancing
(the timer PPI coexists with the NIC SPI), and zero `-d int` aborts; under
forced **GICv3** (`-machine virt,gic-version=3`), the same, via the
distributor SPI path. A no-NIC boot (`selftest`/`ls`/`uptime`) was
byte-unchanged with no interrupt enabled. Still QEMU-only (Parallels'
virtio-net is PCI, an unsupported transport); receive is now
interrupt-driven, transmit still polls.

## Network stack, Stage 4j: concurrent TCP connections

The server handled one connection at a time — a second peer's SYN *replaced*
the first, so a browser (several connections per page) or multiple clients
couldn't be served together. This multiplexes up to `MAX_CONNS` (4)
connections, keyed by peer IP+port.

The single `Option<TcpConn>` became `[Option<TcpConn>; MAX_CONNS]`;
`handle_tcp` is now a router (a SYN opens a connection in a free slot —
dropped if all busy, the peer retransmits — every other segment dispatched by
`find_conn` to its connection's state machine, extracted into
`handle_conn_segment`). `serve()` services the RTO and pumps *each* active
connection per wake. Each connection's flow control / fast retransmit / RTO
were already per-`TcpConn`, so this was purely routing + a per-connection
loop.

A guard-page overflow surfaced — and which half overflowed is the point: the
concurrent *streaming* path was fine (4 concurrent 60 KB streams, zero
faults), but a *client* op (`fetch`'s ~5 KB of TCP/DNS buffers) nesting on
top of 4 `TcpConn`s (~2.3 KB each) on `serve()`'s frame overflowed 24 KB.
`STACK_PAGES` grew 6→8 (24→32 KB/region); netd is by far the most
stack-hungry program (4 connections + a full TCP client).

Verified on QEMU: 4 concurrent small fetches all correct; 4 concurrent 60 KB
`SH.BIN` streams all byte-identical (~1 s); a single connection still works;
client net ops, disk, a pipe, `exec`, selftest unregressed; zero aborts, zero
restarts, zero EL0 faults. Still coarse: 4 connections max, a SYN with no
free slot is dropped, round-robin servicing, QEMU-only.

## FAT32 long filename (LFN) read support

The oldest FAT32 limitation, closed on the read side — surfaced by real use:
serving `index.html`. A 4-char extension can't be an 8.3 short name, so a
formatter writes a long filename (LFN) entry plus a mangled alias
(`INDEX~1.HTM`), and `fat32.rs` (in `fsd`) only saw the alias. Now long names
are **read, listed, and matched** by their true names: the web server serves
`/index.html` (correct `text/html` type) and directory listings show real
names.

`walk_dir_with_location` accumulates the run of LFN entries preceding each
short entry — 13 UTF-16 chars each, in reverse order, placed by sequence
number (order-independent), reconstructed to ASCII. The correctness piece:
the long name is used only if its checksum (byte 13, over the short 8.3 name)
matches the short entry, so an orphaned LFN run (deleted file, reused slot)
can't attach the wrong name. `DirEntry.name` grew 12→255 bytes; `name()`
returns the effective name, so `find`/`list_dir` get long names for free.

Read-only: the guest still can't *create* a long-named file (`make_short_name`
is 8.3-only), and deleting one leaves its LFN entries orphaned (harmless —
the checksum guard prevents mis-association). Both are documented follow-ups.

Verified on QEMU against `index.html`, a 21-char name (2 LFN entries), and a
long name in a subdirectory (all written by macOS): served by the web server
and read via the shell (`ls`/`cat`/`cd`, nested). 8.3 files unaffected; the
full write path unregressed; selftest, zero `-d int` aborts.

## Network stack, Stage 4i: TCP retransmit timeout (RTO)

Fast retransmit (4h) recovers a loss only while data keeps flowing (the
peer's dup-ACKs drive it). When the peer goes **silent** — the last segments
of a burst lost, or all its ACKs lost — no frames arrive and it never fires.
RTO is the timer-based fallback.

**Kernel:** `NET_WAIT` now takes a timeout. It used to wake only on a frame
or a message — useless for a timer that must fire when nothing arrives.
`WaitReason::NetInput` gained a `deadline`; the tick wake-check wakes the task
once it passes even with no input. The syscall reads `arg0` as a ms timeout.

**netd:** a per-connection RTO timer (`service_rto`, once per wake) — armed
while data is unacked, restarted (backoff reset) on ACK progress, and on
expiry (only reached when the peer is silent, since dup-ACKs would fire fast
retransmit first) resends from `snd_una` (go-back-N via `rewind_to`) with
exponential backoff (1 s base, capped) and a give-up (RST + close) after 5
tries. `serve()` uses a 200 ms `NET_WAIT` timeout while data is unacked so
netd wakes to check the timer during silence.

Verified on QEMU by temporarily disabling fast retransmit and injecting one
drop (reverted), so only the RTO could recover: a 256 KB transfer took ~6.7 s
(vs ~5 s — the RTO wait) and completed byte-identical, the timer firing once —
exercising the `NET_WAIT` timeout (netd woke during silence), `service_rto`,
and the resend. The normal lossless path is unaffected (the timer never fires
spuriously — `snd_una` advances every check): 256 KB byte-identical at normal
speed. Client net ops, disk, a pipe, selftest unregressed; zero aborts, zero
restarts. Still coarse: fixed 1 s RTO (no RTT estimation), go-back-N (not
SACK), give-up path unexercised, one connection.

## Network stack, Stage 4h: TCP fast retransmit

The server's first loss recovery. A lost segment used to stall the transfer
forever (`snd_una` never advanced past the gap) — fine on SLIRP's lossless
loopback, broken on a real network. This adds **fast retransmit**: three
duplicate ACKs at `snd_una` (the peer re-acking the last in-order byte for
each out-of-order segment past a gap) trigger a **go-back-N** resend from
`snd_una`. Driven by incoming frames — no timer, no kernel change.

`TcpConn` tracks `dup_acks` and `last_rexmit_una` (so leftover dup-ACKs don't
double-retransmit the same gap); `rewind_to(seq)` repositions the send cursor
(the response is a fixed `[prefix][file body][FIN]` stream from `SERVER_ISN+1`,
so a seq maps back to a prefix/file offset), and `pump_send` resends.

A real bug found by testing (an injected drop, since SLIRP can't lose
packets): the first cut recovered the gap but stalled at ~86 KB — a buffering
receiver acks *past* the rewound `snd_nxt` once the gap is filled, leaving
`snd_una > snd_nxt` so the window wraps to look permanently full. Fixed by
keeping `snd_nxt >= snd_una` (fast-forward the cursor when an ACK moves past
it).

Confirmed on QEMU with the drop injection: a 256 KB file that would stall now
streams byte-identical at normal speed; the pcap shows textbook go-back-N
(segments climb to the window edge, jump back to the gap sequence, re-climb).
The normal lossless path is unaffected (retransmit code dormant without
dup-ACKs); shell + client net ops + selftest unregressed; zero aborts, zero
restarts. Still coarse: fast retransmit only (no timer-based RTO for a silent
peer — needs a `NET_WAIT` timeout), go-back-N (not selective/SACK), one
connection.

## Network stack, Stage 4g: HTTP HEAD

A `HEAD` request now returns exactly the headers a `GET` would (including
`Content-Type`/`Content-Length` for a file) with no body. Small, self-
contained, and correct for every response kind: `start_response` builds the
response as usual, then for a HEAD trims the prefix to the header block
(through the first `\r\n\r\n` — present in the file 200, the directory
listing, and the 404/503) and streams no file body. `is_head()` matches
`"HEAD "` (a non-GET/HEAD method is still served like a GET — no 405 yet).

Confirmed on QEMU with a raw client: `HEAD /EFI/ORBS/INIT.CFG` → 200,
Content-Length 16, 0-byte body (headers byte-identical to the GET, which
still returns its 16-byte body); `HEAD /EFI/ORBS` → text/html, 0-byte body;
`HEAD /nope` → 404, 0-byte body. Shell + client net ops + selftest
unregressed; zero aborts, zero restarts.

## Network stack, Stage 4f: a browsable directory listing

A `GET` whose path resolves to a directory (including `/`) now returns a
generated HTML index of its entries, each a link — so the guest's filesystem
is browsable in a browser: `/` → `EFI/` → `ORBS/` → `INIT.CFG`, click through
and open files. Pure `netd`, no kernel/capability change.

`start_response` distinguishes file / directory / neither: stat the path
(`FSOP_READ_FILE`) → a file is served with headers as before; else (not a
no-fs/no-server case) try `list_dir` (`FSOP_LIST_DIR`) → a directory is
rendered as an index; else 404 (no-fs/no-server → 503). `/` lists the root
now (the old fixed landing page is gone). The two fsd calls were unified into
one `fsd_call` helper; `build_listing` turns fsd's newline-separated entries
(dirs suffixed `/`) into `<li><a href=…>` links resolved against the request
path (`.` filtered, `..` kept for parent nav). `PREFIX_MAX` 512→2048 to hold
a listing.

Confirmed on QEMU: `GET /` → `EFI/`; `GET /EFI` → `../ ORBS/ BOOT/`;
`GET /EFI/ORBS` → all the `.BIN` files + `INIT.CFG`, each linked;
`GET /EFI/BOOT` → `BOOTAA64.EFI`; files still serve with Content-Type/Length;
404 for a missing path; hrefs correct at every level. Shell + client net ops
+ selftest unregressed; zero aborts, zero restarts. Still coarse: capped by
fsd's ~512-byte inline listing (big dirs truncate), names+links only (no
sizes/timestamps/sorting), no Content-Length on the listing.

## Network stack, Stage 4e: proper HTTP response headers (Content-Type + Content-Length)

A small polish over the file server, which sent every file as
`application/octet-stream` with no length (so browsers downloaded instead of
rendering, and clients relied on connection-close to find the end). Pure
`netd` — no kernel/capability change.

`start_response` now *stats* the file (one `FSOP_READ_FILE`, whose status is
the real size — `want=1`, the smallest fsd accepts) instead of a probe read:
that both checks existence and gives the size for `Content-Length`.
`content_type()` maps the path extension (case-insensitive) to a MIME type
(html/htm, txt/cfg/md/log, css, js, json, png, jpg/jpeg, gif; else
octet-stream). `build_200_header()` formats the 200 with both headers
(hand-rolled `u64_decimal`); `TcpConn.prefix` became an owned buffer so the
header can be built per-request.

Confirmed on QEMU (`curl -i`): `.cfg`→text/plain len 16, `.htm`→text/html
len 63 (renders), `.bin`→octet-stream len 59968, a 256KB file→Content-Length
262144 with a byte-identical streamed body, 404 for a missing path. (A
`.html` name — 4-char extension — 404s because it isn't a valid FAT 8.3 name
and fsd has no LFN support; not a bug, the documented limitation. `.htm`
works.) Shell + client net ops unregressed; zero aborts, zero restarts.

## Network stack, Stage 4d: TCP send-side flow control (stream files of any size)

Stage 4c capped a served file at ~16 KB because it blasted the whole body at
once — fine only if it fits the peer's window. This adds real **send-side
flow control**: the server never lets unacknowledged data (`snd_nxt -
snd_una`) exceed the peer's advertised window, and streams the body *paced by
the peer's ACKs*, so a file of any size flows. The cap is gone.

The TCP server is stateful across event-loop wakes now: `TcpConn` tracks
`snd_una` (advanced by ACKs), the peer's `window`, and the in-progress
response (a fixed prefix, then a file body streamed from `fsd` by rising
offset). `handle_tcp` updates `snd_una`/`window` per segment but no longer
sends inline; `pump_send` sends one window-bounded segment and `serve()`
loops pump + a mailbox drain, flushing a full window per wake then blocking
in `NET_WAIT` for the next ACK.

**The per-segment mailbox drain is the subtle, load-bearing part.** A large
transfer is many slow `fsd` reads (each chunk = an IPC round trip + a
virtio-blk read, tens of ms under TCG), so netd is busy for seconds — and
the supervisor's ~160 ms health-ping would judge it wedged and **restart it
mid-transfer** (losing the connection state; observed directly as a
truncated download). Draining the mailbox — acking the ping — after *every
segment* keeps the worst ack latency to one segment. Bursts of 16 and 48
each occasionally overran the timeout under TCG; per-segment draining removes
the race at negligible cost.

A real stack overflow surfaced too: the extra frame tipped netd's client-op
path (already nesting 1600/2048-byte buffers near the 16 KB edge) into its
**guard page** — a clean contained fault, exactly what the guard is for.
`STACK_PAGES` grew 4→6 (16→24 KB/region); netd is the most stack-hungry
program.

Confirmed on QEMU: a 256 KB file streams byte-identical three times in a
row, zero restarts/faults, plus 60 KB `SH.BIN`, the small responses, and a
full shell+disk+client-net regression — zero `-d int` aborts. Throughput is
disk-bound (~55 KB/s under TCG), not a TCP-layer limit. Still coarse: no
retransmission, no congestion control, one connection, QEMU-only.

## Network stack, Stage 4c: a static-file HTTP server (netd serves files from fsd)

Stage 4b's server answered every request with one fixed page. This makes it
real: it parses the request-target path and streams that file from the
filesystem server over TCP — `curl localhost:5555/EFI/ORBS/INIT.CFG` returns
the file's actual bytes. The guest serves its own filesystem.

**The interesting part is the cross-server flow, not the parsing:** `netd`
becomes `fsd`'s **first non-shell client**. A request arrives at `netd` over
TCP → `netd` `MSG_CALL`s `fsd` to read the file → `netd` streams it back —
two userland servers cooperating on one external request, the driver-isolation
design paying off. One visible capability change makes it legal: `netd`'s
send-mask (`caps_for_slot`) gains `TO_FSD`. The read reuses the shell's exact
grant/safecopy `FSOP_READ_BULK` path (`cat`'s), ported into `netd`.

`serve_http` serves a landing page for `/`, else probes the file (so a 404
replaces the 200 header rather than following it) and streams a 200 header
plus the body in `SERVE_CHUNK` (1400)-byte segments (one `FSOP_READ_BULK`
each), advancing `snd_nxt`; 404 for a missing file, 503 for no filesystem.
Bounded at `MAX_SERVE` (~16 KB) — no flow control yet, so the body must fit
the peer's window; larger files truncate.

Confirmed on QEMU (`make run-image-server`, zero `-d int` aborts): `curl /`
→ the page; `curl /EFI/ORBS/INIT.CFG` → HTTP 200 + the real file content;
`curl /nope` → 404; `curl /EFI/ORBS/SH.BIN` (72 KB) → the first 16800 bytes,
**byte-identical** to the real file, over 12 body segments + a header (the
multi-segment path). Shell disk commands, `selftest`, and client net ops
unregressed. Still coarse: the ~16 KB cap, no `Content-Length`, one
connection, no directory listing, QEMU-only.

## Network stack, Stage 4b: an async-receive event loop, and a TCP HTTP server

The guest **answers** the network now, not just initiates. Stage 4a's gap
was that `netd` was client-only — it blocked on `MSG_RECV`, so nothing
watched the wire between requests, and a task can't block on IPC *and* poll
for frames at once. This adds the missing async-receive primitive (the
poll/select gap flagged since Stage 2) and a TCP HTTP server you can `curl`
from the host.

**Kernel (a minimal poll/select):** `virtio_net::has_frame()` peeks the RX
used-ring without consuming; `NET_WAIT` (syscall 45, gated to `CAP_NET`)
blocks the caller until either a frame or a message is pending, then returns
(the caller drains both itself). `tasks.rs` gained `WaitReason::NetInput`
(wakes on frame-or-message, consuming neither) and a `send_message` wake so
client calls to `netd` still land sub-tick.

**`netd`:** `serve()` is an event loop now — block in `NET_WAIT`, drain
client requests (`ping`/`resolve`/`fetch`, unchanged) *and* incoming frames.
Frames feed an ARP responder (answers requests for our IP) and a minimal
TCP server on port 80 (SYN → SYN-ACK, request → a fixed HTML page + FIN,
then acks the peer's FIN and closes). One connection at a time, state on
`serve()`'s stack (no static mutable state in userland). `build_tcp` split
into `build_tcp_generic` + client/server port wrappers.

Confirmed on QEMU (`make run-image-server`, SLIRP hostfwd tcp::5555→:80):
host `curl http://localhost:5555/` → `HTTP/1.0 200 OK` + the page, three
connections in a row; the pcap showed a textbook SYN/SYN-ACK/GET/200/
FIN/FIN-ACK exchange. Client ops (`ping`→reply, `resolve`→real DNS,
`fetch`→Example Domain) and the full disk/selftest/pipe surface unregressed;
zero `-d int` aborts. Still coarse: one fixed page, one connection at a
time, no retransmission/congestion control, QEMU-only.

## Network stack, Stage 4a: a client TCP, and a `fetch` command (real HTTP)

The stack's first **TCP**: `fetch <hostname>` opens a client TCP connection
on port 80, sends a minimal HTTP GET, and prints the response — a from-scratch
microkernel fetching a real web page over hand-rolled TCP. Two calls confirmed
with the user first (TCP was the roadmap's "separately scoped" milestone):
**hand-roll** (not `smoltcp`) and **minimal client scope** (active-open only).
**No kernel changes** — `netd` uses the Stage 2 `NET_*` syscalls.

`fetch` chains the whole stack: `netd`'s `NETOP_FETCH` resolves the hostname
(reusing `resolve_ip`), picks the next hop (a minimal default route —
on-subnet → target, else the gateway; the first time off-subnet routing
mattered), ARP-resolves it, and runs a hand-rolled client TCP: `build_tcp`/
`parse_tcp` (with the **TCP checksum over the IPv4 pseudo-header**), and
`tcp_get` — the SYN(+MSS)/SYN-ACK/ACK handshake, the HTTP request, in-order
response reassembly (ACKing each segment), and a clean four-way FIN teardown.
The request carries a Host header (name-based vhosts) and `Connection: close`.
Tight timeouts keep the busy-polling fetch under the supervisor wedge
threshold. The shell's `fetch <hostname>` prints the response (truncated to
one IPC reply, with the full length reported).

Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts, cross-checked
with `tcpdump`: `fetch example.com` → `HTTP/1.1 200 OK` + the real Example
Domain HTML. The pcap showed a complete, correct TCP conversation (SYN /
SYN-ACK `ack=ISN+1` / ACK / GET / 200 / FINs — a clean four-way close), all
checksums accepted. `ping`/`resolve` still work, no supervisor restart. Still
coarse: client active-open only (no `listen`), one connection, no
retransmission, response capped at one reply.

## Network stack, Stage 3: UDP, and a `resolve` command (real DNS)

The stack's first UDP application: `resolve <hostname>` does a **DNS A-record
query over UDP** and prints the resolved IPv4. Builds entirely on Stage 2's
`netd`/`NETOP` plumbing — **no kernel changes** (the `NET_*` syscalls already
move opaque frames; UDP is just a different payload), the payoff of the
driver/protocol split.

`netd` gained a `NETOP_RESOLVE` handler (request = op + hostname; reply =
status + packed IPv4): ARP-resolve the user-net DNS server (`10.0.2.3`),
encode a DNS query (header + hand-rolled QNAME labels), wrap it in
UDP → IPv4 → Ethernet (IP checksum computed, UDP checksum 0 — optional on
IPv4), send, and poll for the response; a hand-rolled DNS parser walks the
answer records (**handling name-compression pointers**) and returns the first
A record, mapping no-answer to `NXDOMAIN` and no-response to `TIMEOUT`. The
shell's `resolve <hostname>` command packs the name into a `MSG_CALL` and
prints `<host> is a.b.c.d`.

Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts, cross-checked
with `tcpdump` — and it resolves **real hostnames** via SLIRP's DNS proxy:
`resolve example.com` → `172.66.147.243`, `resolve one.one.one.one` →
`1.0.0.1`, `resolve nope.invalidtld` → `could not resolve` (`NXDomain`). The
pcap decoded our query as `10.0.2.15.32768 > 10.0.2.3.53: … A? example.com.`;
multi-A responses parsed correctly. `ping` still works, no supervisor
restart. Still coarse: A records only, fixed DNS server, guest-initiated
only. TCP is Stage 4.

## Network stack, Stage 2: the protocol stack moves to userland (`netd`), and a `ping` command

Networking moves out of the EL1 kernel the same way `fsd` and `cond` did: the
kernel keeps only the DMA-owning virtio-net driver (no IOMMU), reached by one
task through gated syscalls, and the whole protocol stack (ARP/IPv4/ICMP)
lives in a new userland server, `netd`. It ends with a real `ping <ip>` at
the shell. Two staged commits.

**2a - a fourth protected task slot + gated `NET_*` syscalls.** `netd` is
`NET_TASK` (slot 4), the fourth boot-loaded, supervised, protected server -
inserting it shifted the spawnable slots to {5,6} (`NUM_TASKS` 6→7, the
`exit`/`kill`/`wait` protections to `<= 4`, a `CAP_NET` arm in
`caps_for_slot`, seven static arrays each +1), the same shape as when `cond`
was inserted at slot 3, regression-tested hard afterward. `NET_SEND` (42) /
`NET_RECV` (43) / `NET_MAC` (44) operate on a kernel-held `NetCell` gated to
`CAP_NET` (`netd` alone), the `BLOCK_*` → fsd pattern; `init_net` installs the
NIC instead of probing it. `netd` proved the userland NIC path with an ARP
round-trip from EL0. A subtlety: a supervised server must *reply* to every
message or the health-ping restarts it, so `netd`'s loop replies with a
status u64 - which acks the ping.

**2b - real ARP + IPv4 + ICMP + `ping`.** `netd` gained a `NETOP_PING` handler
(the `FSOP_*` shape): ARP-resolve the target, build an ICMP echo request in an
IPv4 packet in an Ethernet frame with correct IP/ICMP checksums, send it, poll
for the matching reply. Hand-rolled fixed-buffer (the checksum, the frame
builders, the reply matcher), no crates. Short timeouts (ARP ~500ms, ICMP
~1s) keep an unreachable ping under the supervisor's wedge threshold. The
shell's `ping <a.b.c.d>` parses the quad byte-by-byte (no `str::split`, PIE
relocation safety), `MSG_CALL`s `netd`, maps the status to a message.
**Scope: guest-initiated ping only** - answering a host's ping needs an async
receive loop (the poll/select gap), deliberately deferred.

Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts, cross-checked
with `tcpdump` of the pcap: `ping 10.0.2.2`/`10.0.2.3` → `reply from ...`,
`ping 10.0.2.99` → `unreachable (no ARP reply)`; the pcap showed valid ARP
and ICMP echo request/reply (`id 0x4f42`, checksums validated) for the
reachable hosts. The renumber left everything working (`selftest`, disk
surface, `exec` → slot 5, pipe → slot 6, `ps` shows netd in slot 4), no
supervisor restart. See `docs/roadmap.md`. UDP is Stage 3, TCP Stage 4.

## Network stack, Stage 1: a virtio-net driver, and the first frames this kernel has sent

The first networking this project has ever had. Stage 1 of the network-stack
arc (`docs/roadmap.md`): the **kernel-side virtio-net driver only** - the
DMA-owning half that, per the no-IOMMU DMA constraint, must stay in the
trusted EL1 kernel (a device can DMA anywhere without an IOMMU, so the
ring/buffer owner can't be an untrusted EL0 task - the same rule that keeps
virtio-blk in the kernel). The protocol stack (ARP/IP/ICMP/UDP/TCP) is a
later stage's userland `netd` server; this driver just moves opaque frames.

`kernel/src/virtio_net.rs` reuses `virtio_mmio.rs`'s transport and the block
driver's static-virtqueue + poll-the-used-ring shape (no IRQ). Two real
differences from virtio-blk, both handled: **two virtqueues** (a receiveq
with pre-posted buffers drained incrementally by `poll_frame`, a transmitq
used one-at-a-time by `send_frame`), and a **12-byte `virtio_net_hdr`** on
every frame (`VIRTIO_F_VERSION_1`; `MRG_RXBUF` deliberately not negotiated,
so one buffer per frame). Negotiation spans both feature words
(`VIRTIO_NET_F_MAC` low, `VIRTIO_F_VERSION_1` high); the MAC is read from
config space; TX completion polls a **real wall-clock deadline** off the
generic timer (the xHCI iteration-count lesson).

`main.rs::init_net` is the end-to-end proof (modeled on `init_storage`):
discover + init, log the MAC, then send a **broadcast ARP request** for the
QEMU user-net gateway and decode the reply - proving transmit *and* receive.
Gated behind `virtio_mmio_probe_safe` like storage, so **Stage 1 is QEMU-only
by design** (the scan crashes real Parallels, which exposes virtio-net over
PCI anyway - a transport this project doesn't have). Silent when no NIC is
attached, so plain `make run`/`run-image` are unperturbed.

Confirmed on QEMU **two ways** (a new `make run-net` target attaches the NIC
over virtio-mmio with QEMU user-net + an `-object filter-dump` pcap): the
kernel logs `virtio-net ARP reply - 10.0.2.2 is at 52:55:0a:00:02:02`, and
`net.pcap` decoded independently with `tcpdump` shows the ARP request out and
the reply in - TX and RX confirmed by a source outside the kernel's own
output. Regression: plain `make run` boots normally, `init_net` silent, zero
`-d int` aborts. Still just the driver: no protocol stack, no `NET_*`
syscalls yet (those land with `netd`, Stage 2), polled not interrupt-driven,
QEMU-only. See `docs/roadmap.md`.

## FAT32 interior / random-access writes (`write_at` past EOF) - and a `writeat` builtin

`fat32::write_at` refused any offset past the current end of file
(`Error::InvalidOffset`) - a sparse gap FAT32 can't represent - so it only
did append and sequential-overwrite, which is all `cp` streaming and `>>`
append ever needed. This lifts that: a true **random-access write** at any
offset, zero-filling the gap when the offset is past EOF. The frontier item
the roadmap named next (concrete, unblocked, self-contained - entirely in the
fsd server's `fat32.rs` plus its `FSOP_*` layer, no kernel/scheduler/MMU
risk).

**A finding that shaped the scope: most of "interior writes" already
existed.** `write_at`'s per-sector loop already RMW'd a partial sector
overlapping existing content, so an *interior* overwrite (offset ≤ old_size)
was already coded - it just had **no caller** (`cp`/`>>` are sequential/append
only). And `FSOP_WRITE_AT` (op 13) + the shell's `fs_write_at` wrapper already
existed. The real gaps were exactly two: the `offset > old_size` refusal, and
no user-facing way to invoke an arbitrary-offset write. No new syscall or FSOP
op needed.

**The zero-fill, by generalizing the existing loop.** When `offset > old_size`
the gap `[old_size, offset)` must become **real zero bytes on disk** (FAT32
has no sparse representation, and `extend_chain`'s fresh clusters aren't
zeroed - so without this the gap is garbage, the correctness crux). The
per-sector loop now runs one unified pass over
`[min(old_size, offset), offset + len)`, building each sector from zeros
(positions before `offset`) and data (from `offset`). The boundary sector
straddling `old_size` and the same-sector-gap case both fall out of the
existing RMW branch with no special-casing, and it's byte-identical for the
offset-within-file paths (so `cp`/`>>` don't regress). A `MAX_GAP_FILL`
(1 MiB) cap keeps a fat-fingered offset from zero-filling the volume
(`Error::InvalidOffset`, mapped to `FS_ERR_IO`).

**The consumer:** a `writeat <file> <offset> <text...>` shell builtin - a
random-access write, in place, exercising both the previously-unreachable
interior-overwrite path and the new past-EOF zero-fill. Unlike `write` (full
replace) it leaves the bytes outside the window intact and does not create
the file (it must already exist).

**Confirmed on QEMU (real FAT32), zero `-d int` aborts, with the gap bytes
verified as real `0x00` by hex-inspecting the raw serial log on the host:**
interior overwrite (`AAAXYAAAAA`), append, past-EOF single-sector gap (5
bytes all zero) and **multi-sector gap** (a 1195-byte gap read back all
`0x00` - exercising freshly `extend_chain`'d clusters), the error cases
(nonexistent file, 1 MiB cap), **reboot persistence** (gap zeros + interior
content survive a fresh boot), and FAT16 degradation (shared no-filesystem
message; `help` lists `writeat`). Real-Parallels confirmation pending (the
fat32 path is only reachable there via a mounted USB stick, reads-only by
policy). See `docs/shell-commands.md`.

## Runtime capability delegation - relay-free program-to-program pipes

The IPC send-mask (who-may-call-whom) was a **pure function of task slot**:
a spawned program could reach `{shell, fsd, cond}` and nothing else. So a
program-to-program pipe (`/prog_a | /prog_b`) couldn't stream directly - the
shell relayed every chunk (`producer → shell → consumer`), sitting in the
byte hot path. This adds **runtime delegation**: the shell hands the
producer a capability to send straight to the consumer, taking itself out of
the loop.

**The primitive** (`DELEGATE`, syscall 41): grant one task the runtime right
to send to another - a dynamic addition to the grantee's static send-mask,
consulted by `may_send` after the static check, stored in one per-task slot
(`tasks::DELEGATED_SEND`, the single-slot `grant`/`stdout_target`
precedent), and cleared on task death (both a dying task's delegated-out
capability and any delegation aimed *at* it, so a reused slot can't inherit
a stale one). The rule that makes it safe: **a task may only delegate a
send-capability it *statically* holds** (`may_delegate` checks
`caps_for_slot`, never the delegated slot - no transitive re-delegation).
This self-secures the feature: only the shell statically holds the spawnable
slots' send-caps, so **only the shell can authorize one spawned program to
reach another** - a spawned program cannot, because it doesn't hold that
reach.

**The consumer** (`cmd_pipeline_prog`): the shell spawns the consumer
(stdout → console) and the producer with its stdout aimed straight at the
consumer's slot, `DELEGATE(producer, consumer)`, then only waits for both to
exit - no relay loop. The producer program needs no change: it already
routes output to its stdout target (a raw `msg_send` stream + empty
end-of-stream message when the target isn't the console). The spawn/delegate
race (a tick could let the producer run before the shell delegates) is
closed in the producer, not by pre-arranging slots: `hello`'s send retry now
tolerates `MSG_ERR_DENIED` as well as `MSG_ERR_FULL`, a denied-then-allowed
send being exactly the transient the existing bounded (~3s) retry is for.
The lost auto-kill of a non-reading consumer is the documented tradeoff -
the producer's bounded retry gives up and exits, then the shell's
`WAIT(consumer)` blocks until Ctrl+C, leaving the consumer alive in the
background (`kill` it via `ps`).

Landed in two staged commits (the standing discipline). Stage 1 the inert
kernel primitive - verified byte-identical existing behavior plus a
temporary race-free kernel self-test (reverted) confirming `may_send(4,5)`
went false → true → false across delegate/clear and `may_delegate` was true
for the shell, false for a child. Stage 2 the shell/`hello` wiring.
**Confirmed on QEMU, zero `-d int` aborts:** `HELLO.BIN | UPPER.BIN` streams
both lines uppercased with both tasks reaped (both lines intact proves the
race is closed); the builtin-left pipe (`echo | UPPER.BIN`) and `exec >
file` are unregressed; and the non-reading case (`HELLO.BIN | SH.BIN`)
recovers cleanly on Ctrl+C with the shell responsive afterward. See `docs/architecture.md`. Real-Parallels confirmation of the success path is
pending (it needs the userland binaries on a USB stick + `mount`, per the
standing reads-only policy); the delegation primitive is inert until a pipe
uses it, and a clean boot with it present is already confirmed.

## Userland heap (raw buffer) - and a useful negative result about `alloc`

Programs were fixed-buffer only (16KB stack, no heap, no static state), so
the shell's redirect/pipe capture was a 1024-byte stack array and `cat big
> file` *refused* anything larger. This gives each program a **256KB raw
heap area** in its region (`[code][heap][guard][stack]`), reported by a new
`heap_info` syscall (40), and backs the shell's capture with it - so a large
capture is held whole and written to disk in chunks.

**A go/no-go gate produced a useful negative result first:** a real
`alloc`-backed heap (`Vec`/`String`/`Box`) **can't be built under this
loader on stable.** Prebuilt lib`alloc` carries `R_AARCH64_ABS64`
relocations in its `.rodata` that a `-pie` link rejects (the same
`recompile with -fPIC` wall documented for `slice_error_fail`/`memrchr`),
and the fix - `-Z build-std` to rebuild lib`alloc` with PIE flags - is
nightly-only, off-limits on this stable-only project. One build proved it,
before any real investment. So this is a **raw buffer** a program uses via a
`&mut [u8]`, not a `GlobalAlloc` heap.

`loader.rs` sizes the heap into the region (the guard page is unchanged -
still just below the stack, the heap sits below the guard). The shell's
`Output::Capture` holds a heap-backed slice now, and `finish_redirect`
writes a large capture in `SAFECOPY_MAX` chunks (`write_all`), since one
`fs_write_*` is capped there.

Verified on QEMU, zero `-d int` aborts: **`cat /EFI/ORBS/SH.BIN > /big.bin`
(the full 72KB) captures completely and writes it**, where it used to refuse
at 1024 bytes - proven by round-tripping (`cat /big.bin | UPPER.BIN` shows
the uppercased ELF section names, so the whole file came back). Small `>`,
`>>` append, and the builtin-left pipe all still work; the region grows
~256KB per program (fits the 2MB slot), the idle region gets no heap. Still coarse: a raw buffer, not
dynamic collections; one fixed-size area per program; the shell is the only
consumer so far.

## Stack guard page - and the silent overflow it immediately found

Every userland task ran on a fixed 8KB stack with **no guard**: the layout
was tight (`[code][2 stack pages]`, no gap), so a stack overflow descended
straight into the program's own code - still EL0-accessible - as silent
corruption, no fault. This adds one inaccessible **guard page** immediately
below each stack, so an overflow takes a clean EL0 permission fault that
the existing fault-isolation handler already contains (kills just that
task; halts only for task 0/1).

`loader.rs` grows each region by a page (`[code][1 guard page][stack]`);
`mmu.rs::build_view` derives the guard's address from the region's
`(base, size)` and maps that one L3 page EL1-only - a hole inside the EL0
region, size-gated so the single-page idle region gets none. No ABI
change, no userland change.

**The guard immediately earned its keep by catching a real, pre-existing
bug:** the shell's own `exec` path (`cmd_exec` -> `spawn_path` ->
`fs_call`'s 768+768-byte request/reply buffers + staging + the call chain)
was overflowing its 8KB stack by ~32 bytes into the top of its code region,
every time - silently, because nothing was mapped to fault on it. Fixed by
growing the stack 8KB -> **16KB** (`STACK_PAGES` 2 -> 4, kept in sync
between `loader.rs` and `mmu.rs`).

Verified on QEMU, both directions, zero unexpected aborts: **no false
faults** with the 16KB stack (`selftest`, the previously-faulting `exec`,
streaming `cp` of the 72KB binary, cat-stream+redirect, and the
`hello | upper` program-pipe all run with zero EL0 faults); and a
temp-injected recursion overflow in `hello` (reverted) took a clean EL0
fault in its guard page -> the task killed alone -> the shell survived.
Known limitation (documented): a single >4KB stack frame could skip the
one-page guard - the standard single-guard limitation, still strictly
better than today's silent corruption.

## Program-to-program pipes and `exec … > file`: stdout-over-IPC

Pipes were **builtin-left only** (`builtin | /path/program`, the shell
capturing the builtin's output and streaming it) because a *task's own*
output wasn't capturable - a spawned program's output went straight to the
console. This milestone makes it capturable, delivering both
`programA | programB` (both spawned programs) and `exec prog > file`.

**A scoping finding shaped the design:** the clean pipe has the shell
**relay** (`producer → shell → consumer`), and `producer → shell` is
already permitted by the capability send-mask - so this needed **no
capability delegation** (which the item had been expected to require). The
same "route a program's stdout to the shell" mechanism delivers `exec >
file` too (the shell captures and writes the file).

The mechanism is a per-task **stdout target** (a task index, `CON_TASK` by
default), set at spawn (`SPAWN` gained an arg; new `STDOUT_TARGET` and
`SELF` syscalls, 38/39). A producer program routes output there:
`CON_TASK` → the console server (`DSPOP_WRITE`, unchanged); otherwise a raw
byte stream (chunked data messages + an empty end-of-stream marker) to the
shell. For a pipe the shell relays each chunk on to the consumer (reusing
the existing `pipe_send` + timeout-kill) and forwards EOF; for a redirect
it captures into the buffer `finish_redirect` writes to the file. The
consumer side of a pipe is an ordinary stdin→console filter (`upper`,
unchanged).

Four staged commits (stdout_target plumbing, `hello`'s producer routing,
the `program | program` orchestration, then `exec > file`). Verified on
QEMU, zero `-d int` aborts: `HELLO.BIN | UPPER.BIN` renders both of hello's
lines uppercased through the relay; `exec HELLO.BIN > /h.txt` then `cat`
shows the captured output; the existing `builtin | program` pipe, builtin
`>`/`>>` redirects, and plain `exec` are all unchanged. Still 2-stage (`a | b | c` needs
chaining), producer-`hello`-only for now (any future generator follows the
same pattern), and capture-bounded for the `> file` case (512 bytes, same
cap as existing redirects; pipes stream unbounded).

## A capability model for who-may-call-whom: IPC isolation goes topological

Isolation was MMU-enforced at the *memory* level (per-task page tables,
grant/safecopy) but the IPC *topology* was still flat: any task could
`MSG_SEND`/`MSG_CALL` any task, and the privileged kernel gates were ad-hoc
hardcoded slot checks. This milestone - the roadmap's last big structural
gap vs MINIX - makes isolation topological: a per-slot capability set
enforced at the IPC boundary, so a task can only reach the endpoints its
capabilities allow.

The change is small because **capabilities are a pure function of task
slot** (roles are static: 0 shell, 1 idle, 2 fsd, 3 cond, 4-5 spawnable) -
no stored table, no mutable state, the whole policy in one
`caps_for_slot(slot)`. It packs a **send-mask** (which slots this one may
*initiate* IPC to) plus resource bits (`CAP_BLOCK` for `BLOCK_*`, `CAP_CON`
for the console device). The policy: the shell reaches the two servers and
its children; each program reaches the servers and the shell; `fsd` reaches
`cond` (its logs); `idle`/`cond` initiate nothing. A **reply exemption**
makes request/response work - a server's reply to a caller blocked in a
`MSG_CALL` to it is always allowed (the same "client blocked in a call to
me" condition `SAFECOPY` uses), so only *unsolicited* sends are
mask-checked. The one flow that shaped the policy: `pong`'s echo to a
`send`/`recv` client is an unsolicited server->client send (not a reply),
which is why the child mask includes the shell.

Two staged commits: stage 1 folded the two hardcoded resource gates
(`== FSD_TASK`/`== CON_TASK`) into `cap_has` - a pure refactor, verified
byte-identical; stage 2 added the send-mask and the `may_send` check in the
`MSG_SEND`/`MSG_CALL` arms (returning a new `MSG_ERR_DENIED`). Verified on
QEMU, zero `-d int` aborts: **every existing flow intact** (selftest, the
disk surface over IPC, console output, the pipeline, the pong echo,
exec/ps - no false denials), and **denials fire** (A/B, temp probe
reverted: the shell denied a send to idle, and a spawned `hello` denied a
send outside its `{shell,fsd,cond}` mask - enforcement against untrusted
spawned code - while permitted sends succeed alongside). The supervisor
ping is unaffected (kernel-origin, bypasses the boundary). Static policy only (no
runtime delegation yet - the natural follow-up).

## Active health-ping: catching a server wedged while blocked

The refinement the supervision milestone (below) named as its one gap: the
passive heartbeat catches a server stuck *`Runnable`* (an infinite loop),
but a server stuck *`Blocked`* forever - deadlocked mid-request, waiting on
a reply that never comes - is invisible to it, since a healthy idle server
and a deadlocked one look identical from the outside. The active ping tells
them apart by *poking* the server.

The mechanism needs **no new syscall and no change to any server**, because
it rides the machinery already there. When a supervised server has sat
`Blocked` for a poke interval (~1.3s), `supervisor::poll_ping` (driven by
`on_tick` alongside the passive `heartbeat`) has the caller inject a
`SYSOP_PING` message under a reserved sender sentinel (`KERNEL_SENDER`). A
server idle in its main `msg_recv` is woken by direct delivery and replies
to whatever "sender" the message carried; that reply, addressed back to
`KERNEL_SENDER`, is intercepted by the `MSG_SEND` syscall arm as the ack
(`supervisor::note_ack`). A server stuck mid-sub-call does *not* get woken -
the ping just queues, unseen - so an outstanding ping older than the timeout
(~160ms, far above a healthy ack's tick-or-two) means wedged, and it
restarts on the *same* teardown path the crash and runnable-wedge paths
already use. One ping outstanding at a time, so a server's 4-deep mailbox
can never fill with pings; a `Runnable` server is never pinged (that's the
passive heartbeat's job).

Landed in two staged commits (the project's standing discipline for any
scheduler/SVC-path change): stage 1 the inert plumbing (the `KERNEL_SENDER`/
`SYSOP_PING` ABI constants, the `MSG_SEND` ack interception, the per-`Entry`
ping state + `poll_ping`/`note_ack`), regression-verified byte-identical to
prove the ABI change and the new `MSG_SEND` branch don't perturb the
heavily-exercised `fsd`/`cond` IPC; stage 2 the `on_tick` wiring. Verified
on QEMU, zero `-d int` aborts throughout: the **false-positive gate** (15s
and 10s idle + render-heavy bursts + the full disk surface -> zero spurious
restarts - a healthy server always acks in time); the **ack path live**
(temp instrumentation, reverted: clean inject/ack pairs for both servers);
and a **caught wedge** (temp `fsd` deadlock via an `MSG_CALL` to idle that
never replies, reverted: the ping to `fsd` went unacked -> "wedged -
unresponsive (ping timeout) - restarting" -> "restarted (attempt 1/3)" ->
`fsd` remounted -> disk commands worked again, with `cond` acking normally
throughout and the blocked shell call rescued by `fail_calls_to`). Not
re-run on real Parallels hardware this round - the ping machinery is inert
on a healthy system (a server always acks), the same no-regression posture
the supervision milestone's non-crash paths already have.

## Server supervision + heartbeat: uniform crash recovery, and the first wedge detection

Generalized fault tolerance from a bespoke fsd-only mechanism to a real
supervision registry (MINIX's reincarnation server / Helix's self-heal,
in miniature). New module `kernel/src/supervisor.rs` keeps each
supervised server's boot ELF image and restarts it on either failure
mode: a **crash** (the EL0-fault handler now restarts *any* supervised
slot generically, not just fsd - so the console server `cond` recovers
from a fault too, which it previously couldn't) or a **wedge** (a server
stuck in an infinite loop, which never faults). Wedge detection is a
passive **heartbeat** in `tasks::on_tick`: a healthy server keeps
returning to a `Blocked` state (idle in `msg_recv`, or briefly busy), so
staying continuously `Runnable` for ~2.5s (`WEDGE_TICKS = 128`) is the
wedge signal - no server changes, no new ABI. Both failure modes restart
on the same teardown path (`free_runtime_region` + `revert_input_owner_if`
+ `fail_calls_to` + reload from the kept image + mmu rebuild), with a
shared per-boot restart cap (3) that degrades gracefully past it (a dead
`cond` falls back to the kernel's `PUTC` console; a dead `fsd` degrades
like a missing FSD.BIN).

The fsd-only singletons in `syscall.rs` (`FSD_IMAGE`/`stash_fsd_image`/
`restart_fsd`/`MAX_FSD_RESTARTS`/`FSD_RESTARTS`) were deleted and
replaced by the registry; `loader.rs` registers both servers at boot.
Verified on QEMU by temp fault/wedge injection (reverted after): the
critical false-positive check (idle + bursts never trip a restart), cond
crash+restart, cond wedge+restart, cond restart-cap "giving up" with
graceful `PUTC` fallback, and fsd crash+wedge both recovering - then a
clean full regression (selftest + disk surface, zero aborts, no
supervisor events). The passive heartbeat can't catch a server
*deadlocked while blocked* - an active health ping does, added as the
follow-up milestone above.
**Confirmed on real Parallels hardware too:** with temporary
instrumentation (reverted, never committed), a live `cond` crash recovered
on the framebuffer - the screen cleared to a fresh `cond` banner and the
next `uptime` rendered a real tick count through the restarted server, the
console server recovering on the platform where it's the only console.


## Console server: the console moves out of the kernel (rendering in userland)

The second component moved out of the EL1 kernel (after the filesystem
server): the *steady-state* console. `cond/` (the seventh userland
program) is boot-loaded into a new protected task slot 3
(`syscall_abi::CON_TASK`), exactly like `fsd` in slot 2. Userland text
output no longer goes straight to the kernel - the shell, `hello`,
`pong`, and `upper` now send it to `cond` as batched `DSPOP_WRITE`
messages (`MSG_CALL`), and `cond` forwards it through a new **gated
`CON_WRITE` syscall (33)**, the console analogue of `BLOCK_*` gated to
`FSD_TASK`. A `PUTC` fallback in every client keeps output working if
there's no console server this boot.

Done in two stages. **Stage 1** was the byte-stream backend (the second
protected server, the `DSPOP_*` protocol, userland output as an IPC
stream - the stdout-over-IPC substrate the pipe/redirect items wanted),
zero framebuffer/MMU changes. **Stage 2 moved the framebuffer text-
rendering logic out of the kernel** - the real "rendering driver in
userland." `cond` gained a `Framebuffer` backend (chosen from a new
`CON_INFO` syscall) that owns the cursor, line wrap, scroll decisions,
ANSI parsing, and its **own** copy of the 8x8 font; the kernel keeps only
dumb gated pixel primitives in `fbdev.rs` - `FB_BLIT` (plot glyph
bitmaps), `FB_SCROLL` (`ptr::copy` the screen up), `FB_CLEAR`. Gated
primitives were chosen over mapping the framebuffer into the server's
EL0 view (which would need the per-view device-mapping MMU work that
faulted real Parallels once). A payoff beyond parity: the kernel's old
`fbconsole` never parsed ANSI, so `clear` did nothing on a framebuffer;
`cond`'s parser handles `\x1b[2J`/`\x1b[H`, so `clear` clears. The kernel
keeps a minimal emergency console (fbconsole/UART) for boot and faults.

**A scheduler fix landed alongside it, needed but not originally scoped:
`MSG_CALL` now switches directly to the destination server.** Per-
character echo through IPC exposed that a plain `MSG_CALL` reply waited
up to a full tick - the round-robin picked the always-runnable idle task
before the just-woken server - which dropped burst input outright. The
ABI's `MSG_CALL` doc already promised sub-tick round trips, so
`tasks::block_current_and_switch_to` now takes a `prefer` task and
`MSG_CALL` passes the destination; the server runs, replies (direct-
delivered back), and blocks again before the next tick. Every `fsd` call
got faster too.

**Confirmed on QEMU (`make run` and `make run-image`), zero `-d int`
aborts:** shell banner/`help`/`uptime`/`echo` render through `cond`;
`selftest`'s relocation checks pass (the shell binary changed); `ls`/
`cat` via `fsd` (two servers coexisting); a pipe (`echo ... |
UPPER.BIN`) and `exec HELLO.BIN` route spawned output through `cond`;
`ps` shows all six slots. Stage 2's framebuffer rendering was confirmed
on QEMU `ramfb` by QMP screendump (the pixel-level check): the wrapped
`help` output, `clear` actually clearing, and 32 `help`s scrolling
cleanly. Not yet on real Parallels hardware - the framebuffer backend is
exactly what matters there (Parallels has no UART console), and that
confirmation is on the user, same as every framebuffer milestone.

## FAT32 offset-write (`write_at`): streaming `cp`, unbounded `>>`

The follow-on the grant/safecopy milestone recorded. Every write at the
FAT32 layer was a **full replace** - `write_file` allocated a fresh
cluster chain, freed the old, repointed the entry, and never touched
existing clusters or wrote a partial sector - so `cp`/`>>` were each
bounded by one in-memory buffer and `cp` of a file over `SAFECOPY_MAX`
(2048) refused outright.

`fat32::write_at(path, offset, data)` writes at a byte offset and
**extends** the file without rewriting the bytes before it. The one
genuinely new primitive is a partial-sector **read-modify-write for file
data** (RMW previously existed only for metadata - directory and FAT
entries); a sector overlapping existing content is read, spliced, and
written back, a sector past the old end of file is zero-padded, a full
sector skips the read. It reuses the chain walk, cluster allocation, and
size-field patching that were already there, adds a grow-only size
update, and refuses a write past EOF (no sparse gaps). A new
`FSOP_WRITE_AT` carries the data via grant/safecopy exactly like
`FSOP_WRITE_BULK`.

`cp` streams now - probe the source, truncate the destination, then loop
read-a-chunk/`write_at`-at-the-offset - so it copies a file of **any
size** (bounded by disk space, not a shell buffer). `cp x x` (self-copy)
is refused, because streaming truncates the destination first, which
would destroy the source (the old read-whole-then-write cp was safe by
construction; this isn't). `>>` appends at the file's end via `write_at`
(no read-back), so it works on a target of any existing size - the old
"file too large to append to" refusal is gone.

Verified on QEMU against the real FAT32 image: streaming `cp` of a
5564-byte (non-sector-aligned, multi-chunk) text file is byte-exact
(chunk-boundary markers and all 120 lines correct); `cp` of the 72KB
shell binary persists complete across a reboot (the `.shstrtab` section
name, at the very end of the ELF ~72KB in, reads back - all 36 streamed
chunks landed); `>>` appends correctly and preserves existing content on
both a tiny file (RMW of sector 0) and a 5564-byte file (RMW of the last
partial sector, previously refused); `cp x x` refused; a missing source
leaves the destination untouched; inline `write` unaffected; FAT16
degrades to the shared no-filesystem message. Zero aborts throughout.
**Confirmed on real Parallels hardware** (with the user's go-ahead to
write a scratch file): a streaming `cp` of the 5.7KB `hello.bin` on the
real Lexar stick read back complete, and after a VM reboot a persisted
file was appended to with `>>` - `cat` showing both the original and
appended lines, the partial-sector RMW preserving content on real
silicon; the scratch files were removed afterward.

## Grant/safecopy IPC: enforced capability-based bulk transfer

The second half of MINIX's IPC design, and the fix for the last
user-visible limitation per-task page tables left behind. FSOP v2 made
filesystem requests self-contained (payloads inline, kernel-copied)
because a server can no longer dereference a client pointer - which
capped every operation at what fits one 768-byte message, ~512 bytes.
This milestone moves bulk data directly between two isolated regions
instead, without the message.

Two new syscalls, **enforced, not trust-based** (the user's explicit
call, consistent with the per-task-tables milestone): `grant` (31) - a
client records, in its own single per-task grant slot, that one server
task may bulk-copy an *exact* buffer in the client's own region, in a
given direction; `safecopy` (32) - the server, while handling the
client's `msg_call`, copies within that grant. The kernel authorizes a
copy only when the grant names this server and permits the direction,
the client is *currently* blocked in a call to it (a stale grant is
inert), and both ranges are in bounds. The copy runs at EL1, where all
RAM is identity-mapped read/write in every per-task view - so it reaches
both regions regardless of the active `TTBR0`, while the server can
touch only the designated bytes, only during a call the client
initiated, and never a third task. `safecopy` takes five arguments; the
fifth (direction) is read from the saved trap frame's `x4`, the
4-argument dispatch signature being full.

The FSOP protocol gained `FSOP_READ_BULK`/`FSOP_WRITE_BULK`. `cat` now
**streams a file of any size** (loops the bulk read, one `SAFECOPY_MAX`
= 2048-byte chunk at a time, never holding the whole file - the old
512-byte truncation and its notice are gone); `cp` copies files up to
2048 (was refused past 512); `>`/`>>` redirection captures and appends
past 512 too. Buffer sizes were chosen against the guard-page-less 8KB
userland stack: cp uses a full 2048; the redirect capture/append stay
at 1024 because `cat ... > file` nests the capture, cat's 2048 chunk,
and `fs_call`'s request/reply on one frame. `write`'s content is
bounded by the 128-byte input line, so it stays on the cheap inline
path. Larger transfers still refuse-not-truncate; the ultimate ceiling
is userland memory (no heap), with a FAT32 offset-write primitive and a
userland heap/guard page recorded as the follow-ons.

A real correctness detail, caught by design: a client with a buffer
smaller than `SAFECOPY_MAX` would have the server overrun its grant, so
`FSOP_READ_BULK` carries a `want` parameter bounding the read. Verified
on QEMU against the real FAT32 image: a 5564-byte file streams complete
(chunk-boundary markers all present, all 120 lines); cp of a 1500-byte
file round-trips where it used to refuse, cp of 5564 refuses; a
>512-byte redirect capture and a correctly-ordered `>>` append (not the
historical overwrite bug); reboot persistence; FAT16 graceful
degradation; zero aborts throughout. **Confirmed on real Parallels
hardware end to end**: `selftest` passes (the heavily-modified shell
binary relocates correctly), then `mount` finds the passthrough Lexar
stick, `ls` lists it, and `cat /hello.bin` streams the whole 5784-byte
binary via grant/safecopy - the `ELF` magic, the `.rodata` string, and
the section-name tail (~5KB in) all rendered with no truncation, where
the old `cat` would have stopped at 512 bytes. (A stick present *at
boot* consistently displaced the keyboard in the one-shot xHCI scan -
the known multi-device limitation - so the run used the designed
workflow: boot, then `mount` to rescan for the late-attaching stick.)

## Per-task page tables: MMU-enforced isolation, not trust

The last of the three post-part-2 candidates, and the one that makes
"microkernel" mean something enforceable. Before this, every EL0
region was accessible to every task - isolation was a convention. Now
each scheduler slot runs under its own translation-table view
(`mmu.rs`: per-view L0/L1/L2/L3) with identical kernel/device mappings
but EL0 access to its own region alone; `mmu::activate_task` switches
`TTBR0` and flushes the TLB at every context switch. Touching another
task's memory faults, and the fault-isolation work kills only the
toucher - proven by an A/B probe (a byte-read of the shell's region
from a spawned program succeeded under the old shared map, faulted
under views). The syscall boundary closed its matching gap the same
day: `in_caller_region` validates every `(pointer, length)` against the
calling task's own region, so access can't be laundered through a
kernel copy.

The one thing per-task views broke - the filesystem server
dereferencing pointers into client memory - drove a protocol rework
(chosen with the user over mapping-based grants, which leak neighboring
client memory at page granularity): **FSOP v2**, fully self-contained,
payloads inline in the message (kernel-copied task-to-task), over
768-byte messages (`MSG_MAX_LEN` 64 -> 768). This is the first half of
MINIX's real design; a grant/safecopy primitive can lift the 512-byte
per-op cap later without touching the framing. Per-task ASIDs (a
per-switch-TLBI optimization) were implemented and passed on QEMU but
faulted the idle task on real Parallels hardware - reverted in favor of
the flush-on-switch design, confirmed correct on both platforms;
recorded as a future optimization with the fault evidence. Confirmed on
QEMU (full regression, the isolation A/B proof, fsd crash/restart under
views) and real Parallels (selftest, disk, echo, ps, advancing uptime -
no fault).

## Pipelines: `builtin | program`, data flowing between processes over IPC

The composition layer on top of part-1 IPC and the two-step spawn:
`left | /path/to/program` captures a builtin's output (the redirection
machinery's 512-byte capture, refuse-not-truncate), spawns the program
(`spawn` now returns the new task's slot index - the shell needs it to
stream to, wait on, and kill), streams the capture to it as IPC
messages, and marks end-of-stream with one *empty* message - a
zero-length `msg_send` is legal now, the one ABI-behavior change. The
filter putc's its output as it runs and exits; the shell waits
(Ctrl+C-interruptible), and a program that stops reading its input is
killed after a ~3-second real-tick timeout rather than hanging the
shell on a full mailbox. `upper/` (the sixth userland program, ~80
lines) is the reference filter: uppercases its stream, exits on the
empty message. A real keymap gap found by the scripted real-hardware
smoke: HID keycode 0x31 (backslash/pipe) was missing from
`xhci.rs::keycode_to_ascii` entirely - no physical keyboard could type
`|` on Parallels; fixed, and `test-parallels.sh` gained the
held-Shift `|` chord. Verified on QEMU (echo/ls/cat piped through
UPPER.BIN, every parse error, early-exiting and never-reading right
sides including the timeout kill, FAT16 degradation, full regression,
zero aborts) and on real Parallels hardware (the pipeline typed
through the real keymap, correctly degrading to the no-filesystem
message). v1 limits, documented: one pipe per line, builtin left /
program right, no combining with `>`.

## EL0 fault isolation, and the filesystem server survives its own crashes

The containment payoff of process isolation, plus MINIX-style
supervision in minimal form. Before this, any userland fault - a wild
pointer in any program - took the diverging report-and-halt exception
path and stopped the whole system. Now: a fourth, resumable trampoline
in `exceptions.rs` handles every non-SVC synchronous EL0 exception by
killing just the faulting task (same teardown as `kill`, slot reaped
immediately) and resuming the next runnable one; tasks blocked
mid-`msg_call` to any dying task are woken with a cleanly failed call
on all three death paths (`tasks::fail_calls_to` - previously they
waited forever); and a faulted filesystem server is restarted from its
raw image, kept in a kernel static at boot precisely because the
crashed server *was* the filesystem that would otherwise be needed to
reload it. The fresh server remounts from disk (its state was always
disk-derivable); a 3-restart per-boot cap guards against crash loops,
degrading to the same no-filesystem behavior as a missing FSD.BIN. A
latent gap fixed along the way: runtime-spawned code never got the
dcache-clean/icache-invalidate sequence boot-loaded programs always
had (`tasks::flush_new_code`, now shared by spawn and the restart
path). Verified by direct fault injection on QEMU: a crashing spawned
program killed alone (shell running on, region reclaimed and reused);
fsd crashed mid-call four times - three restarts each followed by a
working remount, with the blocked caller failing cleanly each time,
then the capped give-up; a task-0 fault still halting cleanly; abort
counts in the -d int traces exactly matching the injections. Boot and
typing regression confirmed on real Parallels hardware. Not covered,
deliberately: wedged (non-faulting) servers - no watchdog - and disk
state corrupted mid-write - no journaling.

## Driver isolation part 2: the filesystem moves to userland

The first real component out of the EL1 kernel: `fsd/`, the fifth
userland program, owns the FAT32 engine (the kernel's old `fat32.rs`,
moved essentially verbatim onto a `BLOCK_*`-syscall disk shim) in
protected, boot-loaded task slot 2. The kernel keeps only raw sector
access - `BLOCK_INFO`/`BLOCK_READ`/`BLOCK_WRITE` (26-28), accepted
from the server's slot alone - and syscalls 7-14 (the old `fs_*`
family) are numbering gaps now; their contracts survive unchanged as
the `FSOP_*` request protocol clients speak to the server over IPC.
Enabling work in the same milestone: `MSG_CALL` (29), a synchronous
MINIX-sendrec-shaped call primitive built from direct delivery in
`send_message` (a matching blocked receiver gets the bytes, its saved
`x0`, and a wake immediately - no tick wait) plus a reply-sender
filter on the message wait (an unrelated task's message can no longer
be mistaken for a reply); a two-step staged `spawn` (`SPAWN_STAGE`,
30 - `exec` reads the program via the server in 512-byte chunks, since
the kernel can't read a path anymore); and a server-first two-phase
`mount` with device replacement (preserving first-MOUNTED-wins: an
unmountable boot disk never blocks a later USB stick). Scheduler/MMU
grew to 5 slots/EL0 regions. Two new prebuilt-libcore PIE constraints
found and documented (`str` range slicing, and `rfind`'s `memrchr`).
Confirmed end to end on QEMU (full disk surface over IPC, chunked
exec, reboot persistence, FAT16/missing-FSD.BIN degradation, USB
replace flow, zero aborts throughout) and real Parallels hardware
(mount -> INQUIRY -> server-side FAT32 mount -> ls of the real stick;
reads only by policy).

## Driver isolation part 1: real IPC

The prerequisite the roadmap's driver-isolation item named: fixed-size
(≤64-byte) messages copied through the kernel into bounded per-task
mailboxes (4 pending), `msg_send`/`msg_recv`/`msg_try_recv` (syscalls
23-25), with blocking receive as the third user of the
`WaitReason`/wake-check machinery (Ctrl+C-interruptible, same as
`wait`) and mailboxes cleared on task death so a slot's next occupant
can never receive a predecessor's mail. Proven by `pong/` - the fourth
userland program and first long-lived *server* (the process shape the
part-2 FAT32 filesystem server will have): recv -> echo to sender ->
repeat, `quit` to exit. Confirmed on QEMU (echo round trips,
queue-full at exactly the fifth pending message, the
cleared-mailbox-on-kill proof, blocked-recv Ctrl+C, quit/wait
lifecycle) and real Parallels - where, with the binaries copied onto
the USB stick, the full success path landed a stack of firsts in one
screen: the first runtime program load ever on real Parallels
hardware (`exec /pong.bin` off the mounted stick), a complete IPC
round trip through it, and `exec /hello.bin` + a blocking `wait`
collecting its status.

## USB mass storage - a real disk on real Parallels hardware

The milestone the whole diagnostic arc pointed at: `usb_msd.rs`
(Bulk-Only Transport + SCSI INQUIRY/READ CAPACITY/READ(10)/WRITE(10))
over new bulk endpoints in `xhci.rs`, a `BlockDevice` enum decoupling
`fat32.rs` from virtio-blk, boot-time auto-mount (QEMU) and a `mount`
command with a runtime port rescan (Parallels passthrough attaches
seconds after boot). Two real findings along the way: keyboard events
arriving mid-bulk-transfer must be routed, not dropped (dropping also
kills the keyboard permanently - the interrupt buffer never reposts),
and a freshly attached SCSI device reports Unit Attention (INQUIRY is
exempt; the first READ CAPACITY failed until the standard TEST UNIT
READY/REQUEST SENSE bring-up loop was added - found by the hot-plug
test, not spec-reading). Confirmed on QEMU (auto-mount, reads, writes
with reboot persistence, hot-plug + mount, typing during disk I/O
losing zero keystrokes) and on real Parallels hardware: `mount` on a
real Lexar USB 3.x stick produced its real INQUIRY strings, real
capacity (243,404,800 sectors), a mounted FAT32, and an `ls` of the
stick's actual contents - the first working disk this kernel has ever
had on its primary hardware target.

## `wait()` and reaping - exit statuses become real

The last job-control gap: exit codes went nowhere but a log line.
`exit` now leaves a `Zombie(status)` (code masked to `0..=255`,
POSIX-style) holding its slot until a `wait <n>` collects it - the
collection is the reap; `kill` still reaps immediately (the killer
knows the outcome). `wait` on a live task blocks on the same
machinery as `read_char` (`WaitReason::TaskExit`, the wake-check
generalizing exactly as designed), waking with the status - or
`TASK_KILLED_STATUS` if the target was killed out from under it, or
`WAIT_INTERRUPTED` on Ctrl+C (drained by the wake-check for a waiting
keyboard owner, so one bad `wait` can't brick the session; other
typing during a wait is discarded). `hello/` gained a deliberate
~2-second tick-delay so the blocking path is organically testable.
Honest behavior change: un-waited exited tasks hold their slots -
`ps` shows them, `wait` clears them. Confirmed on QEMU (block → child
exits → status collected, with ticks provably advancing during the
block; zombie-then-collect; slots-exhausted-by-zombies; interrupted
wait; fg/exit revert still firing at death not reap; all error paths)
and real Parallels (error paths, typing unregressed).

## Ctrl+C: the `fg` escape hatch

Job control's documented strand (fg a task that never reads input and
the keyboard was unreachable until it exited) is closed: Ctrl+C typed
while a non-boot-shell task owns the keyboard is intercepted at the
single keyboard-poll choke point, reverting ownership to task 0 and
swallowing the byte - reclamation, not a signal; the foregrounded task
keeps running in the background. Supporting pieces: the xHCI keymap
gained the Ctrl modifier (Ctrl+A..Z -> C0 control bytes; it only
handled Shift before), the shell's line editor now ignores all
unhandled control bytes, and test-parallels.sh gained a CTRL-C chord
pseudo-command. Confirmed on QEMU (fg -> strand -> Ctrl+C -> keyboard
back, nested task still alive) and real Parallels (the chord produced
a genuine 0x03 through the real xHCI path - no stray 'c' - and the
shell ignored it cleanly).

## Job control: `kill <n>` and `fg <n>`

The task-lifecycle arc completes: `kill` (syscall 19) destroys another
task (exit's teardown minus the context switch - a non-current task
isn't executing, its saved context is simply discarded), and `fg` (20)
hands the keyboard to a spawned task - the input owner, hardcoded to
task 0 since the keyboard-routing fix, is runtime state now, with an
automatic revert to task 0 whenever the owner dies (task 0 itself can
never die, so the revert target is always valid). `exec
/EFI/ORBS/SH.BIN` + `fg 2` is a real nested interactive shell session;
its `exit` hands the terminal back. Confirmed on QEMU (separate cwd
state proving input genuinely moved, roles visibly flipped in `ps`,
revert on exit, kill of a background task, all error paths) and real
Parallels (the reachable error-path subset). Documented deliberate
limitation: no interrupt key exists, so `fg` to a non-interactive task
strands the keyboard until it exits - `fg` is for interactive
programs; `kill` is the escape hatch for everything else.

## `ps`, and shell buffers at the syscall cap

Two more small same-day items: a `ps` builtin backed by a new
read-only `task_state` syscall (18) - one line per scheduler slot
(`unused`/`runnable`/`blocked`), the first way userland can see the
spawn/exit lifecycle it now controls (a spawned shell visibly sits
"blocked (waiting for input)"; an exited task's slot visibly returns
to "unused"), with the slot count discovered by probing rather than
leaking into the ABI as a constant. And the shell's `cat`/`ls`/`cp`
buffers doubled from 256 bytes to 512 - the kernel's per-syscall cap
(`MAX_USER_LEN`) and therefore the ceiling, not a choice - confirmed
by cat'ing and cp'ing a 408-byte file untruncated that the old
buffers would have cut off or refused.

## `mv` into directories, and `SPAWN_ERROR` split

Two small follow-ups the same day: `mv <src> <dir>` now moves *into* an
existing directory keeping the basename (shell-side, probing the
destination with the same `fs_list_dir` trick `cd` uses; the trivial
self-move `mv x x` is refused, guarding the degenerate
move-into-itself case). And `spawn`'s collapsed sentinel is split like
`FS_ERROR` was: read failures return the ordinary `FS_ERR_*` code,
plus `SPAWN_ERR_BAD_ELF`/`SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_NO_FREE_SLOT`
- and a failed spawn now returns its allocated region to the runtime
allocator (always the LIFO case). Confirmed on QEMU with organic
triggers for every new path, zero aborts.

## The collapsed `FS_ERROR` sentinel split into specific error codes

The gap flagged since phase 4: every `fs_*` failure collapsed to one
sentinel, so the shell printed guess-lists. `syscall-abi` now reserves
a top band of named codes (`FS_ERR_NOT_FOUND` through `FS_ERR_IO`,
with `FS_ERR_MIN` as the is-any-error floor since read/list return
arbitrary byte counts on success); the kernel maps `fat32::Error`
variants via one `fs_error_code` function; the shell prints one
accurate reason per failure via a shared `print_fs_error`. Backward
compatible - `FS_ERROR`/`NO_FS` keep their values. The specific
messages immediately surfaced a real mis-mapping the collapse had
hidden (`rmdir /` reported "invalid name" - `split_parent` failed
before the root check ran; fixed at the source), and the `>>` append
path now only treats "not found" as create-the-file instead of every
read failure. Confirmed on QEMU with one organic trigger per message,
full happy-path regression, FAT16 degradation unchanged, and a real
Parallels smoke. `SPAWN_ERROR` stays collapsed, deliberately.

## Task destruction (`exit`) - and `hello/`, the second real userland program

The `EXIT` syscall (17) destroys the calling task: slot freed for a
future `spawn`, EL0 mapping dropped (the same masked-IRQ
`mmu::rebuild_with_el0_regions` spawn proved safe), RAM returned to the
runtime bump allocator when LIFO order allows (most-recent allocation
only - anything else leaks, deliberately). Tasks 0 (boot shell, sole
keyboard owner) and 1 (idle) are refused with `EXIT_DENIED`.
`tasks::exit_current_and_switch` mirrors `block_current_and_switch`'s
proven frame-overwrite shape, discarding the context instead of saving
it. The test vehicle is a real deliverable: `hello/`, a ~70-line
second userland program (banner + exit) proving both the exit path and
"the shell is just a program" - zero new toolchain work, the
`aarch64-unknown-none` PIE flags and shared linker script are
workspace-wide. Confirmed on QEMU with the reclaim directly observable
(three exec/exit cycles landing at the same region base, a long-lived
spawned shell interleaved with slot-3 cycles) and on real Parallels
hardware (the typed `exit` refusal - the one reachable path there).


## Directory extension - full directories grow by a cluster instead of failing

`fat32.rs::insert_dir_entry` (the single choke point every
entry-creating operation goes through - `mkdir`/`touch`/`write`/`cp`/
`mv`) now extends a directory's cluster chain when every existing slot
is taken, claim-then-zero-then-link ordering so a partial failure never
corrupts the chain; `Error::DirectoryFull` is deleted (unconstructable).
The real correctness piece: `rmdir` freed exactly one cluster - correct
only while directories were single-cluster by construction - and would
have silently leaked extension clusters; fixed with a shared
`free_chain` helper that also deduplicated `rm`'s and `write_file`'s
identical existing loops. Confirmed organically on QEMU (the test
image's 512-byte clusters fill after 14 entries): 20 files in one
subdirectory, root-directory extension, content round-trip on an
extended-cluster entry, reboot persistence, `rmdir` of the two-cluster
directory, and freed-cluster reuse - zero aborts.

## ESP directory renamed to `\EFI\ORBS\`, and a project logo

The ESP directory became `\EFI\ORBS\` (was `\EFI\OUROBORO\` - both
exist because the full 9-character project name exceeds FAT's 8.3
short-name limit and `fat32.rs` doesn't parse LFN entries; `ORBS` is
the tidier abbreviation). `loader.rs`'s `CONFIG_PATH`, the Makefile's
`esp` target, and current-state docs updated together; confirmed
booting on QEMU (including a real `exec /EFI/ORBS/SH.BIN`) and real
Parallels hardware. The project also gained a logo (`logo1.png`
source at the repo root; resized copies on the website and README,
plus a real favicon replacing the per-page emoji ones).

## xHCI multi-device support - every connected device enumerated, classified, and kept addressed

The keyboard driver's one-port/one-device/one-slot scope is lifted -
the named prerequisite for the USB mass-storage milestone. The
all-in-one `Device` struct split into controller-global state
(`Xhci`), per-device slots with their own EP0 rings and Output Device
Contexts from 4-entry pools (the two statics that were only safe
while the old scan abandoned every non-keyboard), and `KeyboardState`.
The scan enumerates every connected port, logs each device's
interface class/subclass/protocol (with a mass-storage class-`0x08`
callout), keeps non-keyboards addressed, activates the keyboard only
after the scan completes, and routes transfer events by slot ID +
endpoint DCI. Confirmed on a new QEMU three-device rig
(`make run-usb-multi`: keyboard + tablet + storage - the storage
stick classified exactly `0x08`/`0x06`/`0x50`, Bulk-Only Transport)
and on real Parallels hardware (virtual mouse + keyboard concurrently
addressed, typing clean).

## Parallels disk diagnostic - no documented storage controller exists on this platform

A diagnostic round (no driver written - that was the point) settled
the Parallels disk question: with the VM's `sata:0` boot disk attached
and bootable, the PCI bus shows no storage controller of any kind, and
scratch disks deliberately attached as `scsi`/`lsi-sas` then
`lsi-spi` are equally invisible (`buslogic` is rejected by Parallels
itself as EFI-incompatible; `ide`/`scsi` are the only interfaces
prlctl offers ARM64 EFI VMs). All storage flows through a proprietary
non-PCI path - "implement a documented spec" is not available for any
attached-image disk there. The one documented lead left: USB mass
storage over the existing xHCI driver (a first check with a USB 2.0
stick found Parallels routes USB 2.0 passthrough to the EHCI
controller instead; a USB 3.x stick is the pending retry). A permanent
diagnostic improvement fell out: `pci::log_all_devices` returns its
inventory and `main.rs` re-prints it through the post-exit console,
since boot reaches the shell ~2 seconds after power-on - far too fast
to read the UEFI-console rendering. See the roadmap's "Disk on real Parallels hardware" entry.

## Output redirection (`>`/`>>`) - pure shell-side, zero kernel changes

`cmd > file` (create/overwrite) and `cmd >> file` (append) work for
every builtin, composed entirely from the existing `fs_read_file`/
`fs_write_file` syscalls the same way `cp` was - no new syscall, no
kernel changes. `run_line` (`shell/src/main.rs`) peels a trailing
redirect off the line before dispatch; command output flows through an
explicit `Output` sink passed down to handlers (error messages
deliberately stay on the console - the POSIX stdout/stderr split);
`>>` is shell-side read-concatenate-rewrite, bounded by the kernel's
512-byte per-syscall buffer cap. That cap found a real bug in testing:
a 1024-byte append buffer failed `valid_user_range` in a way
indistinguishable from "no such file", silently turning append into
overwrite - fixed by sizing the buffer at the cap. Confirmed on QEMU
(overwrite/append/create-empty, reboot persistence, both overflow
refusals - one organically, via six consecutive appends - and every
error case, zero aborts) and on real Parallels hardware (the `NO_FS`
path; typing `>` there needed a real `test-parallels.sh` extension - a
held-Shift scancode chord via `prlctl`'s `--event press/release`).
See `shell-commands.md`'s user-facing reference.

## Keyboard input routing: one designated owner task, not first-blocked-wins

Found live by the user immediately after `exec` shipped: with two
concurrent shells, keystrokes split unpredictably between them
(`uptime` arriving as `ptime` + a stray `u`), because `on_tick`'s
wake-check polled every `Blocked(Keyboard)` task in index order and
the underlying poll destructively consumes a byte for whoever asks
first. Fixed with `INPUT_OWNER_TASK` (hardcoded to task 0, the
boot-loaded shell - always valid since no task destruction exists):
the wake-check now skips keyboard polling for every other task, which
simply stays blocked, a genuine background task rather than a second
terminal racing the first. Confirmed by re-running the exact live QEMU
scenario that exposed it, plus a real-Parallels regression pass (this
wake-check is the only keyboard path there).

## Dynamic task creation and `exec` - a real `spawn` syscall

A new `spawn` syscall (16) loads a program from disk at runtime and
starts it as an independent task alongside the caller (deliberately
spawn semantics, not POSIX exec-replaces-current-process, though the
shell command is named `exec`). Needed: a runtime physical-page bump
allocator (nothing could hand out RAM after boot services exited), a
re-callable `mmu::install_identity_map` (stashed memory map +
extra-device list, `rebuild_with_el0_regions`), the scheduler grown
from 2 fixed slots to 4 (`TaskState::Unused`), and `loader.rs`'s ELF
core split (`elf_region_size`/`populate_region`) so the same parsing
serves boot-time and runtime loads. A real bug found by testing:
`Vec`-based program-header parsing *hangs silently* when first reached
from the runtime path - the global allocator is boot-services-backed
and invalid after exit, and misuse doesn't fault, it just hangs -
fixed with a fixed-capacity array. Confirmed on QEMU end to end (a
second shell instance genuinely running concurrently); on real
Parallels hardware only the error path is reachable (no disk driver
exists there at all - pre-existing gap), confirmed clean. See `architecture.md`'s process-model section.

## Blocking primitives - tasks can really wait, and a second SVC-frame bug

`READ_CHAR` (15) is the first genuinely blocking syscall: instead of
the shell busy-polling `try_read_char` every scheduler slice, the task
is suspended (`TaskState::Blocked(WaitReason::Keyboard)`) and
`on_tick`'s wake-check resumes it with the byte already in `x0` once
one arrives. Deliberately never executes `wfe` anywhere (real
Parallels hardware has a confirmed unresolved hang for EL0 `wfe`).
Testing this immediately exposed a second real, pre-existing
`exceptions.rs` bug (after the relocating-loader milestone's `x9`
clobber): the SVC trampoline's saved frame didn't match `Context`'s
real field layout and never saved `SP_EL0` at all - harmless while
every syscall resumed its own caller, fatal the moment a blocking
syscall loaded a *different* task's context through it. Fixed to match
the IRQ trampoline's proven layout. Confirmed on QEMU (a real
4-second block with zero wake events, then instant wake on input) and
on real Parallels hardware.
section.

## A real relocating loader - and a pre-existing SVC-trampoline bug it surfaced along the way

**The goal:** replace the flat, position-*dependent* userland-program
loader with a real ELF64 loader that parses `PT_LOAD` segments and
processes `R_AARCH64_RELATIVE` self-relocations against wherever a
program actually loads - the documented fix (`roadmap.md`) for this
project's single most-repeated bug class: `core::fmt`'s argument-
dispatch table and slice/literal comparisons both crashing for the
identical reason (an absolute data pointer baked in for a link-time
base of `0x0` that never matches the real runtime load address).

**Delivered:** `kernel/src/loader.rs` now hand-rolls a real ELF64
parser (header, program headers, section headers, `Elf64_Rela`
entries) and applies every `R_AARCH64_RELATIVE` relocation it finds in
`.rela.dyn` against the real load address; `LoadedProgram` gained a
real `entry` field, used by `tasks.rs` instead of assuming it equals
`base`. `shell`'s toolchain switched to `relocation-model=pic` + `-pie`
+ `--no-dynamic-linker` (`.cargo/config.toml`), `shell/linker.ld` gained
`.rela.dyn`/`.dynsym`/`.dynamic`/`.data.rel.ro` output sections, and the
Makefile's `shell-bin` target now hardcodes `--release` for userland
program builds - a real, confirmed toolchain constraint, not a style
choice (a debug build fails to *link* at all, due to an
`R_AARCH64_ABS64` relocation inside prebuilt `libcore`'s own object
code). The shell gained a permanent `selftest` builtin proving both
previously-crashing patterns - `write!`/`core::fmt` and a slice-vs-
literal comparison - now work correctly.

**A second, genuinely important finding along the way:** a real,
pre-existing bug in `exceptions.rs`'s SVC trampoline, latent since the
syscall boundary was first built and never triggered before this
milestone's different register allocation happened to expose it - the
EC check at the top of the SVC vector slot clobbers `x9` *before* the
trampoline's own save sequence gets a chance to preserve it, silently
discarding whatever value userland had live in `x9` at the moment of
`svc`. Root-caused via a direct register-survival probe (raw inline
`asm!`, not guessed) and fixed by saving `x9` to a scratch stack slot
before the EC check runs.

**Confirmed working end to end** via the same piped-stdin QEMU
technique as every prior milestone, against both `make run`'s FAT16
vvfat and the real FAT32 `esp.img`: `selftest`'s three checks all pass;
`help`/`echo`/`uptime` (a genuinely multi-digit tick count, the exact
previously-crashing shape) all produce correct output; the full disk-
command surface round-trips correctly. Zero aborts in `-d int`
cross-checks. **Confirmed on real Parallels hardware too**, via `make
test-parallels`: `selftest`'s three checks all passed identically, and
`uptime` printed a genuine 4-digit tick count with no crash - direct
real-hardware confirmation of the `x9` fix specifically.

## xHCI busy-waits switched from iteration-bounded to time-bounded - a real bug the user found, not this project's own testing

**The goal:** none going in - this was a live bug report. Booting the
real `esp.hdd` normally in Parallels (a manually-launched, live VM
window - not `make test-parallels`, which drives Parallels' own
synthetic keyboard headlessly) produced `xhci: keyboard not available
(Command ring: timed out waiting for a completion event)`, something
none of this project's own real-hardware testing had ever hit.

**Delivered:** every busy-wait in `kernel/src/xhci.rs`
(`wait_command_completion`, `wait_transfer_event`, the port-scan loop,
`poll_until`) switched from a fixed iteration count (`POLL_ITERS`) to a
genuine wall-clock deadline, using the ARM generic timer's free-running
counter (`CNTPCT_EL0`/`CNTFRQ_EL0` - pure system-register reads,
needing no GIC or interrupts, the same property `timer.rs` already
relied on for its own timer setup). `timer.rs` gained a new
`pub(crate) fn now_ticks()` for this reuse.

**Root cause:** a fixed iteration count is only a valid stand-in for
real elapsed time if the host never stalls the guest's vCPU for any
real duration while it spins - untrue under a real hypervisor. The
strongest evidence: reproducing the bug a second time showed a
*different* xHCI command timing out than the first attempt (very early
port setup, then much later at what's almost certainly the interrupt
endpoint's `Configure Endpoint` command) - a pattern that indicts the
wait mechanism itself, not any one command's logic. The most likely
trigger: a live-rendered VM window competing for real host CPU/GPU
time, something none of this project's own headless scripted testing
ever does.

**Confirmed fixed by the user directly**, on the exact real-world
scenario that originally failed - not a scripted reproduction. Also
regression-tested on QEMU and via this project's own `make
test-parallels`, neither of which had ever reproduced the original bug
but both confirm no regression to the already-working path.

## MADT/GICv3: real interrupt-controller discovery for Parallels - full preemptive multitasking confirmed working end to end

**The goal:** replace `gic.rs`'s old QEMU-devicetree-derived GICv2
addresses (already confirmed unsafe on real Parallels hardware) with real ACPI MADT discovery, and add a
GICv3 driver so Parallels - which almost certainly runs GICv3, not
GICv2 - can actually reach it.

**Delivered:** new `kernel/src/madt.rs` (MADT parsing: GICD/GICC/GICR
structures, cross-checked against Linux's `actbl2.h`), `kernel/src/gicv3.rs`
(a real GICv3 backend - system-register CPU interface, per-CPU
redistributor discovery and wake-up), and `kernel/src/gic.rs` turned
into a version-dispatch facade over `gicv2.rs`/`gicv3.rs` so `main.rs`/
`exceptions.rs`'s call sites barely changed. Confirmed on QEMU two
independent ways (a devicetree dump and the real MADT parse agreeing
exactly, both for default GICv2 and a newly forced `-machine
virt,gic-version=3`, `make run-gicv3`) before ever risking real
hardware. Two real GICv3 bugs found and fixed on QEMU first - a PPI
defaulting to Group 0 (FIQ) instead of Group 1 (IRQ) without an
explicit `GICR_IGROUPR0` write, and `GICD_CTLR` needing more than
GICv2's single enable bit - both cross-checked against Linux's own
`gic_cpu_init`/`gic_dist_init`.

**On real Parallels hardware:** MADT discovery confirmed clean and safe
(`GIC V3, GICD @ 0x2410000, GICC/GICR @ 0x2500000`, genuinely different
addresses from QEMU's - resolving whether Parallels' MADT describes an
interrupt controller at all, previously an open question given its
absent SPCR). GIC/timer IRQ delivery itself is conclusively confirmed
working there too - a real, correctly-incrementing `uptime`
(`533` -> `752` ticks in one observed run), isolated via a
single-variable diagnostic (temporarily skipping just the task-switch
call while leaving GIC/timer otherwise fully active).

**A second, separate, real bug found in the process - root-caused and
fixed the same session.** The actual task switch (`tasks::on_tick`)
hung the system outright the first time it ran on real hardware - this
exact interrupt-delivery-plus-context-swap combination had never
executed on real hardware before (Parallels had no working GIC/timer at
all until this milestone). A single-variable diagnostic isolated it to
the task switch specifically, not GIC/timer delivery (confirmed solid
on its own). Leading suspect: task 1's idle loop, `wfe` - real
hardware's `wfe` may be trapped/emulated by the host hypervisor in a
way QEMU/TCG's never is. Swapping the idle loop for a plain busy-spin
and re-testing confirmed it: the hang was completely gone, verified by
a sustained interactive test with a correctly, continuously
incrementing tick count throughout. Task switching is now
unconditionally enabled on every platform - preemptive multitasking
works on real Parallels hardware for the first time ever, with zero
change to QEMU's behavior (retested end to end there too, both GIC
versions, no regressions). A real, secondary, minor finding along the
way - also root-caused and fixed the same day: an occasional dropped
keystroke under active task switching, traced to a genuine logic bug in
`xhci.rs::Device::poll_key` (not a hypervisor timing quirk) - a single
polled report can legitimately carry more than one newly-pressed
keycode at once, and the original code only ever translated and
returned the first one, silently discarding any second forever. Fixed
with a small `pending` buffer draining every qualifying keycode from a
report. Confirmed fixed on real Parallels hardware: ten consecutive
`uptime` invocations back to back, zero drops.

**Made practical by a new discovery the same day:** Parallels Desktop's
own CLI, `prlctl`, can script an entire real-hardware test round trip
(boot, type via `send-key-event`, screenshot via `capture`) with no
human watching the VM live - now `make test-parallels`
(`scripts/test-parallels.sh`). Every real-hardware round trip in this
milestone went through it. See `roadmap.md`'s "Testing infrastructure"
section.

## USB HID keyboard driver: confirmed - real, physical keyboard input on real Parallels hardware, first time ever

**The goal:** a real input path for Parallels, closing the gap the GOP
framebuffer console milestone left open (write-only, no keyboard driver
at all). A from-scratch xHCI driver (`kernel/src/xhci.rs`): capability/
operational register bring-up, command ring, event ring, device slot
enable/address over control transfers, and - the mechanism that turned
out to actually be required - a real interrupt IN transfer ring.

**Five independently-confirmed real-hardware bugs, none visible on
QEMU**, each found by direct evidence rather than guessing:

1. A PCI Command register bit-position error - `CMD_MEMORY_SPACE` was
   `1 << 0` (I/O Space Enable) instead of `1 << 1` (Memory Space Enable),
   found by decoding an observed before/after register dump
   (`0x0010 -> 0x0015`) that showed I/O Space + Bus Master set, never
   Memory Space. Explained every earlier "nothing responds" symptom on
   both QEMU and real hardware at once.
2. A genuine firmware panic on real hardware -
   `PANIC@11.28 UEFI-exception-ArmPciCpuIo2Dxe.dll`, decoded from
   Parallels' own hypervisor crash log - from a PCI config-space
   BAR-reassignment write (a standard technique, needed to work around
   QEMU's own OVMF build leaving this device's BAR completely
   unassigned) that real PCIe firmware doesn't tolerate the way QEMU's
   software model does. Fixed by never writing to PCI config space
   beyond the one narrow Command-register enable.
3. The discovered BAR (`0x8000004000` on QEMU) landed outside the
   identity map's original single-L0-table-entry span (a real
   simplifying assumption from early in this project, never revisited
   until a real PCI BAR needed an address outside the first 512GB).
   Fixed by generalizing `mmu.rs` to allocate further top-level table
   entries on demand.
4. The deepest finding: Parallels' USB passthrough doesn't forward HID
   *class* requests (`SET_PROTOCOL`, `GET_REPORT`) to the real device at
   all - confirmed by a live, correct `GET_DESCRIPTOR` *standard*
   request returning Parallels' own real registered USB vendor ID
   (`0x203a`) right next to a `GET_REPORT` that kept echoing this
   driver's own Setup packet back (byte-for-byte, tracked exactly across
   a changed request length - not a coincidence). Fixed by switching to
   a real interrupt endpoint, armed via the standard `Configure Endpoint`
   xHCI *command* (not a class request), the same mechanism every
   production USB HID driver actually uses at runtime.
5. Once the interrupt endpoint was delivering real live data, it turned
   out to be reading Parallels' virtual *mouse*, not the keyboard - this
   driver's port scan had just grabbed the first connected device.
   Fixed by scanning every connected port and checking each device's
   actual HID interface protocol (`bInterfaceProtocol=1`, Keyboard)
   before configuring it.

**Confirmed working end to end on real Parallels hardware, not just
QEMU:** typed a full command line (`abc`), used backspace, pressed
Enter, got the shell's real `unknown command` response - the complete
keyboard-to-shell round trip, on a real physical USB keyboard.

Full technical write-up - including the debugging techniques that found
each bug (poisoned DMA buffers, widening a request to break a suspicious
size coincidence, using a known-good standard request as a control,
decoding raw exception registers by hand) - in
[`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md), written to
be useful to other bare-metal-OS developers hitting the same class of
problem, not just as this project's own history.

**Still coarse, worth knowing before building on this:** one port, one
device, one slot, no hot-plug, no hubs, no real HID report-descriptor
parsing (boot-protocol's fixed 8-byte layout assumed directly, and
`SET_PROTOCOL` almost certainly still isn't reaching the device either -
this only works because the real keyboard happens to use the same
simple layout regardless), no stall recovery on the interrupt endpoint
specifically (EP0's setup-time control transfers do recover from a
Stall), only the first matching interrupt IN endpoint is ever
configured. Preemptive multitasking is still unavailable on Parallels
(a separate, already-tracked gap - see `roadmap.md`).

## GOP framebuffer console: confirmed - a real, working shell prompt on real Parallels hardware, first time ever

**The goal:** with the broadened `qemu_device_region_safe` gate in
place, the user tested a fifth time. Success - the complete predicted
sequence reached on real hardware with no further issues:
`framebuffer console live` → `skipping virtio-blk` → `skipping
GIC/timer init` → `shell ready` → the userland shell's own banner and a
live `$` prompt.

This is the first time this project has ever reached a running,
prompt-displaying shell on real Parallels hardware - the actual goal
the whole console-discovery effort (devicetree/ACPI/PCI, then
virtio-console, then five rounds of GOP-framebuffer-console hardware
testing) was for. Confirmed working on real hardware, not just QEMU:
GOP discovery/mapping, direct-write display visibility with no flush
needed, `fbconsole.rs`'s actual text rendering, the exception handler
reporting through it, the MMU identity map, and EL0 task entry.

**What's not working: keyboard input, a separate, already-known gap.**
The framebuffer console is write-only by design - this kernel has no
keyboard driver at all. The shell is genuinely running, but nothing
typed reaches it yet. A real input path needs a keyboard driver (USB
HID over UEFI, most likely - Parallels' own PCI inventory already shows
real USB controllers present). Preemptive multitasking also isn't
available on Parallels yet (the safety gate disables GIC/timer setup
there), needing real interrupt-controller discovery (ACPI MADT, likely
GICv3) to fix - a separate, substantial follow-up.

## GOP framebuffer console, round four: the GIC crashes too - broadened the safety gate beyond virtio-mmio

**The goal:** with the `virtio_mmio_probe_safe` gate in place, the user
tested a fourth time. `skipping virtio-blk` printed cleanly - that fix
worked - but the boot halted again with a second exception.

- Decoded the same way as the previous crash: `esr_el1=0x96000050`
  gives EC `0x25` (Data Abort, same EL) and DFSC `0x10` (Synchronous
  External Abort - a real bus fault), same signature as before, but
  `far_el1=0x8000000` this time - exactly `gic.rs`'s `GICD_BASE`,
  specifically `gic::init()`'s very first write.
- **A structural finding, not a second instance of the same bug:**
  `gic.rs`'s addresses are the identical kind of QEMU-shaped convention
  as `virtio_mmio.rs`'s - confirmed only via a QEMU devicetree dump,
  never discovered on Parallels. Nothing in this project's fixed
  low-1GB device-region convention has ever been confirmed safe on
  Parallels, only on QEMU.
- One real exception: `timer.rs` is pure system-register access
  (`cntfrq_el0` etc.), architecturally safe on any ARMv8 CPU regardless
  of platform device layout - only the GIC, needed to forward the
  timer's interrupt, depends on the unconfirmed address.
- **Fix:** renamed the gate `qemu_device_region_safe` (from
  `virtio_mmio_probe_safe`) and broadened it to also cover
  `gic::init()`/`gic::enable_interrupt()`, skipped together with
  `timer::arm()` as one block when unsafe. Verified `tasks::start()`
  has no dependency on GIC/timer having run (a straight `eret` into
  task 0's saved context) before shipping this, not assumed - skipping
  this block still reaches a working interactive shell, just without
  preemption (`uptime` would report a static count in this mode, a
  real, minor, documented limitation).
- Re-verified on QEMU, now checking ticks genuinely increase (88 → 214
  a few seconds later) to confirm the normal path still works, not just
  "didn't crash." A forced `discovery=None` + `ramfb` test (the actual
  Parallels shape) showed the complete intended sequence end to end:
  framebuffer console live → clean virtio-blk skip → clean GIC/timer
  skip → shell ready → a real working userland shell prompt. Zero
  aborts. Not yet re-confirmed on real Parallels hardware.

## GOP framebuffer console, round three: it renders real text on Parallels, and directly confirmed the virtio_mmio crash

**The goal:** with the console-fallback reorder in place, the user
tested a third time. Real progress - genuinely readable kernel text
rendered live on real Parallels hardware after `exit_boot_services`, a
first for this project - but the boot still halted, this time with an
exception report as the last thing printed.

- The rendered exception (`EXCEPTION vector=4 esr_el1=0x96000010
  far_el1=0xa000000 elr_el1=0xbba94c68`) decodes to EC `0x25` (Data
  Abort, same EL) with DFSC `0x10` (Synchronous External abort - a real
  bus fault) at `FAR_EL1 = virtio_mmio::SLOT_BASE` exactly - direct,
  decoded proof (not just a strong suspicion) that the virtio-mmio
  magic-value scan crashes real Parallels hardware on its very first
  read.
- The freeze this time was total, not just a lost console: `init_storage()`
  runs unconditionally after the console fallback chain, and
  `virtio_blk::Device::discover()` goes through the identical
  `virtio_mmio::find_device` scan - so the console reorder alone wasn't
  enough, the disk driver's own use of the same unsafe scan was always
  going to hit the same wall moments later.
- **Fix:** a new `virtio_mmio_probe_safe` flag (`true` only when a
  byte-stream console was found via devicetree/ACPI/PCI - the one
  platform this scan has ever been confirmed safe on) now gates *every*
  caller of `virtio_mmio::find_device`, both `try_virtio_console` and
  virtio-blk discovery. `virtio_mmio.rs`'s doc comments were corrected -
  the old claim that unbacked reads "read as 0" on real hardware was
  never confirmed, and is now confirmed wrong.
- Re-verified on QEMU across three scenarios: the normal ACPI-console
  regression pass (virtio-blk still initializes normally, confirmed
  against both FAT16 vvfat and a real FAT32 image with working `ls`/
  `cat`), and a forced `discovery=None` + `ramfb` test simulating the
  actual Parallels shape end to end - framebuffer console live, a clean
  "skipping virtio-blk" message, and a fully working interactive shell
  prompt. Zero aborts across all three. Not yet re-confirmed on real
  Parallels hardware.

## GOP framebuffer console, round two: display-write visibility confirmed, virtio-console MMIO scan reordered out of the way

**The goal:** with the `open_protocol_exclusive` fix in place, the user
tested again on real Parallels hardware. Progress - GOP discovery now
succeeds (`GOP framebuffer @ 0x20000000, 1024x768, stride=1024,
format=Bgr`) and boot reaches further than before - but the screen
still froze, with no framebuffer console output ever appearing.

- **A temporary raw full-framebuffer fill diagnostic** (`0xff` to every
  byte, placed right after the MMU switch, bypassing `fbconsole.rs`/
  `font.rs` entirely) isolated the question that mattered most: does a
  direct write to this physical address actually reach the display at
  all, given that a real GPU might need an explicit flush/present step
  unlike QEMU's `ramfb`. Verified visible on `ramfb` first (solid white
  via QMP screendump), then handed to the user for real hardware.
- **Result: solid white screen on real Parallels hardware** - confirming
  the MMU mapping and direct-write display visibility both work, no
  flush needed. But the screen stayed solid white, frozen - meaning
  execution never reached the actual framebuffer-console rendering.
- **Root cause: `try_virtio_console()` ran immediately afterward, and
  its MMIO scan carries an unconfirmed assumption** -
  `virtio_mmio.rs`'s `find_device` reads a magic-value register at 32
  fixed QEMU-specific addresses, with a comment asserting (never
  actually confirmed) that an unpopulated slot "reads as 0" on real
  hardware. A real bus fabric commonly raises an external abort for a
  read to genuinely unbacked device-memory space instead. With no
  console installed yet at that point on Parallels, a fault there is
  completely silent.
- **Fix: reordered the fallback chain** so the now-hardware-validated
  framebuffer console is tried before `try_virtio_console`, not after -
  justified by independent evidence (`pci.rs`'s device inventory) that
  virtio-console doesn't exist on Parallels at all, so demoting it costs
  nothing there. Re-verified on QEMU: the forced-fallback `ramfb` test
  and the normal ACPI-console regression pass are both unchanged. Zero
  aborts. Not yet re-confirmed on real Parallels hardware.

## GOP framebuffer console fix: `open_protocol_exclusive` was disconnecting Parallels' own boot console

**The goal:** the user tested the framebuffer console (below) on real
Parallels hardware and it still didn't work - a screenshot showed
devicetree/ACPI/PCI discovery and the PCI device dump rendering
normally, then nothing further, not even `framebuffer::discover()`'s own
unconditional success/failure log line.

- Root cause, found by reading the `uefi` crate's own doc comment rather
  than guessing: `framebuffer.rs` opened `GraphicsOutput` with
  `open_protocol_exclusive`, which is specified to forcibly disconnect
  any driver holding the protocol `ByDriver` (the crate's own example:
  "opening the SERIAL_IO_PROTOCOL exclusively will disconnect the
  console driver from it"). On real Parallels hardware, firmware's own
  text console holds GOP `ByDriver` to render the boot screen - so the
  very first call into `discover()` silently killed the visible console
  before anything else could print. QEMU's `ramfb` test never caught
  this because it always ran with `-display none`, so there was no
  console driver attached to GOP to disconnect.
- **Fix:** switched to `OpenProtocolAttributes::GetProtocol` via the
  `unsafe` generic `uefi::boot::open_protocol` - a read-only, non-owning
  open, safe here since `discover()` only reads `ModeInfo`/`FrameBuffer`
  once and never touches the `GraphicsOutput` object again.
- Re-verified end to end on QEMU after the fix: GOP discovery still
  succeeds identically, the forced-fallback screendump still renders
  boot messages and the shell's banner/prompt correctly, and a full
  regression pass on the normal ACPI-console dev loop is unchanged. Zero
  aborts. **Not yet re-confirmed on real Parallels hardware** - that
  remains the next step.

## GOP framebuffer console: a fifth, better-grounded lead for Parallels output

**The goal:** find a real answer for Parallels console output after
virtio-console (below) was confirmed a dead end there. Prompted by the
user asking for research into how other OSes (Linux/FreeBSD) handle a
console on Parallels ARM64, given that Linux is known to run there.

- Deep research (a forked subagent, independently re-verified rather
  than trusted wholesale) turned up two claims. One - a specific PCI
  device ID mapping for Parallels' proprietary "ToolGate" mechanism -
  turned out to be unsourced by its own citation and was discarded. The
  other held up under direct verification: a FreeBSD forum thread
  (https://forums.freebsd.org/threads/parallels-on-macos-apple-silicon-freebsd-14-stuck-on-virtio_gpu.96762/)
  shows a real FreeBSD boot log on Parallels ARM64 with `VT: Replacing
  driver 'efifb' with new 'virtio_gpu'` - direct confirmation that a
  generic UEFI GOP framebuffer (`efifb`) drives early console output on
  that exact platform, and that Parallels' own VM config only offers two
  "video type" options (a proprietary GPU or VirtIO GPU) with no serial
  option at all. This matches this project's own Parallels boot
  screenshots, which already show UEFI graphics output working.
- **New module: `kernel/src/framebuffer.rs`** - discovers
  `EFI_GRAPHICS_OUTPUT_PROTOCOL` (a standard, fully-specified UEFI
  protocol, unlike every previous console mechanism this project has had
  to guess an address or convention for) during boot services, returning
  the framebuffer's physical base/size, resolution, stride, and pixel
  format. Only `Rgb`/`Bgr` (4 bytes/pixel, fixed known layout) are
  supported - `Bitmask`/`BltOnly` are rejected outright, since `BltOnly`
  specifically has no direct-memory-access path usable after
  `exit_boot_services` at all.
- **New module: `kernel/src/font.rs`** - a public-domain 8x8 bitmap font
  (`dhepper/font8x8` on GitHub, itself based on Marcel Sondaar/IBM's
  public-domain VGA fonts), downloaded verbatim via `curl` rather than a
  summarizing fetch specifically to avoid silent transcription errors in
  hex glyph data, then mechanically sliced down to the 95 printable-ASCII
  glyphs (0x20-0x7E) and embedded as a `const` array.
- **New module: `kernel/src/fbconsole.rs`** - a `Write`-implementing text
  console over the raw framebuffer: draws glyphs on a fixed character-cell
  grid, tracks a cursor, and scrolls via a raw `ptr::copy` pixel-row
  memmove directly in the framebuffer (deliberately no text buffer -
  matches this kernel's zero-heap discipline). Write-only: no keyboard
  driver exists in this kernel at all yet, a real gap independent of this
  console.
- **`console.rs` gained a `Framebuffer` variant** and an `is_installed()`
  accessor, so `main.rs` can gate the framebuffer console as a genuine
  last resort - tried only once devicetree/ACPI/PCI *and* virtio-console
  have all failed, since a real byte-stream console (which also gets
  input) is strictly more capable whenever one exists.
- **`mmu.rs::install_identity_map` gained an optional `framebuffer`
  argument.** Most of the time this needs no new mapping at all - on
  QEMU's `ramfb` device, the framebuffer address already falls inside the
  discovered RAM span, so the existing RAM loop covers it for free. Only
  if a framebuffer's containing 1GB block is *still* unmapped after that
  loop does this add one more Device-nGnRnE block for it (same
  convention as the existing fixed low-1GB device block, just at
  whatever address the framebuffer actually reports) - a real
  possibility on hardware this has never been tested against, not yet
  confirmed either way.
- **QEMU testing needed a real display device, and `-nographic` doesn't
  provide one at all** (confirmed by direct testing: `NoGop`). Verified
  instead with `-device ramfb -display none -serial file:...` (a
  RAM-backed, direct-access framebuffer QEMU builds specifically for
  headless use) plus a QMP `screendump` HMP command to capture the
  rendered output as a `.ppm` image for visual inspection - a new
  verification technique for this project, since every prior console
  driver could be checked through its own text output alone.
  `virtio-gpu-pci` was also tried and confirmed to report `BltOnly` under
  this QEMU/OVMF combination - a real, confirmed case of `discover()`'s
  pixel-format rejection actually triggering, not just a theoretical
  branch.
- **Confirmed working, not just "renders something":** a boot-time
  screendump (with a temporary `if true` override, matching this
  project's established technique for testing a fallback that QEMU's own
  ACPI console would otherwise always win over first) showed correctly
  rendered kernel boot messages and the loaded shell program's own
  banner/prompt - proving both `console::println!` and the shell's
  userland `putc` syscall reach the framebuffer, not just kernel-side
  text. A second run added a temporary 90-line print loop to force the
  scroll path: the screendump showed a clean, correctly-ordered scrolled
  view with no corruption or ghosting. Both temporary overrides were
  reverted before committing. Zero aborts in `-d int` cross-checks across
  both runs. A full regression pass on the normal QEMU dev-loop config
  (`-nographic`, real ACPI/PL011 console, piped-stdin `help`/`uptime`)
  confirmed no change in behavior when a byte-stream console exists -
  GOP is still discovered and logged, but the framebuffer console never
  installs, exactly as designed.
- **Still coarse, worth knowing before building on this:** no colour, no
  ANSI escape parsing (the shell's `clear` command's escape sequence just
  draws junk glyphs here instead of clearing the screen); no keyboard
  input at all (this console is write-only, and this kernel has no
  keyboard driver of any kind yet - a gap independent of this milestone);
  the MMU's device-block fallback path for a framebuffer outside the
  discovered RAM span has never been exercised against real hardware,
  only reasoned about; and confirmation against actual Parallels hardware
  is still pending - this was built and verified against QEMU's `ramfb`
  only, the same "confirmed on QEMU, awaiting real-hardware confirmation"
  posture every other console mechanism in this project has gone through
  first.

## Parallels console: virtio-console confirmed not applicable, with real hardware evidence

**The goal:** resolve the one open question the virtio-console milestone
(below) left for the user - does `try_virtio_console` actually find and
drive a device on real Parallels hardware. The user booted the real
`esp.hdd` on real Parallels-on-Apple-Silicon hardware and reported back;
this entry covers what that testing found and the diagnostic built to
make sense of it.

- Real Parallels boot confirmed devicetree/ACPI/PCI 16550 all fail
  exactly as already documented, and showed no visible output at all
  afterward - inherently ambiguous on its own, since the UEFI graphics
  console only renders during boot services, so "the driver ran and
  found nothing" and "the driver ran and hung" look identical on that
  screen.
- The user's separately configured Parallels "Serial Port" device
  (output to a file) received nothing at all, not even boot-firmware
  noise - unlike QEMU, where EDK2 opportunistically mirrors its own
  debug output onto any attached virtio-serial chardev. Inconclusive on
  its own (this firmware build might just not do that), so it ruled
  nothing in or out.
- **New permanent diagnostic: `pci::log_all_devices`
  (`kernel/src/pci.rs`)**, reusing the same boot-services
  `PciRootBridgeIo` walk `discover_uart16550` already proved safe on
  this hardware (reaching `NoSerialDevice` there, not `NoRootBridge`,
  was already proof the walk itself works). Logs every PCI device's
  vendor:device and class:subclass through the still-working pre-exit
  UEFI console, whenever all three normal console-discovery mechanisms
  fail - cheap, read-only, and adds no noise to the normal QEMU boot
  path.
- **The real Parallels PCI inventory this produced**: an Intel HD Audio
  controller, an Intel USB2 controller, an NEC USB3 controller, a
  virtio device (vendor `0x1af4`) with device ID `0x1000` -
  **virtio-net, not virtio-console** (which would be device ID `0x1003`
  or `0x1043`, neither present) - and one unclassified device under
  Parallels' own PCI vendor ID (`0x1ab8`, device `0x4000`, class
  `0xff`).
- **Conclusion: Parallels' serial port is very likely a proprietary
  Parallels device, not virtio-console at all** - no public
  specification exists for it. Reverse-engineering it (blind
  register/BAR probing against an undocumented protocol) was considered
  and explicitly declined - an open-ended task with no guaranteed
  payoff, categorically different from implementing a documented spec.
  A deliberate stopping point, not an assumption.
- A second, smaller finding from the same round of testing: the
  `esp.hdd` "opens as a folder" attachment concern that prompted this
  investigation was never a real problem - the user's Parallels version
  just labels the file-open dialog's button "Open" instead of "Choose."
  Confirmed by the same boot log successfully reading `INIT.CFG`/the
  shell binary off the real disk.

## virtio-console — a real transmit-only driver, confirmed on QEMU

**The goal:** the real lead flagged (and deliberately deferred) for
Parallels console output — `kernel/src/virtio_console.rs`, a fourth
console-discovery mechanism reusing `virtio_mmio.rs`'s existing
transport, modeled directly on `virtio_blk.rs`'s discover/init/one-
virtqueue/poll-based-completion shape.

- Discovery, feature negotiation (`VIRTIO_F_VERSION_1` only), and
  transmitq0 (queue 1) setup, mirroring `virtio_blk.rs` closely.
  Receiveq0 (queue 0) deliberately left unconfigured - transmit-only,
  matching the plan this milestone started from.
- New `Console::Virtio` variant: `write_str` batches through a local
  buffer with `\n`->`\r\n` translation and sends chunked virtqueue
  transfers (a full virtqueue round trip per `Device::write` call makes
  per-byte sends too expensive for whole log lines); `write_byte` sends
  one byte per call (used for the userland shell's character-at-a-time
  output); `read_byte` always returns `None` - no receive path exists.
- **A real placement constraint found while building this, not assumed
  going in:** unlike devicetree/ACPI/PCI (which install their console
  immediately after `exit_boot_services`), virtio-mmio discovery needs
  the device region mapped under this kernel's *own* translation tables,
  which only happens after `mmu::install_identity_map`. `try_virtio_console`
  in `main.rs` therefore runs later than the other three - a real,
  accepted consequence: boot messages between `exit_boot_services` and
  there are lost on a virtio-console-only platform.
- **Verified end to end on QEMU**, using the same kind of temporary,
  documented source-level force this project used to originally verify
  `exceptions.rs` (QEMU's default ACPI-first boot never organically
  reaches this fallback): discovery, feature negotiation, virtqueue
  setup, and real bytes reaching the host chardev all confirmed, `\r\n`
  bytes verified in the raw output via `xxd`, and the normal (unforced)
  boot path re-confirmed unaffected afterward. New `make
  run-virtio-console` Makefile target for future testing. Zero aborts in
  `-d int` cross-checks.
- **A genuine surprise along the way:** `-machine virt,acpi=off`, tried
  first as a way to organically force all three existing mechanisms to
  fail, instead made *devicetree* discovery succeed (this OVMF build
  apparently advertises a DTB when ACPI is disabled) - a real, useful
  finding about this specific firmware configuration, not the forcing
  mechanism actually needed here.
- **Parallels itself remains unconfirmed** - this environment has no way
  to boot real Parallels hardware. Whether Parallels exposes its
  console via virtio-mmio at all (vs. virtio-pci), and at the same
  address range this QEMU-confirmed driver assumes, is a real open
  question only a real Parallels boot can answer.
- **Still transmit-only, no RX**, deliberately - a receive virtqueue
  (symmetrically simpler than transmit) is the natural next step once
  Parallels itself is confirmed reachable this way.

## Phase 8 — `mv`, and the last real correctness risk in the write-support arc

**The goal:** `mv <src> <dst>`, the last cheap command-level win left in
the "close up the easy write-support commands" arc (phases 4-7) before
moving on to bigger work.

- `Fs::mv` reuses the file/directory's *existing* cluster chain rather
  than reading and rewriting content: locate `src`'s entry, insert a new
  entry for `dst` with the same cluster/size/kind, then free `src`'s old
  entry - same "write the new thing before touching the old one"
  ordering as `write_file`'s overwrite path, so a failure partway
  through (most likely `Error::DirectoryFull` on `dst`'s parent) never
  leaves `src` half-deleted. `dst` must not already exist - `mv` refuses
  rather than overwriting it or moving `src` inside it if `dst` happens
  to be an existing directory, narrower than a real `mv`'s full
  semantics.
- **The one real correctness risk, caught by design rather than by a
  crash:** when a *directory* moves to a *different* parent, its own
  `..` entry has to be patched to point at the new parent (or cluster
  `0`, root's convention, if the new parent is root) - otherwise a
  moved directory's `cd ..` would keep resolving to its *old* parent
  forever, silently. This is the identical cluster-`0`-means-root
  convention from phase 3c's `Fs::find`, reapplied on a write path
  rather than reintroduced as a new bug. Reused `patch_entry_cluster_size`
  (phase 6) to make the fix, rather than writing a new low-level sector
  patcher for it.
- One new syscall, `fs_mv` (14, `(src ptr, src len, dst ptr, dst len)`),
  and a new `mv <src> <dst>` shell builtin - the cheapest of the write
  commands to add on the shell side, since it's a single syscall call
  with no buffer/content handling at all, unlike `cp`.
- **Confirmed working end to end, with the `..`-fixup specifically
  exercised, not just reasoned about:** a same-directory rename; a
  cross-directory move of a plain file; a cross-directory move of a
  *directory* containing its own subdirectory, followed immediately by
  `cd`-ing into the moved directory and back out with `cd ..` - which
  correctly landed at the *new* parent, not the old one; renaming a
  directory in place (same parent) and confirming a doubly-moved nested
  directory's own `..` still resolved correctly afterward; destination-
  already-exists, missing-source, and missing-destination-parent all
  correctly rejected with the source left untouched. Persistence
  confirmed by an actual reboot - all of the above state, including the
  moved/renamed directory tree, was still correct on a fresh mount.
  `make run` (FAT16, no mount) still degrades gracefully. Zero aborts in
  `-d int` cross-checks across every session.
- **Still coarse:** no move-into-an-existing-directory-keeping-basename
  semantics (real `mv`'s most common shortcut); no cycle detection (`mv`
  a directory into its own descendant isn't guarded against); same
  8.3-short-name constraints as `mkdir`/`touch` apply to `dst`'s name.

## Phase 7 — `cp`, pure shell-side plumbing over existing syscalls

**The goal:** `cp <src> <dst>`, the next item phase 6's own changelog
entry and the roadmap parking lot both flagged as newly buildable now
that files can hold real content.

- **No new syscall, no kernel changes at all.** `cmd_cp` reads `src`'s
  entire content into a local stack buffer via the existing
  `fs_read_file` (the same syscall `cat` already uses), then writes
  that buffer to `dst` via the existing `fs_write_file` (phase 6) -
  creating `dst` if it doesn't exist, replacing it if it does, same
  semantics as `write`. Pure shell-side composition of two primitives
  that were already there.
- **Copying a file onto itself is safe by construction, not a special
  case.** The read completes in full, into the shell's own buffer,
  before the write ever starts - there's no window where a partial
  write could clobber a read still in progress.
- **Refuses rather than silently truncates when a source file is too
  large for the shell's read buffer** — a genuinely different judgment
  call than `cat`'s, which prints a truncated prefix with a notice.
  `cat` displaying an incomplete file is merely incomplete; `cp`
  producing an incomplete copy would be a *wrong* copy, so it errors
  outright instead.
- Confirmed working end to end against the real `esp.img`: copy to a
  new destination and to an existing one (overwrite), copy a file onto
  itself (content unchanged), copy a real pre-existing file
  (`INIT.CFG`) to a new name, copy into a subdirectory, and the usual
  error set (missing source, destination is a directory, missing
  destination parent) all correctly handled. `make run` (FAT16, no
  mount) still degrades gracefully. Zero aborts in `-d int`
  cross-checks. No new persistence check needed beyond what phase 6
  already established for `fs_read_file`/`fs_write_file` themselves -
  `cp` adds no new on-disk code path, only a new caller of two already
  reboot-tested ones.
- **Still coarse:** copy size is bounded by the shell's read buffer
  (256 bytes, same as `cat`'s); no recursive directory copy, no `-r`
  flag or any flags at all; no `mv`.

## Phase 6 — `write`, the first way to put real content into a file

**The goal:** close the gap phase 5 left open — every file this kernel
could create was permanently zero bytes, since `touch` only ever
produces empty files. `write_file` is the "actual blocker for `cp`,
output redirection, and anything else that needs a file to hold more
than zero bytes" that phase 5's changelog entry and the roadmap parking
lot both flagged.

- `Fs::write_file` creates a file with exactly the given content, or
  fully replaces (not appends to) an existing file's content. Ordering
  matters: the new cluster chain is allocated and written *before*
  anything about an existing file is touched (freeing its old chain,
  patching its directory entry), so a failure partway through never
  frees or unlinks a file that was already there.
- Two new private helpers: `write_chain` (allocates and writes a fresh
  cluster chain for arbitrary-length data, linking each cluster to the
  next via `write_fat_entry` as it goes) and
  `patch_entry_cluster_size` (rewrites just an existing entry's cluster
  and size fields in place, leaving name/attribute/timestamps alone —
  what makes overwriting different from creating).
- One new syscall, `fs_write_file` (13, `(path ptr, path len, data ptr,
  data len)`), and a new `write <file> <words...>` shell builtin that
  joins the remaining words with spaces (same style as `echo`) and
  writes the result as the file's entire content.
- **A real bug found immediately by testing, not by inspection:**
  `write <file>` with no content words (a legitimate "truncate to
  empty" case, exactly matching `touch`'s own empty-file semantics)
  failed with a generic disk error instead of succeeding. Root cause:
  the syscall's argument-sanity check (`valid_user_range`) rejected any
  zero-length buffer, correct for `fs_list_dir`/`fs_read_file`'s output
  buffers (a zero-length destination is pointless there) but wrong for
  `fs_write_file`'s *input* data, where empty is a real, meaningful
  value. Fixed with a second check, `valid_user_range_allow_empty`,
  used only for the data argument (the path argument still can't be
  empty).
- Confirmed working end to end against the real `esp.img`: create a
  file with content, `cat` shows it exactly; overwrite with shorter
  content, `cat` shows only the new content (no stale trailing bytes
  from the longer original); the empty-write fix specifically
  re-tested and confirmed fixed; writing to an existing directory or a
  missing parent both correctly rejected; a genuine reboot-and-remount
  persistence check (write, reboot, `cat` still shows the content); no
  corruption of pre-existing files; `make run` (FAT16, no mount) still
  degrades gracefully. Zero aborts in `-d int` cross-checks across
  every session.
- **Still coarse:** every write is a full replace, no append and no
  partial/offset writes; content is bounded by whatever the caller can
  fit in one buffer (the shell's `write` command specifically is capped
  by its 128-byte input line, so it can only ever produce a single
  FAT32 cluster's worth of content on this test image — `rm`'s
  multi-cluster cluster-chain-freeing loop, added in phase 5, remains
  logically implemented and reasoned-through but still not exercised by
  an end-to-end test, since nothing in this kernel can yet create a
  file spanning more than one cluster). No `cp`, no output redirection
  yet — those need shell-side plumbing this syscall alone doesn't
  provide (see `roadmap.md`'s parking lot).

## Phase 5 — file lifecycle: `touch`/`rm`

**The goal:** round out phase 4's directory-only write support with the
file equivalent — create and remove files, not just directories.

- `Fs::touch` turned out simpler than `Fs::mkdir`: a real FAT32 empty
  file needs no allocated cluster at all (a directory entry with
  starting cluster `0` and size `0` is a valid empty file per spec), so
  it's just one `insert_dir_entry` call, no `find_free_cluster`/
  `zero_cluster`/`.`/`..` writes. Succeeds as a no-op if the file already
  exists (no RTC to update a modification time with, so "no-op" is the
  honest approximation of real `touch`'s behavior there).
- `Fs::rm` mirrors `Fs::rmdir`'s shape, plus one thing `rmdir` never
  needed: freeing the target's entire cluster chain (a loop over
  `next_cluster`/`write_fat_entry`) before freeing its directory entry -
  a no-op for an empty file, but real for anything with content once a
  future "write file contents" syscall exists.
- Two new syscalls, `fs_touch`/`fs_rm` (11/12, path pointer/length only),
  and matching `touch`/`rm` shell builtins.
- **A latent bug caught by reasoning, not by testing:** `Fs::find`'s
  cluster-`0`-means-root substitution (from phase 3c) applied to *every*
  resolved entry, not just directories - harmless before this milestone
  (nothing but a `..` entry ever had cluster `0`), but `touch` was about
  to make "cluster `0`, and it's a file" common. Fixed by gating the
  substitution on `is_dir` before writing any test that could have hit
  it.
- **Still no way to write content into a file** - `touch` only ever
  produces zero-byte files. `cp`/output redirection need a real
  "write file contents" syscall this project doesn't have yet.
- Confirmed working end to end against the real `esp.img`, including the
  same reboot-and-remount persistence check as phase 4, `rm`/`rmdir`
  correctly refusing each other's target (a directory vs. a file), and
  no corruption of pre-existing files.

## A shared syscall-ABI crate

Every "known rough edges"/"next milestone" note since phase 3c had
flagged the same thing: syscall numbers and sentinel values were
hand-duplicated between `kernel/src/syscall.rs`'s dispatch table and
`shell/src/main.rs`'s caller, kept in sync only by convention. Fixed
with a third workspace member, `syscall-abi/` — a plain `#![no_std]`
library crate holding nothing but `pub const` syscall numbers and
sentinel values, no logic. Both `kernel` and `shell` now depend on it
via a path dependency and reference constants directly
(`syscall_abi::FS_MKDIR`, etc.) instead of local, independently-numbered
consts — a future syscall added on one side with the wrong number
literally fails to compile on the other rather than silently
misbehaving at runtime. Confirmed safe against this project's specific
relocation risk (every value is a scalar `u64` const, inlined as an
immediate at the use site under both targets — fundamentally different
from the `core::fmt`/slice-literal-comparison bug, which is specifically
about pointers to literal `.rodata` data).

Also folded in during the same stretch of work: a real UX bug where
every `fs_*` syscall collapsed "no filesystem is mounted this boot" and
"the filesystem is mounted but this operation failed" into the same
`u64::MAX` sentinel, making every disk command on `make run`'s FAT16
disk look identical to a genuinely broken path. Fixed by splitting the
sentinel (`NO_FS`, `u64::MAX - 1`, distinct from `FS_ERROR`) so the
shell can print an explicit "no filesystem mounted" message instead.

## Phase 4 — first filesystem write support: `mkdir`/`rmdir`

**The goal:** cross the write-support line phase 3 deliberately drew, for
the narrowest useful case — creating and removing empty directories — not
the full write surface (`rm`/`touch`/`cp`/`mv`/redirection) at once.

- `virtio_blk::Device` gained `write_sector`, sharing a `submit_request`
  helper with `read_sector` (the only real difference between the two
  requests is the data descriptor's write-flag direction and the
  request-type field).
- `fat32.rs` gained `write_fat_entry` (keeps every FAT copy in sync, not
  just the first — reads only ever needed the first), `find_free_cluster`,
  `zero_cluster`, and `write_raw_entry`, plus `Fs::mkdir`/`Fs::rmdir`
  themselves.
- Two new syscalls, `fs_mkdir`/`fs_rmdir` (9/10, path pointer/length
  only), and matching `mkdir`/`rmdir` shell builtins.
- **Deliberately narrow, not a full write implementation:** no
  directory-extension (a full parent directory makes `mkdir` fail rather
  than growing it), no file creation/deletion, no `cp`/`mv`, and a
  conservative 8.3 short-name character set (ASCII alphanumerics plus
  `_`/`-` only) for names this kernel creates.
- Confirmed working end to end against the real `esp.img`, including a
  genuine reboot-and-remount persistence check (not just a live
  in-memory one), root-removal and already-exists rejection, and
  no corruption of pre-existing files. The on-disk write ordering each operation follows is deliberate
  (claim-before-use for `mkdir`'s cluster allocation,
  check-everything-before-writing-anything for `rmdir`).

## Phase 3 — a fully functional shell with disk commands

**The goal:** `ls`, `cat`, `cd`, `pwd` — a shell that can actually browse
and read the filesystem it's running from, not just talk to the console.

**Why this is bigger than phase 2, and can't be shortcut:** every disk
read so far happens during the UEFI boot-services window, before
`exit_boot_services` — a one-shot, one-way door. There is no way to reach
back into UEFI's filesystem protocol once the shell is actually running;
"disk commands" by definition means reading files *after* boot, which
means finally building the runtime storage stack that's been deliberately
deferred twice already (once in the phase-1 shell milestone, again in the
disk-loading milestone) rather than a shortcut on top of what exists. This
phase is really three dependent stages:

### 3a. A real block device driver

- **Transport: virtio-mmio, confirmed and deliberately chosen over
  QEMU's own default.** Addresses confirmed via the same devicetree-dump
  technique as GICv2/the timer (32 slots, `0xa000000`, `0x200` apart).
  Worth recording since it wasn't the obvious path: a plain
  `-drive ...,media=disk` with no `if=`/`-device` actually auto-attaches
  as **virtio-blk-pci**, not virtio-mmio — reaching that at runtime would
  need this kernel's own PCI/ECAM config-space walk (a real subsystem on
  its own, comparable to writing PCI enumeration a second time, since the
  existing `pci.rs` is boot-services-only). The Makefile now attaches the
  drive as virtio-mmio explicitly instead, sidestepping that entirely.
  Modern (non-legacy) register interface, also deliberately chosen and
  verified via direct QEMU-monitor memory peeks before any driver code
  was written — including a real bug in the diagnostic process itself (a truncated
  monitor read that briefly looked like "no block device exists at all").
  Parallels' own virtio-mmio behavior is still unconfirmed — same open
  question already on record for virtio-console.
- **Driver scope, as built:** device discovery, feature negotiation (just
  `VIRTIO_F_VERSION_1`), one virtqueue, synchronous polling block reads.
  Write support stayed out of scope, as planned at the time (see phase
  4, above).
- Confirmed working end to end: reads sector 0 back and checks the real
  MBR boot signature, not just "no error returned." See
  `kernel/src/virtio_mmio.rs`/`kernel/src/virtio_blk.rs`.
- **Kernel-resident, not a user-space driver process — a deliberate,
  explicit choice, not an oversight.** `docs/research-minix-boot.md`
  raised a real fork here: writing virtio-blk (and eventually
  virtio-console) as an isolated EL0 driver process would be a concrete
  step toward this project's stated microkernel goal, using the driver as
  the forcing function for dynamic task creation and real IPC. Decided
  against, for now — that would pull the process-model/IPC work (see
  `roadmap.md`'s parking lot) into phase 3's critical path, and phase 3's
  actual goal is disk commands working at all. Revisit once there's more
  than one reason to want driver isolation; virtio itself doesn't
  require it.

### 3b. A filesystem reader

- **Target format: FAT32, as planned** — matches `make image`'s
  `hdiutil -fs FAT32` output, what Parallels ultimately boots from too.
  Real surprise along the way: `make run`'s fast dev-loop disk
  (`fat:rw:esp`, QEMU's `vvfat`) turned out to be **FAT16**, confirmed by
  decoding its BPB by hand before writing any parser code (`BS_FilSysType`
  literally reads `"FAT16   "`) — `make run-image` (Makefile target)
  boots the real `esp.img` instead, since `run`'s disk can never satisfy
  a FAT32 mount.
- **Decided: hand-rolled, not a crate** — turned out to be more than a
  style preference: this reader runs after `exit_boot_services`, where
  the global allocator is no longer valid, and every `no_std` FAT crate
  surveyed assumes an allocator is reachable somewhere in its stack. A
  hard constraint, not just precedent.
- **A second real surprise, since resolved:** this project's own
  `\EFI\OUROBOROS\` directory name didn't fit FAT's 8.3 short-name limit
  (9 characters) — real FAT32 formatters handle that with a long-filename
  (LFN) entry this reader doesn't parse. Resolved at the start of 3c by
  renaming the directory to `\EFI\OUROBORO\` (8 characters) rather than
  implementing LFN parsing — this project controls the name, so once 3c's
  actual need (the shell navigating there) was concrete, renaming was the
  cheaper, more honest fix. `fat32.rs` still has no LFN support in
  general; any *other* 9+ character name is still unreachable.
- Confirmed working end to end: lists `\EFI\BOOT`, reads `BOOTAA64.EFI`
  back (a real multi-cluster file, not a single-block special case) and
  checks its exact size and PE header magic against the real built
  binary. See `kernel/src/fat32.rs`.

### 3c. New syscalls + the commands themselves

- **Syscall shape, as built:** `fs_list_dir`/`fs_read_file` (7/8), each
  taking `(path ptr, path len, buf ptr, buf len)` and writing into the
  caller's buffer directly, rather than the originally-sketched
  `open_dir`/`read_dir`/`open_file`/`read_file`/`close` handle-based
  shape — simpler, and sufficient since nothing here needs a persistent
  open handle across multiple calls. This is also what pushed the
  syscall ABI itself from 1 argument to 4 (`x0`-`x3`).
- **All four commands built, all read-only, matching the original
  target:** `ls [path]` (`fs_list_dir`), `cat <file>` (`fs_read_file`),
  `pwd` (shell-local `cwd` state, no syscall), `cd <path>` (shell-local
  state + `fs_list_dir` for validation, no dedicated "exists" syscall).
- **Two real bugs found by testing the actual commands, not by
  inspection** - a slice/string-vs-literal comparison (`cwd_bytes != b"/"`)
  crashed for the same underlying reason `core::fmt` does (a data
  reference computed for link-time base `0x0`), and a FAT32 `..`-entry
  convention (cluster `0` means "root," not root's real cluster number)
  that wasn't handled hung the *entire system* - masked-IRQ syscall
  context, nothing left to preempt a runaway computation. Both fixed;
  `shell/src/main.rs` also gained path normalization (`normalize_path`)
  as a direct result, collapsing `.`/`..` instead of letting `cwd`
  accumulate them literally.

**Deliberately out of scope at the time** (later picked up as phases
4/5, or still open — see `roadmap.md`): any write support (`mkdir`,
`rm`, `touch`, `cp`, `mv`, output redirection — writing to a real
filesystem is a meaningfully bigger risk than reading, and doesn't block
anything read-only commands need); `stat`/`file`/`find`/`tree`; wiring
`loader.rs` itself to use the new runtime driver instead of
boot-services file I/O (boot-time loading already works and has no
reason to change).

## Phase 2 — real commands

`help`, `echo`, `uptime`, `clear`, with `uptime` backed by a real new
syscall (`get_ticks`) rather than being another echo demo.

## Phase 1.5 — the shell becomes a real process

Moved from a kernel-compiled EL0 blob to a genuine separate program,
loaded from disk at boot and selected by a config file — see
[`processes.md`](processes.md). Unplanned at the start of phase 1, but a
prerequisite for phase 2 meaning anything more than kernel demos.

## Phase 1 — interactive echo shell

Real UART input, a line editor, live character echo with backspace/DEL
handling.

## Phase 0 — boot infrastructure

UEFI entry, console discovery (devicetree/ACPI/PCI), exception vectors,
a real MMU identity map, GICv2 + timer-driven preemption, the syscall
boundary, and two-task preemptive round-robin scheduling. See
[`architecture.md`](architecture.md).

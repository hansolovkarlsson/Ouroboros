# The libc arc — a userland C personality, ending at picolibc

*A design retrospective (a twenty-first piece, 2026-08-28). The arc that made
Ouroboros run C: six steps in a span — (1) a first C program, (2) `.data`/`.bss`
loader support, (3) a hand-rolled minimal libc, (4) file I/O + pipe-aware
output, (5) fids (server-side open-file handles), and (6) porting **picolibc**,
the real C library. The headline is that the arc changed essentially no kernel
or loader code — C portability turned out to be a userland personality riding
the machinery that was already there.*

For the milestone facts see `CHANGELOG.md` (steps 1–6); for the day-narrative
see `journal.md`. This is the retrospective — the threads that ran through the
arc and what each cost.

## The spine: portability is a personality, not a kernel property

The POSIX-divergence postmortem (the thirteenth) ended on a claim made with no
code behind it yet: that C portability would come back as a *userland libc
personality*, not a POSIX kernel — the way Fuchsia, MINIX3, and APE run C over
non-POSIX kernels. This arc is that claim executed, and the striking
confirmation is the diff: across all six steps, the kernel and loader were
touched **once**, and that once was a *deletion*. Everything else is new files
under `libc/` and one prebuilt library under `third_party/`. The syscall trap
surface, the loader's ELF parsing, the servers — none of it moved. A C program
spawns like any `/bin` program, talks to `fsd` over the same ninep verbs, and
exits through the same `EXIT`. "libc" here is a client of the OS, not part of
it. That is the whole thesis, and the arc is its proof.

## The `.data`/`.bss` "milestone" was a deletion, not a build

The one kernel-adjacent step (2) is the sharpest instance of the spine. Non-
trivial C needs mutable globals and zeroed `.bss`; the arc's second step was
"add `.data`/`.bss` support to the loader." But the loader *already* copied each
PT_LOAD segment's `p_filesz` initialized bytes and zeroed the `p_memsz −
p_filesz` tail — it had to, to load the Rust programs, whose `.bss` it was
already zeroing. The only thing forbidding C globals was a pair of
`ASSERT(SIZEOF(.data)==0)` / `ASSERT(SIZEOF(.bss)==0)` guards in
`programs/linker.ld`, left there from when the Rust programs were verified to
have none. The "milestone" was removing two assertions and writing a regression
test (`hello.c` mutating a `.data` global and a `.bss` global, confirmed fresh
per spawn). The lesson is one worth distrusting yourself over: when a capability
looks missing, check whether it's actually *forbidden* rather than *absent* —
the loader had done this correctly for months.

## The one real gate throughout: the PIE relocation contract

If the arc had a recurring adversary, it was the same one every `/bin` program
has: the loader applies `R_AARCH64_RELATIVE` and nothing else, so an
`R_AARCH64_ABS64` in the binary is unloadable. `-fPIC` is the C-side of the Rust
build's `relocation-model=pic`, and it is the entire reason any of this works.
It bit at both ends of the arc. In the hand-rolled middle (step 3), the compiler
synthesizes `memcpy`/`memset` for struct and array copies, which show up as
*undefined symbols* if you don't provide them — a few lines each, added when they
first appeared. And at the far end, the *whole viability* of the picolibc port
was a single question: does picolibc, built `-fPIC`, produce zero `ABS64`? It
does — `llvm-objdump -R` showed 22–23 `R_AARCH64_RELATIVE` and nothing else, a
`static-pie` the loader eats unchanged. The port was never gated on picolibc's
size or complexity; it was gated on that one relocation histogram, and `-fPIC`
settled it. (This is the same family as the `&str`-slice PIE trap the Rust
programs keep re-learning — [[reference-str-slice-pie-trap]] — just seen from
the C side.)

## The load-bearing interface was the porting layer — and picolibc proved it by reuse

The most important design decision in the arc is invisible in step 6 because it
was made in steps 3–4: a **narrow waist** of syscall stubs —
`write`/`read`/`open`/`close`/`lseek`/`fstat`/`sbrk`/`_exit` in `libc/src/os.c`
and `file.c`. When the hand-rolled libc was built, those stubs were "just how
our `printf` reaches the console." Their real value showed up at step 6:
picolibc, built with `-Dposix-console=true`, wires its `stdin`/`stdout`/`stderr`
to fd 0/1/2 and bottoms its entire stdio out at `read`/`write` — *the same
stubs*. So the picolibc port needed **no new porting code**. It links picolibc's
`libc.a` against our `crt0` + `os.o` + `file.o` (recompiled against picolibc's
own headers so `struct stat` and the open flags line up), drops our
`stdio.c`/`stdlib.c`/`string.c`, and picolibc supplies the rest. Build the
narrow waist deliberately and both your throwaway libc and a real one plug into
the same holes. Step 5 (fids) is the same lesson in a second key: a server-side
open-file handle is *simultaneously* a POSIX fd and a 9P fid, so the one feature
paid off for C portability and the Plan 9 model at once — the "a POSIX fd ≈ a 9P
fid" deferral from cluster Phase 0 finally cashed.

## The foreign library exposes your platform's edges

A genuinely external dependency is a good stress test of the toolchain, and
picolibc found three host-specific edges the hand-rolled code never could:

- **Modern clang makes implicit-function-declaration a hard error**, which trips
  a couple of picolibc 1.8.9 tinystdio files (e.g. `fcvtl.c` → `fcvtl_r`). Fixed
  by demoting it in the meson cross-file — a warning, not a bug in our code.
- **Apple clang defaults to the Mach-O linker (`ld64`)**, which rejects meson's
  GNU-linker probe (`-Wl,--version` → `ld: unknown options: -Bstatic -EL`). The
  cross-file's `c` binary carries `--ld-path=…/gcc-ld/ld.lld` to force LLD.
- **macOS ships compiler-rt only as Mach-O**, unusable for our ELF link — so the
  two 128-bit shift builtins picolibc's exact-float (ryu) path needs,
  `__lshrti3`/`__ashlti3`, had to be carried ourselves. The subtlety that makes
  this a real trap: you *cannot* implement a 128-bit shift builtin using a
  128-bit shift, because clang lowers a variable-count 128-bit shift *to a call
  to those very symbols* — infinite recursion. They must split the value into
  two 64-bit halves and shift those (thirty lines, compiler-rt's `twords`
  layout).

None of these is deep, but each is the kind of edge you only meet by linking
someone else's real code — the foreign-observer principle (validate against a
genuinely foreign artifact) applied to the toolchain instead of a filesystem.

## The IPC shape leaks into the C runtime

Two runtime bugs came from the gap between "a C `write` call" and "an IPC
message." At step 4, piping a C program's output (`cfile | grep hello`) produced
merged and dropped lines: `printf` emitted one `write(1, &c, 1)` per character,
each a separate `MSG_SEND` to the consumer, and the consumer's mailbox filled
and the naive send *gave up*. The fix was two things the hand-rolled `stdio`
already needed but hadn't been forced to get right: **buffer stdout** (flush on
newline / full / exit, not per char) and **yield-and-retry on a full mailbox**
(`MSG_ERR_FULL`) instead of dropping — mirroring `ulib::pipe_out`. The honest
coda is at step 6: picolibc's `posix-console` stdout is *unbuffered*, so it
reintroduces the one-`write`-per-char shape at the console (correct, because our
`write` still routes it, but chatty — one IPC round trip per character). That's
logged as the arc's one open follow-up: line-buffer it via `setvbuf` or a shim
at the `write` boundary. Buffering is not a stdio nicety here; it is the
impedance match between a byte-at-a-time C API and a message-passing OS.

## Packaging a prebuilt without a build dependency

A smaller but real decision: picolibc needs meson + ninja to build, which a
fresh checkout shouldn't require. The debug `libc.a` is 4.7 MB; stripped of
debug info it is 1.2 MB and still links. So the arc commits the **stripped lib +
headers** (`third_party/picolibc-prebuilt`, ~2.1 MB) and gitignores the 39 MB
source clone and the build trees; `scripts/build-picolibc.sh` regenerates the
prebuilt, and the meson cross-file is committed as a *template* (`@LLVM_BIN@`
resolved from `rustc --print sysroot` at build time) so no host-specific path is
checked in. The rule of thumb: commit the *artifact* a build needs, keep the
*means of reproducing it* in a script, and never commit a machine-specific path.

## The tally

Six steps, one kernel deletion, one new prebuilt library, and a working
`/bin/CPICO` printing `pi=3.14159  e=2.718e+00  g=1.23457e+06` — float
formatting the hand-rolled `printf` never had. What is *done* is the mechanism:
a C program `#include`s the standard headers, calls `printf`/`malloc`/`qsort`/
`strtol`, and runs. What is *not* done — and is genuinely different in kind — is
porting a real application (SQLite, a small C compiler). That is now "port one
more program," not "invent the mechanism," which is exactly the sentence the
POSIX-divergence postmortem predicted the arc would earn the right to say. See
[[reference-redox-os-cousin]] (relibc as the same idea, further along) and
`docs/processes.md`'s "Writing a program in C" for the how-to.

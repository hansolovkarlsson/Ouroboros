# Processes: loading, memory, and writing your own

How Ouroboros gets a userland program from a file on disk into a running
EL0 task, why it works the way it does, and what's involved in writing a
replacement for the default shell. Reference documentation for the current
design — see `CLAUDE.md` for the reasoning trail and what was tried first,
[`CHANGELOG.md`](CHANGELOG.md) for completed milestones, and
[`roadmap.md`](roadmap.md) for what's still ahead.

## Motivation

The shell used to be kernel code: a fixed EL0 poll loop with all its
actual line-editing logic living at EL1, compiled permanently into the
kernel binary. That made it impossible to replace without rebuilding the
kernel, which doesn't match how any real Unix-like system works — the
shell is supposed to be *a program*, swappable through configuration (like
`/etc/passwd`'s shell field on Linux), not a kernel feature.

Getting there needed three things: a way to get a compiled program off
disk and into memory, a way to tell the kernel which program to run, and
enough memory-allocation flexibility to give an arbitrarily-sized program
its own space. All three are covered below.

## Loading mechanism

There is **no runtime disk driver**. `loader.rs` does all its work during
the UEFI boot-services window, before `exit_boot_services` — the same
window the kernel's own binary was loaded in, using UEFI's own FAT32
driver on the ESP (`SimpleFileSystem` protocol via `boot::get_image_file_system`).
This is a deliberate scope decision, not a placeholder: a real
post-boot storage stack (a virtio-blk driver, feature negotiation, a
request queue, plus a filesystem reader) is a genuinely separate,
much larger subsystem — comparable in scope to the virtio-console work
later built for Parallels console output (`kernel/src/virtio_console.rs`,
see `CLAUDE.md`) — and was explicitly *not* what this milestone needed. It gets real once something
needs to load a program *after* boot (dynamic `exec()`-style spawning);
until then, everything that will ever run has to be known and loadable at
boot time.

The sequence, all before `exit_boot_services`:

1. Read `\EFI\ORBS\INIT.CFG` — a config file containing exactly one
   line: the path of the program to load.
2. Read that program's bytes.
3. Allocate an EL0-accessible, 2MB-aligned region sized to fit it (see
   "Memory model" below) and copy the bytes in.

If any step fails, boot fails loudly (a panic through the UEFI logger) —
there's no fallback program, matching this project's established stance on
console discovery: no confirmed data means no guessing, not a silent wrong
answer.

## Configuration

`\EFI\ORBS\INIT.CFG` is deliberately minimal: one line, no key/value
syntax, no comments, just a path (trimmed of surrounding whitespace). The
Makefile's `esp` target writes it automatically, pointing at the built-in
shell. Note the directory is `ORBS`, not `OUROBOROS` — the full project
name is 9 characters, one over FAT's 8.3 short-name limit. The reason is
partly historical now: `fat32.rs` *reads* long filenames today (LFN
reconstruction), so a 9-character name would be reachable — but it still
can't *create* one, so keeping this project-controlled path 8.3 avoids
depending on LFN write support that doesn't exist. (It was first truncated
to `OUROBORO`, exactly 8 characters, then renamed to the tidier `ORBS`.)

```
\EFI\ORBS\SH.BIN
```

To run a different program at boot, either edit this file directly on the
ESP, or point `INIT.CFG` at a different `.bin` staged there — no kernel
rebuild required. This is the actual "replaceable through configuration"
behavior the whole design is for.

The format only grows if something actually needs a second setting —
don't add key/value parsing preemptively.

## Memory model

A loaded program gets one EL0-accessible region: its code (and rodata) at
the base, then one inaccessible **guard page**, then a fixed stack
allowance (currently 8 pages, 32KB), with the stack pointer starting at the
top and growing down - so a stack overflow lands in the guard page and
takes a clean fault instead of corrupting the code below (see "Stack guard
page" in `CLAUDE.md`; the guard has repeatedly caught real overflows, each
growing the stack - the shell's `exec` path forced 8KB->16KB, and the
network server forced 16KB->24KB->32KB as it gained TCP buffers and then
concurrent connections).
Below the guard is a 256KB **raw heap area** the program reaches via the
`heap_info` syscall (a `&mut [u8]`, not a `GlobalAlloc`-backed heap - see
"Binary format" for why `alloc`'s `Vec`/`String` can't be used here) - the
shell uses it to hold a redirect/pipe capture far larger than its stack, so
`cat big > file` works. `.bss`/`.data` are now supported too (see "Binary
format" below) — beyond code, the heap area, and a stack, a program can now
have real static state.

**Why the region is 2MB-aligned even though it's usually only a few KB:**
`mmu.rs` gives a region fine-grained (4KB page) EL0 access by splitting
exactly one 2MB L2 slot into an L3 sub-table. That only works if the whole
region fits inside a single 2MB-aligned slot. `boot::allocate_pages` only
guarantees 4KB alignment, so a multi-page region could easily straddle a
2MB boundary depending on where the allocator happens to put it. Rather
than teaching `mmu.rs` to split a region across multiple L2 slots,
`loader.rs` asks for `size + 2MB` worth of pages, then frees whatever
falls before/after the first 2MB-aligned address in that range — the
region that's left over is guaranteed not to straddle a slot, the same
guarantee the kernel's own compile-time EL0 statics used to get for free
from `#[repr(align(N))]`. This costs up to just under 2MB of
transiently-allocated-then-freed memory per program at boot; with 512MB of
RAM in the QEMU config, that's not a meaningful cost.

Two independent regions exist at once: the loaded program (task 0) and a
small fixed 4KB idle-task stub (task 1, still compiled into the kernel —
see `docs/architecture.md`'s process model section). `mmu.rs` handles up
to two such regions, sized for exactly this case; see its module doc
comment (`MAX_EL0_REGIONS`) for what happens if that's ever exceeded (a
loud warning and a fail-safe EL1-only mapping, not silent corruption).

## Binary format

Real ELF64, self-relocating — not a flat binary anymore. `loader.rs`
hand-rolls a small ELF64 parser (header, program headers, section
headers, `Elf64_Rela` entries — no crate, matching this project's
established discipline of hand-rolling formats simple enough to
justify it, same as `acpi.rs`/`madt.rs`/`fat32.rs`): it walks `PT_LOAD`
program headers to copy each segment's bytes to `region_base +
p_vaddr` (zeroing `p_memsz - p_filesz` past that — a real `.bss` region
falls out of this for free, see below), then finds `.rela.dyn` by name
and applies every `R_AARCH64_RELATIVE` entry it contains against
`region_base`, fixing up every absolute pointer the compiler needed to
bake into data for wherever the program actually landed. See
`CLAUDE.md`'s "A real relocating loader" section for the full history —
this replaced a flat, position-*dependent* loader that copied raw bytes
to a fixed address with no relocation step at all, which was this
project's single most-repeated bug class.

Consequences of "real ELF, real relocations, but still narrowly scoped
— no dynamic linking, no imported symbols, no `exec()`":

- **Entry must still be the first byte, by convention, not necessity.**
  The linker script (`programs/linker.ld`) still sets the link address to
  `0x0` and places the entry symbol first in `.text` via
  `KEEP(*(.text.start))`, keeping `LoadedProgram::entry == ::base`
  trivially true — but `loader.rs` computes `entry` as `base + e_entry`
  from the real ELF header now, so a program's entry point doesn't
  *have* to be its first byte the way it did under the old flat loader;
  this project's own shell just still keeps it that way, out of
  convenience, not obligation.
- **`.bss`/`.data` are now supported** (the loader's `.data`/`.bss`
  milestone). `loader.rs` loads initialized data (`p_filesz`) and zeroes
  `.bss` (`p_memsz - p_filesz`) for every `PT_LOAD` segment, in both the
  boot-load and runtime-spawn paths, re-initialized fresh per spawn; the
  old `programs/linker.ld` ASSERTs that rejected them are gone. So a
  userland program may have mutable statics/globals — a C global or
  file-scope array, or a simple Rust `static mut`. **Caveat for Rust
  programs specifically:** this does *not* lift the separate liballoc
  ceiling — `alloc`'s collections still fail to link on an
  `R_AARCH64_ABS64` in prebuilt liballoc (a different constraint; see the
  heap milestone). Plain scalar/array `static`s are fine.
- **Build with `relocation-model=pic` + `-pie` + `--no-dynamic-linker`**
  (`.cargo/config.toml`'s `[target.aarch64-unknown-none]`), not
  `relocation-model=static` anymore. This makes the compiler emit
  `R_AARCH64_RELATIVE` self-relocations for absolute data pointers it
  can't express PC-relatively (`core::fmt`'s argument-dispatch tables,
  literal references used in certain codegen shapes — see below),
  instead of baking in raw base-`0x0` addresses with nothing to fix
  them up. `-pie` specifically matters: `relocation-model=pic` *alone*
  produces an ordinary static executable with GOT entries silently
  resolved to base-`0x0` addresses at link time — the identical bug,
  one level down. `--no-dynamic-linker` is correct because there
  genuinely is none — no `PT_INTERP`, no imported symbols, nothing
  `ld.so` would normally resolve.
- **Must be built with `--release`, not debug — a hard, confirmed
  toolchain constraint, not a style preference.** A debug build of a
  userland program fails to *link* at all under this model:
  `rust-lld` rejects an `R_AARCH64_ABS64` relocation inside the
  prebuilt (not rebuilt-per-project — see `rust-toolchain.toml`)
  `libcore.rlib`'s own `core::fmt::builders::PadAdapter` object code,
  pulled in by ordinary debug-build panic/bounds-check formatting
  machinery regardless of whether your own code calls
  `write!`/`format!` at all. A release build's optimizer eliminates
  enough of that unreachable-in-practice code that the poisoned object
  never gets linked in. `make shell-bin` already does this — replicate
  it (`cargo build -p <your-crate> --target aarch64-unknown-none
  --release`) for any new program.
- **Some libcore code paths break the link even in `--release` — a
  small, growing list of known instances of the same underlying
  constraint.** Confirmed cases: `&str[a..b]` range slicing of a `str`
  (its out-of-bounds panic path, `slice_error_fail`, formats the
  offending string — use non-panicking `.get(a..b)` instead; found
  during the output-redirection milestone), and `str::rfind` with a
  `char` pattern (pulls libcore's `memrchr`, whose prebuilt non-PIC
  object carries `R_AARCH64_ABS64` relocations outright — use a manual
  reverse byte scan; found porting `fat32.rs` into the filesystem
  server). Byte-slice (`[u8]`) indexing, `find`/forward `memchr`,
  `copy_from_slice`, and `core::fmt` itself are all confirmed fine.
  The failure is loud (a link error naming `R_AARCH64_ABS64`), never
  silent — when you hit a new one, replace the libcore call with a
  hand-rolled equivalent and add it to this list.
- **The whole `alloc` crate is unusable** for the same reason, and there's
  no hand-rolled-around-it fix: prebuilt lib`alloc`'s own `.rodata`
  (anonymous const data that `Vec`/`String`/`Box` pull in unavoidably)
  carries `R_AARCH64_ABS64` relocations, so `extern crate alloc` +
  any collection fails the `-pie` link. The only fix is `-Z build-std`
  (rebuild lib`alloc` with PIE flags), which is nightly-only and off-limits
  on this stable project — so **no `Vec`/`String`/`Box`**. The userland
  heap is a *raw buffer* (`heap_info`), not a `GlobalAlloc` heap, because
  of this (found by the go/no-go gate that opened the userland-heap
  milestone).
- **`core::fmt` (`write!`, `{}` formatting) is safe now — confirmed,
  not just reasoned about.** Historically unsafe here for the identical
  reason the old flat loader made *any* absolute data pointer unsafe
  (`core::fmt::Arguments` builds its per-argument dispatch out of a
  function-pointer array in `.rodata`, not direct `bl` calls) — but
  that mechanism is exactly what real relocation processing fixes. The
  shell's `selftest` builtin (`shell/src/main.rs`) exercises `write!`
  over a small `core::fmt::Write` wrapper around `putc` specifically to
  prove this, and it works. `print_u64_decimal` is left as hand-rolled
  decimal formatting anyway — not because it has to be, just because it
  already existed and doesn't need `core::fmt`'s machinery for
  something this simple.
- **Comparing a slice/string against a literal is safe now too, same
  reasoning, also confirmed via `selftest`.** The historical crash
  (`cwd_bytes != b"/"` in `cd`'s old path-resolution code) was the
  identical class of bug — a reference to literal `.rodata` data needing
  a relocation that never happened under the old model. `resolve_path`'s
  `is_root`/`is_dot`/`is_dotdot` helpers still use scalar comparisons
  (not because it's still required, just because they were already
  written that way and there was no reason to change working code as
  part of this milestone).
- **Still no dynamic linking or imported symbols.** Exactly one
  relocation type is supported (`R_AARCH64_RELATIVE`) — anything else is
  a hard loader error, not silently ignored, since this project has no
  shared libraries and nothing else should ever legitimately appear in
  `.rela.dyn`.
- **A second (and up to a fourth) program can now be loaded and started
  at runtime, without a reboot** — the shell's `exec <path>` command,
  backed by a new `spawn` syscall (`SPAWN`, 16). This is genuinely
  `tasks::spawn` (adds a new, independent task alongside the caller),
  not POSIX exec-replaces-current-process — the caller keeps running.
  See `docs/architecture.md`'s "Dynamic task creation" section and
  `CLAUDE.md`'s "Dynamic task creation and `exec()`" section for the
  full design and the real `Vec`/allocator hang bug found building it.
  A task can also *end*: the `exit` syscall (17) destroys the calling
  task, frees its slot for a future `spawn`, unmaps its region, and
  returns its RAM to the runtime allocator when LIFO order allows -
  see `hello/`, the second real userland program, whose whole job is
  printing a banner and exiting (the reference for how a program ends
  itself; the boot shell, task 0, is refused - it's the sole keyboard
  owner).

## Syscall ABI available to a program

Every syscall number and sentinel value below lives in the `syscall-abi`
crate (`syscall-abi/src/lib.rs`), a third workspace member both the
kernel and any userland program depend on directly - add it as a
dependency in your program's `Cargo.toml` (see the default shell's for an
example) and use `syscall_abi::FS_MKDIR` etc. rather than hand-copying
numbers. See `docs/architecture.md`'s syscall table for the full list.
`try_read_char` (`TRY_READ_CHAR`, non-blocking, returns `NO_CHAR` when
nothing is waiting), `read_char` (`READ_CHAR`, blocking - suspends the
calling task and switches to another runnable one instead of returning
immediately, resuming with the byte once one arrives; see
`docs/architecture.md`'s "Process model" section for how blocking
actually works) cover interactive input. **A `/bin` program *can* read the
keyboard** now (a pager, an editor, a REPL): the shell hands a foreground
command keyboard ownership at spawn, so `read_char` returns real keystrokes;
`ulib::read_char()` is the wrapper, and `programs/shellutils/readkey` is a
minimal working example. Ctrl+C **terminates** a foreground program (the kernel
kills it and returns to the shell) — it's not delivered as a catchable signal
yet. A *background*/piped program never owns the keyboard, so `read_char` there
blocks forever; a pipeline consumer reads its stdin as messages (below), not from
the keyboard. **Convention:** a program that takes arguments should call
`ulib::usage_if_requested(b"usage: ...")` as the first line of `_start` - it
prints the usage and exits when invoked with `-?`, the uniform help flag every
arg-taking command (builtin and `/bin`) honors. **Output goes to the console
server, not straight to the kernel:** a program builds its text into a
`DSPOP_WRITE` message and sends it to the console server (task 3,
`CON_TASK`) via `msg_call`, falling back to `putc` (`PUTC`, one raw byte)
only if no console server is loaded this boot. Every userland program
here carries the same small `con_write` helper for this (copy the
shell's or `hello/`'s); it's what moved console *rendering* out of the
kernel (see `docs/architecture.md`'s "The console server"). `get_ticks`
(`GET_TICKS`, added for phase 2's `uptime` builtin) is the pattern to
follow whenever a command needs real kernel state it can't get any other
way.

**File operations are not syscalls anymore** — they're IPC requests to
the filesystem server (`fsd/`, task 2), sent as `FSOP_*` messages via
`msg_call` (see `docs/architecture.md`'s "The filesystem server" and
the protocol notes under its syscall table). The old eight `fs_*`
syscalls' numbers (7-14) are deliberate gaps now; their exact contracts
survive unchanged as the protocol ops, and the shell's `fs_call`/
`fs_list_dir`/`fs_read_file`/... wrapper functions
(`shell/src/main.rs`) are the reference client to copy — a request is a
header (op + four little-endian u64 params) plus an inline payload
(path, then data), the reply a status u64 plus an inline result, all
copied task-to-task by the kernel (the payloads are *inline*, not
pointers — per-task page tables make a client's memory unreadable by
the server; see `docs/architecture.md`'s protocol section).
Two failure values every client should know: `NO_FS` (`u64::MAX - 1`)
means no filesystem is available (nothing mounted, or no server loaded
this boot — the wrappers fold `msg_call`'s no-such-task answer into it),
and any value `>= FS_ERR_MIN` is an error, with one named `FS_ERR_*`
code per real reason (not found, already exists, disk full, ...).

`spawn` (`SPAWN`, 16) starts a staged program image as a new,
independent task — since the kernel has no filesystem, the caller first
reads the program via the server (`FSOP_READ_AT`, 512 bytes per round
trip) and feeds each chunk through `spawn_stage` (30) into the kernel's
staging buffer; see the shell's `cmd_exec` for the reference flow and
`docs/architecture.md`'s "Dynamic task creation" section for the
kernel-side mechanics. A userland program makes these directly via `svc`:

```rust
#[inline(always)]
fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}
```

The numbers/constants themselves come from `syscall-abi` (see above), not
hardcoded separately in `kernel/src/syscall.rs` and `shell/src/main.rs` -
see "Known rough edges" below for what this still doesn't cover (pointer/
length validation, per-error-reason detail).

**Pipeline filter programs** are the other program shape worth
knowing (see `docs/shell-commands.md`'s "Pipelines"): stdin is
`msg_recv` (1-64-byte data messages, then one *empty* message meaning
end-of-stream), stdout is `con_write` (routed to the console server, as
above), and finishing means `exit` - the shell waits on the slot.
`upper/src/main.rs` is the ~80-line reference to copy. No argv exists; a
filter's behavior has to be baked in.

## Writing a replacement program

The default shell (`shell/`) is a real, if minimal, example to copy. To
write your own:

1. **New crate**, `no_std` + `no_main`, built for `aarch64-unknown-none`
   (already an installed target — see `rust-toolchain.toml`).
2. **Copy `programs/linker.ld`** as-is, or adapt it — the constraints above
   (entry first, no `.bss`/`.data`, the `.rela.dyn`/`.dynsym`/`.dynamic`/
   `.data.rel.ro` sections LLD needs for a well-formed self-relocating
   `ET_DYN`) apply to any program loaded this way, not just the shell.
3. **Add a target-specific `rustflags` entry** in the workspace's
   `.cargo/config.toml` if your linker script lives somewhere other than
   `programs/linker.ld` (the existing `[target.aarch64-unknown-none]` section
   points at a fixed path).
4. **Write `_start`**, marked `#[no_mangle]` and placed in a
   `.text.start`-linked section so the linker script's `KEEP` picks it up:
   ```rust
   #[no_mangle]
   #[link_section = ".text.start"]
   pub extern "C" fn _start() -> ! {
       main()
   }
   ```
5. **No global mutable state** — anything you'd normally reach for a
   `static mut` or a non-const `static` for needs to be a local variable
   passed down through your call stack instead (see `shell/src/main.rs`'s
   `on_byte` for the pattern: buffer and length live in `main`'s frame,
   passed by `&mut` reference).
6. **`core::fmt` and slice/string-vs-literal comparisons are both safe
   now** — see "Binary format" above for why they used to crash and
   what actually fixed it (real relocation processing, not a coding
   convention). Ordinary Rust idioms are fine; `shell/src/main.rs`'s
   `selftest` builtin is a working example of both.
7. **A `#[panic_handler]`** — there's no `std`, and no `uefi` crate's
   panic handling either (that only exists on the boot-services side).
   Looping on `wfe` forever is a reasonable minimum.
8. **Build and stage it in release mode — required, not optional** (see
   "Binary format" above for why debug builds fail to link at all):
   `cargo build -p <crate> --target aarch64-unknown-none --release`,
   then `llvm-objcopy --strip-all <elf> <name>.bin` (no `-O binary` —
   the loader needs a real ELF, not a flat dump; see the Makefile's
   `shell-bin` target for the exact invocation, including where to find
   `llvm-objcopy` — it isn't on `PATH` by default). Copy the resulting
   `.bin` (still a real, stripped ELF despite the extension) onto the
   ESP and point `INIT.CFG` at it.

## Writing a program in C (the userland-libc arc)

A program doesn't have to be Rust — the loader only cares about the ELF, not the
source language. The first C program (`libc/hello.c`, built by `make
chello-bin`) runs through the identical loader and syscall boundary. The
toolchain path, all from tools already present (no cross-toolchain to install):

1. **Compile with `clang`** targeting `aarch64-unknown-none` (ELF, not the host
   Mach-O): `--target=aarch64-unknown-none -ffreestanding -fPIC
   -fno-stack-protector`. `-fPIC` is the C equivalent of the Rust build's
   `relocation-model=pic`.
2. **Link with Rust's bundled LLD** (`ld.lld`, found in the toolchain's
   `gcc-ld/` dir) against the *same* `programs/linker.ld` and PIE flags the Rust
   programs use (`-pie --no-dynamic-linker -z max-page-size=4096`) — producing
   the same self-relocating `ET_DYN`.
3. **`llvm-objcopy --strip-all`** to the `.bin` (a stripped ELF), staged like any
   other `/bin` program.

The **constraints are the loader's, and they bite C harder than Rust**:

- **Globals/statics work** (the loader's `.data`/`.bss` milestone). Initialized
  file-scope data (`int g = 7;`) lands in `.data` and is loaded from the file;
  uninitialized data (`static int counter;`, `char buf[64];`) lands in `.bss`
  and is zeroed — both per PT_LOAD segment, in both the boot-load and
  runtime-spawn paths, and **re-initialized fresh on every spawn** (a mutation
  from one run doesn't leak into the next). A global holding a pointer emits an
  `R_AARCH64_RELATIVE` the loader applies. (String *literals* were always fine —
  they live in `.rodata`.) The region is RW (in fact RWX — the W^X weakness noted
  elsewhere), so statics are writable.
- **`_start` is the entry**, placed in `.text.start` (offset 0) via
  `__attribute__((section(".text.start")))`. The kernel has already set up the
  EL0 stack, so `_start` just calls `main` and then the `EXIT` syscall.
- **No libc yet.** `hello.c` carries its own inline `svc` syscall stubs. The real
  library grows from here as a set of POSIX syscall stubs (`_write` → `cond`,
  `_read`/`_open` → `fsd`'s `NP_*`, `_sbrk` → the userland heap, `_exit` →
  `EXIT`), then a ported `picolibc`/`newlib` on top — see `roadmap.md`'s "POSIX /
  C-program portability."
- **Watch the relocations** the same way (`llvm-readobj --dyn-relocations`): an
  `R_AARCH64_ABS64` is unloadable. Simple code is PC-relative and needs none;
  richer code emits `R_AARCH64_RELATIVE`, which the loader handles. `memcpy`/
  `memset` calls the compiler synthesizes for struct/array copies would be
  *undefined symbols* — provide them (a few lines each) when they first appear.

## Known rough edges

Worth knowing before building further on this:

- **~~No shared syscall-ABI crate~~ - fixed.** Syscall numbers and every
  sentinel (`NO_CHAR`, `FS_ERROR`, `NO_FS`) now live in `syscall-abi/`, a
  third workspace member both `kernel/src/syscall.rs` and any userland
  program depend on directly (`syscall-abi::FS_MKDIR`, etc.), rather than
  hand-duplicated local consts kept in sync only by convention. It's a
  plain `#![no_std]` lib with no logic - safe to depend on from either
  target this project builds for, since every value is a scalar integer
  inlined at the use site, not a pointer needing relocation. Still only
  useful within this repository - a program built elsewhere would need
  to either depend on this crate too or re-derive the same numbers by
  hand.
- **~~One program, loaded once, at boot~~ - partially fixed.** The
  shell's `exec <path>` command (backed by the new `spawn` syscall, see
  above) can load and start a second, third, or fourth program at
  runtime, without a reboot - genuinely `tasks::spawn`, not
  exec-replaces-current-process, so the caller keeps running too. Still
  real limits: a task can end itself (`exit`, syscall 17 - slot freed,
  region unmapped, RAM reclaimed in the common LIFO case), another
  task can be ended (`kill`, 19), the keyboard can be handed to a
  spawned task and back (`fg`, 20 - ownership reverts to task 0 when
  the owner dies; Ctrl+C reclaims it), and a task's exit status can be
  awaited and collected (`wait`, 21 - exited tasks hold their slot as
  zombies until waited; `kill` reaps immediately),
  no way to reload an *already-running* task's program in place, and
  the runtime allocator's reclaim is LIFO-or-leak (a bump cursor, not
  a free list), so pathological spawn/exit orderings can still strand
  regions for the rest of the boot.
- **Fixed 5-task scheduler.** `tasks.rs` has five slots - tasks 0 (the
  boot-loaded shell), 1 (idle), and 2 (the filesystem server's reserved
  slot) are permanent; slots 3 and 4 start `Unused` and are filled by
  `spawn`. Once both spawnable slots are in use, a further `exec` fails
  with `SPAWN_ERR_NO_FREE_SLOT` rather than growing the scheduler
  further.
- **No `alloc`-backed heap, and no `.bss`** — so no dynamic collections
  (`Vec`/`String`/`Box`) and no static mutable state. There **is** a 256KB
  *raw* heap area per program (`heap_info` syscall, a `&mut [u8]`), which
  lifts fixed-buffer caps (the shell's redirect/pipe capture uses it) - but
  a real `GlobalAlloc` heap is blocked: prebuilt lib`alloc` has
  `R_AARCH64_ABS64` relocations a `-pie` link rejects, and rebuilding it
  needs nightly `-Z build-std` (see "Binary format").
- **Stack size is fixed** (4 pages, 16KB) regardless of what a program
  actually needs, but there **is** a guard page now — a stack overflow
  lands in an inaccessible page below the stack and takes a clean EL0
  fault (the task is killed alone) instead of silently corrupting the code
  below. One caveat: a single stack frame larger than the one-page guard
  (4KB) could skip over it — the standard single-guard limitation.
- **Must be built in `--release`, not debug.** A real, confirmed
  toolchain constraint (an `R_AARCH64_ABS64` relocation inside prebuilt
  `libcore`'s own object code that only a release build's optimizer
  eliminates), not a preference - see "Binary format" above.
- **~~No ELF, no relocations, no dynamic linking~~ - real ELF and real
  `R_AARCH64_RELATIVE` relocations, fixed.** See "Binary format" above
  and `CLAUDE.md`'s "A real relocating loader" section. Still no
  dynamic linking, no imported symbols, no `exec()` - deliberately out
  of scope, not a remaining gap in what was attempted.
- **~~No `core::fmt`, and no comparing a slice/string against a
  literal~~ - both safe now, confirmed via the shell's `selftest`
  builtin.** See "Binary format" above for what actually fixed this
  (real relocation processing, not a coding convention to remember).
- **A crashing program no longer takes the system down.** An EL0
  fault (wild pointer, undefined instruction, ...) kills just the
  faulting task - reported on the console with ESR/FAR/ELR, slot
  reaped, memory reclaimed when allocation order allows - and the rest
  of the system keeps running; the filesystem server specifically is
  restarted from a kept image (up to 3 times per boot). The boot shell
  and idle task are the exceptions: their faults still halt, honestly.
  There **is** a guard page now - a stack overflow lands in an
  inaccessible page below the stack and faults cleanly (the task killed
  alone) rather than silently corrupting adjacent memory (see the
  fixed-stack
  bullet above).
- **Isolation is MMU-enforced now, not trust-based.** Each task runs
  under its own translation-table view (`mmu.rs`) in which only its own
  region is EL0-accessible - touching another task's memory faults and
  kills only the toucher. The complementary syscall-boundary check
  (`syscall.rs::in_caller_region`) validates every `(pointer, length)`
  argument against the caller's own region, so access can't be
  laundered through a kernel copy. The filesystem protocol carries no
  cross-task pointers at all (payloads are inline). Bulk file data now
  moves via the **grant/safecopy** capability (syscalls 31/32,
  kernel-mediated copy between two regions, authorized by an explicit
  grant plus an active call), which lifted the 512-byte cap for file
  reads/writes to `SAFECOPY_MAX` (2048) per op and lets `cat` stream any
  size. What remains: the 512-byte inline cap still bounds directory
  *listings* (`ls`); a single non-streaming transfer is
  userland-memory-bound, but a program has a 256KB raw heap area now
  (`heap_info`) on top of its 32KB stack, which the shell uses to capture
  large redirect/pipe output; and the stack now has a
  *guard page* (an overflow faults cleanly and kills just that task,
  rather than silently corrupting the program's own region - except a
  single >4KB frame could skip the one-page guard).
- **Write support covers directories, files with real content,
  copying, renaming/moving, and random-access writing at a byte offset**
  (`fat32::write_at` - write data at any offset, extending the file,
  without rewriting the bytes before it, via a partial-sector
  read-modify-write; an offset past the end of file zero-fills the gap,
  bounded by a 1 MiB cap). `cp` streams through it and handles a file of
  any size; `>>` appends at the end of a file of any existing size; the
  `writeat` builtin writes in place at any offset. What's still missing:
  no way to *shrink* a file except by full-replacing it (no
  truncate-to-length), no recursive `cp`, no
  move-into-an-existing-directory-keeping-basename shortcut for `mv`, no
  cycle detection (`mv` a directory into its own descendant isn't guarded
  against). Output redirection (`>`/`>>`) is entirely shell-side (see
  `docs/shell-commands.md`'s "Output redirection" section) - `>` is a
  full replace, `>>` is a `write_at` append; the *new* output is bounded
  by the 1024-byte capture, but the target file it appends to isn't. A parent directory that's out of free
  entry slots is grown by a cluster automatically (`insert_dir_entry`'s
  extension — directories never *shrink*, though: an emptied extension
  cluster stays linked until the directory is removed). Every filesystem
  operation returns a specific `FS_ERR_*` code for every failure reason
  ("already exists", "disk full", "invalid name", ... — a reserved top
  band `>= FS_ERR_MIN` in `syscall-abi`, mapped from the server's
  `fat32::Error` by `fsd/src/main.rs::error_code`), alongside the
  existing `NO_FS` distinction — the old one-collapsed-sentinel gap is
  closed, and `spawn`'s codes are split the same way. See `CLAUDE.md`'s "Phase 4" through "Phase 8"
  sections. **This closes the write-support arc phase 3 deliberately
  deferred** - what's left in the parking lot from here is genuinely
  bigger work, not more commands of this shape.

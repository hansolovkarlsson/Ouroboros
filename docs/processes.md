# Processes: loading, memory, and writing your own

How Ouroboros gets a userland program from a file on disk into a running
EL0 task, why it works the way it does, and what's involved in writing a
replacement for the default shell. Reference documentation for the current
design — see `CLAUDE.md` for the reasoning trail and what was tried first,
and [`roadmap.md`](roadmap.md) for where this is headed (phase 3's runtime
storage stack in particular will change several of the constraints noted
below).

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
that's separately deferred for Parallels console output — and was
explicitly *not* what this milestone needed. It gets real once something
needs to load a program *after* boot (dynamic `exec()`-style spawning);
until then, everything that will ever run has to be known and loadable at
boot time.

The sequence, all before `exit_boot_services`:

1. Read `\EFI\OUROBORO\INIT.CFG` — a config file containing exactly one
   line: the path of the program to load.
2. Read that program's bytes.
3. Allocate an EL0-accessible, 2MB-aligned region sized to fit it (see
   "Memory model" below) and copy the bytes in.

If any step fails, boot fails loudly (a panic through the UEFI logger) —
there's no fallback program, matching this project's established stance on
console discovery: no confirmed data means no guessing, not a silent wrong
answer.

## Configuration

`\EFI\OUROBORO\INIT.CFG` is deliberately minimal: one line, no key/value
syntax, no comments, just a path (trimmed of surrounding whitespace). The
Makefile's `esp` target writes it automatically, pointing at the built-in
shell. Note the directory is `OUROBORO`, not `OUROBOROS` — deliberately 8
characters: the runtime FAT32 reader (`fat32.rs`) doesn't parse long
filenames, and a 9-character name would only be reachable through FAT's
mangled 8.3 alias.

```
\EFI\OUROBORO\SH.BIN
```

To run a different program at boot, either edit this file directly on the
ESP, or point `INIT.CFG` at a different `.bin` staged there — no kernel
rebuild required. This is the actual "replaceable through configuration"
behavior the whole design is for.

The format only grows if something actually needs a second setting —
don't add key/value parsing preemptively.

## Memory model

A loaded program gets one EL0-accessible region: its code (and rodata) at
the base, followed by a fixed stack allowance (currently 2 pages, 8KB),
with the stack pointer starting at the top and growing down. No heap, no
`.bss`/`.data` support (see "Binary format" below) — everything a program
needs beyond code and a stack has to be a local variable today.

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

Flat, position-dependent raw machine code — not ELF. The kernel doesn't
parse program headers, process relocations, or understand any container
format; it copies bytes to a fixed address and sets `ELR_EL1` to the very
start of that address. This is deliberately the simplest thing that could
work, not a permanent design constraint — a real ELF loader is a
reasonable future step once something needs what only ELF provides
(position independence, explicit segment permissions, symbol info).

Consequences of "flat binary, no loader smarts":

- **Entry must be the first byte.** A linker script (see
  `shell/linker.ld`) sets the link address to `0x0` and places the entry
  symbol first in `.text` via `KEEP(*(.text.start))`.
- **No `.bss`.** There's no crt0 here to zero memory before your code
  runs, and the loader only copies exactly the file's bytes — a
  zero-initialized static's space wouldn't exist in the loaded region at
  all. `shell/linker.ld` defines `.bss` and `.data` output sections but
  `ASSERT`s they're empty, so a program that accidentally introduces
  global mutable state fails to *link*, not to run correctly.
- **No relocations, no PIC.** Built with `relocation-model=static`. Since
  the load address isn't known at compile time in general (though it
  currently always ends up being whatever `loader.rs`'s allocator picks),
  a program must not embed absolute addresses of its own symbols — the
  shell doesn't, since its "global state" (the input buffer) is a local
  variable in `main`'s stack frame, not a static.
- **`core::fmt` (`write!`, `{}` formatting) is unsafe to use, and will
  crash.** Discovered directly, not by inspection: the shell's first
  `uptime` implementation used `write!` to format a tick count and
  immediately crashed on the very first call (`Instruction Abort`, `ELR_EL1`
  landing on a tiny near-zero address rather than real code). Root cause:
  `core::fmt::Arguments` builds its per-argument dispatch out of *data* — an
  array of function pointers, one per formatted value — rather than direct
  `bl` calls. A direct call compiles to a PC-relative branch and stays
  correct no matter where the binary ends up loaded; a function pointer
  baked into `.rodata` at compile time for a program linked at base `0x0`
  is only correct if the program actually runs at `0x0` — which it never
  does (see "Memory model" above). There is no relocation processing to fix
  such a pointer up at load time. `shell/src/main.rs`'s `print_u64_decimal`
  is the replacement: a hand-rolled decimal formatter using only direct
  calls. Any future program needs to avoid `core::fmt` for the same reason,
  until this gets a real relocating loader.
- **Comparing a slice/string against a literal is unsafe too, for the
  identical reason - a second, separately confirmed instance, not a
  hypothetical extension.** Phase 3c's `cd` needed to check whether the
  current directory was already root (`cwd_bytes != b"/"`) and crashed the
  same way `write!` did - `ELR_EL1` inside the shell's own code,
  `FAR_EL1` a small, build-layout-dependent address. Bisected with
  temporary `print_line` calls until the exact statement was isolated.
  The fix is the same shape as `core::fmt`'s: don't reference the literal
  at all, just compare scalars (`bytes.len() == 1 && bytes[0] == b'/'`
  instead of `bytes != b"/"`; `shell/src/main.rs`'s `is_root`/`is_dot`/
  `is_dotdot` are the pattern to copy). Direct calls and individual
  `u8`/`usize` comparisons are fine; anything needing a *reference* to
  literal data in `.rodata` isn't.

## Syscall ABI available to a program

Every syscall number and sentinel value below lives in the `syscall-abi`
crate (`syscall-abi/src/lib.rs`), a third workspace member both the
kernel and any userland program depend on directly - add it as a
dependency in your program's `Cargo.toml` (see the default shell's for an
example) and use `syscall_abi::FS_MKDIR` etc. rather than hand-copying
numbers. See `docs/architecture.md`'s syscall table for the full list.
`try_read_char` (`TRY_READ_CHAR`, non-blocking, returns `NO_CHAR` when
nothing is waiting) and `putc` (`PUTC`, one raw byte, no newline
translation) cover interactive I/O; `get_ticks` (`GET_TICKS`, added for
phase 2's `uptime` builtin) is the pattern to follow whenever a command
needs real kernel state it can't get any other way. `fs_list_dir`/
`fs_read_file` (`FS_LIST_DIR`/`FS_READ_FILE`, added for phase 3c's disk
commands) are the first syscalls needing more than one argument — a path
pointer/length and a buffer pointer/length at once — which is why the
syscall ABI itself supports up to 4 arguments (`x0`-`x3`), not just one.
`fs_mkdir`/`fs_rmdir` (`FS_MKDIR`/`FS_RMDIR`, added for phase 4) are the
first syscalls that write to disk, and `fs_touch`/`fs_rm`
(`FS_TOUCH`/`FS_RM`, added for phase 5) round out file lifecycle the
same way — each of these four takes just a path pointer/length, no
output buffer. All six `fs_*` syscalls share two distinct failure
sentinels: `FS_ERROR` (`u64::MAX`) for "mounted, but this operation
failed" (a program still can't tell "already exists" from "disk full"
within that — see `CLAUDE.md`'s "Phase 4"/"Phase 5" sections) and
`NO_FS` (`u64::MAX - 1`) specifically for "no filesystem is mounted this
boot" — added after real testing showed every disk command failing
identically on `make run` (FAT16) looked like a broken path rather than
"nothing's mounted," with the real cause visible only in the kernel's
own boot log. A userland program makes these directly via `svc`:

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

## Writing a replacement program

The default shell (`shell/`) is a real, if minimal, example to copy. To
write your own:

1. **New crate**, `no_std` + `no_main`, built for `aarch64-unknown-none`
   (already an installed target — see `rust-toolchain.toml`).
2. **Copy `shell/linker.ld`** as-is, or adapt it — the constraints above
   (entry first, no `.bss`/`.data`) apply to any program loaded this way,
   not just the shell.
3. **Add a target-specific `rustflags` entry** in the workspace's
   `.cargo/config.toml` if your linker script lives somewhere other than
   `shell/linker.ld` (the existing `[target.aarch64-unknown-none]` section
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
6. **No `core::fmt` (`write!`, `{}`), and no comparing a slice/string
   against a literal either** — see "Binary format" above for why both
   crash (the same root cause). Format numbers by hand (`shell/src/main.rs`'s
   `print_u64_decimal` is a ready-made example), build strings out of
   byte/`&str` loops through `putc`, and write comparisons like
   `shell/src/main.rs`'s `is_root`/`is_dot`/`is_dotdot` (scalar `len()`/
   indexed-byte checks) instead of `slice == b"literal"`.
7. **A `#[panic_handler]`** — there's no `std`, and no `uefi` crate's
   panic handling either (that only exists on the boot-services side).
   Looping on `wfe` forever is a reasonable minimum.
8. **Build and stage it**: `cargo build -p <crate> --target aarch64-unknown-none`,
   then `llvm-objcopy -O binary --strip-all <elf> <name>.bin` (see the
   Makefile's `shell-bin` target for the exact invocation, including where
   to find `llvm-objcopy` — it isn't on `PATH` by default). Copy the `.bin`
   onto the ESP and point `INIT.CFG` at it.

## Known rough edges

Worth knowing before building further on this:

- **~~No shared syscall-ABI crate~~ - fixed.** Syscall numbers and every
  sentinel (`NO_CHAR`, `FS_ERROR`, `NO_FS`) now live in `syscall-abi/`, a
  third workspace member both `kernel/src/syscall.rs` and any userland
  program depend on directly (`syscall-abi::FS_MKDIR`, etc.), rather than
  hand-duplicated local consts kept in sync only by convention. It's a
  plain `#![no_std]` lib with no logic - safe to depend on from either
  target this project builds for, since every value is a scalar integer
  inlined at the use site, not a pointer needing relocation (so it
  doesn't run into the "no comparing a slice/string against a literal"
  restriction below). Still only useful within this repository - a
  program built elsewhere would need to either depend on this crate too
  or re-derive the same numbers by hand.
- **One program, loaded once, at boot.** No `exec()`, no spawning a second
  process, no way to reload a program without rebooting.
- **Fixed 2-task scheduler.** `tasks.rs` has exactly two slots; a second
  *loaded* program has nowhere to run without displacing the idle task or
  growing the scheduler into something that isn't just a fixed array.
- **No heap for userland programs**, and no `.bss`, so no static mutable
  state at all — every program is constrained to stack-local state, same
  as the shell.
- **Stack size is fixed** (2 pages, 8KB) regardless of what a program
  actually needs, and there's no guard page — a stack overflow silently
  corrupts whatever memory follows it rather than faulting.
- **No ELF, no relocations, no dynamic linking.** Every program is a flat,
  position-dependent blob built specifically for wherever `loader.rs`'s
  allocator happens to place it that boot (which happens to always work
  today, but isn't a guarantee anything currently enforces).
- **No `core::fmt`, and no comparing a slice/string against a literal.**
  A direct consequence of the above, but easy to hit by accident (both
  compile fine and only fail at runtime) — see "Binary format"'s callout
  for what actually goes wrong and why. Goes away once there's a real
  relocating loader.
- **Disk-command pointer/length arguments are trusted, not validated.**
  `fs_list_dir`/`fs_read_file`/`fs_mkdir`/`fs_rmdir`/`fs_touch`/`fs_rm`
  dereference the caller's `(pointer, length)` pairs directly, checked
  only against a minimal sanity bound (`syscall.rs::valid_user_range`) —
  not against the calling program's actual mapped region. Fine with
  exactly one, currently-trusted userland program; a real gap once that
  stops being true.
- **Write support (phases 4-5) covers empty directories and zero-byte
  files only, deliberately.** No `cp`/`mv`, no output redirection, and
  no way to write actual *content* into a file — `touch` only ever
  produces zero-byte files, so a real "write file contents" syscall is
  the actual blocker for anything beyond create/delete. `mkdir` also
  can't grow a parent directory that's out of free entry slots — it
  fails rather than allocating another cluster for the parent. Every
  `fs_*` syscall distinguishes "no filesystem mounted" (`NO_FS`) from
  everything else, but every *other* failure reason still collapses to
  one `FS_ERROR` sentinel, so a program can't yet tell "already exists"
  from "disk full" from "bad name". See `CLAUDE.md`'s "Phase 4"/"Phase 5"
  sections.

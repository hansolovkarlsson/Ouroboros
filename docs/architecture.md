# Ouroboros architecture

Reference documentation for how the kernel is built, as of the current
codebase — not a development log. For the history behind each design
decision (what was tried, what broke, how it was diagnosed), see
`CLAUDE.md` at the repository root; this document assumes those decisions
and describes the result.

For the process-loading/userland-program model specifically (the newest
subsystem, and the one most likely to change next), see
[`processes.md`](processes.md). For completed milestones, see
[`CHANGELOG.md`](CHANGELOG.md); for what's planned next and why, see
[`roadmap.md`](roadmap.md). For how this boot flow compares to a mature
microkernel's (MINIX), see
[`research-minix-boot.md`](research-minix-boot.md).

## Design goals

From `notes.txt`, the original brief:

- Microkernel architecture
- POSIX-ish system calls (not ABI-compatible with Linux, just
  similarly-shaped)
- Preemptive multitasking
- Filesystem choice still open
- Draws ideas from Linux, Minix, and Plan 9
- Primary test target is Parallels on Apple Silicon, not just an emulator

## Boot flow

The kernel is itself a UEFI application (`kernel/`, built for
`aarch64-unknown-uefi`) — there is no separate bootloader stage. `main()`
in `kernel/src/main.rs` runs the following, in order:

1. **UEFI init** (`uefi::helpers::init`). Logging, the global allocator,
   and UEFI protocol access all work normally from here through step 4.
2. **Console discovery** (`devicetree.rs` → `acpi.rs` → `pci.rs`, first
   match wins). Determines the console's MMIO address and driver kind, but
   doesn't touch it yet — see "Console" below.
3. **Program loading** (`loader.rs`). Reads a config file and a program
   binary off the ESP via UEFI's own filesystem protocol, into a freshly
   allocated, page-aligned buffer. Still boot-services-only; see
   [`processes.md`](processes.md) for why this happens here rather than
   after boot.
4. **`exit_boot_services`**. The UEFI memory map is captured (not
   discarded — `mmu.rs` uses it) and permanently leaves the boot-services
   world. Nothing after this point may use `log::*`, `alloc`, or any UEFI
   protocol.
5. **`exceptions::install`**. Points `VBAR_EL1` at the kernel's own vector
   table before anything else gets a chance to fault.
6. **Console installation**. If discovery in step 2 succeeded, the driver
   is constructed now and raw MMIO console output becomes available
   (`console::println!`).
7. **`mmu::install_identity_map`**. Replaces firmware's translation tables
   with the kernel's own (see "Memory layout" below).
8. **virtio-console fallback** (`try_virtio_console`), tried only if step
   2's three mechanisms all failed. Has to run here, after step 7, not
   alongside step 6 — see "Console" below for why.
9. **GIC + timer init** (`gic.rs`, `timer.rs`). Arms the periodic
   preemption tick.
10. **`tasks::init`**. Builds both EL0 tasks' initial `Context` (see
    "Process model" below).
11. **IRQs unmasked**, then **`tasks::start`** drops to EL0 and never
    returns — every further transition back to EL1 goes through the
    exception vector table.

## Privilege model

Two exception levels are in play: **EL1** (the kernel) and **EL0**
(userland tasks). There is no EL2/hypervisor use — the kernel has only
ever been observed booting directly at EL1, typical for a UEFI OS loader.

EL0 code cannot access kernel memory, MMIO, or privileged system
registers; the only way back to EL1 is a trap — a syscall (`svc`), a
fault, or the timer interrupt — handled through the exception vector table
(`exceptions.rs`) and, for syscalls specifically, dispatched by
`syscall.rs`. See "Syscall ABI" below for the calling convention.

## Memory layout

`mmu.rs` builds a 4-level (L0→L1→L2→L3) identity map and switches
`TTBR0_EL1` to it — the MMU is never disabled; this only changes which
tables it walks. Two coarse block kinds cover almost everything:

| Region | Range | Attributes |
|---|---|---|
| Device | `0x0`–`0x3FFF_FFFF` (fixed low 1GB) | Device-nGnRnE, non-executable |
| RAM | Whatever the UEFI memory map reports as general RAM | Normal WB, executable, EL1-only |

Within RAM, up to two small regions get finer-grained (4KB page) EL0
access instead of the default EL1-only 2MB/1GB blocks — one per EL0 task.
See [`processes.md`](processes.md) for how those regions are sized and
placed, and `mmu.rs`'s own module doc comment for the full mechanics
(including two real bugs that shaped this design: a starting-level
mismatch that caused a fault loop, and an EL0-vs-EL1-code-sharing issue
that was worked around rather than root-caused).

The walk starts at **L0** with `T0SZ=20`, matching firmware's own
configuration — deliberately, not a simplification opportunity; a
single-level-simpler table was tried first and hard-faulted.

## Exception handling

`exceptions.rs` installs a 16-entry AArch64 vector table. Two shapes:

- **Diverging** (most vectors): capture `ESR_EL1`/`FAR_EL1`/`ELR_EL1`,
  report through the console if one exists, halt. Never returns.
- **Resumable** (IRQ vectors, and the SVC vector when `ESR_EL1`'s EC field
  is `0x15`): save the full general-purpose register set plus
  `SP_EL0`/`ELR_EL1`/`SPSR_EL1` to a stack frame (`Context`, mirrored
  exactly by `tasks.rs`'s task state), call into Rust, restore, `eret`.

The IRQ path's saved frame is what makes task switching possible: a timer
tick's handler is handed a pointer to that live frame and can overwrite it
in place (`tasks::on_tick`) — the trampoline's restore-and-`eret`
afterward doesn't know or care that the values changed underneath it.

FP/SIMD registers are not part of the saved context. Fine today (nothing
running touches them), a real limitation for whatever runs next.

## Process model

`tasks.rs` implements strict round-robin scheduling between exactly two
EL0 tasks — no priorities, no blocking, no queue, no dynamic
creation/destruction. The *only* thing that ever moves execution from one
task to the other is the timer tick catching one mid-execution and
swapping its saved `Context` for the other's.

- **Task 0** runs whatever program `loader.rs` loaded from disk — see
  [`processes.md`](processes.md).
- **Task 1** is a genuine idle task: a bare `wfe` loop, nothing else.

The tick period is 20ms (`timer::TICK_INTERVAL_MS`) — short specifically
so that a task waiting its turn (e.g. the shell, when task 1 currently has
the CPU) doesn't introduce perceptible input lag. A 1-second period was
tried first and produced up to a full second of latency, exactly as
unconditional round-robin scheduling would predict.

## Syscall ABI

Convention: syscall number in `x8`, up to 4 arguments in `x0`-`x3`, return
value in `x0` — chosen to match Linux's shape (a reasonable default for a
"POSIX-ish" project), not for ABI compatibility with anything. Grew from
1 argument to 4 for phase 3c's file-I/O syscalls, which need a path
pointer/length and a buffer pointer/length at once — see
`exceptions.rs`'s module doc comment for how the SVC trampoline marshals
them. `fs_mkdir`/`fs_rmdir` (9/10, phase 4) are the first syscalls this
kernel exposes that actually write to disk — see `fat32.rs`/
`virtio_blk.rs` for the write path underneath them. Every number and
sentinel in the table below is a constant in the `syscall-abi` crate
(`syscall-abi/src/lib.rs`), not a bare literal — both
`kernel/src/syscall.rs`'s dispatch table and `shell/src/main.rs` depend
on it directly, so the two sides of this ABI can't silently drift apart
the way hand-duplicated numbers did before this crate existed.

| Number | Name | Arguments | Returns | Notes |
|---|---|---|---|---|
| 0 | `print` | value to log | `0` | Debug/demo only — logs through the kernel console with a fixed prefix, not a general write primitive |
| 1 | `double` | a number | `arg0 * 2` | Demo only — proves a return value survives the trampoline |
| 2 | `report` | a task ID | `0` or `u64::MAX` | Demo only — original two-task milestone's proof of per-task syscall state |
| 3 | `try_read_char` | ignored | a byte, or `NO_CHAR` (`u64::MAX`) if none waiting | Non-blocking |
| 4 | `putc` | a byte | `0` | Raw single-byte console write, no newline translation |
| *5* | *(gap)* | | | `shell_input` used to live here; removed when line editing moved into userland. Left unfilled rather than renumbered — see `syscall.rs`'s module doc comment |
| 6 | `get_ticks` | ignored | preemption tick count since boot | Added for phase 2's `uptime` builtin — the first syscall added specifically so a loaded program could read real kernel state, not just I/O |
| 7 | `fs_list_dir` | path ptr, path len, buf ptr, buf len | bytes written, `NO_FS`, or `FS_ERROR` | Formats each entry as `name\n`/`name/\n`, truncating rather than erroring if `buf` is too small |
| 8 | `fs_read_file` | path ptr, path len, buf ptr, buf len | the file's real size (may exceed `buf`'s length — compare to detect truncation), `NO_FS`, or `FS_ERROR` | |
| 9 | `fs_mkdir` | path ptr, path len | `0`, `NO_FS`, or `FS_ERROR` | Creates an empty directory. Every non-`NO_FS` failure reason (already exists, invalid name, parent missing, disk full, ...) still collapses to the same `FS_ERROR` |
| 10 | `fs_rmdir` | path ptr, path len | `0`, `NO_FS`, or `FS_ERROR` | Removes an empty directory. Same collapsed-error contract as `fs_mkdir` |
| 11 | `fs_touch` | path ptr, path len | `0`, `NO_FS`, or `FS_ERROR` | Creates an empty (zero-byte) file, or succeeds as a no-op if one already exists there. Same collapsed-error contract as `fs_mkdir` |
| 12 | `fs_rm` | path ptr, path len | `0`, `NO_FS`, or `FS_ERROR` | Removes a file (not a directory — use `fs_rmdir` for those). Same collapsed-error contract as `fs_mkdir` |
| 13 | `fs_write_file` | path ptr, path len, data ptr, data len | `0`, `NO_FS`, or `FS_ERROR` | Creates a file with exactly `data`'s contents, or fully overwrites (not appends to) an existing file's contents. `data len` may legitimately be `0` (truncate to empty) — the one `fs_*` argument pair where a zero length is valid rather than rejected, see below |
| 14 | `fs_mv` | src ptr, src len, dst ptr, dst len | `0`, `NO_FS`, or `FS_ERROR` | Renames or moves the file or directory at `src` to `dst`, reusing its existing cluster chain rather than reading and rewriting content. `dst` must not already exist |
| other | — | — | `u64::MAX` | Logged as unknown |

All eight `fs_*` syscalls share two distinct failure sentinels, not one:
`FS_ERROR` (`u64::MAX`) for "the filesystem is mounted but this operation
failed" (not found, already exists, bad name, disk full, ...), and `NO_FS`
(`u64::MAX - 1`) specifically for "there's no mounted filesystem at all"
(e.g. `make run`'s vvfat disk is FAT16, not FAT32 — see `fat32.rs`).
Added after real user confusion: without the split, every disk command
failing on `make run` looked identical to a genuinely broken path, and the
actual cause was only ever visible in the kernel's own boot log, never to
the shell itself. `shell/src/main.rs`'s `cmd_ls`/`cmd_cat`/`cmd_cd`/
`cmd_mkdir`/`cmd_rmdir`/`cmd_touch`/`cmd_rm`/`cmd_write`/`cmd_cp`/`cmd_mv`
all match on the raw return value against both sentinels so they can
print "no filesystem mounted" instead of a command-specific "not
found"/"failed" message when that's the real cause. Safe to keep
numerically distinct from any real success value: byte counts and file
sizes returned on success are always far below `u64::MAX - 1`.

`fs_write_file`'s `data` argument needed a genuine second validation
rule, not just reused `valid_user_range` logic: `syscall.rs`'s argument
sanity check normally rejects any zero-length `(pointer, length)` pair,
correct for `fs_list_dir`/`fs_read_file`'s output buffers (a zero-length
destination is pointless) but wrong here, where empty data is a real,
meaningful case (truncating a file to zero bytes, exactly matching
`fs_touch`'s own empty-file semantics) — caught immediately by testing
`write <file>` with no content. `valid_user_range_allow_empty` covers
this: same bound, but a zero length passes as long as the pointer is
still non-null. Only `fs_write_file`'s data argument uses it; every path
argument, including this syscall's own, still uses the stricter
`valid_user_range`.

The `syscall-abi` crate only covers the numbers/sentinels themselves, not
argument validation or per-error-reason detail — see
[`processes.md`](processes.md)'s "known rough edges" for what's still a
real gap (pointer/length arguments are trusted, not checked against the
caller's actual mapped region; every `fs_*` failure beyond "no filesystem
mounted" still collapses to one `FS_ERROR` value) and for how to depend
on this crate when writing a new userland program.

## Console

`console.rs` holds a global `Console` handle (`Pl011`, `Uart16550`, or
`Virtio`), installed once after `exit_boot_services` and shared between
`main()` and the exception handler (a fault needs somewhere to report
through too). All three drivers support read and write, though `Virtio`'s
read always returns nothing — see below.

Which driver, and at what address, is determined by trying up to four
mechanisms in order, logging why each failed before trying the next:

1. **Devicetree** (`devicetree.rs`) — confirmed dead on both QEMU (default
   `acpi=on` config) and Parallels; both are ACPI-oriented. (A later,
   incidental finding: QEMU's bundled firmware switches to advertising a
   devicetree blob instead when booted with `acpi=off` — an artifact of
   that specific, non-default configuration, not a change to this
   mechanism's normal QEMU/Parallels behavior.)
2. **ACPI RSDP → XSDT → SPCR** (`acpi.rs`) — works on QEMU, confirmed dead
   on Parallels (no SPCR table entry at all).
3. **PCI enumeration for a class 0x07/0x00 serial controller** (`pci.rs`)
   — confirmed dead on both (neither platform exposes a PCI 16550).
4. **virtio-mmio for a console device** (`virtio_console.rs`, device ID
   3) — tried only if all three above fail, and only *after*
   `mmu::install_identity_map` rather than alongside the other three (see
   `virtio_console.rs`'s module doc comment for the real MMU-mapping
   constraint that forces this later placement). Confirmed working end to
   end on QEMU: discovery, feature negotiation, transmitq0 setup, and
   real data reaching the host. Transmit-only — no receive virtqueue
   exists yet, so `Console::Virtio`'s read always returns `None`.
   Parallels itself is unconfirmed: whether it exposes its console via
   virtio-mmio at all (vs. virtio-pci), and if so at the same address
   range this driver assumes, needs a real boot on real hardware to know.

There is deliberately no hardcoded fallback address for any driver: one
existed early in this project and was removed after confirming it
hard-crashes real Parallels hardware.

## What's not here yet

This document describes the current, working state. For active
limitations and what's reasonably next, see `CLAUDE.md`'s "Next milestone"
section, which is kept current as the project's actual state changes —
not duplicated here, since it would just go stale.

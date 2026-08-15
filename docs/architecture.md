# Ouroboros architecture

Reference documentation for how the kernel is built, as of the current
codebase — not a development log. For the history behind each design
decision (what was tried, what broke, how it was diagnosed), see
`CLAUDE.md` at the repository root; this document assumes those decisions
and describes the result.

For the process-loading/userland-program model specifically (the newest
subsystem, and the one most likely to change next), see
[`processes.md`](processes.md). For what's planned next and why, see
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
8. **GIC + timer init** (`gic.rs`, `timer.rs`). Arms the periodic
   preemption tick.
9. **`tasks::init`**. Builds both EL0 tasks' initial `Context` (see
   "Process model" below).
10. **IRQs unmasked**, then **`tasks::start`** drops to EL0 and never
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

Convention: syscall number in `x8`, first argument in `x0`, return value
in `x0` — chosen to match Linux's shape (a reasonable default for a
"POSIX-ish" project), not for ABI compatibility with anything.

| Number | Name | `arg0` | Returns | Notes |
|---|---|---|---|---|
| 0 | `print` | value to log | `0` | Debug/demo only — logs through the kernel console with a fixed prefix, not a general write primitive |
| 1 | `double` | a number | `arg0 * 2` | Demo only — proves a return value survives the trampoline |
| 2 | `report` | a task ID | `0` or `u64::MAX` | Demo only — original two-task milestone's proof of per-task syscall state |
| 3 | `try_read_char` | ignored | a byte, or `NO_CHAR` (`u64::MAX`) if none waiting | Non-blocking |
| 4 | `putc` | a byte | `0` | Raw single-byte console write, no newline translation |
| *5* | *(gap)* | | | `shell_input` used to live here; removed when line editing moved into userland. Left unfilled rather than renumbered — see `syscall.rs`'s module doc comment |
| 6 | `get_ticks` | ignored | preemption tick count since boot | Added for phase 2's `uptime` builtin — the first syscall added specifically so a loaded program could read real kernel state, not just I/O |
| other | — | — | `u64::MAX` | Logged as unknown |

There is no shared ABI crate yet — these numbers are duplicated by hand in
`kernel/src/syscall.rs` (the dispatch table) and `shell/src/main.rs` (the
caller). See [`processes.md`](processes.md)'s "known rough edges" for why
that's a real gap once more than one userland program exists.

## Console

`console.rs` holds a global `Console` handle (`Pl011` or `Uart16550`),
installed once after `exit_boot_services` and shared between `main()` and
the exception handler (a fault needs somewhere to report through too).
Both drivers are polling, not interrupt-driven, and now support both read
and write (`uart.rs`, `uart16550.rs`).

Which driver, and at what address, is determined by
`discover_console`, trying three mechanisms in order and logging why each
failed before trying the next:

1. **Devicetree** (`devicetree.rs`) — confirmed dead on both QEMU and
   Parallels; both platforms are ACPI-oriented.
2. **ACPI RSDP → XSDT → SPCR** (`acpi.rs`) — works on QEMU, confirmed dead
   on Parallels (no SPCR table entry at all).
3. **PCI enumeration for a class 0x07/0x00 serial controller** (`pci.rs`)
   — confirmed dead on both (neither platform exposes a PCI 16550).

Console output on Parallels is a known, deliberately paused gap — see
`CLAUDE.md` for the virtio-console lead that's the likely next step there.
There is deliberately no hardcoded fallback address for any driver: one
existed early in this project and was removed after confirming it
hard-crashes real Parallels hardware.

## What's not here yet

This document describes the current, working state. For active
limitations and what's reasonably next, see `CLAUDE.md`'s "Next milestone"
section, which is kept current as the project's actual state changes —
not duplicated here, since it would just go stale.

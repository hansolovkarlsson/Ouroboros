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

Within RAM, up to five small regions get finer-grained (4KB page) EL0
access instead of the default EL1-only 2MB/1GB blocks — one per EL0 task
(boot shell, idle, filesystem server, and two spawnable slots).
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

- **Diverging** (EL1 faults and every other unexpected vector): capture
  `ESR_EL1`/`FAR_EL1`/`ELR_EL1`, report through the console if one
  exists, halt. Never returns — a *kernel* fault has nothing safe to
  resume.
- **Resumable** (IRQ vectors, the SVC vector when `ESR_EL1`'s EC field
  is `0x15`, and — since the fault-isolation milestone — every *other*
  synchronous EL0 exception): save the full general-purpose register
  set plus `SP_EL0`/`ELR_EL1`/`SPSR_EL1` to a stack frame (`Context`,
  mirrored exactly by `tasks.rs`'s task state), call into Rust,
  restore, `eret`.

The IRQ path's saved frame is what makes task switching possible: a timer
tick's handler is handed a pointer to that live frame and can overwrite it
in place (`tasks::on_tick`) — the trampoline's restore-and-`eret`
afterward doesn't know or care that the values changed underneath it.

**An EL0 fault is contained, not fatal** — the actual payoff of process
isolation. A userland wild pointer used to take the diverging path and
halt the whole system; now the fault handler
(`rust_el0_fault_handler`) reports it, tears down *just the faulting
task* (same teardown as `kill`: region freed, keyboard reverted,
anyone blocked mid-`msg_call` to it woken with a failed call —
`tasks::fail_calls_to` — slot reaped), overwrites the frame with the
next runnable task's context, and the trampoline resumes the survivor.
Tasks 0 (the boot shell) and 1 (idle) faulting still halt — nothing
meaningful survives those. If the dead task is the filesystem server,
the kernel restarts it from an image kept at boot (see "The filesystem
server" below).

FP/SIMD registers are not part of the saved context. Fine today (nothing
running touches them), a real limitation for whatever runs next.

## Process model

`tasks.rs` implements round-robin scheduling among up to `NUM_TASKS` (5)
EL0 task slots — no priorities, no queue. Every task is either `Unused`,
`Runnable`, `Blocked(reason)`, or `Zombie(status)`; the scheduler only
ever picks among the runnable ones. Two things move execution from one
task to another: the timer tick catching the current task mid-execution
and switching to the next runnable one (`on_tick`), and a blocking
syscall (`read_char`, `wait`, `msg_recv`, `msg_call` — see the syscall
table below) suspending its own caller immediately and switching away
mid-syscall (`block_current_and_switch`) rather than waiting for the
next tick.

- **Task 0** runs whatever program `loader.rs` loaded from disk — see
  [`processes.md`](processes.md).
- **Task 1** is a genuine idle task: a busy-spin loop (`nop; b 1b`), never
  blocked, always runnable — real Parallels hardware has a confirmed,
  unresolved hang when an EL0 task executes `wfe` (see `tasks.rs`'s
  module doc comment), so idling is a real spin, not an architectural
  `wfe` wait, and blocking syscalls are built the same way for the same
  reason (see below) rather than having a task call `wfe` itself.
- **Task 2** is reserved for the filesystem server (`fsd/`, boot-loaded
  from `\EFI\ORBS\FSD.BIN` — see "The filesystem server" below). It
  stays `Unused` if no FSD.BIN exists, and `spawn` never fills it: the
  slot is block-syscall-privileged, so a spawned program landing there
  would inherit the server's disk access.
- **Task slots 3 and 4** start `Unused` and are filled by `tasks::spawn`
  when the `spawn` syscall starts a further program at runtime (the
  shell's `exec <path>` command) — see "Dynamic task creation" below.

### The filesystem server (`fsd/`) — the first component moved out of the kernel

The FAT32 filesystem lives in userland (driver isolation part 2, the
MINIX-style first move — pure logic, no MMIO/DMA, which couldn't be
meaningfully isolated without an IOMMU anyway). The split:

- **The kernel keeps the device**: `virtio_blk.rs`/`usb_msd.rs` and the
  `BlockDevice` enum stay kernel-side, held in `syscall.rs`'s block
  cell. Three syscalls (`block_info`/`block_read`/`block_write`) expose
  it one 512-byte sector at a time — **only to task 2** (any other
  caller gets `BLOCK_ERR_DENIED`), which is the "supervised" in
  "supervised EL0 process".
- **The server owns the filesystem**: `fsd/src/fat32.rs` is the
  kernel's old module, moved essentially verbatim (its `BlockDevice`
  became a zero-sized shim over the `block_*` syscalls). The mounted
  state lives in the server's own stack frame — userland programs have
  no static mutable state, and the server is one infinite request loop
  that never returns.
- **Clients speak IPC**: requests are `FSOP_*` messages (see the
  syscall table's protocol section below), normally via `msg_call`'s
  synchronous round trip. The shell's `fs_*` wrapper functions are the
  reference client; every disk command, redirect, and `exec`'s program
  loading flows through them.

A missing FSD.BIN degrades gracefully: the kernel logs a warning at
boot, slot 2 stays `Unused`, and every request fails with
`TASK_ERR_NO_SUCH_TASK` — which the shell reports with its ordinary
no-filesystem message.

**A crashed server is restarted, not fatal** (the supervision half of
EL0 fault isolation — MINIX's reincarnation server, minimal edition):
`loader.rs` keeps FSD.BIN's raw bytes in a kernel static at boot
(128KB cap), and the fault handler restarts a faulted server from that
copy (`syscall::restart_fsd`) — no filesystem needed to reload it,
which matters since the crashed server *was* the filesystem. The
client whose call the server died under gets a cleanly failed call
(the shell shows its no-filesystem message once); the fresh server
re-runs its own startup and remounts from disk — all its state was
always derivable from disk, which is what makes this a real recovery.
A per-boot cap (3 restarts) guards against crash loops: past it, the
kernel gives up and slot 2 stays `Unused`, the same degradation as a
missing FSD.BIN. What this doesn't cover: a *wedged* (looping,
non-faulting) server — no watchdog exists, and Ctrl+C remains the
rescue for a client blocked calling one; and disk state a crashing
server corrupted mid-write — there's no journaling.

### Dynamic task creation (`spawn`/`exec`)

`exec <path>` in the shell loads a *second* (or third/fourth) program
from disk and starts it running **alongside** whatever's already
running — this is `tasks::spawn`, not POSIX exec-replaces-current-process
semantics. The shell command is named `exec` to match
`docs/roadmap.md`'s original wording for this item, but nothing about
the calling task is replaced or stopped; it keeps running, and the new
program becomes a new, independent task in whichever spawnable slot
(3-4) is `Unused`.

Since the filesystem moved to userland, "loads from disk" is a
two-step, client-driven flow: the shell reads the program via the
filesystem server (`FSOP_READ_AT`, 512 bytes per round trip), feeds
each chunk into the kernel's 128KB staging buffer (`spawn_stage`, 30),
and only then invokes `spawn` (16) with the staged total — the kernel
itself can't read a path anymore. The kernel-side pieces, in the order
the `spawn` syscall then exercises them:

1. **A runtime physical-page bump allocator** (`tasks::allocate_runtime_region`)
   — `boot::allocate_pages` (what `loader.rs` uses for task 0 at boot) is
   a UEFI boot-services API, long gone by the time a shell command can
   run. `NEXT_RUNTIME_REGION_TOP` starts at the top of discovered RAM
   (`mmu::ram_span()`) and grows downward, one 2MB-aligned slot per call.
   Deliberately the simplest correct thing for a first version: no
   destruction, no reuse, no free list.
2. **The same ELF loading `loader.rs` already had**, split so its parsing
   half (`elf_region_size`) and its copy/relocate half
   (`populate_region`) can each be called independently of *how* the
   destination region was obtained — boot-time `load()` still does its
   own `boot::allocate_pages`/`free_pages` dance around them; the syscall
   handler instead uses the bump allocator from step 1.
3. **`mmu::install_identity_map` called a second time**, with the same
   RAM span and every existing task's EL0 region plus the new one, to
   make the new region EL0-accessible. Reuses the exact mechanism that
   already replaced firmware's own tables at boot (swap the whole table
   set under `TTBR0_EL1` while code keeps executing) rather than a new
   incremental-remap primitive — the SVC handler holds IRQs masked
   throughout, the same reasoning that already makes
   `block_current_and_switch` safe. `mmu.rs` stashes the original boot
   memory map and extra-device list (`STORED_MEMORY_MAP`/
   `STORED_EXTRA_DEVICES`) specifically so this second call doesn't need
   UEFI boot services to reconstruct them.
4. **`tasks::spawn`** finds the first `Unused` slot, installs a fresh
   `Context` (entry = the loaded program's real relocated entry point,
   `sp_el0` = the top of its own region), and marks it `Runnable`.

**A real bug found building this, not a hypothetical:** the first
version hung completely — no exception, no output, `-d int` showed zero
aborts — when `spawn`'s ELF parsing path (`elf_region_size` →
`parse_program_headers`) used a `Vec` internally. That path runs both at
boot (where `alloc` is fine, boot services are still up) and from this
runtime syscall (where they aren't — `exit_boot_services` already made
the global allocator boot-services-backed and invalid). The allocator
doesn't panic or return an error in that state; it hangs. Fixed by
giving `parse_program_headers` a fixed-capacity `[ProgramHeader;
MAX_PROGRAM_HEADERS]` (16) instead of a `Vec`, with a hard
`TooManyProgramHeaders` error for the (currently unreachable) case of an
ELF exceeding that bound.

**Keyboard input is routed to a single designated owner task, not
whichever blocked task happens to ask first.** The first version of this
feature had a real bug here, found immediately by testing: with more
than one task simultaneously `Blocked(WaitReason::Keyboard)` (e.g. two
shell instances both idling on `read_char`), `on_tick`'s wake-check
polled every blocked task's wait reason once per tick in index order —
since the underlying poll (`syscall::poll_keyboard_byte`) destructively
consumes a byte the instant anything asks, a single keystroke went to
whichever task happened to still be blocked at that exact tick, which
flips from tick to tick as tasks trade being blocked and running.
Fixed by introducing `tasks::INPUT_OWNER_TASK` (hardcoded to task 0, the
boot-loaded shell — never destroyed, and there's no job-control
mechanism yet that could legitimately reassign this): the wake-check now
skips polling keyboard input entirely for every other task, so an
unconsumed byte just stays queued in the console/xHCI driver's own
hardware buffer until the owner task's own wait asks for it. A
non-owner task blocked on `Keyboard` simply stays blocked — it behaves
like a genuine background task with no input, not a second terminal
racing the first, but it also has no way to *become* the owner without a
future `fg`/job-control mechanism this kernel doesn't have yet.

**Real Parallels hardware confirmation:** the full flow — including the
success path, a second program really loading from disk and running —
is confirmed on real hardware, via the USB mass-storage path (`mount` a
passed-through stick holding the program binaries; see `CLAUDE.md`'s
"Driver isolation, part 1" section for the first such run, and the
userland-filesystem milestone for the same flow through the fsd
server).

**Blocking primitives.** A task that has nothing to do until some
condition holds (a keyboard byte arriving, another task exiting, a
message arriving — `WaitReason::Keyboard`/`TaskExit`/`Message`) can
block instead of busy-polling. `on_tick` runs a wake-check every tick
before its normal scheduling decision: for each blocked task, it
evaluates that task's wait reason, and if satisfied, stashes the
resulting value into the task's saved context (`x0`) and marks it
runnable again — the task resumes exactly where its blocking syscall
left off, with the value already in hand, indistinguishable from the
syscall having simply returned it. Worst-case wake latency from the
tick path is one tick period — but message delivery doesn't wait for
it: `tasks::send_message` *directly delivers* to a destination already
blocked in a matching message wait (copy into its buffer, stash its
`x0`, mark it runnable), the eager version of the wake-check's own
logic, which is what makes `msg_call`'s synchronous round trips — and
therefore every filesystem operation — sub-tick. The shell's main loop
uses `read_char` instead of the busy-poll it used before — see
`shell/src/main.rs`'s `main` for the current shape.

The tick period is 20ms (`timer::TICK_INTERVAL_MS`) — short specifically
so that a task waiting its turn (e.g. the shell, when task 1 currently has
the CPU) doesn't introduce perceptible input lag. A 1-second period was
tried first and produced up to a full second of latency, exactly as
unconditional round-robin scheduling would predict.

## Syscall ABI

Convention: syscall number in `x8`, up to 4 arguments in `x0`-`x3`, return
value in `x0` — chosen to match Linux's shape (a reasonable default for a
"POSIX-ish" project), not for ABI compatibility with anything. Grew from
1 argument to 4 for phase 3c's file-I/O syscalls, which needed a path
pointer/length and a buffer pointer/length at once — see
`exceptions.rs`'s module doc comment for how the SVC trampoline marshals
them. The eight `fs_*` syscalls that motivated that growth are gone
again (numbers 7–14 are deliberate gaps now): the filesystem lives in
userland (the `fsd/` server — see "The filesystem server" below), and
file operations are IPC requests to it, not syscalls. Every number and
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
| *7–14* | *(gaps)* | | | The eight `fs_*` syscalls (`list_dir`/`read_file`/`mkdir`/`rmdir`/`touch`/`rm`/`write_file`/`mv`) lived here until the filesystem moved out of the kernel entirely. Their exact contracts survive unchanged as the filesystem server's `FSOP_*` request protocol (below); the numbers stay unfilled, same stable-ABI reasoning as the gap at 5 |
| 15 | `read_char` | ignored | a byte | Blocking — if none is waiting, suspends the calling task and switches to another runnable one instead of returning `NO_CHAR`; resumes with the byte once available. See `tasks.rs`'s `block_current_and_switch` and the "Blocking primitives" section below |
| 16 | `spawn` | total staged length | `0`, `SPAWN_ERROR`, or a `SPAWN_ERR_*` code | Parses, relocates, and starts the program image previously fed into the kernel's 128KB staging buffer via `spawn_stage` (30), as a **new, independent task** alongside the caller — `tasks::spawn`, not exec-replaces-current-process. **Contract changed with the userland-filesystem milestone**: it used to take a path, but the kernel has no filesystem to read one with anymore — the caller reads the program via the filesystem server (`FSOP_READ_AT`, 512 bytes at a time) and stages it first; see the shell's `cmd_exec` for the reference flow. Failures: `SPAWN_ERR_BAD_ELF`/`SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_NO_FREE_SLOT`; a failure after the region allocation gives the memory back (`tasks::free_runtime_region` — always the LIFO case). See "Dynamic task creation" above |
| 17 | `exit` | exit code | **does not return** (or `EXIT_DENIED`) | Destroys the calling task: its slot becomes spawnable again, its EL0 mapping is removed (a fresh `mmu::rebuild_with_el0_regions`), and its RAM is returned to the runtime bump allocator when LIFO order allows (`tasks::free_runtime_region` — most-recent allocation only; anything else leaks, deliberately). Tasks 0 (the boot shell — the sole keyboard owner), 1 (idle), and 2 (the filesystem server) are refused with `EXIT_DENIED`, the only case where this syscall returns. The exit code is kept as `Zombie(status)` until a `wait` collects it |
| 18 | `task_state` | task index | a `TASK_STATE_*` code, or `TASK_STATE_INVALID` past the last slot | Read-only observability for the spawn/exit lifecycle (the shell's `ps`): `UNUSED`/`RUNNABLE`/`BLOCKED` per slot. Probing indices upward until `INVALID` is how a caller discovers the slot count without it leaking into the ABI |
| 19 | `kill` | task index | `0`, `TASK_ERR_PROTECTED`, or `TASK_ERR_NO_SUCH_TASK` | Destroys another task — `exit`'s teardown minus the context switch (a non-current task isn't executing; its saved context is simply discarded). Tasks 0–2 (boot shell, idle, filesystem server) are protected. If the killed task held the keyboard, ownership reverts to task 0 |
| 20 | `fg` | task index | `0`, `TASK_ERR_PROTECTED`, or `TASK_ERR_NO_SUCH_TASK` | Hands keyboard ownership to the given task (the wake-check's input-owner state, previously hardcoded to task 0). The caller's own next blocking read then waits until the foregrounded task exits, is killed, or the user types Ctrl+C (`0x03`, intercepted at the keyboard-poll choke point whenever a non-boot-shell task owns the keyboard — ownership reverts to task 0 and the byte is swallowed; reclamation, not a signal). Idle can't be foregrounded; index 0 is an explicit "give it back" |
| 21 | `wait` | task index | exit status `0..=255`, `TASK_KILLED_STATUS` (`0x100`), `WAIT_INTERRUPTED`, `TASK_ERR_PROTECTED`, or `TASK_ERR_NO_SUCH_TASK` | Blocks until the task dies (via `WaitReason::TaskExit` — the same blocking machinery as `read_char`), or returns immediately if it's already a zombie. **Collecting the status is what reaps**: an exited task holds its slot as `Zombie(status)` until waited (`kill` reaps immediately instead — the killer already knows the outcome). Ctrl+C interrupts a wait (the target keeps running); other typing during a wait is discarded. Waiting on 0–2/yourself is refused (guaranteed deadlock — those tasks never die) |
| 22 | `mount` | replace flag | `0` (a USB device was installed), `MOUNT_ALREADY`, or `MOUNT_NO_DEVICE` | **The device half only** since the filesystem moved to userland: rescans the xHCI ports for storage devices that attached after boot (`xhci::rescan_ports`) and installs the first mass-storage device (`usb_msd.rs`, Bulk-Only Transport + SCSI) as the kernel's block device for the server to reach via `block_read`/`block_write`. Actually mounting what it holds is the server's `FSOP_MOUNT` request; the shell's `mount` command composes the two, server first. `replace != 0` allows swapping out an installed-but-unmountable device (e.g. `make run`'s FAT16 vvfat disk) — callers pass it only after the server confirms nothing is mounted. The Parallels workflow is unchanged: boot, wait, `mount` |
| 23 | `msg_send` | dest task, buf ptr, len | `0`, `TASK_ERR_NO_SUCH_TASK`, `MSG_ERR_TOO_BIG` (>64 bytes), or `MSG_ERR_FULL` | IPC: copies the message into the destination's bounded mailbox (4 pending max). No shared memory, no blocking sends. A dead task's queued mail is cleared on exit/kill — a slot's next occupant can never receive a predecessor's messages |
| 24 | `msg_recv` | buf ptr, len | `(sender << 32) \| copied_len`, or `RECV_INTERRUPTED` (Ctrl+C) | Blocks until a message arrives (`WaitReason::Message` — the third user of the blocking machinery), copying the oldest queued message into the buffer |
| 25 | `msg_try_recv` | buf ptr, len | same as `msg_recv`, or `NO_MSG` | The non-blocking sibling, same pairing as `try_read_char`/`read_char` |
| 26 | `block_info` | — | capacity in sectors, `BLOCK_ERR_NO_DEVICE`, or `BLOCK_ERR_DENIED` | Raw block-device introspection for the filesystem server. **All three `block_*` syscalls are gated to task 2** (`FSD_TASK`): any other caller gets `BLOCK_ERR_DENIED` — the kernel holds the device, and exactly one task may ask it to touch the disk |
| 27 | `block_read` | LBA, buf ptr | `0` or a `BLOCK_ERR_*` code | Reads exactly one 512-byte sector (the length is implied, not passed). Same `FSD_TASK` gate |
| 28 | `block_write` | LBA, buf ptr | `0` or a `BLOCK_ERR_*` code | Writes exactly one 512-byte sector. Same gate |
| 29 | `msg_call` | dest task, req ptr, req len, reply ptr | `(dest << 32) \| reply_len`, `RECV_INTERRUPTED`, `TASK_ERR_NO_SUCH_TASK`, `TASK_ERR_PROTECTED` (self-call), or a `MSG_ERR_*` code | Synchronous request/response (MINIX's `sendrec` shape): sends the request, then blocks for a reply **from `dest` specifically** — a message from any other task stays queued for a later `msg_recv` instead of being mistaken for the reply (`WaitReason::Message`'s sender filter). With direct delivery on both hops (`tasks::send_message` copies straight into a matching blocked receiver's buffer and wakes it), a call to a server blocked in `msg_recv` round-trips without waiting for a tick on either side. The reply buffer is a fixed 64 bytes — the 4-argument ABI is exactly full |
| 30 | `spawn_stage` | offset, chunk ptr, chunk len | `0` or `SPAWN_ERROR` | Copies one chunk of a program image into the kernel's spawn staging buffer — the feed half of the two-step `spawn` (16) |
| other | — | — | `u64::MAX` | Logged as unknown |

### The filesystem request protocol (`FSOP_*`)

File operations are messages to the filesystem server (task 2,
`FSD_TASK`), normally sent via `msg_call`. A request is 56 bytes
(`FS_REQUEST_LEN`): the op as a little-endian u64 at offset 0, then six
little-endian u64 arguments — pointers and lengths into the *caller's*
memory, which the server reads and writes directly (the same
single-address-space trust model the old `fs_*` syscalls' pointer
arguments had, now spanning two EL0 regions instead of EL0-to-kernel).
The reply is a single u64 in the reply message's first 8 bytes, carrying
exactly the old syscalls' return-value semantics — byte counts and real
sizes on success, or a value in the reserved error band (`>= FS_ERR_MIN`):
`NO_FS` when nothing is mounted, a specific `FS_ERR_*` code per real
failure reason, `FS_ERROR` for argument-validation rejections. A call to
an empty task-2 slot (no FSD.BIN on the ESP this boot) fails at the
`msg_call` layer with `TASK_ERR_NO_SUCH_TASK`, which the shell's
wrappers fold into `NO_FS` — literally true. Ops (constants in
`syscall-abi`, contracts identical to the old syscalls of the same
names): `FSOP_LIST_DIR`, `FSOP_READ_FILE`, `FSOP_READ_AT` (the one new
capability — windowed reads at a byte offset, what `exec`'s chunked
program loading is built on), `FSOP_WRITE_FILE` (zero-length data is
valid — truncate to empty), `FSOP_MKDIR`, `FSOP_RMDIR`, `FSOP_TOUCH`,
`FSOP_RM`, `FSOP_MV`, and `FSOP_MOUNT` (the FS half of the `mount`
command). The shell's nine `fs_*` wrapper functions
(`shell/src/main.rs::fs_call` and friends) are the reference client.

The `syscall-abi` crate covers the numbers, sentinels, and protocol
constants, not argument validation — see
[`processes.md`](processes.md)'s "known rough edges" for what's still a
real gap (pointer/length arguments are trusted, not checked against the
caller's actual mapped region — by the kernel and the server alike) and
for how to depend on this crate when writing a new userland program.

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
   **Confirmed dead on Parallels**, with real evidence, not just an
   untested guess: a full PCI device inventory taken on real hardware
   (via `pci::log_all_devices`, a diagnostic run whenever the three
   mechanisms above all fail) shows virtio's real vendor ID present only
   for networking, no virtio-console device ID over PCI at all, and no
   direct evidence of one over MMIO either. Parallels' actual serial
   port is very likely a proprietary device (PCI vendor `0x1ab8`, class
   `0xff`/unclassified) with no public specification — see `CLAUDE.md`'s
   "virtio-console" section for the full device inventory and reasoning.

When all three of devicetree/ACPI/PCI 16550 fail, `pci::log_all_devices`
(`pci.rs`) logs every PCI device's vendor:device and class:subclass
through the still-working boot-services console — a general-purpose
diagnostic, not specific to virtio, kept as a real tool for whatever the
next "why can't this platform's hardware be found" question turns out
to be.

There is deliberately no hardcoded fallback address for any driver: one
existed early in this project and was removed after confirming it
hard-crashes real Parallels hardware.

## What's not here yet

This document describes the current, working state. For active
limitations and what's reasonably next, see `CLAUDE.md`'s "Next milestone"
section, which is kept current as the project's actual state changes —
not duplicated here, since it would just go stale.

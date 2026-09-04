# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Start here

`scratch/daily-standup.md` — written at the end of the previous working day to
be read at the start of the next: where the tree was left, what went in, and
what is outstanding. `scratch/` is gitignored and is not part of this
repository, so the file is absent on a fresh clone and on any day that was not
closed out. When it is absent, `git log` and the documents named below are the
way in.

## What this is

Ouroboros is an ARM64 (aarch64) operating system written in Rust (plus some
assembly where needed), still in its earliest stages. Design goals, taken
from the project's original brief and `README.md`:

- Microkernel architecture
- POSIX-ish system calls
- Preemptive multitasking
- Filesystem choice still undecided (research needed; likely a simple FS first)
- Draws ideas from Linux, Minix, and Plan 9
- Primary test target is Parallels on Apple Silicon, not just an emulator

## Where the history and design rationale live

This file used to also carry a milestone-by-milestone narrative of
everything built so far. That history now lives under `docs/`, so this
file can stay focused on durable, load-bearing guidance. `docs/README.md`
is the full annotated index of everything under `docs/`; the pointers below
are the ones worth having in mind before you open it:

- **`docs/CHANGELOG.md`** — the full milestone record, phase 0 to the
  present, newest first. Every completed step (the shell, disk/FAT32
  support, USB keyboard + storage, the userland servers, the network
  stack, …) is recorded there in condensed form. Check it for *what was
  built, and why it works the way it does*.
- **`docs/ROADMAP.md`** — the forward-looking plan of known future work
  (open frontier, remaining follow-ups, north-star directions, open gaps).
- **`docs/roadmap-completed.md`** — the finished arcs that used to live in
  `ROADMAP.md`, moved out so the roadmap stays forward-looking (the
  *plan-shaped* companion to `CHANGELOG.md`'s condensed milestone log).
- **The postmortems under `docs/`** (twenty-nine of them) — the design, bug
  and process retrospectives: *the traps already hit and the lessons
  learned*. Read the relevant one before reworking a subsystem.
  `docs/README.md` indexes all of them with a full annotation each, and every
  postmortem itself opens with an abstract and its spine in a blockquote — so
  the index is enough to pick one. The five whose lessons are load-bearing
  well beyond the subsystem they came from:
  - `cluster-keys-postmortem.md` — *a step is only verifiable if the check can
    fail*. Most of that arc's real findings were checks that could not.
  - `repairing-the-repairs-postmortem.md` — *a repair is a change, and changes
    have the same defect rate as the code they fix*; keep the fix the size of
    the bug.
  - `blind-instruments-postmortem.md` — *the observer is a check too, and it is
    the one nobody mutates*. Five tools reported success while proving nothing.
  - `unspellable-postmortem.md` — *make the wrong thing unspellable, not
    un-grepped*: a required parameter beats an opt-in wrapper.
  - `review-and-split-postmortem.md` — *a green signal is a claim, not
    evidence*, and *a diff too big to review is too big to fix*.
  - `true-when-written-postmortem.md` — *the comment was true when it was
    written; that is the problem*. A claim that guards behaviour is a check
    nobody has written yet, and the edit that falsifies it is always in
    another file — so compiler, tests and review all miss it.
- **`docs/journal.md`** — a chronological dev-log (narrative "what and why
  each day"), a lighter companion to the milestone-oriented `CHANGELOG.md`.
- **`docs/README.md`** — the annotated index of every document under `docs/`;
  **`docs/source-map.md`** — the annotated index of every source file. Both
  hold the long-form detail this file used to carry inline.
- **Reference docs**: `docs/architecture.md` (boot flow, privilege model,
  memory layout, exceptions, syscall ABI, console), `docs/processes.md`
  (userland loading, the ELF/PIE binary format, and the relocation-class
  traps — **read this before writing any userland program**), and
  `docs/shell-commands.md` (the shell builtins).

The sections below are the deliberate exception: durable "read this before
touching the code" guidance for the boot path, console discovery, the MMU
switch, exception vectors, the timer tick, the syscall boundary, and task
switching — kept inline because a session editing that code needs it at
hand. `docs/CHANGELOG.md` also covers these as milestones; the versions
here are the ones to keep current when the code changes.

## Boot architecture (read this before touching boot/entry code)

The kernel builds directly as a UEFI application for the
`aarch64-unknown-uefi` target — there is no separate bootloader stage yet.
This is a deliberate choice, not a placeholder to "fix later" casually:
Parallels boots ARM VMs exclusively through UEFI firmware, with no
equivalent to QEMU's `-kernel` direct-boot shortcut, so UEFI is the only
boot path that works on both the fast QEMU dev loop and the real Parallels
test target. Don't reach for direct-kernel-boot conveniences (multiboot,
raw `-kernel` loading, etc.) without accounting for the fact that they won't
work on the Parallels target.

The `[[bin]]` in `kernel/Cargo.toml` is named `BOOTAA64` deliberately: UEFI
firmware auto-boots removable media at `\EFI\BOOT\BOOTAA64.EFI`, so the
build output can be staged straight into an ESP layout with no renaming step.

As the kernel outgrows what's reasonable to run under UEFI boot services,
expect a split into a thin UEFI bootloader stage that loads and hands off to
a separate kernel binary. That split hasn't happened yet.

`main()` never returns `Status::SUCCESS` — it logs a boot message and parks
the core in a `wfe` spin loop (`halt()` in `kernel/src/main.rs`) instead.
Returning to firmware is a dead end for kernel code.

`main()` now calls `boot::exit_boot_services(None)` partway through and
permanently leaves the UEFI environment. Everything before that call may use
`log::*`, `alloc`, and UEFI protocols as normal; everything after may not —
the UEFI logger and global allocator are boot-services-backed and will
panic/misbehave if touched post-exit. Console output after that point goes
through `kernel/src/uart.rs`, a polling PL011 driver taking a runtime base
address — no fallback address, see below. The returned memory map is kept
(not discarded) — `mmu.rs` uses it to identity-map real discovered RAM.

### Console discovery: three mechanisms tried, all confirmed dead ends on Parallels — the real lead is now built, see "virtio-console" below

**Status as of this writing (historical - see the "virtio-console"
section further below for what actually got built and where it
stands):** all three mechanisms below are confirmed not to work on
Parallels. There's a real, promising lead for what actually would
(virtio-console, at the end of this section) — but implementing it is a
genuinely different, smaller-than-AML subsystem (virtio device
discovery, feature negotiation, a transmit virtqueue), not a quick
follow-up, so it was deliberately not started this session. Kernel
development continues against QEMU (which has a fully working console via
mechanism 2 below) in the meantime.

Three modules, tried in this order by `discover_console()` in `main.rs`,
each logging why it failed before the next is tried:

1. **`kernel/src/devicetree.rs`** — `EFI_DTB_TABLE_GUID` in the UEFI config
   table. **Confirmed dead on both platforms**: QEMU's bundled firmware
   (Homebrew's `edk2-stable202408-prebuilt.qemu.org`) and Parallels' own
   firmware both report `DiscoveryError::NoDtb` — both are ACPI-oriented.
   Kept tried first in case it's ever useful on other hardware.
2. **`kernel/src/acpi.rs`** — hand-rolled RSDP → XSDT → SPCR table parsing
   (not the `acpi` crate — this only needs a handful of fixed-offset struct
   reads, nothing like devicetree's variable-length format that justified
   pulling in `fdt`). **Confirmed working on QEMU**, first try: resolves the
   same PL011 address (`0x0900_0000`) that used to be hardcoded, except now
   via genuine discovery, and the post-exit UART write to it succeeds
   (`boot services exited, console live` actually prints) — the first time
   that's happened in this project through anything other than a hardcoded
   guess. **Confirmed dead on Parallels**, and not from a parsing bug: RSDP
   and XSDT both parse fine there (so ACPI itself is present and readable),
   but there is no SPCR table entry at all among Parallels' ACPI tables —
   `DiscoveryError::NoSpcr`. Tested with *and without* a serial port device
   added to the VM's hardware settings — same result either way, so this
   isn't "no device configured," it's "Parallels doesn't describe its
   console via SPCR regardless."
3. **`kernel/src/pci.rs`** — enumerates PCI devices via the boot-services
   `PciRootBridgeIo` protocol, looking for a PCI class 0x07 subclass 0x00
   ("Serial controller") device — the 8250/16450/16550 family, per the PCI
   Code and ID Assignment spec, *never* how a PL011 would be identified over
   PCI. A match here is a genuinely different piece of hardware from what
   the other two modules look for, hence `kernel/src/uart16550.rs` (a
   completely different register layout: THR/LSR, not PL011's DR/FR) and
   `console::Console`'s two variants. Verified end-to-end on QEMU (which has
   no such device on the default `virt` machine, so this correctly reports
   `NoSerialDevice` there rather than erroring out on the protocol calls
   themselves). **Confirmed dead on Parallels too**: `DiscoveryError::NoSerialDevice`
   — no PCI serial controller there either. `uart16550.rs`'s register stride
   (4 bytes) was a guess for this attempt, never actually exercised against
   real hardware.

All three failing cleanly, on real hardware, without crashing the VM — that
itself is a real (if less exciting) confirmation that removing the hardcoded
fallback address and adding exception vectors did what they were meant to:
three wrong guesses in a row, and Parallels just stays up.

**The real lead: virtio-console, not a classic UART at all.** Search
research (not yet verified against this project's own code) turned up that
Parallels' Apple Silicon virtualization is built on macOS's Virtualization
framework, which exposes devices — network, storage, entropy, *and serial
port* — via virtio, and that `console=hvc0` (`hvc` = "hypervisor console",
Linux's virtio-console driver name) is the standard console parameter for
this class of VM. That would cleanly explain all three dead ends above:
none of them were ever going to find a virtio device, because virtio
consoles don't work like a UART — no simple MMIO byte registers, just
virtqueues (descriptor rings in memory), feature negotiation, and a
transport (virtio-mmio or virtio-pci). **Update: implemented — see the
"virtio-console" section further below.** It turned out this couldn't
land on the boot-services side of `exit_boot_services` the way the
paragraph below originally called for; see that section for the real
constraint (device-region mapping under this kernel's own MMU tables,
not firmware's) that forced a later placement instead.

`find_dtb`/`find_rsdp` (need the UEFI config table) and each PL011 module's
`discover_pl011` (pure memory parsing, no boot service) run **before**
`exit_boot_services`, even though the parsing halves don't themselves need
boot services — so the result gets logged through the UEFI console (works
on any platform) before any raw MMIO is touched. `pci.rs`'s
`discover_uart16550` has no such split — PCI enumeration is entirely
boot-services-based throughout, so the whole thing runs before exit, no
part of it could run after even if it wanted to. **This paragraph
originally continued: "keep any future discovery mechanism (virtio-console
included) on this same side of the `exit_boot_services` call" — turned out
not to be possible for virtio-console specifically; see the
"virtio-console" section below for why, kept here rather than silently
edited away since it was the real plan going in, not a mistake to erase.**

### There is no fallback UART address, and there should not be one again

There used to be one — `uart::QEMU_VIRT_PL011_BASE`, written to whenever
devicetree discovery failed. It was removed after direct confirmation that
it hard-crashes real Parallels hardware: nothing is mapped at QEMU's PL011
address on Parallels' virtual chipset, so the write faults, and (at the
time) with no exception vectors installed, that fault had nowhere to go and
took the whole VM down. `main.rs` now only ever constructs a `Uart` when
`discover_pl011` actually returned an address (`if let Ok(base) = discovery`
in `main()`) — no confirmed address means no post-exit console output, not
a guess. Exception vectors exist now (below), so a bad guess would at least
be survivable if this were reintroduced — but there's still no reason to
guess when the alternative is just not writing to memory you don't have a
real address for.

### Exception vectors: implemented and verified working, not just compiling

`kernel/src/exceptions.rs` installs a minimal AArch64 vector table (VBAR_EL1)
right after `exit_boot_services`, before anything else gets a chance to
fault. On any synchronous exception, IRQ, FIQ, or SError, it reports
`ESR_EL1`/`FAR_EL1`/`ELR_EL1` and the vector index through the global
console (`kernel/src/console.rs` — a `Sync`-wrapped
`UnsafeCell<Option<Console>>` shared between `main()` and the exception
handler, since a fault needs somewhere to report through too; `Console` is
a plain enum over `Uart`/`Uart16550`, not `Box<dyn fmt::Write>` — a trait
object would need to allocate, and the console is only ever installed after
`exit_boot_services`, where the global allocator is boot-services-backed
and no longer usable) and halts, rather than leaving a bad access to run
into whatever an unconfigured VBAR_EL1 does. This kernel has only ever been
observed at EL1 (typical for a UEFI OS loader) — not verified at any other EL.

**A real gotcha hit and fixed while building this, worth not repeating:**
the vector table was first placed in a custom section (`.section
.text.exceptions`). It linked fine and `VBAR_EL1` pointed at the right
address, but jumping to it faulted immediately — `ESR_EL1` decoded to EC
0x21 (Instruction Abort, same EL) with IFSC `0x0F` (Permission Fault, level
3): the page existed but wasn't executable. The PE/COFF backend apparently
doesn't infer executable-section characteristics for an unrecognized custom
section name the way it does for plain `.text`. Fixed by using `.text`
directly in the `global_asm!` block instead of a custom section name.

**How this was actually verified**, since "it compiles" proves nothing for
assembly wired up this way: temporarily forced a console onto QEMU's known
real PL011 address (`0x0900_0000`, confirmed valid all session) right after
`exceptions::install()`, then deliberately did `write_volatile(0 as *mut
u8, 0xAB)` — 0x0 being unmapped in QEMU's `virt` memory map (RAM starts at
0x40000000) — and ran QEMU with `-d int -D <logfile>` to get its own
internal exception trace independent of anything our kernel prints. First
attempt (the custom-section version) showed the initial Data Abort dispatch
correctly, immediately followed by an endless identical `Prefetch Abort` at
the vector table's own address — a fault loop, the same failure shape that
made Parallels report a crash. After the `.text` fix: exactly one Data
Abort, our own handler's line printed (`EXCEPTION vector=4
esr_el1=0x96000047 far_el1=0x0 elr_el1=0x...`), and the trace showed no
further exceptions — a clean, stable halt. That temporary test code (forced
console + deliberate fault) was removed after confirming this; don't expect
to find it in `main()`.

### MMU: identity-mapped on our own tables, not firmware's — a real starting-level bug, worth reading before touching this again

`kernel/src/mmu.rs` replaces firmware's translation tables with our own
right after `exit_boot_services` + `exceptions::install()`. Firmware runs
with paging already on; this doesn't turn the MMU on, it swaps which tables
`TTBR0_EL1` points at while it stays continuously enabled — same MAIR_EL1/
TCR_EL1/TTBR0_EL1 write sequence, barriered with `dsb`/`isb`/`tlbi vmalle1`/
`ic ialluis` throughout. Deliberately coarse: two 1GB block mappings, one
Device (fixed low 1GB, a QEMU-shaped convention like the console addresses)
and one Normal WB executable RAM block — but the RAM range comes from the
*real* UEFI memory map (`exit_boot_services`'s return value, no longer
discarded), not a hardcoded address. Same lesson as the UART fallback:
hardcoding a QEMU-specific address here would risk the identical failure
mode on Parallels.

**A real bug, not a style choice: the walk starts at L0 (`T0SZ=20`,
matching firmware's own config, read back and verified at runtime), via a
2-level L0→L1 table, not a single L1 table with `T0SZ=25`.** The single-table
version was tried first — architecturally legal, every table entry and
every TCR_EL1/MAIR_EL1 bit hand-verified correct against authoritative
bit-layout references (Linux's `pgtable-hwdef.h`, `arch/arm64/tools/sysreg`)
and independently re-derived with a throwaway Python decode of the actual
runtime register values — and it hard-faulted anyway: a Permission fault at
translation level 2 on the very next instruction after the switch, then an
identical fault on the exception vector table itself, looping forever. PXN/
UXN weren't it (removing both entirely changed nothing). What fixed it,
confirmed by direct A/B test with everything else held constant, was
matching firmware's *starting level* rather than switching to a different
one. Full write-up of the debugging path is in `mmu.rs`'s module doc
comment — read it before ever "simplifying" this back to one table.

Verified two ways, not just "it prints a confirmation line": the informational
log line reports the real discovered RAM span and block range, and a
temporary deliberate fault at an address genuinely unmapped by the new
tables (block index 2) confirmed the exception handler still works
correctly *after* the switch, under our own tables, not just under
firmware's — different code path than the exception-vector verification
above, worth re-testing again if this module changes.

### Timer-driven preemption tick: GICv2 + ARM generic timer, real IRQ round-trips

**Superseded for anything platform/address-related by the "MADT/GICv3"
section much further below - kept here as accurate history of this
milestone, not silently rewritten.** `gic.rs` is now a version-dispatch
facade (`gicv2.rs`/`gicv3.rs` backends), and its addresses come from a
real ACPI MADT parse (`madt.rs`), not the QEMU devicetree dump described
in this section.

`kernel/src/gic.rs` (GICv2 distributor + CPU interface) and
`kernel/src/timer.rs` (ARM generic non-secure EL1 physical timer, PPI 14 →
GIC INTID 30) give the kernel a periodic 1-second tick, delivered as a real
IRQ that interrupts `halt()`'s `wfe` loop and resumes it afterward — the
first exception this kernel needs to *return from* rather than report-and-
halt (see below).

Addresses/GIC version are a QEMU-shaped convention like `mmu.rs`'s device
region, but not guessed from memory this time: confirmed for *this* QEMU
install by dumping its internal devicetree
(`qemu-system-aarch64 -machine virt,dumpdtb=...` — QEMU always builds this
internally regardless of whether firmware exposes it to the guest; nothing
our own kernel reads at boot) and inspecting it with `dtc`. That's how the
GICv2 addresses (GICD 0x08000000, GICC 0x08010000) and the timer PPI number
were pinned down, not assumption. Register-bit-level details (GICD/GICC
offsets, generic timer CTL bits) were cross-checked against Linux headers,
same discipline as `mmu.rs`.

**The IRQ vector had to become fundamentally different from the other 15.**
Every other vector in `exceptions.rs` shares one path: capture ESR/FAR/ELR,
report, halt — it never returns, so it never needed to preserve anything.
IRQ is the first one that has to *resume* the interrupted code, so its
vector slot (index 5, IRQ at EL1h) got its own trampoline: full x0-x30 +
ELR_EL1/SPSR_EL1 save to the stack, a normal `bl` into Rust (not the
diverging `b` the other 15 use), full restore, `eret`. FP/SIMD (Q0-Q31)
registers are deliberately *not* saved — nothing running today uses them,
and the only interrupted context that exists is `halt()`'s trivial spin
loop. That stops being safe the moment real interruptible work with FP/SIMD
state exists, which matters for whenever actual task switching is
built, not this milestone.

**Verified as sustained, not just "it prints once":** ran under QEMU for
20+ seconds, confirmed 14 consecutive ticks at the correct ~1-second
spacing, no corruption, no drift, no crash — meaningful because a save/
restore bug wouldn't necessarily show up on the *first* round-trip, only
after several. Cross-checked against QEMU's own `-d int` exception trace
for the same run: zero aborts across the entire session, confirming
nothing is silently faulting alongside the visible tick output.

Worked correctly on the first real boot attempt — a contrast worth noting
against the MMU work, where every value was *also* verified correct ahead
of time and it still took real debugging to find the actual bug. Getting it
right on paper isn't a substitute for booting it, but it isn't worthless
either.

### Syscall boundary: EL0 entry, the svc trap, and EL0 actually running code — all confirmed working

`kernel/src/syscall.rs` drops to EL0 (`enter`) and provides an `svc`-based
syscall path back to EL1 (`dispatch`, called number-in-x8/arg0-in-x0,
return-in-x0, Linux's convention, chosen as reasonable for a "POSIX-ish"
project — not Linux-ABI-compatible, just a familiar shape). `exceptions.rs`
has a second resumable vector path for this (`3:`): slot 8 (Synchronous,
lower EL AArch64) checks ESR_EL1's EC field first, since EL0 faults land in
the same slot as `svc` — only EC=0x15 takes the syscall trampoline, anything
else falls through to the ordinary diverging report-and-halt path shared
with every other vector. Slot 9 (IRQ, lower EL AArch64) reuses the exact
same resumable IRQ trampoline as slot 5 — a tick firing *while EL0 runs*
lands in a different vector slot than one firing at EL1h, easy to miss and
would have silently broken tick delivery the moment EL0 started running.

**The real blocker (first attempt): EL0 had no memory it was allowed to
execute from.** Sharing `mmu.rs`'s single EL1-only RAM block between EL0
and actively-executing kernel code, then trying to just flip its
permissions to also allow EL0, hard-faulted for reasons a first,
extensive investigation could not resolve (see git history around the
"second unresolved mystery" commit if the detail ever matters again).

**Resolution: give EL0 a genuinely separate, isolated region instead.**
`syscall.rs` now reserves one dedicated 8KB slot (not the originally-planned
2MB — see `syscall.rs`'s module doc comment for the precisely-bisected
`rustc`/PE-COFF hard limit, a real compiler crash bug, not a design choice,
that forced 8KB) holding only the EL0 demo task and its stack; `mmu.rs`
grew a fourth translation table level (L3, 4KB pages) to give *only that
region's own pages* EL0 access, while every other page/block in RAM —
including all the kernel code that keeps running immediately after the
table switch — stays on the already-proven-safe EL1-only permissions. This
worked, and one more real bug turned up immediately once it did: EL0's own
`wfe` traps to EL1 by default (`SCTLR_EL1.nTWE`/`nTWI`, unrelated to the
mapping work), diagnosed directly from the exception's EC value and fixed
in `syscall.rs`.

**Confirmed working end to end, not just "boots":** the EL0 demo task's
real `svc` round-trip succeeds (`syscall from EL0 (number=0, arg0=0x2a)`),
and 14 consecutive timer ticks fired correctly afterward with no repeated
faults — EL0 reached and stayed in its post-syscall idle loop, correctly
preempted and resumed by the tick each time. Cross-checked against QEMU's
own `-d int` trace: exactly one `[SVC]` exception, zero aborts across the
whole run.

### Preemptive task switching: a real task struct, two EL0 tasks, confirmed alternating

`kernel/src/tasks.rs` is the first real scheduler this kernel has had. Before
this milestone, "preemption" meant exactly one thing: the tick IRQ correctly
interrupted and resumed whatever was running (`halt()`'s spin loop, or the
syscall-boundary milestone's single EL0 demo task) — there was never more
than one thing to switch *between*. Now there are two independent EL0 tasks,
and the tick is what alternates them.

**Design, built directly on the syscall-boundary milestone's isolation
approach**, not a rework of it: one EL0-accessible region, still the 8KB
ceiling forced by the `rustc`/PE-COFF alignment bug (see the syscall-boundary
section above and `tasks.rs`'s own module doc comment) — but now split into
two 4KB slots, one task each, since 4KB is `mmu.rs`'s finest page granularity
anyway. Each task is a tiny hand-written `global_asm!` loop (report in via a
new syscall, `wfe`, repeat) differing from the other only in a hardcoded task
ID. There's no cooperative yielding anywhere — the tick catching a task
mid-`wfe` and swapping its saved context for the other task's is the *only*
thing that ever moves execution from one to the other.

**`exceptions.rs`'s resumable IRQ trampoline (`2:`) needed one real change to
make this possible, not just a new caller.** Previously it saved/restored
`x0`-`x30`/`ELR_EL1`/`SPSR_EL1` and discarded them once the interrupted code
resumed — fine when there was only ever one context to return to. Task
switching requires handing that saved frame to Rust *as data*, not just
scratch space: `SP_EL0` was added to the saved set (it never needed to be
before — one context never needs to remember its own stack pointer), and the
whole frame's address is now passed to `rust_irq_handler` as an argument
(`mov x0, sp` before the `bl`). On a timer tick, `tasks::on_tick` overwrites
that frame in place — the interrupted task's registers copied out to its own
saved `Context`, the next task's saved `Context` copied in — and the
trampoline's restore-and-`eret` doesn't know or care that the frame now holds
different values than it saved a moment ago. That's the entire scheduler:
strict round-robin between exactly two tasks, no priorities, no blocking, no
queue, implemented as a struct-copy inside an existing IRQ path rather than
any new control-flow mechanism.

A new syscall (`report`, number 2) proves it's real: each task's loop calls
it with its own task ID as `arg0`; `syscall.rs` keeps one counter per task
(`TASK_REPORTS`) and prints `task {id} report #{count}`. This is also what
confirmed the earlier "second syscall" milestone's dispatch table actually
generalizes — three syscalls now (`print`, `double`, `report`), not one.

**Confirmed working, sustained, cross-checked, not just "boots":** a 20-second
QEMU run produced clean strict alternation — `task 0 report #1`, `tick 1`,
`task 1 report #1`, `tick 2`, `task 0 report #2`, ... — through at least 15
ticks with no skips, repeats, or out-of-order reports. Cross-checked against
QEMU's own `-d int` trace for the same kind of run: exactly 16 `[SVC]`
exceptions for 16 report lines (8 per task), 538 `[IRQ]` exceptions (the
tick, firing roughly every ~37ms of wall time under TCG emulation — not
literally 1000ms per `timer.rs`'s nominal interval, expected under emulation
and not itself a bug), and zero aborts across the whole run.

**Still coarse, worth knowing before building on it:** exactly two tasks,
hardcoded, no task creation/destruction API; no priorities or blocking, just
round-robin; FP/SIMD state still isn't part of `Context` (inherited
limitation from `exceptions.rs`, see its module doc comment) — fine since
neither task's hand-written code touches it, but a real limitation for
whatever runs here next; and both tasks still share the one 8KB region's W^X
weakness noted in the syscall-boundary section (code and stack are both
executable, no separation) — now doubled, since it's true per-task rather
than a one-off.

## Commands

```sh
make build                  # cargo build (debug) - kernel only, see below
make build PROFILE=release  # release profile
make shell-bin               # build shell/ for aarch64-unknown-none + strip (same pattern: hello-bin, pong-bin, fsd-bin)
make run                    # stage ESP dir (kernel + userland binaries incl. the fsd filesystem server + config) + boot in QEMU with a virtio-mmio block device attached (fast dev loop - vvfat backing, FAT16, not FAT32 - see "Phase 3b")
make run-virtio-console      # same as `run`, plus a virtio-mmio console device attached (for testing virtio_console.rs - see "virtio-console" above for why this alone doesn't organically trigger the fallback)
make run-net                 # same as `run`, plus a virtio-net device on virtio-mmio + QEMU user-mode (SLIRP) networking + an -object filter-dump pcap (net.pcap) - the dev loop for the network stack (kernel/src/virtio_net.rs, Stage 1); init_net's boot ARP probe exercises SLIRP's gateway, see "Network stack, Stage 1" above
make run-image-net           # `run-image` (real FAT32, disk commands work) *and* the NIC from `run-net` in one boot - the fullest QEMU run: fsd mounts, the shell/disk commands work, and init_net's ARP probe runs, with net.pcap dumped
make run-image-server        # like run-image-net, plus SLIRP hostfwd tcp::5555->:80, so `curl http://localhost:5555/` on the host reaches netd's TCP HTTP server (the guest answering the network - see "Network stack, Stage 4b" above)
make run-image-9p            # run-image-server plus a second hostfwd tcp::5640->:564 for netd's 9P export listener - the host reads the GUEST's disk over TCP via scripts/np9p_client.py (cluster Phase 1 step 1a, the export gateway)
make run-image-9p-client     # run-image-net shape (NIC + real FAT32, no hostfwd needed): the GUEST remote-mounts a HOST-run 9P server (scripts/np9p_server.py, reached at 10.0.2.2 over SLIRP) - `mount -r 10.0.2.2:5641 /mnt/a; ls /mnt/a; cat /mnt/a/HELLO.TXT` (cluster Phase 1 step 1c, the remote-mount client)
make run-image-2vm-a         # TWO-VM cluster (Phase 1 step 1d): machine A - exports its disk over a shared L2 QEMU socket link (listen=:12340, MAC :0a -> IP 10.0.2.10). Run this FIRST (it listens), in its own terminal
make images-2vm              # build BOTH FAT32 node images (per-machine keypairs: the rig can no longer copy one image twice)
make images-2vm-ext2         # the same for the ext2 pair. BUILT TOGETHER BY ONE TARGET ON PURPOSE: both builds pass through the same intermediates, so letting each run-target build its own would let two terminals interleave and give both guests the SAME identity - the exact condition per-machine keys exist to prevent. A run target with no image tells you to run this rather than building one
make run-image-2vm-ext2-a    # the two-VM cluster on EXT2 (own port 12341, so it coexists with the FAT32 pair) - the ONLY rig that can test cluster PERMISSIONS, since FAT32 records no mode and every remote request looks permitted there whatever identity it carries. image-ext2 stages /etc/cluster/id at mode 0600 (without it the export is fail-closed; 0600 because ext2 is the one image where fsd ENFORCES modes and a machine's private key is what its identity rests on). Drive both nodes with scripts/drive-2vm.py
make run-image-2vm-ext2-b    # machine B of the ext2 pair (connect, IP .11)
make run-image-2vm-b         # two-VM cluster: machine B - connects the shared link (connect=:12340, MAC :0b -> IP 10.0.2.11); in B's shell `mount -r 10.0.2.10:564 /mnt/a; ls /mnt/a` reads MACHINE A's disk over 9P/TCP (no SLIRP). netd derives its IP from the MAC's last octet (default :56 -> .15, so SLIRP runs are unchanged)
make run-usb-kbd             # same as `run`, plus an xHCI controller + USB keyboard + HMP monitor socket for sendkey keystroke injection (see "USB HID keyboard driver" above)
make run-usb-multi           # same as `run-usb-kbd`, plus a usb-tablet and a usb-storage stick on the same controller - the three-device rig for xhci.rs's multi-device scan (see "xHCI multi-device support" above)
make image                  # build build/esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make run-image               # boot build/esp.img (genuine FAT32) instead of run's vvfat - needed for anything that reads the filesystem at runtime (the fsd server and every disk command)
make run-image-gpt           # build build/espgpt.img (build/esp.img's FAT32 wrapped in a bootable GPT disk via scripts/mkgpt.py) and boot it - exercises fsd's GPT partition discovery (the disk has no real MBR table)
make run-image-exfat         # build build/espexfat.img (two-partition MBR: exFAT partition 1 + FAT32 ESP partition 2, via newfs_exfat + scripts/mkexfat.py) and boot it - fsd mounts the exFAT partition (FAT32 probe fails, exFAT probe succeeds), UEFI boots the FAT32 ESP; exercises fsd/src/exfat.rs (the exFAT read-write arm)
make run-image-ext2          # build build/espext2.img (two-partition MBR: ext2 partition 1 + FAT32 ESP partition 2, via e2fsprogs' mke2fs + scripts/mkext2.py) and boot it - fsd mounts the ext2 partition (FAT32 + exFAT probes fail, ext2 succeeds), UEFI boots the FAT32 ESP; exercises fsd/src/ext2.rs (the ext2 read-write arm). Needs `brew install e2fsprogs`
make parallels-hdd          # wrap build/esp.img into build/esp.hdd, a Parallels-native virtual hard disk
make test-parallels          # scripted real-hardware round trip via prlctl - see below
make test                   # host unit tests + clippy --all-targets for the pure crates (accounts, regex, ed25519, clusterkeys, ninep-abi) + the cross-language wire-constant check
make check-relocs           # the PIE contract: no R_AARCH64_ABS64 in any userland binary
make clean
```

`make run`/`make run-image` require QEMU (`brew install qemu`, which also
provides the aarch64 OVMF firmware they point at). `make image` requires
macOS's `hdiutil`. `make parallels-hdd` additionally requires Parallels
Desktop installed (uses its bundled `prl_disk_tool`). `make shell-bin`
(and therefore `make esp`/`make run`) needs `rustup component add
llvm-tools` for `llvm-objcopy` - see the Makefile's `OBJCOPY` comment for
why it isn't just on `PATH`.

The kernel and the userland programs have no unit test suite — they are
pre-alpha code that mostly proves it boots, and most of it can only run on
the target. The **pure crates are the exception and now have one**:
`make test` runs the host unit tests for every crate with no I/O, no
syscalls and no target dependency (`accounts`, `regex`, `ed25519`,
`clusterkeys`, `ninep-abi` — 129 tests as of 2026-09-01, and the number is
checked by running it, not by incrementing). It exists because such a crate can otherwise have
**no build coverage at all**: it is a workspace member but not a
default-member, so until something depends on it, `cargo build`, `make
build` and `make esp` all stay green while it is broken. Run it before
pushing anything that touches those crates. There is also, as of 2026-08-16, a scripted real-hardware
*smoke* test: `make test-parallels` (`scripts/test-parallels.sh`) rebuilds
`build/esp.hdd`, boots the registered Parallels VM headlessly via `prlctl`
(Parallels Desktop's own CLI, `man prlctl` - discovered this session, not
previously known to this project), types a `;`-separated list of shell
commands through `prlctl send-key-event` (real decimal PS/2 Set-1
scancodes - `prlctl` rejects hex), and saves a `prlctl capture`
screenshot after each one, e.g. `make test-parallels CMDS="help;ls;uptime"`.
No human needs to watch the VM live or type on a physical keyboard.
Confirmed working end to end: `help`/`echo hi`/`uptime` all produced
correct output in the captured screenshots, including the driver's own
`xhci::report` debug lines showing genuine HID reports arriving through
the same interrupt-endpoint code path the USB keyboard postmortem is
about. **Update: that `xhci::report` line is gone now** - it turned out
to be a real usability bug, not just harmless noise: unconditionally
printing every raw HID report (press *and* release) through the console
meant that on Parallels, where the framebuffer console is the *only*
console, ordinary interactive typing flooded the screen with report
dumps interleaved with the shell's actual output - found by the user
directly, using the real `build/esp.hdd` normally with a physical keyboard,
not via `make test-parallels`. Removed outright (`xhci.rs::poll_key`)
rather than gated behind a flag - it had already served its purpose
confirming the driver end to end, and wasn't needed for normal
operation. A future `test-parallels` screenshot won't show it anymore;
that's expected, not a regression. One other real caveat about this
testing method, unrelated to the above: `send-key-event` drives Parallels' own synthetic
keyboard device, not the specific physical USB keyboard from that
postmortem - a legitimate stand-in for scripted regression checks, but
not a substitute for real-physical-hardware confirmation of anything
USB-passthrough-specific. See `docs/ROADMAP.md`'s "Testing infrastructure"
section for more.

## Toolchain

Pinned via `rust-toolchain.toml` (stable channel, targets
`aarch64-unknown-uefi` and `aarch64-unknown-none` — both install
automatically on first build). Both targets ship prebuilt `core`/`alloc`
on stable, so no nightly toolchain or `-Z build-std` is needed.
`.cargo/config.toml` defaults the build target to `aarch64-unknown-uefi`
(for `kernel`, the workspace's only default member - see `Cargo.toml`'s
`default-members` comment for why `shell` is deliberately excluded from
that default) and separately configures `[target.aarch64-unknown-none]`
(for `shell`, always built explicitly with `--target aarch64-unknown-none`
- see the Makefile). Plain `cargo build`/`cargo clippy` at the repo root
already target `kernel` correctly; building `shell` needs the explicit
`-p shell --target aarch64-unknown-none` (or just `make shell-bin`).

## Structure

A skeleton — one line per file, enough to find the right one and to know what
else a change will touch. The full annotation for every entry is in
`docs/source-map.md` (source) and `docs/README.md` (docs). Each file also
carries its own `//!` module doc — but on the most-changed files those have
themselves fallen behind (see `docs/source-map.md`'s closing caveat), so the
code is the authority and every annotation is a claim to check.

```
docs/                every document is annotated in full in `docs/README.md` - read that index
                     rather than guessing from filenames, since several are named for a subsystem
                     but organized around a lesson. The load-bearing ones:
  manual.md          the one-stop user manual: prerequisites, building, running, the shell tour, the CLUSTER section, the syscall ABI
  architecture.md    reference: boot flow, privilege model, memory layout, exceptions, syscall ABI, console
  processes.md       reference: userland loading, the ELF/PIE binary format, the relocation-class traps - READ THIS BEFORE WRITING ANY USERLAND PROGRAM
  shell-commands.md  reference: the default shell's builtin commands
  CHANGELOG.md       the milestone record, phase 0 to the present, newest first - what was built, and why it works the way it does
  ROADMAP.md         the forward-looking plan (finished arcs live in roadmap-completed.md; the cluster direction in roadmap-cluster.md)
  journal.md         chronological dev-log, a lighter companion to CHANGELOG.md
  testing-qemu.md    every `make run-*` target, the FAT32/exFAT/ext2/GPT test images, the 9P host peers, the two-node cluster rig
  testing-parallels.md, testing-pi4.md   the real-hardware guides. Both share one caveat: NO NETWORKING, so the whole cluster is QEMU-only
  gap-analysis.md    per-subsystem have/partial/don't inventory vs mainstream Unixes, capped by a ranked list of the biggest gaps
  archive/           contemporaneous build logs, kept for archaeology only - not the reference
  *-postmortem.md    the twenty-eight design/bug/process retrospectives (see the section above, and docs/README.md)
  research-*.md      synthesis notes on MINIX/Plan 9/Helix/Redox, the GUI stack, and where the design should go next

kernel/              every file annotated in full in `docs/source-map.md`; each also carries its own `//!`
  src/main.rs        #[entry]: UEFI init, console/MADT/PSCI discovery, loader, ExitBootServices, then exceptions/mmu/xhci/storage/net/gic/timer/tasks
  src/uart.rs        PL011 console driver (post-ExitBootServices only)
  src/uart16550.rs   16550 console driver - PCI-discovered consoles, genuinely different hardware
  src/devicetree.rs  console discovery via the UEFI devicetree (dead end on QEMU and Parallels)
  src/acpi.rs        console discovery via RSDP -> XSDT -> SPCR (works on QEMU, dead end on Parallels) + the shared find_table walk
  src/madt.rs        GIC version/address discovery via the ACPI MADT - where gic.rs's addresses actually come from
  src/power.rs       POWER syscall backend: PSCI SYSTEM_OFF via the FADT-discovered conduit, else halt
  src/pci.rs         PCI enumeration: serial-console discovery, discover_xhci, log_all_devices
  src/console.rs     global console handle (Pl011|Uart16550|Virtio|Framebuffer), shared with the exception handler
  src/framebuffer.rs GOP discovery: resolution/stride/format + framebuffer base - the console that works on Parallels
  src/font.rs        embedded 8x8 bitmap font, printable ASCII only (cond keeps its own copy)
  src/fbconsole.rs   framebuffer text console - the kernel's EMERGENCY/boot console only; steady state is cond
  src/fbdev.rs       dumb framebuffer primitives for cond (FB_BLIT/FB_SCROLL/FB_CLEAR), gated to CON_TASK
  src/exceptions.rs  VBAR_EL1 vector table + fault reporting, and the three resumable paths (IRQ, SVC, EL0 fault)
  src/mmu.rs         per-task translation tables. READ ITS MODULE DOC FIRST: the L0 start level is a fixed bug, not a style choice
  src/gic.rs         version-dispatching facade over gicv2/gicv3, selected by madt.rs
  src/gicv2.rs       GICv2 backend: distributor + memory-mapped CPU interface
  src/gicv3.rs       GICv3 backend: redistributor + ICC_* sysreg CPU interface - confirmed on QEMU and Parallels
  src/timer.rs       ARM generic timer (EL1 physical, PPI 14 / INTID 30), TICK_INTERVAL_MS
  src/loader.rs      INIT.CFG + the boot programs off the ESP; ELF64 parsing + R_AARCH64_RELATIVE processing
  src/supervisor.rs  server supervision: restart a crashed or wedged server from its boot image, per-boot cap
  src/syscall.rs     the svc dispatch table - the authority on which syscalls exist (numbers/sentinels in syscall-abi)
  src/tasks.rs       task slots, round-robin scheduler, mailboxes, grants, capability send-mask, per-task identity.
                     THE AUTHORITY on slot numbers and counts - restating that map elsewhere has drifted before,
                     see docs/asking-the-right-question-postmortem.md
  src/virtio_mmio.rs virtio-mmio transport: 32-slot discovery, modern register layout
  src/block.rs       BlockDevice enum (Virtio | UsbMsd) - what fsd's disk layer sits on
  src/usb_msd.rs     USB mass storage: Bulk-Only Transport + SCSI over xhci's bulk endpoints, with BOT error recovery
  src/virtio_blk.rs  virtio-blk: feature negotiation, one virtqueue, polling sector read/write
  src/virtio_console.rs  transmit-only virtio-console - works on QEMU, NOT what Parallels' serial port is
  src/virtio_rng.rs  virtio-rng, backing the RANDOM syscall; absent on Parallels/Pi and that is a supported case
  src/virtio_net.rs  virtio-net: rx/tx queues, the 12-byte header, IRQ-driven receive - the DMA-owning half of the net stack
  src/xhci.rs        from-scratch xHCI: rings, multi-device port scan, HID interrupt endpoint, storage endpoint reset

programs/            ALL userland programs, grouped by role. Annotated in full in `docs/source-map.md`
  linker.ld          the shared PIE linker script EVERY program uses. The relocation contract lives here:
                     R_AARCH64_RELATIVE is fine, R_AARCH64_ABS64 is unloadable. See docs/processes.md BEFORE
                     writing a userland program, and `make check-relocs` to verify one
  shell/             the default shell: line editor, builtins, pipelines, redirection, globs, completion, login
                     (src/login.rs). Deliberately MINIMAL - a command stays builtin only if it mutates shell
                     state, needs job control, or must run with no disk mounted; everything else is in /bin
  servers/fsd/       THE FILESYSTEM/STORAGE SERVER, protected slot 2, the only task BLOCK_* accepts. Owns the
                     NP_* verb dispatch, permission enforcement, fids, and erase/partition/format
    src/vfs.rs       the Filesystem enum (Fat32|ExFat|Ext2|Proc) + mount/probe - fsd's internal multiplexer
    src/fat32.rs     FAT32 read/write incl. LFN read+write, offset writes, mkfs
    src/exfat.rs     exFAT read/write + mkfs
    src/ext2.rs      ext2 read/write + mkfs - the arm that proved the abstraction (a genuinely different inode model)
    src/partition.rs GPT (CRC-validated, backup fallback) + MBR partition discovery
    src/proc.rs      the synthetic /proc - the first NON-disk arm, which is what makes the enum a real VFS
    src/disk.rs      BLOCK_* syscall shim
  servers/cond/      THE CONSOLE SERVER, protected slot 3. Two backends: byte-stream via CON_WRITE, or render
                     glyphs itself via FB_*. Owns the font, cursor, wrap, scroll and ANSI parsing
  servers/netd/      THE NETWORK SERVER, protected slot 4. The whole protocol stack in userland: ARP/IPv4/ICMP/
                     UDP/DNS/TCP, an HTTP static-file server, the 9P export gateway + cluster auth, remote
                     execution (cpu), and the /net/tcp dial-out/dial-in connection files
  servers/accountd/  THE ACCOUNT SERVER, protected slot 5. Exists because a setuid binary CANNOT work here:
                     the shell reads the binary, so "this is setuid" would be a claim by a task the capability
                     model distrusts. One op (ACCTOP_PASSWD); it authorizes on SENDER_ID, never GET_ID(sender)
  fileutils/         ls cat mkdir rmdir touch rm cp mv writeat chmod chown tree write more (cp/mv REFUSE an
                     existing destination without -f - deliberate, on a system with no undo)
  textutils/         the pipeline filters: upper wc grep head tail nl rev uniq sort (sort is the one that
                     cannot stream, so it is the one that uses the heap)
  netutils/          ping resolve fetch dial serve - reach netd via the TO_NET cap the shell delegates at spawn
  shellutils/        echo uptime clear pwd readkey send recv selftest man printenv id args edtest
  admin/             passwd useradd groupadd usermod clusterkey - root-only account + cluster-identity tools
  demos/             hello (how a program ends itself), pong (the IPC echo-server shape)

ulib/                shared userland support library: syscall wrappers, argv/env, cwd + path resolution, the fs
                     client layer, the netd client, output routing, and the one #[panic_handler]
syscall-abi/         syscall numbers, sentinels, error values, the FSOP_* constants - kernel and userland share it
ninep-abi/           the NP_* cluster verb set, `resolve_ns`, and THE NORMATIVE WIRE SPEC for cluster auth
                     (checked across all three implementations by scripts/check-wire-constants.py on every `make test`)
ed25519/             hand-rolled Ed25519: SHA-512, field, curve, scalar, sign/verify. No heap. Deterministic
                     signing is why it was chosen - hardware entropy is absent on Parallels and the Pi
clusterkeys/         the /etc/cluster/{id,id.pub,authorized} file format. No trust-on-first-use
accounts/            /etc/{passwd,shadow,group} parsing/formatting, SHA-256 hashing, salts, lookups, rewrites
regex/               a small POSIX-ERE engine behind grep. An explicit backtracking stack, not host recursion
libc/                the C-portability arc: crt0 + syscall stubs + a narrow waist (write/read/open/sbrk/_exit)
                     that BOTH a hand-rolled libc and a real PICOLIBC link against unchanged
                     (third_party/picolibc-prebuilt; regenerate with scripts/build-picolibc.sh)
scripts/             test-parallels.sh (real-hardware smoke test), drive-qemu.py + drive-2vm.py (drive the guest
                     shell / a two-node cluster unattended - the fussy paced typing is load-bearing, see
                     docs/testing-qemu.md), mk{gpt,exfat,ext2,clusterkeys,passwd,group}.py (build the test disk
                     images and the staged /etc files), np9p_{client,server}.py (the host-side 9P peers - the
                     FOREIGN OBSERVER for both directions of the export)
```

Sixty-one-crate workspace. `kernel` and the shared libs (`ulib`,
`syscall-abi`, `ninep-abi`, `accounts`, `regex`, `ed25519`, `clusterkeys`) sit at the repo root; **every userland
program lives under `programs/`, grouped by role** (`programs/shell`,
`programs/servers/{fsd,cond,netd,accountd}`, `programs/demos/{hello,pong}`,
`programs/fileutils/*`, `programs/textutils/*`, `programs/netutils/*`,
`programs/shellutils/*`, `programs/admin/*`) - a
reorganization done once the flat top-level list grew unwieldy, purely a
directory move (crate *package* names are unchanged, so the Makefile - which
builds by `-p <name>` and reads `target/.../<name>` - needed no edits;
only each moved crate's `path = "../..."` deps deepened, the workspace
`members` list, and `.cargo/config.toml`'s `-Tprograms/linker.ld` changed).
Every userland crate is deliberately excluded from the workspace's
`default-members` (see `Cargo.toml`) since they need a different `--target`
than `kernel`; `syscall-abi` needs no such exclusion (a plain lib, no
`[[bin]]` to conflict with a target) and gets built automatically as
`kernel`'s path dependency. New programs slot into the matching
`programs/<category>/` dir (or a new category), depending on `syscall-abi`
(and usually `ulib`) via the `../../../` path the siblings use.

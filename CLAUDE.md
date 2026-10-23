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
- **The postmortems under `docs/postmortems/`** (thirty-one of them) — the design, bug
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
- **`docs/work-journal/`** — a chronological dev-log, one file per day (narrative "what and why
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
Returning to firmware is a dead end for kernel code. **Since 2026-10-02 its
first act is to leave the firmware's stack for the kernel's own** (256 KB
in the image's `.bss`, `KERNEL_STACK`), then `kernel_main` runs there and
never returns: on the Raspberry Pi the firmware's stack is 16 KB with its
page tables directly below it, and the kernel overflowed it into them,
which was every firmware fault the board showed (`testing-pi4.md`
section 6). The first log line says which build it is (the commit, `+dirty`,
the profile, from `kernel/build.rs`, which reruns every build) and which
stack it is on.

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

**Since 2026-10-10 every fault line names its core (`EXCEPTION core=N
...`, from `TPIDR_EL1`), and a secondary core's fault halts that core
alone (`smp.rs`).** `kernel/src/exceptions.rs` installs a minimal AArch64 vector table (VBAR_EL1)
right after `exit_boot_services`, before anything else gets a chance to
fault (on an EL2 handoff the write is made then but is live only from the
drop to EL1; see the 2026-10-02 update at the end of this section). On any synchronous exception, IRQ, FIQ, or SError, it reports
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
**Update 2026-10-01: the Raspberry Pi's firmware hands off at EL2**, and
every `_EL1` write after the exit (this table, the MMU switch, the timer)
then goes to a register the running level does not use; found by the early
fault reporter (`earlyfault.rs`), reproduced on QEMU with `-machine
virt,virtualization=on`. The plan is `docs/roadmap/roadmap-el1-drop.md`.
**Built 2026-10-02 (`el2.rs`): the kernel logs the level it was handed off
at, and at EL2 `mmu::switch_to_identity_map` drops to EL1 instead of
switching**, with EL1's tables and vectors prepared from EL2 and an `eret`
into a running MMU; this table's `VBAR_EL1` write, made from EL2, takes
effect at that `eret`. Proven on QEMU (`make test-el1-drop`); the Pi 4
boot is pending.

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

**Update 2026-10-02: the install is two halves now** (`build_identity_map`,
memory only, then `switch_to_identity_map`), and on the Raspberry Pi's EL2
handoff the switch is not the TTBR0 swap described below but the drop to
EL1 in `el2.rs`: EL1's regime and `SCTLR_EL1` are written from EL2 and an
`eret` lands on the tables; a handoff at any other level halts with a line.
The paragraphs below describe the EL1 path, which is unchanged.

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
built, not this milestone. **Update (2026-09-20): slot 5 no longer takes
the resumable path.** The kernel never runs at EL1 with IRQs unmasked
(masked at `exit_boot_services`, and the first `eret` into task 0 is what
unmasks, for EL0 only), so an IRQ at EL1h is a broken invariant and slot 5
reports and halts like the other diverging vectors; slot 9 (the tick
arriving from EL0) is the one resumable IRQ path. See `synccell.rs` for
why the invariant is load-bearing. **Update (2026-10-07): FP/SIMD is
saved now**, `q0`-`q31`, `FPCR` and `FPSR` on every resumable path and in
`Context` (800 bytes). The premise above had stopped being true long
before: the kernel's memcpy runs through `q0` and every userland program
keeps values in vector registers, so a register live across a syscall came
back changed. Found by `cond`'s reverse video drawing wrong rows; checked
by `make test-fpsimd`.

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
(Since 2026-09-20 slot 9 is the only resumable IRQ path; slot 5 diverges,
see the update above.)

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
round-robin; FP/SIMD state wasn't part of `Context` (inherited
limitation from `exceptions.rs`; it is since 2026-10-07, see the update in
the timer section above); and both tasks still share the one 8KB region's W^X
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
make run-usb-hub             # the Raspberry Pi's USB layout on QEMU: keyboard + stick BEHIND a usb-hub, tablet on a root port (QEMU's hub is full-speed, so the Pi's transaction-translator case is not modelled)
make test-usb-hub            # the hub layout driven and graded (scripts/test-usb-hub.py): three boots, about three minutes - keyboard + stick behind the hub, then --usb-boot (booted FROM a stick behind the hub, mounted through it), then --stall (the same, from `make image-stall`'s copy carrying the \MSDSTALL flag, so QEMU's stick stalls on every seventh CBW and on a short CSW read for another, about 30 of each, and each must be recovered: the CBW by its first retry, the CSW in place); `--direct` is the control with the same devices on root ports; a boot where QEMU's own firmware stalls in its USB boot (`UsbBootExecCmd`) and never loads the kernel is INCONCLUSIVE, not failed checks, still not a pass; every run's verdict goes to scratch/usb-hub-runs.log so the stall reads as a rate, and a stalled transcript is kept in scratch/usb-hub-inconclusive/ - run it whenever xhci.rs's port scan, device setup or storage recovery changes
make image                  # build build/esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make run-image               # boot build/esp.img (genuine FAT32) instead of run's vvfat - needed for anything that reads the filesystem at runtime (the fsd server and every disk command)
make run-image-gpt           # build build/espgpt.img (build/esp.img's FAT32 wrapped in a bootable GPT disk via scripts/mkgpt.py) and boot it - exercises fsd's GPT partition discovery (the disk has no real MBR table)
make run-image-exfat         # build build/espexfat.img (two-partition MBR: exFAT partition 1 + FAT32 ESP partition 2, via newfs_exfat + scripts/mkexfat.py) and boot it - fsd mounts the exFAT partition (FAT32 probe fails, exFAT probe succeeds), UEFI boots the FAT32 ESP; exercises fsd/src/exfat.rs (the exFAT read-write arm)
make disk                   # build/disk.img (scripts/mkdisk.py, one-disk step 4): ONE disk, the FAT32 ESP as partition 1 and the ext2 root as partition 2; fsd mounts the root, told by the kernel which partition it booted from (BLOCK_BOOT_START, 77)
make run-disk               # boot build/disk.img on virtio-blk
make run-image-ext2          # build build/espext2.img (two-partition MBR: ext2 partition 1 + FAT32 ESP partition 2, via e2fsprogs' mke2fs + scripts/mkext2.py) and boot it - fsd mounts the ext2 partition (FAT32 + exFAT probes fail, ext2 succeeds), UEFI boots the FAT32 ESP; exercises fsd/src/ext2.rs (the ext2 read-write arm). Needs `brew install e2fsprogs`
make parallels-hdd          # wrap build/esp.img into build/esp.hdd, a Parallels-native virtual hard disk
make sdcard SDCARD=/Volumes/OUROBOROS  # stage the Pi 4 / Pi 400 boot card on an already-formatted FAT volume: pinned pftf firmware (installed once, since its settings live in RPI_EFI.fd) + build/esp on top. Never formats. KEEP_ETC=1 keeps the card's /etc (default re-stages it, resetting Pi-made accounts), FIRMWARE=1 reinstalls the firmware, EJECT=1 ejects. See docs/testing/testing-pi4.md section 4
make stick STICK=/Volumes/STICK       # the USB stick, the Pi's only disk once the kernel runs (no SD driver after the exit): `make sdcard` in stick mode, no firmware and no EFI tree, FAT32 or exFAT on partition 1 of an MBR disk, its /etc KEPT by default (KEEP_ETC=0 re-stages), never formats. Stage card and stick from the same build. testing-pi4.md section 4, "The USB stick"
make test-parallels          # scripted real-hardware round trip via prlctl - see below
make test-early-fault        # the early fault reporter on QEMU (scripts/test-early-fault.py), three boots: with \EARLYFAULT the kernel's own dump of a fault taken in the firmware's code (the register rows before the first frame, the firmware's tables walked to an invalid entry at the ESR's level, the DXE core named, a kernel frame in the backtrace); with \EARLYFAULT and \WALKFAULT the report faulting inside its own image walk, the rows and then the nested-fault line with FAR and the image under read; and with no flag the shell, answering `help`; about ninety seconds - run it whenever earlyfault.rs or console discovery changes
make run-el2                 # `run` with the firmware handing off at EL2 (-machine virt,virtualization=on), as the Raspberry Pi's does: the dev loop for the EL1 drop (kernel/src/el2.rs); the log must say `running at EL2 after the exit` then `dropped from EL2 to EL1`
make test-el1-drop           # the EL1 drop's rig: test-early-fault.py --el2, the same three boots handed off at EL2; the control must show the drop and the shell answering `help` after a dwell, where before the drop the first eret into task 0, made at EL2, restored the firmware's stale ELR_EL2 and landed at address 0; about a minute - run it whenever el2.rs, mmu.rs's switch or exceptions::install changes
make test-async-rmount       # rebuilds the image, then three driven QEMU boots against host-run 9P peers (scripts/test-async-rmount.sh): the parked remote mount is served, a parked reply never reaches a recycled slot, and a live peer is served while a silent one is parked - run it whenever netd's client paths or the kernel's reply path change (MSG_REPLY, SENDER_CALL, tasks::reply_target: its recycled-slot check rides the call handle since 2026-10-09)
make test-call-interrupt     # an interrupted call's late answer goes nowhere (scripts/test-call-interrupt.py): two host 9P peers holding every reply 4.5 s; Ctrl+C and Ctrl+\ cut the boot shell's `cd` into a remote directory short, and the next `cd` (to a missing directory on the other peer) fails on its own answer, the late "exists" refused by MSG_REPLY; fails with the call number not compared and with a call's wait not interruptible; one boot, about a minute - run it whenever the message waits, the boot shell's interrupt, MSG_CALL, MSG_REPLY or SENDER_CALL changes
make test-held-keys          # rebuilds the image, then five driven QEMU boots of the held user keys (scripts/test-held-keys.py): login holds, logout and a killed shell drop, an ordinary user is refused; minutes, so not in `make test` - run it whenever login, the shell's session loop or netd's held-key table changes
make test-heap               # the user heap on a booted image (scripts/test-heap.py): /bin/CMEM twice in one boot, each checking a heap of at least 1 MiB, malloc holding nearly all of it, and every byte 0 before the first malloc (the second run starts in the slot the first gave back); about a minute - run it whenever HEAP_PAGES, populate_region or the runtime region allocator changes
make test-malloc            # picolibc's malloc on a booted image (scripts/test-malloc.py, /bin/CMALLOC): 20,000 random malloc, calloc, memalign, realloc and free at each of 500 to 4000 slots, untimed, every block written whole with a pattern of its slot and offset and read back whole before it is freed, after a realloc and at the end; back to the prompt (a library missing -fno-builtin-free or -fno-builtin-malloc hangs in a realloc), the pattern intact, calloc zero and memalign aligned (those two cannot fail under nano-malloc, they hold another allocator to the contract); then the cost an operation at each count, reported not graded (nano-malloc 362 to 1740 ns, DevTools's note, deferred), with the timed runs' refusals graded; controls: the library shipped until 2026-10-15, one with only -fno-builtin-free, a calloc leaving a byte set, a block over another live one, a 48-byte overrun, a timed malloc refusing 1 in 100; one boot, about a minute - run it whenever picolibc is rebuilt or its malloc options or cross file change
make test-fpsimd            # a task's FP/SIMD registers across the kernel (scripts/test-fpsimd.py, /bin/FPPROBE): all 32 vector registers, FPCR and FPSR loaded in one asm block, then a YIELD and a spin the tick preempts (at least two ticks), alone and as `fpprobe | fpprobe -` (two probes side by side, different rounding modes); fails on a kernel without the save and on one missing only the FPCR or only the FPSR restore; one boot, about a minute - run it whenever exceptions.rs's trampolines, Context or tasks.rs's switch paths change
make test-spawn-owner       # the owner of the kernel's spawn staging (scripts/test-spawn-owner.py, /bin/STAGEPROBE): a chunk staged with nothing below it (`stageprobe sparse`, over the shell's copy of the probe) must be SPAWN_ERROR and start nothing, since 2026-10-16 (fails with SPAWN_STAGE taking a chunk anywhere); a program holding the staging mid-spawn (`exec /bin/stageprobe hold`) makes the shell's next command refuse with the code's message and print nothing, and a second stager's SPAWN_STAGE answer SPAWN_ERR_STAGING_BUSY; the holder's exit, its `kill`, and its abandon (a SPAWN of total 0, the holder still alive) each release it; the four refusal checks fail with claim_staging answering Ok to everyone, and with SPAWN not releasing the boot shell keeps the staging and no holder can claim; one boot, about two minutes, `SMP=4` for four cores - run it whenever claim_staging, release_staging_of, end_task or the shell's and netd's staging flows change
make test-spawn-size        # the size of a program file spawn accepts (scripts/test-spawn-size.py): on a copy of the image, /bin/ARGS zero-padded to 200 KiB (past the 128 KiB bound of before 2026-10-16), to exactly SPAWN_STAGING_SIZE (1 MiB, read from syscall-abi), and one byte more: the first two run, the third is refused with the shell's message naming the bound, and a spawn after it runs; fails with the constant back at 131072 (the kernel's assert that it covers the image bound taken out, or it does not build); each run's time from its Enter reported (a 1 MiB file 1.5 s, the 13 KB /bin/args 0.8 s); it drives the shell's loop only, not netd's cpu; one boot, about a minute - run it whenever SPAWN_STAGING_SIZE, SPAWN_STAGE or the shell's spawn_path changes
make test-proc-cpus         # /proc/cpus, the cores that take tasks as a file (scripts/test-proc-cpus.py): two boots, -smp 1 and -smp 4; after `mount -p /proc`, `ls /proc` lists cpus, `cat /proc/cpus` prints 1 then 4 on a line of its own, and `cat /proc/cpus/x` is not a directory; about a minute and a half - run it whenever fsd's proc.rs or CORE_INFO changes
make bootstick BOOTSTICK=/dev/diskN  # one-disk step 5a (scripts/bootstick.py): disk.img's layout written to a WHOLE disk, the old disk's /etc, /Users, boot counter and signature carried (CARRY=0 resets /etc and /Users, EJECT=1); PI=1 makes the Pi 4's stick, the pinned pftf firmware on partition 1 (scripts/pftf.env), carried whole on a re-stage, FIRMWARE=1 reinstalling it with config.txt kept, PI=0 dropping it; refuses a disk that is not empty or ours, internal media, a Pi card. BOOTSTICK may be a disk image file (mkfile -n 128m build/stick.img), attached and written the same way; `make run-bootstick BOOTSTICK=build/stick.img` boots one
make test-bootstick         # bootstick against image files (scripts/test-bootstick.py): the refusals (a PI=true among them) leave their disks untouched; a `make stick` stick, a written esp.img and a dd copy of disk.img taken as ours; the Pi's stick: every pinned firmware file on a fresh one and a manifest naming them, a file only its manifest names carried by it, the release's names with no manifest, its RPI_EFI.fd and config.txt carried and nothing else of its root (a \NOXHCI flag not), FIRMWARE=1, PI=0, an ESP that will not mount refused unless PI=0; a fresh file holds the ESP and root as built; then four boots: the Pi stick, a user added, a re-stage after which she logs in and the counter goes on, and CARRY=0 refusing her (the control); about three minutes - run it whenever bootstick.py, pftf.sh, mbrdisk.py, disksig.py or the root's recipe changes
make test-boot-partition    # the boot partition the kernel names (scripts/test-boot-partition.py, docs/roadmap/roadmap-one-disk.md step 1): seventeen boots, the FAT32, GPT, ext2 and exFAT images on virtio, esp.img as a USB stick, esp.img on USB and on virtio with usbstick.img beside it on the other bus, where the boot disk must be the one installed and `ls /` after login must show the ESP's tree, and esp.img on NVMe (which the kernel cannot drive) with neither other disk the boot disk, where virtio must go in as the fallback with its reason; each boot alone on its bus must install its own disk directly (step 3, fails with is_boot_disk answering unknown, with the GPT match refusing, and with the fallback dropped); and disk.img on virtio and as a USB stick, where fsd must mount the ext2 root, not the ESP, and every boot checks the filesystem fsd mounted (step 4, fails with fsd ignoring BLOCK_BOOT_START and with the stick installed with no start); then copies of disk.img and espgpt.img: `format ext2` must reformat the root and leave the ESP (disk-format), a zeroed root superblock must make fsd say it fell back to the ESP read-only and refuse root's `touch` there (disk-badroot, decision D5), and a GPT disk must be refused by `format` (gpt-format); and disk-data.img, the ESP, an exFAT data partition and the ext2 root: the root mounts by its type (0x83), not the data partition before it, `format` takes the root and leaves the data partition, and with the root damaged the ESP comes up read-only, never the data partition, and `format` takes the damaged root, also with the boot partition unknown to fsd (disk-data, disk-data-format, disk-data-badroot, disk-data-unknown); each `boot partition:` line, read by bootdisk.rs from the HardDrive node of the image's own device path before the exit, must equal the partition the host reads in the image's table (number, start, size, MBR signature or GPT GUID); an MBR image without a disk signature, or two sharing one (scripts/disksig.py, step 2), stops the run before any boot; fails with the kernel naming partition 1 (ext2 and exFAT, whose ESP is partition 2) and with the host reader taking the first partition; a boot the firmware never starts is INCONCLUSIVE (drive-qemu.py's classifier, every boot since 2026-10-22); about seven minutes - run it whenever bootdisk.rs, main.rs's choice of block device, fsd's mount or format, or an image's partition layout changes
make test-cond-vt           # cond's escape sequences on the framebuffer (scripts/test-cond-vt.py, /bin/VTPROBE): QEMU with -device ramfb (the shell's output then reaches ONLY the framebuffer), the screen taken by QMP screendump and decoded per 8x8 cell with cond's own font, VTPROBE's screen (cursor addressing, K and J, reverse video, the deferred wrap) compared with a model cell by cell, then `vtprobe hold` killed by Ctrl+C in reverse video and the shell's next prompt read as normal video; since 2026-10-11 the cursor too: the cell after the prompt inverted and the row's only inverted cell, gone while `vtprobe hide` holds `CSI ? 25 l`, back on the prompt after Ctrl+C kills it (each failing with FB_CURSOR a no-op, cond ignoring the hide, or the shell's reset not showing), and after `vtprobe leave` exits with the cursor hidden and video reversed, which the prompt keeps, `clear` must bring both back (fails with clear's prefix removed); one boot, under a minute - run it whenever cond's framebuffer backend, its font, the FB_* primitives or the shell's reset after a kill change
make test-nav-keys          # the navigation and function keys on QEMU's USB keyboard (scripts/test-nav-keys.py): the eleven keys, Tab and Escape pressed by monitor sendkey, /bin/READKEY must read exactly their VT100 sequences, a 9 and a 27, Esc then q must quit `more` and a letter typed after Esc must reach the shell, Tab must complete a filename in the shell; then the shell must take arrows typed mid-line (USB and serial) as nothing (a USB letter among them proves delivery), login a user name typed with ESC [ D in it, `more` one Down as one screen, and a password set at useradd with ESC [ D in it must log in typed plain (the run boots a copy of the image); one boot, about a minute - run it whenever xhci.rs's key mapping, the shell's line editor or login's reader changes
make test-cwinsz            # the console's size read by an ordinary program (scripts/test-cwinsz.py, /bin/CWINSZ): two boots; on the serial console ioctl(TIOCGWINSZ) answers 0 by 0 on fds 0-2 (size unknown), EBADF/ENOTTY otherwise, ENOTTY for fds 1-2 when piped; on -device ramfb (read back by QMP screendump with test-cond-vt.py's decoder) it answers the screen's grid and `more` fills the screen exactly; about a minute and a half - run it whenever CON_INFO, ioctl, ulib::screen_size or more's paging changes
make test-kbd-queue         # the kernel's keyboard queue (scripts/test-kbd-queue.py): /bin/READKEY spin runs without reading while keys are typed on QEMU's USB keyboard (monitor sendkey) and serial line, and every byte must come back, also while the keyboard owner waits in MSG_RECV (`readkey spin 150 keep | readkey spin 0 msg`); type-ahead left by a program that exits (spin keep) reaches the shell whole; Ctrl+C during the spin, and behind 72 queued letters, must still end it and flush the queue; a full queue drops a whole key (63 letters, Up, b); one boot, about two and a half minutes - run it whenever the keyboard path in syscall.rs, the tick's read in tasks.rs or Ctrl+C's handling changes
make test-kbd-mode          # the keyboard modes (scripts/test-kbd-mode.py, /bin/READKEY raw and mode): in raw mode (KBD_MODE, syscall 70) Ctrl+C is read as 3 on the serial line and QEMU's USB keyboard, and Ctrl+\ ends the program from either, while Ctrl on the ISO `<>` key (0x64) does not; `readkey mode` after them, in a slot they ran in, starts cooked; cooked, Ctrl+C and Ctrl+\ both end it, each kill line naming its key; the boot shell ignores both bytes, and Ctrl+\ interrupts its stuck `wait`, even behind a full queue, while a raw program's Ctrl+C left queued when it exits (`readkey spin 100 rawkeep`) does not interrupt the shell's wait on it (no zombie in `ps`, and `echo qk` typed behind it runs); a busy raw program (`readkey spin 100 raw`) reads a Ctrl+C typed meanwhile as 3; an Esc then Ctrl+\ at the wait leaves the shell's next line whole (`Ozq`); each check fails with its part of the kernel removed (`Ozq` with E1 and E2 together: E1 alone is host-checked); one boot, about a minute - run it whenever interrupt_key_check, KBD_MODE, xhci.rs's Ctrl mapping or end_task's per-task resets change
make test-kbd-cut           # cut keys in the kernel's keyboard queue (scripts/test-kbd-cut.py): an arrow read in part by `readkey one` leaves nothing of `[A` for the shell, a letter typed after a bare Escape is kept (from the serial line, and after the USB Escape key), nothing typed after a kill is eaten, `ctermios flush` (TCSAFLUSH) discards keys typed ahead and `ctermios drain` (TCSADRAIN) keeps them; one boot, about a minute and a half - run it whenever the keyboard queue in syscall.rs, a change of keyboard owner in tasks.rs or KBD_MODE's flush changes
make test-ctermios          # terminal settings in a C program (scripts/test-ctermios.py, /bin/CTERMIOS): Edit's own raw recipe through tcsetattr and read back, flags the console cannot apply accepted and reported off, the saved settings restored, ISIG following KBD_MODE set directly, EBADF/ENOTTY/EINVAL/EFAULT; raw mode reads Ctrl+C as 3 (serial and USB) and after the restore Ctrl+C ends the program; a program ended by Ctrl+\ while raw leaves the next one starting with ISIG set; one boot, about a minute - run it whenever libc's termios, sys/termios.h or KBD_MODE changes
make measure-syscost        # the cost of a syscall (scripts/measure-syscost.py, /bin/SYSCOST): three runs of 200,000 GET_TICKS calls and the median in ns a call; a measurement, not a rig, cited by exceptions.rs's FP/SIMD decision (eager stays: 24,934 ns on the tree vs 25,232 ns with the save removed, both TCG's exception cost) and by the kernel lock's cost (27,786 ns with it, step 4(a)); about a minute
make test-smp               # multi-core (docs/roadmap/roadmap-smp.md): six boots (scripts/test-smp.py): -smp 4 on the dev-loop machine, under virt,virtualization=on, under gic-version=3 and under both, -smp 1, and -smp 4 with the \SMPFAULT flag on a vvfat ESP copy; the boot log's `MADT: N cores` line and one `MADT: core i` line per core must match -smp with distinct MPIDRs and the boot core's affinity among them, every other core must say `smp: core N up at EL1 (...), idling` and then `smp: core N ticking` (its own timer into its own idle loop at EL0, step 4(b)) and the summary all started cores up, each must answer the boot core's kick SGI only once the kernel lock is free (step 4(c)), /bin/COREPROBE must run on a secondary alone and two at once as `coreprobe | coreprobe -`, and `coreprobe hold` must be ended by Ctrl+C on its own core (step 4(d)), the shell must answer `help` before and after a ten-second dwell, the EL2 boot must show the drop, and with the flag the first started core's fault line must name it (`EXCEPTION core=N`) while the shell still answers; about seven minutes; since 2026-10-11 also `exec /bin/coreprobe hold` in the background, `ps` showing it `running on core N` (a secondary; `runnable` on one core, where the shell has it), `cat /proc/N/core` the same, and `kill N` ending it - run it whenever smp.rs, el2.rs, madt.rs's GICC parse, the GIC backends' per-core init or SGIs, the kernel lock or the console lock changes
make test-listing-walk      # a listing page's walk stops at the page's end, seen by QEMU's virtio-blk trace (scripts/test-listing-walk.py), the one observer that can (a walker ignoring ControlFlow gives the same pages): /include on FAT32 and exFAT, and on ext2 /walk, 300 empty files (without inode reads /include is too short there to show a stop), on a root the rig rebuilds from the image's own tree with debugfs and mke2fs, once with the filetype feature and once without (the inode fallback), each checked by e2fsck -fn and each listing /kinds with its two directories marked; two boots each, `echo DIR/qqq*` (the shell lists DIR for the glob, which matches nothing) against `echo DIR/qqqq` (the same binary, no listing), and a glob that must match must print its names; the per-sector difference is one listing, whose p + 2 requests must not all do the same work: the counts must share no factor above 1; on ext2 a listing must also read fewer sectors than /walk has entries (no inode read an entry); fails on main before 2026-10-17 (every count a multiple of 4), with only exFAT's or ext2's walker ignoring Break (that image alone), with ext2 reading an inode an entry (3,698 sectors while an inode was read as two sectors, before 2026-10-20), and with the type byte trusted on a volume without filetype (/kinds all files); each boot starting with one listing, so fsd's caches are in the same state for every measured run; reports the sectors a listing reads (63 on FAT32, 64 on ext2 and 121 on exFAT on 2026-10-20); about six minutes, needs e2fsprogs - run it whenever a filesystem's list_dir, ReaddirFill or fsd's NP_READDIR changes
make test-spawn-reads       # spawning a program resolves its path once a read request, exactly (scripts/test-spawn-reads.py, scripts/disktrace.py): on a copy of each of FAT32, ext2 and exFAT the host puts /bin/ARGS in the root as /SMALL and zero-padded to 200 KiB as /BIG (hdiutil and diskutil, or debugfs on ext2), each boot warms with `/SMALL w` then spawns one of them twice, each must print `argv[1] = a`, and the root directory's first sector (found on the host from the boot sector or by debugfs), read once a resolution, must be read exactly as many more times a spawn of /BIG as /BIG makes more NP_READ requests (23); fails with fsd resolving once a 2 KiB piece (93) and with the shell reading 512 bytes a request, as on main before 2026-10-17 (374); then two exFAT boots copy a 640 KiB program with the guest's `cp` (fsd writes FAT-chained files) and a spawn of it must read exactly the FAT sectors one walk of its chain reads through exFAT's FAT cache, the chain read off the disk by the host (6 on the rig's image), fails with the read cursor ignored (1,436), with the cache bypassed (159) and with the cache not kept in step by FAT writes (the program does not run); about four minutes, needs e2fsprogs - run it whenever the shell's or netd's spawn read, fsd's NP_READ, a filesystem's read_each or read cursor, or GRANT's cap changes
make test-write-walk        # a write walks a file's cluster chain once, exactly (scripts/test-write-walk.py): on a copy of the exFAT image, with the ESP mounted at /mnt/f, `cp` of 640 KiB onto exFAT from the ESP and onto the ESP from exFAT, so the source's FAT reads land in the other filesystem's FAT; each destination's FAT reads, against a boot copying an empty file, must equal the design's count, simulated a 2 KiB request at a time through the FAT cache from the chain cp built and the free space before it, both read off the disk (19 on exFAT, 134 on FAT32, where a FAT write reads no copy but the first: 2,690 before 2026-10-21, when it read the second copy too), and the copy must hold the source's bytes; fails with either write cursor ignored (1,412 and 9,389, measured before the mirror of 2026-10-21, with the second copy's reads) and with exFAT's cursor saved a cluster off (the bytes); since 2026-10-18 also a copy within each volume (on exFAT /SRC made chained by a first copy in both boots, on FAT32 /mnt/f/HUGE itself), the source walked on from its read cursor before each write through the same cache (323 on exFAT, 3,333 on FAT32 on the tree of 2026-10-18; the counts move with the files staged), failing with the cursors dropped at every FAT write (867 and 5,048, FAT32's measured before the mirror); since 2026-10-21 too, where a FAT32 copy reads 134 and 774, the ESP's second FAT copy planted with the bad-cluster mark at every free entry, each FAT sector of the copy's chain required to be the first copy's in both and a sector it did not write still to differ, failing with each copy's own sector patched; about two and a half minutes - run it whenever a filesystem's write_at, cursors, FAT cache, allocator, write_fat_entry or ends_walk changes
make test-stale-cursor      # no chain cursor outlives its file (scripts/test-stale-cursor.py, /bin/CURSORPROBE): on a copy of the exFAT image with a third partition, an exFAT of 512-byte clusters made by newfs_exfat, the probe on the exFAT root (4 KiB clusters: the read sequence; the write sequences cannot be built there and must say skip), the ESP at /mnt/f and the small exFAT at /mnt/x (all four): a file removed, its first cluster given to a new file on another chain, then the new file read (`read`), overwritten (`write-in-place`) or appended to (`write-append`, and `write-one`, a one-cluster file on the removed file's first cluster, with no link written between the remove and the append) where the old cursor was, every file read back against a pattern of file and offset, and after the boot the kept files' chains read off the disk (witnesses W1 and W2 either side of the removed file) to confirm each stale case was built, which the probe cannot see (fails with FAT32's free path not lowering its hint, where the probe passes everything); every sequence builds its stale case by a removal, so a rule wrong only for a truncation or a relink is not seen (fsd makes neither); also a size-0 NoFatChain file P the host plants on the small exFAT, its cluster's FAT entry pointing at Q's, written past its cluster by `writeat`: Q's bytes must be unchanged and P's on a chain of its own (since 2026-10-19 an entry of size 0 holds no cluster, so the write takes a new one; ensure_chained ended the named cluster from 2026-10-18); fails with either filesystem's read or write drop removed from write_fat_entry, each on its own sequences (another file's bytes, B's own bytes where X was written, B short at 2048), with the drop decided on the value written rather than the entry replaced (`write-one` alone, B short at 512), and with the size-0 entry's named cluster kept where its set is read (Q's bytes); one boot, about a minute - run it whenever a filesystem's cursors, write_fat_entry, ends_walk, ensure_chained, allocator or free path changes
make test-grow-dir          # a directory grown past its first cluster lists and finds every entry (scripts/test-grow-dir.py, /bin/DIRPROBE): on a copy of the exFAT image, the probe's files made in an empty NoFatChain directory macOS made (as it made /etc, /Users and /SUB), in one the host made of size 0 with no cluster, in a NoFatChain one of size 0 still naming a cluster the host freed, in a new directory on the exFAT root (4 KiB clusters, so grown twice) and on the ESP at /mnt/f (the FAT32 control), each found, listed and touched again by the probe; and in one the host made as fsd before 2026-10-18 left a directory it grew (two clusters, DataLength one, 50 files of which a reader sees 42), a hidden file not found and an append to one, a create, a mkdir and a new file's write refused with an I/O error (nothing leaked) while a touch of a file inside the size goes ahead and rmdir answers not empty, nothing of it changed (fsd refuses a create or rmdir in a directory whose chain is not its size, as Linux does); a directory and two files of size 0 naming /man's directory cluster removed, overwritten and appended to, and files whose chains link on past their size into /man's cluster removed and appended to, /man's cluster allocated and as it was; after the boot each directory's chain read off the disk, on exFAT its DataLength and ValidDataLength the whole chain, every other cluster of the exFAT partition holding the bytes it held unless the boot allocated it, nothing before the cluster heap changed but the FAT's cluster entries, fsck_exfat -n naming only the planted chain and fsck_msdos -n clean; fails with the free-slot scan not stopping at the size (entry sets written into /man's directory and over the new files' data), with the size not recorded (the 43rd file not found; the directory then grows at every create until the volume is full), with the NoFatChain directory not given a FAT chain first (an entry set split, fsck's complaint), with the refusal removed or made for a touch, with a size-0 directory taken to own the cluster it names, with rmdir or an overwrite freeing a size-0 entry's named chain, with a size-0 entry's cluster kept where its set is read (the append), with mkdir's refusal after its allocation (fsck: the bitmap), with rmdir refusing before its emptiness check, with a file freed along its whole chain, with an append following a link past the file's size, and with a directory with no cluster given none (fsd lost its volume); one boot, about a minute and a half - run it whenever a filesystem's grow_dir, find_free_run, create_entry, room, dir_extent, walk_dir, rmdir, free_file or directory entry writing changes
make test-cpoll             # poll for the keyboard in a C program (scripts/test-cpoll.py, /bin/CPOLL): timeouts 0, 200 ms (200-260, blocked in KEY_WAIT_UNTIL to a tick's precision, no spinning) and -1, POLLOUT for fd 1, POLLNVAL for a closed fd, fd 0 answered when another entry is ready, a blocked sleep with no fds (SLEEP_UNTIL), EFAULT for a null array, open of /net/ip refused ENOSYS after `mount -n /net` (no /net fd exists, so poll's POLLERR for one has no check: this line is the tripwire that says so), the C library's buffer for fd 1 flushed before the wait, a key typed before a poll of fds 0 and 1 answered POLLIN, and a key poll saw left in the kernel's queue when the program exits without reading (`cpoll leave`, the shell gets it); one boot, about a minute - run it whenever libc's poll, <poll.h>, open's refusal of /net or the read of fd 0 changes
make test-edit              # DevTools's editor on Ouroboros (scripts/test-edit.py, /bin/edit built by `make edit-bin` from EDIT_DIR, ../DevTools/edit): on a copy of the image, `edit hello.txt` opens, ^Q Q repeats a delete on poll's timeout until ^E stops it, Left through the 50 ms escape timeout moves the cursor, ^K D saves, and cat prints `hXi`; one boot, about a minute - run it whenever libc's poll, termios or read of fd 0 changes, or Edit's Ouroboros platform file does; with `CLINK=devtools` too (DevTools's `ld` links it) whenever loader.rs, programs/linker.ld or the C link flags change, and with `CCOMP=devtools CLINK=devtools` (DevTools's `cc`, `as` and `ld`, CC_DIR and AS_DIR) whenever the C headers or PICO_INC change; the image's /bin/EDIT is read back and must be the binary built for the toolchain asked (edit.bin, edit-ld.bin, edit-cc.bin, edit-ld-cc.bin)
make test-cargs             # argv in a C program (scripts/test-cargs.py): /bin/ARGS and libc/cargs.c built twice, /bin/CARGS (picolibc) and /bin/CARGSH (the hand-rolled libc), run with the same arguments in three cases (a few, none, and 15, the most the shell passes); the C programs must print ARGS's lines apart from argv[0]; one more word, as a command, a pipeline stage and after `exec`, must be refused with nothing run, and so must a glob one byte past ARGV_MAX (512), while one landing on it exactly runs (made on a copy of the image); one boot, about a minute - run it whenever libc/src/crt0.c, the kernel's argv store or the shell's argv split changes
make test-cerrno            # errno from the C file layer (scripts/test-cerrno.py, /bin/CERRNO): two boots, FAT32 as root and ext2 as root and then as `user`, 39 checks each as root (ENOENT, ENOTDIR, ENAMETOOLONG, EMFILE, EBADF, EINVAL, EOVERFLOW, ESPIPE, EFAULT, EISDIR, the console fds, and stat(path) against fstat's record, with every fd in use, and of the console and /net bindings) and, as `user`, EACCES opening /etc/shadow while stat of it succeeds, and the modes and owners the image gave two files; about two minutes - run it whenever libc/src/file.c's error paths or stat, or fsd's answers to them, change
make test-cenv              # the environment in a C program (scripts/test-cenv.py, /bin/CENV and /bin/CENVH): before and after four `set`s (the last a 128-byte value, the shell's longest), both C programs print /bin/PRINTENV's lines, and CENV's getenv answers SOURCE_DATE_EPOCH, PATH, the long value, a set name and an unset one, against the rig's own values, and /bin/RDPROBE reads all five per-task stores through a 4 KiB buffer (the only check that the kernel accepts a buffer larger than a store); one boot, about a minute - run it whenever libc/src/crt0.c or the kernel's env store changes
make test-cclock            # the clock in a C program (scripts/test-cclock.py, /bin/CCLOCK): gettimeofday and clock_gettime from MONOTONIC_US (1970 plus uptime, no wall clock); two runs in one boot with a gap the host times, each ctime checked against Python's formatting and the guest clock against the host's pace; about a minute - run it whenever libc/pico/clock.c or MONOTONIC_US changes
make test-cfenv             # the floating-point exception flags picolibc's math leaves (scripts/test-cfenv.py, `/bin/CPICO fenv`): flags raised and then cleared, and two operations that must raise them (the observer's own checks), then round(1e308), round(-1e308) and fdim(-1e308, 1e308) raising none and fdim(1e308, -1e308) a real overflow; fails on the libc.a built until 2026-10-16 without -frounding-math, with fetestexcept answering 0, and with feclearexcept doing nothing; one boot, about a minute - run it whenever picolibc is rebuilt or libc/picolibc-cross.txt changes
make test-include            # the C headers on the disk (scripts/test-include.py): on FAT32, ext2 and exFAT, every file under /include and /include/clang by path with its size, the whole listing of every directory, the two longer than one NP_READDIR page included (paged since 2026-10-16; fails with fsd ignoring the start index), `echo st*` in /include giving exactly the staged st names (the shell's own pager, past the first page), three files cat to the host's bytes, and names made on the guest keeping their case on all three (FAT32 by its case flags); three boots, about five minutes - run it whenever the header staging, or a filesystem's directory listing, changes
make test-cpp                # the C-hosting arc's finish line (scripts/test-cpp.py): /bin/cpp, DevTools's cpp built by `make cpp-bin` from CPP_DIR (../DevTools/cpp), run on a copy of the FAT32 image; cpp -o of hello.c, picodemo.c and high.c (UTF-8, high bytes, __DATE__) read back off the image byte for byte equal to the same cpp on the Mac built with signed and with unsigned char, clang compiles them, and `cpp hello.c` prints the program; one boot, about a minute and a half - run it whenever the C runtime, the header stage or cpp-bin changes; with `CLINK=devtools` too whenever loader.rs, programs/linker.ld or the C link flags change, and with `CCOMP=devtools CLINK=devtools` (DevTools's `cc`, `as` and `ld`) whenever the C headers or PICO_INC change; the image's /bin/CPP is read back and must be the binary built for the toolchain asked (cpp.bin, cpp-ld.bin, cpp-cc.bin, cpp-ld-cc.bin)
make test-unmount            # `unmount` with a partition mount and a held file, on a copy of the ext2 image (scripts/test-unmount.py, /bin/CFIDHOLD): both trees cleared, the held fid answering NO_FS and never handed to the next opener, the shell's own partition binding dropped, `erase` allowed after; about a minute and a half - run it whenever FSOP_UNMOUNT, the fid table or the shell's mount code changes
make test-crename            # unlink, rename and fstat in the C port (scripts/test-crename.py, /bin/CRENAME, /bin/CFSTAT): two boots, FAT32 and ext2 (a partition at /mnt/f for EXDEV), the checks Proem's and Edit's handoff notes name; about two minutes - run it whenever libc's unlink/rename/fstat, its headers, or fsd's NP_RM/NP_MV/NP_FSTAT changes
make test-mv-into           # a directory moved into itself by a case-only name (scripts/test-mv-into.py): three boots, FAT32, exFAT and ext2; each boot must say it mounted the format named; `mkdir /Docs; touch /Docs/keep.txt; mv /Docs /docs` must answer `cannot move /Docs to a subdirectory of itself, /docs/Docs` and leave keep.txt in /Docs on FAT32 and exFAT (where /bin/mv sends /docs/Docs, and fsd's guard compared bytes until 2026-10-19: the subtree lost, exit 0), and be a plain rename on ext2, where `mv /A /a` must still move /A into /a; `mv /Same /Same/sub` refused on all three; fails with fsd's names_match comparing bytes on FAT32 and exFAT and folding case on ext2 (the other spellings, `//`, `.` and `..`, are ninep-abi's host tests in `make test`); about two minutes - run it whenever fsd's NP_MV guard, a filesystem's mv or name matching, or /bin/mv's into-a-directory rewrite changes
make test-fat-link          # FAT32 never reaches past its volume, by a link or a first cluster (scripts/test-fat-link.py): a disk the rig builds, the ESP, a witness partition of 0xFF bytes after it and the ext2 root; eight files on the ESP, five whose links (both FAT copies) are damaged, two to clusters in the witness, one to a cluster whose FAT entry is there, one to the bad-cluster mark and one to cluster 1, and three whose directory entries name first clusters in the witness or with their entry there, each checked before the boot; after `mount 0 /mnt/f` and an `ls` listing them, reads must stop at the volume (RD one cluster, HEADR refused), appends extend inside it (APP) or are refused (HEADW), every `rm` succeed and free only the volume's clusters, FAT entry 1 stay as it was and the witness be unchanged; fails with main's fat32.rs before 2026-10-19 (8 checks, the witness written by two appends and two frees in both FAT copies) and with free_chain's first-cluster rule removed (HEADM's rm an I/O error); shares scripts/fat32img.py with test-write-walk; one boot, about a minute - run it whenever FAT32's next_cluster, fat_entry, write_fat_entry, cluster_to_lba, free_chain, write_at, read path or cluster_end changes
make test-mount-gate        # the MOUNT syscall answers the filesystem server alone (scripts/test-mount-gate.py, /bin/MOUNTPROBE): esp.img on virtio and a copy of usbstick.img 1 MiB longer on USB, on QEMU arguments of its own (no monitor socket); the builtin `mount` must first report the virtio copy's size, then `mountprobe` (MOUNT with replace set from an ordinary task) must be denied and `ls /` still list the ESP, then after `unmount` and `erase disk` `mount -a` must mount the stick through fsd's own rescan (FSOP_MOUNT calls MOUNT itself) and the builtin `mount` report the stick copy's size; fails with the kernel's gate removed (the stick installed under the mounted ESP, `ls /` reading it, `erase disk` erasing it) and with fsd's rescan removed (`mount -a` finds nothing); one boot, about a minute - run it whenever the kernel's MOUNT arm, fsd's FSOP_MOUNT or the shell's mount -a changes
make test-probe-ioerr       # a read error inside a partition's probe is an I/O error, not "no filesystem" (scripts/test-probe-ioerr.py): seven boots, the firmware booting esp.img (espgpt.img in GPT0) from NVMe (which the kernel cannot drive, so virtio goes in as the fallback and the boot partition is unknown), on virtio a copy of espexfat.img or espext2.img, signed again, attached through QEMU's blkdebug with every read of one sector failing EIO (past the table's sectors 0 and 1, checked before the boot), a USB stick beside it; MOUNT (exFAT alone, its root directory sector failing): fsd's boot line says the mount failed with a disk I/O error, `mount -a` answers that the disk was not replaced, no stick installed; FORMAT (the same): `format ext2` refused and the partition every byte as it was (the bytes are the check: the old format wrote ext2, then printed the same refusal); BESIDE (exFAT with the ESP): the ESP mounts with the line that a partition before it would not read, `mount 0` answers `device I/O error`, `format ext2` refused and both partitions as they were; EXT2 (ext2 alone, its first sector failing, which FAT32's and exFAT's probes read): ext2 mounts; TABLE (since 2026-10-22, espgpt.img with its GPT header, sector 1, failing: the table itself unreadable, #324's case): answered as MOUNT; SECTOR0 (since 2026-10-22, the exFAT copy whole with its MBR, sector 0, failing): the kernel holds the disk as not the boot disk (D6), refuses the stick and installs the disk as the fallback, and fsd answers as in MOUNT; GPT0 (the same day, espgpt.img booted from NVMe and a copy of it on virtio, its protective MBR failing): the kernel takes the copy for the boot disk from its GPT headers and installs it first; fails with the probe answering None for a read error (the stick at `/`, the partition overwritten), with the probe stopping at its first arm's error (EXT2), with format going on past a candidate that would not read (the ESP formatted) with `mount N` answering no filesystem, and with an unreadable table read as no partitions (TABLE: the stick at `/`), and with the kernel dropping a disk whose sector 0 will not read (SECTOR0: the stick at `/`), and with the kernel answering an unreadable sector 0 itself (GPT0: held, then the fallback); a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100, drive-qemu.py's classifier, since 2026-10-22); about two minutes (122 s on 2026-10-22, seven boots) - run it whenever fsd's probe, mount, format, `mount N`, partition discovery, mount -a's rescan or the kernel's init_storage changes
make test-volume-bound      # a volume claiming more than its partition is not mounted (scripts/test-volume-bound.py): fourteen boots, the firmware booting esp.img from NVMe, on virtio a disk built by scripts/mbrdisk.py from the images' own volumes, each case checked on the host before its boot by a model of the mounts' rules (it breaks exactly the one rule meant); the ext2 root mounts first and `mount N /mnt/pN` tries each other partition; PARTS: esp.img's FAT32, espexfat.img's exFAT and an ext2, each cut short with the next partition inside its claimed volume, all refused; MIXED: the FAT32 in a table entry of size 0 and the whole exFAT, which must mount and list their /etc, and the FAT32 at sector 2^32 - 65,536 on a 2 TiB sparse disk, refused; STALE and LONG: a 16 MiB disk, a 24 MiB exFAT boot sector with its root past the end, its entry the disk's size (STALE) or the volume's, past the device (LONG): the boot line says no mountable partition (not an I/O error), `format ext2` formats and `mount -a` mounts the ext2; WRAP: 2 FATs of 0x80000000 sectors (wraps u32), not mounted; SHAPES-A: exFAT's heap past its volume, FatOffset 8, root past the heap; B: exFAT's FAT too short, cluster shift 64, FAT32's root at cluster_end; C: FAT32 with no FATs, ext2's first data block 0 with 1 KiB blocks, too many groups; D: exFAT's FAT into its heap, FAT32 with mirroring off and its second FAT live (BPB_ExtFlags 0x0081); E: FAT32 with no reserved sectors, ext2's block-size shift 32, descriptor table past its blocks; F: ext2 with no inodes, ext2 with 64-byte inodes, ext2 with fewer groups than its blocks make; G: ext2 with more blocks or inodes a group than a bitmap block holds; H: ext2 whose group 0 descriptor puts its inode table past the group, its block bitmap on the descriptor table's block (which Linux's ext2_check_descriptors accepts) or its inode bitmap on its block bitmap, refused as an I/O error (damage a fsck repairs, so `format` does not take the volume for empty); each refused; BESIDE: a size-0 FAT32 with the next partition inside it refused, and an exFAT whose bitmap starts at the next partition's first sector mounts, a write to it answers an I/O error and the partition after it is unchanged; every rule taken out in turn fails the boot holding its case; `python3 scripts/test-volume-bound.py NAME...` runs named boots; a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100, drive-qemu.py's classifier, since 2026-10-22); about eight minutes - run it whenever a filesystem's mount_at, fsd's probe or volume_bound, or the partition table's sizes change
make test-ext2-blocks       # ext2 never reaches past its volume by a block number read off the disk (scripts/test-ext2-blocks.py): one boot, the firmware booting esp.img from NVMe, on virtio the ext2 root of espext2.img and three 4 MiB ext2 volumes made by mke2fs with 1 KiB blocks, A, C and B, then an unpartitioned WITNESS of 0xA5 bytes with a zeroed block; on A, RD's second pointer in the witness (`cat` stops at an I/O error), WR's (an append refused, its size kept), IND's indirect pointer at a zeroed witness block (an append refused, where it wrote the new pointer there), GHOST's entry naming an inode past the count in a second group whose descriptor slot copies the first's (an I/O error, not SRC's bytes), SRC read whole, `rm GHOST` dropping the entry, `rm RD` freeing only its own block (its inode left with a deletion time), L, a fast symlink whose first pointer names SRC's block, its i_blocks 2 as an attribute block makes it (`rm` leaves SRC's block allocated), Z, its i_blocks damaged to 0 (`rm` frees its block), BIGD, 270 KiB to its double-indirect block (`rm` frees all 273), RMI's and OWI's indirect blocks swapped for witness blocks (`rm RMI` frees its inode, `echo x > OWI` holds x CR LF, no error), DUP's second pointer naming a free block (`rm` succeeds) and A's free counts matching its bitmap after all of it; since 2026-10-21 on A also the last three blocks of group 0's inode table: T, its bit cleared alone and its bytes a pattern, and R, the last reserved descriptor block, its bit cleared too (the next allocation, OWI's new block, must pass over both, setting their bits and counting nothing, and leave T's bytes), META's second pointer naming T2 and MI's indirect pointer naming T3, every word of it SRC's block number (`rm` frees neither, nor SRC's block), GD's pointer naming the descriptor table's block (`rm` leaves it allocated), and MG's indirect pointer naming R2, the reserved block before R, every word of it SRC's (`rm` frees neither); on C, made without filetype, H naming an inode past the count (`ls` lists F, G and H; C's bitmaps planted in the witness until 2026-10-21, refused at mount now, test-volume-bound's SHAPES-H), since 2026-10-22 C made with 5,000 inodes and HIGH (empty) moved to the last, whose bit is in the inode bitmap's second sector, `rm` must clear it; and since 2026-10-21 C left one block free: appends to DIND (three blocks needed, to its double-indirect range) and TWELVE (two, to its indirect range) answer disk full with both files as planted, then ONEB gets the block, and after `rm G` `cp` of TWELVE answers disk full with CP keeping G's old block as its first KiB; on B every block used and the last group's padding bits clear (`echo x > NEW` disk full) and the inode count one past those in use (`touch NEW2` disk full, the bit clear); the witness unchanged; every plant read back on the host first; fails with Volume's bound out (RD 2,048 bytes, the witness written by WR and IND), the inode-number bound out (GHOST reads SRC), the block cap out (B an I/O error), the inode cap out (NEW2 an I/O error, its bit set), free_block freeing past the volume (A's bitmap), free_pointed reading a pointer block past it (RMI and OWI I/O errors), the counts raised for a clear bit (A's counts) the listing reading an inode past the count (`ls /mnt/c`), the fast-symlink test out or made by i_blocks (SRC's block freed by `rm L`; Z's kept), the pointer walk's span one block at every depth (BIGD's last block kept), rm reading GHOST's inode, the deletion time left out, the allocator taking a metadata block (T's pattern overwritten, or R, below meta_end), counting a block it passes over (A's counts), free_block freeing a group's bitmap or table block (META's T2, MI's T3) or one below meta_end (GD's), free_pointed reading a metadata block as pointers (SRC's block freed through T3, or through R2 with is_metadata's meta_end arm out), and ensure_block taking a block before it knows it can take them all (ONEB disk full: main's order, the give-back out, the indirect branch not asking for its pointer block), and write_at returning before its inode (CP empty, G's block lost), and bitmap_free writing the block's first sector for every bit (HIGH's bit set); a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100, drive-qemu.py's classifier, since 2026-10-22); about a minute, needs e2fsprogs - run it whenever ext2's Volume, block_sector, inode_place, alloc_block, alloc_inode, free_block, free_inode, bitmap_free, free_all_blocks, free_pointed, pointer_sector, ensure_block, list_dir, rm, write_inode, is_metadata, in_zone or meta_end changes
make test-long-set          # exFAT sets are patched whole, and only in the spec's shape (scripts/test-long-set.py, scripts/exfatimg.py): three boots of copies of the exFAT image; LONG: the root given a second cluster and /VEND planted with a 20-entry set (File, Stream, a name, 17 vendor extension entries) from the first cluster's last slot, read back and fsck_exfat -n clean before the boot, then `echo hello >> /VEND` (patch_stream_ext, File and Stream in two sectors and clusters) must leave the set 20 entries in place, the vendor entries as planted, the Stream naming its cluster and exactly 7 bytes, the SetChecksum over all 20, the data exact and fsck_exfat clean; SHAPE: GAPPY (a deleted slot inside) and PRIM (a primary inside) planted, `echo x >>` each must leave their slots as planted and make new files; RENAME (since 2026-10-21): /VEND's 20-entry set, its File entry given attributes and times, `mv /VEND /VendorRenamedLong` (two name entries) then `mv` to /V2 (one), /V2's set must be 20 entries with the File entry's attributes and times, the Stream's flags, ValidDataLength (7 of a contiguous cluster), cluster and DataLength, and the 17 vendor entries as planted, its checksum whole and fsck_exfat clean, and STRAY (its name entry after a vendor entry, fsck's one complaint before the boot) renamed to S2 in the spec's shape; fails with the sum capped at 19 (fsck: "Incorrect checksum for directory entry set. Delete?"), with walk_dir taking a misshapen set (PRIM's slots written), and with a rename building its set afresh (main before 2026-10-21) or leaving the vendor entries where the old name ended, with the Stream built afresh, and with only the name entries after the Stream dropped (STRAY's carried); a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100, drive-qemu.py's classifier, since 2026-10-22); about three minutes - run it whenever exFAT's patch_stream_ext, set_checksum, walk_dir's set assembly, slot_at, mv or renamed_set changes
make test-valid-length      # exFAT bytes past a file's ValidDataLength are zeros, read or written (scripts/test-valid-length.py): one boot of a copy of the exFAT image, four files planted preallocated (one contiguous cluster each, DataLength the cluster, ValidDataLength 7, `hello\r\n` then 0xEE); `cp /RD /RDC` must make a copy of hello and zeros, its ValidDataLength its size; `writeat /MID 100 x` must leave ValidDataLength 101, DataLength the cluster's and zeros from 7 to 100, and `cp /MID /MIDC` read zeros past the x; `echo hi >> /APP` both lengths the cluster and 4 and the gap zeros; `writeat /LOW 2 Z` ValidDataLength still 7; `write /NEW made` (a whole-file write, `write_file`) ValidDataLength its size; `echo y >> /BIG`, a 2 MiB preallocation, refused (more than MAX_GAP_FILL past its valid bytes) and BIG as planted; fsck_exfat -n clean before and after; fails with the read ignoring ValidDataLength, with the gap filled from DataLength, with both lengths written as one with ValidDataLength not decoded (main before 2026-10-22) with a written file's ValidDataLength short of its size with a file made whole given ValidDataLength 0 and with the gap bound measured from the size (BIG's append taken); a boot QEMU's firmware never starts is INCONCLUSIVE (exit 100); about a minute - run it whenever exFAT's read_entry, read_file, write_at, patch_stream_ext or walk_dir's Stream decoding changes
make test-exfat-names       # exFAT names outside ASCII are names of their own (scripts/test-exfat-names.py): one boot of a copy of the exFAT image, the host planting café (U+00E9, `acute`), then cafè (U+00E8, `grave`) and caf with U+1F600 (a surrogate pair, `astral`), each a contiguous cluster, and since the review of the same day 100 of U+4E2D (300 bytes of UTF-8) and `lone` with an unpaired surrogate, which a listing shows ASCII with `?` (`echo /*` must give 100 `?`s and `ls /` the name whole (since 2026-10-22; /bin/ls cut it at its 84-byte line before), `ls /` must give `lone?`, `cat` of each its bytes, `touch /lone?` no second set), all read back and fsck_exfat -n clean first; typed as UTF-8 on the serial line, `ls /` must list the first three by their UTF-8 names, `cp /grave` to a name with a pair make it (five units), `cat /cafè` print grave (the second set), `cat /CAF` + U+1F600 astral (ASCII case folded beside a pair), `echo new > /cafë` make a file of its own with `cat /café` still acute, and `mv /cafè /grave` move cafè's set; after the boot grave's set names cafè's cluster, café's set and bytes as planted and fsck_exfat -n clean; fails with main's exfat.rs before 2026-10-21, which looked names up by their ASCII rendering, `?` for every other unit (no UTF-8 name found), with the lookup folding every unit outside ASCII to one (grave read as acute, café overwritten and moved), with no `?` fallback (the root's listing fails at the overlong name, /bin with it), with no `?` arm in the lookup, with a pair refused on create and with `ls` cutting a long name; the rig removes cafë in its boot, since a name fsd makes outside ASCII carries an ASCII-only NameHash fsck_exfat rejects (a roadmap item); about a minute - run it whenever exFAT's walk_dir name assembly, entry_in, units_match, names_match, utf8_name or list_dir changes
make test-dir-scan          # making files in an exFAT directory reads its sectors a few passes a file, not one a slot (scripts/test-dir-scan.py, /bin/DIRPROBE, scripts/disktrace.py): two boots of a copy of the exFAT image, `dirprobe /D 1` and `dirprobe /D 100`, QEMU counting every virtio-blk read; /D's sectors (its chain read off the disk after the boot) must be read at most 8 passes of the directory a create (4.3 on the tree of 2026-10-19) and written at most 2.5 times a file (1.36), and the 100-file boot's extra reads in all are printed (4,686); fails with main's exfat.rs before 2026-10-19 (19.0 passes, 3.24 writes, 19,026 reads) and with a set written a slot at a time (3.24 writes); the slot cursor's chain walks hit the FAT cache and are not graded; about a minute and a half - run it whenever exFAT's find_free_run, slot_at, rewrite_slots, write_set_at, delete_set, grow_dir, walk_dir or room changes
make test-keyboard-chain     # rebuilds the image, then four driven QEMU boots through the nested-shell keyboard-chain recipes (scripts/test-keyboard-chain.sh); minutes, so not in `make test` - run it whenever tasks.rs's keyboard ownership changes
make test                   # host unit tests + clippy --all-targets for the pure crates (accounts, regex, ed25519, clusterkeys, ninep-abi, keyseq, parttable) + the cross-language wire-constant check + check-site + the rigs' INCONCLUSIVE classifier on fixed transcripts (`drive-qemu.py --self-test`, test-usb-hub's until 2026-10-22) + the disk signature rules (`disksig.py --self-test`, one-disk step 2) + check-xhci-barriers (so it also builds the kernel) + check-link-layout + check-picolibc
make check-relocs           # the PIE contract: no R_AARCH64_ABS64 in any userland binary
make check-xhci-barriers    # the xHCI driver's two DMA barriers found in the built kernel image (scripts/check-xhci-barriers.py); no QEMU rig can see one missing - ALSO RUN BY `make test` since 2026-10-03
make check-site             # the published GitHub Pages site vs the documents it abridges - ALSO RUN BY `make test` since 2026-09-05
make check-statics          # every static in kernel/src against the multi-core plan's inventory (docs/roadmap/roadmap-smp.md, scripts/check-statics.py), each in exactly one class row; fails naming a static in no row, in two, or gone from the tree - ALSO RUN BY `make test` since 2026-10-10 (step 0 of the multi-core arc)
make check-link-layout      # programs/linker.ld (comments aside) and LDFLAGS_RUSTSHIM against the pin of the copy DevTools's ld holds as --target=ouroboros (scripts/check-link-layout.py); fails until the pin is updated, which is when DevTools is sent a notice - ALSO RUN BY `make test` since 2026-10-14. `make CLINK=devtools` links /bin/edit and /bin/cpp with that ld (built from LD_DIR, ../DevTools/ld), ld.lld the default
make check-picolibc         # the committed C library (scripts/check-picolibc.py): libc.a hashed whole against a pin (it rebuilds byte for byte; the archive shipped until 2026-10-15 hung in realloc and only its bytes tell it apart), and the two header trees DevTools's cc reads from this working copy, third_party/picolibc-prebuilt/include and libc/pico/include, as git would track them; a changed libc.a owes test-malloc and the C rigs, a changed header tree owes DevTools a notice, before the pin moves - ALSO RUN BY `make test` since 2026-10-15
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
`clusterkeys`, `ninep-abi`, `keyseq`, `parttable`: 238 tests as of 2026-10-19, and the number is
checked by running it, not by incrementing), clippy over those crates' test
targets too, the cross-language wire-constant check
(`scripts/check-wire-constants.py`: Rust against the two C headers and the
two Python peers), the 9P host peer's verb-dispatch self-test
(`scripts/np9p_server.py --self-test`, one request per verb, added in #99
after a range test silently swallowed five verbs its docstring claimed; since
2026-09-23 also a KEYED session against `np9p_client.py` over loopback, every
verb plus the refusals on both sides, which is most of the suite's ~20 s), and
`check-site` (above), and since 2026-10-03 `check-xhci-barriers` (above),
which builds the kernel and finds the xHCI driver's two DMA barriers in its
image, the one check here no QEMU rig could stand in for, and since
2026-10-10 `check-statics` (above), every static in the kernel against the
multi-core plan's inventory, and since 2026-10-13 `disksig.py --self-test`,
the disk signature every image recipe writes (one-disk step 2). Since
2026-10-14 also `check-link-layout` (above), the layout DevTools's `ld`
copies, and since 2026-10-15 `check-picolibc` (above), `libc.a` and the headers
DevTools's `cc` reads, each pinned. The suite exists
because such a crate can otherwise have
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
  roadmap/           the sub-plans: the seven cluster documents (roadmap-cluster*.md), roadmap-fid-verbs.md,
                     roadmap-async-rmount.md, roadmap-session-auth.md, roadmap-user-keys.md, roadmap-el1-drop.md
                     (the Pi hands off at EL2; the kernel assumes EL1), roadmap-c-hosting.md (a C program as a Unix command)
                     roadmap-ctrl-c.md (Ctrl-C as a key for a raw program, with Ctrl+\ as the way out)
                     roadmap-smp.md (multi-core: the end of the single-core argument, the arc from 2026-10-12)
                     and roadmap-one-disk.md (one disk for boot and root: the ESP first, the root beside it, the arc after multi-core)
  work-journal/      chronological dev-log, one file per day plus an index README - a lighter companion to CHANGELOG.md
  testing/           testing-qemu.md (every `make run-*` target, the test images, the 9P host peers, the two-node
                     cluster rig), testing-parallels.md and testing-pi4.md (the real-hardware guides, sharing one
                     caveat: NO NETWORKING, so the whole cluster is QEMU-only), testing-exfat.md
  gap-analysis.md    per-subsystem have/partial/don't inventory vs mainstream Unixes, capped by a ranked list of the biggest gaps
  archive/           contemporaneous build logs, kept for archaeology only - not the reference
  postmortems/       the thirty-one design/bug/process retrospectives, one file each (see the section above, and docs/README.md)
  research/          synthesis notes on MINIX/Plan 9/Helix/Redox, the GUI stack, and where the design should go next

kernel/              every file annotated in full in `docs/source-map.md`; each also carries its own `//!`
  build.rs           the build identity (commit, +dirty, profile) the boot logs; reruns every build on purpose
  src/main.rs        #[entry]: the switch to the kernel's own 256 KB stack first (the Pi firmware's is 16 KB over its page tables), then
                     UEFI init, console/MADT/PSCI discovery, loader, ExitBootServices, then exceptions/mmu/xhci/storage/net/gic/timer/tasks
  src/uart.rs        PL011 console driver (post-ExitBootServices only)
  src/uart16550.rs   16550 console driver - PCI-discovered consoles, genuinely different hardware
  src/devicetree.rs  console discovery via the UEFI devicetree (dead end on QEMU and Parallels)
  src/dtranges.rs    devicetree address translation through `ranges` (bus -> CPU; the Pi 4's PL011 is 0x7e201000 on the bus, 0xfe201000 for the CPU)
  src/acpi.rs        console discovery via RSDP -> XSDT -> SPCR (works on QEMU, dead end on Parallels) + the shared find_table walk
  src/madt.rs        GIC version/address discovery via the ACPI MADT - where gic.rs's addresses actually come from
  src/power.rs       POWER syscall backend: PSCI SYSTEM_OFF via the FADT-discovered conduit, else halt
  src/pci.rs         PCI enumeration: serial-console discovery, discover_xhci, log_all_devices
  src/console.rs     global console handle (Pl011|Uart16550|Virtio|Framebuffer), shared with the exception handler
  src/framebuffer.rs GOP discovery: resolution/stride/format + framebuffer base - the console that works on Parallels
  src/font.rs        embedded 8x8 bitmap font, printable ASCII only (cond keeps its own copy)
  src/fbconsole.rs   framebuffer text console - the kernel's EMERGENCY/boot console only; steady state is cond
  src/fbdev.rs       dumb framebuffer primitives for cond (FB_BLIT/FB_SCROLL/FB_CLEAR, and FB_CURSOR: the one inverted cell, kernel-owned), gated to CON_TASK
  src/smp.rs         the secondary cores (multi-core step 2): PSCI CPU_ON per MADT core, the no-stack entry stub that drops each
                     core to EL1 (or turns its MMU on in place) on the boot core's tables, a 64 KB stack per core, TPIDR_EL1 the
                     core index, this core's GIC interface, `core N up`, then (step 4(b)) its own idle loop at EL0 with its own
                     timer. Any core can kick another into the kernel by SGI 0 under the kernel lock (step 4(c)). Since step 4(d) the
                     spawned programs run on the secondaries (tasks::may_run_here: the shell, idle and servers stay on the boot core)
  src/el2.rs         the exception level the firmware hands off at (logged on every boot) and the drop from EL2 to EL1 when it is 2:
                     EL1's regime set to the built identity map, SCTLR_EL1 whole, HCR_EL2 to el2.rs's HCR_EL2_VALUE (RW, HCD,
                     no traps, interrupts to EL1), the EL1 timer allowed, the firmware's EL2 timer off, then eret on the same stack. The Pi hands off at EL2; QEMU does with
                     virtualization=on (`make run-el2`). Called from mmu::switch_to_identity_map, so main.rs has one install
  src/earlyfault.rs  the early fault reporter: a fault BEFORE exceptions::install() goes through the firmware's vectors, and a RELEASE
                     firmware (the Pi's) prints one line for it; this registers the kernel's own handler through the firmware's CPU
                     protocol and prints ESR/FAR/ELR, a backtrace and the loaded image holding each address, on the serial
                     console, or on the framebuffer when there is none
  src/exceptions.rs  VBAR_EL1 vector table + fault reporting, and the three resumable paths (IRQ, SVC, EL0 fault)
  src/mmu.rs         per-task translation tables, built (memory only) then switched (registers, or the EL2 drop) as two halves.
                     READ ITS MODULE DOC FIRST: the L0 start level is a fixed bug, not a style choice
  src/gic.rs         version-dispatching facade over gicv2/gicv3, selected by madt.rs
  src/gicv2.rs       GICv2 backend: distributor + memory-mapped CPU interface
  src/gicv3.rs       GICv3 backend: redistributor + ICC_* sysreg CPU interface - confirmed on QEMU and Parallels
  src/timer.rs       ARM generic timer (EL1 physical, PPI 14 / INTID 30), TICK_INTERVAL_MS
  src/loader.rs      INIT.CFG + the boot programs off the ESP; ELF64 parsing + R_AARCH64_RELATIVE processing
  src/supervisor.rs  server supervision: restart a crashed or wedged server from its boot image, per-boot cap
  src/synccell.rs    SyncCell<T>, the one mutable-static wrapper: the single-core argument stated once, not per cell
  src/lock.rs        the kernel lock (multi-core step 4(a)): one core in the kernel at a time, taken at every resumable entry's
                     Rust half and released before its eret, owned by core index, never reentrant (a re-entry halts with a line)
  src/syscall.rs     the svc dispatch table - the authority on which syscalls exist (numbers/sentinels in syscall-abi)
  src/tasks.rs       task slots, round-robin scheduler, mailboxes, grants, capability send-mask, per-task identity.
                     THE AUTHORITY on slot numbers and counts - restating that map elsewhere has drifted before,
                     see docs/postmortems/asking-the-right-question-postmortem.md
  src/virtio_mmio.rs virtio-mmio transport: 32-slot discovery, modern register layout
  src/block.rs       BlockDevice enum (Virtio | UsbMsd) - what fsd's disk layer sits on
  src/usb_msd.rs     USB mass storage: Bulk-Only Transport + SCSI over xhci's bulk endpoints, with BOT error recovery
  src/virtio_blk.rs  virtio-blk: feature negotiation, one virtqueue, polling sector read/write
  src/virtio_console.rs  transmit-only virtio-console - works on QEMU, NOT what Parallels' serial port is
  src/virtio_rng.rs  virtio-rng, backing the RANDOM syscall; absent on Parallels/Pi and that is a supported case
  src/bootflags.rs   boot flag files at the ESP root: \NOXHCI, \XHCINOWR, \FBCON switch off one boot step to bisect a hang on hardware;
                     \MSDSTALL is a QEMU test fault (QEMU's stick only) for test-usb-hub.py --stall; \EARLYFAULT plants a fault in
                     the firmware's own code before the exit, and \WALKFAULT one inside the reporter's own image walk, both for
                     test-early-fault.py
  src/bootid.rs      the boot identity: a persisted per-boot counter + EFI_RNG boot entropy, before ExitBootServices (BOOT_ID)
  src/bootdisk.rs    the boot partition: the HardDrive node of the image's own device path, read before the exit (one-disk step 1),
                     and is_boot_disk, which makes the disk booted from the block device (step 3); the matcher itself is parttable's
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
  shellutils/        echo uptime clear pwd readkey send recv selftest bootid man printenv id args edtest rdprobe fpprobe coreprobe vtprobe syscost stageprobe cursorprobe dirprobe mountprobe
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
parttable/           partition tables, read: the MBR and GPT parsing (both CRCs, the backup header, checked bounds), the partition list
                     with root types, and the boot-disk matcher, through a read-sector closure; the ONE copy, which the kernel's
                     bootdisk.rs and fsd's partition.rs both call, host-tested in make test
keyseq/              tells a key's escape sequence (ESC [ A) from ordinary input: the ONE filter every keyboard reader uses
                     (the shell's line editor, login, ulib::read_line and read_key), so two readers never disagree on what was typed;
                     the kernel uses it too, to keep keys whole in its keyboard queue
libc/                the C-portability arc: crt0 + syscall stubs + a narrow waist (write/read/open/sbrk/_exit)
                     that BOTH a hand-rolled libc and a real PICOLIBC link against unchanged
                     (third_party/picolibc-prebuilt; regenerate with scripts/build-picolibc.sh)
nsresolve/           a Rust staticlib wrapping ninep_abi::resolve_ns, so a C program reaches the SAME namespace
                     resolver ulib and netd use. EVERY C program links it (file.c depends on it); the link needs
                     --gc-sections, see docs/source-map.md
scripts/             test-parallels.sh (real-hardware smoke test), drive-qemu.py + drive-2vm.py (drive the guest
                     shell / a two-node cluster unattended - the fussy paced typing is load-bearing, see
                     docs/testing/testing-qemu.md), mk{gpt,exfat,ext2,clusterkeys,passwd,group}.py (build the test disk
                     images and the staged /etc files), np9p_{client,server}.py (the host-side 9P peers - the
                     FOREIGN OBSERVER for both directions of the export)
```

Seventy-two-crate workspace (`cargo metadata`'s count, 2026-10-14). `kernel` and the shared libs (`ulib`,
`syscall-abi`, `ninep-abi`, `accounts`, `regex`, `ed25519`, `clusterkeys`, `keyseq`, `parttable`, `nsresolve`) sit at the repo root; **every userland
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

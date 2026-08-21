# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Ouroboros is an ARM64 (aarch64) operating system written in Rust (plus some
assembly where needed), still in its earliest stages. Design goals, taken
from `notes.txt` and `README.md`:

- Microkernel architecture
- POSIX-ish system calls
- Preemptive multitasking
- Filesystem choice still undecided (research needed; likely a simple FS first)
- Draws ideas from Linux, Minix, and Plan 9
- Primary test target is Parallels on Apple Silicon, not just an emulator

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

### Interactive echo shell (phase 1 of "get to a shell"): real UART input, a line editor, confirmed by piped-stdin testing

First real input path this kernel has had — every console driver before this
was write-only. Goal for this phase (user-defined): a terminal that accepts
typed input and echoes it back, as the foundation phase 2 will build command
parsing on top of.

**UART RX added to both console drivers**, `uart.rs` (PL011: poll `FR`'s
RXFE bit, read `DR` masked to the low byte — the upper bits are framing/
parity/break/overrun error flags, deliberately ignored for now, not checked)
and `uart16550.rs` (poll `LSR`'s Data Ready bit, read `RBR` at the same
offset as `THR` — direction disambiguates). Both non-blocking: `None` if
nothing is waiting, never spins. `console.rs` grew matching `read_byte()`/
`putc()` (raw single-byte write, no `\n`→`\r\n` translation — unlike
`write_str`, callers that want a newline send `\r\n` themselves) alongside
the existing `write_str`-based path.

**Two new syscalls exposed the UART to EL0**: `try_read_char` (3, returns a
byte or `syscall::NO_CHAR` — `u64::MAX`, out of any real byte's range so
one comparison tells them apart) and `putc` (4, arg0 = the byte). A third,
`shell_input` (5, arg0 = the byte), is the actual interactive primitive —
see below.

**Design call: keep EL0 code trivial, put the line editor at EL1.** Writing
a buffering/backspace-handling line editor as hand-assembled `global_asm!`
(the only way to run logic in the isolated EL0 region — see `syscall.rs`'s
module doc comment on the `rustc`/PE-COFF alignment ceiling that forces
this) would be painful and fragile. Instead `tasks.rs`'s task 0 became a
poll loop — `try_read_char`, and if a byte came back (it's already sitting
in x0), chain straight into `shell_input` with no register shuffling — and
all the real logic (buffer, backspace/DEL erase via the standard `\b`,
space, `\b` sequence, Enter → echo the completed line) lives in ordinary
Rust in the new `shell.rs`, called from `syscall.rs`'s dispatch arm for
syscall 5. **Task 1 changed too**: its old "report and loop" demo body was
replaced with a bare `wfe` loop — a genuine idle task — because once task 0
does real interactive I/O, task 1 periodically printing to the same console
mid-round-robin would corrupt the terminal output. For the same reason, the
per-tick `tick N` debug log in `exceptions.rs`'s IRQ handler was removed
(the tick counter itself, `TICKS`, is kept, just no longer printed) — at
roughly 27 ticks/sec, it would otherwise interleave with every keystroke.

**Confirmed working via piped-stdin QEMU testing, not just "compiles and
boots":** since driving a real interactive terminal isn't scriptable, QEMU
was run with its stdin attached to a FIFO (`mkfifo` + `exec 3<>fifo` to hold
the write end open without EOF), letting bytes be injected on a timer after
polling the console log for the `shell ready` banner. That polling step
turned out to matter: an early attempt that just slept a fixed 3 seconds
before sending input produced *no* echoed output at all — the likely cause
is UEFI's own boot-time console input handling (hotkey polling, etc.)
draining or discarding whatever sits in the PL011 RX FIFO before the kernel
ever starts. Waiting for the kernel's own "shell ready" line before sending
anything fixed it completely.

Three lines exercised end to end: `hi`, backspace, ` there` → buffer
correctly ends up `h there` (proving append and erase both work, not just
raw echo); a second full line typed and submitted normally; and
`backspace all` followed by 4× DEL + 3× backspace (7 erases against a
13-byte buffer) → buffer correctly ends up `backsp`. All three produced the
expected `you typed: ...` line. Cross-checked against QEMU's own `-d int`
trace for the same run: 89 `[SVC]` exceptions (three `try_read_char` +
`shell_input` pairs per typed byte, plus the erase bytes — consistent with
the byte counts above), 527 `[IRQ]`s, and zero aborts.

**Still coarse, worth knowing before building on it:** no prompt character,
no cursor/arrow-key handling, no history, buffer overflow is silently
dropped rather than reported, and "echo the completed line back" is
literally all Enter does right now — phase 2 (commands) is specifically
about replacing that one step with real parsing/dispatch, and nothing else
in `shell.rs` should need to change shape for it.

### Input lag fix: the round-robin tick period, and a real `-d int` measurement trap

Shortly after the echo shell above, real usage surfaced a genuine bug: typed
characters took up to about a second to echo. Root cause, confirmed by
direct investigation rather than guessed: `tasks.rs`'s round-robin swaps
tasks **unconditionally on every tick**, whether or not either task has
anything to do. With `TICK_INTERVAL_MS = 1000` (its value since the
timer-tick milestone), a keystroke arriving while task 1 (the idle task)
happened to be scheduled would sit untouched in the UART RX FIFO for up to
one full tick period — task 0 (the shell) simply wasn't running to poll for
it — before the next tick swapped back. Fix: `timer.rs` now owns
`TICK_INTERVAL_MS` as a single shared constant (previously duplicated
between `main.rs`'s initial `arm()` call and a private copy in
`exceptions.rs`, a real drift risk on its own), lowered to 20ms. Worst-case
latency drops to ~20ms, imperceptible; re-verified via the same piped-stdin
QEMU testing as the shell milestone, with a fresh `-d int` cross-check
still showing zero aborts at the higher tick rate.

**A real measurement trap surfaced while diagnosing this, worth not
repeating:** the very first attempt to characterize the actual tick rate
used `-d int`'s raw exception count over an entire QEMU run (boot included)
and concluded the tick was firing every ~20-40ms even *before* this fix —
which would have meant the 1-second lag theory was wrong. That number was
an artifact: UEFI firmware's own boot-time code (DXE dispatch, PCI
enumeration, etc. — still resident and executing at EL1 for the first
several seconds of every run, well before our kernel installs its own
`VBAR_EL1`) takes real IRQ exceptions of its own, at a much higher rate
than our kernel ever will. In one measured 10-second window, 500 of 527
total `[IRQ]` exceptions traced back to a single non-kernel EL1 address
(firmware's own code, well outside our image's address range) — almost all
boot noise, not ticks. A temporary debug print directly inside `timer::arm`
(counting real re-arm calls, immune to this contamination since `arm` is
never called by firmware) gave the true number: ticks roughly 1 second
apart, exactly matching the configured value and confirming `arm`'s
frequency-to-ticks math (`cntfrq_el0 / 1000 * interval_ms`) was never
actually buggy. Lesson for next time: any `-d int` rate measurement must
either start counting only after a known post-boot log line (as the
piped-stdin shell tests already did for a different reason) or instrument
the suspected code path directly — a whole-run exception count silently
includes several seconds of firmware activity that has nothing to do with
the kernel being tested.

### The shell becomes a real disk-loaded process, not kernel code

User-directed architecture shift: the shell (line editor and all) used to
be permanently compiled into the kernel binary — EL0-capable in theory,
but not in any way that let it be replaced without rebuilding the kernel.
That doesn't match how any real Unix-like system works, so this milestone
made it a genuine separate program, loaded from disk, selected by
configuration — see `docs/processes.md` for the full reference (design,
memory model, binary format, and a guide to writing a replacement) and
`docs/architecture.md` for how this fits the rest of the system. Only a
summary of what changed and what was learned lives here.

**Scope decision, made explicitly before writing any code:** "loaded from
disk" did not have to mean a real runtime block-device driver. UEFI's own
FAT32 driver on the ESP (`SimpleFileSystem` protocol) already reads files
just fine during the boot-services window — the same window the kernel's
own binary gets loaded in. `loader.rs` reads a config file and a program
binary that way, entirely before `exit_boot_services`, and a real
virtio-blk-plus-filesystem stack (needed eventually for runtime `exec()`)
stays deferred, on the same footing as the already-deferred Parallels
virtio-console work. This was an explicit user choice between two
presented options, not a default assumed unilaterally.

**New crate, new target, new toolchain wrinkle.** `shell/` is a second
workspace member, built for `aarch64-unknown-none` (not
`aarch64-unknown-uefi`) with its own linker script
(`shell/linker.ld`) placing `_start` at file/VA offset 0. Building it
uncovered two real build-system issues, both fixed:
- A plain `cargo build`/`cargo clippy` at the repo root tried to build
  `shell` too (it's a workspace member) using the workspace-default target
  (`aarch64-unknown-uefi`, from `.cargo/config.toml`) — which fails to
  link, since `shell` isn't a UEFI application. Fixed with
  `default-members = ["kernel"]` in the root `Cargo.toml`: `shell` stays a
  real workspace member (shared `Cargo.lock`, `cargo build -p shell
  --target aarch64-unknown-none` works) but is never implicitly pulled
  into a bare `cargo build`.
- Producing a raw flat binary from the linked ELF needs `objcopy`, which
  doesn't ship with Xcode's toolchain on macOS. `rustup component add
  llvm-tools` provides `llvm-objcopy`, but — unlike `rustc`/`cargo`
  themselves — it isn't proxied by `rustup which` or put on `PATH`; only
  `cargo-binutils`' `cargo objcopy` subcommand knows to find it that way,
  and pulling in a whole extra cargo subcommand for one invocation felt
  like more dependency than this needed. The Makefile instead computes its
  fixed, real location directly: `$(rustc --print sysroot)/lib/rustlib/
  $(host-triple)/bin/llvm-objcopy`.

**A genuinely nice accident: this kills the rustc/PE-COFF 8KB alignment
ceiling** that shaped the previous two milestones (`tasks.rs`'s original
8KB-static EL0 region, `syscall.rs`'s module doc comment). That ceiling
only ever existed because EL0 code had to be a `#[repr(align(N))]` static
*compiled into the kernel image itself*, and PE/COFF can't represent a
section alignment above 8KB. A disk-loaded program doesn't need any
compile-time-aligned static at all — `loader.rs` asks UEFI's
`boot::allocate_pages` for however many runtime-determined pages a program
needs, which has no such ceiling. The ceiling doesn't disappear from the
codebase (`tasks.rs`'s now-tiny idle-task region is still a compile-time
static, just small enough it was never affected), but it stops being a
constraint on "how big can a real program be."

**A new alignment problem took its place, solved differently.**
`boot::allocate_pages` only guarantees 4KB alignment, not the 2MB
`mmu.rs`'s L2/L3-splitting logic implicitly relied on (the old compile-time
static's `#[repr(align(0x2000))]` made straddling a 2MB boundary
impossible by construction; a runtime multi-page allocation has no such
guarantee). Rather than teaching `mmu.rs` to split a region across
multiple L2 slots, `loader.rs` over-allocates by one 2MB slot's worth of
pages and frees whatever falls outside the first 2MB-aligned address in
that range (`boot::free_pages` accepts freeing any page-aligned sub-range
of a prior allocation, confirmed against the UEFI spec's own description
of `FreePages`, not assumed) — recovering the same non-straddling
guarantee the old static got for free, just at runtime instead of compile
time. `mmu.rs` itself only needed a smaller, mechanical generalization on
top of that: `EL0_L3_TABLES` grew from one static to a
`MAX_EL0_REGIONS`-sized array, since two independent regions (task 0's
loaded program, task 1's small idle stub) can now legitimately need two
separate L3 splits instead of one.

**Cache maintenance got a real correctness fix, not just a rename.** The
previous milestones' `dc cvau` (clean D-cache line to point of unification,
required before invalidating I-cache for self-modifying code) was called
exactly once per task regardless of that task's code size — never actually
wrong on QEMU (TCG doesn't model real cache incoherency, so this was
untested against hardware that would notice), but a program loaded from
disk can legitimately be larger than one 64-byte cache line, so `tasks.rs`
now loops `dc cvau` over the whole loaded region before the usual single
`ic ialluis`+barrier sequence.

**`shell_input` (syscall 5) is gone.** Line editing moved out of the
kernel (`shell.rs`, deleted) and into the userland program itself
(`shell/src/main.rs`, real compiled Rust calling `try_read_char`/`putc`
directly) — the actual architectural change "the shell is a separate
process" was about, not just relocating where its bytes live. The syscall
dispatch table has a deliberate gap at number 5 rather than renumbering
anything after it.

**Confirmed working end to end via the same piped-stdin QEMU technique as
the earlier echo-shell milestone:** boot log shows `loader.rs` reading the
config file and program, `mmu.rs` mapping both EL0 regions
(`[(0x5c600000, 0x4000), (0x5c766000, 0x1000)]` in one observed run), and
the loaded program itself printing its own startup banner
("`Ouroboros userland shell`") and prompt — output that only exists
because *userland code*, not kernel code, produced it. A full interactive
sequence (type `hi`, backspace, type ` there`, Enter) round-tripped
correctly through the disk-loaded binary's own line editor, ending in the
expected `h there` and a fresh prompt. Zero aborts in a `-d int`
cross-check across the run.

**Still coarse, worth knowing about before building on it:** see
`docs/processes.md`'s "known rough edges" section (no shared syscall-ABI
crate, one program loaded once at boot with no `exec()`, fixed 2-task
scheduler, no heap/`.bss` for userland programs, fixed unguarded 8KB
stack, no ELF/relocations) rather than repeating it here — that document
is the one to keep current as these get addressed.

### Phase 2: real commands, and a genuine PIC/relocation gotcha found the hard way

Replaced the loaded shell's "echo the completed line" step
(`shell/src/main.rs`) with real tokenizing (`str::split_whitespace`, no
quoting) and a small builtin dispatch: `help`, `echo`, `uptime`, `clear`,
and an "unknown command" fallback for anything else. An empty line (bare
Enter, or all-whitespace) does nothing, matching a real shell.

**`uptime` needed a new syscall, not just new shell logic** — the whole
point, per the plan going in, was a command whose output means something
real rather than being another demo. `get_ticks` (6) exposes
`exceptions.rs`'s `TICKS` counter (already tracked, previously only used
internally) to userland; `exceptions.rs` grew a `pub fn ticks()` accessor
for `syscall.rs` to call. This is also the pattern for whatever a future
builtin needs from the kernel: add the accessor, add the syscall, keep the
gap-at-5 numbering discipline (see the syscall boundary/disk-loading
sections above).

**A real bug, found immediately, not by inspection: `write!`/`core::fmt`
crashes any loaded program that uses it.** `uptime`'s first implementation
used `write!(writer, "{ticks} ticks")` to format the tick count — and
crashed on the very first call, `ELR_EL1` landing on a tiny near-zero
address instead of real code (`Instruction Abort`, confirmed via the
exception handler's own report, not guessed). Root cause: `core::fmt`
builds its argument-formatting dispatch out of *data* — an array of
function pointers, one per formatted argument, baked into `.rodata` at
compile time for a binary linked at base `0x0`. Direct function calls
compile to PC-relative `bl` and stay correct wherever the binary actually
loads (which is never `0x0` — see the disk-loading section above); a
pointer *value* stored as data has no such self-correcting property, and
with `relocation-model=static` and no relocation processing in
`loader.rs`, nothing ever fixes it up for the real load address. Every
prior use of `putc`/direct calls in the shell had simply never exercised
this path. Fixed by hand-rolling decimal formatting
(`print_u64_decimal` in `shell/src/main.rs`) instead of going through
`core::fmt` at all — the only viable fix short of writing a real
relocating loader, which stays out of scope for now. Documented prominently
in `docs/processes.md` (both the "Binary format" section and the
"Writing a replacement program" guide) since this is exactly the kind of
thing a future program author would hit by surprise, silently-at-compile-time,
loudly-at-runtime.

**Confirmed working via the same piped-stdin QEMU technique as every prior
interactive milestone:** `help`, `echo hello world`, `uptime` (twice, with
other commands and a delay in between, showing the tick count actually
increases - 78 then 160 in one observed run), an unknown command, and an
empty line all produced exactly the expected output, including `clear`'s
raw ANSI escape sequence reaching the console. Zero aborts in a `-d int`
cross-check across the whole sequence.

### Phase 3a: a real virtio-blk driver, and a transport choice made by measurement

First runtime (post-`exit_boot_services`) disk I/O this kernel has ever
done — everything before this read files only during UEFI boot services
(`loader.rs`), a one-shot window. `docs/roadmap.md`'s phase 3a: a
synchronous, polling virtio-blk driver, proven by reading sector 0 back
and checking it against a value nothing but a real disk read could
produce (the MBR boot signature, `0x55 0xAA` at bytes 510-511).

**The transport choice (virtio-mmio vs. virtio-blk-pci) was settled by
directly inspecting what QEMU actually does, not by assumption.** Two
real, confirmed findings from `info qtree` (via the QEMU monitor) shaped
this:

- Plain `-drive ...,media=disk` with no `if=`/`-device` — the Makefile's
  `run` target until this milestone — auto-attaches as **virtio-blk-pci**,
  not virtio-mmio. First diagnosed backwards: an early `info qtree` query
  appeared to show no block device *anywhere*, which would have meant the
  ESP was never reachable by anything — obviously wrong, since the kernel
  demonstrably boots from it every session. Root cause was a monitor-read
  bug in the diagnostic script, not QEMU: reading the socket with a single
  fixed sleep before going non-blocking cut the response off partway
  through (consistently at the same byte count, which is what made it look
  like real data rather than truncation at first) — the PCIe section,
  further down, was never actually read. Fixed by polling with an idle
  timeout instead of a fixed sleep; the full response showed
  `virtio-blk-pci` at PCI address `02.0` all along.
- Reaching a PCI-attached device at runtime (as opposed to via UEFI boot
  services, which already had `pci.rs` for a different purpose) needs a
  driver-owned ECAM/config-space walk this project doesn't have — a real
  subsystem on its own, comparable in shape to writing PCI enumeration
  twice. **Decision: attach the drive as virtio-mmio explicitly instead**
  (`-device virtio-blk-device` + `if=none`), avoiding that subsystem
  entirely — `virtio_mmio.rs`'s 32-slot scan (addresses confirmed via the
  same devicetree-dump technique as `gic.rs`/`timer.rs`) needs no bus
  enumeration protocol at all.

**Modern (non-legacy) register interface, also a deliberate, verified
choice.** QEMU's `virtio-mmio` transport defaults to `force-legacy=true`
(confirmed via `-device virtio-mmio,help`'s printed default) — an older,
more complex register interface (page-frame-number/alignment-based queue
setup instead of explicit 64-bit desc/avail/used addresses). The Makefile
now passes `-global virtio-mmio.force-legacy=false`; the resulting
register layout was verified directly via the QEMU monitor's `xp/1xw`
(examine physical memory) before any driver code was written — reading
`MagicValue=0x74726976`, `Version=2`, `DeviceID=2`, `VendorID=0x554d4551`
("QEMU") straight out of guest physical memory, not inferred from the spec
alone.

**A genuinely surprising finding, traced to its actual cause rather than
left as a curiosity:** peeking the block device's Status register the
same way, *before* our kernel had run at all, showed `0xf`
(`ACKNOWLEDGE|DRIVER|FEATURES_OK|DRIVER_OK`) already set — a device that
looked fully initialized despite no driver of ours ever touching it. Cause,
once traced through: EDK2 firmware bundles its own virtio-blk driver, and
that driver is *how firmware itself* finds and loads this kernel and reads
`loader.rs`'s config/program files during boot services — the same device,
initialized by someone else first. `Device::init` therefore resets the
device (`Status = 0`) unconditionally as its first step, per the virtio
spec's requirement for a driver taking ownership of a device, rather than
assuming a clean slate.

**Cache coherence for the virtqueue needed no extra work, confirmed rather
than assumed dangerous:** the devicetree dump that confirmed the transport
addresses also showed `dma-coherent;` on every `virtio_mmio` node — QEMU
stating outright that virtio DMA on this platform is cache-coherent, unlike
the genuinely-needed cache maintenance around `tasks.rs`'s self-modifying
EL0 code. Ordinary memory barriers (`dsb sy` before the notify doorbell,
`dmb sy` inside the completion poll) are still used, for ordering, not
coherence — a different concern the "dma-coherent" property doesn't cover.

**Confirmed working end to end, not just "no error returned":** one QEMU
boot produced `virtio-blk ready, capacity 1032192 sectors` followed by
`virtio-blk read sector 0, boot signature 0xaa55 (valid MBR)` — a specific,
independently-verifiable value (vvfat's synthesized FAT volume really does
carry a valid MBR at sector 0) that this code could not produce by
accident. Interactive shell use (`uptime`, `help`) immediately afterward in
the same boot confirmed nothing about the existing boot sequence regressed.
Zero aborts in a `-d int` cross-check.

**Still coarse, worth knowing before building on it:** read-only (no
write support, matching `docs/roadmap.md`'s explicit phase-3 scope), one
request in flight at a time (no queuing/batching), polling completion
rather than interrupt-driven (this device's IRQ line is real - GIC INTID
depends on which of the 32 slots QEMU happens to populate - but wiring it
was deliberately skipped for this first cut, matching every other driver
in this kernel so far), and not yet verified against anything but QEMU
(Parallels' own virtio implementation, if it has one, is unconfirmed -
same open question already on record for virtio-console).

### Phase 3b: a hand-rolled FAT32 reader, and `make run`'s dev loop turned out to be the wrong disk format

`kernel/src/fat32.rs`: read-only FAT32 (MBR partition lookup, BPB parsing,
FAT cluster-chain traversal, 8.3-only directory entries) built directly
over `virtio_blk::Device`, no crate. Phase 3b of `docs/roadmap.md`.

**Hand-rolled wasn't just precedent this time - it's a hard constraint.**
Every parser before this (ACPI, devicetree, virtio) was hand-rolled by
choice. FAT32 has a real reason beyond that: this reader runs after
`exit_boot_services`, where the global allocator is no longer valid (it
was boot-services-backed). Every `no_std` FAT crate surveyed assumes an
allocator is reachable somewhere in its stack (directory listings as
`Vec`, path handling as `String`) - reworking one to run with zero heap
would likely be more effort than writing exactly the subset needed by
hand. Confirms the call `docs/roadmap.md` flagged as an open question
rather than deciding unilaterally.

**A real, confirmed surprise: `make run`'s fast dev-loop disk isn't
FAT32.** Before writing any parser code, sector 0 and the partition's
BPB were dumped through a temporary boot-time hex print (same discipline
as phase 3a's `xp/1xw` monitor peeks) and decoded by hand. `make run`'s
`fat:rw:esp` (QEMU's `vvfat` driver) turned out to produce **FAT16**, not
FAT32 - `BS_FilSysType` literally reads `"FAT16   "`, and `RootEntryCount`/
`FATSz16` are both nonzero, fields that real FAT32 requires to be zero.
`esp.img` (built by `hdiutil -fs FAT32`, what `make image`/
`parallels-hdd` and therefore Parallels itself ultimately boot from) is
genuinely FAT32, confirmed the same way. The Makefile gained a new
`run-image` target (boots `esp.img` directly with the same virtio-mmio
setup as `run`) specifically because `run`'s vvfat can never satisfy
`fat32.rs::Fs::mount` - not a bug in the reader, a real format mismatch
between the fast QEMU loop and everything else.

**A second real surprise, found by testing against real content, not by
inspection: this project's own `\EFI\OUROBOROS\` directory name doesn't
fit FAT's 8.3 short-name limit.** `OUROBOROS` is 9 characters. Real FAT32
formatters (macOS's `hdiutil`/`newfs_msdos` included) handle this by
writing a long-filename (LFN) entry holding the real name plus a mangled
8.3 alias (`OUROBO~2`) for compatibility - and this reader deliberately
doesn't parse LFN entries yet (every file this project creates was
*assumed* to fit 8.3; that assumption was simply wrong for one of its own
directories). Rather than either implementing LFN parsing now or renaming
an established, widely-referenced path before phase 3c's shell/UX needs
are known, this was left as a documented, confirmed gap - verification
instead used `\EFI\BOOT\BOOTAA64.EFI`, where every path component already
fits 8.3 cleanly.

**Resolved at the start of phase 3c: renamed, not parsed around.**
`\EFI\OUROBOROS\` became `\EFI\OUROBORO\` (8 characters) - `loader.rs`'s
`CONFIG_PATH`, the Makefile's `esp` target, and every doc reference
updated together. **(Renamed again later, 2026-08-17, to `\EFI\ORBS\` -
a tidier abbreviation the user preferred, same 8.3 reasoning; the
transcripts below record the `OUROBORO` era as they happened.)** This project controls the name, so once phase 3c's
actual need (the shell's own `cd`/`ls` navigating there) was concrete,
renaming was clearly cheaper and more honest than implementing LFN
parsing to accommodate one avoidable 9-letter directory. Confirmed
working: the runtime FAT32 reader now reads `\EFI\OUROBORO\INIT.CFG`
successfully and gets back the same content `loader.rs` read via UEFI
boot services earlier the same boot - two independent code paths, same
file, same bytes.

**Confirmed working end to end against real content, not just "mount
succeeded":** listing `\EFI\BOOT` correctly showed `.`, `..`, and
`BOOTAA64.EFI` with its exact real size; reading `BOOTAA64.EFI` back
(346,624 bytes at the time, spanning roughly 677 clusters at this image's
512-byte cluster size - a real multi-cluster chain traversal, not a
single-block special case) produced that same size and the correct PE
header magic (`4D 5A`, "MZ") - values nothing but a genuine, correct
multi-sector read could produce, independently checked against `ls -la`
on the built binary rather than hardcoded (a hardcoded expected size
would have been self-referentially wrong the moment this very code
changed the kernel binary's own size). Interactive shell use immediately
after in the same boot confirmed nothing regressed. Zero aborts in a
`-d int` cross-check.

**Still coarse, worth knowing before building on it:** no long filenames
(see above); read-only; only the first FAT32-typed MBR partition is
considered (no GPT, no multi-partition disks); FAT12/16 are explicitly
rejected rather than supported (`Error::NotFat32`), matching this phase's
FAT32-only scope. (One thing this originally said - "no `.`/`..`
special-casing beyond what falls out of walking them like any other
entry" - turned out to be wrong in a way that hung the whole system;
see "Phase 3c" below.)

### Phase 3c: real disk commands, and two genuinely new classes of bug found by testing, not inspection

`ls`/`cat`/`cd`/`pwd` are real now - phase 3 (`docs/roadmap.md`) is
complete. The syscall ABI grew from 1 argument to 4 (`fs_list_dir`/
`fs_read_file` need a path pointer/length and a buffer pointer/length at
once - `exceptions.rs`'s SVC trampoline now reloads `x0`-`x3` fresh from
its saved stack frame rather than juggling live registers, and
`syscall.rs` persists a global mounted `fat32::Fs` for both syscalls to
share). `shell/src/main.rs` gained a `cwd` buffer, a `resolve_path`/
`normalize_path` pair, and four command handlers - all still no-heap,
stack-local state, same discipline as everything else in that crate.

**A second, confirmed instance of the `core::fmt`-class relocation bug —
this time in ordinary comparison code, not formatting.** `cd`'s path
resolution needs to know whether the current directory is already root
(`cwd_bytes != b"/"`, comparing a `[u8]` slice against a byte-string
literal) - and that single comparison crashed, `ELR_EL1` inside the
shell's own code, `FAR_EL1` a small, code-layout-dependent address that
shifted between builds as debug prints were added/removed - exactly the
signature of a data reference computed for this binary's link-time base
of `0x0`, wrong once loaded anywhere else (the same root cause
`print_u64_decimal`'s doc comment already documents for `core::fmt`).
Bisected by binary-searching temporary `print_line` calls through
`resolve_path` until the exact crashing statement was isolated - not
guessed. Fixed by replacing every slice/string comparison against a
literal (`!= b"/"`, and the `.`/`..` checks `normalize_path` needed) with
scalar comparisons instead (`len() == 1 && bytes[0] == b'/'`), which
never trip this. **The practical rule this adds to "no `core::fmt`" for
anyone writing a loaded program:** avoid comparing a slice/string against
a literal too, for the identical reason - direct calls and scalar
(integer/byte) comparisons are fine; anything that needs a *reference* to
literal data baked into `.rodata` isn't.

**A second, unrelated real bug, found only by testing actual navigation,
not by inspecting the code:** `cd ..` twice in a row (e.g.
`/EFI/BOOT` → `..` → `..`) hung the *entire system* - zero console
output, zero reported exceptions, nothing. Root cause: a FAT32
subdirectory's `..` entry conventionally stores cluster `0` to mean "the
root directory," not root's own real cluster number - a real on-disk
convention, not a hypothetical. `fat32.rs::Fs::find` didn't know this, so
that `0` flowed straight into `cluster_to_lba`'s `cluster - 2`, an
unsigned underflow that wrapped to a huge, garbage sector number. The
resulting read didn't fault (no exception was ever reported) because
there was nothing to preempt it: this all runs inside a syscall, and
exception entry masks IRQs until the next `eret` - the timer tick that
would ordinarily rescue a runaway loop never got a chance to fire. Fixed
in `find`: substitute `self.root_cluster` whenever a resolved entry's
cluster is `0`. Confirmed via piped-stdin QEMU testing, isolating the
exact step: a single `cd ..` (`/EFI/BOOT` → `/EFI/BOOT/..`) worked fine;
a second one on top of it hung every time, before the fix, deterministically.

**A related shell-side cleanup, motivated by the same testing, not just
cosmetics:** `resolve_path` now normalizes (`normalize_path`) after
concatenating, collapsing `.`/`..` instead of leaving them in `cwd`
literally. Without it, `cwd` accumulated an ever-growing literal `../..`
suffix on every `cd ..` rather than shrinking back toward root - which,
beyond just looking wrong in `pwd`, meant every subsequent lookup
re-walked the same already-visited directories for no reason, and made
the cluster-`0` bug above easier to hit twice in a row instead of once.

**Confirmed working end to end, both fixes verified together:**
`cd EFI` → `cd OUROBORO` → `pwd` (correctly `/EFI/OUROBORO`) → `ls`
(correct real entries) → `cat INIT.CFG` (correct real content) →
`cat SH.BIN` (binary content printed raw, plus a correct truncation
notice - the file is bigger than the shell's 256-byte read buffer);
separately, `cd ..` twice from `/EFI/BOOT` now lands cleanly back at `/`
with no hang; `cd`/`cat` to nonexistent paths report clean errors instead
of crashing. `make run` (FAT16, no filesystem mounted) still degrades
gracefully - `ls`/`cat`/`cd` report errors, `pwd` still works, boot
proceeds normally. Zero aborts in a `-d int` cross-check across every
scenario above.

**Still coarse, worth knowing before building on it:** no multi-word
arguments (`cd "a b"` isn't supported, no quoting - matches `echo`'s
existing limitation); `cd ..` past root silently stays at root rather
than erroring; paths are capped at a small fixed depth
(`MAX_COMPONENTS`, 16) and length (`PATH_SIZE`, 128) with no graceful
"path too complex" message beyond a bare "path too long"; `cat`'s read
buffer is a fixed 256 bytes, so any file bigger than that is genuinely
truncated, not paged; the pointer/length arguments `fs_list_dir`/
`fs_read_file` take are trusted, not validated against the calling
program's actual mapped region (fine with exactly one, currently-trusted
userland program - see `syscall.rs`'s module doc comment).

### Phase 4: `mkdir`/`rmdir`, the first real filesystem write support

`docs/roadmap.md` had deliberately parked "any write support" past phase 3
because of real corruption risk (FAT table updates, cluster allocation).
This milestone crossed that line for the narrowest useful case: creating
and removing empty directories.

**`virtio_blk::Device` gained `write_sector`, its first write path ever.**
Shares a `submit_request` helper with the existing `read_sector` (both were
previously one monolithic function); the only real difference between a
read and a write request is which way the data descriptor's
`VIRTQ_DESC_F_WRITE` flag points (device-writable for a read, the reverse
for a write) and the request-type field in the header (`BLK_T_OUT` vs
`BLK_T_IN`) - everything else (the 3-descriptor layout, notify, poll the
used ring, check the status byte) is identical, which is why sharing one
function was correct rather than duplicating it.

**`fat32.rs` gained the actual write primitives**, each doing exactly one
thing: `write_fat_entry` (read-modify-write, preserving the existing
entry's top 4 reserved bits, and - unlike every read path in this module,
which only ever consults the first FAT copy - written to *every* FAT copy
the volume has, since a write is what the redundancy is actually for);
`find_free_cluster` (linear scan of the first FAT copy only, bounded by
the FAT's own size so a full disk fails cleanly rather than looping);
`zero_cluster` (a fresh directory cluster must read back as "no entries",
which only holds if it's actually zeroed - FAT32's `DIR_ENTRY_END` is
`0x00`, so an unzeroed cluster would contain garbage that might not present
as empty); and `write_raw_entry` (writes one 32-byte directory entry field
by field, no RTC on this kernel so timestamps are left zeroed rather than
faked). `walk_dir` was generalized into `walk_dir_with_location`, so
`rmdir` can find and patch a target's own directory entry (mark it
`DIR_ENTRY_FREE`) without a second, separate directory-walking
implementation.

**Name encoding is deliberately conservative, not a full FAT-legal
implementation.** `make_short_name` only accepts ASCII alphanumerics, `_`,
and `-`, one optional `.` splitting an up-to-8-character base from an
up-to-3-character extension - anything else (spaces, most punctuation,
names needing LFN) is rejected outright as `Error::InvalidName` rather than
approximated. This only constrains what *this kernel* can create; existing
on-disk names outside that set (created by a real formatter/OS) are
unaffected, same as the existing no-LFN read limitation.

**`mkdir`'s on-disk sequence, in order, and why that order matters:**
resolve the parent (must exist, must be a directory) and reject if the
name already exists; find a free cluster and mark it end-of-chain in the
FAT *before* writing anything else, so a failure partway through leaves
the cluster correctly claimed rather than reusable; zero it; write its
`.`/`..` entries (`..` gets cluster `0` when the parent is root, the same
convention `fat32.rs::Fs::find` already had to special-case on the read
side - see phase 3c's writeup below); only then link the new entry into
the parent via `insert_dir_entry`. **Deliberately no directory-extension
support**: if the parent's existing allocated clusters have no free entry
slot, `insert_dir_entry` returns `Error::DirectoryFull` rather than
allocating and linking in a new cluster for the parent - a real, documented
limitation, not an oversight, matching this module's existing "narrowest
useful case first" discipline. **(Update: lifted - see "Directory
extension" further below; `DirectoryFull` no longer exists.)**

**`rmdir` checks everything before writing anything**, so a rejected call
never partially applies: must resolve to a directory, must not be root
(`target.cluster == self.root_cluster || target.cluster == 0` - both
checked, since the cluster-`0`-means-root convention means an unresolved
root reference could otherwise slip through), and every entry inside it
must be `.`/`..` or it fails with `Error::DirectoryNotEmpty`. Only after
all three pass: free the cluster in the FAT (`write_fat_entry(_, 0)`), then
locate the target's own entry in the parent (via
`walk_dir_with_location`) and mark it `DIR_ENTRY_FREE`.

**Two new syscalls, `fs_mkdir`/`fs_rmdir` (9/10)**, both taking just a path
pointer/length (no output buffer, unlike `fs_list_dir`/`fs_read_file`) -
the gap-at-5 numbering discipline from phase 3c continues, no renumbering.
`shell/src/main.rs` gained `cmd_mkdir`/`cmd_rmdir` following the exact
shape of `cmd_ls`/`cmd_cat`/`cmd_cd`, and was written with phase 3c's two
documented relocation-class gotchas in mind from the start (no
`core::fmt`, no slice/string-vs-literal comparisons) rather than
discovering them a third time.

**Confirmed working end to end via the same piped-stdin QEMU technique as
every prior interactive milestone, against the real `esp.img` (not `make
run`'s FAT16 vvfat, which can't mount at all - see phase 3b):** created a
directory, listed it, `cd`'d into it, created and removed a nested
subdirectory, `cd ..`'d back out, removed the outer directory, confirmed
`ls` shows it gone. Safety checks confirmed real, not just plausible:
`rmdir /` correctly refused (`CannotRemoveRoot`), `mkdir /EFI/BOOT`
against an already-existing directory correctly refused (`AlreadyExists`).
**Persistence confirmed by an actual reboot, not just a live in-memory
check**: created a directory, killed QEMU entirely, booted a fresh QEMU
instance against the same `esp.img`, and the directory was still there via
`ls` on the fresh mount - then removed it and confirmed it was gone on
that same fresh boot, proving the write path reaches real disk sectors
rather than some cached state. Pre-existing files
(`/EFI/OUROBORO/INIT.CFG`, `/EFI/BOOT/BOOTAA64.EFI`) were re-read after
all of the above and came back byte-identical to before - no corruption
of unrelated disk contents. `make run` (FAT16, no filesystem mounted)
still degrades gracefully: `mkdir`/`rmdir` report a clean failure rather
than crashing. Zero aborts (`Data Abort`/`Prefetch Abort`/`Undefined
Instruction`) in `-d int` cross-checks across all three QEMU sessions
(the main test sequence, the fresh-reboot persistence check, and the
FAT16 degradation check).

**Still coarse, worth knowing before building on this:** no directory
growth (a full parent directory's existing clusters means `mkdir` fails
rather than extending it); no file creation/deletion, only directories
(`touch`/`rm` stay unimplemented); no `mv`/`cp`; every error collapses to
one sentinel at the syscall boundary (`u64::MAX`), so userland can't yet
distinguish "already exists" from "disk full" from "bad name" - `syscall.rs`'s
`fs_mkdir`/`fs_rmdir` doc comments flag this explicitly; and the
conservative short-name character set means some technically-valid 8.3
names (e.g. containing spaces) can't be created by this kernel even though
they could be read if another tool created them.

**A real UX bug found immediately by the user testing this milestone,
not by inspection: every disk command failing on `make run` was
indistinguishable from a genuinely broken path.** Booting `make run`
(FAT16 vvfat - see phase 3b) and typing `ls` produced `ls: no such
directory`; `mkdir test` produced `mkdir: failed (already exists, bad
name, parent missing, or disk full)` - both technically true (there is
no mounted filesystem to have a directory in) but actively misleading,
since the *actual* cause (`FAT32 mount failed (no FAT32 partition...)`)
was logged once at boot and never surfaced to the shell again. Every
`fs_*` syscall was collapsing two genuinely different failure classes -
"no filesystem is mounted this boot" and "the filesystem is mounted but
this specific operation failed" - into the exact same `u64::MAX`
sentinel, so the shell had no way to tell them apart even if it wanted
to. **Fixed by splitting the sentinel, not by adding more text-matching
logic:** `syscall.rs` gained a second, numerically distinct sentinel,
`NO_FS` (`u64::MAX - 1`), returned by all four `fs_*` syscalls
specifically when `FsCell` is empty, while every other failure still
returns the original `FS_ERROR` (`u64::MAX`) - safe to keep distinct
from any real success value, since byte counts/file sizes never
approach `u64::MAX - 1`. `shell/src/main.rs`'s five disk commands now
`match` the raw syscall return against both sentinels (a plain integer
`match`, not a slice/string literal comparison - doesn't trigger the
relocation-class bug documented elsewhere in this file) and print a
shared, explicit `no filesystem mounted this boot (...)` message instead
of their normal command-specific error whenever `NO_FS` comes back.
Confirmed via the same piped-stdin QEMU technique as every other
milestone: all five commands (`ls`/`cat`/`cd`/`mkdir`/`rmdir`) now print
the new message on `make run`'s FAT16 disk, while a fresh `make
run-image` boot still produces the normal, unchanged success/error
output (`ls` lists real entries, `mkdir`/`cat`/`cd` report their usual
specific errors for bad paths). Zero aborts in `-d int` cross-checks on
both boots.

### A shared syscall-ABI crate, closing a long-tracked gap

Every "known rough edges"/"next milestone" note since phase 3c had
flagged the same thing: syscall numbers and sentinel values were
hand-duplicated between `kernel/src/syscall.rs`'s dispatch table and
`shell/src/main.rs`'s caller, kept in sync only by convention. By this
point there were ten syscalls and three sentinel constants (`NO_CHAR`,
`FS_ERROR`, `NO_FS` - the last one added just one milestone ago) - a
real, growing risk, not a hypothetical one anymore.

**Fixed with a third workspace member, `syscall-abi/`** - a plain
`#![no_std]` library crate holding nothing but `pub const` syscall
numbers and sentinel values, no logic. Both `kernel` and `shell` now
depend on it via a path dependency (`syscall-abi = { path =
"../syscall-abi" }`) and reference constants directly
(`syscall_abi::FS_MKDIR`, etc.) instead of local, independently-numbered
consts. `kernel/src/syscall.rs`'s `dispatch` match arms now match on
`syscall_abi::PRINT`/`syscall_abi::TRY_READ_CHAR`/etc. rather than bare
integer literals, so a future syscall added on one side with the wrong
number literally fails to compile on the other rather than silently
misbehaving at runtime.

**Confirmed safe against this project's specific relocation risk, not
just "it's simple so it's probably fine."** Every value in `syscall-abi`
is a scalar `u64` const - the compiler inlines these as immediate
operands at the use site in both `kernel` (target
`aarch64-unknown-uefi`) and `shell` (target `aarch64-unknown-none`),
the same way a local `const` always did. This is fundamentally different
from the `core::fmt`/slice-literal-comparison bug documented in the
phase 2/3c sections above, which is specifically about *pointers* to
literal data in `.rodata` computed for a link-time base of `0x0` - a
plain integer has no such pointer, so depending on a cross-crate numeric
constant from the unrelocated `shell` binary carries none of that risk.
`shell/src/main.rs`'s `cmd_ls`/etc. still `match` raw syscall return
values against `NO_FS`/`FS_ERROR` exactly as before (see the previous
section) - those are still plain integer comparisons, just imported from
`syscall-abi` now instead of being local consts.

**Confirmed working end to end, not just "it compiles":** both `cargo
build`/`cargo clippy -- -D warnings` (kernel) and `cargo build -p shell
--target aarch64-unknown-none`/`cargo clippy -p shell --target
aarch64-unknown-none -- -D warnings` (shell) stayed clean throughout -
`syscall-abi` itself needed no target-specific configuration or
`default-members` entry, since `kernel` pulling it in as a dependency is
enough for a bare `cargo build` at the repo root to build it too. A full
piped-stdin QEMU regression pass against a fresh `esp.img` (`help`,
`uptime`, `ls`, `mkdir`/`cd`/`pwd`/`rmdir`, `cat` on both a real file and
a nonexistent one) produced byte-identical output to before this
refactor, with zero aborts in a `-d int` cross-check - this was a pure
internal restructuring, and testing confirmed it changed no observable
behavior.

### Phase 5: `touch`/`rm`, and a latent bug the new feature would have hit immediately

`mkdir`/`rmdir` (phase 4) proved directory write support; this milestone
rounds out file lifecycle with `touch` (create an empty file, or no-op if
one already exists) and `rm` (remove a file). Deliberately still narrow -
no way to *write content* into a file, since that needs a syscall this
project doesn't have yet (an `fs_write_file`/append primitive) - so every
file this kernel can create is, and stays, zero bytes until that lands.

**`touch` turned out to be simpler than `mkdir`, not harder.** A real
FAT32 empty file needs no cluster allocated at all - a directory entry
with starting cluster `0` and size `0` *is* a valid empty file, per spec.
So `Fs::touch` skips every step `mkdir` needs beyond the last one: no
`find_free_cluster`, no `zero_cluster`, no `.`/`..` writes - just one
`insert_dir_entry` call with cluster `0`, size `0`, and attribute byte
`0` (no directory bit). `Fs::rm` mirrors `Fs::rmdir`'s shape (walk to the
target, then to its parent to locate and free its own entry), with one
addition `rmdir` never needed: freeing a file's *entire* cluster chain
first (a loop over `next_cluster`/`write_fat_entry`, a no-op for an
empty file whose cluster is `0`) rather than just one cluster.

**A latent bug in `Fs::find`, caught by reasoning through `touch` before
writing any test, not by a crash:** the cluster-`0`-means-root
substitution from phase 3c (see that section above) applied to *every*
resolved path component, not just directories. That was harmless before
this milestone, because nothing this kernel had ever created could
legitimately have cluster `0` other than a `..` entry pointing at root -
but `touch` was about to make "cluster `0`, and it's a *file*, not
root" a real, common case. Without a fix, resolving a path to a freshly
`touch`ed empty file would have silently rewritten its cluster to
`self.root_cluster`, and `rm`-ing it would have tried to free *root's*
cluster in the FAT - corrupting the entire filesystem's root directory,
not just failing cleanly. Fixed by gating the substitution on
`current.is_dir`: every `..` entry is definitionally a directory, so the
original `cd ..` fix is unaffected, while a file's legitimate cluster-`0`
now passes through unmodified. This was caught during design, before any
test could have hit it - included here specifically so it isn't
mistaken for something "confirmed by testing" the way every other bug in
this file is; it wasn't, it was caught by tracing through what `find`
would do to a value it had never had to handle before.

**Two new syscalls, `fs_touch`/`fs_rm` (11/12)**, continuing past the
mkdir/rmdir pair with no gap - the gap-at-5 discipline only applies to
the one syscall (`shell_input`) that was actually removed, not to every
future addition. Both take just a path pointer/length, same shape as
`fs_mkdir`/`fs_rmdir`, and share the same `NO_FS`/`FS_ERROR` sentinel
split. `shell/src/main.rs` gained `cmd_touch`/`cmd_rm` following the
exact shape of `cmd_mkdir`/`cmd_rmdir`.

**Confirmed working end to end via the same piped-stdin QEMU technique
as every prior interactive milestone, against the real `esp.img`:**
`touch newfile` → `ls` (shows `NEWFILE`, 0 bytes) → `cat newfile` (empty,
as expected) → `touch newfile` again (silent no-op, not an error) →
`rm newfile` → `ls` (gone) → `rm newfile` again (correctly `no such
file`); `touch EFI` and `rm EFI` (an existing directory) both correctly
rejected rather than corrupting anything; `touch /nope/newfile` (missing
parent) correctly rejected; `touch`/`rm` inside a subdirectory
(`mkdir sub` → `touch sub/inner` → `cat sub/inner` → `rm sub/inner`)
round-tripped correctly, including confirming `rm` refuses a directory
(`rm sub` failed, `rmdir sub` succeeded once actually empty) - proving
`rm`/`rmdir` stay properly distinct rather than one silently accepting
the other's target. **Persistence confirmed by an actual reboot, not
just a live check**: `touch persist`, killed QEMU, booted a fresh
instance against the same `esp.img`, `persist` was still there via `ls`,
removed it, confirmed gone. Pre-existing files (`INIT.CFG`,
`BOOTAA64.EFI`, `SH.BIN`) re-read afterward and unchanged. `make run`
(FAT16, no mount) still degrades gracefully: `touch`/`rm` report the
shared "no filesystem mounted" message rather than crashing. Zero aborts
(`Data Abort`/`Prefetch Abort`/`Undefined Instruction`) in `-d int`
cross-checks across all four QEMU sessions (the main sequence, the
pre-existing-file check, the reboot-persistence check, and the FAT16
degradation check).

**Still coarse, worth knowing before building on this:** no way to write
actual content into a file - `touch` only ever produces zero-byte files,
and there's no `cp`/output redirection to change that; `rm`'s
per-failure detail collapses to the same one `FS_ERROR` sentinel every
other `fs_*` syscall does (can't tell "not found" from "is a directory"
without matching error text); the conservative 8.3 short-name character
set (from `mkdir`) applies here too, so some technically-valid names
still can't be created by this kernel.

### Phase 6: `write`, and a real bug in argument validation, not filesystem logic

Phase 5's own "still coarse" callout named the gap directly: `touch`
only ever produces zero-byte files, so every file this kernel could
create was permanently empty. This milestone closes that - `write`
gives a file real content for the first time.

**`Fs::write_file` reuses phase 4/5's write primitives rather than
inventing new ones.** A new `write_chain` helper allocates and writes a
fresh cluster chain for arbitrary data, linking clusters via the same
`write_fat_entry` `mkdir`/`touch` already use; a new
`patch_entry_cluster_size` helper rewrites just an existing entry's
cluster/size fields in place (leaving name/attribute/timestamps alone),
for the overwrite case. **Ordering is deliberate, same discipline as
`mkdir`'s claim-before-use and `rmdir`'s check-before-write:** the new
chain is allocated and written *before* anything about an existing file
is touched, so a failure partway through never frees or unlinks content
that was already safely on disk.

**The real bug this time was in argument validation, not filesystem
logic - and it was caught immediately by testing, the very first thing
tried.** `write hello.txt` with no words after the filename is a
legitimate case: truncate the file to empty, exactly matching `touch`'s
own empty-file semantics. It failed instead, with the generic "write:
failed" message. Root cause: `syscall.rs`'s `valid_user_range` sanity
check rejects any zero-length `(pointer, length)` pair - correct for
`fs_list_dir`/`fs_read_file`'s *output* buffers, where a zero-length
destination is pointless, but wrong for `fs_write_file`'s *input* data,
where empty is a real, meaningful value the caller might genuinely mean.
Fixed with a second check, `valid_user_range_allow_empty` (same bound,
but a zero length passes as long as the pointer is still non-null),
used only for the data argument - the path argument still can't be
empty, and still uses the original `valid_user_range`.

**Confirmed working end to end via the same piped-stdin QEMU technique
as every prior interactive milestone, against the real `esp.img`:**
`write hello.txt hello world ...` followed by `cat` shows the exact
content; overwriting with shorter content and `cat`-ing again shows
*only* the new, shorter content - no stale trailing bytes left over
from the longer original, confirming the old chain is actually freed
and the size field actually updated, not just the first few bytes
patched in place; the empty-write case specifically re-tested after the
fix and confirmed working; `write` to an existing directory and to a
path with a missing parent both correctly rejected. **Persistence
confirmed by an actual reboot, not just a live check:** wrote a file,
killed QEMU, booted a fresh instance against the same `esp.img`, `cat`
still showed the content. Pre-existing files re-read afterward and
unchanged. `make run` (FAT16, no mount) still degrades gracefully - the
shared "no filesystem mounted" message, not a crash. Zero aborts in
`-d int` cross-checks across every session (the main sequence, the
empty-write regression check, the reboot-persistence check, and the
FAT16 degradation check).

**Still coarse, worth knowing before building on this:** every write
fully replaces a file's content - no append, no partial/offset writes.
Content is bounded by whatever the caller can pass in one buffer; the
shell's own `write` command is additionally capped by its 128-byte
input line, so nothing it creates can ever span more than one FAT32
cluster on this test image - which means **phase 5's `rm` cluster-chain
-freeing loop is still logically implemented and reasoned through, but
still not exercised by an actual multi-cluster test**, since nothing in
this kernel can yet create a file large enough to need one. No output
redirection (`>`/`>>`) yet - needs shell-level parsing this syscall
alone doesn't provide (splitting `cmd > file` into a command and a
target) - see `docs/roadmap.md`'s parking lot. (`cp` turned out not to
need a new syscall at all - see "Phase 7" below.)

### Phase 7: `cp`, pure shell-side plumbing over existing syscalls

`cp <src> <dst>` needed no new syscall and no kernel changes at all -
the two primitives it composes, `fs_read_file` (already `cat`'s) and
`fs_write_file` (phase 6), were already there. `cmd_cp` reads `src`'s
entire content into a local stack buffer, then writes that buffer to
`dst` - creating it if it doesn't exist, replacing it if it does, same
semantics as `write`. The one thing worth being precise about: the read
completes in full before the write ever starts, so copying a file onto
itself is safe by construction, not something that needed a special
case.

**A genuinely different judgment call than `cat`'s, made deliberately:**
`cat` truncates a too-large file and prints a notice, since an
incomplete *display* is still useful. `cp` refuses outright instead -
an incomplete *copy* is a wrong copy, not a lesser one, so there's no
version of "truncate and warn" that's the right default here.

**Confirmed working end to end via the same piped-stdin QEMU technique
as every prior interactive milestone, against the real `esp.img`:** copy
to a new destination and to an existing one (overwrite), copy a file
onto itself (content unchanged, confirming the safe-by-construction
ordering), copy a real pre-existing file (`INIT.CFG`) to a new name,
copy into a subdirectory, and the usual error set (missing source,
destination is a directory, missing destination parent) all correctly
handled. `make run` (FAT16, no mount) still degrades gracefully. Zero
aborts in `-d int` cross-checks. No separate reboot-persistence check
this time - `cp` adds no new on-disk code path, only a new caller of
`fs_read_file`/`fs_write_file`, both already reboot-tested in their own
milestones.

**Still coarse:** copy size bounded by the shell's 256-byte read buffer
(same as `cat`'s); no recursive directory copy, no flags of any kind; no
`mv`.

### Phase 8: `mv`, and the last real correctness risk in the write-support arc

`mv <src> <dst>` closes out the "easy cheap-win commands" arc (phases
4-7) with the last genuine correctness risk in it, not just another
thin wrapper over existing primitives the way `cp` was.

**`Fs::mv` reuses the existing cluster chain rather than reading and
rewriting content** - locate `src`'s entry, insert a new entry for
`dst` with the same cluster/size/kind, free `src`'s old entry only once
the new one is safely linked in (same "write the new thing first"
ordering `write_file`'s overwrite path already established). `dst` must
not already exist; `mv` refuses rather than overwriting it or moving
`src` inside it if `dst` happens to already be a directory - narrower
than a real `mv`, deliberately.

**The real risk, caught by tracing through the design before writing
any test, the same way phase 5's `touch`/`Fs::find` bug was:** moving a
*directory* to a *different* parent means its own `..` entry now points
at the wrong place unless something fixes it - the identical
cluster-`0`-means-root convention documented in `find`'s doc comment
since phase 3c, this time on a write path instead of a read one.
Skipping this would have reintroduced the *exact* class of bug that
already cost real debugging time once (the original `cd ..` hang) - just
via `mv` instead of a bare `..` traversal. Fixed by patching the moved
directory's `..` entry (reusing `patch_entry_cluster_size` from phase 6,
not a new low-level sector patcher) to point at the new parent's
cluster, or `0` if the new parent is root.

**Confirmed working end to end, with the `..`-fixup specifically
exercised in QEMU, not just reasoned through on paper:** a same-directory
rename; a cross-directory move of a plain file; a cross-directory move
of a directory that itself contains a subdirectory, followed
*immediately* by `cd`-ing into the moved directory and back out with
`cd ..` - landing correctly at the *new* parent, confirmed via `pwd`,
not the old one; renaming a directory in place (same parent, so no
`..`-patch needed) and then confirming a previously-cross-parent-moved
nested subdirectory's own `..` still resolved correctly through *that*
rename too (its parent's cluster number didn't change, only the
parent's own name did); destination-already-exists, missing-source, and
missing-destination-parent all correctly rejected with the source
completely untouched (re-`cat`'d afterward to confirm). **Persistence
confirmed by an actual reboot**, same as every prior write milestone -
killed QEMU, booted fresh against the same `esp.img`, the entire moved/
renamed tree (including the twice-relocated nested subdirectory) still
resolved correctly. `make run` (FAT16, no mount) still degrades
gracefully. Zero aborts in `-d int` cross-checks across every session
(the main sequence, the rejection-case check, the reboot-persistence
check, and the FAT16 degradation check).

**Still coarse:** no move-into-an-existing-directory-keeping-basename
shortcut (real `mv`'s most common everyday case beyond a plain rename)
**(Update: done - shell-side, probing the destination with the same
`fs_list_dir` trick `cd` uses; `mv x x` is refused outright)**;
no cycle detection (nothing stops moving a directory into its own
descendant - the trivial self-move is guarded now, deeper cycles still
aren't); `dst`'s name is still bound by the same conservative 8.3
character set `mkdir`/`touch` already impose. This closes the write-
support arc phase 3 explicitly deferred (mkdir/rmdir/touch/rm/write/cp/
mv, phases 4-8) - what's left in `docs/roadmap.md`'s parking lot from
here is genuinely bigger work, not more cheap wins in this vein.

### virtio-console: a real, working transmit-only driver, confirmed on QEMU - and confirmed *not applicable to Parallels*, with real evidence

The real lead flagged (and deliberately deferred) in the "Console
discovery" section above, finally built: `kernel/src/virtio_console.rs`,
a fourth console-discovery mechanism reusing `virtio_mmio.rs`'s existing
transport - the same one `virtio_blk.rs` already proved out for disk
I/O - rather than a new one. Modeled directly on `virtio_blk.rs`'s
discover/init/single-virtqueue/poll-based-completion shape, since the
two devices' basic operation is structurally identical (feature
negotiation, one virtqueue, synchronous completion) even though what
flows through the queue is completely different (one variable-length
message descriptor instead of a fixed 3-part block request).

**A real placement constraint discovered while building this, not
assumed going in: this can't run at the same point in boot as
devicetree/ACPI/PCI do.** Those three install their console immediately
after `exit_boot_services`, before `mmu::install_identity_map` - fine
for them, since none of their discovery touches raw MMIO at all
(devicetree/ACPI parse boot-services-supplied pointers; PCI enumeration
is entirely boot-services-based). `virtio_mmio::find_device`'s scan is
raw MMIO reads, and its own safety contract requires the low-1GB device
region mapped under *this kernel's own* translation tables - true only
after `mmu::install_identity_map` runs (the same reason
`virtio_blk::Device::discover` in `init_storage` already only ever runs
at that point, not earlier). `try_virtio_console` in `main.rs` therefore
runs right after the identity-map confirmation line, not alongside the
other three - a real, accepted consequence: every boot message between
`exit_boot_services` and there is silently lost on a virtio-console-only
platform, since there's nothing to print through yet. Untested whether
firmware's own pre-MMU-swap tables would have covered this region too
(which would have allowed the earlier placement) - not assumed, just
left as a real open question for later, per this project's usual
discipline of confirming rather than guessing.

**Verified end to end on QEMU, not just "it compiles" - the whole
chain confirmed working, from discovery through a real byte reaching
the host.** QEMU's default `virt`+OVMF combination always resolves a
console via ACPI/SPCR before virtio-console ever gets a chance to run,
so proving this driver actually works needed the same kind of temporary,
documented force this project has used before (see `exceptions.rs`'s
original verification): `main.rs`'s `if !found_console_early` was
temporarily changed to `if true`, a `virtio-serial-device`/`virtconsole`
device pair was attached via a new `make run-virtio-console` target
(writing to a separate chardev file, `vcon.log`), and the kernel was
booted normally. Confirmed, precisely:

- Discovery found the device, version 2 (modern interface) as expected.
- Feature negotiation succeeded (`VIRTIO_F_VERSION_1` offered and
  accepted, `FEATURES_OK` came back set).
- Transmitq0 (queue 1) setup succeeded and `DRIVER_OK` was reached.
- `console::install`'s own confirmation line
  ("`virtio-console live (fallback...)`") and every subsequent kernel
  boot message (`virtio-blk ready`, `FAT32 mount failed`, `shell ready`,
  the userland shell's own banner and prompt) all appeared in `vcon.log`
  - meaning `write_str`'s batched, chunked sends and `write_byte`'s
  single-byte sends (the userland shell's prompt `$ ` specifically) both
  really round-tripped through the transmit virtqueue to the host,
  not just queued locally. `xxd` on the raw bytes confirmed genuine
  `0d 0a` (`\r\n`) sequences, not just terminal rendering that happened
  to look right.
- The **normal**, unforced boot path was separately re-confirmed
  afterward (temporary force reverted) with the same virtio-console
  device still attached but idle: ACPI/SPCR still won as before,
  the interactive shell still worked normally (typed `help`/`echo`
  round-tripped correctly), and `run-image`'s real-FAT32 boot was
  unaffected too - the new device sitting unused doesn't perturb the
  existing, working path.
- Zero aborts (`Data Abort`/`Prefetch Abort`/`Undefined Instruction`) in
  `-d int` cross-checks across every session above (the forced test, the
  normal-boot regression check, and the `run-image` regression check).

**A genuine surprise along the way, worth recording so it isn't
re-discovered:** the first attempt to organically force *all three*
existing mechanisms to fail (rather than editing source) was
`-machine virt,acpi=off`, on the theory that no ACPI tables would mean
no SPCR, no RSDP-derived console. Instead, this OVMF build apparently
switches to advertising a devicetree blob when ACPI is disabled -
devicetree discovery, "confirmed dead on both platforms" under every
*previous* test in this project (always run with the default `acpi=on`),
*succeeded* under this specific configuration. A real, useful data point
about this firmware build's behavior, not a path to what was actually
needed here - hence the source-level force instead.

**Still coarse, worth knowing before building on this - transmit-only is
the headline limitation, not an oversight:** no receive/input path at
all (`Console::Virtio`'s `read_byte` always returns `None`) - a
Parallels boot using this fallback would let you *see* the shell, not
type into it, until a receive virtqueue is added (symmetrically simpler
than transmit: post device-writable buffers to receiveq0/queue 0,
deliberately left unconfigured this phase, and poll the used ring for
arrivals). No `VIRTIO_CONSOLE_F_SIZE`/`F_MULTIPORT`/`F_EMERG_WRITE`
negotiated - none needed for one fixed-size default port. `write_byte`
(character echo, if RX existed) would pay a full virtqueue round trip
per keystroke, unlike `write_str`'s batched sends - real overhead
compared to a raw UART's direct MMIO write, accepted for a first cut.

**Parallels itself: tested on real hardware by the user, and the answer
is a confirmed no - not "still unconfirmed" anymore, a real negative
result with real evidence behind it.** Booting the actual `esp.hdd` on
real Parallels-on-Apple-Silicon hardware (not this environment, which
has no way to do that itself) showed all three of devicetree/ACPI/PCI
16550 failing exactly as already documented, and - critically - showed
no visible output at all afterward, from `virtio-console` or anything
else. That alone couldn't distinguish "the driver ran and found nothing"
from "the driver ran and hung": the UEFI graphics console visible in the
boot screenshot only ever renders during boot services, so anything that
happens after `exit_boot_services` - working or not - is invisible on
that same screen regardless. Getting past that ambiguity took one more
diagnostic round-trip.

**First, the user's own separately reported "Serial Port" hardware
device (configured in Parallels, output redirected to a file) turned
out to receive *nothing at all*, not even boot-firmware noise - unlike
QEMU, where EDK2's own pre-exit debug output gets opportunistically
mirrored onto any attached virtio-serial chardev regardless of what our
own kernel does. That absence was itself inconclusive (this specific
Parallels firmware build might just not do that mirroring trick), so it
ruled nothing in or out on its own.**

**What actually settled it: a new, permanent diagnostic,
`pci::log_all_devices` (`kernel/src/pci.rs`), reusing the same
boot-services `PciRootBridgeIo` walk `discover_uart16550` already proved
safe on this exact hardware** (that function reaching
`DiscoveryError::NoSerialDevice` there, rather than `NoRootBridge`, was
already proof PCI enumeration itself works on Parallels - this just logs
every device found instead of filtering for one class). Wired into
`main.rs` to run automatically, logged through the still-working
pre-exit UEFI console, whenever all three normal mechanisms fail - cheap,
safe (read-only PCI config-space reads, no writes), and adds zero noise
to the normal QEMU boot path where ACPI already succeeds.

**The real Parallels PCI device inventory this produced, decoded:**

| Vendor | Device | Class:Subclass | What it is |
|---|---|---|---|
| `0x8086` (Intel) | `0x293e` | `0x04:0x03` | HD Audio controller |
| `0x8086` (Intel) | `0x265c` | `0x0c:0x03` | USB2 EHCI controller |
| `0x1033` (NEC) | `0x0194` | `0x0c:0x03` | USB3 xHCI controller |
| `0x1af4` (**virtio**) | `0x1000` | `0x02:0x00` | **virtio-net** - not console |
| `0x1ab8` (**Parallels**) | `0x4000` | `0xff:0x00` | Parallels' own proprietary device |

virtio's real vendor ID (`0x1af4`) genuinely does appear on the bus -
but only for networking (device ID `0x1000`). A virtio-console device
would show up as device ID `0x1003` (legacy/transitional) or `0x1043`
(modern-only, per the virtio spec's PCI device ID scheme) - neither is
present. **This rules out virtio-console over PCI, on top of the
already-empty result from virtio-console over MMIO** (no direct
evidence virtio-mmio *specifically* failed on this exact boot - nothing
can print post-exit either way - but the complete absence of any PCI
virtio-console device, combined with Parallels choosing not to expose
one via the transport this driver already scans, makes an MMIO-only
virtio-console existing somewhere else a real stretch, not a live
possibility worth chasing further without new evidence).

**The actual conclusion: Parallels' serial port almost certainly isn't
virtio-console at all - it's something proprietary.** The one
unexplained device left, vendor `0x1ab8` (Parallels' own registered PCI
vendor ID) device `0x4000`, class `0xff` ("vendor-specific,
unclassified" - not a standard PCI device class at all), is the
strongest remaining candidate for what the configured Serial Port
device actually *is* on the guest side. There's no public specification
for it the way there is for virtio - reaching it would mean blind
register/BAR probing against an undocumented protocol, not implementing
a known spec the way this entire driver was built. **Explicitly decided
not to pursue that here** - a real, open-ended reverse-engineering
task with no guaranteed payoff, categorically different in kind from
"implement a documented protocol," and a call the user made deliberately
rather than one assumed. Recorded as a confirmed dead end with real
evidence behind it, the same treatment the original three UART
mechanisms got - not a soft "still unconfirmed" the way it read before
this exchange.

**A second, smaller finding from the same testing round: the earlier
"opens as a folder" `esp.hdd` attachment concern (see "Parallels disk
attachment" below) was never a real problem.** The user's Parallels
version simply labels the file-open dialog's action button "Open"
rather than "Choose" - once that was clear, attaching and booting
worked correctly, confirmed by the same boot log showing
`loaded shell program` after successfully reading `INIT.CFG`/the shell
binary off the real `esp.hdd`/`esp.dmg` on genuine Parallels hardware.

### GOP framebuffer console: a fifth, better-grounded lead - built and confirmed on QEMU, real-hardware confirmation still pending

With virtio-console confirmed dead on Parallels (above), the user asked
for deeper research into how other OSes get a console on Parallels ARM64
at all, given that Linux is known to run there. A forked research
subagent's findings were independently re-verified rather than trusted
outright - one specific claim (a PCI device ID for Parallels'
proprietary "ToolGate" mechanism) turned out to be unsourced by its own
citation and was discarded, but a second held up: a real FreeBSD forum
thread with a boot log from that exact platform
(https://forums.freebsd.org/threads/parallels-on-macos-apple-silicon-freebsd-14-stuck-on-virtio_gpu.96762/)
shows `VT: Replacing driver 'efifb' with new 'virtio_gpu'` - direct
evidence that a generic UEFI GOP framebuffer drives early console output
on Parallels ARM64, before any GPU-specific driver even loads. That
thread's screenshots of Parallels' own VM settings also show only two
"video type" options (a proprietary Parallels GPU or VirtIO GPU) and no
serial option at all - consistent with every dead end already confirmed
above, and consistent with this project's own Parallels boot
screenshots, which already show UEFI graphics output working right up
until boot services exit.

**Unlike every previous console mechanism here, GOP needs no address
guessing or platform convention at all** - `EFI_GRAPHICS_OUTPUT_PROTOCOL`
is a standard, fully-specified UEFI protocol, queried identically on any
platform that implements it. New module `kernel/src/framebuffer.rs`
queries it during boot services (`uefi::boot::open_protocol_exclusive`,
the same access pattern `pci.rs` already established) for the current
mode's resolution, stride, and pixel format, and the framebuffer's
physical base address and size. Only `PixelFormat::Rgb`/`Bgr` (4
bytes/pixel, a fixed, known byte layout) are accepted -
`Bitmask`/`BltOnly` are rejected outright, `BltOnly` specifically because
it has no direct-memory-access path at all, only boot-services-only
`Blt()` calls that can't survive `exit_boot_services`.

**A real bitmap font had to come from somewhere, and got the same
transcription-safety treatment virtio-console's register-bit values
got.** `kernel/src/font.rs` embeds `dhepper/font8x8`'s public-domain 8x8
font (itself based on Marcel Sondaar/IBM's public-domain VGA fonts),
downloaded verbatim via `curl` rather than through a summarizing fetch -
deliberately, since hex glyph-data transcription errors are exactly the
kind of mistake that wouldn't show up until a specific character
rendered wrong on screen, hard to notice and harder to trace back. A
small Python script sliced the 95 printable-ASCII glyphs (0x20-0x7E) out
of the downloaded header and generated the Rust `const` array
mechanically, not by hand.

**New module `kernel/src/fbconsole.rs`** implements `core::fmt::Write`
directly over the raw framebuffer pointer: draws glyphs on a fixed
character-cell grid (`width`/`height` divided by 8), tracks a cursor,
and - deliberately no text buffer anywhere, matching this kernel's
zero-heap, no-persistent-buffer discipline for anything that isn't
already a small fixed-size global - scrolls by `core::ptr::copy`-ing
pixel rows within the framebuffer itself and blanking the newly-exposed
bottom row. No colour, no ANSI escape parsing (the shell's `clear`
command's escape sequence just draws whatever blank glyph `font.rs`
returns for a non-printable byte, rather than actually clearing the
screen) - a real, documented limitation, not an oversight. Write-only:
`read_byte` always returns `None`, for a different reason than
`virtio_console.rs`'s same limitation - there is no keyboard driver of
any kind in this kernel yet, independent of this console entirely.

**`console.rs` gained a `Framebuffer` variant and an `is_installed()`
accessor**, so `main.rs` can gate this as a genuine last resort: tried
only after devicetree/ACPI/PCI *and* virtio-console have all failed,
since a real byte-stream console (which also gets input) is strictly
more capable than a write-only text grid whenever one actually exists.
GOP discovery itself still has to run unconditionally before
`exit_boot_services`, regardless of whether a byte-stream console was
already found - it's boot-services-only, same constraint devicetree/ACPI
parsing and PCI enumeration already have.

**`mmu.rs::install_identity_map` gained an optional `framebuffer`
argument, generalizing the existing block-mapping abstraction rather
than adding a separate one.** Confirmed via the identity-map's own log
line: on QEMU's `ramfb` device, the framebuffer's physical address
(`0x5c7a0000`) already falls inside the discovered RAM span
(`0x40000000`-`0x60000000`), so the ordinary RAM loop covers it for free
- no new mapping needed, and no separate log line fires for that case.
Only if a framebuffer's containing 1GB block is *still* unmapped after
that loop does this add one more Device-nGnRnE block for it, at whatever
address the framebuffer actually reports (the same convention the fixed
low-1GB device block already uses, just no longer hardcoded to block
0) - a real, reasoned-through possibility for hardware where the
framebuffer lives outside normal RAM, but **not yet exercised against
anything but QEMU's RAM-backed `ramfb`**, so this fallback path's
correctness (particularly the `ptr::copy` scroll doing a bulk memmove
against Device-nGnRnE memory, which has stricter access-ordering rules
than Normal memory) is reasoned about, not confirmed.

**QEMU testing needed a real display device, and this project's existing
`-nographic` dev loop doesn't provide one at all** - confirmed by direct
testing (`framebuffer::discover()` returns `NoGop`, not an address).
`-device ramfb -display none -serial file:...` (dropping `-nographic`
entirely, since it conflicts with an explicit `-serial`) gives QEMU's
purpose-built headless framebuffer device - RAM-backed, direct-access,
confirmed `800x600`/`Bgr`/stride `800`. `-device virtio-gpu-pci -display
none` was also tried, specifically to see `BltOnly` actually get
rejected rather than assume it would - confirmed: `discover()` correctly
returned `UnsupportedPixelFormat(BltOnly)`, no crash. **A genuinely new
verification technique for this project**, since every prior console
driver could be checked through its own text output alone: QEMU's QMP
`screendump` command (sent over a Unix-socket QMP connection, not the
serial console at all) captures the actual rendered framebuffer contents
to a `.ppm` image, which can then be visually inspected directly -
necessary here because there's no other way to confirm pixel-level
rendering actually happened correctly, as opposed to just "didn't
crash."

**Confirmed working, not just "produced no fault," via two separate
screendumps, each with a temporary override reverted afterward - the
same "temporarily force" technique already established for testing
virtio-console** (`if !console::is_installed()` forced to `if true`,
since QEMU's own ACPI console would otherwise always win first and the
framebuffer fallback would never get a chance to run):
- **First screendump**: correctly rendered kernel boot messages
  (`framebuffer console live`, `virtio-blk ready`, the FAT32-mount
  failure message expected on `make run`'s non-FAT32 vvfat disk, `shell
  ready`) followed by the loaded shell program's own startup banner
  (`Ouroboros userland shell`) and prompt - proving both
  `console::println!` (kernel-side) and the shell's userland `putc`
  syscall (a completely different code path, going through the syscall
  boundary) both correctly reach the framebuffer, not just kernel text.
- **Second screendump**, with a temporary 90-line print loop added
  specifically to overflow one screen (a `75`-row grid at this
  resolution) and force the scroll path: showed a clean, correctly
  ordered, non-corrupted scrolled view (lines 21 through 88, plus the
  shell's own banner/prompt still intact below them) - confirming the
  `ptr::copy` memmove-based scroll works correctly on real framebuffer
  memory, not just in isolation.
- Both runs cross-checked against QEMU's own `-d int` trace: zero
  aborts.
- **A full regression pass on the normal QEMU dev-loop config**
  (`-nographic`, real ACPI/PL011 console, piped-stdin `help`/`uptime`)
  confirmed no behavior change when a byte-stream console already
  exists: GOP is still discovered and logged (`GOP framebuffer @
  0x5c7a0000...`), but the framebuffer console never installs, and the
  shell's interactive echo/command handling is byte-for-byte the same as
  before this milestone. Zero aborts.

**Still coarse, and real-hardware confirmation is still pending, worth
knowing before treating this as "done":** no colour, no ANSI escape
parsing, no keyboard input (independent gap, not something this
milestone could fix on its own), and the MMU's device-block fallback
path for a framebuffer outside the discovered RAM span has never been
tested against anything but QEMU's RAM-backed `ramfb` - real Parallels
hardware might exercise a genuinely different path through that code
than anything verified so far. **This environment cannot boot Parallels
directly** - confirming this actually renders text on a real Parallels
screen after boot services exit (not just during boot, which already
worked before this milestone) is the next real step, and is on the user
to test with the current `esp.hdd`.

### GOP framebuffer console, take two: `open_protocol_exclusive` was silently killing the boot console on real Parallels hardware

The user tested the framebuffer console (above) on real Parallels
hardware and reported it still didn't work - a screenshot showed
devicetree/ACPI/PCI discovery and the `pci::log_all_devices` dump
rendering exactly as before, then **nothing**: no GOP discovery log
line, no `loaded shell program` line, nothing - even though
`framebuffer::discover()`'s very next statement in `main.rs`
unconditionally logs either success or failure. Not a hang and not a
crash: a self-inflicted console death.

**Root cause, found by reading the `uefi` crate's own doc comment for
`open_protocol_exclusive` rather than guessing:** exclusive-mode opens
are specified to forcibly disconnect any driver holding the protocol
`ByDriver` - the doc comment's own example is "opening the
SERIAL_IO_PROTOCOL exclusively will disconnect the console driver from
it." `framebuffer::discover()` opened `GraphicsOutput` with exactly that
attribute. On real Parallels hardware, firmware's own text console (the
same one every boot screenshot in this project shows rendering) holds
GOP `ByDriver` to draw it - so the instant `discover()` ran, it silently
disconnected the console from the screen, before a single further log
line could print. **QEMU's `ramfb` test never caught this** because that
test always ran with `-display none`: no console driver was ever
attached to GOP in the first place, so there was nothing to disconnect -
the exact same shape of gap that let the original virtio-console driver
look confirmed-on-QEMU while being wrong for Parallels' actual transport.

**Fix: switch to `OpenProtocolAttributes::GetProtocol`** (via the
`unsafe` generic `uefi::boot::open_protocol`, since the convenience
`open_protocol_exclusive` wrapper only ever offers the exclusive
attribute) - a read-only, non-owning open that doesn't touch driver
ownership at all. Safe for this module's actual usage: `discover()`
reads `ModeInfo`/`FrameBuffer` exactly once, synchronously, and never
touches the `GraphicsOutput` object again after returning (the raw
framebuffer pointer is what crosses into post-exit code, not the
protocol object itself - see `fbconsole.rs`'s safety note).

**Re-verified on QEMU after the fix, not just "compiles":** the full
`ramfb`/screendump verification from the original milestone was re-run
end to end - GOP discovery still succeeds identically
(`GOP framebuffer @ 0x5c7a0000...`), the forced-fallback screendump
still shows boot messages and the shell's own banner/prompt rendering
correctly, and a full regression pass on the normal `-nographic`
ACPI-console dev loop (`help`/`uptime` via piped stdin) still works
byte-for-byte as before. Zero aborts in `-d int` cross-checks on every
run. **Still not re-confirmed on real Parallels hardware** - that
remains on the user, same as before; this fix is reasoned from the
`uefi` crate's documented `open_protocol_exclusive` behavior plus the
exact symptom shown in the reported screenshot, not from a second round
of real-hardware testing.

### GOP framebuffer console, take three: the mapping and display update both work - the freeze was `try_virtio_console`'s unconfirmed MMIO-scan assumption

With the `open_protocol_exclusive` fix in place (above), the user tested
again on real Parallels hardware. Progress, but still not working: GOP
discovery now succeeds (`GOP framebuffer @ 0x20000000, size=0x300000,
1024x768, stride=1024, format=Bgr`) and boot reaches `loaded shell
program` - further than before - but the screen still froze there, with
nothing rendered afterward.

**Disambiguated with a minimal, unambiguous probe: a temporary raw
full-framebuffer fill (`0xff` to every byte), placed immediately after
the MMU switch and bypassing `fbconsole.rs`/`font.rs` entirely.** The
question this was built to answer: does a direct write to this exact
physical address, mapped by this exact identity map, actually reach the
display at all - given that unlike QEMU's `ramfb` (a device
purpose-built to be a dumb, always-scanned-out memory region), a real
GPU might need an explicit flush/present step that only firmware's own
driver knows how to issue, which would make every future fbconsole fix
invisible regardless of correctness. Verified visible on QEMU's `ramfb`
first (solid white via QMP screendump) to confirm the technique itself
before spending a real-hardware round trip on it.

**Result: the user saw a solid white screen** - confirming both the MMU
mapping and direct-write display visibility work correctly on real
Parallels hardware, with no explicit flush needed. This is the load-
bearing assumption the entire framebuffer-console approach depends on,
and it holds. But the screen **stayed** solid white, frozen, exactly
like the previous failure - meaning execution never reached the actual
`FbConsole`-based rendering at all. Confirmed by asking the user
directly (not assumed): the white screen never changed further, no
black-background text ever appeared.

**Root cause: `try_virtio_console()` ran immediately after the fill, and
its own scan carries an unconfirmed assumption.** `virtio_mmio.rs`'s
`find_device` reads a magic-value register at 32 fixed, QEMU-specific
addresses, with a comment stating "real hardware reads as 0, not the
magic value" - asserted, never actually confirmed the way this
project's other platform conventions have been (each cross-checked via
a devicetree dump or a monitor peek before being trusted). A real bus
fabric commonly raises an external abort for a read to genuinely
unbacked device-memory space, unlike QEMU's lenient software-emulated
bus, which just returns a fixed pattern as a courtesy. Since no console
exists yet at that point on Parallels (devicetree/ACPI/PCI have all
already failed), a fault there is completely silent - indistinguishable
from "the scan just found nothing," which is exactly the ambiguity that
already made the *original* virtio-console real-hardware test
inconclusive, before this whole framebuffer-console effort even started.
Independent supporting evidence, not just plausibility: `pci.rs`'s
device inventory (see "virtio-console" above) already proved no
virtio-console device exists on Parallels over PCI at all - so this scan
was never going to find anything real there regardless of whether it's
also unsafe to run.

**Fix: reordered the console fallback chain so the framebuffer console
is tried before `try_virtio_console`, not after.** The original ordering
was deliberately byte-stream-console-first ("more capable whenever one
exists") - reasonable in the abstract, but wrong now that virtio-console
is confirmed both nonexistent and possibly actively unsafe to probe for
on this specific, real platform, while the framebuffer console's core
mechanism is now hardware-validated. `main.rs`'s two fallback blocks
were swapped, and `try_virtio_console`'s gate changed from
`!found_console_early` to `!console::is_installed()` (the `found_console_early`
binding was removed, now unused) - so it only runs if the framebuffer
console *also* failed to install, not unconditionally whenever
devicetree/ACPI/PCI failed. Zero behavior change on any platform where a
byte-stream console already exists (QEMU's normal dev loop: ACPI always
wins, so neither fallback ever runs, confirmed by a full regression
pass) or where GOP doesn't exist either (both fallbacks still get their
chance, same as before). A real side benefit beyond just avoiding the
crash: if virtio-mmio scanning does fault on some future platform, there
will finally be a console installed to report the exception through,
instead of a silent freeze like this one.

**Re-verified on QEMU after the reorder:** the forced-fallback `ramfb`
screendump test still renders identically, and the normal `-nographic`
ACPI-console regression pass (`help`/`uptime` via piped stdin) is
byte-for-byte unchanged. Zero aborts in `-d int` cross-checks on both.
**Still not re-confirmed on real Parallels hardware** - this reorder is
reasoned from the fill diagnostic's result plus the `virtio_mmio.rs`
comment's unconfirmed assumption, not from a third round of
real-hardware testing yet.

### GOP framebuffer console, take four: it works - real text, rendered live on Parallels, and it directly confirmed the `virtio_mmio` crash

With the reorder from "take three" in place, the user tested a third
time. **Success, and a first for this project: readable kernel text,
rendered live on real Parallels hardware, after `exit_boot_services`.**
The screenshot showed two real lines - `framebuffer console live
(fallback - every byte-stream mechanism failed)`, then a second line the
exception handler produced on its own. Both genuinely rendered through
`fbconsole.rs`'s font/glyph/cursor logic, not just the raw fill from
"take three" - the reorder fix worked exactly as designed.

**The second line was a real exception report, and decoding it turned
the previous section's suspicion into direct proof.** `EXCEPTION
vector=4 esr_el1=0x96000010 far_el1=0xa000000 elr_el1=0xbba94c68`:
`ESR_EL1` bits 31:26 (EC) = `0x25` (Data Abort, same exception level -
EL1, matching vector 4's "Synchronous, Current EL, SPx" slot exactly);
bits 5:0 (DFSC) = `0x10` (Synchronous External abort, not a
translation-table-walk fault - a real bus fault, not a permission or
mapping bug, ruling out an `mmu.rs` mistake). `FAR_EL1 = 0xa000000`
matches `virtio_mmio::SLOT_BASE` exactly - the very first read, of the
very first scan slot. Not "very likely" anymore: this is a directly
observed, fully decoded real bus fault at the exact address the
previous section's reasoning pointed at.

**But the freeze this time was worse than "take three"'s, in a way the
reorder alone didn't fix: this was the last thing ever printed, meaning
the whole boot halted right there** - the interactive shell never
started. Tracing why: `init_storage()` runs unconditionally right after
the console fallback chain (regardless of which console, if any,
installed), and `virtio_blk::Device::discover()` goes through the exact
same `virtio_mmio::find_device` scan that just crashed for
virtio-console - so even with the console-fallback reorder working
correctly, the *disk* driver's own use of the identical unsafe scan was
always going to hit the same wall moments later. The reorder fixed the
console race but not the underlying problem: this scan is unsafe for
*any* caller on this hardware.

**Fix: a new `virtio_mmio_probe_safe` flag, computed once in `main`
right after `discover_console` returns, gating every caller of
`virtio_mmio::find_device` - both `try_virtio_console` and
`init_storage`'s virtio-blk discovery - not just reordering them.**
`true` only when a byte-stream console was actually found via
devicetree/ACPI/PCI - the one platform shape (QEMU) this scan has ever
been confirmed safe on. This is a heuristic, documented as one, not a
proof (a platform could in principle lack an early console yet still
safely support virtio-mmio) - but it's grounded in the one real data
point available, and it directly prevents a repeat of this exact,
now-decoded crash. The deeper, principled fix would be a resumable EL1
synchronous-fault path so a probe read could fail soft instead of
halting the kernel - a real gap (`exceptions.rs` only supports IRQ and
the EL0-SVC path as resumable; every other synchronous fault is a
diverging report-and-halt) - but that's meaningfully bigger work than
this specific crash needed to unblock Parallels, so it's recorded as a
real future improvement (see "Next milestone" below) rather than done
here.

`virtio_mmio.rs`'s module doc comment and its inline "unpopulated slot"
comment were both corrected - the old claim that real hardware "reads
as 0" was never actually confirmed, and is now confirmed *wrong*, not
just unconfirmed.

**Re-verified on QEMU after the gate, including the actual disk path,
not just the console:** the normal `-nographic` ACPI-console regression
pass (`help`/`uptime`) is unchanged, `virtio_mmio_probe_safe` correctly
comes back `true` there so virtio-blk still initializes exactly as
before - confirmed against *both* `make run`'s FAT16 vvfat (graceful
"no FAT32 partition" message, matching every prior session) *and* a real
FAT32 `esp.img` (`FAT32 mounted, disk commands available`, then real
`ls`/`cat` output matching actual disk content) - not just the console
half of the fix. A third test, forcing `discovery` to `None` while still
using `ramfb` (simulating the actual Parallels-shaped "no early console,
GOP present" case end to end), showed the complete intended sequence:
`framebuffer console live` → a clean `skipping virtio-blk
(unconfirmed-safe virtio-mmio scan on this platform)` message → `shell
ready` → the userland shell's own banner and prompt, all rendered
correctly - no crash, no fault, a real usable shell. Zero aborts across
all three `-d int` cross-checks. **Still not re-confirmed on real
Parallels hardware** - the previous two "fixed, needs a real-hardware
recheck" cycles make that the obvious next step again, but this is the
first version where reasoning through the evidence gives real confidence
it'll actually reach a working shell, not just a further diagnostic.

### GOP framebuffer console, take five: the GICD crashes too - the whole low-1GB QEMU device convention is unsafe on Parallels, not just virtio-mmio

The user tested a fourth time with the `virtio_mmio_probe_safe` gate in
place. Real progress again - `skipping virtio-blk` printed cleanly,
proving that fix worked exactly as designed - but the boot halted again
with a second exception, decoded the same way as "take four":
`EXCEPTION vector=4 esr_el1=0x96000050 far_el1=0x8000000
elr_el1=0xbba95010`. Same EC (`0x25`, Data Abort at EL1) and same DFSC
(`0x10`, Synchronous External Abort - a real bus fault), but a
different `FAR_EL1` this time: `0x8000000`, which is exactly
`gic.rs`'s `GICD_BASE` - specifically `gic::init()`'s very first write
(`GICD_CTLR`, offset 0).

**This is a structural finding, not just a second instance of the same
bug: `gic.rs`'s addresses are the identical kind of thing as
`virtio_mmio.rs`'s** - a fixed, QEMU-shaped convention (`0x08000000`
distributor / `0x08010000` CPU interface, GICv2), confirmed only by
dumping *QEMU's own* internal devicetree, never discovered on
Parallels. Real Parallels hardware (Apple Silicon virtualization)
almost certainly doesn't expose a GICv2 distributor at that address at
all - it may not even be GICv2 there. The pattern across "take four"
and "take five" together: *nothing* in this project's fixed low-1GB
device-region convention has ever been confirmed safe on Parallels,
only on QEMU - virtio-mmio and the GIC are just the first two things
that happened to get touched and crash.

**One real exception: `timer.rs` is architecturally safe regardless.**
Unlike `virtio_mmio.rs`/`gic.rs`, the ARM generic timer is accessed
purely through system registers (`mrs`/`msr` on `cntfrq_el0`,
`cntp_tval_el0`, `cntp_ctl_el0`) - no memory-mapped I/O, no
platform-specific address, safe on any ARMv8 CPU by construction. Only
the *GIC* (needed to actually forward the timer's interrupt to the CPU)
depends on the unconfirmed address.

**Fix: broadened the same gate rather than inventing a second one.**
The flag from "take four" was renamed `qemu_device_region_safe`
(from `virtio_mmio_probe_safe`, since it's no longer just about
virtio-mmio) and now also gates `gic::init()`/`gic::enable_interrupt()`
- skipped together as one block, with `timer::arm()` skipped alongside
them since arming a timer whose interrupt will never be forwarded
anywhere is just dead work, not because `timer::arm()` itself is
unsafe. **Checked this doesn't break EL0 entry before shipping it, not
assumed:** `tasks::start()` does a straight `eret` into task 0's saved
context with no dependency on the GIC or timer having run at all,
so skipping this block entirely still reaches a working interactive
shell - just without preemption (no tick ever fires, so `tasks.rs`'s
round-robin never switches away from task 0, which is fine for a
single always-runnable interactive task; `uptime` would report a
static, never-increasing count in this mode, a real minor limitation
worth knowing about, not a bug).

**Re-verified on QEMU across the same three scenarios as "take four,"
now including a check that ticks genuinely still increase:** the normal
ACPI-console regression pass showed `uptime` reporting 88 ticks, then
214 ticks a few seconds later - proof the GIC/timer path still
initializes and fires normally when `qemu_device_region_safe` is true,
not just "didn't crash." The forced `discovery=None` + `ramfb` test
(the actual Parallels shape) showed the complete intended sequence:
`framebuffer console live` → `skipping virtio-blk` → `skipping
GIC/timer init` → `shell ready` → the userland shell's own banner and
prompt - a real, working shell reached end to end, no crash, no fault.
Zero aborts across both. **Still not re-confirmed on real Parallels
hardware** - three "fixed, needs a recheck" cycles in, but each one has
gotten further than the last, and this is the first version reasoned
through to actually reach a working shell rather than another
diagnostic.

### GOP framebuffer console, take six: confirmed - a real, working shell prompt on real Parallels hardware, first time ever

The user tested a fifth time with the broadened `qemu_device_region_safe`
gate in place. **Success - the complete predicted sequence, exactly as
reasoned through in "take five," reached on real hardware:**
`framebuffer console live` → `skipping virtio-blk (unconfirmed-safe
virtio-mmio scan on this platform) - disk commands won't work this
boot` → `skipping GIC/timer init (unconfirmed-safe device region on
this platform) - no preemption this boot` → `shell ready - type and
press Enter` → the loaded userland shell's own banner
(`Ouroboros userland shell`) and a live `$` prompt. No crash, no
exception, nothing further needed.

This is the first time this project has ever reached a running,
prompt-displaying shell on real Parallels hardware - the actual goal
the whole "get a Parallels console" effort (virtio-console, then the
GOP framebuffer console, across five hardware round trips) was for.
Four real bugs found and fixed along the way, each confirmed by direct
hardware evidence rather than guessed: `open_protocol_exclusive`
disconnecting the boot console, `try_virtio_console`'s scan freezing
the boot with nothing to report through, a decoded Synchronous External
Abort proving `virtio_mmio::find_device` crashes outright, and a second
decoded instance of the same fault proving the entire low-1GB
QEMU-shaped device-region convention (not just virtio-mmio) is unsafe
here.

**What's confirmed working now, on real Parallels hardware, not just
QEMU:** GOP discovery and mapping; direct framebuffer writes reaching
the display with no explicit flush; the exception handler reporting
through that display; `fbconsole.rs`'s actual font/glyph/cursor
rendering (not just a raw fill); the MMU identity map surviving real
hardware's own memory layout; and `tasks::start()`'s EL0 entry reaching
and running the loaded shell program.

**What's not confirmed yet, because it's a separate, already-known,
already-documented gap: keyboard input.** The framebuffer console is
write-only by design (`fbconsole.rs`'s `read_byte` always returns
`None`) - this kernel has no keyboard driver of any kind, independent
of everything fixed this session. The prompt on screen is real and the
shell is genuinely running, but nothing typed at a physical keyboard
currently reaches it. This was already flagged as a known limitation
before any of this round's hardware testing began, not a new surprise -
and the user directly confirmed it on the same real hardware: typing at
the keyboard produced no visible change on screen, exactly as the
design predicts, not a crash or a different symptom - see "Next
milestone" below for what a real input path would need (USB HID over
UEFI, most likely).

### Parallels disk attachment: a real trap, not a hunch

Getting `esp.img` to actually boot in Parallels took a few wrong turns worth
recording so they don't get re-discovered:

- Parallels' Hard Disk device rejects a raw `.img` outright — it only takes
  its own `.hdd` container format.
- Attaching the raw image to the **CD/DVD** device instead *looks* like it
  works (Parallels accepts arbitrary files there) but doesn't: the optical
  driver expects an ISO9660 filesystem, and `esp.img` is MBR+FAT32 (a hard
  disk layout). Symptom was firmware seeing a `BLK` device in the EFI Shell
  but resolving zero `FS` mappings — the device was visible, the content
  just wasn't recognized as anything the CD-ROM driver understands.
- The actual path: `make parallels-hdd` converts `esp.img` → `esp.dmg` (via
  `hdiutil convert`) → `esp.hdd` (via `prl_disk_tool create --hdd ... --dmg
  ...`, `prl_disk_tool`'s only documented way to build a `.hdd` from an
  existing raw image). `prl_disk_tool` silently fails ("Unable to open the
  disk image") if given relative paths — always pass absolute ones. Attach
  the resulting `esp.hdd` as the Hard Disk device, not `esp.img` and not
  CD/DVD.
- `esp.hdd` stores a pointer to `esp.dmg`'s absolute path rather than a copy
  of its data — the two files must stay together.

This was all confirmed by directly booting `esp.img` in QEMU with a real
`-drive format=raw` (not the `vvfat` passthrough `make run` uses) before
touching Parallels at all, to first rule out `make image` producing a
malformed disk. It didn't — the image was always fine; only the Parallels
attachment mechanism was wrong.

### Next milestone

**Parallels has a real, working console now - confirmed, not just
reasoned through.** The GOP framebuffer console (see "GOP framebuffer
console" above) reached a genuine milestone in "take six": a live,
prompt-displaying shell running on real Parallels hardware, the actual
goal the whole console-discovery effort (three console mechanisms tried
and rejected, then virtio-console, then this) was for. Five real
hardware round trips got there, each finding and fixing one genuine
bug: `open_protocol_exclusive` disconnecting firmware's own console
driver from GOP ("take two"); `try_virtio_console`'s MMIO scan freezing
the boot with no console yet installed to report through ("take
three"'s reorder got a console installed, and that console then
rendered the exception that led to the real fix); a decoded Synchronous
External Abort proving `virtio_mmio::find_device` crashes real hardware
outright, fixed with a safety gate ("take four"); and a *second*,
differently-addressed instance of the identical fault at `gic.rs`'s
`GICD_BASE`, proving the entire fixed low-1GB QEMU-shaped device-region
convention (not just virtio-mmio) is unsafe here, fixed by broadening
the same gate to cover GIC/timer setup too ("take five") - followed by
confirmation ("take six") that the complete fix, all four bugs
addressed together, reaches a real shell prompt with no further issues.

**What's confirmed working now, on real Parallels hardware:** GOP
discovery and mapping; direct framebuffer writes reaching the display
with no explicit flush; `fbconsole.rs`'s actual font/glyph/cursor
rendering; the exception handler reporting through that display; the
MMU identity map surviving real hardware's own memory layout; and
`tasks::start()`'s EL0 entry reaching and running the loaded shell
program end to end, with a live `$` prompt on screen.

**What was the real next gap at the time this section was written -
keyboard input - is now done too.** See "USB HID keyboard driver"
further below: a real, physical USB keyboard now types into this same
shell on this same real Parallels hardware, full round trip (letters,
backspace, Enter). Preemptive multitasking still isn't available on
Parallels - not blocking the keyboard work, which turned out not to
need it (the interrupt endpoint this driver uses is polled the same
iteration-bounded way everything else in this kernel is, not
IRQ-driven). **Update: real interrupt-controller discovery (ACPI MADT,
GICv3) is now done, and so is preemptive multitasking on Parallels - see
"MADT/GICv3" further below.** GIC/timer IRQ delivery, and the task
switch itself (after root-causing and fixing a real hang - task 1's
idle loop needed a busy-spin instead of `wfe` on real hardware), are
both confirmed working end to end on real Parallels hardware, with a
sustained interactive test showing a correctly, continuously
incrementing `uptime`.

**In the meantime, kernel development continues against QEMU**, which has a
fully working console via ACPI/SPCR (plus confirmed-working fallback paths
via virtio-console and, now, the GOP framebuffer console - both currently
only reachable as a fallback or via the same temporary-force technique
used to verify each). MMU/identity-paging, the timer-driven
preemption tick, the syscall boundary, real preemptive task switching, a
working interactive echo shell, a shell that's a real disk-loaded,
configuration-selected userland program, a working runtime virtio-blk
driver (read and write), a working runtime FAT32 reader, and real disk
commands covering the full read/write file-management surface
(`ls`/`cat`/`cd`/`pwd`/`mkdir`/`rmdir`/`touch`/`rm`/`write`/`cp`/`mv` -
see "Phase 8" above, alongside the earlier `help`/`echo`/`uptime`/
`clear` - and `docs/architecture.md`/`docs/processes.md`/
`docs/CHANGELOG.md` for the reference write-up) are all done and
confirmed working, not just structurally plausible.

**Phase 3, and the entire original "get to a shell" plan, are done - and
phases 4 through 8 (write support) have since taken that further than
the original plan called for, and have now closed out.** Phase 1
(accept input, echo it back), phase 2 (real commands backed by real
kernel state), and phase 3 (disk commands - `ls`/`cat`/`cd`/`pwd`, and
the full runtime storage stack underneath them: virtio-blk, FAT32, new
syscalls) are all confirmed working end to end - and `mkdir`/`rmdir`
(phase 4), `touch`/`rm` (phase 5), `write` (phase 6), `cp` (phase 7),
then `mv` (phase 8, see above - the last one with a genuine correctness
risk, not just a thin wrapper) crossed and then repeatedly extended the
write-support line phase 3 had deliberately drawn (see
`docs/CHANGELOG.md`'s "Phase 3" entry), for the narrowest useful case
each time. **This closed the "easy cheap-win commands" arc deliberately,
and the next real step after it (Parallels virtio-console, see above) is
now done too**, at least on the QEMU side - what's left in the gaps
below and `docs/roadmap.md`'s parking lot is genuinely bigger work (a
relocating loader, blocking primitives, virtio-console RX, driver
isolation), not more small commands in this vein. This section (and the
"gaps" paragraph below it) still tracks what's true right now;
`docs/roadmap.md` is the one to check for what's next, `docs/CHANGELOG.md`
for the full history of what's already done.

What's still coarse and worth knowing about before building on any of
this — kept current in `docs/processes.md`'s "known rough edges" rather
than duplicated here, since that's the document meant to track it as
these get addressed (the shared syscall-ABI crate that used to be listed
here first is done - see "A shared syscall-ABI crate" above): no
`core::fmt`/`write!` usable
in any loaded program, and (a real, separately-confirmed extension of
that same gap - see "Phase 3c" above) **no slice/string comparison
against a literal either** - both crash for the identical reason (a data
reference computed for this binary's link-time base of `0x0`); exactly
one program, loaded once, at boot, with no `exec()`; a fixed 2-task
scheduler (no dynamic task creation); no heap or `.bss` for userland
programs, so no static mutable state at all; a fixed, unguarded 8KB stack
per program; no ELF, no relocations, no dynamic linking (all of the
above - `core::fmt`, literal comparisons, and the lack of ELF/relocations
- are the same underlying limitation, not separate ones); `fat32.rs` has
no long-filename support in general (this project's own ESP directory was
renamed - `\EFI\ORBS\`, well inside FAT's 8.3 limit, not the 9-character
`\EFI\OUROBOROS\` - specifically to stay reachable without needing it,
but any *other* 9+
character name still isn't), only looks at the first FAT32-typed MBR
partition, and while a file can now hold real content, be copied, and be
renamed/moved, every write is still a full replace at the syscall/FAT32
layer (no append/offset-write primitive - the shell's `>>` composes
read-then-full-rewrite on top, bounded by the kernel's 512-byte
per-syscall cap; see "Output redirection" below), no recursive `cp`,
full directories now grow by a cluster automatically (see "Directory
extension" below - directories never *shrink*, though; an emptied
extension cluster stays linked until the directory is removed), the
cluster-chain-freeing path (now shared as `free_chain`) is exercised by
multi-cluster *directories* but still not by an actual multi-cluster
*file* (nothing yet
can create one that large - see "Phase 6" above), and every write-path
error collapses to one sentinel at the syscall boundary so userland
can't distinguish *why* an operation failed **(Update: fixed - see
"Splitting the collapsed FS_ERROR sentinel" further below; every
`fs_*` failure now returns a specific `FS_ERR_*` code)** (see "Phase 4" through
"Phase 8" above); disk-command pointer/length arguments are trusted, not
validated against the caller's actual mapped region (fine with exactly
one, currently-trusted userland program); and `virtio_console.rs` is
transmit-only - no receive path, and now confirmed not to matter for
Parallels specifically (see "virtio-console" above: real hardware
testing found no virtio-console device there at all, over either
transport this project can drive) - a receive virtqueue is still a real
gap for any *other* platform that does expose console via virtio-mmio/
virtio-pci, just not Parallels. `fbconsole.rs` (the GOP framebuffer
console, see above) is also write-only, for an unrelated reason - no
keyboard driver exists in this kernel at all, so even a platform where
the framebuffer console is the only option has no way to type into it
yet; has no colour or ANSI escape parsing; and its MMU device-block
fallback mapping (for a framebuffer outside the discovered RAM span) is
unverified against anything but QEMU's RAM-backed `ramfb`. Also still
true from before this milestone: strict round-robin only, no priorities
or blocking; FP/SIMD state still isn't saved anywhere (`exceptions.rs`'s
`Context`).

Reasonable next steps from here (this paragraph predates the USB HID
keyboard driver milestone further below, which closed the item it used
to list first - kept as-is except for that, rather than silently
rewritten, per this file's own discipline of recording what was true at
the time): real interrupt-controller discovery (ACPI MADT parsing, most
likely - the standard way firmware exposes this, and Parallels almost
certainly uses GICv3, a materially different register interface from
`gic.rs`'s current GICv2 code: system-register-based CPU interface
instead of memory-mapped, redistributors instead of a single
distributor for SGI/PPI routing) would lift the "no preemption on
Parallels" limitation from "take five" above - a real, substantial
follow-up, not a quick fix, deliberately not attempted in the same pass
that found the crash. **Update: done, see "MADT/GICv3" further below -
GICv3 was indeed the right guess, and it lifted the crash risk and
confirmed real IRQ delivery works, but preemption itself surfaced a
separate, new, still-unresolved real-hardware bug in the task switch,
so it's not fully lifted yet either.** `pci::log_all_devices` is a real, reusable
diagnostic worth remembering for any future "why can't this platform's
hardware be found" question; a real relocating loader (ELF + relocation
processing), which would lift both the `core::fmt` and
literal-comparison restrictions. **Update: done - see "A real relocating
loader" further below, confirmed on QEMU (both `core::fmt` and a
slice/literal comparison now work correctly via the new `selftest`
command), with real Parallels hardware confirmation still outstanding -
and it surfaced a second, genuinely unrelated, real pre-existing kernel
bug along the way (the SVC trampoline losing `x9` across every syscall),
also fixed there.** Blocking/waiting primitives so tasks
can do more than an unconditional round-robin `wfe` loop; or output
redirection (`>`/`>>`), now that `write_file`/`cp` give it something
real to build on but shell-side command-line parsing still doesn't
exist. **Update: both done - see "Blocking primitives" and "Output
redirection" further below.** Parallels' *byte-stream* console output stays a confirmed dead
end for virtio-console unless real documentation for its proprietary
serial device (vendor `0x1ab8`)
someday surfaces - not something to keep chasing without that.

## USB HID keyboard driver: confirmed - real, physical keyboard input on real Parallels hardware, first time ever

The gap the GOP framebuffer console milestone left open (write-only, no
keyboard driver at all) is now closed: a real, physical USB keyboard
types into the userland shell, on real Parallels-on-Apple-Silicon
hardware, full round trip confirmed - individual letters, backspace,
Enter submitting a real command line and getting the shell's genuine
`unknown command` response back. **A full technical postmortem, with
decoded register values, byte-level evidence, and the debugging
techniques that found each bug, lives in
[`docs/xhci-keyboard-postmortem.md`](docs/xhci-keyboard-postmortem.md)**
- written deliberately to be useful to other bare-metal-OS developers
hitting the same class of problem, not just as this project's own
history. This section is the shorter version, for this file's own
narrative record.

New module, `kernel/src/xhci.rs`: a from-scratch xHCI (USB3 host
controller) driver - capability/operational register bring-up, a
command ring, an event ring, device slot enable/address over control
transfers, and - the mechanism that turned out to actually be required,
see below - a real interrupt IN transfer ring. Discovered via a
generalized `pci.rs` (class 0x0c/subclass 0x03/prog-if 0x30, the
standard xHCI class code) rather than a fixed address, on the same
reasoning as the GOP framebuffer's own address: genuinely *discovered*,
not a QEMU-shaped guess, so - unlike `virtio_mmio.rs`/`gic.rs` -
deliberately *not* gated behind `qemu_device_region_safe`.

**Five independently-confirmed real-hardware bugs, none visible on
QEMU, each found by direct evidence rather than guessing:**

1. **A PCI Command register bit-position error.** `CMD_MEMORY_SPACE`
   was defined as `1 << 0` - which is actually I/O Space Enable, not
   Memory Space Enable (bit 1). Found by a diagnostic printing the
   Command register's value before/after this driver's own write,
   re-printed through the post-exit console specifically (boot-services
   `log::info!` output is lost the moment `fbconsole.rs` clears the
   screen for its own use - a real, recurring constraint on debugging
   anything that might crash later in boot on this platform, see the
   postmortem's "why is there no serial log" aside for the full reason).
   One real test showed `0x0010 -> 0x0015` - bits 0 and 2 set (I/O Space,
   Bus Master), never bit 1 (Memory Space). This explained every earlier
   "register reads 0xffffffff" symptom on *both* QEMU and Parallels at
   once - Memory Space had never actually been enabled by any prior
   version of this code, on any platform, despite the write "succeeding"
   every time.
2. **A genuine UEFI firmware panic on real hardware**, not a kernel
   crash - Parallels' own hypervisor log (`libMonitorArm.dylib`)
   recorded `mon.abort.message = PANIC@11.28
   UEFI-exception-ArmPciCpuIo2Dxe.dll`, a fault inside firmware's own PCI
   config-space I/O driver, before this kernel's own exception vectors
   even exist. Root cause: a BAR-reassignment probe (`write 0xFFFFFFFF`,
   read back the size mask, write a real address) - a completely
   standard technique, needed because QEMU's specific OVMF build leaves
   this device's BAR totally unassigned (confirmed via `info pci`/
   `info mtree` in the QEMU monitor: nothing binds a driver to this xHCI
   controller during a boot that loads its kernel over virtio-mmio, so
   firmware never bothers). QEMU's own PCI emulation tolerates this
   probe without complaint; real PCIe firmware does not. Fixed by never
   writing PCI config space beyond the one narrow, conditional
   Command-register enable - `pci.rs::discover_xhci` is read-only again,
   the same discipline `discover_uart16550`/`log_all_devices` already
   established safely on this exact hardware.
3. **The discovered BAR landed outside `mmu.rs`'s original
   single-L0-table-entry span** - `0x8000004000` on the QEMU dev loop,
   exactly 512GB, squarely outside the first (and, until now, only)
   top-level page-table entry this kernel's identity map ever set up.
   A real, if quiet, assumption from early in this project (every RAM
   address and the one hardcoded device region are always low) that a
   genuinely *discovered* PCI BAR has no obligation to respect. Fixed by
   generalizing `install_identity_map` to allocate further top-level
   (L0) table entries on demand, keyed by whatever address a discovered
   device's BAR actually needs, rather than assuming everything fits in
   the first 512GB.
4. **The deepest finding: Parallels' USB passthrough doesn't forward
   HID *class* requests to the real device at all.** `SET_PROTOCOL`/
   `GET_REPORT` (the two mechanisms this driver was originally built
   around, polling the keyboard the same way real BIOS/UEFI keyboard
   drivers traditionally do) kept coming back as either this driver's
   own `GET_REPORT` Setup packet, echoed back byte-for-byte (confirmed
   by decoding the returned bytes against the exact request sent,
   including a `wLength` field that tracked a deliberately-changed
   request size exactly across two separate real-hardware test rounds -
   ruling out a fixed-size buffer-aliasing coincidence), or, in one
   test, a block of what decoded cleanly as unrelated Interface/HID/
   Endpoint descriptor bytes. A poisoned-buffer test (fill the DMA
   target with `0xee` immediately before ringing the doorbell) confirmed
   the data stage genuinely executes and genuinely overwrites the
   buffer - not a stuck or skipped transfer, just never real device
   data. The test that actually settled it: a *standard* request,
   `GET_DESCRIPTOR(Device)`, issued right next to a failing
   `GET_REPORT`, came back as a perfectly valid device descriptor -
   `bLength=0x12`, `bDescriptorType=0x01`, and (once tested against the
   real keyboard specifically) `idVendor=0x203a`, Parallels' own real,
   registered USB vendor ID for this exact virtual keyboard. Standard
   requests reach the real device correctly; class requests do not get
   forwarded at all - a genuine gap in Parallels' USB passthrough
   implementation for this device class, confirmed with hardware
   evidence, not assumed. Fixed by abandoning `GET_REPORT` polling
   entirely and using a real **interrupt endpoint** instead - armed via
   `Configure Endpoint`, a standard xHCI *command*, not a class
   *request* at all, and once armed, delivering real reports with zero
   further request/response round trips of any kind for this class of
   bug to hide in. This is also, not incidentally, the same mechanism
   every production USB HID driver actually uses at runtime; `GET_REPORT`
   is more of a one-shot query mechanism, evidently less
   thoroughly implemented in this specific passthrough layer.
5. **Once the interrupt endpoint was delivering real, live, changing
   data, it turned out to be reading Parallels' virtual mouse, not the
   keyboard.** This driver's port scan had just grabbed the first
   connected device - confirmed by the report bytes changing
   continuously as the mouse moved over the VM's window, not by any
   error. A real Parallels VM exposes at least a virtual mouse/tablet on
   the same xHCI controller as the keyboard, and nothing before this had
   ever needed to tell them apart. Fixed by scanning every connected
   port and parsing each device's actual Configuration descriptor for a
   genuine HID Boot-Protocol-Keyboard interface (`bInterfaceClass=3`,
   `bInterfaceProtocol=1` - `2` is Mouse) before committing to
   configuring its interrupt endpoint, rather than treating "found *a*
   HID device" as "found *the* keyboard."

**Still coarse, worth knowing before building on this:** one port, one
device, one slot **(Update: lifted - see "xHCI multi-device support"
further below; every connected device is now enumerated, classified,
and kept concurrently addressed)**, no hot-plug, no hubs (route string
always 0), no real
HID report-descriptor parsing (boot-protocol's fixed 8-byte layout is
assumed directly - and since `SET_PROTOCOL` almost certainly still isn't
reaching the device either per finding 4 above, this only works because
the real keyboard happens to use the same simple layout regardless of
which protocol is nominally active); no stall recovery on the interrupt
endpoint specifically (EP0's own setup-time control transfers do recover
from a Stall - see `recover_from_stall` - the interrupt endpoint just
logs and re-arms); only the first matching interrupt IN endpoint found
is ever configured. Preemptive multitasking is still unavailable on
Parallels (a separate, already-tracked gap, not something this milestone
needed - the interrupt endpoint is polled the same
iteration-bounded busy-poll way every other wait in this kernel is, not
IRQ-driven).

## MADT/GICv3: real interrupt-controller discovery for Parallels - and full preemptive multitasking, confirmed working end to end on real hardware

The "preemption on Parallels" gap flagged at the end of the USB HID
keyboard milestone above is now **fully closed**: real ACPI MADT
parsing replaces the old `qemu_device_region_safe` heuristic for
GIC/timer setup specifically, a GICv3 driver was built and confirmed
working end to end on real Parallels hardware, and a second, separate
real bug found in the process - the actual task switch hanging the
first time it ever ran on real hardware - was root-caused (well enough
to fix, if not fully explained) and fixed the same session. Real,
preemptive, two-task round-robin multitasking now works on real
Parallels hardware for the first time ever, confirmed by a sustained
interactive test with a genuinely, correctly incrementing `uptime`
throughout.

**Why MADT, not another QEMU devicetree dump.** `gic.rs`'s old addresses
(`0x0800_0000`/`0x0801_0000`) were GICv2-only and confirmed only via a
QEMU-internal devicetree dump - the exact same kind of QEMU-shaped
convention that already crashed real Parallels hardware once (a decoded
Synchronous External Abort with `FAR_EL1` matching `GICD_BASE` exactly -
see "take five" above), which is why GIC/timer setup got folded into
`qemu_device_region_safe`'s blanket skip in the first place. ACPI's MADT
table (`"APIC"` signature - the spec's own historical x86 name for it,
not `"MADT"`) is the platform's genuine, portable way of describing its
interrupt controller, the same role SPCR plays for the console. New
module `kernel/src/madt.rs` reuses `acpi.rs`'s RSDP -> XSDT walk
(refactored into a shared `find_table(rsdp, signature)` helper once a
second real caller needed it, not speculatively) and parses the MADT's
GICD (Type 0x0C - carries a `GIC Version` byte: 2, 3, or 4, the
authoritative way to pick a driver, not a guess), GICC (Type 0x0B - a
GICv3 system may describe each CPU's redistributor via this structure's
own `GICR Base Address` field instead of a separate structure), and GICR
(Type 0x0E - a contiguous redistributor region) structures. Struct field
layouts cross-checked against Linux's `include/acpi/actbl2.h`
(`acpi_madt_generic_interrupt`/`_distributor`/`_redistributor`), same
discipline `gic.rs`/`mmu.rs` already hold their register-bit sourcing to.
Every field read is bounds-checked against the structure's own declared
`length` first - a real, live concern, not paranoia: the GICC structure
specifically has grown across ACPI revisions, and blindly reading a
newer field out of an older/shorter table entry would be reading
garbage.

**Confirmed on QEMU two independent ways before ever touching
Parallels**, per the scoping plan's staged, QEMU-first testing approach.
First, against QEMU's *default* GICv2 config: `madt::discover` reported
`GIC V2, GICD @ 0x8000000, GICC/GICR @ 0x8010000` - an exact match for
the already-known-correct devicetree-derived values, confirming the
parser without touching any new driver code. Second, QEMU's `virt`
machine can be forced onto GICv3 (`-machine virt,gic-version=3` -
confirmed via `-machine virt,help`, not assumed; a new `make run-gicv3`
Makefile target wraps this). A devicetree dump under that flag
independently confirmed `GICD @ 0x08000000` (size `0x10000`) and a
*single contiguous* GICR region at `0x080a0000` (size `0xf60000`, sized
for many possible CPUs, `#redistributor-regions = <1>`) - and
`madt::discover`, parsing the real ACPI MADT (a completely independent
code path from the devicetree dump), reported the exact same addresses
and size. Two independent sources agreeing is what "confirmed," not
"probably right," means throughout this project.

**The GICv3 driver (`kernel/src/gicv3.rs`) and the version-dispatch
facade.** `gic.rs`'s original GICv2 content moved unchanged into
`gicv2.rs` (now taking its addresses as parameters instead of hardcoded
constants); `gic.rs` itself became a thin facade holding whichever
`madt::GicInfo` was actually discovered and dispatching all four calls
(`init`/`enable_interrupt`/`acknowledge`/`end_of_interrupt`) to the
right backend - `exceptions.rs`'s two call sites don't change at all,
`main.rs` gains one `gic::configure(info)` call ahead of the existing
pair. GICv3 is architecturally different enough from GICv2 that this
was a real driver, not a find-and-replace: the CPU interface is
system-register-based (`ICC_SRE_EL1`/`ICC_PMR_EL1`/`ICC_IGRPEN1_EL1`/
`ICC_IAR1_EL1`/`ICC_EOIR1_EL1`), not memory-mapped at all; PPI enable
moves from the distributor to a per-CPU **Redistributor** region
(`GICR_ISENABLER0`), which first needs waking (`GICR_WAKER.ProcessorSleep`
cleared, then polled until `ChildrenAsleep` clears - no GICv2 equivalent
at all); and finding *this* CPU's own redistributor frame within a
region that may describe several needs a real walk (`GICR_TYPER`'s
Affinity field compared against this CPU's own `MPIDR_EL1`, stopping at
a match or the "Last" bit, with the frame stride depending on
`GICR_TYPER.VLPIS`) rather than assuming frame 0, matching this
project's standing discipline of discovering rather than assuming (the
same reasoning `xhci.rs` scanning every port instead of trusting the
first device found already established). `mmu.rs`'s `extra_devices`
mechanism (already generalized for the xHCI BAR) needed no new
mapping logic, just a size bump (`MAX_EXTRA_L1_TABLES` 2 -> 4,
`main.rs`'s `extra_devices` array 2 -> 4 entries) since real Parallels
GIC addresses were completely unconfirmed going in and nothing ruled out
them needing their own top-level table entries the way the xHCI BAR did
(in the event, both GICD and GICR turned out to land inside the
already-mapped low regions on both QEMU and Parallels - but the
generalization exists for whenever that stops being true).

**Two real GICv3 bugs found on QEMU, before ever risking a Parallels
round trip on them - exactly what the staged testing approach was
for.** First attempt: `uptime` stuck at 0 forever under forced GICv3,
no crash, no fault - the tick simply never reached this kernel's
handler. Root cause, cross-checked against Linux's own `gic_cpu_init`
(`irq-gic-v3.c`): a PPI defaults to Group 0 (FIQ) unless a redistributor
register (`GICR_IGROUPR0`) explicitly reassigns it to Group 1 (IRQ,
the group `ICC_IGRPEN1_EL1` actually enables) - fixed by writing
`0xffff_ffff` there, matching Linux's own value exactly. Second attempt,
same symptom persisting: `GICD_CTLR`'s enable bit isn't a single bit the
way GICv2's was - bit 0 alone only enables Group 0 at the distributor
level too, regardless of the redistributor's own per-interrupt group
assignment; needs `ENABLE_G1 | ENABLE_G1A | ARE_NS` together (values
`0x1`/`0x2`/`0x10`, same Linux cross-check), plus waiting for
`GICD_CTLR.RWP` to clear before proceeding - both fixes confirmed by a
full round trip afterward: `uptime` correctly incrementing (`6` -> `46`
-> `170` ticks across three checks), then a sustained 20-second gap
(`88` -> `876` ticks, consistent with the TCG timing variance already
documented for the GICv2 case), zero aborts in a `-d int` cross-check
across every run.

**Real Parallels hardware, using `make test-parallels` (see below) for
the first time to drive the actual round trips - itself a first for
this project.** Staged exactly per the scoping plan: MADT discovery was
shipped and tested *before* the real GICv3 register sequence ever ran
there (a temporary, source-level "log the discovery, skip
`gic::init()`" flag, the same "temporarily force, test, revert"
discipline the console-discovery saga established) - confirmed clean,
resolving the biggest open risk going in (whether Parallels' MADT
describes an interrupt controller at all, the same open question its
absent SPCR had already raised doubt about - it does: `GIC V3, GICD @
0x2410000, GICC/GICR @ 0x2500000 (size 0x40000)`, genuinely different
addresses from QEMU's, no crash, no hang). Only then was the real
`gic::init()`/`gic::enable_interrupt()` sequence enabled for a second
round trip.

**That second round trip hit a new, real, currently-unresolved bug -
and it isn't the MADT/GICv3 work itself.** With GIC/timer fully
enabled, the framebuffer console (Parallels' only console) went
completely silent the instant the first tick fired: no exception
reported, no further output, keystrokes stopped being echoed at all -
indistinguishable, from the screen alone, between "hung" and "still
running but not listening." A single-variable diagnostic (temporarily
skipping just `tasks::on_tick`'s actual context swap, in
`exceptions.rs`'s `rust_irq_handler`, while leaving everything else -
GIC init, interrupt enable, acknowledge, end-of-interrupt, `TICKS`
counting - fully active) isolated it precisely: with the switch
skipped, `uptime` reported a real, correctly-incrementing tick count on
real Parallels hardware (`533` -> `752` in one observed run) and the
shell stayed completely responsive; with it enabled, the very first
switch hangs the system outright. **GIC/timer IRQ delivery itself is
now conclusively confirmed solid on real Parallels hardware** - the bug
is specifically in the task switch, which had never run on real
hardware before this session (Parallels had no working GIC/timer at all
until now, so this exact interrupt-delivery-plus-context-swap
combination was simply never exercised there). `tasks::on_tick` itself
is unchanged and still fully exercised on QEMU (GICv2 and forced GICv3
both retested end to end after this was found, zero regressions).

**Take two, same session: root-caused well enough to fix, task switching
now fully enabled everywhere.** Initially shipped disabled on Parallels
(`exceptions.rs` gained a temporary `TASK_SWITCH_ENABLED` flag,
defaulted off there) as an honestly unresolved mystery, same treatment
as the EL0/RAM-permission bug in the boot-bringup arc - but a real
lead was still on the table (task 1's idle loop uses `wfe`, and real
hardware's `wfe` semantics under a hypervisor were the leading
suspect), so it was tested directly rather than left there: swapped
`tasks.rs`'s idle-task `wfe` for a plain busy-spin (`nop; b 1b`), left
task switching fully enabled, and re-tested on real Parallels hardware.
**The hang was completely gone.** A sustained interactive test - several
commands in sequence, real typed input via `prlctl send-key-event`, not
just a bare `uptime` poll - showed a correctly, continuously
incrementing tick count throughout (`644` -> `1210` in one observed
run) with no hang, confirming this is the real fix, not a fluke.
`TASK_SWITCH_ENABLED` and its setter were removed entirely -
`exceptions.rs`'s IRQ handler calls `tasks::on_tick` unconditionally
again, exactly like every version of this code before Parallels ever
had a working GIC/timer at all. **Root cause still not fully proven,
only worked around** - the leading hypothesis remains that real
hardware's `wfe` is trapped/emulated by the host hypervisor (Apple's
own virtualization layer, one level above this guest kernel) in a way
QEMU/TCG's `wfe` never is; this kernel's `nTWE`/`nTWI` bits only ever
controlled whether EL0's own `wfe` traps *to EL1*, a decision this
kernel owns completely, and whatever EL2 does to `wfe` above that is
outside this kernel's control or visibility entirely. The busy-spin
fix's only real cost is idle power efficiency, not a concern this
kernel has optimized for anywhere else (task 0's own I/O polling
already busy-waits) - see `tasks.rs`'s `el0_idle_template` doc comment
for the full writeup, kept there rather than only here since that's
where a future reader is most likely to encounter it.

**A real, secondary, non-fatal finding along the way - root-caused and
fixed the same session, not left as a guess.** An occasional dropped
keystroke was observed twice during real-hardware testing with task
switching active (e.g. `uptime` arriving at the shell as `uptme` or
`uptie` - one character short, even though the xHCI driver's own debug
log confirmed all the expected HID reports, including the missing
character's, were actually received). The "single outstanding report
buffer" theory considered at the time turned out to be wrong - the real
bug was in `xhci.rs::Device::poll_key` itself, a genuine logic error
independent of any hypervisor timing quirk: a single polled report can
legitimately contain *more than one* newly-pressed keycode at once
(most likely when a poll gets skipped - this task preempted between two
hardware samples, missing an intermediate report, a real new
possibility once preemption started working at all - but not
exclusively; two keys pressed within one poll interval can do it too,
preemption or not). The original code returned on the *first* qualifying
keycode in a report after already recording that whole report as
`last_report` - so any second new keycode in the same report was
silently gone forever, since `last_report` already included it and it
could never look "new" again. Fixed by draining every qualifying
keycode from a report into a small `pending` buffer (`[u8; 5]` - a
report's `buf[2..8]` holds at most 6 simultaneous keycodes, and the
first match is always returned immediately rather than queued, so at
most 5 can ever need to wait) instead of discarding everything past the
first. Confirmed fixed on real Parallels hardware: ten consecutive
`uptime` invocations, back to back, all recognized correctly with zero
drops - a real contrast against the intermittent failures observed
before the fix.

**Scripted real-hardware testing (`make test-parallels`,
`scripts/test-parallels.sh`) was what made this entire investigation
practical in a single session** - discovered via Parallels Desktop's
own CLI, `prlctl` (`man prlctl`), on 2026-08-16, the same day as this
milestone. Every real-hardware round trip above (discovery-only,
full-GIC-enabled, the task-switch isolation diagnostic, the final
shipped-state confirmation) went through it: rebuild `esp.hdd`, boot
the registered VM headlessly, type commands via `prlctl send-key-event`
(real decimal PS/2 Set-1 scancodes - `prlctl` rejects hex), and
screenshot via `prlctl capture` after each one - no human watching the
VM live, no physical keyboard, the exact manual loop every earlier
postmortem in `docs/` paid wall-clock time for, now driven headlessly.
See `docs/roadmap.md`'s "Testing infrastructure" section for the full
writeup.

**Still coarse, worth knowing before building on this:** the idle task's
`wfe` -> busy-spin swap is a confirmed-working fix, not a confirmed
root cause - if task switching on some *other* future platform ever
hangs the same way, don't assume it's automatically the same bug
without re-confirming; the dropped-keystroke bug above is fixed, but
`xhci.rs`'s `pending` buffer assumes boot-protocol's fixed 6-simultaneous-
keycode report shape, same as every other assumption this driver
already makes about that format; the MADT parser only reads the first
GICD/GICC/GICR structures it finds (spec requires exactly one GICD, but
a real multi-cluster system could have several GICR structures this
parser doesn't yet merge/choose between); `gicv3.rs`'s
redistributor-frame walk is real discovery logic but has only ever been
exercised against single-CPU configurations (QEMU with default `-smp`
and Parallels' own single-vCPU test VM) - never tested against a
genuinely multi-core guest; and virtio-blk/virtio-mmio on Parallels
remain completely unaddressed by this work (a real device that, per the
existing PCI inventory, simply isn't present there over any transport
this project can drive - a different, harder problem than the
interrupt-controller one this milestone solved).

## xHCI's busy-waits were iteration-bounded, not time-bounded - a real, user-reported bug on real hardware

Found by the user directly, not in any of this project's own scripted
testing: booting the real `esp.hdd` normally in Parallels (not via
`make test-parallels`, which drives Parallels' own synthetic keyboard
headlessly) produced `xhci: keyboard not available (Command ring: timed
out waiting for a completion event)` - the keyboard driver failed
outright, on real hardware, in a way none of this session's own
real-hardware testing had ever hit.

**The key clue was reproducing it a second time and getting a
different failure point.** The first report failed almost immediately
in port setup (most likely Enable Slot or Address Device, the very
first command-ring operations); a second attempt, on request, got all
the way through slot addressing, configuration, and interrupt-endpoint
descriptor discovery before failing on what's almost certainly the
final `Configure Endpoint` command. Two different commands timing out
on two different boots is a strong signal that the *wait mechanism
itself* is the problem, not any one command's own logic - the same
kind of pattern-over-single-instance reasoning this project has relied
on throughout (see, e.g., the two differently-addressed
`virtio_mmio`/`GICD_BASE` bus faults in "take four/five" above, which
turned out to indict an entire addressing *convention*, not one
address).

**Root cause:** every busy-wait in `xhci.rs` (`wait_command_completion`,
`wait_transfer_event`, the port-scan loop, `poll_until`) was bounded by
a fixed iteration count (`POLL_ITERS`, 2,000,000), on the documented
reasoning that no interrupts or timer are available yet at that point
in boot. True, but a fixed iteration count is only a valid proxy for
real elapsed time if the host never preempts this guest's vCPU for any
real duration while it spins - and a real hypervisor doesn't guarantee
that. The one real difference between this session's own successful
testing (`make test-parallels`, headless, no live-rendered VM window)
and the user's manual run (a live VM window, actually being watched) is
exactly the kind of thing that would compete for real host CPU/GPU time
and stall the guest's vCPU mid-poll for a real, unpredictable duration -
consuming zero of the loop's "iterations" while real wall-clock time
that should have counted toward the timeout quietly passed.

**Fix:** replaced every `POLL_ITERS`-bounded loop with a genuine
wall-clock deadline, using the ARM generic timer's free-running
physical counter (`CNTPCT_EL0`) compared against a real millisecond
budget (`POLL_TIMEOUT_MS`, 1000ms) derived from `CNTFRQ_EL0` - both
pure system-register reads, the identical "no GIC, no interrupts, safe
on any ARMv8 CPU by construction" property `timer.rs`'s own
`frequency_hz`/`arm` already established and now exposes
(`pub(crate) fn now_ticks()`) for exactly this kind of reuse elsewhere
in the kernel.

**Confirmed fixed by the user directly, on the exact real-world
scenario that originally failed** - not a scripted test, a live,
manually-launched VM window, the same way the bug was first hit. Also
regression-tested on QEMU (`make run-usb-kbd`, real keystrokes via the
QEMU monitor) and via this project's own `make test-parallels` -
neither of which had ever reproduced the bug in the first place, but
both confirm the change doesn't regress the fast, already-working path.

**Lesson:** "no interrupts available yet, so this has to be
iteration-bounded" was true when it was first written, but stopped
being the *only* option the moment `timer.rs` proved the generic
timer's counter registers need no interrupts either - worth
remembering any time a past constraint gets cited as a reason a design
can't be improved; check whether the constraint that motivated it is
still actually the binding one. And a bug that only a human, watching a
real, live-rendered VM window, could trigger - never a script driving
the same VM headlessly - is itself a real, useful data point about
where the bug lives, not just bad luck.

## A real relocating loader - and a genuinely pre-existing kernel bug it surfaced, not a bug it introduced

Every userland program used to be a flat, position-*dependent* binary:
linked at a fixed base of `0x0`, `relocation-model=static`, copied
byte-for-byte to wherever `boot::allocate_pages` happened to place it,
with no relocation step to fix the mismatch between "compiled assuming
base `0x0`" and "actually running somewhere else." That mismatch was
this project's single most-repeated bug class - the `core::fmt`
argument-dispatch-table crash and, separately, the slice/string-vs-
literal comparison crash in `cd`'s old path logic, both root-caused to
the identical mechanism (an absolute *data* pointer baked in for the
wrong base address) and both previously avoided only by hand-discipline
("don't use `write!`", "don't compare a slice to a literal"),
documented in `docs/processes.md` rather than actually fixed.
`docs/roadmap.md` had long flagged "a real relocating loader" as the
actual fix. This is that.

**`loader.rs` now parses real ELF64**, hand-rolled (matching this
project's established discipline - `acpi.rs`/`madt.rs`/`fat32.rs` all
hand-roll their formats rather than pull in a crate, and this module
runs entirely before `exit_boot_services`, so `alloc` is fully available
here unlike `fat32.rs`): header, program headers, section headers
(kept - `objcopy --strip-all` removes the symbol table and debug info,
confirmed by direct comparison not to touch section headers, program
headers, or `.rela.dyn`'s contents), and `Elf64_Rela` entries, all
small fixed-offset structs read via `read_unaligned` at checked
offsets, the same discipline `acpi.rs` already established for external
file data. `load_elf_into_el0_region` walks `PT_LOAD` segments (copying
`p_filesz` bytes, zeroing the `p_memsz - p_filesz` remainder - a real
`.bss` region, if a program ever has one, falls out of this for free,
not a separate feature), computes the region size from `max(p_vaddr +
p_memsz)` across them rather than the old flat loader's raw file
length, then finds `.rela.dyn` by name (via the section header string
table) and applies every `R_AARCH64_RELATIVE` entry - `value = base +
addend` written at `base + offset` - against wherever the allocator
actually placed the program. Any other relocation type is a hard
`UnsupportedRelocation` error, not silently skipped: this project has
no shared libraries and no imported symbols, so nothing else should
ever legitimately appear. `LoadedProgram` gained a real `entry` field
(`base + e_entry`, distinct from `base` even though the two happen to
stay equal today thanks to `linker.ld`'s `KEEP(*(.text.start))`
discipline) - `tasks.rs`'s `elr_el1` now uses it instead of assuming
equality itself.

**The toolchain side, confirmed by direct experiment before writing any
loader code, not assumed from documentation.** `.cargo/config.toml`'s
`[target.aarch64-unknown-none]` switched from `relocation-model=static`
to `relocation-model=pic` plus `-pie`, `--no-dynamic-linker` (no
dynamic linker of any kind exists here), and `-z max-page-size=4096`
(LLD defaults `PT_LOAD` alignment to 64KB, needlessly inflating a
few-KB program; 4KB matches `mmu.rs`'s own EL0 page granularity) - each
flag's necessity verified with a throwaway scratch crate and
`llvm-readobj -r`/`-l`/`-h`, not guessed. A real, non-obvious
distinction found along the way: `relocation-model=pic` *alone*
(without `-pie`) makes LLD silently resolve GOT entries to final,
link-time (base-`0x0`) addresses in an ordinary *static* executable -
reproducing the exact bug this whole effort is for, just one level
down; `-pie` is what actually produces a genuine `ET_DYN` binary with a
runtime-processable `.rela.dyn`. `shell/linker.ld` gained `.rela.dyn`,
`.dynsym`/`.dynamic` (structurally required by LLD for a well-formed
`ET_DYN`, never read by the loader), and - found necessary by direct
experiment, not anticipated in the original plan - `.data.rel.ro`
(routes compiler-generated relocated-but-logically-constant data, e.g.
panic-location tracking for implicit bounds checks, out of the path
that would otherwise sweep it into `.data` via the wildcard and
spuriously trip the still-unchanged "no static mutable state" `ASSERT`
- confirmed by inspecting `.data`'s address against `.rela.dyn`'s
offsets before adding the fix). The `.bss`/`.data` `ASSERT`s themselves
were deliberately left in place - lifting them would be a real,
separate capability expansion (userland static mutable state), not a
side effect of fixing relocation, and out of scope for this pass.

**A real, hard toolchain constraint, found and confirmed empirically:
userland programs must be built in `--release`, not debug.** A debug
build of the real `shell` crate fails to *link* at all under the new
model - `rust-lld: error: relocation R_AARCH64_ABS64 cannot be used
against symbol '<core::fmt::builders::PadAdapter as
core::fmt::Write>::write_str'; recompile with -fPIC`, originating
entirely from the prebuilt (not rebuilt-per-project - see
`rust-toolchain.toml`) `libcore.rlib`'s own object code, pulled in by
ordinary debug-build panic/bounds-check formatting machinery regardless
of whether `shell`'s own code calls `write!`/`format!` at all -
confirmed via `llvm-nm` that none of `shell`'s own compilation units
reference `PadAdapter` directly. A release build's optimizer eliminates
enough of that unreachable-in-practice code that the poisoned object
never gets pulled into the link - confirmed by testing a release build
of the exact same crate, which linked with zero errors. Not something
worth reintroducing `-Z build-std` (nightly-only) to work around -
`rust-toolchain.toml`'s own comment already states none is needed, and
this project has stayed on stable throughout. `Makefile`'s `shell-bin`
target now hardcodes `--release` for `cargo build -p shell`, regardless
of the overall `$(PROFILE)` used for the kernel, and stages a stripped
*ELF* (`objcopy --strip-all`, no more `-O binary`) instead of the old
raw flat binary.

**The real acceptance test isn't "it boots" - it's that `write!` and a
slice-vs-literal comparison, the exact two previously-crashing
patterns, now work.** `shell/src/main.rs` gained a permanent `selftest`
builtin (not a throwaway test, listed in `help`'s own output) that
exercises both directly: a `core::fmt::Write` impl over `putc`,
`write!`-formatting a runtime-computed (not constant-folded) value, and
a slice/`&str` comparison against a `b"..."`/`"..."` literal using
values built at runtime rather than borrowed from a literal themselves
- the exact shape that crashed as `cwd_bytes != b"/"` in the old
`resolve_path`. `print_u64_decimal` itself is left as hand-rolled
decimal formatting anyway (it already worked and doesn't need
`core::fmt` for something this simple) - its own doc comment now
explains this is a historical note, not a live restriction.

**A second, genuinely important thing this work found: a real,
pre-existing bug in `exceptions.rs`'s SVC trampoline, latent since the
syscall boundary was first built, never triggered until this milestone's
different codegen happened to expose it.** First real-hardware... no,
first real-QEMU symptom: `print_u64_decimal`, unmodified, crashed
printing any *multi-digit* value through the new loader - single-digit
values (and anything the compiler could constant-fold and inline)
worked fine, but a genuine multi-iteration call - the digit-print loop,
computing a base pointer once before the loop and reusing it across
several `putc` syscalls - reliably faulted on the *second* iteration,
`ELR_EL1` landing back inside that same loop, `FAR_EL1` a small,
near-null address. Root-caused, not guessed: a `qemu_device_region_safe`-
style test (forcing GIC/timer off entirely) ruled out a nested-IRQ-
during-SVC theory outright - the crash persisted with the timer
completely disabled. A direct, isolated register-survival probe (raw
inline `asm!`: load a magic `u64` into `x9`, issue one `svc`, read `x9`
back, compare) proved conclusively that **`x9` does not survive a
syscall round-trip** - despite `exceptions.rs`'s SVC path (`3:`)
appearing, on a first read, to save and restore it symmetrically
around the `bl` into `dispatch()`. Disassembling the actual compiled
vector table (not just the source) found the real bug: **the EC check
at the very top of vector slot 8 - `mrs x9, esr_el1` - runs *before*
the "3:" trampoline's own save sequence ever gets a chance to preserve
`x9`, so whatever value userland had live in `x9` at the moment of
`svc` is already gone by the time "3:" saves (and later "correctly"
restores) it - saving and restoring *a* value, just the wrong one (the
shifted `ESR_EL1`/EC field, which happens to equal `0x15` for an SVC -
matching the small, near-`0x15`-ish `FAR_EL1` values observed exactly,
since that clobbered value then got used as part of a pointer
computation).** This was never about the relocating loader being wrong
- every relocation and segment copy this milestone produces was
independently confirmed correct (a temporary debug dump of every
applied relocation matched `llvm-readobj -r`'s report exactly) before
this detour even started. It's a real, general bug that could affect
*any* userland code keeping a value live in `x9` across a syscall -
simply never exercised before, since no prior build's register
allocation happened to do that. **Fixed** by saving `x9` to a scratch
stack slot immediately, before the EC check clobbers it, and having
"3:" recover the real value from that slot before its own save sequence
runs (the non-SVC/fault path restores and discards it just as fast,
for symmetry, though correctness doesn't depend on that half - "1:"
never returns to userland regardless). Confirmed fixed via the same
register-survival probe (`x9 SURVIVED`, not `x9 CHANGED`) and then via
the full `selftest`/regression pass below.

**Confirmed working end to end, both fixes together, via the same
piped-stdin QEMU technique as every prior interactive milestone - twice,
once against `make run`'s FAT16 vvfat and once against the real FAT32
`esp.img`:** `selftest` prints `write!/core::fmt: 42 (expect 42)`,
`slice-vs-literal comparison: true (expect true)`, and `str-vs-literal
comparison: true (expect true)` correctly; `help`, `echo`, and `uptime`
(a genuinely multi-digit tick count, `126`, the exact previously-crashing
shape) all produce correct output with no crash; the full disk-command
surface (`ls`/`cat`/`mkdir`/`cd`/`pwd`/`touch`/`write`/`cat`/`cp`/`mv`/
`ls`/`rm`×2/`cd ..`/`rmdir`/`ls`) round-tripped correctly against the
real FAT32 image, ending back at a clean state; an unknown command
(`bogus`) correctly reports `unknown command: bogus`, not garbage (the
old symptom, before the `x9` fix, of the same underlying bug corrupting
a string-print loop's own loop-carried pointer instead of a digit
loop's). Zero aborts (`Data Abort`/`Prefetch Abort`/`Undefined
Instruction`) in `-d int` cross-checks across both sessions.

**Confirmed on real Parallels hardware too, via `make test-parallels`
(`CMDS="selftest;help;echo hi;uptime"`), not just QEMU.** `selftest`
produced the identical three correct lines
(`write!/core::fmt: 42 (expect 42)`, `slice-vs-literal comparison: true
(expect true)`, `str-vs-literal comparison: true (expect true)`) on
real hardware - the actual acceptance test for the whole milestone,
passing on the platform that matters most to this project.
`uptime` reported `1226 ticks since boot` - a genuine 4-digit value,
correctly printed via the exact multi-iteration `print_u64_decimal`
loop that used to crash from the `x9` bug, real, direct confirmation
that fix holds on real hardware too, not just under QEMU/TCG. `echo hi`
round-tripped correctly. One unrelated, already-documented artifact
showed up in the same test: `help` arrived at the shell as `hep` (a
dropped keystroke via `prlctl send-key-event`'s synthetic keyboard
timing - the same class of issue the MADT/GICv3 milestone's
"dropped-keystroke bug" section already covers, not a regression from
this work) - but the resulting `unknown command: hep` printed as clean
text, not corrupted garbage, which is itself further confirmation that
multi-character string printing (`print_str`'s own multi-`svc` loop)
is unaffected by the `x9` fix, as expected. Zero visible corruption or
crashes across the whole sequence.

**Still coarse, worth knowing before building on this:** no dynamic
linking, no `exec()`/multi-program support (still one program, loaded
once, at boot); no W^X segment permission separation (still one
combined RWX EL0 region - segment-level R-X/RW split was explicitly out
of scope for this pass); no static mutable state for userland programs
(the `.bss`/`.data` `ASSERT`s stay in place, deliberately); every
userland program must now be built in release mode, a real, permanent
constraint on anything written to replace `shell` (see
`docs/processes.md`).

## Blocking primitives - real task blocking, and a second real bug the SVC path was hiding

`tasks.rs`'s scheduler used to be a strict, unconditional round-robin
between exactly two fixed tasks: the timer tick alternated them every
20ms *regardless of whether task 0 (the shell) had anything to do* -
its main loop busy-polled `try_read_char()` every iteration rather than
actually waiting for a keystroke, because there was no way for a task
to tell the scheduler "don't run me again until X happens." This was
the last item in `docs/roadmap.md`'s parking lot phrased as "tasks
aren't limited to unconditional round-robin `wfe` polling" - now done.

**Why not just call `wfe` instead of busy-polling - the design
question this milestone actually turned on.** The obvious-looking
alternative (mark the task blocked, return immediately, let it call
`wfe()` once) was rejected outright, not overlooked: real Parallels
hardware has a confirmed, unresolved hang when an EL0 task executes
`wfe` (see `tasks.rs`'s own module doc comment - task 1's idle loop had
to become a busy-spin specifically because of this, root-caused well
enough to work around but not fully explained). A task blocking via its
own `wfe` call would almost certainly hit the identical hang. The
design shipped here never executes `wfe` anywhere: a blocked task is
suspended entirely by the scheduler and a different, already-runnable
task's saved context is loaded in its place - exactly the same
mechanism that already safely idles task 1 today, just triggered from
a syscall instead of only from the tick.

**Design, in four pieces.** `tasks.rs` gained real per-task state
(`TaskState::Runnable`/`Blocked(WaitReason)`, one `WaitReason` variant
today - `Keyboard`) and `block_current_and_switch(frame, reason)`:
saves the calling task's context, marks it blocked, loads the next
*runnable* task's context in its place, and returns
`frame.gpr[0]` (that task's own `x0`, post-overwrite) as its own result
- the one subtle piece, explained below. `on_tick` gained a wake-check,
run before its normal scheduling decision: for each blocked task,
evaluate its `WaitReason` (`syscall.rs`'s new `poll_keyboard_byte`,
factored out of the existing `TRY_READ_CHAR` handler and shared by
both), and if satisfied, stash the value into that task's *saved*
`Context.gpr[0]` and mark it runnable - it resumes exactly where its
blocking syscall left off, with the value already in `x0`,
indistinguishable from the syscall having simply returned it.
`syscall::dispatch` gained a 6th parameter, `frame: *mut Context` (AAPCS64
places it in `x5`, following `number`/`arg0..arg3` in `x0..x4`) - the
same pointer `rust_irq_handler` already gets via `mov x0, sp`, now
available to the SVC path too via one new line,
`mov x5, sp`, right before the existing `bl {rust_syscall_handler}` -
every syscall except the new one ignores it. A new syscall,
`READ_CHAR` (15): checks for a byte first (same non-blocking fast path
`TRY_READ_CHAR` already had), and only calls
`block_current_and_switch` if nothing's available yet.
`TRY_READ_CHAR` stays exactly as it was - not deprecated, still useful
for a caller that genuinely wants non-blocking semantics; the shell's
main loop is just no longer one of them, and its byte-count-only
comment explaining the old busy-poll (which cited
`qemu_device_region_safe`, a heuristic the later MADT/GICv3 work
already removed) is rewritten to describe the real mechanism instead.

**Why `block_current_and_switch` has to return `frame.gpr[0]`, not
just `0` or the byte: a real correctness subtlety, not a stylistic
choice.** `exceptions.rs`'s SVC trampoline unconditionally writes
`dispatch`'s return value into the frame's `x0` slot after the call
(`str x0, [sp, #0]`) - fine for a syscall resuming its own caller, but
`block_current_and_switch` just overwrote `*frame` with a *different*
task's entire saved context. Returning that task's own `x0` (already
sitting there post-overwrite) turns the trampoline's blind write into a
harmless no-op instead of clobbering the resumed task's real value -
the whole mechanism works without changing the trampoline's control
flow at all.

**A second real, pre-existing bug found by testing this, not
introduced by it - found on the very first real-hardware-shaped test,
on QEMU, before ever risking real Parallels hardware on it (same
staged, QEMU-first discipline as every scheduler/SVC-path change in
this project since the `x9` bug).** The very first `block_current_and_switch`
test crashed: `EXCEPTION vector=8 esr_el1=0x8200000f far_el1=0x5c75a000
elr_el1=0x5c75a000` - EC `0x20` (Instruction Abort, lower EL), IFSC
`0x0f` (Permission fault, level 3), `FAR_EL1`/`ELR_EL1` both exactly
task 1's `SP_EL0` value (its stack top), not anywhere inside its
actual 8-byte idle loop. Root cause: `exceptions.rs`'s SVC trampoline
("3:") was *never* designed to produce a frame that's a genuinely
interchangeable `Context` the way the IRQ path's "2:" trampoline
always has been - it saved `ELR_EL1`/`SPSR_EL1` at byte offset 248,
but `Context`'s real field order (`gpr`, then `sp_el0`, then
`elr_el1`, then `spsr_el1`) puts `sp_el0` there instead, with
`elr_el1` at 256; worse, "3:" never saved or restored `SP_EL0` at all,
since a synchronous SVC trap never needs it - hardware leaves EL0's own
`SP_EL0` untouched across the trap, so no previous syscall, always
resuming its own caller, ever needed to treat it as real state.
Harmless for every syscall before this one; the instant a blocking
syscall tried to load a *different* task's context through that
mismatched layout, task 1's real `SP_EL0` value landed in `ELR_EL1`
instead. **Fixed** by adding a genuine `SP_EL0` save/restore
(`mrs`/`msr sp_el0`) at the correct offset (248) and moving `ELR_EL1`/
`SPSR_EL1` to 256/264, exactly matching `Context`'s real layout and
"2:"'s own already-proven shape.

**Confirmed working, staged and re-verified at each step, the same
discipline as the relocating-loader milestone's `x9` fix - not "changed
it once and moved on":** (1) task-state scaffolding alone, both tasks
always runnable, verified byte-for-byte identical shell behavior on
QEMU; (2) the `frame` parameter wired through with nothing using it
yet, same zero-behavior-change verification; (3) the real
`block_current_and_switch`/wake-check, tested via a temporary
`blockread` shell command with block/wake events logged - the exact
crash above was found here, fixed, then re-verified: typing `blockread`
produced `TEMP-DEBUG task 0 blocking` immediately, *zero* wake events
during a real 4-second gap with no input (the direct proof this
genuinely blocks, not just "looks the same from outside"), then
`TEMP-DEBUG task 0 waking with value=0x78` and `got byte: 120` the
instant a key was sent - and the temporary command/logging were
removed once confirmed; (4) the shell's real main loop switched to
`read_char`, full interactive regression (typing, backspace, every
command, `selftest`) plus the full disk-command surface against a real
FAT32 image, both zero aborts in `-d int` cross-checks, and a direct
proof the idle task keeps running while the shell blocks: `uptime`
advancing from `254` to `384` ticks across a 3-second gap with nothing
typed. **Confirmed on real Parallels hardware too**, via
`make test-parallels` (`CMDS="selftest;help;echo hello world;uptime"`):
all four commands typed correctly through the new blocking path (every
keystroke now goes through it - the busy-poll no longer exists to fall
back to), zero corruption, correct multi-digit `uptime` (`1363` ticks).

**Still coarse, worth knowing before building on this:** one wait
reason (keyboard input) - the mechanism generalizes (`WaitReason` is a
real enum, `STATES` is already sized by `NUM_TASKS`), but nothing else
uses it yet; worst-case wake latency is still one tick period (20ms),
unchanged from before - this fixes *what* the scheduler does while
waiting, not *how fast* it notices, since keyboard input still has no
real interrupt path of its own (`xhci.rs` is still polled); task 1
never blocks and this doesn't change that; still no dynamic task
creation, so "another runnable task to switch to" is always just "the
idle task" today, not a real scheduling choice - the value here is
architectural correctness and a real mechanism to build on, not a
measurable performance win yet (this kernel doesn't optimize for idle
power anywhere else either).

## Dynamic task creation and `exec()`: a real `spawn` syscall, and a `Vec`/alloc hang found by testing

Closes the last real item in `docs/roadmap.md`'s parking lot phrased as
"running more than one loaded program, or reloading one without a
reboot." Before this, `tasks.rs`'s scheduler had exactly two fixed,
compile-time task slots (the loaded shell, and idle) - no way to start
a second program without rebooting.

**Naming, deliberately precise: this is `spawn`, not POSIX
exec-replaces-current-process.** The roadmap's own wording said
"exec()," so the shell command is named `exec`, but the mechanism
underneath (`tasks::spawn`) adds a new task *alongside* the caller - it
never replaces or stops it. `exec <path>` in the shell loads a program
from disk and starts it running concurrently; the shell that ran the
command keeps going right where it was.

**Reused the boot-time table-swap mechanism a second time, rather than
building a new incremental-remap primitive.** Making a freshly-allocated
region EL0-accessible while the kernel is already running under its own
tables sounds like it needs a new "add one mapping to the live table
set" operation - but this kernel already proved, at boot, that swapping
`TTBR0_EL1` to an entirely new table set while code is *actively
executing* under the old one is safe (that's literally how firmware's
own tables got replaced by this kernel's in the first place). Calling
`mmu::install_identity_map` again, later, with the same RAM span and
every existing task's EL0 region plus one new one appended, is the
identical operation done twice - not a new mechanism to get right, with
the new call's SVC handler holding IRQs masked throughout (same
reasoning that already makes `block_current_and_switch` safe). This
needed `install_identity_map` to take ownership of (and a new
`rebuild_with_el0_regions` to reuse) a stashed copy of the original UEFI
memory map and extra-device list (`mmu.rs`'s new `STORED_MEMORY_MAP`/
`STORED_EXTRA_DEVICES`), since boot services - and therefore any way to
reconstruct the memory map from scratch - are long gone by the time a
shell command can run this. `MAX_EL0_REGIONS` (and the `EL0_L2_TABLES`/
`EL0_L3_TABLES` pools sized from it) grew from a hardcoded 2 to 4 -
`install_identity_map`'s own region-building logic didn't need to
change at all, it already looped over however many entries the array
held.

**A runtime physical-page allocator, deliberately the simplest correct
thing.** `loader.rs`'s existing allocation (`boot::allocate_pages`) is a
UEFI boot-services API, gone the instant `exit_boot_services` returns -
nothing before this milestone could hand out RAM at runtime at all.
`tasks::allocate_runtime_region` is a bump cursor
(`NEXT_RUNTIME_REGION_TOP`), initialized once from `mmu::ram_span()` to
the top of discovered RAM and grown *downward*, one 2MB-aligned slot per
call. No destruction, no reuse, no free list - explicitly out of scope,
matching `mmu.rs`'s existing "each EL0 region must independently fit
inside one 2MB slot" invariant by construction, the same way
`loader.rs`'s original over-allocate-and-trim trick already guaranteed
it for task 0.

**`tasks.rs` grew past two fixed slots.** `NUM_TASKS` 2 → 4, `TaskState`
gained `Unused` (slots 2/3 start there; 0/1 stay exactly as before - no
change to `init()`), a new `REGIONS` array records each task's own
`(base, size)` once at creation time (needed to rebuild the full
`el0_regions` array for the `install_identity_map` call above), and a
new `tasks::spawn(context, region)` finds the first `Unused` slot,
installs the context and region, marks it `Runnable`, and returns its
index (or `SpawnError::NoFreeSlot` if all four are taken).
`next_runnable`'s existing wrap-around scan needed no change - it was
already written to generalize to any `NUM_TASKS` during the blocking-
primitives milestone, specifically so this would be true here.

**`loader.rs` split so its ELF-parsing core is reusable independent of
how the destination region was obtained** - `elf_region_size` (pure
parsing) and `populate_region` (copy segments, apply
`R_AARCH64_RELATIVE` relocations, compute the real entry point) used to
be one function coupled to boot-time `boot::allocate_pages`. Boot-time
`load()` now calls both explicitly around its own allocate/free dance,
unchanged in behavior; the new `spawn` syscall calls the same two
functions around `tasks::allocate_runtime_region` instead.

**A new syscall, `spawn` (16, the next free number after
`read_char`).** `syscall.rs`: reads the whole program file via the
shared `FS` instance into a fixed 128KB EL1 staging buffer (no `alloc`
at runtime, same "fixed static buffer" discipline as everything else in
this kernel post-`exit_boot_services`), refuses anything larger than
that outright rather than loading it truncated (matches `cp`'s "a
partial copy is a wrong copy" reasoning, not `cat`'s "truncated display
is still useful" one - a truncated *program* would just crash), then
runs the `elf_region_size` → `allocate_runtime_region` →
`populate_region` → `tasks::spawn` →
`mmu::rebuild_with_el0_regions` sequence above. Any failure at any step
(bad ELF, no free slot, disk error) returns `SPAWN_ERROR` - nothing
already-running is touched until the very last step
(`tasks::spawn`), so there's no partial state to unwind on failure.

**A real, significant bug found by testing, not review: `Vec`-based
program-header parsing hangs completely when called from this new
runtime path - no exception, no output, `-d int` shows zero aborts.**
`parse_program_headers` (called by `elf_region_size`) had always used
`Vec::with_capacity`/`.push` - correct when `loader.rs` only ever ran
during boot services, where the global allocator is fully valid, but
this milestone is what first made `elf_region_size` reachable from a
*runtime* syscall, long after `exit_boot_services` made that same
allocator boot-services-backed and invalid. Unlike every other post-exit
misuse this project has hit, this one doesn't panic, fault, or return an
error - it just hangs, silently, with the rest of the system frozen too
(this all runs inside an SVC handler with interrupts masked). Diagnosed
by adding temporary step-boundary debug prints inside the new
`spawn_program` and re-testing, isolating the freeze to the very next
call after "read N bytes" succeeded. **Fixed** by giving
`parse_program_headers` a fixed-capacity `ProgramHeaders` struct
(`[ProgramHeader; MAX_PROGRAM_HEADERS]`, 16) instead of a `Vec`, with a
real `LoaderError::TooManyProgramHeaders` for the (currently
unreachable in practice) case of an ELF exceeding that bound, rather
than silently truncating.

**Confirmed working end to end on QEMU, staged the same way as every
scheduler/SVC-path change since the `x9` bug - each stage independently
regression-tested before the next depended on it:** the refactor stages
(`loader.rs` split, runtime allocator + `mmu.rs` generalization,
`tasks.rs`'s `Unused`/`spawn`) each produced byte-for-byte identical
existing behavior before any of it was wired together; the `spawn`
syscall itself, first exercised via a temporary throwaway shell command
that spawned the shell binary against itself (removed once confirmed),
showed the second `install_identity_map` rebuild logged, the newly
spawned instance's own "Ouroboros userland shell" banner printing (real,
independent task execution, not just "didn't crash"), and the original
shell staying responsive throughout; the real, permanent `exec <path>`
command then got a full regression pass - every existing command
(`help`, `selftest`, `uptime`, the full `ls`/`cat`/`cd`/`mkdir`/`write`/
`cp`/`mv`/`rm`/`rmdir` disk-command surface), `exec`'s own error cases
(missing argument, nonexistent file), and a real successful `exec`
leaving two tasks alive - `uptime` confirmed still advancing (269 → 509
ticks) across the whole sequence with a second task running throughout,
zero aborts in `-d int` cross-checks. **A known, expected artifact, not
a new bug**, observed while two shell instances were alive at once:
keystrokes get split unpredictably between whichever instance happens
to be `Blocked(WaitReason::Keyboard)` at the tick a byte arrives -
exactly the "routing keyboard input to a specific blocked task" gap
already named out of scope for the blocking-primitives milestone, not
something this feature was meant to fix. **Update: fixed the same day -
see "Keyboard input routing" immediately below.**

**Confirmed on real Parallels hardware too, honestly split by what's
actually reachable there.** `spawn`'s error paths are confirmed working
end to end via `make test-parallels`: `exec /efi/ouroboro/sh.bin`
correctly reported "no filesystem mounted this boot" (the same shared
message every `fs_*` command already gives there), with `uptime`
continuing to advance normally afterward (1523 → 1960 ticks) and no
crash. The actual **success** path - a second program really loading
and running - could not be exercised on real hardware at all, for a
reason that has nothing to do with this feature: Parallels has no
working virtio-blk driver of any kind yet (a pre-existing, already-
documented gap, see `CLAUDE.md`'s "Next milestone" section), so there's
no filesystem to load a second program *from* there regardless. Every
other piece this milestone touches - the `mmu.rs` region-count
generalization, the stashed memory map, the 4-slot scheduler - is
exercised by ordinary boot and interactive shell use on real hardware,
which continues to work correctly with no regression.

**Still coarse, worth knowing before building on this:** no task
destruction - a spawned task's slot and its allocated RAM are permanent
for the rest of the boot, the bump allocator never frees **(Update:
fixed - see "Task destruction" further below; tasks can exit, slots
free, and the allocator reclaims in LIFO order)**; at most 4
total task slots, hardcoded; a spawned task gets the same fixed
round-robin share as any other runnable task, no priorities. (Keyboard
input routing, the one item this list used to name as unsolved, is
fixed - see immediately below.)

## Keyboard input routing: a real bug in the wake-check, fixed the same day dynamic task creation shipped

Reported directly by the user immediately after trying `exec` live:
typing a command right after spawning a second interactive shell showed
letters arriving split between the two - `uptime` typed once, one
instance's prompt showing `ptime`, the other showing `unknown command:
ptime` from a stray `u`. Root cause, found by reading `on_tick`'s
wake-check loop (`tasks.rs`) rather than guessed: `WaitReason::Keyboard`
polls `syscall::poll_keyboard_byte`, which *destructively* consumes one
byte from the console/xHCI driver the instant anything asks for one - it
has no notion of which task "should" get it. The wake-check polled every
`Blocked` task's reason once per tick, in index order, so a single
keystroke went to whichever task happened to still be `Blocked(Keyboard)`
at that exact tick - and which task that is flips constantly as tasks
trade being blocked and running, exactly matching the observed
letter-by-letter split.

**Fix: designate exactly one task, `INPUT_OWNER_TASK` (hardcoded to task
0, the boot-loaded shell), as the only task whose `Blocked(Keyboard)` the
wake-check will ever actually poll hardware for.** Hardcoded rather than
a runtime-settable value deliberately - task 0 is never destroyed (task
destruction exists now - see "Task destruction" further below - but
task 0 specifically is refused by the `EXIT` syscall for exactly this
reason), so it's always a valid, permanent
owner, and there's no job-control mechanism (`fg`/`bg`, a
controlling-task handoff) that could ever legitimately reassign this
today; a settable knob would just be unused. Needed no new buffering: a
byte a non-owner task would have consumed instead now simply stays
queued in the console/xHCI driver's own hardware buffer - untouched,
since the wake-check now skips polling for it entirely - until the owner
task's own wait is what asks. A task other than the owner that blocks on
`Keyboard` just stays blocked, honestly, until this kernel ever grows
real job control - it behaves like a genuine background task with no
input, not a second terminal racing the first.

**Confirmed fixed via the identical live QEMU test that surfaced the
bug**, run twice - once with the bug present (transcript showed exactly
the `ptime`/`u` split described above), once after the fix (`exec`, then
`help`, `uptime`, `echo routing works now`, `uptime` again, all arriving
completely clean, the second shell's own banner visible but not stealing
any further input) - then a full disk-command regression pass
(`selftest`/`ls`/`mkdir`/`write`/`cat`/`rm`/`rmdir`/unknown-command) with
the second task still alive throughout, zero aborts in `-d int`
cross-checks both times. **Confirmed on real Parallels hardware too**,
via `make test-parallels` (`help`/`echo`/`uptime`) - this fix touches the
same wake-check loop that's the *only* keyboard input path on real
hardware (the xHCI driver), so a regression there would have broken
ordinary single-task typing, not just the multi-task case; confirmed
clean, `uptime` reporting a real, correctly incrementing value (1067
ticks). The actual multi-task routing fix itself couldn't be re-exercised
on real Parallels hardware, for the same pre-existing, unrelated reason
`exec`'s success path couldn't be in the milestone above: no working
virtio-blk driver there yet, so there's no way to `exec` a second program
from disk on real hardware at all today.

**Still a real limitation, not fully solved:** only one task can ever
receive keyboard input - any other task blocked on it waits forever with
no way to become the owner short of a future `fg`/job-control mechanism.
Good enough for "a background task doesn't corrupt the foreground
shell's input," not a real multi-program input model. **(Update: solved
- see "Job control" further below; `fg <n>` reassigns ownership at
runtime, with automatic revert to task 0 when the owner dies.)**

## Output redirection (`>`/`>>`): pure shell-side, and a real bug found where the syscall ABI's buffer cap meets append

The last open item from the original write-support arc
(`docs/roadmap.md`'s parking lot): `cmd > file` (create/overwrite) and
`cmd >> file` (append) now work for every builtin. Like `cp`, this is
**zero kernel changes** - it composes the two syscalls that already
exist (`fs_read_file`/`fs_write_file`), no `syscall-abi` change, no
SVC-path/scheduler/page-table risk. Everything lives in
`shell/src/main.rs`; `docs/shell-commands.md` has the user-facing
reference (its new "Output redirection" section).

**Design, in three pieces.** (1) `run_line` scans the line's whitespace
tokens for the first standalone `>` or `>>` before dispatch
(`parse_redirect` - the operator must be its own token, `echo hi>f` is
one word, same no-quoting tokenization rule as everywhere else) and
splits the line there, so `write`/`cp`/`mv` (which re-parse the raw
line) never see the operator. (2) Command *output* goes through an
explicit `Output` sink (`Console` or `Capture`) passed down to the
handlers by `&mut`, exactly like `cwd` already is - a module-level
"current sink" static is impossible in this program, which is
deliberately built with no static mutable state at all (`linker.ld`
asserts `.data`/`.bss` empty). Command *error* messages deliberately
bypass the sink and keep printing to the console - the POSIX
stdout/stderr split, without needing a second sink to represent stderr.
Like `sh`, the target is created/truncated even if the command printed
nothing (or errored): `> f.txt` with no command at all legitimately
creates an empty file, which is also real reuse of
`valid_user_range_allow_empty` - the empty-write case phase 6 already
had to make valid. (3) After dispatch returns, `finish_redirect` writes
the capture: full replace for `>`; for `>>`, read-concatenate-rewrite
(the kernel has no append primitive - a known FAT32-layer gap this
deliberately does *not* close, composing full-replace writes instead,
same narrowest-useful-case discipline as everything else). Both
overflow cases refuse outright and write *nothing* - `cp`'s "a partial
copy is a wrong copy" reasoning, not `cat`'s truncate-and-note. One
`cat`-specific call: its tidy-terminal trailing newline and its
truncation notice are display niceties that stay console-only, so
`cat a > b` copies `a`'s bytes exactly rather than appending bytes `a`
never contained.

**A real bug found immediately by testing, not review: the syscall
ABI's per-buffer cap (`MAX_USER_LEN`, 512 bytes, `syscall.rs`) made
append silently behave as overwrite.** The first cut used a 1024-byte
combined buffer for `>>`'s read-back - and `valid_user_range` rejects
any `(pointer, length)` pair longer than 512, returning the rejection
as the same `FS_ERROR` sentinel a genuinely missing file produces. The
append path treats "no such file" as "append to empty" (that's how
`>> new.txt` creates files), so the existing content was silently
discarded and every `>>` acted like `>` - caught because the very first
QEMU append test showed `cat f.txt` returning only the appended line.
Fixed by sizing the combined buffer *at* the kernel's cap
(`APPEND_BUFFER_SIZE = 512`, with a doc comment naming the constraint);
a bigger buffer could never have been written back anyway, since
`fs_write_file`'s data argument is bounded by the same 512-byte check.
The capture buffer (`CAPTURE_SIZE`) is 512 for the same reason -
anything capturable is guaranteed writable.

**A second, smaller toolchain instance of a known constraint, worth
recording:** the first build failed to *link* - `R_AARCH64_ABS64
cannot be used against local symbol` out of the prebuilt `libcore` -
because `parse_redirect`'s `&line[..offset]` str-slicing pulls in
`core::str::slice_error_fail`'s panic path, which formats the offending
string with enough of `core::fmt` to drag non-PIC libcore objects into
the link. Same root class as the documented "userland must build in
release" constraint from the relocating-loader milestone, hit here
*even in* release. Fixed with non-panicking `.get()` slicing (the
offsets are guaranteed char boundaries anyway - the token is a
whitespace-split subslice of the line), which pulls none of that in.

**Confirmed working end to end via the same piped-stdin QEMU technique
as every prior interactive milestone, against the real FAT32
`esp.img`:** create (`echo hello > f.txt` → `cat` shows it), overwrite
(only the new content remains), append (`hello`/`more`/`third line
here` accumulating correctly across two `>>` calls), create-by-append
(`>> new.txt`) and create-empty (`> empty.txt`, `ls` confirms both),
`uptime > u.txt` and `ls > l.txt` (correct real contents), `cat f.txt >
g.txt` (byte-exact copy), and every error case: missing target (`echo
hi >`), extra token (`echo hi > a b`), missing parent (`ls > /nope/f`)
- all clean, specific messages. **Both overflow refusals confirmed
real, one organically:** six 100-character `echo ... >> big.txt`
appends in a row - the first five legitimately fit (510 bytes ≤ 512),
the sixth correctly refused with the file's content intact; the capture
overflow (unreachable organically - no builtin can emit > 512 bytes)
was verified with the established temporarily-force-then-revert
technique (`CAPTURE_SIZE = 16`: `help > h.txt` refused, `h.txt`
confirmed never created via `ls`, a small `echo short > s.txt` still
worked, force reverted). **Persistence confirmed by an actual reboot**
(`echo persisted content > keep.txt`, fresh QEMU boot, `cat` shows it).
`make run`'s FAT16 (no mount) degrades to the shared "no filesystem
mounted" message for both `>` and `>>`, with unredirected commands
unaffected. Zero aborts (`Data Abort`/`Prefetch Abort`/`Undefined
Instruction`) in `-d int` cross-checks across every session above.

**Confirmed on real Parallels hardware too, via `make test-parallels` -
which needed a real extension to type `>` at all:** the script's
scancode table had no shifted characters, so `scripts/test-parallels.sh`
gained a held-Shift chord (`--event press`/`--event release` around the
base key's scancode - `prlctl`'s flagless form sends a full
press+release for one key and can't express a chord). That round trip
confirmed the whole input path for a chorded character end to end
(synthetic keyboard → HID modifier byte → `xhci.rs`'s
`keycode_to_ascii` shift mapping → the shell's parser), and redirection
itself correctly hit the shared `NO_FS` message on real hardware
(Parallels still has no disk driver of any kind - the pre-existing,
unrelated gap every `fs_*` command already has there), with `uptime`
advancing normally afterward. One dropped keystroke in the transcript
(`f.xt` for `f.txt`) is the already-documented `send-key-event` timing
artifact, not a regression.

**Still coarse, worth knowing before building on this:** `>>` is
bounded append (existing + new ≤ 512 bytes), not real append - a
genuine append/offset-write syscall stays a known gap; capture is 512
bytes, refuse-not-truncate; no `2>` (errors always go to the console,
by design, but there's no way to redirect them); ~~no pipes~~ (**done - see "Pipelines" above**: `builtin | program`
over IPC, one pipe per line), no input
redirection (`<`), no quoting, no glued operators (`cmd>f`).

## xHCI multi-device support: every connected device enumerated, classified, and kept addressed - the groundwork the USB mass-storage milestone needs

`xhci.rs`'s deliberate one-port/one-device/one-slot scope (see the USB
HID keyboard milestone above) is lifted: the port scan now enumerates
**every** connected device (bounded by a 4-entry pool), reads and logs
each one's Device and Configuration descriptors - every interface's
class/subclass/protocol printed, with an explicit callout for mass
storage (class `0x08`) - and keeps every successfully-set-up device
concurrently addressed in its own slot, instead of abandoning
everything that isn't a boot-protocol keyboard. This is the prerequisite
the USB-mass-storage roadmap item names (keyboard + storage stick must
coexist on the controller), done while waiting on a USB 3.x stick for
that milestone's own scoping check - which this also upgrades for free:
a passed-through stick's boot log now shows its decoded interfaces (the
whole storage-scoping answer), not just a connected port.

**What actually had to change, found by reading the code rather than
assumed:** two pieces of per-device DMA state were single statics that
only ever worked *because* the old scan abandoned non-keyboards. The
Output Device Context (the memory the xHC itself writes a device's
state into, via its DCBAA entry) and the EP0 transfer ring (each
device's Address Device command declares its own dequeue pointer; the
old shared ring needed an explicit rewind dance per candidate, see the
old `ep0_enqueue = 0` comment) both became 4-entry pools. The old
all-in-one `Device` struct split into `Xhci` (controller-global:
command/event ring state, `db_base`/`ir0_base`, `ctx_dwords`),
`DeviceSlot` (per-device: port/speed/slot ID/EP0 ring position; pool
index = which pool entries it owns), and `KeyboardState` (interrupt
ring + report edge-detection state, at most one). Genuinely shared
scratch (`INPUT_CONTEXT`, `CTRL_BUF`) stays shared - operations are
strictly serialized, one command/transfer in flight ever.

**Two ordering/routing decisions worth knowing the reasons for:**
(1) the keyboard is *activated* (SET_CONFIGURATION, SET_PROTOCOL,
Configure Endpoint, first buffer arm - all moved into a post-scan
`activate_keyboard`) only after the whole scan finishes - once the
interrupt endpoint is armed, a keystroke queues a Transfer Event at any
moment, which must not interleave with a later device's setup-time
waits. (2) Transfer-event handling now filters by slot ID and endpoint
DCI (both carried in the event TRB): `wait_transfer_event` takes the
expected slot and skips-and-logs mismatches, and `poll_key` only
accepts events matching the keyboard's slot + DCI - with multiple live
slots that's routing, not luck, and it's the shape bulk-transfer
completions will need. Non-keyboards deliberately get no
`SET_CONFIGURATION` (descriptors are fully readable in the addressed
state - that's how any OS picks a configuration - and activating one
belongs to whatever driver eventually drives the device). A failed
port's pool entry is *sacrificed*, not reused: its Output Device
Context may already be bound into hardware's DCBAA for an enabled slot,
and handing the same context to the next device would be exactly the
two-slots-one-context corruption the pools exist to prevent.

**Confirmed working end to end, staged, on both platforms.** Stage 1
(the pure state refactor, scan semantics unchanged) was regression-
tested on QEMU before any scan change: `usb-kbd` keystrokes injected
via the monitor's `sendkey` typed real commands correctly, zero aborts.
Stage 2 (the full scan) got a new three-device rig, `make
run-usb-multi` (`usb-kbd` + `usb-tablet` + `usb-storage` over a scratch
image on one xHCI controller): the boot log showed all three enumerated
and correctly classified - the storage stick as `class=0x08
subclass=0x06 protocol=0x50` (SCSI-transparent, Bulk-Only Transport -
exactly the interface the storage milestone will drive) with the
callout line, the tablet as a non-keyboard HID device left addressed,
the keyboard activated after the scan and typing `uptime`/`selftest`
correctly. Single-device (`run-usb-kbd`) and no-USB (`make run`)
regressions both clean, zero aborts everywhere. **Real Parallels
hardware** (`make test-parallels`): the virtual mouse (vendor `0x203a`,
HID protocol `0x02`) enumerated and left addressed, the keyboard
classified and activated, and a full command set (`help`, `selftest`,
`uptime`, `echo hi`) typed correctly with zero dropped keystrokes -
the risk-class gate, since every xHCI bug so far has been
real-hardware-only, passed on the first attempt.

**Still coarse, worth knowing before building on this:** no hot-plug
(the scan still runs exactly once at boot - a device attached *after*
it is invisible until the next boot; not what hid the USB 2.0 stick,
though - that was EHCI routing, see the diagnostic above, disproved as
late-attach by its own 6-second recheck. **Update: the one-shot scan
IS what nearly hid the USB *3.x* stick** - Parallels attaches
passthrough devices a few seconds after VM start, and the stick was
only caught because a temporary diagnostic delay happened to push the
scan late enough; see `docs/roadmap.md`'s storage item - the
mass-storage milestone needs a delayed/repeated scan or minimal
hot-plug); no hubs; at most 4 devices;
only the first keyboard found is
driven; addressed non-keyboard devices are inert - enumerated and held,
with no driver to do anything with them yet (that's the next
milestone); interrupt-endpoint stall recovery unchanged.

## Directory extension: full directories grow instead of failing - and the rmdir leak it would have created, fixed in the same pass

The last small FAT32 item from the parking lot, done as the deliberate
light task while waiting on the USB 3.x stick. `fat32.rs`'s
`insert_dir_entry` - the single choke point every entry-creating
operation goes through (`mkdir`/`touch`/`write`/`cp`/`mv`) - now grows
a directory by one cluster when every existing slot is taken, instead
of returning `Error::DirectoryFull` (the variant is deleted outright -
unconstructable code, not a deprecated path). Ordering follows the
module's established discipline: the fresh cluster is claimed
end-of-chain in the FAT and zeroed (an unzeroed directory cluster
would present garbage as entries - `DIR_ENTRY_END` is `0x00`) *before*
the old last cluster links to it, so a failure partway leaves at worst
a claimed-but-orphaned cluster, never a chain pointing at garbage. All
four building blocks (`find_free_cluster`/`write_fat_entry`/
`zero_cluster`/the slot-search loop itself) already existed - this was
ordering, not new machinery.

**The real correctness piece was in `rmdir`, not `mkdir`: it freed
exactly one cluster.** Correct while every directory was single-cluster
*by construction* - a silent leak the moment an emptied-out
multi-cluster directory got removed, which is exactly what
filling-then-clearing a directory produces. Fixed by freeing the whole
chain - and `rm` and `write_file`'s overwrite path already carried the
identical chain-freeing loop, duplicated, so all three now share one
`free_chain` helper (which reads each FAT entry *before* zeroing it,
or the walk would destroy its own links).

**Confirmed organically on QEMU against the real `esp.img` - the
512-byte clusters make this genuinely reachable, not theoretical
(16 entries per cluster, 2 taken by `.`/`..`):** filled a fresh
subdirectory with 20 files (extension triggered at the 15th), `ls`
shows all 20, `write`/`cat` round-tripped content on a file whose
entry lives in the extension cluster, the *root* directory was
extended the same way (18+ entries - root is an ordinary cluster
chain, same code path), and `/EFI/ORBS/INIT.CFG` re-read byte-identical
afterward. **Persistence and the rmdir fix confirmed across a real
reboot:** all 20 files and the extended-cluster content survived a
fresh boot; `rm` × 20 then `rmdir` succeeded on the now-empty
two-cluster directory; a second directory then refilled to 18 entries
cleanly - freed-cluster *reuse* being the observable consequence of
the FAT entries actually getting zeroed - and everything cleaned back
to a bare root. `make run`'s FAT16 still degrades to the shared
no-filesystem message; a Parallels boot smoke (`uptime`, 1161 ticks)
confirmed no kernel regression on real hardware (the fat32 path itself
is unreachable there - no disk driver, the standard honest split).
Zero aborts in `-d int` cross-checks across every session.

**Still coarse:** directories never *shrink* - an emptied extension
cluster stays linked until the directory is removed (FAT drivers
generally don't shrink directories either); everything else about the
module's scope (8.3-only, no LFN, first-FAT32-partition-only) is
unchanged.

## Task destruction: a real `exit` syscall, and a second real userland program to prove it

The `exec` milestone's longest-standing "still coarse" item - a spawned
task's slot and RAM were permanent for the rest of the boot - is
closed: the `EXIT` syscall (17, `arg0` = exit code) destroys the
calling task, its slot becomes spawnable again, its EL0 mapping is
dropped (a fresh `mmu::rebuild_with_el0_regions`, the same masked-IRQ
rebuild `spawn` already proved safe), and its RAM goes back to the
runtime bump allocator when LIFO order allows
(`tasks::free_runtime_region` - cursor restored iff the region was the
most recent allocation; anything else leaks, deliberately - a bump
cursor, not a free list). Tasks 0 (the boot shell - the sole keyboard
owner, see `INPUT_OWNER_TASK`) and 1 (idle) are refused with
`EXIT_DENIED`, the only case where `EXIT` ever returns to its caller.
`tasks::exit_current_and_switch` mirrors `block_current_and_switch`'s
proven frame-overwrite shape exactly - same trampoline contract, same
return-`frame.gpr[0]` subtlety - with the one difference that the
current context is discarded instead of saved.

**The test vehicle is a real deliverable: `hello/`, the second userland
program this project has ever had.** A spawned *shell* can never
exercise `exit` (it blocks forever on keyboard input only task 0
receives), so proving destruction needed a program that runs to
completion by itself - and a second program is also the living proof of
"the shell is just a program," loaded and relocated by the identical
mechanism. Zero new toolchain work: `.cargo/config.toml`'s
`aarch64-unknown-none` flags (PIE, the shared `shell/linker.ld` - the
`-T` path is workspace-relative) apply to any crate on that target;
`hello/` is ~70 lines, staged as `\EFI\ORBS\HELLO.BIN` by a `hello-bin`
Makefile target mirroring `shell-bin` (same release-only constraint).
The shell gained an `exit` builtin - always refused for the boot shell
itself, with a clear message, but the reference for how a replacement
program ends itself.

**Confirmed working end to end on QEMU, with the reclaim directly
observable, not inferred:** three consecutive `exec /EFI/ORBS/HELLO.BIN`
runs each printed hello's banner, `task 2 exited (code 0)`, and - the
LIFO reclaim made visible by the identity-map rebuild log - landed at
the *same region base* (`0x5fe00000`) every time, with the region
dropping to `(0, 0)` after each exit. A long-lived `exec SH.BIN` then
held slot 2 while two more hello runs cycled through slot 3 at the next
base down (`0x5fc00000`), also reclaimed and reused. `exit` at the boot
shell printed the refusal and the shell kept running; `uptime`/
`selftest` clean; zero aborts. **Confirmed on real Parallels hardware**
via `make test-parallels`: the typed `exit` → the refusal message
rendered on the real framebuffer console (the one reachable path there
- no disk driver means no way to spawn an exitable program yet, the
standard honest split), `uptime` advancing normally.

**Still coarse, worth knowing before building on this:** exit is
voluntary only - nothing can end *another* task (`kill` belongs with a
future job-control mechanism); exit codes carry no meaning beyond the
kernel's log line (no `wait()`/reaping - nothing waits on tasks);
the allocator reclaim is LIFO-or-leak, so pathological spawn/exit
orderings can still strand regions for the rest of the boot; tasks 0/1
can never exit, by design.

## Splitting the collapsed FS_ERROR sentinel: userland finally learns why an operation failed

The gap flagged in every milestone since phase 4 - all `fs_*` syscall
handlers mapped `Err(_)` to one `FS_ERROR` sentinel, so the shell
printed guess-lists ("mkdir: failed (already exists, bad name, parent
missing, or disk full)") - is closed. `syscall-abi` gained a reserved
top band of named codes (`FS_ERR_NOT_FOUND`/`NOT_A_FILE`/
`NOT_A_DIRECTORY`/`INVALID_NAME`/`ALREADY_EXISTS`/`NOT_EMPTY`/
`IS_ROOT`/`DISK_FULL`/`IO`, continuing down from the existing
`NO_FS = MAX-1`), plus the one derived predicate callers actually
need: **any value `>= FS_ERR_MIN` (`MAX-15`, with headroom) is an
error** - necessary because `fs_read_file`/`fs_list_dir` return
arbitrary byte counts on success, so exact-match arms can't cover a
multi-code error space. Backward compatible: `FS_ERROR` (`MAX`) stays,
now only the generic fallback for argument-validation rejections.
Kernel-side it's one mapping function (`syscall.rs::fs_error_code` -
the can't-happen-post-mount mount-shape variants map to `FS_ERR_IO`
rather than being omitted) and eight mechanical call sites; shell-side
one shared `print_fs_error(cmd, code)` (integer match + literal
`print_str`, the established relocation-safe idioms) replaced every
guess-list message.

**A real mis-mapping surfaced the moment the messages got specific -
exactly the class of thing the collapsed sentinel had been hiding:**
`rmdir /` reported "invalid name" instead of "can't remove the root
directory", because `fat32::rmdir`'s `split_parent` call failed with
`InvalidName` *before* the root check ever ran. Invisible for as long
as both causes collapsed into the same vague message; fixed at the
source (`split_parent` returning `None` in `rmdir` *means* the path is
the root - an empty path can't get past the syscall boundary - so it
maps to `CannotRemoveRoot` now). The redirect append path also got
sharper for free: `>>` only treats `FS_ERR_NOT_FOUND` as
"create the file", instead of treating *every* read failure that way.

**Confirmed on QEMU with one organic trigger per message:** `mkdir
/EFI` → "already exists", `mkdir /nope/x` → "no such file or
directory", `mkdir waytoolongname` → "invalid name", `cat /EFI` → "is
a directory", `cd /EFI/ORBS/INIT.CFG` → "not a directory", `rmdir
/EFI` → "directory not empty", `rmdir /` → "can't remove the root
directory" (after the fix above), `rm /EFI`/`touch /EFI`/`echo hi >>
/EFI` → "is a directory" - plus a full happy-path regression
(write/cat/cp/mv/ls/rm/rmdir round trip, `selftest`) byte-identical,
`make run`'s FAT16 still printing the shared `NO_FS` message, and a
real-Parallels smoke (`ls` → the no-filesystem message, typing
unregressed). Zero aborts in every `-d int` cross-check.

**Still collapsed, deliberately:** ~~`SPAWN_ERROR` (bad ELF / too large /
no free slot / disk error) - same pattern, separate small follow-up if
ever needed.~~ **Done, same day:** `spawn` returns the ordinary
`FS_ERR_*` code for read failures and
`SPAWN_ERR_BAD_ELF`/`SPAWN_ERR_TOO_LARGE`/`SPAWN_ERR_NO_FREE_SLOT`
for the rest (all in the same `>= FS_ERR_MIN` band); a failed spawn now
also gives its allocated region back (`free_runtime_region` - always
the LIFO case, since a failed spawn's allocation is the most recent).
And `mv` gained real `mv`'s move-into-directory shortcut, shell-side
(destination probed with `cd`'s `fs_list_dir` trick, basename appended;
`mv x x` refused). Both confirmed on QEMU: `mv f1 d1` → `ls d1` shows
it, `mv d1/f1 /` back out, self-move refused for files and directories,
`exec /nope` → "no such file or directory", `exec /EFI` → "is a
directory", `exec INIT.CFG` → "not a loadable program (bad ELF)", two
spawned shells then a third program → "no free task slot" - zero
aborts.

## Job control: `kill <n>` and `fg <n>` - a spawned shell is a real nested interactive session now

The last piece of the task-lifecycle arc (spawn → observe → exit →
destroy-another → hand over the terminal), and the milestone that turns
`exec` from a demo into something usable: `exec /EFI/ORBS/SH.BIN` then
`fg 2` is a genuine nested shell session - typing goes to the spawned
shell (its own separate `cwd` state proves it: `pwd` there answers `/`
while the outer shell sits in `/EFI`), and its `exit` (which *works*
for tasks ≥ 2) hands the keyboard straight back to the boot shell.

**The design center is one small state change with an invariant:**
`INPUT_OWNER_TASK` (the keyboard-routing fix's hardcoded `const 0`)
became a runtime `AtomicUsize` - the wake-check's single comparison
reads it, `FG` (syscall 20) stores it, and **any death of the current
owner reverts it to task 0** (`revert_input_owner_if`, wired into both
the `EXIT` and `KILL` teardowns). Task 0 can never die - `EXIT` and
`KILL` both refuse it - so the revert target is always valid: the same
permanence argument the original hardcoding relied on, now load-bearing
for the revert too. `KILL` (19) is `exit`'s teardown minus the context
switch (a non-current task isn't executing - single core, IRQs masked
during SVC; it's parked at an `eret` boundary, so its saved context is
simply discarded): log, free region (LIFO-or-leak), revert owner if
held, slot → `Unused`, one mmu rebuild. Both syscalls validate by
index (`TASK_ERR_PROTECTED` for 0/1 - though `fg 0` is allowed as an
explicit "give it back"; `TASK_ERR_NO_SUCH_TASK` otherwise), with the
two codes joining the reserved error band (`FS_ERR_MIN` moved from
`MAX-15` to `MAX-31` to restore headroom - safe, both ABI sides import
the floor from the shared crate). Shell-side: `kill`/`fg` builtins and
the shell's first numeric-argument parser (`parse_u64`, a hand-rolled
digit loop).

**A real, documented limitation, deliberate:** `fg` to a task that
never reads keyboard input strands the keyboard until that task exits
on its own - there is no interrupt key (no Ctrl+C/SIGINT concept
anywhere in this kernel). `fg` is for interactive programs; the escape
hatch for everything else is "don't fg it, kill it." Recorded in
`docs/shell-commands.md` up front rather than discovered later.

**Confirmed working on QEMU, the full story in one session:** `cd
/EFI` in shell 0 → `exec SH.BIN` → `ps` (task 2 blocked) → `fg 2` →
`pwd` answers `/` (shell 2's own cwd - proof input moved, not assumed)
→ `ps` from *inside* shows task 0 blocked/task 2 runnable (the roles
visibly flipped) → `echo` round-trips → `exit` → the kernel's `task 2
exited` line → `pwd` answers `/EFI` again (ownership reverted) → `ps`
shows slot 2 unused. Separately: `kill 2` on a background shell
(logged, slot freed), and every error path (`kill 0`/`kill 1`/`fg 1` →
protected; `kill 3`/`fg 9` → no such task; missing/non-numeric
arguments → usage lines). Zero aborts. **Confirmed on real Parallels
hardware** (`make test-parallels`): `ps` plus the protected/no-such-task
error paths render correctly, typing unregressed - the success path
needs a spawnable second program, which still needs a disk driver
there (the standing gap).

**Still coarse, worth knowing before building on this:** ~~no
`wait()`/reaping (exit codes still go nowhere but the log line)~~
**(closed the same day - see the wait/reaping paragraph below)**; ~~no
interrupt key (the `fg`-a-non-reader strand above)~~ **(closed the
same day - see the Ctrl+C paragraph below)**; `bg` doesn't exist
because it isn't needed - non-owners run freely, background is the
default state; one keyboard, one owner, no per-task terminals.

**Same-day follow-up: the Ctrl+C escape hatch.** The
`fg`-a-non-reader strand is closed: Ctrl+C (`0x03`, ETX) typed while a
non-boot-shell task owns the keyboard is intercepted at
`syscall.rs::poll_keyboard_byte` - the single choke point every
keyboard path funnels through (wake-check, `READ_CHAR`'s fast path,
`TRY_READ_CHAR`) - reverting ownership to task 0 and swallowing the
byte, with a kernel log line for feedback. Deliberately *reclamation,
not a signal*: nothing is delivered to the foregrounded task, it keeps
running in the background (`kill` it if it should die). Two supporting
pieces: `xhci.rs`'s `keycode_to_ascii` gained the Ctrl modifier
(Ctrl+A..Z map to the classic C0 control bytes - it only handled Shift
before, so a real keyboard couldn't produce `0x03` at all), and the
shell's line editor now ignores *all* unhandled control bytes instead
of appending them invisibly to the buffer (so Ctrl+C while the boot
shell owns the keyboard is a clean no-op, and stray control bytes
can't silently corrupt a command line anymore).
`scripts/test-parallels.sh` gained a `CTRL-C` pseudo-command (a real
held-Ctrl chord via `--event press/release`, same technique as the `>`
Shift chord). **Confirmed on QEMU end to end:** `fg 2` -> `pwd`
answering `/` (input in the nested shell) -> a raw `0x03` -> the
kernel's reclaim line -> `pwd` answering `/EFI` (input back in the
boot shell) -> `ps` showing task 2 still alive in the background ->
`kill 2`; a no-op Ctrl+C with task 0 owning printed nothing and left
the next command clean; zero aborts. **Confirmed on real Parallels
hardware:** `echo one` / the `CTRL-C` chord / `echo two` typed cleanly
with no stray `c` between them - direct proof the Ctrl modifier
mapping produced `0x03` through the real xHCI path (a broken mapping
would have typed a plain `c`) and the shell ignored it.

**And the same day again: `wait()` and reaping - exit statuses are
real now.** `exit` leaves `TaskState::Zombie(status)` (code masked to
a byte, POSIX-style, so it can never collide with the ABI's error
band) holding the *slot* - not memory, which is still freed at death
exactly as before - until a `wait <n>` (syscall 21) collects the
status, which is what reaps the slot back to spawnable. `kill` still
reaps immediately, deliberately - the killer already knows the
outcome, no POSIX reap-what-you-kill chore. `wait` on a live task
blocks via `WaitReason::TaskExit(n)` - the blocking machinery's
first second variant, generalizing exactly as the blocking-primitives
milestone designed it to - waking with the status, or
`TASK_KILLED_STATUS` (`0x100`, one past any real status) if the
target was killed mid-wait, or `WAIT_INTERRUPTED` on Ctrl+C: the
wake-check drains a keyboard byte when evaluating the *keyboard
owner's* `TaskExit` wait specifically, because otherwise one `wait`
on a never-exiting task would brick the whole session (the Ctrl+C
hatch lives in the keyboard poll, which nothing else would be
running) - any non-Ctrl+C byte typed during a wait is deliberately
discarded, like typing at a busy foreground job in `sh`. Waiting on
0/1/yourself is refused up front (guaranteed deadlock). `hello/`
gained a deliberate ~100-tick delay loop between banner and exit -
not filler: an instant exit would always be a zombie before `wait`
could even be typed, so the *blocking* path would be organically
untestable. **An honest behavior change:** un-waited exited tasks
hold their slots (`ps` shows `` exited - `wait` to collect ``); three
un-waited `exec HELLO.BIN`s exhaust the slots where they used to
recycle silently - the price of statuses being collectable, verified
deliberately. **Confirmed on QEMU:** the full blocking path (`wait 2`
typed while hello ran → shell blocked → hello's goodbye → `task 2
exited with code 0`, with `uptime` jumping 142→569 across the block -
the waiter genuinely yielded); zombie-then-collect; the
slots-held-by-zombies exhaustion and recovery; an interrupted wait on
a never-exiting spawned shell (Ctrl+C → `wait: interrupted`, task
still alive, `kill` + `wait` → no-such-task); the fg/exit keyboard
revert still firing at *death* (typing returned to shell 0 while task
2 was still an uncollected zombie); every error path; zero aborts.
**Confirmed on real Parallels:** the error paths render, typing
unregressed.

## Parallels disk diagnostic: no documented storage controller exists on this platform - confirmed with fresh evidence, not just the old inventory

A dedicated diagnostic round (2026-08-17, no driver written - that was
the point) settled the "could Parallels expose a disk this kernel could
drive?" question before any implementation effort got committed to it.

**A permanent diagnostic improvement fell out first:
`pci::log_all_devices` now returns its inventory, and `main.rs`
re-prints it through the post-exit console.** The reason is itself a
finding worth keeping: on real Parallels hardware, boot reaches the
shell about **two seconds** after `prlctl start`, and the framebuffer
console clears the screen the moment it installs - so the boot-services
`log::info!` rendering of the PCI inventory is unreadable in practice.
Confirmed empirically before writing any code: a capture loop
screenshotting the booting VM at 0.4-second intervals never caught
anything but the finished shell. The fix is the same stash-and-reprint
pattern the xHCI bring-up's diagnostics already needed for the identical
reason - capture during boot services (a fixed
`[PciDeviceId; MAX_LOGGED_DEVICES]` array, no heap), print after a
console installs. Only runs on the no-early-console path, so the normal
QEMU dev loop is untouched (confirmed by regression: zero inventory
lines, shell byte-identical, zero aborts). The dump also gained
`prog_if` - it's what distinguishes storage-controller flavors (AHCI is
class `0x01`/`0x06`/prog-if `0x01`) and was already load-bearing for
xHCI discovery.

**The experiment, then the findings.** Baseline: the VM's own boot disk
is attached as `sata:0` (`prlctl list -i`), firmware demonstrably boots
from it - and the PCI bus shows **no storage controller of any kind**,
just the same five devices as the historical inventory (HD Audio, EHCI,
xHCI, virtio-net, and Parallels' proprietary vendor-`0x1ab8` device).
So "SATA" in Parallels' config does not mean an AHCI controller on the
guest bus. Then, deliberately: `prlctl` offers an ARM64 EFI VM exactly
two disk interfaces (`ide` and `scsi` - probed directly, not assumed);
a scratch second disk attached as `scsi` subtype `lsi-sas`, then
`lsi-spi` (real, documented Fusion-MPT hardware, which would have been
an implementable spec), produced a **byte-identical inventory both
times** - the emulated controllers simply don't exist on Apple Silicon,
even when the config accepts them. `buslogic`, the third subtype, is
rejected by Parallels itself ("cannot use both the BusLogic SCSI
controller and EFI firmware"). The scratch disk and VM config were
fully restored afterward.

**Conclusion, recorded the same way the virtio-console dead end was:**
all storage on Parallels Apple Silicon flows through a
non-PCI/proprietary path - the `0x1ab8` device is the only remaining
candidate, and driving it means reverse-engineering an undocumented
protocol, the same category of work already explicitly declined for the
serial port. "Implement a documented spec" is not available for any
attached-image disk on this platform. **The one genuinely documented
lead left is USB mass storage**: the xHCI controller is on the bus and
this kernel already drives it end to end for the keyboard; a USB
storage device passed through to the VM would be reachable via Bulk-Only
Transport + SCSI commands over that same driver - a real spec, building
on the project's own working code, at the cost of the disk being a real
USB stick rather than `esp.hdd`. Untested, and needs a real
passed-through device to scope - see `docs/roadmap.md`'s new
"Disk on real Parallels hardware" parking-lot entry.

## USB mass storage: a real disk on real Parallels hardware, at last

The milestone every diagnostic this week pointed at: `usb_msd.rs`
(Bulk-Only Transport + SCSI) over new bulk endpoints in `xhci.rs`
gives Parallels its first working disk - `mount` on a real
passed-through Lexar USB 3.x stick produced its real INQUIRY strings
(`vendor='Lexar' product='USB Flash Drive'`), its real capacity
(243,404,800 sectors), a mounted FAT32, and an `ls` of the stick's
actual contents, live on real hardware. Five pieces, staged and
individually verified:

1. **`block.rs`** - a `BlockDevice` enum (`Virtio` | `UsbMsd`)
   decoupling `fat32.rs` from virtio-blk (the same enum-over-trait-
   object idiom as `Console`; a pure refactor first, regression-tested
   before anything new). First-mounted-wins - virtio stays the QEMU
   dev loop's primary.
2. **Bulk endpoints in `xhci.rs`** - the multi-device scan classifies
   the mass-storage interface (`find_msd_bulk_endpoints`),
   `activate_storage` configures both bulk endpoints in one Configure
   Endpoint command (max packet by speed, MaxBurst 0 - no SuperSpeed
   companion parsing), and `bulk_transfer` is the synchronous
   transport. **The correctness piece:** `wait_transfer_event` now
   *routes* keyboard events arriving mid-bulk-transfer through the
   factored `process_keyboard_report` instead of dropping them -
   dropping would lose the keystroke *and* leave the interrupt buffer
   unreposted, a permanently dead keyboard the first time someone
   typed during disk I/O. Verified by firing keystrokes interleaved
   with disk commands: zero drops.
3. **`usb_msd.rs`** - CBW/CSW framing with tag validation, INQUIRY /
   READ CAPACITY(10) (non-512-byte block sizes refused) / READ(10) /
   WRITE(10), one sector per call, modeled on `virtio_blk.rs`'s
   synchronous polling shape. **A real SCSI detail found organically
   by the hot-plug test, not the spec-reading:** a freshly attached
   device reports Unit Attention - INQUIRY succeeded (it's
   spec-exempt) while the first READ CAPACITY failed with CHECK
   CONDITION, fixed with the standard TEST UNIT READY / REQUEST SENSE
   bring-up loop.
4. **`mount` (syscall 22)** - a runtime port rescan
   (`xhci::rescan_ports`, skipping ports already owned, tombstoning
   failed setups so their pool entries and half-configured hardware
   slots are never reused) plus `try_mount_usb_storage`, shared with
   the boot-time auto-mount. This is the Parallels workflow the
   enumeration diagnostics demanded: passthrough attaches a few
   seconds *after* the boot scan, so - boot, wait a moment, type
   `mount`.
5. **`run-usb-multi`'s stick is a real FAT32 image now** (hdiutil,
   marker file), so QEMU's three-device rig organically mounts from
   USB (its virtio drive is unmountable FAT16 vvfat) - plus monitor
   `device_add` hot-plugging for the rescan path.

**Verified:** QEMU - boot auto-mount, `ls`/`cat` of the marker file,
`write`/`mkdir` + reboot persistence over WRITE(10), the full
hot-plug-then-`mount` flow, typing-during-I/O, `run-image` virtio
regression, zero aborts everywhere. **Real Parallels hardware** - the
full `mount` → INQUIRY → capacity → FAT32 → `ls` chain on the real
stick, ticks advancing throughout; real-stick testing kept read-only
by policy (writes proven on QEMU; write to a real stick only with an
explicit go-ahead). One unreproduced anomaly noted honestly: the very
first real-hardware run after assigning the stick showed no synthetic
keystrokes arriving at all (system state indistinguishable from
healthy in the screenshots); two subsequent full runs, and a
six-command timeline probe spanning the attach window, were all
completely clean - most likely the `prlctl send-key-event` stream was
lost during initial attach turbulence, not a kernel fault.

**Still coarse, worth knowing before building on this:** no BOT error
recovery (a failed/stalled command fails the operation; the spec's
Reset Recovery sequence is a documented gap, same posture as the
interrupt endpoint's); one storage device (first wins); one sector
per transfer (throughput is a non-goal); no hot-plug *detection* (the
rescan is user-triggered via `mount`, not event-driven); exFAT sticks
won't mount (INQUIRY/capacity still prove the transport - reformat
FAT32 to use one); no unmount.

## Driver isolation, part 1: real IPC - message passing between tasks

The final parking-lot item's honest first half. Fixed-size messages
(≤64 bytes, `MSG_MAX_LEN`), **copied** through the kernel into bounded
per-task mailboxes (`tasks.rs::MAILBOXES`, 4 pending each) - no shared
memory anywhere, deliberately: copying is the isolation-friendly
semantics, and at this size the cost is nothing. Three syscalls:
`msg_send` (23 - fails fast on a missing task, an oversized message,
or a full mailbox; no blocking sends), `msg_recv` (24 - blocks via
`WaitReason::Message { buf, len }`, the third user of the blocking
machinery `read_char` and `wait` already proved twice; the wake-check
copies the oldest message straight into the receiver's waiting buffer
and wakes it with `(sender << 32) | len` packed), and `msg_try_recv`
(25, the non-blocking sibling). The same Ctrl+C escape hatch as
`wait`: a `recv` with no sender coming must not brick the session.
**A dead task's queued mail dies with it** - `clear_mailbox` wired
into both teardown paths, so a slot's next occupant can never receive
a predecessor's messages (verified directly, not assumed - see below).

**`pong/` - the fourth userland program, and the first long-lived
server**: recv -> echo each message back to its sender -> repeat;
`quit` exits cleanly. This is the process shape part 2's FAT32
filesystem server will have, not another run-and-exit demo. Shell
gained `send <task> <words...>`/`recv` builtins - test plumbing, but
exercising exactly the calls any IPC client makes.

**Confirmed on QEMU, every scenario:** the echo round trip (`send 2
hello ipc world` -> `recv` -> `task 2: hello ipc world`, looping);
`quit` -> the kernel's exit line -> `wait 2` collecting status 0 ->
`send` to the freed slot refusing with no-such-task; queue-full at
exactly the fifth pending message to a never-receiving task; **the
mailbox-clearing proof**: four messages queued to a task, `kill` it,
`exec` a fresh pong into the same slot, `send ping`/`recv` returns
`ping` - not the dead task's `m1`; a blocked `recv` interrupted by
Ctrl+C; zero aborts. **Confirmed on real Parallels hardware:** the
error paths and a blocked `recv` interrupted by a real held-Ctrl
chord on the physical keyboard path - **and then, with the user's
binaries copied onto the USB stick, the full success path too, a
stack of firsts in one screen:** `mount` -> `ls` showing
`PONG.BIN`/`HELLO.BIN`/`SH.BIN` on the stick -> `exec /pong.bin`
(**the first program ever loaded from disk at runtime on real
Parallels hardware** - the identity-map rebuild line shows real
hardware's own RAM span, `0x40000000-0xc0000000`, and the runtime
regions) -> `ps` showing the server blocked in `recv` -> a complete
IPC round trip (`send 2 hello ...` -> `recv` -> the echo back) ->
`quit`/`wait` collecting status 0 with the region freed - and
separately `exec /hello.bin` with `wait 2` typed during hello's
delay, **the blocking wait path proven on real hardware** (the typed
characters visibly interleaved with hello's own output - two tasks
genuinely sharing the console). Every userland facility this project
has - spawn, IPC, job control, wait/reaping - now demonstrated on the
platform it was built for.

**Part 2 is done - see "Driver isolation, part 2" immediately below.**

**Still coarse:** ~~receive-from-any only (no selective receive)~~
(partially closed: `MSG_CALL`'s reply wait filters by sender, and the
mailbox pop supports a sender filter - a general selective `recv`
still doesn't exist); no blocking sends or back-pressure; senders are
never notified when a receiver dies; no capabilities (any task can
message any task - the kernel's existing trust model); 64-byte
messages (enough for part 2's FS protocol, which passes pointers
rather than payloads - see below).

## Driver isolation, part 2: the filesystem moves to userland

The first real component out of the EL1 kernel, done 2026-08-18 in
five independently-tested stages, each committed and regressed before
the next depended on it (the project's standing staged discipline).
The result: **the kernel contains no filesystem at all** -
`kernel/src/fat32.rs` (1040 lines) moved into `fsd/`, the fifth
userland program, and every file operation is now an IPC round trip.
Two architectural calls made explicitly with the user before any code:
**direct client IPC** (the shell's nine typed `fs_*` wrappers became
`MSG_CALL` round trips; the kernel's fs_* syscalls 7-14 are numbering
gaps now, like 5 - not kernel forwarding stubs, which would have kept
the whole FS interface in the kernel forever), and **a synchronous
call primitive** (`MSG_CALL`, 29 - MINIX-sendrec-shaped) rather than
tick-paced plain send+recv, fixing both the reply mis-routing hole (a
blocked `MSG_RECV` took the oldest message from *anyone*) and latency
(~40ms/op and a ~5s `exec` otherwise).

**The pieces, in stage order:**

1. **Block custody + syscalls (26-28).** A `BlockCell` in `syscall.rs`
   holds the raw `BlockDevice`; `BLOCK_INFO`/`BLOCK_READ`/`BLOCK_WRITE`
   expose it one 512-byte sector at a time, **gated to `FSD_TASK`
   (task 2) alone** - the actual "supervised" in "supervised EL0
   process": the kernel holds the device, exactly one task may ask it
   to touch the disk. Verified from EL0 via a temporary shell builtin
   (denied when gated, then capacity/MBR-signature/write-echo with the
   gate temporarily forced, then reverted).
2. **IPC upgrades.** Direct delivery in `tasks::send_message` (a
   destination already blocked in a matching message wait gets the
   bytes copied straight into its buffer, the packed result in its
   saved `x0`, and a wake - the eager version of the wake-check's own
   logic, no new mechanism), a `from` sender filter on
   `WaitReason::Message` with selective mailbox pop
   (`try_recv_message_from` - non-matching messages stay queued in
   order), and the `MSG_CALL` arm itself: send (direct-delivers,
   waking the server), then block on `Message { from: Some(dest) }` -
   `block_current_and_switch`'s existing immediate switch lands on the
   just-woken server, so the whole round trip is sub-tick with zero
   new scheduler mechanisms. Self-call refused up front (guaranteed
   deadlock, `WAIT`'s own precedent). Verified: pong regression
   byte-identical; a call's reply filter proven directly (a stale
   queued message from another task skipped, then correctly returned
   by a later plain `recv`); Ctrl+C interrupting a call to a
   never-replying task.
3. **`fsd/` as protected task 2.** Boot-loaded by `loader::load_fsd`
   (`\EFI\ORBS\FSD.BIN`, a fixed path - the server is infrastructure
   with a fixed slot clients hardcode, not the configurable `INIT.CFG`
   program; missing FSD.BIN = warn and boot on, every request then
   failing no-such-task, which the shell folds into its ordinary
   no-filesystem message). `NUM_TASKS` 4 -> 5, `MAX_EL0_REGIONS` (and
   the EL0 L2/L3 pools) 4 -> 5, spawnable slots now 3-4 (`spawn` scans
   from `FIRST_SPAWNABLE = 3` and never touches slot 2 - a spawned
   program landing in the block-syscall-privileged slot would inherit
   the server's disk access), task 2 joins 0/1 in the
   `EXIT`/`KILL`/`WAIT` protections.
4. **The move itself.** `fsd/src/fat32.rs` is the kernel module
   essentially verbatim - the `BlockDevice` it owned by value became a
   zero-sized `Disk` shim over the block syscalls - plus one new
   method, `read_at` (windowed reads at a byte offset, skipping whole
   clusters to the window). The server speaks the `FSOP_*` protocol
   (56-byte requests: op + six LE u64 args, *pointers into the
   client's memory* - which is why 64-byte messages were enough, no
   size bump: the same single-address-space trust model the old
   syscall pointer arguments had, one level up; replies are one u64
   with the old syscalls' exact return semantics, so `print_fs_error`
   and every cmd_* arm work unchanged). The mounted `Fs` lives in the
   server's own stack frame (userland has no static mutable state;
   the server is one infinite request loop). `SPAWN` (16) was
   recontracted - the kernel can't read a path anymore, so `exec`
   reads via `FSOP_READ_AT` in 512-byte chunks, feeds `SPAWN_STAGE`
   (30) into the existing 128KB staging buffer, then `SPAWN` takes the
   staged total. `MOUNT` (22) is the device half only, with a replace
   flag; the shell's `mount` is server-first (FSOP_MOUNT, then device
   replacement only after the server confirms nothing is mounted, then
   FSOP_MOUNT again) - **a regression caught by reasoning before it
   shipped**: naive first-installed-wins device custody would have let
   `make run`'s unmountable FAT16 vvfat virtio disk permanently hog
   the cell and block a later USB stick; the server-first flow
   preserves the old first-MOUNTED-wins behavior, verified directly
   against the run-usb-multi rig.
5. **Docs + verification** (this section, architecture/processes/
   shell-commands/manual/roadmap/CHANGELOG).

**A genuinely new instance of the prebuilt-libcore PIE constraint,
found the hard way porting fat32.rs:** `str::rfind` with a `char`
pattern pulls libcore's `memrchr`, whose prebuilt non-PIC object
carries `R_AARCH64_ABS64` relocations a PIE userland link rejects
outright - even in release. Joins the documented `slice_error_fail`
(str range slicing - hit again here in `split_parent`/
`make_short_name`, fixed with `.get()`) and debug-build cases; fixed
with a manual reverse byte scan. The failure is loud (a link error),
never silent; `docs/processes.md`'s "Binary format" now keeps the
list (byte-slice indexing, forward `find`/memchr, `copy_from_slice`,
and `core::fmt` itself are all confirmed fine).

**Confirmed working end to end on QEMU, zero aborts in `-d int`
cross-checks throughout:** the full disk-command surface over IPC
against real FAT32 (`ls`/`cat`/`cd`/`pwd`/`mkdir`/`write`/`cat`/`cp`/
`mv`/`rm`/`rmdir`, `>`/`>>` redirects), every error path organically
(not-found, is-a-directory, bad ELF, already-exists, `rmdir /`),
chunked `exec` of HELLO.BIN and PONG.BIN with the full
exit/wait/send/recv lifecycle alongside the fsd server (two servers
coexisting, no cross-talk - the reply filter's whole point),
`selftest`, FAT16 degradation byte-compatible with before (the shared
no-filesystem message; `mount` cascading to a clean no-device answer),
the missing-FSD.BIN boot (warn, no hang, no-such-task -> no-filesystem
messages), the USB replace flow on the three-device rig (vvfat fails,
`mount` swaps in the stick, its FAT32 mounts, marker file listed), and
reboot persistence (a file written through the whole
shell -> IPC -> server -> BLOCK_WRITE -> virtio chain read back on a
fresh boot, pre-existing files untouched). **Confirmed on real
Parallels hardware** via `make test-parallels`: `fsd: filesystem
server ready` at boot, `selftest` clean, and the complete
`mount` -> xHCI rescan -> Lexar INQUIRY -> `usb-msd block device
installed` -> **`fsd: FAT32 mounted` (the mount itself now running in
userland over BLOCK_READ)** -> `ls` of the real stick's contents ->
`uptime` advancing (1669 ticks). Reads only on the real stick, per
standing policy.

**Still coarse, worth knowing before building on this:** a crashed or
wedged server means no disk until reboot - there's no
restart/supervision (MINIX's reincarnation-server idea, a real future
milestone; Ctrl+C rescues a *client* blocked on a call, but nothing
rescues the server itself); the isolation is still trust-based, not
enforced (all EL0 regions are mutually accessible - the server can
read/write any client memory, and any task could corrupt the server's;
per-task page tables are the real fix); `FSOP_*` request pointers are
trusted exactly like syscall pointers were; one filesystem, one
device (the block cell holds exactly one; a second stick can't be
mounted alongside); `read_at` exists server-side but `cat` still
reads from offset 0 only (no paging); and the fsd server's fat32
engine still carries all the pre-existing FAT32 limitations (8.3-only,
no LFN, first-FAT32-partition-only, no append/offset-write underneath
`>>`).

## EL0 fault isolation, and the filesystem server survives its own crashes

The follow-up the part-2 milestone's "still coarse" list named first -
and it opened with a finding bigger than the item itself: **any EL0
fault used to halt the entire kernel.** `exceptions.rs`'s slot 8
(Synchronous, lower EL) only took the resumable path for `svc`; every
other EL0 exception - a wild pointer in any userland program - fell
through to the diverging report-and-halt path. A microkernel that
moved its filesystem to userland *for fault containment* was
converting every userland fault into a whole-system halt. Two layers
fixed that, done 2026-08-18:

**Layer A - EL0 faults are contained.** A fourth trampoline ("4:") in
`exceptions.rs`: slot 8's EC-check fall-through now builds the same
272-byte `Context` frame as the IRQ/SVC paths and calls
`rust_el0_fault_handler`, which reports the fault (ESR/FAR/ELR + task
index), tears down just the faulting task (the `KILL` arm's exact
order: region freed LIFO-or-leak, keyboard reverted, slot reaped
immediately via the new `tasks::kill_current_and_switch` - the
frame-interchange contract again: the handler overwrites the frame
with the next runnable task's context and the trampoline's blind
restore resumes the survivor). Tasks 0/1 faulting still halt,
honestly - nothing meaningful survives the keyboard owner's or idle's
death. Alongside it, a fix all three death paths (exit/kill/fault)
gained: `tasks::fail_calls_to(dead)` wakes anyone blocked
mid-`MSG_CALL` to a dying task with `TASK_ERR_NO_SUCH_TASK` (the
shell's `fs_call` maps it to `NO_FS`) - previously they waited
forever, Ctrl+C being the only rescue.

**Layer B - the filesystem server is supervised** (MINIX's
reincarnation server, minimal edition). **(Update: this fsd-only
machinery was later generalized into `kernel/src/supervisor.rs` -
`FSD_IMAGE`/`stash_fsd_image`/`restart_fsd` no longer exist under those
names; a registry now supervises both fsd and cond, and adds wedge
detection on top of this crash path. See "Server supervision +
heartbeat" below. The description here is the original fsd-only design,
kept as accurate history.)** `loader::load_fsd` keeps
FSD.BIN's raw bytes in a kernel static (`FSD_IMAGE`, 128KB cap) while
boot services can still read the ESP - kept precisely because the
crashed server *was* the filesystem that would otherwise be needed to
reload it. On a task-2 fault, `syscall::restart_fsd` reparses and
reloads that image into a fresh region (`tasks::install_task`, the
direct-slot variant `spawn` deliberately can't provide - it must never
fill the block-syscall-privileged slot 2 itself) and the fresh server
re-runs its own startup: device probe, remount from disk - its state
was always disk-derivable, which is what makes this real recovery. A
per-boot cap (3 restarts) guards crash loops; past it the kernel gives
up and slot 2 stays `Unused`, the same graceful degradation as a
missing FSD.BIN.

**A latent pre-existing gap found and fixed along the way:**
runtime-`spawn`ed code never got the dcache-clean/icache-invalidate
sequence `tasks::init` gives boot-loaded programs - invisible on QEMU
(TCG models no cache incoherency) and never observed to bite on real
Parallels hardware, but never correct to omit. `tasks::flush_new_code`
packages `init`'s inline sequence; `spawn` and `install_task` both
call it now.

**Verified by direct fault injection on QEMU, each scenario's abort
count in the `-d int` trace exactly matching the injections (the
zero-aborts discipline, adapted):** a TEMP-crashing `hello` (null
write) killed alone - `EL0 FAULT task=3` reported with the exact
injected `FAR_EL1`, shell kept running, the freed region visibly
reused by a subsequent spawn, full lifecycle clean after; fsd crashed
mid-call (a TEMP magic-op crash + TEMP shell trigger, both reverted) -
the blocked caller woke with the no-filesystem message (fail_calls_to
proven organically: the caller was blocked on the reply when the
server died), restart attempts 1/3 -> 2/3 -> 3/3 each followed by the
fresh server's banner, a successful remount, and working disk
commands, then the fourth crash hitting the cap and degrading cleanly
with the shell alive; a task-0 fault (TEMP shell null write) reporting
and halting cleanly - one abort, no fault loop. Full no-injection
regression byte-identical (disk surface over IPC, exec/exit/wait/kill,
IPC coexistence, selftest, zero aborts), and a real-Parallels boot +
typing + uptime regression via `make test-parallels` (the fault
machinery is architecture-level and inert until a fault; no crash
injection ships).

**Still coarse, worth knowing before building on this:** a *wedged*
(looping, non-faulting) server is not detected - that needs a
watchdog/heartbeat, deliberately out of scope, and Ctrl+C remains the
client-side rescue; no journaling - disk state a server corrupted
mid-write before crashing stays corrupted; the restart cap is per-boot
total, not rate-based; stack overflows may still silently corrupt
rather than fault (no guard page - unchanged); and fault containment
is still trust-based isolation (a task can corrupt another's memory
*without* faulting - per-task page tables remain the real fix, queued
as the next big milestone).

## Pipelines: `builtin | program` - data flowing between processes over IPC

The queued milestone after fault isolation, done 2026-08-18: `left |
/path/to/program` pipes a builtin's captured output into a freshly
spawned program as a real IPC stream. Scoped honestly around what
exists: spawned programs receive no argv and their output isn't
capturable (putc goes straight to the console), so v1 is **builtin
left, program right, one pipe per line, no combining with `>`** - all
refused with specific messages, all documented in
docs/shell-commands.md's "Pipelines" section.

Mechanism - zero new syscalls: the shell captures the left side (the
redirection machinery's 512-byte capture, refuse-not-truncate), spawns
the right side via the existing two-step staged flow (**`SPAWN` now
returns the new task's slot index** instead of a bare 0 - the shell
needs it to stream to, wait on, and kill; success values stay below
the error band, so every existing caller is unaffected), streams the
capture in 1-64-byte `MSG_SEND`s, and marks end-of-stream with one
*empty* message - **a zero-length message is legal now**, the one
ABI-behavior change (the pointer must still be non-null; the empty
message is the pipeline EOF convention, documented on `MSG_SEND` in
syscall-abi). The filter's shape (`upper/`, the sixth userland
program, ~80 lines, the reference to copy): stdin is `msg_recv`,
stdout is `putc`, EOF is the empty message, finishing is `exit` - the
shell `wait`s on the slot, Ctrl+C-interruptible.

**The one real robustness piece:** a right side that never reads its
input (e.g. piping into a spawned shell, which only reads the
keyboard) fills the 4-deep mailbox and would hang the shell in a
send-retry loop forever - Ctrl+C couldn't rescue it, since the shell
would be running, not blocked. `pipe_send` bounds the retry with a
real-tick deadline (~3s via `GET_TICKS`), then kills the program and
says why. A right side that *exits early* mid-stream is legitimate
(head-like filters): `MSG_SEND`'s no-such-task answer just ends the
streaming, and the wait reports how the program actually ended.

**A real keymap gap found by the scripted real-hardware smoke test:**
HID keycode 0x31 (backslash/pipe) was missing from
`xhci.rs::keycode_to_ascii` entirely - the smoke test's new held-Shift
`|` chord (added to `scripts/test-parallels.sh` alongside the existing
`>` chord) arrived at the driver and was dropped, which also meant **no
physical keyboard could type a pipeline on Parallels**. Fixed
(`0x31 -> \\ / |`), and the rerun typed the pipeline correctly through
the real xHCI path.

**Verified on QEMU:** `echo`/`ls`/`cat` piped through UPPER.BIN
(uppercased output, child exit 0, slot reaped and reused); every parse
error (missing left/right, extra tokens, both `|`+`>` orders refused);
an early-exiting non-reader (hello - messages queue, it exits, the
pipeline completes); the never-reading case (`help | SH.BIN` - mailbox
fills, ~3s, "stopped reading its input - killing it", slot reclaimed);
FAT16 degradation (the shared no-filesystem message); a full
byte-identical regression of the existing surface. Zero aborts in
every `-d int` cross-check. **Verified on real Parallels hardware:**
the pipeline command typed through the fixed keymap and correctly
degraded to the no-filesystem message (UPPER.BIN isn't on the stick,
and disk arrives post-boot there); boot/typing/uptime regression
clean.

**Still coarse, worth knowing before building on this:** one pipe per
line - `a | b | c` needs either shell-side chaining of captures or
real program-to-program streams; the left side must be a builtin - a
task's own output isn't capturable, which is the same gap that blocks
`program | program` and `exec ... > file` alike (a stdout-over-IPC
redirection model is the real fix, a candidate for the per-task
milestone era); filters get no argv (nothing passes arguments to
spawned programs at all); and the 512-byte capture bounds how much can
flow through one pipeline.

## Per-task page tables: MMU-enforced isolation, not trust (and FSOP v2)

The last of the three post-part-2 candidates, done 2026-08-18 in three
staged commits + one revert - the change that makes this kernel's
isolation *enforced* rather than a convention. Before it, every EL0
region was EL0-accessible to every task: any task could read or corrupt
any other's memory (the shell's, the filesystem server's) without
faulting. Now each of the five scheduler slots runs under its own
translation-table view.

**The design cascade, and a user decision it forced.** Per-task views
break exactly one thing: the fsd server dereferencing pointers into
client memory in FSOP requests (every *kernel-side* copy keeps working -
kernel mappings are identical in every view). Two ways to fix it were
weighed with the user: mapping-based grants (MINIX-*style* as often
described) vs. inline payloads + kernel copies (MINIX's *actual*
production design - `sys_safecopy`). Chosen: **inline payloads**, because
sub-page grants would leak neighboring client memory into the server -
the opposite of the milestone's point - and because copying is the
first half of the real MINIX design, with a grant/safecopy primitive a
clean later addition for bulk data. See the plan discussion; this was an
explicit call, not a default.

**Stage 1 - FSOP v2 (`36b4aa6`), done first, under the still-shared
map.** The protocol became fully self-contained: a request is a header
(op + four u64 params) plus inline payload (path, then data); a reply is
a status u64 plus inline result; everything copied task-to-task, no
pointer crossing a boundary. `MSG_MAX_LEN` 64 -> 768 to carry it
(Message.len u16, mailboxes ~16KB, message syscalls get their own
`valid_msg_range`). The 512-byte per-op cap survives as `FS_DATA_MAX`.
The server now touches only its own two buffers; the shell's `fs_call`
builds requests and unpacks replies, every wrapper signature unchanged
upward. Regressed byte-clean on QEMU before any MMU change.

**Stage 2 - per-task views + syscall hardening (`4eb9db4`) - the actual
payoff.** `mmu.rs` builds one L0/L1/L2/L3 view per task: identical
kernel/device mappings, EL0 access to that task's region alone (a view
fine-grains only its own region's one 2MB slot - simpler than the old
shared map's up-to-five-splits). `mmu::activate_task` switches TTBR0 +
flushes the TLB, hooked into every `CURRENT` change in `tasks.rs`. And
the long-documented "syscall pointers trusted, not validated" gap
*had* to close here - with the MMU enforcing EL0 isolation, an
unvalidated syscall pointer was the last way to reach another task's
memory (by having the kernel touch it); `in_caller_region` checks every
pair against the caller's own region. **Proven by A/B fault injection:**
a temporary probe reading a byte of the shell's region from a spawned
program *succeeded* under the stage-1 shared map (control) and *faulted*
under views (`EL0 FAULT far_el1=0x5c600000`, permission fault, faulter
killed alone, shell alive) - the isolation is real, not assumed. fsd
crash/restart re-verified under views.

**Stage 3 - ASIDs + nG (`2ac55b8`), attempted, reverted (`f23101b`).**
To drop the per-switch TLBI: nG-tagged EL0 pages, ASID-per-view TTBR0,
plain-write switches. Passed every QEMU test *and* the isolation probe -
but on real Parallels hardware it faulted the idle task on its own
instruction fetch (`EL0 FAULT task=1 esr=0x8200000f`, instruction
permission fault). Per the plan's own contingency, reverted to stage
2's flush-on-switch (correct on both platforms, re-confirmed on real
Parallels: selftest, echo, ps, advancing uptime, no fault) - one
variable at a time isolated it to the ASID change specifically, not
views. ASIDs are only a per-switch-TLBI optimization this kernel
doesn't need (a tick already does far heavier work); recorded as a
future item with the fault evidence, not chased further this session.
The root cause is unproven - leading candidates are real hardware not
honoring nG the way TCG does, or a break-before-make gap when a rebuild
changes a view whose ASID has live entries.

**Confirmed on real Parallels hardware** (stage 1+2): `selftest`
correct, disk auto-mounted from the USB stick, `echo`/`ps`/`uptime`
clean with ticks advancing across the idle task - isolation enforced,
no regression. Reads only on the stick, standing policy. One
observed-and-ignored USB enumeration flake (the attached stick
occasionally displacing keyboard enumeration on a given boot) is
unrelated to this work.

**Still coarse, worth knowing before building on this:** the 512-byte
per-op FSOP payload cap (a grant/safecopy primitive lifts it - the
recorded next IPC step); ASIDs reverted, so a context switch still
flushes the TLB (cheap relative to a tick's other work); no stack
guard page (an overflow corrupts the program's own region - within its
isolation boundary, not another task's); and program-to-program pipes
still need a stdout-over-IPC model (a task's own output isn't
capturable, so pipelines stay builtin-left).

## Grant/safecopy IPC: enforced capability-based bulk transfer, and a streaming `cat`

The second half of MINIX's IPC design, closing the last user-visible
limitation the per-task-page-tables milestone left behind. FSOP v2 made
filesystem requests self-contained (payloads inline, kernel-copied
task-to-task) precisely because a server can no longer dereference a
client pointer under per-task views - which capped every operation at
what fits one 768-byte message (`FS_DATA_MAX`, ~512 bytes). `cat`
truncated at 512, `cp`/`>>` refused files past their buffers. This
milestone moves bulk data *directly between two isolated regions*
instead, without stuffing it through the message.

**Enforced, not trust-based - a deliberate user decision, weighed
against the simpler alternative.** The choice was between a real MINIX
`safecopy`-style *grant* (the client pre-registers an exact buffer; the
kernel enforces the server can touch only those bytes) and a simpler
"server names a client pointer, kernel checks it's within the calling
client's region" model. The latter is less code but lets a buggy or
compromised server read/write *anywhere* in its caller's region, not
just an agreed buffer - a trust assumption, right after a whole
milestone spent making isolation *enforced*. The user picked the
enforced grant, explicitly to avoid that class of security issue and
because it's the capability every future out-of-kernel component will
want (retrofitting enforcement later would touch every call site).

**The two syscalls:**
- `GRANT` (31): a client records, in its own single per-task grant slot
  (the `MAILBOXES` pattern - `GRANTS: [GrantSlot; NUM_TASKS]`, cleared
  at every task death alongside `clear_mailbox`), that task `grantee`
  may bulk-copy an exact `(ptr, len)` buffer *in the client's own
  region*, in a direction (`GRANT_READ` = grantee may read it,
  `GRANT_WRITE` = grantee may write it). Validated: grantee exists, dir
  is a nonzero subset of the two bits, `len <= SAFECOPY_MAX`, and
  `in_caller_region(ptr, len)`.
- `SAFECOPY` (32): a *server* names the client it's serving and copies
  `len` bytes between the client's granted buffer and the server's own
  `local` buffer. `tasks::safecopy` authorizes it only when **all**
  hold: the grant is active, names this server, and permits the
  direction; the client is *currently* `Blocked(Message { from:
  Some(server) })` - an active `MSG_CALL` to this server (the temporal
  bound - a stale grant is inert because once the call returns the
  client is `Runnable`, not blocked-calling-me); `client_off + len`
  stays within the granted buffer; and the resulting client range is
  within the client's live region. The copy is a raw
  `copy_nonoverlapping` between the two identity-mapped addresses -
  which works across per-task views because **all RAM stays
  coarse-block identity-mapped EL1-RW in every view; only the EL0-access
  overlay is per-task** (`mmu.rs:171-176`), so the kernel reaches both
  regions at EL1 regardless of the active `TTBR0`. **Not gated to one
  task** (unlike `BLOCK_*`): the grant plus the active call *is* the
  capability, so any future server can use it. It takes five arguments;
  the fifth (direction) is read from the saved trap frame's `x4` (the
  `dispatch` signature's four named args are full) - exactly the
  mechanism the SVC trampoline's own doc comment already described for a
  5th arg.

**FSOP gained two bulk ops.** `FSOP_READ_BULK` (params `path len,
offset, want`) - the server reads up to `min(want, SAFECOPY_MAX)` bytes
from `offset` into its working buffer and `SAFECOPY`s them into the
client's `GRANT_WRITE` buffer; reply is status only (bytes delivered, 0
at EOF). `FSOP_WRITE_BULK` (params `path len, data len`) - the server
`SAFECOPY`s the client's `GRANT_READ` buffer in, then `write_file`s it.
The `want` parameter is a real correctness piece, reasoned through
before testing: without it the server would read a full `SAFECOPY_MAX`
chunk and try to `SAFECOPY` it into a client buffer smaller than that,
overrunning the grant - so a client with a smaller buffer passes its
length as `want`.

**The callers.** `cmd_cat` now **streams a file of any size**: loop
`fs_read_bulk` over a rising offset, `putc` each chunk, stop at a
genuine 0 - into one fixed `SAFECOPY_MAX` chunk buffer, never holding
the whole file (the old single-`fs_read_file`-at-offset-0 truncation
and its "(truncated)" notice are gone). `cmd_cp` reads the source via a
new `fs_read_all` helper (a one-byte `fs_read_file` "stat" for the real
size - so it can refuse an oversize file - plus one `fs_read_bulk`
chunk for the content) and writes via `fs_write_bulk`, lifting cp from
512 to `SAFECOPY_MAX`. `finish_redirect` (`>`/`>>`) uses the same
helpers. `cmd_write` deliberately stays on the cheap inline
`fs_write_file`: its content is bounded by the 128-byte input line,
always under 512, so it pays no `GRANT`.

**Buffer sizing was a genuine engineering call, made against the
guard-page-less 8KB userland stack, not a default.** The worst nesting
is `cat big > file`: `run_line`'s capture buffer, `cmd_cat`'s
`SAFECOPY_MAX` (2048) streaming chunk, and `fs_call`'s 768+768
request/reply all live on one frame. So `SAFECOPY_MAX` is 2048 and
`cp`'s buffer is a full 2048 (cp doesn't nest with a capture), but the
redirect **capture (1024) and append buffer (1024) stay *below*
`SAFECOPY_MAX`** to keep that nesting under budget. This is exactly the
"drop it if any stack-overflow symptom appears" guidance the plan went
in with, applied proactively from the nesting analysis rather than
after a silent corruption - the right instinct on a stack with no guard
page.

**Confirmed on QEMU against the real FAT32 `esp.img`, staged and
committed per stage** (the project's standing discipline): stage 1 (the
primitive, unreachable until a caller exists) verified only that both
task-death paths still tear down cleanly with the new `clear_grant`
calls, zero aborts; stage 2 proved the *whole* stage-1 chain
end-to-end via a 5564-byte file streaming complete (three 2048 chunks -
unique markers planted in chunks 1/2/3 all present, all 120 lines in
order, no truncation), plus small/empty/nonexistent files; stage 3 -
cp of a 1500-byte file round-trips where it used to refuse, cp of 5564
correctly refuses (>2048), an 800-byte redirect capture (was
512-capped), a correctly-**ordered** `>>` append (existing content
*then* the appended line - not the historical `>>`-behaves-like-`>`
overwrite bug, which the old 1024-append-buffer's read-back-rejection
once caused), a small inline write, and an empty redirect. **Reboot
persistence confirmed** - a cp'd and a redirected file both survived a
fresh QEMU boot with markers intact, pre-existing `INIT.CFG` unchanged
(the bulk write reaches real disk sectors). `make run`'s FAT16 degrades
to the shared "no filesystem mounted" message. Zero aborts
(`Data Abort`/`Prefetch Abort`/`Undefined Instruction`) in `-d int`
cross-checks across every session.

**Confirmed on real Parallels hardware, end to end** (`make
test-parallels`, reads-only per standing policy). First, the regression:
`selftest`'s three relocation checks (`write!`/`core::fmt`,
slice-vs-literal, str-vs-literal) all pass - meaningful, since the shell
binary was heavily modified this milestone and must load/relocate
correctly on real hardware; `echo`/`uptime` clean. Then the actual
bulk-read proof: `mount` (the runtime rescan found the passthrough
Lexar stick - `usb-msd: INQUIRY -> vendor='Lexar'`, `fsd: FAT32
mounted`), `ls /` (the stick's real contents), and **`cat /hello.bin`
streamed the whole 5784-byte binary** - the `ELF` magic at the start,
the embedded `.rodata` string ("Hello from a second program!"), and the
section-name tail (`.text .dynstr ... .rodata`) all rendered, ~5KB in,
with no truncation notice. The *old* `cat` would have stopped at 512
bytes and never reached that tail, so this is a direct, unambiguous
demonstration that the multi-chunk grant/safecopy bulk read works on
real silicon. The shell stayed fully responsive afterward (the next
command typed and got a clean text response - one dropped keystroke
via `send-key-event`'s synthetic keyboard, the documented artifact, not
a crash).

**A real-hardware enumeration wrinkle worth knowing (not caused by this
work): a stick connected *at boot* consistently displaced the keyboard**
- the boot log showed `keyboard not available (devices found, but no
boot-protocol keyboard among them)` on every boot where the stick was
present during the one-shot xHCI scan, so nothing could be typed. This
is the multi-device-scan limitation already on record (the 4-device
pool / Parallels' enumeration timing), just more consistent than
"occasionally" when a real stick is in the mix. The working path is the
*designed* Parallels workflow: let the VM boot with the keyboard
enumerated first, then type `mount`, whose runtime rescan
(`xhci::rescan_ports`) picks up the stick that attached a few seconds
later - keyboard intact, stick found. That's exactly the boot that
produced the successful `cat` above.

**Still coarse, worth knowing before building on this:** the per-op
transfer is capped at `SAFECOPY_MAX` (2048) - `cat` streams past it in a
loop, but a *single* `write`/`cp`/`>>` still refuses past 2048 (a
streaming cp needs a FAT32 offset/append-write primitive - there are no
partial writes at the FAT32 layer - and/or a userland heap; the fixed
8KB unguarded stack is the real ceiling on one buffer); directory
*listings* (`ls`) still use the 512-byte inline path (no bulk variant);
the grant is a single per-task slot (one in-flight call's worth, which
is all the synchronous model needs); and there's still no stack guard
page.

## FAT32 offset-write (`write_at`): streaming `cp` and unbounded `>>`

The follow-on the grant/safecopy milestone recorded as the exact thing
needed for genuinely large file writes. Every write at the FAT32 layer
was a **full replace**: `fat32::write_file` always allocated a fresh
cluster chain (`write_chain`), freed the old one, and repointed the
directory entry - it never mutated existing clusters and never did a
partial-sector write. So `cp`/`>>` were each bounded by one in-memory
buffer, and `cp` of a file over `SAFECOPY_MAX` (2048) refused outright.

**`fat32::write_at(path, offset, data)`** writes `data` at a byte
`offset`, extending the file (allocating clusters, growing the size
field), **without** rewriting the bytes before `offset`. Most of it
reuses primitives that were already there - the chain walk
(`next_cluster`), cluster allocation/linking (`find_free_cluster` +
`write_fat_entry`), the size/cluster patch (`patch_entry_cluster_size`),
and `read_at`'s offset-traversal template. **The one genuinely new
primitive is `write_partial_sector` - a read-modify-write of a partial
sector of file *data*.** Before this, RMW existed only for metadata
(directory entries via `patch_entry_cluster_size`, FAT entries via
`write_fat_entry`); every data write built fresh whole clusters and had
nothing to preserve. The per-sector write decides, for each sector: a
**full** sector is written directly (no read); a partial sector that
overlaps the file's existing content (`sector_start < old_size`) is
**RMW'd** (read, splice, write - preserving the bytes outside the write
window); a partial sector entirely past the old end of file is
**zero-padded** (`write_chain`'s pattern - freshly allocated, nothing to
preserve). Grow-only size update (`max(old_size, offset+len)`); a
previously-empty (`touch`ed, cluster-0) file gets its head cluster
allocated here; a write past EOF is refused (`Error::InvalidOffset` - no
sparse gaps, which sequential/append callers never hit). `extend_chain`
factors out the allocate-mark-link tail extension. A new `FSOP_WRITE_AT`
(op 13) carries the data via grant/safecopy `GRANT_READ`, structurally
identical to `FSOP_WRITE_BULK`.

**`cp` streams now, and copies a file of any size.** `cmd_cp` probes the
source exists (via a one-byte `fs_read_file` stat - so a bad source
*never* clobbers the destination), truncates/creates `dst` empty
(`fs_write_bulk(dst, &[])`), then loops `fs_read_bulk(src, off, chunk)` →
`fs_write_at(dst, off, chunk)` over a rising offset until the read
returns 0. No buffer ever holds more than one `SAFECOPY_MAX` chunk, so
`cp` handles a file bounded only by disk space. **A real correctness trap
caught by design, not testing: `cp x x` (self-copy) is guarded.**
Streaming truncates `dst` first, which would destroy `src` if they're
the same resolved path - the old read-whole-then-write cp was safe by
construction, this isn't. Reuses `mv`'s exact same-path byte-equality
check (two runtime buffers, relocation-safe). Streaming cp is also
**non-atomic** (an interrupted copy leaves `dst` truncated) - inherent to
streaming without holding the whole file; "a partial copy is a wrong
copy" is already the stance.

**A subtle test-coverage point worth remembering: streaming `cp` never
exercises the RMW branch.** Because cp truncates `dst` to empty first,
`old_size` grows monotonically and every write lands sector-aligned at
EOF (the zero-pad/full-sector branches) - the partial-sector RMW only
fires when appending *into* an existing non-sector-aligned file, which is
exactly what **`>>`** does. So write_at isn't fully exercised by cp
alone; the `>>` rewire is what proves the RMW path.

**`>>` appends at the file's end via `write_at` now** - `finish_redirect`
stats the destination's size (one-byte `fs_read_file`) and
`fs_write_at(dst, size, captured)`, with no read-back of the existing
content. This drops the old read-concatenate-rewrite's combined buffer
*and* its "file too large to append to" refusal, so `>>` works on a
target of any existing size (the new output is still bounded by the
1024-byte capture - the input side). A missing `dst` is created
(`fs_write_bulk`). `write`/`>` are unchanged. The now-unused `fs_read_all`
helper and `APPEND_BUFFER_SIZE` constant were removed.

**Confirmed on QEMU against the real FAT32 `esp.img`, tested holistically
because write_at is dead code until wired** (so the primitive and both
consumers landed and were proven together, the project's "prove a
primitive via its first consumer" pattern): streaming `cp` of a 5564-byte
non-sector-aligned multi-chunk text file is byte-exact (chunk-boundary
markers all present, all 120 lines in order); `cp` of the 72KB shell
binary persists **complete** across a reboot - the `.shstrtab` section
name, which lives at the very *end* of the ELF (~72KB in), reads back on
the fresh boot, proving all 36 streamed chunks landed and the size field
is right at scale; `>>` appends correctly and **preserves existing
content** on both a tiny file (RMW of sector 0) and the 5564-byte file
(RMW of the last partial sector, which the old `>>` refused as
">1024"); `cp x x` refused; a missing source leaves `dst` untouched;
inline `write` unaffected; `make run`'s FAT16 degrades to the shared "no
filesystem mounted" message. Zero aborts (`Data Abort`/`Prefetch
Abort`/`Undefined Instruction`) in `-d int` cross-checks across every
session (main cp/`>>`, reboot persistence, small-file RMW, FAT16
degradation).

**Confirmed on real Parallels hardware too** (with the user's explicit
go-ahead to write a scratch file - the standing policy is reads-only on
the real stick otherwise). On the real Lexar stick: `echo streamtest >
/wtest.txt` created a file; `cp /hello.bin /wcopy.bin` streamed a
byte-complete copy of the 5.7KB binary (catting it back showed the `ELF`
header, the `.rodata` string, and the section-name tail ~5.7KB in - all
three chunks); after a VM reboot `/wtest.txt` still read `streamtest`
(real write persistence), and `echo appendline >> /wtest.txt` then
`cat` showed **`streamtest` then `appendline`** - the partial-sector RMW
of the existing file's sector 0 preserving its content and appending on
real silicon. The scratch files were `rm`'d afterward, leaving the stick
as found. Two runs hit the known stick-at-boot-displaces-the-keyboard
flake (nothing typed); the successful runs used the designed
boot-then-`mount` workflow, same as the grant/safecopy milestone.

**Still coarse, worth knowing before building on this:** no
*interior/random-access* writes - `write_at` refuses an offset past the
current end of file (no sparse files), so it does append and
sequential-overwrite, not seek-anywhere (a future editor/log would want
that); streaming `cp` is non-atomic (truncate-then-append); the *new*
content of a single `write`/`>>` is still bounded by its input source
(the 128-byte line / the 1024-byte capture), which a userland heap +
guard page would lift; and directories still never shrink.

## Driver isolation, part 3: the console server (both stages, framebuffer rendering in userland)

The second component moved out of the EL1 kernel, after the filesystem
server: the *steady-state* console. `cond/` (the seventh userland
program, "console daemon") is boot-loaded into a new protected task slot
3 (`syscall_abi::CON_TASK`), exactly like `fsd` in slot 2. Userland text
output no longer goes straight to the kernel: the shell, `hello`, `pong`,
and `upper` now send their output to `cond` as batched `DSPOP_WRITE`
messages (via `MSG_CALL`), and `cond` forwards it to the kernel console
through a new **gated `CON_WRITE` syscall (33)** - the console analogue of
how `BLOCK_*` is gated to `FSD_TASK`. A `PUTC` fallback in every client
keeps output working if there's no console server this boot (missing
`COND.BIN`, or any call failure) - so output always reaches *a* console.

**Stage 1 was the byte-stream backend** (a second protected server, the
`DSPOP_*` protocol, userland output as an IPC stream - the stdout-over-IPC
substrate the pipe/redirect items wanted), proven on QEMU with zero
framebuffer/MMU changes. **Stage 2 moved the framebuffer text-rendering
logic out of the kernel** - the actual "rendering driver in userland."
`cond` gained a `Framebuffer` backend chosen at startup from a new
`CON_INFO` syscall: it holds the cursor, does line wrap and scroll
*decisions*, parses ANSI, and looks each character up in its **own** copy
of `font.rs` (the font is the thing that was console-driver logic).
The kernel keeps only **dumb pixel primitives** in `fbdev.rs`, gated to
`CON_TASK` like `BLOCK_*` is to `FSD_TASK`: `FB_BLIT` (35, plot a run of
8-byte glyph bitmaps from the server's region at cell (col,row)),
`FB_SCROLL` (36, `ptr::copy` the framebuffer up N rows), `FB_CLEAR` (37).
The framebuffer-access mechanism was a deliberate choice (gated
primitives, not mapping the framebuffer into the server's EL0 view -
which would need the per-view device-mapping MMU surgery that faulted
real Parallels once, the reverted ASIDs; see the plan). `main.rs` calls
`fbdev::install` whenever a framebuffer is discovered and mapped,
independent of which console the *kernel* installs - so on QEMU+`ramfb` a
byte-stream (UART) console wins the kernel's own slot while `cond` still
renders to the framebuffer. **The payoff beyond parity: ANSI works now** -
the kernel's old `fbconsole` never parsed escapes, so `clear` did nothing
on a framebuffer; `cond`'s parser handles `\x1b[2J`/`\x1b[H`, so `clear`
actually clears.

The kernel keeps a minimal emergency console (its existing `fbconsole`
for boot/faults, or the byte-stream UART) regardless - the fault handlers
and all post-`exit_boot_services` bring-up print with no userland
available (see `console.rs`/`exceptions.rs`). On QEMU+`ramfb` the two
don't interfere (kernel logs go to the UART, `cond` to `ramfb` - separate
devices); on a **framebuffer-only** platform (Parallels) they share the
screen (kernel during boot/faults, `cond` in steady state after it
`FB_CLEAR`s on startup). The one recurring interference source - the long
`mmu.rs` identity-map rebuild diagnostic that fired on every spawn/exit -
was made boot-only (a `log` flag through `build_tables`); a couple of
short kernel operational lines (`task N exited`, fault reports) still
reach the kernel console, minor and mostly terminal-ish, left for the
real-hardware assessment.

**A real scheduler fix landed alongside this, needed but not originally
scoped: `MSG_CALL` now switches directly to the destination server.**
Routing per-character echo through IPC exposed that a plain `MSG_CALL`
reply waited up to a full tick - `block_current_and_switch`'s
`next_runnable` picked the always-runnable idle task (slot 1) before the
just-woken server, so the round trip only completed when the round-robin
came back around. At ~a tick per echoed character that dropped burst
input outright (confirmed by a burst-injection test garbling `help` to
`hl`). The ABI's own `MSG_CALL` doc already *claimed* sub-tick round trips
("direct delivery on both hops... without waiting for a tick"), so this
made reality match it: `tasks::block_current_and_switch_to` takes a
`prefer` task and `MSG_CALL` passes the destination (runnable after its
direct delivery), so the server runs, replies (direct-delivered back,
waking the caller), and blocks again all before the next tick. Every
`fsd` call got faster too; burst input renders clean afterward.
`block_current_and_switch` stays as a `prefer: None` wrapper for the
keyboard/wait/plain-recv blocks that shouldn't prefer anyone.

**Stage 1 confirmed on QEMU end to end, both `make run` (byte-stream/
FAT16) and `make run-image` (real FAT32), zero `-d int` aborts:** the
shell banner, `help`, `uptime`, and `echo` all render through `cond`;
`selftest`'s three relocation checks pass (the shell binary changed -
`con_write` was added - so it must still load and relocate correctly);
`ls`/`cat` work via `fsd` (two servers, slots 2 and 3, coexisting with no
cross-talk); a pipe (`echo hi there | /EFI/ORBS/UPPER.BIN` -> `HI THERE`)
and `exec /EFI/ORBS/HELLO.BIN` both route the spawned program's output
through `cond`; `ps` shows all six slots (0 shell, 1 idle, 2 fsd, 3 cond,
4-5 spawnable).

**Stage 2 (framebuffer) confirmed on QEMU `ramfb`, by QMP screendump** -
the only way to check pixel-level rendering, the same technique that
verified the kernel's original `fbconsole` (a `-device ramfb -display
none` boot, a serial socket for input + kernel log, a QMP socket for
`screendump` to a PPM, inspected as an image). Three captures, zero
aborts: (1) the boot/shell banners plus `$ help` with the commands list
**correctly line-wrapped** across three rows; (2) after `clear`, a
blanked screen with only the post-clear content at the top - **ANSI
actually clearing**, which never worked on the kernel's fbconsole; (3)
32 `help`s rendered and **scrolled** cleanly (the `FB_SCROLL` `ptr::copy`
memmove), no corruption. The byte-stream backend was re-confirmed
unregressed on `make run` (no framebuffer -> `CON_INFO` reports
byte-stream -> `CON_WRITE` -> UART). **Not yet on real Parallels
hardware** - the framebuffer backend is exactly what matters most there
(Parallels has no UART console, so `cond` uses the framebuffer path), and
this is the first version reasoned/verified far enough to expect a real
rendered userland console there; that confirmation is on the user, same
as every framebuffer milestone before (this environment can't boot
Parallels).

**Still coarse, worth knowing before building on this:** real-Parallels
confirmation is pending (above); the framebuffer backend has no colour
and only a *minimal* ANSI parser (CSI `J`/`H` acted on, everything else
swallowed - enough for `clear`, not a real terminal); rendering is one
`FB_BLIT` syscall per glyph (fine - each is fast EL1 work while the
client's `MSG_CALL` is blocked, and a run could be batched later);
`con_write`'s `MSG_CALL` carries a 768-byte reply buffer per call (the
ABI's fixed reply size), the one real stack cost; per-character keystroke
echo is one `MSG_CALL` each (sub-tick now, but still an IPC round trip
per typed character); and on a framebuffer-only platform a few short
*kernel-own* operational lines (`task N exited`, fault reports) still
reach the shared screen at the kernel's own fbconsole cursor - confirmed
on real Parallels hardware, where the fix below was verified visually.
**The main visible offender turned out to be `fsd`, not the kernel:** the
filesystem server was the one userland client still printing via `PUTC`
(missed in Stage 1's client sweep), so its `fsd: ...` startup lines
rendered stranded mid-screen while every other program's output went
through `cond`. Fixed - `fsd`'s `print` now uses `con_write` like the
others (a nested `MSG_CALL`, shell -> `fsd` -> `cond`, which resolves
fine: `cond` never calls back), re-confirmed on real Parallels hardware
(the `fsd` line now renders at `cond`'s cursor, interleaved correctly).
The remaining kernel-own lines were then handled too, by the real fix:
**a "kernel console goes quiet once `cond` owns the screen" handoff.**
`console.rs` gained a `CONSOLE_QUIET` flag; `main` arms it right before
`tasks::start` *only* when the kernel's own console is a framebuffer and
`cond` is loaded to take it over (i.e. Parallels) - so ordinary
`console::println!` (task exited, USB/mount diagnostics, ...) is
suppressed there, while on a byte-stream console (QEMU's UART) it stays
on, logs intact for dev. **Fault reports bypass it** via a new
`println_force!` / `console::print_force` (the four fault-reporting sites
in `exceptions.rs` use it) - a fault is worth showing even if it
overwrites the server's screen. Confirmed on QEMU that the byte-stream
path is unaffected (`task N exited` still logs, quiet stays off); the
suppression itself only activates on a framebuffer console, which on QEMU
can't coexist with UART input (a forced framebuffer console kills the
UART read path), so the on-framebuffer behaviour is verified by
inspection here and confirmable on real Parallels hardware. The `fbdev.rs` `ptr::copy` scroll is confirmed on
QEMU's RAM-backed `ramfb`; a Device-nGnRnE framebuffer (if Parallels maps
it outside the RAM span) has stricter ordering rules and is unverified,
the same open question the kernel's `fbconsole` already carried.

## Server supervision + heartbeat: crash recovery for every server, and the first wedge detection

Before this, fault tolerance was narrow and bespoke: crash recovery was
**fsd-only** (`syscall::restart_fsd`, special-cased in the fault handler
as `if current == FSD_TASK`), the console server `cond` had *no* recovery
at all (a `cond` fault left slot 3 dead), and a server stuck in an
infinite loop was never noticed by anything - the top microkernel gap
`roadmap.md` and `microkernel-comparison.md` both named. This milestone
(2026-08-19) builds the general mechanism - MINIX's reincarnation server
/ Helix's self-heal in miniature: a registry of supervised servers,
uniform crash recovery for all of them, and a **heartbeat** that catches
a wedged server and restarts it on the same path as a crash. New module,
`kernel/src/supervisor.rs`.

**The registry** (`supervisor.rs`) generalizes the fsd-only machinery
that used to live in `syscall.rs` (`FSD_IMAGE`/`stash_fsd_image`/
`restart_fsd`/`MAX_FSD_RESTARTS`/`FSD_RESTARTS`, all deleted from there):
a fixed `[Entry; MAX_SUPERVISED]` (4 slots), each holding a supervised
server's `slot`, its raw ELF **image kept from boot** (`[u8; IMG_CAP]`,
128KB - fsd is the biggest today, cond a few KB), a per-boot `restarts`
count, and the heartbeat's `runnable_ticks`. The kept image is
load-bearing for exactly the reason the fsd-only version already needed
it: one of the servers *is* the filesystem, so a dead server can't be
reloaded from disk - the kernel keeps its own copy. Same single-core
`UnsafeCell`/`Sync` contract as the old `FSD_IMAGE` (filled once at boot,
afterward touched only from the fault handler and `on_tick`, both
IRQs-masked, never reentrant).

**Stage 1 - generalized crash recovery** (mirrors the proven fault
path). `loader.rs` registers *both* servers now
(`supervisor::register(FSD_TASK, ...)` in `load_fsd`, and the new
`supervisor::register(CON_TASK, ...)` in `load_cond`);
`exceptions.rs::rust_el0_fault_handler` replaced its fsd special-case
with the generic `if supervisor::is_supervised(current) {
supervisor::restart(current) }`. `restart(slot)` is the generalized
`restart_fsd`: reparse the kept image (`loader::elf_region_size` ->
`allocate_runtime_region` -> `populate_region`), `install_task(slot,
...)`, per-slot restart cap. The **caller** still does teardown + the
mmu rebuild, the exact contract `restart_fsd` had with the fault handler.
The headline new capability: **cond now recovers from a fault too**, not
just fsd.

**Stage 2 - the heartbeat** (the genuinely new detection, in the
scheduler hot path). `tasks::on_tick`, before its round-robin switch,
runs one cheap check per supervised slot: `blocked = matches!(*STATES[slot],
Blocked(_))`, then `supervisor::heartbeat(slot, blocked)`. The detection
is **passive by design** - a healthy server (idle in `msg_recv`, or
briefly busy) keeps returning to a `Blocked` state; a wedged one stays
`Runnable`. So `heartbeat` resets `runnable_ticks` on any `Blocked`
observation and increments it otherwise, returning `true` exactly once
(`== WEDGE_TICKS`, not `>=`, so a give-up past the cap leaves it climbing
without re-firing) when the slot has been continuously `Runnable` for
`WEDGE_TICKS` (128 ticks ≈ 2.5s at the 20ms tick - safely above any real
request, which completes in far less than one tick). On a wedge, the
restart runs on the **exact teardown path the fault handler uses**:
`free_runtime_region` + `revert_input_owner_if` + `fail_calls_to`, then -
if the wedged slot is the interrupted `current` -
`kill_current_and_switch(frame)` + `restart` + `rebuild_with_el0_regions`
+ `return` (skip the normal switch, exactly like the fault handler);
otherwise `kill_task(slot)` + `restart` + rebuild and fall through to the
ordinary round-robin.

**Passive, not an active ping - a deliberate trade-off, documented.**
Reading task state needs *no server changes and no new ABI*: `cond`,
`fsd`, and every future server get supervised for free. The cost is that
it can't catch a server *blocked forever* on a deadlocked call (rarer,
and `fail_calls_to` already rescues the dead-*target* case), and can't
tell a genuine multi-second workload from a wedge - neither exists on
this single-user, fast-request system. An active `*_PING` op the servers
ack is the stronger signal and a clean future refinement, noted rather
than built. **(Update, 2026-08-20: built - see "Active health-ping"
below. It turned out to need no `*_PING` op the servers implement at all:
a server replies to any unknown op, and that reply, addressed to a
reserved `KERNEL_SENDER` sentinel, is the ack - so it kept the
"no server changes" property this paragraph valued.)**

**Confirmed on QEMU, every behavior, by temp fault/wedge injection
(reverted after) - staged crash-recovery-first, per the plan:** the
**critical false-positive check** first - boot, ~14s idle plus normal
commands (`help`/`ls`/`cat`, render-heavy `help` bursts): *zero* wedge or
restart events, shell responsive throughout (a healthy idle/busy server
never trips). Then, with temp triggers: **cond crash** -> `EL0 FAULT
task=3` -> `server slot 3 restarted (attempt 1/3)`, output working after;
**cond wedge** -> `server slot 3 wedged (no progress) - restarting` ->
restart; **cond restart cap** -> `failed more than 3 times this boot -
giving up`, *and* output kept flowing afterward because the shell's
`con_write` falls back to `PUTC` when cond is permanently dead (graceful
degradation, same shape as a missing COND.BIN); **fsd crash** and **fsd
wedge** both restart identically (the generalization preserving fsd's
old behavior while adding the wedge path). A final clean regression
(temp code fully reverted): `selftest` passes, the full disk surface
(`mkdir`/`write`/`cat`/`cp`/`ls`/`rm`/`rmdir`) works, `uptime` advancing,
**no supervisor events**, zero aborts in the `-d int` cross-check.

**A real testing finding worth keeping (not a bug):** the shell writes
its output to `cond` **byte-by-byte** (`putc` -> a 1-byte `DSPOP_WRITE`
each), not batched - so a multi-byte magic string ("CRASHME") never
arrives at `cond` as one contiguous payload, and an early trigger keyed
on `windows(7)`/`len == 7` silently never fired. Proven by an
unconditional-crash probe (crash on the *first* client `DSPOP_WRITE`),
which showed `cond` faulting on the shell banner one character at a time
(`O`, `u`, `r`, ... each its own message + restart). The working triggers
keyed on a single distinctive typed byte instead. The takeaway for future
`cond`-side testing: it sees the shell's output one byte per message.

**Confirmed on real Parallels hardware too - cond crash recovery, live,
the case that matters most here.** The shipped kernel has no fault
injection, so this needed temporary instrumentation (a `cond` crash on a
poison byte, a one-char `z` shell builtin to send it, and the restart log
forced past `CONSOLE_QUIET` - all reverted after, never committed), then
`make test-parallels CMDS="help;z;uptime;z;uptime"`. The captures showed
the recovery unmistakably: after each `z`, the framebuffer cleared to a
fresh `cond: console server ready` banner (the restarted `cond` runs
`FB_CLEAR` then reprints it - the fault line the kernel's emergency
fbconsole drew gets wiped by that clear), and the following `uptime`
rendered a real tick count (`957 ticks since boot`) *through the restarted
`cond`* - the console server, the only thing drawing the screen on this
platform, crashed and came back, twice, with the shell responsive
throughout. This is the first live confirmation that a supervised server
whose own job is the display recovers on the platform where that's the
sole console. (A real *wedge* recovery on Parallels, and `fsd` recovery
there, weren't separately re-exercised - `fsd` needs a disk, still absent
on Parallels; both share this exact restart path, now hardware-proven for
`cond`.) One incidental note for future testers: the synthetic keyboard
(`prlctl send-key-event`) drops the odd keystroke, so a multi-char trigger
word can arrive mangled - a single-char command (`z`) is far more robust,
and firing it twice hedges the rest.

**Still coarse, worth knowing before building on this:** the heartbeat is
passive, so a server *deadlocked while `Blocked`* (waiting forever on a
call that will never complete) isn't caught by *it* - only a `Runnable`
wedge is; the active health ping (see "Active health-ping" below, added
2026-08-20) is the fix, and now catches exactly that case.
The restart cap is a per-boot *total* per slot (crashes and wedges share
it), not rate-based - a server that recovers fine for hours then fails
once still counts against the same 3. A wedged server *past* the cap is
left looping, its CPU share wasted honestly rather than killed (killing
it outright, leaving the slot `Unused`, is the alternative - a dead FS
server already degrades that way on the crash path). And supervision is
still restart-from-a-kept-image only: no journaling, so on-disk state a
server corrupted mid-write before dying stays corrupted (unchanged from
the fsd-only recovery).

## Active health-ping: catching a server wedged while `Blocked`

The one gap the supervision milestone above named as its own next step,
built 2026-08-20: the passive heartbeat catches a server stuck *`Runnable`*
(an infinite loop), but a server stuck *`Blocked`* forever - deadlocked
mid-request, waiting on a reply that never comes - is invisible to it,
because a healthy idle server (parked in `msg_recv`) and a deadlocked one
are indistinguishable from the outside. The only way to tell them apart is
to *poke* the server and see if it responds.

**The mechanism is entirely kernel-side and needs no server changes - the
awkward part, sidestepped.** The kernel isn't a task, so it can't
`MSG_CALL` a server and await a reply the normal way. Instead: when
`supervisor::poll_ping` (driven by `on_tick` alongside the passive
`heartbeat`) observes a supervised server that's sat `Blocked` for a poke
interval (`PING_INTERVAL`, ~1.3s), it returns `Inject`, and `on_tick` calls
the *existing* `tasks::send_message(KERNEL_SENDER, slot, &ping)` with a
reserved sender sentinel (`KERNEL_SENDER = 0xFE`, fitting `Message.sender`'s
`u8` and clear of every real task index). A server idle in its main
`MSG_RECV` (`Blocked(Message{from:None})`) is woken by direct delivery and
replies to whatever "sender" the message carried; that reply, addressed
back to `KERNEL_SENDER`, is intercepted by the `MSG_SEND` syscall arm
(before its normal dest validation) as the ack -
`supervisor::note_ack(current)`. **No new syscall, no `fsd`/`cond` change:**
a server already replies to any unknown op (an `FS_ERROR`/status-0 reply),
and that reply *is* the ack. A `SYSOP_PING` op (`0xFFFF`, clear of the
`FSOP_*`/`DSPOP_*` ops which start at 1) rides in the ping's header purely
for self-documentation.

**The detection turns on which `Blocked` state the server is in - and that
falls out of the message machinery for free.** A server stuck mid-sub-call
(`Blocked(Message{from:Some(x)})`, waiting on a specific reply) is *not*
woken by the ping - `send_message`'s direct-delivery only fires when
`from` is `None` or matches the sender, and `KERNEL_SENDER` matches
neither - so the ping just queues in its mailbox, unseen, and no ack comes
back. That's exactly the deadlock signal: an outstanding ping older than
`PING_TIMEOUT` (~160ms, 8 ticks - far above a healthy ack's tick-or-two)
means wedged, restarted on the *same* teardown path the crash and
runnable-wedge paths already use (`free_runtime_region` +
`revert_input_owner_if` + `fail_calls_to` + reload from the kept image +
mmu rebuild). One ping outstanding at a time (so a server's 4-deep mailbox
can never fill with pings); a `Runnable` server is never pinged (that's the
passive heartbeat's job, and `poll_ping` resets its ping state whenever it
sees the server `Runnable`).

**Honest value framing:** in today's 2-server acyclic topology
(`clients -> fsd -> cond`, `cond` calls no one) a genuine blocked-deadlock
can't actually form - `fail_calls_to` rescues callers when a target *dies*,
and the passive heartbeat restarts a `Runnable`-wedged target (which then
runs `fail_calls_to`). So the active ping is a **forward-looking**
robustness investment: it becomes load-bearing once the call graph grows
(A->B->A cycles, more servers), where two mutually-`Blocked` servers would
otherwise hang undetected forever. Small, low-risk, kernel-only - but with
no natural failing case to demonstrate *today* without temporary artificial
instrumentation.

**Landed in two staged commits (the standing discipline for any
scheduler/SVC-path change).** Stage 1 the inert plumbing (the ABI
constants, the `MSG_SEND` ack interception, the per-`Entry` ping state +
`poll_ping`/`note_ack`), committed and regression-verified byte-identical
first - the point being to prove the ABI change and the new `MSG_SEND`
branch don't perturb the heavily-exercised `fsd`/`cond` IPC in isolation,
before any ping is ever sent. Stage 2 the `on_tick` wiring.

**Confirmed on QEMU (esp.img FAT32), zero `-d int` aborts throughout:**
- **The false-positive gate (critical):** twice - 15s and 10s of idle plus
  render-heavy `help` bursts and the full disk surface
  (`selftest`/`mkdir`/`write`/`cat`/`cp`/`ls`/`mv`/`rm`/`rmdir`) - *zero*
  spurious supervisor events. Healthy servers are pinged every ~1.3s and
  ack well inside the timeout.
- **The ack path is live** (temp instrumentation, reverted): clean
  `inject slot 3 -> ack slot 3` and `inject slot 2 -> ack slot 2` pairs -
  the ping genuinely pokes both servers and both reply.
- **A caught wedge** (temp instrumentation, reverted): an `fsd` deadlock
  injected via an `MSG_CALL` to idle (task 1, which never replies - the one
  `Blocked` state the ping can't wake) produced exactly the intended
  sequence - `inject slot 2` with *no* matching ack -> `server slot 2
  wedged - unresponsive (ping timeout) - restarting` -> `restarted
  (attempt 1/3)` -> `fsd` remounted -> disk commands worked again. `cond`
  (slot 3) kept acking throughout (the wedge was isolated to `fsd`), and
  the shell's blocked `ls /wedge` call was rescued by `fail_calls_to`.

The wedge test's own trigger was a lesson worth keeping: the first attempt
put the temp wedge in `fsd`'s `FSOP_READ_FILE` arm and triggered it with
`cat /wedge` - which did nothing, because `cat` streams via `FSOP_READ_BULK`
(grant/safecopy) now, not `FSOP_READ_FILE`. Moved to a generic check right
after the request header is decoded (any path-bearing op, triggered with
`ls /wedge`), it fired immediately.

**Not re-run on real Parallels hardware this round** - the ping machinery
is inert on a healthy system (a server always acks in time), the same
no-regression posture the supervision milestone's own non-crash paths
already carry; there's no natural blocked-deadlock to exercise on real
hardware without the same temporary instrumentation used on QEMU.

**Still coarse, worth knowing before building on this:** the detection is
heuristic, not a proof - it can't tell a genuine multi-second workload from
a wedge (none exists on this single-user system), and its real value is
forward-looking (no blocked-deadlock can form in the current acyclic
2-server topology - see the value framing above). Timing constants
(`PING_INTERVAL` 64 ticks, `PING_TIMEOUT` 8 ticks) are tunable. The restart
cap is shared with the crash/runnable-wedge paths (a per-boot total per
slot). And, unchanged: no journaling, so disk state a server corrupted
mid-write before wedging stays corrupted.

## The capability model: who-may-call-whom (IPC topology, enforced)

Isolation was MMU-enforced at the *memory* level (per-task page tables,
grant/safecopy) and fault-contained, but the IPC *topology* was still flat
and trust-based: any task could `MSG_SEND`/`MSG_CALL` any task, and the
privileged kernel gates were ad-hoc hardcoded slot checks
(`current_task() == FSD_TASK` for `BLOCK_*`, `== CON_TASK` for the console
device). Nearly every recent milestone's "still coarse" note named the
same gap ("enforced at memory, trust-based at IPC"). This milestone makes
isolation **topological**: a per-slot capability set, enforced at the IPC
boundary, so a task can only reach the endpoints it's allowed to - the
roadmap's last big structural gap vs MINIX.

**The simplification that made it small: capabilities are a pure function
of task slot.** Task-slot roles are static (0 shell, 1 idle, 2 fsd, 3
cond, 4-5 spawnable), so a task's capabilities are a pure function of its
slot - no stored table, no mutable state, no per-creation plumbing, and a
restarted server or a spawned child gets the right caps automatically
(fitting the kernel's no-heap discipline). The entire policy lives in one
`tasks::caps_for_slot(slot) -> u32`: the low `NUM_TASKS` bits are the IPC
**send-mask** (which slots this one may *initiate* a send/call to), the
high bits are resource caps (`CAP_BLOCK` = may use `BLOCK_*`; `CAP_CON` =
may use `CON_WRITE`/`CON_INFO`/`FB_*`). (Runtime capability *delegation* -
a task granting a cap to another - would need mutable state and is
explicitly future work; v1 is a static policy.)

The policy, validated against every real IPC flow:

| slot | role | send-mask | resource |
|---|---|---|---|
| 0 | shell | {2 fsd, 3 cond, 4, 5} | - |
| 1 | idle | {} | - |
| 2 | fsd | {3 cond} | `CAP_BLOCK` |
| 3 | cond | {} | `CAP_CON` |
| 4,5 | spawnable | {0 shell, 2 fsd, 3 cond} | - |

**The reply exemption is what makes request/response work, and it's the
subtle piece.** A server replies to a caller via `MSG_SEND(caller)`, and
the caller is blocked in a `MSG_CALL` to that server
(`Blocked(Message{from: Some(server)})`). Such a reply is *always* allowed
regardless of the send-mask - it completes an authorized round trip rather
than initiating a new one. This is the same "the client is blocked in a
call to me" condition `SAFECOPY` already keys off, and it's what lets
`cond` (send-mask `{}`) reply to any caller. So only *unsolicited* sends
are mask-checked: `tasks::may_send(src, dest)` returns `true` if it's such
a reply, else if `caps_for_slot(src)`'s send-mask has `dest`. Enforced in
the `MSG_SEND` and `MSG_CALL` syscall arms, returning a new
`MSG_ERR_DENIED`. The kernel's own supervisor ping bypasses this entirely
(it calls `send_message` directly, not through the syscall boundary; its
ack is intercepted before validation).

**The one flow that shaped the policy - and why the child mask includes
the shell:** `pong` (a spawned echo server) replies to a `send`/`recv`
client via an *unsolicited* `MSG_SEND(0)` - the shell did `send` then a
*separate* `recv`, so the shell isn't blocked in a call to pong, and the
reply exemption does *not* fire. That send is mask-governed, so the child
send-mask has to include slot 0. It's the specific non-reply
server->client send the "every flow intact" gate had to prove.

**Landed in two staged commits.** Stage 1 folded the two hardcoded
resource gates into the capability vocabulary
(`block_access_allowed`/`con_access_allowed` -> `cap_has(current,
CAP_BLOCK/CAP_CON)`) - a pure refactor, equivalent by construction (fsd
is the only `CAP_BLOCK` slot, cond the only `CAP_CON`), verified
byte-identical (fsd still mounts/reads the disk, cond still renders all
output). Stage 2 added the send-mask + `may_send` enforcement.

**Confirmed on QEMU, zero `-d int` aborts across every run:**
- **Every existing flow intact:** `selftest`, the full disk surface over
  IPC (shell->fsd), all console output (shell->cond), the pipeline
  (shell->child->cond), the pong echo (shell->child *and* pong's
  unsolicited send->shell), `exec`/`ps` - no false denials.
- **Denials fire (A/B, temp probe reverted):** `send 1` from the shell
  (shell->idle, not in the shell's mask) -> `permission denied`; and a
  temp-instrumented spawned `hello` refused a `MSG_SEND` to idle (outside
  its `{shell,fsd,cond}` child mask) - proving the mask is enforced
  against untrusted *spawned code*, the actual security point - while
  permitted sends (the pong echo) succeed in the same run.

**Still coarse, worth knowing before building on this:** the policy is
*static* (a pure function of slot) - no runtime delegation, so a spawned
program that legitimately needs to reach an endpoint outside
`{shell,fsd,cond}` (another server, a sibling for program-to-program
pipes) can't be granted it yet; that's the natural follow-up (a
spawn-with-capabilities API, or a delegation primitive). `GRANT`/`SAFECOPY`
were left as-is - already the enforced-capability model for bulk transfer
(the grant + active call *is* the capability). And the send-mask is
coarse-grained per-slot: it says *whether* A may message B, not *what* A
may say (message contents are still trusted, same as every pointer the
syscall boundary already trusts within the single address space).

## Program-to-program pipes and `exec … > file`: stdout-over-IPC

Pipes were **builtin-left only** - `builtin | /path/program` worked (the
shell captures the builtin's output into a 512-byte buffer and streams it),
but `programA | programB` didn't, because a *task's own* output wasn't
capturable: a spawned program's output went straight to the console
(`con_write` -> `cond`). Same reason `exec prog > file` didn't exist. This
milestone makes a task's output routable, delivering both.

**The scoping finding that shaped it (and dropped delegation).** I picked
this expecting it to "absorb capability delegation," but the clean design
doesn't need it. The cleanest pipe has the shell **relay**
(`producerA -> shell -> consumerB`), and `producer -> shell` is *already*
permitted by the capability send-mask (a spawned task's mask includes the
shell). So no capability is granted to anyone. Better, the *same* mechanism
- "a program's stdout can be routed to the shell instead of the console" -
delivers **both** `programA | programB` (the shell relays A's output to B)
*and* `exec prog > file` (the shell captures A's output and writes it to a
file). The direct-streaming alternative (A sends straight to B, needing a
delegated A->B capability) is worse engineering here: it pushes
flow-control/retry logic into every producer, gives no help to `exec >
file`, and exists only to exercise delegation. So this milestone uses the
relay model; **delegation stays a separate future item** (for direct
task-to-task streaming, or a program that runs its own server).

**The mechanism: a per-task `stdout_target`.** A spawned program's output
goes to a stdout target - a task index, defaulting to `CON_TASK`. The shell
sets it per spawn: normal `exec prog` / `builtin | prog` -> the console; a
pipe producer or `exec prog > file` -> the shell itself. Three ABI
additions: `SPAWN` (16) gained a second argument (the target - existing
callers pass `CON_TASK`); `STDOUT_TARGET` (38) returns the caller's target;
`SELF` (39) returns the caller's own task index, which the shell needs to
route a producer's stdout back to itself (a foreground-spawned shell isn't
task 0). Kernel-side it's a per-task `STDOUT_TARGET` array
(`tasks.rs`, default `CON_TASK`, set at spawn, reset on death) - small.

**A producer routes output through its target** (`hello`, the one generator
program, and any future one): `target == CON_TASK` -> `con_write` (the
`DSPOP_WRITE` console path, unchanged); otherwise a raw byte stream
(`MSG_MAX_LEN`-chunked data messages, with a bounded full-mailbox retry) to
the target, plus an empty end-of-stream message when done. **The consumer
side of a pipe is unchanged** - `upper` stays an ordinary stdin
(`msg_recv`) -> console filter that exits on the empty EOF message; in the
relay model its stdin just comes from the shell's relay rather than a
captured builtin buffer.

**The shell orchestration** (`shell/src/main.rs`): `cmd_pipeline` branches
on a `/path` left into `cmd_pipeline_prog`, which spawns the consumer
(stdout -> console), spawns the producer (stdout -> this shell via
`self_task()`), relays each chunk the producer sends on to the consumer
(the existing `pipe_send` + timeout-kill), forwards the empty EOF, and
`wait`s both (Ctrl+C interrupts the relay and kills both). `cmd_exec` now
receives the redirect sink: `out.is_console()` -> the current
fire-and-forget spawn; otherwise spawn with stdout -> shell,
`capture_program_output` relays into the sink until EOF and reaps, and
`run_line`'s existing `finish_redirect` writes the file (a Ctrl+C/error
kills the program and marks the capture overflowed, so no partial file is
written).

**Confirmed on QEMU, zero `-d int` aborts across every run:**
- `/EFI/ORBS/HELLO.BIN | /EFI/ORBS/UPPER.BIN` -> both of hello's lines
  rendered uppercased through the relay (`HELLO...` / `GOODBYE...`), both
  tasks exit cleanly and reap; the existing `builtin | program` pipe
  (`echo | UPPER` -> `BUILTIN PIPE`) unchanged.
- `exec /EFI/ORBS/HELLO.BIN > /h.txt` then `cat /h.txt` -> hello's captured
  banner + goodbye; the builtin `>`/`>>` redirect and plain `exec`
  (fire-and-forget to the console) unchanged.
- `selftest`, `ls`, `echo` unaffected.

**Still coarse, worth knowing before building on this:** 2-stage only
(`a | b | c` needs shell-side chaining of relays); the only producer
program is `hello` (any future generator follows the same `stdout_target`
pattern - `upper` is a consumer/filter, unchanged); `exec > file` is
capture-bounded (512 bytes, `refuse-not-truncate`, same as existing
redirects - pipes themselves stream unbounded since they relay rather than
buffer); the relay routes through the shell (a bottleneck, fine at this
scale - direct task-to-task streaming is what delegation would later
enable); and a producer's output helper (`hello`) is per-crate, not shared
(same as `con_write` today - a shared userland runtime crate is a broader
future cleanup). **(Update, 2026-08-21: the "relay routes through the shell"
bottleneck is gone for program-to-program pipes - runtime capability
delegation now lets the producer stream *directly* to the consumer, shell
out of the byte path. See "Runtime capability delegation" below. `exec >
file` still routes through the shell, since capture is the point there.)**

## Stack guard page: silent overflow becomes a clean fault - and it found a real bug

Every userland task ran on a fixed 8KB stack with **no guard**. The region
layout was tight - `[code_pages][2 stack pages]`, no gap - and the stack
grows *down* from the top (`sp_el0 = base + size`), so an overflow
descended straight into the program's own code, which is still
EL0-accessible: **silent corruption, no fault.** This was the one "still
coarse" item the whole isolation arc kept flagging (grant/safecopy's buffer
sizing was hand-tuned specifically against this unguarded stack).

**The fix: one inaccessible guard page immediately below the stack.**
`loader.rs` grows each region by a page - `[code_pages][1 guard page]
[STACK_PAGES stack pages]`, stack still at the top, code still at the
bottom, only the region grows. `mmu.rs::build_view` derives the guard's
address from the region's `(base, size)` (no plumbing change to
`el0_regions`) - `base + size - (STACK_PAGES+1)*PAGE`, the page just below
the stack - and maps that one L3 page `kernel_page_4k` (EL1-only), a hole
*inside* the EL0 region. An overflow into it takes an EL0 permission fault,
which the existing fault-isolation handler already contains: kills just
that task (and, for a supervised server, a supervisor restart), or halts
for task 0/1 (nothing meaningful survives the shell/idle dying). The
address is size-gated so the single-page idle region (a bare asm busy-spin
that never uses a stack) gets no guard. No ABI change, no userland change,
no policy. **The one duplicated value: `mmu.rs`'s `guard_page_addr` carries
its own `STACK_PAGES` const that must equal `loader.rs`'s** (the same
duplicate-a-value pattern `RUNTIME_SLOT_ALIGN` already uses) - a mismatch
misplaces the guard.

**The guard immediately earned its keep by catching a real, silent,
pre-existing bug - the whole point, demonstrated on the first injection
test.** `exec /EFI/ORBS/HELLO.BIN` faulted the *shell* (task 0), not the
spawned program: `EL0 FAULT task=0 esr_el1=0x9200004f`
(EC 0x24 Data Abort from a lower EL, DFSC 0x0f permission fault level 3 - an
EL0 write to the EL1-only guard page) `far_el1=0x5c60cfe0`, exactly 32 bytes
below the shell's 8KB stack bottom, inside its guard page. The shell's own
`exec` path - `cmd_exec` -> `spawn_path` -> `fs_call`'s 768+768-byte
request/reply buffers + the 512-byte chunk staging + the call chain - uses
just over 8KB of stack, so it had been overflowing by ~32 bytes into the
top of its code region *every single time*, silently, unnoticed because
nothing was mapped there to fault on it (the corrupted bytes evidently
weren't critical). **Fixed by growing the stack 8KB -> 16KB**
(`STACK_PAGES` 2 -> 4). This is a real "the guard page paid for itself
immediately" result, not a hypothetical.

**Confirmed on QEMU, both directions, zero unexpected aborts:**
- **No false faults** (16KB stack): `selftest`, the previously-faulting
  `exec HELLO.BIN` (now clean), streaming `cp` of the 72KB `SH.BIN`, the
  cat-stream+redirect-capture nesting (grant/safecopy's own worst case),
  and the `HELLO.BIN | UPPER.BIN` program-pipe all ran with **zero EL0
  faults**, zero `-d int` aborts. Real usage stays well within 16KB, so the
  guard (below it) is never touched.
- **Catches a real overflow, contained** (temp recursion in `hello`, a
  per-frame 256-byte buffer used after the recursive call to defeat
  tail-call elimination, `black_box(depth)` to stop the compiler proving it
  infinite - reverted after): `EL0 FAULT task=4 ... far_el1` 8 bytes into
  hello's own guard page -> `task 4 killed after fault` -> the shell stayed
  fully responsive (`uptime`/`ls` after). The one `-d int` abort was
  exactly the injected overflow, caught and contained.

**Still coarse, worth knowing before building on this:** a single stack
frame larger than one page (4KB) - a giant local array - could *skip over*
the one-page guard into the code below, the standard single-guard-page
limitation; incremental overflows (recursion, normal frame growth) hit it,
and it's strictly better than the previous silent corruption regardless. No
userland heap still (a separate item) - programs are still fixed-buffer,
now on a 16KB guarded stack instead of an 8KB unguarded one. And a
mismatched `STACK_PAGES` between `loader.rs` and `mmu.rs` would silently
misplace the guard - kept equal by a cross-referencing comment on each.

## Userland heap: a raw buffer, because `alloc` can't link under this loader

Programs were fixed-buffer only - `#![no_std]`, no allocator, `.bss`/`.data`
asserted empty (no static mutable state), every buffer a stack local. So
the shell's redirect/pipe capture was a 1024-byte stack array and `cat big
> file` *refused* anything larger (`docs/processes.md`'s "known rough
edges" kept flagging this). This gives each program a heap.

**The go/no-go gate, and the useful negative result it produced.** The
milestone opened by testing the risky assumption directly - can a real
`alloc`-backed heap (`Vec`/`String`/`Box`) even be built here? - before
investing in the plumbing. It **can't, on stable:** adding a
`#[global_allocator]` + `extern crate alloc` + a `selftest` `Vec`/`String`
check failed at *link* with `relocation R_AARCH64_ABS64 cannot be used
against local symbol; recompile with -fPIC`, from prebuilt lib`alloc`'s own
`.rodata` (anonymous const data - vtable-ish absolute pointers that
`Vec`/`String` pull in unavoidably). This is the exact
`R_AARCH64_ABS64`-in-PIE wall already documented for `slice_error_fail`/
`memrchr`/`find`, one level deeper: the prebuilt lib`alloc` on stable wasn't
built `-fPIC`, so its absolute relocations can't survive a `-pie` link. The
only fix is `-Z build-std` (rebuild lib`alloc` with this project's PIE
flags), which is **nightly-only** and breaks the stable-only invariant
(`rust-toolchain.toml`; the relocating-loader milestone already declined
`-Z build-std` for exactly this reason). **One build retired the whole
risk** - the gate did its job - and the answer forced a pivot (decided with
the user) from an `alloc` heap to a **raw buffer** one.

**The design that shipped: a kernel-provided raw heap area, no allocator.**
Each program's region grew to `[code][256KB heap][guard][16KB stack]`
(`loader.rs`, `HEAP_PAGES = 64`). The guard page is *unchanged* - still
`STACK_PAGES+1` pages from the region end (just below the stack); the heap
sits *below* the guard, above the code, and is ordinary EL0-accessible
region memory (no `mmu.rs` change - the heap pages are `el0_page_4k` like
the code, only the guard is the EL1-only hole). A `heap_info` syscall (40,
`field` like `con_info`) reports the area's `(base, size)` - the kernel
computes it from the region's `(base, size)` and the fixed layout
(`loader::heap_area`, `HEAP_PAGES+GUARD+STACK` pages down from the end;
`(0, 0)` for a region too small, i.e. the idle task's single page). A
program reaches it with a `&mut [u8]` - a raw buffer, *not* `Vec`/`Box`/
`String`. **No `GlobalAlloc`, so no static state, so the `.bss` assert
stayed in place** - the raw-buffer approach sidesteps the whole
static-state problem the `alloc` version would have needed to solve.

**The consumer: the shell's redirect/pipe capture is heap-backed now.**
`get_heap()` (in `shell/src/main.rs`) returns the heap region as a
`&'static mut [u8]` (safe by the shell's single-capture-at-a-time
discipline - a redirect *or* a pipe, never nested, so no aliasing);
`Output::Capture` holds a slice instead of a 1024-byte stack array;
`run_line`'s redirect path and `cmd_pipeline`'s builtin-left path both
capture into it. Because a single `fs_write_*` is `SAFECOPY_MAX`-capped, a
large capture is written to disk in `SAFECOPY_MAX` chunks by a new
`write_all` helper (`>` truncates then writes from 0; `>>` appends at the
stat'd EOF). `CAPTURE_SIZE` (the old 1024-byte cap) is gone.

**Confirmed on QEMU, zero `-d int` aborts:**
- **The headline win:** `cat /EFI/ORBS/SH.BIN > /big.bin` - the full 72KB -
  captures completely and writes it (chunked), where it used to refuse at
  1024 bytes. Proven by round-tripping: `cat /big.bin | /EFI/ORBS/UPPER.BIN`
  renders the *uppercased* ELF section names (`.TEXT`/`.SHSTRTAB`), so the
  whole file came back through the heap capture, the chunked write, a fresh
  read, and the pipe.
- Regressions: small `>` (`echo … > f`), `>>` append (lines in order), and
  the builtin-left pipe (`echo … | UPPER` -> uppercased) all still work;
  `selftest`, `exec`, streaming `cp`, and the disk surface unaffected. The
  region grows ~256KB per program (fits the 2MB slot); the idle region
  correctly gets no heap.

**Still coarse, worth knowing before building on this:** it's a *raw
buffer*, not dynamic collections - a program that wants `Vec`/`String`/`Box`
still can't have them (blocked by the `alloc`/PIE wall above; a hand-rolled
growable type over the heap is the workaround if ever needed). One
fixed-size area per program (256KB, no growth). The shell is the only
consumer so far (any future program gets the heap via `heap_info` the same
way). And `get_heap`'s `&'static mut` is sound only under the shell's
one-capture-at-a-time usage - a program capturing two things at once would
alias it.

## Runtime capability delegation: relay-free program-to-program pipes, and a self-securing rule

The capability model (see "The capability model" above) enforced the IPC
topology with a **static** per-slot send-mask: `tasks::caps_for_slot(slot)`
is a pure function of slot, so a spawned program could reach
`{shell, fsd, cond}` and nothing else. That made a program-to-program pipe
(`/prog_a | /prog_b`) impossible to stream directly - neither spawnable
slot's mask includes the other - so the shell relayed every chunk
(`producer → shell → consumer`, the stdout-over-IPC milestone), sitting in
the byte hot path. This milestone (2026-08-21) adds **runtime delegation**:
the shell hands the producer a capability to send straight to the consumer,
taking itself out of the loop. It's the "hard consumer" the
capability-and-hardening postmortem said delegation lacked when it shipped
the pipes *without* it.

**The self-securing insight, which is the whole reason this stayed small.**
The safety rule is "you may only delegate a send-capability you *statically*
hold" (`tasks::may_delegate` checks `caps_for_slot`, **not** the dynamically-
delegated slot - no transitive re-delegation, so nothing can be laundered
onward). That rule falls out perfectly here: only **slot 0 (the shell)**
statically holds `TO_SPAWN_4 | TO_SPAWN_5`; spawnable tasks hold only
`TO_SHELL | TO_FSD | TO_CON`. So **only the shell can authorize
producer→consumer streaming** - a spawned program literally cannot delegate
that reach, because it doesn't have it. No `CAP_DELEGATE` bit, no
delegation-gate slot check: the existing static policy secures delegation by
construction.

**The mechanism** mirrors the single-slot `GRANTS`/`STDOUT_TARGET`
precedent. One delegated send-target per task (`tasks::DELEGATED_SEND`, a
`NO_DELEGATION` sentinel = `NUM_TASKS`); `may_send` consults it after the
static mask (`DELEGATED_SEND[src] == dest`); `DELEGATE` (syscall 41) sets it
after `may_delegate` passes; `clear_delegate` (wired into all three teardown
paths - `exit`/`kill`/fault - alongside `clear_grant`) clears **both** a
dying task's delegated-out target *and* any delegation pointing *at* it (a
cheap `NUM_TASKS` scan), so a reused slot can never inherit a stale one. One
delegated target per task is enough for the one consumer (a 3-stage
`a | b | c`, whose middle stage would both send and receive, is out of scope,
shell-side too).

**The shell consumer** (`cmd_pipeline_prog`): spawn the consumer
(stdout → console), spawn the producer with its stdout aimed straight at the
consumer's slot (not back at the shell), `DELEGATE(producer, consumer)`, then
only `wait` on both - the relay loop is gone. **The producer program needed
no change** - `hello` already routes output to its `stdout_target` (a raw
`msg_send` stream + empty end-of-stream message when the target isn't the
console), so pointing it at the consumer just works, given the permission.

**The one real correctness piece: the spawn/delegate race, closed in the
producer.** A tick can preempt the shell in the window between spawning the
producer and calling `DELEGATE`, letting the producer run and attempt its
first send *before* it's authorized - which returns `MSG_ERR_DENIED`. Rather
than pre-arranging slot numbers (brittle - a pre-existing spawned task
breaks the assumption), `hello`'s `pipe_out` (and its EOF send) now retry on
`MSG_ERR_DENIED` as well as `MSG_ERR_FULL`: a denied-then-allowed send is
exactly the transient the existing bounded (~3s) retry is for, and the shell
delegates within one tick (~20ms), far inside that budget. A genuinely
never-allowed target (a bug) still times out and gives up, same as a
never-draining mailbox.

**The documented tradeoff:** the old relay auto-killed a non-reading consumer
(the shell was feeding it, so it knew). In the direct model the shell is out
of the loop, so it can't - a non-reading consumer leaves the producer's
bounded retry to give up and exit, after which the shell's `WAIT(consumer)`
blocks until Ctrl+C, leaving the consumer alive in the background (`kill` it
via `ps`). Accepted, not a regression in reachability: the shell is never
unrecoverably stuck (Ctrl+C always rescues the wait).

**Staged, the standing discipline for any scheduler/SVC-path-adjacent
change.** Stage 1 the inert kernel primitive (`DELEGATE`, `may_delegate`,
`DELEGATED_SEND`, the `may_send` clause, teardown clearing) with **no
caller** - provably inert, since `DELEGATED_SEND` stays `NO_DELEGATION` for
every task until something delegates, so `may_send` is unchanged for all real
traffic. Verified on QEMU by a **temporary race-free kernel self-test**
(reverted): `may_send(4,5)` went `false → true → false` across
delegate/clear, and `may_delegate` was `true` for the shell (holds
`TO_SPAWN_5`) but `false` for a spawnable child - the self-securing property,
proven directly. Plus a piped-stdin regression against real FAT32
(`selftest`'s three relocation checks, the disk surface, the existing
builtin-left pipe, `exec`/`exit`), byte-identical, zero `-d int` aborts.
Stage 2 the shell/`hello` wiring.

**Confirmed working end to end on QEMU, zero aborts across three sessions:**
`/EFI/ORBS/HELLO.BIN | /EFI/ORBS/UPPER.BIN` streamed both of hello's lines
uppercased (`HELLO FROM A SECOND PROGRAM!` / `GOODBYE - EXITING NOW.`) with
the producer (task 5) exiting, then the consumer (task 4) finishing and
exiting, both reaped - **both lines arriving intact is the direct proof the
race is closed** (no dropped chunk). The builtin-left pipe
(`echo hi there | UPPER.BIN` → `HI THERE`) and `exec HELLO.BIN > /pout.txt`
(the other stdout-target consumer, unaffected by `hello`'s `pipe_out` change)
were byte-identical. The non-reading case (`HELLO.BIN | SH.BIN` - a spawned
shell never reads its mailbox) behaved exactly as designed: hello timed out
and exited, the shell blocked on `WAIT(consumer)`, a raw `0x03` interrupted
it (`pipe: consumer wait interrupted`), and `ps`/`uptime` afterward confirmed
the shell responsive with the non-reader still alive in the background.

**Not re-confirmed on real Parallels hardware this round** - the primitive is
inert on a healthy system (nothing delegates until a program-to-program pipe
runs), and the success path additionally needs the userland binaries on the
USB stick plus `mount` (the standing reads-only-on-the-real-stick posture);
a clean boot with the kernel change present was already confirmed on QEMU,
and the boot/typing regression is the realistic scripted `make test-parallels`
check if wanted.

**Still coarse, worth knowing before building on this:** one delegated target
per task (enough for a 2-stage pipe; `a | b | c` needs more); non-transitive
by rule (the self-securing property depends on it); in practice only the
shell ever delegates (the same rule); and the lost auto-kill of a non-reading
consumer (Ctrl+C is the escape). A general, transitive, revocable
capability-passing mechanism (any task handing any held capability onward -
MINIX's full grant model) is the remaining gap, and what a spawned program
running its *own* server, or true relay-free `a | b | c`, would need.

## FAT32 interior / random-access writes: `write_at` past EOF, and a `writeat` builtin

The FAT32 offset-write milestone (`fat32::write_at`, see above) deliberately
**refused any offset past the current end of file** (`Error::InvalidOffset`)
- a sparse gap FAT32 can't represent - so it only ever did append and
sequential-overwrite, which is all its callers (`cp` streaming, `>>` append)
ever needed. This milestone lifts that: `write_at` now supports a true
**random-access write** at any offset, zero-filling the gap when the offset
is past EOF. It's the frontier item `docs/roadmap.md` named next (concrete,
unblocked, self-contained - entirely in the fsd server's `fat32.rs` plus its
`FSOP_*` layer, no kernel/scheduler/MMU risk).

**A finding that shaped the scope: most of "interior writes" already
existed.** `write_at`'s per-sector loop already RMW'd a partial sector
overlapping existing content, so an *interior* overwrite (offset ≤ old_size)
was already coded - it just had **no caller that exercised it** (`cp`/`>>`
are sequential/append only, always offset ≤ old_size). And `FSOP_WRITE_AT`
(op 13) + the shell's `fs_write_at` wrapper (bulk data via grant/safecopy)
already existed. So the real gaps were exactly two: the `offset > old_size`
refusal (no zero-filled gap), and no *user-facing* way to invoke an
arbitrary-offset write. No new syscall or FSOP op was needed.

**The zero-fill, done by generalizing the existing loop rather than bolting
on a separate pass.** When `offset > old_size`, the gap `[old_size, offset)`
must become **real zero bytes on disk** - FAT32 has no sparse representation,
and the clusters `extend_chain` allocates are *not* zeroed, so without this
the gap would be garbage (the correctness crux). The clean implementation:
the per-sector loop now runs one unified pass over
`[min(old_size, offset), offset + len)`, and for each sector builds its
bytes from **zeros for positions before `offset`** (the gap) and **`data`
from `offset` on**. This means:
- The boundary sector that straddles `old_size` (unaligned old size) is
  handled by the *existing* `sector_start < old_size` RMW branch - it reads
  the sector, splices in the chunk (which is zeros past `old_size`),
  preserving the real bytes before `old_size`. No special-casing.
- The same-sector case (a small gap within one sector) falls out of the same
  chunk-building - the zeros and data are spliced together in one RMW, so
  the stale `[old_size, offset)` bytes are overwritten with zeros rather than
  left as garbage (the subtle bug a naive "write only at offset" would have).
- Full interior sectors and fresh past-EOF sectors take the existing
  direct-write / zero-pad branches unchanged.
- It's **byte-identical for the offset ≤ old_size paths** (when
  `offset ≤ old_size`, `write_start = offset` and every chunk byte is `data`,
  exactly the old behavior) - verified, so `cp`/`>>` don't regress.

**A `MAX_GAP_FILL` (1 MiB) cap**: a gap is zero-filled sector by sector, so a
fat-fingered huge offset must not try to zero-fill the whole volume.
`write_at` refuses `offset - old_size > MAX_GAP_FILL` with
`Error::InvalidOffset` - which also keeps that variant *constructed* (it was
formerly only produced by the now-removed refusal, and is still matched in
the fsd error mapping, where it maps to `FS_ERR_IO` → the shell's "device
I/O error"). A dedicated error code for it would be ABI expansion for a rare
guard; `FS_ERR_IO` is acceptable.

**The consumer: a `writeat <file> <offset> <text...>` shell builtin** -
a random-access write, in place, exercising both the previously-unreachable
interior-overwrite path and the new past-EOF zero-fill. Unlike `write`
(full replace) it leaves the bytes outside the written window intact, and
unlike `write` it does *not* create the file (the file must already exist -
`write_at` needs the entry; use `write`/`touch` first). This is the
project's standing "prove a primitive via its first consumer" pattern -
`write_at`'s interior/past-EOF paths had never been reachable before.

**Confirmed working end to end on QEMU (real FAT32 `esp.img`), zero `-d int`
aborts across every session - with the gap bytes verified as real `0x00` by
hex-inspecting the raw serial log on the host** (no in-guest hexdump exists;
`cat`'s raw bytes, NULs included, land in the captured serial output):
interior overwrite (`write /f.txt AAAAAAAAAA` → `writeat /f.txt 3 XY` →
`cat` → `AAAXYAAAAA`, proving RMW preserves the surrounding bytes - the
path `cp`/`>>` never reached); append at exact EOF; a past-EOF
**single-sector** gap (5 bytes, all `0x00`); a past-EOF **multi-sector** gap
(`writeat /h.txt 1200 ENDHERE` on a 5-byte file → the 1195-byte gap read
back **all `0x00`, only distinct value `[0]`** - the case that exercises
freshly `extend_chain`'d clusters and would have shown garbage if the
zero-fill were wrong); the error cases (`writeat` on a nonexistent file →
"no such file or directory"; past the 1 MiB cap → "device I/O error");
**reboot persistence** (the multi-sector gap's zeros and the interior
content both survived a fresh boot against the same `esp.img`, proving the
writes reached real disk sectors); and FAT16 degradation (`make run`'s
unmountable vvfat → the shared "no filesystem mounted" message; `help` lists
`writeat`). **Not yet re-confirmed on real Parallels hardware** - the fat32
path is only reachable there via a mounted USB stick, under the standing
reads-only policy (a scratch write needs an explicit go-ahead); the
boot/typing regression is the realistic scripted check otherwise.

**Still coarse, worth knowing before building on this:** `writeat` requires
the file to already exist (no create-on-write); the new content of one
`writeat` is bounded by the shell's input line (`BUFFER_SIZE`, 128 bytes),
and a single `write_at`/`fs_write_at` call is still capped at `SAFECOPY_MAX`
(2048) like every other bulk op; a past-EOF gap is capped at 1 MiB
(`MAX_GAP_FILL`); the 1 MiB-cap error surfaces as the generic "device I/O
error" (no dedicated code); and there's still no in-guest way to *inspect*
raw bytes (no hexdump), so a NUL gap looks blank in `cat` - the zero-fill
was verified host-side. Directories still never shrink, and everything else
about the module's scope (8.3-only, no LFN, first-FAT32-partition-only) is
unchanged.

## Network stack, Stage 1: a virtio-net driver, and the first frames this kernel has ever sent

The first networking this project has ever had. Stage 1 of the network-stack
arc scoped in `docs/roadmap.md`: the **kernel-side virtio-net driver only** -
the DMA-owning half that, per the no-IOMMU DMA constraint, must stay in the
trusted EL1 kernel (without an IOMMU a device can DMA anywhere, so the
ring/buffer owner can't be an untrusted EL0 task - the same rule that keeps
virtio-blk in the kernel). The protocol stack (ARP/IP/ICMP/UDP/TCP) is a
later stage's userland `netd` server; this driver just moves opaque frames.

**Built directly on the existing virtio infrastructure.** `virtio_net.rs`
reuses `virtio_mmio.rs`'s transport (the same one `virtio_blk.rs` proved
out) and is modeled on the block driver's shape: static aligned virtqueue
structures in the `UnsafeCell`-wrapper idiom, feature negotiation, and
poll-the-used-ring completion (no IRQ wiring, matching every driver here).
The `Desc`/`Avail`/`Used` ring types are redefined locally rather than shared
with `virtio_blk.rs` - a small, deliberate duplication (the
`RUNTIME_SLOT_ALIGN` call) that keeps the new module from touching the proven
block driver at all.

**Two real differences from virtio-blk, both handled:**
- **Two virtqueues, not one.** virtio-net has a receiveq (queue 0) and a
  transmitq (queue 1), each with its own desc/avail/used rings. Receive
  buffers are *pre-posted* at init (the device fills them asynchronously as
  frames arrive) and drained incrementally by `poll_frame`, then re-posted -
  unlike the block driver's strictly one-request-at-a-time model. Transmit
  stays one-at-a-time (`send_frame`).
- **A 12-byte `virtio_net_hdr` prefixes every frame** in both directions.
  `VIRTIO_F_VERSION_1` makes it 12 bytes (including the trailing
  `num_buffers`), regardless of `VIRTIO_NET_F_MRG_RXBUF` - which is
  deliberately *not* negotiated, so every frame fits one buffer and
  `num_buffers` is always 1. On transmit the header is all zeros (no
  checksum/GSO offload requested); on receive the device writes it and
  `poll_frame` skips past it.

**Feature negotiation spans both feature words**, unlike virtio-blk (which
touched only the high word): `VIRTIO_NET_F_MAC` is low-word bit 5,
`VIRTIO_F_VERSION_1` is high-word bit 0. The MAC is read from config space
after negotiation (the first `virtio_net_config` field, 6 bytes at offset 0).
`send_frame`'s TX-completion poll uses a **real wall-clock deadline** off the
generic timer (`timer::now_ticks`/`frequency_hz`), the xHCI-driver lesson
that an iteration count is not a valid proxy for elapsed time under a
hypervisor - not the unbounded loop virtio-blk still uses.

**`init_net` in `main.rs` is Stage 1's end-to-end proof**, modeled on
`init_storage`: it discovers + inits the NIC, logs the MAC, then sends a
**broadcast ARP request** for the QEMU user-net gateway (`10.0.2.2`, "tell
`10.0.2.15`") and polls for the reply, decoding it - proving transmit *and*
receive, not just "no error." The IP addresses are a hardcoded QEMU user-net
convention (a QEMU-shaped choice like the device-region addresses elsewhere),
replaced by real ARP/DHCP in `netd` later. Gated behind
`virtio_mmio_probe_safe` exactly like `init_storage` (the virtio-mmio scan
crashes real Parallels hardware, and Parallels exposes virtio-net over PCI
anyway - a transport this project doesn't have, so **Stage 1 is QEMU-only by
design**). Silent when no NIC is attached (`NotFound` returns without
logging), so plain `make run`/`run-image` are unperturbed.

**Confirmed on QEMU two ways, the "verify against a source outside the
kernel's own output" discipline** (the same reasoning behind the FAT32
gap's host-side hex inspection). A new `make run-net` target attaches
`virtio-net-device` over virtio-mmio with QEMU user-mode (SLIRP) networking
plus an `-object filter-dump` writing every frame to `net.pcap`:
- The kernel's own log: `virtio-net ready, MAC 52:54:00:12:34:56` then
  `virtio-net ARP reply - 10.0.2.2 is at 52:55:0a:00:02:02` (SLIRP's gateway
  MAC).
- The pcap, decoded independently with `tcpdump`: the ARP request going
  *out* (`who-has 10.0.2.2 tell 10.0.2.15`, broadcast from our MAC) and the
  reply coming *in* (`10.0.2.2 is-at 52:55:0a:00:02:02`, unicast to us) -
  TX and RX both confirmed by something other than this kernel's own
  decoding.
- Regression: plain `make run` (no NIC) boots normally, `init_net` silent,
  storage unaffected, zero `-d int` aborts.

**Still coarse, worth knowing before building on this - Stage 1 is
deliberately just the driver:** no protocol stack (the ARP probe is a fixed
hand-built frame, not a real ARP implementation - that's `netd`, Stage 2); no
gated `NET_SEND`/`NET_RECV` syscalls yet (they land with `netd`, their first
consumer, rather than as dead code now); polled, not interrupt-driven (a NIC
is the strongest case yet for real IRQ-driven RX since packets arrive
unsolicited, but polled matches every other driver for the first cut);
QEMU-only (Parallels' virtio-net is PCI, needing a virtio-pci transport -
see `docs/roadmap.md`); small fixed rings (`RX_COUNT` 4, `QUEUE_SIZE` 8),
fine for a polled ARP round-trip, not tuned for throughput; and the
hardcoded QEMU user-net IPs in `init_net`. **(Update: `init_net`'s ARP probe
is gone as of Stage 2 below - the kernel no longer sends anything; it just
installs the NIC into a cell for `netd` to drive.)**

## Network stack, Stage 2: the protocol stack moves to userland (`netd`), and a `ping` command

Stage 2 moves networking out of the EL1 kernel the same way the filesystem
(`fsd`) and console (`cond`) already moved: the kernel keeps only the
DMA-owning virtio-net driver (no IOMMU - a device can DMA anywhere, so the
ring/buffer owner must stay trusted), reached by exactly one task through
gated syscalls, and the whole protocol stack (ARP/IPv4/ICMP) lives in a new
userland server, `netd`. It ends with a real `ping <ip>` you can type at the
shell. Built in two staged commits (2a plumbing, 2b protocol), the standing
discipline.

**A fourth protected task slot, and the renumber it forced.** `netd` is
`syscall_abi::NET_TASK` (slot 4) - the fourth boot-loaded, supervised,
`exit`/`kill`/`wait`-protected server after the shell(0)/idle(1)/fsd(2)/
cond(3). Inserting it shifted the spawnable slots from {4,5} to {5,6}, so
`NUM_TASKS` 6→7, `MAX_EL0_REGIONS` 6→7, `FIRST_SPAWNABLE` 4→5, the
`exit`/`kill`/`wait` protections extend to `<= 4`, `caps_for_slot` gained a
`NET_TASK` arm (`CAP_NET` + logs to cond) and the shell gained `TO_NET`, and
the seven `NUM_TASKS`/`MAX_EL0_REGIONS`-sized static arrays each grew one
element - exactly the shape of the change that inserted `cond` at slot 3. The
riskiest part is that renumber (it touches the task-slot invariant everything
depends on), so it was regression-tested hard: after it, `selftest`, the disk
surface, `exec` (now spawns into slot 5), `ps` (shows netd blocked in slot
4), and a program pipe (spawns into slot 6) all still work, zero aborts.

**Stage 2a - the gated `NET_*` syscalls and a `netd` that drives the NIC from
userland.** `NET_SEND` (42), `NET_RECV` (43), and `NET_MAC` (44) operate on a
kernel-held `NetCell` (the `BlockCell`/`install_block_device` pattern -
`init_net` installs the device instead of probing it) and are gated to
`CAP_NET`, i.e. `netd` alone. Frames are validated with `in_caller_region`,
not `valid_user_range`, since an Ethernet frame exceeds the 512-byte
`MAX_USER_LEN` cap. `netd` (the eighth userland program) proved the whole
EL0→gated-syscall→kernel-driver→wire path in 2a by building a broadcast ARP
request *from userland* and receiving the reply - the same probe `init_net`
used to do in the kernel, now one privilege level up. `loader::load_netd`
(`\EFI\ORBS\NETD.BIN`) registers it with the supervisor for crash/wedge
recovery like the other servers.

**A real subtlety the server loop has to get right: the supervisor's
health-ping.** A supervised server that just `MSG_RECV`s and discards would
never *reply*, so the active health-ping (see "Active health-ping" above)
would time out and restart it every cycle. `netd`'s request loop therefore
replies to *every* message with a status u64 - which, for the ping message
addressed to the kernel's sentinel, is exactly the ack. Confirmed by an 8s
idle run showing zero supervisor events on slot 4.

**Stage 2b - real ARP + IPv4 + ICMP, and the `ping` command.** `netd` gained
a `NETOP_PING` request handler (the `FSOP_*` IPC shape - an op plus a target
IPv4, a one-u64 status reply). Given a target it ARP-resolves the host (send
request, poll for the reply, extract the MAC), builds an ICMP echo request
inside an IPv4 packet inside an Ethernet frame - with correct **IP and ICMP
Internet checksums** (one hand-rolled `ip_checksum` over 16-bit big-endian
words, folded) - sends it, and polls for the matching echo reply (right
source IP, ICMP type 0, our id). All hand-rolled fixed-buffer, no crates, no
heap - the ACPI/FAT32/virtio precedent. Timeouts are deliberately short (ARP
~500ms, ICMP ~1s) so even a worst-case unreachable ping stays under the
supervisor's ~2.5s wedge threshold while `netd` busy-polls (it's `Runnable`,
not `Blocked`, during a ping). The shell's `ping <a.b.c.d>` command parses the
dotted quad **byte-by-byte** (no `str::split`, to stay clear of the
PIE/libcore relocation gotchas documented in `docs/processes.md`),
`MSG_CALL`s `netd`, and maps the `NET_PING_*` status to a message.

**Scope decision, made explicitly with the user: guest-initiated ping only.**
Replying to a *host's* ping needs `netd` to receive frames asynchronously (a
frame arrives unsolicited, with no client call outstanding) - the unified
poll/select gap documented in `research-directions.md`, since a task blocks
on exactly one thing today. Deferred deliberately; guest-initiated ping still
exercises the entire stack (ARP + IPv4 + ICMP).

**Confirmed on QEMU (`make run-image-net`, real FAT32 + the NIC), zero
`-d int` aborts, cross-checked against a `tcpdump` of the `filter-dump`
pcap** (the "verify against a source outside the kernel's own output"
discipline): `ping 10.0.2.2` and `ping 10.0.2.3` → `reply from ...`;
`ping 10.0.2.99` → `unreachable (no ARP reply)`; bad input → usage. The pcap
showed the ARP request/reply and a **valid ICMP echo request/reply** (`id
0x4f42`, checksums validated by `tcpdump` with no "bad cksum") for each
reachable host, and only an unanswered ARP for the unused one. The shell
stayed responsive after (`uptime` advancing), no supervisor restart.

**Still coarse, worth knowing before building on this:** guest-initiated ping
only (no async receive → can't answer a host ping - the poll/select gap);
QEMU-only (Parallels' virtio-net is PCI, still needs a virtio-pci transport);
one ping at a time with a fixed ICMP id/seq (no static state in userland to
count from); the source IP `10.0.2.15` and /24 on-subnet assumption are the
QEMU user-net convention (no DHCP, no routing/gateway resolution for
off-subnet targets - a real target outside 10.0.2.0/24 would need the gateway
ARP'd instead); ~~no UDP/TCP yet (UDP is Stage 3, TCP Stage 4)~~ **(UDP is
done - Stage 3 below; TCP is Stage 4)**; and the NIC driver is still polled,
not IRQ-driven.

## Network stack, Stage 3: UDP, and a `resolve` command (real DNS)

Stage 3 adds **UDP** and, with it, the stack's first real application-layer
protocol: `resolve <hostname>` does a **DNS A-record query over UDP** and
prints the resolved IPv4. It builds entirely on Stage 2's `netd`/`NETOP`
plumbing - **no kernel changes** (the `NET_*` syscalls already move opaque
frames; UDP is just a different payload), which is the payoff of the
driver/protocol split: a whole new transport lands purely in userland.

**The new `netd` operation.** `NETOP_RESOLVE` (the `FSOP_*` request shape:
request = op + the hostname bytes; reply = a status u64 + a packed-IPv4 u64).
Given a hostname `netd` ARP-resolves the QEMU user-net DNS server
(`10.0.2.3`), encodes a DNS query (12-byte header + the hostname as
length-prefixed **QNAME labels**, hand-rolled byte-by-byte), wraps it in a
UDP datagram → IPv4 packet → Ethernet frame (IP header checksum computed;
**UDP checksum 0**, which is optional on IPv4 and which SLIRP accepts - a
deliberate simplification, avoiding the UDP pseudo-header checksum), sends
it, and polls for the response. A hand-rolled DNS parser walks the answer
records - **handling name-compression pointers** (the `0xc0` two-byte
terminal), the one genuinely fiddly part - and returns the first A record.
No answers → `NXDOMAIN`; no response → `TIMEOUT`; distinct statuses the shell
maps to messages. `serve`/`reply` were generalized to carry a byte payload
(resolve's reply is 16 bytes, status + IP, vs ping's 8-byte status).

**The `resolve` command** (`shell/src/main.rs`) packs the hostname into a
`NETOP_RESOLVE` `MSG_CALL` to `netd` and prints `<host> is a.b.c.d` (the
octets via `put_u64_decimal`) or the error. The DNS timeout (~1.5s) plus the
ARP (~500ms) stays under the supervisor's ~2.5s wedge threshold while `netd`
busy-polls (it's `Runnable`, not `Blocked`, during a resolve).

**Confirmed on QEMU (`make run-image-net`), zero `-d int` aborts,
cross-checked against a `tcpdump` of the pcap** - and it resolves **real
hostnames** through SLIRP's DNS proxy (which forwards to the host's
resolver): `resolve example.com` → `example.com is 172.66.147.243` (a genuine
Cloudflare address), `resolve one.one.one.one` → `one.one.one.one is 1.0.0.1`,
`resolve nope.invalidtld` → `could not resolve` (the server returned
`NXDomain`). The pcap decoded our hand-built query cleanly as
`10.0.2.15.32768 > 10.0.2.3.53: … A? example.com.` with a valid UDP/IP
framing, and the multi-A-record responses were parsed correctly (first A
taken). `ping` still works; no supervisor restart.

**Still coarse, worth knowing before building on this:** DNS only queries the
fixed user-net server (`10.0.2.3`) and only reads **A records** (no AAAA/CNAME
chasing beyond a directly-returned A, no `/etc/hosts`, no caching); UDP is
send-one/receive-one with a fixed source port and checksum 0 (no real
socket/port abstraction, no UDP checksum validation on receive); still
guest-initiated only (no listening UDP service - same async-receive gap as
ping); and no TCP yet (Stage 4, the big one - the connection state machine,
retransmission, and windows, where the hand-roll-vs-`smoltcp` decision
actually bites; see `docs/roadmap.md`).

## Commands

```sh
make build                  # cargo build (debug) - kernel only, see below
make build PROFILE=release  # release profile
make shell-bin               # build shell/ for aarch64-unknown-none + strip (same pattern: hello-bin, pong-bin, fsd-bin)
make run                    # stage ESP dir (kernel + userland binaries incl. the fsd filesystem server + config) + boot in QEMU with a virtio-mmio block device attached (fast dev loop - vvfat backing, FAT16, not FAT32 - see "Phase 3b")
make run-virtio-console      # same as `run`, plus a virtio-mmio console device attached (for testing virtio_console.rs - see "virtio-console" above for why this alone doesn't organically trigger the fallback)
make run-net                 # same as `run`, plus a virtio-net device on virtio-mmio + QEMU user-mode (SLIRP) networking + an -object filter-dump pcap (net.pcap) - the dev loop for the network stack (kernel/src/virtio_net.rs, Stage 1); init_net's boot ARP probe exercises SLIRP's gateway, see "Network stack, Stage 1" above
make run-image-net           # `run-image` (real FAT32, disk commands work) *and* the NIC from `run-net` in one boot - the fullest QEMU run: fsd mounts, the shell/disk commands work, and init_net's ARP probe runs, with net.pcap dumped
make run-usb-kbd             # same as `run`, plus an xHCI controller + USB keyboard + HMP monitor socket for sendkey keystroke injection (see "USB HID keyboard driver" above)
make run-usb-multi           # same as `run-usb-kbd`, plus a usb-tablet and a usb-storage stick on the same controller - the three-device rig for xhci.rs's multi-device scan (see "xHCI multi-device support" above)
make image                  # build esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make run-image               # boot esp.img (genuine FAT32) instead of run's vvfat - needed for anything that reads the filesystem at runtime (the fsd server and every disk command)
make parallels-hdd          # wrap esp.img into esp.hdd, a Parallels-native virtual hard disk
make test-parallels          # scripted real-hardware round trip via prlctl - see below
make clean
```

`make run`/`make run-image` require QEMU (`brew install qemu`, which also
provides the aarch64 OVMF firmware they point at). `make image` requires
macOS's `hdiutil`. `make parallels-hdd` additionally requires Parallels
Desktop installed (uses its bundled `prl_disk_tool`). `make shell-bin`
(and therefore `make esp`/`make run`) needs `rustup component add
llvm-tools` for `llvm-objcopy` - see the Makefile's `OBJCOPY` comment for
why it isn't just on `PATH`.

There is no unit test suite — this is pre-alpha kernel code that mostly
proves it boots. There is, as of 2026-08-16, a scripted real-hardware
*smoke* test: `make test-parallels` (`scripts/test-parallels.sh`) rebuilds
`esp.hdd`, boots the registered Parallels VM headlessly via `prlctl`
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
directly, using the real `esp.hdd` normally with a physical keyboard,
not via `make test-parallels`. Removed outright (`xhci.rs::poll_key`)
rather than gated behind a flag - it had already served its purpose
confirming the driver end to end, and wasn't needed for normal
operation. A future `test-parallels` screenshot won't show it anymore;
that's expected, not a regression. One other real caveat about this
testing method, unrelated to the above: `send-key-event` drives Parallels' own synthetic
keyboard device, not the specific physical USB keyboard from that
postmortem - a legitimate stand-in for scripted regression checks, but
not a substitute for real-physical-hardware confirmation of anything
USB-passthrough-specific. See `docs/roadmap.md`'s "Testing infrastructure"
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

```
docs/
  manual.md          the one-stop user manual: prerequisites, building, running on QEMU/Parallels, scripted testing, shell tour, condensed syscall table, writing-a-program essentials - links into the deeper references below
  tutorial.md        build-an-OS-like-this-from-scratch tutorial: the working path only (no bug narratives), staged from UEFI hello world through PIE userland/disk/multitasking, with code samples drawn from this kernel's confirmed-working source
  architecture.md    reference doc: boot flow, privilege model, memory layout, exceptions, process model, syscall ABI, console
  processes.md       reference doc: process loading/config mechanism, memory model, binary format, writing a replacement program
  shell-commands.md  reference doc: the default shell's builtin commands - syntax, behavior, known limitations
  CHANGELOG.md       historical record of completed milestones (phase 0 through the most recent), newest first
  roadmap.md         forward-looking plan: parking lot of known future work, not yet sequenced
  research-minix-boot.md   research note: how MINIX boots (x86 boot monitor + boot image, ARM's U-Boot chain) vs Ouroboros's UEFI-native boot, sourced from MINIX's own docs
  research-helix-os.md     research note: Helix OS's layered, trait-based kernel design and hot-reload/self-healing fault tolerance, sourced from the HelixOS-Org/helix repo and docs
  research-directions.md   research note (synthesis): a comparative deep dive across MINIX/Linux/Plan 9/Helix identifying Plan 9's per-process namespaces + uniform file protocol (9P) as the standout next architecture - the one mechanism that would unify fsd/cond's protocols, the capability send-mask, per-task isolation, and delegation; recalibrates the older influence notes' now-stale "no fault isolation" framing
  xhci-keyboard-postmortem.md   debugging postmortem: the five real-hardware bugs found bringing up USB keyboard input on Parallels, written for other bare-metal-OS developers - see "USB HID keyboard driver" above
  boot-bringup-postmortem.md    debugging postmortem: exception vectors, the MMU switch, GIC/timer, the EL0/syscall boundary, and the console-discovery saga (devicetree/ACPI/PCI/virtio-console dead ends, then the GOP framebuffer console) - the first of four related postmortems
  shell-and-filesystem-postmortem.md   debugging postmortem: the relocation-class bug family (core::fmt, then literal comparisons), the disk-loaded userland shell, virtio-blk/FAT32 bring-up, the cluster-0 hang bug, and write support - the second piece, between boot-bringup-postmortem.md and xhci-keyboard-postmortem.md
  usb-storage-postmortem.md     debugging postmortem: the one-day arc giving real Parallels hardware a disk - proving no PCI storage controller exists there, USB speed routing (2.0->EHCI vs 3.x->xHCI), late passthrough attach, the mid-transfer keyboard-death trap, SCSI Unit Attention, and the day's error-band/toolchain findings - the fourth piece, see "USB mass storage" above
  isolation-and-dataflow-postmortem.md   design retrospective (a fifth piece, a different era from the four hardware-bringup postmortems above): the one-day arc that took isolation from a convention to MMU-enforced - EL0 fault isolation + fsd supervision, pipelines, per-task page tables + FSOP v2, grant/safecopy IPC, the FAT32 offset-write - with the spine "enforcing isolation breaks every cheap data path, so you rebuild them as explicit authorized operations", the enforced-vs-trust decision, the cp-x-x self-destruct trap, the "streaming cp never exercises the RMW branch" coverage insight, and the per-task-ASID works-on-QEMU-faults-on-silicon reversal
  console-server-postmortem.md   design retrospective (a sixth piece, the continuation of the isolation arc): moving the console out of the kernel into a userland server - a driver the kernel itself depends on is a split not a move, the gated-primitives-vs-mapped-memory framebuffer decision, the scheduler lie that routing per-character echo through IPC exposed (a documented-but-unenforced sub-tick-IPC invariant), the missed fsd client that stranded on real hardware, the kernel-console quiet handoff, and verifying pixels by screendump - see "Driver isolation, part 3" above
  capability-and-hardening-postmortem.md   design retrospective (a seventh piece, 2026-08-20): a one-day arc of five milestones - the active health-ping, the capability model (who-may-call-whom), program-to-program pipes, the stack guard page, and the userland heap - with the spine "scope it down before you build it": three of the five shrank when asked whether the clean design needed the hard part (the ping needs no new syscall since a reply is an ack; capabilities are a pure function of slot, no mutable table; pipes need no delegation since the shell can relay), the guard page found a real silent 8KB `exec`-path overflow in the shell on its first test, and a one-build go/no-go gate proved `alloc`'s collections can't be PIE-linked on stable (`R_AARCH64_ABS64` in prebuilt liballoc, `-Z build-std` nightly-only) before any of the heap plumbing was written, forcing the raw-buffer pivot - covers the five "Server supervision"/"capability model"/"pipes"/"guard page"/"heap" sections above

kernel/
  src/main.rs        #[entry] point: UEFI init, console discovery, madt::discover(), loader::load(), ExitBootServices, exceptions::install(), mmu::install_identity_map(), xhci::init(), init_storage()/init_net() (both gated on virtio_mmio_probe_safe), gic+timer init, tasks::init(), then tasks::start()
  src/uart.rs        raw PL011 console driver (read + write), used only after ExitBootServices
  src/uart16550.rs   raw 16550-compatible console driver (read + write; PCI-discovered consoles; different hardware, different driver)
  src/devicetree.rs  console UART discovery via the UEFI-provided devicetree (dead end on QEMU/Parallels)
  src/acpi.rs        console UART discovery via ACPI RSDP -> XSDT -> SPCR (works on QEMU, dead end on Parallels), plus the shared find_table(rsdp, signature) walk madt.rs also uses
  src/madt.rs        real GIC version/address discovery via the ACPI MADT (GICD/GICC/GICR structures) - replaces the old QEMU-devicetree-derived gic.rs addresses, see "MADT/GICv3" above
  src/pci.rs         console UART discovery via PCI enumeration for a class 0x07/0x00 serial controller, plus log_all_devices (a reusable full-bus diagnostic dump that also *returns* the inventory so main.rs can re-print it through the post-exit console - see "Parallels disk diagnostic" above) and discover_xhci (class 0x0c/0x03/0x30, read-only - see "USB HID keyboard driver" above)
  src/console.rs     global console handle (Console enum: Pl011 | Uart16550 | Virtio | Framebuffer), shared between main() and the exception handler
  src/framebuffer.rs GOP (EFI_GRAPHICS_OUTPUT_PROTOCOL) discovery: resolution/stride/pixel format + framebuffer physical base/size - the real lead for Parallels, see "GOP framebuffer console" above
  src/font.rs        embedded public-domain 8x8 bitmap font (dhepper/font8x8), printable ASCII 0x20-0x7E only
  src/fbconsole.rs   text console rendered directly into the GOP framebuffer: glyph drawing, cursor, ptr::copy-based scroll - write-only, no ANSI parsing; now the kernel's *emergency/boot* console only (steady-state userland rendering moved to the cond server - see "Driver isolation, part 3"), see "GOP framebuffer console" above
  src/fbdev.rs       dumb framebuffer primitives for the console server: plot a run of glyph bitmaps (FB_BLIT), scroll (FB_SCROLL), clear (FB_CLEAR), all gated to CON_TASK - the pixel plumbing cond drives while it owns the font/cursor/wrap/scroll/ANSI logic, see "Driver isolation, part 3"
  src/exceptions.rs  AArch64 exception vector table (VBAR_EL1) + fault reporting; SVC path (slot 8/"3:") saves x9 to a scratch stack slot before the EC check clobbers it (see "A real relocating loader" above) and saves/restores SP_EL0 at Context's real offset alongside ELR/SPSR, passing dispatch() the frame pointer itself (see "Blocking primitives" above); slot 8's non-SVC fall-through is the resumable EL0-fault path ("4:"/rust_el0_fault_handler) - kills just the faulting task, and restarts it via supervisor::restart if it was a supervised server (fsd/cond), see "EL0 fault isolation" and "Server supervision + heartbeat" above
  src/mmu.rs         per-task translation-table views (one L0/L1/L2/L3 per scheduler slot; identity-mapped kernel/device shared, EL0 access to each task's own region only - enforced isolation, see "Per-task page tables" above), up to MAX_EL0_REGIONS (5) tasks/views, activate_task switches TTBR0+TLBI per context switch, an optional framebuffer device-block fallback, and (since "Dynamic task creation") a stashed memory map/extra-device list plus rebuild_with_el0_regions so install_identity_map can be called a second time at runtime - see "Dynamic task creation and exec()" above
  src/gic.rs         version-dispatching facade over gicv2.rs/gicv3.rs, selected by madt.rs's real discovery - see "MADT/GICv3" above
  src/gicv2.rs       GICv2 backend (distributor + memory-mapped CPU interface), addresses now passed in from madt::GicInfo instead of hardcoded
  src/gicv3.rs       GICv3 backend (redistributor wake/discovery, ICC_* system-register CPU interface) - confirmed working on QEMU and real Parallels hardware, see "MADT/GICv3" above
  src/timer.rs       ARM generic timer (non-secure EL1 physical timer, PPI 14 / GIC INTID 30), TICK_INTERVAL_MS
  src/loader.rs      reads INIT.CFG + a program off the ESP during boot services (plus the filesystem server \EFI\ORBS\FSD.BIN and console server \EFI\ORBS\COND.BIN, each registered with supervisor::register for crash/wedge recovery), into 2MB-aligned EL0-accessible regions, real ELF64 parsing + R_AARCH64_RELATIVE relocation processing - see "A real relocating loader" above and docs/processes.md; elf_region_size/populate_region split so the same parsing/loading logic is reusable from the runtime spawn path and supervisor restarts too, see "Dynamic task creation and exec()" and "Server supervision + heartbeat" above
  src/supervisor.rs  server supervision registry: keeps each supervised server's boot ELF image and restarts it on a crash (generic fault hook) or a wedge (on_tick's heartbeat), with a per-boot restart cap - the generalized replacement for syscall.rs's old fsd-only restart_fsd/FSD_IMAGE, now covering fsd AND cond, see "Server supervision + heartbeat" above
  src/syscall.rs     svc syscall dispatch table (print/double/report/try_read_char/putc/get_ticks/read_char/spawn/exit/task_state/kill/fg/wait/mount/msg_send/msg_recv/msg_try_recv/block_info/block_read/block_write/msg_call/spawn_stage/grant/safecopy/.../delegate; gaps at 5 and 7-14 - the old fs_* syscalls moved to the fsd server's FSOP_* protocol) plus the block-device cell (the kernel's entire remaining storage role) and in_caller_region validation - grant/safecopy are the enforced bulk-transfer primitive (see "Grant/safecopy IPC"), delegate (41) is runtime capability delegation of the IPC send-mask (see "Runtime capability delegation" above)
  src/tasks.rs       up to 6 task slots (task 0 = loaded program, task 1 = idle, task 2 = filesystem server, task 3 = console server, slots 4-5 spawn/exit-cycled) + the round-robin scheduler over Runnable/Blocked/Unused/Zombie task state (block_current_and_switch, exit_current_and_switch, on_tick's wake-check + the supervisor heartbeat, tasks::spawn, allocate_runtime_region/free_runtime_region, mailboxes with direct delivery + sender-filtered receive, per-task grant slots + safecopy for the bulk-transfer primitive, per-slot capability send-mask + per-task runtime delegation for who-may-call-whom) - confirmed working, see "Blocking primitives", "Dynamic task creation and exec()", "Task destruction", "Driver isolation" parts 1/2/3, "Grant/safecopy IPC", "The capability model", "Runtime capability delegation", and "Server supervision + heartbeat" above
  src/virtio_mmio.rs virtio-mmio transport: 32-slot device discovery, modern (non-legacy) register layout, addresses confirmed via devicetree dump
  src/block.rs       BlockDevice enum (Virtio | UsbMsd) - the block-device abstraction fat32.rs sits on, see "USB mass storage" above
  src/usb_msd.rs     USB mass storage: Bulk-Only Transport + SCSI (INQUIRY/READ CAPACITY/READ(10)/WRITE(10)) over xhci.rs's bulk endpoints - Parallels' first working disk, see "USB mass storage" above
  src/virtio_blk.rs  runtime virtio-blk driver: feature negotiation, one virtqueue, synchronous polling sector reads and writes - phase 3a (reads)/phase 4 (writes), confirmed working, see above
  src/virtio_console.rs  transmit-only virtio-console driver over the same transport - device discovery, feature negotiation, transmitq0 - confirmed working on QEMU; confirmed *not* what Parallels' serial port actually is, see "virtio-console" above
  src/virtio_net.rs  virtio-net driver over the same transport: receiveq (pre-posted buffers) + transmitq, VIRTIO_F_VERSION_1/VIRTIO_NET_F_MAC negotiation, the 12-byte virtio_net_hdr, send_frame/poll_frame moving opaque Ethernet frames - the DMA-owning kernel half of the network stack (Stage 1), QEMU-only (gated like virtio-blk; Parallels' virtio-net is PCI). See "Network stack, Stage 1" above and main.rs's init_net
  src/xhci.rs        from-scratch xHCI (USB3 host controller) driver: command/event rings, multi-device port scan (every connected device enumerated, its interfaces logged, and kept concurrently addressed - up to 4, see "xHCI multi-device support" above), per-device EP0 rings/output contexts, and a real interrupt endpoint for HID keyboard reports - confirmed working end to end with a real keyboard on real Parallels hardware, see "USB HID keyboard driver" above and docs/xhci-keyboard-postmortem.md

shell/               userland default shell - a separate crate, built for aarch64-unknown-none, loaded from disk (not compiled into the kernel)
  linker.ld          self-relocating (PIE) linker script: entry at offset 0, .rela.dyn/.dynsym/.dynamic/.data.rel.ro, no .bss/.data (see "A real relocating loader" above and docs/processes.md)
  src/main.rs        line editor + command dispatch (help/echo/uptime/clear/ls/cat/cd/pwd/mkdir/rmdir/touch/rm/write/writeat/cp/mv/mount/ping/resolve/exec/exit/ps/kill/fg/wait/send/recv/selftest, plus `> file`/`>> file` output redirection and `| /path/program` pipes on any of them - see "Output redirection", "Pipelines", "Runtime capability delegation", and "FAT32 interior/random-access writes" above), running as real EL0 userland code, built for real relocation (relocation-model=pic, --release only) - main loop blocks on read_char (syscall 15) instead of busy-polling, see "Blocking primitives" above; see docs/processes.md

fsd/                 fifth userland program: THE FILESYSTEM SERVER (driver isolation part 2) - owns the FAT32 engine, speaks the FSOP_* protocol to clients over IPC (including the bulk FSOP_READ_BULK/FSOP_WRITE_BULK ops that move file data via grant/safecopy - see "Grant/safecopy IPC" above), and is the only task the BLOCK_* syscalls accept; boot-loaded into protected task slot 2, see "Driver isolation, part 2" above
  src/main.rs        the request loop: recv -> decode FsRequest -> dispatch to Fs -> reply one u64; mounted Fs lives in main's stack frame (no static state in userland)
  src/fat32.rs       the kernel's old hand-rolled FAT32 module, moved essentially verbatim (MBR, BPB, FAT chains, 8.3 entries, full read/write support) plus read_at (windowed offset reads) and write_at (random-access offset writes - interior overwrite via a partial-sector read-modify-write, and past-EOF writes that zero-fill the gap, bounded by MAX_GAP_FILL; behind streaming cp, unbounded >>, and the writeat builtin - see "FAT32 offset-write" and "FAT32 interior/random-access writes" above); its BlockDevice became disk.rs's syscall shim
  src/disk.rs        zero-sized Disk handle: read_sector/write_sector/capacity as BLOCK_READ/BLOCK_WRITE/BLOCK_INFO syscall wrappers

cond/                seventh userland program: THE CONSOLE SERVER (driver isolation part 3) - owns the steady-state console; userland output flows to it as batched DSPOP_WRITE messages over IPC. Two backends (chosen from CON_INFO): byte-stream (forward to the kernel console via the gated CON_WRITE syscall 33 - QEMU's UART) and framebuffer (render glyphs itself via the gated FB_BLIT/FB_SCROLL/FB_CLEAR primitives - the console rendering logic, font included, moved out of the kernel's fbconsole; Parallels, QEMU ramfb). Boot-loaded into protected task slot 3 (CON_TASK), accepted by CON_WRITE/FB_* alone, see "Driver isolation, part 3" above
  src/main.rs        the request loop (recv -> decode DSPOP_WRITE -> render -> reply, the filesystem server's shape) plus two backends chosen from CON_INFO at startup: byte-stream (forward to the kernel console via CON_WRITE) and framebuffer (render glyphs here - cursor, wrap, scroll, a small ANSI parser - via FB_BLIT/FB_SCROLL/FB_CLEAR)
  src/font.rs        cond's own copy of the 8x8 bitmap font (from kernel/src/font.rs) - the font is what "console driver logic" meant, so it moved to userland; the kernel keeps a copy only for its emergency fbconsole

netd/                eighth userland program: THE NETWORK SERVER (network stack, Stage 2) - owns the protocol stack (ARP/IPv4/ICMP) in userland; the kernel keeps only the DMA-owning virtio-net driver, reached by this task alone via the gated NET_SEND/NET_RECV/NET_MAC syscalls (the fsd/BLOCK_* pattern). Boot-loaded into protected task slot 4 (NET_TASK), supervised. Speaks the NETOP_PING request protocol to clients (the shell's `ping` command); hand-rolled fixed-buffer frame building (ARP/IPv4/ICMP/UDP/DNS) + Internet checksums, no crates. NETOP_RESOLVE (Stage 3) does DNS-over-UDP for the shell's `resolve` command. See "Network stack, Stage 2" and "Stage 3" above
  src/main.rs        the request loop (NETOP_PING -> arp_resolve + icmp_echo; NETOP_RESOLVE -> a DNS A-query over UDP) plus the ARP/IPv4/ICMP/UDP/DNS frame builders, the DNS response parser (name-compression aware), the reply matchers, and ip_checksum

upper/               sixth userland program: the pipeline filter demo (uppercases its piped input) - the reference for the filter-program shape: stdin = msg_recv, stdout = con_write (routed through the console server), EOF = the empty message, finish = exit; see "Pipelines" above
  src/main.rs        ~80 lines: the recv/uppercase/putc loop

pong/                fourth userland program: the IPC echo server (recv -> send back to sender; `quit` exits) - the long-lived server shape the fsd filesystem server now actually has, see "Driver isolation, part 1" above
  src/main.rs        ~90 lines: the recv/echo loop over MSG_RECV/MSG_SEND

hello/               second userland program: prints a banner and exits via the EXIT syscall - the living proof of "the shell is just a program" and the reference for how a program ends itself (see "Task destruction" above)
  src/main.rs        ~70 lines: _start, a putc-loop print, the exit call

syscall-abi/         shared syscall ABI crate - syscall numbers, sentinel/error values, and the fsd server's FSOP_* request-protocol constants, depended on directly by the kernel and every userland program - see docs/processes.md
  src/lib.rs         #![no_std], no logic, just pub consts - safe under either target since every value is a scalar inlined at the use site, not a pointer needing relocation

scripts/
  test-parallels.sh  scripted real-hardware smoke test via prlctl (start/type/capture/stop) - invoked by `make test-parallels`, see "## Commands" above and docs/roadmap.md's "Testing infrastructure" section
```

Nine-crate workspace (`kernel`, `shell`, `hello`, `pong`, `fsd`,
`upper`, `cond`, `netd`, `syscall-abi`), with every userland crate deliberately excluded from the
workspace's `default-members` (see `Cargo.toml`) since they need a
different `--target` than `kernel`; `syscall-abi` needs no such
exclusion (a plain lib, no `[[bin]]` to conflict with a target) and gets
built automatically as `kernel`'s path dependency. Expect further growth
as more userland programs emerge, each depending on `syscall-abi` the
same way `shell` does.

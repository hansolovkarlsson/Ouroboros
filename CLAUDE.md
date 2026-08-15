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

### Console discovery: three mechanisms tried, all confirmed dead ends on Parallels — deferred, see below for the real lead

**Status: deferred as of this writing.** All three mechanisms below are
confirmed not to work on Parallels. There's a real, promising lead for what
actually would (virtio-console, at the end of this section) — but
implementing it is a genuinely different, smaller-than-AML subsystem
(virtio device discovery, feature negotiation, a transmit virtqueue), not a
quick follow-up, so it was deliberately not started this session. Kernel
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
transport (virtio-mmio or virtio-pci). Implementing this is the next real
step for Parallels console output, whenever this gets picked back up — see
Next milestone.

`find_dtb`/`find_rsdp` (need the UEFI config table) and each PL011 module's
`discover_pl011` (pure memory parsing, no boot service) run **before**
`exit_boot_services`, even though the parsing halves don't themselves need
boot services — so the result gets logged through the UEFI console (works
on any platform) before any raw MMIO is touched. `pci.rs`'s
`discover_uart16550` has no such split — PCI enumeration is entirely
boot-services-based throughout, so the whole thing runs before exit, no
part of it could run after even if it wanted to. Keep any future discovery
mechanism (virtio-console included) on this same side of the
`exit_boot_services` call for the same reason: whatever address you find,
you want to have already reported it before you risk writing to it.

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
updated together. This project controls the name, so once phase 3c's
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
useful case first" discipline.

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
this kernel can yet create a file large enough to need one. No `cp`, no
output redirection (`>`/`>>`) - both need shell-level plumbing this
syscall alone doesn't provide (parsing `cmd > file`, or reading one
file's content back out to hand to another `write_file` call) - see
`docs/roadmap.md`'s parking lot.

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

**Parallels console output is deliberately paused, not abandoned.** All
three UART discovery mechanisms are confirmed dead ends there (devicetree,
ACPI/SPCR, PCI 16550 — see previous section), and the real lead
(virtio-console) is a genuinely different, smaller-than-AML subsystem that
was a deliberate stopping point for this session rather than a quick
follow-up. When this gets picked back up: implement virtio-console
discovery and a minimal transmit-only driver (virtio-mmio or virtio-pci
transport, feature negotiation, one virtqueue) — see the previous section
for the reasoning behind that lead.

**In the meantime, kernel development continues against QEMU**, which has a
fully working console via ACPI/SPCR. MMU/identity-paging, the timer-driven
preemption tick, the syscall boundary, real preemptive task switching, a
working interactive echo shell, a shell that's a real disk-loaded,
configuration-selected userland program, a working runtime virtio-blk
driver (read and write), a working runtime FAT32 reader, real disk
commands (`ls`/`cat`/`cd`/`pwd`), real filesystem write support for
directories and files (`mkdir`/`rmdir`/`touch`/`rm`), and now a way to
put real content into a file (`write` - see "Phase 6" above, alongside
the earlier `help`/`echo`/`uptime`/`clear` - and
`docs/architecture.md`/`docs/processes.md`/`docs/CHANGELOG.md` for the
reference write-up) are all done and confirmed working, not just
structurally plausible.

**Phase 3, and the entire original "get to a shell" plan, are done - and
phases 4/5/6 (write support) have since gone further than that plan
called for.** Phase 1 (accept input, echo it back), phase 2 (real
commands backed by real kernel state), and phase 3 (disk commands -
`ls`/`cat`/`cd`/`pwd`, and the full runtime storage stack underneath
them: virtio-blk, FAT32, new syscalls) are all confirmed working end to
end - and `mkdir`/`rmdir` (phase 4), `touch`/`rm` (phase 5), then
`write` (phase 6, see above) crossed and then repeatedly extended the
write-support line phase 3 had deliberately drawn (see
`docs/CHANGELOG.md`'s "Phase 3" entry), for the narrowest useful case
each time (empty directories, then zero-byte files, then real content).
There is no more numbered phase queued up - what comes next is an open
choice among the gaps below, not a predetermined next step. This
section (and the "gaps" paragraph below it) still tracks what's true
right now; `docs/roadmap.md` is the one to check for what's next,
`docs/CHANGELOG.md` for the full history of what's already done.

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
renamed - `\EFI\OUROBORO\`, 8 characters, not `\EFI\OUROBOROS\` -
specifically to stay reachable without needing it, but any *other* 9+
character name still isn't), only looks at the first FAT32-typed MBR
partition, and while a file can now hold real content, every write is a
full replace (no append, no partial/offset writes), there's still no
`cp` or output redirection (both need shell-level plumbing `write_file`
alone doesn't provide), no `mv`, no directory-extension (a full parent
directory makes `mkdir` fail rather than growing it), `rm`'s
multi-cluster cluster-chain-freeing path (phase 5) remains untested by
an actual multi-cluster file (nothing yet can create one that large -
see "Phase 6" above), and every write-path error collapses to one
sentinel at the syscall boundary so userland can't distinguish *why* an
operation failed (see "Phase 4"/"Phase 5"/"Phase 6" above); disk-command
pointer/length arguments are trusted, not validated against the caller's
actual mapped region (fine with exactly one, currently-trusted userland
program). Also still true from before this milestone: strict round-robin
only, no priorities or blocking; FP/SIMD state still isn't saved
anywhere (`exceptions.rs`'s `Context`).

Reasonable next steps from here: `cp`/output redirection, now that
`write_file` gives them something real to build on (see
`docs/roadmap.md`'s parking lot for what each still needs); a real
relocating loader (ELF + relocation processing), which would also lift
both the `core::fmt` and literal-comparison restrictions; blocking/waiting
primitives so tasks can do more than an unconditional round-robin `wfe`
loop; or finally circling back to Parallels virtio-console now that the
kernel has enough infrastructure (MMU, exceptions, EL0, real task
switching, real input, disk-loaded userland, real read/write runtime
storage) to make that work meaningfully once it lands.

## Commands

```sh
make build                  # cargo build (debug) - kernel only, see below
make build PROFILE=release  # release profile
make shell-bin               # build shell/ for aarch64-unknown-none + objcopy to a raw .bin
make run                    # stage ESP dir (kernel + shell binary + config) + boot in QEMU with a virtio-mmio block device attached (fast dev loop - vvfat backing, FAT16, not FAT32 - see "Phase 3b")
make image                  # build esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make run-image               # boot esp.img (genuine FAT32) instead of run's vvfat - needed for anything that reads the filesystem at runtime (fat32.rs and up)
make parallels-hdd          # wrap esp.img into esp.hdd, a Parallels-native virtual hard disk
make clean
```

`make run`/`make run-image` require QEMU (`brew install qemu`, which also
provides the aarch64 OVMF firmware they point at). `make image` requires
macOS's `hdiutil`. `make parallels-hdd` additionally requires Parallels
Desktop installed (uses its bundled `prl_disk_tool`). `make shell-bin`
(and therefore `make esp`/`make run`) needs `rustup component add
llvm-tools` for `llvm-objcopy` - see the Makefile's `OBJCOPY` comment for
why it isn't just on `PATH`.

There is no test suite yet — this is pre-alpha kernel code that only proves
it boots.

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
  architecture.md    reference doc: boot flow, privilege model, memory layout, exceptions, process model, syscall ABI, console
  processes.md       reference doc: process loading/config mechanism, memory model, binary format, writing a replacement program
  shell-commands.md  reference doc: the default shell's builtin commands - syntax, behavior, known limitations
  CHANGELOG.md       historical record of completed milestones (phase 0 through the most recent), newest first
  roadmap.md         forward-looking plan: parking lot of known future work, not yet sequenced
  research-minix-boot.md   research note: how MINIX boots (x86 boot monitor + boot image, ARM's U-Boot chain) vs Ouroboros's UEFI-native boot, sourced from MINIX's own docs
  research-helix-os.md     research note: Helix OS's layered, trait-based kernel design and hot-reload/self-healing fault tolerance, sourced from the HelixOS-Org/helix repo and docs

kernel/
  src/main.rs        #[entry] point: UEFI init, console discovery, loader::load(), ExitBootServices, exceptions::install(), mmu::install_identity_map(), gic+timer init, tasks::init(), then tasks::start()
  src/uart.rs        raw PL011 console driver (read + write), used only after ExitBootServices
  src/uart16550.rs   raw 16550-compatible console driver (read + write; PCI-discovered consoles; different hardware, different driver)
  src/devicetree.rs  console UART discovery via the UEFI-provided devicetree (dead end on QEMU/Parallels)
  src/acpi.rs        console UART discovery via ACPI RSDP -> XSDT -> SPCR (works on QEMU, dead end on Parallels)
  src/pci.rs         console UART discovery via PCI enumeration for a class 0x07/0x00 serial controller
  src/console.rs     global console handle (Console enum: Pl011 | Uart16550), shared between main() and the exception handler
  src/exceptions.rs  AArch64 exception vector table (VBAR_EL1) + fault reporting
  src/mmu.rs         replaces firmware's translation tables with our own identity map (L0->L1->L2->L3 as needed), up to MAX_EL0_REGIONS independent EL0 regions
  src/gic.rs         GICv2 driver (QEMU virt addresses, confirmed via devicetree dump)
  src/timer.rs       ARM generic timer (non-secure EL1 physical timer, PPI 14 / GIC INTID 30), TICK_INTERVAL_MS
  src/loader.rs      reads INIT.CFG + a program off the ESP during boot services, into a 2MB-aligned EL0-accessible region - see docs/processes.md
  src/syscall.rs     svc syscall dispatch table (print/double/report/try_read_char/putc/get_ticks/fs_list_dir/fs_read_file/fs_mkdir/fs_rmdir/fs_touch/fs_rm/fs_write_file, gap at 5) - confirmed working end to end, see above
  src/tasks.rs       task 0 (loaded program) + task 1 (idle) + the round-robin scheduler (Context save/restore in the tick path) - confirmed alternating, see above
  src/virtio_mmio.rs virtio-mmio transport: 32-slot device discovery, modern (non-legacy) register layout, addresses confirmed via devicetree dump
  src/virtio_blk.rs  runtime virtio-blk driver: feature negotiation, one virtqueue, synchronous polling sector reads and writes - phase 3a (reads)/phase 4 (writes), confirmed working, see above
  src/fat32.rs       hand-rolled FAT32 over virtio_blk::Device: MBR, BPB, FAT cluster chains, 8.3-only directory entries, mkdir/rmdir/touch/rm/write_file write support - phase 3b (reads)/phase 4-6 (writes), confirmed working, see above

shell/               userland default shell - a separate crate, built for aarch64-unknown-none, loaded from disk (not compiled into the kernel)
  linker.ld          flat-binary linker script: entry at offset 0, no .bss/.data (see docs/processes.md)
  src/main.rs        line editor + command dispatch (help/echo/uptime/clear/ls/cat/cd/pwd/mkdir/rmdir/touch/rm/write), running as real EL0 userland code - see docs/processes.md

syscall-abi/         shared syscall ABI crate - syscall numbers + sentinel values (NO_CHAR/FS_ERROR/NO_FS), depended on directly by both kernel and shell, no more hand-duplicated numbers - see docs/processes.md
  src/lib.rs         #![no_std], no logic, just pub consts - safe under either target since every value is a scalar inlined at the use site, not a pointer needing relocation
```

Three-crate workspace (`kernel`, `shell`, `syscall-abi`), with `shell`
deliberately excluded from the workspace's `default-members` (see
`Cargo.toml`) since it needs a different `--target` than `kernel`;
`syscall-abi` needs no such exclusion (a plain lib, no `[[bin]]` to
conflict with a target) and gets built automatically as `kernel`'s path
dependency. Expect further growth as more userland programs emerge, each
depending on `syscall-abi` the same way `shell` does.

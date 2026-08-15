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
working interactive echo shell, and now a shell that's a real disk-loaded,
configuration-selected userland program (see "The shell becomes a real
disk-loaded process" above, and `docs/architecture.md`/`docs/processes.md`
for the reference write-up) are all done and confirmed working, not just
structurally plausible.

**Phase 1 of "get to a shell" (a terminal that accepts input and echoes it
back) is done, and so is the disk-loading milestone that followed it before
phase 2 started.** Phase 2, per the original plan, is commands: replace
the loaded shell's (`shell/src/main.rs`'s) "echo the completed line" step
with real tokenizing and a dispatch table of builtins. Natural first
builtins: something that reads kernel state already tracked somewhere
(`exceptions.rs`'s `TICKS`, `tasks.rs`'s `CURRENT`) so a command's output
means something real rather than being another demo — but note these
would need a *new* syscall to expose that state to userland, since the
shell can no longer just read kernel statics directly now that it's a
separate program.

What's still coarse and worth knowing about before building on any of
this — kept current in `docs/processes.md`'s "known rough edges" rather
than duplicated here, since that's the document meant to track it as these
get addressed: no shared syscall-ABI crate (numbers duplicated by hand
between `syscall.rs` and `shell/`); exactly one program, loaded once, at
boot, with no `exec()`; a fixed 2-task scheduler (no dynamic task
creation); no heap or `.bss` for userland programs, so no static mutable
state at all; a fixed, unguarded 8KB stack per program; no ELF, no
relocations, no dynamic linking. Also still true from before this
milestone: strict round-robin only, no priorities or blocking; FP/SIMD
state still isn't saved anywhere (`exceptions.rs`'s `Context`).

Reasonable next steps from here: phase 2 (command parsing/dispatch,
above, plus whatever new syscalls it needs); a shared syscall-ABI crate,
now that there are two independent call sites to keep in sync by hand;
blocking/waiting primitives so tasks can do more than an unconditional
round-robin `wfe` loop; a real runtime storage stack (virtio-blk + a
filesystem), needed for `exec()`-style dynamic loading and for Parallels
to ever run programs it can't compile in ahead of time; or finally
circling back to Parallels virtio-console now that the kernel has enough
infrastructure (MMU, exceptions, EL0, real task switching, real input,
disk-loaded userland) to make that work meaningfully once it lands.

## Commands

```sh
make build                  # cargo build (debug) - kernel only, see below
make build PROFILE=release  # release profile
make shell-bin               # build shell/ for aarch64-unknown-none + objcopy to a raw .bin
make run                    # stage ESP dir (kernel + shell binary + config) + boot in QEMU
make image                  # build esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make parallels-hdd          # wrap esp.img into esp.hdd, a Parallels-native virtual hard disk
make clean
```

`make run` requires QEMU (`brew install qemu`, which also provides the
aarch64 OVMF firmware `make run` points at). `make image` requires macOS's
`hdiutil`. `make parallels-hdd` additionally requires Parallels Desktop
installed (uses its bundled `prl_disk_tool`). `make shell-bin` (and
therefore `make esp`/`make run`) needs `rustup component add llvm-tools`
for `llvm-objcopy` - see the Makefile's `OBJCOPY` comment for why it isn't
just on `PATH`.

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
  src/syscall.rs     svc syscall dispatch table (print/double/report/try_read_char/putc, gap at 5) - confirmed working end to end, see above
  src/tasks.rs       task 0 (loaded program) + task 1 (idle) + the round-robin scheduler (Context save/restore in the tick path) - confirmed alternating, see above

shell/               userland default shell - a separate crate, built for aarch64-unknown-none, loaded from disk (not compiled into the kernel)
  linker.ld          flat-binary linker script: entry at offset 0, no .bss/.data (see docs/processes.md)
  src/main.rs        the interactive line editor, now running as real EL0 userland code - see docs/processes.md
```

Two-crate workspace (`kernel` and `shell`), with `shell` deliberately
excluded from the workspace's `default-members` (see `Cargo.toml`) since
it needs a different `--target` than `kernel`. Expect further growth as
shared code (a syscall ABI crate - see `docs/processes.md`'s "known rough
edges") and any future userland programs emerge.

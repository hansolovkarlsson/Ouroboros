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

## Where the history and design rationale live

This file used to also carry a milestone-by-milestone narrative of
everything built so far. That history now lives under `docs/`, so this
file can stay focused on durable, load-bearing guidance:

- **`docs/CHANGELOG.md`** — the full milestone record, phase 0 to the
  present, newest first. Every completed step (the shell, disk/FAT32
  support, USB keyboard + storage, the userland servers, the network
  stack, …) is recorded there in condensed form. Check it for *what was
  built, and why it works the way it does*.
- **`docs/roadmap.md`** — the forward-looking parking lot of known future
  work, not yet sequenced.
- **The postmortems under `docs/`** (ten of them — boot bring-up; shell
  & filesystem; xHCI keyboard; USB storage; isolation & dataflow; console
  server; capability & hardening; network stack; userland maturation
  (/bin, pipelines, VFS/GPT); the filesystems arc (exFAT + ext2 read/write,
  and the real-hardware pass that found the xHCI keyboard↔storage bug)) are
  the design and bug retrospectives. Read the relevant one for *the traps
  already hit and the lessons learned* before reworking a subsystem.
- **`docs/journal.md`** — a chronological dev-log (narrative "what and why
  each day"), a lighter companion to the milestone-oriented `CHANGELOG.md`.
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
make run-usb-kbd             # same as `run`, plus an xHCI controller + USB keyboard + HMP monitor socket for sendkey keystroke injection (see "USB HID keyboard driver" above)
make run-usb-multi           # same as `run-usb-kbd`, plus a usb-tablet and a usb-storage stick on the same controller - the three-device rig for xhci.rs's multi-device scan (see "xHCI multi-device support" above)
make image                  # build build/esp.img, a raw MBR+FAT32 disk image (not directly usable by Parallels - see below)
make run-image               # boot build/esp.img (genuine FAT32) instead of run's vvfat - needed for anything that reads the filesystem at runtime (the fsd server and every disk command)
make run-image-gpt           # build build/espgpt.img (build/esp.img's FAT32 wrapped in a bootable GPT disk via scripts/mkgpt.py) and boot it - exercises fsd's GPT partition discovery (the disk has no real MBR table)
make run-image-exfat         # build build/espexfat.img (two-partition MBR: exFAT partition 1 + FAT32 ESP partition 2, via newfs_exfat + scripts/mkexfat.py) and boot it - fsd mounts the exFAT partition (FAT32 probe fails, exFAT probe succeeds), UEFI boots the FAT32 ESP; exercises fsd/src/exfat.rs (the exFAT read-write arm)
make run-image-ext2          # build build/espext2.img (two-partition MBR: ext2 partition 1 + FAT32 ESP partition 2, via e2fsprogs' mke2fs + scripts/mkext2.py) and boot it - fsd mounts the ext2 partition (FAT32 + exFAT probes fail, ext2 succeeds), UEFI boots the FAT32 ESP; exercises fsd/src/ext2.rs (the ext2 read-write arm). Needs `brew install e2fsprogs`
make parallels-hdd          # wrap build/esp.img into build/esp.hdd, a Parallels-native virtual hard disk
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

Some annotations below carry cross-references worded “see … above” — those
pointed at milestone sections that have moved to `docs/CHANGELOG.md` (and the
postmortems under `docs/`); look there for the full write-up.

```
docs/
  manual.md          the one-stop user manual: prerequisites, building, running on QEMU/Parallels, scripted testing, shell tour, condensed syscall table, writing-a-program essentials - links into the deeper references below
  tutorial.md        build-an-OS-like-this-from-scratch tutorial: the working path only (no bug narratives), staged from UEFI hello world through PIE userland/disk/multitasking, with code samples drawn from this kernel's confirmed-working source
  architecture.md    reference doc: boot flow, privilege model, memory layout, exceptions, process model, syscall ABI, console
  processes.md       reference doc: process loading/config mechanism, memory model, binary format, writing a replacement program
  shell-commands.md  reference doc: the default shell's builtin commands - syntax, behavior, known limitations
  testing-exfat.md   how-to: build the two-partition exFAT test disk (make run-image-exfat), drive the read-write surface from the shell, and verify against macOS's own driver (fsck_exfat + a byte-identical cmp) - the QEMU rig behind fsd/src/exfat.rs
  CHANGELOG.md       historical record of completed milestones (phase 0 through the most recent), newest first
  journal.md         chronological dev-log: narrative "what was worked on and why" per day - a lighter companion to CHANGELOG.md's milestone record
  roadmap.md         forward-looking plan: parking lot of known future work, not yet sequenced
  roadmap-cluster.md the big long-term direction: a Plan 9-style resource-sharing cluster (distributed Ouroboros) - phased plan from today's local servers to a two-node disk-sharing cluster over 9P-over-TCP and beyond, with the shared-memory "single system image" mirage explicitly out of scope. The vision the project is ultimately aiming at
  research-minix-boot.md   research note: how MINIX boots (x86 boot monitor + boot image, ARM's U-Boot chain) vs Ouroboros's UEFI-native boot, sourced from MINIX's own docs
  research-helix-os.md     research note: Helix OS's layered, trait-based kernel design and hot-reload/self-healing fault tolerance, sourced from the HelixOS-Org/helix repo and docs
  research-directions.md   research note (synthesis): a comparative deep dive across MINIX/Linux/Plan 9/Helix identifying Plan 9's per-process namespaces + uniform file protocol (9P) as the standout next architecture - the one mechanism that would unify fsd/cond's protocols, the capability send-mask, per-task isolation, and delegation; recalibrates the older influence notes' now-stale "no fault isolation" framing
  xhci-keyboard-postmortem.md   debugging postmortem: the five real-hardware bugs found bringing up USB keyboard input on Parallels, written for other bare-metal-OS developers - see "USB HID keyboard driver" above
  boot-bringup-postmortem.md    debugging postmortem: exception vectors, the MMU switch, GIC/timer, the EL0/syscall boundary, and the console-discovery saga (devicetree/ACPI/PCI/virtio-console dead ends, then the GOP framebuffer console) - the first of four related postmortems
  shell-and-filesystem-postmortem.md   debugging postmortem: the relocation-class bug family (core::fmt, then literal comparisons), the disk-loaded userland shell, virtio-blk/FAT32 bring-up, the cluster-0 hang bug, and write support - the second piece, between boot-bringup-postmortem.md and xhci-keyboard-postmortem.md
  usb-storage-postmortem.md     debugging postmortem: the one-day arc giving real Parallels hardware a disk - proving no PCI storage controller exists there, USB speed routing (2.0->EHCI vs 3.x->xHCI), late passthrough attach, the mid-transfer keyboard-death trap, SCSI Unit Attention, and the day's error-band/toolchain findings - the fourth piece, see "USB mass storage" above
  isolation-and-dataflow-postmortem.md   design retrospective (a fifth piece, a different era from the four hardware-bringup postmortems above): the one-day arc that took isolation from a convention to MMU-enforced - EL0 fault isolation + fsd supervision, pipelines, per-task page tables + FSOP v2, grant/safecopy IPC, the FAT32 offset-write - with the spine "enforcing isolation breaks every cheap data path, so you rebuild them as explicit authorized operations", the enforced-vs-trust decision, the cp-x-x self-destruct trap, the "streaming cp never exercises the RMW branch" coverage insight, and the per-task-ASID works-on-QEMU-faults-on-silicon reversal
  console-server-postmortem.md   design retrospective (a sixth piece, the continuation of the isolation arc): moving the console out of the kernel into a userland server - a driver the kernel itself depends on is a split not a move, the gated-primitives-vs-mapped-memory framebuffer decision, the scheduler lie that routing per-character echo through IPC exposed (a documented-but-unenforced sub-tick-IPC invariant), the missed fsd client that stranded on real hardware, the kernel-console quiet handoff, and verifying pixels by screendump - see "Driver isolation, part 3" above
  capability-and-hardening-postmortem.md   design retrospective (a seventh piece, 2026-08-20): a one-day arc of five milestones - the active health-ping, the capability model (who-may-call-whom), program-to-program pipes, the stack guard page, and the userland heap - with the spine "scope it down before you build it": three of the five shrank when asked whether the clean design needed the hard part (the ping needs no new syscall since a reply is an ack; capabilities are a pure function of slot, no mutable table; pipes need no delegation since the shell can relay), the guard page found a real silent 8KB `exec`-path overflow in the shell on its first test, and a one-build go/no-go gate proved `alloc`'s collections can't be PIE-linked on stable (`R_AARCH64_ABS64` in prebuilt liballoc, `-Z build-std` nightly-only) before any of the heap plumbing was written, forcing the raw-buffer pivot - covers the five "Server supervision"/"capability model"/"pipes"/"guard page"/"heap" sections above (plus a next-day addendum: runtime capability delegation, the deferred pipes consumer, turned out self-securing)
  network-stack-postmortem.md   design retrospective (an eighth piece, 2026-08-21): the one-day arc that built the whole network stack - from a virtio-net driver to a concurrent HTTP server (ping/resolve/fetch + a static-file server) - with these threads: the no-IOMMU driver/protocol split (DMA owner stays in the kernel, the protocol stack becomes the netd userland server), the async-receive primitive a server needs (NET_WAIT blocking on frames-or-messages, then + a timeout for the RTO), testing loss recovery on a lossless wire (SLIRP can't drop, so inject a drop in the sender + disable fast retransmit to isolate the RTO), two real bugs found only in a packet trace (a go-back-N snd_una>snd_nxt unsigned-wrap stall; the supervisor restarting netd mid-transfer when a burst ran too long), the guard page catching netd's stack overflow twice more (16->24->32KB), and verifying against tcpdump/curl rather than the kernel's own log - covers all the "Network stack, Stage 1..4j" sections above
  userland-and-pipelines-postmortem.md   design retrospective (a ninth piece, 2026-08-22): the day the shell's compiled-in builtins became a real /bin of standalone programs with multi-stage pipelines, then the filesystem arc began - threaded on "where does a command belong": the ps/kill/wait revert (job control is inherently a builtin - a spawned command runs in a reused slot and lists/races itself), a capability u32 bit-collision caught before shipping (send-mask reaching CAP_CON's bit at NUM_TASKS=10), delegation needing less than feared (netd via a targeted DELEGATE; a linear pipe needs only one-target-per-task delegation, so general delegation still has no consumer), the pipeline /-prefix heuristic replaced wholesale rather than special-cased again, and building a bootable GPT disk with valid CRCs (scripts/mkgpt.py) just to test the GPT parser - the recurring discipline "scope it down; let testing find the boundaries," three of the day's decisions being subtractions
  filesystems-arc-postmortem.md   design retrospective (a tenth piece, 2026-08-23): the day exFAT and ext2 joined fsd - each read-only then read-write, all behind the unchanged FSOP_* protocol - capped by the real-hardware Parallels pass. Lessons: an abstraction is only tested by a genuinely *different* implementation (exFAT was FAT-shaped; ext2's inode model is what proved the Filesystem enum); validate against a foreign checker not your own reader (macOS's exFAT driver + fsck_exfat + e2fsck + debugfs); a field's meaning can depend on state (a freed inode's small i_dtime was misread by e2fsck as an orphan-list next-pointer - "corrupted orphan linked list"); read-first/write-later + staged writes keep the surface debuggable; and the emulator hides an entire class of bug (the xHCI keyboard<->mass-storage contention the real-hardware pass found, invisible on QEMU where keyboard=synthetic + storage=virtio-blk never share a bus)

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
  src/exceptions.rs  AArch64 exception vector table (VBAR_EL1) + fault reporting; SVC path (slot 8/"3:") saves x9 to a scratch stack slot before the EC check clobbers it (see "A real relocating loader" above) and saves/restores SP_EL0 at Context's real offset alongside ELR/SPSR, passing dispatch() the frame pointer itself (see "Blocking primitives" above); slot 8's non-SVC fall-through is the resumable EL0-fault path ("4:"/rust_el0_fault_handler) - kills just the faulting task, and restarts it via supervisor::restart if it was a supervised server (fsd/cond), see "EL0 fault isolation" and "Server supervision + heartbeat" above; rust_irq_handler routes the timer PPI to tasks::on_tick and the NIC receive SPI to tasks::on_net_irq (IRQ-driven RX, see docs/CHANGELOG.md's "Network stack, Stage 4k")
  src/mmu.rs         per-task translation-table views (one L0/L1/L2/L3 per scheduler slot; identity-mapped kernel/device shared, EL0 access to each task's own region only - enforced isolation, see "Per-task page tables" above), up to MAX_EL0_REGIONS (10, kept equal to tasks::NUM_TASKS; the table pools are [const {..}; N] so they auto-scale) tasks/views, activate_task switches TTBR0+TLBI per context switch, an optional framebuffer device-block fallback, and (since "Dynamic task creation") a stashed memory map/extra-device list plus rebuild_with_el0_regions so install_identity_map can be called a second time at runtime - see "Dynamic task creation and exec()" above
  src/gic.rs         version-dispatching facade over gicv2.rs/gicv3.rs, selected by madt.rs's real discovery - see "MADT/GICv3" above
  src/gicv2.rs       GICv2 backend (distributor + memory-mapped CPU interface), addresses now passed in from madt::GicInfo instead of hardcoded; enable_interrupt routes an SPI (>=32, e.g. the NIC) to CPU 0 via GICD_ITARGETSR, not just PPIs
  src/gicv3.rs       GICv3 backend (redistributor wake/discovery, ICC_* system-register CPU interface; enable_interrupt handles SPIs via the distributor - GICD_ISENABLER/IROUTER/IGROUPR - as well as PPIs via the redistributor) - confirmed working on QEMU and real Parallels hardware, see "MADT/GICv3" above
  src/timer.rs       ARM generic timer (non-secure EL1 physical timer, PPI 14 / GIC INTID 30), TICK_INTERVAL_MS
  src/loader.rs      reads INIT.CFG + a program off the ESP during boot services (plus the filesystem server \EFI\ORBS\FSD.BIN and console server \EFI\ORBS\COND.BIN, each registered with supervisor::register for crash/wedge recovery), into 2MB-aligned EL0-accessible regions, real ELF64 parsing + R_AARCH64_RELATIVE relocation processing - see "A real relocating loader" above and docs/processes.md; elf_region_size/populate_region split so the same parsing/loading logic is reusable from the runtime spawn path and supervisor restarts too, see "Dynamic task creation and exec()" and "Server supervision + heartbeat" above
  src/supervisor.rs  server supervision registry: keeps each supervised server's boot ELF image and restarts it on a crash (generic fault hook) or a wedge (on_tick's heartbeat), with a per-boot restart cap - the generalized replacement for syscall.rs's old fsd-only restart_fsd/FSD_IMAGE, now covering fsd AND cond, see "Server supervision + heartbeat" above
  src/syscall.rs     svc syscall dispatch table (print/double/report/try_read_char/putc/get_ticks/read_char/spawn/exit/task_state/kill/fg/wait/mount/msg_send/msg_recv/msg_try_recv/block_info/block_read/block_write/msg_call/spawn_stage/grant/safecopy/.../delegate, and the argv syscalls args_stage/get_argc/get_arg (spawned programs receive arguments - see "Standalone binaries, Stage 1"); gaps at 5 and 7-14 - the old fs_* syscalls moved to the fsd server's FSOP_* protocol) plus the block-device cell (the kernel's entire remaining storage role) and in_caller_region validation - grant/safecopy are the enforced bulk-transfer primitive (see "Grant/safecopy IPC"), delegate (41) is runtime capability delegation of the IPC send-mask (see "Runtime capability delegation" above)
  src/tasks.rs       NUM_TASKS (10) task slots (task 0 = loaded program, task 1 = idle, task 2 = filesystem server, task 3 = console server, task 4 = network server, slots 5-9 spawn/exit-cycled - FIRST_SPAWNABLE=5, five spawnable; per-task arrays are [const {..}; NUM_TASKS] so they auto-scale, and the caps u32 keeps resource caps at bits 16+ clear of the low-NUM_TASKS-bit send-mask) + the round-robin scheduler over Runnable/Blocked/Unused/Zombie task state (block_current_and_switch, exit_current_and_switch, on_tick's wake-check + the supervisor heartbeat, tasks::spawn, allocate_runtime_region/free_runtime_region, mailboxes with direct delivery + sender-filtered receive, WaitReason::NetInput (with an optional deadline, so NET_WAIT can time out - the network server's RTO timer) + the async-receive wake for the network server, per-task grant slots + safecopy for the bulk-transfer primitive, per-slot capability send-mask + per-task runtime delegation for who-may-call-whom, per-task argv store set at spawn/cleared on death for the argv ABI) - confirmed working, see "Blocking primitives", "Dynamic task creation and exec()", "Task destruction", "Driver isolation" parts 1/2/3, "Grant/safecopy IPC", "The capability model", "Runtime capability delegation", and "Server supervision + heartbeat" above
  src/virtio_mmio.rs virtio-mmio transport: 32-slot device discovery, modern (non-legacy) register layout, addresses confirmed via devicetree dump
  src/block.rs       BlockDevice enum (Virtio | UsbMsd) - the block-device abstraction fat32.rs sits on, see "USB mass storage" above
  src/usb_msd.rs     USB mass storage: Bulk-Only Transport + SCSI (INQUIRY/READ CAPACITY/READ(10)/WRITE(10)) over xhci.rs's bulk endpoints - Parallels' first working disk, see "USB mass storage" above. Now with BOT error recovery (bot_command retries with xhci::storage_reset_endpoint between attempts) - the real-Parallels "Mode A" fix where keyboard/storage contention stalled a bulk endpoint that then stayed halted forever; see docs/CHANGELOG.md's "xHCI keyboard <-> USB-storage contention"
  src/virtio_blk.rs  runtime virtio-blk driver: feature negotiation, one virtqueue, synchronous polling sector reads and writes - phase 3a (reads)/phase 4 (writes), confirmed working, see above
  src/virtio_console.rs  transmit-only virtio-console driver over the same transport - device discovery, feature negotiation, transmitq0 - confirmed working on QEMU; confirmed *not* what Parallels' serial port actually is, see "virtio-console" above
  src/virtio_net.rs  virtio-net driver over the same transport: receiveq (pre-posted buffers) + transmitq, VIRTIO_F_VERSION_1/VIRTIO_NET_F_MAC negotiation, the 12-byte virtio_net_hdr, send_frame/poll_frame moving opaque Ethernet frames, plus IRQ-driven receive (has_frame/intid/ack_interrupt: the RX queue's GIC SPI wakes netd via exceptions.rs + tasks::on_net_irq - Stage 4k; TX interrupts suppressed, transmit still polls) - the DMA-owning kernel half of the network stack, QEMU-only (gated like virtio-blk; Parallels' virtio-net is PCI). See docs/CHANGELOG.md's "Network stack" entries and main.rs's init_net
  src/xhci.rs        from-scratch xHCI (USB3 host controller) driver: command/event rings, multi-device port scan (every connected device enumerated, its interfaces logged, and kept concurrently addressed - up to 4, see "xHCI multi-device support" above), per-device EP0 rings/output contexts, and a real interrupt endpoint for HID keyboard reports - confirmed working end to end with a real keyboard on real Parallels hardware, see "USB HID keyboard driver" above and docs/xhci-keyboard-postmortem.md. The boot port scan is minimum-settle + debounce (SCAN_MIN_SETTLE_MS/SCAN_DEBOUNCE_MS/SCAN_SETTLE_CAP_MS), NOT "break on first connected device" - the real-Parallels "Mode B" fix where a fast SuperSpeed stick won the enumeration race and the slower synthetic keyboard was missed for the whole boot; also storage_reset_endpoint (Reset Endpoint + Set TR Dequeue) for usb_msd's BOT recovery ("Mode A"). See docs/CHANGELOG.md's "xHCI keyboard <-> USB-storage contention"

programs/            ALL userland programs live here (aarch64-unknown-none, loaded from disk, not compiled into the kernel), grouped by role under category dirs; kernel + the shared libs (ulib, syscall-abi) stay at the repo root. Crate paths below are shown in full (programs/<category>/<crate>/). See the "Twenty-nine-crate workspace" note after this block for the move rationale.
  linker.ld          the shared self-relocating (PIE) linker script EVERY program uses: entry at offset 0, .rela.dyn/.dynsym/.dynamic/.data.rel.ro, no .bss/.data. Referenced by .cargo/config.toml as -Tprograms/linker.ld (path relative to the workspace root, not any one crate); see "A real relocating loader" above and docs/processes.md

programs/shell/      userland default shell - loaded from disk (not compiled into the kernel)
  src/main.rs        line editor + command dispatch (help/cd/pwd/write/mount/unmount/erase/partition/exec/exit/ps/kill/fg/wait/send/recv/selftest/env/set/unset builtins - bare `mount` now *lists* what's mounted via FSOP_MOUNT_INFO (format + partition LBA + capacity), `mount -a` performs the old mount action, `unmount` drops it via FSOP_UNMOUNT (milestone 1); `erase disk` (FSOP_ERASE) and `partition [fat32|exfat|ext2]` (FSOP_PARTITION, MBR) prepare a blank disk (milestone 2), `format [fat32|exfat|ext2]` (FSOP_FORMAT, mkfs - FAT32 + exFAT + ext2, all three) lays a filesystem into the partition (milestone 3, complete) - these three MUST be builtins not /bin programs (they run when nothing is mounted, exactly when /bin can't be read to load a program); the disk-management arc, see docs/roadmap.md - the whole filesystem command surface (echo/uptime/clear/ls/cat/mkdir/rmdir/touch/rm/cp/mv/writeat) AND the network commands (ping/resolve/fetch) are externalized to /bin now, Stage 4; the netd ones reach the network server via TO_NET the shell delegates at spawn (delegate_net in run_path_command), plus `> file`/`>> file` output redirection and multi-stage `a | b | c` pipelines (split_pipeline/cmd_pipeline: N program stages spawned right-to-left, each producer->consumer link a DELEGATE, argv + PATH per stage, first stage may be a builtin captured; see docs/roadmap.md's multi-stage-pipeline arc); an unknown command is then looked up as a program on PATH via run_path_command - /bin by bare name, foreground/reaped - see "Standalone binaries, Stage 2"; plus env/set/unset and $VAR expansion over a stack-local env store, with PATH a real variable driving the lookup - see "Standalone binaries, Stage 3"), running as real EL0 userland code, built for real relocation (relocation-model=pic, --release only) - main loop blocks on read_char (syscall 15) instead of busy-polling, see "Blocking primitives" above; see docs/processes.md

programs/servers/fsd/    fifth userland program: THE FILESYSTEM SERVER (driver isolation part 2) - owns the FAT32 engine, speaks the FSOP_* protocol to clients over IPC (including the bulk FSOP_READ_BULK/FSOP_WRITE_BULK ops that move file data via grant/safecopy - see "Grant/safecopy IPC" above), and is the only task the BLOCK_* syscalls accept; boot-loaded into protected task slot 2, see "Driver isolation, part 2" above
  src/main.rs        the request loop: recv -> decode FsRequest -> dispatch to the mounted vfs::Filesystem -> reply one u64; it lives in main's stack frame (no static state in userland). Also the disk-management ops handled before the mounted-fs guard: FSOP_MOUNT_INFO/FSOP_UNMOUNT (milestone 1); the raw-disk FSOP_ERASE (erase_disk, zeroes leading sectors) / FSOP_PARTITION (partition_disk, writes a single-partition MBR) (milestone 2); and FSOP_FORMAT (format_disk -> find_partition reads the MBR, then fat32::Fs::format / exfat::Fs::format / ext2::Fs::format - FAT32 + exFAT + ext2 mkfs, all three) (milestone 3, complete), all refusing while mounted - the server is now a "storage server" (erase/partition/format all three filesystems from within the guest), see docs/roadmap.md's disk-management arc
  src/vfs.rs         the internal filesystem-multiplexing layer (the VFS refactor): a Filesystem enum (Fat32 | ExFat | Ext2) whose per-op methods forward to the arm, plus mount() which discovers partitions (partition.rs) and probes each FAT32-then-exFAT-then-ext2 (first that validates wins), name() for the mount log, and partition_lba() for FSOP_MOUNT_INFO (each Fs now stores its volume's first sector - the disk-management arc's milestone 1). An enum not dyn Trait (no_std, no heap - the block::BlockDevice/console::Console pattern); a further format is a new arm + a probe/branch in mount. Clients never see it - the FSOP_* protocol is already FS-agnostic, which ext2 (a genuinely different inode model) proved by needing zero changes above fsd
  src/exfat.rs       exFAT read-write (the more-filesystems arc, steps 2-3; the first real use of the Filesystem enum's second arm). READ: mount_at parses the log2-shift boot sector, walk_dir reassembles directory *entry sets* (0x85 File + 0xC0 Stream-Ext + 0xC1 File-Name, reporting each set's primary slot + length so writes can locate it) into a DirEntry, advance() handles contiguous (NoFatChain, skips the FAT) vs FAT-chained clusters, UTF-16 names rendered ASCII; up-case table (0x82) approximated by ASCII case-fold. WRITE: free clusters via the allocation bitmap (alloc_cluster/bitmap_set, located at mount from root's 0x81); created files/dirs are FAT-chained (NoFatChain=0), so allocation parallels fat32's write_chain; create_entry/build_entry_set compute both checksums (whole-set SetChecksum + up-cased NameHash), delete_set clears in-use bits; full surface (touch/write_file/write_at/mkdir/rm/rmdir/mv). Read machinery mirrors fat32.rs; validated against macOS's driver + fsck_exfat. See "More filesystems, steps 2-3" in docs/CHANGELOG.md. FORMAT (mkfs, disk-management milestone 3): Fs::format writes the main+backup boot regions (VBR + extended boot signatures + boot checksum), a FAT, and a cluster heap of allocation bitmap + minimal compressed up-case table + root dir; fsck_exfat-clean (needs a real attached device, not a plain file)
  src/ext2.rs        ext2 read-write (the more-filesystems arc, steps 4-5; the real test of the Filesystem abstraction - a genuinely different inode model through the unchanged FSOP_*). READ: inodes own metadata (a dir entry is just name->inode number); read_inode/resolve locate an inode via block group descriptors; block_for maps a logical block to physical via 12 direct + single/double indirect pointers (triple=EOF, a 0 pointer=sparse hole->zeros); find/walk_dir resolve paths case-SENSITIVELY (Unix). WRITE: bitmap-based block+inode allocation (alloc_block/alloc_inode) keeping free counts consistent in BOTH the group descriptor and superblock (+bg_used_dirs_count); ensure_block allocates direct/indirect pointers; write_inode/insert_dirent (slack-split)/remove_dirent; mkdir/rmdir maintain link counts + a cross-dir mv fixes ".."; a freed inode gets links 0 + a real i_dtime (a small value is misread by e2fsck as an orphan-list pointer). Full surface (touch/write_file/write_at/mkdir/rm/rmdir/mv); validated with e2fsck + debugfs. Symlinks reported not followed; root is inode 2. See "More filesystems, steps 4-5" in docs/CHANGELOG.md. FORMAT (mkfs, disk-management milestone 3): Fs::format writes a deliberately minimal but e2fsck-clean single-block-group, 4KiB-block (first_data_block=0), 128-byte-inode, filetype-feature ext2 - superblock + the one block-group descriptor + block/inode bitmaps (used + nonexistent padding bits) + a zeroed inode table with root (inode 2) and lost+found (inode 11) + their dir blocks; no backup SB (single group), sparse_super/resize/large_file off; capped at one group (128MiB). One reused 4KiB scratch buffer, not eight (eight overflowed fsd's stack guard page); e2fsck -fn clean + debugfs reads back a guest-written file. See "More filesystems, steps 4-5" and "Disk management tools, milestone 3 (step 3)" in docs/CHANGELOG.md
  src/partition.rs   partition discovery (GPT + multi-partition milestone): discover() enumerates a disk's partition start LBAs - GPT (detected by the "EFI PART" signature at LBA 1, then the header + entry array) or classic MBR - bounded/fixed-buffer, no alloc. Above any one filesystem, so a GPT/MBR disk is discovered the same way regardless of the FS in each partition (see scripts/mkgpt.py + `make run-image-gpt` for GPT testing)
  src/fat32.rs       the kernel's old hand-rolled FAT32 module, moved essentially verbatim (BPB, FAT chains, 8.3 entries, full read/write support); mounts via mount_at(disk, partition_lba) now (partition discovery moved up to partition.rs/vfs.rs - the GPT milestone) plus long-filename (LFN) *read* support (walk_dir_with_location reconstructs a long name from the checksum-validated LFN entries preceding a short entry, so long names read/list/match - see "FAT32 long filename (LFN) read support" above; writing long names still isn't supported) plus read_at (windowed offset reads) and write_at (random-access offset writes - interior overwrite via a partial-sector read-modify-write, and past-EOF writes that zero-fill the gap, bounded by MAX_GAP_FILL; behind streaming cp, unbounded >>, and the writeat builtin - see "FAT32 offset-write" and "FAT32 interior/random-access writes" above); plus Fs::format (mkfs - the inverse of mount_at: boot sector + FSInfo + backups, both FATs' reserved entries, zeroed root; Microsoft fatgen103 layout formulas; disk-management milestone 3, validated by macOS fsck_msdos); plus a `read_cursor` on `Fs` (the v0.4.1 "large-read fsd restart" fix): read_at's seek used to re-walk the cluster chain from the file's start every call (O(n²) over a chunked read, and one late-offset request issuing enough uncached FAT reads to run past the supervisor's runnable-wedge and get fsd restarted mid-read), so a forward/sequential read now resumes from the last position; chain *position* only (never data), invalidated at write_fat_entry so it can't follow a stale chain - see CHANGELOG's "Large-read fsd restart fixed"; its BlockDevice became disk.rs's syscall shim
  src/disk.rs        zero-sized Disk handle: read_sector/write_sector/capacity as BLOCK_READ/BLOCK_WRITE/BLOCK_INFO syscall wrappers

programs/servers/cond/   seventh userland program: THE CONSOLE SERVER (driver isolation part 3) - owns the steady-state console; userland output flows to it as batched NP_WRITE_FILE messages over IPC (the uniform ninep-abi verb set - cond ignores the tree/path and renders the inline data; DSPOP_* retired in cluster Phase 0 step 0e). Two backends (chosen from CON_INFO): byte-stream (forward to the kernel console via the gated CON_WRITE syscall 33 - QEMU's UART) and framebuffer (render glyphs itself via the gated FB_BLIT/FB_SCROLL/FB_CLEAR primitives - the console rendering logic, font included, moved out of the kernel's fbconsole; Parallels, QEMU ramfb). Boot-loaded into protected task slot 3 (CON_TASK), accepted by CON_WRITE/FB_* alone, see "Driver isolation, part 3" above
  src/main.rs        the request loop (recv -> decode NP_WRITE_FILE -> render -> reply, the filesystem server's shape) plus two backends chosen from CON_INFO at startup: byte-stream (forward to the kernel console via CON_WRITE) and framebuffer (render glyphs here - cursor, wrap, scroll, a small ANSI parser - via FB_BLIT/FB_SCROLL/FB_CLEAR)
  src/font.rs        cond's own copy of the 8x8 bitmap font (from kernel/src/font.rs) - the font is what "console driver logic" meant, so it moved to userland; the kernel keeps a copy only for its emergency fbconsole

programs/servers/netd/   eighth userland program: THE NETWORK SERVER (network stack, Stage 2) - owns the protocol stack (ARP/IPv4/ICMP) in userland; the kernel keeps only the DMA-owning virtio-net driver, reached by this task alone via the gated NET_SEND/NET_RECV/NET_MAC syscalls (the fsd/BLOCK_* pattern). Boot-loaded into protected task slot 4 (NET_TASK), supervised. Speaks the NETOP_PING request protocol to clients (the shell's `ping` command); hand-rolled fixed-buffer frame building (ARP/IPv4/ICMP/UDP/DNS) + Internet checksums, no crates. NETOP_RESOLVE (Stage 3) does DNS-over-UDP for `resolve`; NETOP_FETCH (Stage 4a) does a client-TCP HTTP GET for `fetch`. Stage 4b makes it event-driven (blocks in NET_WAIT, drains client messages *and* incoming frames each wake) and adds a TCP HTTP server on port 80 + an ARP responder - the guest answers the network now, not just initiates. Stage 4c makes that HTTP server a real static-file server: it parses the request path and streams the file from fsd over TCP (netd is fsd's first non-shell client, reached via a new netd->fsd capability). Stage 4d adds TCP send-side flow control (window tracking + ACK-paced streaming) so a file of *any* size streams, not just what fits one window. Stage 4e adds proper HTTP response headers (Content-Type by extension + Content-Length via an fsd stat). Stage 4f makes a GET of a directory return a browsable HTML index (links resolved against the request path) - the guest's filesystem is browsable in a browser. Stage 4g adds HTTP HEAD (identical headers to GET, no body, for every response kind). Stage 4h adds TCP fast retransmit (three dup-ACKs -> go-back-N resend), the server's first loss recovery. Stage 4i adds a timer-based RTO (via a new NET_WAIT timeout) for when the peer goes silent. Stage 4j multiplexes up to MAX_CONNS (4) concurrent connections (handle_tcp routes segments to a [Option<TcpConn>; N] by peer). Stage 4l makes the RTO adaptive: an RFC 6298 estimator (rtt_update) measures round-trip time via the MONOTONIC_US clock syscall and derives per-connection SRTT/RTTVAR/RTO, replacing the fixed base (Karn's algorithm gates sampling to new, non-retransmitted data). Stage 4m returns a 405 Method Not Allowed (with an Allow header) for any method other than GET/HEAD. Stage 4n adds TCP congestion control (Reno): a per-connection cwnd/ssthresh grows on ACKs (slow-start then congestion avoidance) and shrinks on loss (fast retransmit halves, RTO resets to one segment), so the send window is min(cwnd, peer window). Stage 4o adds sender-side SACK (RFC 2018): the SYN-ACK advertises SACK-permitted, parse_tcp_in extracts SACK blocks, and sack_retransmit resends only the hole on a fast retransmit (send_seg_at/retransmit_one), falling back to go-back-N when the peer sends no SACK (which SLIRP never does - see the CHANGELOG). (Stage 4k, IRQ-driven RX, is kernel-side - see virtio_net.rs/gicv2.rs/gicv3.rs.) See docs/CHANGELOG.md's "Network stack" entries
  src/main.rs        the NET_WAIT event loop (drain client requests -> NETOP_PING/RESOLVE/FETCH; drain frames -> ARP replies + the TCP server state machine) plus start_response (request-path parsing + a landing page or a file from fsd) and pump_send (streaming bounded by min(cwnd, peer window) - Reno congestion control plus the peer's flow-control window - ACK-paced, one segment per call with a per-segment mailbox drain so the supervisor health-ping is always acked), read_file_chunk's grant/safecopy FSOP_READ_BULK, the ARP/IPv4/ICMP/UDP/DNS/TCP frame builders (build_tcp_generic shared by client build_tcp + server build_tcp_srv), the DNS/TCP parsers, next-hop routing, ip_checksum and the TCP pseudo-header checksum, plus loss recovery: fast retransmit (dup-ACKs -> rewind_to), a timer-based, RTT-estimated RTO (service_rto over a NET_WAIT timeout; rtt_update computes RFC 6298 SRTT/RTTVAR from MONOTONIC_US samples), and sender-side SACK (parse_tcp_in extracts SACK blocks, sack_retransmit resends only the hole via send_seg_at/retransmit_one, go-back-N fallback when the peer sends none)

programs/textutils/  the pipeline filter commands - upper/ wc/ grep/ head (/bin/UPPER, /bin/GREP, /bin/WC, /bin/HEAD) - chainable ulib filters: stdin = ulib::pipe_recv, stdout = write_out(stdout_target) so each can be a middle stage, EOF in = the empty message, EOF out = end_of_stream, finish = exit. upper uppercases; grep <pattern> keeps matching lines (substring, no regex); wc counts lines/words/bytes; head [N] takes the first N lines (default 10) then exits early. upper is the reference shape - see the multi-stage-pipeline arc in docs/roadmap.md
  src/main.rs        ~80 lines: the recv/uppercase/putc loop

programs/demos/pong/     fourth userland program: the IPC echo server (recv -> send back to sender; `quit` exits) - the long-lived server shape the fsd filesystem server now actually has, see "Driver isolation, part 1" above
  src/main.rs        ~90 lines: the recv/echo loop over MSG_RECV/MSG_SEND

programs/demos/hello/    second userland program: prints a banner and exits via the EXIT syscall - the living proof of "the shell is just a program" and the reference for how a program ends itself (see "Task destruction" above)
programs/shellutils/args/  ninth userland program: prints the argument vector it was spawned with (GET_ARGC/GET_ARG) - the proof of the argv ABI (see "Standalone binaries, Stage 1" - the first step of the /bin/PATH/environment arc)
ulib/                shared userland support library (a lib crate, not a program): syscall wrappers, argv reading (GET_ARGC/GET_ARG), cwd (GET_CWD) + path resolution (resolve/concat_path/normalize_path), the fs client layer (fs_call/fs_list_dir/fs_read_file/fs_read_bulk/fs_write_bulk/fs_write_at/fs_op_path/fs_mv/is_fs_error/fs_error), the netd client (net_call, with a bounded MSG_ERR_DENIED retry for the delegation race), parse_u64, output routing (con_write/pipe_out/write_out), decimal formatting, exit, and the one #[panic_handler] - the foundation the externalized /bin commands share (see "Standalone binaries, Stage 4")
programs/fileutils/ (ls cat mkdir rmdir touch rm cp mv writeat), programs/netutils/ (ping resolve fetch), programs/shellutils/ (echo uptime clear + args above)  the externalized commands (Stage 4): former shell builtins, now real /bin programs (/bin/ECHO, /bin/LS, /bin/PING, ...) over ulib, found by PATH. echo/uptime/clear need neither fsd nor cwd; the filesystem commands (ls/cat + mkdir/rmdir/touch/rm + cp/mv/writeat) resolve paths against the cwd the shell delivers at spawn (CWD_STAGE/GET_CWD) and talk to fsd via ulib's fs helpers; the network commands (ping/resolve/fetch) reach netd via ulib::net_call, using the TO_NET capability the shell delegates to them at spawn (a spawnable slot doesn't hold it statically). The whole fs+net command surface lives here now; only write/cd/pwd/exec/exit and job control (ps/kill/wait/fg) + mount/selftest/help stay builtin
  src/main.rs        ~70 lines: _start, a putc-loop print, the exit call

syscall-abi/         shared syscall ABI crate - syscall numbers, sentinel/error values, and the fsd server's FSOP_* request-protocol constants, depended on directly by the kernel and every userland program - see docs/processes.md
  src/lib.rs         #![no_std], no logic, just pub consts - safe under either target since every value is a scalar inlined at the use site, not a pointer needing relocation

scripts/
  test-parallels.sh  scripted real-hardware smoke test via prlctl (start/type/capture/stop) - invoked by `make test-parallels`, see "## Commands" above and docs/roadmap.md's "Testing infrastructure" section
  mkgpt.py           builds build/espgpt.img: build/esp.img's FAT32 partition wrapped in a bootable GPT disk (protective MBR + primary/backup GPT headers with valid CRC32s + an ESP entry) - for testing fsd's GPT partition discovery, since macOS has no GPT tooling. Invoked by `make image-gpt`/`run-image-gpt`
  mkexfat.py         builds build/espexfat.img: a two-partition MBR disk, exFAT partition first (fsd mounts it) + the FAT32 ESP second (UEFI boots it) - so fsd mounts exFAT while the disk still boots. The exFAT payload (build/exfatpart.img) is built by the Makefile via newfs_exfat (hdiutil can't make exFAT). For testing fsd/src/exfat.rs. Invoked by `make image-exfat`/`run-image-exfat`
  mkext2.py          builds build/espext2.img: the same two-partition trick with an ext2 partition first (fsd mounts it) + the FAT32 ESP second (UEFI boots it). The ext2 payload (build/ext2part.img) is built by the Makefile via e2fsprogs' mke2fs -d (macOS has no native ext2 tooling; `brew install e2fsprogs`). For testing fsd/src/ext2.rs. Invoked by `make image-ext2`/`run-image-ext2`
```

Twenty-nine-crate workspace. `kernel` and the two shared libs (`ulib`,
`syscall-abi`) sit at the repo root; **every userland program lives under
`programs/`, grouped by role** (`programs/shell`, `programs/servers/{fsd,
cond,netd}`, `programs/demos/{hello,pong}`, `programs/fileutils/*`,
`programs/textutils/*`, `programs/netutils/*`, `programs/shellutils/*`) - a
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

# From "it doesn't boot" to a real console: an ARM64 kernel bring-up postmortem

This is a write-up of the earlier stretch of work getting
[Ouroboros](../README.md), a from-scratch ARM64 microkernel, from "UEFI
hands us control" to a genuinely stable, interactive foundation:
exception handling that survives a real fault, an MMU running on the
kernel's own page tables instead of firmware's, a timer-driven
preemption tick, a real EL0/syscall boundary, and - the part that took
the most real-hardware round trips - an actual console on real
Parallels-on-Apple-Silicon hardware, after three separate discovery
mechanisms and one whole driver turned out to be dead ends.

Like [the xHCI keyboard postmortem](xhci-keyboard-postmortem.md), this
is kept separate from the project's own internal history
(`CLAUDE.md`/`CHANGELOG.md`) because most of what's here isn't really
Ouroboros-specific. If you're bringing up your own bare-metal ARM64
kernel and something works fine under QEMU but not on real hardware, or
your exception vector table faults the instant you jump to it, or your
MMU switch hangs forever right after you flip `TTBR0_EL1` - there's a
real chance one of these is your bug too.

## The recurring shape of these bugs

Almost everything below falls into one of two buckets:

- **"It compiles and it should work" wasn't good enough.** Several of
  these were verified correct by hand, cross-checked against Linux's own
  kernel headers, and *still* didn't work - because the actual bug was
  one level below where the reasoning was happening (a linker section
  attribute, a compiler's alignment ceiling, a hypervisor's device
  emulation policy).
- **QEMU is a permissive host; real hardware is not.** A hardcoded
  address that's merely wrong on QEMU quietly does nothing. The same
  wrong address on real Parallels hardware takes an entire VM down. More
  than one fix below exists specifically because "confirmed on QEMU"
  turned out not to mean "confirmed."

## Exception vectors: a linker section name that silently wasn't executable

**Symptom:** a minimal AArch64 exception vector table, installed into
`VBAR_EL1` right after leaving UEFI boot services, faulted the instant
any exception tried to reach it - including the deliberate test fault
used to verify it worked at all.

**What made this hard:** everything *looked* right. The table linked
without error. `VBAR_EL1` read back exactly the address it was set to.
The vector table's own assembly was correct. And yet:

```
ESR_EL1 decoded: EC 0x21 (Instruction Abort, same EL)
                 IFSC 0x0F (Permission Fault, level 3)
```

The page the vector table lived on existed and was mapped - just not
executable, which is a permissions fault a page-not-present bug would
never produce, and is very easy to misread as "my MMU setup is wrong"
rather than "my linker script is wrong."

**Root cause:** the vector table's assembly had been placed in a custom
section, `.section .text.exceptions`, on the reasonable assumption that
a section starting with `.text` would inherit the usual
executable-section treatment. It doesn't, at least not reliably on this
project's PE/COFF-targeting toolchain (`aarch64-unknown-uefi`): the
backend infers executable-section characteristics from *exact*,
recognized section names, not prefixes. An unrecognized custom name
gets emitted as a plain data section - readable, present at the right
address, and non-executable.

**Fix:** use the literal `.text` section name directly in the
`global_asm!` block, not a custom one.

**How this was actually verified, not just "believed fixed":** rather
than trust a clean boot, the fix was checked by deliberately forcing a
fault at a known-unmapped address and cross-referencing the emulator's
own internal interrupt trace (`qemu -d int`) against the kernel's own
fault report. Before the fix, the trace showed the *correct* initial
dispatch into the vector table, immediately followed by an endless
identical fault at the vector table's own address - a fault loop, since
jumping into the (non-executable) handler immediately re-faults. That's
also, not coincidentally, exactly the failure shape that had separately
been observed taking down real hardware entirely (see the "no fallback
address" section below) - a genuinely useful pattern to recognize:
an infinite tight fault loop, rather than a single reported fault, often
means the *handler itself* is unreachable, not that the original fault
was unusual.

> **Lesson:** when a linker/section-attribute question is in play,
> don't trust "it links without error." Verify the actual runtime
> property you need (executable, in this case) by deliberately
> exercising it, not by inspecting the successful build output.

## The MMU switch: matching firmware's own translation-table starting level

**Symptom:** replacing UEFI firmware's translation tables with the
kernel's own - same technique either way, just swapping which tables
`TTBR0_EL1` points at while the MMU stays continuously enabled - hard-
faulted immediately after the switch: a Permission fault at translation
level 2 on the very next instruction, then an identical fault on the
exception vector table itself, looping forever.

**What made this hard:** the first version used a single L1 table with
`T0SZ=25` (39-bit input address space) - architecturally legal, and
every table entry, every `MAIR_EL1`/`TCR_EL1` bit was hand-verified
correct against Linux's own `pgtable-hwdef.h`, independently
re-derived from the real runtime register values with a throwaway
Python decode. PXN/UXN weren't it either - removing both entirely
changed nothing. All the reasoning checked out; the kernel still
faulted the instant it ran under its own tables.

**Root cause, confirmed by direct A/B test with everything else held
constant:** firmware had configured its own tables with `T0SZ=20`
(44-bit input address space), which requires an extra top-level (L0)
table before L1. Switching to `TTBR0_EL1` pointing at a differently-
*structured* table hierarchy - a single L1 table instead of an L0→L1
chain - while the MMU stayed continuously enabled (no
disable/re-enable step in between) appears not to be tolerated, at
least on this CPU model, even when every individual attribute bit is
correct. Matching firmware's starting level - not simplifying to a
smaller, "sufficient" hierarchy - was the actual, sole fix; the rest of
the reasoning had been right all along.

> **Lesson:** a live `TTBR0_EL1` switch, with the MMU never actually
> disabled, may be far less forgiving about *structural* changes
> (starting level, table depth) than about attribute changes, even
> though both are "just a table walk" on paper. If you're replacing
> firmware's tables in place rather than disabling the MMU first, read
> back and match firmware's own starting configuration before assuming
> a cleaner, smaller table structure will work.

## An honestly unresolved mystery: EL0 couldn't share a page with running EL1 code

Not every bug in this arc got a clean root cause - and that's worth
recording too, since routing around an unexplained failure is sometimes
the right call, not a failure of diagnosis.

**The goal:** let EL0 (user-mode) code execute out of the same RAM
block the kernel's own EL1 code was actively running from, just with
the page permissions widened to allow EL0 access too
(`AP[2:1] = 01`, EL1+EL0 read/write).

**What happened:** an immediate, identical-looking hard fault - a
Permission fault on the very first instruction fetch after the table
switch, in exactly the same shape as the MMU bug above.

**What was ruled out, each by a direct, repeatable test, not
assumption:** the AP bit's position (cross-checked three separate ways,
including a from-scratch Python re-derivation of the actual runtime
descriptor value); `UXN` (removing it entirely changed nothing); `PAN`/
`EPAN` (this CPU is architecturally too old to implement it at all -
confirmed directly by attempting the raw system-register access and
getting an "unknown instruction" trap, not a permission effect);
shareability attributes and an explicit `ic ialluis`; and page
granularity (rebuilding the same region as 2MB blocks instead of a
single 1GB block changed nothing). A read-only control case
(`AP[2:1] = 10`, EL1-only, no EL0 execute) correctly produced a *data*
abort instead of an *instruction* abort - solid evidence the AP bits
were being interpreted correctly in general; only this one specific
combination (granting EL0 execute *and* changing AP away from the
narrowest value) faulted, which architecturally shouldn't be possible,
since AP doesn't gate execute permission at all - only PXN/UXN do.

**Resolution, not explanation:** rather than keep chasing it, EL0 code
was moved into a genuinely *separate*, dedicated memory region -
isolated from any actively-executing EL1 code - with its own page-table
entries. That worked immediately and has been reliable ever since. The
underlying "why" was never found.

> **Lesson:** when a change that should be architecturally legal keeps
> producing an architecturally-shouldn't-be-possible fault, and you've
> ruled out every documented cause you can find, changing the *shape*
> of what you're doing (isolate instead of widen) can be the right call
> even without a root cause - and it's worth writing the mystery down
> as a mystery rather than papering over it with a guessed explanation.

## GIC and the timer tick: confirmed addresses, not textbook ones

Bringing up a periodic preemption tick (GICv2 distributor/CPU interface
plus the ARM generic timer) needed real hardware addresses for the
interrupt controller. Rather than use commonly-quoted "QEMU virt"
addresses from memory or documentation, the actual values were pinned
down by dumping the specific QEMU build's own internal devicetree
(`qemu-system-aarch64 -machine virt,dumpdtb=...`, which QEMU always
constructs internally regardless of whether it exposes one to the
guest) and decoding it directly. This is the same discipline that later
turned out to matter enormously for the console-discovery work below -
an address "everyone knows" for one specific emulator build is still a
guess, not a fact, the moment you're not on that exact build or that
exact hardware.

The interrupt path itself needed a real architectural change, not just
new addresses: every exception vector before this one only ever needed
to *report and halt* - diverge, never return. A periodic tick has to
*resume* whatever it interrupted, which meant this vector needed its own
full register-save/restore trampoline (general-purpose registers plus
`ELR_EL1`/`SPSR_EL1`) and a normal function call into the handler instead
of a diverging jump. Verified as genuinely sustained, not just "printed
once": a 20+ second run producing correctly-spaced ticks with no drift
or corruption, cross-checked against the emulator's own internal
exception trace showing zero aborts across the entire run.

## A compiler/toolchain ceiling, found by bisection: 8KB, not 2MB

Giving EL0 code its own isolated memory region (see the mystery above)
meant that region needed to be a compile-time-aligned static - and the
original plan was a comfortable, round 2MB alignment. That triggered a
genuine compiler crash on this specific target
(`aarch64-unknown-uefi`, a PE/COFF binary format). Rather than accept
"2MB doesn't work" as the final answer, the actual limit was found by
bisection: PE/COFF section alignment tops out at exactly 8KB on this
toolchain - not a round number, not something documented anywhere
obvious, just the real ceiling. The isolated EL0 region was sized to
8KB to fit under it.

(This ceiling later disappeared on its own, as a genuine side effect of
an unrelated design change - once user programs were loaded from disk
at runtime rather than compiled into the kernel image, they no longer
needed a compile-time-aligned static at all. Worth noting as a small
example of how a real, hard-won constraint can become moot for reasons
that have nothing to do with solving it directly.)

## A quieter EL0 bug: `wfe` traps back to EL1 by default

Once EL0 code could actually run, its idle loop (`wfe` - wait for
event, used between polls) immediately trapped back into EL1 instead of
actually idling. Root cause: `SCTLR_EL1`'s `nTWE`/`nTWI` bits, which by
default make `wfe`/`wfi` at a lower exception level trap to EL1 rather
than execute directly - a control completely unrelated to page tables
or the memory-isolation work happening at the same time, easy to miss
if you assume every EL0 fault must be a permissions issue. Diagnosed
directly from the trapped exception's own cause code, and fixed by
clearing those bits before ever dropping to EL0.

## Console discovery: three real mechanisms, three confirmed dead ends, in order

With the kernel itself stable, the next real problem was output: how do
you find a serial console on hardware you don't control the
description of? Three standard discovery mechanisms were tried, each
logging exactly why it failed before the next was attempted - genuinely
useful discipline, since "no console found" alone tells you nothing
about which of several possible causes is real.

1. **Devicetree** (`EFI_DTB_TABLE_GUID` in the UEFI configuration
   table). Confirmed dead on *both* the emulator and real hardware -
   both report no devicetree blob at all, because both platforms
   describe their hardware via ACPI instead.
2. **ACPI RSDP → XSDT → SPCR** (a hand-rolled parser - not a full ACPI
   library, since this only needs a handful of fixed-offset struct
   reads). Confirmed *working* on the emulator, first try - real
   discovery, not a hardcoded guess, resolving the same UART address a
   hardcoded fallback used to assume. Confirmed *dead* on real hardware,
   and importantly, not from a parsing bug: the RSDP and XSDT both
   parsed successfully (so ACPI itself is present and well-formed there
   too), there is simply no SPCR table entry among the platform's own
   ACPI tables at all - tested with and without a serial port device
   explicitly configured in the VM's own settings, with no difference
   either way. The platform genuinely doesn't describe its console this
   way, regardless of configuration.
3. **PCI enumeration** for a class 0x07/0x00 ("Serial controller")
   device - the 8250/16450/16550 UART family, a completely different
   piece of hardware from what the other two mechanisms look for.
   Confirmed dead on both platforms too - no such PCI device exists on
   either.

**A real, hard-earned lesson living inside this discipline:** there used
to be a hardcoded fallback address, used whenever discovery failed, on
the theory that a wrong guess is better than no console at all. It was
removed after direct confirmation that it hard-crashes real hardware -
nothing is mapped at that specific emulator-shaped address on the real
platform's virtual chipset, so the write faults, and with no exception
vectors installed yet at the point this ran, that fault had nowhere to
go and took the entire VM down. After exception vectors existed, the
same category of mistake would at least have been *survivable* - but
the real fix was structural: **no confirmed address means no console
output, full stop, never a guess.** All three discovery failures above
being reported cleanly, without crashing anything, is itself a genuine
confirmation that this discipline was working as intended.

## virtio-console: a real, working driver - confirmed not to be the answer here

Research suggested Parallels' Apple Silicon virtualization exposes its
devices (network, storage, entropy, and serial) via virtio, the same
convention QEMU uses - and that this class of hypervisor typically
expects a guest to use `console=hvc0` rather than a classic UART. A
full transmit-path virtio-console driver was built against this lead:
device discovery over the virtio-mmio transport, feature negotiation,
one virtqueue, confirmed working end-to-end on the emulator (every
kernel boot message reaching a real chardev on the host side, confirmed
byte-for-byte, not just "looked right").

**Confirmed, directly, not to be what real hardware uses.** A
permanent PCI-bus diagnostic (enumerate every device, log its class and
vendor/device ID) was built specifically to answer this on real
hardware, and it found real virtio devices present - but only for
networking (device ID `0x1000`), never anything matching a
virtio-console device ID over either transport this project can drive.
The one remaining unexplained device on the bus carries the platform
vendor's own PCI vendor ID with an unclassified device class - almost
certainly the real serial mechanism, and almost certainly proprietary
and undocumented. Reverse-engineering an undocumented device was
considered and explicitly declined as open-ended with no guaranteed
payoff - a real, deliberate scope decision, not an oversight. The
virtio-console driver itself stays in the tree, genuinely useful on any
platform that *does* expose console this way; it's simply confirmed,
with real evidence, not to be this one.

## The GOP framebuffer console: the real answer, and four more real hardware bugs

With every byte-stream mechanism confirmed dead, the fallback that
actually worked was a standard, fully-specified UEFI protocol
(`EFI_GRAPHICS_OUTPUT_PROTOCOL`) needing no address guessing or
platform convention at all - just query the current display mode and
write text directly into the framebuffer. Getting this working on real
hardware took five real-hardware test rounds and found four more real,
independently-confirmed bugs.

### Bug: `open_protocol_exclusive` silently disconnects firmware's own console

**Symptom:** on the first real-hardware test, boot progressed normally
right up through PCI enumeration - and then simply stopped. No crash,
no further output of any kind, not even the framebuffer discovery
module's own unconditional success/failure log line that should have
printed next.

**Root cause:** the framebuffer protocol was being opened in
*exclusive* mode - which, per the UEFI protocol's own documented
semantics, forcibly disconnects any other driver currently holding it.
Firmware's own boot-time text console was still holding that exact
protocol to render the very screen the test was being watched on - so
the instant this code ran, it silently disconnected the visible console
from the display, before a single further log line could print. An
emulator-based test of the same code never caught this, because that
test ran fully headless - there was no console driver attached to the
protocol in the first place, so there was nothing to disconnect.

**Fix:** open the protocol in non-exclusive, read-only mode instead -
this code only ever needs to query the current mode and framebuffer
address once, never needs ownership.

### Bug: a later console-discovery step froze the boot with no console yet live

Once the framebuffer bug above was fixed, discovery and mapping worked
- but the boot froze solid before ever reaching a state where anything
was visibly working. Reordering the fallback chain (try the now-proven
framebuffer path before the still-unproven virtio-mmio-based one)
didn't just work around this - it got a *console* installed before the
freeze, which meant the next attempt could finally *see* what was
actually happening: a real, decoded exception, described next.

### Bug: a confirmed real bus fault reading a fixed, unconfirmed device address

With a console now live to report through, the exact failure became
visible directly:

```
ESR_EL1 decoded: EC 0x25 (Data Abort, same EL)
                 DFSC 0x10 (Synchronous External abort - a real bus
                            fault, not a permission or mapping issue)
FAR_EL1: matched a fixed virtio-mmio scan address exactly
```

**Root cause:** the virtio-mmio device-discovery scan reads a fixed set
of addresses looking for a "magic value" marking a populated device
slot - a convention confirmed correct on the emulator by directly
peeking those addresses through its own monitor interface before any
driver code was written. What had never been verified was what an
*unpopulated* slot does on hardware that isn't the emulator: the
working assumption (never actually tested) was "reads back as zero."
On real hardware, the very first probe address instead produced a
genuine external bus abort - real hardware objects loudly to a read
targeting nothing, where the emulator's software model just returns a
pattern as a courtesy.

**Fix:** gate every caller of this scan behind a heuristic - only
attempt it on platforms where a byte-stream console was already found
through one of the confirmed-safe discovery mechanisms above, since
that's the only platform shape (the emulator) this scan has ever
actually been proven safe on.

### Bug: the identical fault, at a different fixed address - a structural finding, not a second instance

Once the scan above was gated off, the boot progressed further and hit
a *second*, differently-addressed instance of the exact same fault
signature - this time with the faulting address matching the interrupt
controller's own fixed, "well-known for this emulator" base address.

**The real significance:** this wasn't just a second bug to patch - it
was proof that the entire convention of trusting a fixed, low-address,
emulator-shaped location for *any* device (not just the one that
happened to be tried first) is unsafe on real hardware. The timer
itself turned out to be architecturally exempt from this - it's
accessed purely through CPU system registers, never memory-mapped I/O
at all, so it's safe on any ARM core regardless of platform-specific
addressing. Only the interrupt controller (needed to actually deliver
the timer's interrupt to the CPU) depended on the unconfirmed
convention.

**Fix:** broadened the same safety gate to cover interrupt-controller
and timer setup as well, not just the original scan - accepting no
preemptive multitasking on this platform as a real, known, documented
consequence, confirmed not to block reaching a working shell at all
(the task-switching code has no dependency on the interrupt controller
having run).

**The payoff:** the very next real-hardware test reached the actual
goal of this entire arc - a live, rendering, prompt-displaying shell on
real hardware, for the first time in the project's history.

## Techniques that generalized well

- **When a fault is a permission fault, not a translation fault, look
  one level up the stack** - a linker section attribute, a page
  permission bit, something *about* the mapping rather than whether it
  exists at all.
- **A fault loop (the same fault, repeating forever) usually means the
  handler itself is unreachable**, not that the underlying condition is
  unusual - worth recognizing as its own distinct failure shape.
- **Re-derive register/attribute values independently before trusting
  them**, even ones that have already been checked once - a second,
  differently-sourced derivation (in this case, decoding the *actual
  runtime* register contents with a throwaway script, independent of
  whatever set them) catches transcription mistakes a single careful
  read-through won't.
- **Don't trust an address "everyone knows" for one specific emulator
  build.** Dumping and decoding the exact build's own internal
  description of its hardware, every time, is what actually made the
  GIC/timer addresses reliable.
- **A permissive host (an emulator) and a strict one (real hardware)
  will disagree specifically about invalid accesses** - a wrong guess
  that's silently tolerated in one place can be a hard crash in
  another. Treat "confirmed on the emulator" as a real but incomplete
  confirmation, not a final one.
- **When you can't find a root cause after genuinely exhausting the
  documented explanations, changing the shape of the problem
  (isolating instead of widening, in this project's case) is a
  legitimate resolution** - and it's worth writing down as an
  unresolved mystery rather than quietly implying it was understood.

## Where this ended up

A kernel that survives a real fault and reports it usefully instead of
just disappearing; an MMU running on its own tables, matching real
discovered RAM rather than a hardcoded range; a genuine preemption tick;
a real EL0/syscall boundary; and, after three dead-end discovery
mechanisms and one dead-end driver, a real, live, interactive console on
real Parallels-on-Apple-Silicon hardware - the actual foundation
everything in [the later keyboard-input work](xhci-keyboard-postmortem.md)
was eventually built on top of.

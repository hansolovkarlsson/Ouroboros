# Getting a USB keyboard working on Parallels (Apple Silicon): a debugging postmortem

This is a write-up of one day's work getting a real, physical USB keyboard
talking to [Ouroboros](../README.md), a from-scratch ARM64 microkernel,
running on Parallels Desktop on Apple Silicon. It's kept separate from
this project's own historical record (`CLAUDE.md`, `CHANGELOG.md`)
because the bugs here aren't really Ouroboros-specific — they're the kind
of thing anyone writing a bare-metal USB/xHCI driver and testing it
against a real hypervisor is likely to run into, and there wasn't an
existing write-up we could find that covered them. If you're building
your own OS and your keyboard driver "sort of" works — enumerates fine,
sends what look like correct commands, but you get garbage or silence
back — some of this may save you a day of your own. This is the third of
three related pieces: [`boot-bringup-postmortem.md`](boot-bringup-postmortem.md)
covers the earliest, most foundational bring-up work - exception
vectors, the MMU switch, and the console-discovery saga that found a
working display in the first place - and
[`shell-and-filesystem-postmortem.md`](shell-and-filesystem-postmortem.md)
covers everything between that bare prompt and a real, disk-backed
userland shell, which is what this document's driver finally had
something to type into.

Six real, confirmed bugs, each found by direct evidence (register
dumps, decoded exception registers, byte-for-byte comparisons) rather
than guessing. In order:

1. A PCI Command register bit-position bug that looked like a platform
   quirk until it wasn't.
2. Real PCIe firmware panicking on a PCI config-space write that a
   software emulator (QEMU) tolerated silently.
3. A physical PCI BAR landing outside a simplifying assumption our own
   page tables had baked in.
4. Parallels' USB passthrough silently not forwarding an entire class of
   USB request to the physical device.
5. A working driver reading the wrong physical device.
6. A driver that quietly dropped a keystroke once real preemption gave
   it something new to be interrupted by — found weeks later, in a
   separate milestone, but it belongs here with the rest of this
   driver's history.

## Background: why this was hard to see coming

Ouroboros develops primarily against QEMU (`aarch64`, `virt` machine),
because it's fast to iterate against and fully scriptable. Real hardware
testing — in this case, Parallels Desktop running the guest on Apple
Silicon — happens periodically, by a human, and each round trip costs
real wall-clock time: rebuild, re-image a disk, boot the VM, look at the
screen (there's no serial console on this guest — see the "why is there
no serial log" aside below), and report back. That asymmetry matters:
QEMU is where you can iterate fast and be wrong nine times before
breakfast; real hardware is where you find out that some of your
"confirmed" fixes were confirmed against the wrong thing.

Every one of the five bugs below is invisible on QEMU. All five only
ever showed up on real Parallels hardware, and four of them straight-up
*worked correctly* on QEMU first try, which is exactly the trap: a driver
that passes its only automated-feeling test can still be wrong in ways
that only real, spec-compliant (or spec-*non*-compliant) hardware will
show you.

### Aside: why there's no serial log for any of this

If you're used to `dmesg`-driving your bring-up, the biggest practical
obstacle here isn't any single bug — it's that this guest has no working
byte-stream console on Parallels at all. Parallels' ARM64 firmware
doesn't describe its console via ACPI SPCR, doesn't expose it over PCI in
any form our driver could find, and (a whole separate investigation)
doesn't appear to implement virtio-console either. The only console that
works post-boot-services is a raw GOP framebuffer text console: it's
write-only (there was, at the *start* of this, no keyboard input at
all — this document is literally about fixing that), and it's the only
thing you get to look at. Every one of the debugging techniques below is
shaped by that constraint: nothing that happens during UEFI boot
services is ever visible again once the framebuffer console clears the
screen and takes over, so any diagnostic you want visible *after* a
crash has to be re-printed through the post-boot console, not just
logged once during discovery. More than one debugging round in this
project was extended by a day purely because a useful diagnostic line
had already scrolled off-screen or been overwritten by the time a crash
happened.

## Bug 1: the Command register bit that wasn't where we thought

**Symptom:** After discovering the xHCI controller's PCI BAR and writing
what we believed was "enable Memory Space + Bus Master" to its PCI
Command register, every register read through that BAR came back as
`0xffffffff` — the standard PCI "nothing responds here" pattern — on
*both* QEMU and real Parallels hardware.

**The trap:** This is exactly the shape of bug you can talk yourself out
of finding, because it presents as a platform/environment problem, not a
code problem. On QEMU specifically, the controller's PCI BAR read back as
completely unassigned by firmware (`0` in both the low and high BAR
dwords) — which is real and true (see "a whole separate rabbit hole"
below) — so we built and shipped a whole BAR-self-assignment routine
before ever suspecting the *enable* bit itself was wrong. Multiple
follow-up hypotheses (wrong write width, wrong access granularity,
whether the write needed to be 16-bit vs 32-bit) all got tested and
ruled out first, because they were more plausible-sounding than "you
transcribed a bit position wrong."

**How it was actually found:** A diagnostic that printed the Command
register's value *before* and *after* our own write, surfaced through
the post-boot console (see the aside above — this diagnostic had to be
re-printed post-crash, or it would never have been seen). One real
Parallels test produced:

```
PCI command register 0x0010 -> 0x0015
```

`0x0010` = `0b10000` = bit 4 set (Memory Write and Invalidate Enable — an
unrelated bit, apparently firmware's own default). `0x0015` =
`0b10101` = bits 0, 2, and 4 set. Bit 2 is Bus Master Enable, correctly
set. But bit 0 is **I/O Space Enable** — not Memory Space Enable, which
is bit **1**. The constant in our driver was:

```rust
const CMD_MEMORY_SPACE: u16 = 1 << 0; // WRONG — this is I/O Space Enable
const CMD_BUS_MASTER:   u16 = 1 << 2; // correct
```

Real off-by-one, in the literal sense — bit 0 vs bit 1 — sitting quietly
in a constant that had been copy-reasoned-about half a dozen times
without anyone re-deriving it from the PCI Local Bus spec's actual bit
table. An xHCI controller has no I/O-space BAR to enable at all, so this
bit's value was, from the device's perspective, meaningless — we were
never actually enabling the one bit (Memory Space) that would let a
memory-mapped BAR respond to anything.

**Lesson:** When a register write "succeeds" (no error, readback matches
what you wrote) but has no observable effect, re-derive the bit
positions you're relying on from the spec table directly, by hand, before
looking anywhere else. A successful write of the *wrong bit* is
indistinguishable, from a software point of view, from every other kind
of "nothing happened" failure, and it will not be the first hypothesis
you reach for.

## Bug 2: real firmware panics where software emulation just shrugs

**Symptom:** Booting on real Parallels hardware with an early ("let's
just write the BAR ourselves") version of the driver didn't produce a
kernel crash or an exception report at all — the whole *VM* aborted, with
Parallels showing a crash-report dialog.

**What was in the crash report:** Parallels' own hypervisor log
(`libMonitorArm.dylib`, the actual VM monitor implementation) recorded:

```
mon.abort.message = PANIC@11.28 UEFI-exception-ArmPciCpuIo2Dxe.dll
```

`ArmPciCpuIo2Dxe.dll` is the EDK2 firmware driver that implements
low-level CPU I/O access *for PCI config space itself* — the thing
underneath the higher-level PCI protocol our own driver calls into. A
panic there means firmware's own PCI access code hit something it
couldn't handle and deliberately halted, *before our kernel's own code
ever got control* — this happens during UEFI boot services, well before
our own exception vector table is even installed, so there was no way
for our kernel to catch or report this itself. The VM just stops.

**Root cause:** the specific operation that triggered it was a
BAR-reassignment probe — the classic "write `0xFFFFFFFF` to the BAR,
read back the size mask, write a real address" dance, done because
QEMU's specific firmware build leaves this controller's BAR completely
unassigned (see the aside below) and we needed to fix that ourselves to
get anywhere on the QEMU dev loop. This is a completely standard, textbook
technique — real PCI bus drivers do it constantly — and QEMU's own PCI
emulation tolerates it without complaint. Real PCIe hardware/firmware,
apparently, does not: whatever internal state that sequence disturbed on
a real, in-use PCI device was enough to make firmware's own PCI I/O layer
panic outright.

**Fix:** stopped touching PCI config space with anything but *reads*, plus
one narrow, conditional Command-register write (see Bug 1) — no BAR
reassignment, no size probing, ever, on real hardware. If a real BAR
comes back as genuinely unassigned on real hardware, the honest answer is
"this driver can't safely fix that," not "let's try writing to it and
see."

**Aside — a whole separate rabbit hole this triggered:** getting *to*
this bug required first discovering that QEMU's own OVMF firmware build
leaves an unclaimed PCI device's BAR at `0` and its Command register at
`0` — full stop, no address assigned at all — specifically because
nothing during boot ever binds a driver to it (this kernel boots over
virtio-mmio, not this xHCI PCI device, so nothing in the boot path ever
touches it). That's a real, confirmed, QEMU/OVMF-build-specific quirk
(confirmed via QEMU's own monitor: `info pci` reported the BAR as "not
mapped", `info mtree` showed no live memory region backing it at all),
and it's *not* representative of what production firmware does — real
Parallels firmware, being a fuller/production implementation, had
already assigned this controller a real, sane, low BAR address
(`0x10007000`) without any help from us. **The self-assignment logic
built to work around the QEMU quirk should never have been allowed to
run unconditionally on a platform where it wasn't needed** — it's exactly
what triggered the firmware panic in Bug 2. If you find yourself building
a workaround for one specific dev-loop's laziness, gate it as narrowly as
possible, and be suspicious of it the moment you're back on real
hardware.

## Bug 3: a real PCI BAR doesn't respect your simplifying assumptions

**Symptom:** Even after the address-space bugs above were sorted out,
the very first register read through the discovered BAR took a genuine
CPU exception — not a driver-level error, an actual `Data Abort`.

**Decoded:** `ESR_EL1 = 0x96000004`. Splitting that: bits `31:26` (the
Exception Class) = `0x25` = Data Abort; bits `5:0` (the fault status
code) = `0x04` = "Translation fault, level 0" — not a permissions
problem, a genuine *nothing is mapped here* fault, at the level-0 (top)
table walk.

**Root cause:** our own page tables. This kernel's identity-map setup had
been written under the assumption that all RAM and every device region
it would ever need lived within the first 512GB of address space — one
top-level (L0) page-table entry's worth. Real, honest reasoning at the
time: RAM addresses are always low, and the one "device region" this
project had ever hardcoded was also low. A discovered PCI BAR is under
no such obligation. This controller's BAR resolved to `0x8000004000` on
the QEMU dev loop — which is, not coincidentally, *exactly* 512GB (QEMU's
`virt` machine places its high 64-bit PCIe MMIO window starting there) —
squarely outside the single L0 entry our page tables had ever set up.

**Fix:** generalized the identity-map builder to allocate additional
top-level table entries on demand, for whichever address range a
discovered device's BAR actually needs, instead of assuming everything
interesting fits in the first 512GB. This is a good general lesson for
anyone hardcoding an address-space assumption while bringing up a kernel
incrementally: a value that's "always true" for every hardcoded address
you've picked yourself stops being true the moment you start reading
addresses that hardware (or firmware) picked instead.

## Bug 4: an entire USB request *class* silently going nowhere

This is the deepest and most surprising finding of the day, and the one
most likely to be useful to someone else.

**Symptom:** Once the controller was actually enumerating the keyboard
correctly (slot enabled, device addressed, `SET_CONFIGURATION`
succeeding), polling it for input via the standard USB HID mechanism —
repeated `GET_REPORT` control requests to the default control endpoint —
never returned anything resembling real keyboard data. Across several
rounds of testing it came back as:

- This driver's *own* `GET_REPORT` Setup packet, byte-for-byte, echoed
  straight back as if it were the response (confirmed by decoding the
  returned bytes against the exact request we'd sent — `bmRequestType`,
  `bRequest`, `wValue`, `wIndex`, `wLength`, all present, in order,
  including matching a *changed* `wLength` across two different test
  builds — this wasn't a coincidence, it was our own request being
  handed back to us).
- In one test, what decoded cleanly as a chunk of a *different* standard
  descriptor (Interface + HID + Endpoint descriptor bytes) — real
  descriptor-shaped data, just not what we'd asked for and not a live
  input report either.

Neither of those is "the device sent nothing." Both are "the response
buffer contains *something* structured," which is a much more confusing
failure mode than a clean timeout or error — it looks like success from
a distance.

**Ruling out the obvious explanations, in order, each with direct
evidence rather than assumption:**

- *Are we matching the wrong completion event?* Added a diagnostic
  printing every transfer-completion event's own pointer field next to
  the address of the TRB we expected it to correspond to. They matched,
  every time. Not this.
- *Is this a buffer-size coincidence* (our request and our own Setup
  packet happened to be exactly the same length)? Widened the requested
  data length from 8 bytes to 64 across two separate real-hardware test
  rounds. The garbage tracked the *new* length exactly (the trailing
  `wLength` field visible in the echoed bytes changed from `0x0008` to
  `0x0040` to match). Not a coincidence — whatever's happening adapts to
  what we send, in real time. Not a fixed-size aliasing bug.
- *Is the DMA target buffer even being written to at all?* Filled the
  destination buffer with a distinctive, otherwise-impossible byte
  pattern (`0xEE` repeated) immediately before ringing the transfer
  doorbell. The poison pattern did *not* survive — the buffer was
  genuinely overwritten by something. So the transfer mechanism itself
  (rings, doorbells, completion events, DMA) was all working correctly;
  the *content* being delivered was simply not real device data.

**The test that actually settled it:** issuing a completely different,
*standard* (not class-specific) USB request — `GET_DESCRIPTOR(Device)` —
right next to a failing `GET_REPORT`. It came back perfectly:

```
[12, 01, 00, 02, 00, 00, 00, 40, 27, 06, 01, 00, 00, 00, 01, 04, 0b, 01]
```

`bLength = 0x12` (18, correct), `bDescriptorType = 0x01` (Device,
correct), and further in, `idVendor = 0x0627` / a later test against the
real physical keyboard showed `idVendor = 0x203a` — Parallels' own real,
registered USB vendor ID for its virtual keyboard passthrough. This is
unambiguously live, correct, real device data.

**The conclusion:** *standard* USB control requests (`GET_DESCRIPTOR`,
`SET_CONFIGURATION`) reach the real, physical device correctly through
Parallels' USB passthrough. *Class-specific* HID requests
(`SET_PROTOCOL`, `GET_REPORT` — the two mechanisms this driver had been
built around) do not get forwarded to the real device at all. This is a
real gap in Parallels' USB passthrough implementation for this device
class, not a bug in any driver talking to it — and it's the kind of thing
that's very easy to misdiagnose as "my driver's USB request encoding
must be subtly wrong," because the wire-level bytes we were sending were
genuinely correct the entire time.

**The fix:** stop using `GET_REPORT` (a class request) to poll for
input at all. Use the mechanism every production USB HID driver actually
uses at runtime anyway: the device's **interrupt IN endpoint**. Once
configured — via `Configure Endpoint`, a standard xHCI *command*, not a
USB class *request* at all — the host controller polls the physical
device on its own schedule and delivers new reports with zero further
request/response round trips for software to get wrong. This sidesteps
the whole broken code path structurally, rather than working around it.
(`SET_PROTOCOL(Boot Protocol)`, also a class request, almost certainly
still isn't reaching the device either — left in place as a best-effort,
non-fatal attempt, with the driver's HID report parsing written to cope
either way, since some devices use the same simple report layout in both
Boot and Report protocol regardless of which one is nominally active.)

**Lesson:** if a request "succeeds" (no error status, no timeout) but
its content looks *wrong in a structured way* — not zeroed, not random
noise, but shaped like something else you know about — check whether the
platform underneath you might be substituting or failing to route that
specific request, rather than assuming your own encoding of it is wrong.
And when a "simpler" mechanism (control-transfer polling) and a "more
correct, more standard" mechanism (interrupt endpoints) are both
available, be aware that a passthrough/virtualization layer may only
have been built and tested against the one real operating systems
actually use.

## Bug 5: a perfectly working driver, reading the wrong device

**Symptom:** After getting the interrupt endpoint fully configured and
armed, real live data started flowing — continuously. Real, changing,
non-garbage bytes, confirmed by the fact that moving the mouse over the
VM's window produced a steady stream of changing report data.

That's the tell: it was the **mouse**, not the keyboard. A real
Parallels VM exposes at least a virtual mouse/tablet on the same xHCI
controller as the keyboard, and this driver's port-scanning logic — find
the first port reporting a connected device, use that one — had no way
to tell them apart. It grabbed whichever device happened to enumerate on
the lower-numbered port.

**Fix:** don't trust "the first connected port" at all. Walk *every*
connected port, and for each one, fetch its Configuration descriptor and
actually check the HID interface's declared class and protocol
(`bInterfaceClass = 3` for HID, `bInterfaceProtocol = 1` specifically for
Boot-Protocol Keyboard — `2` is Mouse) before committing to configuring
its interrupt endpoint. If a device isn't a keyboard, move on to the
next connected port rather than treating "found *a* HID device" as
"found *the* keyboard."

**Lesson:** in a virtualized/multi-device environment, "the first thing
that responds" is never a substitute for actually checking that you've
found the specific thing you're looking for — even (especially) once
everything else about your driver is working correctly. This bug only
became visible *because* the rest of the driver was finally right; it
had been silently waiting behind four other bugs the whole time.

## Bug 6: a driver that assumed one report never contains two keystrokes

This one didn't surface the same day as the other five — it took a
later, separate milestone (real ACPI-based interrupt-controller
discovery, giving this platform working preemptive multitasking for the
first time) to expose it, because that milestone changed something this
driver had always quietly relied on: how often it actually gets to run.

**Symptom:** once preemptive task-switching started working on real
hardware, typed input occasionally arrived at the shell one character
short — `uptime` typed correctly would sometimes show up as `uptme` or
`uptie`. Not a hang, not a crash, not even consistent: it happened
roughly once every several commands, never reproduced identically twice,
and — the detail that made it genuinely confusing — the driver's own
debug log showed every expected HID report arriving, *including* the
one for the character that went missing.

**The trap:** that last detail pointed straight at the wrong culprit.
The obvious-looking explanation was a hardware/timing race — this
driver only ever keeps one interrupt-transfer buffer posted to the
device at a time, re-arming it fresh after each read, and once the
consuming task only runs in alternating time slices instead of
continuously, a report arriving before that re-arm completes has less
slack than it used to. That's a real, entirely plausible failure mode
for polled USB HID input in general — it just wasn't what was actually
happening here, and assuming it was would have sent the fix in the
wrong direction (retry/backoff logic, or a second buffer) instead of at
the real bug.

**Root cause, found by actually reading the code path the log line
lived in rather than trusting the log line's implication:** a single
polled HID report can legitimately contain more than one newly-pressed
keycode at once. USB HID's boot-protocol keyboard report format reports
*currently held* keys, up to six at a time, not a queue of press/release
events — so if this driver's own poll happens to land after two
separate keys have both transitioned to pressed since the last poll
(most likely when a poll gets skipped because this task was preempted
between two hardware samples, but not exclusively — two real keys
pressed within one poll interval can do it too, preemption or not), both
show up together in the very next report it reads. The driver's report
handler looked like this:

```rust
let previous = self.last_report;
self.last_report = buf;                    // recorded before scanning
console::println!("xhci: report {buf:02x?}");

for &keycode in &buf[2..8] {
    if keycode == 0 || keycode < 4 { continue; }
    if previous[2..8].contains(&keycode) { continue; }
    if let Some(ascii) = keycode_to_ascii(keycode, shift) {
        return Some(ascii);                 // returns on the FIRST match
    }
}
```

`self.last_report = buf` runs — recording this entire report as "already
seen" — *before* the scan loop that looks for newly-pressed keys, and
the loop itself returns on the first qualifying keycode it finds. If a
report contains two new keycodes, the first is correctly translated and
returned; the second is gone forever, not just delayed — the very next
poll compares against `last_report`, which already includes it, so it
can never look "new" again. The debug print a few lines above the loop
explains exactly why the log showed the "missing" character's report:
that line runs unconditionally for every accepted report, whether the
scan loop below it manages to return all of that report's new keys or
only some of them.

**Fix:** stop discarding everything past the first match. Drain *every*
qualifying keycode from a report into a small fixed-size pending buffer
(5 slots — a report holds at most 6 simultaneous keycodes, and the
first one found is always returned immediately rather than queued, so
at most 5 can ever need to wait), and drain that buffer on subsequent
polls before ever touching the event ring again:

```rust
fn poll_key(&mut self) -> Option<u8> {
    if self.pending_len > 0 {
        let ascii = self.pending[0];
        self.pending.copy_within(1..self.pending_len, 0);
        self.pending_len -= 1;
        return Some(ascii);
    }
    // ... unchanged: pop an event, read the report, re-arm, edge-detect ...
    let mut result = None;
    for &keycode in &buf[2..8] {
        if keycode == 0 || keycode < 4 { continue; }
        if previous[2..8].contains(&keycode) { continue; }
        let Some(ascii) = keycode_to_ascii(keycode, shift) else { continue };
        if result.is_none() {
            result = Some(ascii);
        } else {
            self.pending[self.pending_len] = ascii;
            self.pending_len += 1;
        }
    }
    result
}
```

**Confirmed fixed on real hardware, not just reasoned through:** ten
consecutive `uptime` invocations typed back-to-back, all recognized
correctly with zero drops — a clear, direct contrast against the
intermittent failures observed before the fix, on the exact platform
and exact input pattern that had triggered it.

**Lesson:** a debug log line that runs unconditionally, positioned
*before* the logic that determines the actual outcome, can make a bug
look like it's in a completely different layer of the system than it
actually is. "The log shows the data arrived" and "the log shows the
data was correctly handled" are different claims, and it's worth being
precise about which one a given print statement is actually proving
before trusting it to rule anything out. It's also a reminder that a
platform change elsewhere in the system (here: real preemption finally
working) can expose a latent bug in completely unrelated code that had
simply never been exercised under those conditions before — this
report-coalescing possibility had presumably always existed, it just
needed a poll gap wide enough to matter.

## Debugging techniques that generalized well

A few things that were reused across more than one bug above, worth
calling out on their own:

- **Poison your DMA buffers.** Fill a transfer's destination buffer with
  a fixed, distinctive, otherwise-impossible pattern immediately before
  triggering the transfer. Whether the poison survives (and exactly how
  much of it) tells you directly whether a DMA write happened at all,
  and how much data was really transferred — much more informative than
  just eyeballing whatever bytes come back.
- **Widen a request to break a suspicious coincidence.** If a buggy
  result's *shape* matches some other value in your system (in this
  case: our own request's byte length), deliberately change that value
  and see whether the bug's shape changes with it. If it does, you've
  confirmed a real dependency, not ruled one out.
- **Reach for the most standard, most boring request you can, as a
  control.** When a specific request type is misbehaving, test the
  most basic, universally-implemented request in the same protocol
  family right next to it. A clean pass on `GET_DESCRIPTOR` while
  `GET_REPORT` fails is a completely different diagnosis than both
  failing.
- **Decode raw exception registers by hand, every time.** `ESR_EL1`'s
  Exception Class and fault-status-code fields, `FAR_EL1`'s faulting
  address — these turn "the VM crashed" into "a Data Abort at exactly
  this address, for exactly this documented architectural reason,"
  which is the difference between a mystery and a lead.
- **Route every diagnostic through whatever console survives a crash,**
  not just the one that's convenient to log through at the time. On a
  platform with no working byte-stream console (see the aside near the
  top), a diagnostic that only prints during boot services is
  worthless the moment something later crashes — the value has to
  reach a console that's still going to be on screen afterward.
- **Fix one variable per real-hardware round trip, and predict the
  outcome before you spend it.** Real-hardware testing here required a
  human, a VM reboot, and a screenshot each time — expensive compared
  to the QEMU dev loop. Every round trip in this session targeted one
  specific, falsifiable hypothesis (not "let's see what changes"),
  which is what made five real bugs findable in one day instead of one.

## Where this ended up

A real, physical USB keyboard, on real Parallels-on-Apple-Silicon
hardware, typing into a shell running on a from-scratch kernel with no
prior USB support at all — full round trip confirmed: individual
keystrokes, backspace, Enter submitting a real command line, with every
keystroke landing correctly even under real preemptive multitasking.
Getting there took a from-scratch xHCI driver (capability/operational
register programming, command ring, event ring, device slot enumeration,
control transfers, and finally a real interrupt-endpoint transfer ring)
and six real, independently-confirmed hardware bugs along the way — five
found in the initial one-day push, none of them visible on the software
emulator this project otherwise develops against day to day, and a
sixth found weeks later, exposed only once a separate milestone gave
this platform real preemption for the first time.

If you're doing the equivalent work for your own kernel: budget for the
fact that "works on QEMU" and "works on a real hypervisor" are
genuinely different claims, that a firmware/passthrough layer's rough
edges will not announce themselves as errors, and that the most
confusing failures are the ones that come back looking like *something*,
not nothing.

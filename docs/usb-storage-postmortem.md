# Giving Parallels a disk: a one-day debugging postmortem

This is a write-up of one day's work (with a lot of smaller milestones
along the way) taking [Ouroboros](../README.md), a from-scratch ARM64
microkernel, from "our primary hardware target has no disk and we don't
know if it ever can" to reading a real USB stick, loading programs from
it at runtime, and passing IPC messages between them — on real
Parallels Desktop on Apple Silicon. It's the fourth of four related
pieces: [`boot-bringup-postmortem.md`](boot-bringup-postmortem.md)
covers exception vectors, the MMU, and the console-discovery saga;
[`shell-and-filesystem-postmortem.md`](shell-and-filesystem-postmortem.md)
covers the road to a disk-backed userland shell (on QEMU — this
document is about getting the same thing on real hardware);
[`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md) covers the
USB keyboard driver this day's storage driver was built on top of.

Like the others, this is kept separate from the project's own
historical record (`CLAUDE.md`, `CHANGELOG.md`) because most of what's
here isn't Ouroboros-specific: if you're writing a bare-metal OS and
wondering how to get *any* disk on a hypervisor that doesn't offer you
one, or building a USB mass-storage driver over your own xHCI stack,
some of this may save you real time. Everything below was confirmed by
direct evidence — decoded descriptors, on-screen device inventories,
byte-level round trips — not guessed.

## The starting problem: a hypervisor with no disk for you

Parallels on Apple Silicon boots our kernel fine (UEFI reads it off a
virtual hard disk), and by the start of this day the kernel had a
working framebuffer console, USB keyboard, preemptive multitasking, and
a full shell — but no way to touch a disk *after* boot. Firmware reads
the boot volume during UEFI boot services; the moment
`exit_boot_services` runs, that access is gone, and every disk command
in the shell printed "no filesystem mounted this boot."

The obvious question: what storage interface does Parallels actually
expose to the guest, and can we drive it?

### Finding 1: the answer is "none" — and proving a negative takes real experiments

A PCI device inventory (a diagnostic the kernel already carried) showed
five devices: HD Audio, EHCI, xHCI, virtio-net, and one
vendor-specific Parallels device (`vendor=0x1ab8`, class `0xff`). **No
storage controller of any kind** — despite the VM demonstrably booting
from its `sata:0` hard disk. "SATA" in Parallels' configuration does
not mean an AHCI controller on the guest bus.

That could still have meant "wrong attachment type," so we tested the
alternatives directly, from the host, using Parallels' own CLI:
`prlctl` told us an ARM64 EFI VM accepts exactly two disk interfaces
(`ide` and `scsi`), and `scsi` accepts three subtypes. Attaching a
scratch disk as `scsi`/`lsi-sas` and then `scsi`/`lsi-spi` — real,
documented LSI Fusion-MPT hardware that would have been an
implementable spec — produced a **byte-identical PCI inventory both
times**. The emulated controllers simply don't exist on Apple Silicon,
even when the configuration accepts them. The third subtype,
`buslogic`, was refused by Parallels itself ("cannot use both the
BusLogic SCSI controller and EFI firmware").

Conclusion, with evidence rather than resignation: all storage on this
platform flows through a proprietary, non-PCI path. "Implement a
documented spec" is not available for any attached-image disk. The one
documented thing on that bus with a path to storage was the xHCI
controller we already drove for the keyboard — which pointed at USB
mass storage via a passed-through physical stick.

**A tooling problem worth recording from the same session:** the
kernel's PCI inventory prints during UEFI boot services, and this
platform's framebuffer console clears the screen when it installs —
about **two seconds** after power-on. We tried to screenshot the
inventory with a capture loop at 0.4-second intervals and never caught
anything but the finished shell. The fix was to make the diagnostic
*return* its data so the kernel could re-print it through the console
that survives. If your target boots faster than you can read, don't
race it — replay the data on the other side of the transition.

### Finding 2: USB passthrough routes by speed — USB 2.0 goes somewhere you can't follow

First enumeration attempt used the USB 2.0 stick we had on hand
(Parallels lists it as `high` speed). Passed through and confirmed
`Connected-To-Vm: YES` — and it **never appeared on the xHCI
controller**. A temporary diagnostic (dump every connected xHCI port at
scan time, wait six wall-clock seconds, dump again) showed the same two
ports — virtual mouse, keyboard — before *and* after the wait.

The VM has both an EHCI (USB 2.0) and an xHCI (USB 3.0) controller,
and Parallels routes passthrough devices by their speed: **`high`-speed
devices land on EHCI, `super`-speed devices on xHCI.** We had no EHCI
driver and no desire to write a second host-controller driver for one
stick, so the answer was: get a USB 3.x stick. When one arrived, it
appeared on an xHCI root port at `speed=4` (SuperSpeed), enumerated
through our own multi-device scan, and presented exactly interface
`class=0x08 subclass=0x06 protocol=0x50` — SCSI-transparent command set
over Bulk-Only Transport, the textbook mass-storage stack.

### Finding 3: passthrough attaches *late* — your boot-time scan will miss it

The same six-second diagnostic produced a second, subtler finding: on
Parallels, a passed-through USB device attaches **several seconds after
the VM starts** — after our kernel's one-shot boot-time port scan has
already run (boot to shell takes ~2 seconds; the attach lands around
6–8). The very first bare run missed the stick entirely; the diagnostic
build only caught it because the deliberate 6-second delay happened to
push the scan late enough.

This turned "no hot-plug" from a documented nicety into a hard design
input: the storage driver got a runtime port rescan, triggered
explicitly by a `mount` shell command (boot, wait a moment, type
`mount`). QEMU's monitor (`device_add usb-storage`) turned out to be a
perfect stand-in for testing this: genuine hot-plug into a running VM,
fully scripted.

### Finding 4: what single-device assumptions quietly hide

Before storage, the xHCI driver was deliberately one-device: the scan
tried each port until it found the keyboard and *abandoned* everything
else. Preparing for keyboard + stick coexistence exposed two pieces of
per-device state that were single **only because of that abandonment**:

- The **Output Device Context** — the memory the controller itself
  writes a device's state into, via a per-slot pointer array. One
  shared context with two live slots pointing at it is real
  memory corruption, not a bookkeeping wrinkle.
- The **EP0 transfer ring** — each device's Address Device command
  declares its own ring position to hardware. The old shared ring
  needed an explicit software rewind per candidate device; two live
  devices can't share the memory at all.

Both became small per-device pools. Two ordering/routing rules came out
of the same work, and both generalize: **activate the endpoint that
generates unsolicited traffic last** (the keyboard's interrupt endpoint
is armed only after the whole scan, so keystroke events can't interleave
with a later device's setup), and **route completion events by slot ID
and endpoint ID** — with more than one live device, "the next transfer
event" and "my transfer event" are different things.

### Finding 5: the mid-transfer keystroke that would have killed the keyboard permanently

The sharpest correctness point in the storage driver was found by
reading the event loop before it ever failed: during a bulk transfer,
the driver waits synchronously for *its* completion event on the shared
event ring. A keystroke arriving in that window posts *its* event to
the same ring. The naive wait skips-and-drops foreign events — which
here would not just lose the keystroke: the keyboard's single interrupt
buffer is only re-armed when its event is *processed*, so a dropped
event means **the keyboard never receives anything again**. Typing
during disk I/O would brick input for the rest of the boot.

The fix is routing, not dropping: the wait hands keyboard-slot events
to the exact same report-processing code the normal poll uses (factored
so both can call it), which re-arms the buffer and queues the
keystrokes. Verified by firing keystrokes interleaved with disk
commands: zero lost characters. If your driver has any synchronous
wait on a shared completion queue, ask what happens to everyone else's
completions while you wait.

### Finding 6: SCSI Unit Attention — found by the hot-plug test, not the spec

The QEMU hot-plug test produced a beautifully diagnosable failure on
first contact with a freshly attached device: `INQUIRY` succeeded,
then `READ CAPACITY(10)` failed with CSW status 1, and a manual retry
succeeded. That pattern *is* the explanation: a freshly
attached-or-reset SCSI device reports a **Unit Attention** condition,
failing the first command with CHECK CONDITION until the sense data is
fetched — and `INQUIRY` is spec-exempt from Unit Attention, which is
why it sailed through. The standard bring-up (a short TEST UNIT READY /
REQUEST SENSE loop before the first real command) cleared it. Real
sticks set Unit Attention too, so the QEMU-found fix mattered on
hardware. Lesson: when command A works and command B fails on a fresh
device, check which commands are exempt from pending conditions before
suspecting your transport.

### Finding 7: two error codes that collapse into one will eventually lie to you

Two same-day findings from outside the storage driver, same underlying
moral. First: the kernel bounds every userland syscall buffer at 512
bytes, and rejects longer ones with the same generic sentinel used for
"no such file." The shell's shiny new append operator (`>>`) used a
1024-byte buffer for its read-back — the rejection was
indistinguishable from "file doesn't exist," whose legitimate meaning
in an append is "create it." Result: **append silently behaved as
overwrite**, discarding the existing content, caught only because the
very first append test compared actual file contents. Second: the day
we split the collapsed filesystem error into specific codes, the new
messages instantly exposed a real mis-mapping that had been hiding for
weeks — `rmdir /` reported "invalid name" instead of "can't remove the
root directory," because a path-splitting helper failed before the
root check ever ran. Collapsed error codes don't just annoy users;
they hide bugs from their own authors.

### Finding 8: a "can't happen yet" is a bug with a start date

Directory extension (letting `mkdir` grow a full directory by a
cluster) was a small feature — the real find was in `rmdir`, which
freed exactly *one* cluster. That was correct for as long as every
directory was single-cluster **by construction**, and became a silent
cluster leak the moment directories could grow — exactly the sequence
(fill a directory, empty it, remove it) the new feature made routine.
The same shape appeared earlier in the project and will appear again:
an invariant maintained by a limitation elsewhere is a bug scheduled to
trigger when that limitation is lifted. When you lift one, grep for
everyone who was leaning on it.

### Finding 9: the toolchain can poison you even in release mode

Ouroboros userland is position-independent and links against Rust's
*prebuilt* `libcore`, which contains some non-PIC object code. The
known rule was "userland must build in release" (debug builds pull the
poisoned objects in via panic machinery). This day added a sharper
corollary: even in release, an ordinary `&str[..n]` slice pulled in
`core::str::slice_error_fail` — whose can't-actually-happen error path
formats the offending string with enough of `core::fmt` to drag a
non-PIC object into the link and fail the build outright. The fix was
non-panicking `.get()` indexing. If you link prebuilt core libraries
under a stricter relocation model than they were built for, panic
*formatting* paths are your enemy even when the panics are impossible.

### Finding 10: the anomaly we recorded and did not explain

Honesty section. The very first real-hardware run after assigning the
USB stick to the VM showed **no synthetic keystrokes arriving at
all** — the boot completed normally and the screen simply never
echoed a single typed character. It never reproduced: a control run
without the stick was clean, a six-command timeline probe typed
straight through the attach window was clean, and every subsequent
full run was clean. Our best (unproven) explanation is that the
scripted keystroke stream (`prlctl send-key-event`) was lost during
first-attach USB turbulence on the host side. It's recorded as exactly
that — one unreproduced run, bounded by control experiments — rather
than silently forgotten or hand-wavingly "fixed." Negative results and
unexplained one-offs belong in the record; the alternative is
re-debugging them from scratch when they resurface.

## Methodology notes: the headless hardware lab

None of the above would fit in a day without the scripted
real-hardware loop built the previous day, extended twice today:

- `prlctl` drives the VM headlessly (start, per-scancode typing,
  screenshots); `prlsrvctl usb set/del` assigns and revokes a physical
  USB device to the VM with no GUI. Every hardware experiment in this
  document ran unattended.
- Typing grew **chords**: `>` as a held-Shift scancode pair, and a
  `CTRL-C` pseudo-command as a held-Ctrl chord — which incidentally
  proved the kernel's own Ctrl-modifier keymap end to end (a broken
  mapping would have typed a visible stray `c`; the screen showed
  none).
- **One variable per round trip** stayed the rule: the USB 2.0 stick
  question got its own boot; the late-attach question got a dedicated
  diagnostic build; the stick-attach anomaly got a control run without
  the stick before anything was concluded.
- **Organic tests beat synthetic ones** where the system allows them:
  512-byte clusters made directory extension reachable with 15 real
  files; a deliberate 2-second delay in a test program made the
  blocking-`wait` path testable by a human-speed typist; QEMU monitor
  hot-plug made the rescan path testable without hardware.

## The ending

The same screen that started the day saying "no filesystem mounted
this boot" ended it showing: `mount` finding a real Lexar stick,
`INQUIRY` returning its real vendor strings, a 116 GiB capacity, a
mounted FAT32, `ls` of the stick's actual files — then `exec
/pong.bin` loading a program off that stick at runtime (a first for
this kernel on real hardware), a complete IPC round trip through it,
and a blocking `wait` collecting a second program's exit status, with
the two tasks' output visibly interleaved on the console. Every
userland facility the kernel has now runs on the platform it was built
for.

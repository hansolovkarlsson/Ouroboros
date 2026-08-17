# Ouroboros changelog

Historical record of completed milestones, newest first. For
forward-looking plans, see [`roadmap.md`](roadmap.md); for the
debugging history and lessons behind each decision (what was tried,
what broke, how it was diagnosed), see `CLAUDE.md`; for *how* something
here actually works today, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md).

## Directory extension - full directories grow by a cluster instead of failing

`fat32.rs::insert_dir_entry` (the single choke point every
entry-creating operation goes through - `mkdir`/`touch`/`write`/`cp`/
`mv`) now extends a directory's cluster chain when every existing slot
is taken, claim-then-zero-then-link ordering so a partial failure never
corrupts the chain; `Error::DirectoryFull` is deleted (unconstructable).
The real correctness piece: `rmdir` freed exactly one cluster - correct
only while directories were single-cluster by construction - and would
have silently leaked extension clusters; fixed with a shared
`free_chain` helper that also deduplicated `rm`'s and `write_file`'s
identical existing loops. Confirmed organically on QEMU (the test
image's 512-byte clusters fill after 14 entries): 20 files in one
subdirectory, root-directory extension, content round-trip on an
extended-cluster entry, reboot persistence, `rmdir` of the two-cluster
directory, and freed-cluster reuse - zero aborts. See `CLAUDE.md`'s
"Directory extension" section.

## ESP directory renamed to `\EFI\ORBS\`, and a project logo

The ESP directory became `\EFI\ORBS\` (was `\EFI\OUROBORO\` - both
exist because the full 9-character project name exceeds FAT's 8.3
short-name limit and `fat32.rs` doesn't parse LFN entries; `ORBS` is
the tidier abbreviation). `loader.rs`'s `CONFIG_PATH`, the Makefile's
`esp` target, and current-state docs updated together; confirmed
booting on QEMU (including a real `exec /EFI/ORBS/SH.BIN`) and real
Parallels hardware. The project also gained a logo (`logo1.png`
source at the repo root; resized copies on the website and README,
plus a real favicon replacing the per-page emoji ones).

## xHCI multi-device support - every connected device enumerated, classified, and kept addressed

The keyboard driver's one-port/one-device/one-slot scope is lifted -
the named prerequisite for the USB mass-storage milestone. The
all-in-one `Device` struct split into controller-global state
(`Xhci`), per-device slots with their own EP0 rings and Output Device
Contexts from 4-entry pools (the two statics that were only safe
while the old scan abandoned every non-keyboard), and `KeyboardState`.
The scan enumerates every connected port, logs each device's
interface class/subclass/protocol (with a mass-storage class-`0x08`
callout), keeps non-keyboards addressed, activates the keyboard only
after the scan completes, and routes transfer events by slot ID +
endpoint DCI. Confirmed on a new QEMU three-device rig
(`make run-usb-multi`: keyboard + tablet + storage - the storage
stick classified exactly `0x08`/`0x06`/`0x50`, Bulk-Only Transport)
and on real Parallels hardware (virtual mouse + keyboard concurrently
addressed, typing clean). See `CLAUDE.md`'s "xHCI multi-device
support" section.

## Parallels disk diagnostic - no documented storage controller exists on this platform

A diagnostic round (no driver written - that was the point) settled
the Parallels disk question: with the VM's `sata:0` boot disk attached
and bootable, the PCI bus shows no storage controller of any kind, and
scratch disks deliberately attached as `scsi`/`lsi-sas` then
`lsi-spi` are equally invisible (`buslogic` is rejected by Parallels
itself as EFI-incompatible; `ide`/`scsi` are the only interfaces
prlctl offers ARM64 EFI VMs). All storage flows through a proprietary
non-PCI path - "implement a documented spec" is not available for any
attached-image disk there. The one documented lead left: USB mass
storage over the existing xHCI driver (a first check with a USB 2.0
stick found Parallels routes USB 2.0 passthrough to the EHCI
controller instead; a USB 3.x stick is the pending retry). A permanent
diagnostic improvement fell out: `pci::log_all_devices` returns its
inventory and `main.rs` re-prints it through the post-exit console,
since boot reaches the shell ~2 seconds after power-on - far too fast
to read the UEFI-console rendering. See `CLAUDE.md`'s "Parallels disk
diagnostic" section and the roadmap's "Disk on real Parallels
hardware" entry.

## Output redirection (`>`/`>>`) - pure shell-side, zero kernel changes

`cmd > file` (create/overwrite) and `cmd >> file` (append) work for
every builtin, composed entirely from the existing `fs_read_file`/
`fs_write_file` syscalls the same way `cp` was - no new syscall, no
kernel changes. `run_line` (`shell/src/main.rs`) peels a trailing
redirect off the line before dispatch; command output flows through an
explicit `Output` sink passed down to handlers (error messages
deliberately stay on the console - the POSIX stdout/stderr split);
`>>` is shell-side read-concatenate-rewrite, bounded by the kernel's
512-byte per-syscall buffer cap. That cap found a real bug in testing:
a 1024-byte append buffer failed `valid_user_range` in a way
indistinguishable from "no such file", silently turning append into
overwrite - fixed by sizing the buffer at the cap. Confirmed on QEMU
(overwrite/append/create-empty, reboot persistence, both overflow
refusals - one organically, via six consecutive appends - and every
error case, zero aborts) and on real Parallels hardware (the `NO_FS`
path; typing `>` there needed a real `test-parallels.sh` extension - a
held-Shift scancode chord via `prlctl`'s `--event press/release`).
See `CLAUDE.md`'s "Output redirection" section and
`shell-commands.md`'s user-facing reference.

## Keyboard input routing: one designated owner task, not first-blocked-wins

Found live by the user immediately after `exec` shipped: with two
concurrent shells, keystrokes split unpredictably between them
(`uptime` arriving as `ptime` + a stray `u`), because `on_tick`'s
wake-check polled every `Blocked(Keyboard)` task in index order and
the underlying poll destructively consumes a byte for whoever asks
first. Fixed with `INPUT_OWNER_TASK` (hardcoded to task 0, the
boot-loaded shell - always valid since no task destruction exists):
the wake-check now skips keyboard polling for every other task, which
simply stays blocked, a genuine background task rather than a second
terminal racing the first. Confirmed by re-running the exact live QEMU
scenario that exposed it, plus a real-Parallels regression pass (this
wake-check is the only keyboard path there). See `CLAUDE.md`'s
"Keyboard input routing" section.

## Dynamic task creation and `exec` - a real `spawn` syscall

A new `spawn` syscall (16) loads a program from disk at runtime and
starts it as an independent task alongside the caller (deliberately
spawn semantics, not POSIX exec-replaces-current-process, though the
shell command is named `exec`). Needed: a runtime physical-page bump
allocator (nothing could hand out RAM after boot services exited), a
re-callable `mmu::install_identity_map` (stashed memory map +
extra-device list, `rebuild_with_el0_regions`), the scheduler grown
from 2 fixed slots to 4 (`TaskState::Unused`), and `loader.rs`'s ELF
core split (`elf_region_size`/`populate_region`) so the same parsing
serves boot-time and runtime loads. A real bug found by testing:
`Vec`-based program-header parsing *hangs silently* when first reached
from the runtime path - the global allocator is boot-services-backed
and invalid after exit, and misuse doesn't fault, it just hangs -
fixed with a fixed-capacity array. Confirmed on QEMU end to end (a
second shell instance genuinely running concurrently); on real
Parallels hardware only the error path is reachable (no disk driver
exists there at all - pre-existing gap), confirmed clean. See
`CLAUDE.md`'s "Dynamic task creation and exec()" section and
`architecture.md`'s process-model section.

## Blocking primitives - tasks can really wait, and a second SVC-frame bug

`READ_CHAR` (15) is the first genuinely blocking syscall: instead of
the shell busy-polling `try_read_char` every scheduler slice, the task
is suspended (`TaskState::Blocked(WaitReason::Keyboard)`) and
`on_tick`'s wake-check resumes it with the byte already in `x0` once
one arrives. Deliberately never executes `wfe` anywhere (real
Parallels hardware has a confirmed unresolved hang for EL0 `wfe`).
Testing this immediately exposed a second real, pre-existing
`exceptions.rs` bug (after the relocating-loader milestone's `x9`
clobber): the SVC trampoline's saved frame didn't match `Context`'s
real field layout and never saved `SP_EL0` at all - harmless while
every syscall resumed its own caller, fatal the moment a blocking
syscall loaded a *different* task's context through it. Fixed to match
the IRQ trampoline's proven layout. Confirmed on QEMU (a real
4-second block with zero wake events, then instant wake on input) and
on real Parallels hardware. See `CLAUDE.md`'s "Blocking primitives"
section.

## A real relocating loader - and a pre-existing SVC-trampoline bug it surfaced along the way

**The goal:** replace the flat, position-*dependent* userland-program
loader with a real ELF64 loader that parses `PT_LOAD` segments and
processes `R_AARCH64_RELATIVE` self-relocations against wherever a
program actually loads - the documented fix (`roadmap.md`) for this
project's single most-repeated bug class: `core::fmt`'s argument-
dispatch table and slice/literal comparisons both crashing for the
identical reason (an absolute data pointer baked in for a link-time
base of `0x0` that never matches the real runtime load address).

**Delivered:** `kernel/src/loader.rs` now hand-rolls a real ELF64
parser (header, program headers, section headers, `Elf64_Rela`
entries) and applies every `R_AARCH64_RELATIVE` relocation it finds in
`.rela.dyn` against the real load address; `LoadedProgram` gained a
real `entry` field, used by `tasks.rs` instead of assuming it equals
`base`. `shell`'s toolchain switched to `relocation-model=pic` + `-pie`
+ `--no-dynamic-linker` (`.cargo/config.toml`), `shell/linker.ld` gained
`.rela.dyn`/`.dynsym`/`.dynamic`/`.data.rel.ro` output sections, and the
Makefile's `shell-bin` target now hardcodes `--release` for userland
program builds - a real, confirmed toolchain constraint, not a style
choice (a debug build fails to *link* at all, due to an
`R_AARCH64_ABS64` relocation inside prebuilt `libcore`'s own object
code). The shell gained a permanent `selftest` builtin proving both
previously-crashing patterns - `write!`/`core::fmt` and a slice-vs-
literal comparison - now work correctly.

**A second, genuinely important finding along the way:** a real,
pre-existing bug in `exceptions.rs`'s SVC trampoline, latent since the
syscall boundary was first built and never triggered before this
milestone's different register allocation happened to expose it - the
EC check at the top of the SVC vector slot clobbers `x9` *before* the
trampoline's own save sequence gets a chance to preserve it, silently
discarding whatever value userland had live in `x9` at the moment of
`svc`. Root-caused via a direct register-survival probe (raw inline
`asm!`, not guessed) and fixed by saving `x9` to a scratch stack slot
before the EC check runs.

**Confirmed working end to end** via the same piped-stdin QEMU
technique as every prior milestone, against both `make run`'s FAT16
vvfat and the real FAT32 `esp.img`: `selftest`'s three checks all pass;
`help`/`echo`/`uptime` (a genuinely multi-digit tick count, the exact
previously-crashing shape) all produce correct output; the full disk-
command surface round-trips correctly. Zero aborts in `-d int`
cross-checks. **Confirmed on real Parallels hardware too**, via `make
test-parallels`: `selftest`'s three checks all passed identically, and
`uptime` printed a genuine 4-digit tick count with no crash - direct
real-hardware confirmation of the `x9` fix specifically. Full writeup
in `CLAUDE.md`'s "A real relocating loader" section.

## xHCI busy-waits switched from iteration-bounded to time-bounded - a real bug the user found, not this project's own testing

**The goal:** none going in - this was a live bug report. Booting the
real `esp.hdd` normally in Parallels (a manually-launched, live VM
window - not `make test-parallels`, which drives Parallels' own
synthetic keyboard headlessly) produced `xhci: keyboard not available
(Command ring: timed out waiting for a completion event)`, something
none of this project's own real-hardware testing had ever hit.

**Delivered:** every busy-wait in `kernel/src/xhci.rs`
(`wait_command_completion`, `wait_transfer_event`, the port-scan loop,
`poll_until`) switched from a fixed iteration count (`POLL_ITERS`) to a
genuine wall-clock deadline, using the ARM generic timer's free-running
counter (`CNTPCT_EL0`/`CNTFRQ_EL0` - pure system-register reads,
needing no GIC or interrupts, the same property `timer.rs` already
relied on for its own timer setup). `timer.rs` gained a new
`pub(crate) fn now_ticks()` for this reuse.

**Root cause:** a fixed iteration count is only a valid stand-in for
real elapsed time if the host never stalls the guest's vCPU for any
real duration while it spins - untrue under a real hypervisor. The
strongest evidence: reproducing the bug a second time showed a
*different* xHCI command timing out than the first attempt (very early
port setup, then much later at what's almost certainly the interrupt
endpoint's `Configure Endpoint` command) - a pattern that indicts the
wait mechanism itself, not any one command's logic. The most likely
trigger: a live-rendered VM window competing for real host CPU/GPU
time, something none of this project's own headless scripted testing
ever does.

**Confirmed fixed by the user directly**, on the exact real-world
scenario that originally failed - not a scripted reproduction. Also
regression-tested on QEMU and via this project's own `make
test-parallels`, neither of which had ever reproduced the original bug
but both confirm no regression to the already-working path. Full
writeup in `CLAUDE.md`'s "xHCI's busy-waits were iteration-bounded"
section.

## MADT/GICv3: real interrupt-controller discovery for Parallels - full preemptive multitasking confirmed working end to end

**The goal:** replace `gic.rs`'s old QEMU-devicetree-derived GICv2
addresses (already confirmed unsafe on real Parallels hardware, see
"take five" in `CLAUDE.md`) with real ACPI MADT discovery, and add a
GICv3 driver so Parallels - which almost certainly runs GICv3, not
GICv2 - can actually reach it.

**Delivered:** new `kernel/src/madt.rs` (MADT parsing: GICD/GICC/GICR
structures, cross-checked against Linux's `actbl2.h`), `kernel/src/gicv3.rs`
(a real GICv3 backend - system-register CPU interface, per-CPU
redistributor discovery and wake-up), and `kernel/src/gic.rs` turned
into a version-dispatch facade over `gicv2.rs`/`gicv3.rs` so `main.rs`/
`exceptions.rs`'s call sites barely changed. Confirmed on QEMU two
independent ways (a devicetree dump and the real MADT parse agreeing
exactly, both for default GICv2 and a newly forced `-machine
virt,gic-version=3`, `make run-gicv3`) before ever risking real
hardware. Two real GICv3 bugs found and fixed on QEMU first - a PPI
defaulting to Group 0 (FIQ) instead of Group 1 (IRQ) without an
explicit `GICR_IGROUPR0` write, and `GICD_CTLR` needing more than
GICv2's single enable bit - both cross-checked against Linux's own
`gic_cpu_init`/`gic_dist_init`.

**On real Parallels hardware:** MADT discovery confirmed clean and safe
(`GIC V3, GICD @ 0x2410000, GICC/GICR @ 0x2500000`, genuinely different
addresses from QEMU's - resolving whether Parallels' MADT describes an
interrupt controller at all, previously an open question given its
absent SPCR). GIC/timer IRQ delivery itself is conclusively confirmed
working there too - a real, correctly-incrementing `uptime`
(`533` -> `752` ticks in one observed run), isolated via a
single-variable diagnostic (temporarily skipping just the task-switch
call while leaving GIC/timer otherwise fully active).

**A second, separate, real bug found in the process - root-caused and
fixed the same session.** The actual task switch (`tasks::on_tick`)
hung the system outright the first time it ran on real hardware - this
exact interrupt-delivery-plus-context-swap combination had never
executed on real hardware before (Parallels had no working GIC/timer at
all until this milestone). A single-variable diagnostic isolated it to
the task switch specifically, not GIC/timer delivery (confirmed solid
on its own). Leading suspect: task 1's idle loop, `wfe` - real
hardware's `wfe` may be trapped/emulated by the host hypervisor in a
way QEMU/TCG's never is. Swapping the idle loop for a plain busy-spin
and re-testing confirmed it: the hang was completely gone, verified by
a sustained interactive test with a correctly, continuously
incrementing tick count throughout. Task switching is now
unconditionally enabled on every platform - preemptive multitasking
works on real Parallels hardware for the first time ever, with zero
change to QEMU's behavior (retested end to end there too, both GIC
versions, no regressions). A real, secondary, minor finding along the
way - also root-caused and fixed the same day: an occasional dropped
keystroke under active task switching, traced to a genuine logic bug in
`xhci.rs::Device::poll_key` (not a hypervisor timing quirk) - a single
polled report can legitimately carry more than one newly-pressed
keycode at once, and the original code only ever translated and
returned the first one, silently discarding any second forever. Fixed
with a small `pending` buffer draining every qualifying keycode from a
report. Confirmed fixed on real Parallels hardware: ten consecutive
`uptime` invocations back to back, zero drops. Full writeup in
`CLAUDE.md`'s "MADT/GICv3" section.

**Made practical by a new discovery the same day:** Parallels Desktop's
own CLI, `prlctl`, can script an entire real-hardware test round trip
(boot, type via `send-key-event`, screenshot via `capture`) with no
human watching the VM live - now `make test-parallels`
(`scripts/test-parallels.sh`). Every real-hardware round trip in this
milestone went through it. See `roadmap.md`'s "Testing infrastructure"
section.

## USB HID keyboard driver: confirmed - real, physical keyboard input on real Parallels hardware, first time ever

**The goal:** a real input path for Parallels, closing the gap the GOP
framebuffer console milestone left open (write-only, no keyboard driver
at all). A from-scratch xHCI driver (`kernel/src/xhci.rs`): capability/
operational register bring-up, command ring, event ring, device slot
enable/address over control transfers, and - the mechanism that turned
out to actually be required - a real interrupt IN transfer ring.

**Five independently-confirmed real-hardware bugs, none visible on
QEMU**, each found by direct evidence rather than guessing:

1. A PCI Command register bit-position error - `CMD_MEMORY_SPACE` was
   `1 << 0` (I/O Space Enable) instead of `1 << 1` (Memory Space Enable),
   found by decoding an observed before/after register dump
   (`0x0010 -> 0x0015`) that showed I/O Space + Bus Master set, never
   Memory Space. Explained every earlier "nothing responds" symptom on
   both QEMU and real hardware at once.
2. A genuine firmware panic on real hardware -
   `PANIC@11.28 UEFI-exception-ArmPciCpuIo2Dxe.dll`, decoded from
   Parallels' own hypervisor crash log - from a PCI config-space
   BAR-reassignment write (a standard technique, needed to work around
   QEMU's own OVMF build leaving this device's BAR completely
   unassigned) that real PCIe firmware doesn't tolerate the way QEMU's
   software model does. Fixed by never writing to PCI config space
   beyond the one narrow Command-register enable.
3. The discovered BAR (`0x8000004000` on QEMU) landed outside the
   identity map's original single-L0-table-entry span (a real
   simplifying assumption from early in this project, never revisited
   until a real PCI BAR needed an address outside the first 512GB).
   Fixed by generalizing `mmu.rs` to allocate further top-level table
   entries on demand.
4. The deepest finding: Parallels' USB passthrough doesn't forward HID
   *class* requests (`SET_PROTOCOL`, `GET_REPORT`) to the real device at
   all - confirmed by a live, correct `GET_DESCRIPTOR` *standard*
   request returning Parallels' own real registered USB vendor ID
   (`0x203a`) right next to a `GET_REPORT` that kept echoing this
   driver's own Setup packet back (byte-for-byte, tracked exactly across
   a changed request length - not a coincidence). Fixed by switching to
   a real interrupt endpoint, armed via the standard `Configure Endpoint`
   xHCI *command* (not a class request), the same mechanism every
   production USB HID driver actually uses at runtime.
5. Once the interrupt endpoint was delivering real live data, it turned
   out to be reading Parallels' virtual *mouse*, not the keyboard - this
   driver's port scan had just grabbed the first connected device.
   Fixed by scanning every connected port and checking each device's
   actual HID interface protocol (`bInterfaceProtocol=1`, Keyboard)
   before configuring it.

**Confirmed working end to end on real Parallels hardware, not just
QEMU:** typed a full command line (`abc`), used backspace, pressed
Enter, got the shell's real `unknown command` response - the complete
keyboard-to-shell round trip, on a real physical USB keyboard.

Full technical write-up - including the debugging techniques that found
each bug (poisoned DMA buffers, widening a request to break a suspicious
size coincidence, using a known-good standard request as a control,
decoding raw exception registers by hand) - in
[`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md), written to
be useful to other bare-metal-OS developers hitting the same class of
problem, not just as this project's own history.

**Still coarse, worth knowing before building on this:** one port, one
device, one slot, no hot-plug, no hubs, no real HID report-descriptor
parsing (boot-protocol's fixed 8-byte layout assumed directly, and
`SET_PROTOCOL` almost certainly still isn't reaching the device either -
this only works because the real keyboard happens to use the same
simple layout regardless), no stall recovery on the interrupt endpoint
specifically (EP0's setup-time control transfers do recover from a
Stall), only the first matching interrupt IN endpoint is ever
configured. Preemptive multitasking is still unavailable on Parallels
(a separate, already-tracked gap - see `roadmap.md`).

## GOP framebuffer console: confirmed - a real, working shell prompt on real Parallels hardware, first time ever

**The goal:** with the broadened `qemu_device_region_safe` gate in
place, the user tested a fifth time. Success - the complete predicted
sequence reached on real hardware with no further issues:
`framebuffer console live` → `skipping virtio-blk` → `skipping
GIC/timer init` → `shell ready` → the userland shell's own banner and a
live `$` prompt.

This is the first time this project has ever reached a running,
prompt-displaying shell on real Parallels hardware - the actual goal
the whole console-discovery effort (devicetree/ACPI/PCI, then
virtio-console, then five rounds of GOP-framebuffer-console hardware
testing) was for. Confirmed working on real hardware, not just QEMU:
GOP discovery/mapping, direct-write display visibility with no flush
needed, `fbconsole.rs`'s actual text rendering, the exception handler
reporting through it, the MMU identity map, and EL0 task entry.

**What's not working: keyboard input, a separate, already-known gap.**
The framebuffer console is write-only by design - this kernel has no
keyboard driver at all. The shell is genuinely running, but nothing
typed reaches it yet. A real input path needs a keyboard driver (USB
HID over UEFI, most likely - Parallels' own PCI inventory already shows
real USB controllers present). Preemptive multitasking also isn't
available on Parallels yet (the safety gate disables GIC/timer setup
there), needing real interrupt-controller discovery (ACPI MADT, likely
GICv3) to fix - a separate, substantial follow-up.

## GOP framebuffer console, round four: the GIC crashes too - broadened the safety gate beyond virtio-mmio

**The goal:** with the `virtio_mmio_probe_safe` gate in place, the user
tested a fourth time. `skipping virtio-blk` printed cleanly - that fix
worked - but the boot halted again with a second exception.

- Decoded the same way as the previous crash: `esr_el1=0x96000050`
  gives EC `0x25` (Data Abort, same EL) and DFSC `0x10` (Synchronous
  External Abort - a real bus fault), same signature as before, but
  `far_el1=0x8000000` this time - exactly `gic.rs`'s `GICD_BASE`,
  specifically `gic::init()`'s very first write.
- **A structural finding, not a second instance of the same bug:**
  `gic.rs`'s addresses are the identical kind of QEMU-shaped convention
  as `virtio_mmio.rs`'s - confirmed only via a QEMU devicetree dump,
  never discovered on Parallels. Nothing in this project's fixed
  low-1GB device-region convention has ever been confirmed safe on
  Parallels, only on QEMU.
- One real exception: `timer.rs` is pure system-register access
  (`cntfrq_el0` etc.), architecturally safe on any ARMv8 CPU regardless
  of platform device layout - only the GIC, needed to forward the
  timer's interrupt, depends on the unconfirmed address.
- **Fix:** renamed the gate `qemu_device_region_safe` (from
  `virtio_mmio_probe_safe`) and broadened it to also cover
  `gic::init()`/`gic::enable_interrupt()`, skipped together with
  `timer::arm()` as one block when unsafe. Verified `tasks::start()`
  has no dependency on GIC/timer having run (a straight `eret` into
  task 0's saved context) before shipping this, not assumed - skipping
  this block still reaches a working interactive shell, just without
  preemption (`uptime` would report a static count in this mode, a
  real, minor, documented limitation).
- Re-verified on QEMU, now checking ticks genuinely increase (88 → 214
  a few seconds later) to confirm the normal path still works, not just
  "didn't crash." A forced `discovery=None` + `ramfb` test (the actual
  Parallels shape) showed the complete intended sequence end to end:
  framebuffer console live → clean virtio-blk skip → clean GIC/timer
  skip → shell ready → a real working userland shell prompt. Zero
  aborts. Not yet re-confirmed on real Parallels hardware.

## GOP framebuffer console, round three: it renders real text on Parallels, and directly confirmed the virtio_mmio crash

**The goal:** with the console-fallback reorder in place, the user
tested a third time. Real progress - genuinely readable kernel text
rendered live on real Parallels hardware after `exit_boot_services`, a
first for this project - but the boot still halted, this time with an
exception report as the last thing printed.

- The rendered exception (`EXCEPTION vector=4 esr_el1=0x96000010
  far_el1=0xa000000 elr_el1=0xbba94c68`) decodes to EC `0x25` (Data
  Abort, same EL) with DFSC `0x10` (Synchronous External abort - a real
  bus fault) at `FAR_EL1 = virtio_mmio::SLOT_BASE` exactly - direct,
  decoded proof (not just a strong suspicion) that the virtio-mmio
  magic-value scan crashes real Parallels hardware on its very first
  read.
- The freeze this time was total, not just a lost console: `init_storage()`
  runs unconditionally after the console fallback chain, and
  `virtio_blk::Device::discover()` goes through the identical
  `virtio_mmio::find_device` scan - so the console reorder alone wasn't
  enough, the disk driver's own use of the same unsafe scan was always
  going to hit the same wall moments later.
- **Fix:** a new `virtio_mmio_probe_safe` flag (`true` only when a
  byte-stream console was found via devicetree/ACPI/PCI - the one
  platform this scan has ever been confirmed safe on) now gates *every*
  caller of `virtio_mmio::find_device`, both `try_virtio_console` and
  virtio-blk discovery. `virtio_mmio.rs`'s doc comments were corrected -
  the old claim that unbacked reads "read as 0" on real hardware was
  never confirmed, and is now confirmed wrong.
- Re-verified on QEMU across three scenarios: the normal ACPI-console
  regression pass (virtio-blk still initializes normally, confirmed
  against both FAT16 vvfat and a real FAT32 image with working `ls`/
  `cat`), and a forced `discovery=None` + `ramfb` test simulating the
  actual Parallels shape end to end - framebuffer console live, a clean
  "skipping virtio-blk" message, and a fully working interactive shell
  prompt. Zero aborts across all three. Not yet re-confirmed on real
  Parallels hardware.

## GOP framebuffer console, round two: display-write visibility confirmed, virtio-console MMIO scan reordered out of the way

**The goal:** with the `open_protocol_exclusive` fix in place, the user
tested again on real Parallels hardware. Progress - GOP discovery now
succeeds (`GOP framebuffer @ 0x20000000, 1024x768, stride=1024,
format=Bgr`) and boot reaches further than before - but the screen
still froze, with no framebuffer console output ever appearing.

- **A temporary raw full-framebuffer fill diagnostic** (`0xff` to every
  byte, placed right after the MMU switch, bypassing `fbconsole.rs`/
  `font.rs` entirely) isolated the question that mattered most: does a
  direct write to this physical address actually reach the display at
  all, given that a real GPU might need an explicit flush/present step
  unlike QEMU's `ramfb`. Verified visible on `ramfb` first (solid white
  via QMP screendump), then handed to the user for real hardware.
- **Result: solid white screen on real Parallels hardware** - confirming
  the MMU mapping and direct-write display visibility both work, no
  flush needed. But the screen stayed solid white, frozen - meaning
  execution never reached the actual framebuffer-console rendering.
- **Root cause: `try_virtio_console()` ran immediately afterward, and
  its MMIO scan carries an unconfirmed assumption** -
  `virtio_mmio.rs`'s `find_device` reads a magic-value register at 32
  fixed QEMU-specific addresses, with a comment asserting (never
  actually confirmed) that an unpopulated slot "reads as 0" on real
  hardware. A real bus fabric commonly raises an external abort for a
  read to genuinely unbacked device-memory space instead. With no
  console installed yet at that point on Parallels, a fault there is
  completely silent.
- **Fix: reordered the fallback chain** so the now-hardware-validated
  framebuffer console is tried before `try_virtio_console`, not after -
  justified by independent evidence (`pci.rs`'s device inventory) that
  virtio-console doesn't exist on Parallels at all, so demoting it costs
  nothing there. Re-verified on QEMU: the forced-fallback `ramfb` test
  and the normal ACPI-console regression pass are both unchanged. Zero
  aborts. Not yet re-confirmed on real Parallels hardware.

## GOP framebuffer console fix: `open_protocol_exclusive` was disconnecting Parallels' own boot console

**The goal:** the user tested the framebuffer console (below) on real
Parallels hardware and it still didn't work - a screenshot showed
devicetree/ACPI/PCI discovery and the PCI device dump rendering
normally, then nothing further, not even `framebuffer::discover()`'s own
unconditional success/failure log line.

- Root cause, found by reading the `uefi` crate's own doc comment rather
  than guessing: `framebuffer.rs` opened `GraphicsOutput` with
  `open_protocol_exclusive`, which is specified to forcibly disconnect
  any driver holding the protocol `ByDriver` (the crate's own example:
  "opening the SERIAL_IO_PROTOCOL exclusively will disconnect the
  console driver from it"). On real Parallels hardware, firmware's own
  text console holds GOP `ByDriver` to render the boot screen - so the
  very first call into `discover()` silently killed the visible console
  before anything else could print. QEMU's `ramfb` test never caught
  this because it always ran with `-display none`, so there was no
  console driver attached to GOP to disconnect.
- **Fix:** switched to `OpenProtocolAttributes::GetProtocol` via the
  `unsafe` generic `uefi::boot::open_protocol` - a read-only, non-owning
  open, safe here since `discover()` only reads `ModeInfo`/`FrameBuffer`
  once and never touches the `GraphicsOutput` object again.
- Re-verified end to end on QEMU after the fix: GOP discovery still
  succeeds identically, the forced-fallback screendump still renders
  boot messages and the shell's banner/prompt correctly, and a full
  regression pass on the normal ACPI-console dev loop is unchanged. Zero
  aborts. **Not yet re-confirmed on real Parallels hardware** - that
  remains the next step.

## GOP framebuffer console: a fifth, better-grounded lead for Parallels output

**The goal:** find a real answer for Parallels console output after
virtio-console (below) was confirmed a dead end there. Prompted by the
user asking for research into how other OSes (Linux/FreeBSD) handle a
console on Parallels ARM64, given that Linux is known to run there.

- Deep research (a forked subagent, independently re-verified rather
  than trusted wholesale) turned up two claims. One - a specific PCI
  device ID mapping for Parallels' proprietary "ToolGate" mechanism -
  turned out to be unsourced by its own citation and was discarded. The
  other held up under direct verification: a FreeBSD forum thread
  (https://forums.freebsd.org/threads/parallels-on-macos-apple-silicon-freebsd-14-stuck-on-virtio_gpu.96762/)
  shows a real FreeBSD boot log on Parallels ARM64 with `VT: Replacing
  driver 'efifb' with new 'virtio_gpu'` - direct confirmation that a
  generic UEFI GOP framebuffer (`efifb`) drives early console output on
  that exact platform, and that Parallels' own VM config only offers two
  "video type" options (a proprietary GPU or VirtIO GPU) with no serial
  option at all. This matches this project's own Parallels boot
  screenshots, which already show UEFI graphics output working.
- **New module: `kernel/src/framebuffer.rs`** - discovers
  `EFI_GRAPHICS_OUTPUT_PROTOCOL` (a standard, fully-specified UEFI
  protocol, unlike every previous console mechanism this project has had
  to guess an address or convention for) during boot services, returning
  the framebuffer's physical base/size, resolution, stride, and pixel
  format. Only `Rgb`/`Bgr` (4 bytes/pixel, fixed known layout) are
  supported - `Bitmask`/`BltOnly` are rejected outright, since `BltOnly`
  specifically has no direct-memory-access path usable after
  `exit_boot_services` at all.
- **New module: `kernel/src/font.rs`** - a public-domain 8x8 bitmap font
  (`dhepper/font8x8` on GitHub, itself based on Marcel Sondaar/IBM's
  public-domain VGA fonts), downloaded verbatim via `curl` rather than a
  summarizing fetch specifically to avoid silent transcription errors in
  hex glyph data, then mechanically sliced down to the 95 printable-ASCII
  glyphs (0x20-0x7E) and embedded as a `const` array.
- **New module: `kernel/src/fbconsole.rs`** - a `Write`-implementing text
  console over the raw framebuffer: draws glyphs on a fixed character-cell
  grid, tracks a cursor, and scrolls via a raw `ptr::copy` pixel-row
  memmove directly in the framebuffer (deliberately no text buffer -
  matches this kernel's zero-heap discipline). Write-only: no keyboard
  driver exists in this kernel at all yet, a real gap independent of this
  console.
- **`console.rs` gained a `Framebuffer` variant** and an `is_installed()`
  accessor, so `main.rs` can gate the framebuffer console as a genuine
  last resort - tried only once devicetree/ACPI/PCI *and* virtio-console
  have all failed, since a real byte-stream console (which also gets
  input) is strictly more capable whenever one exists.
- **`mmu.rs::install_identity_map` gained an optional `framebuffer`
  argument.** Most of the time this needs no new mapping at all - on
  QEMU's `ramfb` device, the framebuffer address already falls inside the
  discovered RAM span, so the existing RAM loop covers it for free. Only
  if a framebuffer's containing 1GB block is *still* unmapped after that
  loop does this add one more Device-nGnRnE block for it (same
  convention as the existing fixed low-1GB device block, just at
  whatever address the framebuffer actually reports) - a real
  possibility on hardware this has never been tested against, not yet
  confirmed either way.
- **QEMU testing needed a real display device, and `-nographic` doesn't
  provide one at all** (confirmed by direct testing: `NoGop`). Verified
  instead with `-device ramfb -display none -serial file:...` (a
  RAM-backed, direct-access framebuffer QEMU builds specifically for
  headless use) plus a QMP `screendump` HMP command to capture the
  rendered output as a `.ppm` image for visual inspection - a new
  verification technique for this project, since every prior console
  driver could be checked through its own text output alone.
  `virtio-gpu-pci` was also tried and confirmed to report `BltOnly` under
  this QEMU/OVMF combination - a real, confirmed case of `discover()`'s
  pixel-format rejection actually triggering, not just a theoretical
  branch.
- **Confirmed working, not just "renders something":** a boot-time
  screendump (with a temporary `if true` override, matching this
  project's established technique for testing a fallback that QEMU's own
  ACPI console would otherwise always win over first) showed correctly
  rendered kernel boot messages and the loaded shell program's own
  banner/prompt - proving both `console::println!` and the shell's
  userland `putc` syscall reach the framebuffer, not just kernel-side
  text. A second run added a temporary 90-line print loop to force the
  scroll path: the screendump showed a clean, correctly-ordered scrolled
  view with no corruption or ghosting. Both temporary overrides were
  reverted before committing. Zero aborts in `-d int` cross-checks across
  both runs. A full regression pass on the normal QEMU dev-loop config
  (`-nographic`, real ACPI/PL011 console, piped-stdin `help`/`uptime`)
  confirmed no change in behavior when a byte-stream console exists -
  GOP is still discovered and logged, but the framebuffer console never
  installs, exactly as designed.
- **Still coarse, worth knowing before building on this:** no colour, no
  ANSI escape parsing (the shell's `clear` command's escape sequence just
  draws junk glyphs here instead of clearing the screen); no keyboard
  input at all (this console is write-only, and this kernel has no
  keyboard driver of any kind yet - a gap independent of this milestone);
  the MMU's device-block fallback path for a framebuffer outside the
  discovered RAM span has never been exercised against real hardware,
  only reasoned about; and confirmation against actual Parallels hardware
  is still pending - this was built and verified against QEMU's `ramfb`
  only, the same "confirmed on QEMU, awaiting real-hardware confirmation"
  posture every other console mechanism in this project has gone through
  first.

## Parallels console: virtio-console confirmed not applicable, with real hardware evidence

**The goal:** resolve the one open question the virtio-console milestone
(below) left for the user - does `try_virtio_console` actually find and
drive a device on real Parallels hardware. The user booted the real
`esp.hdd` on real Parallels-on-Apple-Silicon hardware and reported back;
this entry covers what that testing found and the diagnostic built to
make sense of it.

- Real Parallels boot confirmed devicetree/ACPI/PCI 16550 all fail
  exactly as already documented, and showed no visible output at all
  afterward - inherently ambiguous on its own, since the UEFI graphics
  console only renders during boot services, so "the driver ran and
  found nothing" and "the driver ran and hung" look identical on that
  screen.
- The user's separately configured Parallels "Serial Port" device
  (output to a file) received nothing at all, not even boot-firmware
  noise - unlike QEMU, where EDK2 opportunistically mirrors its own
  debug output onto any attached virtio-serial chardev. Inconclusive on
  its own (this firmware build might just not do that), so it ruled
  nothing in or out.
- **New permanent diagnostic: `pci::log_all_devices`
  (`kernel/src/pci.rs`)**, reusing the same boot-services
  `PciRootBridgeIo` walk `discover_uart16550` already proved safe on
  this hardware (reaching `NoSerialDevice` there, not `NoRootBridge`,
  was already proof the walk itself works). Logs every PCI device's
  vendor:device and class:subclass through the still-working pre-exit
  UEFI console, whenever all three normal console-discovery mechanisms
  fail - cheap, read-only, and adds no noise to the normal QEMU boot
  path.
- **The real Parallels PCI inventory this produced**: an Intel HD Audio
  controller, an Intel USB2 controller, an NEC USB3 controller, a
  virtio device (vendor `0x1af4`) with device ID `0x1000` -
  **virtio-net, not virtio-console** (which would be device ID `0x1003`
  or `0x1043`, neither present) - and one unclassified device under
  Parallels' own PCI vendor ID (`0x1ab8`, device `0x4000`, class
  `0xff`).
- **Conclusion: Parallels' serial port is very likely a proprietary
  Parallels device, not virtio-console at all** - no public
  specification exists for it. Reverse-engineering it (blind
  register/BAR probing against an undocumented protocol) was considered
  and explicitly declined - an open-ended task with no guaranteed
  payoff, categorically different from implementing a documented spec.
  A deliberate stopping point, not an assumption.
- A second, smaller finding from the same round of testing: the
  `esp.hdd` "opens as a folder" attachment concern that prompted this
  investigation was never a real problem - the user's Parallels version
  just labels the file-open dialog's button "Open" instead of "Choose."
  Confirmed by the same boot log successfully reading `INIT.CFG`/the
  shell binary off the real disk.

## virtio-console — a real transmit-only driver, confirmed on QEMU

**The goal:** the real lead flagged (and deliberately deferred) for
Parallels console output — `kernel/src/virtio_console.rs`, a fourth
console-discovery mechanism reusing `virtio_mmio.rs`'s existing
transport, modeled directly on `virtio_blk.rs`'s discover/init/one-
virtqueue/poll-based-completion shape.

- Discovery, feature negotiation (`VIRTIO_F_VERSION_1` only), and
  transmitq0 (queue 1) setup, mirroring `virtio_blk.rs` closely.
  Receiveq0 (queue 0) deliberately left unconfigured - transmit-only,
  matching the plan this milestone started from.
- New `Console::Virtio` variant: `write_str` batches through a local
  buffer with `\n`->`\r\n` translation and sends chunked virtqueue
  transfers (a full virtqueue round trip per `Device::write` call makes
  per-byte sends too expensive for whole log lines); `write_byte` sends
  one byte per call (used for the userland shell's character-at-a-time
  output); `read_byte` always returns `None` - no receive path exists.
- **A real placement constraint found while building this, not assumed
  going in:** unlike devicetree/ACPI/PCI (which install their console
  immediately after `exit_boot_services`), virtio-mmio discovery needs
  the device region mapped under this kernel's *own* translation tables,
  which only happens after `mmu::install_identity_map`. `try_virtio_console`
  in `main.rs` therefore runs later than the other three - a real,
  accepted consequence: boot messages between `exit_boot_services` and
  there are lost on a virtio-console-only platform.
- **Verified end to end on QEMU**, using the same kind of temporary,
  documented source-level force this project used to originally verify
  `exceptions.rs` (QEMU's default ACPI-first boot never organically
  reaches this fallback): discovery, feature negotiation, virtqueue
  setup, and real bytes reaching the host chardev all confirmed, `\r\n`
  bytes verified in the raw output via `xxd`, and the normal (unforced)
  boot path re-confirmed unaffected afterward. New `make
  run-virtio-console` Makefile target for future testing. Zero aborts in
  `-d int` cross-checks.
- **A genuine surprise along the way:** `-machine virt,acpi=off`, tried
  first as a way to organically force all three existing mechanisms to
  fail, instead made *devicetree* discovery succeed (this OVMF build
  apparently advertises a DTB when ACPI is disabled) - a real, useful
  finding about this specific firmware configuration, not the forcing
  mechanism actually needed here.
- **Parallels itself remains unconfirmed** - this environment has no way
  to boot real Parallels hardware. Whether Parallels exposes its
  console via virtio-mmio at all (vs. virtio-pci), and at the same
  address range this QEMU-confirmed driver assumes, is a real open
  question only a real Parallels boot can answer.
- **Still transmit-only, no RX**, deliberately - a receive virtqueue
  (symmetrically simpler than transmit) is the natural next step once
  Parallels itself is confirmed reachable this way.

## Phase 8 — `mv`, and the last real correctness risk in the write-support arc

**The goal:** `mv <src> <dst>`, the last cheap command-level win left in
the "close up the easy write-support commands" arc (phases 4-7) before
moving on to bigger work.

- `Fs::mv` reuses the file/directory's *existing* cluster chain rather
  than reading and rewriting content: locate `src`'s entry, insert a new
  entry for `dst` with the same cluster/size/kind, then free `src`'s old
  entry - same "write the new thing before touching the old one"
  ordering as `write_file`'s overwrite path, so a failure partway
  through (most likely `Error::DirectoryFull` on `dst`'s parent) never
  leaves `src` half-deleted. `dst` must not already exist - `mv` refuses
  rather than overwriting it or moving `src` inside it if `dst` happens
  to be an existing directory, narrower than a real `mv`'s full
  semantics.
- **The one real correctness risk, caught by design rather than by a
  crash:** when a *directory* moves to a *different* parent, its own
  `..` entry has to be patched to point at the new parent (or cluster
  `0`, root's convention, if the new parent is root) - otherwise a
  moved directory's `cd ..` would keep resolving to its *old* parent
  forever, silently. This is the identical cluster-`0`-means-root
  convention from phase 3c's `Fs::find`, reapplied on a write path
  rather than reintroduced as a new bug. Reused `patch_entry_cluster_size`
  (phase 6) to make the fix, rather than writing a new low-level sector
  patcher for it.
- One new syscall, `fs_mv` (14, `(src ptr, src len, dst ptr, dst len)`),
  and a new `mv <src> <dst>` shell builtin - the cheapest of the write
  commands to add on the shell side, since it's a single syscall call
  with no buffer/content handling at all, unlike `cp`.
- **Confirmed working end to end, with the `..`-fixup specifically
  exercised, not just reasoned about:** a same-directory rename; a
  cross-directory move of a plain file; a cross-directory move of a
  *directory* containing its own subdirectory, followed immediately by
  `cd`-ing into the moved directory and back out with `cd ..` - which
  correctly landed at the *new* parent, not the old one; renaming a
  directory in place (same parent) and confirming a doubly-moved nested
  directory's own `..` still resolved correctly afterward; destination-
  already-exists, missing-source, and missing-destination-parent all
  correctly rejected with the source left untouched. Persistence
  confirmed by an actual reboot - all of the above state, including the
  moved/renamed directory tree, was still correct on a fresh mount.
  `make run` (FAT16, no mount) still degrades gracefully. Zero aborts in
  `-d int` cross-checks across every session.
- **Still coarse:** no move-into-an-existing-directory-keeping-basename
  semantics (real `mv`'s most common shortcut); no cycle detection (`mv`
  a directory into its own descendant isn't guarded against); same
  8.3-short-name constraints as `mkdir`/`touch` apply to `dst`'s name.

## Phase 7 — `cp`, pure shell-side plumbing over existing syscalls

**The goal:** `cp <src> <dst>`, the next item phase 6's own changelog
entry and the roadmap parking lot both flagged as newly buildable now
that files can hold real content.

- **No new syscall, no kernel changes at all.** `cmd_cp` reads `src`'s
  entire content into a local stack buffer via the existing
  `fs_read_file` (the same syscall `cat` already uses), then writes
  that buffer to `dst` via the existing `fs_write_file` (phase 6) -
  creating `dst` if it doesn't exist, replacing it if it does, same
  semantics as `write`. Pure shell-side composition of two primitives
  that were already there.
- **Copying a file onto itself is safe by construction, not a special
  case.** The read completes in full, into the shell's own buffer,
  before the write ever starts - there's no window where a partial
  write could clobber a read still in progress.
- **Refuses rather than silently truncates when a source file is too
  large for the shell's read buffer** — a genuinely different judgment
  call than `cat`'s, which prints a truncated prefix with a notice.
  `cat` displaying an incomplete file is merely incomplete; `cp`
  producing an incomplete copy would be a *wrong* copy, so it errors
  outright instead.
- Confirmed working end to end against the real `esp.img`: copy to a
  new destination and to an existing one (overwrite), copy a file onto
  itself (content unchanged), copy a real pre-existing file
  (`INIT.CFG`) to a new name, copy into a subdirectory, and the usual
  error set (missing source, destination is a directory, missing
  destination parent) all correctly handled. `make run` (FAT16, no
  mount) still degrades gracefully. Zero aborts in `-d int`
  cross-checks. No new persistence check needed beyond what phase 6
  already established for `fs_read_file`/`fs_write_file` themselves -
  `cp` adds no new on-disk code path, only a new caller of two already
  reboot-tested ones.
- **Still coarse:** copy size is bounded by the shell's read buffer
  (256 bytes, same as `cat`'s); no recursive directory copy, no `-r`
  flag or any flags at all; no `mv`.

## Phase 6 — `write`, the first way to put real content into a file

**The goal:** close the gap phase 5 left open — every file this kernel
could create was permanently zero bytes, since `touch` only ever
produces empty files. `write_file` is the "actual blocker for `cp`,
output redirection, and anything else that needs a file to hold more
than zero bytes" that phase 5's changelog entry and the roadmap parking
lot both flagged.

- `Fs::write_file` creates a file with exactly the given content, or
  fully replaces (not appends to) an existing file's content. Ordering
  matters: the new cluster chain is allocated and written *before*
  anything about an existing file is touched (freeing its old chain,
  patching its directory entry), so a failure partway through never
  frees or unlinks a file that was already there.
- Two new private helpers: `write_chain` (allocates and writes a fresh
  cluster chain for arbitrary-length data, linking each cluster to the
  next via `write_fat_entry` as it goes) and
  `patch_entry_cluster_size` (rewrites just an existing entry's cluster
  and size fields in place, leaving name/attribute/timestamps alone —
  what makes overwriting different from creating).
- One new syscall, `fs_write_file` (13, `(path ptr, path len, data ptr,
  data len)`), and a new `write <file> <words...>` shell builtin that
  joins the remaining words with spaces (same style as `echo`) and
  writes the result as the file's entire content.
- **A real bug found immediately by testing, not by inspection:**
  `write <file>` with no content words (a legitimate "truncate to
  empty" case, exactly matching `touch`'s own empty-file semantics)
  failed with a generic disk error instead of succeeding. Root cause:
  the syscall's argument-sanity check (`valid_user_range`) rejected any
  zero-length buffer, correct for `fs_list_dir`/`fs_read_file`'s output
  buffers (a zero-length destination is pointless there) but wrong for
  `fs_write_file`'s *input* data, where empty is a real, meaningful
  value. Fixed with a second check, `valid_user_range_allow_empty`,
  used only for the data argument (the path argument still can't be
  empty).
- Confirmed working end to end against the real `esp.img`: create a
  file with content, `cat` shows it exactly; overwrite with shorter
  content, `cat` shows only the new content (no stale trailing bytes
  from the longer original); the empty-write fix specifically
  re-tested and confirmed fixed; writing to an existing directory or a
  missing parent both correctly rejected; a genuine reboot-and-remount
  persistence check (write, reboot, `cat` still shows the content); no
  corruption of pre-existing files; `make run` (FAT16, no mount) still
  degrades gracefully. Zero aborts in `-d int` cross-checks across
  every session.
- **Still coarse:** every write is a full replace, no append and no
  partial/offset writes; content is bounded by whatever the caller can
  fit in one buffer (the shell's `write` command specifically is capped
  by its 128-byte input line, so it can only ever produce a single
  FAT32 cluster's worth of content on this test image — `rm`'s
  multi-cluster cluster-chain-freeing loop, added in phase 5, remains
  logically implemented and reasoned-through but still not exercised by
  an end-to-end test, since nothing in this kernel can yet create a
  file spanning more than one cluster). No `cp`, no output redirection
  yet — those need shell-side plumbing this syscall alone doesn't
  provide (see `roadmap.md`'s parking lot).

## Phase 5 — file lifecycle: `touch`/`rm`

**The goal:** round out phase 4's directory-only write support with the
file equivalent — create and remove files, not just directories.

- `Fs::touch` turned out simpler than `Fs::mkdir`: a real FAT32 empty
  file needs no allocated cluster at all (a directory entry with
  starting cluster `0` and size `0` is a valid empty file per spec), so
  it's just one `insert_dir_entry` call, no `find_free_cluster`/
  `zero_cluster`/`.`/`..` writes. Succeeds as a no-op if the file already
  exists (no RTC to update a modification time with, so "no-op" is the
  honest approximation of real `touch`'s behavior there).
- `Fs::rm` mirrors `Fs::rmdir`'s shape, plus one thing `rmdir` never
  needed: freeing the target's entire cluster chain (a loop over
  `next_cluster`/`write_fat_entry`) before freeing its directory entry -
  a no-op for an empty file, but real for anything with content once a
  future "write file contents" syscall exists.
- Two new syscalls, `fs_touch`/`fs_rm` (11/12, path pointer/length only),
  and matching `touch`/`rm` shell builtins.
- **A latent bug caught by reasoning, not by testing:** `Fs::find`'s
  cluster-`0`-means-root substitution (from phase 3c) applied to *every*
  resolved entry, not just directories - harmless before this milestone
  (nothing but a `..` entry ever had cluster `0`), but `touch` was about
  to make "cluster `0`, and it's a file" common. Fixed by gating the
  substitution on `is_dir` before writing any test that could have hit
  it. See `CLAUDE.md`'s "Phase 5" section for the full story.
- **Still no way to write content into a file** - `touch` only ever
  produces zero-byte files. `cp`/output redirection need a real
  "write file contents" syscall this project doesn't have yet.
- Confirmed working end to end against the real `esp.img`, including the
  same reboot-and-remount persistence check as phase 4, `rm`/`rmdir`
  correctly refusing each other's target (a directory vs. a file), and
  no corruption of pre-existing files.

## A shared syscall-ABI crate

Every "known rough edges"/"next milestone" note since phase 3c had
flagged the same thing: syscall numbers and sentinel values were
hand-duplicated between `kernel/src/syscall.rs`'s dispatch table and
`shell/src/main.rs`'s caller, kept in sync only by convention. Fixed
with a third workspace member, `syscall-abi/` — a plain `#![no_std]`
library crate holding nothing but `pub const` syscall numbers and
sentinel values, no logic. Both `kernel` and `shell` now depend on it
via a path dependency and reference constants directly
(`syscall_abi::FS_MKDIR`, etc.) instead of local, independently-numbered
consts — a future syscall added on one side with the wrong number
literally fails to compile on the other rather than silently
misbehaving at runtime. Confirmed safe against this project's specific
relocation risk (every value is a scalar `u64` const, inlined as an
immediate at the use site under both targets — fundamentally different
from the `core::fmt`/slice-literal-comparison bug, which is specifically
about pointers to literal `.rodata` data). See `CLAUDE.md`'s "A shared
syscall-ABI crate" section for the full story.

Also folded in during the same stretch of work: a real UX bug where
every `fs_*` syscall collapsed "no filesystem is mounted this boot" and
"the filesystem is mounted but this operation failed" into the same
`u64::MAX` sentinel, making every disk command on `make run`'s FAT16
disk look identical to a genuinely broken path. Fixed by splitting the
sentinel (`NO_FS`, `u64::MAX - 1`, distinct from `FS_ERROR`) so the
shell can print an explicit "no filesystem mounted" message instead —
see `CLAUDE.md`'s "Phase 4" section addendum.

## Phase 4 — first filesystem write support: `mkdir`/`rmdir`

**The goal:** cross the write-support line phase 3 deliberately drew, for
the narrowest useful case — creating and removing empty directories — not
the full write surface (`rm`/`touch`/`cp`/`mv`/redirection) at once.

- `virtio_blk::Device` gained `write_sector`, sharing a `submit_request`
  helper with `read_sector` (the only real difference between the two
  requests is the data descriptor's write-flag direction and the
  request-type field).
- `fat32.rs` gained `write_fat_entry` (keeps every FAT copy in sync, not
  just the first — reads only ever needed the first), `find_free_cluster`,
  `zero_cluster`, and `write_raw_entry`, plus `Fs::mkdir`/`Fs::rmdir`
  themselves.
- Two new syscalls, `fs_mkdir`/`fs_rmdir` (9/10, path pointer/length
  only), and matching `mkdir`/`rmdir` shell builtins.
- **Deliberately narrow, not a full write implementation:** no
  directory-extension (a full parent directory makes `mkdir` fail rather
  than growing it), no file creation/deletion, no `cp`/`mv`, and a
  conservative 8.3 short-name character set (ASCII alphanumerics plus
  `_`/`-` only) for names this kernel creates.
- Confirmed working end to end against the real `esp.img`, including a
  genuine reboot-and-remount persistence check (not just a live
  in-memory one), root-removal and already-exists rejection, and
  no corruption of pre-existing files. See `CLAUDE.md`'s "Phase 4"
  section for the full story, including the on-disk write ordering each
  operation follows and why it's ordered that way (claim-before-use for
  `mkdir`'s cluster allocation, check-everything-before-writing-anything
  for `rmdir`).

## Phase 3 — a fully functional shell with disk commands

**The goal:** `ls`, `cat`, `cd`, `pwd` — a shell that can actually browse
and read the filesystem it's running from, not just talk to the console.

**Why this is bigger than phase 2, and can't be shortcut:** every disk
read so far happens during the UEFI boot-services window, before
`exit_boot_services` — a one-shot, one-way door. There is no way to reach
back into UEFI's filesystem protocol once the shell is actually running;
"disk commands" by definition means reading files *after* boot, which
means finally building the runtime storage stack that's been deliberately
deferred twice already (once in the phase-1 shell milestone, again in the
disk-loading milestone) rather than a shortcut on top of what exists. This
phase is really three dependent stages:

### 3a. A real block device driver

- **Transport: virtio-mmio, confirmed and deliberately chosen over
  QEMU's own default.** Addresses confirmed via the same devicetree-dump
  technique as GICv2/the timer (32 slots, `0xa000000`, `0x200` apart).
  Worth recording since it wasn't the obvious path: a plain
  `-drive ...,media=disk` with no `if=`/`-device` actually auto-attaches
  as **virtio-blk-pci**, not virtio-mmio — reaching that at runtime would
  need this kernel's own PCI/ECAM config-space walk (a real subsystem on
  its own, comparable to writing PCI enumeration a second time, since the
  existing `pci.rs` is boot-services-only). The Makefile now attaches the
  drive as virtio-mmio explicitly instead, sidestepping that entirely.
  Modern (non-legacy) register interface, also deliberately chosen and
  verified via direct QEMU-monitor memory peeks before any driver code
  was written — see `CLAUDE.md`'s "Phase 3a" section for the full story,
  including a real bug in the diagnostic process itself (a truncated
  monitor read that briefly looked like "no block device exists at all").
  Parallels' own virtio-mmio behavior is still unconfirmed — same open
  question already on record for virtio-console.
- **Driver scope, as built:** device discovery, feature negotiation (just
  `VIRTIO_F_VERSION_1`), one virtqueue, synchronous polling block reads.
  Write support stayed out of scope, as planned at the time (see phase
  4, above).
- Confirmed working end to end: reads sector 0 back and checks the real
  MBR boot signature, not just "no error returned." See
  `kernel/src/virtio_mmio.rs`/`kernel/src/virtio_blk.rs`.
- **Kernel-resident, not a user-space driver process — a deliberate,
  explicit choice, not an oversight.** `docs/research-minix-boot.md`
  raised a real fork here: writing virtio-blk (and eventually
  virtio-console) as an isolated EL0 driver process would be a concrete
  step toward this project's stated microkernel goal, using the driver as
  the forcing function for dynamic task creation and real IPC. Decided
  against, for now — that would pull the process-model/IPC work (see
  `roadmap.md`'s parking lot) into phase 3's critical path, and phase 3's
  actual goal is disk commands working at all. Revisit once there's more
  than one reason to want driver isolation; virtio itself doesn't
  require it.

### 3b. A filesystem reader

- **Target format: FAT32, as planned** — matches `make image`'s
  `hdiutil -fs FAT32` output, what Parallels ultimately boots from too.
  Real surprise along the way: `make run`'s fast dev-loop disk
  (`fat:rw:esp`, QEMU's `vvfat`) turned out to be **FAT16**, confirmed by
  decoding its BPB by hand before writing any parser code (`BS_FilSysType`
  literally reads `"FAT16   "`) — `make run-image` (Makefile target)
  boots the real `esp.img` instead, since `run`'s disk can never satisfy
  a FAT32 mount. See `CLAUDE.md`'s "Phase 3b" section for the full story.
- **Decided: hand-rolled, not a crate** — turned out to be more than a
  style preference: this reader runs after `exit_boot_services`, where
  the global allocator is no longer valid, and every `no_std` FAT crate
  surveyed assumes an allocator is reachable somewhere in its stack. A
  hard constraint, not just precedent.
- **A second real surprise, since resolved:** this project's own
  `\EFI\OUROBOROS\` directory name didn't fit FAT's 8.3 short-name limit
  (9 characters) — real FAT32 formatters handle that with a long-filename
  (LFN) entry this reader doesn't parse. Resolved at the start of 3c by
  renaming the directory to `\EFI\OUROBORO\` (8 characters) rather than
  implementing LFN parsing — this project controls the name, so once 3c's
  actual need (the shell navigating there) was concrete, renaming was the
  cheaper, more honest fix. `fat32.rs` still has no LFN support in
  general; any *other* 9+ character name is still unreachable.
- Confirmed working end to end: lists `\EFI\BOOT`, reads `BOOTAA64.EFI`
  back (a real multi-cluster file, not a single-block special case) and
  checks its exact size and PE header magic against the real built
  binary. See `kernel/src/fat32.rs`.

### 3c. New syscalls + the commands themselves

- **Syscall shape, as built:** `fs_list_dir`/`fs_read_file` (7/8), each
  taking `(path ptr, path len, buf ptr, buf len)` and writing into the
  caller's buffer directly, rather than the originally-sketched
  `open_dir`/`read_dir`/`open_file`/`read_file`/`close` handle-based
  shape — simpler, and sufficient since nothing here needs a persistent
  open handle across multiple calls. This is also what pushed the
  syscall ABI itself from 1 argument to 4 (`x0`-`x3`) — see `CLAUDE.md`'s
  "Phase 3c" section.
- **All four commands built, all read-only, matching the original
  target:** `ls [path]` (`fs_list_dir`), `cat <file>` (`fs_read_file`),
  `pwd` (shell-local `cwd` state, no syscall), `cd <path>` (shell-local
  state + `fs_list_dir` for validation, no dedicated "exists" syscall).
- **Two real bugs found by testing the actual commands, not by
  inspection** - see `CLAUDE.md`'s "Phase 3c" section for the full
  writeup: a slice/string-vs-literal comparison (`cwd_bytes != b"/"`)
  crashed for the same underlying reason `core::fmt` does (a data
  reference computed for link-time base `0x0`), and a FAT32 `..`-entry
  convention (cluster `0` means "root," not root's real cluster number)
  that wasn't handled hung the *entire system* - masked-IRQ syscall
  context, nothing left to preempt a runaway computation. Both fixed;
  `shell/src/main.rs` also gained path normalization (`normalize_path`)
  as a direct result, collapsing `.`/`..` instead of letting `cwd`
  accumulate them literally.

**Deliberately out of scope at the time** (later picked up as phases
4/5, or still open — see `roadmap.md`): any write support (`mkdir`,
`rm`, `touch`, `cp`, `mv`, output redirection — writing to a real
filesystem is a meaningfully bigger risk than reading, and doesn't block
anything read-only commands need); `stat`/`file`/`find`/`tree`; wiring
`loader.rs` itself to use the new runtime driver instead of
boot-services file I/O (boot-time loading already works and has no
reason to change).

## Phase 2 — real commands

`help`, `echo`, `uptime`, `clear`, with `uptime` backed by a real new
syscall (`get_ticks`) rather than being another echo demo.

## Phase 1.5 — the shell becomes a real process

Moved from a kernel-compiled EL0 blob to a genuine separate program,
loaded from disk at boot and selected by a config file — see
[`processes.md`](processes.md). Unplanned at the start of phase 1, but a
prerequisite for phase 2 meaning anything more than kernel demos.

## Phase 1 — interactive echo shell

Real UART input, a line editor, live character echo with backspace/DEL
handling.

## Phase 0 — boot infrastructure

UEFI entry, console discovery (devicetree/ACPI/PCI), exception vectors,
a real MMU identity map, GICv2 + timer-driven preemption, the syscall
boundary, and two-task preemptive round-robin scheduling. See
[`architecture.md`](architecture.md).

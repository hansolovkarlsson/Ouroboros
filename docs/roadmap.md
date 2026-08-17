# Ouroboros roadmap

Forward-looking plan — what's next and why, in plan form rather than
chronological narrative. For completed milestones, see
[`CHANGELOG.md`](CHANGELOG.md); for *how* something already built
actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see `CLAUDE.md`. This document is the one to
update first when direction changes; the others describe what exists,
this one describes where it's going.

## Testing infrastructure: scripted real-hardware round trips

Every real-hardware bug in `xhci-keyboard-postmortem.md` and
`boot-bringup-postmortem.md` cost a manual round trip: rebuild, re-image,
boot Parallels, watch the screen, type on a physical keyboard, report
back. `make test-parallels` (`scripts/test-parallels.sh`) closes that gap
using `prlctl`, Parallels Desktop's own CLI (`man prlctl`) — discovered
2026-08-16, not something this project had used before. It rebuilds
`esp.hdd`, boots the registered VM headlessly, types a `;`-separated list
of shell commands via `prlctl send-key-event` (real decimal PS/2 Set-1
scancodes — `prlctl` rejects hex), and saves a screenshot
(`prlctl capture`) after each one, all with no human watching the VM
live. Confirmed working end to end: `help`/`echo hi`/`uptime` all
produced correct, readable output in the captured screenshots, including
the `xhci::report` debug lines showing genuine HID reports reaching the
same interrupt-endpoint code path the physical-keyboard postmortem is
about (`send-key-event` drives Parallels' own synthetic keyboard device,
not that specific physical one — a real distinction, though the code
path it exercises is the same one).

This doesn't replace real-physical-hardware confirmation for anything
USB-passthrough-specific (the xHCI postmortem's bugs 1-5 needed the real
device), but for everything else — does a shell command still work after
a change, did a fix regress the boot sequence — this turns what used to
be a human-paced manual check into something that can run unattended and
be reviewed after the fact from the saved screenshots.

## Parking lot (known future work, not yet sequenced)

Pulled from `docs/processes.md`'s "known rough edges" and `CLAUDE.md`'s
running "next milestone" notes — real gaps, not yet a committed next
phase:

- ~~xHCI keyboard failing outright on a real, manually-launched
  Parallels VM~~ — **done, found and fixed the same day, by the user
  directly** (not by any of this project's own scripted testing, which
  never reproduced it — see `docs/roadmap.md`'s "Testing infrastructure"
  section for why: `make test-parallels` drives Parallels' own synthetic
  keyboard headlessly, with no live-rendered VM window competing for
  host CPU/GPU time). Root cause: every busy-wait in `xhci.rs` was
  bounded by a fixed iteration count, not real elapsed time — a real
  hypervisor can stall the guest's vCPU for a genuine, unpredictable
  duration (e.g. while actually rendering a live VM window on screen)
  without an iteration count reflecting that at all. Fixed by switching
  every wait to a genuine wall-clock deadline using the ARM generic
  timer's free-running counter (`CNTPCT_EL0`/`CNTFRQ_EL0`, pure
  system-register reads, no GIC or interrupts needed — the same
  property `timer.rs` already relies on). Confirmed fixed by the user
  on the exact real-world scenario that originally failed. See
  `CLAUDE.md`'s "xHCI's busy-waits were iteration-bounded" section for
  the full writeup.
- ~~Diagnose the real-Parallels task-switch hang~~ — **done, same day it
  was found.** Root cause traced (well enough to fix) to task 1's idle
  loop using `wfe` — real hardware's `wfe` semantics under Apple's
  virtualization layer are the leading (still not fully proven)
  explanation. Confirmed fixed by swapping the idle loop for a plain
  busy-spin (`nop; b 1b`, see `tasks.rs`'s `el0_idle_template` doc
  comment) and re-testing: a sustained real-hardware interactive session
  showed a correctly, continuously incrementing tick count (`644` ->
  `1210` in one observed run) with no hang. Task switching is now
  unconditionally enabled again (the temporary `TASK_SWITCH_ENABLED`
  gate was removed entirely) — preemptive multitasking works on real
  Parallels hardware for the first time ever. A real, secondary, minor
  finding along the way, also root-caused and fixed the same day: an
  occasional dropped keystroke under active task switching, traced to a
  genuine logic bug in `xhci.rs::Device::poll_key` (not a hypervisor
  timing quirk) - a single polled report can legitimately carry more
  than one newly-pressed keycode at once, and the original code only
  ever translated and returned the first one, silently discarding any
  second one forever. Fixed with a small `pending` buffer draining every
  qualifying keycode from a report instead of just the first. Confirmed
  fixed on real Parallels hardware: ten consecutive `uptime` invocations
  back to back, zero drops.
- ~~Confirm virtio-console on real Parallels hardware~~ — done, and the
  answer is no. Tested on real hardware: a full PCI device inventory
  (`pci::log_all_devices`, kept as a permanent diagnostic) shows no
  virtio-console device over PCI, and no direct evidence of one over
  MMIO either. Parallels' actual serial port is very likely a
  proprietary device (PCI vendor `0x1ab8`, no public spec) - see
  `CLAUDE.md`'s "virtio-console" section. Reverse-engineering that
  device was considered and explicitly declined (open-ended, no
  guaranteed payoff); revisit only if real documentation or driver
  source for it ever surfaces. The virtio-console *driver* itself stays
  in the tree, confirmed working on QEMU - just not the answer for
  Parallels.
- ~~Build a GOP framebuffer console (the real lead after virtio-console)~~
  — done, **and confirmed reaching a real, working shell prompt live on
  real Parallels hardware** ("take six" - `framebuffer.rs`/`font.rs`/
  `fbconsole.rs`, see `CLAUDE.md`/`CHANGELOG.md`). Five real-Parallels
  test rounds, each finding and fixing one real bug: `open_protocol_exclusive`
  disconnecting firmware's own boot console from GOP; `try_virtio_console`'s
  MMIO scan freezing the boot with no console yet installed to report
  through (a reorder got a console installed, and that console then
  rendered the exception that led to the real fix); a genuine bus fault
  in `virtio_mmio::find_device`'s scan, decoded directly from that
  rendered exception, fixed with a safety gate covering every caller of
  that scan; and a *second*, differently-addressed instance of the
  identical fault at `gic.rs`'s `GICD_BASE` - proof the entire fixed
  low-1GB QEMU-shaped device-region convention (not just virtio-mmio) is
  unsafe on Parallels, fixed by broadening the same gate
  (`qemu_device_region_safe`) to cover GIC/timer setup too. **This is
  now done** - and the console's write-only limitation is also now done
  (see the USB HID keyboard driver item below). What was left from this
  era - no preemption on Parallels - is also now done, see the
  "Preemption on Parallels" item below.
- ~~A USB HID keyboard driver~~ — **done, and confirmed with a real,
  physical keyboard typing a full command line (with backspace) into the
  shell on real Parallels-on-Apple-Silicon hardware.** A genuinely large
  addition, as flagged when this was scoped: a from-scratch xHCI driver
  (`kernel/src/xhci.rs`) covering capability/operational register
  programming, the command ring, the event ring, device slot enable/
  address, control transfers, and - the mechanism that turned out to
  actually be required, see below - a real interrupt IN transfer ring.
  Five independently-confirmed real-hardware bugs along the way, none
  visible on QEMU: a PCI Command register bit-position error (Memory
  Space Enable is bit 1, not bit 0 - explained both the QEMU dev-loop
  quirk below and the real-hardware failure); a firmware panic
  (`PANIC@11.28 UEFI-exception-ArmPciCpuIo2Dxe.dll`, decoded from
  Parallels' own hypervisor crash log) from a PCI config-space
  BAR-reassignment write that real firmware doesn't tolerate the way
  QEMU's does; the discovered BAR landing outside the identity map's
  original single-L0-table-entry span, fixed by generalizing
  `mmu.rs` to allocate further top-level table entries on demand; the
  deepest finding, that Parallels' USB passthrough doesn't forward HID
  *class* requests (`SET_PROTOCOL`, `GET_REPORT`) to the real device at
  all (confirmed via a live, correct `GET_DESCRIPTOR` *standard* request
  returning Parallels' own real registered USB vendor ID, `0x203a`,
  right next to a `GET_REPORT` that kept echoing this driver's own Setup
  packet back) - fixed by using a real interrupt endpoint (armed via the
  standard `Configure Endpoint` xHCI *command*, not a class request) the
  way every production USB HID driver actually works at runtime, instead
  of polling `GET_REPORT`; and, once that interrupt endpoint was
  delivering real live data, discovering it was reading Parallels'
  virtual *mouse*, not the keyboard - fixed by scanning every connected
  port and checking each device's actual HID interface protocol
  (`bInterfaceProtocol=1`, Keyboard) before configuring it. Full
  technical write-up, including the debugging techniques that found each
  bug, in [`xhci-keyboard-postmortem.md`](xhci-keyboard-postmortem.md) -
  written to be useful to other bare-metal-OS developers hitting the
  same class of problem, not just this project's own history.
  **Still coarse, worth knowing before building on this:** one port, one
  device, one slot (**update: lifted** - see the multi-device entry
  further below; every connected device is now enumerated, classified,
  and kept concurrently addressed), no hot-plug, no hubs, no real HID
  report-descriptor parsing (boot-protocol's fixed 8-byte layout assumed
  directly), no stall recovery on the interrupt endpoint specifically,
  only the first interrupt IN endpoint on a matching interface is ever
  configured.
- ~~Preemption on Parallels~~ — **fully done.** Real ACPI MADT discovery
  (`kernel/src/madt.rs`) replaced the old heuristic for GIC/timer setup,
  a GICv3 driver (`kernel/src/gicv3.rs`) is confirmed working on real
  Parallels hardware, and the task-switch hang found in the process
  (see the parking-lot item above) is fixed too - real, preemptive,
  two-task round-robin multitasking now works end to end on real
  Parallels hardware, confirmed by a sustained interactive test with a
  genuinely, correctly incrementing `uptime` throughout. See
  `CLAUDE.md`'s "MADT/GICv3" section for the full writeup, including the
  two real GICv3 bugs found and fixed on QEMU first (`GICR_IGROUPR0`
  Group-1 assignment, `GICD_CTLR`'s multi-bit enable) before ever
  risking a Parallels round trip on them, and the task-switch fix
  itself (idle task `wfe` -> busy-spin) - root-caused well enough to
  fix, not fully proven why real hardware's `wfe` didn't work as
  expected.
- ~~**Output redirection (`>`/`>>`)** — needs shell-level parsing this
  project doesn't have yet (splitting `cmd > file` into a command and a
  target). `cp` is done (see below); redirection is the one piece of
  the original "output redirection (`>`/`>>`), and anything else that
  needs a file to hold more than zero bytes" item still open.~~
  **Done** — `> file` (create/overwrite) and `>> file` (append) work
  for every builtin, entirely shell-side over the existing
  `fs_read_file`/`fs_write_file` syscalls (zero kernel changes, same
  compose-what-exists approach as `cp`): `run_line` peels a trailing
  redirect off the line before dispatch, command output goes through an
  explicit `Output` sink passed down to the handlers (a capture buffer
  when redirecting; error messages deliberately stay on the console -
  the POSIX stdout/stderr split), and `>>` is read-concatenate-rewrite
  bounded by the kernel's 512-byte per-syscall cap. That cap found a
  real bug during testing: a 1024-byte append buffer failed the
  kernel's `valid_user_range` check in a way indistinguishable from
  "no such file", silently turning append into overwrite. Confirmed on
  QEMU (overwrite/append/create-empty/persistence-across-reboot/both
  overflow refusals/error cases, zero aborts) and on real Parallels
  hardware (the `NO_FS` path - no disk driver exists there - plus a
  `test-parallels.sh` extension typing `>` as a real held-Shift
  scancode chord). See `CLAUDE.md`'s "Output redirection" section and
  `docs/shell-commands.md`.
- **Lifting `mkdir`'s no-directory-extension limitation** - a full
  parent directory currently makes `mkdir` fail rather than growing it.
- ~~A real relocating loader (ELF + relocation processing) — would also
  lift the current `core::fmt`/`write!` restriction in userland
  programs.~~ **Done** — real ELF64 parsing and `R_AARCH64_RELATIVE`
  relocation processing, confirmed on both QEMU and real Parallels
  hardware (`core::fmt`/`write!` and slice/literal comparisons both work
  correctly now, via the shell's `selftest` command, on both platforms),
  plus a real pre-existing kernel bug this work surfaced and fixed (the
  SVC trampoline losing `x9` across every syscall — also confirmed fixed
  on real hardware via a genuinely multi-digit `uptime` value). See
  `CLAUDE.md`'s "A real relocating loader" section for the full writeup.
- ~~Blocking/waiting primitives, so tasks aren't limited to unconditional
  round-robin `wfe` polling.~~ **Done** — `tasks.rs` gained real
  `Runnable`/`Blocked(reason)` task state, a `block_current_and_switch`
  that suspends the calling task and switches to another runnable one
  mid-syscall (not via `wfe` - real Parallels hardware has a confirmed,
  unresolved hang when an EL0 task executes it, so this mechanism
  deliberately never does), and a per-tick wake-check. The shell's main
  loop now blocks on a real `read_char` syscall instead of busy-polling.
  Confirmed on QEMU and real Parallels hardware. A real, if incidental,
  bug surfaced and fixed along the way: the SVC trampoline's saved-frame
  layout never matched `Context`'s real field order (no `SP_EL0` slot at
  all, `ELR`/`SPSR` at the wrong offset) - harmless for every syscall
  before this one, since none needed the frame to be a fully
  interchangeable `Context`, but it directly corrupted the resumed
  task's `ELR_EL1` the first time one did. See `CLAUDE.md`'s "Blocking
  primitives" section for the full writeup.
- ~~Dynamic task creation and `exec()` — running more than one loaded
  program, or reloading one without a reboot.~~ **Done** — a new `spawn`
  syscall (16) loads a program from disk at runtime and starts it as a
  genuinely new, independent task alongside whatever's already running
  (`tasks::spawn`, not exec-replaces-current-process — the shell command
  is named `exec` to match this item's original wording, but nothing
  about the calling task is replaced). Needed a runtime physical-page
  bump allocator (nothing before this handed out RAM after boot
  services exited), `mmu::install_identity_map` made callable a second
  time (reusing the exact "swap the whole table set while code keeps
  running" mechanism already proven at boot, not a new incremental-remap
  primitive), and the scheduler grown from a fixed 2 slots to 4 with an
  `Unused` state for the two new ones. A real bug surfaced and fixed
  along the way: the ELF parser's `Vec`-based program-header parsing
  hung completely (no exception, no output) when called from this new
  runtime path, since the global allocator is boot-services-backed and
  invalid post-`exit_boot_services` — fixed with a fixed-capacity
  `[ProgramHeader; 16]` instead. Confirmed on QEMU (two shell instances
  alive concurrently, ticks still advancing, zero aborts) and on real
  Parallels hardware for every piece except the actual disk-load success
  path, which real hardware can't reach yet — a pre-existing,
  already-tracked gap (no working virtio-blk on Parallels at all, see
  below), not something this feature introduces. See `CLAUDE.md`'s
  "Dynamic task creation and `exec()`" section and
  `docs/architecture.md`'s "Dynamic task creation" section for the full
  writeup.
- **Disk on real Parallels hardware — diagnosed (2026-08-17), and the
  answer rules out every documented PCI storage controller.** A
  dedicated diagnostic round (see `CLAUDE.md`'s "Parallels disk
  diagnostic" section) confirmed with fresh evidence, not the old
  inventory alone: the VM's own boot disk is attached as `sata:0`, yet
  the PCI bus shows **no storage controller of any kind** — the same
  five devices as ever (audio, EHCI, xHCI, virtio-net, and Parallels'
  proprietary vendor-`0x1ab8` device). Deliberately attaching a scratch
  disk as `scsi` with subtype `lsi-sas`, then `lsi-spi` (`prlctl set
  --device-add hdd --iface scsi --subtype ...` — the only interfaces
  prlctl offers an ARM64 EFI VM are `ide` and `scsi`; `buslogic` is
  rejected by Parallels itself as EFI-incompatible) changed nothing:
  the inventory is byte-identical, the emulated controllers simply
  don't exist on Apple Silicon. Conclusion: *all* storage on this
  platform flows through a non-PCI/proprietary path (the `0x1ab8`
  device is the only candidate), so "implement a documented spec"
  is not available for any *attached-image* disk. **The one real
  documented-spec lead left: USB mass storage.** The xHCI controller is
  on the bus and this kernel already drives it end to end (the
  keyboard); a USB storage device passed through to the VM would be
  reachable via USB Mass Storage Bulk-Only Transport + SCSI commands
  over that same driver — a genuinely documented protocol stack,
  building on the project's own working xHCI code, at the cost of disk
  content living on a real USB stick rather than `esp.hdd`. Untested;
  needs a real passed-through USB storage device to even scope
  properly.
- **USB mass storage over xHCI — the follow-up to the diagnostic
  above, parked until it can be scoped against real hardware.**
  **First enumeration check run (2026-08-17), with a real finding: a
  USB 2.0 stick never appears on the xHCI controller at all.** A
  SanDisk Cruzer Glide (high-speed/USB 2.0, per Parallels' own device
  listing) was passed through via `prlsrvctl usb set` and confirmed
  `Connected-To-Vm: YES` while the VM ran — yet a temporary in-kernel
  diagnostic (dump every connected xHCI port at scan time, wait 6
  wall-clock seconds, dump again; removed after the answer) showed
  only the same two ports as always (virtual mouse, keyboard), before
  *and* after the wait. Conclusion: Parallels routes USB 2.0
  passthrough devices to the **EHCI (USB2) controller** — also on the
  PCI bus, but a whole separate host-controller driver this kernel
  doesn't have — not to xHCI. **Next concrete step: repeat the same
  zero-code check with a USB 3.x stick**, which should land on the
  xHCI root ports; only if that works does the driver plan below
  apply as written (the alternative — an EHCI driver just to reach
  USB 2.0 devices — is a second full HC bring-up, much worse value).
  If a 3.x stick enumerates,
  the driver work is: recognize a Mass Storage interface
  (`bInterfaceClass=0x08`, subclass `0x06` SCSI-transparent, protocol
  `0x50` Bulk-Only Transport) during the port scan the same way the
  keyboard's HID interface is recognized today, configure its bulk
  IN/OUT endpoint pair (the first non-interrupt endpoint type this
  driver would ever drive), and implement BOT's CBW/CSW framing around
  a minimal SCSI command set (`INQUIRY`, `READ CAPACITY(10)`,
  `READ(10)`, `WRITE(10)`) — then adapt `fat32.rs` to sit on a second
  block-device backend besides `virtio_blk::Device`.
  **Update (2026-08-17): the multi-device groundwork is done** — the
  known constraint this item named (one-port/one-device, keyboard and
  stick must coexist) is lifted: `xhci.rs`'s scan now enumerates every
  connected device into its own slot with per-device EP0 rings/output
  contexts, logs every interface's class/subclass/protocol (a
  passed-through stick's boot log now directly shows the mass-storage
  classification this item's scoping check needs, with an explicit
  class-`0x08` callout - already proven against QEMU's `usb-storage`
  via the new `make run-usb-multi` three-device rig, which showed
  exactly `0x08`/`0x06`/`0x50`), and keeps non-keyboard devices
  addressed, ready for a driver. Confirmed on real Parallels hardware
  (mouse + keyboard concurrently addressed, typing unregressed). See
  `CLAUDE.md`'s "xHCI multi-device support" section. Remaining
  constraints going in: everything stays polled, matching the rest of
  the kernel; no hot-plug (the stick must be attached before boot);
  no hubs.
- **Actual microkernel-style driver isolation** — moving drivers
  (starting with virtio-blk/virtio-console) out of the EL1 kernel and
  into supervised EL0 processes, per `docs/research-minix-boot.md`'s
  comparison (process-boundary isolation, MINIX's answer) and
  `docs/research-helix-os.md`'s (a trait-based mechanism/policy split
  inside one address space, a different real answer to the same
  question — see that note's "what this says about Ouroboros's current
  shape"). Dynamic task creation (above) is done; real IPC is still
  needed before this is worth starting.

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

- **Diagnose the real-Parallels task-switch hang** — new, found while
  closing the "Preemption on Parallels" item below. GIC/timer IRQ
  delivery is conclusively confirmed working on real Parallels hardware
  (a real, correctly-incrementing `uptime`, isolated via a
  single-variable diagnostic - see `CLAUDE.md`'s "MADT/GICv3" section),
  but the actual task switch (`tasks::on_tick`'s `Context` swap) hangs
  the system outright the very first time it runs there - no exception
  reported, keystrokes stop being echoed, indistinguishable from a dead
  machine from the framebuffer console's write-only view. Currently
  shipped disabled there (`exceptions.rs`'s `TASK_SWITCH_ENABLED`,
  gated the same way `virtio_mmio_probe_safe` is). Leading unconfirmed
  hypothesis: real hardware's `WFE` may be trapped/emulated by the host
  hypervisor above this guest kernel in a way QEMU/TCG's never is - this
  kernel's `nTWE`/`nTWI` bits only ever controlled the EL0->EL1 trap it
  owns itself. Not yet tested; needs real hardware round trips to
  investigate further.
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
  (see the USB HID keyboard driver item below). What's left from this
  era: no preemption on Parallels (needs real interrupt-controller
  discovery - ACPI MADT, likely GICv3 - a separate, substantial
  follow-up, tracked below).
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
  device, one slot, no hot-plug, no hubs, no real HID report-descriptor
  parsing (boot-protocol's fixed 8-byte layout assumed directly), no
  stall recovery on the interrupt endpoint specifically, only the first
  interrupt IN endpoint on a matching interface is ever configured.
- ~~Preemption on Parallels~~ — **half done.** Real ACPI MADT discovery
  (`kernel/src/madt.rs`) replaced the old heuristic for GIC/timer setup,
  and a GICv3 driver (`kernel/src/gicv3.rs`) is confirmed working on
  real Parallels hardware - a genuine, correctly-incrementing `uptime`
  there for the first time ever. But the *task switch* itself hangs the
  first time it runs on real hardware (a new, separate, unresolved bug -
  GIC/timer IRQ delivery is conclusively confirmed fine, isolated via a
  single-variable diagnostic), so preemptive multitasking specifically
  is still off on Parallels, gated behind `exceptions.rs`'s
  `TASK_SWITCH_ENABLED`. Leading unconfirmed hypothesis: real hardware's
  `WFE` may be trapped/emulated by the host hypervisor (Apple's own
  virtualization layer, above this guest kernel entirely) in a way
  QEMU/TCG's `WFE` never is - this kernel's `nTWE`/`nTWI` bits only ever
  controlled the EL0->EL1 trap it owns, not whatever EL2 does above
  that. See `CLAUDE.md`'s "MADT/GICv3" section for the full writeup,
  including the two real GICv3 bugs found and fixed on QEMU first
  (`GICR_IGROUPR0` Group-1 assignment, `GICD_CTLR`'s multi-bit enable)
  before ever risking a Parallels round trip on them. Real next step:
  diagnose the task-switch hang specifically - a genuinely new,
  real-hardware-only mystery, not a continuation of this item's original
  scope.
- **Output redirection (`>`/`>>`)** — needs shell-level parsing this
  project doesn't have yet (splitting `cmd > file` into a command and a
  target). `cp` is done (see below); redirection is the one piece of
  the original "output redirection (`>`/`>>`), and anything else that
  needs a file to hold more than zero bytes" item still open.
- **Lifting `mkdir`'s no-directory-extension limitation** - a full
  parent directory currently makes `mkdir` fail rather than growing it.
- A real relocating loader (ELF + relocation processing) — would also
  lift the current `core::fmt`/`write!` restriction in userland programs.
- Blocking/waiting primitives, so tasks aren't limited to unconditional
  round-robin `wfe` polling.
- Dynamic task creation and `exec()` — running more than one loaded
  program, or reloading one without a reboot.
- **Actual microkernel-style driver isolation** — moving drivers
  (starting with virtio-blk/virtio-console) out of the EL1 kernel and
  into supervised EL0 processes, per `docs/research-minix-boot.md`'s
  comparison (process-boundary isolation, MINIX's answer) and
  `docs/research-helix-os.md`'s (a trait-based mechanism/policy split
  inside one address space, a different real answer to the same
  question — see that note's "what this says about Ouroboros's current
  shape"). Explicitly deferred until there's more than one reason to
  want it: it needs dynamic task creation and real IPC first, not just
  a virtio driver.

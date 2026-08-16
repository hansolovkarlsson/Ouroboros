# Ouroboros roadmap

Forward-looking plan — what's next and why, in plan form rather than
chronological narrative. For completed milestones, see
[`CHANGELOG.md`](CHANGELOG.md); for *how* something already built
actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see `CLAUDE.md`. This document is the one to
update first when direction changes; the others describe what exists,
this one describes where it's going.

## Parking lot (known future work, not yet sequenced)

Pulled from `docs/processes.md`'s "known rough edges" and `CLAUDE.md`'s
running "next milestone" notes — real gaps, not yet a committed next
phase:

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
  now done** - what's left is two separate, already-known gaps, not more
  console debugging: no keyboard input on Parallels (the console is
  write-only, and this kernel has no keyboard driver at all - see the
  keyboard-driver item below, now the clear next step for Parallels
  specifically), and no preemption there either (needs real
  interrupt-controller discovery - ACPI MADT, likely GICv3 - a separate,
  substantial follow-up).
- **A USB HID keyboard driver — the clear next step for Parallels
  specifically, now that the console works but has no input path.**
  Research (two independent sources, not just one search result taken
  at face value) confirmed this is a well-grounded target, not a repeat
  of virtio-console's proprietary dead end: a real Parallels forum
  thread about Linux ARM64 guests shows actual `xhci_hcd` errors tied
  to the keyboard/mouse, on the exact same xHCI PCI device
  (`0x1033:0x0194`) `pci::log_all_devices` already found present on
  this hardware; and an independent USB-ID database confirms `VID_203A`
  ("PARALLELS Virtual Keyboard" in real guest `lsusb` output) is a
  genuine USB-IF-registered vendor ID belonging to Parallels itself -
  i.e. a standard, spec-compliant USB HID device over a standard
  controller, discoverable by any conformant driver, not an
  undocumented channel. **Genuinely large scope, larger than anything
  built so far** - xHCI controller init (capability/operational
  registers, device context array, command ring, event ring), device
  slot/address enumeration over control transfers, then HID
  boot-protocol keyboard reports on top, all polling-based rather than
  interrupt-driven (matches this project's existing driver style, and
  necessary anyway since Parallels has no working GIC yet - see above).
  Explicitly not started - the user chose to pause here after the
  console milestone rather than begin immediately given the size.
  Narrowest useful first slice, when picked back up: minimal xHCI init
  + one device enumerated + polling boot-protocol reports, no hot-plug,
  no EHCI, no full HID report-descriptor parsing - enough to type one
  character into the shell, same discipline as every driver in this
  project so far.
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

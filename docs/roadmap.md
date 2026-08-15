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

- **Confirm virtio-console on real Parallels hardware.** The driver
  (`kernel/src/virtio_console.rs`) is built and verified end to end on
  QEMU, but this environment can't boot real Parallels - the user needs
  to boot `esp.hdd` (`make parallels-hdd`) and report whether
  `try_virtio_console` finds a device there, and at the address range
  this driver assumes. If it doesn't, the open questions are whether
  Parallels even uses virtio-mmio transport for its console (vs.
  virtio-pci) and whether it places virtio-mmio slots at the same
  addresses QEMU does.
- **A receive virtqueue for `virtio_console.rs`** - transmit-only right
  now, so a Parallels boot using this fallback (once confirmed working)
  can be *watched* but not *typed into*. Symmetrically simpler than the
  transmit path already built: post device-writable buffers to
  receiveq0 (queue 0, currently left unconfigured), poll the used ring
  for arrivals.
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

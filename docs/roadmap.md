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

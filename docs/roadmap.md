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

- **A "write file contents" syscall** - the actual blocker for `cp`,
  output redirection (`>`/`>>`), and anything else that needs a file to
  hold more than zero bytes. Every file this kernel can create (`touch`)
  is permanently empty without this.
- **Lifting `mkdir`'s no-directory-extension limitation** - a full
  parent directory currently makes `mkdir` fail rather than growing it.
- `mv`/rename support.
- A real relocating loader (ELF + relocation processing) — would also
  lift the current `core::fmt`/`write!` restriction in userland programs.
- Blocking/waiting primitives, so tasks aren't limited to unconditional
  round-robin `wfe` polling.
- Dynamic task creation and `exec()` — running more than one loaded
  program, or reloading one without a reboot.
- Parallels virtio-console (console output on Parallels is still
  deliberately paused).
- **Actual microkernel-style driver isolation** — moving drivers
  (starting with virtio-blk/console once they exist) out of the EL1
  kernel and into supervised EL0 processes, per
  `docs/research-minix-boot.md`'s comparison. Explicitly deferred until
  there's more than one reason to want it: it needs dynamic task
  creation and real IPC first, not just a virtio driver.

# Ouroboros roadmap

Forward-looking milestone plan — what's done, what's next, and why, in
plan form rather than chronological narrative. For *how* something already
built actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see `CLAUDE.md`. This document is the one to update
first when direction changes; the others describe what exists, this one
describes where it's going.

## Done

- **Phase 0 — boot infrastructure.** UEFI entry, console discovery
  (devicetree/ACPI/PCI), exception vectors, a real MMU identity map, GICv2
  + timer-driven preemption, the syscall boundary, and two-task preemptive
  round-robin scheduling. See `architecture.md`.
- **Phase 1 — interactive echo shell.** Real UART input, a line editor,
  live character echo with backspace/DEL handling.
- **Phase 1.5 — the shell becomes a real process.** Moved from a
  kernel-compiled EL0 blob to a genuine separate program, loaded from disk
  at boot and selected by a config file — see `processes.md`. Unplanned at
  the start of phase 1, but a prerequisite for phase 2 meaning anything
  more than kernel demos.
- **Phase 2 — real commands.** `help`, `echo`, `uptime`, `clear`, with
  `uptime` backed by a real new syscall (`get_ticks`) rather than being
  another echo demo.

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

- **Transport:** virtio-mmio almost certainly, matching QEMU's `virt`
  machine (same reasoning already used for GICv2/the timer — confirmed via
  devicetree dump, not assumed) and plausibly Parallels too, per the
  virtio-console lead already recorded in `CLAUDE.md` (Parallels'
  Virtualization framework exposes storage over virtio as well as serial).
  Needs its own confirmation pass the same way GICv2's addresses were
  pinned down, not reused by assumption.
- **Driver scope:** device discovery, feature negotiation, one virtqueue,
  synchronous block read requests to start (write support is explicitly
  out of scope for this phase — see "Deliberately out of scope" below).
- This is genuinely comparable in size to the still-deferred
  Parallels virtio-console work — a real subsystem, not an afternoon.
- **Kernel-resident, not a user-space driver process — a deliberate,
  explicit choice, not an oversight.** `docs/research-minix-boot.md`
  raised a real fork here: writing virtio-blk (and eventually
  virtio-console) as an isolated EL0 driver process would be a concrete
  step toward this project's stated microkernel goal, using the driver as
  the forcing function for dynamic task creation and real IPC. Decided
  against, for now — that would pull the process-model/IPC work (parking
  lot, below) into phase 3's critical path, and phase 3's actual goal is
  disk commands working at all. Revisit once there's more than one
  reason to want driver isolation; virtio itself doesn't require it.

### 3b. A filesystem reader

- **Target format: FAT32.** Not a new choice — it's already the format
  `make image` produces and the ESP is formatted as, so targeting it means
  runtime reads and the boot-time ESP share one format story instead of
  two. Revisit only if a real reason to support something else shows up.
- **Open question, worth deciding before writing code:** hand-roll a
  minimal FAT32 reader (directory entries + cluster-chain traversal — more
  involved than ACPI/SPCR's fixed-offset struct reads, but the same spirit)
  or pull in an existing `no_std`-compatible crate (e.g. `fatfs`). The
  project has hand-rolled every parser so far specifically to avoid
  depending on more than the data actually needs (see `acpi.rs`'s doc
  comment on why it isn't the `acpi` crate) — but FAT32 is enough more
  complex than SPCR that a well-tested crate might be the better call here.
  Flagging this explicitly rather than deciding it unilaterally.

### 3c. New syscalls + the commands themselves

File I/O needs to go through the kernel the same way console I/O does —
userland has no direct hardware access. Reasonable syscall shape:
`open_dir`/`read_dir` (or a single "list this path" call, simpler for a
first cut), `open_file`/`read_file`/`close`. Exact shape is an
implementation detail to settle once 3a/3b exist, not before.

**Commands, ranked by how directly they build on 3a/3b and how much they're
worth to a "browse the filesystem" experience:**

| Command | What it needs | Priority |
|---|---|---|
| `ls [path]` | directory listing | Core — the first thing to prove the stack works |
| `cat <file>` | file read | Core — proves file content, not just names |
| `pwd` | shell-local state only (no new syscall) | Core — trivial once `cd` exists |
| `cd <path>` | shell-local state + validating the path exists (a directory listing or stat call) | Core — makes `ls`/`cat` usable with relative paths |

That's the phase-3 target: four commands, all read-only, all directly
exercising the new storage stack. Deliberately not more than that — see
below.

**Deliberately out of scope for phase 3** (candidates for a phase 4, once
3a-3c are proven):

- **Any write support** — `mkdir`, `rm`, `touch`, `cp`, `mv`, output
  redirection (`>`, `>>`). Writing to a real filesystem is a meaningfully
  bigger risk than reading (FAT table updates, cluster allocation, the
  chance of actually corrupting the disk image on a bug) and doesn't
  block anything read-only commands need. Get reads solid and tested
  first.
- `stat`/`file`, `find`, `tree` — nice-to-haves, not core to "the shell
  can browse the disk," easy to add once `ls`/`cat` exist.
- Wiring `loader.rs` itself to use this new runtime driver instead of
  boot-services file I/O. Tempting for consistency, but boot-time loading
  already works and has no reason to change; conflating "make the shell
  useful" with "refactor a working subsystem" would slow phase 3 down for
  no user-visible benefit.

## Parking lot (known future work, not yet sequenced)

Pulled from `docs/processes.md`'s "known rough edges" and `CLAUDE.md`'s
running "next milestone" notes — real gaps, just not phase 3's problem:

- A shared syscall-ABI crate (numbers currently hand-duplicated between
  `kernel/src/syscall.rs` and every userland program).
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
  `docs/research-minix-boot.md`'s comparison. Explicitly deferred past
  phase 3 (see 3a's note above) rather than decided by drift: it needs
  dynamic task creation and real IPC first, not just a virtio driver.

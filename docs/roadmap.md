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
- **Phase 3a — a real virtio-blk driver.** Runtime (post-boot-services)
  block I/O, proven by reading sector 0 back and checking the real MBR
  boot signature. Read-only, synchronous, polling — see `CLAUDE.md`'s
  "Phase 3a" section for how the transport (virtio-mmio, not
  virtio-blk-pci) got decided and verified.

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

### 3a. A real block device driver — done

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
  Write support stayed out of scope, as planned (see "Deliberately out of
  scope" below).
- Confirmed working end to end: reads sector 0 back and checks the real
  MBR boot signature, not just "no error returned." See
  `kernel/src/virtio_mmio.rs`/`kernel/src/virtio_blk.rs`.
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

### 3b. A filesystem reader — done

- **Target format: FAT32, as planned** — matches `make image`'s
  `hdiutil -fs FAT32` output, what Parallels ultimately boots from too.
  Real surprise along the way: `make run`'s fast dev-loop disk
  (`fat:rw:esp`, QEMU's `vvfat`) turned out to be **FAT16**, confirmed by
  decoding its BPB by hand before writing any parser code (`BS_FilSysType`
  literally reads `"FAT16   "`) — `make run-image` (new Makefile target)
  boots the real `esp.img` instead, since `run`'s disk can never satisfy
  a FAT32 mount. See `CLAUDE.md`'s "Phase 3b" section for the full story.
- **Decided: hand-rolled, not a crate** — the open question this section
  used to flag. Turned out to be more than a style preference: this
  reader runs after `exit_boot_services`, where the global allocator is
  no longer valid, and every `no_std` FAT crate surveyed assumes an
  allocator is reachable somewhere in its stack. A hard constraint, not
  just precedent.
- **A second real surprise:** this project's own `\EFI\OUROBOROS\`
  directory name doesn't fit FAT's 8.3 short-name limit (9 characters) —
  real FAT32 formatters handle that with a long-filename (LFN) entry this
  reader doesn't parse yet. Left as a documented gap rather than fixed
  preemptively; see `kernel/src/fat32.rs`'s module doc comment and the
  "reasonable next steps" note below on what to do about it before 3c
  needs to navigate there.
- Confirmed working end to end: lists `\EFI\BOOT`, reads `BOOTAA64.EFI`
  back (a real multi-cluster file, not a single-block special case) and
  checks its exact size and PE header magic against the real built
  binary. See `kernel/src/fat32.rs`.

### 3c. New syscalls + the commands themselves

**One thing to resolve before this starts:** `\EFI\OUROBOROS\` (holding
the shell binary and `INIT.CFG`) won't be reachable by name once `cd`/`ls`
are real, since `fat32.rs` doesn't parse the long-filename entry real FAT32
formatters write for a 9-character name — it only sees the mangled 8.3
alias (`OUROBO~2`). Two ways out: rename the directory to fit 8.3 (cheap,
this project controls the name, but touches an already-documented,
widely-referenced path), or implement LFN parsing (more general, more
code, benefits any future file with a long name, not just this one).
Worth deciding with 3c's actual UX in mind, not before.

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

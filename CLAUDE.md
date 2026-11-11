# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Start here

`scratch/daily-standup.md` is written at the end of the previous working day to
be read at the start of the next: where the tree was left, what went in, and
what is outstanding. `scratch/` is gitignored and is not part of this
repository, so the file is absent on a fresh clone and on any day that was not
closed out. When it is absent, `git log` and the documents named below are the
way in.

## What this is

Ouroboros is an ARM64 (aarch64) operating system written in Rust (plus some
assembly where needed), still in its earliest stages. Design goals, taken
from the project's original brief and `README.md`:

- Microkernel architecture
- POSIX-ish system calls
- Preemptive multitasking
- Filesystem choice still undecided (research needed; likely a simple FS first)
- Draws ideas from Linux, Minix, and Plan 9
- Primary test target is Parallels on Apple Silicon, not just an emulator

## Where the history and design rationale live

This file used to also carry a milestone-by-milestone narrative of
everything built so far. That history now lives under `docs/`, so this
file can stay focused on durable, load-bearing guidance. `docs/README.md`
is the full annotated index of everything under `docs/`; the pointers below
are the ones worth having in mind before you open it:

- **`docs/CHANGELOG.md`**: the full milestone record, phase 0 to the
  present, newest first. Every completed step (the shell, disk/FAT32
  support, USB keyboard + storage, the userland servers, the network
  stack, …) is recorded there in condensed form. Check it for *what was
  built, and why it works the way it does*.
- **`docs/ROADMAP.md`**: the forward-looking plan of known future work
  (open frontier, remaining follow-ups, north-star directions, open gaps).
- **`docs/roadmap-completed.md`**: the finished arcs that used to live in
  `ROADMAP.md`, moved out so the roadmap stays forward-looking (the
  *plan-shaped* companion to `CHANGELOG.md`'s condensed milestone log).
- **The postmortems under `docs/postmortems/`** (thirty-one of them) hold the design, bug
  and process retrospectives: *the traps already hit and the lessons
  learned*. Read the relevant one before reworking a subsystem.
  `docs/README.md` indexes all of them with a full annotation each, and every
  postmortem itself opens with an abstract and its spine in a blockquote, so
  the index is enough to pick one. The five whose lessons are load-bearing
  well beyond the subsystem they came from:
  - `cluster-keys-postmortem.md`: *a step is only verifiable if the check can
    fail*. Most of that arc's real findings were checks that could not.
  - `repairing-the-repairs-postmortem.md`: *a repair is a change, and changes
    have the same defect rate as the code they fix*; keep the fix the size of
    the bug.
  - `blind-instruments-postmortem.md`: *the observer is a check too, and it is
    the one nobody mutates*. Five tools reported success while proving nothing.
  - `unspellable-postmortem.md`: *make the wrong thing unspellable, not
    un-grepped*: a required parameter beats an opt-in wrapper.
  - `review-and-split-postmortem.md`: *a green signal is a claim, not
    evidence*, and *a diff too big to review is too big to fix*.
  - `true-when-written-postmortem.md`: *the comment was true when it was
    written; that is the problem*. A claim that guards behaviour is a check
    nobody has written yet, and the edit that falsifies it is always in
    another file, so compiler, tests and review all miss it.
- **`docs/work-journal/`**: a chronological dev-log, one file per day (narrative "what and why
  each day"), a lighter companion to the milestone-oriented `CHANGELOG.md`.
- **`docs/README.md`**: the annotated index of every document under `docs/`;
  **`docs/source-map.md`**: the annotated index of every source file. Both
  hold the long-form detail this file used to carry inline.
- **Reference docs**: `docs/architecture.md` (boot flow, privilege model,
  memory layout, exceptions, syscall ABI, console), `docs/processes.md`
  (userland loading, the ELF/PIE binary format, and the relocation-class
  traps: **read this before writing any userland program**), and
  `docs/shell-commands.md` (the shell builtins).

The section "Rules for boot and low-level code" below is the deliberate
exception: durable "read this before touching the code" guidance, kept inline
because a session editing that code needs it at hand.

## Rules for boot and low-level code (read before touching it)

Each line is a hazard already hit. The full story behind each (the
debugging, how it was verified, the dead ends) is in `docs/boot-history.md`,
moved out of this file word for word on 2026-11-02; the code's module docs
are the authority on how it works now. **This list is the version to keep
current when the code changes**; the history is a record and is not.

- **UEFI is the only boot path.** The kernel is a UEFI application
  (`aarch64-unknown-uefi`, `[[bin]]` named `BOOTAA64` so it boots from
  `\EFI\BOOT\BOOTAA64.EFI` with no rename). Parallels boots ARM VMs only
  through UEFI, so no multiboot, no `-kernel` shortcuts. A split into a thin
  loader plus a kernel binary is expected one day; it has not happened.
- **`main()` never returns to firmware**, and its first act is to leave the
  firmware's stack for `KERNEL_STACK` (256 KB in `.bss`): the Pi firmware's
  stack is 16 KB with its page tables directly below, and overflowing it was
  every firmware fault the board showed (`testing-pi4.md` section 6). The
  first log line names the build (`kernel/build.rs`) and the stack.
- **After `exit_boot_services`, no `log::*`, no `alloc`, no UEFI protocol.**
  The logger and global allocator are boot-services-backed. Output goes
  through `console.rs`, which is why `Console` is an enum and not a
  `Box<dyn Write>`. The returned memory map is kept: `mmu.rs` maps real RAM
  from it.
- **No fallback UART address, ever.** A guessed QEMU PL011 address
  hard-crashed Parallels. A `Uart` is built only from a discovered address;
  no address means no post-exit console, not a guess. The same goes for RAM
  and device ranges: discover them, never hardcode QEMU's map.
- **virtio-mmio is probed only where the DSDT declares it present** (`_HID
  "LNRO0005"` and a `_STA` that says so, `acpi::virtio_mmio_transports`;
  `find_device` takes that list). A read of an undeclared slot, or of
  Parallels' declared GPU in a VM without it, is a synchronous external
  abort; a console found was once taken to mean QEMU, and when Parallels
  27.1 gained one every boot faulted (2026-11-04).
- **Console discovery order is devicetree, ACPI SPCR, PCI serial**, the
  parsing done before the exit so failures log through UEFI. All three were
  dead on Parallels until 27.1 (2026-11-03): its VM's serial port is now
  found by one of them (which one is not yet read) and writes
  `parallels/com.output.txt`. virtio-console is transmit-only and is not
  Parallels' serial port.
- **The exception vector table must be in plain `.text`.** In a custom
  section (`.text.exceptions`) the PE/COFF backend made the page
  non-executable: EC 0x21, IFSC 0x0F on the first jump, then a fault loop.
  `exceptions.rs` says so at the `.text` line.
- **Vectors go in right after the exit, before anything can fault.** At an
  EL2 handoff the `VBAR_EL1` write is live only from the drop to EL1. Every
  fault line names its core (`EXCEPTION core=N`); a secondary's fault halts
  that core alone.
- **The MMU walk starts at L0 (`T0SZ=20`, matching firmware), never one L1
  table.** The one-table version was hand-verified correct and still
  faulted; matching firmware's starting level was the A/B-tested fix. Read
  `mmu.rs`'s module doc before "simplifying" it. Install is two halves,
  `build_identity_map` (memory) then `switch_to_identity_map` (registers).
- **At an EL2 handoff (the Pi, `make run-el2`) the switch is the drop to EL1
  in `el2.rs`**, an `eret` into a running MMU; any other level halts with a
  line. Run `make test-el1-drop` when `el2.rs`, the switch or
  `exceptions::install` changes.
- **The kernel never runs at EL1 with IRQs unmasked.** Masked at the exit;
  the first `eret` into task 0 unmasks for EL0 only. So slot 5 (IRQ at EL1h)
  reports and halts as a broken invariant, and slot 9 (IRQ from EL0) is the
  one resumable IRQ path. `synccell.rs` explains why this is load-bearing.
- **Slot 8 (sync from EL0) checks `ESR_EL1.EC` first**: only 0x15 (`svc`)
  takes the syscall trampoline; EL0 faults land in the same slot. Syscalls
  are number in `x8`, arg0 in `x0`, return in `x0` (Linux's shape, not its
  ABI). EL0's `wfe`/`wfi` trap unless `SCTLR_EL1.nTWE`/`nTWI` are set.
- **Every resumable path saves `q0`-`q31`, `FPCR` and `FPSR`** (`Context`,
  800 bytes). The kernel's memcpy and every userland program use vector
  registers; without the save a value live across a syscall changed.
  `make test-fpsimd` when a trampoline, `Context` or a switch path changes.
- **A task switch rewrites the trampoline's saved frame in place**
  (`SP_EL0` included); restore-and-`eret` returns into whichever task the
  frame now holds. `tasks.rs` is the authority on slots and counts.
- **The tick is the generic timer's EL1 physical timer (PPI 14, INTID 30);
  GIC version and addresses come from the ACPI MADT** (`madt.rs`, then the
  `gicv2.rs`/`gicv3.rs` backends behind `gic.rs`), not from a devicetree dump.
- **"It compiles" proves nothing here.** The verification that has worked:
  a temporary deliberate fault at an address known unmapped, run under QEMU
  with `-d int -D <log>`, expecting exactly one exception and our handler's
  line, and a sustained run (many ticks) with zero aborts in the trace.
  Remove the temporary fault afterwards.

## Prose style

The no-em-dash rule in `~/.claude/CLAUDE.md` holds for the living documents
and for code comments. The dated records keep their punctuation as written:
`docs/work-journal/`, `docs/archive/`, `docs/release-notes/`,
`docs/handoffs/`, `docs/CHANGELOG.md`, the postmortems and their pages on
the site (`docs/site/postmortem-*.html`), `docs/boot-history.md` and
`docs/roadmap-completed.md`. Each says what was written on its day, and
rewriting one is a decision, not a fix. New text in them follows the rule.
Settled by Hans on 2026-11-03, after the first `/style-check` sweep. The
site's other pages were swept the same day (#403).

## Commands

One line per target. **The full entry for each, with what it checks, its
controls and the "run it whenever X changes" clause, is in
`docs/testing/make-targets.md`.** Before pushing, grep that file for the
function, file or syscall you changed: every rig names what it guards, and
the grep is how you find the rigs a change owes. A new target gets its full
entry there and one line here, never the long form here.

```sh
make build                    # cargo build (debug)
make build PROFILE=release    # release profile
make shell-bin                # build shell/ for aarch64-unknown-none + strip
make run                      # stage the ESP dir and boot QEMU with a virtio-blk vvfat disk (FAT16): the fast dev loop
make run-virtio-console       # same as `run`, plus a virtio-mmio console device attached
make run-net                  # `run` plus virtio-net, SLIRP networking and a net.pcap dump
make run-image-net            # `run-image` (real FAT32, disk commands work) *and* the NIC from `run-net` ...
make run-image-server         # like run-image-net, plus SLIRP hostfwd tcp::5555->:80
make run-image-9p             # run-image-server plus a second hostfwd tcp::5640->:564 for netd's 9P ...
make run-image-9p-client      # the guest remote-mounts a host-run 9P server (scripts/np9p_server.py)
make run-image-2vm-a          # two-VM cluster, machine A: exports its disk (run first, it listens)
make images-2vm               # build BOTH FAT32 node images
make images-2vm-ext2          # the same for the ext2 pair (one target on purpose: per-machine keys)
make run-image-2vm-ext2-a     # the two-VM cluster on ext2: the only rig that tests cluster permissions
make run-image-2vm-ext2-b     # machine B of the ext2 pair
make run-image-2vm-b          # two-VM cluster, machine B: `mount -r 10.0.2.10:564 /mnt/a` reads A's disk
make run-usb-kbd              # same as `run`, plus an xHCI controller + USB keyboard + HMP monitor socket ...
make run-usb-multi            # same as `run-usb-kbd`, plus a usb-tablet and a usb-storage stick on the ...
make run-usb-hub              # the Raspberry Pi's USB layout on QEMU
make test-usb-hub             # the hub layout driven and graded: three boots
make image                    # build build/esp.img, a raw MBR+FAT32 disk image
make run-image                # boot build/esp.img (real FAT32): needed for fsd and every disk command
make run-image-gpt            # build build/espgpt.img (GPT-wrapped FAT32) and boot it
make run-image-exfat          # build build/espexfat.img (exFAT + FAT32 ESP) and boot it
make disk                     # build/disk.img: one disk, FAT32 ESP as partition 1 and the ext2 root as partition 2
make run-disk                 # boot build/disk.img on virtio-blk
make run-sd                   # disk.img on QEMU's sdhci-pci, the ESP booted from virtio-blk-pci: the root from an SD card
make run-image-ext2           # build build/espext2.img (ext2 + FAT32 ESP) and boot it; needs e2fsprogs
make parallels-hdd            # wrap build/esp.img into build/esp.hdd, a Parallels-native virtual hard disk
make sdcard SDCARD=/Volumes/OUROBOROS # the Pi card's boot partition alone, its root left whole (a whole Pi card is `make bootstick PI=1`)
make stick STICK=/Volumes/STICK # the Pi's USB stick, its root beside a card `make sdcard` staged (a bootstick card needs none)
make test-parallels           # scripted real-hardware round trip via prlctl
make test-early-fault         # the early fault reporter on QEMU
make run-el2                  # `run` with the firmware handing off at EL2
make test-el1-drop            # the EL1 drop: test-early-fault's three boots handed off at EL2
make test-async-rmount        # six driven QEMU boots against host-run 9P peers
make test-call-interrupt      # an interrupted call's late answer goes nowhere
make test-held-keys           # six driven QEMU boots of the held user keys
make test-run-callback        # a `cpu` run's /host callbacks served while the run is in flight, and only then
make test-run-orphan          # a `cpu` run's command dies with the run's connection, closed by a FIN or a RST
make test-cred-client         # the client sends a user credential: served while the user is logged in, refused and its session closed after
make test-no-identity         # an export with no /etc/cluster/id runs nothing, in either signed format
make test-heap                # the user heap on a booted image
make test-malloc              # picolibc's malloc on a booted image
make test-fpsimd              # a task's FP/SIMD registers across the kernel
make test-spawn-owner         # the owner of the kernel's spawn staging
make test-spawn-size          # the size of a program file spawn accepts
make test-proc-cpus           # /proc/cpus, the cores that take tasks as a file
make test-stack-free          # a stack's never-written bytes, counted by the kernel and read as /proc/<n>/stackfree
make test-sd                  # an SD card as the root disk: QEMU's sdhci-pci, two cards, writes across a power off, the firmware partition refused
make bootstick BOOTSTICK=/dev/diskN # write disk.img's layout to a whole disk or image file (PI=1 for the Pi 4)
make test-bootstick           # bootstick against image files
make test-sdcard              # make sdcard keeps a card's boot counter, against an image file
make test-boot-partition      # the boot partition the kernel names
make test-cond-vt             # cond's escape sequences on the framebuffer
make test-nav-keys            # the navigation and function keys on QEMU's USB keyboard
make test-cwinsz              # the console's size read by an ordinary program
make test-kbd-queue           # the kernel's keyboard queue
make test-kbd-mode            # the keyboard modes
make test-kbd-cut             # cut keys in the kernel's keyboard queue
make test-ctermios            # terminal settings in a C program
make measure-syscost          # the cost of a syscall
make test-smp                 # multi-core: six boots across -smp, EL2 and GIC versions
make test-listing-walk        # a listing page's walk stops at the page's end, seen by QEMU's virtio-blk trace
make test-spawn-reads         # spawning a program resolves its path once a read request, exactly
make test-write-walk          # a write walks a file's cluster chain once, exactly
make test-stale-cursor        # no chain cursor outlives its file
make test-grow-dir            # a directory grown past its first cluster lists and finds every entry
make test-cpoll               # poll for the keyboard in a C program
make test-edit                # DevTools's editor on Ouroboros
make test-cargs               # argv in a C program
make test-cerrno              # errno from the C file layer
make test-cenv                # the environment in a C program
make test-cclock              # the clock in a C program
make test-cfenv               # the floating-point exception flags picolibc's math leaves
make test-include             # the C headers on the disk
make test-cpp                 # the C-hosting arc's finish line
make test-unmount             # `unmount` with a partition mount and a held file, on a copy of the ext2 image
make test-mount-points        # a parent lists the mounts below it, and bare `mount` lists every mount
make test-crename             # unlink, rename and fstat in the C port
make test-mv-into             # a directory moved into itself by a case-only name
make test-fat-link            # FAT32 never reaches past its volume, by a link or a first cluster
make test-mount-gate          # the MOUNT syscall answers the filesystem server alone
make test-probe-ioerr         # a read error inside a partition's probe is an I/O error, not "no filesystem"
make test-blank-disk          # a blank virtio disk reaches fsd, below any USB stick
make test-volume-bound        # a volume claiming more than its partition is not mounted
make test-ext2-blocks         # ext2 never reaches past its volume by a block number read off the disk
make test-long-set            # exFAT sets are patched whole, and only in the spec's shape
make test-disk-gate           # exFAT's disk gate, the second line under fsd's verb gate
make test-valid-length        # exFAT bytes past a file's ValidDataLength are zeros, read or written
make test-exfat-names         # exFAT names outside ASCII are names of their own
make test-fat32-names         # FAT32 long names outside ASCII are names of their own
make test-fat-extflags        # FAT32 with mirroring off reads and writes its live FAT alone
make test-mv-undo             # a failed exFAT `mv` is rolled back, both files as they were
make test-dir-scan            # making files in an exFAT directory reads its sectors a few passes a file, ...
make test-free-chain          # exFAT frees a FAT-chained file a sector at a time
make test-alloc-batch         # exFAT sets a request's FAT entries and bits a sector at a time
make test-batch-refused       # a FAT batch the disk refuses, and its clusters given back by their bits
make test-keyboard-chain      # four driven QEMU boots through the nested-shell ...
make test                     # host unit tests, clippy and the host-side checks: run before every push
make check-relocs             # the PIE contract: no R_AARCH64_ABS64 in any userland binary
make check-stack-chains       # every userland program's deepest call chain against its stack, read off the built binaries
make check-xhci-barriers      # the xHCI driver's two DMA barriers found in the built kernel image
make check-site               # the published GitHub Pages site vs the documents it abridges
make check-statics            # every static in kernel/src against the multi-core plan's inventory
make check-disk-handle        # no fsd file but disk.rs can reach the disk
make check-doc-pointers       # every pointer to a document's section names one that exists
make check-link-layout        # programs/linker.ld against the pin DevTools's ld holds
make check-picolibc           # the committed C library

make clean
```

`make run`/`make run-image` need QEMU (`brew install qemu`, which also
provides the aarch64 OVMF firmware). `make image` needs macOS's `hdiutil`,
`make parallels-hdd` Parallels Desktop's `prl_disk_tool`, the ext2 targets
e2fsprogs, and `make shell-bin` (so `make esp`/`make run`) `rustup component
add llvm-tools` for `llvm-objcopy` (the Makefile's `OBJCOPY` comment says why).

The kernel and userland programs have no unit test suite; the **pure crates**
(`accounts`, `regex`, `ed25519`, `clusterkeys`, `ninep-abi`, `keyseq`,
`parttable`, `amlscan`, `heldkeys`) do, and `make test` runs them with the host-side checks. Such a
crate is a workspace member but not a default-member, so until something
depends on it `cargo build`, `make build` and `make esp` stay green while it
is broken: run `make test` before pushing anything that touches one.
`make test-parallels` is the scripted real-hardware smoke test (`prlctl`
keystrokes and screenshots). What `make test` covers, and the history of
`test-parallels`, are in `docs/testing/make-targets.md`.

## Toolchain

Pinned via `rust-toolchain.toml` (stable channel, targets
`aarch64-unknown-uefi` and `aarch64-unknown-none`, which both install
automatically on first build). Both targets ship prebuilt `core`/`alloc`
on stable, so no nightly toolchain or `-Z build-std` is needed.
`.cargo/config.toml` defaults the build target to `aarch64-unknown-uefi`
(for `kernel`, the workspace's only default member - see `Cargo.toml`'s
`default-members` comment for why `shell` is deliberately excluded from
that default) and separately configures `[target.aarch64-unknown-none]`
(for `shell`, always built explicitly with `--target aarch64-unknown-none`
- see the Makefile). Plain `cargo build`/`cargo clippy` at the repo root
already target `kernel` correctly; building `shell` needs the explicit
`-p shell --target aarch64-unknown-none` (or just `make shell-bin`).

## Structure

A skeleton: one line per file, enough to find the right one and to know what
else a change will touch. The full annotation for every entry is in
`docs/source-map.md` (source) and `docs/README.md` (docs). Each file also
carries its own `//!` module doc, but on the most-changed files those have
themselves fallen behind (see `docs/source-map.md`'s closing caveat), so the
code is the authority and every annotation is a claim to check.

```
docs/                every document is annotated in full in `docs/README.md` - read that index
                     rather than guessing from filenames, since several are named for a subsystem
                     but organized around a lesson. The load-bearing ones:
  manual.md          the one-stop user manual: prerequisites, building, running, the shell tour, the CLUSTER section, the syscall ABI
  architecture.md    reference: boot flow, privilege model, memory layout, exceptions, syscall ABI, console
  processes.md       reference: userland loading, the ELF/PIE binary format, the relocation-class traps - READ THIS BEFORE WRITING ANY USERLAND PROGRAM
  shell-commands.md  reference: the default shell's builtin commands
  CHANGELOG.md       the milestone record, phase 0 to the present, newest first - what was built, and why it works the way it does
  ROADMAP.md         the forward-looking plan (finished arcs live in roadmap-completed.md; the cluster direction in roadmap-cluster.md)
  roadmap/           the sub-plans: the seven cluster documents (roadmap-cluster*.md), roadmap-fid-verbs.md,
                     roadmap-async-rmount.md, roadmap-session-auth.md, roadmap-user-keys.md, roadmap-el1-drop.md
                     (the Pi hands off at EL2; the kernel assumes EL1), roadmap-c-hosting.md (a C program as a Unix command)
                     roadmap-ctrl-c.md (Ctrl-C as a key for a raw program, with Ctrl+\ as the way out)
                     roadmap-smp.md (multi-core: the end of the single-core argument, the arc from 2026-10-12)
                     roadmap-one-disk.md (one disk for boot and root: the ESP first, the root beside it, the arc after multi-core)
                     and roadmap-sd-card.md (the SD card as the Pi's only disk: an SDHCI driver, QEMU first, then EMMC2)
  work-journal/      chronological dev-log, one file per day plus an index README - a lighter companion to CHANGELOG.md
  testing/           testing-qemu.md (every `make run-*` target, the test images, the 9P host peers, the two-node
                     cluster rig), testing-parallels.md and testing-pi4.md (the real-hardware guides, sharing one
                     caveat: NO NETWORKING, so the whole cluster is QEMU-only), testing-exfat.md
  gap-analysis.md    per-subsystem have/partial/don't inventory vs mainstream Unixes, capped by a ranked list of the biggest gaps
  archive/           contemporaneous build logs, kept for archaeology only - not the reference
  postmortems/       the thirty-one design/bug/process retrospectives, one file each (see the section above, and docs/README.md)
  research/          synthesis notes on MINIX/Plan 9/Helix/Redox, the GUI stack, and where the design should go next

kernel/              every file annotated in full in `docs/source-map.md`; each also carries its own `//!`
  build.rs           the build identity (commit, +dirty, profile) the boot logs; reruns every build on purpose
  src/main.rs        #[entry]: the switch to the kernel's own 256 KB stack first (the Pi firmware's is 16 KB over its page tables), then
                     UEFI init, console/MADT/PSCI discovery, loader, ExitBootServices, then exceptions/mmu/xhci/storage/net/gic/timer/tasks
  src/uart.rs        PL011 console driver (post-ExitBootServices only)
  src/uart16550.rs   16550 console driver - PCI-discovered consoles, genuinely different hardware
  src/devicetree.rs  console discovery via the UEFI devicetree (dead end on QEMU and Parallels)
  src/dtranges.rs    devicetree address translation through `ranges` (bus -> CPU; the Pi 4's PL011 is 0x7e201000 on the bus, 0xfe201000 for the CPU)
  src/acpi.rs        console discovery via RSDP -> XSDT -> SPCR (works on QEMU, dead end on Parallels) + the shared find_table walk
  src/madt.rs        GIC version/address discovery via the ACPI MADT - where gic.rs's addresses actually come from
  src/power.rs       POWER syscall backend: PSCI SYSTEM_OFF via the FADT-discovered conduit, else halt
  src/pci.rs         PCI enumeration: serial-console discovery, discover_xhci, discover_sdhci, log_all_devices
  src/console.rs     global console handle (Pl011|Uart16550|Virtio|Framebuffer), shared with the exception handler
  src/framebuffer.rs GOP discovery: resolution/stride/format + framebuffer base - the console that works on Parallels
  src/font.rs        embedded 8x8 bitmap font, printable ASCII only (cond keeps its own copy)
  src/fbconsole.rs   framebuffer text console - the kernel's EMERGENCY/boot console only; steady state is cond
  src/fbdev.rs       dumb framebuffer primitives for cond (FB_BLIT/FB_SCROLL/FB_CLEAR, and FB_CURSOR: the one inverted cell, kernel-owned), gated to CON_TASK
  src/smp.rs         the secondary cores (multi-core step 2): PSCI CPU_ON per MADT core, the no-stack entry stub that drops each
                     core to EL1 (or turns its MMU on in place) on the boot core's tables, a 64 KB stack per core, TPIDR_EL1 the
                     core index, this core's GIC interface, `core N up`, then (step 4(b)) its own idle loop at EL0 with its own
                     timer. Any core can kick another into the kernel by SGI 0 under the kernel lock (step 4(c)). Since step 4(d) the
                     spawned programs run on the secondaries (tasks::may_run_here: the shell, idle and servers stay on the boot core)
  src/el2.rs         the exception level the firmware hands off at (logged on every boot) and the drop from EL2 to EL1 when it is 2:
                     EL1's regime set to the built identity map, SCTLR_EL1 whole, HCR_EL2 to el2.rs's HCR_EL2_VALUE (RW, HCD,
                     no traps, interrupts to EL1), the EL1 timer allowed, the firmware's EL2 timer off, then eret on the same stack. The Pi hands off at EL2; QEMU does with
                     virtualization=on (`make run-el2`). Called from mmu::switch_to_identity_map, so main.rs has one install
  src/earlyfault.rs  the early fault reporter: a fault BEFORE exceptions::install() goes through the firmware's vectors, and a RELEASE
                     firmware (the Pi's) prints one line for it; this registers the kernel's own handler through the firmware's CPU
                     protocol and prints ESR/FAR/ELR, a backtrace and the loaded image holding each address, on the serial
                     console, or on the framebuffer when there is none
  src/exceptions.rs  VBAR_EL1 vector table + fault reporting, and the three resumable paths (IRQ, SVC, EL0 fault)
  src/mmu.rs         per-task translation tables, built (memory only) then switched (registers, or the EL2 drop) as two halves.
                     READ ITS MODULE DOC FIRST: the L0 start level is a fixed bug, not a style choice
  src/gic.rs         version-dispatching facade over gicv2/gicv3, selected by madt.rs
  src/gicv2.rs       GICv2 backend: distributor + memory-mapped CPU interface
  src/gicv3.rs       GICv3 backend: redistributor + ICC_* sysreg CPU interface - confirmed on QEMU and Parallels
  src/timer.rs       ARM generic timer (EL1 physical, PPI 14 / INTID 30), TICK_INTERVAL_MS
  src/loader.rs      INIT.CFG + the boot programs off the ESP; ELF64 parsing + R_AARCH64_RELATIVE processing
  src/supervisor.rs  server supervision: restart a crashed or wedged server from its boot image, per-boot cap
  src/synccell.rs    SyncCell<T>, the one mutable-static wrapper: the single-core argument stated once, not per cell
  src/lock.rs        the kernel lock (multi-core step 4(a)): one core in the kernel at a time, taken at every resumable entry's
                     Rust half and released before its eret, owned by core index, never reentrant (a re-entry halts with a line)
  src/syscall.rs     the svc dispatch table - the authority on which syscalls exist (numbers/sentinels in syscall-abi)
  src/tasks.rs       task slots, round-robin scheduler, mailboxes, grants, capability send-mask, per-task identity.
                     THE AUTHORITY on slot numbers and counts - restating that map elsewhere has drifted before,
                     see docs/postmortems/asking-the-right-question-postmortem.md
  src/virtio_mmio.rs virtio-mmio transport: probes only the transports the firmware's DSDT declares (amlscan), modern register layout
  src/block.rs       BlockDevice enum (Virtio | UsbMsd | Sdhci) - what fsd's disk layer sits on
  src/sdhci.rs       an SD card on a standard SDHCI, brought up from scratch by the SD spec and read by PIO, one sector a
                     time, 32-bit register access only; QEMU's sdhci-pci and the Pi's EMMC2 (SD-card arc; the Pi's only disk since step 4); reads and writes, never the firmware partition or the table that finds it
  src/usb_msd.rs     USB mass storage: Bulk-Only Transport + SCSI over xhci's bulk endpoints, with BOT error recovery
  src/virtio_blk.rs  virtio-blk: feature negotiation, one virtqueue, polling sector read/write
  src/virtio_console.rs  transmit-only virtio-console - works on QEMU, NOT what Parallels' serial port is
  src/virtio_rng.rs  virtio-rng, backing the RANDOM syscall; absent on Parallels/Pi and that is a supported case
  src/bootflags.rs   boot flag files at the ESP root: \NOXHCI, \XHCINOWR, \FBCON switch off one boot step to bisect a hang on hardware;
                     \MSDSTALL is a QEMU test fault (QEMU's stick only) for test-usb-hub.py --stall; \EARLYFAULT plants a fault in
                     the firmware's own code before the exit, and \WALKFAULT one inside the reporter's own image walk, both for
                     test-early-fault.py
  src/bootid.rs      the boot identity: a persisted per-boot counter + EFI_RNG boot entropy, before ExitBootServices (BOOT_ID)
  src/sdscan.rs      what the firmware says of the SD card's controller (SD-card arc steps 0, 0b): devicetree, ACPI _HID, SdIsArasan,
                     and the declared EMMC2's registers, read only; runs only under \SDDUMP, which also prints the tables in hex
  src/bootdisk.rs    the boot partition: the HardDrive node of the image's own device path, read before the exit (one-disk step 1),
                     and is_boot_disk, which makes the disk booted from the block device (step 3); the matcher itself is parttable's
  src/virtio_net.rs  virtio-net: rx/tx queues, the 12-byte header, IRQ-driven receive - the DMA-owning half of the net stack
  src/xhci.rs        from-scratch xHCI: rings, multi-device port scan, HID interrupt endpoint, storage endpoint reset

programs/            ALL userland programs, grouped by role. Annotated in full in `docs/source-map.md`
  linker.ld          the shared PIE linker script EVERY program uses. The relocation contract lives here:
                     R_AARCH64_RELATIVE is fine, R_AARCH64_ABS64 is unloadable. See docs/processes.md BEFORE
                     writing a userland program, and `make check-relocs` to verify one
  shell/             the default shell: line editor, builtins, pipelines, redirection, globs, completion, login
                     (src/login.rs). Deliberately MINIMAL - a command stays builtin only if it mutates shell
                     state, needs job control, or must run with no disk mounted; everything else is in /bin
  servers/fsd/       THE FILESYSTEM/STORAGE SERVER, protected slot 2, the only task BLOCK_* accepts. Owns the
                     NP_* verb dispatch, permission enforcement, fids, and erase/partition/format
    src/vfs.rs       the Filesystem enum (Fat32|ExFat|Ext2|Proc) + mount/probe - fsd's internal multiplexer
    src/fat32.rs     FAT32 read/write incl. LFN read+write, offset writes, mkfs
    src/exfat.rs     exFAT read/write + mkfs
    src/utf16.rs     long names as UTF-16 units: the one rendering to UTF-8 (or `?`s) exFAT and FAT32 both list and look up by
    src/ext2.rs      ext2 read/write + mkfs - the arm that proved the abstraction (a genuinely different inode model)
    src/partition.rs GPT (CRC-validated, backup fallback) + MBR partition discovery
    src/proc.rs      the synthetic /proc - the first NON-disk arm, which is what makes the enum a real VFS
    src/disk.rs      BLOCK_* syscall shim
  servers/cond/      THE CONSOLE SERVER, protected slot 3. Two backends: byte-stream via CON_WRITE, or render
                     glyphs itself via FB_*. Owns the font, cursor, wrap, scroll and ANSI parsing
  servers/netd/      THE NETWORK SERVER, protected slot 4. The whole protocol stack in userland: ARP/IPv4/ICMP/
                     UDP/DNS/TCP, an HTTP static-file server, the 9P export gateway + cluster auth, remote
                     execution (cpu), and the /net/tcp dial-out/dial-in connection files
  servers/accountd/  THE ACCOUNT SERVER, protected slot 5. Exists because a setuid binary CANNOT work here:
                     the shell reads the binary, so "this is setuid" would be a claim by a task the capability
                     model distrusts. One op (ACCTOP_PASSWD); it authorizes on SENDER_ID, never GET_ID(sender)
  fileutils/         ls cat mkdir rmdir touch rm cp mv writeat chmod chown tree write more (cp/mv REFUSE an
                     existing destination without -f - deliberate, on a system with no undo)
  textutils/         the pipeline filters: upper wc grep head tail nl rev uniq sort (sort is the one that
                     cannot stream, so it is the one that uses the heap)
  netutils/          ping resolve fetch dial serve - reach netd via the TO_NET cap the shell delegates at spawn
  shellutils/        echo uptime clear pwd readkey send recv selftest bootid man printenv id args edtest rdprobe fpprobe coreprobe vtprobe syscost stageprobe cursorprobe dirprobe mountprobe stackprobe openprobe
  admin/             passwd useradd groupadd usermod clusterkey - root-only account + cluster-identity tools
  demos/             hello (how a program ends itself), pong (the IPC echo-server shape)

ulib/                shared userland support library: syscall wrappers, argv/env, cwd + path resolution, the fs
                     client layer, the netd client, output routing, and the one #[panic_handler]
syscall-abi/         syscall numbers, sentinels, error values, the FSOP_* constants - kernel and userland share it
ninep-abi/           the NP_* cluster verb set, `resolve_ns`, and THE NORMATIVE WIRE SPEC for cluster auth
                     (checked across all three implementations by scripts/check-wire-constants.py on every `make test`)
ed25519/             hand-rolled Ed25519: SHA-512, field, curve, scalar, sign/verify. No heap. Deterministic
                     signing is why it was chosen - hardware entropy is absent on Parallels and the Pi
clusterkeys/         the /etc/cluster/{id,id.pub,authorized} file format. No trust-on-first-use
accounts/            /etc/{passwd,shadow,group} parsing/formatting, SHA-256 hashing, salts, lookups, rewrites
regex/               a small POSIX-ERE engine behind grep. An explicit backtracking stack, not host recursion
parttable/           partition tables, read: the MBR and GPT parsing (both CRCs, the backup header, checked bounds), the partition list
                     with root types, and the boot-disk matcher, through a read-sector closure; the ONE copy, which the kernel's
                     bootdisk.rs and fsd's partition.rs both call, host-tested in make test
amlscan/             the devices an ACPI table declares present by _HID (virtio-mmio's "LNRO0005", the Pi's EMMC2 "BRCME88C"), their _CRS
                     registers and interrupt, by a bounded walk of the AML that reads two method shapes exactly; the kernel probes no
                     other virtio-mmio address. Host-tested in make test, on the Pi's own tables too
heldkeys/            netd's held user keys: the table, whose key signs for a parked request, and when a run sent under one ends; liveness
                     is passed in, so the rules that turn on a login dead and not yet reaped run on the host. netd is the one caller
keyseq/              tells a key's escape sequence (ESC [ A) from ordinary input: the ONE filter every keyboard reader uses
                     (the shell's line editor, login, ulib::read_line and read_key), so two readers never disagree on what was typed;
                     the kernel uses it too, to keep keys whole in its keyboard queue
libc/                the C-portability arc: crt0 + syscall stubs + a narrow waist (write/read/open/sbrk/_exit)
                     that BOTH a hand-rolled libc and a real PICOLIBC link against unchanged
                     (third_party/picolibc-prebuilt; regenerate with scripts/build-picolibc.sh)
nsresolve/           a Rust staticlib wrapping ninep_abi::resolve_ns, so a C program reaches the SAME namespace
                     resolver ulib and netd use. EVERY C program links it (file.c depends on it); the link needs
                     --gc-sections, see docs/source-map.md
scripts/             test-parallels.sh (real-hardware smoke test), drive-qemu.py + drive-2vm.py (drive the guest
                     shell / a two-node cluster unattended - the fussy paced typing is load-bearing, see
                     docs/testing/testing-qemu.md), mk{gpt,exfat,ext2,clusterkeys,passwd,group}.py (build the test disk
                     images and the staged /etc files), np9p_{client,server}.py (the host-side 9P peers - the
                     FOREIGN OBSERVER for both directions of the export)
```

Seventy-nine-crate workspace (`cargo metadata`'s count, 2026-11-10). `kernel` and the shared libs (`ulib`,
`syscall-abi`, `ninep-abi`, `accounts`, `regex`, `ed25519`, `clusterkeys`, `heldkeys`, `keyseq`, `parttable`, `amlscan`, `nsresolve`) sit at the repo root; **every userland
program lives under `programs/`, grouped by role** (`programs/shell`,
`programs/servers/{fsd,cond,netd,accountd}`, `programs/demos/{hello,pong}`,
`programs/fileutils/*`, `programs/textutils/*`, `programs/netutils/*`,
`programs/shellutils/*`, `programs/admin/*`) - a
reorganization done once the flat top-level list grew unwieldy, purely a
directory move (crate *package* names are unchanged, so the Makefile - which
builds by `-p <name>` and reads `target/.../<name>` - needed no edits;
only each moved crate's `path = "../..."` deps deepened, the workspace
`members` list, and `.cargo/config.toml`'s `-Tprograms/linker.ld` changed).
Every userland crate is deliberately excluded from the workspace's
`default-members` (see `Cargo.toml`) since they need a different `--target`
than `kernel`; `syscall-abi` needs no such exclusion (a plain lib, no
`[[bin]]` to conflict with a target) and gets built automatically as
`kernel`'s path dependency. New programs slot into the matching
`programs/<category>/` dir (or a new category), depending on `syscall-abi`
(and usually `ulib`) via the `../../../` path the siblings use.

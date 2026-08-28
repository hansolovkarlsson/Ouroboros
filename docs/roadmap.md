# Ouroboros roadmap

Forward-looking plan — what's next and why, in plan form rather than
chronological narrative. **Completed arcs and milestones have been moved
out** to [`roadmap-completed.md`](roadmap-completed.md) (the plan-shaped
record — how each arc was sequenced and what was learned) and
[`CHANGELOG.md`](CHANGELOG.md) (the condensed milestone log), so this
document stays about what's *still open*. For *how* something already built
actually works, see [`architecture.md`](architecture.md) and
[`processes.md`](processes.md); for the debugging history and lessons
behind each decision, see the postmortems under `docs/` and `CLAUDE.md`.
This document is the one to update first when direction changes.

> **The long-term direction** — a Plan 9-style **resource-sharing cluster**
> (distributed Ouroboros: machines sharing storage/devices/services over
> per-machine namespaces and a uniform file protocol) now has its own phased
> plan in [`roadmap-cluster.md`](roadmap-cluster.md). The Plan 9 "local
> namespace + uniform protocol" work below is **Phase 0** of that arc — the
> foundation the whole distributed vision builds on, not a standalone item.

## What's next (the current frontier)

The microkernel arc is largely built — the FAT32 **filesystem** (`fsd`),
the **console** (`cond`), and the **network** server (`netd`) all run as
supervised, MMU-isolated userland servers, with a capability model, crash
recovery, and grant/safecopy bulk IPC (all in
[`roadmap-completed.md`](roadmap-completed.md) / [`CHANGELOG.md`](CHANGELOG.md)).
What's still open on that arc, in rough order of value:

1. **General / transitive capability delegation.** The delegation shipped
   2026-08-21 is deliberately coarse: one delegated target per task,
   non-transitive, in practice shell-only. Making it general (any task hands
   any held capability onward, revocably — MINIX's full grant model) would
   unlock true relay-free `a | b | c` and a spawned program running its
   *own* server. The catch: **neither consumer exists yet**, so building
   this first would repeat the "premature, a mechanism without a hard
   consumer" trap the capability-and-hardening postmortem flagged for
   delegation itself. Build the consumer first, or wait until one is
   actually wanted.

2. **Per-task ASIDs, revisited** — a pure TLB-flush-per-switch optimization
   that passed on QEMU but faulted the idle task on real Parallels and was
   reverted (see the isolation postmortem for the decoded fault evidence);
   needs a proven break-before-make sequence. Low value — a context switch
   already does far heavier work than the per-switch flush it would save.

The stack **guard page** (16KB guarded stack, which immediately caught a
real silent 8KB overflow in the shell's own `exec` path) and the 256KB raw
**userland heap** (`heap_info` — a real `alloc`-backed heap stays blocked on
stable: prebuilt lib`alloc` has `R_AARCH64_ABS64` relocations a `-pie` link
rejects, and `-Z build-std` is nightly-only), formerly tracked here, both
shipped 2026-08-20. See `CHANGELOG.md`.

**Deferred / blocked** (recorded, not chased): moving a *third* driver
out is limited by the no-IOMMU DMA constraint (the block transport can't
safely leave the kernel); reverse-engineering Parallels' proprietary
serial/storage device (vendor `0x1ab8`, no public spec); and an EHCI
driver for USB 2.0 sticks (a whole second host-controller bring-up for
poor value).

## Completed arcs (moved out)

These arcs are **done**; their full plan-shaped write-ups moved to
[`roadmap-completed.md`](roadmap-completed.md), and the condensed milestone
record is in [`CHANGELOG.md`](CHANGELOG.md):

- **The microkernel arc** — `fsd`/`cond`/`netd` as supervised MMU-isolated
  servers, EL0 fault isolation + supervision + heartbeat, the capability
  model + runtime delegation, per-task page tables, grant/safecopy IPC.
- **The network stack** — virtio-net driver + `netd` (ARP/IPv4/ICMP/UDP/DNS
  and a full TCP with flow control, RTO, congestion control, SACK), an HTTP
  static-file server, `ping`/`resolve`/`fetch`.
- **More filesystems** — GPT/MBR discovery, the VFS refactor, FAT32 + exFAT +
  ext2 read *and* write, plus the `stat` op.
- **Disk management** — `mount`-info/`unmount`, `erase`/`partition`, and
  `format` (mkfs) for all three filesystems.
- **Standalone binaries** — `/bin`, PATH, argv/cwd ABI, `ulib`, and the whole
  fs+net command surface externalized; then a *minimal* shell (only genuinely
  shell-coupled commands stay builtin).
- **Multi-stage pipelines** — N-stage `a | b | c` of standalone filters.
- **Shell interactive features** — output redirection, filename wildcards,
  tab completion, `-?` usage help, `man` pages, and the keyboard-ownership arc
  that lets interactive programs be `/bin` binaries.

## Remaining follow-ups from completed arcs (small, unsequenced)

The small open tails those arcs deliberately left:

- **ext4.** Much larger (extents, journaling, htree, checksums, 64-bit) and
  the no-alloc fixed-buffer constraint makes a big FS genuinely harder — a
  separate large arc, not a near-term ext2 follow-on.
- **A `/dev` namespace.** Only if multi-disk/partition addressing arrives (the
  Plan 9 devfs direction); nothing to name yet with one block device.

## Testing infrastructure: scripted real-hardware round trips

> **Direction update (2026-08-26): Parallels real-hardware testing is PARKED.**
> QEMU (single machine *and* the two-node cluster on a shared socket link — see
> [`testing-qemu.md`](testing-qemu.md)) is the working dev/test loop and is
> **good enough for now**. Parallels was never going to prove the cluster anyway
> — it has no working NIC transport (virtio-PCI, unsupported), so networking and
> the whole Plan 9 cluster are unreachable there (see
> [`testing-parallels.md`](testing-parallels.md) for the full analysis, kept as a
> "perhaps later" reference, not an active plan). **The intended physical target
> is now 2× Raspberry Pi 4** (real ARM hardware, ordered 2026-08-26): the Plan 9
> resource-sharing mechanics are a better fit for genuine physical machines than a
> VM, so a real two-node cluster on the Pis is the eventual real-hardware proof.
> A concrete Pi test plan is now written -- [`testing-pi4.md`](testing-pi4.md), 2026-08-28, ahead of the boards, with every claim labelled (predicted) or (confirmed) so the first bench session turns it into a log. Note its headline finding: **the Pi's GENET NIC is not virtio either**, so 2x Pi 4 does not by itself deliver the two-node cluster proof -- that needs USB-Ethernet over the existing xHCI stack, or a GENET driver, first. The
> `prlctl`/`make test-parallels` tooling below stays available but is no longer a
> priority.
>
> **Pi-4 bring-up reference (pre-read, for when the boards arrive):**
> `docs/research-redox-and-pi.md` (Part 2) maps the
> `rust-raspberrypi-OS-tutorials` repo onto our situation. The key call: **try
> the [pftf/RPi4](https://github.com/pftf/RPi4) EDK2 UEFI+ACPI firmware first** —
> a Pi 4 under it exposes UEFI + ACPI + a GOP framebuffer, so our existing boot
> path (UEFI loader, ACPI MADT → `gicv2.rs` for the Pi 4's GIC-400/GICv2, GOP
> `fbconsole`) should carry over largely unchanged, rather than rewriting for raw
> `kernel8.img` boot. The tutorials stay the fallback reference for the raw
> BCM2711 facts (peripheral base `0xFE00_0000`, GIC-400 at GICD `0xFF84_1000`/
> GICC `0xFF84_2000`, PL011-not-mini-UART, GPIO14/15 = ALT0, the serial rig:
> USB-serial to TX/RX/GND, **not** VCC). See [[project-physical-hardware-target]].

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

## POSIX / C-program portability: a userland libc personality (STARTED 2026-08-28)

> **Progress: the foundation is proven.** A C program now runs on Ouroboros
> (`libc/hello.c`, `make chello-bin`): clang → `aarch64-unknown-none` ELF →
> Rust's LLD against `programs/linker.ld` → the existing loader → the syscall
> boundary, spawned like any `/bin` program (`# chello` → `hello from C on
> Ouroboros`). No loader or kernel change was needed. That closes the one real
> uncertainty — the toolchain path — so the rest is *growing a libc*, not
> inventing the mechanism. **`.data`/`.bss` support landed next** (the second
> step): userland programs may now have mutable statics/globals — the loader
> already loaded initialized data and zeroed `.bss` per PT_LOAD segment, so this
> was removing the linker-script ASSERTs and verifying (fresh-per-spawn,
> `data=7 bss=0` → `data=8 bss=5`, RELATIVE relocs only). That was the real
> blocker for non-trivial C. **A minimal libc landed next** (third step):
> `libc/` now has standard headers + sources (`crt0`, syscall stubs, `printf`,
> `malloc`/`free` over `sbrk`, `string.h`) — a C program `#include`s `<stdio.h>`
> and calls `printf`/`malloc` (`make cdemo-bin`, `/bin/CDEMO`: formatted output,
> heap allocation, `sum(1..100)=5050`). **Remaining steps, in order:** (1) file
> I/O — `open`/`read`/`close`/`fstat` over `fsd`'s `NP_*` with an fd table, plus a
> stdout-target-aware `write` so a C program participates in pipes/redirection;
> (2) port `picolibc`/`newlib` on top; then C programs like SQLite and a small C
> compiler become "port one more program." See `docs/processes.md`'s "Writing a
> program in C." The reasoning below is the original parked plan, still accurate.

**The goal, restated honestly.** The original `notes.txt` intent was
"POSIX-ish system calls." What actually got built is *not* POSIX and not
Linux — it's a message-passing microkernel ABI (see
`docs/architecture.md`'s "Philosophy — not POSIX, not Linux" subsection):
a tiny syscall trap surface plus a set of userland servers reached by IPC,
and — via the cluster arc — the same verbs over TCP. That divergence was
*forced* by the microkernel/isolation work (a filesystem the kernel
depends on is a split, not a driver) and then *rationalized* by the Plan 9
direction (one uniform file protocol, per-task namespaces). **The decision
here is to keep that design, not to force POSIX back into the kernel** —
and to recover C-program portability the way real microkernels do: as a
**userland POSIX personality**, not a kernel ABI.

**The key realization: POSIX is a libc, not a kernel.** Existing C
programs call `libc` (`open`/`read`/`printf`/`malloc`), never raw
syscalls. So the port target is the *bottom edge of a libc*, whose stubs
translate into this project's existing server messages — `read(fd)` →
`FSOP_READ`/`NP_READ` to `fsd`, `write(1,…)` → `cond`, `socket`/`connect`
→ `netd`. The kernel and servers stay exactly as they are. This is a
solved shape, not a contradiction: **Fuchsia** (Zircon microkernel, *zero*
POSIX syscalls, pure message-passing channels) runs POSIX C programs via a
userland compat layer (musl + `fdio`); MINIX3 and Plan 9's APE do the
same. A message-passing microkernel running unmodified C programs is
normal.

**Shape of the work, when it's eventually picked up:**

- **Port a small libc** — `newlib` or `picolibc` first (designed for
  exactly this, a porting layer of ~17-20 "syscall stubs": `_open`,
  `_read`, `_write`, `_close`, `_lseek`, `_fstat`, `_sbrk`, `_exit`, a
  process-creation call). `musl` later for real completeness. Much of the
  substrate already exists: `read`/`write`/`lseek`/stat map onto
  `FSOP_READ_AT`/the `read_cursor`/`FSOP_MOUNT_INFO`; `_sbrk`/`malloc`
  onto the existing userland heap (`heap_info`); `_exit` onto `EXIT`.

- **The architectural mismatches** (not just missing functions — think
  about these before they can bite):
  - **`fork()` — the big one.** There is no `fork`, only `spawn` (a new
    task alongside the caller, no address-space copy). The honest answer
    (Fuchsia's answer): implement **`posix_spawn` natively** — it maps
    almost directly onto `SPAWN`/`SPAWN_STAGE`/`ARGS_STAGE` + the
    stdout-target flow — and accept that programs which `fork()` and keep
    running in *both* halves (not fork-then-exec) need porting. Most
    well-behaved programs are fork-then-exec, which `posix_spawn` covers.
  - **File descriptors.** POSIX wants integer fds with a stable open-file
    handle + cursor; the current protocol is **path-per-op** (each verb
    carries a path, no server-side handle — the Phase 0 fid deferral). An
    fd table mapping `fd → (server, handle, offset)` is a *userland*
    construct (libc/`fdio`), buildable entirely on top of today's servers.
  - **`select`/`poll`, signals, `mmap`.** The blocking primitives
    (`msg_recv`/`read_char`/`NET_WAIT`) are the substrate for poll; signals
    mostly get stubbed in a first port; anonymous `mmap` maps to region
    allocation, file-backed is harder.

- **The one connection worth remembering: a POSIX fd ≈ a Plan 9 fid.**
  Phase 0 *deferred* fids (verbs stayed path-based, which paid off over TCP
  in Phase 1). But an open-file handle with a cursor is exactly what a 9P
  fid is *and* exactly what a POSIX `fd` needs — so adding fids someday
  serves the "proper" 9P model **and** POSIX portability in a single move.
  Until then, the only thing to protect while the ABI is fluid is that the
  server protocols stay *expressive enough to build fids/fds on* (stat, a
  cursored handle, directory iteration, a poll-able wait) — which they
  already are. So the design isn't painted into a corner; it has a clean
  future step (fids) that pays off twice.

- **The existence proof to read first: Redox OS's `relibc`.** Redox is a
  Rust microkernel with exactly this architecture (non-POSIX kernel, POSIX
  in a userland libc) and it *ships* — real C/C++ programs and Rust `std`
  both run on it via `relibc`. Two transferable tricks from it:
  `relibc` **targets both Redox and Linux** (thin syscall wrapper on Linux,
  `libredox` on Redox), so the libc is host-testable before the OS backend
  exists; and Redox pushed **`fork`/`execve` into userspace** (`redox-rt`),
  synthesizing `fork` as `clone` without `CLONE_VM` — the answer to "but C
  calls `fork()`" without putting `fork` back in the kernel. See
  `docs/research-redox-and-pi.md`.

Not sequenced, not started — this is parked so the reasoning isn't lost.
It only matters once running third-party C code is actually a goal, which
is a long way off.

## North-star directions ("Polaris" planning pass, 2026-08-26, not sequenced)

A batch of longer-horizon directions captured together — what would move
Ouroboros from "a microkernel that boots, runs a shell, and clusters" toward
a system you could actually *live in*: a richer terminal, richer commands
with real argument handling, more of the standard command set, a security /
identity model, on-device compilation, and an honest map of what mainstream
Unixes still have that this doesn't. **None are designed or sequenced yet;**
each is recorded so the reasoning and the starting points aren't lost.
Several build directly on things that already exist (cond's small ANSI
parser, ext2's on-disk permission bits, the per-task capability model, the
cluster-auth HMAC, `ulib`, and the POSIX-libc plan above), which is the point
of writing them down now rather than from scratch later.

### 1. Terminal escape codes / VT100 (scoped-ish, the nearest of these)

cond already renders a **small ANSI parser** in the framebuffer backend
(cursor, wrap, scroll — see `CLAUDE.md`'s "Driver isolation, part 3"), so
this is *extending an existing subsystem*, not a new one. The goal is a
usefully-complete VT100/VT220-ish terminal: SGR colors + bold/underline/
reverse, cursor positioning (`ESC[H`, `ESC[<n>;<m>H`), line/screen erase
(`ESC[K`, `ESC[2J`), save/restore cursor, and scroll regions — the subset a
full-screen program (an editor, `less`, a `top`) needs to paint a screen.

**What exists to build on / the hard parts.** The rendering primitives
(`FB_BLIT`/`FB_SCROLL`/`FB_CLEAR`) are already gated to cond and already do
glyph runs + scroll, so *color* is mostly a per-glyph attribute added to the
blit path, and *positioning* is arithmetic cond already does for wrap. The
genuinely new pieces: a color-capable font blit (foreground/background per
cell), a real parser state machine (parameter accumulation, intermediate
bytes) rather than the current minimal one, and — the awkward one — an
**input** path for the responses some sequences require (cursor-position
report, device attributes), which today's one-way `NP_WRITE_FILE` output
model doesn't carry back. Reading the byte-stream UART backend (QEMU) is
straightforward; the framebuffer backend (Parallels) is the one that matters
and has no return channel yet. **Consumer question:** the first real
consumer is a full-screen program that paints and repaints a screen. Note the
**pager already shipped** (2026-08-27, `more`/`less`) *without* this — it
scrolls line-by-line and clears with the minimal `ESC[2J`/`ESC[H` cond already
has, so it isn't the consumer that forces the full escape set. The true
consumer is an **editor** (or a `top`), so build the terminal and its first
editor close together, driven by real need rather than guessed — the
keyboard-ownership arc (below/shipped) already cleared the "an interactive
`/bin` program can read keys" prerequisite both need.

### 2. Richer commands: flags, arguments, real option parsing (scoped, incremental)

Today's `/bin` commands take mostly positional arguments, and several are
deliberately minimal (the open-gaps list notes `grep` is still substring-only;
`ls -l`/`-a`, `grep -i/-v/-n`, `sort` (with `-r/-n/-u/-f`), and a `-?` usage
flag have since shipped).
The direction: give the existing commands the flags that make them actually
usable — `ls -l`/`-a`, `grep -i`/`-r`/`-n` (and eventually real patterns),
`rm -r`/`-f`, `cp -r`, `cat -n`, `head -n`/`tail`, `wc -l`/`-w`/`-c`
selection — plus a shared **option-parsing helper in `ulib`** so every
command parses `-x`/`--long`/`--` the same way instead of hand-rolling it.

**What exists to build on / the hard parts.** `ls -l` is the tell: it needs
a **richer stat surface** than the protocol exposes today — mode bits, size,
mtime, link count, uid/gid. ext2 *already stores* all of that (a guest-
written file showed up as `inode 12, 0644, 42 bytes`), so `-l` is partly a
matter of surfacing metadata the on-disk driver already reads through a
`FSOP_STAT`-shaped op — but FAT/exFAT have no Unix mode/owner, so the stat
surface has to degrade honestly per filesystem (the same "present what the
FS can model" discipline ext2 read-only already used). Recursive flags
(`-r`) want directory-tree walking in the client, which is new but small.
This is a broad-but-shallow arc — many small, independently-shippable
increments, each one command's flags — and it's the natural companion to
item 4 (the two are "make the command set real"). It also feeds item 5:
`chmod`/`chown` are exactly "a write path for the stat surface `ls -l`
reads."

### 3. More `/bin` commands (scoped, incremental)

The standard toolset still missing, roughly in cheapness order (`tail`, `nl`,
`rev`, `uniq`, and now `sort` already shipped): `tee`, `tr`, `cut`,
`find`, `du`, `df`,
`date`/`sleep` (both want a wall-clock the kernel already has via the timer
counter and `MONOTONIC_US`), `env`-as-a-program, `true`/`false`/`yes`, and a
`kill`-by-name. The **pager** (`more`/`less`) **shipped 2026-08-27** — the
keyboard-ownership arc (a foreground `/bin` program can read the keyboard)
was its enabler, so it's a `/bin` program now, not a builtin. The remaining
hard one is an **editor** — it needs item 1's cursor addressing plus item 4's
richer input, and it's the real consumer that would justify item 1's terminal
work.

**What exists to build on.** Every one of these is "a new crate under
`programs/<category>/` over `ulib`, found by PATH" — the externalization arc
is complete and the pattern is turnkey (a filter reads `pipe_recv`, writes
`write_out`; a fs command resolves against the delivered cwd and calls the
`fs_*` helpers), and the keyboard-ownership arc proved even an *interactive*
program (one that reads keys while running) can be a `/bin` binary — the pager
is the existence proof. So most of these are genuinely small. The one that
isn't: an **editor** (needs item 1's cursor addressing + item 4's richer
input). (`sort` — the one filter that can't stream — shipped by buffering the
whole input in its heap and sorting an in-place line index, with a documented
size cap.) Cheap wins first; the editor last, gated on item 1.

### 4. Login, users, security, file permissions (a substantial arc, medium-term)

> **Progress (2026-08-28): steps 1 (*identity*) and 2 (*login*) are done.**
> Step 1: a kernel-owned uid/gid per task (`SET_ID`/`GET_ID`, default root,
> inherited across spawn, root-gated), observable via `/bin/id`, with the prompt
> showing `#`/`$`. Kept the binding **in the kernel** (the unforgeable root of
> trust), reversing this section's earlier "probably userland" lean. Step 2: the
> shell **gates each session on a `login:` prompt**, authenticating against
> `/etc/passwd` (`SHA-256(salt‖password)`) and dropping to that user — using a
> POSIX **saved-uid** so logout restores root and re-prompts (chosen over a
> `login`-as-init process to avoid rewiring the capability model's "slot 0 =
> shell" assumption; a user can't escalate since `su` is root-only + children
> can't restore root). See `CHANGELOG.md` and `docs/gap-analysis.md` §6.
> Step 3 (*enforcement*) is now done too: `fsd`'s `check_access` gates every
> file verb — the caller's uid/gid (`GET_ID(sender)`) vs. the inode owner/mode,
> owner→group→other, root bypass, `FS_ERR_PERM` on refusal, `chmod` owner-only /
> `chown` root-only. ext2-only (FAT/exFAT unrestricted). **The core arc is
> complete.** Deferred refinements (smaller, unsequenced): the ancestor-directory
> search (`x`) traversal check, per-user *cluster* identity, `/etc/shadow`
> (passwords out of the world-readable `passwd`), `passwd`/`useradd`, per-user
> `/home` (home is `/` today), and groups.

The biggest of these — the step from "single implicit user, whoever's at the
keyboard" to a real **identity and permission model**. Pieces: a notion of
*users* (uid/gid — **done**), a **login** prompt gating the shell, credential
storage (a `/etc/passwd`-shaped file with hashed passwords), per-file
**ownership + permission bits** actually *enforced* on `fsd` operations, and
a privilege boundary for the operations that should need it (format, mount,
kill another user's task, the cluster export).

**What exists to build on — more than it looks.** Three foundations are
already in place: (a) **ext2 already carries uid/gid/mode on disk** — the
metadata a permission check needs is read today and simply ignored above
`fsd`; enforcing it is "check the caller's identity against the inode's mode
in the `FSOP_*` dispatch," which is why item 2's stat surface is the natural
precursor. (b) The **capability model** is already a per-task "who may do
what" mechanism at the IPC boundary — a user/permission layer is its
higher-level cousin, and the two want to be reconciled, not built in
parallel (a logged-in user's tasks get a capability set derived from their
identity). (c) **Cluster auth already has real crypto** — hand-rolled
SHA-256/HMAC (`netd/src/hmac.rs`, NIST/RFC-validated) and a
fail-closed key-file pattern (`CLUSTER.KEY` read once at boot via `fsd`) —
so password *hashing* and a `/etc/passwd` read reuse machinery that exists.

**The hard parts / open questions.** Who is the *kernel's* notion of a
user — the kernel schedules tasks, not people, so identity is likely a
userland construct (a login server / an identity carried in the capability
set) rather than a uid field in the task struct, matching the "personality
in userland" stance the POSIX section takes. FAT/exFAT can't store owners at
all, so enforcement is inherently ext2-only (or a shadow permission store) —
an honest per-filesystem degradation again. And the **cluster** angle is the
interesting one: the export currently authenticates the *machine* (shared
key), not a *user* — per-user identity across the cluster is explicitly a
named future tier in the cluster-auth postmortem, and this arc is where it
would land. This is a multi-milestone arc, not one task; sequence it *after*
item 2's stat surface exists, since permission enforcement has nothing to
check against until then.

**A mechanism to borrow from Redox: the namespace *is* the sandbox.** Redox
sandboxes a process by restricting which schemes (resources) its namespace
can *name*, down to a "null namespace" that leaves a daemon only its
pre-opened handles after init. Ouroboros already has both halves — per-task
namespaces (`bind`/NS_SET) and the capability send-mask — but hasn't joined
them (an empty namespace today means "unchanged," not "no access"). Making
the namespace the enforcement boundary is exactly the reconciliation point
(b) above wants, and Redox is the working model. Its RedoxFS also boots the
kernel off an **encrypted partition** — the reference for at-rest security
when this arc reaches disk encryption. See `docs/research-redox-and-pi.md`.

### 5. An on-device compiler: C and/or Rust (north-star, very large)

The self-hosting dream — compile a program *on* Ouroboros rather than
cross-compiling from the Mac. Recorded honestly because the scale is very
different for the two languages, and because it's tightly coupled to the
POSIX-libc plan above.

**C is the realistic target; Rust almost certainly isn't (near-term).** A
Rust compiler self-hosting is effectively out of reach — `rustc` is enormous,
assumes a hosted std/LLVM, and this project can't even PIE-link prebuilt
`liballoc` on stable (the recurring `-Z build-std` wall). A **small C
compiler** (`tcc`, `chibicc`, `cproc`+`qbe`) is a real possibility, but *only
on top of the userland libc personality* — a C compiler is a C program: it
needs `fopen`/`malloc`/`fork`-or-`posix_spawn`/`_exit`, i.e. it's a *consumer
of item "POSIX / C-program portability" above*, not independent of it. So the
honest sequence is: libc personality first, then a C compiler is "port one
more (large, self-contained) C program." Below even that, the realistic
*first* step toward on-device code generation is much smaller — an
**assembler** (text → the ELF the loader already parses) or a tiny toy
language — which needs no libc and would prove the write-a-program-then-run-it
loop end to end. **Consumer question, stated plainly:** on-device compilation
is a *want*, not a *need* — nothing here requires it, cross-compilation works
fine — so this is a "because it's the Ouroboros thing to do" goal (the name
is a snake eating its tail; a system that can build itself is the literal
endgame), sequenced behind everything with an actual consumer.

### 6. Document what MINIX / Linux / Unix have that Ouroboros doesn't (a doc task — the organizing exercise) — DONE (2026-08-26)

**Done — see [`gap-analysis.md`](gap-analysis.md).** A factual, per-subsystem
*have / partial / don't* inventory of the current boundary (process model,
syscall surface, VFS/fds, terminal, libc, users/permissions, networking,
devices, memory, scheduling, cluster, time, the utility set, init, and
observability), each row noting what it would take and which arc it maps to,
capped by a ranked "biggest gaps" synthesis. It confirmed the sequencing hunch
above: a per-file `FSOP_STAT` surface is the keystone (it gates `ls -l`, richer
flags, *and* permissions), and a POSIX libc + fds is the second. Original
framing kept below.

Not a feature — a **gap-analysis document**, and the meta-item that helps
sequence the other five. `docs/comparison.md` already frames Ouroboros
against MINIX/Linux/Unix/Plan 9/Helix as a "what you gain, what you give up"
table; this extends that from *philosophy* to a *concrete checklist*: the
syscalls, the libc functions, the `/bin` utilities, the subsystems (signals,
job control depth, pipes-to-files, TTY line discipline, `/dev`, users/groups,
mmap, dynamic linking, a real VFS with per-FS servers, sockets-as-fds, cron/
init/service management, swap/paging) — each marked *have / partial / don't*,
with a one-line "why not / what it would take" pointing at the relevant
roadmap arc.

**Why it's worth doing early.** It's cheap (a doc, no code), it's the
natural *input* to prioritizing items 1–5 (it surfaces which gaps are one
small program vs. a multi-milestone arc), and it's the kind of honest
self-accounting this project already values — the postmortems and the
POSIX-divergence reflection are the same instinct. The risk to avoid is
turning it into an aspirational feature list; keep it a *factual* inventory
of the current boundary, the way the open-gaps list tracks
specific known gaps, just organized as a coherent map rather than a running
list. This is the one to do *first* of the six, precisely because it tells
you the order for the rest.

### Additional directions (2026-08-27 batch, not sequenced)

A second batch, captured the same way. Several **extend items 1–6 above** rather
than being new — flagged as such so the roadmap doesn't fork — and the genuinely
new ones (links, a GPU, cluster data redundancy, SQLite) get the same "what
exists to build on / hard parts / consumer question" treatment.

**a. Users, login, passwords, permissions — and per-user home directories (extends item 4).**
Item 4 already scopes the identity/permission arc: a login prompt, an
`/etc/passwd`-shaped file with hashed passwords (reusing the cluster-auth
SHA-256/HMAC), and ext2 mode/uid/gid actually *enforced* at the `FSOP_*`
dispatch — ext2-only, because FAT/exFAT can't store owners. The addition here is
**per-user home directories**: a `/home/<user>` the login sets as the shell's
initial cwd — a small convention layered on the permission work, not a separate
arc. Still sequenced after item 2's stat surface (nothing to check against until
then).

**b. Links: hard links + symbolic links (new, ext2-only).**
The Unix link model, which ext2 already half-supports: an inode owns the data and
a directory entry is just `name → inode`, `fsd`'s ext2 arm already keeps
`i_links_count` consistent for `mkdir`/`rmdir`, and it already *reports*
(doesn't follow) symlinks. So a **hard link** is "a second directory entry
pointing at an existing inode, `i_links_count` bumped," and a **symlink** is "an
inode whose data is a target path." The work: `ln`/`ln -s` commands,
`FSOP_LINK`/`FSOP_SYMLINK` ops, and **symlink-following in path resolution**
(with loop detection / a depth cap) — the last is the only genuinely new
mechanism, and it's shared with item 4 (a `/home` symlink) and the stat surface
(link count + type in `ls -l`). **ext2-only** (FAT/exFAT have no link concept),
the same honest per-FS degradation as permissions. Small given the ext2
foundation; pairs with items 2/4.

**c. A text editor + full-screen terminal control (extends items 1 and 3).**
Item 1 (VT100/cursor addressing in `cond`) plus the editor already noted under
items 3/4 *are* this. The specific question raised — **"graphics mode only?"** —
is worth recording an answer to: the **framebuffer** backend (Parallels, the
real target) needs `cond` to grow real cursor positioning / erase / scroll
regions (item 1's core) *and* an input return-channel for the sequences that
need one (the awkward part item 1 flags); but the **byte-stream UART** backend
(QEMU serial) already passes ANSI straight through to a host terminal, so an
editor can be *developed and tested there first* and the framebuffer terminal
caught up to it. So: not graphics-mode-only, but the framebuffer is where the
real work is. Build the terminal and its first editor together (item 1's
consumer question).

**d. On-device compilers, C and Rust (extends item 5).**
Item 5 already covers this in full: a small **C** compiler (`tcc`/`chibicc`/
`cproc`+`qbe`) is realistic *on top of the userland libc personality* (a C
compiler is a C program); **Rust** self-hosting is effectively out of reach
(`rustc`'s size + the recurring `-Z build-std` PIE wall); and an **assembler** or
a tiny toy language is the small first step that needs no libc. No change —
recorded here as a pointer.

**e. Download and run a Rust toolchain — "GnuRust" / gccrs (new, the far end of item 5).**
The ambitious flip side of item 5: rather than *writing* a compiler, *acquire* a
prebuilt one — **gccrs** (the GCC Rust front end) or a ported Rust toolchain —
and run it on-device. The reality check makes it the furthest-out item here: it
needs (1) the POSIX libc personality mature enough to run a very large C++
program (gccrs is C++), (2) a filesystem with real capacity and enough RAM, and
(3) a **download/fetch flow** (the network stack + a `fetch`-to-file path exist;
a real package step doesn't). So it's a *consumer of the libc + fetch
capabilities*, even further out than a small C compiler — and, like item 5, a
"because it's the Ouroboros thing to do" goal, not a need. Recorded as the
north-star tip of the compiler direction.

**f. Graphics card / GPU support (new, large, QEMU-shaped start).**
Today the only "graphics" is the boot-discovered **GOP linear framebuffer** that
`cond` blits glyphs into — no acceleration, no mode-setting, no display-controller
driver. Real GPU support is a large hardware arc; the realistic starting point
(matching every other device here) is **virtio-gpu on QEMU** — a virtio device
over the existing `virtio_mmio` transport, like virtio-net/blk, giving
mode-setting and a 2D blitter under the same DMA-in-the-kernel /
protocol-in-userland split the whole system already uses. A real discrete GPU is
out of scope. **Consumer question, stated plainly:** nothing needs it yet — the
framebuffer console suffices, and the terminal/editor work (items 1/c) lives
happily on the plain framebuffer — so this is for an eventual windowing system /
graphical apps, sequenced behind everything with a nearer consumer. Note
virtio-gpu as the entry point when the time comes.

**g. Cluster data redundancy — documents failsafed across nodes (new, a later cluster phase).**
The cluster (see [`roadmap-cluster.md`](roadmap-cluster.md)) shares disk and
resources today, but a document lives on exactly **one** node — lose that node,
lose the file. The direction: **automatic replication** so data is mirrored
across cluster nodes and survives a node failure (a write on one node propagated
to others, with failure detection and recovery). This is a genuine
distributed-systems arc — the cluster-distributed postmortem deliberately scoped
**single-writer + clean-disconnect** and put concurrent-writer/replication *out
of scope*, so this is exactly where that boundary would be revisited: a
replication protocol, a consistency contract (quorum? primary-backup?), conflict
handling, and failure detection. Large, and gated on the consistency model being
worked out; it belongs as a later phase in `roadmap-cluster.md`, not a near-term
item. It's the strongest "why" the project has for going distributed *beyond*
resource-sharing. **The substrate to borrow from Redox: RedoxFS's shape** — a
small Rust filesystem (a daemon, exactly `fsd`'s model) with copy-on-write plus
**data *and* metadata checksums**, written from scratch rather than porting ZFS
(Redox tried the ZFS port and abandoned it as microkernel-hostile). Checksums +
CoW are the integrity substrate a replication scheme needs; RedoxFS is the "write
it small, don't port a giant" precedent. See `docs/research-redox-and-pi.md`.

**h. SQLite — an on-device database (new, the canonical first libc port).**
SQLite is a single-file, dependency-light **C library** — the textbook "port one
self-contained C program" target — so it's a direct **consumer of the POSIX libc
personality** (the section above): it needs `open`/`read`/`write`/`fsync`/
`lseek`, optionally a little `mmap`, and file locking. Once the libc runs C
programs, SQLite is a high-value, self-contained first real port (a real database
on the device) *and* an excellent libc **test case** — it exercises a large slice
of the file API and its own test suite is exhaustive. Recorded as a concrete,
motivating milestone for the libc arc: "the libc is real when SQLite runs on it."

## Open gaps (small, from the old parking lot)

Known small gaps, not yet sequenced (the *completed* parking-lot entries — USB
keyboard, GOP console, preemption, task destruction, driver isolation, etc. — are
in [`roadmap-completed.md`](roadmap-completed.md)):

- **`grep` has no regex** (it now takes `-i`/`-v`/`-n`, but matching is still a
  plain substring). Real patterns are a separate, larger arc — see North-star
  item 2 for the shared `ulib` option parser and richer matching.

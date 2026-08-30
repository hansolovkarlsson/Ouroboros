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
So is the users/permissions arc, as of 2026-08-30 — bar one item, which is why
that item leads this list rather than sitting in the north-star section it came
from. In rough order of value (item 1 is the next *arc*; 2 and 3 are what the
microkernel arc itself still leaves open):

1. **Per-user cluster identity — ATTEMPTED and REJECTED 2026-08-30; rebuild
   pending, design settled.**

   A first implementation (PR #42, kept open and marked *do not merge*) was
   rejected by review with fifteen findings, **five of them independent ways a
   remote request still reached `fsd` with root authority**. The wire design was
   right and survives: send the caller's **name** (not a uid — two nodes number
   users independently), **inside the MAC** (free, since the MAC was already
   there), resolved on the far side through its own `/etc/passwd`, refusing a
   name it does not know.

   **The mechanism was wrong, in a way worth stating before rebuilding.**
   Identity was attached by *opt-in* wrapper functions and held in a *latch*
   between two messages. Opt-in meant a missed call site was silent — one path
   (`fsd_write_at`) was simply forgotten, so remote `cp`/`>>`/`writeat` ran as
   root. The latch meant any other task's request to `fsd` cleared it, falling
   back to `netd`'s root. Four of the five holes came from those two properties,
   not from four separate slips.

   **The rebuild:** identity as a **required parameter** of the two chokepoints
   (`fsd_call3`, `read_file_chunk`/`fsd_write_at`) with an explicit
   `FirstParty` value for `netd`'s own reads — so "unproxied" does not compile —
   and carried **in the NP request** (offset 40 / `a3` is free) rather than
   latched, which removes the interleaving hole and covers `NP_RUN` at once.
   See [`unspellable-postmortem.md`](unspellable-postmortem.md).

   Two things the attempt did land, merged separately: the **ext2 two-node test
   rig** (the FAT32 pair cannot test permissions at all) and the `NP_WRITE_AT`
   **crash fix**. Also still open beyond the rebuild: **per-user keys** — what
   was attempted defends against the *users* of a trusted node, but a peer
   holding the shared key can claim any name.

   *The problem, unchanged:* the 9P export authenticates the *machine* (a shared cluster key), never a
   *user*, and `netd` relays every remote request under its **own root
   identity** — so `fsd`'s `check_access` hits its root bypass and
   short-circuits before any mode is consulted. Concretely, today: an
   unprivileged user on node B can `mount -r <A> /mnt/a` and read every hash in
   node A's `/etc/shadow`, and `cpu A passwd root` reaches node A's account
   server with root authority.

   **Not a regression** — the export has been machine- rather than
   user-authenticated since it was built, and `mount -r` is not root-gated.
   What changed on 2026-08-30 is the payload: before `accountd` there was no
   privileged writer on the far end of that hole.

   The design forks are real and unpicked, which is why this is an arc and not
   a follow-up: whether identity travels **in the 9P frame** (per request) or
   is negotiated **once per connection**; whether the cluster key stays a
   single shared secret or becomes **per-user**; and what a remote uid even
   *means* between two machines with independent `/etc/passwd` files (map by
   name? by number? a per-node translation table? refuse and require an
   explicit mapping?). The cluster-auth postmortem already named per-peer
   identity as a deliberate later tier — this is that tier.

   **Considered and not taken: a per-user `~/.shadow`.** Recorded here because per-user credential records are exactly what this arc
   will reach for, and the reasoning below is the thing to re-read when it does.

   The question: `/etc/shadow` is mode 0600 root, so a user cannot write their
   own password — which is the entire reason `accountd` exists. What if each
   user's secret lived in `~/.shadow` instead, owned by them at 0600? Then a
   user can write it with no privilege at all, root still reads it through the
   root bypass, and the server is unnecessary.

   **It works.** `login` already learns the home directory from the
   world-readable `/etc/passwd` before it knows who you are, so it can find the
   file; `passwd` becomes an ordinary program; a task slot, an IPC protocol and
   ~270 lines disappear. This is not a bad idea, and it is worth understanding
   why it was not taken rather than assuming it was never thought of.

   **What it costs is the property that makes a credential store worth having:
   the record stops being outside the control of the principal it
   authenticates.** Three consequences, ascending:

   - **`passwd`'s policy becomes advisory.** Its empty-password rejection — and
     any future length or complexity rule — is enforced in a program the user
     need not run. They can write the file directly with `writeat`, or compute
     a hash with their own program (there is a C toolchain). With a server, the
     server is the *only* writer and policy sits at a choke point.
   - **The old-password proof becomes unenforceable**, and that check's whole
     point is lost with it. It never protected against the user — they are
     already authenticated as themselves. It protects against *someone at their
     unattended terminal*, for whom overwriting a user-writable file is a
     one-liner.
   - **Disabling, expiry and lockout become impossible.** Root disables an
     account; the account's owner edits it back. Ouroboros has none of these
     today, so the cost is entirely future — but it forecloses the category
     rather than deferring it.

   Smaller structural warts: root's home is `/`, so root's record would be
   `/.shadow`; a service account with no home has nowhere to put one; and
   `useradd` grows more fragile, since the home would have to exist and be
   chowned *before* the password commits, undoing the ordering that makes
   `/etc/passwd` the single commit point.

   **The good idea inside it is separable, and worth keeping** — see the
   `/etc/shadow.d/` follow-up below. The *split* (one record per user) is sound
   on its own; it is putting the split somewhere the user **owns** that gives
   away the guarantee. Split and ownership are independent choices, and only
   the second one is the problem.

2. **General / transitive capability delegation.** The delegation shipped
   2026-08-21 is deliberately coarse: one delegated target per task,
   non-transitive, in practice shell-only. Making it general (any task hands
   any held capability onward, revocably — MINIX's full grant model) would
   unlock true relay-free `a | b | c` and a spawned program running its
   *own* server. The catch: **neither consumer exists yet**, so building
   this first would repeat the "premature, a mechanism without a hard
   consumer" trap the capability-and-hardening postmortem flagged for
   delegation itself. Build the consumer first, or wait until one is
   actually wanted.

3. **Per-task ASIDs, revisited** — a pure TLB-flush-per-switch optimization
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
- **Users, permissions & account management** (2026-08-28 → 2026-08-30) — a
  kernel-owned identity per task, a login gate, `fsd` permission enforcement
  with ancestor-`x` traversal, `/etc/shadow`, supplementary groups, the
  on-device account tools over a shared pure `accounts` crate, and finally
  `accountd` — a fourth server (protected slot 5) so a user can change their *own*
  password — with the message credential bound at **send** underneath it.
  **One item remains and it is the next arc, promoted to the frontier below:
  per-user cluster identity.**

## Remaining follow-ups from completed arcs (small, unsequenced)

The small open tails those arcs deliberately left:

- **ext4.** Much larger (extents, journaling, htree, checksums, 64-bit) and
  the no-alloc fixed-buffer constraint makes a big FS genuinely harder — a
  separate large arc, not a near-term ext2 follow-on.
- **A `/dev` namespace.** Only if multi-disk/partition addressing arrives (the
  Plan 9 devfs direction); nothing to name yet with one block device.
- **`/etc/shadow.d/<name>` — one credential record per user, in a *root-owned*
  directory** (dir 0755 root, files 0600 root). The salvageable half of the
  `~/.shadow` idea above: it keeps the per-user split and drops the per-user
  ownership, so `accountd` remains the only writer and every policy check stays
  at its choke point. Three concrete wins, none of them speculative:
  - **It bounds the read by construction.** A whole-file read of `/etc/shadow`
    reporting `0` on overflow is what locked out every account *including root*
    at ~23 entries (see the ledger below). That was fixed by streaming one line;
    a per-user file makes the bug unrepresentable instead of handled.
  - **It removes the whole-file rewrite**, and with it the reason
    `accounts::changed_span` and the write-only-the-differing-bytes path had to
    exist — those were written because truncating the shared file would lock
    everyone out mid-update.
  - **It is probably the shape per-user cluster identity wants**, since a
    credential that must be named per user across machines is already a
    per-user record.

  Not urgent: the streaming read and the non-destructive write already close the
  failure modes it would prevent. It is a simplification with a security
  argument, not a fix.

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
> heap allocation, `sum(1..100)=5050`). **File I/O + pipe-aware output landed
> next** (fourth step): `open`/`read`/`write`/`close`/`lseek`/`fstat` over `fsd`
> with an fd table, and a stdout-target-aware `write` so a C program works in a
> pipeline (`make cfile-bin`, `/bin/CFILE`: writes a file, reads it back;
> `cfile | grep hello` filters its output). **Fids landed next** (fifth step): fsd
> gained real server-side open-file handles (`NP_OPEN`/`NP_PREAD`/`NP_PWRITE`/
> `NP_FSTAT`/`NP_CLUNK`, a per-client fid table, permission checked once at open),
> the C libc uses them, and they coexist with the path verbs — the
> deferred-since-Phase-0 "a POSIX fd ≈ a 9P fid" feature, paying off for both C
> portability and the 9P model. **picolibc landed next** (sixth step, the real C
> library): `picolibc` 1.8.9 is built `-fPIC` (so it self-relocates under our
> loader — `R_AARCH64_RELATIVE` only, zero `ABS64`) and linked against OUR
> porting layer — the same `crt0`/syscall stubs (`write`/`read`/`open`/`sbrk`/
> `_exit`), which is exactly what picolibc's `posix-console` stdio bottoms out
> at, plus two 128-bit-shift builtins its float printf needs (`libc/pico/
> builtins.c`). `make cpico-bin`, `/bin/CPICO`: **full `%f`/`%e`/`%g` float
> formatting** (ryu), `snprintf`, `qsort`, `malloc`, `strtol` — unmodified
> standard C the hand-rolled libc couldn't run. The prebuilt static lib + headers
> are committed under `third_party/picolibc-prebuilt` (regenerate with
> `scripts/build-picolibc.sh`), so `make` needs no meson/ninja. **The arc's one
> open follow-up — picolibc's unbuffered console stdout — closed 2026-08-29**:
> stdout is line-buffered at the `write` boundary (in `file.c`, so it serves
> whichever C library is linked), stderr and a read-from-stdin stay unbuffered,
> and exit flushes from `_exit` — which also fixed a real hang, since a picolibc
> program links picolibc's `exit()`, not our `stdlib.c`'s, so it had never been
> sending a pipe consumer its end-of-stream marker (`cpico | wc` hung). See
> `CHANGELOG.md`. **Remaining:**
> port a real application on top (SQLite, a small C compiler) — now "port one
> more program," not "invent the mechanism." See `docs/processes.md`'s "Writing a
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

**Shape of the work:**

- **~~Port a small libc~~ — DONE (picolibc, 2026-08-28).** `picolibc` is
  ported and running (`/bin/CPICO`: `%f`/`%e`/`%g` float printf, `snprintf`,
  `qsort`, `malloc`, `strtol`), built `-fPIC` so it self-relocates under the
  existing loader with zero `ABS64`, linked against the same syscall stubs the
  hand-rolled libc used (`write`/`read`/`open`/`sbrk`/`_exit` — picolibc's
  `posix-console` stdio bottoms out at exactly those). No kernel/loader change.
  The full six-step arc (first C program → `.data`/`.bss` → minimal libc → file
  I/O + pipes → fids → picolibc) is recorded in `roadmap-completed.md` and
  `docs/libc-arc-postmortem.md`. **The mechanism is done; the remaining bullets
  below are the still-forward parts.**

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

- **~~The one connection worth remembering: a POSIX fd ≈ a Plan 9 fid~~ —
  DONE (fids, libc arc step 5).** Phase 0 *deferred* fids (verbs stayed
  path-based, which paid off over TCP in Phase 1). The libc arc cashed the
  deferral: `fsd` now has server-side open-file handles (`NP_OPEN`/`NP_PREAD`/
  `NP_PWRITE`/`NP_FSTAT`/`NP_CLUNK`, a per-client fid table, permission checked
  once at open), a fid is directly usable as a C fd, and they coexist with the
  path verbs — one feature serving the 9P model *and* POSIX portability, exactly
  as predicted.

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

**Status (2026-08-28): the mechanism is built, the arc's remainder is
forward-looking.** The six-step libc arc is complete through a running picolibc
(see `roadmap-completed.md` for the sequenced record and
`docs/libc-arc-postmortem.md` for the retrospective). What remains is genuinely
different in kind — "port one more program": a real application (SQLite, a small
C compiler), plus the still-open architectural mismatches above (`posix_spawn`
native / `fork` in userspace à la Redox's `redox-rt`, `select`/`poll`/signals/
`mmap`). Those matter only once running third-party C code is an active goal.

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

### 4. Login, users, security, file permissions — DONE (2026-08-28)

**Complete.** The full arc — identity, login, enforcement, and account
management — is finished; the sequenced plan-shaped record moved to
[`roadmap-completed.md`](roadmap-completed.md) and the milestone log is in
[`CHANGELOG.md`](CHANGELOG.md). Retrospectives:
[`users-and-permissions-postmortem.md`](users-and-permissions-postmortem.md)
(steps 1–3) and
[`account-management-postmortem.md`](account-management-postmortem.md) (step 4).
What shipped: a kernel-owned uid/gid per task; a `login:` gate over
`/etc/passwd`; `fsd` permission enforcement (ext2); and the account tools
(`passwd`/`useradd`/`groupadd`/`usermod`, `su`/`id` by name, `/etc/group`
primary-gid groups, `/Users` homes + `~`) on a shared host-tested `accounts`
crate, plus creator-owned new inodes.

**Still open (deferred refinements, unsequenced):**

- **Self-service `passwd`** — a non-root user changing their own password needs
  a privileged path: a dedicated **`accountd`** server (the `accounts` crate is
  built to slot into it) or a setuid mechanism. Root-only tools ship today.
  **In flight** as PR #30: the server exists, builds and boots, and `passwd`
  becomes a pure IPC client of it — held back with five code-review findings
  outstanding, including a `/etc/shadow` mode predicate and a recycled-slot
  TOCTOU (the kernel's message carries a bare slot number with no generation
  counter, so a sender that exits and is replaced between send and dequeue is
  authorised as its successor).
- ~~**A virtio-entropy RNG**~~ — **shipped 2026-08-29.** A `virtio_rng.rs` driver
  (one virtqueue, device-writable descriptor, polled) behind a `RANDOM` syscall;
  `accounts::salt_from` takes the bytes and reports whether the salt is strong,
  so `passwd`/`useradd` use real entropy where a device exists and say "no
  hardware RNG - using a weaker clock-derived salt" where it doesn't. `make esp`
  targets `run-image`/`run-image-ext2` now attach `-device virtio-rng-device`;
  the other targets deliberately don't, so the degradation path stays exercised.
  Verified by creating the same account on three boots: the two with the device
  produced different salts, the one without printed the warning.
- ~~**Supplementary group membership**~~ — **shipped 2026-08-29.** `SET_ID`'s
  `arg2`/`arg3` carry a supplementary gid list (`MAX_SUPP_GROUPS` 8) alongside
  the packed identity word, so identity and membership change in ONE call and a
  session can never keep the previous user's groups; `GET_GROUPS` reads it back,
  a child inherits it at spawn, and `fsd` grants the group triad on a primary OR
  supplementary match. `usermod -G` sets the list, `id` prints it. Setting a
  non-empty list is root-only — membership is a privilege grant, so it is gated
  separately from the identity change it travels with.
- ~~**`/etc/shadow`**~~ — **shipped 2026-08-29.** The salts and hashes moved out
  of the world-readable `/etc/passwd` (now four fields) into `/etc/shadow`, mode
  0600 root-owned, which `fsd`'s enforcement makes genuinely unreadable to a
  non-root user on ext2. Legacy 6-field lines still verify and `usermod`
  migrates them. The lookup STREAMS one line rather than reading the whole file:
  a whole-file read reports 0 on overflow, which for `/etc/passwd` safely means
  "no accounts, start a root session" but for `/etc/shadow` means "no secret"
  and locked out every account, root included, at ~23 accounts.
- ~~**Ancestor-directory `x`-traversal**~~ — **shipped 2026-08-29.** Enforcement
  walks every ancestor's search bit, not just the object and its parent.
- **Per-user cluster identity** — **the only item of this arc still open, and
  promoted to "What's next" above on 2026-08-30** once `accountd` gave the hole
  a privileged writer on the far end. The 9P export authenticates the *machine*
  (shared key), not a *user*.
- ~~**Symbolic-mode `chmod`** (`u+x`)~~ — **shipped 2026-08-29** (`u+x`, `go-w`,
  `a=rx`, `u+rw,go+r`, copy-source `g=u`, conditional `X`, `s`/`t`; octal still
  works and stays absolute). A real `/etc/skel` for `useradd` **also shipped
  2026-08-29** (top-level files copied into a newly created home, owner + mode
  carried across; absent by default, subdirectories skipped). Its twin, **`chown` by name**
  (`chown alice:staff`, resolved via the `accounts` crate like `su`/`id`), also
  **shipped 2026-08-29** - numeric ids still work, and an all-digits field stays
  an id.

**A mechanism to borrow from Redox: the namespace *is* the sandbox.** Redox
sandboxes a process by restricting which schemes its namespace can name (down to
a "null namespace"). Ouroboros has both halves — per-task namespaces (`bind`/
`NS_SET`) and the capability send-mask — but hasn't joined them (an empty
namespace means "unchanged," not "no access"). Making the namespace the
enforcement boundary is the reconciliation the self-service/privilege work wants;
Redox is the working model (and RedoxFS's encrypted partition is the reference
for at-rest security). See `docs/research-redox-and-pi.md`.

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
virtio-gpu as the entry point when the time comes. **The whole GUI stack above
this** — how far up toward SDL/GTK it could go, which layer actually blocks, and
why a Plan 9 `/dev/draw`-shaped `drawd` server (not an `SDL_Surface` pixel-ship
model) is the fit for a 768-byte inline ABI with no shared memory — is worked out
in [`research-gui-stack.md`](research-gui-stack.md). Its finding: the mouse
driver and a `drawd` draw server are the two steps that unlock everything else,
and the pixel-transfer model is the day-one decision to get right.

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

## Review findings against shipped code (2026-08-29 →)

Raised by code review of the security tier and **verified**, but left unfixed
at the time because they concern code already on `main` rather than the branch
under review. Recorded here so they are not lost with the review transcript.

Kept as a **ledger**: an item that gets fixed is struck through with the date
and left in place, rather than deleted. Two reasons. A reader wants to know a
hazard was *considered*, not just that it is absent today; and the section
would otherwise silently shrink into looking like nothing was ever found.

- **The 9P export bypasses permissions entirely.** `netd` relays a remote
  request to `fsd` under its *own* root identity, so `check_access`'s
  `if uid == 0 { return true }` short-circuits before any mode is consulted —
  and `mount -r` is not root-gated (the shell's only `shell_uid() != 0` check is
  `cmd_su`). On the two-node rig, an unprivileged user on node B can
  `mount -r <A> /mnt/a; cat /mnt/a/etc/shadow` and read every hash on node A;
  `cpu A passwd root` is a second door, since a spawned remote child inherits
  netd's root. **Not a regression** — the export has always been
  machine-authenticated rather than user-authenticated — but `/etc/shadow` gives
  it a payload it did not have before — and `accountd` (2026-08-30) sharpened
  that considerably: `cpu A passwd root` now reaches a *privileged writer* on
  the far end, not just a readable file. This is the concrete argument for
  **per-user cluster identity**, which was promoted out of the north-star
  section to the top of "What's next" on 2026-08-30 precisely because of it.
  **This finding is the specification for that arc**, and closes with it.
- **`fsd`'s per-request cost multiplied** when ancestor-`x` traversal landed:
  `path_allows` now costs 2 + (ancestors + 1) + 1 path resolutions where it cost
  1, `NP_OPEN` with `O_RDWR` does three ancestor walks, and `caller_id` issues
  both credential syscalls (`SENDER_ID`/`SENDER_GROUPS` since 2026-08-30, when
  the recycled-slot escalation below was closed; `GET_ID`/`GET_GROUPS` before
  that) two or three times per request — including fetching an 8-gid list for a
  root caller that returns immediately. (2026-08-30: those two calls are now
  `SENDER_ID`/`SENDER_GROUPS`, which read a captured cell rather than
  validating a live slot — marginally cheaper, but the *count* is unchanged, so
  this finding stands.) Same shape
  as the v0.4.1 FAT32 O(n²) read that ran past the supervisor's runnable-wedge
  and got `fsd` restarted mid-read; the cost is milliseconds per sector on real
  USB-MSD, not QEMU virtio-blk. **Unmeasured on hardware.**
- **`NP_STAT`/`NP_CHMOD`/`NP_CHOWN` skip the "does this filesystem model modes?"
  short-circuit**, calling `ancestors_searchable` directly instead of going
  through `path_allows`. On FAT32/exFAT — which record no mode, so every other
  verb short-circuits to allow — a non-root `cat ../f` succeeds while
  `ls -l ../f` is refused. Every `ls -l` entry also pays a guaranteed-useless
  ancestor walk. Note `check_access` is **default-allow** (`_ => true`), so a
  future `NP_` verb added without an arm here ships unauthenticated.
- ~~**A server authorized on the *current* occupant of the sender's slot.**~~
  **Fixed 2026-08-30.** `GET_ID(sender)` answered "who occupies slot N now",
  not "who sent this": a non-root task could `MSG_SEND` (non-blocking), `EXIT`,
  and have its slot reaped and re-spawned before `fsd` drained its mailbox, at
  which point the request was authorized as whatever landed there — root, if a
  root command did. Slots 5+ are the pool the shell recycles for every command,
  so this was the ordinary path, not an exotic one. The earlier `is_live` guard
  closed only the *dead*-slot half; a recycled slot is alive and
  indistinguishable, because a message carries a bare `u8` slot number with no
  generation. The kernel now binds the sender's credential at send
  (`SENDER_ID`/`SENDER_GROUPS`) — see `docs/architecture.md`'s syscall table.
  Raised against the unmerged account server, but it was `fsd`, in shipped
  code, that had it on every permission check and every fid op. Written up in
  [`asking-the-right-question-postmortem.md`](asking-the-right-question-postmortem.md).
- ~~**One malformed export frame could kill the network for the boot.**~~
  **Fixed 2026-08-30** (#44). `NP_WRITE_AT` sliced `&payload[p0..p0 + dlen]`
  with the range *start* unclamped, and two sibling arms had a wrapping add that
  put `end` below a clamped `start` — both panic, and a panic in `netd` parks it
  and burns a supervisor restart. Fixed as a class with one clamping helper.
  Raised by the review of #42 as pre-existing; proven both directions with the
  host-side Python peer. Note the `-d int` health bar reads `0` either way: a
  userland panic parks a task rather than raising a CPU exception, so the signal
  is the supervisor's restart line.
- **`warn_if_unprotected` fails open** — but does *not* misfire today, which is
  a distinction worth keeping straight. **Tested 2026-08-29 on all three
  images**: the warning correctly appears on FAT32 and exFAT and correctly stays
  silent on ext2, so the mitigation works as designed and this is not a live
  hole.

  The structure is still wrong, and it is the *reason* it works that should
  worry us. `mounted_fs_unprotected` returns `false` — "this filesystem enforces
  permissions" — for **any** non-zero `FSOP_MOUNT_INFO` status, including the
  `NO_FS` that means "`fsd` has not finished mounting yet". It is the first
  statement of `login()`, and it has **no retry**, while `read_account_file` in
  the same file carries a bounded 200-try `NO_FS` retry *precisely because login
  can beat the mount*. One of those two functions is defended against a race the
  other ignores.

  It passes on QEMU because virtio-blk mounts before login asks. The device that
  would lose that race is USB-MSD on real Parallels, which is measurably slower
  and where [`testing-parallels.md`](testing-parallels.md) already flags a
  `netd` boot-race risk for the same reason — and where the failure is silent,
  since the whole symptom is a warning that *doesn't* print. Same shape as the
  xHCI keyboard↔storage contention: invisible on the emulator, real on hardware.

  Two further defects in the same three lines, both unexercised today: it
  inspects only tree 0 (a multi-mount with `/etc` on a different tree
  misreports) and string-matches `"ext2"` (a future mode-modelling filesystem
  would raise a false alarm). `fsd` already derives this structurally from
  `stat().mode.is_some()`, which is what this should ask instead.
- ~~**`libc/include/sys.h`'s `FS_ERR_MIN` had drifted from the Rust
  constant.**~~ **Fixed 2026-08-30** (#37). The C header hand-mirrored the
  reserved-error floor at `MAX-33` while `accountd`'s codes moved it to
  `MAX-38`, so a C caller would have read `ACCT_ERR_IO` as a *successful*
  return value. No live consumer (no C program calls `accountd`), which is
  exactly why nothing caught it. The Rust definition now carries a note back to
  the mirror, since the definition is what gets edited next. *Recorded as a
  strike-through rather than deleted, per the ledger note above — it was
  removed outright when fixed, which was the wrong call and is corrected here.*
- **`useradd` accepts an empty password** while `passwd` now rejects one, so an
  account created by pressing Enter twice is loginable by pressing Enter. It is
  the only writer of an *initial* secret, so it is the one that most needs the
  check. Pre-existing.

## Open gaps (small, from the old parking lot)

Known small gaps, not yet sequenced (the *completed* parking-lot entries — USB
keyboard, GOP console, preemption, task destruction, driver isolation, etc. — are
in [`roadmap-completed.md`](roadmap-completed.md)):

- **An intermittent failure of `cp` across a remote mount, observed once.**
  2026-08-30, on the two-node ext2 rig: `cp /mnt/a/README.TXT /mnt/a/COPY.TXT`
  returned `cp: failed` in one run and succeeded in the two that followed,
  including a re-run of the byte-identical script. Recorded rather than
  dismissed, because an intermittent failure that is not written down is
  indistinguishable from one nobody has hit yet.

  **Not the wire-clamp change** (`wire_slice`, the same day): that rewrite is
  provably a no-op on every input the old expression did not panic on — for
  `off <= len` the two produce the same range, since `len - start` *is* the
  old `saturating_sub`, and they diverge only where the old form's range start
  was out of bounds. So the cause is older than that fix and still unknown.
  Suspicion, untested: the export is stop-and-wait, and a remote `cp` is the
  longest chain of round trips any command makes — a dropped segment plus the
  RTO is the obvious candidate, and `net-ext2-*.pcap` from a failing run would
  settle it. Reproducing it is the first step, and may take a loop.
- **`mv` cannot replace an existing destination.** All three filesystem arms
  return `AlreadyExists` if the target name is taken, so `mv a b` fails where
  every Unix would overwrite `b`. Found 2026-08-30 while fixing the account
  server's destructive-rewrite finding, whose obvious fix — write a temp file,
  rename over the target — has nowhere to land because of this. (That finding
  got a better fix anyway: write only the bytes that differ. But the *general*
  "update a file without a window in which it is empty" pattern still has no
  primitive.) The user-visible half is a one-line surprise; the interesting half
  is that a real replace should be **atomic**, and `ext2` can very nearly manage
  it — replacing a name means overwriting one directory entry's inode number in
  place, a single block write, after which the name resolves to the new content
  and the old inode is merely orphaned (leaked blocks, `e2fsck`-fixable) rather
  than the name being briefly absent. FAT32/exFAT have no inode indirection, so
  there it is unavoidably delete-then-rename with a window, and that difference
  should be stated rather than papered over.
- ~~**`grep` has no regex**~~ — **shipped 2026-08-29.** Patterns are POSIX
  **extended** regular expressions (`.` `*` `+` `?` `[...]` `^` `$` `|` `(...)`),
  via a new pure, host-tested **`regex` crate** at the repo root; `-F` keeps the
  old literal-substring behaviour. Bounded by design (an explicit backtracking
  stack, not recursion; empty-body repeats refused so every accepted pattern
  terminates; a step budget whose exhaustion reports `Limit`, never a silent
  "no"). Still open, each a real addition rather than a tweak: back-references,
  `{n,m}` counted repetition, `[:alpha:]` class names, and submatch capture —
  plus the shared `ulib` option parser of North-star item 2, still unbuilt. The
  `regex` crate is deliberately reusable: an editor's search and a `find` are
  the next consumers.
- ~~**`useradd` is not atomic**~~ — **fixed 2026-08-29.** The `/etc/passwd` write
  is now the single commit point: the group entry and home directory are prepared
  first, a failed prep commits nothing and exits non-zero, and a failed commit
  rolls the prep back (`accounts::remove_line`, `rmdir`). See `CHANGELOG.md`.
- ~~**Three near-identical small-file readers**~~ — **the two shell copies merged
  2026-08-29** into one `read_account_file` (carrying login's boot-time `NO_FS`
  retry), used by `login`, `su`, and `id`'s name lookups. `ulib::read_file_all`
  stays separate by design — it lives in the `/bin` programs, and the shell has
  its own fs layer. Likewise `ulib::read_line` still duplicates
  `login::read_field` (the same split); consolidate if the shell ever gains a
  `ulib` dependency.

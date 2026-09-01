# Ouroboros vs. the systems it draws from — what you gain, what you give up

This is the honest, user-facing comparison: if you already know MINIX,
Linux, Unix, Plan 9, or Helix, what do you *get* and what do you *lose* by
looking at Ouroboros? It's deliberately blunt about the losses — Ouroboros
is **pre-alpha**, a from-scratch research OS, and most of what a mature
system gives you it does not.

For the *design-influence* lens (what Ouroboros deliberately borrowed from
each and why), see `docs/research-directions.md`, `research-minix-boot.md`,
and `research-helix-os.md` — those are about ideas absorbed. **This doc is
about the practical trade for a would-be user.**

## What Ouroboros actually is (the baseline for every row below)

- **ARM64 (aarch64) only**, boots as a **UEFI application**; primary target
  is a VM (QEMU dev loop + Parallels on Apple Silicon), confirmed on real
  Parallels hardware.
- A **microkernel**: a tiny syscall trap surface; the filesystem (`fsd`),
  console (`cond`), and network (`netd`) run as **isolated userland
  servers**, reached by message-passing IPC. Faults are contained
  (MMU/EL0-enforced per-task isolation) and crashed servers are
  **supervised and restarted**.
- **Plan 9-shaped**: one uniform file protocol (the `ninep-abi` verb set),
  **per-task namespaces** with `bind`, and a real **distributed cluster** —
  the same verbs run over TCP, so another machine's disk, `/proc`,
  `/dev/cons`, and `/net` are mountable, and `cpu host cmd` runs a program
  on another node while it reads *your* files at `/host`.
- **Written in Rust** (plus a little assembly): memory-safe kernel and
  servers, small auditable trusted base.
- **Filesystems**: FAT32, exFAT, ext2 — all read *and* write.
- **Networking**: a hand-rolled stack (ARP/IPv4/ICMP/UDP/DNS/TCP) with a
  concurrent HTTP server.
- **Preemptive multitasking**, round-robin, **single core** (no SMP), no
  priorities.
- **Not POSIX, no `fork`** (a `spawn` model instead); **a libc exists as a
  userland personality — picolibc is ported** (C programs run: float `printf`,
  `qsort`, `malloc`), though no real *application* is ported yet and there's no
  dynamic linking; programs are position-independent `aarch64-none`
  binaries.
- **Accounts and permissions exist, but cluster keys are per-machine** — there
  is a real user model (`/etc/passwd`, `/etc/shadow`, groups, `login`, enforced
  file modes on ext2), and a remote request carries the requesting user's name so
  the far side applies its own permissions to it. What is missing is per-user
  *keys*: authentication is per-**machine** (an Ed25519 keypair per node since
  v0.16.0), so an authorized machine can claim any of its own users' names — and
  there is no on-the-wire encryption. Trusted-LAN by
  design. Not self-hosting (you cross-compile from macOS).

Keep that list in mind: the "you gain" columns are real, but every "you
give up" column is measured against a system that is *finished* and one
that is *barely begun*.

## At-a-glance capability matrix

| Dimension | Ouroboros | MINIX 3 | Linux | Traditional Unix | Plan 9 | Helix |
|---|---|---|---|---|---|---|
| Kernel model | Microkernel | Microkernel | Monolithic | Monolithic | Hybrid (file-server kernel) | Microkernel (layered, trait-based) |
| Memory-safe impl language | **Rust** | C | C | C | C | **Rust** |
| Fault isolation of drivers/servers | **Yes (MMU + supervised restart)** | Yes (reincarnation server) | No (in-kernel) | No | Partial | Yes (hot-reload focus) |
| POSIX / C-program portability | **Partial** (userland libc: picolibc ported, no app yet) | Yes | Yes | Yes (the standard) | Partial (APE) | No |
| "Everything is a file" / namespaces | **Yes (per-task ns + bind)** | Partial | Partial | Partial | **Yes (the origin)** | No |
| Distributed / network-transparent | **Yes (9P-over-TCP, remote cpu)** | No | Add-on (NFS, etc.) | Add-on | **Yes (native 9P)** | No |
| SMP / multicore | No | Yes | Yes | Yes | Yes | Varies |
| Architectures | aarch64 only | x86, ARM | Nearly all | Many | Several | Early-stage |
| Users / permissions / auth | **None** | Full | Full | Full | Full (factotum) | Minimal |
| Self-hosting (builds itself) | No | Yes | Yes | Yes | Yes | No |
| Maturity | **Pre-alpha** | Mature | Production | Decades | Mature/stable | Early-stage |

## Per-system: what you gain, what you give up

### vs. MINIX (3)

| You gain | You give up |
|---|---|
| A **real distributed cluster** (Plan 9 9P-over-TCP: remote disk/proc/console/net, remote `cpu`) — MINIX has no equivalent. | Maturity and a **full server fleet** — MINIX has VFS, process manager, network, and many drivers; Ouroboros has three servers. |
| **Rust** memory safety end-to-end vs. MINIX's C. | **POSIX compatibility** — MINIX runs real Unix software and is self-hosting; Ouroboros runs neither yet. |
| **Per-task namespaces + one uniform verb set** rather than MINIX's fixed service topology. | A proven **reincarnation/driver framework** with years of hardening across x86 *and* ARM. |

*Shared DNA:* both are microkernels with supervised, restartable userland
servers and capability-style copy IPC (`grant`/`safecopy` ≈ MINIX's
`safecopy`). Ouroboros absorbed MINIX's isolation-and-recovery idea; MINIX
gives you the mature, POSIX version of it.

### vs. Linux

| You gain | You give up |
|---|---|
| **Fault isolation**: a driver/filesystem/network crash is contained and *restarted*, not a kernel panic. | **Essentially everything practical** — Linux runs the world: vast hardware support, every language/runtime, full POSIX. |
| A **tiny, auditable trusted base** in memory-safe Rust vs. a huge monolithic C kernel. | **SMP and performance**, a real security model (users, permissions, namespaces, cgroups, seccomp), mature filesystems and networking. |
| A **clean distributed model** (resources as files over TCP) built in, not bolted on. | Ubiquity — Linux is one `apt install` from anything; Ouroboros runs a handful of programs on one VM target. |

*In one line:* you trade all of Linux's breadth and maturity for a small,
isolated, comprehensible microkernel you can read in an afternoon.

### vs. traditional Unix (the SUS/POSIX model + the Unix philosophy)

| You gain | You give up |
|---|---|
| Plan 9 took "everything is a file" **further than Unix and made it distributed** — Ouroboros inherits that: remote resources are files, namespaces are per-process. | The **POSIX ABI itself** — the entire point of Unix portability, where C programs just compile and run. Ouroboros is "Unix-*ish* in feel" only. |
| A **memory-safe microkernel** instead of a monolithic C design. | `fork`/`exec`, signals, users/permissions, and the enormous portable software ecosystem and standards conformance. |
| Small pipeline filters (`grep`/`wc`/`head`/`tail`/`nl`/`rev`/`uniq`/`upper`) and a shell keep the **Unix philosophy** in spirit. | Real completeness — Unix is a specification with implementations; Ouroboros is one unfinished experiment. |

### vs. Plan 9

*The closest sibling — the model Ouroboros is deliberately following.*

| You gain | You give up |
|---|---|
| **Rust** memory safety (Plan 9 is C). | **Completeness and coherence** — Plan 9 is a finished OS: 9P everywhere, a real window system (rio/acme), a full toolchain, self-hosting, multiple architectures. |
| **MMU-enforced isolation + supervision/self-heal** across process boundaries, plus a **capability send-mask** governing who-may-call-whom. | Per-user **keyed** authentication (factotum/secstore) — Ouroboros has the user model (accounts, groups, enforced modes, and a remote request that carries who is asking), but the *key* is per-machine, so an authorized machine can claim any of its own users' names; no on-the-wire encryption, trusted-LAN by design. |
| Runs on **modern ARM64/UEFI/Apple Silicon** and real hardware, actively built. | The **finished namespace model** — Plan 9 has fids, union mounts, and full per-process namespaces; Ouroboros **deferred fids** (path-per-op) and has a subset. |

*In one line:* Ouroboros has the *shape* of Plan 9 with a fraction of its
surface — the trade is memory safety + enforced isolation + modern hardware
now, against Plan 9's decade of completeness.

### vs. Helix (the Rust hot-reload / self-healing research OS)

*Both are early-stage Rust OSes, so this is more "different bets" than
"mature vs. immature."*

| You gain | You give up |
|---|---|
| A working **distributed cluster**, the **Plan 9 uniform protocol + namespaces**, **three filesystems**, a **network stack**, and **real-hardware (Parallels) bring-up** — breadth Helix's kernel-design focus doesn't cover. | **Hot-reload *with state migration*** — Helix's pause → snapshot → swap → restore → **rollback**. Ouroboros only restarts a crashed server from its *boot image* (state is lost, not migrated). |
| **Cross-process MMU/EL0 isolation** — faults contained at a hardware boundary, not just between in-kernel trait layers. | Helix's explicit **mechanism/policy layering** vocabulary and its live-upgrade discipline as a first-class design goal. |

*Shared DNA:* both pick Rust for a memory-safe OS and both treat
self-healing as central. Ouroboros went wide (isolation + distribution +
filesystems + net); Helix went deep on live upgrade and layered kernel
structure.

## The honest summary

- **If you want to *use* an OS today** — run software, use hardware, get
  work done — every system in this table beats Ouroboros, most by a wide
  margin. That is not its purpose.
- **If you want to *read and understand* a whole OS**, or study a
  from-scratch **microkernel + Plan 9 distributed model in memory-safe
  Rust**, Ouroboros offers something the mature systems bury under decades
  of code: a small, coherent, comprehensible whole where the boot path, the
  syscall boundary, the servers, and the cluster protocol all fit in your
  head.
- **The biggest thing you give up across the board is POSIX / C portability**
  — and that gap is *now half-closed*: the userland libc personality is built
  (picolibc is ported, C programs run — see `docs/libc-arc-postmortem.md`), so
  what's left is porting a real *application* and the `fork`/`select`/signals
  mismatches, not inventing the mechanism. Deliberately a userland personality,
  never a POSIX kernel — see `docs/roadmap.md`.

See also: `docs/architecture.md` (how the pieces fit),
`docs/roadmap-cluster.md` (where the distributed direction is headed), and
the research notes above for the design-influence view.

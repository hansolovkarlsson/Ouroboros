# Gap analysis: what mainstream Unixes have that Ouroboros doesn't (yet)

A **factual inventory of the current boundary** — the concrete companion to
[`comparison.md`](comparison.md). Where that doc argues the *trade* ("what you
gain, what you give up") at the level of philosophy, this one is a checklist:
for each capability a MINIX/Linux/Unix/Plan 9 user expects, does Ouroboros
**have** it, have it **partially**, or **not** — and, in one line, *what it
would take* and which roadmap arc it lives under.

This exists to **sequence the work**, not to aspire. It is the organizing
exercise behind the "North-star directions" section of
[`roadmap.md`](roadmap.md): it surfaces which gaps are one small program and
which are multi-milestone arcs, so the order of the next arcs falls out of the
map rather than being guessed. Keep it *factual* — a description of where the
line is today, the way the roadmap's parking lot tracks specific known gaps,
just organized as a coherent map. When a row changes, update it here.

**Status legend:**

- ✅ **Have** — present and confirmed working (see `CHANGELOG.md`).
- ◐ **Partial** — a real but scoped/limited version exists; the notes say how
  it falls short.
- ✗ **Don't** — not present. The notes say what it would take.

Cross-references: syscalls are named as in [`syscall-abi/src/lib.rs`]; the
server protocols are `FSOP_*` (fsd) / `NETOP_*` (netd) / the `ninep-abi` verb
set; roadmap arcs are cited by their `roadmap.md` section.

---

## 1. Process & task model

| Capability | Status | Notes / what it would take |
|---|---|---|
| Spawn a new task | ✅ | `SPAWN` + `SPAWN_STAGE` (two-step: the caller reads the ELF via `fsd` and stages it, since the kernel has no filesystem). Runs *alongside* the caller. |
| `posix_spawn`-shape (spawn + args + cwd + stdout) | ◐ | The pieces exist (`ARGS_STAGE`/`CWD_STAGE`/stdout target), but there's no `posix_spawn` API surface — it's the shell's bespoke flow. The POSIX-libc arc maps `posix_spawn` onto exactly these. |
| `fork()` | ✗ | No address-space copy exists, by design (a microkernel can't cheaply honor it — see the POSIX-divergence postmortem). The plan is `posix_spawn` native + porting fork-then-exec programs. |
| `exec()` (replace current image) | ✗ | The shell's `exec` builtin is a misnomer — it *spawns* alongside, nothing about the caller is replaced. True image-replacement isn't implemented. |
| `wait()` / reaping / exit status | ✅ | `WAIT` (21) blocks on a task's death, returns a byte status; a `Zombie(status)` slot holds until waited (or `kill`ed, which reaps immediately). |
| Kill another task | ✅ | `KILL` (19) — but by **slot index**, not by signal. |
| Signals (`SIGINT`/`SIGTERM`/handlers/`sigaction`) | ✗ | Ctrl+C is **keyboard reclamation**, not a delivered signal (`FG`'s doc: nothing is delivered to the task). A real signal mechanism is unbuilt. |
| Job control (`fg`/`bg`/`&`/jobs) | ◐ | `fg`/`ps`/`kill`/`wait` exist as builtins; **no background `&`**, no job table, foreground-only. Deliberately builtin (slot reuse makes an external `ps` race itself — see the userland-and-pipelines postmortem). |
| Stable PIDs | ◐ | Task **slot indices** (0–9) identify tasks, but a freed slot is reused immediately, so a number isn't a stable identity across commands. |
| Process groups / sessions / controlling terminal | ✗ | No grouping abstraction; there is one implicit keyboard owner (`FG`/`INPUT_OWNER_TASK`), not a session/pgrp model. |
| Parent/child process trees | ✗ | Tasks are flat peers in the scheduler; no recorded parent, no orphan reparenting, no process hierarchy. |

## 2. System-call surface

| Category | Status | Notes |
|---|---|---|
| I/O to a server (files/console/net) | ✅ | Not raw syscalls — messages to `fsd`/`cond`/`netd` (`FSOP_*`/`NP_*`/`NETOP_*`) over `MSG_CALL`. The kernel trap surface is tiny (~40 live syscalls). |
| Raw block device | ◐ | `BLOCK_INFO`/`BLOCK_READ`/`BLOCK_WRITE`, gated to `fsd` alone — one 512-byte sector per call. |
| Bulk transfer / zero-copy | ◐ | `GRANT`/`SAFECOPY` (capability-gated, ≤2048 B/op, loop to stream). MINIX's `safecopy` shape; no page-flip/`mmap` zero-copy. |
| Time | ◐ | `GET_TICKS` (20 ms tick) + `MONOTONIC_US` (µs since boot). **No wall-clock/RTC** (no real date). |
| Namespaces / `bind` | ✅ | `NS_SET`/`GET_NS` — per-task Plan 9 namespaces, inherited at spawn. Ahead of most Unixes here. |
| Memory / `brk`/`mmap` | ◐ | A fixed raw heap area (`HEAP_INFO`), no `sbrk` growth, no `mmap`. |
| `ioctl`, `fcntl`, `poll`/`select` (general) | ✗ | No general fd-control or multiplexing syscalls; `NET_WAIT` is a *netd-only* two-source wait, not a general `poll`. |

## 3. Filesystem, VFS & file handles

| Capability | Status | Notes / what it would take |
|---|---|---|
| Multiple filesystems | ✅ | FAT32, exFAT, ext2 — all read+write, behind fsd's `Filesystem` enum. |
| GPT + MBR partitions, multi-mount | ✅ | `partition::discover`; several partitions mountable at once (`FSOP_MOUNT_AT` → a tree bound into the namespace). |
| Mount / unmount / mkfs / partition / erase | ✅ | The disk-management arc: `FSOP_MOUNT`/`UNMOUNT`/`FORMAT`/`PARTITION`/`ERASE`. |
| Integer file descriptors (open→fd, cursor, `dup`) | ◐ | **fids exist** (libc arc step 5): `fsd` has server-side open-file handles (`NP_OPEN`→fid≥3, `NP_PREAD`/`NP_PWRITE`/`NP_FSTAT`/`NP_CLUNK`, a per-client table), directly usable as a C fd — a POSIX fd *is* a 9P fid, paying off twice as predicted. The path-per-op verbs coexist. Still missing: `dup`/`dup2`, and fds for non-file objects (sockets/pipes). |
| `open`/`close`/`read`/`write`/`lseek` | ✅ | Both as **fids** (a cursored open-file handle in `fsd`, the C libc's path) and as path-per-op verbs (`FSOP_READ_AT`/`WRITE_AT` + a `read_cursor`, the shell's path). |
| `stat`/`fstat` (per-file metadata) | ◐ | A per-file **`stat` op** (`NP_STAT`): size, a directory flag, a broken-down calendar mtime, **and POSIX mode/uid/gid** (guarded by a `mode_valid` byte) — backs `ls -l`. ext2 surfaces its real on-disk `i_mode`/`i_uid`/`i_gid` (`ls -l` shows `drwx------ 0 0` for `lost+found`, real owners for user files); FAT32 decodes the timestamp; exFAT/ext2/`/proc` leave the time unset and FAT32/exFAT/`/proc` leave mode/owner unmodeled (`ls -l` synthesizes a conventional mode, `-` owner). Still no cursored `fstat` (path-per-op). |
| File permissions (mode bits) | ◐ | **Stored, surfaced, and writable — not yet enforced**: ext2's uid/gid/mode are read *and written* (`stat`/`ls -l`/`chmod`/`chown`); FAT/exFAT can't model them. The one remaining step is *enforcement* — check caller identity vs. inode mode in the `FSOP_*` dispatch (roadmap item 4). |
| `chmod`/`chown` | ◐ | **Present, ext2-only** (`NP_CHMOD`/`NP_CHOWN` → `/bin/chmod`, `/bin/chown`): numeric **and symbolic** modes (`chmod 755`, `chmod u+x`/`go-w`/`a=rx`/`g=u`, comma-separated clauses, conditional `X`) and owners **by name or id** (`chown alice:staff`, `chown 501:20`, resolved through `/etc/passwd`+`/etc/group` with the shared `accounts` lookups); the on-disk inode fields are patched in place (e2fsck-clean). FAT32/exFAT/`/proc` return `FS_ERR_NOT_SUPPORTED`. |
| Symbolic links | ◐ | ext2 symlinks are **reported, not followed**; no creation. |
| Hard links | ✗ | Not implemented (ext2 link counts are maintained for dirs, but no user-facing `ln`). |
| Named pipes (FIFOs) / Unix domain sockets | ✗ | Pipes are shell-orchestrated IPC streams (below), not filesystem objects. |
| `/dev` device namespace | ✗ | Deliberately deferred — one block device, nothing to name yet. The Plan 9 devfs direction (roadmap "Disk management / /dev"). |
| `/proc` | ◐ | A synthetic read-only `/proc` (task state per slot) exists in fsd; far thinner than Linux's. |
| Journaling / crash consistency | ✗ | Writes are staged/ordered but not journaled; a mid-write crash can corrupt (noted in the isolation postmortem). |

## 4. Terminal, TTY & I/O plumbing

| Capability | Status | Notes / what it would take |
|---|---|---|
| Console output | ✅ | Via `cond` (byte-stream on QEMU UART, framebuffer glyph-rendering on Parallels). |
| Keyboard input | ✅ | USB HID (xHCI) on real hardware; kernel delivers bytes to the input owner. |
| ANSI / VT100 escape codes | ◐ | cond has a **small** ANSI parser (cursor/wrap/scroll). No SGR color, cursor addressing, erase-region, scroll regions — and **no return channel** for query sequences. The terminal arc (roadmap item 1). |
| Output redirection `>` / `>>` | ✅ | Shell-side, for every builtin/command. |
| Pipelines `a \| b \| c` | ◐ | Real N-stage pipelines of `/bin` programs (IPC + capability delegation); **`a \| b > file`/`>> file` compose** (last stage captured to the file); and a **single builtin may sit at any position** (`cat f \| ps`, `ls \| ps \| grep x` — a non-first builtin drains its upstream and becomes the source). Still no job control / `&` backgrounding of a whole pipeline. |
| `select`/`poll`/`epoll` (general multiplexing) | ✗ | Only `NET_WAIT` (netd's two sources). A program can't wait on "stdin *or* a socket." A pager/editor needs this (roadmap item 3). |
| TTY line discipline (canonical mode, `^U`/`^W`, raw mode) | ◐ | The shell's own line editor handles backspace; there's no general TTY layer a program can put into raw mode. |
| `isatty`/pseudo-terminals (ptys) | ✗ | No pty layer; nothing multiplexes a terminal. |

## 5. C / libc / language runtime

| Capability | Status | Notes / what it would take |
|---|---|---|
| A libc (C) | ◐→✓ | **Exists as a userland POSIX personality — picolibc is ported** (`/bin/CPICO`: float `printf`, `snprintf`, `qsort`, `malloc`, `strtol`), *not* a kernel ABI. Built `-fPIC` (self-relocates under the loader, zero `ABS64`), linked against ~8 syscall stubs (`write`/`read`/`open`/`close`/`lseek`/`fstat`/`sbrk`/`_exit`) that map onto existing server messages. A hand-rolled minimal libc (`libc/src`) preceded it. **Done:** the mechanism (unmodified C stdlib code runs). **Left:** porting a real *application* — see the libc-arc postmortem + `roadmap.md`. |
| `malloc`/`free` (real heap) | ◐ | **C:** picolibc's `malloc`/`free`/`realloc` over the `sbrk` stub (`HEAP_INFO`) — a real allocator. **Rust:** a raw fixed **heap area** used as `&mut [u8]`; **no `alloc`-backed `Vec`/`Box`/`String`** — prebuilt `liballoc` isn't PIE-linkable on stable (`R_AARCH64_ABS64`; `-Z build-std` is nightly). |
| Dynamic linking / shared libraries | ✗ | Every program is a **static position-independent** `aarch64-none` binary (`R_AARCH64_RELATIVE` only). No `.so`, no `dlopen`. picolibc is linked statically. |
| Threads (shared address space) | ✗ | One task = one thread of control in its own region. No `pthread`, no in-process threads. picolibc built `-Dthread-local-storage=false`, global `errno`. |
| On-device compiler (C) | ✗ | A small C compiler (tcc/chibicc) is realistic **atop the libc, which now exists** (a compiler is a C program) — it becomes "port one more program." Realistic *first* step is still an **assembler** (text → the ELF the loader already parses). Roadmap item 5. |
| On-device compiler (Rust) | ✗ | Effectively out of reach near-term (rustc's scale + the `-Z build-std` wall). |
| Self-hosting (builds itself) | ✗ | You cross-compile from macOS. The endgame the project's name points at, gated behind everything above. |

## 6. Users, security & permissions

| Capability | Status | Notes / what it would take |
|---|---|---|
| User accounts / uid / gid | ● | **A full account model now exists.** A kernel-owned uid/gid per task (`SET_ID`/`GET_ID`, default root, inherited across spawn, root-gated) *plus* the userland account layer: `/etc/passwd` + `/etc/group`, a shared `accounts` crate, `/bin/{passwd,useradd,groupadd,usermod}` (root-only), `su <name>`, `id` with names, per-user `/Users/<name>` homes. Groups are **primary-gid** (one kernel gid); supplementary membership is the remaining tier. |
| Login / authentication (passwords) | ● | **Present**: the shell gates every session on a `login:` prompt, authenticating username + password against `/etc/passwd` (`name:uid:gid:home:salt:hash`, `hash = SHA-256(salt‖password)`; echo-off entry, constant-time check) then `SET_ID`-ing to that user; `logout` restores root (saved-uid) and re-prompts. Accounts are now **managed on-device** (`useradd`/`groupadd`/`passwd`/`usermod`, all root-only — self-service `passwd` is the deferred `accountd`/setuid tier), homes live under `/Users`, and `~` expands. Thinner than Unix: no `/etc/shadow`, salts are clock-derived (weak), and PAM/sessions/supplementary-groups don't exist. |
| Enforced file permissions | ◐ | **Enforced on ext2**: `fsd`'s `check_access` gates every file verb — the caller's uid/gid (`GET_ID(sender)`) vs. the inode owner/mode, owner→group→other, root bypasses, `FS_ERR_PERM` on refusal (`chmod` owner-only, `chown` root-only). FAT/exFAT stay unrestricted (no mode to check). Thinner than POSIX: the search (`x`) bit on ancestor directories isn't checked yet (only the object + its parent), and remote/cluster requests are still machine-authenticated (root), not per-user. |
| Privilege boundary (root / sudo) | ◐ | A per-task **capability send-mask** governs who-may-call-whom (topological isolation) — the mechanism a privilege model would build on, but there's no user-facing privileged/unprivileged split. |
| Cluster authentication | ◐ | **Machine-level**: a shared cluster key, mutually authenticated (HMAC) on the 9P export, fail-closed, `\NOEXEC`. **No per-user identity, no replay protection, no on-the-wire encryption** — each a named next tier (cluster-auth postmortem). Trusted-LAN by design. |
| Cryptography | ◐ | Hand-rolled SHA-256 + HMAC-SHA256 (NIST/RFC-validated). No TLS, no public-key, no at-rest encryption. |
| Sandboxing beyond MMU isolation (seccomp/cgroups/quotas) | ✗ | Isolation is per-task MMU + the capability mask; no resource quotas or syscall filtering. |

## 7. Networking

| Capability | Status | Notes |
|---|---|---|
| Ethernet / ARP / IPv4 / ICMP | ✅ | Hand-rolled in `netd`; `ping`. |
| UDP + DNS | ✅ | `resolve` (DNS-over-UDP). |
| TCP client | ✅ | `fetch` (HTTP GET), plus congestion control, SACK, adaptive RTO. |
| TCP server | ✅ | Concurrent HTTP static-file server (up to 4 conns), directory listings, `HEAD`, `405`. |
| Dial-out / dial-in over another node's NIC | ✅ | `/net/tcp` connection files (`dial`/`serve`) — a Plan 9 resource-sharing capability most Unixes lack. |
| Sockets as file descriptors (BSD `socket`/`bind`/`connect`) | ✗ | The interface is netd's IPC protocol + path-based `/net/tcp`, closer to Plan 9 than to BSD sockets. A POSIX socket layer would be a libc/`netd` addition. |
| IPv6 | ✗ | IPv4 only. |
| Real-hardware NIC (Parallels) | ✗ | Parallels' virtio-net is **PCI**; the kernel has virtio-**mmio** only (QEMU). A virtio-pci transport is the gating sub-project. |

## 8. Devices, drivers & DMA

| Capability | Status | Notes |
|---|---|---|
| Block storage | ✅ | virtio-blk (QEMU) + USB mass storage / xHCI (Parallels). |
| Console / framebuffer | ✅ | GOP framebuffer + UART, owned by `cond`. |
| USB keyboard (xHCI) | ✅ | From-scratch xHCI + HID; real-hardware confirmed. |
| USB: hubs / hot-plug / EHCI (USB 2.0) | ✗ | One tier of ports, boot-time scan, no hubs; USB 2.0 sticks route to EHCI (undriven). |
| NIC | ◐ | virtio-net (QEMU) only; PCI transport missing for Parallels. |
| `/dev` namespace / driver framework | ✗ | Drivers are ad-hoc kernel modules or fixed servers; no uniform device model or dynamic driver loading. |
| DMA safety without an IOMMU | ◐ | The constraint that keeps DMA-owning drivers *in* the kernel (block/NIC can't safely leave). Shapes the whole architecture; not a "fix," a fact. |

## 9. Memory management

| Capability | Status | Notes |
|---|---|---|
| MMU paging + per-task page tables | ✅ | Per-slot L0–L3 translation views; EL0 sees only its own region. |
| Stack guard page | ✅ | 16 KB guarded stack (caught real overflows). |
| Runtime physical-page allocator | ◐ | A bump allocator (LIFO reclaim in the common case; leaks otherwise — no free list). |
| Demand paging / swap / paging-to-disk | ✗ | All resident; no swap. |
| `mmap` (anon + file-backed) | ✗ | Anon could map to region allocation; file-backed is harder. Unbuilt. |
| Copy-on-write / shared memory | ✗ | IPC is **copy only** (no shared pages) — a deliberate isolation choice; `fork` COW would need new machinery. |

## 10. Concurrency & scheduling

| Capability | Status | Notes |
|---|---|---|
| Preemptive multitasking | ✅ | Timer-driven, round-robin, confirmed on real hardware. |
| Blocking primitives | ✅ | `Runnable`/`Blocked(reason)`, `READ_CHAR`/`WAIT`/`MSG_RECV`/`NET_WAIT`. |
| Priorities / nice / real-time scheduling | ✗ | Strict round-robin, no priorities. |
| SMP / multicore | ✗ | Single core. A large arc (per-core state, locking, IPI, cache coherence). |
| In-process threads | ✗ | See §5. |

## 11. Distributed / cluster — *where Ouroboros is ahead*

| Capability | Status | Notes |
|---|---|---|
| Uniform file protocol over TCP (9P-ish) | ✅ | The `ninep-abi` verb set, same verbs local and remote. |
| Per-task namespaces + remote mount | ✅ | `mount -r host:port /mnt/a`; another machine's disk as files. |
| Resources as files (`/proc`, `/dev/cons`, `/net`) | ✅ | Another node's processes/console/network identity, mountable. |
| Remote execution (`cpu`) with namespace import | ✅ | `cpu host cmd` runs on the remote while reading *your* files at `/host`. |
| Chunked/streamed remote output | ◐ | `cpu` output via a chunked pull (`NETOP_RUN_MORE`), bounded ~2 KB per run; truly unbounded streaming is a later refinement (roadmap-cluster). |

*Most rows here have **no equivalent** in MINIX/Linux/Unix out of the box —
this is the Plan 9 inheritance, and the project's distinctive strength.*

## 12. Time & clocks

| Capability | Status | Notes |
|---|---|---|
| Monotonic clock | ✅ | `MONOTONIC_US` (µs since boot). |
| Preemption tick | ✅ | `GET_TICKS`. |
| Wall-clock / RTC / real date | ✗ | No calendar time; `date` can't be written meaningfully yet. Needs an RTC read (or NTP over the existing stack). |
| Timers / `sleep`/`alarm`/`setitimer` | ◐ | `NET_WAIT` has a timeout (netd's RTO); no general per-task sleep/timer API. A `sleep` command needs one. |
| Timezones | ✗ | N/A until wall-clock exists. |

## 13. The standard command-line utility set

**Have** (`/bin`, over `ulib`): `ls tree cat mkdir rmdir touch rm cp mv writeat`
(fileutils) · `echo uptime clear args` (shellutils) · `grep wc head tail nl rev
uniq upper` (textutils, chainable filters) · `ping resolve fetch dial serve`
(netutils).
**Builtins:** `help cd pwd write mount unmount erase partition format exec exit
ps kill fg wait send recv selftest env set unset cpu`.

**Missing, roughly by cost** (roadmap items 2 & 3):

| Utility | Status | Notes |
|---|---|---|
| `sort` | ✅ | `-r`/`-n`/`-u`/`-f`. The one filter that can't stream — buffers the whole input in its heap and heapsorts an in-place line index; a documented size cap (truncate + warn) handles the fixed-buffer/no-alloc constraint. |
| `tail` `nl` `rev` `uniq` | ✅ | Shipped as `/bin` `ulib` filters (the turnkey pattern). `tail`/`uniq` bounded to a fixed line buffer; `nl` is `cat -n` style. |
| `tee` `tr` `cut` | ✗ | The remaining small `ulib` filters (`tee` also needs an fsd write path for its file arg). |
| `find` `du` `df` | ✗ | Client-side directory walking (new but small); `df` wants the volume info fsd already returns. |
| `date` `sleep` | ✗ | Blocked on wall-clock (§12) and a per-task timer respectively. |
| `chmod` `chown` `ln` | ◐ | `chmod`/`chown` **shipped** (ext2-only; `chmod` takes octal *or* symbolic modes, `chown` names *or* numeric ids); `ln`/`ln -s` are still open (the ext2 link foundation exists — `i_links_count` is maintained and symlinks are reported — so it's a small follow-up, see roadmap §b). |
| Command flags (`ls -l`, `grep -i/-r`, `rm -r`, `cp -r`, real regex) | ◐ | `ls` takes `-l`/`-a` (on the `stat` op) and `grep` takes `-i`/`-v`/`-n`; the rest are still positional-only, and `grep` matching is still substring (no regex). A shared `ulib` option parser would generalize flag handling (roadmap item 2). |
| An editor + a pager (`less`/`more`) | ✗ | The two consumers that justify the VT100 arc (§4) and general `select` — gated on items 1 & 3. |
| `sed` `awk` `find … -exec` | ✗ | Larger; realistic once a libc/scripting layer exists. |

## 14. Boot, init & service management

| Capability | Status | Notes |
|---|---|---|
| Boot to a shell | ✅ | UEFI → kernel → servers → shell, on QEMU and real Parallels. |
| Server supervision / restart | ◐ | `supervisor.rs` restarts `fsd`/`cond`/`netd` on crash or wedge (from the boot image; **state is lost, not migrated** — cf. Helix). Capped per boot. |
| General service/daemon manager (init system) | ✗ | The three servers are hardcoded at boot; no `init`/`systemd`-style unit model, no user-defined services. |
| `cron` / scheduled jobs | ✗ | None. |
| Runlevels / targets / dependency ordering | ✗ | None. |
| Config files (`/etc`) | ✗ | Only `INIT.CFG` (which program to load) and `CLUSTER.KEY`; no general config layer. |

## 15. Observability & debugging

| Capability | Status | Notes |
|---|---|---|
| Process listing | ◐ | `ps` (slot table); `/proc/<n>/state`. No CPU/memory accounting per task. |
| Exit-status reporting | ✅ | `wait` returns a byte status. |
| Kernel/fault reporting | ✅ | Exception handler prints ESR/FAR/ELR; emergency console. |
| Logging framework / `dmesg` / syslog | ✗ | Ad-hoc console prints; no ring buffer or log files. |
| Debugger / `ptrace` / core dumps | ✗ | None (QEMU `-d int` and boot-log capture are the tools). |
| Performance counters / profiling | ✗ | None. |

---

## The biggest gaps, ranked — and where they lead

Reading the map top-down, the highest-leverage missing pieces, each pointing
at a roadmap arc:

1. **The users/permissions arc — DONE (2026-08-28).** All four pieces landed:
   the `stat` mode/owner surface + `chmod`/`chown` (ext2, e2fsck-clean); a
   kernel-owned uid/gid per task (`SET_ID`/`GET_ID`, saved-uid); a `login` gate
   against `/etc/passwd`; and **`fsd` permission enforcement** (`check_access` —
   caller uid/gid vs. inode owner/mode, root bypass, `FS_ERR_PERM`). What remains
   are *refinements*, not the arc: the ancestor-directory search (`x`) check,
   per-user *cluster* identity, `/etc/shadow`, `passwd`/`useradd`, per-user
   `/home`, and groups. (The user/identity + login model — once ranked
   separately — is part of this and likewise done.)
2. **A userland libc personality — DONE, mechanism (2026-08-28).** POSIX/C
   portability was the single biggest thing given up across every comparison.
   The six-step libc arc built it: a C program runs, and **picolibc is ported**
   (`/bin/CPICO`: float `printf`, `qsort`, `malloc`, `strtol`) — as a userland
   personality, no kernel change. **fids** (the file-descriptor gap, once ranked
   separately) landed as step 5: a cursored server-side handle that is *both* a
   9P fid and a POSIX fd, one feature two payoffs. What *remains* and is now the
   highest-leverage gap: **porting a real application** (SQLite, a small C
   compiler) — "port one more program" — plus the architectural mismatches
   (`posix_spawn`/`fork`, `select`/`poll`, signals, `mmap`). → roadmap "POSIX /
   C-program portability", `roadmap-completed.md`, `docs/libc-arc-postmortem.md`.
3. **A richer terminal + its first full-screen consumer.** VT100 color/cursor
   addressing, built alongside an editor or pager so the escape set is
   driven by a real need. → roadmap items 1 & 3.
4. **General `select`/`poll`, signals, and a service manager.** Each unblocks
   a class of "normal Unix program" (multiplexers, job control, daemons) —
   medium arcs, and the natural companions to porting real C applications.

Two whole categories are deliberately *further off* and not near-term gaps to
close: **SMP/multicore** and **swap/`mmap`/COW** — large, and nothing in the
current direction needs them yet.

And the counterweight worth stating plainly: §11 is the column where
Ouroboros **leads** the comparison set — a working distributed cluster, remote
`cpu`, and resources-as-files are things most of these systems don't have out
of the box. The gaps above are what it would take to make the *single-machine*
experience as complete as its *distributed* one already is.

---

*See also:* [`comparison.md`](comparison.md) (the philosophy-level trade),
[`roadmap.md`](roadmap.md) (the arcs each gap maps to),
[`architecture.md`](architecture.md) (how today's pieces fit), and the
postmortems under `docs/` for why the boundary sits where it does.

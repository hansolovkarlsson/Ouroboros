# Where Ouroboros stands: microkernel, protection, fault tolerance

A self-assessment against the three ideas this project draws on, measured
as they actually stand in the code today — not aspiration. It sits
alongside the two outward-looking research notes,
[`research-minix-boot.md`](research-minix-boot.md) and
[`research-helix-os.md`](research-helix-os.md), and reads best after
[`architecture.md`](architecture.md), which is the authoritative
reference for the mechanisms named below.

**This note supersedes the "current shape" sections of both research
notes.** Both were written before the driver-isolation, per-task-page-
table, and fault-containment milestones (all 2026-08-18), and both state
flatly that Ouroboros has "*no* fault isolation of any kind." That is no
longer true, which is exactly what makes this re-assessment worth
writing. Where those notes say "none yet," read this instead.

The short version: on the **three** services it has moved out (the
filesystem, the console, and now the network stack), Ouroboros is a
genuine microkernel — enforced isolation, capability-based bulk IPC, and
real crash recovery. The mechanisms are the real thing. What separates
it from MINIX and Helix is **breadth and generality**, not honesty of
the individual pieces.

---

## Axis 1 — Microkernel (MINIX)

The defining MINIX move is that **the kernel does not contain the
services**: drivers, the filesystem, and the network stack are separate
user-space processes, and the kernel is just mechanism (IPC, scheduling,
privilege) underneath them. Measured against that one test:

### Where Ouroboros genuinely qualifies now

- **The entire FAT32 filesystem lives in userland.** `fsd/` (the
  filesystem server) runs at EL0 as an ordinary loaded program in task
  slot 2. `kernel/src/fat32.rs` no longer exists — its ~1040 lines moved
  out of the kernel verbatim into `fsd/src/fat32.rs`. Every `ls`, `cat`,
  `cp`, `mkdir`, `write` is now an IPC round trip from the shell to a
  separate process, not a syscall into kernel code.
- **The kernel's remaining storage role is deliberately tiny and
  gated.** Three syscalls (`BLOCK_INFO` / `BLOCK_READ` / `BLOCK_WRITE`,
  one 512-byte sector at a time) are all that's left, and they are
  **accepted only from task 2** (`FSD_TASK`). That gate is the literal
  meaning of "supervised": the kernel holds the block device, and
  exactly one task is permitted to ask it to touch the disk. A spawned
  program can never land in slot 2 (`spawn` scans from
  `FIRST_SPAWNABLE = 5`), so it can never inherit that privilege.
- **The IPC converged on MINIX's actual primitives, not a hand-wave.**
  `MSG_CALL` (syscall 29) is sendrec-shaped: a synchronous request/reply
  that blocks the client on a reply *filtered by sender*, so a reply
  can't be mis-routed the way a plain "receive from anyone" would allow.
  This is the same shape MINIX's `_syscall()` has always had (implemented
  as `sendrec` to a server).
- **Bulk data crosses the boundary by MINIX's real mechanism, too.**
  Once per-task page tables made a server unable to dereference a client
  pointer, file data moves by **grant/safecopy**: the client pre-registers
  an exact `(ptr, len, direction)` capability (`GRANT`, syscall 31), and
  the kernel copies only those bytes, only while the call is live, only
  in the granted direction (`SAFECOPY`, syscall 32). This is MINIX's
  `sys_safecopy` — chosen deliberately over the simpler "server names a
  client pointer and the kernel bounds-checks it" model, because the
  simpler one re-introduces exactly the trust the isolation milestone had
  just removed. (See [`architecture.md`](architecture.md) and the
  grant/safecopy section of
  [`isolation-and-dataflow-postmortem.md`](isolation-and-dataflow-postmortem.md).)

### Where it falls short of MINIX

- **Three services, not a driver fleet.** MINIX runs disk, network,
  keyboard, and every device driver as separate servers started in
  dependency order by `/etc/rc`. Ouroboros has **three** out-of-kernel
  servers now — the filesystem (`fsd`), the console (`cond`), and the
  network protocol stack (`netd`) — but the USB/xHCI stack, the block
  device and NIC *drivers* (which own DMA and can't leave without an
  IOMMU), the GIC, and the scheduler are all still undifferentiated EL1
  kernel code. Note the pattern: the *policy* (FAT32 logic, console
  rendering, ARP/IP/ICMP) moved out; the DMA-owning *driver* half stayed,
  reached by its one server through gated syscalls.
- **A boot image of a few fixed programs, not a typed fleet.** MINIX packs
  kernel + PM + VFS + RS + drivers into `/boot/image`. Ouroboros loads one
  configurable program (the shell, via `INIT.CFG`) plus three fixed-path
  infrastructure servers (`\EFI\ORBS\{FSD,COND,NETD}.BIN`). Four loaded
  programs, a start in that direction but not yet a typed set of
  cooperating servers started in dependency order.
- **The `who-may-call-whom` capability model is coarse, but no longer
  purely static.** IPC is not flat — a per-slot send-mask, enforced at the
  `MSG_SEND`/`MSG_CALL` boundary, restricts which endpoints each task may
  reach (a spawned program can reach only the two servers and the shell,
  not arbitrary tasks or the device gates). The static per-slot mask is now
  a *baseline* that can be extended at runtime: `DELEGATE` (syscall 41) lets
  a task hand another a send-capability it statically holds — MINIX's grant
  mechanism, in miniature for the send topology. It's used today for
  relay-free program-to-program pipes (the shell delegates a producer the
  right to stream directly to its consumer). It stays coarse: one delegated
  target per task, and delegation is confined by the "may only delegate what
  you statically hold" rule (no transitive re-delegation), so in practice
  only the shell delegates. A general capability-passing mechanism (any task
  handing any held capability onward, transitively) is the remaining gap.
- **No process manager / no PID namespace / no `fork`.** Task creation is
  `spawn` (add a task alongside the caller), not the POSIX
  fork-exec-wait lineage MINIX's PM administers. There *is* a
  wait/reaping path (`WAIT`, syscall 21, with `Zombie(status)` slots),
  but no process hierarchy above it.

**Verdict:** structurally a microkernel on the services it actually moved,
using the right IPC primitives and an enforced IPC capability topology that
is now a static baseline plus runtime delegation. Short of MINIX mainly on
*breadth* (three servers, not a full fleet) and on *general* capability
delegation (today's is coarse — one delegated target per task, non-transitive,
in practice shell-only — where MINIX's grants are general and transitive).

---

## Axis 2 — Protective states / enforced isolation

"Protected" was aspirational in this project until very recently, so it's
worth being precise about the line that got crossed.

### Before (the state the research notes describe)

Isolation was a **convention**. Every EL0 region was accessible to every
task — the shell could read or write the filesystem server's memory and
vice versa, with no fault. "Supervised EL0 process" meant "we gate a
syscall by task index." Real, but soft: nothing *stopped* one task from
reaching into another's memory, it was just impolite.

### Now — hardware-enforced, and proven by fault injection

- **Per-task MMU page-table views.** Each of the seven scheduler slots
  runs under its own translation tables (`mmu.rs`): identical kernel and
  device mappings in every view, but **EL0 access to that task's own
  region alone**. `mmu::activate_task` swaps `TTBR0` and flushes the TLB
  on every context switch.
- **This was verified, not assumed.** An A/B fault-injection probe read a
  byte of the shell's region from a spawned program: it *succeeded* under
  the old shared map (the control) and *faulted* under per-task views
  (`EL0 FAULT far_el1=0x5c600000`, a permission fault, faulting task
  killed alone, shell alive). The isolation is demonstrably real.
- **The last soft path closed with it: syscall pointers are now
  validated.** With the MMU enforcing EL0-to-EL0 isolation, the remaining
  way to reach another task's memory was to have the *kernel* touch a
  bad pointer on your behalf. `in_caller_region` (`syscall.rs`) now
  checks every pointer/length argument against the caller's own mapped
  region. Isolation is enforced on both the direct path (MMU) and the
  kernel-mediated path (validation) — a real closure of a limitation
  that had been documented as open since the FS syscalls were first
  written.

### The honest caveats

- **ASIDs are reverted.** The optimization to tag EL0 pages non-global
  and give each view its own ASID (skipping the per-switch TLB flush)
  passed every QEMU test *and* the isolation probe — then faulted the
  idle task on its own instruction fetch on real Parallels silicon. It
  was reverted to flush-on-switch (correct on both platforms). A useful
  reminder that "protected" is validated per platform, not proven in the
  abstract. The per-switch flush is cheap relative to what a timer tick
  already does, so nothing is lost but an optimization.
- **The privilege model is two-level and flat within EL0.** EL1 kernel
  vs. EL0 userland is hardware-enforced; among EL0 tasks there are no
  rings of trust, only mutually-exclusive memory views.

**Verdict:** this is the axis where Ouroboros is *strongest* relative to
both influences. MINIX gets EL0-equivalent isolation from separate
address spaces; Helix explicitly does **not** have hardware-enforced
isolation between modules (its safety leans on Rust's memory safety and
hot-reload bookkeeping — see the research note). Ouroboros now has real,
MMU-enforced, injection-verified per-task isolation, with a stack guard
page turning silent overflow into a clean, contained fault. The one thing
left on this axis is the reverted per-task-ASID TLB optimization.

---

## Axis 3 — Fault tolerance (Helix OS, and MINIX's RS)

Helix's pitch is self-healing: a module crashes or hangs, the kernel
detects it and restarts it (with attempted state migration) without a
reboot. MINIX's is the reincarnation server: a crashed *process* is
restarted from a clean slate because it was never in the kernel to begin
with. Ouroboros has a real, minimal version of both — and it landed the
same day as the isolation work, which is not a coincidence.

### What exists

- **Every EL0 fault is contained.** This was the sleeper finding of the
  isolation arc: *before* the fix, any userland wild-pointer fault fell
  through the kernel's report-and-halt path. A microkernel that moved its
  filesystem to userland *for containment* was converting every FS bug
  into a whole-system halt. Now `exceptions.rs`'s slot-8 fall-through
  (`rust_el0_fault_handler`) tears down **just the faulting task** —
  region freed, keyboard ownership reverted, slot reaped — and the
  survivors keep running. (Tasks 0 and 1 faulting still halt honestly;
  nothing meaningful survives the keyboard owner's or the idle task's
  death.)
- **Servers are supervised, uniformly** — MINIX's reincarnation server,
  minimal edition, generalized (`kernel/src/supervisor.rs`). A registry
  keeps each supervised server's ELF image from boot (`fsd`, `cond`, and `netd`,
  each registered at load — kept precisely because one crashed server
  *is* the filesystem you'd otherwise need in order to reload it) and
  restarts one from that copy on a fault, into a fresh region; the new
  server re-runs its own startup (probe the device, remount from disk).
  **Its state was always disk-derivable, which is what makes this real
  recovery rather than a restart that loses everything.** A shared
  3-restart-per-boot cap guards loops; past it the kernel degrades
  gracefully (a dead `fsd` slot stays `Unused`, the same path as a
  missing `FSD.BIN`; a dead `cond` falls back to the kernel's `PUTC`
  console).
- **A wedged server is caught, not just a crashed one — either way it
  wedges.** A server stuck in an infinite loop never faults, so the crash
  path can't see it; a passive **heartbeat** in the scheduler tick catches
  that: a healthy server keeps returning to a `Blocked` state, so staying
  continuously `Runnable` for ~2.5s is the wedge signal. A server stuck
  the *other* way — `Blocked` forever, deadlocked mid-request — is
  invisible to that (a healthy idle server looks the same), so an **active
  health ping** covers it: the supervisor pokes a long-`Blocked` server
  with a message and restarts it if the ack doesn't come back in time,
  needing no server-side changes (the server's ordinary reply is the ack).
  Both restart on the same path as a crash. This is exactly the "health
  checks for hangs, not just crashes" primitive Helix names — and it now
  covers both flavors of hang.
- **Clients don't hang on a dying server.** A task blocked mid-`MSG_CALL`
  to a server that dies is woken with `TASK_ERR_NO_SUCH_TASK`
  (`fail_calls_to`, wired into all three death paths — exit, kill, and
  fault), which the shell folds into its ordinary "no filesystem"
  message. Previously such a caller waited forever, with Ctrl+C the only
  rescue.

### Where it's short of Helix (and of MINIX's RS)

- **Kernel-resident drivers still have no containment.** Supervision now
  covers the userland servers uniformly (`fsd`, `cond`, and `netd`, via the
  `supervisor.rs` registry — no longer special-cased to one task), and
  catches both crashes and wedges. But a crash in any *kernel-resident*
  driver (xHCI, virtio-blk/the block transport) still takes the system
  down — it's EL1 code, and the no-IOMMU DMA constraint is what keeps the
  block transport in the kernel for now (see `roadmap.md`).
- **Wedge detection is heuristic, not a proof.** Both detectors are sound
  on this single-user, fast-request system — a healthy server returns to
  `Blocked` in far less than a tick, and acks a ping within a tick or two —
  but neither can tell a genuine multi-second workload from a wedge, and
  the active ping's value is largely forward-looking: in today's small,
  acyclic server topology a true blocked-deadlock can't actually form
  (`fail_calls_to` rescues callers of a *dying* server, and a
  runnable-wedged target is restarted before anything cycles), so the ping
  earns its keep once the call graph grows. And there's no journaling — a
  server that corrupted on-disk state mid-write before dying comes back to
  the corruption. Ctrl+C still rescues a blocked *client*.
- **Restart is not hot-reload.** Helix's headline is *live* replacement
  of a running module with new code, with state snapshot/restore and
  automatic rollback. Ouroboros reloads the *same* image from a static,
  and recovers state by re-deriving it from disk, not by migrating it.
  That's closer to MINIX's "restart from a clean slate" than to Helix's
  live swap.
- **No journaling.** If a server corrupts the disk mid-write and *then*
  crashes, recovery restarts a healthy server onto corrupted data. Fault
  tolerance here is about surviving a *crash*, not guaranteeing data
  integrity across one.

**Verdict:** a genuine, working reincarnation story for one service, with
the crucial "state is disk-derivable" property that makes it real
recovery. Short of both influences on generality (one supervised task,
not all), on the non-crashing failure mode (no watchdog), and on
liveness of the swap (reload, not hot-reload).

---

## Consolidated scorecard

Read this as "how close is the *mechanism*," not "is it as mature." All
three projects are pre-alpha; MINIX is the only one with decades of
real-world hardening behind the design.

| Dimension | MINIX | Helix | Ouroboros (today) |
|---|---|---|---|
| Services outside the kernel | Full fleet (disk, net, FS, all drivers) as separate address-space processes | Modules outside the TCB, but within one address space | **Three** — the FAT32 filesystem (`fsd`), the console (`cond`), and the network protocol stack (`netd`), real separate EL0 processes |
| Isolation mechanism | Separate address spaces (hardware) | Rust memory safety + hot-reload bookkeeping (no hardware boundary between modules) | **Per-task MMU page-table views (hardware), injection-verified** + validated syscall pointers |
| IPC | Synchronous `SEND`/`RECEIVE`/`SENDREC`, fixed 64-byte messages | Trait-boundary calls + IPC queuing in the scheduler | Synchronous `MSG_CALL` (sendrec-shaped, sender-filtered) + copy-by-mailbox; **grant/safecopy** capability for bulk data |
| Bulk data across the boundary | `sys_safecopy` (capability-gated copy) | Within one address space (no cross-space copy needed) | **`GRANT` + `SAFECOPY`** — the same capability model as MINIX |
| Crash recovery | RS restarts the crashed *process* from a clean slate | Self-heal: watchdog + health monitor + recovery, with attempted state migration | A supervisor registry reloads any crashed server from a kept image (fsd remounts from disk — state is disk-derived); **shared 3-restart cap** |
| Non-crashing hang recovery | Timeouts / RS health | Health checks for hangs (first-class) | **Yes** — a passive heartbeat catches a *runnable* wedge; an active ping catches a *blocked* wedge |
| Live code replacement | No (restart, not hot-swap) | **Yes** — pause/snapshot/swap/restore/rollback | No (reload same image) |
| Supervision scope | Uniform (RS parents every boot-image process) | Uniform (self-heal framework) | **Uniform** — a registry supervises every boot-image server (`fsd`, `cond`, `netd`) |
| Trust topology | Capability-gated endpoints between servers | Trait boundaries | **Capability send-mask** — a per-slot mask enforced at the IPC boundary restricts who each task may reach, now a static baseline plus runtime `DELEGATE` (coarse: one target, non-transitive, in practice shell-only) |
| Kernel/policy split | Kernel is mechanism; PM/VFS/RS are policy | Explicit: `core/` (mechanism) vs. `subsystems/` + `modules_impl/` (policy) | **Partial** — the FS and console are out; scheduler, MMU, and the remaining drivers are still kernel-resident |

---

## What would close the gap, in order of payoff

Not commitments — the natural next moves toward the fuller ideal, and the
shape each would take given what already exists:

The first three moves on this list have since shipped, in order:

- ~~**Move a second driver out of the kernel.**~~ **Done** — the console
  server (`cond`) is a second real EL0 process, proving the `fsd` pattern
  (IPC + grant/safecopy) generalizes.
- ~~**A general supervision / heartbeat mechanism.**~~ **Done** — a
  supervisor registry restarts any server (fsd, cond, netd) on a crash; a
  passive heartbeat catches a *runnable* wedge and an active ping catches
  a *blocked* wedge.
- ~~**A capability model for who-may-call-whom.**~~ **Done** — a per-slot
  IPC send-mask enforced at the `MSG_SEND`/`MSG_CALL` boundary makes the
  isolation topological, not just memory-level.
- ~~**Runtime capability delegation (basic).**~~ **Done** — `DELEGATE`
  (syscall 41) extends the static send-mask at runtime, confined by a "may
  only delegate what you statically hold" rule; its first consumer is
  relay-free program-to-program pipes (the shell out of the byte path).
- ~~**The stdout-over-IPC payoff** (program-to-program pipes,
  `exec … > file`).~~ **Done** — a per-task stdout target routes a program's
  output to the console, the shell (for capture/relay), or, with delegation,
  straight to a consumer.
- ~~**A stack guard page.**~~ **Done** — an inaccessible page below each
  task's stack turns silent overflow into a clean, contained fault (it found
  a real latent 8KB overflow in the shell's `exec` path on its first test).

What's left, in rough order of payoff:

1. **General capability delegation.** Today's `DELEGATE` is coarse — one
   delegated target per task, non-transitive, in practice shell-only.
   MINIX's grant mechanism is general and transitive (any task hands any
   held capability onward, revocably) — needed for direct task-to-task
   streaming without a relay, or a spawned program that runs its own server.
2. **More breadth** — a third component out of the kernel (limited by the
   no-IOMMU DMA constraint on the block transport).
3. **The smaller, already-recorded items** that harden the isolation
   itself: revisiting **per-task ASIDs** with a proven break-before-make
   sequence (the reverted optimization — see the isolation postmortem for
   the real-hardware fault evidence).

None of these is a small task, and none is needed to call the current
state honest. They're the difference between "the mechanisms are real"
(true today) and "the mechanisms are general" (the next era of the
project).

---

## Sources and cross-references

- [`architecture.md`](architecture.md) — the authoritative reference for
  every mechanism named above (privilege model, per-task views, syscall
  ABI, FSOP protocol, grant/safecopy, console discovery).
- [`research-minix-boot.md`](research-minix-boot.md) and
  [`research-helix-os.md`](research-helix-os.md) — the outward-looking
  notes on the two influences. Their "current shape" / "what this says
  about Ouroboros" sections predate the isolation work and are superseded
  by this note; their descriptions of MINIX and Helix themselves stand.
- [`research-directions.md`](research-directions.md) — the forward-looking
  companion to this note: a synthesis across all four influences
  (adding the Plan 9 material) identifying Plan 9's namespaces + uniform
  file protocol as the standout next architecture — the one mechanism that
  would unify the servers, the capability model, and delegation.
- [`isolation-and-dataflow-postmortem.md`](isolation-and-dataflow-postmortem.md)
  — the day-by-day account of the EL0 fault isolation, `fsd`
  supervision, per-task page tables, and grant/safecopy milestones that
  this note assesses.
- [`CHANGELOG.md`](CHANGELOG.md) — the full milestone history, newest
  first.

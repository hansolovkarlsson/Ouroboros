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

The short version: on its **one** moved service, Ouroboros is now a
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
  `FIRST_SPAWNABLE = 3`), so it can never inherit that privilege.
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

- **One service, not a driver fleet.** MINIX runs disk, network,
  keyboard, and every device driver as separate servers started in
  dependency order by `/etc/rc`. Ouroboros has exactly **one**
  out-of-kernel server (the filesystem). The USB/xHCI stack, the block
  device itself, the console/framebuffer, the GIC, and the scheduler are
  all still undifferentiated EL1 kernel code.
- **No boot image of multiple programs.** MINIX packs kernel + PM + VFS +
  RS + drivers into `/boot/image`. Ouroboros loads one configurable
  program (the shell, via `INIT.CFG`) plus one fixed-path infrastructure
  server (`\EFI\ORBS\FSD.BIN`). It's two loaded programs, not a typed set
  of cooperating servers.
- **No `who-may-call-whom` capability model.** Any task can `MSG_CALL`
  any task; the kernel's trust model is still flat within EL0. MINIX's
  server topology is more principled — a driver is granted the specific
  endpoints it may talk to.
- **No process manager / no PID namespace / no `fork`.** Task creation is
  `spawn` (add a task alongside the caller), not the POSIX
  fork-exec-wait lineage MINIX's PM administers. There *is* a
  wait/reaping path (`WAIT`, syscall 21, with `Zombie(status)` slots),
  but no process hierarchy above it.

**Verdict:** structurally a microkernel on the service it actually moved,
using the right IPC primitives. Short of MINIX on *breadth* (one server,
not many) and *topology* (flat trust, no capability graph).

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

- **Per-task MMU page-table views.** Each of the five scheduler slots
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

- **No stack guard page.** An EL0 stack overflow silently corrupts the
  program's *own* region. It's contained to that task's isolation
  boundary — it can't reach another task — but it's silent rather than a
  clean fault.
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
MMU-enforced, injection-verified per-task isolation. It lacks only a
guard page and the reverted TLB optimization.

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
- **The filesystem server is supervised** — MINIX's reincarnation
  server, minimal edition. On a task-2 fault, `syscall::restart_fsd`
  reloads `fsd`'s image (kept in a kernel static, `FSD_IMAGE`, precisely
  because the crashed server *was* the filesystem you'd otherwise need in
  order to reload it) into a fresh region, and the new server re-runs its
  own startup: probe the device, remount from disk. **Its state was
  always disk-derivable, which is what makes this real recovery rather
  than a restart that loses everything.** A 3-restart-per-boot cap guards
  crash loops; past it the kernel degrades gracefully (slot stays
  `Unused`, the same path as a missing `FSD.BIN`).
- **Clients don't hang on a dying server.** A task blocked mid-`MSG_CALL`
  to a server that dies is woken with `TASK_ERR_NO_SUCH_TASK`
  (`fail_calls_to`, wired into all three death paths — exit, kill, and
  fault), which the shell folds into its ordinary "no filesystem"
  message. Previously such a caller waited forever, with Ctrl+C the only
  rescue.

### Where it's short of Helix (and of MINIX's RS)

- **Only `fsd` is supervised.** Recovery is special-cased for one task,
  not a general policy engine watching every component. MINIX's RS and
  Helix's self-heal framework supervise uniformly. A crash in any
  *kernel-resident* driver (xHCI, virtio-blk, console) still has no
  containment at all — it's EL1 code, and a bug there takes the system
  down exactly as the research notes describe.
- **No wedged-server detection.** A server stuck in an infinite loop —
  *not* faulting — is invisible. Catching that needs a watchdog /
  heartbeat, which doesn't exist. Ctrl+C rescues a blocked *client*, but
  nothing rescues a looping *server*. Helix names exactly this (health
  checks for hangs, not just crashes) as a first-class primitive.
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
| Services outside the kernel | Full fleet (disk, net, FS, all drivers) as separate address-space processes | Modules outside the TCB, but within one address space | **One** — the FAT32 filesystem server (`fsd`), a real separate EL0 process |
| Isolation mechanism | Separate address spaces (hardware) | Rust memory safety + hot-reload bookkeeping (no hardware boundary between modules) | **Per-task MMU page-table views (hardware), injection-verified** + validated syscall pointers |
| IPC | Synchronous `SEND`/`RECEIVE`/`SENDREC`, fixed 64-byte messages | Trait-boundary calls + IPC queuing in the scheduler | Synchronous `MSG_CALL` (sendrec-shaped, sender-filtered) + copy-by-mailbox; **grant/safecopy** capability for bulk data |
| Bulk data across the boundary | `sys_safecopy` (capability-gated copy) | Within one address space (no cross-space copy needed) | **`GRANT` + `SAFECOPY`** — the same capability model as MINIX |
| Crash recovery | RS restarts the crashed *process* from a clean slate | Self-heal: watchdog + health monitor + recovery, with attempted state migration | `restart_fsd` reloads the server, remounts from disk (state is disk-derived); **3-restart cap** |
| Non-crashing hang recovery | Timeouts / RS health | Health checks for hangs (first-class) | **None** — no watchdog/heartbeat |
| Live code replacement | No (restart, not hot-swap) | **Yes** — pause/snapshot/swap/restore/rollback | No (reload same image) |
| Supervision scope | Uniform (RS parents every boot-image process) | Uniform (self-heal framework) | **Special-cased** — only `fsd` |
| Trust topology | Capability-gated endpoints between servers | Trait boundaries | **Flat** within EL0 — any task may message any task |
| Kernel/policy split | Kernel is mechanism; PM/VFS/RS are policy | Explicit: `core/` (mechanism) vs. `subsystems/` + `modules_impl/` (policy) | **Partial** — the FS is out; scheduler, MMU, drivers, console are still kernel-resident |

---

## What would close the gap, in order of payoff

Not commitments — the natural next moves toward the fuller ideal, and the
shape each would take given what already exists:

1. **Move a second driver out of the kernel.** This is the single highest
   -value proof: that `fsd` wasn't a one-off, and that the IPC +
   grant/safecopy machinery generalizes. A console server or a block
   server are the obvious candidates. Doing it a second time is what
   turns "a microkernel on one service" into "a microkernel."
2. **A general supervision / heartbeat mechanism.** Today `restart_fsd`
   is bespoke. A uniform "any registered server can be health-checked and
   restarted" facility — MINIX's RS, or Helix's self-heal pipeline in
   miniature — would catch the *wedged* (looping, non-faulting) failure
   mode that currently has no answer, and would make the recovery story
   apply to whatever the step above moves out.
3. **A capability model for who-may-call-whom.** The flat "any task can
   message any task" trust topology is the last big structural gap
   against MINIX. A per-task table of permitted endpoints would make the
   isolation *topological*, not just memory-level.
4. **The smaller, already-recorded items** that harden the isolation
   itself: a stack **guard page** (turn silent stack-overflow corruption
   into a clean fault), and revisiting **per-task ASIDs** with a proven
   break-before-make sequence (the reverted optimization — see the
   isolation postmortem for the real-hardware fault evidence).

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
- [`isolation-and-dataflow-postmortem.md`](isolation-and-dataflow-postmortem.md)
  — the day-by-day account of the EL0 fault isolation, `fsd`
  supervision, per-task page tables, and grant/safecopy milestones that
  this note assesses.
- [`CHANGELOG.md`](CHANGELOG.md) — the full milestone history, newest
  first.

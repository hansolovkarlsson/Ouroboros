# Multi-core: the plan

**The arc for the week of 2026-10-12, set by Hans on 2026-10-10**, while
DevTools's C compiler is being finished and Ouroboros has a clear week. The
roadmap's Pi direction 4 asked for a plan document before any code, the way
`roadmap-el1-drop.md` was, and this is it. Nothing in it is built.

Today one core runs everything. The firmware hands the kernel off on one
core, the others are never started, and one argument makes every mutable
static in the kernel sound: the kernel runs on one core and never runs at
EL1 with interrupts unmasked, so no two threads of execution ever exist
(`kernel/src/synccell.rs`, "The argument"). Multi-core is the end of that
argument. The hard part of this arc is not starting the cores, which is a
few hundred lines against interfaces the kernel already finds; it is
replacing one sentence that covers about a hundred and twenty statics with
a proof per static.

## What the argument covers

The census, computed rather than copied (the postmortems are clear that a
count written down rots):

```sh
grep -c -E '^(pub(\(crate\))? )?static ' kernel/src/*.rs | awk -F: '{s+=$2} END {print s}'   # every static
grep -c 'SyncCell<' kernel/src/*.rs | awk -F: '{s+=$2} END {print s}'                           # SyncCell uses
grep -c '^unsafe impl.*Sync for' kernel/src/*.rs | awk -F: '{s+=$2} END {print s}'            # hand-rolled Sync
```

On 2026-10-10 these gave about 120 statics, 42 `SyncCell` uses and 23
hand-rolled `Sync` wrappers (`synccell.rs`'s own among them); the rest are
atomics and immutable tables. The atomics are not safe by being atomic: an
`AtomicUsize` named `CURRENT` is correct on one core and wrong on four, and
`INPUT_OWNER` read on one core while the tick on another changes it is a
race the type does not see. The invariant the argument also rests on, that
EL1 never runs with IRQs unmasked, stays true per core and is not what
changes; what changes is that two cores can be inside the kernel at once.

## The inventory, by owner

Every static falls in one of five classes. The class decides what step 4
does to it. The plan's first deliverable (step 0) is a check that every
static in the tree is in one of these classes by name, so a static added
later cannot be left out of the argument silently.

| class | what is in it | what multi-core does to it |
| --- | --- | --- |
| **A. Boot-only.** Written before the exit, or before the second core starts, read after. | `bootid::IDENTITY`; the eleven `earlyfault` cells (dead once `exceptions::install` runs); `power::CONDUIT`; `gic::INFO`; `mmu::STORED_MEMORY_MAP`, `STORED_EXTRA_DEVICES`, `STORED_UNCACHED`; `main::ENTRY_SP`; `font::FONT8X8_PRINTABLE` | Nothing. The argument becomes "written before `CPU_ON`", and `CPU_ON` is the happens-before edge: a barrier before the call, and the secondaries read what the boot core wrote. |
| **B. Per-core by nature.** One per core, or held in a system register. | `main::KERNEL_STACK`; `gicv3::SGI_BASE` (this core's redistributor frame, found by `MPIDR_EL1`, so it is wrong on any other core today); the timer, which is system registers; `tasks::CURRENT`; `exceptions::TICKS`; the exception frame on each core's stack | Arrays indexed by core, with the core's index in `TPIDR_EL1` so the index is one instruction away in every trampoline. The stack array is the first thing the secondary entry stub needs. |
| **C. Scheduler-owned.** The per-slot tables, indexed by task, touched by the task itself and by others (a sender to its mailbox, a parent at spawn, the supervisor, a kill). | In `tasks.rs`: `TASKS`, `REGIONS`, `STATES`, `MAILBOXES`, `GRANTS`, `ARGVS`, `ENVS`, `CWDS`, `NAMESPACES`, `SENDER_CREDS`, `IDS`, `SAVED_IDS`, `GROUPS`, `GROUP_COUNTS`, `PARENTS`, `DELEGATED_SEND`, `STDOUT_TARGET`, `CURRENT_CALLS`, `NEXT_CALL`, `GENERATIONS`, `NEXT_GENERATION`, `NEXT_RUNTIME_REGION_TOP`, `IDLE_REGION`; `supervisor::REGISTRY`; the translation tables in `mmu.rs` (`L0_TABLES`, `L1_TABLES`, `EXTRA_L1_TABLES`, `EL0_L2_TABLES`, `EL0_L3_TABLES`, `NC_L2_TABLES`, `NC_L3_TABLES`, `NC_NOTES`, `DEFERRED_REFUSAL`); the staging buffers in `syscall.rs` (`SPAWN_STAGING`, `ARGS_STAGING`, `CWD_STAGING`, `ENV_STAGING`, `PENDING_ENV_LEN`) and `TASK_REPORTS` | One scheduler lock first: a spinlock taken at every kernel entry that touches a table, released before the `eret`, held only with IRQs masked (which at EL1 they always are). Per-slot locks later, if the measurement in step 4 says the one lock costs. The staging buffers become per-core (class B) rather than locked, since a spawn on two cores at once is otherwise serialized by a 2 MB copy. |
| **D. Device-owned.** A device's handle and its DMA memory. | `syscall::BLOCK`, `NET`, `RNG`; `xhci::XHCI` and its seventeen DMA statics (`DMA_POOL`, the rings, contexts and buffers); the twenty virtio wrappers (`DescTable`, `AvailRing`, `UsedRing`, buffers) in `virtio_blk`, `virtio_net`, `virtio_rng`, `virtio_console`; `usb_msd`'s four counters; `gicv3::GICD_BASE` | A lock per device. Each has one user today (`BLOCK_*` is `fsd`'s alone, `NET_*` `netd`'s), which would allow pinning instead, but the xHCI controller is already shared between the tick's keyboard poll and `fsd`'s storage requests, so a per-device lock is the choice that is correct for all four without a placement rule. The DMA barriers `check-xhci-barriers` finds stay as they are; the uncached region is in the shared tables, so every core maps it alike. |
| **E. Console and keyboard.** Written from every core's fault handler, from `cond`'s syscalls and from the tick. | `console::CONSOLE`, `CONSOLE_QUIET`; `fbdev::FB`; `syscall::KBD_QUEUE`; `tasks::INPUT_OWNER`, `PREVIOUS_OWNERS`, `FOREGROUND_COMMAND`, `PENDING_KILL`, `KBD_RAW`; `exceptions::NET_INTID` | A console lock of its own, which a fault handler takes with a timeout so a core that faults while another holds it still reports (the fault line then says the console was busy). The keyboard state goes under the scheduler lock: it is scheduler state (who is foreground, who dies), and the tick that reads ahead for the owner runs on one core (the next section). |

The classes are claims. Step 0's check holds the names; the review of each
step holds the class.

## The three decisions to take before cores run tasks

Each of these is cheaper to settle on one core than to redo under a lock,
and each is already a tail on the roadmap. They are step 3.

1. **FP/SIMD: eager or lazy.** The save is eager on every resumable path
   since #227, with "measure before going lazy" held. A lazy scheme traps
   the first FP use after a switch through `CPACR_EL1.FPEN`, which is
   per-core state and a per-core trap handler. Decide once, before there
   are four of each. The measurement is the cost of the 800-byte save on a
   syscall that uses no FP, from `MONOTONIC_US` around a tight `GET_ID`
   loop, eager against a build with the save removed.
2. **The tick's jobs.** The tick does four things besides switching: it
   counts (`TICKS`, uptime), it reads the keyboard ahead for the owner
   (#232), it drives the supervisor's progress-per-tick wedge detection
   (`WEDGE_TICKS` at 128 ticks of 20 ms) and it ends timed waits (`poll`,
   `SLEEP_UNTIL`, `KEY_WAIT_UNTIL`). With a timer per core there are four
   tick streams. The decision proposed here: the boot core's tick keeps all
   four jobs, every other core's tick only switches tasks and ends its own
   tasks' timed waits. The supervisor then watches a server wherever it
   runs, by its state, not by whose tick saw it. This is recorded in
   `tasks.rs` and `supervisor.rs` before step 4 changes either.
3. **Task 0's entry.** `tasks::start`'s first `eret` loads task 0's FP state
   but not its general registers, bypassing the trampolines (a tail of
   #227). A secondary core enters its first task the same way, so there
   would be four copies of the bypass. Make the trampolines' restore tail
   the one entry into any task first, on one core, with `make test-fpsimd`
   as the rig that must stay green.

## Design

**Starting a core.** The boot core does everything it does today, up to the
point where the tables are built and `exceptions::install` has run. Before
`tasks::start`, it reads the MADT's GICC entries for the other cores' MPIDR
values (`madt.rs` already walks the table for the GIC; the per-core
structures are type 0x0B, one per core) and calls PSCI `CPU_ON` (SMC64
function `0xC400_0003`: target MPIDR, entry address, a context word) for
each, through the conduit `power.rs` found in the FADT. The context word is
the core's index. The call is made from the exception level the boot core
runs at after the drop, EL1, so on a platform whose conduit is `hvc` and
whose firmware handed off at EL2 the call cannot be made (an `hvc` from EL1
would reach the kernel's own abandoned EL2). `power.rs` already states this
case for power-off. On the Pi the conduit is `smc` and TF-A answers it;
QEMU's `virt` machine answers `hvc` when it hands off at EL1 and (predicted)
`smc` under `virtualization=on`, since the guest then owns EL2. Step 2
checks both on QEMU before the board.

**What a secondary core wakes into.** `CPU_ON` delivers the core with the
MMU and caches off, at the exception level the firmware uses for the boot
core (EL2 on the Pi, EL1 on QEMU's default), with `x0` holding the context
word. The entry stub, in `global_asm!` next to `KERNEL_STACK`'s switch, does
in order: load the stack top for its index from the per-core stack array,
write its index to `TPIDR_EL1`, and, if at EL2, run `el2.rs`'s drop with
the boot core's already-built identity map (EL1's `TTBR0`, `MAIR`, `TCR` and
`SCTLR` are the registers `switch_to_identity_map` already writes), else
run the register half of the switch directly. Only then does it call Rust:
`VBAR_EL1`, its GIC redistributor (`gicv3::init` already finds this core's
frame by `MPIDR_EL1`, which is why it is class B), the CPU interface, then
a line through the console lock, `core N up at EL1`, then `wfe` with IRQs
masked. The tables are shared, so one `tlbi` and `ic` per core at the
switch and no table change thereafter needs a cross-core shootdown until a
spawn changes a region, which is step 4's problem (an SGI that asks every
core to `tlbi vmalle1`).

**The fault reporter learns the core.** Every `EXCEPTION` line gains the
core index from `TPIDR_EL1`, and the halt on a fault becomes a halt of that
core with a line, not of the machine: a secondary that faults in step 2
must leave the boot core's shell working, or the step cannot be bisected.

**Scheduling, when it comes.** One run queue, protected by the scheduler
lock, and a `CURRENT` per core. A core's tick takes the lock, saves the
interrupted task into its slot, picks the next runnable task that is not
running on another core (a `STATES` value `Running(core)` makes that one
comparison), and restores it. A task made runnable by a message from
another core is picked up at that core's next tick or by an SGI sent to an
idle core, whichever the measurement favours. Servers start pinned to the
boot core, with their device locks making the pin a performance choice and
not a correctness one. Kills and keyboard owner changes become an SGI to the
core that runs the target, so the per-task resets in `end_task` run where
the task's registers are.

## Steps, each with the check that can fail

- [ ] **Step 0. This plan, and the inventory as a check.**
      `scripts/check-statics.py`: lists every `static` in `kernel/src` by
      the grep above and requires each name to appear in exactly one class
      row of this document's table; fails naming any static the table does
      not hold, and any name in the table that the tree no longer has.
      Added to `make test`. The check fails today if one name in the table
      above is misspelt, which is how it is proven when it is written.
- [ ] **Step 1. The cores found.** `madt.rs` returns the list of GICC
      entries (MPIDR, enabled flag) and the boot log says `N cores: ...`.
      Check: QEMU `-smp 4` says 4 and lists four affinities; `-smp 1` says
      1; `make run` is unchanged. The line is read by the rig of step 2.
- [ ] **Step 2. The cores started and parked.** `CPU_ON` per core, the
      entry stub, per-core stacks, the drop per core, `VBAR_EL1`, the
      redistributor, the console lock, the `core N up` line, `wfe`. The
      fault reporter prints the core. Rig: `make test-smp` boots `-smp 4`
      and requires four `core N up` lines (and the boot core's own), then
      the shell answering `help`, then a dwell of ten seconds with no fault
      line, then the same under `virt,virtualization=on` (the EL2 path per
      core). Controls: with the stub's MMU enable removed the secondaries
      fault and the rig turns red on their lines while the shell still
      answers, which also proves the per-core fault report; with
      `CPU_ON` not called the rig is red on the count. Then the Pi 4 over
      serial: four lines, one boot. Parallels: however many vCPUs the VM
      has, by `make test-parallels`'s screenshot of the boot log.
- [ ] **Step 3. The three decisions,** each recorded in the code it
      governs with its measurement or rig: the FP cost figure in
      `exceptions.rs`, the tick's jobs in `tasks.rs` and `supervisor.rs`,
      and task 0 entered through the trampolines with `make test-fpsimd`
      and `make test-kbd-mode` green.
- [ ] **Step 4. Tasks on cores.** In order, each its own PR: (a) the
      scheduler lock and the device locks, taken and released on one core,
      with the median 9P verb cycle measured before and after (the figure
      to beat is 4.53 ms from `roadmap-session-auth.md`); (b) `CURRENT` per
      core, an idle task per core, the timer armed on every core, the
      boot core's tick keeping its jobs; (c) the SGI for kills, owner
      changes and TLB shootdown; (d) placement, round-robin across cores
      with servers pinned. The rig grows with each: two spinning programs
      both progressing in `ps`, `fpprobe | fpprobe -` with the two probes on
      two cores (the FP state of each core's task kept apart), and the whole
      existing suite run with an `SMP=4` knob on `drive-qemu.py`'s `Guest`,
      because the keyboard, kill and call rigs are the checks that would
      see a race. QEMU's multi-threaded TCG runs each virtual core on a host
      thread, so a race here is a real race, not a modelled one.

## Size

Step 0 and 1 together are a day. Step 2 is two or three days, most of it
the entry stub and the EL2 path per core, and the Pi boot. Step 3 is two
days, one of them measurement. Step 4 is a week or more, and (a) alone
decides whether the rest is a week or a month. The week set for this arc
covers steps 0 to 3 and the start of 4(a), which is the honest size; the
arc is done when 4(d) is, not when the week is.

## Risks

- **A static the inventory missed.** The one way this arc fails silently.
  Step 0's check is the answer, and it has to be in `make test`, not a
  rig, because the static that falsifies the argument is added in another
  file on another day (`true-when-written-postmortem.md`).
- **The conduit after the drop.** An `hvc` conduit with an EL2 handoff
  means no `CPU_ON` from EL1. The Pi is `smc`; QEMU under
  `virtualization=on` is predicted `smc` and checked in step 2. If a
  platform turns out to need `hvc` from EL2, `CPU_ON` moves before the
  drop, and the secondaries are started at EL2 by a boot core still at EL2,
  which the design above allows (the stub drops each core itself).
- **A lock taken where it cannot be.** The scheduler lock is held only with
  IRQs masked and never across an `eret`, so the one deadlock is an SGI
  handler that takes a lock its sender holds. The rule, stated in the lock's
  module: an SGI handler takes no lock. Its work is a flag the target core
  acts on at its next tick.
- **Performance under one lock.** Measured in 4(a) before anything else is
  built on it. If the verb cycle regresses past what the measurement
  tolerates, per-slot locks for class C come forward.
- **The DMA memory and the caches.** Unchanged in content: the uncached
  region and the barriers are already there for the Pi's non-coherent
  PCIe, and shared tables map it alike on every core. The new exposure is
  a device lock released before a DMA buffer's last barrier; the rule is
  barrier then release, in that order, in every device path.
- **The hardware targets.** Parallels' vCPU count is a VM setting; the Pi
  4's four A72s wake through TF-A. Neither has a rig beyond the boot log,
  so step 2's hardware checks are the boot log read by eye, as every Pi
  check is today.

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
| **A. Boot-only.** Written before the exit, or before the second core starts, read after. | `bootid::IDENTITY`; `madt::CORES`, the cores the MADT lists, written by `discover` before the exit; `mmu::BOOT_REGIME`, the EL1 regime the boot core switched to, `smp::PARAMS`, what a secondary reads with its MMU off, and `smp::BOOT_INDEX`, the boot core's own MADT index, all written once before any `CPU_ON`; the early fault reporter's cells, dead once `exceptions::install` runs: `earlyfault::CONSOLE`, `earlyfault::FRAMEBUFFER`, `earlyfault::STACKS`, `earlyfault::RAM`, `earlyfault::RAM_TRUNCATED`, `earlyfault::TEST_FAULT`, `earlyfault::READING`, `earlyfault::PLANT_WALK_FAULT`, `earlyfault::IMAGE_RANGE`, `earlyfault::IMAGE_TABLE`, `earlyfault::ENTERED`; `power::CONDUIT`; `gic::INFO`; `gicv3::GICD_BASE`; `exceptions::NET_INTID`; `mmu::STORED_MEMORY_MAP`, `mmu::STORED_EXTRA_DEVICES`, `mmu::STORED_UNCACHED`; `main::ENTRY_SP`; the boot flags `usb_msd::STALLS_REQUESTED` and `usb_msd::INJECT_STALLS`; `font::FONT8X8_PRINTABLE`, which is immutable | Nothing. The argument becomes "written before `CPU_ON`", and `CPU_ON` is the happens-before edge: a barrier before the call, and the secondaries read what the boot core wrote. |
| **B. Per-core by nature.** One per core, or held in a system register. | `main::KERNEL_STACK` and `smp::STACKS`, one per secondary; `smp::CORE_UP`, each core's own mark; `gicv3::SGI_BASE` (this core's redistributor frame, found by `MPIDR_EL1`, so it is wrong on any other core today); the timer, which is system registers; `tasks::CURRENT`; `exceptions::TICKS`; the exception frame on each core's stack; the staging buffers a syscall copies through, `syscall::SPAWN_STAGING`, `syscall::ARGS_STAGING`, `syscall::CWD_STAGING`, `syscall::ENV_STAGING` and `syscall::PENDING_ENV_LEN`, per core rather than locked, since a spawn on two cores at once would otherwise be serialized by a 2 MB copy | Arrays indexed by core, with the core's index in `TPIDR_EL1` so the index is one instruction away in every trampoline. The stack array is the first thing the secondary entry stub needs. |
| **C. Scheduler-owned.** The per-slot tables, indexed by task, touched by the task itself and by others (a sender to its mailbox, a parent at spawn, the supervisor, a kill). | `tasks::TASKS`, `tasks::REGIONS`, `tasks::STATES`, `tasks::MAILBOXES`, `tasks::GRANTS`, `tasks::ARGVS`, `tasks::ENVS`, `tasks::CWDS`, `tasks::NAMESPACES`, `tasks::SENDER_CREDS`, `tasks::IDS`, `tasks::SAVED_IDS`, `tasks::GROUPS`, `tasks::GROUP_COUNTS`, `tasks::PARENTS`, `tasks::DELEGATED_SEND`, `tasks::STDOUT_TARGET`, `tasks::CURRENT_CALLS`, `tasks::NEXT_CALL`, `tasks::GENERATIONS`, `tasks::NEXT_GENERATION`, `tasks::NEXT_RUNTIME_REGION_TOP`, `tasks::IDLE_REGION`; `supervisor::REGISTRY`; `lock::KERNEL_LOCK`, the lock itself (step 4(a)); the translation tables and their notes, `mmu::L0_TABLES`, `mmu::L1_TABLES`, `mmu::EXTRA_L1_TABLES`, `mmu::EL0_L2_TABLES`, `mmu::EL0_L3_TABLES`, `mmu::NC_L2_TABLES`, `mmu::NC_L3_TABLES`, `mmu::NC_NOTES`, `mmu::DEFERRED_REFUSAL`; `syscall::TASK_REPORTS` | One scheduler lock first: a spinlock taken at every kernel entry that touches a table, released before the `eret`, held only with IRQs masked (which at EL1 they always are). Per-slot locks later, if the measurement in step 4 says the one lock costs. |
| **D. Device-owned.** A device's handle and its DMA memory. | `syscall::BLOCK`, `syscall::NET`, `syscall::RNG`; the xHCI controller, `xhci::XHCI`, and its DMA memory, `xhci::DMA_POOL`, `xhci::USB_CBW_BUF`, `xhci::USB_CSW_BUF`, `xhci::USB_DATA_BUF`, `xhci::DCBAA`, `xhci::SCRATCHPAD_ARRAY`, `xhci::SCRATCHPAD_PAGES`, `xhci::COMMAND_RING`, `xhci::EP0_RINGS`, `xhci::INT_RING`, `xhci::BULK_IN_RING`, `xhci::BULK_OUT_RING`, `xhci::EVENT_RING`, `xhci::ERST`, `xhci::INPUT_CONTEXT`, `xhci::OUTPUT_DEVICE_CONTEXTS`, `xhci::CTRL_BUF`, `xhci::INT_BUF`; the USB storage counters `usb_msd::NEXT_TAG` and `usb_msd::RECOVERY_LOG_COUNT`; the virtio rings and buffers, `virtio_blk::DESC_TABLE`, `virtio_blk::AVAIL_RING`, `virtio_blk::USED_RING`, `virtio_blk::REQ_HEADER`, `virtio_blk::REQ_STATUS`, `virtio_net::RX_DESC`, `virtio_net::RX_AVAIL`, `virtio_net::RX_USED`, `virtio_net::TX_DESC`, `virtio_net::TX_AVAIL`, `virtio_net::TX_USED`, `virtio_net::RX_BUFS`, `virtio_net::TX_BUF`, `virtio_rng::DESC_TABLE`, `virtio_rng::AVAIL_RING`, `virtio_rng::USED_RING`, `virtio_rng::RNG_BUF`, `virtio_console::DESC_TABLE`, `virtio_console::AVAIL_RING`, `virtio_console::USED_RING` | A lock per device. Each has one user today (`BLOCK_*` is `fsd`'s alone, `NET_*` `netd`'s), which would allow pinning instead, but the xHCI controller is already shared between the tick's keyboard poll and `fsd`'s storage requests, so a per-device lock is the choice that is correct for all four without a placement rule. The DMA barriers `check-xhci-barriers` finds stay as they are; the uncached region is in the shared tables, so every core maps it alike. |
| **E. Console and keyboard.** Written from every core's fault handler, from `cond`'s syscalls and from the tick. | `console::CONSOLE`, `console::CONSOLE_QUIET` and `console::CONSOLE_LOCK`, the lock itself; `fbdev::FB`; `syscall::KBD_QUEUE`; `tasks::INPUT_OWNER`, `tasks::PREVIOUS_OWNERS`, `tasks::FOREGROUND_COMMAND`, `tasks::PENDING_KILL`, `tasks::KBD_RAW` | A console lock of its own, which a fault handler takes with a timeout so a core that faults while another holds it still reports (the fault line then says the console was busy). The keyboard state goes under the scheduler lock: it is scheduler state (who is foreground, who dies), and the tick that reads ahead for the owner runs on one core (the next section). |

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

- [x] **Step 0. This plan, and the inventory as a check. Done 2026-10-10.**
      `scripts/check-statics.py` lists every `static` in `kernel/src`
      (indented ones too, so `tasks::CURRENT` inside its module counts) and
      requires each, as `module::NAME`, to appear in exactly one class row
      of the table above; it fails naming any static the table does not
      hold, any name in two rows, and any name the tree no longer has. Run
      by `make test` and `make check-statics`. Proven when written: a
      misspelt name in the table and a static added to a kernel file each
      turned it red with the name.
- [x] **Step 1. The cores found. Done 2026-10-10.** `madt::discover`
      collects every GICC entry's MPIDR and Enabled flag into
      `madt::CORES` (up to `MAX_CORES`, 16, with a `truncated` flag past
      that), and the boot log says `MADT: N cores (this core affinity A, mpidr M)` (`madt::affinity` masks the register to its affinity fields, which is all the table records: QEMU's boot core reads `0x8000_0000` where the table says `0x0`)
      then one `MADT: core i: mpidr M, enabled` line per core, before the
      exit. `make test-smp` (`scripts/test-smp.py`) boots `-smp 4` and
      `-smp 1`: the count, one line per core with distinct MPIDRs, the boot
      core among them, every core enabled, the shell answering `help`, no
      fault line. Step 2 extends the same rig. Control: with the MPIDR
      collection removed the count reads 0 and the rig is red.
- [x] **Step 2. The cores started and parked. Built 2026-10-10 on QEMU;
      the Pi 4 and Parallels boots are still owed.** `kernel/src/smp.rs`:
      `CPU_ON` per core through `power::cpu_on`, the entry stub
      `smp_secondary_entry` (no stack until the MMU is on; at EL2 the
      hypervisor controls and EL1's regime as `el2::drop_to_el1` writes
      them, then `eret`; at EL1 the regime and the MMU on in place), one
      64 KB stack per secondary, `TPIDR_EL1` the core index, this core's
      GIC interface (`gic::init_this_core`, split out of both backends),
      the console lock, the `smp: core N up at EL1 (...), parked` line,
      `wfe` with everything masked. `rust_exception_handler` names the
      core (`EXCEPTION core=N ...`, the MADT's index, which the boot core
      takes as its own once the table is read: ACPI fixes no order on
      ARM) and checks that core's own stack canary. `make test-smp` is
      six boots: `-smp 4` on the dev-loop machine, under
      `virt,virtualization=on`, under `gic-version=3` and under both (three up lines,
      the summary `3 of 3`, `help`, a ten-second dwell, `help` again, no
      fault), `-smp 1` (no up line, `0 of 0`), and `-smp 4` with
      `\SMPFAULT` (the first core started takes `udf` after its up line;
      the fault line names it; the shell still answers). All green on the
      first run but for the rig's own expectations of a vvfat boot. The
      high review of #261 then found four defects the rig had not: a
      secondary at MADT index 0 took a stack below the array, the stub's
      own code was not cleaned to PoC, a secondary's GICv3 init clobbered
      the boot core's redistributor pointer, and a core dying with the
      console lock made every later line pay the bound; each fixed in the
      same PR, the GICv3 boot added for the third. A second high review
      found four more worth the name: `CPU_ON`'s asm let the compiler keep
      `entry` in a register SMCCC lets the callee scratch; `CON_WRITE` took
      the console lock per byte, so another core's line could land inside
      `cond`'s escape sequence; a core counted as up before its first
      console write; and a panic on a secondary (GICv3's redistributor
      lookup) had no path but the firmware's. Each fixed, the lock now
      owned by core so a takeover leaves one owner, and the EL2-with-GICv3
      boot added. Control: with `CPU_ON` not called the rig is red on the
      up lines and the summary.
      Pi 4 over serial and Parallels: by eye from the boot log, not yet
      done.
- [x] **Step 3. The three decisions. Done 2026-10-10.** (1) FP/SIMD
      stays eager, on a measurement: `make measure-syscost`
      (`/bin/SYSCOST`, 200,000 `GET_TICKS` calls, three runs, the median)
      gave 24,934 ns a syscall on the tree and 25,232 ns with the save and
      restore macros emptied; under TCG the exception round trip is about
      25 µs and the save is within its noise, so the figure that would
      change the decision is the Pi's, owed. Recorded in `exceptions.rs`'s
      module doc with what a lazy scheme would cost per core. (2) The
      tick's jobs: the boot core's tick keeps counting, the keyboard
      read-ahead, the supervisor's checks and timed waits; a secondary's
      tick (step 4) switches its own tasks and ends their timed waits.
      Recorded on `tasks::on_tick` and in `supervisor.rs`. (3) Task 0
      enters through the trampolines' restore tail (`resume_frame`, a
      global label in `exceptions.rs`'s IRQ path) from a copy of its
      Context laid out as a frame, so x0-x30 are loaded too and no eret
      into a task exists outside the trampolines; `make test-fpsimd`,
      `make test-kbd-mode` and `make test-smp` green.
- [ ] **Step 4. Tasks on cores.** In order, each its own PR: (a) **done
      2026-10-10: the kernel lock**, `kernel/src/lock.rs`, taken at the
      start of every resumable entry's Rust half (a syscall, the tick, an
      EL0 fault) and released before the trampoline's `eret`, owned by
      core index, never reentrant (a re-entry or a stray release halts
      with a line). One lock, not the scheduler lock and device locks the
      plan first named: while one core at a time is in the kernel, a
      device lock guards nothing the kernel lock does not, so per-device
      and per-slot locks are the SPLITTING of this one, each paid for by a
      measurement, after 4(b) has a second core contending for it. The
      measurement here is `make measure-syscost`: 24,934 ns a syscall
      before, 27,786 ns with the lock, an uncontended acquire and release
      per entry, which under TCG is two atomic read-modify-writes through
      its slow path; on hardware those are tens of nanoseconds, and the
      Pi's figure is the one that counts. The 9P verb cycle (4.53 ms on
      the two-VM rig) is the figure for the split, not for this. (b) `CURRENT` per
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

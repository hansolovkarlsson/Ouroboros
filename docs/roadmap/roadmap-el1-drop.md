# Dropping to EL1: the plan

**The Pi port's first real finding, scoped before any code.** The Raspberry
Pi's firmware (pftf, over TF-A's BL31) hands the kernel off at **EL2**, and
the kernel has only ever run at EL1. Everything it writes after
`exit_boot_services` to make the machine its own, the vector base, the
translation tables, the timer, goes to an `_EL1` register that the exception
level it is running at does not use. The boot therefore keeps running on the
firmware's EL2 tables and vectors while its log says otherwise.

**Status 2026-10-02: done. Steps 0 to 2 built and proven on QEMU, merged as
#188 (`367ceda`) after five review rounds; step 3, the board, ran the same
day: the drop landed on the Pi 4 and the shell came up. Step 4, the xHCI
takeover with the reporter, stays open, now beside an `ExitBootServices`
fault at the same firmware address (`testing-pi4.md` section 6).** The
drop is `kernel/src/el2.rs`; `make test-el1-drop` is its rig, and a
mutation that disabled the drop turned the rig red on three checks. Read
on the way: QEMU's firmware leaves `SCTLR_EL1` at `0x30d0198d` on an EL1
handoff, which is what the drop writes; at EL2 it leaves `HCR_EL2` at
`0x8000038` (`RW` with `IMO`/`FMO`/`AMO`), so the drop's `RW`-only write
is what moves interrupts to EL1; and its FADT names `smc` under
`virtualization=on` and `hvc` plain, both of which power off.

Grounded in the code at `2c0c8d6` (2026-10-01). Found by the early fault
reporter's first dump on a Pi 4 (`testing-pi4.md` section 6): a fault
delivered through the firmware's vectors after `exceptions::install()`,
`SPSR` in EL2h, a translation fault at `0xa000000` after the identity map had
"installed". Reproduced the same evening on QEMU with
`-machine virt,virtualization=on`: the firmware then runs at EL2 as well, the
boot reaches `shell ready` on QEMU's permissive firmware map, and the
firmware's EL2 timer interrupt, which the kernel's EL1 timer setup never
stopped, lands in the firmware's vectors and faults at address 0 (*as
written on 2026-10-01. Wrong: the dump's fields, `esr=0x86000007`,
`elr=0`, `spsr=0x800003c9`, are an instruction abort at address 0 taken at
EL2h with interrupts masked, which is the register table's own row for
`eret`: `tasks::start`'s first `eret` into task 0 restored the firmware's
stale `ELR_EL2` and `SPSR_EL2`. No interrupt was involved. Found by the
fifth review on 2026-10-02; `el2.rs`'s module doc is the account*). **That
option is the dev loop for this plan: every step below is checked on QEMU
before it goes near a board.**

## What is wrong, register by register

Every system register the kernel touches after the exit, from a grep of the
`msr`/`mrs` lines (`exceptions.rs`, `mmu.rs`, `tasks.rs`, `timer.rs`,
`gicv3.rs`, `main.rs`):

| Register | Written by | At EL2 it is |
|---|---|---|
| `VBAR_EL1` | `exceptions::install` | not the vector base in use (`VBAR_EL2` is) |
| `TTBR0_EL1`, `TCR_EL1`, `MAIR_EL1` | `mmu::install_identity_map` | not the translation regime in use (`TTBR0_EL2` and friends are) |
| `SCTLR_EL1` (`nTWE`/`nTWI`) | `tasks.rs` | not the control register in use |
| `CNTP_CTL_EL0`, `CNTP_TVAL_EL0` | `timer.rs` | the EL1 physical timer, which is not what interrupts EL2 (the firmware uses `CNTHP`) |
| `ELR_EL1`, `SPSR_EL1`, `SP_EL0` and `eret` | `main.rs`, `tasks.rs`, `exceptions.rs` | `eret` at EL2 restores from `ELR_EL2`/`SPSR_EL2`: the first entry into task 0 goes somewhere else entirely |
| `ICC_*_EL1` | `gicv3.rs` | reachable only if `ICC_SRE_EL2.Enable` is set; the Pi is GICv2, so moot there, but a GICv3 board at EL2 would trap |
| `PAR_EL1` via `AT S1E1R` | `mmu.rs`'s device check | walks the EL1 regime that is not in use, so the check passes on tables nobody runs on |

Reads of `ID_AA64MMFR0_EL1`, `CTR_EL0`, `CNTFRQ_EL0`, `CNTPCT_EL0` and
`MPIDR_EL1` are fine at any EL.

Not affected: everything before the exit (boot services run the machine at
whatever EL they have), and the xHCI takeover fault, which happens before the
exit in firmware code and stays its own item.

## The decision: drop, do not stay

Two shapes were weighed. **Stay at EL2** and use the `_EL2` registers
throughout: no. Without VHE (the Cortex-A72 is ARMv8.0) EL0 cannot run under
EL2's translation regime, and with `HCR_EL2.TGE` set EL0 runs with stage 1
disabled, so there is no process isolation at all; the kernel's whole EL0
story would have to be rebuilt. **Drop to EL1** right after the exit, as every
general-purpose kernel's entry code does (Linux's `el2_setup`): yes. The
kernel keeps its design; the firmware is done with the machine by then; and
on QEMU and Parallels, which hand off at EL1, the drop is a no-op guarded by
`CurrentEL`.

## Design: prepare EL1 at EL2, then `eret` into a running MMU

The naive drop, `eret` to EL1 with its MMU off and then switch on, opens a
window where the kernel runs with every access Device-nGnRnE while dirty
lines written under the firmware's cacheable EL2 mapping (the stack, the
statics, the memory map) sit in the cache unseen. Avoid the window instead of
cleaning it:

1. **At EL2, after the exit, build the identity map** exactly as
   `mmu::install_identity_map` builds it today (the RAM span from the memory
   map, the device block, the console, the framebuffer, the EL0 regions), but
   stop before the register writes. This means splitting `install_identity_map`
   into a build half (pure memory) and a switch half (register writes); the
   EL1 path keeps today's switch half untouched.
2. **At EL2, set EL1's regime to those tables**: `MAIR_EL1`, `TCR_EL1`,
   `TTBR0_EL1`, then `SCTLR_EL1` with `M`, `C`, `I` set and the RES1 bits,
   `nTWE`/`nTWI` included (*as built: without `nTWE`/`nTWI`, which
   `tasks.rs` ORs in on both paths as before; the value is composed from
   named bits in `el2.rs`, `SPAN` among them*); `tlbi vmalle1` for the
   regime that has never run (*as built, after `VTTBR_EL2` is zeroed*);
   `VBAR_EL1` to the kernel's table so there is no window at EL1 with no
   vectors. All of these are writable from EL2.
3. **At EL2, set the hypervisor controls for a plain EL1 guest of itself**:
   `HCR_EL2 = RW` (EL1 is AArch64) and nothing else (*as built: `RW | HCD`
   plus the PAuth/MTE no-trap bits, `el2.rs`'s `HCR_EL2_VALUE`; the
   reviews added `HCD` so an `hvc` from EL1 reports through the kernel's
   vectors*) (no `VM`, so no stage 2;
   no `TGE`; no `IMO`/`FMO`/`AMO`, so interrupts route to EL1; no traps);
   `CNTHCTL_EL2.EL1PCTEN | EL1PCEN` so EL1 may use the physical counter and
   timer; `CNTVOFF_EL2 = 0`; `CPTR_EL2` with no FP/SIMD trap (`0x33ff`, the
   RES1 pattern); `CPACR_EL1.FPEN = 0b11` so EL0 programs may use FP, which
   on an EL1 handoff the firmware had already allowed; `HSTR_EL2 = 0`;
   `ICC_SRE_EL2.SRE | Enable` only when the GIC is v3.
4. **`SP_EL1 = SP`**, `SPSR_EL2` = EL1h with DAIF masked, `ELR_EL2` = the
   next instruction, `eret`. The kernel continues on the same stack, now at
   EL1, on its own tables, with its own vectors, with the same cacheability
   it had a moment before.
5. **Everything after the drop is unchanged**, except that
   `exceptions::install()` and `mmu::install_identity_map`'s switch half become
   no-ops on this path (already done at step 2), which the code says rather
   than silently repeats.

The guard is `CurrentEL`: at EL1, none of this runs and the boot is today's.
The kernel logs the EL it was handed at the first line after the exit, and
logs the drop, so a capture says which path ran.

## What it changes for the firmware's leftovers

- The firmware's EL2 timer (`CNTHP`) is still armed after the exit; on QEMU
  it fired into the firmware's vectors (*as written; it did not, see
  above. The drop still switches `CNTHP_CTL_EL2` off*). After the drop EL2 takes no
  interrupts (`HCR_EL2.IMO` is 0 and nothing at EL1 unmasks until task 0),
  and the EL1 timer's own IRQ is INTID 30, not the hypervisor timer's 26,
  which `gic.rs` never enables. Disable `CNTHP_CTL_EL2` at the drop anyway,
  so nothing is left ticking.
- The early fault reporter's registered handler is unreachable once EL1 has
  its own vectors; its job ends at the drop, as it does on EL1 platforms at
  `exceptions::install()`.
- PSCI: `power.rs` uses the conduit the FADT names. An `hvc` from EL1 now
  traps to EL2, to the firmware's leftover `VBAR_EL2`, with nobody to answer.
  The Pi's TF-A answers `smc`; if its FADT names `hvc`, `power.rs` falls back
  to the halt, as it does with no conduit. Check the FADT's flag on the board
  (the kernel should log the conduit; it does not today). (*As built: the
  drop sets `HCR_EL2.HCD`, so an `hvc` at EL1 is an undefined instruction
  reported by the kernel's vectors; `power.rs` treats an `hvc` conduit on a
  dropping boot as none, logs every branch, and `shutdown` halts.*)

## Steps, each with the check that can fail

0. ~~**Log `CurrentEL` after the exit, on every platform**, nothing else.~~
   **Done 2026-10-02** (`el2::current_el`, the line `running at EL{n} after
   the exit`, `make run-el2`): QEMU plain prints EL1; `virtualization=on`
   prints EL2 and then, as predicted, reaches `shell ready` and faults at
   address 0 through the firmware's vectors.
1. ~~**Split `mmu::install_identity_map`** into build and switch, behaviour
   unchanged.~~ **Done 2026-10-02**: `build_identity_map` returns a
   `Planned` that `switch_to_identity_map` requires, `build_tables` no
   longer switches, the runtime rebuild switches itself, and `el1_regime`
   is the one derivation of the register values. Checked by the module
   doc's deliberate fault (a write to block index 2 after the switch,
   reported by the kernel's handler, then removed), `make run` to the
   shell, `test-early-fault.py`, and three spawned programs on the image.
2. ~~**The drop**~~ **Done 2026-10-02**, `kernel/src/el2.rs`, called from
   `switch_to_identity_map` when `CurrentEL` is 2: the register sequence of
   the design above, `ICC_SRE_EL2` gated on `ID_AA64PFR0_EL1.GIC` rather
   than the MADT (on a GICv2 the register is undefined), `SCTLR_EL1`
   written whole. Checked on `virtualization=on`: the log says `running at
   EL2 after the exit` then `dropped from EL2 to EL1, on our own tables and
   vectors`, the shell answers `help` after a twelve-second dwell, QEMU's
   `-d int` trace shows IRQs and SVCs and no abort, the same with
   `gic-version=3`, the FAT32 image logs in and spawns, and `shutdown`
   powers off. QEMU plain: unchanged. `make test-el1-drop` runs
   `test-early-fault.py --el2`, whose control requires the handoff level,
   the drop, the identity map line and the answered `help`; with the drop
   disabled it fails on three checks.
3. ~~**The Pi 4 boot, `NOXHCI`**~~ **Done 2026-10-02, by Hans, on the second
   of two boots** (`testing-pi4.md` section 6): `running at EL2 after the
   exit`, `dropped from EL2 to EL1, on our own tables and vectors`,
   `identity map installed`, the GIC and the tick up, `shell ready` on HDMI
   with the serial terminal as the keyboard. The virtio-mmio scan did not
   fault under the kernel's tables (the low 1GB is a Device block there), so
   the bisection predicted below did not happen; the first of the two boots
   died in `ExitBootServices` at the firmware address the 2026-10-01 takeover
   boots died at, which is now its own item. As planned:
   **The Pi 4 boot, `NOXHCI`**: the log past the exit now means what it
   says; `identity map installed` is followed by whatever the Pi does next
   on its own tables, which is the first time that has been observed. The
   virtio-mmio scan at `0xa000000` is expected to fault still, now into the
   kernel's own vectors with the kernel's own report, which is the next
   bisection, and `virtio_mmio_probe_safe`'s premise (a serial console means
   QEMU) is the next thing to retire.
4. **Then the xHCI takeover**, with the reporter, as planned: untouched by
   this plan, and now the only pre-exit item.

## Size

About 150 lines of kernel code (the drop, the split, the log line), a
Makefile target, a test extension, the Pi guide's checkpoint list rewritten
for the drop. Steps 0 to 2 are a day on QEMU; step 3 is one bench round trip.

## Risks

- **The L0 start level.** `mmu.rs`'s module doc records that a single L1
  table with `T0SZ=25` hard-faulted and matching the firmware's `T0SZ=20`
  fixed it, cause never found. On the drop path the EL1 regime starts from
  nothing, so there is no firmware configuration to match; keep `T0SZ=20`
  and the four-level walk as they are, and if the eret into the new regime
  faults, this is the first suspect. The reporter's dump will say so.
- **Cache maintenance at the eret.** The tables are written at EL2 through a
  cacheable mapping and walked by the EL1 regime; the walk reads memory, so
  `dsb ishst` before the TTBR write is what today's switch already does.
  Same for the I-cache: `ic ialluis` after the switch.
- **`SCTLR_EL1`'s reset value is unknown** on the EL2 handoff: write the
  whole register, not read-modify-write.
- **Parallels** hands off at EL1 (every Parallels boot ran the kernel's own
  vectors and tables), so the guard keeps it on today's path; it is parked
  anyway.

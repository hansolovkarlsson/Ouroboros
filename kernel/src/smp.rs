//! The secondary cores: started through PSCI `CPU_ON`, brought to EL1 on
//! the kernel's own tables and vectors, and parked (multi-core step 2,
//! `docs/roadmap/roadmap-smp.md`).
//!
//! The boot core does everything it does today. Once the identity map is
//! installed, the vectors are live and the GIC's distributor is up, it
//! fills [`PARAMS`] with the EL1 regime it runs on (`mmu::boot_regime`),
//! the register values `el2.rs` writes for a drop, the vector table's
//! address and the stack array's, cleans that block to the point of
//! coherency (a secondary reads it with its MMU off, so it must be in
//! memory, not in the boot core's cache), and calls `CPU_ON` for every
//! core the MADT lists but itself (`madt::CORES`, by affinity). A
//! secondary wakes in [`smp_secondary_entry`] with the MMU and caches off,
//! `x0` its core index, at the level the firmware uses for the boot core
//! (EL2 on the Raspberry Pi and QEMU `virtualization=on`, EL1 on QEMU's
//! default and Parallels). The stub uses no stack until the MMU is on, so
//! no line of the stack is ever written with one attribute and read with
//! another: it loads the parameters, sets `TPIDR_EL1` to the core index
//! (the one place a core's index lives; `core_index` reads it from every
//! trampoline and fault line), and then either, at EL2, programs the
//! hypervisor controls and EL1's regime exactly as `el2::drop_to_el1` does
//! and `eret`s into EL1 with the MMU running, or, at EL1, writes the
//! regime and turns the MMU on in place. Only then does it take its stack
//! and call [`secondary_main`], which brings up this core's GIC interface,
//! says `core N up at EL1`, and (step 4(b)) enters its own idle loop at
//! EL0 through `resume_frame`, with its timer armed: from then on it
//! alternates the idle loop and its tick, which only counts
//! (`CORE_TICKS`) and returns. The boot core's tick keeps every job it
//! has (step 3, decision 2); placing tasks on a secondary is step 4(d).
//!
//! Any core can kick another into the kernel (step 4(c)): [`kick`] records
//! a reason for the core and sends it [`KICK_SGI`], with the kernel lock
//! held and without waiting; the kicked core takes the SGI through the
//! ordinary IRQ path, under the same lock, and [`on_kick`] acts on every
//! reason recorded. [`start`] checks it once per core: kicked with the lock
//! held, a core must not answer until the lock is released, then must.
//! There is no kick for a TLB shootdown: `tlbi vmalle1is` and `ic ialluis`
//! reach every core in the inner-shareable domain in hardware, and every
//! rewrite of the shared tables already ends with them (`mmu::switch_full`).
//!
//! A fault on a secondary halts that core alone: the vector table is the
//! same, `rust_exception_handler` names the core (`core=N`, the MADT's
//! index, which the boot core takes as its own once the table is read) and
//! checks that core's own stack canary, and its halt loop spins the
//! faulting core, so the boot core's shell goes on. The `\SMPFAULT` boot
//! flag makes the first core started take an undefined instruction right
//! after its up line, which is how `make test-smp` sees that.

use crate::madt::{self, CoreList, MAX_CORES};
use crate::synccell::SyncCell;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};

/// What [`smp_secondary_entry`] reads with the MMU off: the EL1 regime and
/// the register values the boot core wants every core to share. Field
/// order is the stub's `ldp` order; the offsets are asserted below.
#[repr(C)]
pub struct SecondaryParams {
    mair: u64,
    tcr: u64,
    ttbr0: u64,
    sctlr: u64,
    stack_base: u64,
    stack_size: u64,
    vbar: u64,
    cptr: u64,
    gic_sysregs: u64,
    hcr: u64,
    cnthctl: u64,
    cpacr: u64,
    spsr: u64,
    /// `\SMPFAULT`: the core index that takes a deliberate fault after its
    /// up line, `NO_FAULT_CORE` for none. Read by Rust only.
    fault_core: u64,
}
const _: () = assert!(core::mem::offset_of!(SecondaryParams, mair) == 0);
const _: () = assert!(core::mem::offset_of!(SecondaryParams, ttbr0) == 16);
const _: () = assert!(core::mem::offset_of!(SecondaryParams, stack_base) == 32);
const _: () = assert!(core::mem::offset_of!(SecondaryParams, vbar) == 48);
const _: () = assert!(core::mem::offset_of!(SecondaryParams, gic_sysregs) == 64);
const _: () = assert!(core::mem::offset_of!(SecondaryParams, cnthctl) == 80);
const _: () = assert!(core::mem::offset_of!(SecondaryParams, spsr) == 96);

/// Written once by [`start`] on the boot core before any `CPU_ON`, then
/// read by every secondary (class A of the plan's inventory).
static PARAMS: SyncCell<SecondaryParams> = SyncCell::new(SecondaryParams {
    mair: 0,
    tcr: 0,
    ttbr0: 0,
    sctlr: 0,
    stack_base: 0,
    stack_size: 0,
    vbar: 0,
    cptr: 0,
    gic_sysregs: 0,
    hcr: 0,
    cnthctl: 0,
    cpacr: 0,
    spsr: 0,
    fault_core: NO_FAULT_CORE,
});

/// `fault_core` when no core is to fault: not 0, which is a valid index.
const NO_FAULT_CORE: u64 = u64::MAX;

/// The boot core's own MADT index, written by [`start`] once the MADT is
/// read, which is when its `TPIDR_EL1` becomes that index too (class A).
/// Until then the boot core is core 0 by `set_boot_core_index`.
static BOOT_INDEX: AtomicU64 = AtomicU64::new(0);

/// The word at the base of each secondary's stack, as `main.rs`'s
/// `STACK_CANARY` at the boot core's: an overflow overwrites it first.
const STACK_CANARY: u64 = 0x5141_5141_5141_5141;

/// Ticks taken on each core (class B): a secondary's own count, which the
/// boot core's `TICKS` does not include (step 3, decision 2).
static CORE_TICKS: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(0) }; MAX_CORES];

/// Whether this core is the boot core: the one whose tick does the
/// scheduler's jobs and whose current task is the scheduler's.
pub fn is_boot_core() -> bool {
    core_index() == BOOT_INDEX.load(Ordering::Relaxed)
}

/// A secondary core's tick (step 4(b)): counted, and said once, the first
/// time, so a rig can see that the core's timer fires and its IRQ path
/// round-trips through EL0. The frame goes back unchanged: the idle loop.
pub fn secondary_tick() {
    let core = core_index() as usize;
    if CORE_TICKS[core].fetch_add(1, Ordering::Relaxed) == 0 {
        crate::console::println!("Ouroboros kernel: smp: core {core} ticking (its first tick, from its idle loop at EL0)");
    }
}

/// The SGI that kicks a core into the kernel (step 4(c)): enabled on every
/// core, taken by `exceptions.rs`'s IRQ path under the kernel lock like any
/// other entry, and acted on by [`on_kick`].
pub const KICK_SGI: u32 = 0;

/// Why each core was kicked, a bit per reason (class B: one word per core,
/// written by a sender and taken by the core itself, both under the kernel
/// lock). Kicks merge: two senders before the core takes the SGI are one
/// interrupt with two bits, and every bit is acted on.
static KICKS: [AtomicU32; MAX_CORES] = [const { AtomicU32::new(0) }; MAX_CORES];

/// A kick that asks only to be answered: [`start`]'s check that every core
/// takes an SGI, and takes it under the kernel lock. Step 4(d) adds the
/// kick that ends a task running on the kicked core.
const KICK_PING: u32 = 1 << 0;

/// Pings each core has answered (class B, the core's own count).
static PINGS_ANSWERED: [AtomicU64; MAX_CORES] = [const { AtomicU64::new(0) }; MAX_CORES];

/// Kicks the core whose MADT index is `core`, for `reason`: the reason is
/// recorded for it, then the SGI sent, and nothing waits for it to be
/// taken. The caller holds the kernel lock, which the kicked core's handler
/// takes, so the handler runs once the caller has released it and not
/// before: a sender that waited for the answer while holding the lock
/// would wait forever (the plan's one deadlock, `roadmap-smp.md`'s risks).
/// Called without the lock, this halts the kernel with a line, the same
/// broken rule as `lock.rs`'s. `Err` says why the core cannot be reached,
/// with the reason taken back.
fn kick(core: usize, reason: u32) -> Result<(), &'static str> {
    if !crate::lock::held_by_me() {
        crate::console::println_force!(
            "Ouroboros kernel: smp: a kick sent by core {} without the kernel lock",
            core_index()
        );
        crate::lock::halt_kernel();
        crate::power::halt();
    }
    if core >= MAX_CORES {
        return Err("no such core");
    }
    KICKS[core].fetch_or(reason, Ordering::Relaxed);
    // SAFETY: the GIC is up (a core is started only with one), KICK_SGI is
    // below 16, and `core` is in range; send_sgi orders the store above
    // before the SGI.
    let sent = unsafe { crate::gic::send_sgi(KICK_SGI, core) };
    if sent.is_err() {
        // Taken back under the same lock that every sender holds, so no
        // other sender's bit is lost with it.
        KICKS[core].fetch_and(!reason, Ordering::Relaxed);
    }
    sent
}

/// This core's [`KICK_SGI`], under the kernel lock (`exceptions.rs`'s IRQ
/// path): every reason recorded for it since its last kick, acted on.
pub fn on_kick() {
    let core = core_index() as usize;
    let reasons = KICKS[core].swap(0, Ordering::Relaxed);
    if reasons & KICK_PING != 0 {
        PINGS_ANSWERED[core].fetch_add(1, Ordering::Release);
    }
}

/// Which interrupt each core has acknowledged and is waiting for the
/// kernel lock with, [`NOT_WAITING`] when none (class B: the core's own
/// mark, read by [`ping`]). Set by `exceptions.rs`'s IRQ entry between the
/// acknowledge and the lock, cleared once the lock is taken.
static WAITING: [AtomicU32; MAX_CORES] = [const { AtomicU32::new(NOT_WAITING) }; MAX_CORES];
const NOT_WAITING: u32 = u32::MAX;

/// This core has acknowledged `intid` and is about to wait for the kernel
/// lock with it.
pub fn note_waiting(intid: u32) {
    WAITING[core_index() as usize].store(intid, Ordering::Release);
}

/// This core holds the kernel lock: it waits with nothing.
pub fn note_in_kernel() {
    WAITING[core_index() as usize].store(NOT_WAITING, Ordering::Release);
}

/// How many kicks [`ping`] sends before it gives up on seeing one arrive
/// while the lock is held. An attempt is indecisive when the core takes
/// its tick first and waits for the lock with that instead; the kick is
/// then taken after the release, and the next attempt tries again.
const PING_ATTEMPTS: usize = 5;

/// [`start`]'s check of the kick, for one started core. Each attempt sends
/// the kick with the kernel lock held and keeps holding it until the core
/// is seen waiting for the lock with an interrupt it has acknowledged
/// ([`WAITING`]). When that interrupt is the kick, the attempt is
/// decisive: the lock is held a millisecond more, so a handler that acted
/// before taking the lock would have answered, and the core must not have
/// answered; then, the lock released, it must answer within a second.
/// Decisive by observation, not by a timing assumption: a delivery slower
/// than any fixed hold would otherwise pass a handler that skipped the
/// lock (the review of #265). One line, which `make test-smp` reads.
fn ping(core: usize) {
    let me = core_index();
    for _ in 0..PING_ATTEMPTS {
        let before = PINGS_ANSWERED[core].load(Ordering::Acquire);
        crate::lock::acquire();
        if let Err(why) = kick(core, KICK_PING) {
            crate::lock::release();
            crate::console::println!("Ouroboros kernel: smp: core {core} cannot be kicked ({why})");
            return;
        }
        let deadline = crate::timer::now_ticks() + wait_ticks();
        let mut seen = NOT_WAITING;
        while seen == NOT_WAITING && crate::timer::now_ticks() < deadline {
            seen = WAITING[core].load(Ordering::Acquire);
            core::hint::spin_loop();
        }
        let decisive = seen == KICK_SGI;
        if decisive {
            let until = crate::timer::now_ticks() + crate::timer::frequency_hz() / 1000;
            while crate::timer::now_ticks() < until {
                core::hint::spin_loop();
            }
        }
        let answered_under_lock = PINGS_ANSWERED[core].load(Ordering::Acquire) != before;
        crate::lock::release();
        if answered_under_lock {
            crate::console::println!(
                "Ouroboros kernel: smp: core {core} answered a kick while core {me} held the kernel lock: its handler ran outside the lock"
            );
            return;
        }
        let deadline = crate::timer::now_ticks() + wait_ticks();
        while PINGS_ANSWERED[core].load(Ordering::Acquire) == before && crate::timer::now_ticks() < deadline {
            core::hint::spin_loop();
        }
        if PINGS_ANSWERED[core].load(Ordering::Acquire) == before {
            crate::console::println!("Ouroboros kernel: smp: core {core} did not answer a kick within a second");
            return;
        }
        if decisive {
            crate::console::println!(
                "Ouroboros kernel: smp: core {core} answered a kick from core {me}, once the kernel lock was free"
            );
            return;
        }
    }
    crate::console::println!(
        "Ouroboros kernel: smp: core {core} answered every kick but was never seen waiting with one while the lock was held ({PING_ATTEMPTS} attempts)"
    );
}

/// A secondary core's stack. Smaller than the boot core's 256 KB: a parked
/// core runs `secondary_main` and a fault report, and step 4 sizes it for
/// tasks when it comes.
pub const SECONDARY_STACK_SIZE: usize = 64 * 1024;
#[repr(C, align(16))]
struct SecondaryStacks([[u8; SECONDARY_STACK_SIZE]; MAX_CORES]);
/// One stack per MADT index (class B: per core by nature). Core `i` takes
/// entry `i`, whose top is `base + (i + 1) * SECONDARY_STACK_SIZE`; the
/// boot core's entry goes unused, since the MADT may list it at any index
/// (ACPI fixes no order on ARM), and indexing by the table's own index is
/// what keeps every core's index, stack and up mark in step.
static STACKS: SyncCell<SecondaryStacks> =
    SyncCell::new(SecondaryStacks([[0; SECONDARY_STACK_SIZE]; MAX_CORES]));

/// This core's stack canary: intact or not, and the stack's base, for the
/// fault line. The boot core's is `main.rs`'s 256 KB stack; a secondary's
/// is its `STACKS` entry.
pub(crate) fn this_core_stack_canary() -> (bool, u64) {
    let core = core_index();
    if is_boot_core() {
        return (crate::stack_canary_intact(), crate::stacks().0.0);
    }
    let base = STACKS.get() as u64 + core * SECONDARY_STACK_SIZE as u64;
    // SAFETY: the base word of this core's own stack, written by `start`
    // before the core ran; volatile since nothing the compiler sees writes it.
    (unsafe { core::ptr::read_volatile(base as *const u64) } == STACK_CANARY, base)
}

/// Per core: 0 not started, [`CORE_UP_IDLE`] up and into its idle loop,
/// [`CORE_FAILED`] parked without a GIC interface (class B). Written by the
/// core itself, read by the boot core's wait in [`start`].
static CORE_UP: [AtomicU8; MAX_CORES] = [const { AtomicU8::new(0) }; MAX_CORES];
const CORE_UP_IDLE: u8 = 1;
const CORE_FAILED: u8 = 2;

/// This core's index, from `TPIDR_EL1`: 0 on the boot core (`main.rs`
/// writes it first), the MADT's index on a secondary (the stub writes it
/// before anything else). Readable at EL1 and EL2.
pub fn core_index() -> u64 {
    let idx: u64;
    unsafe { asm!("mrs {0}, tpidr_el1", out(reg) idx, options(nomem, nostack, preserves_flags)) };
    idx
}

/// Marks this core as core 0 in `TPIDR_EL1`. The boot core, at its start,
/// before the MADT says which index it really has ([`start`] corrects it).
pub fn set_boot_core_index() {
    unsafe { asm!("msr tpidr_el1, xzr", options(nomem, nostack, preserves_flags)) };
}

fn set_core_index(idx: u64) {
    unsafe { asm!("msr tpidr_el1, {0}", in(reg) idx, options(nomem, nostack, preserves_flags)) };
}

global_asm!(
    r#"
    .text
    .global smp_secondary_entry
    .balign 64
smp_secondary_entry:
    msr  daifset, #0xf
    mov  x19, x0
    adrp x1, {params}
    add  x1, x1, :lo12:{params}
    ldp  x2, x3, [x1, #0]
    ldp  x4, x5, [x1, #16]
    ldp  x6, x7, [x1, #32]
    ldp  x11, x12, [x1, #48]
    ldp  x13, x14, [x1, #64]
    ldp  x15, x16, [x1, #80]
    ldr  x17, [x1, #96]
    add  x8, x19, #1
    mul  x8, x8, x7
    add  x8, x6, x8
    msr  tpidr_el1, x19
    mrs  x9, CurrentEL
    lsr  x9, x9, #2
    cmp  x9, #2
    b.ne 1f
    msr  hcr_el2, x14
    isb
    cbz  x13, 5f
    mov  x10, #0x9
    msr  icc_sre_el2, x10
    isb
    msr  ich_hcr_el2, xzr
5:
    msr  vttbr_el2, xzr
    isb
    dsb  ishst
    msr  mair_el1, x2
    msr  tcr_el1, x3
    msr  ttbr0_el1, x4
    msr  vbar_el1, x11
    tlbi vmalle1
    dsb  ish
    msr  sctlr_el1, x5
    mrs  x10, midr_el1
    msr  vpidr_el2, x10
    mrs  x10, mpidr_el1
    msr  vmpidr_el2, x10
    msr  cnthctl_el2, x15
    msr  cntvoff_el2, xzr
    msr  cnthp_ctl_el2, xzr
    msr  cptr_el2, x12
    msr  hstr_el2, xzr
    mrs  x10, mdcr_el2
    and  x10, x10, #0x1f
    msr  mdcr_el2, x10
    msr  cpacr_el1, x16
    msr  sp_el1, x8
    adr  x10, 2f
    msr  elr_el2, x10
    msr  spsr_el2, x17
    ic   iallu
    dsb  ish
    isb
    eret
1:
    dsb  ishst
    msr  mair_el1, x2
    msr  tcr_el1, x3
    msr  ttbr0_el1, x4
    msr  vbar_el1, x11
    tlbi vmalle1
    dsb  ish
    isb
    msr  sctlr_el1, x5
    ic   iallu
    dsb  ish
    isb
    msr  cpacr_el1, x16
    isb
    mov  sp, x8
2:
    mov  x0, x19
    bl   {secondary_main}
3:
    wfe
    b    3b
    .global smp_secondary_entry_end
smp_secondary_entry_end:
"#,
    params = sym PARAMS,
    secondary_main = sym secondary_main,
);

unsafe extern "C" {
    fn smp_secondary_entry();
    fn smp_secondary_entry_end();
}

/// A secondary core's Rust half, on its own stack, at EL1 with the MMU on:
/// this core's GIC interface, what another core needs to kick it and the
/// kick enabled (step 4(c)), its timer, the up line, then (step 4(b)) its
/// own idle loop at EL0, from which it takes its ticks and its kicks.
extern "C" fn secondary_main(core: u64) -> ! {
    let mpidr = crate::gicv3::read_mpidr();
    // A GIC that will not come up on this core is a line and a parked core
    // without one, not a panic: a panic on a secondary after the exit has
    // no path but the firmware's, which is gone.
    let gic = match unsafe { crate::gic::init_this_core() } {
        Ok(this) => this,
        Err(why) => {
            crate::console::println!(
                "Ouroboros kernel: smp: core {core} (mpidr {mpidr:#x}) has no GIC interface ({why}), parked without one"
            );
            CORE_UP[core as usize].store(CORE_FAILED, Ordering::Release);
            loop {
                unsafe { asm!("wfe", options(nomem, nostack, preserves_flags)) };
            }
        }
    };
    // Everything the idle loop needs, before the line that claims it
    // (step 4(b)): the idle view's tables (slot 1's, which map the loop's
    // page for EL0; TTBR0 is per core), EL0's wfe allowed, the timer PPI
    // enabled at this core's own interface and armed (interrupts stay
    // masked at EL1, so it fires only once the loop runs), this core's
    // current slot the idle slot.
    crate::mmu::activate_task(crate::tasks::TaskIndex::IDLE);
    crate::tasks::allow_el0_wfe();
    unsafe { crate::gic::enable_ppi_this_core(crate::timer::INTID, gic) };
    // The kick (step 4(c)): what another core needs to send this one an
    // SGI, and the SGI enabled here. A core that cannot be sent one still
    // idles; the boot core's ping says it cannot be kicked.
    if let Err(why) = unsafe { crate::gic::note_this_core(core as usize) } {
        crate::console::println!("Ouroboros kernel: smp: core {core} cannot be sent an SGI ({why})");
    }
    unsafe { crate::gic::enable_ppi_this_core(KICK_SGI, gic) };
    crate::tasks::set_current_idle_here();
    crate::timer::arm(crate::timer::TICK_INTERVAL_MS);
    crate::console::println!(
        "Ouroboros kernel: smp: core {core} up at EL{} (mpidr {mpidr:#x}, affinity {:#x}), idling",
        crate::el2::current_el(),
        madt::affinity(mpidr)
    );
    // The mark after the line and after the setup: a core counts as up
    // only once it has survived its first console write and has nothing
    // left to do but enter the loop, so the boot core's summary vouches
    // for a state the core has reached (`main.rs`'s set_quiet comes after
    // `start` returns and cannot drop a late up line; the `ticking` line,
    // later, is a println and may be quieted: a signal, not a report).
    CORE_UP[core as usize].store(CORE_UP_IDLE, Ordering::Release);
    // SAFETY: the one write to PARAMS happened before this core started.
    if unsafe { (*PARAMS.get()).fault_core } == core {
        crate::console::println!("Ouroboros kernel: smp: core {core} taking the \\SMPFAULT undefined instruction");
        unsafe { asm!("udf #0", options(nomem, nostack)) };
    }
    // The one entry into a task, tasks::enter_frame. From here the core
    // alternates EL0 and its tick, every interrupt masked at EL1; its
    // registers live in the frame on this stack, saved and restored by
    // the tick's trampoline; slot 1's Context is the boot core's idle and
    // is not touched.
    unsafe { crate::tasks::enter_frame(crate::tasks::idle_context()) }
}

/// How long [`start`] waits for each core's up mark, in timer ticks of
/// the generic counter: one second.
fn wait_ticks() -> u64 {
    crate::timer::frequency_hz()
}

/// Starts every core the MADT lists but this one, and waits for each to
/// report up. Logs one line per core and a summary. On the boot core,
/// after the identity map, the vectors, `gic::init` and `tasks::init`
/// (the idle page every core runs), before the console goes quiet; each
/// secondary arms its own timer.
///
/// # Safety
/// Once, on the boot core, at EL1, with the console installed.
pub unsafe fn start(cores: &CoreList, fault_first: bool) {
    let me = madt::affinity(crate::gicv3::read_mpidr());
    let n = cores.count.min(MAX_CORES);
    // The boot core's own index: the MADT fixes no order on ARM, so it may
    // be any entry. From here its TPIDR_EL1 is that index, its stack still
    // main.rs's. A table that does not list this core at all (or lists it
    // under an affinity its MPIDR does not carry) starts nothing: an entry
    // started with index 0 would share the boot core's index, its fault
    // lines and its canary.
    match (0..n).find(|&i| cores.mpidr[i] == me) {
        Some(mine) => {
            BOOT_INDEX.store(mine as u64, Ordering::Relaxed);
            crate::tasks::relocate_current(0, mine as u64);
            set_core_index(mine as u64);
            // The boot core can be kicked too, once its index is the MADT's,
            // when there is another core to kick it: a uniprocessor GICv2
            // reads ITARGETSR as zero (RAZ by the architecture), and a
            // line saying so would be about a sender that does not exist.
            if n > 1 {
                if let Err(why) = unsafe { crate::gic::note_this_core(mine) } {
                    crate::console::println!("Ouroboros kernel: smp: core {mine} (the boot core) cannot be sent an SGI ({why})");
                }
            }
        }
        None => {
            crate::console::println!(
                "Ouroboros kernel: smp: this core (affinity {me:#x}) is not among the MADT's {n} cores; no core started"
            );
            return;
        }
    }
    // `\SMPFAULT`: the first core that will be started takes the fault.
    let fault_core = if fault_first {
        (0..n).find(|&i| cores.mpidr[i] != me && cores.enabled[i]).map_or(NO_FAULT_CORE, |i| i as u64)
    } else {
        NO_FAULT_CORE
    };
    let regime = crate::mmu::boot_regime();
    let params = SecondaryParams {
        mair: regime.mair,
        tcr: regime.tcr,
        ttbr0: regime.ttbr0,
        sctlr: crate::el2::SCTLR_EL1_MMU_ON,
        stack_base: STACKS.get() as u64,
        stack_size: SECONDARY_STACK_SIZE as u64,
        vbar: crate::exceptions::vector_table_addr(),
        cptr: crate::el2::cptr_el2_value(),
        gic_sysregs: u64::from(crate::el2::gic_sysregs_implemented()),
        hcr: crate::el2::HCR_EL2_VALUE,
        cnthctl: crate::el2::CNTHCTL_EL2_EL1_PHYS,
        cpacr: crate::el2::CPACR_EL1_FPEN,
        spsr: crate::el2::SPSR_EL1H_MASKED,
        fault_core,
    };
    // SAFETY: the one write, before any CPU_ON; then cleaned to the point
    // of coherency, since the stub reads it with the MMU off. The stub's
    // own code too: the firmware's loader cleaned the image to the point
    // of unification, which on an A72 is the shared L2, and a fresh core
    // with its MMU off fetches from memory (Linux's EFI stub cleans the
    // image to PoC for this reason).
    unsafe { *PARAMS.get() = params };
    crate::mmu::clean_to_poc(PARAMS.get() as u64, core::mem::size_of::<SecondaryParams>() as u64, 1, 0);
    let entry = smp_secondary_entry as *const () as u64;
    let entry_end = smp_secondary_entry_end as *const () as u64;
    crate::mmu::clean_to_poc(entry, entry_end - entry, 1, 0);
    let mut started = 0usize;
    let mut up = 0usize;
    for (i, up_mark) in CORE_UP.iter().enumerate().take(n) {
        let aff = cores.mpidr[i];
        if aff == me {
            continue;
        }
        // This core's stack canary, under the stack the stub will take.
        let base = STACKS.get() as u64 + i as u64 * SECONDARY_STACK_SIZE as u64;
        // SAFETY: the base word of a stack nothing has used.
        unsafe { core::ptr::write_volatile(base as *mut u64, STACK_CANARY) };
        if !cores.enabled[i] {
            crate::console::println!("Ouroboros kernel: smp: core {i} (affinity {aff:#x}) is not enabled in the MADT, left off");
            continue;
        }
        match crate::power::cpu_on(aff, entry, i as u64) {
            Ok(()) => started += 1,
            Err(code) => {
                crate::console::println!("Ouroboros kernel: smp: CPU_ON for core {i} (affinity {aff:#x}) refused, PSCI error {code}");
                continue;
            }
        }
        let deadline = crate::timer::now_ticks() + wait_ticks();
        while up_mark.load(Ordering::Acquire) == 0 && crate::timer::now_ticks() < deadline {
            core::hint::spin_loop();
        }
        match up_mark.load(Ordering::Acquire) {
            CORE_UP_IDLE => up += 1,
            CORE_FAILED => {} // its own line said why
            _ => crate::console::println!("Ouroboros kernel: smp: core {i} (affinity {aff:#x}) did not report up within a second"),
        }
    }
    crate::console::println!(
        "Ouroboros kernel: smp: {up} of {started} started cores up, {} cores in all",
        cores.count
    );
    // Every core that came up must take a kick, and only once the kernel
    // lock is free (step 4(c)). On every boot, not behind a flag: the line
    // is the one check of SGI delivery a hardware boot log has (the Pi's
    // and Parallels' are read by eye), and it costs the time a kick takes
    // to arrive, a few milliseconds per core, plus a second for a core
    // that is up but cannot take one, which is the case it reports.
    for (i, up_mark) in CORE_UP.iter().enumerate().take(n) {
        if up_mark.load(Ordering::Acquire) == CORE_UP_IDLE {
            ping(i);
        }
    }
}

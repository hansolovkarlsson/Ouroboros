//! The kernel lock: one core in the kernel at a time (multi-core step
//! 4(a), `docs/roadmap/roadmap-smp.md`).
//!
//! Every resumable entry into the kernel, a syscall, the tick, an EL0
//! fault, takes [`KERNEL_LOCK`] at the start of its Rust half and releases
//! it at the end, before the trampoline's `eret`. Held only with IRQs
//! masked, which at EL1 they always are (`synccell.rs`), and never across
//! an `eret`, so a core that holds it is running kernel code and nothing
//! else, and the single-core argument that makes every class C and D
//! static sound (`roadmap-smp.md`'s inventory) becomes "the holder of the
//! kernel lock": the same argument, with the lock as its witness. That is
//! what lets step 4(b) run tasks on a second core without touching any of
//! those statics' users; per-device locks and per-slot locks are the
//! splitting of this one, measured first (4(a)'s own measurement is `make
//! measure-syscost`, an uncontended acquire and release per syscall).
//!
//! Not reentrant, by design: the Rust half of an entry is where the lock is
//! taken, and nothing inside it enters the kernel again (an exception at
//! EL1 is a diverging fault, which takes no lock and halts). The one other
//! taker is `smp::start`'s check of the kick (step 4(c)), on the boot core
//! before any task runs, which holds it around an SGI to show the kicked
//! core waits for it.
//!
//! A kick's SGI is taken like every IRQ, under this lock, and its sender
//! holds this lock (`smp::kick` refuses otherwise) and never waits for the
//! answer: a sender that waited holding the lock for a handler that needs
//! it would deadlock. The plan first ruled the other way, that an SGI
//! handler takes no lock and leaves a flag for the next tick; that would
//! make the SGI no faster than the tick it is meant to beat.
//! A core that finds the lock its own has broken that rule, and the kernel
//! says so and halts rather than deadlock in silence. The diverging fault
//! reporter does not take it either: it halts, and may be reporting from
//! inside the holder, in which case the state behind the lock is suspect,
//! so `power::halt` on a core that holds the lock (or on the boot core,
//! whose death is the system's) halts the KERNEL: `HALTED` is set, the
//! lock stays with the dead core, and every other core parks at its next
//! `acquire` with a line. A secondary that halts holding nothing (a fault
//! in its own setup, `\SMPFAULT`) halts alone, and the rest go on.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The core index plus one of the holder, 0 when free (class C of the
/// plan's inventory: the lock itself).
static KERNEL_LOCK: AtomicU64 = AtomicU64::new(0);

/// Takes the lock for this core. Spins while another core holds it.
pub fn acquire() {
    let me = crate::smp::core_index() + 1;
    loop {
        if HALTED.load(Ordering::Acquire) {
            crate::console::println_force!("Ouroboros kernel: core {} stopped: the kernel has halted", me - 1);
            loop {
                unsafe { core::arch::asm!("wfe", options(nomem, nostack, preserves_flags)) };
            }
        }
        match KERNEL_LOCK.compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return,
            Err(owner) if owner == me => {
                crate::console::println_force!(
                    "Ouroboros kernel: the kernel lock re-entered on core {}: an entry inside an entry",
                    me - 1
                );
                halt_kernel(); // a broken invariant inside an entry: the kernel's halt, whoever found it
                crate::power::halt();
            }
            Err(_) => core::hint::spin_loop(),
        }
    }
}

/// Set once the kernel has halted (`power::halt` on a core that holds the
/// lock, or on the boot core): every other core stops at its next
/// `acquire`, since the state behind the lock may be half-written and
/// nothing may run over it. Never cleared.
static HALTED: AtomicBool = AtomicBool::new(false);

/// Whether the kernel has halted (`halt_kernel`).
pub fn is_halted() -> bool {
    HALTED.load(Ordering::Acquire)
}

/// Whether this core holds the lock.
pub fn held_by_me() -> bool {
    KERNEL_LOCK.load(Ordering::Relaxed) == crate::smp::core_index() + 1
}

/// Halts the kernel: the lock stays with its holder, and every core that
/// comes to `acquire` after this parks for good with a line.
pub fn halt_kernel() {
    HALTED.store(true, Ordering::Release);
}

/// Releases the lock this core holds. Releasing a lock another core
/// holds, or a free one, is the same broken rule as re-entry, and halts.
pub fn release() {
    let me = crate::smp::core_index() + 1;
    if KERNEL_LOCK.compare_exchange(me, 0, Ordering::Release, Ordering::Relaxed).is_err() {
        crate::console::println_force!(
            "Ouroboros kernel: the kernel lock released on core {} without being held by it",
            me - 1
        );
        halt_kernel();
        crate::power::halt();
    }
}

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
//! Not reentrant, by design: the Rust half of an entry is the one place
//! the lock is taken, and nothing inside it enters the kernel again (an
//! exception at EL1 is a diverging fault, which takes no lock and halts).
//! A core that finds the lock its own has broken that rule, and the kernel
//! says so and halts rather than deadlock in silence. The diverging fault
//! reporter does not take it either: it halts this core, and may be
//! reporting from inside the holder.

use core::sync::atomic::{AtomicU64, Ordering};

/// The core index plus one of the holder, 0 when free (class C of the
/// plan's inventory: the lock itself).
static KERNEL_LOCK: AtomicU64 = AtomicU64::new(0);

/// Takes the lock for this core. Spins while another core holds it.
pub fn acquire() {
    let me = crate::smp::core_index() + 1;
    loop {
        match KERNEL_LOCK.compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return,
            Err(owner) if owner == me => {
                crate::console::println_force!(
                    "Ouroboros kernel: the kernel lock re-entered on core {}: an entry inside an entry",
                    me - 1
                );
                crate::power::halt();
            }
            Err(_) => core::hint::spin_loop(),
        }
    }
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
        crate::power::halt();
    }
}

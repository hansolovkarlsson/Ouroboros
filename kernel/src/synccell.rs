//! `SyncCell<T>`: a `static` this kernel mutates in place, and the one
//! statement of why that is sound.
//!
//! Every mutable global in the kernel (a task's saved context, a page
//! table, the staging buffer a `SPAWN` reads, the console handle the fault
//! handler reports through) used to be its own `struct X(UnsafeCell<T>)`
//! with its own `unsafe impl Sync for X {}` and its own SAFETY comment,
//! and every one of those comments said the same thing in different
//! words. Fourteen of them in `tasks.rs` and `mmu.rs` alone by the count
//! that put this on the ledger. The argument is made once here instead,
//! and a wrapper that needs nothing but that argument is a `SyncCell<T>`,
//! not a new type. **The measured count, stated here and nowhere else:**
//! `grep -c '^unsafe impl.*Sync for' kernel/src/*.rs` (anchored, so a doc
//! comment quoting the phrase does not count) summed to 46 before this
//! module and 24 after it: this module's own impl and the 23 hand-rolled
//! wrappers that remain, named below, so 23 became a `SyncCell`.
//!
//! **The argument.** This kernel runs on one core and never unmasks
//! interrupts while it is itself running: EL1 code executes either at boot
//! (before any task exists, IRQs masked until the first `eret` into task 0
//! restores that task's SPSR; `main.rs` once unmasked a few instructions
//! early, the one exception, removed when this argument was written down),
//! inside SVC dispatch, inside the tick's IRQ trampoline, or inside the EL0
//! fault handler, and taking an exception masks further IRQs until the
//! next `eret`, so none of those contexts can be entered while another is
//! in progress. There is therefore never a
//! second thread of execution that could observe a cell mid-write, which
//! is exactly the guarantee `Sync` asks for. `T: Send` is the bound a
//! shared cell of `T` needs in general (a `Mutex<T>` is `Sync` on the same
//! condition); every `T` here satisfies it, and the bound stays so that a
//! future `T` holding a raw pointer has to argue its own case rather than
//! inherit this one.
//!
//! **What this does not claim.** It is not a lock and gives no exclusion
//! between two references obtained from `get`; the discipline that no code
//! holds a `&mut` across a call that could re-enter the same cell is the
//! caller's, as it was with the hand-rolled wrappers. A cell that also
//! needs an alignment (`mmu::Table`, `tasks::IdleRegion`) keeps its own
//! `#[repr(align)]` newtype around a `SyncCell` and derives `Sync` from it
//! instead of asserting it. The DMA rings and pages in the virtio, xHCI and
//! USB storage drivers keep their own wrappers: their SAFETY arguments are
//! about the device writing memory, a different claim from this one. The
//! xHCI controller handle itself (`xhci::XHCI`) holds no pointer and is a
//! `SyncCell`. `gic.rs`'s `GicCell` stays a `Cell` for its by-value
//! `get`/`set`, and is the last of the 23.

use core::cell::UnsafeCell;

/// A mutable `static` for single-core kernel code; see the module doc for
/// the whole argument. `get` hands out the raw pointer, as `UnsafeCell::get`
/// does, and every dereference is the caller's `unsafe`.
#[repr(transparent)]
pub(crate) struct SyncCell<T>(UnsafeCell<T>);

// SAFETY: the module doc's argument - one core, interrupts masked for the
// whole of every EL1 context, so nothing can observe a cell mid-write.
unsafe impl<T: Send> Sync for SyncCell<T> {}

impl<T> SyncCell<T> {
    pub(crate) const fn new(value: T) -> Self {
        SyncCell(UnsafeCell::new(value))
    }

    /// The raw pointer to the value, the same contract as
    /// [`UnsafeCell::get`].
    pub(crate) const fn get(&self) -> *mut T {
        self.0.get()
    }
}

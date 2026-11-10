//! **Held user keys**, `netd`'s end: the table and its rules are the
//! `heldkeys` crate (step 4 of `docs/roadmap/roadmap-user-keys.md`, Decision
//! 4; a crate since 2026-11-10 so its rules run on the host). What is left
//! here is the one thing that crate cannot know: whether a task is alive,
//! which only the kernel says.

use crate::syscall;

/// The kernel's word on who is alive.
pub struct KernelTasks;

/// The table `serve` keeps, asking the kernel.
pub type Held = heldkeys::Held<KernelTasks>;

/// An empty table.
pub fn new() -> Held {
    heldkeys::Held::new(KernelTasks)
}

impl heldkeys::Tasks for KernelTasks {
    /// Whether the task `owner` names is still running: its slot holds the same
    /// packed identity and is runnable or blocked, not a zombie.
    ///
    /// State FIRST, identity second, so a slot recycled at any point between the
    /// two reads fails the identity comparison: a state read from the new occupant
    /// is never paired with the old occupant's identity.
    fn alive(&self, owner: u64) -> bool {
        let slot = syscall_abi::task_id_slot(owner);
        let state = syscall(syscall_abi::TASK_STATE, slot);
        let now = syscall(syscall_abi::TASK_IDENTITY, slot);
        now == owner && (state == syscall_abi::TASK_STATE_RUNNABLE || state == syscall_abi::TASK_STATE_BLOCKED)
    }
}

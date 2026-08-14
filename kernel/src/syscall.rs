//! The `svc` trap path from EL0 back to EL1 — the dispatch table itself.
//! Dropping to EL0 and the tasks that run there now live in `tasks.rs`;
//! this module is just what they call into.
//!
//! Calling convention (chosen to match Linux's, a reasonable default for a
//! "POSIX-ish" OS per this project's stated goals, not because anything
//! here is Linux-ABI-compatible): syscall number in x8, first argument in
//! x0, return value in x0. `exceptions.rs`'s slot-8 trampoline is what
//! actually marshals registers into and out of [`dispatch`]'s call
//! signature — see its module doc comment for why that trampoline exists
//! and how it differs from every other vector.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::console;

/// Two independent counters (one per `tasks.rs` task), proof — alongside
/// `double`/`print` below — that syscalls arriving from *different*,
/// actually-switching contexts still land in the right dispatch arm with
/// the right per-caller state, not just that the dispatch table has more
/// than one entry.
static TASK_REPORTS: [AtomicU64; crate::tasks::NUM_TASKS] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Called from the exception vector's SVC trampoline (`exceptions.rs`) with
/// the syscall number (from x8) and first argument (from x0), running at
/// EL1 with the kernel's own stack and every privilege EL0 lacks - the
/// entire reason this indirection exists. Its return value becomes EL0's
/// new x0 after `eret`.
///
/// Three syscalls, not one - a real dispatch table. `double`/`print` were
/// deliberately chained by the original single-task demo (double's return
/// value fed straight into print's argument) to prove a return value
/// survives the trampoline intact; `report` is what `tasks.rs`'s two tasks
/// actually call, each with its own task ID as `arg0`.
pub extern "C" fn dispatch(number: u64, arg0: u64) -> u64 {
    match number {
        0 => {
            console::println!("Ouroboros kernel: syscall from EL0: print(arg0={arg0:#x})");
            0
        }
        1 => {
            let result = arg0.wrapping_mul(2);
            console::println!("Ouroboros kernel: syscall from EL0: double(arg0={arg0:#x}) = {result:#x}");
            result
        }
        2 => {
            let task_id = arg0 as usize;
            match TASK_REPORTS.get(task_id) {
                Some(counter) => {
                    let count = counter.fetch_add(1, Ordering::Relaxed) + 1;
                    console::println!("Ouroboros kernel: task {task_id} report #{count}");
                    0
                }
                None => u64::MAX,
            }
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

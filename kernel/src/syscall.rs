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
use crate::shell;

/// Two independent counters (one per `tasks.rs` task), proof — alongside
/// `double`/`print` below — that syscalls arriving from *different*,
/// actually-switching contexts still land in the right dispatch arm with
/// the right per-caller state, not just that the dispatch table has more
/// than one entry.
static TASK_REPORTS: [AtomicU64; crate::tasks::NUM_TASKS] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Sentinel `try_read_char` returns in x0 when no byte is waiting -
/// out of range for any real byte (0-255), so callers can tell the two
/// apart with a single comparison.
pub const NO_CHAR: u64 = u64::MAX;

/// Called from the exception vector's SVC trampoline (`exceptions.rs`) with
/// the syscall number (from x8) and first argument (from x0), running at
/// EL1 with the kernel's own stack and every privilege EL0 lacks - the
/// entire reason this indirection exists. Its return value becomes EL0's
/// new x0 after `eret`.
///
/// Six syscalls now. `double`/`print` were deliberately chained by the
/// original single-task demo (double's return value fed straight into
/// print's argument) to prove a return value survives the trampoline
/// intact; `report` is what `tasks.rs`'s original two demo tasks called,
/// each with its own task ID as `arg0` (task 1 has since become a plain
/// idle loop that calls nothing - see `tasks.rs`). `try_read_char`/`putc`/
/// `shell_input` are the real input/output primitives the interactive
/// shell (`shell.rs`) is built on: task 0's poll loop chains the first two
/// directly (a byte `try_read_char` returns becomes `shell_input`'s `arg0`
/// with no extra register shuffling), and all the actual line-editing
/// logic lives in `shell.rs`, not here or in EL0 code.
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
        3 => match console::read_byte() {
            Some(byte) => byte as u64,
            None => NO_CHAR,
        },
        4 => {
            console::putc(arg0 as u8);
            0
        }
        5 => {
            shell::on_byte(arg0 as u8);
            0
        }
        _ => {
            console::println!("Ouroboros kernel: syscall from EL0: unknown number={number}");
            u64::MAX
        }
    }
}

//! Global console handle, shared between `main`'s boot sequence and the
//! exception handler — a fault needs somewhere to report through too, if a
//! console happens to have been installed before it fired.

use core::cell::UnsafeCell;
use core::fmt;

use crate::uart::Uart;

struct ConsoleCell(UnsafeCell<Option<Uart>>);

// SAFETY: single-core, no preemption, no interrupts unmasked yet - nothing
// can run concurrently with whatever's touching this.
unsafe impl Sync for ConsoleCell {}

static CONSOLE: ConsoleCell = ConsoleCell(UnsafeCell::new(None));

/// Installs `uart` as the global console. Must only be called after
/// `exit_boot_services`, and only once.
pub fn install(uart: Uart) {
    unsafe {
        *CONSOLE.0.get() = Some(uart);
    }
}

/// Writes to the global console if one has been installed; silently does
/// nothing otherwise — there may genuinely be no console yet (e.g. a fault
/// before discovery/`install` has run).
pub fn print(args: fmt::Arguments) {
    if let Some(uart) = unsafe { (*CONSOLE.0.get()).as_mut() } {
        let _ = fmt::Write::write_fmt(uart, args);
    }
}

macro_rules! println {
    ($($arg:tt)*) => {
        $crate::console::print(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use println;

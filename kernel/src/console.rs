//! Global console handle, shared between `main`'s boot sequence and the
//! exception handler — a fault needs somewhere to report through too, if a
//! console happens to have been installed before it fired.

use core::cell::UnsafeCell;
use core::fmt;

use crate::uart::Uart;
use crate::uart16550::Uart16550;

/// Either driver a discovered console might turn out to need. A plain enum,
/// not `Box<dyn fmt::Write>`: constructing a trait object would allocate,
/// and the console is only ever installed after `exit_boot_services`, where
/// the global allocator is boot-services-backed and no longer usable.
pub enum Console {
    Pl011(Uart),
    Uart16550(Uart16550),
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        match self {
            Console::Pl011(uart) => uart.write_str(s),
            Console::Uart16550(uart) => uart.write_str(s),
        }
    }
}

struct ConsoleCell(UnsafeCell<Option<Console>>);

// SAFETY: single-core, no preemption, no interrupts unmasked yet - nothing
// can run concurrently with whatever's touching this.
unsafe impl Sync for ConsoleCell {}

static CONSOLE: ConsoleCell = ConsoleCell(UnsafeCell::new(None));

/// Installs `console` as the global console. Must only be called after
/// `exit_boot_services`, and only once.
pub fn install(console: Console) {
    unsafe {
        *CONSOLE.0.get() = Some(console);
    }
}

/// Writes to the global console if one has been installed; silently does
/// nothing otherwise — there may genuinely be no console yet (e.g. a fault
/// before discovery/`install` has run).
pub fn print(args: fmt::Arguments) {
    if let Some(console) = unsafe { (*CONSOLE.0.get()).as_mut() } {
        let _ = fmt::Write::write_fmt(console, args);
    }
}

macro_rules! println {
    ($($arg:tt)*) => {
        $crate::console::print(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use println;

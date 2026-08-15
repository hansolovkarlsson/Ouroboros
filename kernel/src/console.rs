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

impl Console {
    /// Raw single-byte write, no `\n` -> `\r\n` translation (unlike
    /// `write_str`) - callers that need a newline send `\r\n` themselves.
    /// Used for echoing input verbatim, where a translated `\n` would be
    /// wrong for e.g. a literal byte the line editor is about to erase.
    fn write_byte(&mut self, byte: u8) {
        match self {
            Console::Pl011(uart) => uart.write_byte(byte),
            Console::Uart16550(uart) => uart.write_byte(byte),
        }
    }

    /// Non-blocking: `None` if no byte is waiting.
    fn read_byte(&mut self) -> Option<u8> {
        match self {
            Console::Pl011(uart) => uart.read_byte(),
            Console::Uart16550(uart) => uart.read_byte(),
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

/// Writes one raw byte to the global console if one has been installed;
/// silently does nothing otherwise, same as [`print`].
pub fn putc(byte: u8) {
    if let Some(console) = unsafe { (*CONSOLE.0.get()).as_mut() } {
        console.write_byte(byte);
    }
}

/// Non-blocking read of one byte from the global console. `None` if there
/// is no console installed yet, or there is one but nothing is waiting.
pub fn read_byte() -> Option<u8> {
    unsafe { (*CONSOLE.0.get()).as_mut() }.and_then(Console::read_byte)
}

//! Global console handle, shared between `main`'s boot sequence and the
//! exception handler — a fault needs somewhere to report through too, if a
//! console happens to have been installed before it fired.

use core::cell::UnsafeCell;
use core::fmt;

use crate::fbconsole::FbConsole;
use crate::uart::Uart;
use crate::uart16550::Uart16550;
use crate::virtio_console;

/// Either driver a discovered console might turn out to need. A plain enum,
/// not `Box<dyn fmt::Write>`: constructing a trait object would allocate,
/// and the console is only ever installed after `exit_boot_services`, where
/// the global allocator is boot-services-backed and no longer usable.
pub enum Console {
    Pl011(Uart),
    Uart16550(Uart16550),
    /// The real lead for Parallels console output - see
    /// `virtio_console.rs`'s module doc comment. Transmit-only: this
    /// variant's `read_byte` always returns `None`, a real, documented
    /// limitation, not a bug (no receive virtqueue exists yet).
    Virtio(virtio_console::Device),
    /// The GOP-framebuffer console - the real lead for Parallels, since
    /// `Virtio` above is a confirmed dead end there. See
    /// `framebuffer.rs`/`fbconsole.rs`'s module doc comments. Write-only,
    /// same limitation as `Virtio`, for a different reason (no keyboard
    /// driver exists at all, not just no receive virtqueue).
    Framebuffer(FbConsole),
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        match self {
            Console::Pl011(uart) => uart.write_str(s),
            Console::Uart16550(uart) => uart.write_str(s),
            Console::Virtio(dev) => write_str_virtio(dev, s),
            Console::Framebuffer(fb) => fb.write_str(s),
        }
    }
}

/// `\n` -> `\r\n` translation, same as `Uart`/`Uart16550`'s `write_str`,
/// but batched through a local buffer and sent as chunked virtqueue
/// transfers rather than one MMIO write per byte - a raw UART write is
/// cheap enough per-byte that batching wouldn't matter, but each
/// `virtio_console::Device::write` call is a full virtqueue round trip
/// (notify + poll the used ring), so sending a whole log line as one (or
/// a few) transfers instead of dozens matters here in a way it doesn't
/// for the other drivers.
fn write_str_virtio(dev: &mut virtio_console::Device, s: &str) -> fmt::Result {
    const CHUNK: usize = 256;
    let mut buf = [0u8; CHUNK];
    let mut len = 0usize;

    // SAFETY, both `write` calls below: `dev` was only ever constructed
    // after a successful `init()` - see `main.rs`'s console-discovery
    // sequence.
    for byte in s.bytes() {
        if byte == b'\n' {
            if len == buf.len() {
                let _ = unsafe { dev.write(&buf[..len]) };
                len = 0;
            }
            buf[len] = b'\r';
            len += 1;
        }
        if len == buf.len() {
            let _ = unsafe { dev.write(&buf[..len]) };
            len = 0;
        }
        buf[len] = byte;
        len += 1;
    }
    if len > 0 {
        let _ = unsafe { dev.write(&buf[..len]) };
    }
    Ok(())
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
            // SAFETY: same as write_str_virtio's.
            Console::Virtio(dev) => {
                let _ = unsafe { dev.write(&[byte]) };
            }
            Console::Framebuffer(fb) => fb.put_char(byte),
        }
    }

    /// Non-blocking: `None` if no byte is waiting - always, for
    /// `Virtio`, since this driver has no receive path yet (see
    /// `virtio_console.rs`'s module doc comment).
    fn read_byte(&mut self) -> Option<u8> {
        match self {
            Console::Pl011(uart) => uart.read_byte(),
            Console::Uart16550(uart) => uart.read_byte(),
            Console::Virtio(_) => None,
            Console::Framebuffer(_) => None,
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

/// Whether a console has been installed yet - used by `main.rs` to decide
/// whether a later fallback mechanism (virtio-console, then the
/// framebuffer console) still needs to try.
pub fn is_installed() -> bool {
    unsafe { (*CONSOLE.0.get()).is_some() }
}

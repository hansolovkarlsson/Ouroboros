//! Global console handle, shared between `main`'s boot sequence and the
//! exception handler — a fault needs somewhere to report through too, if a
//! console happens to have been installed before it fired.

use core::fmt;
use crate::synccell::SyncCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

static CONSOLE: SyncCell<Option<Console>> = SyncCell::new(None);

/// The console lock (class E of the multi-core plan's inventory): the
/// index plus one of the core that holds it, 0 when free, held across one
/// write (a line, or a `CON_WRITE` batch) so two cores' output does not
/// interleave byte by byte. A core that finds the lock its own proceeds at
/// once: that is a fault handler printing inside a print the fault cut
/// short, and the holder never resumes. Any other holder is waited for up
/// to `LOCK_SPINS` tries, then the writer takes the lock over by owner
/// word, which makes the stale holder's later `unlock` a no-op (it
/// compares the owner), so the lock never has two owners and a holder that
/// died with it costs the bound once. A live holder that was merely slow
/// then finishes its write unlocked, one interleaved line: the price of
/// never deadlocking a fault report.
static CONSOLE_LOCK: AtomicU64 = AtomicU64::new(0);
const LOCK_SPINS: u32 = 50_000_000;

/// Takes the console lock and says whether this call took it: `false` when
/// this core already held it (a fault report inside a print, or a print
/// inside an `FB_*` primitive), and the matching `unlock(false)` then leaves
/// it held for the outer holder. The pair is how a nested use cannot drop
/// the lock from under the code that took it.
#[must_use]
pub(crate) fn lock() -> bool {
    let me = crate::smp::core_index() + 1;
    for _ in 0..LOCK_SPINS {
        match CONSOLE_LOCK.compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(owner) if owner == me => return false,
            Err(_) => core::hint::spin_loop(),
        }
    }
    CONSOLE_LOCK.swap(me, Ordering::Acquire);
    true
}

/// Releases the console lock if `acquired` says this core's `lock` took it.
pub(crate) fn unlock(acquired: bool) {
    if !acquired {
        return;
    }
    let me = crate::smp::core_index() + 1;
    let _ = CONSOLE_LOCK.compare_exchange(me, 0, Ordering::Release, Ordering::Relaxed);
}

/// Lifts the console server's cursor (an inverted framebuffer cell) while
/// the kernel's own console draws, when that console is the framebuffer:
/// only then does a kernel line land on the server's screen. On a
/// byte-stream console (QEMU's UART, the Pi's PL011, with `cond` on the
/// framebuffer beside it) the cursor stays where it is and nothing is paid.
/// Console lock held by the caller.
fn lift_cursor_if_drawing() -> Option<(usize, usize)> {
    match unsafe { (*CONSOLE.get()).as_ref() } {
        Some(Console::Framebuffer(_)) => crate::fbdev::lift_cursor(),
        _ => None,
    }
}

/// When set, ordinary [`print`]/[`println`] output is suppressed - the
/// "kernel console goes quiet once `cond` owns the screen" handoff.
/// `main` sets it only on a framebuffer-only platform where the userland
/// console server (`cond`) is loaded and rendering to the same
/// framebuffer the kernel's own `fbconsole` would (Parallels); there,
/// kernel operational logs (`task N exited`, the USB/mount diagnostics,
/// ...) would otherwise render at the kernel `fbconsole`'s stale cursor
/// and corrupt `cond`'s output. On a byte-stream console (QEMU's UART)
/// this stays off - those logs interleave harmlessly and are useful for
/// dev. Fault reports bypass it entirely via [`print_force`] - a fault is
/// worth showing even if it overwrites the server's screen.
static CONSOLE_QUIET: AtomicBool = AtomicBool::new(false);

/// Installs `console` as the global console. Must only be called after
/// `exit_boot_services`, and only once.
pub fn install(console: Console) {
    unsafe {
        *CONSOLE.get() = Some(console);
    }
}

/// Whether the installed console renders to a framebuffer (as opposed to
/// a byte stream) - the case where the kernel and the userland console
/// server share a screen and the kernel needs to go quiet. Used by `main`
/// to decide whether to arm [`set_quiet`].
pub fn is_framebuffer() -> bool {
    matches!(unsafe { (*CONSOLE.get()).as_ref() }, Some(Console::Framebuffer(_)))
}

/// Silence ([`true`]) or restore ([`false`]) ordinary kernel console
/// output - see [`CONSOLE_QUIET`].
pub fn set_quiet(quiet: bool) {
    CONSOLE_QUIET.store(quiet, Ordering::Relaxed);
}

/// Writes to the global console if one has been installed, unless the
/// console has been quieted ([`set_quiet`]); silently does nothing when
/// there's no console yet (e.g. a fault before discovery/`install` ran).
pub fn print(args: fmt::Arguments) {
    if CONSOLE_QUIET.load(Ordering::Relaxed) {
        return;
    }
    print_force(args);
}

/// Like [`print`], but ignores [`set_quiet`] - for fault reports, which
/// must reach a console even after the kernel has otherwise gone quiet.
pub fn print_force(args: fmt::Arguments) {
    with_console(|console| {
        let _ = fmt::Write::write_fmt(console, args);
    });
}

/// Every write to the installed console goes through here: the lock taken
/// (or found held by this core), the console server's cursor lifted when
/// this console draws on its screen, the write, the cursor back, the lock
/// released if it was taken here. The one place the sequence is spelled,
/// so a writer cannot drop a step of it. Nothing happens without a console.
fn with_console(write: impl FnOnce(&mut Console)) {
    let held = lock();
    let lifted = lift_cursor_if_drawing();
    if let Some(console) = unsafe { (*CONSOLE.get()).as_mut() } {
        write(console);
    }
    crate::fbdev::restore_cursor(lifted);
    unlock(held);
}

macro_rules! println {
    ($($arg:tt)*) => {
        $crate::console::print(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use println;

/// [`println`] that bypasses [`set_quiet`] (via [`print_force`]) - the
/// fault handlers' reporting macro.
macro_rules! println_force {
    ($($arg:tt)*) => {
        $crate::console::print_force(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use println_force;

/// Writes one raw byte to the global console if one has been installed;
/// silently does nothing otherwise, same as [`print`].
pub fn putc(byte: u8) {
    write_bytes(core::slice::from_ref(&byte));
}

/// Writes a batch of raw bytes under the lock once: `CON_WRITE`'s path,
/// so another core's line cannot land inside `cond`'s escape sequence and
/// a batch costs two atomic operations rather than two per byte.
pub fn write_bytes(bytes: &[u8]) {
    with_console(|console| {
        for &b in bytes {
            console.write_byte(b);
        }
    });
}

/// Non-blocking read of one byte from the global console. `None` if there
/// is no console installed yet, or there is one but nothing is waiting.
pub fn read_byte() -> Option<u8> {
    unsafe { (*CONSOLE.get()).as_mut() }.and_then(Console::read_byte)
}

/// Whether a console has been installed yet - used by `main.rs` to decide
/// whether a later fallback mechanism (virtio-console, then the
/// framebuffer console) still needs to try.
pub fn is_installed() -> bool {
    unsafe { (*CONSOLE.get()).is_some() }
}

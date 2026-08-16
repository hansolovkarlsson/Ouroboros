//! Framebuffer console discovery via `EFI_GRAPHICS_OUTPUT_PROTOCOL` (GOP) -
//! the real lead for Parallels console output, replacing the virtio-console
//! lead confirmed dead there (see `CLAUDE.md`'s "virtio-console" section).
//! Real evidence, not another guess: a Parallels ARM64 VM's own config only
//! offers two "video type" options (a proprietary Parallels GPU, or
//! VirtIO GPU) - no serial option exists at all - and a real user's FreeBSD
//! boot log on that exact platform shows `efifb` (the generic UEFI GOP
//! framebuffer driver) driving console output before any GPU-specific
//! driver takes over. That's the same mechanism already visibly working in
//! this project's own Parallels boot screenshots - this module continues
//! writing into that same framebuffer past `exit_boot_services`, instead of
//! it going dark the moment boot services end.
//!
//! Unlike every previous console-discovery module, GOP needs no address
//! guessing or platform-specific convention at all: it's a standard,
//! fully-specified UEFI protocol, queried the same way on every platform.
//! Must run before `exit_boot_services` (`uefi::boot::open_protocol` is
//! boot-services-only) - same constraint as `pci.rs`'s enumeration, no
//! post-exit half.
//!
//! **A real, confirmed bug lived here once: opening GOP with
//! `open_protocol_exclusive`.** That attribute does exactly what its name
//! says - the `uefi` crate's own doc comment for it warns that any driver
//! holding the protocol `ByDriver` gets forcibly disconnected
//! (`disconnect_controller`), citing `SERIAL_IO_PROTOCOL` disconnecting
//! the console driver as its own example. On real Parallels hardware,
//! firmware's own text console (what every boot screenshot in this
//! project shows) holds GOP `ByDriver` to render it - so the very first
//! call into this module silently killed the visible boot console before
//! a single further log line could print, with no crash and no visible
//! symptom beyond "everything after the PCI dump just stops." Confirmed
//! by testing on real Parallels hardware, not inferred: a boot screenshot
//! showed exactly this - devicetree/ACPI/PCI discovery and the PCI device
//! dump all rendered, then nothing, not even
//! `framebuffer::discover()`'s own success/failure log line that
//! unconditionally follows in `main.rs`. QEMU's `ramfb` test (used to
//! verify this module before real hardware was available) never caught
//! it: that test always ran with `-display none`, so there was no console
//! driver attached to GOP to disconnect in the first place. Fixed by
//! switching to `OpenProtocolAttributes::GetProtocol` (via the `unsafe`
//! generic `open_protocol`, not the exclusive convenience wrapper) -
//! read-only querying, doesn't touch driver ownership at all. Safe here:
//! this module only ever reads `ModeInfo`/`FrameBuffer` once, synchronously,
//! and never touches the `GraphicsOutput`/`FrameBuffer` objects again
//! after this function returns (see `fbconsole.rs`'s safety note on why
//! the raw pointer, not the protocol object, is what crosses into
//! post-exit code).

use uefi::boot::{OpenProtocolAttributes, OpenProtocolParams};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

#[derive(Debug, Clone, Copy)]
pub struct Info {
    /// Physical base address of the framebuffer.
    pub base: u64,
    /// Framebuffer size in bytes.
    pub size: usize,
    pub width: usize,
    pub height: usize,
    /// Pixels per scan line - may exceed `width` (padding for alignment),
    /// per the GOP spec. Always use this, not `width`, to compute a pixel's
    /// byte offset.
    pub stride: usize,
    pub format: PixelFormat,
}

#[derive(Debug)]
pub enum Error {
    /// No handle on the system supports `GraphicsOutput` at all.
    NoGop,
    /// A GOP exists, but its pixel format is `Bitmask` or `BltOnly` - this
    /// module only speaks the two formats with a fixed, known byte layout
    /// per pixel (`Rgb`/`Bgr`, both 4 bytes: 3 colour bytes + 1 reserved).
    UnsupportedPixelFormat(PixelFormat),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoGop => write!(f, "no GraphicsOutput protocol found"),
            Error::UnsupportedPixelFormat(format) => write!(f, "unsupported pixel format {format:?} (only Rgb/Bgr are implemented)"),
        }
    }
}

/// Queries the first available `GraphicsOutput` protocol for its current
/// mode's resolution/stride/pixel format, and the framebuffer's physical
/// base address and size. Doesn't touch the framebuffer's contents - pure
/// discovery, same spirit as every other `discover_*` in this project.
///
/// # Safety
/// None beyond the ordinary boot-services requirement: must be called
/// before `exit_boot_services`.
pub fn discover() -> Result<Info, Error> {
    let handle = uefi::boot::get_handle_for_protocol::<GraphicsOutput>().map_err(|_| Error::NoGop)?;
    // SAFETY: GetProtocol is a read-only, non-owning open - we only read
    // ModeInfo/FrameBuffer once below and never touch `gop` again after
    // this function returns, so there's no risk of the interface being
    // mutated or removed out from under a held reference. Deliberately
    // not `open_protocol_exclusive`: see this module's doc comment for
    // the real bug that came from disconnecting firmware's own console
    // driver on real Parallels hardware.
    let mut gop = unsafe {
        uefi::boot::open_protocol::<GraphicsOutput>(
            OpenProtocolParams { handle, agent: uefi::boot::image_handle(), controller: None },
            OpenProtocolAttributes::GetProtocol,
        )
    }
    .map_err(|_| Error::NoGop)?;

    let mode_info = gop.current_mode_info();
    let format = mode_info.pixel_format();
    if !matches!(format, PixelFormat::Rgb | PixelFormat::Bgr) {
        return Err(Error::UnsupportedPixelFormat(format));
    }
    let (width, height) = mode_info.resolution();
    let stride = mode_info.stride();

    let mut fb = gop.frame_buffer();
    let base = fb.as_mut_ptr() as u64;
    let size = fb.size();

    Ok(Info { base, size, width, height, stride, format })
}

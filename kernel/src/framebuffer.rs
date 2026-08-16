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
//! Must run before `exit_boot_services` (`uefi::boot::open_protocol_exclusive`
//! is boot-services-only) - same constraint as `pci.rs`'s enumeration, no
//! post-exit half.

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
    let mut gop = uefi::boot::open_protocol_exclusive::<GraphicsOutput>(handle).map_err(|_| Error::NoGop)?;

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

//! Dumb framebuffer primitives for the console server (`cond`) - the
//! framebuffer analogue of the `block::BlockDevice` behind the `BLOCK_*`
//! syscalls. The *text* logic (font, cursor, line wrap, scroll
//! decisions, ANSI parsing) lives in `cond` now; this is only the pixel
//! plumbing it drives through the gated `FB_*` syscalls (`syscall.rs`,
//! accepted from `CON_TASK` alone): plot a run of glyph bitmaps, scroll
//! the screen up, clear it.
//!
//! `cond` looks each character up in its *own* copy of `font.rs` and
//! sends the 8-byte glyph bitmaps here - the font (the thing that was
//! "console driver logic") is what moved to userland; this keeps only
//! the raw put-pixel/memmove, a dumb 2D blitter.
//!
//! The kernel's own `fbconsole` (the emergency/boot console) writes the
//! *same* framebuffer independently, for boot messages and fault
//! reports. Single-core with no preemption inside a syscall, so the two
//! never run concurrently; on a framebuffer-only platform they do share
//! the screen (kernel during boot/faults, `cond` in steady state) - see
//! `CLAUDE.md`'s "Driver isolation, part 3".

use core::cell::UnsafeCell;

use crate::framebuffer::Info;

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 8;
const BYTES_PER_PIXEL: usize = 4;

/// One glyph bitmap is 8 bytes (one per pixel row), matching `font.rs`.
pub const GLYPH_BYTES: usize = 8;

struct FbDev {
    base: *mut u8,
    stride: usize,
    cols: usize,
    rows: usize,
}

struct FbCell(UnsafeCell<Option<FbDev>>);
// SAFETY: single-core; set once at boot before any task runs, then only
// read/written from within the FB_* syscall arms (IRQs masked, never
// reentrant) - the same reasoning as every other per-boot global here.
unsafe impl Sync for FbCell {}
static FB: FbCell = FbCell(UnsafeCell::new(None));

/// Records the framebuffer geometry for the `FB_*` syscalls. Called once
/// from `main` when a framebuffer was discovered and mapped, regardless
/// of which console the kernel itself installed - so `cond` can render to
/// the framebuffer even on a platform (QEMU + `ramfb`) where a byte-stream
/// console won the kernel's own console slot.
///
/// # Safety
/// `info.base` must be a valid, writable framebuffer mapped into this
/// kernel's identity map (true after `mmu::install_identity_map` ran with
/// this framebuffer folded in), same contract as `FbConsole::new`.
pub unsafe fn install(info: &Info) {
    unsafe {
        *FB.0.get() = Some(FbDev {
            base: info.base as *mut u8,
            stride: info.stride,
            cols: info.width / GLYPH_W,
            rows: info.height / GLYPH_H,
        });
    }
}

pub fn is_present() -> bool {
    unsafe { (*FB.0.get()).is_some() }
}

pub fn cols() -> usize {
    unsafe { (*FB.0.get()).as_ref().map_or(0, |f| f.cols) }
}

pub fn rows() -> usize {
    unsafe { (*FB.0.get()).as_ref().map_or(0, |f| f.rows) }
}

fn put_pixel(fb: &FbDev, x: usize, y: usize, white: bool) {
    let off = (y * fb.stride + x) * BYTES_PER_PIXEL;
    let level: u8 = if white { 0xff } else { 0x00 };
    // Rgb/Bgr are channel-order symmetric for pure white/black (see
    // fbconsole's put_pixel), so this needs no pixel-format branch.
    unsafe {
        fb.base.add(off).write_volatile(level);
        fb.base.add(off + 1).write_volatile(level);
        fb.base.add(off + 2).write_volatile(level);
    }
}

fn draw_one(fb: &FbDev, glyph: &[u8], col: usize, row: usize) {
    let x0 = col * GLYPH_W;
    let y0 = row * GLYPH_H;
    for (dy, bits) in glyph.iter().enumerate().take(GLYPH_H) {
        for dx in 0..GLYPH_W {
            put_pixel(fb, x0 + dx, y0 + dy, (bits >> dx) & 1 != 0);
        }
    }
}

/// Plot `count` consecutive 8-byte glyph bitmaps at cells
/// `(col..col+count, row)`. `glyphs` must be at least `count * GLYPH_BYTES`
/// long (the syscall arm validates the caller's buffer). Cells past the
/// last column, or a row past the last, are skipped rather than wrapping -
/// wrap/scroll decisions belong to `cond`.
pub fn blit_glyphs(glyphs: &[u8], count: usize, col: usize, row: usize) {
    let Some(fb) = (unsafe { (*FB.0.get()).as_ref() }) else {
        return;
    };
    if row >= fb.rows {
        return;
    }
    for i in 0..count {
        let c = col + i;
        if c >= fb.cols {
            break;
        }
        let start = i * GLYPH_BYTES;
        if start + GLYPH_BYTES > glyphs.len() {
            break;
        }
        draw_one(fb, &glyphs[start..start + GLYPH_BYTES], c, row);
    }
}

/// Scroll the screen up by `n` text rows (memmove within the framebuffer,
/// same as `fbconsole::scroll` but for a run of rows), blanking the
/// newly-exposed bottom. `n >= rows` clears the whole screen.
pub fn scroll(n: usize) {
    let Some(fb) = (unsafe { (*FB.0.get()).as_ref() }) else {
        return;
    };
    if n == 0 {
        return;
    }
    if n >= fb.rows {
        clear();
        return;
    }
    let row_bytes = fb.stride * BYTES_PER_PIXEL;
    let shift_bytes = row_bytes * GLYPH_H * n;
    let total_bytes = row_bytes * GLYPH_H * fb.rows;
    unsafe {
        core::ptr::copy(fb.base.add(shift_bytes), fb.base, total_bytes - shift_bytes);
        // Blank the bottom n text rows the copy exposed.
        core::ptr::write_bytes(fb.base.add(total_bytes - shift_bytes), 0, shift_bytes);
    }
}

/// Blank the entire framebuffer.
pub fn clear() {
    let Some(fb) = (unsafe { (*FB.0.get()).as_ref() }) else {
        return;
    };
    let total_bytes = fb.stride * BYTES_PER_PIXEL * GLYPH_H * fb.rows;
    unsafe {
        core::ptr::write_bytes(fb.base, 0, total_bytes);
    }
}

//! Dumb framebuffer primitives for the console server (`cond`) - the
//! framebuffer analogue of the `block::BlockDevice` behind the `BLOCK_*`
//! syscalls. The *text* logic (font, cursor, line wrap, scroll
//! decisions, ANSI parsing) lives in `cond` now; this is only the pixel
//! plumbing it drives through the gated `FB_*` syscalls (`syscall.rs`,
//! accepted from `CON_TASK` alone): plot a run of glyph bitmaps, scroll
//! the screen up, clear it, invert one cell (the server's cursor).
//!
//! `cond` looks each character up in its *own* copy of `font.rs` and
//! sends the 8-byte glyph bitmaps here - the font (the thing that was
//! "console driver logic") is what moved to userland; this keeps only
//! the raw put-pixel/memmove, a dumb 2D blitter.
//!
//! The kernel's own `fbconsole` (the emergency/boot console) writes the
//! *same* framebuffer independently, for boot messages and fault
//! reports. One core is in the kernel at a time (`lock.rs`) and `cond`
//! stays on the boot core, so the two never run concurrently; on a framebuffer-only platform they do share
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
    /// The cell `invert_cell` last inverted and has not inverted back:
    /// the console server's cursor, as this side knows it. The kernel's
    /// own prints lift it while they draw (`lift_cursor`).
    cursor: Option<(usize, usize)>,
}

struct FbCell(UnsafeCell<Option<FbDev>>);
// SAFETY: one core is in the kernel at a time (`lock.rs`, taken at every
// resumable entry's Rust half), and the FB_* syscall arms, the only
// writers of this record, run under it; the console's `lift_cursor` and
// `restore_cursor` read the record and write pixels only, under the
// console lock. `SyncCell` would state the argument for us, except that
// `FbDev` holds the framebuffer's raw `*mut u8` and is not `Send`, so the
// bound refuses it and this wrapper argues its own case: set once at boot
// before any task runs. (Before 2026-10-11 this comment made the
// single-core argument, which the multi-core arc ended.)
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
            cursor: None,
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

/// Makes `rows` runs of `len` bytes, starting at byte offset `off` and one
/// pixel row apart, visible to the display: the framebuffer is ordinary
/// cacheable memory, and the display engine reads memory, not the CPU's
/// caches (`docs/testing/testing-pi4.md` Risk 7, the Raspberry Pi). EVERY
/// write path below ends in a call to this - the pixels are not on screen
/// until it runs - and it costs one cache barrier however many rows.
fn publish(fb: &FbDev, off: usize, len: usize, rows: usize) {
    let stride_bytes = (fb.stride * BYTES_PER_PIXEL) as u64;
    crate::mmu::clean_to_poc(fb.base as u64 + off as u64, len as u64, rows as u64, stride_bytes);
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
    let mut drawn = 0usize;
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
        drawn += 1;
    }
    // The run of cells is contiguous within each of its pixel rows.
    let off = (row * GLYPH_H * fb.stride + col * GLYPH_W) * BYTES_PER_PIXEL;
    publish(fb, off, drawn * GLYPH_W * BYTES_PER_PIXEL, GLYPH_H);
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
    publish(fb, 0, total_bytes, 1);
}

/// Inverts the pixels of cell `(col, row)`: white to black and black to
/// white, every channel. The console server's cursor is this: drawn by
/// inverting the cell the cursor is on, taken off by inverting it again,
/// so the server keeps no copy of what the cell holds and the screen stays
/// the one truth. The cell is recorded as the cursor until the same cell is
/// inverted again. A cell past the last column or row is skipped.
pub fn invert_cell(col: usize, row: usize) {
    let Some(fb) = (unsafe { (*FB.0.get()).as_mut() }) else {
        return;
    };
    if col >= fb.cols || row >= fb.rows {
        return;
    }
    invert_pixels(fb, col, row);
    fb.cursor = if fb.cursor == Some((col, row)) { None } else { Some((col, row)) };
}

/// Takes the console server's cursor off the screen while the kernel's own
/// console draws (a fault report, forced past `CONSOLE_QUIET`): the cell
/// gets its pixels back and the record stands. `restore_cursor` puts it
/// back. Without this the kernel's lines, and the scroll they can cause,
/// would redraw the cell under an inversion the server still believes in,
/// and the server's next write would invert a glyph that was never the
/// cursor. The cursor is then over whatever the kernel left in that cell,
/// which is the server's to redraw, and nothing is left inverted for good.
pub fn lift_cursor() -> Option<(usize, usize)> {
    let fb = unsafe { (*FB.0.get()).as_ref() }?;
    let (col, row) = fb.cursor?;
    invert_pixels(fb, col, row);
    fb.cursor
}

/// Puts back what `lift_cursor` took off.
pub fn restore_cursor(lifted: Option<(usize, usize)>) {
    let Some(fb) = (unsafe { (*FB.0.get()).as_ref() }) else {
        return;
    };
    if let Some((col, row)) = lifted {
        invert_pixels(fb, col, row);
    }
}

fn invert_pixels(fb: &FbDev, col: usize, row: usize) {
    let x0 = col * GLYPH_W;
    let y0 = row * GLYPH_H;
    for dy in 0..GLYPH_H {
        for dx in 0..GLYPH_W {
            let off = ((y0 + dy) * fb.stride + x0 + dx) * BYTES_PER_PIXEL;
            // The three colour channels, as put_pixel writes them.
            for channel in 0..3 {
                unsafe {
                    let p = fb.base.add(off + channel);
                    p.write_volatile(!p.read_volatile());
                }
            }
        }
    }
    let off = (y0 * fb.stride + x0) * BYTES_PER_PIXEL;
    publish(fb, off, GLYPH_W * BYTES_PER_PIXEL, GLYPH_H);
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
    publish(fb, 0, total_bytes, 1);
}

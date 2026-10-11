//! Dumb framebuffer primitives for the console server (`cond`) - the
//! framebuffer analogue of the `block::BlockDevice` behind the `BLOCK_*`
//! syscalls. The *text* logic (font, cursor, line wrap, scroll
//! decisions, ANSI parsing) lives in `cond` now; this is only the pixel
//! plumbing it drives through the gated `FB_*` syscalls (`syscall.rs`,
//! accepted from `CON_TASK` alone): plot a run of glyph bitmaps, scroll
//! the screen up, clear it, and keep the server's cursor: one cell shown
//! inverted, which this side owns so that every other primitive and the
//! kernel's own prints can draw around it.
//!
//! `cond` looks each character up in its *own* copy of `font.rs` and
//! sends the 8-byte glyph bitmaps here - the font (the thing that was
//! "console driver logic") is what moved to userland; this keeps only
//! the raw put-pixel/memmove, a dumb 2D blitter.
//!
//! The kernel's own `fbconsole` (the emergency/boot console) writes the
//! *same* framebuffer independently, for boot messages and fault
//! reports. The console lock serialises the two (`FbCell`'s SAFETY note
//! below: the `FB_*` arms take it, the kernel's prints hold it; the kernel
//! lock does not cover a halting core's fault report); on a framebuffer-only platform they do share
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
    /// The console server's cursor: the one cell drawn inverted, set by
    /// `FB_CURSOR`. This side owns it, so a blit over it draws that glyph
    /// inverted, a scroll, a clear and the kernel's own prints lift it first
    /// and put it back after (`lift_cursor`, `restore_cursor`), and nothing
    /// is left inverted that is not the cursor. It dies with the server
    /// (`server_ended`), so a restarted server starts with none.
    cursor: Option<(usize, usize)>,
}

struct FbCell(UnsafeCell<Option<FbDev>>);
// SAFETY: every path that touches the record or the pixels holds the
// console lock (`console::lock`, reentrant per core and bounded, so a
// fault report never deadlocks on it): the FB_* syscall arms take it
// around their work, under the kernel lock they already hold (the same
// order the fault handler takes them in), and the console's own prints
// hold it for their whole write, where `lift_cursor` and `restore_cursor`
// run. A halting secondary's fault report and the boot core's FB_* arm
// are serialised by it, not by the kernel lock. The lock is bounded: a
// holder that does not finish in `LOCK_SPINS` is taken over, so that a
// fault report never deadlocks, and in that one case a reader can see the
// record mid-write. The price is bounded too: `invert_pixels` refuses a
// cell past the screen, so the worst outcome is one wrong cell inverted.
// The geometry fields are written once at `install` and read unlocked.
// `SyncCell` would state the argument for us, except that
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

/// The three colour channels of a pixel, as `put_pixel` writes them; the
/// fourth byte is padding and stays.
const PIXEL_MASK: u32 = 0x00ff_ffff;

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

/// Draws one glyph at a cell, its bits inverted when the cursor is on
/// that cell, so the cursor shows on the new glyph with no second pass.
fn draw_one(fb: &FbDev, glyph: &[u8], col: usize, row: usize, inverted: bool) {
    let x0 = col * GLYPH_W;
    let y0 = row * GLYPH_H;
    let flip = if inverted { 0xff } else { 0 };
    for (dy, bits) in glyph.iter().enumerate().take(GLYPH_H) {
        let bits = bits ^ flip;
        for dx in 0..GLYPH_W {
            put_pixel(fb, x0 + dx, y0 + dy, (bits >> dx) & 1 != 0);
        }
    }
}

/// Plot `count` consecutive 8-byte glyph bitmaps at cells
/// `(col..col+count, row)`. `glyphs` must be at least `count * GLYPH_BYTES`
/// long (the syscall arm validates the caller's buffer). Cells past the
/// last column, or a row past the last, are skipped rather than wrapping -
/// wrap/scroll decisions belong to `cond`. The glyph that lands on the
/// cursor's cell is drawn inverted, so the cursor sits on it at the cost
/// of the one publish the run pays anyway.
pub fn blit_glyphs(glyphs: &[u8], count: usize, col: usize, row: usize) {
    let held = crate::console::lock();
    if let Some(fb) = unsafe { (*FB.0.get()).as_ref() } {
        if row < fb.rows {
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
                draw_one(fb, &glyphs[start..start + GLYPH_BYTES], c, row, fb.cursor == Some((c, row)));
                drawn += 1;
            }
            // The run of cells is contiguous within each of its pixel rows.
            let off = (row * GLYPH_H * fb.stride + col * GLYPH_W) * BYTES_PER_PIXEL;
            publish(fb, off, drawn * GLYPH_W * BYTES_PER_PIXEL, GLYPH_H);
        }
    }
    crate::console::unlock(held);
}

/// Scroll the screen up by `n` text rows (memmove within the framebuffer,
/// same as `fbconsole::scroll` but for a run of rows), blanking the
/// newly-exposed bottom. `n >= rows` clears the whole screen. The cursor
/// stays on its screen cell: the server scrolls because its cursor is on
/// the last row, and that is where it still is.
pub fn scroll(n: usize) {
    let held = crate::console::lock();
    if let Some(fb) = unsafe { (*FB.0.get()).as_ref() } {
        if n > 0 {
            let lifted = lift_cursor();
            if n >= fb.rows {
                clear_pixels(fb);
            } else {
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
            restore_cursor(lifted);
        }
    }
    crate::console::unlock(held);
}

/// Blank the entire framebuffer. The cursor, if there is one, is drawn
/// again on its cell afterwards: the server's `CSI 2 J` leaves its cursor
/// where it was, and a server restarted by the supervisor clears first and
/// then places its own cursor with `FB_CURSOR`, which moves this one.
pub fn clear() {
    let held = crate::console::lock();
    if let Some(fb) = unsafe { (*FB.0.get()).as_ref() } {
        let lifted = lift_cursor();
        clear_pixels(fb);
        restore_cursor(lifted);
    }
    crate::console::unlock(held);
}

fn clear_pixels(fb: &FbDev) {
    let total_bytes = fb.stride * BYTES_PER_PIXEL * GLYPH_H * fb.rows;
    unsafe {
        core::ptr::write_bytes(fb.base, 0, total_bytes);
    }
    publish(fb, 0, total_bytes, 1);
}

/// Puts the console server's cursor on cell `(col, row)`, or nowhere when
/// the cell is past the screen (`FB_CURSOR_NONE` is the spelling for
/// that): the old cell gets its pixels back, the new one is inverted, and
/// the record follows. The one writer of the record. The same cell again
/// costs nothing.
pub fn set_cursor(col: u64, row: u64) {
    let held = crate::console::lock();
    if let Some(fb) = unsafe { (*FB.0.get()).as_mut() } {
        let wanted = usize::try_from(col)
            .ok()
            .zip(usize::try_from(row).ok())
            .filter(|&(c, r)| c < fb.cols && r < fb.rows);
        if wanted != fb.cursor {
            if let Some((c, r)) = fb.cursor {
                invert_pixels(fb, c, r);
            }
            fb.cursor = wanted;
            if let Some((c, r)) = wanted {
                invert_pixels(fb, c, r);
            }
        }
    }
    crate::console::unlock(held);
}

/// The console server's life ended (`tasks::end_task`, or the supervisor
/// restarting a wedged one): its cursor goes with it, so the next life
/// starts with none and no client has to know about the last one.
pub fn server_ended(slot: u64) {
    if slot == syscall_abi::CON_TASK {
        set_cursor(syscall_abi::FB_CURSOR_NONE, 0);
    }
}

/// Takes the cursor off the screen while something else draws: the cell
/// gets its pixels back and the record stands, so `restore_cursor` puts
/// the inversion back on the same cell, over whatever was drawn there.
/// Called with the console lock held, by the primitives above and by the
/// console's own prints (a fault report, forced past `CONSOLE_QUIET`,
/// whose lines and the scroll they can cause would otherwise redraw the
/// cell under an inversion the server still believes in).
pub fn lift_cursor() -> Option<(usize, usize)> {
    let fb = unsafe { (*FB.0.get()).as_ref() }?;
    let (col, row) = fb.cursor?;
    invert_pixels(fb, col, row);
    fb.cursor
}

/// Puts back what `lift_cursor` took off. Console lock held, as above.
pub fn restore_cursor(lifted: Option<(usize, usize)>) {
    let Some(fb) = (unsafe { (*FB.0.get()).as_ref() }) else {
        return;
    };
    if let Some((col, row)) = lifted {
        invert_pixels(fb, col, row);
    }
}

/// Inverts the pixels of one cell, white to black and black to white: one
/// 32-bit XOR of the three colour channels per pixel. A cell past the
/// screen is skipped here too, so a torn record (see `FbCell`'s SAFETY
/// note) can cost a cell and never a write outside the framebuffer.
fn invert_pixels(fb: &FbDev, col: usize, row: usize) {
    if col >= fb.cols || row >= fb.rows {
        return;
    }
    let x0 = col * GLYPH_W;
    let y0 = row * GLYPH_H;
    for dy in 0..GLYPH_H {
        for dx in 0..GLYPH_W {
            let off = ((y0 + dy) * fb.stride + x0 + dx) * BYTES_PER_PIXEL;
            unsafe {
                let p = fb.base.add(off) as *mut u32;
                p.write_volatile(p.read_volatile() ^ PIXEL_MASK);
            }
        }
    }
    let off = (y0 * fb.stride + x0) * BYTES_PER_PIXEL;
    publish(fb, off, GLYPH_W * BYTES_PER_PIXEL, GLYPH_H);
}

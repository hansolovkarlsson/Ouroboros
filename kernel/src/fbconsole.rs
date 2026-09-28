//! Text console rendered directly into a UEFI GOP framebuffer - the real
//! lead for Parallels console output (see `framebuffer.rs`'s module doc
//! comment for why: virtio-console is a confirmed dead end there, but GOP
//! is a standard protocol and the same framebuffer this project's own
//! Parallels boot screenshots already show working).
//!
//! Draws fixed 8x8 glyphs (`font.rs`) on a character-cell grid, tracks a
//! cursor, and scrolls by memmove-ing pixel rows within the framebuffer
//! itself - deliberately no text buffer (matches this kernel's zero-heap,
//! no-static-mutable-state discipline for anything that isn't already a
//! fixed-size global). No colour, no ANSI escape parsing (a raw escape
//! byte just draws whatever blank glyph `font.rs` returns for a
//! non-printable code point - the shell's `clear` command's escape
//! sequence works fine on the byte-stream consoles that already parse it
//! host-side in a real terminal, but has no effect here beyond printing
//! junk glyphs; a real fix would mean teaching this module to parse ANSI
//! itself, out of scope for the initial cut). Write-only: `read_byte`
//! always returns `None` - there's no keyboard driver in this kernel at
//! all yet, a real gap independent of this console, not something this
//! module could address on its own.
//!
//! # Safety model
//! The raw framebuffer pointer/stride/format are captured during boot
//! services (`framebuffer::discover`) and used via `write_volatile`/
//! `ptr::copy` after `exit_boot_services` - the `GraphicsOutput`/
//! `FrameBuffer` objects themselves are never held past boot services. The
//! UEFI spec doesn't guarantee the pointer stays valid post-exit (see
//! `framebuffer.rs`), but this is the only way to find out - confirmed
//! working on QEMU's `ramfb` device (screendump-verified, see
//! `CLAUDE.md`'s framebuffer-console section). Whether the framebuffer
//! lands inside the identity map's discovered-RAM span or needs
//! `mmu.rs`'s separate device-region fallback determines its memory type:
//! inside RAM (QEMU `ramfb`, the Raspberry Pi) it is ordinary write-back
//! cacheable memory, and every write path here ends in `publish`, which
//! cleans the written bytes out to memory for the display engine - it
//! reads memory, not the CPU's caches (`docs/testing/testing-pi4.md` Risk
//! 7); outside RAM (a PCI BAR, Parallels) it is a Device-nGnRnE block, on
//! which the clean is a no-op. The `ptr::copy` scroll path is
//! screendump-checked on the former (QEMU `ramfb`).

use crate::font;
use crate::framebuffer::Info;

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 8;
const BYTES_PER_PIXEL: usize = 4;

pub struct FbConsole {
    base: *mut u8,
    stride: usize,
    cols: usize,
    rows: usize,
    cursor_col: usize,
    cursor_row: usize,
}

// SAFETY: single-core, no preemption, no interrupts unmasked until after
// this is installed - nothing can touch the framebuffer concurrently, same
// reasoning as every other driver in this project.
unsafe impl Send for FbConsole {}

impl FbConsole {
    /// # Safety
    /// `info.base` must be a valid, writable framebuffer of at least
    /// `info.size` bytes, mapped into this kernel's own identity map -
    /// true once `mmu::install_identity_map` has run with this
    /// framebuffer's `(base, size)` passed as its `framebuffer` argument.
    pub unsafe fn new(info: &Info) -> Self {
        let mut console = FbConsole {
            base: info.base as *mut u8,
            stride: info.stride,
            cols: info.width / GLYPH_W,
            rows: info.height / GLYPH_H,
            cursor_col: 0,
            cursor_row: 0,
        };
        console.clear();
        console
    }

    fn pixel_offset(&self, x: usize, y: usize) -> usize {
        (y * self.stride + x) * BYTES_PER_PIXEL
    }

    /// Makes `rows` runs of `len` bytes, starting at byte offset `off` and
    /// one pixel row apart, visible to the display - see `fbdev::publish`,
    /// the same rule: every write path here ends in a call to this, with
    /// one cache barrier however many rows it covers.
    fn publish(&self, off: usize, len: usize, rows: usize) {
        let stride_bytes = (self.stride * BYTES_PER_PIXEL) as u64;
        crate::mmu::clean_to_poc(self.base as u64 + off as u64, len as u64, rows as u64, stride_bytes);
    }

    /// Both `Rgb` and `Bgr` are 4 bytes/pixel (3 colour + 1 reserved) per
    /// the GOP spec - `discover()` rejects every other format, see its
    /// `UnsupportedPixelFormat`. White and black are channel-order
    /// symmetric, so this module doesn't need to track which format it
    /// got - a future colour beyond pure white/black would need to start
    /// storing and branching on it.
    fn put_pixel(&mut self, x: usize, y: usize, white: bool) {
        let off = self.pixel_offset(x, y);
        let level: u8 = if white { 0xff } else { 0x00 };
        unsafe {
            self.base.add(off).write_volatile(level);
            self.base.add(off + 1).write_volatile(level);
            self.base.add(off + 2).write_volatile(level);
        }
    }

    fn draw_glyph(&mut self, col: usize, row: usize, c: u8) {
        let glyph = font::glyph(c).unwrap_or(&[0; GLYPH_H]);
        let x0 = col * GLYPH_W;
        let y0 = row * GLYPH_H;
        for (dy, bits) in glyph.iter().enumerate() {
            for dx in 0..GLYPH_W {
                let set = (bits >> dx) & 1 != 0;
                self.put_pixel(x0 + dx, y0 + dy, set);
            }
        }
        self.publish(self.pixel_offset(x0, y0), GLYPH_W * BYTES_PER_PIXEL, GLYPH_H);
    }

    /// Blanks the text area in one write and one publish (it used to draw
    /// a space glyph into every cell, each with its own cache clean).
    fn clear(&mut self) {
        let total = self.stride * BYTES_PER_PIXEL * GLYPH_H * self.rows;
        unsafe {
            core::ptr::write_bytes(self.base, 0, total);
        }
        self.publish(0, total, 1);
        self.cursor_col = 0;
        self.cursor_row = 0;
    }

    /// Shifts every pixel row up by one glyph row via `ptr::copy`
    /// (memmove-safe for the overlapping source/dest ranges this is), then
    /// blanks the newly-exposed bottom text row. No text buffer to redraw
    /// from - see module doc comment.
    fn scroll(&mut self) {
        let row_bytes = self.stride * BYTES_PER_PIXEL;
        let glyph_row_bytes = row_bytes * GLYPH_H;
        let total_pixel_rows = self.rows * GLYPH_H;
        let move_bytes = row_bytes * (total_pixel_rows - GLYPH_H);
        unsafe {
            core::ptr::copy(self.base.add(glyph_row_bytes), self.base, move_bytes);
            // Blank the bottom text row the copy exposed.
            core::ptr::write_bytes(self.base.add(move_bytes), 0, glyph_row_bytes);
        }
        self.publish(0, move_bytes + glyph_row_bytes, 1);
    }

    fn newline(&mut self) {
        self.cursor_col = 0;
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll();
        }
    }

    /// Draws one byte at the cursor and advances it, wrapping/scrolling as
    /// needed. `\r`/`\n` are handled as cursor motion, not glyphs; every
    /// other byte (including non-ASCII/control bytes `font.rs` has no
    /// glyph for) draws whatever `draw_glyph` falls back to - see module
    /// doc comment on why this doesn't parse ANSI escapes.
    pub fn put_char(&mut self, c: u8) {
        match c {
            b'\r' => self.cursor_col = 0,
            b'\n' => self.newline(),
            _ => {
                self.draw_glyph(self.cursor_col, self.cursor_row, c);
                self.cursor_col += 1;
                if self.cursor_col >= self.cols {
                    self.newline();
                }
            }
        }
    }
}

impl core::fmt::Write for FbConsole {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            self.put_char(byte);
        }
        Ok(())
    }
}

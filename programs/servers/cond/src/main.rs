//! The console server (console daemon) - the seventh userland program,
//! and the second real component moved out of the EL1 kernel (after the
//! filesystem server, `fsd/`). It owns the *steady-state* console:
//! userland text output flows to it over IPC (a `DSPOP_WRITE` message,
//! normally via `MSG_CALL`), and it puts that text on the actual console,
//! while the kernel keeps only a minimal path for its own boot and fault
//! reporting.
//!
//! Boot-loaded by the kernel (`loader::load_cond`, `\EFI\ORBS\COND.BIN`)
//! into task slot 3 (`syscall_abi::CON_TASK`), which is exit/kill/wait-
//! protected and never used by `spawn` - exactly like the filesystem
//! server in slot 2. Same build shape as every other userland program
//! here: `aarch64-unknown-none`, release-only, shared linker script,
//! constants from `syscall-abi`, no static mutable state (the backend's
//! cursor lives in `main`'s stack frame).
//!
//! **Two backends, chosen at startup from `CON_INFO`:**
//! - **Byte-stream** (QEMU's UART path): forward text to the kernel's
//!   console through the gated `CON_WRITE` syscall. Nothing to render.
//! - **Framebuffer** (Parallels, QEMU `ramfb`): render text *here* - this
//!   is the console rendering logic (the 8x8 font, cursor, line wrap,
//!   scroll decisions, ANSI parsing) moved out of the kernel's
//!   `fbconsole`. Each glyph is looked up in this program's own `font`
//!   and blitted via `FB_BLIT`; the kernel keeps only the dumb pixel
//!   primitives (`FB_BLIT`/`FB_SCROLL`/`FB_CLEAR`, `kernel/src/fbdev.rs`).

#![no_std]
#![no_main]

mod font;

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
#[link_section = ".text.start"]
pub extern "C" fn _start() -> ! {
    main()
}

const REPLY_PAYLOAD: usize = syscall_abi::FS_REPLY_PAYLOAD as usize;
const DATA_MAX: usize = syscall_abi::FS_DATA_MAX as usize;
/// Request header size for the uniform verb set - the offset of a console
/// write's inline text (`NP_WRITE_FILE`'s payload). One word past `FSOP_*`.
const NP_HDR: usize = ninep_abi::NP_REQ_PAYLOAD as usize;

fn main() -> ! {
    let mut backend = detect_backend();
    write_text(&mut backend, b"cond: console server ready\r\n");

    let mut req = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    let mut reply = [0u8; syscall_abi::MSG_MAX_LEN as usize];
    loop {
        let packed = syscall4(syscall_abi::MSG_RECV, req.as_mut_ptr() as u64, req.len() as u64, 0, 0);
        if packed >= syscall_abi::FS_ERR_MIN {
            break;
        }
        let sender = packed >> 32;
        let len = ((packed & 0xffff_ffff) as usize).min(req.len());
        let reply_len = handle(&mut backend, &req[..len], &mut reply);
        syscall4(syscall_abi::MSG_SEND, sender, reply.as_mut_ptr() as u64, reply_len as u64, 0);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Decodes one request and builds a status-only reply. The console is written
/// with the uniform verb set (`ninep-abi`, the Phase 0 cluster protocol): an
/// `NP_WRITE_FILE` whose inline data is the text to render - a write to the
/// console "file". cond serves only the console, so it ignores the `tree` and
/// `path` fields and just renders the data. Any other verb is acked with status
/// 0 - lost output never wedges a client's `MSG_CALL`.
fn handle(backend: &mut Backend, req: &[u8], reply: &mut [u8]) -> usize {
    reply[..8].copy_from_slice(&0u64.to_le_bytes());
    if req.len() < NP_HDR {
        return REPLY_PAYLOAD;
    }
    if read_u64(req, 0) == ninep_abi::NP_WRITE_FILE {
        // NP_WRITE_FILE: data_len at param a1 (offset 24), inline data at the
        // payload offset. path_len (a0) is 0 from our clients - cond needs no path.
        let text_len = read_u64(req, 24) as usize;
        let payload = &req[NP_HDR..];
        let n = text_len.min(payload.len()).min(DATA_MAX);
        write_text(backend, &payload[..n]);
    }
    REPLY_PAYLOAD
}

// ---------------------------------------------------------------------
// Backends
// ---------------------------------------------------------------------

enum Backend {
    ByteStream,
    Framebuffer(Fb),
}

fn detect_backend() -> Backend {
    if syscall4(syscall_abi::CON_INFO, syscall_abi::CON_INFO_KIND, 0, 0, 0)
        == syscall_abi::CON_KIND_FRAMEBUFFER
    {
        let cols = syscall4(syscall_abi::CON_INFO, syscall_abi::CON_INFO_COLS, 0, 0, 0) as usize;
        let rows = syscall4(syscall_abi::CON_INFO, syscall_abi::CON_INFO_ROWS, 0, 0, 0) as usize;
        syscall4(syscall_abi::FB_CLEAR, 0, 0, 0, 0);
        Backend::Framebuffer(Fb {
            cols: cols.max(1),
            rows: rows.max(1),
            col: 0,
            row: 0,
            wrap_pending: false,
            reverse: false,
            ansi: Ansi::Ground,
            params: [0; MAX_PARAMS],
            nparams: 0,
            private: false,
        })
    } else {
        Backend::ByteStream
    }
}

fn write_text(backend: &mut Backend, bytes: &[u8]) {
    match backend {
        Backend::ByteStream => con_write_raw(bytes),
        Backend::Framebuffer(fb) => {
            for &c in bytes {
                fb.put_char(c);
            }
        }
    }
}

/// The framebuffer backend's rendering state - a character-cell cursor and
/// a small ANSI escape parser. No pixels here: it looks glyphs up in
/// `font` and drives the kernel's `FB_*` primitives.
///
/// The escape sequences it acts on are the set a full-screen program needs
/// (DevTools's Edit asked for them, `docs/handoffs/2026-10-05-from-edit-
/// editor-console.md`, item 1): `CSI row;col H`, `CSI Ps K`, `CSI Ps J`,
/// `CSI Ps m` for reverse video (7 on, 0 and 27 off), and the private
/// `CSI ? ... h/l` forms, which are parsed and drawn as nothing. Every other
/// sequence is parsed to its final byte and dropped, so it never prints.
struct Fb {
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    /// A glyph went into the last column and the cursor stayed on it; the
    /// wrap happens when the next glyph arrives, not now (VT100's deferred
    /// wrap). Without it, writing the last cell of the last row scrolls the
    /// screen, which a full-screen program's bottom line always does.
    wrap_pending: bool,
    /// SGR 7: glyphs are drawn with their bits inverted. Erasing ignores
    /// it and always blanks to the background.
    reverse: bool,
    ansi: Ansi,
    /// The CSI sequence being parsed: its numeric parameters, how many
    /// were started (`;` starts the next), and whether it had a private
    /// marker (`?`, as in `CSI ? 25 l`).
    params: [u32; MAX_PARAMS],
    nparams: usize,
    private: bool,
}

/// Parameters kept per CSI sequence; any past this are parsed and ignored.
const MAX_PARAMS: usize = 8;

/// Blank glyph bitmaps for erasing, blitted `BLANK_CELLS` at a time.
const BLANK_CELLS: usize = 64;
static BLANK: [u8; BLANK_CELLS * 8] = [0; BLANK_CELLS * 8];

enum Ansi {
    Ground,
    Esc,
    Csi,
}

impl Fb {
    fn put_char(&mut self, c: u8) {
        match self.ansi {
            Ansi::Ground => match c {
                0x1b => self.ansi = Ansi::Esc, // ESC
                b'\r' => {
                    self.col = 0;
                    self.wrap_pending = false;
                }
                b'\n' => {
                    self.newline();
                    self.wrap_pending = false;
                }
                // Backspace. With a wrap pending the cursor is still on the
                // glyph just drawn, so it only cancels the wrap: the shell's
                // `BS ' ' BS` then erases that glyph and lands back on it.
                0x08 => {
                    if self.wrap_pending {
                        self.wrap_pending = false;
                    } else {
                        self.col = self.col.saturating_sub(1);
                    }
                }
                _ => {
                    if let Some(glyph) = font::glyph(c) {
                        if self.wrap_pending {
                            self.newline();
                            self.wrap_pending = false;
                        }
                        let mut cell = *glyph;
                        if self.reverse {
                            for bits in cell.iter_mut() {
                                *bits = !*bits;
                            }
                        }
                        syscall4(
                            syscall_abi::FB_BLIT,
                            cell.as_ptr() as u64,
                            1,
                            self.col as u64,
                            self.row as u64,
                        );
                        if self.col + 1 < self.cols {
                            self.col += 1;
                        } else {
                            self.wrap_pending = true;
                        }
                    }
                    // Non-printable, non-control bytes are dropped rather
                    // than drawing junk (the kernel's old fbconsole drew a
                    // blank for them) - the shell only ever sends
                    // printable text plus \r\n\b and ANSI.
                }
            },
            Ansi::Esc => {
                // Only the CSI form (ESC [) is understood; anything else
                // ends the sequence.
                if c == b'[' {
                    self.params = [0; MAX_PARAMS];
                    self.nparams = 0;
                    self.private = false;
                    self.ansi = Ansi::Csi;
                } else {
                    self.ansi = Ansi::Ground;
                }
            }
            Ansi::Csi => match c {
                // An ESC inside a sequence abandons it and starts another.
                0x1b => self.ansi = Ansi::Esc,
                b'0'..=b'9' => {
                    if self.nparams == 0 {
                        self.nparams = 1;
                    }
                    if let Some(p) = self.params.get_mut(self.nparams - 1) {
                        *p = p.saturating_mul(10).saturating_add((c - b'0') as u32);
                    }
                }
                b';' => {
                    // An empty field before the `;` is a parameter too (0).
                    self.nparams = (self.nparams.max(1) + 1).min(MAX_PARAMS + 1);
                }
                b'<'..=b'?' => self.private = true,
                0x40..=0x7e => {
                    if !self.private {
                        self.csi(c);
                    }
                    self.ansi = Ansi::Ground;
                }
                // Intermediate bytes and C0 controls inside a sequence.
                _ => {}
            },
        }
    }

    /// Parameter `i` of the sequence just parsed, 0 when it was not given.
    fn param(&self, i: usize) -> usize {
        if i < self.nparams.min(MAX_PARAMS) {
            self.params[i] as usize
        } else {
            0
        }
    }

    /// Acts on a CSI sequence's final byte. Positions are 1-based on the
    /// wire, 0 or absent meaning 1, and clamp to the screen.
    fn csi(&mut self, final_byte: u8) {
        match final_byte {
            b'H' => {
                self.row = self.param(0).clamp(1, self.rows) - 1;
                self.col = self.param(1).clamp(1, self.cols) - 1;
                self.wrap_pending = false;
            }
            b'K' => {
                match self.param(0) {
                    0 => self.blank(self.col, self.row, self.cols - self.col),
                    1 => self.blank(0, self.row, self.col + 1),
                    2 => self.blank(0, self.row, self.cols),
                    _ => {}
                }
                self.wrap_pending = false;
            }
            b'J' => {
                // Erasing does not move the cursor; `clear` sends `CSI H`
                // after `CSI 2 J` for that.
                match self.param(0) {
                    0 => {
                        self.blank(self.col, self.row, self.cols - self.col);
                        for r in self.row + 1..self.rows {
                            self.blank(0, r, self.cols);
                        }
                    }
                    1 => {
                        for r in 0..self.row {
                            self.blank(0, r, self.cols);
                        }
                        self.blank(0, self.row, self.col + 1);
                    }
                    2 | 3 => {
                        syscall4(syscall_abi::FB_CLEAR, 0, 0, 0, 0);
                    }
                    _ => {}
                }
                self.wrap_pending = false;
            }
            b'm' => {
                if self.nparams == 0 {
                    self.reverse = false;
                }
                for i in 0..self.nparams.min(MAX_PARAMS) {
                    match self.params[i] {
                        0 | 27 => self.reverse = false,
                        7 => self.reverse = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// Blanks `n` cells of `row` from `col`, clipped to the line.
    fn blank(&self, col: usize, row: usize, n: usize) {
        let mut col = col;
        let mut left = n.min(self.cols.saturating_sub(col));
        while left > 0 {
            let run = left.min(BLANK_CELLS);
            syscall4(syscall_abi::FB_BLIT, BLANK.as_ptr() as u64, run as u64, col as u64, row as u64);
            col += run;
            left -= run;
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            // Already on the last row: scroll up one and stay put.
            syscall4(syscall_abi::FB_SCROLL, 1, 0, 0, 0);
        }
    }
}

/// Byte-stream backend: push bytes to the kernel console via the gated
/// `CON_WRITE`, chunked at the kernel's per-buffer cap.
fn con_write_raw(bytes: &[u8]) {
    let mut off = 0;
    while off < bytes.len() {
        let n = (bytes.len() - off).min(DATA_MAX);
        syscall4(syscall_abi::CON_WRITE, bytes[off..].as_ptr() as u64, n as u64, 0, 0);
        off += n;
    }
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
        buf[offset + 4],
        buf[offset + 5],
        buf[offset + 6],
        buf[offset + 7],
    ])
}

#[inline(always)]
fn syscall4(number: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "svc #0",
            inout("x0") arg0 => ret,
            in("x1") arg1,
            in("x2") arg2,
            in("x3") arg3,
            in("x8") number,
            options(nostack),
        );
    }
    ret
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}

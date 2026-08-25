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
            ansi: Ansi::Ground,
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
struct Fb {
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
    ansi: Ansi,
}

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
                b'\r' => self.col = 0,
                b'\n' => self.newline(),
                0x08 => self.col = self.col.saturating_sub(1), // backspace
                _ => {
                    if let Some(glyph) = font::glyph(c) {
                        syscall4(
                            syscall_abi::FB_BLIT,
                            glyph.as_ptr() as u64,
                            1,
                            self.col as u64,
                            self.row as u64,
                        );
                        self.col += 1;
                        if self.col >= self.cols {
                            self.newline();
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
                self.ansi = if c == b'[' { Ansi::Csi } else { Ansi::Ground };
            }
            Ansi::Csi => {
                // Collect (and ignore) parameter bytes until the final
                // letter. Handle the two the shell's `clear` uses; ignore
                // the rest. This is what finally makes `clear` actually
                // clear on a framebuffer (the kernel's fbconsole never
                // parsed ANSI - see its module doc comment).
                if (0x40..=0x7e).contains(&c) {
                    match c {
                        b'J' => {
                            syscall4(syscall_abi::FB_CLEAR, 0, 0, 0, 0);
                            self.col = 0;
                            self.row = 0;
                        }
                        b'H' => {
                            self.col = 0;
                            self.row = 0;
                        }
                        _ => {}
                    }
                    self.ansi = Ansi::Ground;
                }
            }
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

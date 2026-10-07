//! Swallows the escape sequences a keyboard sends, so the shell's line
//! editor and login's prompts take an arrow key as nothing rather than as
//! the text `[A`.
//!
//! The USB keyboard sends VT100/xterm sequences for the navigation and
//! function keys (`kernel/src/xhci.rs`, `keycode_to_bytes`): CSI forms such
//! as `ESC [ A` and `ESC [ 5 ~`, and SS3 forms such as `ESC O Q`. A host
//! terminal on QEMU's serial line sends the same. Neither line editor moves
//! a cursor or recalls history, so every sequence is dropped whole: the ESC,
//! its parameters and its final byte.
//!
//! One cost: a bare ESC (the Escape key on a host terminal; the USB keyboard
//! sends none) swallows the byte typed after it.

/// Where the filter is in a sequence.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    /// After `ESC [`: parameter and intermediate bytes until a final byte.
    Csi,
    /// After `ESC O`: one more byte ends it.
    Ss3,
}

pub struct KeySeq {
    state: State,
}

impl KeySeq {
    pub const fn new() -> Self {
        KeySeq { state: State::Ground }
    }

    /// Whether `b` is an ordinary byte for the line editor, false while it
    /// is part of an escape sequence.
    pub fn passes(&mut self, b: u8) -> bool {
        match self.state {
            State::Ground => {
                if b == 0x1b {
                    self.state = State::Esc;
                    return false;
                }
                true
            }
            State::Esc => {
                self.state = match b {
                    b'[' => State::Csi,
                    b'O' => State::Ss3,
                    0x1b => State::Esc,
                    _ => State::Ground,
                };
                false
            }
            State::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    self.state = State::Ground;
                }
                false
            }
            State::Ss3 => {
                self.state = State::Ground;
                false
            }
        }
    }
}

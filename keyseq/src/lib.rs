//! Tells a key's escape sequence apart from ordinary input, for every
//! keyboard reader in userland: the shell's line editor and login's prompts,
//! `ulib::read_line` (`passwd`, `useradd`) and `ulib::read_key` (`more`).
//! One filter for all of them, because two readers that drop a sequence
//! differently disagree about what was typed: a password set through one and
//! typed at the other would never match.
//!
//! The USB keyboard sends VT100/xterm sequences for the navigation and
//! function keys (`kernel/src/xhci.rs`, `keycode_to_bytes`): CSI forms such
//! as `ESC [ A` and `ESC [ 5 ~`, and SS3 forms such as `ESC O Q`. A host
//! terminal on QEMU's serial line sends the same.
//!
//! A control byte (below 0x20, or DEL) arriving inside a sequence ends it
//! and is passed through, as ECMA-48 has a terminal execute it: a sequence
//! cut short (a byte lost, a bare ESC from a host terminal's Escape key)
//! then cannot swallow the Enter or Backspace typed after it. Any other byte
//! after a bare ESC, save `[` and `O`, which start a sequence, is that byte
//! (since 2026-10-09, decided by Hans with the USB Escape key): the Escape
//! key is a key a line editor ignores, and the letter typed after it is
//! kept, not taken for an `ESC x` sequence. The cost is stated: a host
//! terminal's Alt+x (`ESC x`) types `x`, and an `ESC` sequence of another
//! shape (`ESC P`, `ESC ]`) leaves its tail as text. Esc then a typed `[`
//! or `O` is still read as a sequence's start, as on any terminal.
//!
//! Pure: no I/O, no syscalls, no heap, so it builds and is tested on the
//! host (`make test`).

#![no_std]

mod queue;
pub use queue::{KeyQueue, Source, QUEUE_LEN};

/// What one byte of input turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fed {
    /// An ordinary byte, for the reader to act on.
    Byte(u8),
    /// Inside a sequence: read on.
    Pending,
    /// The last byte of a sequence: one key, which a line editor ignores.
    Sequence,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Esc,
    /// After `ESC [`: parameter and intermediate bytes until a final byte.
    Csi,
    /// After `ESC O`: one more byte ends it.
    Ss3,
}

pub(crate) const ESC: u8 = 0x1b;
const DEL: u8 = 0x7f;

pub struct KeySeq {
    state: State,
}

impl Default for KeySeq {
    fn default() -> Self {
        Self::new()
    }
}

impl KeySeq {
    pub const fn new() -> Self {
        KeySeq { state: State::Ground }
    }

    pub fn feed(&mut self, b: u8) -> Fed {
        if self.state == State::Ground {
            if b == ESC {
                self.state = State::Esc;
                return Fed::Pending;
            }
            return Fed::Byte(b);
        }
        if b == ESC {
            // A new sequence abandons the one in progress.
            self.state = State::Esc;
            return Fed::Pending;
        }
        if b < 0x20 || b == DEL {
            self.state = State::Ground;
            return Fed::Byte(b);
        }
        match self.state {
            State::Esc => match b {
                b'[' => {
                    self.state = State::Csi;
                    Fed::Pending
                }
                b'O' => {
                    self.state = State::Ss3;
                    Fed::Pending
                }
                // The Escape key, then a key of its own (see the module doc).
                _ => {
                    self.state = State::Ground;
                    Fed::Byte(b)
                }
            },
            State::Csi => {
                if (0x40..=0x7e).contains(&b) {
                    self.state = State::Ground;
                    Fed::Sequence
                } else {
                    Fed::Pending
                }
            }
            State::Ss3 => {
                self.state = State::Ground;
                Fed::Sequence
            }
            // Handled before the match; kept total rather than a panic path.
            State::Ground => Fed::Byte(b),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// The bytes a reader acts on, and how many sequences it saw.
    fn run(input: &[u8]) -> (Vec<u8>, usize) {
        let mut k = KeySeq::new();
        let mut bytes = Vec::new();
        let mut seqs = 0;
        for &b in input {
            match k.feed(b) {
                Fed::Byte(x) => bytes.push(x),
                Fed::Sequence => seqs += 1,
                Fed::Pending => {}
            }
        }
        (bytes, seqs)
    }

    #[test]
    fn every_key_the_keyboard_sends_is_one_sequence() {
        for seq in [
            &b"\x1b[A"[..], b"\x1b[B", b"\x1b[C", b"\x1b[D", b"\x1b[H", b"\x1b[F",
            b"\x1b[5~", b"\x1b[6~", b"\x1b[3~", b"\x1bOQ", b"\x1bOR",
        ] {
            assert_eq!(run(seq), (Vec::new(), 1), "{seq:?}");
        }
    }

    #[test]
    fn ordinary_text_passes_untouched() {
        let text = b"echo hello [world] O~\r";
        assert_eq!(run(text), (text.to_vec(), 0));
    }

    #[test]
    fn sequences_inside_text_vanish_whole() {
        assert_eq!(run(b"pa\x1b[Dss\x1b[5~\r"), (b"pass\r".to_vec(), 2));
        assert_eq!(run(b"ro\x1bOQot"), (b"root".to_vec(), 1));
    }

    #[test]
    fn a_control_byte_ends_a_cut_sequence_and_passes() {
        // The final byte was lost: Enter must still submit, and the next
        // letter must not be eaten.
        assert_eq!(run(b"ls\x1b[\rls"), (b"ls\rls".to_vec(), 0));
        assert_eq!(run(b"a\x1b[5\x08b"), (b"a\x08b".to_vec(), 0));
        assert_eq!(run(b"a\x1bO\x7fb"), (b"a\x7fb".to_vec(), 0));
        assert_eq!(run(b"a\x1b\rb"), (b"a\rb".to_vec(), 0));
    }

    #[test]
    fn a_new_escape_abandons_the_sequence_in_progress() {
        assert_eq!(run(b"\x1b[5\x1b[Ax"), (b"x".to_vec(), 1));
        assert_eq!(run(b"\x1b\x1b[Bx"), (b"x".to_vec(), 1));
    }

    #[test]
    fn a_bare_escape_keeps_the_byte_after_it() {
        assert_eq!(run(b"\x1bxy"), (b"xy".to_vec(), 0));
        assert_eq!(run(b"\x1bq"), (b"q".to_vec(), 0), "Esc then q at `more` quits");
        assert_eq!(run(b"\x1b\x1bx"), (b"x".to_vec(), 0));
    }

    #[test]
    fn a_bare_escape_then_bracket_or_o_still_starts_a_sequence() {
        assert_eq!(run(b"\x1b[Ax"), (b"x".to_vec(), 1));
        assert_eq!(run(b"\x1bOQx"), (b"x".to_vec(), 1));
    }
}

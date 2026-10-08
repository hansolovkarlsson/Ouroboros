//! The kernel's keyboard queue, here rather than in `kernel/src/syscall.rs`
//! so that it is tested on the host: it is pure logic once the time is
//! passed in, and the cases that matter (a key arriving within a tick of an
//! Escape, a full queue, a reader holding half a key when the keyboard
//! changes hands) cannot be timed from a QEMU rig. The kernel holds one,
//! `KBD_QUEUE`, and feeds it every byte read from a keyboard
//! (`docs/roadmap/roadmap-ctrl-c.md`, step 3).

use crate::{Fed, KeySeq};

/// How many bytes the queue holds: a burst of typing while a program is
/// busy, 16 navigation keys.
pub const QUEUE_LEN: usize = 64;

/// Keyboard bytes read ahead, oldest first, for whoever owns the keyboard
/// when they are read; every byte a reader gets passes through it. It knows
/// where each key starts, from one parser that sees every byte once, and
/// keeps keys whole: a key that does not fit is dropped entirely, its front
/// taken back out, rather than kept cut; and a key a reader took part of is
/// not handed to the next owner ([`KeyQueue::trim_cut_key`]).
///
/// Times are in the caller's ticks: `esc_alone` is how many of them a bare
/// `ESC` waits alone before it counts as a complete key (decision E2 of the
/// Ctrl-C plan), and every call that can end a key takes `now`.
pub struct KeyQueue {
    buf: [u8; QUEUE_LEN],
    /// Per position: the byte starts a key (the parser was between keys).
    starts: [bool; QUEUE_LEN],
    head: usize,
    len: usize,
    /// Every byte read, parsed as keys.
    keys: KeySeq,
    /// The parser is inside a key: the last byte fed did not end one.
    inside: bool,
    /// The key the parser is inside came from the USB keyboard.
    inside_usb: bool,
    /// The time the last byte was fed, for `esc_alone`.
    fed_at: u64,
    esc_alone: u64,
    /// How many bytes at the back belong to the key still arriving.
    partial: usize,
    /// The rest of the key arriving is dropped: its front did not fit, or a
    /// reader that took part of it lost the keyboard.
    dropping: bool,
    /// A reader took part of a key: the last byte popped did not end one.
    taken_mid: bool,
}

impl KeyQueue {
    pub const fn new(esc_alone: u64) -> Self {
        KeyQueue {
            buf: [0; QUEUE_LEN],
            starts: [false; QUEUE_LEN],
            head: 0,
            len: 0,
            keys: KeySeq::new(),
            inside: false,
            inside_usb: false,
            fed_at: 0,
            esc_alone,
            partial: 0,
            dropping: false,
            taken_mid: false,
        }
    }

    /// Whether the next byte must come from the USB keyboard: the parser is
    /// inside a key that began there, and a report's bytes wait whole, so
    /// reading its rest first keeps a serial byte out of it.
    pub fn inside_usb_key(&self) -> bool {
        self.inside && self.inside_usb
    }

    /// A key left open for `esc_alone` (a bare `ESC`, from a host terminal's
    /// Escape key) is complete: its rest is not coming, and what arrives
    /// next starts a key of its own.
    fn expire(&mut self, now: u64) {
        if self.inside && now.saturating_sub(self.fed_at) >= self.esc_alone {
            self.keys = KeySeq::new();
            self.inside = false;
            self.partial = 0;
            self.dropping = false;
            if self.len == 0 {
                self.taken_mid = false;
            }
        }
    }

    /// Appends `byte`, read at `now`, from the USB keyboard if `from_usb`.
    /// When it does not fit, an ordinary byte is dropped, and a byte of a
    /// key's sequence drops the whole key: the bytes of it already queued are
    /// taken back out, and the rest is dropped as it comes.
    pub fn push(&mut self, byte: u8, from_usb: bool, now: u64) {
        self.expire(now);
        let starts = !self.inside;
        let fed = self.keys.feed(byte);
        self.inside = fed == Fed::Pending;
        if starts {
            self.inside_usb = from_usb;
        }
        self.fed_at = now;
        if self.dropping {
            match fed {
                Fed::Pending => return,
                Fed::Sequence => {
                    self.dropping = false;
                    return;
                }
                // A control byte ends a cut sequence and is a key of its own
                // (keyseq passes it through the same way): kept, if it fits.
                Fed::Byte(_) => self.dropping = false,
            }
        } else if self.len == QUEUE_LEN {
            // Taken back only as far as it is still queued: if a reader has
            // already read the front of this key, it holds a cut key, which
            // a full queue cannot prevent (it has no room for the rest).
            self.len -= self.partial.min(self.len);
            self.partial = 0;
            match fed {
                Fed::Pending => {
                    self.dropping = true;
                    return;
                }
                Fed::Sequence => return,
                // A control byte ending the key that did not fit, or an
                // ordinary byte: kept if taking the key back made room.
                Fed::Byte(_) => {}
            }
        }
        if self.len == QUEUE_LEN {
            return;
        }
        let at = (self.head + self.len) % QUEUE_LEN;
        self.buf[at] = byte;
        // A control byte that ended a cut sequence starts a key of its own.
        self.starts[at] = starts || matches!(fed, Fed::Byte(_));
        self.len += 1;
        self.partial = if fed == Fed::Pending { self.partial + 1 } else { 0 };
    }

    /// The one flush, at every interrupt (`Some(byte)`, the interrupt key)
    /// and on request (`None`, `TCSAFLUSH`). An interrupt key is fed to the
    /// parser first: a control byte ends any key in progress, as `keyseq`
    /// has it, so nothing typed after it is taken for that key's rest
    /// (decision E1 of the Ctrl-C plan; until 2026-10-08 the kill's flush
    /// dropped the next byte, and a bare Esc then Ctrl+C ate a letter). A
    /// requested flush has no such byte: a key still arriving is dropped as
    /// it comes, since its front was discarded.
    pub fn flush(&mut self, interrupt: Option<u8>) {
        if let Some(byte) = interrupt {
            let _ = self.keys.feed(byte);
            self.inside = false;
            self.dropping = false;
        } else if self.inside {
            self.dropping = true;
        }
        self.head = 0;
        self.len = 0;
        self.partial = 0;
        self.taken_mid = false;
    }

    /// For a change of keyboard owner at `now`: if a reader took part of a
    /// key, the rest of it goes, the queued bytes up to the next key's start
    /// and, if the key is still arriving, the bytes still to come. Complete
    /// keys typed ahead stay (decision E3). A bare `ESC` left alone long
    /// enough is a whole key first, so a letter typed after one is never
    /// dropped.
    pub fn trim_cut_key(&mut self, now: u64) {
        self.expire(now);
        if !self.taken_mid {
            return;
        }
        while self.len > 0 && !self.starts[self.head] {
            self.head = (self.head + 1) % QUEUE_LEN;
            self.len -= 1;
        }
        if self.len == 0 && self.inside {
            self.dropping = true;
        }
        self.partial = self.partial.min(self.len);
        self.taken_mid = false;
    }

    /// The oldest byte, for a reader.
    pub fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.buf[self.head];
        let in_partial = self.len <= self.partial;
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        // The reader holds part of a key when what follows is the same key:
        // a queued byte that does not start one, or, with nothing queued,
        // the rest of the key still arriving.
        self.taken_mid = if self.len > 0 { !self.starts[self.head] } else { in_partial };
        self.partial = self.partial.min(self.len);
        Some(byte)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const ESC_ALONE: u64 = 2;

    fn push_all(q: &mut KeyQueue, bytes: &[u8], now: u64) {
        for &b in bytes {
            q.push(b, false, now);
        }
    }

    fn drain(q: &mut KeyQueue) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some(b) = q.pop() {
            out.push(b);
        }
        out
    }

    #[test]
    fn bytes_come_out_in_order() {
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"ab\x1b[Ac", 0);
        assert_eq!(drain(&mut q), b"ab\x1b[Ac");
    }

    #[test]
    fn a_cut_key_is_not_handed_to_the_next_owner() {
        // E3: a reader took ESC of an arrow, then lost the keyboard.
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"\x1b[Ax", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        assert_eq!(drain(&mut q), b"x", "the rest of the arrow goes, the next key stays");
    }

    #[test]
    fn a_key_still_arriving_is_dropped_as_it_comes() {
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        push_all(&mut q, b"[Ay", 1);
        assert_eq!(drain(&mut q), b"y");
    }

    #[test]
    fn complete_keys_typed_ahead_stay() {
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"ab\x1b[A", 0);
        assert_eq!(q.pop(), Some(b'a'));
        q.trim_cut_key(0);
        assert_eq!(drain(&mut q), b"b\x1b[A");
    }

    #[test]
    fn a_bare_escape_ends_after_the_interval() {
        // E2: a program read a bare ESC and exited; a letter typed later
        // starts a key of its own and is kept.
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(ESC_ALONE);
        push_all(&mut q, b"echo", ESC_ALONE);
        assert_eq!(drain(&mut q), b"echo");
    }

    #[test]
    fn within_the_interval_the_escape_is_still_open() {
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(ESC_ALONE - 1);
        push_all(&mut q, b"[A", ESC_ALONE - 1);
        assert_eq!(drain(&mut q), b"", "the arrow's rest, inside the interval, is dropped");
    }

    #[test]
    fn nothing_typed_after_an_interrupt_is_eaten() {
        // E1: ESC queued, then Ctrl+C acted on, then a letter at once,
        // inside the interval, so only the flush's own rule can save it.
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"\x1b", 0);
        q.flush(Some(0x03));
        push_all(&mut q, b"e", 0);
        assert_eq!(drain(&mut q), b"e");
    }

    #[test]
    fn a_requested_flush_drops_the_rest_of_a_key_in_flight() {
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"ab\x1b[", 0);
        q.flush(None);
        push_all(&mut q, b"Az", 0);
        assert_eq!(drain(&mut q), b"z");
    }

    #[test]
    fn a_requested_flush_between_keys_keeps_what_comes_after() {
        let mut q = KeyQueue::new(ESC_ALONE);
        push_all(&mut q, b"ab", 0);
        q.flush(None);
        push_all(&mut q, b"z", 0);
        assert_eq!(drain(&mut q), b"z");
    }

    #[test]
    fn a_full_queue_drops_a_whole_key() {
        let mut q = KeyQueue::new(ESC_ALONE);
        let letters = [b'x'; QUEUE_LEN - 1];
        push_all(&mut q, &letters, 0);
        push_all(&mut q, b"\x1b[Ab", 0);
        let mut want = letters.to_vec();
        want.push(b'b');
        assert_eq!(drain(&mut q), want, "nothing of the arrow, the letter after it kept");
    }

    #[test]
    fn a_usb_key_is_read_whole() {
        let mut q = KeyQueue::new(ESC_ALONE);
        q.push(0x1b, true, 0);
        assert!(q.inside_usb_key());
        q.push(b'[', true, 0);
        q.push(b'A', true, 0);
        assert!(!q.inside_usb_key());
        q.push(0x1b, false, 0);
        assert!(!q.inside_usb_key(), "a serial key does not pin the reads to USB");
    }
}

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

/// Where a byte came from, for [`KeyQueue::push`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// The serial line: a key's end is known only from its bytes, or, for a
    /// bare `ESC`, from the quiet after it (`esc_alone`).
    Serial,
    /// The USB keyboard, which sends each key whole, so the driver knows
    /// where it ends: `last` marks the last byte of a key. The Escape key is
    /// a bare `ESC` with `last` set, a complete key at once, where the
    /// parser alone would wait for the rest of a sequence.
    Usb { last: bool },
}

/// Keyboard bytes read ahead, oldest first, for whoever owns the keyboard
/// when they are read; every byte a reader gets passes through it. It knows
/// where each key starts, from one parser that sees every byte once, and
/// keeps keys whole: a key that does not fit is dropped entirely, its front
/// taken back out, rather than kept cut; and a key a reader took part of is
/// not handed to the next owner ([`KeyQueue::trim_cut_key`]).
///
/// Times are in the caller's ticks: a bare `ESC` with nothing after it for
/// MORE than `esc_alone` of them counts as a complete key (decision E2 of the
/// Ctrl-C plan), and every call that can end a key takes `now`. More than,
/// not at least: ticks are whole, so `esc_alone` ticks apart can be as little
/// as `esc_alone - 1` ticks of real time, and the interval is a floor.
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
    usb_alone: u64,
    /// How many bytes at the back belong to the key still arriving.
    partial: usize,
    /// The rest of the key arriving is dropped: its front did not fit, or a
    /// reader that took part of it lost the keyboard.
    dropping: bool,
    /// A reader took part of a key: the last byte popped did not end one.
    taken_mid: bool,
}

impl KeyQueue {
    pub const fn new(esc_alone: u64, usb_alone: u64) -> Self {
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
            usb_alone,
            partial: 0,
            dropping: false,
            taken_mid: false,
        }
    }

    /// Whether the next byte must come from the USB keyboard: the parser is
    /// inside a key that began there, and a report's bytes wait whole, so
    /// reading its rest first keeps a serial byte out of it.
    pub fn inside_usb_key(&mut self, now: u64) -> bool {
        self.expire(now);
        self.inside && self.inside_usb
    }

    /// Whether the next byte must come from the serial line: the parser is
    /// inside a key that began there and has not expired, so a USB byte read
    /// now would land inside it.
    pub fn inside_serial_key(&mut self, now: u64) -> bool {
        self.expire(now);
        self.inside && !self.inside_usb
    }

    /// A key from the serial line left open for more than `esc_alone` (a bare
    /// `ESC`, from a host terminal's Escape key) is complete: its rest is not
    /// coming, and what arrives next starts a key of its own. A USB key is
    /// not held to that interval: a report's bytes wait whole in the driver,
    /// so its rest comes however late it is read, and expiring it then would
    /// let that rest through as text after a trim (the third high review of
    /// #238).
    ///
    /// A USB key has a bound of its own, `usb_alone`, far longer: only a
    /// keyboard gone mid-report leaves one open that long, and without a
    /// bound it would hold the queue open for good.
    fn expire(&mut self, now: u64) {
        let alone = if self.inside_usb { self.usb_alone } else { self.esc_alone };
        if self.inside && now.saturating_sub(self.fed_at) > alone {
            self.keys = KeySeq::new();
            self.inside = false;
            self.partial = 0;
            self.dropping = false;
            if self.len == 0 {
                self.taken_mid = false;
            }
        }
    }

    /// Appends `byte`, read at `now` from `from`.
    /// When it does not fit, an ordinary byte is dropped, and a byte of a
    /// key's sequence drops the whole key: the bytes of it already queued are
    /// taken back out, and the rest is dropped as it comes. The last byte of
    /// a USB key ends the key whatever the parser makes of it, so a USB
    /// Escape is complete at once and the key typed after it is a key of its
    /// own, kept on a change of owner (without it the Escape held the parser
    /// open for `usb_alone`, and a letter typed within it was trimmed as the
    /// Escape's rest).
    pub fn push(&mut self, byte: u8, from: Source, now: u64) {
        let from_usb = matches!(from, Source::Usb { .. });
        self.push_byte(byte, from_usb, now);
        if from == (Source::Usb { last: true }) && self.inside && self.inside_usb {
            self.keys = KeySeq::new();
            self.inside = false;
            self.partial = 0;
            self.dropping = false;
        }
    }

    fn push_byte(&mut self, byte: u8, from_usb: bool, now: u64) {
        self.expire(now);
        // An ESC inside a key starts a new one: keyseq abandons the sequence
        // in progress (review of #238; it was marked as the old key's rest,
        // so a trim dropped a complete key typed ahead behind it).
        let abandons = self.inside && byte == crate::ESC;
        let starts = !self.inside || abandons;
        let fed = self.keys.feed(byte);
        self.inside = fed == Fed::Pending;
        if starts {
            self.inside_usb = from_usb;
        }
        self.fed_at = now;
        if self.dropping && abandons {
            // A new key, not the dropped key's rest: the drop ends here and
            // the new key is queued (second high review of #238).
            self.dropping = false;
        }
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
                // An ESC that abandoned the key it cut short: taking that key
                // back made room, and this ESC starts a new key that fits
                // (fourth high review of #238).
                Fed::Pending if abandons && self.len < QUEUE_LEN => {}
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
        self.partial = match fed {
            Fed::Pending if abandons => 1,
            Fed::Pending => self.partial + 1,
            _ => 0,
        };
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

    /// Whether no byte is queued.
    pub fn is_empty(&self) -> bool {
        self.len == 0
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
    const USB_ALONE: u64 = 50;
    /// A USB key's bytes: every one but its last, and its last.
    const MID: Source = Source::Usb { last: false };
    const LAST: Source = Source::Usb { last: true };

    fn push_all(q: &mut KeyQueue, bytes: &[u8], now: u64) {
        for &b in bytes {
            q.push(b, Source::Serial, now);
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
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"ab\x1b[Ac", 0);
        assert_eq!(drain(&mut q), b"ab\x1b[Ac");
    }

    #[test]
    fn a_cut_key_is_not_handed_to_the_next_owner() {
        // E3: a reader took ESC of an arrow, then lost the keyboard.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b[Ax", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        assert_eq!(drain(&mut q), b"x", "the rest of the arrow goes, the next key stays");
    }

    #[test]
    fn a_key_still_arriving_is_dropped_as_it_comes() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        push_all(&mut q, b"[Ay", 1);
        assert_eq!(drain(&mut q), b"y");
    }

    #[test]
    fn complete_keys_typed_ahead_stay() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"ab\x1b[A", 0);
        assert_eq!(q.pop(), Some(b'a'));
        q.trim_cut_key(0);
        assert_eq!(drain(&mut q), b"b\x1b[A");
    }

    #[test]
    fn a_bare_escape_ends_after_the_interval() {
        // E2: a program read a bare ESC and exited; a letter typed later
        // starts a key of its own and is kept.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(ESC_ALONE + 1);
        push_all(&mut q, b"echo", ESC_ALONE + 1);
        assert_eq!(drain(&mut q), b"echo");
    }

    #[test]
    fn within_the_interval_the_escape_is_still_open() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(ESC_ALONE);
        push_all(&mut q, b"[A", ESC_ALONE);
        assert_eq!(drain(&mut q), b"", "the arrow's rest, inside the interval, is dropped");
    }

    #[test]
    fn nothing_typed_after_an_interrupt_is_eaten() {
        // E1: ESC queued, then Ctrl+C acted on, then a letter at once,
        // inside the interval, so only the flush's own rule can save it.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b", 0);
        q.flush(Some(0x03));
        push_all(&mut q, b"e", 0);
        assert_eq!(drain(&mut q), b"e");
    }

    #[test]
    fn a_requested_flush_drops_the_rest_of_a_key_in_flight() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"ab\x1b[", 0);
        q.flush(None);
        push_all(&mut q, b"Az", 0);
        assert_eq!(drain(&mut q), b"z");
    }

    #[test]
    fn a_requested_flush_between_keys_keeps_what_comes_after() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"ab", 0);
        q.flush(None);
        push_all(&mut q, b"z", 0);
        assert_eq!(drain(&mut q), b"z");
    }

    #[test]
    fn a_full_queue_drops_a_whole_key() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        let letters = [b'x'; QUEUE_LEN - 1];
        push_all(&mut q, &letters, 0);
        push_all(&mut q, b"\x1b[Ab", 0);
        let mut want = letters.to_vec();
        want.push(b'b');
        assert_eq!(drain(&mut q), want, "nothing of the arrow, the letter after it kept");
    }

    #[test]
    fn a_serial_key_holds_the_usb_keyboard_back() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        q.push(0x1b, Source::Serial, 0);
        assert!(q.inside_serial_key(0));
        assert!(!q.inside_serial_key(ESC_ALONE + 1), "only for the interval");
    }

    #[test]
    fn a_usb_key_is_read_whole() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        q.push(0x1b, MID, 0);
        assert!(q.inside_usb_key(0));
        q.push(b'[', MID, 0);
        q.push(b'A', LAST, 0);
        assert!(!q.inside_usb_key(0));
        q.push(0x1b, Source::Serial, 0);
        assert!(!q.inside_usb_key(0), "a serial key does not pin the reads to USB");
    }

    #[test]
    fn a_usb_key_outlasts_the_escape_interval() {
        // A program took ESC of a USB arrow and exited; the shell reads the
        // rest long after the interval. It is still the arrow's rest.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        q.push(0x1b, MID, 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(USB_ALONE);
        assert!(q.inside_usb_key(USB_ALONE), "the rest is still read from USB first");
        q.push(b'[', MID, USB_ALONE);
        q.push(b'A', LAST, USB_ALONE);
        q.push(b'x', LAST, USB_ALONE);
        assert_eq!(drain(&mut q), b"x");
    }

    #[test]
    fn a_new_key_ends_a_drop() {
        // A program read a bare ESC and lost the keyboard inside the
        // interval, so the ESC's rest is being dropped; an arrow typed then
        // is a key of its own and is kept.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        push_all(&mut q, b"\x1b[A", 1);
        assert_eq!(drain(&mut q), b"\x1b[A");
    }

    #[test]
    fn a_usb_key_whose_rest_never_comes_is_given_up() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        q.push(0x1b, MID, 0);
        assert!(q.inside_usb_key(USB_ALONE));
        assert!(!q.inside_usb_key(USB_ALONE + 1));
    }

    #[test]
    fn a_full_queue_keeps_a_new_key_that_abandons_a_cut_one() {
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        // Full with a three-byte cut key at the back, so the arrow fits
        // exactly once that key is taken back out.
        let letters = [b'x'; QUEUE_LEN - 3];
        push_all(&mut q, &letters, 0);
        push_all(&mut q, b"\x1b[5", 0);
        push_all(&mut q, b"\x1b[A", 0);
        let mut want = letters.to_vec();
        want.extend_from_slice(b"\x1b[A");
        assert_eq!(drain(&mut q), want, "the cut key gone, the arrow after it kept");
    }

    #[test]
    fn an_escape_inside_a_key_starts_a_new_one() {
        // A cut sequence, then a whole arrow behind it: a reader takes the
        // first ESC and loses the keyboard; the cut key goes, the arrow stays.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        push_all(&mut q, b"\x1b[5\x1b[Ax", 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        assert_eq!(drain(&mut q), b"\x1b[Ax");
    }

    #[test]
    fn a_usb_escape_is_a_whole_key_at_once() {
        // A program read the Escape key and exited; a letter typed within
        // `usb_alone` is a key of its own, not the Escape's rest.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        q.push(0x1b, LAST, 0);
        assert!(!q.inside_usb_key(0), "nothing more is read for it");
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(1);
        q.push(b'e', LAST, 1);
        assert_eq!(drain(&mut q), b"e");
    }

    #[test]
    fn a_usb_escape_then_a_letter_are_two_keys() {
        // Both queued before a change of owner: the reader took the Escape,
        // and the letter behind it starts a key, so the trim keeps it.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        q.push(0x1b, LAST, 0);
        q.push(b'e', LAST, 0);
        assert_eq!(q.pop(), Some(0x1b));
        q.trim_cut_key(0);
        assert_eq!(drain(&mut q), b"e");
    }

    #[test]
    fn a_usb_escape_ends_a_drop() {
        // The front of a USB key did not fit, so its rest was being dropped;
        // its last byte ends the drop, and the next key is kept.
        let mut q = KeyQueue::new(ESC_ALONE, USB_ALONE);
        let letters = [b'x'; QUEUE_LEN];
        push_all(&mut q, &letters, 0);
        q.push(0x1b, LAST, 0);
        assert_eq!(drain(&mut q), letters.to_vec(), "no room for the Escape");
        q.push(b'e', LAST, 0);
        assert_eq!(drain(&mut q), b"e");
    }
}

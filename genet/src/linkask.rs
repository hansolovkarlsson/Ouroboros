//! When the reader's wait asks the PHY for the link, and what an answer
//! means ([`change`]): the rule the kernel's `Device::link_at_wait`
//! follows, here so it runs on the host against a clock the tests step.
//! The kernel lends the time and does the ask; this says when to ask, how
//! long the wait may block before the next one, and when to slow down.
//!
//! - **Once an [`ASK_INTERVAL_US`]**, measured on the caller's microsecond
//!   clock, and the wait is told the time left until the next ask, not a
//!   whole interval. The kernel's wait ends on a 20 ms tick, so a reader
//!   told "a second" can come back a tick short of one; told a whole
//!   interval again then, it asked about every two seconds (the review of
//!   the late-cable branch, 2026-11-17).
//! - **Backed off after [`FAILED_ASKS_MAX`] asks running whose reads
//!   failed**, to once a [`BACKOFF_INTERVAL_US`] (a minute), so a stuck
//!   MDIO bus (20 ms a read, up to three reads an ask, under the kernel
//!   lock) is not read once a second for good. One failure is not enough:
//!   the boot's own wait allows as many stuck reads running
//!   (`STUCK_READS`), and a single timeout that slowed the asking would
//!   leave the board slow to hear a late cable. An ask that read, up or
//!   down, starts the count again and the pace with it. **Never stopped**
//!   (Hans's choice, the review of 2026-11-18): until then three failures
//!   stopped the asking for good, and with the MAC on a cable pulled after
//!   a bus that went quiet for three seconds was never noticed.
//! - **Asked for as long as the device lives**, the MAC on or off, since
//!   2026-11-18: a link that drops and returns is read again ([`change`]
//!   says what an answer means). Before, the asking ended at the start,
//!   and a cable pulled and put back, or moved to a switch at another
//!   speed, left the MAC at the old one with nothing passing.
//!
//! Linux's driver falls back to polling the PHY on GENET v5, whose link
//! interrupt is unreliable (`bcmmii.c`, "On GENETv5 there is a hardware
//! issue that prevents the signaling of link UP interrupts"). The interval
//! here is this driver's choice.

use crate::phy::{Link, Speed};

/// How often the wait asks the link: once a second.
pub const ASK_INTERVAL_US: u64 = 1_000_000;

/// How many asks running may fail to read before the wait backs off.
pub const FAILED_ASKS_MAX: u32 = 3;

/// How often the wait asks once it has backed off: once a minute.
pub const BACKOFF_INTERVAL_US: u64 = 60_000_000;

/// What the wait is to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Ask the link, then tell [`LinkAsk::answered`] whether it read.
    Now,
    /// Not yet: block at most this many microseconds and come back. Never
    /// 0, so a wait always blocks for something.
    Later { in_us: u64 },
}

/// What an answer changed in the asking, for the caller's one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answered {
    /// The pace is as it was.
    Same,
    /// This failure is the [`FAILED_ASKS_MAX`]th running: the wait now
    /// asks once a [`BACKOFF_INTERVAL_US`].
    BackedOff,
    /// A read after the back-off: the wait asks once a second again.
    Back,
}

/// The wait's asking: when it last asked, and how many asks running
/// failed to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkAsk {
    last_us: Option<u64>,
    failed: u32,
}

impl Default for LinkAsk {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkAsk {
    /// Nothing asked yet: the first wait asks at once.
    pub const fn new() -> Self {
        LinkAsk { last_us: None, failed: 0 }
    }

    /// The time between asks as things stand: a second, or a minute once
    /// [`FAILED_ASKS_MAX`] asks running have failed.
    pub fn interval_us(&self) -> u64 {
        if self.failed >= FAILED_ASKS_MAX { BACKOFF_INTERVAL_US } else { ASK_INTERVAL_US }
    }

    /// What the wait at `now_us` is to do. An [`Ask::Now`] is taken as the
    /// ask made at `now_us`, and the next is due an interval later.
    pub fn at_wait(&mut self, now_us: u64) -> Ask {
        let interval = self.interval_us();
        match self.last_us {
            // A clock read lower than the last ask is no time at all, not
            // a wrap: `saturating_sub` leaves it to wait the interval out.
            Some(at) if now_us.saturating_sub(at) < interval => Ask::Later {
                in_us: interval - now_us.saturating_sub(at),
            },
            _ => {
                self.last_us = Some(now_us);
                Ask::Now
            }
        }
    }

    /// After an [`Ask::Now`]: whether the ask's reads answered (a link up
    /// or down is an answer).
    pub fn answered(&mut self, read: bool) -> Answered {
        if read {
            let was_backed_off = self.failed >= FAILED_ASKS_MAX;
            self.failed = 0;
            return if was_backed_off { Answered::Back } else { Answered::Same };
        }
        self.failed = self.failed.saturating_add(1);
        if self.failed == FAILED_ASKS_MAX { Answered::BackedOff } else { Answered::Same }
    }
}

/// What a link read means for a device whose link, as last read, is
/// `held`: the speed and duplex the MAC was last started on while the
/// link is up, `None` while it is not (or never was).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Nothing to do: the link is as held.
    Same,
    /// Up, where it was not, or up at another speed or duplex: start the
    /// MAC at this one (`Genet::start` again rewrites the speed and keeps
    /// the enables).
    Up { speed: Speed, full: bool },
    /// Was up and is not: refuse sends until it is up again.
    Down,
    /// Up at the speed held, at another duplex. The MAC is not told the
    /// duplex (`mac.rs`'s module doc; U-Boot does not tell it either), so
    /// nothing is started: only the link held changes, and what pause
    /// the link agrees, which turns on the duplex.
    Duplex { full: bool },
}

/// The rule, for every ask (the wait's and a send's). A link that is
/// negotiating, or up at a speed the PHY does not name, is not one the
/// MAC can run on, so it counts as down. A change of speed seen with no
/// down between (a cable moved inside one interval) is an [`Change::Up`]
/// at the new speed: the MAC is never left at a speed the link is not.
///
/// **A drop shorter than an interval is not seen** when the link returns
/// at the speed and duplex held: the status register's link bit latches
/// low, and `phy::status` reads it twice and believes the second read, as
/// U-Boot and Linux do. Nothing in the MAC needs changing then, and no
/// line is printed.
pub fn change(held: Option<(Speed, bool)>, now: Link) -> Change {
    match (held, now) {
        (Some((s, f)), Link::Up { speed, full }) if s == speed => {
            if f == full { Change::Same } else { Change::Duplex { full } }
        }
        (_, Link::Up { speed, full }) => Change::Up { speed, full },
        (Some(_), _) => Change::Down,
        (None, _) => Change::Same,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: u64 = ASK_INTERVAL_US;

    #[test]
    fn the_first_wait_asks_at_once() {
        let mut a = LinkAsk::new();
        assert_eq!(a.at_wait(5), Ask::Now);
    }

    #[test]
    fn a_wait_before_the_interval_is_told_the_time_left() {
        let mut a = LinkAsk::new();
        assert_eq!(a.at_wait(0), Ask::Now);
        assert_eq!(a.answered(true), Answered::Same);
        assert_eq!(a.at_wait(300_000), Ask::Later { in_us: S - 300_000 });
    }

    /// The review's case: the kernel's wait ends on a 20 ms tick, so the
    /// reader comes back up to a tick short of the second. It must be told
    /// the rest of the second, not a whole one, so the ask after is about
    /// a second after the last and not two.
    #[test]
    fn a_reader_back_a_tick_early_asks_a_tick_later_not_a_second() {
        let mut a = LinkAsk::new();
        assert_eq!(a.at_wait(0), Ask::Now);
        a.answered(true);
        let early = S - 19_000;
        assert_eq!(a.at_wait(early), Ask::Later { in_us: 19_000 });
        assert_eq!(a.at_wait(early + 19_000), Ask::Now);
    }

    #[test]
    fn asks_are_an_interval_apart_over_many_waits() {
        let mut a = LinkAsk::new();
        let mut now = 0;
        let mut asks = 0;
        // A reader that comes back every 20 ms, for ten seconds.
        while now <= 10 * S {
            if a.at_wait(now) == Ask::Now {
                asks += 1;
                a.answered(true);
            }
            now += 20_000;
        }
        assert_eq!(asks, 11);
    }

    #[test]
    fn later_is_never_zero() {
        let mut a = LinkAsk::new();
        a.at_wait(0);
        a.answered(true);
        for now in [1, S / 2, S - 1] {
            match a.at_wait(now) {
                Ask::Later { in_us } => assert!(in_us > 0),
                other => panic!("{other:?} at {now}"),
            }
        }
    }

    #[test]
    fn a_clock_read_below_the_last_ask_waits_the_interval_out() {
        let mut a = LinkAsk::new();
        a.at_wait(S * 5);
        a.answered(true);
        assert_eq!(a.at_wait(S), Ask::Later { in_us: S });
    }

    /// The review's other case: one failed ask must not slow the asking.
    #[test]
    fn one_failed_ask_does_not_slow_the_asking() {
        let mut a = LinkAsk::new();
        assert_eq!(a.at_wait(0), Ask::Now);
        assert_eq!(a.answered(false), Answered::Same);
        assert_eq!(a.at_wait(S), Ask::Now);
    }

    #[test]
    fn three_failed_asks_running_back_off_to_a_minute_and_say_so_once() {
        let mut a = LinkAsk::new();
        let mut said = 0;
        for i in 0..FAILED_ASKS_MAX as u64 {
            assert_eq!(a.at_wait(i * S), Ask::Now);
            said += (a.answered(false) == Answered::BackedOff) as u32;
        }
        assert_eq!(said, 1);
        assert_eq!(a.interval_us(), BACKOFF_INTERVAL_US);
        // The last ask was at 2 s: nothing until a minute after it.
        let last = (FAILED_ASKS_MAX as u64 - 1) * S;
        assert_eq!(a.at_wait(last + 10 * S), Ask::Later { in_us: BACKOFF_INTERVAL_US - 10 * S });
        assert_eq!(a.at_wait(last + BACKOFF_INTERVAL_US), Ask::Now);
        // Still failing: still a minute apart, and not said again.
        assert_eq!(a.answered(false), Answered::Same);
        assert_eq!(a.interval_us(), BACKOFF_INTERVAL_US);
    }

    #[test]
    fn a_read_after_the_back_off_brings_the_second_back() {
        let mut a = LinkAsk::new();
        for i in 0..FAILED_ASKS_MAX as u64 {
            a.at_wait(i * S);
            a.answered(false);
        }
        let at = 100 * S;
        assert_eq!(a.at_wait(at), Ask::Now);
        assert_eq!(a.answered(true), Answered::Back);
        assert_eq!(a.interval_us(), S);
        assert_eq!(a.at_wait(at + S / 2), Ask::Later { in_us: S / 2 });
        assert_eq!(a.at_wait(at + S), Ask::Now);
        // An ordinary read is not "back".
        assert_eq!(a.answered(true), Answered::Same);
    }

    #[test]
    fn an_ask_that_reads_starts_the_count_again() {
        let mut a = LinkAsk::new();
        let mut now = 0;
        for read in [false, false, true, false, false] {
            assert_eq!(a.at_wait(now), Ask::Now);
            assert_eq!(a.answered(read), Answered::Same);
            now += S;
        }
        assert_eq!(a.at_wait(now), Ask::Now);
    }

    #[test]
    fn a_link_read_against_the_link_held() {
        let g = Some((Speed::S1000, true));
        let up = |speed, full| Link::Up { speed, full };
        // Not held: up starts it, anything else is no news.
        assert_eq!(change(None, up(Speed::S1000, true)), Change::Up { speed: Speed::S1000, full: true });
        for l in [Link::Down, Link::Negotiating, Link::Unresolved] {
            assert_eq!(change(None, l), Change::Same, "{l:?}");
        }
        // Held: the same link is no news, any not-up is down.
        assert_eq!(change(g, up(Speed::S1000, true)), Change::Same);
        for l in [Link::Down, Link::Negotiating, Link::Unresolved] {
            assert_eq!(change(g, l), Change::Down, "{l:?}");
        }
        // Held, and up at another speed: started again at it, whatever
        // the duplex.
        assert_eq!(change(g, up(Speed::S100, true)), Change::Up { speed: Speed::S100, full: true });
        assert_eq!(change(g, up(Speed::S100, false)), Change::Up { speed: Speed::S100, full: false });
        // Held, at the same speed and another duplex: nothing started.
        assert_eq!(change(g, up(Speed::S1000, false)), Change::Duplex { full: false });
        assert_eq!(change(Some((Speed::S100, false)), up(Speed::S100, true)), Change::Duplex { full: true });
    }
}

//! When the reader's wait asks the PHY for the link, while a boot that found
//! none left the MAC off: the rule the kernel's `Device::link_at_wait`
//! follows, here so it runs on the host against a clock the tests step.
//! The kernel lends the time and does the ask; this says when to ask, how
//! long the wait may block before the next one, and when to stop.
//!
//! - **Once an [`ASK_INTERVAL_US`]**, measured on the caller's microsecond
//!   clock, and the wait is told the time left until the next ask, not a
//!   whole interval. The kernel's wait ends on a 20 ms tick, so a reader
//!   told "a second" can come back a tick short of one; told a whole
//!   interval again then, it asked about every two seconds (the review of
//!   the late-cable branch, 2026-11-17).
//! - **Stopped after [`FAILED_ASKS_MAX`] asks running whose reads failed**,
//!   so a stuck MDIO bus (20 ms a read, up to three reads an ask, under
//!   the kernel lock) is not read once a second for good. One failure is
//!   not enough: the boot's own wait allows as many stuck reads running
//!   (`STUCK_READS`), and a single timeout that stopped the asking would
//!   leave the board deaf to a late cable again. An ask that read, up or
//!   down, starts the count again. A send still asks; this is the wait's.
//!
//! Linux's driver falls back to polling the PHY on GENET v5, whose link
//! interrupt is unreliable (`bcmmii.c`, "On GENETv5 there is a hardware
//! issue that prevents the signaling of link UP interrupts"). The interval
//! here is this driver's choice.

/// How often the wait asks the link: once a second.
pub const ASK_INTERVAL_US: u64 = 1_000_000;

/// How many asks running may fail to read before the wait stops asking.
pub const FAILED_ASKS_MAX: u32 = 3;

/// What the wait is to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Ask the link, then tell [`LinkAsk::answered`] whether it read.
    Now,
    /// Not yet: block at most this many microseconds and come back. Never
    /// 0, so a wait always blocks for something.
    Later { in_us: u64 },
    /// The asking has stopped (too many failed asks running).
    Stopped,
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

    /// What the wait at `now_us` is to do. An [`Ask::Now`] is taken as the
    /// ask made at `now_us`, and the next is due an interval later.
    pub fn at_wait(&mut self, now_us: u64) -> Ask {
        if self.failed >= FAILED_ASKS_MAX {
            return Ask::Stopped;
        }
        match self.last_us {
            // A clock read lower than the last ask is no time at all, not
            // a wrap: `saturating_sub` leaves it to wait the interval out.
            Some(at) if now_us.saturating_sub(at) < ASK_INTERVAL_US => Ask::Later {
                in_us: ASK_INTERVAL_US - now_us.saturating_sub(at),
            },
            _ => {
                self.last_us = Some(now_us);
                Ask::Now
            }
        }
    }

    /// After an [`Ask::Now`]: whether the ask's reads answered (a link up
    /// or down is an answer). `true` when this failure is the one that
    /// stops the asking, for the caller's one line.
    pub fn answered(&mut self, read: bool) -> bool {
        if read {
            self.failed = 0;
            return false;
        }
        self.failed += 1;
        self.failed == FAILED_ASKS_MAX
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
        assert!(!a.answered(true));
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

    /// The review's other case: one failed ask must not stop the asking.
    #[test]
    fn one_failed_ask_does_not_stop_the_asking() {
        let mut a = LinkAsk::new();
        assert_eq!(a.at_wait(0), Ask::Now);
        assert!(!a.answered(false));
        assert_eq!(a.at_wait(S), Ask::Now);
    }

    #[test]
    fn three_failed_asks_running_stop_it_and_say_so_once() {
        let mut a = LinkAsk::new();
        let mut said = 0;
        for i in 0..FAILED_ASKS_MAX as u64 {
            assert_eq!(a.at_wait(i * S), Ask::Now);
            said += a.answered(false) as u32;
        }
        assert_eq!(said, 1);
        assert_eq!(a.at_wait(10 * S), Ask::Stopped);
        assert_eq!(a.at_wait(20 * S), Ask::Stopped);
    }

    #[test]
    fn an_ask_that_reads_starts_the_count_again() {
        let mut a = LinkAsk::new();
        let mut now = 0;
        for read in [false, false, true, false, false] {
            assert_eq!(a.at_wait(now), Ask::Now);
            assert!(!a.answered(read));
            now += S;
        }
        assert_eq!(a.at_wait(now), Ask::Now);
    }
}

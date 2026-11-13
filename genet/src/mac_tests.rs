extern crate std;
use crate::mac::*;
use crate::phy::*;
use crate::*;
use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec::Vec;

/// A write or a delay, in the order the driver made them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    W(usize, u32),
    D(u32),
}
use Op::{D, W};

/// A model of the device for the driver: a register file with the
/// behaviour the two sources describe and the driver depends on. **It is
/// ours**, written from the same reading as the code under test, and every
/// offset and value in this file is typed as a number, not taken from the
/// crate's constants, so a constant changed in the crate fails here.
///
/// What it models: the two indexes the driver cannot set (U-Boot's
/// comments in `rx_ring_init` and `tx_ring_init`), a transmit DMA that
/// takes descriptors only while it and the MAC's transmitter are on, a
/// receive ring that holds at most 256 frames not yet released and counts
/// the rest as discards in the producer register's high half (Linux), a
/// software reset that loses the station address (our assumption, the
/// reason the driver reads it first), and one PHY on MDIO.
///
/// A DMA turned off goes on running until its status register has been
/// read `dma_stop_lag` times (Linux polls it; the lag is ours to set).
///
/// What it watches: a ring's set-up registers and the receive descriptors
/// written while that DMA is enabled, or turned off and not yet stopped,
/// are `violations`.
struct Dev {
    regs: BTreeMap<usize, u32>,
    ops: Vec<Op>,
    reads: Vec<usize>,
    /// `RDMA_PROD_INDEX` whole: the index low, the discard count high.
    rx_prod: u32,
    tx_cons: u16,
    /// Where the consumer index goes once `tx_lag_left` reads have passed.
    tx_target: u16,
    tx_lag: u32,
    tx_lag_left: u32,
    tx_dead: bool,
    /// (buffer address, length and status) of every descriptor taken.
    sent: Vec<(u64, u32)>,
    violations: Vec<String>,
    phy: u8,
    phy_regs: [u16; 32],
    /// A status reading returned once before `phy_regs[1]`: the latch.
    bmsr_first: Option<u16>,
    mdio: u32,
    mdio_stuck: bool,
    /// Reads of `MDIO_CMD` a started command stays busy for.
    mdio_busy: u32,
    mdio_busy_left: u32,
    mdio_result: u32,
    /// An address with no PHY reads all ones with no flag, where it is
    /// flagged otherwise.
    mdio_ones: bool,
    /// Reads of a `DMA_STATUS` before a DMA turned off says disabled;
    /// `u32::MAX` is never. (transmit, receive)
    dma_stop_lag: (u32, u32),
    dma_stop_left: (u32, u32),
}

const SYS_REV: usize = 0x00;
const UMAC_CMD: usize = 0x808;
const MAC0: usize = 0x80c;
const MAC1: usize = 0x810;
const OOB: usize = 0x8c;
const RBUF: usize = 0x300;
const TBUF: usize = 0x600;
const MDIO: usize = 0xe14;
const RDMA_PROD: usize = 0x3008;
const RDMA_CONS: usize = 0x300c;
const RDMA_CTL: usize = 0x3044;
const TDMA_CONS: usize = 0x5008;
const TDMA_PROD: usize = 0x500c;
const TDMA_CTL: usize = 0x5044;
const RX_DESC: usize = 0x2000;
const TX_DESC: usize = 0x4000;
/// Ring 16 enabled and the DMA on.
const DMA_ON: u32 = 0x0002_0001;

/// A receive area above 4 GB, so the high address word is not zero.
const RX_BASE: u64 = 0x1_2340_0000;
const ADDRESS: [u8; 6] = [0xdc, 0xa6, 0x32, 0x01, 0x02, 0x03];

/// The board out of reset, as predicted: v5, an address, everything off.
fn board() -> Dev {
    let mut phy_regs = [0u16; 32];
    phy_regs[1] = 0x796d; // link, negotiated, the gigabit registers present
    phy_regs[2] = 0x600d;
    phy_regs[3] = 0x84a2;
    phy_regs[4] = 0x01e1; // 10 and 100, both duplexes
    phy_regs[5] = 0xc1e1;
    phy_regs[9] = 0x0200; // 1000 full
    phy_regs[10] = 0x3800;
    Dev {
        regs: BTreeMap::from([(SYS_REV, 0x0600_0000), (MAC0, 0xdca6_3201), (MAC1, 0x0203), (OOB, 0x20)]),
        ops: Vec::new(),
        reads: Vec::new(),
        rx_prod: 0,
        tx_cons: 0,
        tx_target: 0,
        tx_lag: 0,
        tx_lag_left: 0,
        tx_dead: false,
        sent: Vec::new(),
        violations: Vec::new(),
        phy: 1,
        phy_regs,
        bmsr_first: None,
        mdio: 0,
        mdio_stuck: false,
        mdio_busy: 0,
        mdio_busy_left: 0,
        mdio_result: 0,
        mdio_ones: false,
        dma_stop_lag: (0, 0),
        dma_stop_left: (0, 0),
    }
}

/// The board as a firmware that used the device might leave it: running,
/// its interrupts unmasked, the status block on, a second ring enabled.
fn left_running() -> Dev {
    let mut d = board();
    d.regs.insert(UMAC_CMD, 0x0b);
    d.regs.insert(RDMA_CTL, DMA_ON | 0x2);
    d.regs.insert(TDMA_CTL, DMA_ON | 0x2);
    d.regs.insert(RBUF, 0x11);
    d.regs.insert(TBUF, 0x11);
    d.regs.insert(0x08, 0x5);
    d
}

impl Dev {
    fn dma_status(ctrl: u32, left: &mut u32) -> u32 {
        if ctrl & 1 != 0 {
            return 0;
        }
        if *left == 0 {
            return 1;
        }
        if *left != u32::MAX {
            *left -= 1;
        }
        0
    }
    fn reg(&self, offset: usize) -> u32 {
        self.regs.get(&offset).copied().unwrap_or(0)
    }
    fn writes(&self) -> Vec<(usize, u32)> {
        self.ops.iter().filter_map(|op| if let W(o, v) = op { Some((*o, *v)) } else { None }).collect()
    }
    fn delayed_us(&self) -> u64 {
        self.ops.iter().map(|op| if let D(us) = op { u64::from(*us) } else { 0 }).sum()
    }
    fn rx_running(&self) -> bool {
        self.reg(RDMA_CTL) & DMA_ON == DMA_ON && self.reg(UMAC_CMD) & 2 != 0
    }
    fn tx_running(&self) -> bool {
        self.reg(TDMA_CTL) & DMA_ON == DMA_ON && self.reg(UMAC_CMD) & 1 != 0 && !self.tx_dead
    }
    /// The device receives a frame whose descriptor reads `length` (pad
    /// included) and `flags`. False when it had nowhere to put it.
    fn deliver_raw(&mut self, length: u32, flags: u32) -> bool {
        assert!(self.rx_running(), "a frame for a receiver that is off");
        let prod = self.rx_prod as u16;
        if prod.wrapping_sub(self.reg(RDMA_CONS) as u16) >= 256 {
            self.rx_prod = self.rx_prod.wrapping_add(1 << 16);
            return false;
        }
        self.regs.insert(RX_DESC + usize::from(prod % 256) * 12, (length << 16) | flags);
        self.rx_prod = (self.rx_prod & 0xffff_0000) | u32::from(prod.wrapping_add(1));
        true
    }
    /// A whole, clean frame of `payload` bytes.
    fn deliver(&mut self, payload: u32) -> bool {
        self.deliver_raw(payload + 2, 0x6000)
    }
}

impl Bus for Dev {
    fn read(&mut self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= 0x1_0000, "read at {offset:#x}");
        self.reads.push(offset);
        match offset {
            RDMA_PROD => self.rx_prod,
            TDMA_CONS => {
                if self.tx_cons != self.tx_target {
                    if self.tx_lag_left > 0 {
                        self.tx_lag_left -= 1;
                    } else {
                        self.tx_cons = self.tx_target;
                    }
                }
                u32::from(self.tx_cons)
            }
            MDIO => {
                if self.mdio & (1 << 29) != 0 && !self.mdio_stuck {
                    if self.mdio_busy_left > 0 {
                        self.mdio_busy_left -= 1;
                    } else {
                        self.mdio = self.mdio_result;
                    }
                }
                self.mdio
            }
            // DMA_STATUS: disabled once the DMA is off and has wound down.
            0x5048 => Self::dma_status(self.reg(TDMA_CTL), &mut self.dma_stop_left.0),
            0x3048 => Self::dma_status(self.reg(RDMA_CTL), &mut self.dma_stop_left.1),
            _ => self.reg(offset),
        }
    }

    fn write(&mut self, offset: usize, value: u32) {
        assert!(offset.is_multiple_of(4) && offset + 4 <= 0x1_0000, "write at {offset:#x}");
        self.ops.push(W(offset, value));
        let rx_setup = matches!(offset, 0x3000 | 0x3010 | 0x3014 | 0x301c | 0x3028 | 0x302c | 0x3040 | 0x304c) || (RX_DESC..RX_DESC + 256 * 12).contains(&offset);
        let tx_setup = matches!(offset, 0x5000 | 0x5010 | 0x5014 | 0x501c | 0x5024 | 0x5028 | 0x502c | 0x5040 | 0x504c);
        let rx_live = self.reg(RDMA_CTL) & 1 != 0 || self.dma_stop_left.1 > 0;
        let tx_live = self.reg(TDMA_CTL) & 1 != 0 || self.dma_stop_left.0 > 0;
        if (rx_setup && rx_live) || (tx_setup && tx_live) {
            self.violations.push(format!("{offset:#x} written with its DMA running"));
        }
        // A DMA turned off starts winding down.
        if offset == TDMA_CTL && self.reg(TDMA_CTL) & 1 != 0 && value & 1 == 0 {
            self.dma_stop_left.0 = self.dma_stop_lag.0;
        }
        if offset == RDMA_CTL && self.reg(RDMA_CTL) & 1 != 0 && value & 1 == 0 {
            self.dma_stop_left.1 = self.dma_stop_lag.1;
        }
        match offset {
            // The two indexes the device owns.
            RDMA_PROD | TDMA_CONS => {}
            UMAC_CMD => {
                if value & 0x2000 != 0 {
                    self.regs.insert(MAC0, 0);
                    self.regs.insert(MAC1, 0);
                }
                self.regs.insert(offset, value);
            }
            TDMA_PROD => {
                if value > 0xffff {
                    self.violations.push(format!("producer index {value:#x} is more than 16 bits"));
                }
                self.regs.insert(offset, value);
                if self.tx_running() {
                    let mut index = self.tx_target;
                    while index != value as u16 {
                        let desc = TX_DESC + usize::from(index % 256) * 12;
                        self.sent.push((u64::from(self.reg(desc + 4)) | u64::from(self.reg(desc + 8)) << 32, self.reg(desc)));
                        index = index.wrapping_add(1);
                    }
                    self.tx_target = value as u16;
                    self.tx_lag_left = self.tx_lag;
                }
            }
            MDIO => {
                self.mdio = value;
                if value & (1 << 29) != 0 && !self.mdio_stuck {
                    if value & (3 << 26) != 2 << 26 || value & 0xffff != 0 {
                        self.violations.push(format!("MDIO command {value:#x} is not a read"));
                    }
                    let (phy, r) = (((value >> 21) & 0x1f) as u8, ((value >> 16) & 0x1f) as usize);
                    let command = value & !(1 << 29);
                    self.mdio_busy_left = self.mdio_busy;
                    self.mdio_result = if phy != self.phy && self.mdio_ones {
                        command | 0xffff
                    } else if phy != self.phy {
                        command | 1 << 28
                    } else if r == 1 {
                        command | u32::from(self.bmsr_first.take().unwrap_or(self.phy_regs[1]))
                    } else {
                        command | u32::from(self.phy_regs[r])
                    };
                }
            }
            _ => {
                self.regs.insert(offset, value);
            }
        }
    }

    fn delay_us(&mut self, us: u32) {
        self.ops.push(D(us));
    }
}

fn up(d: &mut Dev) -> Genet {
    let mut g = Genet::init(d, RX_BASE).expect("the board sets up");
    g.start(d, Speed::S1000);
    assert!(d.violations.is_empty(), "{:?}", d.violations);
    d.ops.clear();
    d.reads.clear();
    g
}

// ---- set-up ----

#[test]
fn init_is_u_boots_sequence_write_for_write() {
    let mut d = board();
    d.rx_prod = 0x0007_1234;
    d.tx_cons = 0x00ab;
    d.tx_target = 0x00ab;
    let g = Genet::init(&mut d, RX_BASE).unwrap();
    assert_eq!(g.mac(), ADDRESS);
    assert!(!g.started());

    let head = [
        // Every interrupt masked and cleared (Linux's bcmgenet_intr_disable).
        W(0x210, 0xffff_ffff), W(0x208, 0xffff_ffff), W(0x250, 0xffff_ffff), W(0x248, 0xffff_ffff),
        // bcmgenet_eth_probe.
        W(0x04, 3), W(0x08, 0), D(10), W(0x808, 0), W(0x808, 0xa000),
        // bcmgenet_umac_reset.
        W(0x08, 2), D(10), W(0x08, 0), D(10), W(0x08, 0), D(10),
        W(0x808, 0), W(0x808, 0xa000), D(2), W(0x808, 0),
        W(0xd80, 7), W(0xd80, 0), W(0x814, 1536), W(0x300, 2), W(0x600, 0), W(0x3b4, 1),
        // bcmgenet_gmac_write_hwaddr.
        W(0x80c, 0xdca6_3201), W(0x810, 0x0000_0203),
        // bcmgenet_disable_dma, with Linux's wait between the two DMAs.
        W(0x5044, 0), D(10_000), W(0x3044, 0), W(0xb34, 1), D(10), W(0xb34, 0),
        // rx_ring_init: the consumer aligned on the producer's low half.
        W(0x304c, 8), W(0x3014, 0), W(0x302c, 0), W(0x3000, 0), W(0x301c, 767),
        W(0x300c, 0x1234), W(0x3010, 0x0100_0800), W(0x3028, 0x0005_0010), W(0x3040, 0x0001_0000),
    ];
    let tail = [
        // tx_ring_init: the producer aligned on the consumer.
        W(0x504c, 8), W(0x5014, 0), W(0x5000, 0), W(0x502c, 0), W(0x501c, 767),
        W(0x500c, 0x00ab), W(0x5024, 1), W(0x5028, 0), W(0x5010, 0x0100_0800), W(0x5040, 0x0001_0000),
        // bcmgenet_enable_dma.
        W(0x5044, 0x0002_0001), W(0x3044, 0x0002_0001),
    ];
    assert_eq!(d.ops.len(), head.len() + 256 * 3 + tail.len());
    assert_eq!(d.ops[..head.len()], head);
    assert_eq!(d.ops[head.len() + 768..], tail);
    // rx_descs_init: address low, address high, then length and DMA_OWN.
    let descs = &d.ops[head.len()..head.len() + 768];
    assert_eq!(descs[..3], [W(0x2004, 0x2340_0000), W(0x2008, 1), W(0x2000, 0x0800_8000)]);
    assert_eq!(descs[3..6], [W(0x2010, 0x2340_0800), W(0x2014, 1), W(0x200c, 0x0800_8000)]);
    assert_eq!(descs[765..], [W(0x2bf8, 0x2347_f800), W(0x2bfc, 1), W(0x2bf4, 0x0800_8000)]);
    for (i, d3) in descs.chunks(3).enumerate() {
        let buffer = RX_BASE + i as u64 * 2048;
        assert_eq!(d3, [W(0x2004 + i * 12, buffer as u32), W(0x2008 + i * 12, (buffer >> 32) as u32), W(0x2000 + i * 12, 0x0800_8000)]);
    }
    assert!(d.violations.is_empty(), "{:?}", d.violations);
}

#[test]
fn the_address_is_read_before_the_reset_and_written_back() {
    // The model's reset zeroes the address registers.
    let mut d = board();
    let g = Genet::init(&mut d, RX_BASE).unwrap();
    assert_eq!(g.mac(), ADDRESS);
    assert_eq!((d.reg(MAC0), d.reg(MAC1)), (0xdca6_3201, 0x0203));
    // Nothing was written before the address was read.
    let first_write = d.ops.iter().position(|op| matches!(op, W(..))).unwrap();
    assert_eq!(first_write, 0);
    assert_eq!(d.reads[..3], [SYS_REV, MAC0, MAC1]);
}

#[test]
fn any_other_version_is_refused_with_nothing_written() {
    for rev in [0x0500_0000, 0, 0x0700_0000, 0xffff_ffff] {
        let mut d = left_running();
        d.regs.insert(SYS_REV, rev);
        assert_eq!(Genet::init(&mut d, RX_BASE).unwrap_err(), InitError::NotV5(rev));
        assert!(d.ops.is_empty());
        assert_eq!(d.reads, [SYS_REV]);
        assert_eq!(quiet(&mut d), None);
        assert!(d.ops.is_empty(), "quiet wrote to version {rev:#x}");
    }
}

#[test]
fn an_address_no_station_has_is_refused_with_nothing_written() {
    for (mac0, mac1, address) in [(0, 0, [0; 6]), (0xffff_ffff, 0xffff, [0xff; 6]), (0x0100_5e00, 0x0001, [1, 0, 0x5e, 0, 0, 1])] {
        let mut d = board();
        d.regs.insert(MAC0, mac0);
        d.regs.insert(MAC1, mac1);
        assert_eq!(Genet::init(&mut d, RX_BASE).unwrap_err(), InitError::NoMac(address));
        assert!(d.ops.is_empty());
    }
}

#[test]
fn a_receive_area_nowhere_or_past_the_end_is_refused() {
    let len = 256 * 2048u64;
    for base in [0, u64::MAX, u64::MAX - len + 1] {
        let mut d = board();
        assert_eq!(Genet::init(&mut d, base).unwrap_err(), InitError::BadBuffers, "{base:#x}");
        assert!(d.ops.is_empty());
    }
    let mut d = board();
    assert!(Genet::init(&mut d, u64::MAX - len).is_ok());
}

#[test]
fn a_device_left_running_ends_in_the_same_state() {
    let mut fresh = board();
    Genet::init(&mut fresh, RX_BASE).unwrap();
    let mut d = left_running();
    Genet::init(&mut d, RX_BASE).unwrap();
    // No ring was set up under a running DMA.
    assert!(d.violations.is_empty(), "{:?}", d.violations);
    // The status block is off and the 2-byte pad on; a bit this driver
    // does not know is kept.
    assert_eq!(d.reg(RBUF), 0x12);
    // The transmit status block is off too, its other bit kept.
    assert_eq!(d.reg(TBUF), 0x10);
    // Ring 16 alone in both directions: the other ring's enable is gone.
    assert_eq!((d.reg(RDMA_CTL), d.reg(TDMA_CTL)), (0x0002_0001, 0x0002_0001));
    // The MAC is off until start.
    assert_eq!(d.reg(UMAC_CMD), 0);
    // Every register the fresh board ends with, this one ends with too,
    // but the one with the kept bit.
    for (offset, value) in &fresh.regs {
        if *offset != RBUF && *offset != TBUF {
            assert_eq!(d.reg(*offset), *value, "register {offset:#x}");
        }
    }
}

#[test]
fn quiet_turns_off_what_can_write_memory_or_interrupt() {
    let mut d = left_running();
    assert_eq!(quiet(&mut d), Some(Stopped { tdma: true, rdma: true }));
    assert_eq!(
        d.ops,
        [
            W(0x210, 0xffff_ffff), W(0x208, 0xffff_ffff), W(0x250, 0xffff_ffff), W(0x248, 0xffff_ffff),
            // The enables off, the speed bits kept, and a frame's time.
            W(0x808, 0x08), D(2000),
            // The transmit DMA, the drain, the receive DMA, the flush.
            W(0x5044, 0x0002_0002), D(10_000), W(0x3044, 0x0002_0002), W(0xb34, 1), D(10), W(0xb34, 0),
        ]
    );
    assert!(!d.rx_running() && !d.tx_running());
}

#[test]
fn a_dma_is_waited_for_until_it_says_it_stopped() {
    // Each takes some polls to stop: a microsecond's wait for each.
    let mut d = left_running();
    d.dma_stop_lag = (7, 40);
    assert_eq!(quiet(&mut d), Some(Stopped { tdma: true, rdma: true }));
    assert_eq!(d.delayed_us(), 2000 + 7 + 10_000 + 40 + 10);
    // The last poll allowed is believed; one more is not waited for.
    let mut d = left_running();
    d.dma_stop_lag = (4999, 5000);
    assert_eq!(quiet(&mut d), Some(Stopped { tdma: true, rdma: false }));
    assert_eq!(d.delayed_us(), 2000 + 4999 + 10_000 + 5000 + 10);
    let mut d = left_running();
    d.dma_stop_lag = (u32::MAX, 0);
    let stopped = quiet(&mut d).unwrap();
    assert_eq!(stopped, Stopped { tdma: false, rdma: true });
    assert!(!stopped.both());
    assert!(quiet(&mut left_running()).unwrap().both());
}

#[test]
fn init_touches_no_ring_of_a_dma_that_did_not_stop() {
    for lag in [(u32::MAX, 0), (0, u32::MAX)] {
        let mut d = left_running();
        d.dma_stop_lag = lag;
        let stopped = Stopped { tdma: lag.0 == 0, rdma: lag.1 == 0 };
        assert_eq!(Genet::init(&mut d, RX_BASE).unwrap_err(), InitError::DmaStuck(stopped));
        assert!(d.violations.is_empty(), "{:?}", d.violations);
        // Nothing at or past the receive descriptors but the two controls.
        assert!(d.writes().iter().all(|w| w.0 < 0x2000 || w.0 == 0x3044 || w.0 == 0x5044), "{:?}", d.writes());
    }
    // A DMA slow to stop is waited for, and then set up.
    let mut d = left_running();
    d.dma_stop_lag = (300, 300);
    assert!(Genet::init(&mut d, RX_BASE).is_ok());
    assert!(d.violations.is_empty(), "{:?}", d.violations);
}

#[test]
fn stop_makes_it_quiet_and_a_send_is_refused_until_the_next_start() {
    let mut d = board();
    let mut g = up(&mut d);
    assert_eq!(g.stop(&mut d), Stopped { tdma: true, rdma: true });
    assert!(!g.started() && !d.rx_running() && !d.tx_running());
    assert_eq!((d.reg(RDMA_CTL) & 1, d.reg(TDMA_CTL) & 1, d.reg(UMAC_CMD) & 3), (0, 0, 0));
    d.ops.clear();
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::NotStarted));
    assert!(d.ops.is_empty());
}

#[test]
fn start_tells_the_mac_the_speed_then_turns_it_on() {
    for (speed, word) in [(Speed::S10, 0x0), (Speed::S100, 0x4), (Speed::S1000, 0x8)] {
        let mut d = board();
        let mut g = Genet::init(&mut d, RX_BASE).unwrap();
        d.ops.clear();
        g.start(&mut d, speed);
        // bcmgenet_adjust_link: OOB_DISABLE (0x20) off, RGMII_LINK and
        // RGMII_MODE_EN on, then ID_MODE_DIS; the speed; the enables.
        assert_eq!(d.ops, [W(0x8c, 0x50), W(0x8c, 0x0001_0050), W(0x808, word), W(0x808, word | 3)]);
        assert!(!d.ops.contains(&D(0)));
        assert!(g.started() && d.rx_running() && d.tx_running());
    }
}

#[test]
fn a_second_start_changes_the_speed_and_leaves_the_mac_on() {
    let mut d = board();
    let mut g = up(&mut d);
    g.start(&mut d, Speed::S100);
    assert_eq!(d.reg(UMAC_CMD), 0x7);
    assert_eq!(d.reg(OOB), 0x0001_0050);
    // The MAC was never off on the way: every write to it had both enables.
    let cmds: Vec<u32> = d.writes().iter().filter(|w| w.0 == UMAC_CMD).map(|w| w.1).collect();
    assert_eq!(cmds, [0x7, 0x7]);
}

// ---- receive ----

#[test]
fn nothing_waiting_is_one_read_and_no_write() {
    let mut d = board();
    let mut g = up(&mut d);
    assert!(!g.has_frame(&mut d));
    assert_eq!(d.reads, [RDMA_PROD]);
    assert_eq!(g.rx_next(&mut d), Rx::None);
    assert!(d.ops.is_empty());
}

#[test]
fn a_frame_is_returned_where_it_lies_until_released() {
    let mut d = board();
    d.rx_prod = 5;
    let mut g = up(&mut d);
    assert!(d.deliver(60));
    assert!(g.has_frame(&mut d));
    let frame = Rx::Frame { buffer: 5, offset: 2, len: 60 };
    assert_eq!(g.rx_next(&mut d), frame);
    // Looking is not taking: the same frame again, and nothing written.
    assert_eq!(g.rx_next(&mut d), frame);
    assert!(d.ops.is_empty());
    assert!(g.rx_release(&mut d));
    assert_eq!(d.ops, [W(RDMA_CONS, 6)]);
    assert!(!g.has_frame(&mut d));
    assert_eq!(g.rx_next(&mut d), Rx::None);
}

#[test]
fn a_release_with_no_frame_held_does_nothing() {
    let mut d = board();
    let mut g = up(&mut d);
    // Nothing was returned.
    assert!(!g.rx_release(&mut d));
    // A second release of one frame.
    assert!(d.deliver(60));
    assert!(matches!(g.rx_next(&mut d), Rx::Frame { .. }));
    assert!(g.rx_release(&mut d));
    assert!(!g.rx_release(&mut d));
    // A dropped frame was released by rx_next itself.
    assert!(d.deliver_raw(66, 0));
    assert_eq!(g.rx_next(&mut d), Rx::Dropped(RxDrop::Fragment));
    assert!(!g.rx_release(&mut d));
    // The consumer moved twice, once a frame, and is level with the device.
    assert_eq!(d.writes(), [(RDMA_CONS, 1), (RDMA_CONS, 2)]);
    assert!(!g.has_frame(&mut d));
}

#[test]
fn the_receive_counter_wraps_at_65536_and_the_ring_at_256() {
    let mut d = board();
    d.rx_prod = 0xfffe;
    let mut g = up(&mut d);
    let mut seen = Vec::new();
    for payload in 100..104 {
        assert!(d.deliver(payload));
        match g.rx_next(&mut d) {
            Rx::Frame { buffer, offset: 2, len } => seen.push((buffer, len)),
            other => panic!("{other:?}"),
        }
        g.rx_release(&mut d);
    }
    assert_eq!(seen, [(254, 100), (255, 101), (0, 102), (1, 103)]);
    assert_eq!(d.writes(), [(RDMA_CONS, 0xffff), (RDMA_CONS, 0), (RDMA_CONS, 1), (RDMA_CONS, 2)]);
    assert!(!g.has_frame(&mut d));
}

#[test]
fn the_ring_index_follows_the_counter_past_the_first_256() {
    // 300 frames from 0x00f0: the buffer is always the counter's low byte.
    let mut d = board();
    d.rx_prod = 0x00f0;
    let mut g = up(&mut d);
    for n in 0..300usize {
        assert!(d.deliver(64));
        assert_eq!(g.rx_next(&mut d), Rx::Frame { buffer: (0xf0 + n) % 256, offset: 2, len: 64 });
        g.rx_release(&mut d);
    }
}

#[test]
fn the_discard_count_in_the_high_half_is_not_a_frame() {
    let mut d = board();
    d.rx_prod = 0x0003_0040;
    let mut g = up(&mut d);
    assert!(!g.has_frame(&mut d));
    d.rx_prod = 0x0009_0040;
    assert!(!g.has_frame(&mut d));
    assert_eq!(g.rx_next(&mut d), Rx::None);
    assert!(d.deliver(64));
    assert_eq!(g.rx_next(&mut d), Rx::Frame { buffer: 0x40, offset: 2, len: 64 });
}

#[test]
fn the_longest_frame_is_the_devices_maximum_and_one_byte_more_is_not() {
    let mut d = board();
    let mut g = up(&mut d);
    // 1536 is what init told the device; with the pad, 1538.
    assert!(d.deliver(1536));
    assert_eq!(g.rx_next(&mut d), Rx::Frame { buffer: 0, offset: 2, len: 1536 });
    g.rx_release(&mut d);
    // One more, the buffer's own 2048, one past it, the field's most.
    for length in [1539, 2048, 2049, 0xfff] {
        d.ops.clear();
        assert!(d.deliver_raw(length, 0x6000));
        assert_eq!(g.rx_next(&mut d), Rx::Dropped(RxDrop::Oversize), "length {length}");
        // Dropped is released: one consumer write, and nothing waits.
        assert_eq!(d.ops.len(), 1);
        assert!(!g.has_frame(&mut d));
    }
    assert_eq!(g.drops(), Drops { oversize: 4, ..Drops::default() });
}

#[test]
fn a_frame_shorter_than_a_header_is_a_runt() {
    let mut d = board();
    let mut g = up(&mut d);
    // Nothing, less than the pad, the pad alone, a byte short of a header.
    for length in [0, 1, 2, 15] {
        assert!(d.deliver_raw(length, 0x6000));
        assert_eq!(g.rx_next(&mut d), Rx::Dropped(RxDrop::Runt), "length {length}");
    }
    assert!(d.deliver_raw(16, 0x6000));
    assert_eq!(g.rx_next(&mut d), Rx::Frame { buffer: 4, offset: 2, len: 14 });
    assert_eq!(g.drops().runt, 4);
}

#[test]
fn a_frame_not_whole_in_its_buffer_is_dropped() {
    let mut d = board();
    let mut g = up(&mut d);
    // No start, no end, neither.
    for flags in [0x4000, 0x2000, 0] {
        assert!(d.deliver_raw(66, flags));
        assert_eq!(g.rx_next(&mut d), Rx::Dropped(RxDrop::Fragment), "flags {flags:#x}");
        assert!(!g.has_frame(&mut d));
    }
    assert_eq!(g.drops().fragment, 3);
}

#[test]
fn each_error_flag_drops_the_frame_and_no_other_bit_does() {
    let mut d = board();
    let mut g = up(&mut d);
    // Overflow, checksum, receive error, not octet aligned, too long.
    for flag in [0x01, 0x02, 0x04, 0x08, 0x10] {
        assert!(d.deliver_raw(66, 0x6000 | flag));
        assert_eq!(g.rx_next(&mut d), Rx::Dropped(RxDrop::Flagged(0x6000 | flag as u16)));
    }
    assert_eq!(g.drops().flagged, 5);
    // Broadcast, multicast, the filter index, the checksum bit and the
    // bit the driver set at init are not errors.
    for flag in [0x40, 0x20, 0x0f80, 0x8000, 0x1000] {
        assert!(d.deliver_raw(66, 0x6000 | flag));
        assert!(matches!(g.rx_next(&mut d), Rx::Frame { len: 64, .. }), "flag {flag:#x}");
        g.rx_release(&mut d);
    }
    assert_eq!(g.drops().flagged, 5);
}

#[test]
fn releasing_a_frame_makes_room_in_a_full_ring() {
    let mut d = board();
    d.rx_prod = 0xff80;
    let mut g = up(&mut d);
    for _ in 0..256 {
        assert!(d.deliver(64));
    }
    // Full: the next is discarded, counted in the high half, and the
    // driver still sees exactly its 256.
    assert!(!d.deliver(64));
    assert_eq!(d.rx_prod, 0x0001_0080);
    assert!(matches!(g.rx_next(&mut d), Rx::Frame { buffer: 0x80, .. }));
    g.rx_release(&mut d);
    assert!(d.deliver(64));
    assert!(!d.deliver(64));
    let mut taken = 0;
    for _ in 0..1000 {
        let Rx::Frame { .. } = g.rx_next(&mut d) else { break };
        g.rx_release(&mut d);
        taken += 1;
    }
    assert_eq!(taken, 256);
}

// ---- transmit ----

const TX_ADDR: u64 = 0x2_0000_1000;

#[test]
fn a_send_before_start_writes_nothing() {
    let mut d = board();
    let mut g = Genet::init(&mut d, RX_BASE).unwrap();
    d.ops.clear();
    d.reads.clear();
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::NotStarted));
    assert!(d.ops.is_empty() && d.reads.is_empty());
}

#[test]
fn a_send_fills_the_descriptor_then_moves_the_producer() {
    let mut d = board();
    d.tx_cons = 7;
    d.tx_target = 7;
    let mut g = up(&mut d);
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Ok(()));
    // Descriptor 7: address low, high, then 60 << 16 with the tag 0x3f at
    // bit 7, append-CRC, SOP and EOP. Then the producer, 8.
    assert_eq!(d.ops, [W(0x4000 + 7 * 12 + 4, 0x0000_1000), W(0x4000 + 7 * 12 + 8, 2), W(0x4000 + 7 * 12, 0x003c_7fc0), W(TDMA_PROD, 8)]);
    assert_eq!(d.sent, [(TX_ADDR, 0x003c_7fc0)]);
}

#[test]
fn the_transmit_counter_wraps_at_65536_and_the_ring_at_256() {
    let mut d = board();
    d.tx_cons = 0xfffe;
    d.tx_target = 0xfffe;
    let mut g = up(&mut d);
    for len in 60..64 {
        assert_eq!(g.tx_send(&mut d, TX_ADDR, len), Ok(()), "length {len}");
    }
    let producers: Vec<u32> = d.writes().iter().filter(|w| w.0 == TDMA_PROD).map(|w| w.1).collect();
    assert_eq!(producers, [0xffff, 0, 1, 2]);
    let descriptors: Vec<usize> = d.writes().iter().filter(|w| (TX_DESC..TX_DESC + 256 * 12).contains(&w.0) && (w.0 - TX_DESC).is_multiple_of(12)).map(|w| (w.0 - TX_DESC) / 12).collect();
    assert_eq!(descriptors, [254, 255, 0, 1]);
    assert_eq!(d.sent.len(), 4);
    assert!(d.violations.is_empty(), "{:?}", d.violations);
}

#[test]
fn a_frame_too_short_or_too_long_is_refused_with_nothing_written() {
    let mut d = board();
    let mut g = up(&mut d);
    for len in [0, 13, 1533, 2048, usize::MAX] {
        assert_eq!(g.tx_send(&mut d, TX_ADDR, len), Err(TxError::BadLength(len)));
    }
    assert!(d.ops.is_empty() && d.reads.is_empty());
    // A bare header is the floor; the device pads a frame under 60.
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 14), Ok(()));
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 1532), Ok(()));
    assert_eq!((d.sent[0].1 >> 16, d.sent[1].1 >> 16), (14, 1532));
}

#[test]
fn a_send_seen_done_at_once_waits_for_nothing() {
    let mut d = board();
    let mut g = up(&mut d);
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Ok(()));
    assert_eq!(d.delayed_us(), 0);
}

#[test]
fn a_send_done_on_the_last_poll_is_done_and_one_later_is_a_timeout() {
    let mut d = board();
    d.tx_lag = 10_000 - 1;
    let mut g = up(&mut d);
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Ok(()));
    // A wait before every poll but the first.
    assert_eq!(d.delayed_us(), 9_999 * 10);

    let mut d = board();
    d.tx_lag = 10_000;
    let mut g = up(&mut d);
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::Timeout));
    assert_eq!(d.delayed_us(), 9_999 * 10, "under 100 ms");
    assert_eq!(d.reads.iter().filter(|r| **r == TDMA_CONS).count(), 1 + 10_000);
}

#[test]
fn a_frame_the_device_never_takes_blocks_the_next_send_until_it_does() {
    let mut d = board();
    let mut g = up(&mut d);
    d.tx_dead = true;
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::Timeout));
    d.ops.clear();
    // The buffer is still that frame's: nothing may be written over it.
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 100), Err(TxError::Busy));
    assert!(d.ops.is_empty());
    assert!(d.sent.is_empty());
    // The device comes back and takes it: the producer is where the
    // driver left it, so the next send lines up.
    d.tx_dead = false;
    d.tx_target = 1;
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 100), Ok(()));
    assert_eq!(d.writes().last(), Some(&(TDMA_PROD, 2)));
    assert_eq!(d.sent, [(TX_ADDR, 0x0064_7fc0)]);
}

#[test]
fn tx_idle_says_when_the_shared_buffer_may_be_written() {
    let mut d = board();
    let mut g = up(&mut d);
    assert!(g.tx_idle(&mut d));
    d.tx_dead = true;
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::Timeout));
    d.ops.clear();
    // The frame is still the device's: one read says so, nothing written.
    assert!(!g.tx_idle(&mut d));
    assert!(d.ops.is_empty());
    d.tx_dead = false;
    d.tx_target = 1;
    assert!(g.tx_idle(&mut d));
}

#[test]
fn a_frame_not_taken_across_the_wrap_is_not_seen_as_taken() {
    // The producer wraps to 0 and the consumer stays at 0xffff: "consumer
    // at or past the producer", U-Boot's test, would call that done.
    let mut d = board();
    d.tx_cons = 0xffff;
    d.tx_target = 0xffff;
    let mut g = up(&mut d);
    d.tx_dead = true;
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::Timeout));
    assert_eq!(g.tx_send(&mut d, TX_ADDR, 60), Err(TxError::Busy));
    assert_eq!(d.writes().iter().filter(|w| w.0 == TDMA_PROD).count(), 1);
}

// ---- the link ----

fn link_of(d: &mut Dev) -> Link {
    let l = link(d, 1).unwrap();
    for (offset, value) in d.writes() {
        assert!(scan_write_allowed(offset, value), "the link read wrote {value:#x} at {offset:#x}");
    }
    assert!(d.violations.is_empty(), "{:?}", d.violations);
    l
}

#[test]
fn the_link_is_what_both_ends_advertise_gigabit_first() {
    let mut d = board();
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S1000, full: true });
    // Six reads: the status twice, then two pairs.
    assert_eq!(d.writes().len(), 2 * 4);

    // The partner offers 1000 half alone; this end offers both.
    let mut d = board();
    d.phy_regs[9] = 0x0300;
    d.phy_regs[10] = 0x0400;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S1000, full: false });

    // The partner offers gigabit and this end does not: 100 full.
    let mut d = board();
    d.phy_regs[9] = 0;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S100, full: true });
    assert_eq!(d.writes().len(), 2 * 6);

    // This end offers 1000 full, the partner 1000 half: no gigabit both
    // have, so 100.
    let mut d = board();
    d.phy_regs[10] = 0x0400;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S100, full: true });
}

#[test]
fn the_slower_speeds_by_their_bits() {
    for (lpa, want) in [
        (0x0180, Link::Up { speed: Speed::S100, full: true }),
        (0x0080, Link::Up { speed: Speed::S100, full: false }),
        (0x0060, Link::Up { speed: Speed::S10, full: true }),
        (0x0020, Link::Up { speed: Speed::S10, full: false }),
        (0x0001, Link::Unresolved),
    ] {
        let mut d = board();
        d.phy_regs[10] = 0;
        d.phy_regs[5] = lpa;
        assert_eq!(link_of(&mut d), want, "partner {lpa:#x}");
    }
    // An ability the partner has and this end does not advertise is not
    // the link's.
    let mut d = board();
    d.phy_regs[10] = 0;
    d.phy_regs[4] = 0x0061;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S10, full: true });
}

#[test]
fn a_phy_without_the_gigabit_registers_is_not_asked_for_them() {
    let mut d = board();
    d.phy_regs[1] = 0x786d;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S100, full: true });
    // The status twice, the advertisement and the partner's: four reads.
    assert_eq!(d.writes().len(), 2 * 4);
}

#[test]
fn no_link_and_a_link_still_negotiating() {
    let mut d = board();
    d.phy_regs[1] = 0x7949;
    assert_eq!(link_of(&mut d), Link::Down);
    assert_eq!(d.writes().len(), 2 * 2);

    let mut d = board();
    d.phy_regs[1] = 0x794d;
    assert_eq!(link_of(&mut d), Link::Negotiating);
}

#[test]
fn the_second_status_reading_is_the_one_believed() {
    // The link bit latches low: an old drop reads once.
    let mut d = board();
    d.bmsr_first = Some(0x7949);
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S1000, full: true });
    // And a link that has gone reads up once at most.
    let mut d = board();
    d.bmsr_first = Some(0x796d);
    d.phy_regs[1] = 0x7949;
    assert_eq!(link_of(&mut d), Link::Down);
}

#[test]
fn a_status_of_all_ones_is_no_phy_not_a_gigabit_link() {
    // Nothing drives the bus and nothing flags it: every register reads
    // ones, which has the link, negotiated and gigabit bits all set.
    let mut d = board();
    d.mdio_ones = true;
    assert_eq!(link(&mut d, 2), Err(MdioError::NoAnswer));
    // Two reads of the status and no more.
    assert_eq!(d.writes().len(), 2 * 2);
    // A PHY whose status is one bit short of all ones is a PHY.
    let mut d = board();
    d.phy_regs[1] = 0xfffd;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S1000, full: true });
}

#[test]
fn a_command_that_takes_some_polls_is_waited_for() {
    let mut d = board();
    d.mdio_busy = 5;
    assert_eq!(link_of(&mut d), Link::Up { speed: Speed::S1000, full: true });
    // Four reads, six polls each, a wait before each poll.
    assert_eq!(d.delayed_us(), 4 * 6 * 10);
}

#[test]
fn a_phy_that_does_not_answer_and_a_bus_that_sticks_are_errors() {
    let mut d = board();
    assert_eq!(link(&mut d, 2), Err(MdioError::NoAnswer));
    let mut d = board();
    d.mdio_stuck = true;
    assert_eq!(link(&mut d, 1), Err(MdioError::Busy));
    // One command's budget, not six.
    assert_eq!(d.delayed_us(), 20_000);
}

#[test]
fn the_speeds_in_megabits() {
    assert_eq!([Speed::S10.mbps(), Speed::S100.mbps(), Speed::S1000.mbps()], [10, 100, 1000]);
}

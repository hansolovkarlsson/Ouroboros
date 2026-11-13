extern crate std;
use super::*;
use std::vec::Vec;

/// A model of the device, written from the same two sources as the code
/// under test: a register file, and an MDIO bus behind `MDIO_CMD` that
/// completes a started command after `busy_polls` reads. It records every
/// write and every delay, which is what the tests of "writes nothing else"
/// and "bounded" read.
struct Model {
    sys_rev: u32,
    mac0: u32,
    mac1: u32,
    umac_cmd: u32,
    /// (address, id1, id2, the two status readings in turn).
    phys: Vec<(u8, u16, u16, [u16; 2])>,
    bmsr_reads: usize,
    /// Whether an empty address sets `MDIO_READ_FAIL` or just reads ones.
    flags_no_answer: bool,
    /// Reads of `MDIO_CMD` a command stays busy for; `u32::MAX` is forever.
    busy_polls: u32,
    /// MDIO commands started, and how many complete before the bus
    /// sticks busy for good.
    starts: usize,
    stick_after: usize,
    /// An address whose PHY answers its identifier's first half alone.
    partial_at: Option<u8>,
    mdio: u32,
    busy_left: u32,
    writes: Vec<(usize, u32)>,
    reads: Vec<usize>,
    delayed_us: u64,
}

/// The board as predicted: v5, a Raspberry Pi address, one PHY at 1.
fn pi() -> Model {
    Model {
        sys_rev: 0x0600_0000,
        mac0: 0xdca6_3201,
        mac1: 0x0000_0203,
        umac_cmd: 0,
        phys: std::vec![(1, 0x600d, 0x84a2, [0x7949, 0x796d])],
        bmsr_reads: 0,
        flags_no_answer: true,
        busy_polls: 3,
        starts: 0,
        stick_after: usize::MAX,
        partial_at: None,
        mdio: 0,
        busy_left: 0,
        writes: Vec::new(),
        reads: Vec::new(),
        delayed_us: 0,
    }
}

impl Bus for Model {
    fn read(&mut self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= BLOCK_LEN as usize, "read at {offset:#x}");
        self.reads.push(offset);
        match offset {
            reg::SYS_REV_CTRL => self.sys_rev,
            reg::SYS_PORT_CTRL => 3,
            reg::EXT_RGMII_OOB_CTRL => 0x50,
            reg::INTRL2_0_MASK_STATUS => 0xffff_ffff,
            reg::UMAC_CMD => self.umac_cmd,
            reg::UMAC_MAC0 => self.mac0,
            reg::UMAC_MAC1 => self.mac1,
            reg::RDMA_CTRL | reg::TDMA_CTRL => 0,
            reg::MDIO_CMD => {
                if self.mdio & MDIO_START_BUSY == 0 {
                    return self.mdio;
                }
                if self.busy_left > 0 {
                    self.busy_left = self.busy_left.saturating_sub(if self.busy_polls == u32::MAX { 0 } else { 1 });
                    return self.mdio;
                }
                // The command completes: the data, or the failure.
                let phy = ((self.mdio >> MDIO_PMD_SHIFT) & 0x1f) as u8;
                let r = ((self.mdio >> MDIO_REG_SHIFT) & 0x1f) as u8;
                assert_eq!(self.mdio & (3 << 26), MDIO_RD, "only reads are modelled");
                let command = self.mdio & !MDIO_START_BUSY & !0xffff;
                let found = self.phys.iter().find(|p| p.0 == phy).copied().filter(|_| self.partial_at != Some(phy) || r == PHY_ID1);
                self.mdio = match found {
                    Some((_, id1, id2, bmsr)) => {
                        command
                            | u32::from(match r {
                                PHY_ID1 => id1,
                                PHY_ID2 => id2,
                                PHY_BMSR => {
                                    self.bmsr_reads += 1;
                                    bmsr[(self.bmsr_reads - 1).min(1)]
                                }
                                _ => 0,
                            })
                    }
                    None if self.flags_no_answer => command | MDIO_READ_FAIL,
                    None => command | 0xffff,
                };
                self.mdio
            }
            other => panic!("read of a register the scan has no business with: {other:#x}"),
        }
    }

    fn write(&mut self, offset: usize, value: u32) {
        self.writes.push((offset, value));
        if offset == reg::MDIO_CMD {
            self.mdio = value;
            if value & MDIO_START_BUSY != 0 {
                self.starts += 1;
                if self.starts > self.stick_after {
                    self.busy_polls = u32::MAX;
                }
                self.busy_left = self.busy_polls;
            }
        }
    }

    fn delay_us(&mut self, us: u32) {
        self.delayed_us += u64::from(us);
    }
}

#[test]
fn the_version_is_bits_27_to_24_and_19_to_16() {
    assert_eq!(version(0x0600_0000), (6, 0));
    assert_eq!(version(0xf6f2_ffff), (6, 2));
    assert_eq!(version(0x0500_0000).0, 5);
    assert_ne!(version(0x6000_0000).0, MAJOR_V5);
}

#[test]
fn the_address_reads_as_u_boot_writes_it() {
    // U-Boot: MAC0 = a0<<24 | a1<<16 | a2<<8 | a3, MAC1 = a4<<8 | a5.
    let a = [0xdc, 0xa6, 0x32, 0x01, 0x02, 0x03];
    let mac0 = u32::from(a[0]) << 24 | u32::from(a[1]) << 16 | u32::from(a[2]) << 8 | u32::from(a[3]);
    let mac1 = u32::from(a[4]) << 8 | u32::from(a[5]);
    assert_eq!(mac(mac0, mac1), a);
    // The high half of MAC1 is not part of it.
    assert_eq!(mac(mac0, mac1 | 0xffff_0000), a);
}

#[test]
fn an_address_no_station_has_is_not_usable() {
    assert!(mac_usable(&[0xdc, 0xa6, 0x32, 0x01, 0x02, 0x03]));
    assert!(!mac_usable(&[0; 6]));
    assert!(!mac_usable(&[0xff; 6]));
    // A group address: the first byte's low bit.
    assert!(!mac_usable(&[0x01, 0x00, 0x5e, 0x00, 0x00, 0x01]));
    // Zeros but for the last byte is still an address.
    assert!(mac_usable(&[0, 0, 0, 0, 0, 1]));
}

#[test]
fn the_read_command_is_u_boots() {
    // MDIO_RD | (addr << MDIO_PMD_SHIFT) | (reg << MDIO_REG_SHIFT), with
    // U-Boot's values: (2 << 26) | (1 << 21) | (2 << 16).
    assert_eq!(mdio_read_command(1, 2), Some(0x0822_0000));
    assert_eq!(mdio_read_command(31, 31), Some(0x0800_0000 | 31 << 21 | 31 << 16));
    assert_eq!(mdio_read_command(32, 0), None);
    assert_eq!(mdio_read_command(0, 32), None);
}

#[test]
fn an_mdio_read_writes_the_command_then_starts_it() {
    let mut m = pi();
    assert_eq!(mdio_read(&mut m, 1, PHY_ID1), Ok(0x600d));
    assert_eq!(m.writes, [(reg::MDIO_CMD, 0x0822_0000), (reg::MDIO_CMD, 0x2822_0000)]);
    // Three busy polls and the one that saw it clear, a wait before each.
    assert_eq!(m.delayed_us, 4 * u64::from(MDIO_POLL_US));
}

#[test]
fn an_address_out_of_range_writes_nothing() {
    let mut m = pi();
    assert_eq!(mdio_read(&mut m, 32, PHY_ID1), Err(MdioError::BadAddress));
    assert_eq!(mdio_read(&mut m, 1, 32), Err(MdioError::BadAddress));
    assert!(m.writes.is_empty() && m.reads.is_empty());
}

#[test]
fn a_start_bit_that_never_clears_ends_in_an_error_after_the_budget() {
    let mut m = pi();
    m.busy_polls = u32::MAX;
    assert_eq!(mdio_read(&mut m, 1, PHY_ID1), Err(MdioError::Busy));
    assert_eq!(m.reads.len(), MDIO_POLLS as usize);
    assert_eq!(m.delayed_us, u64::from(MDIO_POLLS) * u64::from(MDIO_POLL_US));
    assert_eq!(m.delayed_us, 20_000, "20 ms, U-Boot's budget");
}

#[test]
fn a_command_done_on_the_last_poll_is_read() {
    let mut m = pi();
    m.busy_polls = MDIO_POLLS - 1;
    assert_eq!(mdio_read(&mut m, 1, PHY_ID1), Ok(0x600d));
    m.busy_polls = MDIO_POLLS;
    assert_eq!(mdio_read(&mut m, 1, PHY_ID1), Err(MdioError::Busy));
}

#[test]
fn a_flagged_read_is_no_answer() {
    let mut m = pi();
    assert_eq!(mdio_read(&mut m, 2, PHY_ID1), Err(MdioError::NoAnswer));
}

/// Both halves of the scan, as the kernel runs them.
fn scan(m: &mut Model) -> (Registers, Option<PhyScan>) {
    let r = registers(m, &mut |_, _, _| {});
    let p = phys(m, &r);
    if let Some(p) = &p {
        // Every address is in exactly one count, up to where the scan ended.
        let counted = p.phys().len() + p.dropped + p.partial + p.no_answer + p.read_ones + p.read_zero;
        assert_eq!(counted, p.stuck_at.map_or(32, usize::from), "{p:?}");
    }
    for (offset, value) in &m.writes {
        assert!(scan_write_allowed(*offset, *value), "the scan wrote {value:#x} at {offset:#x}");
    }
    (r, p)
}

#[test]
fn the_scans_write_rule_takes_mdio_reads_alone() {
    let read = mdio_read_command(31, 31).unwrap();
    assert!(scan_write_allowed(reg::MDIO_CMD, read));
    assert!(scan_write_allowed(reg::MDIO_CMD, read | MDIO_START_BUSY));
    assert!(scan_write_allowed(reg::MDIO_CMD, mdio_read_command(0, 0).unwrap()));
    // Another register, however harmless the value.
    assert!(!scan_write_allowed(reg::UMAC_CMD, read));
    assert!(!scan_write_allowed(reg::MDIO_CMD + 4, read));
    // A write operation (bit 26), alone or beside the read bit; no operation.
    assert!(!scan_write_allowed(reg::MDIO_CMD, (read & !MDIO_RD) | (1 << 26)));
    assert!(!scan_write_allowed(reg::MDIO_CMD, read | (1 << 26)));
    assert!(!scan_write_allowed(reg::MDIO_CMD, read & !MDIO_RD));
    // Data, the fail flag, or a bit above the start bit.
    assert!(!scan_write_allowed(reg::MDIO_CMD, read | 1));
    assert!(!scan_write_allowed(reg::MDIO_CMD, read | 0x8000));
    assert!(!scan_write_allowed(reg::MDIO_CMD, read | MDIO_READ_FAIL));
    assert!(!scan_write_allowed(reg::MDIO_CMD, read | (1 << 30)));
}

const PI_PHY: Phy = Phy { addr: 1, id: 0x600d_84a2, bmsr: 0x796d };

#[test]
fn the_scan_reads_the_board_as_predicted() {
    let mut m = pi();
    let (r, p) = scan(&mut m);
    let p = p.expect("v5");
    assert!(r.is_v5());
    assert_eq!(version(r.sys_rev), (MAJOR_V5, 0));
    assert_eq!(mac(r.mac0, r.mac1), [0xdc, 0xa6, 0x32, 0x01, 0x02, 0x03]);
    assert_eq!((r.port_ctrl, r.rgmii_oob, r.intr_mask, r.umac_cmd, r.rdma_ctrl, r.tdma_ctrl), (3, 0x50, 0xffff_ffff, 0, 0, 0));
    // The status is the second reading: the link bit latches low.
    assert_eq!(p.phys(), &[PI_PHY]);
    assert_eq!(p.phys()[0].bmsr & BMSR_LINK, BMSR_LINK);
    assert_eq!((p.dropped, p.partial, p.no_answer, p.read_ones, p.read_zero, p.stuck_at), (0, 0, 31, 0, 0, None));
}

#[test]
fn each_register_is_reported_as_it_is_read_the_version_first() {
    // The callback sees a value before the next register is read: what a
    // caller logs there survives a later read that faults.
    let mut m = pi();
    let mut order = Vec::new();
    let mut reads_at_callback = Vec::new();
    let reads = core::cell::Cell::new(0usize);
    struct Counting<'a>(&'a mut Model, &'a core::cell::Cell<usize>);
    impl Bus for Counting<'_> {
        fn read(&mut self, offset: usize) -> u32 {
            self.1.set(self.1.get() + 1);
            self.0.read(offset)
        }
        fn write(&mut self, offset: usize, value: u32) {
            self.0.write(offset, value)
        }
        fn delay_us(&mut self, us: u32) {
            self.0.delay_us(us)
        }
    }
    registers(&mut Counting(&mut m, &reads), &mut |offset, name, value| {
        order.push((offset, name, value));
        reads_at_callback.push(reads.get());
    });
    assert_eq!(order.len(), 9);
    assert_eq!(order[0], (reg::SYS_REV_CTRL, "SYS_REV_CTRL", 0x0600_0000));
    assert_eq!(order[5], (reg::UMAC_MAC0, "UMAC_MAC0", 0xdca6_3201));
    assert_eq!(order.iter().map(|o| o.0).collect::<Vec<_>>(), SCAN_REGISTERS.iter().map(|r| r.0).collect::<Vec<_>>());
    assert_eq!(reads_at_callback, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert!(m.writes.is_empty(), "reading the registers wrote");
}

#[test]
fn the_scan_writes_only_mdio_commands_and_only_reads() {
    let mut m = pi();
    scan(&mut m);
    assert!(!m.writes.is_empty());
    for (offset, value) in &m.writes {
        assert_eq!(*offset, reg::MDIO_CMD);
        assert_eq!(value & (3 << 26), MDIO_RD, "a read operation");
        assert_eq!(value & 0xffff, 0, "no data sent");
    }
    // 32 identifier reads, then three more for the one PHY, two writes each.
    assert_eq!(m.writes.len(), 2 * (32 + 3));
}

#[test]
fn any_other_version_reads_the_version_alone_and_writes_nothing() {
    for rev in [0x0500_0000, 0x0000_0000, 0x0700_0000, 0xffff_ffff] {
        let mut m = pi();
        m.sys_rev = rev;
        let (r, p) = scan(&mut m);
        assert_eq!(r, Registers { sys_rev: rev, ..Registers::default() });
        assert!(!r.is_v5());
        assert_eq!(p, None);
        assert_eq!(m.reads, [reg::SYS_REV_CTRL], "version {rev:#x}: a v5 offset was read");
        assert!(m.writes.is_empty(), "version {rev:#x} was written to");
        assert_eq!(m.delayed_us, 0);
    }
}

#[test]
fn a_dead_mdio_bus_costs_one_commands_wait() {
    let mut m = pi();
    m.busy_polls = u32::MAX;
    let p = scan(&mut m).1.unwrap();
    assert_eq!(p.stuck_at, Some(0));
    assert!(p.phys().is_empty());
    assert_eq!(m.delayed_us, 20_000);
}

#[test]
fn an_empty_address_reading_ones_is_no_phy() {
    let mut m = pi();
    m.flags_no_answer = false;
    let p = scan(&mut m).1.unwrap();
    assert_eq!(p.phys(), &[PI_PHY]);
    assert_eq!((p.read_ones, p.no_answer, p.read_zero), (31, 0, 0));
}

#[test]
fn a_bus_reading_zeros_is_no_phy_and_is_counted() {
    // Every address answers zero with no flag: a bus held low.
    let mut m = pi();
    m.phys = (0..32).map(|a| (a, 0, 0, [0, 0])).collect();
    let p = scan(&mut m).1.unwrap();
    assert!(p.phys().is_empty());
    assert_eq!((p.read_zero, p.dropped, p.partial), (32, 0, 0));
    // One identifier read an address, and no more.
    assert_eq!(m.starts, 32);
}

#[test]
fn more_phys_than_kept_are_counted() {
    let mut m = pi();
    m.phys = (0..6).map(|a| (a, 0x600d, 0x84a0 | u16::from(a), [0x7949, 0x7949])).collect();
    let p = scan(&mut m).1.unwrap();
    assert_eq!(p.phys().len(), MAX_PHYS);
    assert_eq!(p.dropped, 2);
    assert_eq!(p.phys().iter().map(|p| p.addr).collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(p.phys()[3].id, 0x600d_84a3);
}

#[test]
fn no_phy_at_all_is_an_empty_list_not_an_error() {
    let mut m = pi();
    m.phys.clear();
    let p = scan(&mut m).1.unwrap();
    assert!(p.phys().is_empty());
    assert_eq!((p.no_answer, p.stuck_at), (32, None));
}

#[test]
fn a_bus_that_sticks_inside_a_phy_ends_the_scan_there() {
    // Address 0's read and address 1's first complete; the next never does.
    let mut m = pi();
    m.stick_after = 2;
    let p = scan(&mut m).1.unwrap();
    assert_eq!(p.stuck_at, Some(1));
    assert!(p.phys().is_empty(), "a PHY half read is not kept");
    assert_eq!(m.starts, 3, "nothing started after the stuck command");
}

#[test]
fn a_phy_that_answers_in_part_is_counted_not_lost() {
    // Address 1 gives its identifier's first half and then stops
    // answering; address 5 is whole.
    let mut m = pi();
    m.phys.push((5, 0x600d, 0x84a5, [0x7949, 0x796d]));
    m.partial_at = Some(1);
    let p = scan(&mut m).1.unwrap();
    assert_eq!(p.phys(), &[Phy { addr: 5, id: 0x600d_84a5, bmsr: 0x796d }]);
    assert_eq!((p.partial, p.first_partial, p.no_answer), (1, Some(1), 30));
    assert_eq!(p.stuck_at, None);
}

#[test]
fn a_block_is_usable_only_whole_aligned_and_somewhere() {
    assert!(block_usable(0xfd58_0000, 0x1_0000));
    assert!(block_usable(0xfd58_0000, 0x2_0000));
    assert!(!block_usable(0xfd58_0000, 0xffff));
    assert!(!block_usable(0, 0x1_0000));
    assert!(!block_usable(0xfd58_0002, 0x1_0000));
    // A block whose end would pass 2^64: its last word has no address.
    assert!(!block_usable(u64::MAX - 0xffff, 0x1_0000));
    assert!(block_usable(u64::MAX - 0x1_ffff, 0x1_0000));
}

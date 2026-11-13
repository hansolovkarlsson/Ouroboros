//! Broadcom GENET v5, the Raspberry Pi 4's on-board Ethernet: the logic
//! that needs no hardware, over a [`Bus`] the caller passes
//! (`docs/roadmap/roadmap-pi-net.md`, design 2). No QEMU machine models
//! this device, so everything here runs on the host against a model of it
//! (`tests.rs`) and in the kernel against the board. **The model is ours:**
//! the tests check this code against our reading of the device, its
//! arithmetic, its order and its bounds. They cannot catch a wrong
//! reading. The board is the only foreign observer.
//!
//! Two parts. The scan, [`registers`] and [`phys`], is the arc's step 2:
//! what the device says of itself before anything drives it. The driver's
//! logic is step 4: [`mac::Genet`] (reset, the rings, one transmit, one
//! receive) and [`phy::link`] (the link and its speed). Nothing here touches
//! memory: the packet buffers are the caller's, and this crate deals in
//! their addresses and in indexes.
//!
//! **Where each number comes from.** Every register offset and bit here was
//! read in U-Boot's `drivers/net/bcmgenet.c` (the driver for this board's
//! GENET) or in Linux's `drivers/net/ethernet/broadcom/genet/bcmgenet.h`
//! and `bcmgenet.c`, and each constant's doc names which. The files read
//! were U-Boot's at tag `v2024.10` and Linux's at `v6.6`, and
//! `cited_tests.rs` carries the line of each constant in them. The absolute
//! offsets are those files' block offset plus register offset, added here.
//! The PHY's registers are IEEE 802.3 clause 22's, by the names and values
//! of Linux's `include/uapi/linux/mii.h` at the same tag.
//!
//! Pure: no I/O of its own, no heap, no statics, so it builds and is
//! tested on the host (`make test`).

#![no_std]

/// The registers and the delay a caller lends. An offset is in bytes from
/// the start of the device's register block and is always below
/// [`BLOCK_LEN`] and a multiple of 4.
pub trait Bus {
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
    /// Waits at least `us` microseconds.
    fn delay_us(&mut self, us: u32);
}

/// The length of the register block every offset here lies in: 64 KB, what
/// the Pi's `_CRS` declares and U-Boot maps.
pub const BLOCK_LEN: u32 = 0x1_0000;

/// Register offsets from the start of the block.
pub mod reg {
    /// `SYS_REV_CTRL` (U-Boot): the version.
    pub const SYS_REV_CTRL: usize = 0x0000;
    /// `SYS_PORT_CTRL` (U-Boot): the port mode.
    pub const SYS_PORT_CTRL: usize = 0x0004;
    /// `SYS_RBUF_FLUSH_CTRL`, `GENET_SYS_OFF + 0x08` (U-Boot).
    pub const SYS_RBUF_FLUSH_CTRL: usize = 0x0008;
    /// `EXT_RGMII_OOB_CTRL`, `GENET_EXT_OFF + 0x0c` (U-Boot).
    pub const EXT_RGMII_OOB_CTRL: usize = 0x0080 + 0x0c;
    /// `INTRL2_CPU_CLEAR` of block 0, `GENET_INTRL2_0_OFF + 0x08` (Linux):
    /// a bit written clears that pending interrupt.
    pub const INTRL2_0_CLEAR: usize = 0x0200 + 0x08;
    /// `INTRL2_CPU_MASK_STATUS` of block 0, `GENET_INTRL2_0_OFF + 0x0c`
    /// (Linux): a set bit is a masked interrupt.
    pub const INTRL2_0_MASK_STATUS: usize = 0x0200 + 0x0c;
    /// `INTRL2_CPU_MASK_SET` of block 0, `GENET_INTRL2_0_OFF + 0x10`
    /// (Linux): a bit written masks that interrupt.
    pub const INTRL2_0_MASK_SET: usize = 0x0200 + 0x10;
    /// The same two of block 1, from `GENET_INTRL2_1_OFF` (0x0240, Linux).
    pub const INTRL2_1_CLEAR: usize = 0x0240 + 0x08;
    pub const INTRL2_1_MASK_SET: usize = 0x0240 + 0x10;
    /// `RBUF_CTRL`, `GENET_RBUF_OFF + 0x00` (U-Boot).
    pub const RBUF_CTRL: usize = 0x0300;
    /// `RBUF_TBUF_SIZE_CTRL`, `GENET_RBUF_OFF + 0xb4` (U-Boot).
    pub const RBUF_TBUF_SIZE_CTRL: usize = 0x0300 + 0xb4;
    /// `UMAC_CMD`, `GENET_UMAC_OFF + 0x008` (U-Boot).
    pub const UMAC_CMD: usize = 0x0800 + 0x008;
    /// `UMAC_MAC0`, `GENET_UMAC_OFF + 0x00c` (U-Boot): the address's first
    /// four bytes.
    pub const UMAC_MAC0: usize = 0x0800 + 0x00c;
    /// `UMAC_MAC1`, `GENET_UMAC_OFF + 0x010` (U-Boot): its last two.
    pub const UMAC_MAC1: usize = 0x0800 + 0x010;
    /// `TBUF_CTRL` (Linux): at 0x00 of the transmit buffer block, which on
    /// v5 is at 0x0600 (`tbuf_offset` in `bcmgenet.c`'s `GENET_V5`
    /// parameters, line 3811). U-Boot never names it.
    pub const TBUF_CTRL: usize = 0x0600;
    /// `UMAC_MAX_FRAME_LEN`, `GENET_UMAC_OFF + 0x014` (U-Boot).
    pub const UMAC_MAX_FRAME_LEN: usize = 0x0800 + 0x014;
    /// `UMAC_TX_FLUSH`, `GENET_UMAC_OFF + 0x334` (U-Boot).
    pub const UMAC_TX_FLUSH: usize = 0x0800 + 0x334;
    /// `UMAC_MIB_CTRL`, `GENET_UMAC_OFF + 0x580` (U-Boot).
    pub const UMAC_MIB_CTRL: usize = 0x0800 + 0x580;
    /// `MDIO_CMD`, `GENET_UMAC_OFF + 0x614` (U-Boot).
    pub const MDIO_CMD: usize = 0x0800 + 0x614;

    /// The receive descriptors, `GENET_RX_OFF` (U-Boot): 256 of 12 bytes.
    pub const RX_DESCS: usize = 0x2000;
    /// The transmit descriptors, `GENET_TX_OFF` (U-Boot).
    pub const TX_DESCS: usize = 0x4000;
    /// A descriptor's three words (U-Boot's `DMA_DESC_*`): the length and
    /// status, then the buffer's address, low half and high.
    pub const DESC_LENGTH_STATUS: usize = 0x00;
    pub const DESC_ADDRESS_LO: usize = 0x04;
    pub const DESC_ADDRESS_HI: usize = 0x08;
    /// A descriptor's size, `DMA_DESC_SIZE` (U-Boot).
    pub const DESC_SIZE: usize = 12;

    /// Ring 16's registers in the receive DMA, U-Boot's
    /// `RDMA_RING_REG_BASE`: `GENET_RX_OFF`, the 256 descriptors, then 16
    /// rings of 0x40 bytes.
    pub const RDMA_RING: usize = 0x2000 + 256 * 12 + 0x40 * 16;
    /// Ring 16's registers in the transmit DMA, `TDMA_RING_REG_BASE`.
    pub const TDMA_RING: usize = 0x4000 + 256 * 12 + 0x40 * 16;
    /// The receive ring's registers (U-Boot). The device advances the
    /// producer index and the driver the consumer.
    pub const RDMA_WRITE_PTR: usize = RDMA_RING;
    pub const RDMA_PROD_INDEX: usize = RDMA_RING + 0x08;
    pub const RDMA_CONS_INDEX: usize = RDMA_RING + 0x0c;
    pub const RDMA_RING_BUF_SIZE: usize = RDMA_RING + 0x10;
    pub const RDMA_START_ADDR: usize = RDMA_RING + 0x14;
    pub const RDMA_END_ADDR: usize = RDMA_RING + 0x1c;
    pub const RDMA_XON_XOFF_THRESH: usize = RDMA_RING + 0x28;
    pub const RDMA_READ_PTR: usize = RDMA_RING + 0x2c;
    /// The transmit ring's registers (U-Boot). **Not the receive ring's
    /// layout:** here the consumer index is at 0x08 and the producer at
    /// 0x0c, the read pointer at 0x00 and the write pointer at 0x2c. The
    /// driver advances the producer and the device the consumer.
    pub const TDMA_READ_PTR: usize = TDMA_RING;
    pub const TDMA_CONS_INDEX: usize = TDMA_RING + 0x08;
    pub const TDMA_PROD_INDEX: usize = TDMA_RING + 0x0c;
    pub const TDMA_RING_BUF_SIZE: usize = TDMA_RING + 0x10;
    pub const TDMA_START_ADDR: usize = TDMA_RING + 0x14;
    pub const TDMA_END_ADDR: usize = TDMA_RING + 0x1c;
    pub const TDMA_MBUF_DONE_THRESH: usize = TDMA_RING + 0x24;
    pub const TDMA_FLOW_PERIOD: usize = TDMA_RING + 0x28;
    pub const TDMA_WRITE_PTR: usize = TDMA_RING + 0x2c;
    /// Each DMA's own registers after its 17 rings' (U-Boot's
    /// `RDMA_REG_BASE`, `TDMA_REG_BASE`): `DMA_RING_CFG` at 0x00 and
    /// `DMA_SCB_BURST_SIZE` at 0x0c, with `DMA_CTRL` (below) at 0x04.
    pub const RDMA_RING_CFG: usize = RDMA_RING + 0x40;
    pub const RDMA_SCB_BURST_SIZE: usize = RDMA_RING + 0x40 + 0x0c;
    pub const TDMA_RING_CFG: usize = TDMA_RING + 0x40;
    pub const TDMA_SCB_BURST_SIZE: usize = TDMA_RING + 0x40 + 0x0c;
    /// Receive DMA's `DMA_CTRL`: `GENET_RX_OFF` (0x2000), 256 descriptors
    /// of 12 bytes, 17 rings' registers of 0x40 bytes, then 0x04 (U-Boot's
    /// `RDMA_REG_BASE + DMA_CTRL`).
    pub const RDMA_CTRL: usize = 0x2000 + 256 * 12 + 0x40 * 17 + 0x04;
    /// Transmit DMA's `DMA_CTRL`, the same from `GENET_TX_OFF` (0x4000).
    pub const TDMA_CTRL: usize = 0x4000 + 256 * 12 + 0x40 * 17 + 0x04;
}

const _: () = {
    // The two computed offsets, as U-Boot's own arithmetic gives them.
    assert!(reg::RDMA_CTRL == 0x3044 && reg::TDMA_CTRL == 0x5044);
    assert!(reg::TDMA_CTRL + 4 <= BLOCK_LEN as usize);
    // The rings' registers end where each DMA's own begin, and the
    // transmit descriptors start past the receive DMA's last register.
    assert!(reg::RDMA_RING == 0x3000 && reg::TDMA_RING == 0x5000);
    assert!(reg::RDMA_RING_CFG + 4 == reg::RDMA_CTRL && reg::TDMA_RING_CFG + 4 == reg::TDMA_CTRL);
    assert!(reg::RDMA_SCB_BURST_SIZE < reg::TX_DESCS);
};

pub mod mac;
pub mod phy;

/// `CMD_TX_EN`, `UMAC_CMD` bit 0 (U-Boot).
pub const CMD_TX_EN: u32 = 1 << 0;
/// `CMD_RX_EN`, `UMAC_CMD` bit 1 (U-Boot).
pub const CMD_RX_EN: u32 = 1 << 1;
/// `DMA_EN`, bit 0 of either `DMA_CTRL` (U-Boot).
pub const DMA_EN: u32 = 1 << 0;

/// `MDIO_START_BUSY` (U-Boot): written to start, reads set until done.
pub const MDIO_START_BUSY: u32 = 1 << 29;
/// `MDIO_READ_FAIL` (U-Boot defines it, Linux's MDIO driver tests it): no
/// PHY drove the bus for this read.
pub const MDIO_READ_FAIL: u32 = 1 << 28;
/// `MDIO_RD` (U-Boot): the read operation.
pub const MDIO_RD: u32 = 2 << 26;
/// `MDIO_PMD_SHIFT` (U-Boot): the PHY's address, five bits.
pub const MDIO_PMD_SHIFT: u32 = 21;
/// `MDIO_REG_SHIFT` (U-Boot): the register, five bits.
pub const MDIO_REG_SHIFT: u32 = 16;

/// The version major `SYS_REV_CTRL` reads on GENET v5, the only one
/// U-Boot's driver accepts (`major != 6` is its "unsupported").
pub const MAJOR_V5: u8 = 6;

/// The version major, `SYS_REV_CTRL` bits 27:24, and minor, bits 19:16
/// (U-Boot's probe).
pub fn version(sys_rev: u32) -> (u8, u8) {
    (((sys_rev >> 24) & 0x0f) as u8, ((sys_rev >> 16) & 0x0f) as u8)
}

/// The station address the two registers hold: `UMAC_MAC0` is bytes 0 to 3
/// from its top byte down, `UMAC_MAC1` bytes 4 and 5 in its low half
/// (U-Boot's `bcmgenet_gmac_write_hwaddr`, read the other way).
pub fn mac(mac0: u32, mac1: u32) -> [u8; 6] {
    let a = mac0.to_be_bytes();
    let b = mac1.to_be_bytes();
    [a[0], a[1], a[2], a[3], b[2], b[3]]
}

/// Whether `mac` can be a station's address: not all zeros, and not a
/// group address (the first byte's low bit), which all ones is too.
pub fn mac_usable(mac: &[u8; 6]) -> bool {
    mac.iter().any(|&b| b != 0) && mac[0] & 1 == 0
}

/// How an MDIO read ended without a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MdioError {
    /// The PHY address or the register is above 31.
    BadAddress,
    /// The start bit never cleared within [`MDIO_POLLS`] polls.
    Busy,
    /// `MDIO_READ_FAIL`: no PHY answered at that address.
    NoAnswer,
}

/// The most polls of the start bit one MDIO command gets, each after a
/// [`MDIO_POLL_US`] wait: 20 ms in all, U-Boot's `wait_for_bit` budget.
pub const MDIO_POLLS: u32 = 2000;
/// The wait before each poll, in microseconds.
pub const MDIO_POLL_US: u32 = 10;

/// The command word that reads register `reg` of the PHY at `phy`, without
/// the start bit (U-Boot's `bcmgenet_mdio_read`).
pub fn mdio_read_command(phy: u8, reg: u8) -> Option<u32> {
    (phy < 32 && reg < 32).then(|| MDIO_RD | (u32::from(phy) << MDIO_PMD_SHIFT) | (u32::from(reg) << MDIO_REG_SHIFT))
}

/// Reads register `reg` of the PHY at `phy`: the command written, then the
/// command with the start bit (as U-Boot does, in two writes), then the
/// start bit polled until it clears. Two writes to `MDIO_CMD` and nothing
/// else written.
pub fn mdio_read<B: Bus>(bus: &mut B, phy: u8, reg: u8) -> Result<u16, MdioError> {
    let command = mdio_read_command(phy, reg).ok_or(MdioError::BadAddress)?;
    bus.write(reg::MDIO_CMD, command);
    bus.write(reg::MDIO_CMD, command | MDIO_START_BUSY);
    for _ in 0..MDIO_POLLS {
        bus.delay_us(MDIO_POLL_US);
        let v = bus.read(reg::MDIO_CMD);
        if v & MDIO_START_BUSY == 0 {
            return if v & MDIO_READ_FAIL != 0 { Err(MdioError::NoAnswer) } else { Ok(v as u16) };
        }
    }
    Err(MdioError::Busy)
}

/// Whether a write of `value` at `offset` is one the scan ([`registers`]
/// and [`phys`]) makes: to `MDIO_CMD`, a read operation for some PHY and
/// register, with or without the start bit, and no other bit. A [`Bus`]
/// lent for the scan alone refuses any other write, so that "read-only"
/// does not rest on what this crate's other functions will one day do.
pub fn scan_write_allowed(offset: usize, value: u32) -> bool {
    let address = (0x1f << MDIO_PMD_SHIFT) | (0x1f << MDIO_REG_SHIFT);
    offset == reg::MDIO_CMD && value & !(MDIO_START_BUSY | address) == MDIO_RD
}

/// The PHY registers [`phys`] reads, by IEEE 802.3 clause 22: the status
/// register and the identifier's two halves.
pub const PHY_BMSR: u8 = 1;
pub const PHY_ID1: u8 = 2;
pub const PHY_ID2: u8 = 3;
/// The status register's link bit. It latches low, so it is read twice and
/// the second reading kept.
pub const BMSR_LINK: u16 = 1 << 2;
/// The status register's autonegotiation-complete bit.
pub const BMSR_ANEG_DONE: u16 = 1 << 5;

/// One PHY found on the MDIO bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Phy {
    /// Its address, 0 to 31.
    pub addr: u8,
    /// Its identifier, register 2 in the high half and register 3 in the
    /// low.
    pub id: u32,
    /// Its status register, the second of two readings.
    pub bmsr: u16,
}

/// The most PHYs [`phys`] keeps; the board has one.
pub const MAX_PHYS: usize = 4;

/// Whether a declared register block can be this device's: a base that is
/// not zero, is aligned for 32-bit access and leaves room for the block
/// below 2^64, and a length of at least [`BLOCK_LEN`]. A caller checks
/// this before it lends a [`Bus`] over the block.
pub fn block_usable(base: u64, size: u32) -> bool {
    base != 0 && base.is_multiple_of(4) && base.checked_add(u64::from(BLOCK_LEN)).is_some() && size >= BLOCK_LEN
}

/// The registers [`registers`] reads, in the order it reads them, each
/// with its name. The version is first, alone: the rest are read only on
/// v5, whose layout their offsets are.
pub const SCAN_REGISTERS: [(usize, &str); 9] = [
    (reg::SYS_REV_CTRL, "SYS_REV_CTRL"),
    (reg::SYS_PORT_CTRL, "SYS_PORT_CTRL"),
    (reg::EXT_RGMII_OOB_CTRL, "EXT_RGMII_OOB_CTRL"),
    (reg::INTRL2_0_MASK_STATUS, "INTRL2_0 mask status"),
    (reg::UMAC_CMD, "UMAC_CMD"),
    (reg::UMAC_MAC0, "UMAC_MAC0"),
    (reg::UMAC_MAC1, "UMAC_MAC1"),
    (reg::RDMA_CTRL, "RDMA DMA_CTRL"),
    (reg::TDMA_CTRL, "TDMA DMA_CTRL"),
];

/// What [`registers`] read. On any version but v5 only `sys_rev` was read
/// and the rest are zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Registers {
    pub sys_rev: u32,
    pub port_ctrl: u32,
    pub rgmii_oob: u32,
    pub intr_mask: u32,
    pub umac_cmd: u32,
    pub mac0: u32,
    pub mac1: u32,
    pub rdma_ctrl: u32,
    pub tdma_ctrl: u32,
}

impl Registers {
    /// Whether the version read is v5, the one layout this file knows.
    pub fn is_v5(&self) -> bool {
        version(self.sys_rev).0 == MAJOR_V5
    }
}

/// Reads what the device's registers say of it, writing nothing: the
/// version, and then, **only when it reads v5**, the eight other registers
/// of [`SCAN_REGISTERS`], none whose read changes anything. `seen` is
/// called with each value as soon as it is read, before the next read, so
/// a caller that logs there keeps every value read before a read that
/// faults or never returns.
pub fn registers<B: Bus>(bus: &mut B, seen: &mut dyn FnMut(usize, &'static str, u32)) -> Registers {
    let mut v = [0u32; SCAN_REGISTERS.len()];
    for (slot, (offset, name)) in v.iter_mut().zip(SCAN_REGISTERS) {
        *slot = bus.read(offset);
        seen(offset, name, *slot);
        if offset == reg::SYS_REV_CTRL && version(*slot).0 != MAJOR_V5 {
            break;
        }
    }
    let [sys_rev, port_ctrl, rgmii_oob, intr_mask, umac_cmd, mac0, mac1, rdma_ctrl, tdma_ctrl] = v;
    Registers { sys_rev, port_ctrl, rgmii_oob, intr_mask, umac_cmd, mac0, mac1, rdma_ctrl, tdma_ctrl }
}

/// What [`phys`] found on the MDIO bus. The kept list, the five counts and
/// the stuck address account for every address: each either answered whole
/// (kept, or counted in `dropped`), answered in part, was flagged as no
/// answer, read all ones, read zero, or was never reached. So the six sum
/// to 32, or to the stuck address when there is one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PhyScan {
    list: [Phy; MAX_PHYS],
    count: usize,
    /// PHYs that answered whole past [`MAX_PHYS`], not kept.
    pub dropped: usize,
    /// Addresses whose identifier's first half answered and whose second
    /// half or status then did not: something is there and is not well.
    pub partial: usize,
    /// The first such address.
    pub first_partial: Option<u8>,
    /// Addresses the controller flagged as not answering.
    pub no_answer: usize,
    /// Addresses that read all ones with no flag: nothing drives the bus.
    pub read_ones: usize,
    /// Addresses that read zero with no flag. No PHY's identifier is zero,
    /// so thirty-two of these is a bus held low, not thirty-two PHYs.
    pub read_zero: usize,
    /// The address at which a command's start bit never cleared, which
    /// ended the scan there.
    pub stuck_at: Option<u8>,
}

impl PhyScan {
    /// The PHYs that answered whole, by address.
    pub fn phys(&self) -> &[Phy] {
        &self.list[..self.count]
    }
}

/// One address's answer.
enum Answer {
    Phy(Phy),
    Partial,
    Silent,
    Ones,
    Zero,
}

/// Reads the PHY at `addr`: its identifier's first half, and when that is
/// neither absent, all ones nor zero, its second half and its status
/// twice. `Err` only for a start bit that never cleared.
fn read_phy<B: Bus>(bus: &mut B, addr: u8) -> Result<Answer, MdioError> {
    let id1 = match mdio_read(bus, addr, PHY_ID1) {
        Ok(0xffff) => return Ok(Answer::Ones),
        Ok(0) => return Ok(Answer::Zero),
        Ok(v) => v,
        Err(MdioError::Busy) => return Err(MdioError::Busy),
        Err(_) => return Ok(Answer::Silent),
    };
    let mut rest = [0u16; 3];
    for (slot, reg) in rest.iter_mut().zip([PHY_ID2, PHY_BMSR, PHY_BMSR]) {
        *slot = match mdio_read(bus, addr, reg) {
            Ok(v) => v,
            Err(MdioError::Busy) => return Err(MdioError::Busy),
            Err(_) => return Ok(Answer::Partial),
        };
    }
    Ok(Answer::Phy(Phy { addr, id: (u32::from(id1) << 16) | u32::from(rest[0]), bmsr: rest[2] }))
}

/// Scans the MDIO bus's thirty-two addresses for PHYs, and returns `None`
/// having written nothing unless `registers` read v5: the MDIO register is
/// where v5 has it. The read commands are the only writes, each to
/// `MDIO_CMD` alone. A start bit that never clears ends the scan at that
/// address, so a dead bus costs one command's wait and not thirty-two.
///
/// Nothing else may use the MDIO bus meanwhile: a command of another
/// driver's between this one's start and its poll would be read as this
/// one's answer. The caller keeps the firmware's driver out.
pub fn phys<B: Bus>(bus: &mut B, registers: &Registers) -> Option<PhyScan> {
    if !registers.is_v5() {
        return None;
    }
    let mut s = PhyScan::default();
    for addr in 0..32u8 {
        match read_phy(bus, addr) {
            Ok(Answer::Phy(phy)) if s.count < MAX_PHYS => {
                s.list[s.count] = phy;
                s.count += 1;
            }
            Ok(Answer::Phy(_)) => s.dropped += 1,
            Ok(Answer::Partial) => {
                s.partial += 1;
                s.first_partial.get_or_insert(addr);
            }
            Ok(Answer::Silent) => s.no_answer += 1,
            Ok(Answer::Ones) => s.read_ones += 1,
            Ok(Answer::Zero) => s.read_zero += 1,
            Err(_) => {
                s.stuck_at = Some(addr);
                break;
            }
        }
    }
    Some(s)
}

#[cfg(test)]
mod cited_tests;
#[cfg(test)]
mod mac_tests;
#[cfg(test)]
mod tests;

//! The driver's logic: the device made quiet, reset and set up, one frame
//! sent, one frame received. The sequence is U-Boot's
//! `drivers/net/bcmgenet.c` (`bcmgenet_eth_probe`, then
//! `bcmgenet_gmac_eth_start`), which drives this board's device with one
//! queue and no interrupt, as this arc does first.
//!
//! **Where this departs from U-Boot, each on purpose:**
//!
//! - The station address is read before the reset and written back after
//!   it, where U-Boot is handed one from its environment. An address no
//!   station can have ends [`Genet::init`] before any write.
//! - Every interrupt of both blocks is masked and cleared first, as Linux's
//!   `bcmgenet_intr_disable` does. U-Boot never touches them. The kernel
//!   polls this device (the plan's design 7), and a firmware that left one
//!   unmasked must not be able to raise it.
//! - `RBUF_64B_EN` is cleared where U-Boot only sets the bit beside it, and
//!   `TBUF_64B_EN`, which U-Boot never touches, is cleared too. Linux sets
//!   both. Had the firmware's driver done as Linux does, every received
//!   frame would carry a 64-byte status block this code does not skip, and
//!   the first 64 bytes of every frame sent would be taken for one.
//! - The receive DMA's enables for rings 0 to 15 are cleared where U-Boot
//!   only sets ring 16's. This driver sets up ring 16 alone.
//! - A producer index is taken as its low 16 bits (Linux's
//!   `DMA_P_INDEX_MASK`). The receive one carries a count of discarded
//!   frames in its high half, which U-Boot compares along with the index.
//! - A received frame is refused unless it is whole, free of the device's
//!   error flags and inside its buffer, as Linux's receive path checks.
//!   U-Boot hands up whatever length the descriptor reads.
//! - A buffer's address is computed from its index. U-Boot reads it back
//!   from the descriptor, which is the device's to have changed.
//! - A send waits for the device's consumer index to **equal** the
//!   producer, wrapped to 16 bits. U-Boot's `cons < prod` with an unwrapped
//!   producer never ends at the 65536th frame. The wait is bounded in time
//!   ([`TX_POLLS`]), where U-Boot's is 100 reads.
//! - A send is refused while an earlier one is still with the device: all
//!   sends share one buffer, and rewriting it under a frame not yet taken
//!   would send the wrong bytes.
//!
//! **What this does not do:** it writes nothing to the PHY (`phy.rs` says
//! why that is an open question), and it does not tell the MAC the duplex
//! (U-Boot does not either: a half-duplex link runs with the MAC at full).

use crate::phy::Speed;
use crate::{mac, mac_usable, reg, version, Bus, CMD_RX_EN, CMD_TX_EN, DMA_EN, MAJOR_V5};

/// Descriptors in each direction, `TOTAL_DESCS` (U-Boot), all given to
/// ring 16.
pub const DESCS: usize = 256;
/// The default queue, `DEFAULT_Q` (U-Boot): the one ring this driver uses.
pub const DEFAULT_Q: u32 = 16;
/// One receive buffer, `RX_BUF_LENGTH` (U-Boot).
pub const RX_BUF_LEN: usize = 2048;
/// The bytes the device puts before a received frame, `RX_BUF_OFFSET`
/// (U-Boot), by `RBUF_ALIGN_2B`.
pub const RX_PAD: usize = 2;
/// The receive buffers' memory, which the caller owns: [`DESCS`] buffers
/// of [`RX_BUF_LEN`], one after another. Half a megabyte. A ring of fewer
/// descriptors would need less, and no source read here sets one up on
/// this device, so the size is U-Boot's.
pub const RX_AREA_LEN: usize = DESCS * RX_BUF_LEN;
/// The longest received frame this can return: a buffer less the pad.
pub const RX_MAX_FRAME: usize = RX_BUF_LEN - RX_PAD;
/// `ENET_MAX_MTU_SIZE` (U-Boot): 1500 of data, 14 of header, 4 of VLAN tag,
/// 6 of Broadcom tag, 4 of checksum and 8 of pad. Written to
/// `UMAC_MAX_FRAME_LEN`.
pub const MAX_FRAME_LEN: u32 = 1500 + 14 + 4 + 6 + 4 + 8;
/// The shortest frame a send takes: an Ethernet header, this crate's own
/// floor. A frame shorter than Ethernet's 60 is the device's to pad:
/// Linux's `bcmgenet_xmit` says software padding of runt packets would
/// only be needed without `DMA_TX_APPEND_CRC` (`bcmgenet.c` line 2132),
/// and U-Boot pads nothing either.
pub const TX_MIN_FRAME: usize = 14;
/// The longest frame a send takes: [`MAX_FRAME_LEN`] less the checksum.
pub const TX_MAX_FRAME: usize = MAX_FRAME_LEN as usize - 4;

/// `PORT_MODE_EXT_GPHY` (U-Boot): an external gigabit PHY, in
/// `SYS_PORT_CTRL`.
pub const PORT_MODE_EXT_GPHY: u32 = 3;
/// The flush bit of `SYS_RBUF_FLUSH_CTRL`, U-Boot's `BIT(1)`.
pub const RBUF_FLUSH: u32 = 1 << 1;
/// `CMD_SW_RESET` and `CMD_LCL_LOOP_EN`, `UMAC_CMD` bits 13 and 15
/// (U-Boot): the reset is issued with local loopback for a stable clock.
pub const CMD_SW_RESET: u32 = 1 << 13;
pub const CMD_LCL_LOOP_EN: u32 = 1 << 15;
/// `CMD_SPEED_SHIFT` (U-Boot): where `UMAC_CMD` takes a [`Speed`].
pub const CMD_SPEED_SHIFT: u32 = 2;
/// `MIB_RESET_RX`, `_RUNT`, `_TX` (U-Boot), together.
pub const MIB_RESET_ALL: u32 = 0b111;
/// `RBUF_64B_EN` (Linux) and `RBUF_ALIGN_2B` (U-Boot), in `RBUF_CTRL`.
pub const RBUF_64B_EN: u32 = 1 << 0;
/// `TBUF_64B_EN` (Linux), in `TBUF_CTRL`.
pub const TBUF_64B_EN: u32 = 1 << 0;
pub const RBUF_ALIGN_2B: u32 = 1 << 1;
/// `RGMII_LINK`, `OOB_DISABLE`, `RGMII_MODE_EN`, `ID_MODE_DIS` (U-Boot), in
/// `EXT_RGMII_OOB_CTRL`.
pub const RGMII_LINK: u32 = 1 << 4;
pub const OOB_DISABLE: u32 = 1 << 5;
pub const RGMII_MODE_EN: u32 = 1 << 6;
pub const ID_MODE_DIS: u32 = 1 << 16;

/// `DMA_CTRL`'s enable bit for ring 16: `DMA_RING_BUF_EN_SHIFT` is 1
/// (U-Boot), so ring `n` is bit `n + 1`.
pub const DMA_RING16_EN: u32 = 1 << (DEFAULT_Q + 1);
/// `DMA_CTRL`'s enable bits for rings 0 to 15 (`DMA_RING_BUF_EN_MASK`,
/// 0xffff, at that shift).
pub const DMA_RINGS_0_15_EN: u32 = 0xffff << 1;
/// `DMA_MAX_BURST_LENGTH` (U-Boot), which is also the board's own
/// `brcm,max-dma-burst-size`.
pub const DMA_MAX_BURST_LENGTH: u32 = 8;
/// `DMA_FC_THRESH_VALUE` (U-Boot): low threshold 5, high a sixteenth of
/// the ring.
pub const DMA_FC_THRESH_VALUE: u32 = (5 << 16) | (DESCS as u32 >> 4);
/// What `DMA_RING_BUF_SIZE` takes (U-Boot): the ring's descriptors in the
/// high half, a buffer's length in the low.
pub const DMA_RING_BUF_SIZE_VALUE: u32 = ((DESCS as u32) << 16) | RX_BUF_LEN as u32;
/// What `DMA_END_ADDR` takes (U-Boot): the last word of the descriptors.
pub const DMA_END_ADDR_VALUE: u32 = (DESCS * reg::DESC_SIZE / 4 - 1) as u32;

/// A descriptor's length field, in the high half of its first word:
/// `DMA_BUFLENGTH_SHIFT` and `DMA_BUFLENGTH_MASK` (U-Boot).
pub const DMA_BUFLENGTH_SHIFT: u32 = 16;
pub const DMA_BUFLENGTH_MASK: u32 = 0x0fff;
/// `DMA_OWN`, `DMA_EOP`, `DMA_SOP` (U-Boot): the device's, the frame's last
/// buffer, its first.
pub const DMA_OWN: u32 = 0x8000;
pub const DMA_EOP: u32 = 0x4000;
pub const DMA_SOP: u32 = 0x2000;
/// `DMA_TX_APPEND_CRC` and `DMA_TX_QTAG_SHIFT` (U-Boot), with the tag
/// U-Boot sends, 0x3f.
pub const DMA_TX_APPEND_CRC: u32 = 0x0040;
pub const DMA_TX_QTAG: u32 = 0x3f << 7;
/// The receive status's error flags (Linux): `DMA_RX_OV`, `_CRC_ERROR`,
/// `_RXER`, `_NO`, `_LG`, the five its receive path drops a frame for.
pub const DMA_RX_ERRORS: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0008 | 0x0010;
/// `DMA_P_INDEX_MASK` and `DMA_C_INDEX_MASK` (Linux): an index is 16 bits.
pub const DMA_INDEX_MASK: u32 = 0xffff;

/// The most polls of the consumer index one send gets, each but the first
/// after a [`TX_POLL_US`] wait: 100 ms in all, the bound the kernel gives
/// virtio-net's send.
pub const TX_POLLS: u32 = 10_000;
pub const TX_POLL_US: u32 = 10;

const _: () = {
    // U-Boot's own figures for the values computed above.
    assert!(MAX_FRAME_LEN == 1536 && TX_MAX_FRAME == 1532);
    assert!(DMA_FC_THRESH_VALUE == 0x0005_0010 && DMA_RING_BUF_SIZE_VALUE == 0x0100_0800);
    assert!(DMA_END_ADDR_VALUE == 767 && DMA_RING16_EN == 0x0002_0000);
    // A buffer's length and a frame's both fit a descriptor's 12-bit
    // length field.
    assert!(TX_MAX_FRAME <= RX_BUF_LEN && RX_BUF_LEN <= DMA_BUFLENGTH_MASK as usize);
    // The ring index is the counter's low byte: 256 divides 65536.
    assert!(DESCS == 256);
};

/// Why [`Genet::init`] set nothing up. In each case nothing was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    /// `SYS_REV_CTRL` does not read GENET v5; the value read.
    NotV5(u32),
    /// The address in `UMAC_MAC0` and `UMAC_MAC1` is none a station can
    /// have (zeros, or a group address). None is made up.
    NoMac([u8; 6]),
    /// The receive area's address is zero or runs past 2^64.
    BadBuffers,
}

/// Why a send did not happen or was not seen to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxError {
    /// [`Genet::start`] has not run: the MAC's transmitter is off.
    NotStarted,
    /// Shorter than [`TX_MIN_FRAME`] or longer than [`TX_MAX_FRAME`].
    BadLength(usize),
    /// An earlier frame is still with the device. Nothing was written.
    Busy,
    /// The device did not take the frame within [`TX_POLLS`]. It may yet.
    Timeout,
}

/// Why a received frame was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxDrop {
    /// Its length is more than its buffer.
    Oversize,
    /// Its length is no more than the pad.
    Runt,
    /// It does not start and end in this buffer.
    Fragment,
    /// The device flagged an error; the status's low 16 bits.
    Flagged(u16),
}

/// What [`Genet::rx_next`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rx {
    /// Nothing waits.
    None,
    /// A frame: `len` bytes at `offset` in buffer `buffer` of the receive
    /// area (the buffer is at `buffer * RX_BUF_LEN`). It stays there until
    /// [`Genet::rx_release`].
    Frame { buffer: usize, offset: usize, len: usize },
    /// A frame was waiting, was not usable, and has been released.
    Dropped(RxDrop),
}

/// How many received frames were dropped, by cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Drops {
    pub oversize: u32,
    pub runt: u32,
    pub fragment: u32,
    pub flagged: u32,
}

/// The device, set up. Holds what the registers do not: the address and
/// the two indexes this side advances. Where the receive buffers are is the
/// caller's to remember: a frame is named by its buffer's index.
#[derive(Debug)]
pub struct Genet {
    mac: [u8; 6],
    /// The receive consumer index: frames taken, 16 bits. The descriptor
    /// and buffer in turn are its low byte.
    rx_cons: u16,
    /// The transmit producer index: frames handed over, 16 bits.
    tx_prod: u16,
    started: bool,
    drops: Drops,
}

fn set<B: Bus>(bus: &mut B, offset: usize, clear: u32, bits: u32) {
    let v = bus.read(offset);
    bus.write(offset, (v & !clear) | bits);
}

/// U-Boot's `bcmgenet_disable_dma`: both DMAs' enable bits off, then the
/// transmit FIFO flushed.
fn disable_dma<B: Bus>(bus: &mut B) {
    set(bus, reg::TDMA_CTRL, DMA_EN, 0);
    set(bus, reg::RDMA_CTRL, DMA_EN, 0);
    bus.write(reg::UMAC_TX_FLUSH, 1);
    bus.delay_us(10);
    bus.write(reg::UMAC_TX_FLUSH, 0);
}

/// Linux's `bcmgenet_intr_disable`: every interrupt of both blocks masked,
/// and whatever was pending cleared.
fn mask_interrupts<B: Bus>(bus: &mut B) {
    bus.write(reg::INTRL2_0_MASK_SET, 0xffff_ffff);
    bus.write(reg::INTRL2_0_CLEAR, 0xffff_ffff);
    bus.write(reg::INTRL2_1_MASK_SET, 0xffff_ffff);
    bus.write(reg::INTRL2_1_CLEAR, 0xffff_ffff);
}

/// Makes a device the firmware may have left running quiet (the plan's
/// design 4): its interrupts masked, the MAC's transmitter and receiver
/// off, both DMAs off (U-Boot's `bcmgenet_gmac_eth_stop`, after the
/// masking). Does it only to GENET v5, and returns whether it did. After
/// this the device writes to no memory.
pub fn quiet<B: Bus>(bus: &mut B) -> bool {
    if version(bus.read(reg::SYS_REV_CTRL)).0 != MAJOR_V5 {
        return false;
    }
    mask_interrupts(bus);
    set(bus, reg::UMAC_CMD, CMD_TX_EN | CMD_RX_EN, 0);
    disable_dma(bus);
    true
}

impl Genet {
    /// Resets the device and sets it up, from whatever state it is in, to
    /// one state: ring 16 alone in each direction over all 256
    /// descriptors, every receive descriptor pointed at its buffer in the
    /// [`RX_AREA_LEN`] bytes at `rx_base`, both DMAs on, and the MAC's
    /// transmitter and receiver **off** until [`start`](Self::start).
    ///
    /// `rx_base` is the receive area's address as the device sees it. From
    /// here on the device writes there, so the memory must be the
    /// caller's for as long as the device runs.
    pub fn init<B: Bus>(bus: &mut B, rx_base: u64) -> Result<Self, InitError> {
        let sys_rev = bus.read(reg::SYS_REV_CTRL);
        if version(sys_rev).0 != MAJOR_V5 {
            return Err(InitError::NotV5(sys_rev));
        }
        // The address first: the reset below may not keep it.
        let address = mac(bus.read(reg::UMAC_MAC0), bus.read(reg::UMAC_MAC1));
        if !mac_usable(&address) {
            return Err(InitError::NoMac(address));
        }
        if rx_base == 0 || rx_base.checked_add(RX_AREA_LEN as u64).is_none() {
            return Err(InitError::BadBuffers);
        }

        mask_interrupts(bus);

        // bcmgenet_eth_probe: the port mode, and the MAC held in reset.
        bus.write(reg::SYS_PORT_CTRL, PORT_MODE_EXT_GPHY);
        bus.write(reg::SYS_RBUF_FLUSH_CTRL, 0);
        bus.delay_us(10);
        bus.write(reg::UMAC_CMD, 0);
        bus.write(reg::UMAC_CMD, CMD_SW_RESET | CMD_LCL_LOOP_EN);

        // bcmgenet_umac_reset.
        let flush = bus.read(reg::SYS_RBUF_FLUSH_CTRL);
        bus.write(reg::SYS_RBUF_FLUSH_CTRL, flush | RBUF_FLUSH);
        bus.delay_us(10);
        bus.write(reg::SYS_RBUF_FLUSH_CTRL, flush & !RBUF_FLUSH);
        bus.delay_us(10);
        bus.write(reg::SYS_RBUF_FLUSH_CTRL, 0);
        bus.delay_us(10);
        bus.write(reg::UMAC_CMD, 0);
        bus.write(reg::UMAC_CMD, CMD_SW_RESET | CMD_LCL_LOOP_EN);
        bus.delay_us(2);
        bus.write(reg::UMAC_CMD, 0);
        bus.write(reg::UMAC_MIB_CTRL, MIB_RESET_ALL);
        bus.write(reg::UMAC_MIB_CTRL, 0);
        bus.write(reg::UMAC_MAX_FRAME_LEN, MAX_FRAME_LEN);
        set(bus, reg::RBUF_CTRL, RBUF_64B_EN, RBUF_ALIGN_2B);
        set(bus, reg::TBUF_CTRL, TBUF_64B_EN, 0);
        bus.write(reg::RBUF_TBUF_SIZE_CTRL, 1);

        // bcmgenet_gmac_write_hwaddr.
        bus.write(reg::UMAC_MAC0, u32::from_be_bytes([address[0], address[1], address[2], address[3]]));
        bus.write(reg::UMAC_MAC1, u32::from_be_bytes([0, 0, address[4], address[5]]));

        disable_dma(bus);

        // rx_ring_init. The device's producer index cannot be set, so the
        // consumer is aligned on it.
        bus.write(reg::RDMA_SCB_BURST_SIZE, DMA_MAX_BURST_LENGTH);
        bus.write(reg::RDMA_START_ADDR, 0);
        bus.write(reg::RDMA_READ_PTR, 0);
        bus.write(reg::RDMA_WRITE_PTR, 0);
        bus.write(reg::RDMA_END_ADDR, DMA_END_ADDR_VALUE);
        let rx_cons = (bus.read(reg::RDMA_PROD_INDEX) & DMA_INDEX_MASK) as u16;
        bus.write(reg::RDMA_CONS_INDEX, u32::from(rx_cons));
        bus.write(reg::RDMA_RING_BUF_SIZE, DMA_RING_BUF_SIZE_VALUE);
        bus.write(reg::RDMA_XON_XOFF_THRESH, DMA_FC_THRESH_VALUE);
        bus.write(reg::RDMA_RING_CFG, 1 << DEFAULT_Q);

        // rx_descs_init: every descriptor its own buffer.
        for i in 0..DESCS {
            let desc = reg::RX_DESCS + i * reg::DESC_SIZE;
            let buffer = rx_base + (i * RX_BUF_LEN) as u64;
            bus.write(desc + reg::DESC_ADDRESS_LO, buffer as u32);
            bus.write(desc + reg::DESC_ADDRESS_HI, (buffer >> 32) as u32);
            bus.write(desc + reg::DESC_LENGTH_STATUS, ((RX_BUF_LEN as u32) << DMA_BUFLENGTH_SHIFT) | DMA_OWN);
        }

        // tx_ring_init. The device's consumer index cannot be set either.
        bus.write(reg::TDMA_SCB_BURST_SIZE, DMA_MAX_BURST_LENGTH);
        bus.write(reg::TDMA_START_ADDR, 0);
        bus.write(reg::TDMA_READ_PTR, 0);
        bus.write(reg::TDMA_WRITE_PTR, 0);
        bus.write(reg::TDMA_END_ADDR, DMA_END_ADDR_VALUE);
        let tx_prod = (bus.read(reg::TDMA_CONS_INDEX) & DMA_INDEX_MASK) as u16;
        bus.write(reg::TDMA_PROD_INDEX, u32::from(tx_prod));
        bus.write(reg::TDMA_MBUF_DONE_THRESH, 1);
        bus.write(reg::TDMA_FLOW_PERIOD, 0);
        bus.write(reg::TDMA_RING_BUF_SIZE, DMA_RING_BUF_SIZE_VALUE);
        bus.write(reg::TDMA_RING_CFG, 1 << DEFAULT_Q);

        // bcmgenet_enable_dma.
        bus.write(reg::TDMA_CTRL, DMA_RING16_EN | DMA_EN);
        set(bus, reg::RDMA_CTRL, DMA_RINGS_0_15_EN, DMA_RING16_EN | DMA_EN);

        Ok(Genet { mac: address, rx_cons, tx_prod, started: false, drops: Drops::default() })
    }

    /// The station address read before the reset and written back after.
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Whether [`start`](Self::start) has turned the MAC on.
    pub fn started(&self) -> bool {
        self.started
    }

    /// Received frames dropped since [`init`](Self::init), by cause.
    pub fn drops(&self) -> Drops {
        self.drops
    }

    /// Tells the MAC the link's speed and turns its transmitter and
    /// receiver on: U-Boot's `bcmgenet_adjust_link` for the board's
    /// `rgmii-rxid`, then its last step. Run it once the PHY reports a
    /// link, and again when a later link comes up at another speed.
    pub fn start<B: Bus>(&mut self, bus: &mut B, speed: Speed) {
        set(bus, reg::EXT_RGMII_OOB_CTRL, OOB_DISABLE, RGMII_LINK | RGMII_MODE_EN);
        set(bus, reg::EXT_RGMII_OOB_CTRL, 0, ID_MODE_DIS);
        bus.write(reg::UMAC_CMD, (speed as u32) << CMD_SPEED_SHIFT);
        set(bus, reg::UMAC_CMD, 0, CMD_TX_EN | CMD_RX_EN);
        self.started = true;
    }

    /// Whether a received frame waits: one register read, nothing written,
    /// no wait. The kernel calls this from its tick.
    pub fn has_frame<B: Bus>(&self, bus: &mut B) -> bool {
        (bus.read(reg::RDMA_PROD_INDEX) & DMA_INDEX_MASK) as u16 != self.rx_cons
    }

    /// Looks at the next received frame. A usable one is returned and
    /// stays where it is: copy it out, then call
    /// [`rx_release`](Self::rx_release). One that is not usable is
    /// released here and reported, so a caller that wants a frame calls
    /// again, a bounded number of times.
    pub fn rx_next<B: Bus>(&mut self, bus: &mut B) -> Rx {
        if !self.has_frame(bus) {
            return Rx::None;
        }
        let buffer = usize::from(self.rx_cons) % DESCS;
        let len_stat = bus.read(reg::RX_DESCS + buffer * reg::DESC_SIZE + reg::DESC_LENGTH_STATUS);
        let length = ((len_stat >> DMA_BUFLENGTH_SHIFT) & DMA_BUFLENGTH_MASK) as usize;
        let drop = if length > RX_BUF_LEN {
            self.drops.oversize = self.drops.oversize.saturating_add(1);
            RxDrop::Oversize
        } else if len_stat & DMA_EOP == 0 || len_stat & DMA_SOP == 0 {
            self.drops.fragment = self.drops.fragment.saturating_add(1);
            RxDrop::Fragment
        } else if len_stat & DMA_RX_ERRORS != 0 {
            self.drops.flagged = self.drops.flagged.saturating_add(1);
            RxDrop::Flagged(len_stat as u16)
        } else if length <= RX_PAD {
            self.drops.runt = self.drops.runt.saturating_add(1);
            RxDrop::Runt
        } else {
            return Rx::Frame { buffer, offset: RX_PAD, len: length - RX_PAD };
        };
        self.rx_release(bus);
        Rx::Dropped(drop)
    }

    /// Gives the frame [`rx_next`](Self::rx_next) returned back to the
    /// device: its buffer is the device's to fill again. U-Boot's
    /// `bcmgenet_gmac_free_pkt`.
    pub fn rx_release<B: Bus>(&mut self, bus: &mut B) {
        self.rx_cons = self.rx_cons.wrapping_add(1);
        bus.write(reg::RDMA_CONS_INDEX, u32::from(self.rx_cons));
    }

    /// Sends the `len` bytes at `addr` (the address as the device sees it)
    /// as one frame, the device appending the checksum, and waits for the
    /// device to take them, at most [`TX_POLLS`] polls. The bytes must stay as they
    /// are until this returns `Ok`, or until a later call no longer
    /// returns [`TxError::Busy`].
    pub fn tx_send<B: Bus>(&mut self, bus: &mut B, addr: u64, len: usize) -> Result<(), TxError> {
        if !self.started {
            return Err(TxError::NotStarted);
        }
        if !(TX_MIN_FRAME..=TX_MAX_FRAME).contains(&len) {
            return Err(TxError::BadLength(len));
        }
        if !self.tx_idle(bus) {
            return Err(TxError::Busy);
        }
        let desc = reg::TX_DESCS + (usize::from(self.tx_prod) % DESCS) * reg::DESC_SIZE;
        bus.write(desc + reg::DESC_ADDRESS_LO, addr as u32);
        bus.write(desc + reg::DESC_ADDRESS_HI, (addr >> 32) as u32);
        bus.write(desc + reg::DESC_LENGTH_STATUS, ((len as u32) << DMA_BUFLENGTH_SHIFT) | DMA_TX_QTAG | DMA_TX_APPEND_CRC | DMA_SOP | DMA_EOP);
        self.tx_prod = self.tx_prod.wrapping_add(1);
        bus.write(reg::TDMA_PROD_INDEX, u32::from(self.tx_prod));
        for poll in 0..TX_POLLS {
            if poll != 0 {
                bus.delay_us(TX_POLL_US);
            }
            if self.tx_idle(bus) {
                return Ok(());
            }
        }
        Err(TxError::Timeout)
    }

    /// Whether the device has taken every frame handed to it.
    fn tx_idle<B: Bus>(&self, bus: &mut B) -> bool {
        (bus.read(reg::TDMA_CONS_INDEX) & DMA_INDEX_MASK) as u16 == self.tx_prod
    }
}

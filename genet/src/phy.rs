//! The PHY: its set-up, its link and its speed, over MDIO. Two layers.
//!
//! **Any PHY**, by the registers IEEE 802.3 clause 22 gives every one:
//! `link`, reads alone, and private to the crate: [`link_resolved`] is
//! the one way in, and falls back to it for a PHY it does not know. The rule for the speed is U-Boot's
//! `genphy_parse_link` (`drivers/net/phy/phy.c`): what both ends
//! advertise, gigabit first. **That is the best speed the two ends could
//! agree on, not the speed the PHY settled on.** A Broadcom PHY that
//! cannot hold gigabit on the cable it has (two pairs, or a bad one) drops
//! to 100 on its own and goes on advertising 1000; [`link`] would report
//! 1000, the MAC would be set to it, and no frame would pass in either
//! direction, with no error anywhere.
//!
//! **The Pi 4's PHY** (the plan's decision D7, settled 2026-11-13: do what
//! U-Boot does), from U-Boot's `drivers/net/phy/broadcom.c` at `v2024.10`.
//! The board's PHY is predicted to be a BCM54213PE; what this file goes
//! by is the identifier, and the PHYs it takes are the ones U-Boot's
//! BCM54210E driver takes ([`is_bcm54210e`]), which is the driver U-Boot
//! runs on this board.
//!
//! - [`setup`] is `bcm54210e_config` for the board's `rgmii-rxid`: the
//!   PHY's receive clock skew on and its transmit clock delay off
//!   (`bcm54xx_config_clock_delay`), then a reset (`phy_reset`), **in that
//!   order, which is U-Boot's**; Linux resets first. Whether the reset
//!   keeps the two delay settings is not known here, so `setup` reads
//!   both back after it and returns them ([`Delays`]): the first board
//!   run says which it is, and a set-up the PHY did not take is seen and
//!   not assumed.
//! - [`link_resolved`] reads the speed the PHY settled on from its
//!   auxiliary status register (`bcm54xx_parse_status`), which is right
//!   on the poor cable where the advertisements are not.
//!
//! **Where this departs from U-Boot:**
//!
//! - `genphy_config_aneg` is left out. U-Boot runs it before the reset
//!   and the reset undoes it (clause 22: a reset returns the control and
//!   advertisement registers to their defaults, autonegotiation on). The
//!   LED register is left out too; it carries no data.
//! - A reset that never ends is an error ([`SetupError::ResetStuck`]).
//!   U-Boot's `bcm5461_config` throws `phy_reset`'s result away.
//! - The two shadows are read back after the reset. U-Boot does not look.
//! - The starting point is not U-Boot's. U-Boot finds the PHY as the
//!   board's first-stage firmware left it; this finds it as the UEFI
//!   firmware's driver left it at the exit. That is why the read-back is
//!   worth its four commands.
//!
//! While it resets, a PHY may not answer at all. U-Boot's MDIO read never
//! looks at the controller's "no answer" flag and so polls on; the reset
//! poll here does the same on purpose, and only a bus that sticks ends it
//! early. After the reset the PHY negotiates afresh: a link takes seconds
//! to come back, and [`link_resolved`] says `Negotiating` or `Down` until
//! it has.

use crate::{mdio_read, mdio_write, Bus, MdioError, BMSR_ANEG_DONE, BMSR_LINK, PHY_BMSR};

/// `MII_ADVERTISE`: what this end advertises.
pub const PHY_ADVERTISE: u8 = 4;
/// `MII_LPA`: what the link partner advertises.
pub const PHY_LPA: u8 = 5;
/// `MII_CTRL1000`: this end's 1000BASE-T advertisement.
pub const PHY_CTRL1000: u8 = 9;
/// `MII_STAT1000`: the partner's 1000BASE-T advertisement, two bits up
/// from where `MII_CTRL1000` has this end's.
pub const PHY_STAT1000: u8 = 10;

/// `BMSR_ESTATEN`: the PHY has the gigabit registers.
pub const BMSR_ESTATEN: u16 = 0x0100;
/// `LPA_1000FULL` and `LPA_1000HALF`, in `MII_STAT1000`.
pub const STAT1000_FULL: u16 = 0x0800;
pub const STAT1000_HALF: u16 = 0x0400;
/// `LPA_100FULL`, `LPA_100HALF`, `LPA_10FULL`, `LPA_10HALF`: the same bits
/// in `MII_ADVERTISE` and `MII_LPA`.
pub const LPA_100FULL: u16 = 0x0100;
pub const LPA_100HALF: u16 = 0x0080;
pub const LPA_10FULL: u16 = 0x0040;
pub const LPA_10HALF: u16 = 0x0020;

/// A link's speed. The value is the one `UMAC_CMD`'s speed field takes
/// (U-Boot's `UMAC_SPEED_10`, `_100`, `_1000`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    S10 = 0,
    S100 = 1,
    S1000 = 2,
}

impl Speed {
    pub fn mbps(self) -> u32 {
        match self {
            Speed::S10 => 10,
            Speed::S100 => 100,
            Speed::S1000 => 1000,
        }
    }
}

/// What the PHY says of the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// No link: no cable, or no partner.
    Down,
    /// A link whose autonegotiation has not finished.
    Negotiating,
    /// A link, negotiated, at this speed; `full` is its duplex.
    Up { speed: Speed, full: bool },
    /// A link that says it negotiated and names no ability both ends have.
    Unresolved,
}

/// What the status register says before any speed is asked for.
enum Status {
    /// Not a negotiated link: this is the answer.
    Settled(Link),
    /// A negotiated link; the status register as read.
    Negotiated(u16),
}

/// The status register read twice (its link bit latches low, so the first
/// reading can be an old drop).
fn status<B: Bus>(bus: &mut B, phy: u8) -> Result<Status, MdioError> {
    mdio_read(bus, phy, PHY_BMSR)?;
    let bmsr = mdio_read(bus, phy, PHY_BMSR)?;
    // All ones is a bus nobody drives (no PHY, or one held in reset), with
    // every bit of "link up at gigabit" set. The scan counts it apart for
    // the same reason.
    if bmsr == 0xffff {
        return Err(MdioError::NoAnswer);
    }
    Ok(if bmsr & BMSR_LINK == 0 {
        Status::Settled(Link::Down)
    } else if bmsr & BMSR_ANEG_DONE == 0 {
        Status::Settled(Link::Negotiating)
    } else {
        Status::Negotiated(bmsr)
    })
}

/// Reads the link of the PHY at `phy`: the status register twice, and for
/// a negotiated link the advertisement registers of both ends. At most six
/// MDIO reads, each bounded as [`mdio_read`] is, and no write but those
/// read commands. A status that reads all ones is no PHY's answer and is
/// [`MdioError::NoAnswer`].
///
/// Private to the crate: on the Pi's PHY this can name a speed the link
/// does not have (the module doc), and [`link_resolved`] is what a caller
/// is given, so the wrong one cannot be picked.
pub(crate) fn link<B: Bus>(bus: &mut B, phy: u8) -> Result<Link, MdioError> {
    let bmsr = match status(bus, phy)? {
        Status::Negotiated(bmsr) => bmsr,
        Status::Settled(link) => return Ok(link),
    };
    if bmsr & BMSR_ESTATEN != 0 {
        // The partner's two bits against this end's, moved up to match.
        let both = mdio_read(bus, phy, PHY_STAT1000)? & (mdio_read(bus, phy, PHY_CTRL1000)? << 2);
        if both & (STAT1000_FULL | STAT1000_HALF) != 0 {
            return Ok(Link::Up { speed: Speed::S1000, full: both & STAT1000_FULL != 0 });
        }
    }
    let both = mdio_read(bus, phy, PHY_ADVERTISE)? & mdio_read(bus, phy, PHY_LPA)?;
    Ok(if both & (LPA_100FULL | LPA_100HALF) != 0 {
        Link::Up { speed: Speed::S100, full: both & LPA_100FULL != 0 }
    } else if both & (LPA_10FULL | LPA_10HALF) != 0 {
        Link::Up { speed: Speed::S10, full: both & LPA_10FULL != 0 }
    } else {
        Link::Unresolved
    })
}

/// U-Boot's identifier and mask for its BCM54210E driver (`broadcom.c`,
/// `U_BOOT_PHY_DRIVER(bcm54210e)`): the low four bits are the revision.
pub const BCM54210E_ID: u32 = 0x600d_84a0;
pub const BCM54210E_ID_MASK: u32 = 0xffff_fff0;

/// Whether a PHY's identifier (register 2 high, register 3 low, as
/// [`crate::Phy::id`] has it) is the BCM54210E's.
pub fn is_bcm54210e(id: u32) -> bool {
    id & BCM54210E_ID_MASK == BCM54210E_ID
}

/// `MII_BMCR`, the control register, and `BMCR_RESET` in it.
pub const PHY_BMCR: u8 = 0;
pub const BMCR_RESET: u16 = 0x8000;
/// `MIIM_BCM54xx_AUXCNTL`: the auxiliary control register, a window on
/// eight shadow registers chosen by its low three bits.
pub const BCM_AUXCNTL: u8 = 0x18;
/// `MIIM_BCM_AUXCNTL_SHDWSEL_MISC`: the miscellaneous shadow, 7.
pub const BCM_AUXCNTL_MISC: u16 = 0x0007;
/// `MIIM_BCM_AUXCNTL_SHDWSEL_MISC_RGMII_SKEW_EN`: the receive clock skew.
pub const BCM_AUXCNTL_MISC_RGMII_SKEW_EN: u16 = 0x0100;
/// `MIIM_BCM_AUXCNTL_MISC_WREN`: a write to the miscellaneous shadow
/// takes effect only with this set.
pub const BCM_AUXCNTL_MISC_WREN: u16 = 0x8000;
/// `MIIM_BCM54xx_AUXSTATUS` and its link mode field, bits 10 to 8.
pub const BCM_AUXSTATUS: u8 = 0x19;
pub const BCM_AUXSTATUS_LINKMODE_SHIFT: u16 = 8;
pub const BCM_AUXSTATUS_LINKMODE_MASK: u16 = 0x0700;
/// `MIIM_BCM54XX_SHD`: a window on 32 more shadow registers. The shadow's
/// number is in bits 14 to 10, its ten bits of data in bits 9 to 0, and
/// bit 15 (`MIIM_BCM54XX_SHD_WRITE`) makes the access a write.
pub const BCM_SHD: u8 = 0x1c;
pub const BCM_SHD_WRITE: u16 = 0x8000;
pub const BCM_SHD_SEL_SHIFT: u16 = 10;
pub const BCM_SHD_DATA_MASK: u16 = 0x03ff;
/// `BCM54810_SHD_CLK_CTL` and `_GTXCLK_EN`: the clock control shadow, 3,
/// and the transmit clock delay in it.
pub const BCM_SHD_CLK_CTL: u16 = 0x03;
pub const BCM_SHD_CLK_CTL_GTXCLK_EN: u16 = 1 << 9;

/// The most polls of the control register a reset gets, each after a
/// millisecond: U-Boot's `phy_reset`, the half second clause 22 allows.
pub const RESET_POLLS: u32 = 500;
pub const RESET_POLL_US: u32 = 1000;

/// How [`setup`] ended without setting the PHY up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupError {
    /// An MDIO command failed; the PHY may be part set up.
    Mdio(MdioError),
    /// The reset bit never read clear within [`RESET_POLLS`]: it stayed
    /// set, or the PHY never answered again.
    ResetStuck,
}

/// The PHY's two delay settings as read back after [`setup`]'s reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delays {
    /// The auxiliary control register's miscellaneous shadow.
    pub misc: u16,
    /// The clock control shadow's ten bits.
    pub clk: u16,
}

impl Delays {
    /// The receive clock skew is on, as `setup` wrote it.
    pub fn rx_skew(&self) -> bool {
        self.misc & BCM_AUXCNTL_MISC_RGMII_SKEW_EN != 0
    }
    /// The transmit clock delay is on; `setup` wrote it off.
    pub fn tx_delay(&self) -> bool {
        self.clk & BCM_SHD_CLK_CTL_GTXCLK_EN != 0
    }
    /// Both are as `setup` wrote them: the PHY took the writes and its
    /// reset kept them. False is not a failure of the set-up; it is the
    /// fact the board run is asked for.
    pub fn as_written(&self) -> bool {
        self.rx_skew() && !self.tx_delay()
    }
}

impl From<MdioError> for SetupError {
    fn from(e: MdioError) -> Self {
        SetupError::Mdio(e)
    }
}

/// Selects the auxiliary control register's miscellaneous shadow and
/// reads it (U-Boot's `bcm54xx_auxctl_read`: the shadow's number in bits
/// 14:12 chooses what the next read returns).
///
/// All ones is a bus nobody drives, as for the status register, and is
/// [`MdioError::NoAnswer`]: taken as the shadow's value it would read as
/// every setting on.
fn read_misc<B: Bus>(bus: &mut B, phy: u8) -> Result<u16, MdioError> {
    mdio_write(bus, phy, BCM_AUXCNTL, (BCM_AUXCNTL_MISC << 12) | BCM_AUXCNTL_MISC)?;
    match mdio_read(bus, phy, BCM_AUXCNTL)? {
        0xffff => Err(MdioError::NoAnswer),
        misc => Ok(misc),
    }
}

/// Selects the clock control shadow and reads its ten bits of data
/// (U-Boot's `bcm_phy_read_shadow`).
/// All ones, before the data is cut out of it, is [`MdioError::NoAnswer`]
/// here too.
fn read_clk<B: Bus>(bus: &mut B, phy: u8) -> Result<u16, MdioError> {
    mdio_write(bus, phy, BCM_SHD, BCM_SHD_CLK_CTL << BCM_SHD_SEL_SHIFT)?;
    match mdio_read(bus, phy, BCM_SHD)? {
        0xffff => Err(MdioError::NoAnswer),
        raw => Ok(raw & BCM_SHD_DATA_MASK),
    }
}

/// Sets up the PHY at `phy` for the board's `rgmii-rxid`, when `id` says
/// it is one U-Boot's BCM54210E driver takes: `Ok(None)`, with nothing
/// written, for any other. The writes are U-Boot's
/// `bcm54xx_config_clock_delay` and `phy_reset`, in U-Boot's order (the
/// module doc says why the order is kept):
///
/// 1. the miscellaneous shadow of the auxiliary control register read
///    and written back with the receive clock skew on;
/// 2. the clock control shadow read and written back with the transmit
///    clock delay off;
/// 3. the PHY reset, and the reset bit polled until it reads clear. A
///    read the PHY does not answer, or that reads all ones, is a PHY
///    still resetting and is polled past;
/// 4. both shadows read back, and returned: what the PHY holds now. A
///    read-back that gets no answer is an error and not a value: the
///    board's log is read for these two, and all ones would say "both
///    on".
///
/// Eleven MDIO commands and the reset's polls. A PHY that never comes out
/// of reset costs half a second of waits plus 500 reads: about 0.5 s on a
/// bus that answers promptly, and about 10.5 s at the bound, on a bus
/// whose every read takes the whole 20 ms a command is allowed.
pub fn setup<B: Bus>(bus: &mut B, phy: u8, id: u32) -> Result<Option<Delays>, SetupError> {
    if !is_bcm54210e(id) {
        return Ok(None);
    }
    let misc = read_misc(bus, phy)?;
    mdio_write(bus, phy, BCM_AUXCNTL, BCM_AUXCNTL_MISC | misc | BCM_AUXCNTL_MISC_WREN | BCM_AUXCNTL_MISC_RGMII_SKEW_EN)?;

    let clk = read_clk(bus, phy)?;
    mdio_write(bus, phy, BCM_SHD, BCM_SHD_WRITE | (BCM_SHD_CLK_CTL << BCM_SHD_SEL_SHIFT) | (clk & !BCM_SHD_CLK_CTL_GTXCLK_EN))?;

    // phy_reset.
    mdio_write(bus, phy, PHY_BMCR, BMCR_RESET)?;
    let mut out_of_reset = false;
    for _ in 0..RESET_POLLS {
        match mdio_read(bus, phy, PHY_BMCR) {
            // All ones has the reset bit set, so an undriven bus polls on
            // by the same test as a PHY that says it is resetting.
            Ok(bmcr) if bmcr & BMCR_RESET == 0 => {
                out_of_reset = true;
                break;
            }
            Ok(_) | Err(MdioError::NoAnswer) => {}
            Err(e) => return Err(e.into()),
        }
        bus.delay_us(RESET_POLL_US);
    }
    if !out_of_reset {
        return Err(SetupError::ResetStuck);
    }
    Ok(Some(Delays { misc: read_misc(bus, phy)?, clk: read_clk(bus, phy)? }))
}

/// Reads the link of the PHY at `phy`: the status register twice, and for
/// a negotiated link its speed and duplex. For a PHY [`is_bcm54210e`]
/// takes (by `id`) those are what the PHY resolved, from its auxiliary
/// status register (U-Boot's `bcm54xx_parse_status`), three MDIO reads in
/// all. For any other PHY they are the best both ends advertise, by the
/// standard registers. A status, or an auxiliary status, that reads all
/// ones is no PHY's answer and is [`MdioError::NoAnswer`].
pub fn link_resolved<B: Bus>(bus: &mut B, phy: u8, id: u32) -> Result<Link, MdioError> {
    if !is_bcm54210e(id) {
        return link(bus, phy);
    }
    if let Status::Settled(link) = status(bus, phy)? {
        return Ok(link);
    }
    let aux = mdio_read(bus, phy, BCM_AUXSTATUS)?;
    // The PHY stopped driving the bus between the two registers: all ones
    // would read as mode 7, a gigabit link.
    if aux == 0xffff {
        return Err(MdioError::NoAnswer);
    }
    let mode = (aux & BCM_AUXSTATUS_LINKMODE_MASK) >> BCM_AUXSTATUS_LINKMODE_SHIFT;
    // U-Boot's table. Its default is "10 half"; here a mode the table
    // does not have is said to be unresolved.
    Ok(match mode {
        1 => Link::Up { speed: Speed::S10, full: false },
        2 => Link::Up { speed: Speed::S10, full: true },
        3 => Link::Up { speed: Speed::S100, full: false },
        5 => Link::Up { speed: Speed::S100, full: true },
        6 => Link::Up { speed: Speed::S1000, full: false },
        7 => Link::Up { speed: Speed::S1000, full: true },
        _ => Link::Unresolved,
    })
}

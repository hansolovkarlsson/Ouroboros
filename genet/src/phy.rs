//! The PHY's link and its speed, read over MDIO from the registers IEEE
//! 802.3 clause 22 gives every PHY. Reads alone: this file writes nothing
//! to the PHY, so whatever configured it (its reset, its straps, the
//! firmware) still stands. Whether that is enough on the board is open
//! (`docs/roadmap/roadmap-pi-net.md`, decision D7): U-Boot's driver for
//! this PHY also writes its RGMII clock delays, and this file does not.
//!
//! The rule for the speed is U-Boot's `genphy_parse_link`
//! (`drivers/net/phy/phy.c`): what both ends advertise, gigabit first.
//! U-Boot's own driver for the Pi's PHY reads a Broadcom status register
//! where this reads the standard ones.

use crate::{mdio_read, Bus, MdioError, BMSR_ANEG_DONE, BMSR_LINK, PHY_BMSR};

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

/// Reads the link of the PHY at `phy`: the status register twice (its link
/// bit latches low, so the first reading can be an old drop), and for a
/// negotiated link the advertisement registers of both ends. At most six
/// MDIO reads, each bounded as [`mdio_read`] is, and no write but those
/// read commands.
pub fn link<B: Bus>(bus: &mut B, phy: u8) -> Result<Link, MdioError> {
    mdio_read(bus, phy, PHY_BMSR)?;
    let bmsr = mdio_read(bus, phy, PHY_BMSR)?;
    if bmsr & BMSR_LINK == 0 {
        return Ok(Link::Down);
    }
    if bmsr & BMSR_ANEG_DONE == 0 {
        return Ok(Link::Negotiating);
    }
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

//! Where each number was read. One row a constant: its name in the source,
//! this crate's value, the value as the source's line gives it (typed here
//! from that line, block offsets added by hand), the file, and the line.
//!
//! The files: U-Boot `v2024.10` `drivers/net/bcmgenet.c` (U), Linux `v6.6`
//! `drivers/net/ethernet/broadcom/genet/bcmgenet.h` (L) and
//! `include/uapi/linux/mii.h` (M). The line numbers were checked against
//! those files by a script on 2026-11-13 (each line holds a `#define` of
//! the name); the files are not in this tree, so the test below checks the
//! values and not the lines.

use crate::mac::*;
use crate::phy::*;
use crate::*;

#[derive(Clone, Copy)]
enum Src {
    U,
    L,
    M,
}
use Src::*;

#[rustfmt::skip]
const CITED: &[(&str, u64, u64, Src, u32)] = &[
    ("SYS_REV_CTRL", reg::SYS_REV_CTRL as u64, 0x00, U, 40),
    ("SYS_PORT_CTRL", reg::SYS_PORT_CTRL as u64, 0x04, U, 42),
    ("PORT_MODE_EXT_GPHY", PORT_MODE_EXT_GPHY as u64, 3, U, 43),
    ("SYS_RBUF_FLUSH_CTRL", reg::SYS_RBUF_FLUSH_CTRL as u64, 0x08, U, 46),
    ("EXT_RGMII_OOB_CTRL", reg::EXT_RGMII_OOB_CTRL as u64, 0x8c, U, 50),
    ("RGMII_LINK", RGMII_LINK as u64, 1 << 4, U, 51),
    ("OOB_DISABLE", OOB_DISABLE as u64, 1 << 5, U, 52),
    ("RGMII_MODE_EN", RGMII_MODE_EN as u64, 1 << 6, U, 53),
    ("ID_MODE_DIS", ID_MODE_DIS as u64, 1 << 16, U, 54),
    ("RBUF_TBUF_SIZE_CTRL", reg::RBUF_TBUF_SIZE_CTRL as u64, 0x3b4, U, 57),
    ("RBUF_CTRL", reg::RBUF_CTRL as u64, 0x300, U, 58),
    ("RBUF_ALIGN_2B", RBUF_ALIGN_2B as u64, 1 << 1, U, 59),
    ("UMAC_MIB_CTRL", reg::UMAC_MIB_CTRL as u64, 0xd80, U, 62),
    ("UMAC_MAX_FRAME_LEN", reg::UMAC_MAX_FRAME_LEN as u64, 0x814, U, 63),
    ("UMAC_MAC0", reg::UMAC_MAC0 as u64, 0x80c, U, 64),
    ("UMAC_MAC1", reg::UMAC_MAC1 as u64, 0x810, U, 65),
    ("UMAC_CMD", reg::UMAC_CMD as u64, 0x808, U, 66),
    ("MDIO_CMD", reg::MDIO_CMD as u64, 0xe14, U, 67),
    ("UMAC_TX_FLUSH", reg::UMAC_TX_FLUSH as u64, 0xb34, U, 68),
    ("MDIO_START_BUSY", MDIO_START_BUSY as u64, 1 << 29, U, 69),
    ("MDIO_READ_FAIL", MDIO_READ_FAIL as u64, 1 << 28, U, 70),
    ("MDIO_RD", MDIO_RD as u64, 2 << 26, U, 71),
    ("MDIO_PMD_SHIFT", MDIO_PMD_SHIFT as u64, 21, U, 73),
    ("MDIO_REG_SHIFT", MDIO_REG_SHIFT as u64, 16, U, 75),
    ("CMD_TX_EN", CMD_TX_EN as u64, 1 << 0, U, 78),
    ("CMD_RX_EN", CMD_RX_EN as u64, 1 << 1, U, 79),
    ("UMAC_SPEED_10", Speed::S10 as u64, 0, U, 80),
    ("UMAC_SPEED_100", Speed::S100 as u64, 1, U, 81),
    ("UMAC_SPEED_1000", Speed::S1000 as u64, 2, U, 82),
    ("CMD_SPEED_SHIFT", CMD_SPEED_SHIFT as u64, 2, U, 84),
    ("CMD_SW_RESET", CMD_SW_RESET as u64, 1 << 13, U, 86),
    ("CMD_LCL_LOOP_EN", CMD_LCL_LOOP_EN as u64, 1 << 15, U, 87),
    ("MIB_RESET_RX", (MIB_RESET_ALL & 1) as u64, 1 << 0, U, 91),
    ("MIB_RESET_RUNT", (MIB_RESET_ALL & 2) as u64, 1 << 1, U, 92),
    ("MIB_RESET_TX", (MIB_RESET_ALL & 4) as u64, 1 << 2, U, 93),
    ("TOTAL_DESCS", DESCS as u64, 256, U, 96),
    ("DEFAULT_Q", DEFAULT_Q as u64, 0x10, U, 100),
    ("ENET_MAX_MTU_SIZE", MAX_FRAME_LEN as u64, 1500 + 14 + 4 + 6 + 4 + 8, U, 107),
    ("DMA_EN", DMA_EN as u64, 1 << 0, U, 112),
    ("DMA_RING_BUF_EN_SHIFT", (DMA_RING16_EN >> DEFAULT_Q).trailing_zeros() as u64, 0x01, U, 113),
    ("DMA_BUFLENGTH_MASK", DMA_BUFLENGTH_MASK as u64, 0x0fff, U, 115),
    ("DMA_BUFLENGTH_SHIFT", DMA_BUFLENGTH_SHIFT as u64, 16, U, 116),
    ("DMA_RING_SIZE_SHIFT", (DMA_RING_BUF_SIZE_VALUE >> 16) as u64, 256, U, 117),
    ("DMA_OWN", DMA_OWN as u64, 0x8000, U, 118),
    ("DMA_EOP", DMA_EOP as u64, 0x4000, U, 119),
    ("DMA_SOP", DMA_SOP as u64, 0x2000, U, 120),
    ("DMA_MAX_BURST_LENGTH", DMA_MAX_BURST_LENGTH as u64, 0x8, U, 122),
    ("DMA_TX_APPEND_CRC", DMA_TX_APPEND_CRC as u64, 0x0040, U, 125),
    ("DMA_TX_QTAG_SHIFT", DMA_TX_QTAG as u64, 0x3f << 7, U, 128),
    ("DMA_RING_SIZE", (reg::RDMA_RING_CFG - reg::RDMA_RING) as u64, 0x40, U, 131),
    ("DMA_DESC_LENGTH_STATUS", reg::DESC_LENGTH_STATUS as u64, 0x00, U, 135),
    ("DMA_DESC_ADDRESS_LO", reg::DESC_ADDRESS_LO as u64, 0x04, U, 136),
    ("DMA_DESC_ADDRESS_HI", reg::DESC_ADDRESS_HI as u64, 0x08, U, 137),
    ("DMA_DESC_SIZE", reg::DESC_SIZE as u64, 12, U, 138),
    ("GENET_RX_OFF", reg::RX_DESCS as u64, 0x2000, U, 140),
    ("GENET_TX_OFF", reg::TX_DESCS as u64, 0x4000, U, 143),
    ("DMA_FC_THRESH_VALUE", DMA_FC_THRESH_VALUE as u64, (5 << 16) | 16, U, 149),
    ("TDMA_READ_PTR", reg::TDMA_READ_PTR as u64, 0x5000, U, 156),
    ("TDMA_CONS_INDEX", reg::TDMA_CONS_INDEX as u64, 0x5008, U, 157),
    ("TDMA_PROD_INDEX", reg::TDMA_PROD_INDEX as u64, 0x500c, U, 158),
    ("DMA_RING_BUF_SIZE", (reg::RDMA_RING_BUF_SIZE - reg::RDMA_RING) as u64, 0x10, U, 159),
    ("DMA_START_ADDR", (reg::RDMA_START_ADDR - reg::RDMA_RING) as u64, 0x14, U, 160),
    ("DMA_END_ADDR", (reg::RDMA_END_ADDR - reg::RDMA_RING) as u64, 0x1c, U, 161),
    ("DMA_MBUF_DONE_THRESH", (reg::TDMA_MBUF_DONE_THRESH - reg::TDMA_RING) as u64, 0x24, U, 162),
    ("TDMA_FLOW_PERIOD", reg::TDMA_FLOW_PERIOD as u64, 0x5028, U, 163),
    ("TDMA_WRITE_PTR", reg::TDMA_WRITE_PTR as u64, 0x502c, U, 164),
    ("RDMA_WRITE_PTR", reg::RDMA_WRITE_PTR as u64, 0x3000, U, 168),
    ("RDMA_PROD_INDEX", reg::RDMA_PROD_INDEX as u64, 0x3008, U, 169),
    ("RDMA_CONS_INDEX", reg::RDMA_CONS_INDEX as u64, 0x300c, U, 170),
    ("RDMA_XON_XOFF_THRESH", reg::RDMA_XON_XOFF_THRESH as u64, 0x3028, U, 171),
    ("RDMA_READ_PTR", reg::RDMA_READ_PTR as u64, 0x302c, U, 172),
    ("TDMA_REG_BASE", reg::TDMA_RING_CFG as u64, 0x5040, U, 174),
    ("RDMA_REG_BASE", reg::RDMA_RING_CFG as u64, 0x3040, U, 175),
    ("DMA_CTRL", (reg::RDMA_CTRL - reg::RDMA_RING_CFG) as u64, 0x04, U, 177),
    ("DMA_SCB_BURST_SIZE", (reg::RDMA_SCB_BURST_SIZE - reg::RDMA_RING_CFG) as u64, 0x0c, U, 178),
    ("RX_BUF_LENGTH", RX_BUF_LEN as u64, 2048, U, 180),
    ("RX_BUF_OFFSET", RX_PAD as u64, 2, U, 182),
    ("RBUF_64B_EN", RBUF_64B_EN as u64, 1 << 0, L, 191),
    ("INTRL2_CPU_CLEAR", (reg::INTRL2_0_CLEAR - 0x200) as u64, 0x08, L, 240),
    ("INTRL2_CPU_MASK_STATUS", (reg::INTRL2_0_MASK_STATUS - 0x200) as u64, 0x0c, L, 241),
    ("INTRL2_CPU_MASK_SET", (reg::INTRL2_0_MASK_SET - 0x200) as u64, 0x10, L, 242),
    ("GENET_INTRL2_0_OFF", (reg::INTRL2_0_CLEAR - 0x08) as u64, 0x0200, L, 284),
    ("GENET_INTRL2_1_OFF", (reg::INTRL2_1_CLEAR - 0x08) as u64, 0x0240, L, 285),
    ("DMA_P_INDEX_MASK", DMA_INDEX_MASK as u64, 0xffff, L, 345),
    ("DMA_C_INDEX_MASK", DMA_INDEX_MASK as u64, 0xffff, L, 346),
    ("DMA_RX_LG", (DMA_RX_ERRORS & 0x10) as u64, 0x0010, L, 428),
    ("DMA_RX_NO", (DMA_RX_ERRORS & 0x08) as u64, 0x0008, L, 429),
    ("DMA_RX_RXER", (DMA_RX_ERRORS & 0x04) as u64, 0x0004, L, 430),
    ("DMA_RX_CRC_ERROR", (DMA_RX_ERRORS & 0x02) as u64, 0x0002, L, 431),
    ("DMA_RX_OV", (DMA_RX_ERRORS & 0x01) as u64, 0x0001, L, 432),
    ("MII_BMSR", PHY_BMSR as u64, 0x01, M, 17),
    ("MII_PHYSID1", PHY_ID1 as u64, 0x02, M, 18),
    ("MII_PHYSID2", PHY_ID2 as u64, 0x03, M, 19),
    ("MII_ADVERTISE", PHY_ADVERTISE as u64, 0x04, M, 20),
    ("MII_LPA", PHY_LPA as u64, 0x05, M, 21),
    ("MII_CTRL1000", PHY_CTRL1000 as u64, 0x09, M, 23),
    ("MII_STAT1000", PHY_STAT1000 as u64, 0x0a, M, 24),
    ("BMSR_LSTATUS", BMSR_LINK as u64, 0x0004, M, 57),
    ("BMSR_ANEGCOMPLETE", BMSR_ANEG_DONE as u64, 0x0020, M, 60),
    ("BMSR_ESTATEN", BMSR_ESTATEN as u64, 0x0100, M, 62),
    ("LPA_10HALF", LPA_10HALF as u64, 0x0020, M, 97),
    ("LPA_10FULL", LPA_10FULL as u64, 0x0040, M, 99),
    ("LPA_100HALF", LPA_100HALF as u64, 0x0080, M, 101),
    ("LPA_100FULL", LPA_100FULL as u64, 0x0100, M, 103),
    ("LPA_1000FULL", STAT1000_FULL as u64, 0x0800, M, 163),
    ("LPA_1000HALF", STAT1000_HALF as u64, 0x0400, M, 164),
];

#[test]
fn every_constant_has_the_value_its_source_line_gives() {
    for &(name, ours, cited, src, line) in CITED {
        let file = match src {
            U => "U-Boot bcmgenet.c",
            L => "Linux bcmgenet.h",
            M => "Linux mii.h",
        };
        assert_eq!(ours, cited, "{name} ({file} line {line})");
        assert!(line > 0);
    }
}

#[test]
fn only_error_flags_the_source_names_drop_a_frame() {
    // The five rows above are the whole mask.
    assert_eq!(DMA_RX_ERRORS, 0x1f);
}

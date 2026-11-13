//! What the Raspberry Pi 4's Ethernet says of itself: step 2 of
//! `docs/roadmap/roadmap-pi-net.md`, read before the exit and logged
//! through the UEFI console, only under the boot flag `\NETDUMP`
//! (`main.rs`). A reading aid, as `sdscan.rs` was for the SD card: it
//! drives nothing, and its lines settle what the plan lists as predicted
//! (the version, the address the firmware left, the PHY and where it
//! answers, whether the firmware has the device running at this point).
//! That last is read **before the exit**: the firmware's driver may change
//! the enables in its own exit notification, so it is not the state the
//! kernel inherits, and step 5 quiets the device whatever it reads.
//!
//! - **The declaration:** how many GENET devices the ACPI tables declare
//!   present (`amlscan::declared` with `GENET_HID`). With any count but
//!   one, nothing further happens and **no register is touched**: that is
//!   QEMU and Parallels, and `make test-netdump` holds it.
//! - **The device, when exactly one is declared** and its block is one
//!   `genet::block_usable` takes: the pure crate's two halves over the
//!   registers the `_CRS` names. `genet::registers` reads the version and,
//!   only when it reads v5, eight more registers, none whose read changes
//!   anything; **each is logged as it is read**, so a read that faults or
//!   never returns leaves every earlier value in the log. Then
//!   `genet::phys` scans the MDIO bus for PHYs. Its read commands are the
//!   scan's only writes (one register, read operations alone).
//! - **The firmware's driver is kept off the MDIO bus meanwhile.** The scan
//!   runs with the firmware's timer events live, and the firmware's network
//!   stack, when it is bound to the NIC, polls the PHY from one (predicted,
//!   from edk2's sources as remembered). A command of its own between one
//!   of ours and our poll would be read back as our answer, and this
//!   reading is made once, to be believed. So the MDIO half runs at
//!   `TPL_NOTIFY`, where no timer callback runs, and nothing is logged
//!   until the priority is back down. On a live bus that is a few
//!   milliseconds and on a dead one 20 ms (the first command's budget ends
//!   the scan). The bound is far above both: a bus whose every command
//!   completes just inside its 20 ms budget is never cut off, and 32
//!   addresses of up to four commands is about 2.6 s, longer still if the
//!   firmware's stall rounds 10 us up.
//!
//! The registers are read on the firmware's mapping, which its own driver
//! uses. A device declared but not mapped would fault here, through the
//! firmware's vectors, and `earlyfault.rs` would report it: the flag is
//! set to learn exactly that kind of thing.

use crate::acpi;

/// The scan's access to the device: 32-bit volatile reads at `base`, the
/// firmware's stall for the delay, and no write but an MDIO read command
/// (`genet::scan_write_allowed`). The flag is documented as read-only, and
/// this is where that is held: the crate will grow reset and DMA writes
/// over the same trait, and none of them can pass through this bus.
struct Mmio {
    base: usize,
}

impl genet::Bus for Mmio {
    fn read(&mut self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= genet::BLOCK_LEN as usize);
        // SAFETY: inside the 64 KB register block the firmware declares
        // present, on the firmware's own mapping; 32-bit and aligned, the
        // base by `genet::block_usable` (checked by `report` before this
        // is built) and the offset by the assert.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write(&mut self, offset: usize, value: u32) {
        assert!(genet::scan_write_allowed(offset, value), "the scan may write MDIO read commands alone");
        // SAFETY: as `read`; the offset is `MDIO_CMD`'s, by the assert.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }

    fn delay_us(&mut self, us: u32) {
        uefi::boot::stall(core::time::Duration::from_micros(u64::from(us)));
    }
}

/// Logs what the ACPI tables declare of GENET and, when they declare
/// exactly one present, what `genet::registers` and `genet::phys` read
/// from it. Called only
/// under `\NETDUMP`, before `exit_boot_services` only (the logger, the
/// stall and the firmware's mapping are all relied on).
///
/// # Safety
/// `rsdp` as `acpi::find_rsdp` returns it. Any GENET the tables declare
/// present is mapped by the firmware.
pub unsafe fn report(rsdp: Option<*const u8>) {
    let found = match unsafe { acpi::declared(rsdp, amlscan::GENET_HID) } {
        Ok(found) => found,
        Err(e) => {
            log::info!("Ouroboros kernel: net: ACPI tables not read ({e:?}): no register touched");
            return;
        }
    };
    log::info!(
        "Ouroboros kernel: net: GENET declared present: {} (unreadable {}, absent {}, status unknown {}, dropped {})",
        found.as_slice().len(),
        found.unreadable,
        found.absent,
        found.status_unknown,
        found.dropped
    );
    let Some(c) = found.one() else {
        log::info!("Ouroboros kernel: net: not exactly one GENET declared: no register touched");
        return;
    };
    log::info!("Ouroboros kernel: net: GENET registers {:#x}, {:#x} bytes, INTID {:?}", c.base, c.size, c.intid);
    if !genet::block_usable(c.base, c.size) {
        log::info!(
            "Ouroboros kernel: net: that is not a block of {:#x} bytes at an aligned address: no register touched",
            genet::BLOCK_LEN
        );
        return;
    }
    let mut bus = Mmio { base: c.base as usize };
    let r = genet::registers(&mut bus, &mut |offset, what, value| {
        log::info!("Ouroboros kernel: net: GENET +{offset:#06x} {what}: {value:#010x}");
    });
    let (major, minor) = genet::version(r.sys_rev);
    if !r.is_v5() {
        log::info!(
            "Ouroboros kernel: net: GENET version major {major} minor {minor}: NOT v5, so no other register was read and the MDIO bus is not scanned"
        );
        return;
    }
    let m = genet::mac(r.mac0, r.mac1);
    log::info!(
        "Ouroboros kernel: net: GENET version major {major} minor {minor} (v5), MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} ({}); before the exit: transmit enabled {}, receive enabled {}, receive DMA on {}, transmit DMA on {}",
        m[0],
        m[1],
        m[2],
        m[3],
        m[4],
        m[5],
        if genet::mac_usable(&m) { "a station address" } else { "NOT a station address" },
        r.umac_cmd & genet::CMD_TX_EN != 0,
        r.umac_cmd & genet::CMD_RX_EN != 0,
        r.rdma_ctrl & genet::DMA_EN != 0,
        r.tdma_ctrl & genet::DMA_EN != 0
    );
    log::info!("Ouroboros kernel: net: scanning the MDIO bus at TPL_NOTIFY, the firmware's timer events held off (up to about 3 s; 20 ms on a dead bus)");
    let scanned = {
        // SAFETY: raised for the MDIO scan alone, which calls nothing of
        // the firmware's but Stall (allowed at any priority) and is bounded
        // (`genet::MDIO_POLLS` a command, at most four commands an address,
        // the scan ended by a stuck one: the module doc has the figure);
        // the guard restores the priority at this block's end, before
        // anything is logged.
        let _tpl = unsafe { uefi::boot::raise_tpl(uefi::boot::Tpl::NOTIFY) };
        genet::phys(&mut bus, &r)
    };
    let Some(p) = scanned else {
        log::info!("Ouroboros kernel: net: the MDIO scan refused the version it was given: nothing was written");
        return;
    };
    for phy in p.phys() {
        log::info!(
            "Ouroboros kernel: net: PHY at MDIO address {}: id {:#010x}, status {:#06x}, link {}, autonegotiation done {}",
            phy.addr,
            phy.id,
            phy.bmsr,
            if phy.bmsr & genet::BMSR_LINK != 0 { "up" } else { "down" },
            phy.bmsr & genet::BMSR_ANEG_DONE != 0
        );
    }
    log::info!(
        "Ouroboros kernel: net: MDIO: {} PHY(s) answered whole ({} more not kept), {} in part (first at {:?}), {} flagged no answer, {} read all ones, {} read zero",
        p.phys().len(),
        p.dropped,
        p.partial,
        p.first_partial,
        p.no_answer,
        p.read_ones,
        p.read_zero
    );
    match p.stuck_at {
        Some(addr) => log::info!("Ouroboros kernel: net: MDIO stuck busy at address {addr}: the scan stopped there"),
        None => log::info!("Ouroboros kernel: net: MDIO scanned, addresses 0 to 31"),
    }
}

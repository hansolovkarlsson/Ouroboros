//! The Raspberry Pi 4's Ethernet, Broadcom GENET v5: the kernel's half of
//! the driver. The logic is the `genet` crate's (tested on the host, since
//! no QEMU machine has this device); this file lends it what the crate
//! cannot own, the registers, the delay and the receive buffers' memory,
//! and words the boot log. `docs/roadmap/roadmap-pi-net.md` is the plan,
//! and this is its step 5.
//!
//! **Step 5 is bring-up and link, and nothing more.** The device is made
//! quiet ([`quiet`], for every declared GENET, used or not), reset and set
//! up ([`init`]), its PHY set up and its link read and logged,
//! and it is installed so `NET_MAC` answers. The MAC's transmitter and
//! receiver are **left off**: [`Device::send_frame`] refuses and no frame
//! arrives. Sending is step 6 and receiving step 7, each its own trip to
//! the board (one variable a round trip), so nothing here copies a frame.
//!
//! **Found, never assumed** (the plan's design 1): [`discover`] takes the
//! one GENET the ACPI tables declare present, before the exit, and nothing
//! here names an address. On QEMU and Parallels none is declared, nothing
//! is logged, and no register is touched.
//!
//! **Polled** (design 7): [`Device`] reports no interrupt whatever the
//! `_CRS` names, so `main.rs` enables none and the tick's
//! `net_has_frame()` is the only waker. The crate masks every interrupt at
//! the device as well.
//!
//! **The receive buffers** are [`RX_AREA`], one page-aligned static of 256
//! buffers of 2048 bytes that the device writes by DMA. The Pi's firmware
//! says its DMA is not cache-coherent (`_CCA 0`), so `main.rs` passes
//! [`dma_region`] to `mmu.rs` as non-cacheable beside the xHCI's pool
//! whenever the firmware says that, and this driver does no cache
//! maintenance of its own (design 5). [`report_rx_area`] logs how the area
//! really walks, on every boot: on the Pi it must say non-cacheable, and
//! on QEMU, where nothing declares non-coherent DMA, cacheable.

use core::cell::UnsafeCell;

use genet::mac::{Genet, InitError, TxError, RX_AREA_LEN};
use genet::phy::{self, Link};

use crate::{acpi, console, mmu, timer};

/// The receive buffers, the device's to write. Page-aligned and a whole
/// number of pages, so the non-cacheable mapping covers it and nothing
/// else (`mmu.rs` refuses a range that shares a page).
#[repr(C, align(4096))]
struct RxArea(UnsafeCell<[u8; RX_AREA_LEN]>);
// SAFETY: touched by the device (DMA) and, from step 7, by the one core in
// the kernel under the kernel lock; never by two at once.
unsafe impl Sync for RxArea {}
static RX_AREA: RxArea = RxArea(UnsafeCell::new([0; RX_AREA_LEN]));

const _: () = assert!(RX_AREA_LEN.is_multiple_of(4096));

/// The receive area's range `(base, size)`, page-aligned at both ends:
/// what `main.rs` hands `mmu.rs` to map non-cacheable where the firmware
/// declares DMA non-coherent.
pub(crate) fn dma_region() -> (u64, u64) {
    (RX_AREA.0.get() as u64, RX_AREA_LEN as u64)
}

/// Logs how the receive area walks in the tables now running: every page
/// asked of the MMU itself. Called on every boot, device or none, so the
/// line that must say "non-cacheable" on the Pi has a control on QEMU that
/// must say "cacheable".
pub(crate) fn report_rx_area() {
    let (base, size) = dma_region();
    let pages = size / 4096;
    let nc = (0..pages).filter(|p| mmu::walks_as_noncacheable(base + p * 4096)).count() as u64;
    let how = if nc == pages {
        "non-cacheable"
    } else if nc == 0 {
        "cacheable"
    } else {
        "MIXED, which is wrong"
    };
    console::println!(
        "Ouroboros kernel: genet: receive area {base:#x}, {size:#x} bytes: walks as {how} memory ({nc} of {pages} pages non-cacheable)"
    );
}

/// The device's registers, the kernel's delay, and every write the crate
/// makes (the scan's bus in `netscan.rs` lets through MDIO reads alone;
/// this one is the driver's).
#[derive(Clone, Copy)]
struct Mmio {
    base: usize,
}

impl genet::Bus for Mmio {
    fn read(&mut self, offset: usize) -> u32 {
        assert!(offset.is_multiple_of(4) && offset + 4 <= genet::BLOCK_LEN as usize);
        // SAFETY: inside the 64 KB block the firmware declares, which
        // `init` saw walk as device memory at both ends before this bus
        // was built; 32-bit and aligned, the base by `genet::block_usable`
        // and the offset by the assert.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write(&mut self, offset: usize, value: u32) {
        assert!(offset.is_multiple_of(4) && offset + 4 <= genet::BLOCK_LEN as usize);
        // SAFETY: as `read`.
        unsafe { core::ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }

    fn delay_us(&mut self, us: u32) {
        timer::delay_us(u64::from(us));
    }
}

/// The GENET the ACPI tables declare, as [`discover`] took it.
#[derive(Clone, Copy)]
pub(crate) struct Declared {
    pub base: u64,
}

/// The one GENET the ACPI tables declare present, with a block the driver
/// can use. Silent when the tables name none at all (QEMU, Parallels);
/// one line otherwise, taken or not. Two would leave which is the board's
/// a guess, so none is taken. Before the exit, while the tables are
/// certainly mapped; no register is touched.
///
/// # Safety
/// `rsdp` as `acpi::find_rsdp` returns it.
pub(crate) unsafe fn discover(rsdp: Option<*const u8>) -> Option<Declared> {
    let found = match unsafe { acpi::declared(rsdp, amlscan::GENET_HID) } {
        Ok(found) => found,
        Err(e) => {
            log::info!("Ouroboros kernel: genet: ACPI tables not read ({e:?}): no GENET is taken");
            return None;
        }
    };
    let left_out = found.unreadable + found.absent + found.status_unknown + found.dropped;
    let Some(c) = found.one() else {
        if !found.is_empty() || left_out != 0 {
            log::info!(
                "Ouroboros kernel: genet: ACPI's GENET declarations not taken: {} present, {} unreadable, {} absent, {} of unknown status, {} past the list's room",
                found.as_slice().len(),
                found.unreadable,
                found.absent,
                found.status_unknown,
                found.dropped
            );
        }
        return None;
    };
    if !genet::block_usable(c.base, c.size) {
        log::info!(
            "Ouroboros kernel: genet: ACPI's GENET @ {:#x}, {:#x} bytes, not taken: not a block of {:#x} bytes at an aligned address",
            c.base,
            c.size,
            genet::BLOCK_LEN
        );
        return None;
    }
    log::info!(
        "Ouroboros kernel: genet: ACPI declares GENET @ {:#x}, {:#x} bytes, INTID {:?} (not used: the device is polled)",
        c.base,
        c.size,
        c.intid
    );
    Some(Declared { base: c.base })
}

/// The longest the boot waits for a link, and how often it asks. The PHY's
/// reset drops the link, and autonegotiation of a gigabit one takes two
/// to three seconds. A board with no cable waits the whole of it: nothing
/// read before the reset says a cable is absent (a link bit that is low
/// may be a link still coming up), so the wait is not skipped on a guess.
const LINK_WAIT_MS: u64 = 5000;
const LINK_POLL_MS: u64 = 100;

/// The declared device, made quiet: [`quiet`]'s answer, and what [`init`]
/// takes, so the device cannot be set up without having been stopped.
pub(crate) struct Quieted {
    bus: Mmio,
    sys_rev: u32,
}

/// Makes the declared device quiet, whatever becomes of it afterwards
/// (the plan's design 4): its interrupts masked, the MAC's transmitter and
/// receiver off, both DMAs off and seen to be. A firmware that left it
/// receiving would otherwise go on writing frames into memory that is no
/// longer the firmware's. `main.rs` calls this for every declared GENET,
/// also when another NIC holds the cell.
///
/// `None`, each with its line, when that could not be done: the block is
/// not mapped as a device (nothing is touched, and **the device is left
/// as the firmware left it**), the version is not v5 (nothing is
/// written), or a DMA was told to stop and did not say it had.
///
/// # Safety
/// After `mmu::install_identity_map`, once, on the boot core, with nothing
/// else driving the device (the firmware's driver stopped with boot
/// services).
pub(crate) unsafe fn quiet(declared: Declared) -> Option<Quieted> {
    let base = declared.base;
    // Both ends of the block, by the MMU's own walk: its 1GB block may be
    // one RAM's mapping took (`main.rs`, the console's check).
    let last = base + u64::from(genet::BLOCK_LEN) - 4096;
    if !mmu::walks_as_device(base) || !mmu::walks_as_device(last) {
        console::println!(
            "Ouroboros kernel: genet: WARNING: {base:#x} is not mapped as a device: the Ethernet is not touched, and is left as the firmware left it"
        );
        return None;
    }
    let mut bus = Mmio { base: base as usize };
    let sys_rev = genet::Bus::read(&mut bus, genet::reg::SYS_REV_CTRL);
    match genet::mac::quiet(&mut bus) {
        None => {
            let (major, minor) = genet::version(sys_rev);
            console::println!(
                "Ouroboros kernel: genet: version major {major} minor {minor} ({sys_rev:#010x}) is not GENET v5: nothing was written, the Ethernet is not brought up"
            );
            None
        }
        Some(stopped) if !stopped.both() => {
            console::println!(
                "Ouroboros kernel: genet: WARNING: the device was told to stop and did not say it had (transmit DMA stopped {}, receive DMA stopped {}): the Ethernet is not brought up",
                stopped.tdma,
                stopped.rdma
            );
            None
        }
        Some(_) => Some(Quieted { bus, sys_rev }),
    }
}

/// The device, brought up.
pub(crate) struct Device {
    bus: Mmio,
    genet: Genet,
}

/// Brings a quieted device up: the crate's reset and set-up, the PHY found
/// on MDIO and set up, the link read. One line says what it is and one
/// what its link is; each way it can stop short says why. `None` when the
/// device cannot be used; it was made quiet before this, and a set-up that
/// stops part way leaves the MAC off.
///
/// `dma_noncoherent` is what the firmware says of DMA. Where it is not
/// coherent the receive area must walk non-cacheable on every page, and a
/// device whose buffers do not is not set up: the receive DMA would be
/// pointed at memory the CPU reads through a cache the device does not
/// see.
///
/// # Safety
/// As [`quiet`], which made `quieted`.
pub(crate) unsafe fn init(quieted: Quieted, dma_noncoherent: bool) -> Option<Device> {
    let Quieted { mut bus, sys_rev } = quieted;
    let base = bus.base;
    let (area, size) = dma_region();
    if dma_noncoherent && !(0..size / 4096).all(|p| mmu::walks_as_noncacheable(area + p * 4096)) {
        console::println!(
            "Ouroboros kernel: genet: WARNING: the firmware says DMA is not coherent and the receive area does not walk non-cacheable on every page: the Ethernet is not brought up"
        );
        return None;
    }
    let (major, minor) = genet::version(sys_rev);

    let mut device = match Genet::init(&mut bus, area) {
        Ok(genet) => Device { bus, genet },
        Err(InitError::NoMac(m)) => {
            console::println!(
                "Ouroboros kernel: genet: the firmware left no station address ({:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}): none is made up, the Ethernet is not brought up",
                m[0], m[1], m[2], m[3], m[4], m[5]
            );
            return None;
        }
        Err(e) => {
            console::println!("Ouroboros kernel: genet: the device did not set up ({e:?}): the Ethernet is not brought up");
            return None;
        }
    };
    let m = device.genet.mac();
    console::println!(
        "Ouroboros kernel: genet: GENET v5 (version major {major} minor {minor}) @ {base:#x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, set up on ring 16, polled",
        m[0], m[1], m[2], m[3], m[4], m[5]
    );
    // The MDIO scan's version gate reads the version alone.
    device.bring_up_phy(&genet::Registers { sys_rev, ..genet::Registers::default() });
    console::println!("Ouroboros kernel: genet: the MAC's transmitter and receiver are left off: nothing is sent or received yet (step 5)");
    Some(device)
}

impl Device {
    /// Finds the PHY, sets it up when it is one the crate knows, and logs
    /// the link. Whatever happens, the device stays installed: without a
    /// link nothing is started, and the address still answers.
    fn bring_up_phy(&mut self, registers: &genet::Registers) {
        let Some(scan) = genet::phys(&mut self.bus, registers) else {
            console::println!("Ouroboros kernel: genet: the MDIO scan refused the version: no PHY, link unknown");
            return;
        };
        let (addr, id) = match scan.phys() {
            [p] if scan.partial == 0 && scan.dropped == 0 && scan.stuck_at.is_none() => (p.addr, p.id),
            found => {
                console::println!(
                    "Ouroboros kernel: genet: not exactly one PHY on MDIO ({} whole, {} more, {} in part, stuck at {:?}): none is set up, link unknown",
                    found.len(),
                    scan.dropped,
                    scan.partial,
                    scan.stuck_at
                );
                return;
            }
        };
        match phy::setup(&mut self.bus, addr, id) {
            Ok(Some(d)) => console::println!(
                "Ouroboros kernel: genet: PHY at MDIO address {addr}, id {id:#010x}: set up and reset; after the reset its receive clock skew is {} and its transmit clock delay {} (shadows {:#06x}, {:#06x}): {}",
                if d.rx_skew() { "on" } else { "off" },
                if d.tx_delay() { "on" } else { "off" },
                d.misc,
                d.clk,
                if d.as_written() { "as written" } else { "NOT as written" }
            ),
            Ok(None) => console::println!(
                "Ouroboros kernel: genet: PHY at MDIO address {addr}, id {id:#010x}: not one this driver sets up, left as found"
            ),
            Err(e) => {
                console::println!("Ouroboros kernel: genet: PHY at MDIO address {addr}, id {id:#010x}: set-up failed ({e:?}), link unknown");
                return;
            }
        }
        // Asked until the link is up or the time is: a read that fails,
        // or says down or negotiating, is asked again. Only the last
        // answer is reported.
        let deadline = timer::deadline_us(LINK_WAIT_MS * 1000);
        let mut link = phy::link_resolved(&mut self.bus, addr, id);
        while !matches!(link, Ok(Link::Up { .. })) && timer::now_ticks() < deadline {
            timer::delay_us(LINK_POLL_MS * 1000);
            link = phy::link_resolved(&mut self.bus, addr, id);
        }
        match link {
            Ok(Link::Up { speed, full }) => console::println!(
                "Ouroboros kernel: genet: link up, {} Mb/s, {} duplex",
                speed.mbps(),
                if full { "full" } else { "half" }
            ),
            Ok(Link::Down) => console::println!("Ouroboros kernel: genet: link down (waited {LINK_WAIT_MS} ms)"),
            Ok(Link::Negotiating) => console::println!("Ouroboros kernel: genet: link still negotiating (waited {LINK_WAIT_MS} ms)"),
            Ok(Link::Unresolved) => console::println!("Ouroboros kernel: genet: link up at a speed the PHY does not name (waited {LINK_WAIT_MS} ms)"),
            Err(e) => console::println!("Ouroboros kernel: genet: the link did not read ({e:?}, waited {LINK_WAIT_MS} ms)"),
        }
    }

    /// The station address the firmware left, kept across the reset.
    pub(crate) fn mac(&self) -> [u8; 6] {
        self.genet.mac()
    }

    /// Whether a received frame waits: at most one register read, nothing
    /// written, no wait, as the tick's caller needs. **False while the MAC
    /// is not started, without asking the device**: `poll_frame` takes
    /// nothing in step 5, and a "yes" that nothing can consume would wake
    /// the network server on every tick for good.
    pub(crate) fn has_frame(&self) -> bool {
        let mut bus = self.bus;
        self.genet.started() && self.genet.has_frame(&mut bus)
    }

    /// Refuses: the MAC is not started in step 5, and step 6 is where a
    /// frame is first sent.
    pub(crate) fn send_frame(&mut self, _frame: &[u8]) -> Result<(), TxError> {
        Err(TxError::NotStarted)
    }

    /// Nothing: with the MAC's receiver off no frame arrives, and step 7
    /// is where one is first taken.
    pub(crate) fn poll_frame(&mut self, _out: &mut [u8]) -> Option<usize> {
        None
    }
}

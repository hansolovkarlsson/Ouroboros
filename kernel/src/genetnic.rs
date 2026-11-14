//! The Raspberry Pi 4's Ethernet, Broadcom GENET v5: the kernel's half of
//! the driver. The logic is the `genet` crate's (tested on the host, since
//! no QEMU machine has this device); this file lends it what the crate
//! cannot own, the registers, the delay and the receive buffers' memory,
//! and words the boot log. `docs/roadmap/roadmap-pi-net.md` is the plan,
//! and this is its steps 5 and 6.
//!
//! **Step 5 is bring-up and link.** The device is made quiet ([`quiet`],
//! for every declared GENET, used or not), reset and set up ([`init`]),
//! its PHY set up and its link read and logged, and it is installed so
//! `NET_MAC` answers.
//!
//! **Step 6 is the transmitter, and the transmitter alone.** With a link,
//! the MAC is told its speed and its transmitter turned on
//! (`Genet::start` with `Receiver::Off`), and [`Device::send_frame`]
//! copies a frame into the one transmit buffer and hands it to the
//! device. The **receiver stays off** until step 7, its own trip to the
//! board (one variable a round trip): until then the receive DMA writes
//! nothing, so an address the device takes differently from the CPU
//! (nothing has shown they are the same) costs a wrong frame on the wire
//! and no memory. A boot with no link starts nothing; every send after
//! it asks the PHY for the link, until one finds it up and starts the
//! MAC, so a cable put in later works at the next send (the plan's
//! decision D3, as recommended). That asking is up to three MDIO reads
//! under the kernel lock on each such send, and each read may poll for
//! 20 ms on a bus that does not answer; nothing remembers that the last
//! one failed (open in the plan's step 6).
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
//! buffers of 2048 bytes that the device writes by DMA, and **the
//! transmit buffer** is [`TX_AREA`], one page the device reads by DMA. The
//! Pi's firmware says its DMA is not cache-coherent (`_CCA 0`), so
//! `main.rs` passes [`dma_region`] and [`tx_region`] to `mmu.rs` as
//! non-cacheable beside the xHCI's pool whenever the firmware says that,
//! and this driver does no cache maintenance of its own (design 5).
//! [`report_rx_area`] logs how both really walk, on every boot: on the Pi
//! each must say non-cacheable, and on QEMU, where nothing declares
//! non-coherent DMA, cacheable. Non-cacheable is not the whole of it: a
//! store to the buffer is a Normal access that nothing orders against the
//! register write that tells the device to read it, so every register
//! write this driver makes has a `dsb sy` in front of it
//! ([`genet_write32`], the one store [`Mmio`] writes through), the barrier
//! `xhci.rs` puts before every doorbell. `make check-xhci-barriers` finds
//! it in the built image.

use core::cell::UnsafeCell;

use genet::mac::{Genet, InitError, Receiver, TxError, RX_AREA_LEN, TX_MAX_FRAME, TX_MIN_FRAME};
use genet::phy::{self, Link, ReadBack, Speed};

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

/// The transmit buffer's size: one page, so its non-cacheable mapping
/// covers it and nothing else.
const TX_AREA_LEN: usize = 4096;
/// The one transmit buffer, the device's to read. Every send is copied
/// here, so a send is refused while the device still has the last one
/// (`Genet::tx_idle`, asked before the copy).
#[repr(C, align(4096))]
struct TxArea(UnsafeCell<[u8; TX_AREA_LEN]>);
// SAFETY: written by the one core in the kernel under the kernel lock
// (`NET_SEND`), and read by the device; never written by two at once.
unsafe impl Sync for TxArea {}
static TX_AREA: TxArea = TxArea(UnsafeCell::new([0; TX_AREA_LEN]));

// The longest frame a send takes fits the buffer.
const _: () = assert!(TX_MAX_FRAME <= TX_AREA_LEN);

/// The transmit buffer's range `(base, size)`, one page: what `main.rs`
/// hands `mmu.rs` beside [`dma_region`].
pub(crate) fn tx_region() -> (u64, u64) {
    (TX_AREA.0.get() as u64, TX_AREA_LEN as u64)
}

/// Whether the transmit buffer's page walks as non-cacheable in the
/// tables now running.
fn tx_area_noncacheable() -> bool {
    mmu::walks_as_noncacheable(tx_region().0)
}

/// The receive area's range `(base, size)`, page-aligned at both ends:
/// what `main.rs` hands `mmu.rs` to map non-cacheable where the firmware
/// declares DMA non-coherent.
pub(crate) fn dma_region() -> (u64, u64) {
    (RX_AREA.0.get() as u64, RX_AREA_LEN as u64)
}

/// How many of the receive area's pages walk as non-cacheable in the
/// tables now running, and how many pages it has: every page asked of the
/// MMU itself. The one count the log line and [`init`]'s refusal both
/// read.
fn rx_area_noncacheable_pages() -> (u64, u64) {
    let (base, size) = dma_region();
    let pages = size / 4096;
    ((0..pages).filter(|p| mmu::walks_as_noncacheable(base + p * 4096)).count() as u64, pages)
}

/// Logs how the receive area and the transmit buffer walk in the tables
/// now running: every page asked of the MMU itself. Called on every boot,
/// device or none, so each line that must say "non-cacheable" on the Pi
/// has a control on QEMU that must say "cacheable".
pub(crate) fn report_rx_area() {
    let (base, size) = dma_region();
    let (nc, pages) = rx_area_noncacheable_pages();
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
    let (base, size) = tx_region();
    console::println!(
        "Ouroboros kernel: genet: transmit buffer {base:#x}, {size:#x} bytes: walks as {} memory",
        if tx_area_noncacheable() { "non-cacheable" } else { "cacheable" }
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
        // `quiet` saw walk as device memory at both ends before it built
        // this bus, the only place one is built (`init` is handed
        // `quiet`'s); 32-bit and aligned, the base by
        // `genet::block_usable` and the offset by the assert.
        unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
    }

    fn write(&mut self, offset: usize, value: u32) {
        assert!(offset.is_multiple_of(4) && offset + 4 <= genet::BLOCK_LEN as usize);
        // SAFETY: as `read`. Through `genet_write32` and nothing else:
        // it has this one caller, so the image check fails if this write
        // is routed around its barrier.
        unsafe { genet_write32(self.base, value, offset) }
    }

    fn delay_us(&mut self, us: u32) {
        timer::delay_us(u64::from(us));
    }
}

/// Every register write, with its barrier: `dsb sy; str w1, [x0, x2]; ret`,
/// so each store the CPU made before it (a frame copied into the transmit
/// buffer) is complete before the device sees the write that may tell it
/// to read them. Naked, like `xhci.rs`'s `mmio_write32`, so the image holds
/// these fixed instructions whatever the profile, no edit can put a store
/// between the two, and `scripts/check-xhci-barriers.py` can find them.
/// The base and offset are two operands on purpose: the sequence is then
/// not the xHCI's, and that check wants each exactly once. Until
/// 2026-11-14 the barrier was an inline `dsb sy` in `send_frame`, which a
/// later edit could drop with every check green (the review of #463).
///
/// # Safety
/// `base + offset` is a 32-bit register of the device, mapped as device
/// memory ([`Mmio::write`]'s assert and `quiet`'s walk).
#[unsafe(naked)]
unsafe extern "C" fn genet_write32(base: usize, value: u32, offset: usize) {
    core::arch::naked_asm!("dsb sy", "str w1, [x0, x2]", "ret");
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
/// How many link reads running may find the MDIO bus stuck busy before the
/// wait gives up on it.
const STUCK_READS: u32 = 3;

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
/// written), or the receive DMA was told to stop and did not say it had.
/// A transmit DMA that has not yet said so is noted and not refused.
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
    let began = timer::monotonic_us();
    match genet::mac::quiet(&mut bus) {
        None => {
            let (major, minor) = genet::version(sys_rev);
            console::println!(
                "Ouroboros kernel: genet: version major {major} minor {minor} ({sys_rev:#010x}) is not GENET v5: nothing was written, the Ethernet is not brought up"
            );
            None
        }
        // The receive DMA is the one that writes memory: one that has not
        // said it stopped is not set up over.
        Some(stopped) if !stopped.rdma => {
            console::println!(
                "Ouroboros kernel: genet: WARNING: the receive DMA was told to stop and did not say it had (transmit DMA stopped {}): the Ethernet is not brought up",
                stopped.tdma
            );
            None
        }
        Some(stopped) => {
            // Said when it works too: until 2026-11-14 a stop that worked
            // printed nothing, and "it stopped" was read from the absence
            // of a line (the board's first runs, the plan's step 5).
            console::println!(
                "Ouroboros kernel: genet: made quiet in {} us: receive DMA stopped, transmit DMA {}",
                timer::monotonic_us().saturating_sub(began),
                if stopped.tdma { "stopped" } else { "not yet" }
            );
            if !stopped.tdma {
                // It only reads memory, and Linux goes on from here too.
                // The set-up asks it once more, after the MAC's reset.
                console::println!(
                    "Ouroboros kernel: genet: the transmit DMA was told to stop and has not yet said it had: it is asked again after the MAC's reset"
                );
            }
            Some(Quieted { bus, sys_rev })
        }
    }
}

/// The device, brought up.
pub(crate) struct Device {
    bus: Mmio,
    genet: Genet,
    /// The one PHY, by MDIO address and identifier, once it was found and
    /// its set-up did not fail: what a send asks for the link when the
    /// boot found none.
    phy: Option<(u8, u32)>,
    /// Frames the device took.
    sent: u32,
    /// Which of a send's failures has had its line: each is said once,
    /// since `netd` sends again and a dead link would fill the log.
    said_timeout: bool,
    said_busy: bool,
    said_link_error: bool,
}

/// Brings a quieted device up: the crate's reset and set-up, the PHY found
/// on MDIO and set up, the link read. One line says what it is and one
/// what its link is; each way it can stop short says why. `None` when the
/// device cannot be used; it was made quiet before this, and a set-up that
/// stops part way leaves the MAC off.
///
/// `dma_noncoherent` is what the firmware says of DMA. Where it is not
/// coherent the receive area must walk non-cacheable on every page and
/// the transmit buffer on its one, and a device whose buffers do not is
/// not set up: the receive DMA would be pointed at memory the CPU reads
/// through a cache the device does not see, and the transmit DMA would
/// read a frame still in the CPU's.
///
/// # Safety
/// As [`quiet`], which made `quieted`.
pub(crate) unsafe fn init(quieted: Quieted, dma_noncoherent: bool) -> Option<Device> {
    let Quieted { mut bus, sys_rev } = quieted;
    let base = bus.base;
    let (area, _) = dma_region();
    let (nc, pages) = rx_area_noncacheable_pages();
    if dma_noncoherent && (nc != pages || !tx_area_noncacheable()) {
        console::println!(
            "Ouroboros kernel: genet: WARNING: the firmware says DMA is not coherent and the receive area or the transmit buffer does not walk non-cacheable on every page: the Ethernet is not brought up"
        );
        return None;
    }
    let (major, minor) = genet::version(sys_rev);

    let mut device = match Genet::init(&mut bus, area) {
        Ok(genet) => Device { bus, genet, phy: None, sent: 0, said_timeout: false, said_busy: false, said_link_error: false },
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
    match device.bring_up_phy(&genet::Registers { sys_rev, ..genet::Registers::default() }) {
        Some(speed) => device.start(speed),
        None => console::println!(
            "Ouroboros kernel: genet: no link read as up at a speed, so the MAC's transmitter is left off: a send asks the link again{}",
            if device.phy.is_some() { "" } else { " (it cannot: no PHY was set up, so nothing will be sent)" }
        ),
    }
    Some(device)
}

impl Device {
    /// Tells the MAC the speed and turns its transmitter on, the receiver
    /// left off (step 6), and says so.
    fn start(&mut self, speed: Speed) {
        self.genet.start(&mut self.bus, speed, Receiver::Off);
        console::println!(
            "Ouroboros kernel: genet: the MAC's transmitter is on at {} Mb/s, its receiver left off: frames are sent and none is received yet (step 6)",
            speed.mbps()
        );
    }

    /// Finds the PHY, sets it up when it is one the crate knows, and logs
    /// the link: the speed of a link that is up, `None` for anything
    /// else. Whatever happens, the device stays installed: without a
    /// link nothing is started, and the address still answers.
    fn bring_up_phy(&mut self, registers: &genet::Registers) -> Option<Speed> {
        let Some(scan) = genet::phys(&mut self.bus, registers) else {
            console::println!("Ouroboros kernel: genet: the MDIO scan refused the version: no PHY, link unknown");
            return None;
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
                return None;
            }
        };
        match phy::setup(&mut self.bus, addr, id) {
            Ok(Some(set_up)) => {
                // What the firmware's driver left, read before any write.
                let f = set_up.found;
                console::println!(
                    "Ouroboros kernel: genet: PHY at MDIO address {addr}, id {id:#010x}: found with its receive clock skew {} and its transmit clock delay {} (shadows {:#06x}, {:#06x})",
                    if f.rx_skew() { "on" } else { "off" },
                    if f.tx_delay() { "on" } else { "off" },
                    f.misc,
                    f.clk
                );
                match set_up.after {
                    // The set-up was done; its read-back only could not
                    // be read. The link is still asked for.
                    ReadBack::Failed(e) => console::println!(
                        "Ouroboros kernel: genet: PHY set up and reset; its delays did not read back ({e:?}), so what it holds after the reset is not known"
                    ),
                    // The MAC is started with ID_MODE_DIS set, U-Boot's
                    // pairing: it delays nothing, so the transmit clock's
                    // one delay has to be the PHY's (`phy.rs`).
                    ReadBack::Read(d) => console::println!(
                        "Ouroboros kernel: genet: PHY set up and reset; after the reset its receive clock skew is {} and its transmit clock delay {} (shadows {:#06x}, {:#06x}): {}",
                        if d.rx_skew() { "on" } else { "off" },
                        if d.tx_delay() { "on" } else { "off" },
                        d.misc,
                        d.clk,
                        match (d.rx_skew(), d.tx_delay()) {
                            (true, true) => "as U-Boot leaves it",
                            (false, true) => "NOT as U-Boot leaves it: the receive clock has no skew",
                            (true, false) => "NOT as U-Boot leaves it: the transmit clock has no delay, the MAC adding none, so frames sent may be bad",
                            (false, false) =>
                                "NOT as U-Boot leaves it: the receive clock has no skew, and the transmit clock no delay, the MAC adding none, so frames sent may be bad",
                        }
                    ),
                }
            }
            Ok(None) => console::println!(
                "Ouroboros kernel: genet: PHY at MDIO address {addr}, id {id:#010x}: not one this driver sets up, left as found"
            ),
            Err(e) => {
                console::println!("Ouroboros kernel: genet: PHY at MDIO address {addr}, id {id:#010x}: set-up failed ({e:?}), link unknown");
                return None;
            }
        }
        self.phy = Some((addr, id));
        // Asked until the link is up or the time is: a read that says
        // down or negotiating, or that the PHY does not answer (it may
        // still be coming out of its reset), is asked again. A bus whose
        // command never finishes is asked again too, but only
        // `STUCK_READS` times running: each new command is written over
        // the one still busy, and one overrun in a wait of seconds is not
        // a stuck bus. Only the last answer is reported.
        let deadline = timer::deadline_us(LINK_WAIT_MS * 1000);
        let mut stuck = 0;
        let mut link = phy::link_resolved(&mut self.bus, addr, id);
        loop {
            stuck = if matches!(link, Err(genet::MdioError::Busy)) { stuck + 1 } else { 0 };
            if matches!(link, Ok(Link::Up { .. })) || stuck == STUCK_READS || timer::now_ticks() >= deadline {
                break;
            }
            timer::delay_us(LINK_POLL_MS * 1000);
            link = phy::link_resolved(&mut self.bus, addr, id);
        }
        match link {
            Ok(Link::Up { speed, full }) => {
                console::println!(
                    "Ouroboros kernel: genet: link up, {} Mb/s, {} duplex",
                    speed.mbps(),
                    if full { "full" } else { "half" }
                );
                return Some(speed);
            }
            Ok(Link::Down) => console::println!("Ouroboros kernel: genet: link down (waited {LINK_WAIT_MS} ms)"),
            Ok(Link::Negotiating) => console::println!("Ouroboros kernel: genet: link still negotiating (waited {LINK_WAIT_MS} ms)"),
            Ok(Link::Unresolved) => console::println!("Ouroboros kernel: genet: link up at a speed the PHY does not name (waited {LINK_WAIT_MS} ms)"),
            Err(genet::MdioError::Busy) if stuck == STUCK_READS => console::println!(
                "Ouroboros kernel: genet: the link did not read: the MDIO bus stayed busy for {STUCK_READS} reads running, and the wait was ended there"
            ),
            Err(e) => console::println!("Ouroboros kernel: genet: the link did not read ({e:?}, waited {LINK_WAIT_MS} ms)"),
        }
        None
    }

    /// The station address the firmware left, kept across the reset.
    pub(crate) fn mac(&self) -> [u8; 6] {
        self.genet.mac()
    }

    /// Whether a received frame waits: at most one register read, nothing
    /// written, no wait, as the tick's caller needs. False while the
    /// MAC's receiver is off, which is the crate's rule
    /// (`Genet::has_frame`) and is all of steps 5 and 6: `poll_frame`
    /// takes nothing, and a "yes" that nothing can consume would wake the
    /// network server on every tick for good.
    pub(crate) fn has_frame(&self) -> bool {
        let mut bus = self.bus;
        self.genet.has_frame(&mut bus)
    }

    /// Sends one frame: copies it into the transmit buffer and hands it to
    /// the device, which appends the checksum (and pads a frame shorter
    /// than Ethernet's 60 bytes, `genet::mac::TX_MIN_FRAME`). Waits for
    /// the device to take it, at most the crate's `TX_POLLS` (100 ms),
    /// under the kernel lock as virtio-net's send does.
    ///
    /// In order, and the order is the point of each:
    /// 1. The length is checked before anything is read or written.
    /// 2. A MAC not started is a boot that found no link. The link is
    ///    asked for once in this send, with no wait for it to come up (up
    ///    to three MDIO reads, each of which may poll for 20 ms on a bus
    ///    that does not answer), and the MAC started if it is up
    ///    (decision D3); otherwise the send is refused, and a link that
    ///    did not read at all has a line, once. Every send asks again
    ///    while the MAC is not started.
    /// 3. `tx_idle` is asked **before the copy**: the buffer is the one
    ///    the device may still be reading the last frame from, after a
    ///    send that timed out.
    /// 4. The copy, then the crate's register writes, each with its
    ///    `dsb sy` in front of it ([`genet_write32`]): the barrier is what
    ///    orders the buffer's stores before the write that tells the
    ///    device to read them.
    ///
    /// Four things are said, once each: the first frame the device
    /// takes, the first send that times out, the first refused as busy,
    /// and the first whose link did not read.
    pub(crate) fn send_frame(&mut self, frame: &[u8]) -> Result<(), TxError> {
        let len = frame.len();
        if !(TX_MIN_FRAME..=TX_MAX_FRAME).contains(&len) {
            return Err(TxError::BadLength(len));
        }
        if !self.genet.started() {
            let Some((addr, id)) = self.phy else {
                return Err(TxError::NotStarted);
            };
            match phy::link_resolved(&mut self.bus, addr, id) {
                Ok(Link::Up { speed, full }) => {
                    console::println!(
                        "Ouroboros kernel: genet: link up at a send, {} Mb/s, {} duplex",
                        speed.mbps(),
                        if full { "full" } else { "half" }
                    );
                    self.start(speed);
                }
                // Down, negotiating, or up at a speed the PHY does not
                // name: no line, it is the ordinary state of no cable.
                Ok(_) => return Err(TxError::NotStarted),
                // The PHY did not answer, or the bus is stuck: not the
                // same thing as a link that is down.
                Err(e) => {
                    if !self.said_link_error {
                        self.said_link_error = true;
                        console::println!(
                            "Ouroboros kernel: genet: a send asked the link and it did not read ({e:?}): nothing is sent; said once"
                        );
                    }
                    return Err(TxError::NotStarted);
                }
            }
        }
        if !self.genet.tx_idle(&mut self.bus) {
            if !self.said_busy {
                self.said_busy = true;
                console::println!(
                    "Ouroboros kernel: genet: a send refused: the device still has the frame before it ({} sent so far); said once",
                    self.sent
                );
            }
            return Err(TxError::Busy);
        }
        let (buffer, _) = tx_region();
        // SAFETY: `len` is at most `TX_MAX_FRAME`, inside the page; the
        // buffer is this driver's alone, written under the kernel lock,
        // and the device is not reading it (`tx_idle` above).
        unsafe {
            core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer as *mut u8, len);
        }
        match self.genet.tx_send(&mut self.bus, buffer, len) {
            Ok(()) => {
                if self.sent == 0 {
                    console::println!("Ouroboros kernel: genet: first frame sent: {len} bytes, taken by the device");
                }
                self.sent = self.sent.saturating_add(1);
                Ok(())
            }
            Err(e) => {
                if e == TxError::Timeout && !self.said_timeout {
                    self.said_timeout = true;
                    console::println!(
                        "Ouroboros kernel: genet: a frame of {len} bytes was handed to the device and not taken in 100 ms ({} sent before it); said once",
                        self.sent
                    );
                }
                Err(e)
            }
        }
    }

    /// Nothing: with the MAC's receiver off no frame arrives, and step 7
    /// is where one is first taken.
    pub(crate) fn poll_frame(&mut self, _out: &mut [u8]) -> Option<usize> {
        None
    }
}

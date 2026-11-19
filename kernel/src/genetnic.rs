//! The Raspberry Pi 4's Ethernet, Broadcom GENET v5: the kernel's half of
//! the driver. The logic is the `genet` crate's (tested on the host, since
//! no QEMU machine has this device); this file lends it what the crate
//! cannot own, the registers, the delay and the receive buffers' memory,
//! and words the boot log. `docs/roadmap/roadmap-pi-net.md` is the plan,
//! and this is its steps 5 to 7.
//!
//! **Step 5 is bring-up and link.** The device is made quiet ([`quiet`],
//! for every declared GENET, used or not), reset and set up ([`init`]),
//! its PHY set up and its link read and logged, and it is installed so
//! `NET_MAC` answers.
//!
//! **Step 6 is the transmitter.** With a link, the MAC is told its speed
//! and turned on (`Genet::start`), and [`Device::send_frame`] copies a
//! frame into the one transmit buffer and hands it to the device. That
//! step ran with the receiver off, so that an address the device took
//! differently from the CPU would cost a wrong frame on the wire and no
//! memory; the frame was seen on the wire whole (2026-11-14), which is
//! what showed the device takes the CPU's address as its own.
//!
//! **Step 7 is the receiver.** The same start turns it on
//! (`Receiver::On`), and from then on **the device writes memory**: each
//! frame into the buffer of [`RX_AREA`] its descriptor names.
//! [`Device::poll_frame`] copies a waiting frame out and gives its buffer
//! back. Nothing here writes a receive buffer, and a frame is read only
//! between the crate's `rx_next` and `rx_release`, while its buffer is
//! not the device's to fill. A boot with no link starts nothing; every send after
//! it asks the PHY for the link, until one finds it up and starts the
//! MAC (the plan's decision D3, as recommended), and so does `netd`'s
//! wait, once a second ([`Device::link_at_wait`], since 2026-11-17, by the
//! rule in `genet::linkask`), so a cable put in later is heard within
//! about a second whether or not the board sends. The wait goes on
//! asking with the MAC on (since 2026-11-18): a link read down is let go
//! and sends refused until it is up again, and one that returns, at any
//! speed, starts the MAC again at it. That asking is up to
//! three MDIO reads under the kernel lock, and each read may poll for
//! 20 ms on a bus that does not answer; the ask that finds the link up
//! makes two more once the MAC is on, for the pause line. After three
//! asks running fail to read, the wait asks once a minute until one reads
//! (never stops, since the review of #487). A send's asks count with the
//! wait's, and a send does not ask while that back-off holds (its second
//! review, 2026-11-19, which closes the plan's step 6 item that nothing
//! remembered a failed read). A drop shorter than a
//! second that returns at the same speed and duplex is not seen, and
//! needs nothing (`genet::linkask::change`).
//!
//! **Found, never assumed** (the plan's design 1): [`discover`] takes the
//! one GENET the ACPI tables declare present, before the exit, and nothing
//! here names an address. On QEMU and Parallels none is declared, nothing
//! is logged, and no register is touched.
//!
//! **The receive interrupt** (the plan's step 9, 2026-11-16; polled
//! before it, design 7): [`Device`] reports the first INTID the `_CRS`
//! names (189 on the Pi 4), `main.rs` enables it at the GIC, and the IRQ
//! path calls [`Device::ack_interrupt`], which masks and clears receive
//! done at the device and counts, before `netd` is woken. It stays masked
//! until `netd` next waits (`NET_WAIT`), when [`Device::rearm_interrupt`]
//! unmasks it if the ring is empty, as Linux's interrupt and poller do:
//! cleared alone, the device raised it again for every frame still waiting
//! (302 interrupts for 257 frames on the board). So the next interrupt
//! comes only once `netd` has drained the ring and asked to wait. The
//! rearm is the wait's and not the drain's, and a wait that finds a frame
//! there returns for it instead of blocking, so no reader blocks with it
//! masked, a `netd` restarted part way through a drain included. The
//! crate masks every interrupt at the set-up and unmasks receive done
//! alone when the receiver comes on. The tick's `net_has_frame()` stays
//! behind it, as it does for virtio-net: an interrupt missed costs a
//! tick.
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
//! write this driver's bus makes has a `dsb sy` in front of it
//! (`genet_write32`, the one store [`Mmio`] writes through), the barrier
//! `xhci.rs` puts before every doorbell. `make check-xhci-barriers` finds
//! it in the built image.

use core::cell::UnsafeCell;

use genet::mac::{Genet, InitError, Receiver, Rx, RxDrop, TxError, DESCS, RX_AREA_LEN, RX_BUF_LEN, TX_MAX_FRAME, TX_MIN_FRAME};
use genet::linkask::{Answered, Ask, Change, LinkAsk};
use genet::phy::{self, Link, ReadBack, Speed};

use crate::{acpi, console, mmu, timer};

/// The receive buffers, the device's to write. Page-aligned and a whole
/// number of pages, so the non-cacheable mapping covers it and nothing
/// else (`mmu.rs` refuses a range that shares a page).
#[repr(C, align(4096))]
struct RxArea(UnsafeCell<[u8; RX_AREA_LEN]>);
// SAFETY: written by the device alone (DMA), and read by the one core in
// the kernel under the kernel lock (`NET_RECV`), a buffer at a time and
// only while the crate holds that buffer back from the device.
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

/// The driver's bus, in a module of its own so that the barrier store has
/// one caller **by privacy and not by a comment**: `genet_write32` is
/// private to this module, whose one use of it is [`Mmio::write`], so a
/// second caller elsewhere in this file does not compile. The image check
/// needs exactly that: it asks for the store once and at least one call to
/// it, and with a second caller `Mmio::write` could be routed around the
/// barrier with the check still green (the review of this change,
/// 2026-11-14; `xhci.rs` keeps its two register stores in `mod reg` for
/// the same reason, though not `publish_cycle_word`, whose one caller is
/// still held by convention). A second caller inside this module would
/// compile, so the module is kept to the bus alone. What privacy does not stop: a raw `write_volatile` to a
/// register, written anywhere in this file. There is none, and the check
/// does not see one.
mod bus {
    use crate::timer;

    /// The device's registers, the kernel's delay, and every write the
    /// crate makes (the scan's bus in `netscan.rs` lets through MDIO
    /// reads alone and has its own store; this one is the driver's).
    #[derive(Clone, Copy)]
    pub(super) struct Mmio {
        base: usize,
    }

    impl Mmio {
        /// # Safety
        /// `base` is the 64 KB block the firmware declares, usable by
        /// `genet::block_usable` and seen to walk as device memory at
        /// both ends. `quiet` is the one place a bus is built.
        pub(super) unsafe fn new(base: usize) -> Self {
            Self { base }
        }

        pub(super) fn base(&self) -> usize {
            self.base
        }
    }

    impl genet::Bus for Mmio {
        fn read(&mut self, offset: usize) -> u32 {
            assert!(offset.is_multiple_of(4) && offset + 4 <= genet::BLOCK_LEN as usize);
            // SAFETY: inside the block `new` was promised; 32-bit and
            // aligned, the base by `genet::block_usable` and the offset
            // by the assert.
            unsafe { core::ptr::read_volatile((self.base + offset) as *const u32) }
        }

        fn write(&mut self, offset: usize, value: u32) {
            assert!(offset.is_multiple_of(4) && offset + 4 <= genet::BLOCK_LEN as usize);
            // SAFETY: as `read`. The one call to `genet_write32`.
            unsafe { genet_write32(self.base, value, offset) }
        }

        fn delay_us(&mut self, us: u32) {
            timer::delay_us(u64::from(us));
        }
    }

    /// A register write of this bus, with its barrier: `dsb sy; str w1,
    /// [x0, x2]; ret`, so each store the CPU made before it (a frame
    /// copied into the transmit buffer) is complete before the device
    /// sees the write that may tell it to read them. Naked, like
    /// `xhci.rs`'s `mmio_write32`, so the image holds these fixed
    /// instructions whatever the profile, no edit can put a store between
    /// the two, and `scripts/check-xhci-barriers.py` can find them. The
    /// base and offset are two operands on purpose: the sequence is then
    /// not the xHCI's, and that check wants each exactly once. Until
    /// 2026-11-14 the barrier was an inline `dsb sy` in `send_frame`,
    /// which a later edit could drop with every check green (the review
    /// of #463).
    ///
    /// # Safety
    /// `base + offset` is a 32-bit register of the device, mapped as
    /// device memory (`Mmio::write`'s assert and `Mmio::new`'s promise).
    #[unsafe(naked)]
    unsafe extern "C" fn genet_write32(base: usize, value: u32, offset: usize) {
        core::arch::naked_asm!("dsb sy", "str w1, [x0, x2]", "ret");
    }
}
use bus::Mmio;

/// A duration for a log line: [`timer::us_since`]'s answer, said as
/// unknown where the counter's frequency reads 0 and never as a 0 that
/// would read as a measurement.
struct Took(Option<u64>);

impl core::fmt::Display for Took {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(us) => write!(f, "{us} us"),
            None => f.write_str("a time not known (the counter's frequency reads 0)"),
        }
    }
}

/// The GENET the ACPI tables declare, as [`discover`] took it: its block
/// and the first interrupt its `_CRS` names (189 on the Pi 4), which
/// carries the default ring's receive done (the plan's step 9).
#[derive(Clone, Copy)]
pub(crate) struct Declared {
    pub base: u64,
    pub intid: Option<u32>,
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
        "Ouroboros kernel: genet: ACPI declares GENET @ {:#x}, {:#x} bytes, INTID {:?} (its receive interrupt, the tick's poll behind it)",
        c.base,
        c.size,
        c.intid
    );
    Some(Declared { base: c.base, intid: c.intid })
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
    intid: Option<u32>,
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
    // SAFETY: `discover` held the block to `genet::block_usable`, and both
    // of its ends walk as device memory, just above.
    let mut bus = unsafe { Mmio::new(base as usize) };
    let sys_rev = genet::Bus::read(&mut bus, genet::reg::SYS_REV_CTRL);
    // Ticks alone before the stop: nothing that can fail stands between
    // here and it.
    let began = timer::now_ticks();
    let stopped = genet::mac::quiet(&mut bus);
    let took = Took(timer::us_since(began));
    match stopped {
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
                "Ouroboros kernel: genet: WARNING: the receive DMA was told to stop and did not say it had (transmit DMA stopped {}, after {took}; status now: receive {:#010x}, transmit {:#010x}): the Ethernet is not brought up",
                stopped.tdma,
                genet::Bus::read(&mut bus, genet::reg::RDMA_STATUS),
                genet::Bus::read(&mut bus, genet::reg::TDMA_STATUS)
            );
            None
        }
        Some(stopped) => {
            // Said when it works too: until 2026-11-14 a stop that worked
            // printed nothing, and "it stopped" was read from the absence
            // of a line (the board's first runs, the plan's step 5). A
            // transmit DMA that has not said so is a note and not a
            // refusal: it only reads memory, and Linux goes on from here
            // too. `Genet::init` asks it once more after the MAC's reset,
            // when `init` runs and gets that far; the line promises
            // nothing, since not every boot does. The two status
            // registers are read here, after the crate's own polls, so
            // they are the state now and not the reading the crate
            // judged by.
            console::println!(
                "Ouroboros kernel: genet: made quiet in {took}: receive DMA stopped, transmit DMA {}; status now: receive {:#010x}, transmit {:#010x}",
                if stopped.tdma { "stopped" } else { "told to stop and has not yet said it had" },
                genet::Bus::read(&mut bus, genet::reg::RDMA_STATUS),
                genet::Bus::read(&mut bus, genet::reg::TDMA_STATUS)
            );
            Some(Quieted { bus, sys_rev, intid: declared.intid })
        }
    }
}

/// The device, brought up.
pub(crate) struct Device {
    bus: Mmio,
    genet: Genet,
    /// The receive interrupt the firmware declares, reported to `nic.rs`
    /// so `main.rs` enables it at the GIC (the plan's step 9).
    intid: Option<u32>,
    /// Interrupts taken on it, saturating: the step's check, said with
    /// the counts received ([`Device::say_counts`]) and once at the first.
    interrupts: u32,
    /// The one PHY, by MDIO address and identifier, once it was found and
    /// its set-up did not fail: what a send asks for the link when the
    /// boot found none.
    phy: Option<(u8, u32)>,
    /// Frames the device took.
    sent: u32,
    /// Frames taken from the device and handed up.
    received: u32,
    /// Whether the first received frame the crate dropped has had its
    /// line. The counts by cause are the crate's (`Genet::drops`), said
    /// with the count of frames taken ([`Device::say_counts`]).
    said_drop: bool,
    /// Which of a send's failures has had its line: each is said once,
    /// since `netd` sends again and a dead link would fill the log. The
    /// link's is once a link: [`Device::start`] clears it.
    said_timeout: bool,
    said_busy: bool,
    said_link_error: bool,
    /// When `netd`'s wait asks the link and when it stops: the crate's
    /// rule (`genet::linkask`), on `timer::monotonic_us`'s clock
    /// ([`Device::link_at_wait`]).
    link_ask: LinkAsk,
    /// The link as last read, while it is up: the speed and duplex the
    /// MAC was last started on. `None` before the first start and while
    /// the link is down; a send is refused then ([`SendError::NoLink`]).
    /// Set by [`Device::start`], cleared by an ask that reads it down
    /// (`genet::linkask::change`).
    link: Option<(Speed, bool)>,
}

/// Why a send did not happen: the crate's reasons, or no link held. The
/// link is the kernel's to track, not the crate's, so its refusal is
/// here and not in `genet::mac::TxError` (the review of #487).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendError {
    /// The link as last read is not up (a boot that found none, or a link
    /// read down since): refused before anything is read or written.
    NoLink,
    Tx(TxError),
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
    let Quieted { mut bus, sys_rev, intid } = quieted;
    let base = bus.base();
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
        Ok(genet) => Device {
            bus,
            genet,
            intid,
            interrupts: 0,
            phy: None,
            sent: 0,
            received: 0,
            said_drop: false,
            said_timeout: false,
            said_busy: false,
            said_link_error: false,
            link_ask: LinkAsk::new(),
            link: None,
        },
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
        "Ouroboros kernel: genet: GENET v5 (version major {major} minor {minor}) @ {base:#x}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, set up on ring 16",
        m[0], m[1], m[2], m[3], m[4], m[5]
    );
    // Where the firmware left the two indexes the device owns, which the
    // set-up aligns on and does not change. Neither ring depends on its
    // own: the crate counts both rings' descriptors from 0, as the board
    // showed for each (receive 2026-11-15, when the index stood at 10 and
    // 9 and every frame was looked for that far from where it lay;
    // transmit 2026-11-16, with the index written to 10 by a diagnostic,
    // where U-Boot's low byte left the first frame untaken).
    let rx_index = genet::Bus::read(&mut device.bus, genet::reg::RDMA_PROD_INDEX) & genet::mac::DMA_INDEX_MASK;
    let tx_index = genet::Bus::read(&mut device.bus, genet::reg::TDMA_CONS_INDEX) & genet::mac::DMA_INDEX_MASK;
    console::println!(
        "Ouroboros kernel: genet: the indexes the firmware left: receive {rx_index}, transmit {tx_index}"
    );
    // The MDIO scan's version gate reads the version alone.
    match device.bring_up_phy(&genet::Registers { sys_rev, ..genet::Registers::default() }) {
        Some((speed, full)) => device.start(speed, full),
        None => console::println!(
            "Ouroboros kernel: genet: no link read as up at a speed, so the MAC is left off: a send asks the link again{}",
            if device.phy.is_some() { "" } else { " (it cannot: no PHY was set up, so nothing will be sent)" }
        ),
    }
    Some(device)
}

impl Device {
    /// Tells the MAC the speed and turns its transmitter and its receiver
    /// on, and says so. From here the device writes received frames into
    /// [`RX_AREA`]. The line carries two registers read back after the
    /// start, for the day nothing is received: `UMAC_CMD` (the enables,
    /// the speed, bit 4, promiscuous, and bits 8 and 28, pause ignored
    /// both ways: `0x1000010b` at 1000 Mb/s on the board) and `UMAC_MDF_CTRL` (the
    /// destination filters the firmware left on; this driver sets none,
    /// as U-Boot sets none).
    ///
    /// Then, with the MAC on, the pause line ([`Device::say_pause`]): what
    /// the link agreed against what `UMAC_CMD` does. `full` is the link's
    /// duplex, which pause turns on.
    fn start(&mut self, speed: Speed, full: bool) {
        // A link that returns finds the MAC still on: a drop turns nothing
        // off (`ask_link`), so its line says the speed was set again. A
        // link still held is a speed changed with no drop read between
        // (the review of 2026-11-19: the line claimed a drop then).
        let how = match (self.genet.started(), self.link) {
            (false, _) => "the MAC's transmitter and receiver are on at",
            (true, None) => "the MAC, on since before the link dropped, is set again at",
            (true, Some(_)) => "the MAC, on at the speed before, is set again at",
        };
        self.genet.start(&mut self.bus, speed, Receiver::On);
        self.link = Some((speed, full));
        // A link that did not read is said again after the next drop: the
        // line is once a link, not once a boot (the same review).
        self.said_link_error = false;
        let cmd = genet::Bus::read(&mut self.bus, genet::reg::UMAC_CMD);
        console::println!(
            "Ouroboros kernel: genet: {} {} Mb/s (UMAC_CMD {:#010x}, UMAC_MDF_CTRL {:#010x}): frames are sent, and received on the receive interrupt and the tick's poll",
            how,
            speed.mbps(),
            cmd,
            genet::Bus::read(&mut self.bus, genet::reg::UMAC_MDF_CTRL)
        );
        self.say_pause(cmd, full);
    }

    /// Says what each end of the link advertises of pause, what that
    /// agrees, and what the MAC does, read off `cmd` (`UMAC_CMD` as read
    /// after the start, `genet::mac::pause_in_cmd`), not assumed: two MDIO
    /// reads, nothing written, on every start (the boot's, and each a wait
    /// or a send makes when the link comes up or changes speed) and on a
    /// change of duplex alone. The answer to #480's
    /// review, which found the MAC's pause bits set without either end's
    /// advertisement ever read (`genet::phy::pause`). Both registers are
    /// printed whole. A partner whose page was not received (`LPA_LPACK`
    /// clear) is said to be unheard, not to offer no pause. A read that
    /// fails says so and changes nothing.
    fn say_pause(&mut self, cmd: u32, full: bool) {
        fn offered(bits: u16) -> &'static str {
            match (bits & phy::PAUSE_CAP != 0, bits & phy::PAUSE_ASYM != 0) {
                (false, false) => "no pause",
                (true, false) => "symmetric pause",
                (false, true) => "asymmetric pause",
                (true, true) => "symmetric and asymmetric pause",
            }
        }
        fn what(a: phy::Agreed) -> &'static str {
            match (a.send, a.honour) {
                (true, true) => "pause both ways",
                (true, false) => "sending pause and not honouring it",
                (false, true) => "honouring pause and not sending it",
                (false, false) => "no pause",
            }
        }
        let Some((addr, _)) = self.phy else {
            return;
        };
        let does = genet::mac::pause_in_cmd(cmd);
        match phy::pause(&mut self.bus, addr) {
            Ok(p) => match p.agreed(full) {
                Some(agreed) => console::println!(
                    "Ouroboros kernel: genet: pause: this end advertises {}, the partner {} (ADVERTISE {:#06x}, LPA {:#06x}); the link agrees {}, the MAC does {}: {}",
                    offered(p.ours()),
                    offered(p.theirs()),
                    p.advertise,
                    p.lpa,
                    what(agreed),
                    what(does),
                    if agreed == does { "as agreed" } else { "NOT as agreed" }
                ),
                None => console::println!(
                    "Ouroboros kernel: genet: pause: this end advertises {}, the partner's page was not received (ADVERTISE {:#06x}, LPA {:#06x}, bit 14 clear): nothing agreed; the MAC does {}",
                    offered(p.ours()),
                    p.advertise,
                    p.lpa,
                    what(does)
                ),
            },
            Err(e) => console::println!("Ouroboros kernel: genet: pause: the advertisements did not read ({e:?}); the MAC does {}", what(does)),
        }
    }

    /// Finds the PHY, sets it up when it is one the crate knows, and logs
    /// the link: the speed of a link that is up, `None` for anything
    /// else. Whatever happens, the device stays installed: without a
    /// link nothing is started, and the address still answers.
    fn bring_up_phy(&mut self, registers: &genet::Registers) -> Option<(Speed, bool)> {
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
                return Some((speed, full));
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
    /// MAC's receiver is off (a boot with no link yet), which is the
    /// crate's rule (`Genet::has_frame`). A "yes" is one
    /// [`poll_frame`](Self::poll_frame) ends: it takes the frame or drops
    /// it, and either moves the index this compares.
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
    /// 2. No link held (a boot that found none, or a link read down
    ///    since) refuses the send unless it is up now. The link is
    ///    asked for once in this send, with no wait for it to come up (up
    ///    to three MDIO reads, each of which may poll for 20 ms on a bus
    ///    that does not answer, and two more after the start when it is
    ///    up), and the MAC started if it is up
    ///    (decision D3); otherwise the send is refused, and a link that
    ///    did not read at all has a line, once a link. Every send asks
    ///    again while no link is held, unless three asks running have
    ///    failed to read (`genet::linkask`'s back-off: the send is then
    ///    refused unasked, and the wait asks once a minute); one held is
    ///    not asked here (the wait asks it once a second,
    ///    [`Device::link_at_wait`]).
    /// 3. `tx_idle` is asked **before the copy**: the buffer is the one
    ///    the device may still be reading the last frame from, after a
    ///    send that timed out.
    /// 4. The copy, then the crate's register writes, each with its
    ///    `dsb sy` in front of it (`bus`'s `genet_write32`): the barrier is what
    ///    orders the buffer's stores before the write that tells the
    ///    device to read them.
    ///
    /// Four things are said, once each: the first frame the device
    /// takes, the first send that times out, the first refused as busy,
    /// and the first whose link did not read.
    pub(crate) fn send_frame(&mut self, frame: &[u8]) -> Result<(), SendError> {
        let len = frame.len();
        if !(TX_MIN_FRAME..=TX_MAX_FRAME).contains(&len) {
            return Err(SendError::Tx(TxError::BadLength(len)));
        }
        if self.link.is_none() {
            // Backed off, the bus is read once a minute in all, by the
            // wait: a send that asked would read a stuck bus once a send.
            if self.link_ask.backed_off() {
                return Err(SendError::NoLink);
            }
            match self.ask_link("a send") {
                Ok(true) => {}
                // Down, negotiating, up at a speed the PHY does not
                // name, or no PHY: no line, it is the ordinary state of
                // no cable.
                Ok(false) => return Err(SendError::NoLink),
                // The PHY did not answer, or the bus is stuck: not the
                // same thing as a link that is down.
                Err(e) => {
                    if !self.said_link_error {
                        self.said_link_error = true;
                        console::println!(
                            "Ouroboros kernel: genet: a send asked the link and it did not read ({e:?}): nothing is sent; said once"
                        );
                    }
                    return Err(SendError::NoLink);
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
            return Err(SendError::Tx(TxError::Busy));
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
                Err(SendError::Tx(e))
            }
        }
    }

    /// Says how many frames were taken and how many dropped, by cause,
    /// when the count taken reaches 256 and at each 65536: past the first
    /// the ring of 256 buffers has been round, and past the second the
    /// 16-bit index has wrapped, which is what the board's check asks to
    /// have seen. Without these the log holds the first frame and the
    /// first drop and nothing after.
    fn say_counts(&self) {
        if self.received == DESCS as u32 || self.received.is_multiple_of(65536) {
            let d = self.genet.drops();
            console::println!(
                "Ouroboros kernel: genet: {} frames received so far; dropped: {} flagged by the device, {} not whole, {} too short, {} too long; {} receive interrupts taken",
                self.received,
                d.flagged,
                d.fragment,
                d.runt,
                d.oversize,
                self.interrupts
            );
        }
    }

    /// The receive interrupt, for `nic.rs` to report: `main.rs` enables it
    /// at the GIC and `exceptions.rs` wakes `netd` on it. It means receive
    /// done and nothing else, since the crate unmasks that bit alone, and
    /// [`ack_interrupt`](Self::ack_interrupt) takes it down at the device
    /// until the reader's next wait rearms it: `nic.rs`'s two promises.
    /// Before the MAC's receiver is on (a boot that found no link) nothing
    /// is unmasked and the line stays low.
    pub(crate) fn intid(&self) -> Option<u32> {
        self.intid
    }

    /// Masks and clears receive done at the device, so the level-triggered
    /// line drops and stays down until the reader's next wait finds the
    /// ring empty and rearms it ([`Device::rearm_interrupt`]), and counts
    /// the interrupt. From
    /// the IRQ path under the kernel lock, before `netd` is woken; the
    /// first one taken has a line. Masked, not only cleared: cleared alone,
    /// the device raised it again for frames still waiting (the crate's
    /// `ack_rx_interrupt`, which keeps the masked state its rearm reads).
    ///
    /// The crate reads the mask back after its two writes, so they have
    /// reached the GENET when the read returns; the `dsb` after it holds
    /// the IRQ path's end of interrupt, a write to the GIC, until the read
    /// has returned. That orders the end of interrupt after the device's
    /// own registers took the mask. It does not prove the line's fall has
    /// reached the GIC's input through the SoC's wiring: if it lagged, the
    /// GIC would deliver once more, this ack would run on a masked
    /// interrupt (harmless) and count it, and `netd` would be woken with
    /// nothing new.
    pub(crate) fn ack_interrupt(&mut self) {
        self.genet.ack_rx_interrupt(&mut self.bus);
        // SAFETY: a barrier alone; no memory or register is named.
        unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
        if self.interrupts == 0 {
            console::println!(
                "Ouroboros kernel: genet: first receive interrupt taken (INTID {}), and masked at the device until the ring is drained",
                self.intid.unwrap_or(0)
            );
        }
        self.interrupts = self.interrupts.saturating_add(1);
    }

    /// Asks the PHY for the link once, with no wait for it to come up (up
    /// to three MDIO reads, each of which may poll for 20 ms on a bus that
    /// does not answer), and acts on what changed by the crate's rule
    /// (`genet::linkask::change`), with a line naming `who` asked: a link
    /// up where none was held, or up at another speed, starts the MAC at
    /// it (and then two reads more for [`Device::say_pause`], as does a
    /// change of duplex alone); a held link read down is let go, and sends
    /// are refused until it is up again. Every ask tells `link_ask`
    /// whether it read, the wait's and a send's alike, so the two agree on
    /// the bus (the review of 2026-11-19): three failing running back the
    /// asking off, and a read brings it back, each with a line. `Ok(true)` when the link is up
    /// after the ask; `Ok(false)` for a link not up, or no PHY to ask. The
    /// one rule for asking after the boot's wait (decision D3), for a send
    /// and for `netd`'s wait alike.
    fn ask_link(&mut self, who: &str) -> Result<bool, genet::MdioError> {
        let Some((addr, id)) = self.phy else {
            return Ok(false);
        };
        let now = match phy::link_resolved(&mut self.bus, addr, id) {
            Ok(now) => {
                if self.link_ask.answered(true) == Answered::Back {
                    console::println!(
                        "Ouroboros kernel: genet: the link reads again at {who}: the wait asks it once a second again, and a send asks while no link is held"
                    );
                }
                now
            }
            Err(e) => {
                if self.link_ask.answered(false) == Answered::BackedOff {
                    console::println!(
                        "Ouroboros kernel: genet: the link was asked {} times running and did not read (last at {who}: {e:?}): the wait asks it once a minute, and a send does not ask, until it reads",
                        genet::linkask::FAILED_ASKS_MAX
                    );
                }
                return Err(e);
            }
        };
        match genet::linkask::change(self.link, now) {
            Change::Same => {}
            Change::Up { speed, full } => {
                console::println!(
                    "Ouroboros kernel: genet: link up at {who}, {} Mb/s, {} duplex{}",
                    speed.mbps(),
                    if full { "full" } else { "half" },
                    if self.link.is_some() { ", the speed changed" } else { "" }
                );
                self.start(speed, full);
            }
            Change::Duplex { full } => {
                // The MAC is not told the duplex (genet::mac's module doc),
                // so nothing is started; pause turns on it, so its line is
                // said again against UMAC_CMD as it stands.
                if let Some(held) = self.link.as_mut() {
                    held.1 = full;
                }
                console::println!(
                    "Ouroboros kernel: genet: the link's duplex changed at {who}, now {}: the MAC is not told the duplex, so nothing is started",
                    if full { "full" } else { "half" }
                );
                let cmd = genet::Bus::read(&mut self.bus, genet::reg::UMAC_CMD);
                self.say_pause(cmd, full);
            }
            Change::Down => {
                self.link = None;
                console::println!(
                    "Ouroboros kernel: genet: link down at {who} ({}): sends are refused until it is up again; the MAC stays on",
                    now.describe()
                );
            }
        }
        Ok(self.link.is_some())
    }

    /// From `NET_WAIT`, under the kernel lock, before the reader blocks:
    /// asks the link when `genet::linkask` says it is due (once a second)
    /// and acts on what changed ([`Device::ask_link`]), and returns how
    /// long the wait may block in ms, the time left until the next ask,
    /// so the reader comes back to ask. `None`, no limit, with no PHY to
    /// ask. **So `netd` is woken once a second for as long as the device
    /// lives** (once a minute after the back-off), a pass that finds
    /// nothing: the price of asking from the wait, since the tick may not
    /// (`has_frame` may neither write nor wait, and an MDIO read does both).
    ///
    /// **Why the wait asks** (left open by the plan's step 7): until
    /// 2026-11-17 only a send asked the link (D3), and nothing makes
    /// `netd` send, so a cable put in after the boot's wait left the board
    /// deaf, its receiver off, until a command typed on it sent a frame.
    /// The wait is where `netd` comes back to, as step 9's rearm is.
    /// **Since 2026-11-18 it asks with the MAC on too**, so a link that
    /// drops is let go and one that returns, at any speed, is started
    /// again at it; before, the asking ended at the start and the MAC kept
    /// the speed it was started on for good. Up, it costs the status read
    /// twice and the auxiliary status, three MDIO reads a second.
    ///
    /// **Asks that fail slow it to once a minute**, three running
    /// (`linkask`'s rule, never a stop since the review of #487), with a
    /// line each way: a stuck bus costs 20 ms a read, up to three reads an
    /// ask that fails, under the kernel lock (five on the ask that finds
    /// the link up). A send's asks count with these, and a send does not
    /// ask while the asking is backed off ([`Device::ask_link`]).
    pub(crate) fn link_at_wait(&mut self) -> Option<u64> {
        // With no PHY there is nothing to come back for.
        self.phy?;
        match self.link_ask.at_wait(timer::monotonic_us()) {
            Ask::Later { in_us } => Some(in_us.div_ceil(1000)),
            Ask::Now => {
                // Whether it read is `link_ask`'s, told in `ask_link`.
                let _ = self.ask_link("a wait");
                Some(self.link_ask.interval_us() / 1000)
            }
        }
    }

    /// Rearms the receive interrupt an ack masked, if no frame waits: from
    /// `NET_WAIT`, under the kernel lock, before the reader blocks. `false`
    /// when a frame waits: it stays masked, and `NET_WAIT` returns for the
    /// frame instead of blocking; the next wait asks again. One that
    /// arrives after the rearm raises the line. The crate writes nothing
    /// when no ack masked it, so a wait that follows a wait costs nothing.
    pub(crate) fn rearm_interrupt(&mut self) -> bool {
        self.genet.rearm_rx_interrupt(&mut self.bus)
    }

    /// Takes one received frame if a usable one waits: copies it into
    /// `out`, cut to `out`'s length, gives its buffer back to the device,
    /// and returns the frame's own length (14 to
    /// `genet::mac::RX_MAX_FRAME`), which can be more than `out`'s
    /// (`Nic::poll_frame` makes that a dropped frame). Under the kernel lock, with no wait.
    ///
    /// A frame the crate refuses (not whole, flagged by the device, too
    /// short or too long) is released by the crate and the next one
    /// looked at, at most a ring's worth in one call, so a call ends
    /// whatever the device holds. `None` after that many refusals leaves
    /// `has_frame` to say whether more wait.
    ///
    /// Two things are said, once each: the first frame taken, and the
    /// first one dropped, with its cause. The counts are said as the
    /// frames taken pass a ring's worth and each 65536 after
    /// ([`say_counts`](Self::say_counts)).
    pub(crate) fn poll_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        for _ in 0..DESCS {
            match self.genet.rx_next(&mut self.bus) {
                Rx::None => return None,
                Rx::Dropped(why) => {
                    if !self.said_drop {
                        self.said_drop = true;
                        // A flagged frame's status in hex: its bits are
                        // the device's error flags, SOP and EOP.
                        match why {
                            RxDrop::Flagged(status) => console::println!(
                                "Ouroboros kernel: genet: a received frame was dropped (flagged by the device, status {status:#06x}; {} taken before it); said once",
                                self.received
                            ),
                            other => console::println!(
                                "Ouroboros kernel: genet: a received frame was dropped ({other:?}; {} taken before it); said once",
                                self.received
                            ),
                        }
                    }
                }
                Rx::Frame { buffer, offset, len } => {
                    let n = len.min(out.len());
                    let (area, _) = dma_region();
                    let from = area as usize + buffer * RX_BUF_LEN + offset;
                    // SAFETY: the crate returns `buffer` below `DESCS` and
                    // `offset + len` inside one buffer (its `Rx::Frame`
                    // doc, and its assert that the longest frame fits a
                    // buffer after the pad), so the `n` bytes read are
                    // inside `RX_AREA`. The device does not write this
                    // buffer between `rx_next` and `rx_release`. Read
                    // through a raw pointer: no reference to memory the
                    // device writes is made. `out` is the caller's, `n`
                    // inside it.
                    unsafe {
                        core::ptr::copy_nonoverlapping(from as *const u8, out.as_mut_ptr(), n);
                    }
                    self.genet.rx_release(&mut self.bus);
                    if self.received == 0 {
                        console::println!(
                            "Ouroboros kernel: genet: first frame received: {len} bytes, from buffer {buffer} of the receive area"
                        );
                    }
                    self.received = self.received.saturating_add(1);
                    self.say_counts();
                    return Some(len);
                }
            }
        }
        None
    }
}

//! virtio-blk: synchronous sector reads and writes over `virtio_mmio`'s
//! transport. Phase 3a of `docs/roadmap.md` built the read path; write
//! support (`write_sector`) was added later, for `mkdir`/`rmdir`
//! (`fat32.rs`) - both share the same request machinery
//! ([`Device::submit_request`]), since a virtio-blk write is a read
//! request with one flag flipped (see that function's doc comment).
//! Interrupt-driven completion is still deliberately out of scope - this
//! polls the used ring rather than wiring the device's IRQ, matching
//! every other driver in this kernel so far (`uart.rs`/`uart16550.rs`
//! poll too) and avoiding a dependency on confirming this specific
//! device's INTID, which - unlike GICv2/the timer's fixed addresses -
//! depends on *which* of the 32 virtio-mmio slots QEMU happens to
//! populate.
//!
//! ## Why cache maintenance isn't needed here, unlike `tasks.rs`'s EL0 code
//!
//! The virtqueue memory below (descriptor table, avail/used rings, and
//! the request buffers `read_sector` is given) is written by the CPU and
//! read by the device, or vice versa - real DMA, not the CPU reading its
//! own writes. On real hardware that would ordinarily need explicit cache
//! maintenance (clean before the device reads, invalidate before the CPU
//! reads what the device wrote), the same category of problem
//! `tasks.rs::clean_dcache_range` solves for self-modifying EL0 code. It
//! isn't needed here because the devicetree dump that confirmed this
//! transport's addresses (see `virtio_mmio.rs`) also showed
//! `dma-coherent;` on every `virtio_mmio` node - QEMU is telling us this
//! platform's virtio DMA is cache-coherent, not something to assume.
//! Ordinary memory barriers (`dsb`) are still needed, for *ordering*
//! (making sure the avail ring update is visible before the doorbell
//! write, and re-reading the used ring isn't hoisted out of the poll
//! loop) - a different concern from coherence.

use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};

use crate::virtio_mmio::{self, read_reg, write_reg};

pub const DEVICE_ID: u32 = 2;

const QUEUE_SIZE: usize = 8;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

const BLK_T_IN: u32 = 0; // read
const BLK_T_OUT: u32 = 1; // write
const BLK_S_OK: u8 = 0;

#[derive(Debug)]
pub enum Error {
    NotFound,
    UnexpectedVersion(u32),
    FeaturesRejected,
    QueueTooSmall(u32),
    Io(u8),
}

// A hand-written impl, not just the derived Debug above - see
// loader.rs's `LoaderError` for why: rustc's dead-code analysis doesn't
// count a field as used just because `#[derive(Debug)]` prints it.
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotFound => write!(f, "no virtio-mmio block device found"),
            Error::UnexpectedVersion(v) => write!(f, "unsupported virtio-mmio version {v} (only 2, modern, is implemented)"),
            Error::FeaturesRejected => write!(f, "device rejected VIRTIO_F_VERSION_1"),
            Error::QueueTooSmall(max) => write!(f, "device's max queue size ({max}) is smaller than QUEUE_SIZE ({QUEUE_SIZE})"),
            Error::Io(status) => write!(f, "request failed, device status byte {status:#04x}"),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

impl Desc {
    const fn zeroed() -> Self {
        Desc { addr: 0, len: 0, flags: 0, next: 0 }
    }
}

#[repr(C)]
struct Avail {
    flags: u16,
    idx: u16,
    ring: [u16; QUEUE_SIZE],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UsedElem {
    id: u32,
    len: u32,
}

#[repr(C)]
struct Used {
    flags: u16,
    idx: u16,
    ring: [UsedElem; QUEUE_SIZE],
}

#[repr(C)]
struct BlkReqHeader {
    req_type: u32,
    reserved: u32,
    sector: u64,
}

// Each piece of virtqueue state gets its own aligned static, matching
// this project's established UnsafeCell-wrapper idiom for driver-owned
// memory (see mmu.rs's `Table`, tasks.rs's `IdleRegion`). Alignments are
// the virtio spec's minimums (desc table: 16, avail: 2, used: 4) - the
// modern (non-legacy) interface reports each ring's address separately
// (`REG_QUEUE_{DESC,DRIVER,DEVICE}_{LOW,HIGH}`), so unlike the legacy
// interface there's no single combined-region alignment/padding to get
// right, just these three independently.

#[repr(align(16))]
struct DescTable(UnsafeCell<[Desc; QUEUE_SIZE]>);
// SAFETY: single-core; only touched from this module, never concurrently
// (this driver has exactly one request in flight at a time - see
// `read_sector`).
unsafe impl Sync for DescTable {}
static DESC_TABLE: DescTable = DescTable(UnsafeCell::new([Desc::zeroed(); QUEUE_SIZE]));

#[repr(align(2))]
struct AvailRing(UnsafeCell<Avail>);
unsafe impl Sync for AvailRing {}
static AVAIL_RING: AvailRing =
    AvailRing(UnsafeCell::new(Avail { flags: 0, idx: 0, ring: [0; QUEUE_SIZE] }));

#[repr(align(4))]
struct UsedRing(UnsafeCell<Used>);
unsafe impl Sync for UsedRing {}
static USED_RING: UsedRing = UsedRing(UnsafeCell::new(Used {
    flags: 0,
    idx: 0,
    ring: [UsedElem { id: 0, len: 0 }; QUEUE_SIZE],
}));

struct ReqHeader(UnsafeCell<BlkReqHeader>);
unsafe impl Sync for ReqHeader {}
static REQ_HEADER: ReqHeader = ReqHeader(UnsafeCell::new(BlkReqHeader { req_type: 0, reserved: 0, sector: 0 }));

struct ReqStatus(UnsafeCell<u8>);
unsafe impl Sync for ReqStatus {}
static REQ_STATUS: ReqStatus = ReqStatus(UnsafeCell::new(0xff));

pub struct Device {
    base: u64,
}

impl Device {
    /// Scans every virtio-mmio slot for a block device (`DEVICE_ID`, 2) -
    /// see `virtio_mmio::find_device`. Does not touch the device's state.
    ///
    /// # Safety
    /// The low-1GB device region must already be mapped (true from very
    /// early in `main()`).
    pub unsafe fn discover() -> Result<Self, Error> {
        let base = unsafe { virtio_mmio::find_device(DEVICE_ID) }.ok_or(Error::NotFound)?;
        let version = unsafe { read_reg(base, virtio_mmio::REG_VERSION) };
        if version != 2 {
            // Legacy (version 1) isn't implemented - see virtio_mmio.rs's
            // module doc comment. A real value other than 1 or 2 would
            // mean a future spec revision this driver doesn't know either.
            return Err(Error::UnexpectedVersion(version));
        }
        Ok(Device { base })
    }

    /// Resets the device, negotiates the minimal feature set
    /// (`VIRTIO_F_VERSION_1` only - no optional virtio-blk or ring
    /// features), and sets up one virtqueue. Leaves the device
    /// `DRIVER_OK` and ready for [`read_sector`](Self::read_sector).
    ///
    /// # Safety
    /// Must be called at most once per boot (the virtqueue statics are
    /// shared, single-instance state), after [`discover`](Self::discover).
    pub unsafe fn init(&mut self) -> Result<(), Error> {
        let base = self.base;

        // Reset unconditionally, regardless of what state the device is
        // already in - see virtio_mmio.rs's module doc comment on why
        // this device is not necessarily in its reset state when we get
        // it (EDK2's own virtio-blk driver already used it, during boot
        // services, to load this very kernel).
        unsafe {
            write_reg(base, virtio_mmio::REG_STATUS, 0);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE | virtio_mmio::STATUS_DRIVER);

            // Feature negotiation: read what the device actually offers
            // first (word 1, bits 32-63) rather than assuming - a v2
            // device is spec-required to offer VIRTIO_F_VERSION_1 (word 1
            // bit 0), but checking rather than assuming is the same
            // discipline as everywhere else in this project (see e.g.
            // mmu.rs reading TCR_EL1/ID_AA64MMFR0_EL1 back rather than
            // trusting a value it wrote or guessed). Request only that
            // one bit - no optional virtio-blk features (F_RO,
            // F_BLK_SIZE, F_FLUSH, ...) and no optional ring features
            // (F_EVENT_IDX, F_INDIRECT_DESC) - none of them are needed
            // for a single-segment synchronous read, and accepting fewer
            // optional features means less to get wrong.
            write_reg(base, virtio_mmio::REG_DEVICE_FEATURES_SEL, 1);
            let offered_hi = read_reg(base, virtio_mmio::REG_DEVICE_FEATURES);
            if offered_hi & (1 << 0) == 0 {
                return Err(Error::FeaturesRejected); // VIRTIO_F_VERSION_1 not offered - not a real v2 device.
            }
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES_SEL, 1);
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES, 1 << 0); // VIRTIO_F_VERSION_1, word 1 bit 0
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES_SEL, 0);
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES, 0);

            write_reg(
                base,
                virtio_mmio::REG_STATUS,
                virtio_mmio::STATUS_ACKNOWLEDGE | virtio_mmio::STATUS_DRIVER | virtio_mmio::STATUS_FEATURES_OK,
            );
            let status = read_reg(base, virtio_mmio::REG_STATUS);
            if status & virtio_mmio::STATUS_FEATURES_OK == 0 {
                return Err(Error::FeaturesRejected);
            }

            // Queue 0 - virtio-blk has exactly one request queue when
            // VIRTIO_BLK_F_MQ isn't negotiated, which it isn't.
            write_reg(base, virtio_mmio::REG_QUEUE_SEL, 0);
            let max = read_reg(base, virtio_mmio::REG_QUEUE_NUM_MAX);
            if (max as usize) < QUEUE_SIZE {
                return Err(Error::QueueTooSmall(max));
            }
            write_reg(base, virtio_mmio::REG_QUEUE_NUM, QUEUE_SIZE as u32);

            let desc_addr = DESC_TABLE.0.get() as u64;
            let avail_addr = AVAIL_RING.0.get() as u64;
            let used_addr = USED_RING.0.get() as u64;
            write_reg(base, virtio_mmio::REG_QUEUE_DESC_LOW, desc_addr as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DESC_HIGH, (desc_addr >> 32) as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DRIVER_LOW, avail_addr as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DRIVER_HIGH, (avail_addr >> 32) as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DEVICE_LOW, used_addr as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DEVICE_HIGH, (used_addr >> 32) as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_READY, 1);

            write_reg(
                base,
                virtio_mmio::REG_STATUS,
                virtio_mmio::STATUS_ACKNOWLEDGE
                    | virtio_mmio::STATUS_DRIVER
                    | virtio_mmio::STATUS_FEATURES_OK
                    | virtio_mmio::STATUS_DRIVER_OK,
            );
        }
        Ok(())
    }

    /// The device's capacity, in 512-byte sectors - `virtio_blk_config`'s
    /// first field, always present regardless of negotiated features.
    pub fn capacity_sectors(&self) -> u64 {
        unsafe {
            let lo = read_reg(self.base, virtio_mmio::REG_CONFIG) as u64;
            let hi = read_reg(self.base, virtio_mmio::REG_CONFIG + 4) as u64;
            lo | (hi << 32)
        }
    }

    /// Reads one 512-byte sector.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn read_sector(&mut self, sector: u64, buf: &mut [u8; 512]) -> Result<(), Error> {
        // Device-writable (VIRTQ_DESC_F_WRITE) - the device fills this
        // buffer for us.
        unsafe { self.submit_request(BLK_T_IN, sector, buf.as_mut_ptr(), VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT) }
    }

    /// Writes one 512-byte sector. The only difference from
    /// [`read_sector`](Self::read_sector) is which way the data
    /// descriptor's `VIRTQ_DESC_F_WRITE` flag points: here the *device*
    /// reads from `buf` (device-readable, no `F_WRITE`), the reverse of a
    /// read request. First write support this kernel has ever had -
    /// added for phase 4's `mkdir`/`rmdir`, see `fat32.rs`.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn write_sector(&mut self, sector: u64, buf: &[u8; 512]) -> Result<(), Error> {
        unsafe { self.submit_request(BLK_T_OUT, sector, buf.as_ptr() as *mut u8, VIRTQ_DESC_F_NEXT) }
    }

    /// Shared machinery for both requests above: builds the standard
    /// 3-descriptor virtio-blk request (header, data, status), notifies
    /// the device, and polls the used ring until it completes. Only one
    /// request is ever in flight, so there's no bookkeeping across calls;
    /// each call owns the whole queue for its duration. `data_flags` is
    /// the one real difference between a read and a write: whether the
    /// data descriptor is device-writable or device-readable. The
    /// request-type field in the header and the direction data actually
    /// flows are otherwise the only distinction.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init). `data` must be valid
    /// for 512 bytes, readable if `data_flags` omits `VIRTQ_DESC_F_WRITE`
    /// (a write request) or writable if it's set (a read request).
    unsafe fn submit_request(&mut self, req_type: u32, sector: u64, data: *mut u8, data_flags: u16) -> Result<(), Error> {
        let header = unsafe { &mut *REQ_HEADER.0.get() };
        header.req_type = req_type;
        header.reserved = 0;
        header.sector = sector;

        let status_byte = REQ_STATUS.0.get();
        unsafe { write_volatile(status_byte, 0xff) }; // sentinel distinct from any real status code

        let desc = unsafe { &mut *DESC_TABLE.0.get() };
        desc[0] = Desc {
            addr: header as *const BlkReqHeader as u64,
            len: core::mem::size_of::<BlkReqHeader>() as u32,
            flags: VIRTQ_DESC_F_NEXT,
            next: 1,
        };
        desc[1] = Desc { addr: data as u64, len: 512, flags: data_flags, next: 2 };
        desc[2] = Desc { addr: status_byte as u64, len: 1, flags: VIRTQ_DESC_F_WRITE, next: 0 };

        let avail = unsafe { &mut *AVAIL_RING.0.get() };
        let used = unsafe { &*USED_RING.0.get() };
        let slot = (avail.idx as usize) % QUEUE_SIZE;
        avail.ring[slot] = 0; // head descriptor index
        let seen_used_idx = unsafe { read_volatile(&used.idx) };

        unsafe {
            // Ordering barrier: the avail ring update above must be
            // visible in memory before idx bumps (so the device never
            // observes a partially-written entry) and before the
            // doorbell write below (so the device, once notified,
            // definitely sees this request) - not a coherence concern
            // (see module doc comment), a genuine ordering one.
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
        avail.idx = avail.idx.wrapping_add(1);
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write_reg(self.base, virtio_mmio::REG_QUEUE_NOTIFY, 0); // queue 0
        }

        loop {
            unsafe {
                core::arch::asm!("dmb sy", options(nostack, preserves_flags));
            }
            let idx = unsafe { read_volatile(&used.idx) };
            if idx != seen_used_idx {
                break;
            }
        }

        let result = unsafe { read_volatile(status_byte) };
        if result != BLK_S_OK {
            return Err(Error::Io(result));
        }
        Ok(())
    }
}

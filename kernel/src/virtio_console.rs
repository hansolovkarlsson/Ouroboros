//! virtio-console: a transmit-only driver over `virtio_mmio`'s transport -
//! the real lead for console output on Parallels, first raised (and
//! deliberately deferred) in the "Console discovery" section of
//! `CLAUDE.md`: Parallels' Apple Silicon virtualization exposes its
//! serial port via virtio (`console=hvc0` is the standard Linux console
//! parameter for this class of VM), not a classic PL011/16550 UART -
//! which is exactly why all three of `devicetree.rs`/`acpi.rs`/`pci.rs`
//! come back empty there.
//!
//! Modeled directly on `virtio_blk.rs`'s structure (discover/init/one
//! virtqueue/poll-based synchronous completion) - see that module's doc
//! comment for the reasoning behind the shared conventions (modern,
//! non-legacy register interface; no interrupt wiring, since this
//! device's IRQ line depends on which of the 32 virtio-mmio slots it
//! lands on, same reasoning as every other driver in this kernel so
//! far). The real differences: this is transmit-only (no `read`/RX
//! support at all this phase - see the module-level "Still coarse" note
//! at the end), and a message is a single variable-length descriptor
//! rather than virtio-blk's fixed 3-descriptor request shape.
//!
//! ## Queue numbering is fixed by the virtio-console spec, not chosen
//!
//! Port 0 (the always-present default console port) has its receiveq0
//! at virtqueue index 0 and its transmitq0 at index 1 - true whether or
//! not `VIRTIO_CONSOLE_F_MULTIPORT` is negotiated, which it isn't here
//! (multiport support needs a whole separate control queue pair this
//! driver has no use for with exactly one port). This driver only ever
//! configures queue 1 - receiveq0 (queue 0) is deliberately left
//! unconfigured/not-ready, since nothing here reads from it yet. Per
//! spec a driver only needs to set up the queues it actually uses.
//!
//! ## Still coarse, worth knowing before building on this
//!
//! No RX/input support - this is output-only, so a Parallels boot only
//! gets to *see* the shell, not type into it, until a receive path is
//! added (a second virtqueue, symmetrically simpler than transmit: post
//! device-writable buffers to receiveq0, poll the used ring for
//! arrivals). No `VIRTIO_CONSOLE_F_SIZE`/`F_MULTIPORT`/`F_EMERG_WRITE` -
//! none negotiated, matching virtio_blk.rs's "no optional features"
//! discipline. `write`'s per-call virtqueue round-trip is real overhead
//! compared to a raw UART's direct MMIO byte write - acceptable for a
//! first cut (correctness over throughput), but `write_byte`-per-typed-
//! character (see `console.rs`) means every keystroke echo pays a full
//! round trip, unlike `write_str`'s batched sends.

use core::cell::UnsafeCell;
use core::ptr::read_volatile;

use crate::virtio_mmio::{self, read_reg, write_reg};

pub const DEVICE_ID: u32 = 3;

const QUEUE_SIZE: usize = 8;
const TRANSMIT_QUEUE: u32 = 1; // port0's transmitq0 - see module doc comment.

#[derive(Debug)]
pub enum Error {
    NotFound,
    UnexpectedVersion(u32),
    FeaturesRejected,
    QueueTooSmall(u32),
}

// A hand-written impl, not just #[derive(Debug)] - see virtio_blk.rs's
// Error impl (and loader.rs's LoaderError) for why: rustc's dead-code
// analysis doesn't count a field as used just because a derived Debug
// prints it.
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotFound => write!(f, "no virtio-mmio console device found"),
            Error::UnexpectedVersion(v) => write!(f, "unsupported virtio-mmio version {v} (only 2, modern, is implemented)"),
            Error::FeaturesRejected => write!(f, "device rejected VIRTIO_F_VERSION_1"),
            Error::QueueTooSmall(max) => write!(f, "device's max queue size ({max}) is smaller than QUEUE_SIZE ({QUEUE_SIZE})"),
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

// Same per-piece-of-virtqueue-state static idiom as virtio_blk.rs - see
// that module's comment for why each gets its own aligned static rather
// than one combined region (the modern/non-legacy interface reports
// each ring's address separately, so there's no shared alignment/padding
// to get right).

#[repr(align(16))]
struct DescTable(UnsafeCell<[Desc; QUEUE_SIZE]>);
// SAFETY: single-core; only touched from this module, never concurrently
// (one message in flight at a time - see `Device::write`).
unsafe impl Sync for DescTable {}
static DESC_TABLE: DescTable = DescTable(UnsafeCell::new([Desc::zeroed(); QUEUE_SIZE]));

#[repr(align(2))]
struct AvailRing(UnsafeCell<Avail>);
unsafe impl Sync for AvailRing {}
static AVAIL_RING: AvailRing = AvailRing(UnsafeCell::new(Avail { flags: 0, idx: 0, ring: [0; QUEUE_SIZE] }));

#[repr(align(4))]
struct UsedRing(UnsafeCell<Used>);
unsafe impl Sync for UsedRing {}
static USED_RING: UsedRing = UsedRing(UnsafeCell::new(Used {
    flags: 0,
    idx: 0,
    ring: [UsedElem { id: 0, len: 0 }; QUEUE_SIZE],
}));

pub struct Device {
    base: u64,
}

impl Device {
    /// Scans every virtio-mmio slot for a console device (`DEVICE_ID`,
    /// 3) - see `virtio_mmio::find_device`. Does not touch the device's
    /// state.
    ///
    /// # Safety
    /// The low-1GB device region must already be mapped.
    pub unsafe fn discover() -> Result<Self, Error> {
        let base = unsafe { virtio_mmio::find_device(DEVICE_ID) }.ok_or(Error::NotFound)?;
        let version = unsafe { read_reg(base, virtio_mmio::REG_VERSION) };
        if version != 2 {
            return Err(Error::UnexpectedVersion(version));
        }
        Ok(Device { base })
    }

    /// Resets the device, negotiates the minimal feature set
    /// (`VIRTIO_F_VERSION_1` only), and sets up transmitq0 (queue 1) -
    /// see the module doc comment for why receiveq0 (queue 0) is
    /// deliberately left unconfigured. Leaves the device `DRIVER_OK` and
    /// ready for [`write`](Self::write).
    ///
    /// # Safety
    /// Must be called at most once per boot (the virtqueue statics are
    /// shared, single-instance state), after [`discover`](Self::discover).
    pub unsafe fn init(&mut self) -> Result<(), Error> {
        let base = self.base;

        unsafe {
            // Reset unconditionally - same reasoning as virtio_blk.rs's
            // Device::init (a real, confirmed finding there: the device
            // is not necessarily already in its reset state when we get
            // it).
            write_reg(base, virtio_mmio::REG_STATUS, 0);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE | virtio_mmio::STATUS_DRIVER);

            // Feature negotiation: read what's actually offered rather
            // than assuming - same discipline as virtio_blk.rs. Only
            // VIRTIO_F_VERSION_1 (word 1 bit 0) is requested; no
            // optional virtio-console features (F_SIZE, F_MULTIPORT,
            // F_EMERG_WRITE) - none are needed for a single-port,
            // fixed-size, virtqueue-based transmit path.
            write_reg(base, virtio_mmio::REG_DEVICE_FEATURES_SEL, 1);
            let offered_hi = read_reg(base, virtio_mmio::REG_DEVICE_FEATURES);
            if offered_hi & (1 << 0) == 0 {
                return Err(Error::FeaturesRejected);
            }
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES_SEL, 1);
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES, 1 << 0);
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

            write_reg(base, virtio_mmio::REG_QUEUE_SEL, TRANSMIT_QUEUE);
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

    /// Sends `data` to the console, blocking until the device confirms
    /// receipt via the used ring - synchronous, polling completion,
    /// same model as `virtio_blk.rs` and for the same reason (no IRQ
    /// wiring for this device either). A single descriptor, since `data`
    /// is already one contiguous buffer - no chaining needed the way
    /// virtio-blk's 3-part request (header/data/status) requires it.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init). `data` must stay valid
    /// (readable, unmodified) for the duration of the call - true by
    /// construction here, since this never returns before the device has
    /// confirmed it read the buffer.
    pub unsafe fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }

        let desc = unsafe { &mut *DESC_TABLE.0.get() };
        desc[0] = Desc { addr: data.as_ptr() as u64, len: data.len() as u32, flags: 0, next: 0 };

        let avail = unsafe { &mut *AVAIL_RING.0.get() };
        let used = unsafe { &*USED_RING.0.get() };
        let slot = (avail.idx as usize) % QUEUE_SIZE;
        avail.ring[slot] = 0; // head (and only) descriptor index
        let seen_used_idx = unsafe { read_volatile(&used.idx) };

        unsafe {
            // Ordering barriers - not a coherence concern (this
            // platform's virtio DMA is confirmed cache-coherent, see
            // virtio_blk.rs's module doc comment for how), just making
            // sure the avail ring update is visible before the doorbell
            // write, and that re-reading the used ring below isn't
            // hoisted out of the poll loop.
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
        avail.idx = avail.idx.wrapping_add(1);
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write_reg(self.base, virtio_mmio::REG_QUEUE_NOTIFY, TRANSMIT_QUEUE);
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

        Ok(())
    }
}

//! virtio-net: raw Ethernet frame send/receive over `virtio_mmio`'s
//! transport, Stage 1 of the network stack (`docs/roadmap.md`). This is the
//! kernel-side NIC driver only - the DMA-capable half that, per the
//! no-IOMMU DMA constraint, must stay in the trusted EL1 kernel (a device
//! can DMA anywhere without an IOMMU, so the ring/buffer owner can't be an
//! untrusted EL0 task). The protocol stack (ARP/IP/ICMP/UDP/TCP) is a later
//! stage's userland `netd` server reached through gated syscalls; this
//! driver just moves opaque frames.
//!
//! Modeled directly on `virtio_blk.rs` (same transport, same
//! static-virtqueue idiom, same poll-the-used-ring completion - no IRQ
//! wiring, matching every other driver here). Two real differences from the
//! block driver:
//!
//! - **Two virtqueues, not one.** virtio-net has a receiveq (queue 0) and a
//!   transmitq (queue 1); each needs its own desc/avail/used rings, so the
//!   ring statics below are duplicated per direction. Receive buffers are
//!   *pre-posted* at init (the device fills them asynchronously as frames
//!   arrive), then drained incrementally by [`Device::poll_frame`] and
//!   re-posted - unlike the block driver's one-request-at-a-time model.
//! - **A 12-byte virtio_net_hdr prefixes every frame** in both directions
//!   (`VIRTIO_F_VERSION_1` makes it 12 bytes, including the trailing
//!   `num_buffers` field, regardless of `VIRTIO_NET_F_MRG_RXBUF` - which we
//!   deliberately don't negotiate, so every frame fits one buffer and
//!   `num_buffers` is always 1). On transmit the header is all zeros (no
//!   checksum/GSO offload requested); on receive the device writes it and
//!   we skip past it.
//!
//! The `Desc`/`Avail`/`Used` ring types are redefined locally rather than
//! shared with `virtio_blk.rs` - a small, deliberate duplication (the same
//! call the project already makes for values like `RUNTIME_SLOT_ALIGN`)
//! that keeps this new module from touching the proven block driver at all.
//!
//! Cache coherence: same as `virtio_blk.rs` - QEMU's `dma-coherent;`
//! devicetree property means no explicit cache maintenance, only ordering
//! barriers (`dsb`/`dmb`). See that module's doc comment.
//!
//! Platform: QEMU exposes virtio-net over virtio-mmio (`-device
//! virtio-net-device`), reachable with this transport. Parallels exposes it
//! over PCI, which needs a virtio-pci transport this project doesn't have -
//! so this driver, like `virtio_blk`, only runs behind the
//! `virtio_mmio_probe_safe` gate (QEMU), never on real Parallels yet. See
//! `docs/roadmap.md`'s network-stack section.

use core::cell::UnsafeCell;
use core::ptr::read_volatile;

use crate::virtio_mmio::{self, read_reg, write_reg};

pub const DEVICE_ID: u32 = 1;

const QUEUE_SIZE: usize = 8;
/// Number of receive buffers pre-posted to the receiveq. Small is fine for
/// a polled first cut - the ARP round-trip Stage 1 proves needs only one in
/// flight; a busier stack would post more.
const RX_COUNT: usize = 4;
/// Each RX buffer and the single TX buffer: the 12-byte header plus a full
/// 1514-byte Ethernet frame, rounded up.
const BUF_SIZE: usize = 2048;
/// The virtio_net_hdr length under `VIRTIO_F_VERSION_1` (see module doc).
const HDR_LEN: usize = 12;
/// Largest Ethernet frame this driver will send or return (buffer minus the
/// header).
pub const MAX_FRAME: usize = BUF_SIZE - HDR_LEN;

// Single-descriptor chains only (header+frame in one TX buffer, one buffer
// per RX slot), so VIRTQ_DESC_F_NEXT is never needed - only the
// device-writable flag, for the receive descriptors.
const VIRTQ_DESC_F_WRITE: u16 = 2;

// Feature bits. VIRTIO_NET_F_MAC lives in the low feature word (bits 0-31),
// VIRTIO_F_VERSION_1 in the high word (bits 32-63) - so unlike virtio-blk
// (which touched only the high word) this negotiates across both.
const VIRTIO_NET_F_MAC: u32 = 1 << 5; // low word bit 5
const VIRTIO_F_VERSION_1: u32 = 1 << 0; // high word bit 0

#[derive(Debug)]
pub enum Error {
    NotFound,
    UnexpectedVersion(u32),
    FeaturesRejected,
    QueueTooSmall(u32),
    FrameTooLarge(usize),
    TxTimeout,
}

// Hand-written Display (not just derived Debug) for the same reason
// virtio_blk::Error has one - see that module.
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotFound => write!(f, "no virtio-mmio net device found"),
            Error::UnexpectedVersion(v) => write!(f, "unsupported virtio-mmio version {v} (only 2, modern, is implemented)"),
            Error::FeaturesRejected => write!(f, "device rejected the required features (VIRTIO_F_VERSION_1 / VIRTIO_NET_F_MAC)"),
            Error::QueueTooSmall(max) => write!(f, "device's max queue size ({max}) is smaller than QUEUE_SIZE ({QUEUE_SIZE})"),
            Error::FrameTooLarge(len) => write!(f, "frame of {len} bytes exceeds MAX_FRAME ({MAX_FRAME})"),
            Error::TxTimeout => write!(f, "transmit did not complete before the deadline"),
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

// One queue's worth of ring state, aligned to the virtio spec's minimums
// (desc 16, avail 2, used 4). Wrapped in the same UnsafeCell/Sync idiom as
// virtio_blk's - single-core, only ever touched from this module, never
// concurrently (one send in flight, receive drained from a single call
// site). One set per direction.
#[repr(align(16))]
struct DescTable(UnsafeCell<[Desc; QUEUE_SIZE]>);
unsafe impl Sync for DescTable {}
#[repr(align(2))]
struct AvailRing(UnsafeCell<Avail>);
unsafe impl Sync for AvailRing {}
#[repr(align(4))]
struct UsedRing(UnsafeCell<Used>);
unsafe impl Sync for UsedRing {}

const fn new_desc_table() -> DescTable {
    DescTable(UnsafeCell::new([Desc::zeroed(); QUEUE_SIZE]))
}
const fn new_avail() -> AvailRing {
    AvailRing(UnsafeCell::new(Avail { flags: 0, idx: 0, ring: [0; QUEUE_SIZE] }))
}
const fn new_used() -> UsedRing {
    UsedRing(UnsafeCell::new(Used { flags: 0, idx: 0, ring: [UsedElem { id: 0, len: 0 }; QUEUE_SIZE] }))
}

// Receiveq (queue 0) rings + its pre-posted buffers.
static RX_DESC: DescTable = new_desc_table();
static RX_AVAIL: AvailRing = new_avail();
static RX_USED: UsedRing = new_used();

// Transmitq (queue 1) rings + its single buffer.
static TX_DESC: DescTable = new_desc_table();
static TX_AVAIL: AvailRing = new_avail();
static TX_USED: UsedRing = new_used();

#[repr(align(16))]
struct Buffers(UnsafeCell<[[u8; BUF_SIZE]; RX_COUNT]>);
unsafe impl Sync for Buffers {}
static RX_BUFS: Buffers = Buffers(UnsafeCell::new([[0; BUF_SIZE]; RX_COUNT]));

#[repr(align(16))]
struct TxBuffer(UnsafeCell<[u8; BUF_SIZE]>);
unsafe impl Sync for TxBuffer {}
static TX_BUF: TxBuffer = TxBuffer(UnsafeCell::new([0; BUF_SIZE]));

pub struct Device {
    base: u64,
    mac: [u8; 6],
    /// The receiveq used-ring index we've drained up to (see `poll_frame`).
    rx_last_used: u16,
}

impl Device {
    /// Scans every virtio-mmio slot for a net device (`DEVICE_ID`, 1). Does
    /// not touch device state.
    ///
    /// # Safety
    /// The low-1GB device region must already be mapped (true from early in
    /// `main()`), and the caller must have confirmed the virtio-mmio scan is
    /// safe on this platform (`virtio_mmio_probe_safe` - QEMU only; the scan
    /// crashes real Parallels hardware, see `virtio_mmio.rs`).
    pub unsafe fn discover() -> Result<Self, Error> {
        let base = unsafe { virtio_mmio::find_device(DEVICE_ID) }.ok_or(Error::NotFound)?;
        let version = unsafe { read_reg(base, virtio_mmio::REG_VERSION) };
        if version != 2 {
            return Err(Error::UnexpectedVersion(version));
        }
        Ok(Device { base, mac: [0; 6], rx_last_used: 0 })
    }

    /// The device's MAC address (from config space, valid after
    /// [`init`](Self::init) negotiates `VIRTIO_NET_F_MAC`).
    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Resets the device, negotiates `VIRTIO_F_VERSION_1` + `VIRTIO_NET_F_MAC`
    /// (nothing else - no checksum/GSO offload, no `MRG_RXBUF`, no control
    /// queue, keeping every frame one-buffer and the header a fixed 12
    /// bytes), reads the MAC, sets up the receiveq and transmitq, pre-posts
    /// the receive buffers, and leaves the device `DRIVER_OK`.
    ///
    /// # Safety
    /// Must be called at most once per boot (the virtqueue statics are
    /// shared, single-instance), after [`discover`](Self::discover).
    pub unsafe fn init(&mut self) -> Result<(), Error> {
        let base = self.base;
        unsafe {
            // Reset unconditionally (the device may already be initialized -
            // same reasoning as virtio_blk::init).
            write_reg(base, virtio_mmio::REG_STATUS, 0);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE | virtio_mmio::STATUS_DRIVER);

            // Feature negotiation across both words. Check the device offers
            // what we require before requesting it, the same discipline as
            // virtio_blk.
            write_reg(base, virtio_mmio::REG_DEVICE_FEATURES_SEL, 0);
            let offered_lo = read_reg(base, virtio_mmio::REG_DEVICE_FEATURES);
            write_reg(base, virtio_mmio::REG_DEVICE_FEATURES_SEL, 1);
            let offered_hi = read_reg(base, virtio_mmio::REG_DEVICE_FEATURES);
            if offered_lo & VIRTIO_NET_F_MAC == 0 || offered_hi & VIRTIO_F_VERSION_1 == 0 {
                return Err(Error::FeaturesRejected);
            }
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES_SEL, 0);
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES, VIRTIO_NET_F_MAC);
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES_SEL, 1);
            write_reg(base, virtio_mmio::REG_DRIVER_FEATURES, VIRTIO_F_VERSION_1);

            write_reg(
                base,
                virtio_mmio::REG_STATUS,
                virtio_mmio::STATUS_ACKNOWLEDGE | virtio_mmio::STATUS_DRIVER | virtio_mmio::STATUS_FEATURES_OK,
            );
            if read_reg(base, virtio_mmio::REG_STATUS) & virtio_mmio::STATUS_FEATURES_OK == 0 {
                return Err(Error::FeaturesRejected);
            }

            // MAC from config space: the first field of virtio_net_config,
            // 6 bytes at offset 0, little-endian byte order. Read as two
            // u32 words and take the low 6 bytes.
            let w0 = read_reg(base, virtio_mmio::REG_CONFIG);
            let w1 = read_reg(base, virtio_mmio::REG_CONFIG + 4);
            self.mac = [
                w0 as u8,
                (w0 >> 8) as u8,
                (w0 >> 16) as u8,
                (w0 >> 24) as u8,
                w1 as u8,
                (w1 >> 8) as u8,
            ];

            // Receiveq (queue 0).
            self.setup_queue(0, RX_DESC.0.get() as u64, RX_AVAIL.0.get() as u64, RX_USED.0.get() as u64)?;
            // Point each receive descriptor at its buffer (device-writable)
            // and pre-post all of them.
            let rx_desc = &mut *RX_DESC.0.get();
            let rx_avail = &mut *RX_AVAIL.0.get();
            // Address of receive buffer `i` computed from the array base, so
            // no reference into the DMA'd buffer is taken (which the device
            // writes) - avoids the implicit-autoref lint and any aliasing.
            let bufs_base = RX_BUFS.0.get() as u64;
            for (i, d) in rx_desc.iter_mut().enumerate().take(RX_COUNT) {
                let addr = bufs_base + (i * BUF_SIZE) as u64;
                *d = Desc { addr, len: BUF_SIZE as u32, flags: VIRTQ_DESC_F_WRITE, next: 0 };
                rx_avail.ring[i] = i as u16;
            }
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            rx_avail.idx = RX_COUNT as u16;
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write_reg(base, virtio_mmio::REG_QUEUE_NOTIFY, 0);

            // Transmitq (queue 1) - no buffers pre-posted; send_frame fills
            // descriptor 0 on demand.
            self.setup_queue(1, TX_DESC.0.get() as u64, TX_AVAIL.0.get() as u64, TX_USED.0.get() as u64)?;

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

    /// Configures one virtqueue's size and ring addresses and marks it
    /// ready. Shared by the receiveq/transmitq setup in [`init`](Self::init).
    ///
    /// # Safety
    /// Called from `init` with the device in the FEATURES_OK state.
    unsafe fn setup_queue(&self, queue: u32, desc: u64, avail: u64, used: u64) -> Result<(), Error> {
        let base = self.base;
        unsafe {
            write_reg(base, virtio_mmio::REG_QUEUE_SEL, queue);
            let max = read_reg(base, virtio_mmio::REG_QUEUE_NUM_MAX);
            if (max as usize) < QUEUE_SIZE {
                return Err(Error::QueueTooSmall(max));
            }
            write_reg(base, virtio_mmio::REG_QUEUE_NUM, QUEUE_SIZE as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DESC_LOW, desc as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DESC_HIGH, (desc >> 32) as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DRIVER_LOW, avail as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DEVICE_LOW, used as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_DEVICE_HIGH, (used >> 32) as u32);
            write_reg(base, virtio_mmio::REG_QUEUE_READY, 1);
        }
        Ok(())
    }

    /// Sends one Ethernet frame: prefixes the zeroed 12-byte virtio_net_hdr,
    /// posts it to the transmitq as a single device-readable descriptor,
    /// notifies, and polls the used ring until the device consumes it (or a
    /// deadline of `timeout_ticks` generic-timer ticks passes).
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn send_frame(&mut self, frame: &[u8], timeout_ticks: u64) -> Result<(), Error> {
        if frame.len() > MAX_FRAME {
            return Err(Error::FrameTooLarge(frame.len()));
        }
        let buf = unsafe { &mut *TX_BUF.0.get() };
        buf[..HDR_LEN].fill(0); // virtio_net_hdr: no offload requested
        buf[HDR_LEN..HDR_LEN + frame.len()].copy_from_slice(frame);
        let total = HDR_LEN + frame.len();

        let desc = unsafe { &mut *TX_DESC.0.get() };
        desc[0] = Desc { addr: buf.as_ptr() as u64, len: total as u32, flags: 0, next: 0 };

        let avail = unsafe { &mut *TX_AVAIL.0.get() };
        let used = unsafe { &*TX_USED.0.get() };
        let slot = (avail.idx as usize) % QUEUE_SIZE;
        avail.ring[slot] = 0; // head descriptor index
        let seen = unsafe { read_volatile(&used.idx) };
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
        avail.idx = avail.idx.wrapping_add(1);
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write_reg(self.base, virtio_mmio::REG_QUEUE_NOTIFY, 1); // transmitq
        }

        let deadline = crate::timer::now_ticks().wrapping_add(timeout_ticks);
        loop {
            unsafe {
                core::arch::asm!("dmb sy", options(nostack, preserves_flags));
            }
            if unsafe { read_volatile(&used.idx) } != seen {
                return Ok(());
            }
            if crate::timer::now_ticks().wrapping_sub(deadline) < u64::MAX / 2 {
                return Err(Error::TxTimeout);
            }
        }
    }

    /// Whether a frame is waiting in the receive ring, *without* consuming
    /// it - a cheap peek of the used-ring index, used by the tick wake-check
    /// to wake a task blocked in `WaitReason::NetInput` when input arrives
    /// (the async-receive primitive; see `tasks.rs`). A later `poll_frame`
    /// is what actually reads and re-posts the buffer.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn has_frame(&self) -> bool {
        let used = unsafe { &*RX_USED.0.get() };
        unsafe {
            core::arch::asm!("dmb sy", options(nostack, preserves_flags));
        }
        unsafe { read_volatile(&used.idx) != self.rx_last_used }
    }

    /// Non-blocking receive: if the device has delivered a frame since the
    /// last call, copies its payload (past the 12-byte header) into `out`,
    /// re-posts the receive buffer, and returns the frame length; otherwise
    /// returns `None`. Truncates into `out` if the frame is larger than it.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn poll_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        let used = unsafe { &*RX_USED.0.get() };
        unsafe {
            core::arch::asm!("dmb sy", options(nostack, preserves_flags));
        }
        let idx = unsafe { read_volatile(&used.idx) };
        if idx == self.rx_last_used {
            return None;
        }
        let slot = (self.rx_last_used as usize) % QUEUE_SIZE;
        let elem = used.ring[slot];
        let desc_id = (elem.id as usize) % RX_COUNT;
        let total = elem.len as usize;
        let frame_len = total.saturating_sub(HDR_LEN);

        let n = frame_len.min(out.len());
        // Explicit `&*` (not an implicit autoref through the raw pointer):
        // the CPU reads this buffer only after the device delivered the
        // frame (poll ran the used ring), so no concurrent device write.
        let bufs = unsafe { &*RX_BUFS.0.get() };
        out[..n].copy_from_slice(&bufs[desc_id][HDR_LEN..HDR_LEN + n]);

        // Re-post this buffer's descriptor to the receiveq.
        let avail = unsafe { &mut *RX_AVAIL.0.get() };
        let aslot = (avail.idx as usize) % QUEUE_SIZE;
        avail.ring[aslot] = desc_id as u16;
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
        avail.idx = avail.idx.wrapping_add(1);
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write_reg(self.base, virtio_mmio::REG_QUEUE_NOTIFY, 0); // receiveq
        }

        self.rx_last_used = self.rx_last_used.wrapping_add(1);
        Some(frame_len)
    }
}

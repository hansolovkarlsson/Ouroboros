//! virtio-rng (entropy source): the host's random-number generator, over the
//! same `virtio_mmio` transport as block/console/net.
//!
//! This is the simplest virtio device in the spec and the simplest driver in
//! this kernel: one virtqueue, no configuration space, no device-specific
//! feature bits, and a request that is nothing but "here is a buffer, fill it".
//! Modeled on `virtio_console.rs` (discover / init / one virtqueue / polled
//! completion) - read that module's doc comment for the shared conventions
//! (modern non-legacy register interface, no IRQ wiring, reset-first because the
//! device is not necessarily handed to us in its reset state).
//!
//! ## The one structural difference from the console driver
//!
//! The console's descriptor is device-*readable* (we hand the device bytes to
//! print). This one is device-*writable*: the descriptor carries
//! `VIRTQ_DESC_F_WRITE`, the device fills the buffer, and the **used ring's
//! `len` field says how many bytes it actually wrote** - which the spec permits
//! to be fewer than asked for, so [`Device::fill`] returns that count rather
//! than assuming the buffer came back full. A caller that needs N bytes must
//! loop; [`Device::next_u64`] is the one caller today and does exactly that.
//!
//! ## Why this exists
//!
//! Password salts were derived from the monotonic clock (`accounts::make_salt`),
//! documented as weak from the day they shipped: an attacker who knows roughly
//! when an account was created can guess the salt, which is most of what a salt
//! is supposed to prevent. This device is the fix, and the `RANDOM` syscall is
//! how userland reaches it.
//!
//! ## Absent on most platforms, and that is a supported case
//!
//! QEMU only has this device when `-device virtio-rng-device` is passed;
//! Parallels and the Pi have no virtio-mmio at all (see `virtio_mmio.rs`'s
//! `virtio_mmio_probe_safe` gate). So "no entropy device" is the *common* case,
//! not an error path: discovery returns [`Error::NotFound`] quietly and the
//! `RANDOM` syscall reports `RANDOM_UNAVAILABLE`, which callers are expected to
//! handle by degrading loudly rather than failing.

use core::cell::UnsafeCell;
use core::ptr::read_volatile;

use crate::virtio_mmio::{self, read_reg, write_reg};

/// virtio device ID 4 = entropy source (virtio spec, device ID registry).
pub const DEVICE_ID: u32 = 4;

const QUEUE_SIZE: usize = 8;
/// The entropy device has exactly one virtqueue, `requestq`, at index 0.
const REQUEST_QUEUE: u32 = 0;
/// Descriptor flag: the device writes this buffer, rather than reading it.
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// Bytes the driver's own staging buffer can take in one request. The device
/// DMAs into this (a kernel static), never into a caller's memory - the same
/// "DMA owner stays in the kernel" rule the block and NIC drivers follow.
const BUF_LEN: usize = 64;

/// Iterations to wait for the device to complete a request before giving up.
/// Generous for a device answering in microseconds, and finite so a broken or
/// absent-but-probed device cannot wedge the kernel from an ungated syscall.
const POLL_LIMIT: u32 = 10_000_000;

#[derive(Debug)]
pub enum Error {
    NotFound,
    UnexpectedVersion(u32),
    FeaturesRejected,
    QueueTooSmall(u32),
}

// A hand-written impl rather than #[derive] - see virtio_blk.rs's Error for why
// (rustc's dead-code analysis doesn't count a field as used just because a
// derived Debug prints it).
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotFound => write!(f, "no virtio-mmio entropy device found"),
            Error::UnexpectedVersion(v) => {
                write!(f, "unsupported virtio-mmio version {v} (only 2, modern, is implemented)")
            }
            Error::FeaturesRejected => write!(f, "device rejected VIRTIO_F_VERSION_1"),
            Error::QueueTooSmall(max) => {
                write!(f, "device's max queue size ({max}) is smaller than QUEUE_SIZE ({QUEUE_SIZE})")
            }
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

// One aligned static per piece of virtqueue state - the same idiom as
// virtio_blk/virtio_console (the modern interface reports each ring's address
// separately, so there is no combined region whose padding must be got right).
#[repr(align(16))]
struct DescTable(UnsafeCell<[Desc; QUEUE_SIZE]>);
// SAFETY: single-core; only touched from this module, one request in flight at
// a time (`fill` does not return until the device has completed it).
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

/// The buffer the device DMAs entropy into.
#[repr(align(8))]
struct RngBuf(UnsafeCell<[u8; BUF_LEN]>);
unsafe impl Sync for RngBuf {}
static RNG_BUF: RngBuf = RngBuf(UnsafeCell::new([0u8; BUF_LEN]));

pub struct Device {
    base: u64,
}

impl Device {
    /// Scans every virtio-mmio slot for an entropy device (`DEVICE_ID`, 4).
    /// Does not touch the device's state.
    ///
    /// # Safety
    /// The low-1GB device region must already be mapped, and the caller must
    /// have established that scanning virtio-mmio slots is safe on this
    /// platform (see `virtio_mmio.rs` - the scan bus-faults on real Parallels).
    pub unsafe fn discover() -> Result<Self, Error> {
        let base = unsafe { virtio_mmio::find_device(DEVICE_ID) }.ok_or(Error::NotFound)?;
        let version = unsafe { read_reg(base, virtio_mmio::REG_VERSION) };
        if version != 2 {
            return Err(Error::UnexpectedVersion(version));
        }
        Ok(Device { base })
    }

    /// Resets the device, negotiates `VIRTIO_F_VERSION_1` (the entropy device
    /// defines no feature bits of its own), and sets up `requestq`.
    ///
    /// # Safety
    /// Must be called at most once per boot (the virtqueue statics are shared,
    /// single-instance state), after [`discover`](Self::discover).
    pub unsafe fn init(&mut self) -> Result<(), Error> {
        let base = self.base;
        unsafe {
            // Reset unconditionally: firmware may have used this device before
            // us (the confirmed finding behind virtio_blk's identical step).
            write_reg(base, virtio_mmio::REG_STATUS, 0);
            write_reg(base, virtio_mmio::REG_STATUS, virtio_mmio::STATUS_ACKNOWLEDGE);
            write_reg(
                base,
                virtio_mmio::REG_STATUS,
                virtio_mmio::STATUS_ACKNOWLEDGE | virtio_mmio::STATUS_DRIVER,
            );

            // Read what is offered rather than assuming. Word 1 bit 0 is
            // VIRTIO_F_VERSION_1; the entropy device has no others worth taking.
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
                virtio_mmio::STATUS_ACKNOWLEDGE
                    | virtio_mmio::STATUS_DRIVER
                    | virtio_mmio::STATUS_FEATURES_OK,
            );
            if read_reg(base, virtio_mmio::REG_STATUS) & virtio_mmio::STATUS_FEATURES_OK == 0 {
                return Err(Error::FeaturesRejected);
            }

            write_reg(base, virtio_mmio::REG_QUEUE_SEL, REQUEST_QUEUE);
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

    /// Fill `out` with entropy, returning how many bytes were actually written.
    /// Blocks until the device completes the request (polled, like every other
    /// driver here - this device has no IRQ wired).
    ///
    /// The device may return **fewer** bytes than asked for; the count comes
    /// from the used ring rather than being assumed. `out` longer than
    /// [`BUF_LEN`] is filled up to that much in one call.
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn fill(&mut self, out: &mut [u8]) -> usize {
        let want = out.len().min(BUF_LEN);
        if want == 0 {
            return 0;
        }
        let buf = RNG_BUF.0.get();

        let desc = unsafe { &mut *DESC_TABLE.0.get() };
        desc[0] = Desc {
            addr: buf as u64,
            len: want as u32,
            flags: VIRTQ_DESC_F_WRITE, // the device writes; we read afterwards
            next: 0,
        };

        let avail = unsafe { &mut *AVAIL_RING.0.get() };
        let used = unsafe { &*USED_RING.0.get() };
        let slot = (avail.idx as usize) % QUEUE_SIZE;
        avail.ring[slot] = 0; // head (and only) descriptor index
        let seen_used_idx = unsafe { read_volatile(&used.idx) };
        let used_slot = (seen_used_idx as usize) % QUEUE_SIZE;

        unsafe {
            // Ordering only (this platform's virtio DMA is cache-coherent - see
            // virtio_blk.rs): the avail update must be visible before the
            // doorbell, and the used-ring poll must not be hoisted.
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
        }
        avail.idx = avail.idx.wrapping_add(1);
        unsafe {
            core::arch::asm!("dsb sy", options(nostack, preserves_flags));
            write_reg(self.base, virtio_mmio::REG_QUEUE_NOTIFY, REQUEST_QUEUE);
        }

        // BOUNDED. This runs inside a syscall, on the kernel's stack, with the
        // caller blocked - and `RANDOM` is reachable by every task. An unbounded
        // spin on a device that never answers would hang the whole machine, not
        // just the caller, so give up and report a short read instead. The device
        // completes a request in microseconds when it is working at all.
        let mut spins = 0u32;
        loop {
            unsafe {
                core::arch::asm!("dmb sy", options(nostack, preserves_flags));
            }
            if unsafe { read_volatile(&used.idx) } != seen_used_idx {
                break;
            }
            spins += 1;
            if spins > POLL_LIMIT {
                return 0; // caller sees a short read -> RANDOM_UNAVAILABLE
            }
        }

        // How much the device actually wrote - the spec allows less than asked.
        let written = unsafe { read_volatile(&used.ring[used_slot].len) } as usize;
        let n = written.min(want);
        // SAFETY: the device has completed the request (the used index
        // advanced), so the buffer is ours again; `n <= want <= BUF_LEN`.
        // Copied through a raw pointer rather than a reference to the static:
        // taking `&(*buf)[..n]` is an implicit autoref of a raw pointer, which
        // rustc rejects here (it would impose aliasing requirements on memory
        // the device has just been writing).
        unsafe {
            core::ptr::copy_nonoverlapping(buf as *const u8, out.as_mut_ptr(), n);
        }
        n
    }

    /// Eight bytes of entropy as a `u64`, or `None` if the device returned
    /// short (rather than silently padding with zeros - a partially random
    /// value presented as a full one is exactly the kind of quiet weakness this
    /// device exists to remove).
    ///
    /// # Safety
    /// Must be called after [`init`](Self::init).
    pub unsafe fn next_u64(&mut self) -> Option<u64> {
        let mut b = [0u8; 8];
        if unsafe { self.fill(&mut b) } != 8 {
            return None;
        }
        Some(u64::from_le_bytes(b))
    }
}

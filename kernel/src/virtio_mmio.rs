//! virtio-mmio transport: device discovery and the register layout shared
//! by every virtio-mmio device (block, console, net, ...), independent of
//! what any specific device does with it. `virtio_blk.rs` is the first,
//! and so far only, consumer.
//!
//! ## Transport addresses and register layout - confirmed, not assumed
//!
//! QEMU's `virt` machine exposes 32 virtio-mmio transport slots, 0x200
//! bytes apart starting at `0xa000000` - confirmed via the same
//! `qemu-system-aarch64 -machine virt,dumpdtb=...` + `dtc` technique used
//! for `gic.rs`/`timer.rs`, not assumed: every slot showed up as
//! `virtio_mmio@a000000` .. `virtio_mmio@a003e00` in the dumped
//! devicetree, each `reg = <0x0 0xaXXXXXX 0x0 0x200>`.
//!
//! A slot is unpopulated until a `-device virtio-*-device` is actually
//! attached to it - which specific slot a given device lands on is a
//! QEMU implementation detail (observed: the last-numbered slot fills
//! first), so [`find_device`] scans all 32 rather than assuming a slot
//! number. Directly confirmed empirically, not just read from the spec:
//! peeking the block device's slot via the QEMU monitor (`xp/1xw`) before
//! writing any driver code showed `MagicValue=0x74726976` ("virt"),
//! `Version=2`, `DeviceID=2`, `VendorID=0x554d4551` ("QEMU") - exactly
//! the modern (non-legacy) register layout below.
//!
//! **This scan is now confirmed unsafe to run on real Parallels
//! hardware - not "unconfirmed," a direct, decoded crash.** An earlier
//! version of this comment asserted an unpopulated slot "reads as 0, not
//! the magic value" on real hardware - stated as fact, but never
//! actually confirmed. It was wrong. Real-Parallels-hardware testing hit
//! a genuine Synchronous External Abort on the very first read of the
//! very first slot: `ESR_EL1 = 0x96000010` decodes to EC `0x25` (Data
//! Abort, same exception level) with DFSC `0x10` (Synchronous External
//! abort, not a translation-table-walk fault - i.e. a real bus fault,
//! not a permission or mapping bug), and `FAR_EL1` matched [`SLOT_BASE`]
//! exactly. See CLAUDE.md's "GOP framebuffer console, take four" for the
//! full account. `main.rs` now gates every caller of [`find_device`]
//! (`virtio_console`'s discovery and `virtio_blk`'s, the only two) behind
//! a `virtio_mmio_probe_safe` heuristic - true only when a byte-stream
//! console was already found via devicetree/ACPI/PCI, the one platform
//! shape (QEMU) this scan has ever actually been confirmed safe on. This
//! kernel has no resumable EL1 synchronous-fault path (see
//! `exceptions.rs`), so there is currently no way for `find_device`
//! itself to fail soft from a real bus fault - avoiding the scan
//! entirely on unconfirmed platforms is the only mitigation available
//! today.
//!
//! **Modern, not legacy - a deliberate choice, not a default.** QEMU's
//! `virtio-mmio` transport defaults to `force-legacy=true` (confirmed via
//! `-device virtio-mmio,help`'s printed default), which uses an older,
//! more complex register interface (page-frame-number-based queue setup,
//! a `GuestPageSize`/`QueueAlign` dance instead of explicit 64-bit
//! desc/avail/used addresses). The Makefile passes
//! `-global virtio-mmio.force-legacy=false` specifically to get the
//! modern interface instead - simpler to drive correctly, and the more
//! likely match for a real (non-QEMU) hypervisor's default, since
//! "legacy" mode exists purely for backward compatibility with old guest
//! drivers this project has no need to imitate.
//!
//! ## The device is not necessarily in its reset state when we get it
//!
//! A real, confirmed finding, not a hypothetical: peeking the block
//! device's Status register the same way (before our kernel ever runs)
//! showed `0xf` - `ACKNOWLEDGE|DRIVER|FEATURES_OK|DRIVER_OK` - already
//! set. Root cause, once traced through: `loader.rs`'s UEFI filesystem
//! reads (and the firmware's own boot process, which is how it finds and
//! loads this kernel to begin with) go through EDK2's own bundled
//! virtio-blk driver talking to this exact device, entirely during boot
//! services, before our kernel exists. [`Device::init`] therefore resets
//! the device (`Status = 0`) as its first step unconditionally, per the
//! virtio spec's own requirement for a driver taking ownership of a
//! device - not assuming a clean slate.

use core::ptr::{read_volatile, write_volatile};

pub const SLOT_BASE: u64 = 0xa000000;
pub const SLOT_STRIDE: u64 = 0x200;
pub const SLOT_COUNT: u64 = 32;

const MAGIC_VALUE: u64 = 0x74726976; // "virt", little-endian bytes of the ASCII string

// Register byte offsets, modern (non-legacy) interface only - see module
// doc comment for why legacy is deliberately not supported.
pub(crate) const REG_MAGIC_VALUE: usize = 0x000;
pub(crate) const REG_VERSION: usize = 0x004;
pub(crate) const REG_DEVICE_ID: usize = 0x008;
pub(crate) const REG_DEVICE_FEATURES: usize = 0x010;
pub(crate) const REG_DEVICE_FEATURES_SEL: usize = 0x014;
pub(crate) const REG_DRIVER_FEATURES: usize = 0x020;
pub(crate) const REG_DRIVER_FEATURES_SEL: usize = 0x024;
pub(crate) const REG_QUEUE_SEL: usize = 0x030;
pub(crate) const REG_QUEUE_NUM_MAX: usize = 0x034;
pub(crate) const REG_QUEUE_NUM: usize = 0x038;
pub(crate) const REG_QUEUE_READY: usize = 0x044;
pub(crate) const REG_QUEUE_NOTIFY: usize = 0x050;
// Interrupt handling (used by IRQ-driven drivers - virtio_net's RX path).
// InterruptStatus's low bits say why the device interrupted (bit 0 = used
// buffer notification, bit 1 = configuration change); the driver must write
// the same bits back to InterruptACK, or the device won't raise the next
// interrupt (virtio-mmio spec section 4.2.2). A polling-only driver
// (virtio_blk) never touches these.
pub(crate) const REG_INTERRUPT_STATUS: usize = 0x060;
pub(crate) const REG_INTERRUPT_ACK: usize = 0x064;
pub(crate) const REG_STATUS: usize = 0x070;
pub(crate) const REG_QUEUE_DESC_LOW: usize = 0x080;
pub(crate) const REG_QUEUE_DESC_HIGH: usize = 0x084;
pub(crate) const REG_QUEUE_DRIVER_LOW: usize = 0x090;
pub(crate) const REG_QUEUE_DRIVER_HIGH: usize = 0x094;
pub(crate) const REG_QUEUE_DEVICE_LOW: usize = 0x0a0;
pub(crate) const REG_QUEUE_DEVICE_HIGH: usize = 0x0a4;
pub(crate) const REG_CONFIG: usize = 0x100;

// Status register bits (written to REG_STATUS to progress through the
// device initialization state machine - virtio spec section 3.1).
pub(crate) const STATUS_ACKNOWLEDGE: u32 = 1;
pub(crate) const STATUS_DRIVER: u32 = 2;
pub(crate) const STATUS_DRIVER_OK: u32 = 4;
pub(crate) const STATUS_FEATURES_OK: u32 = 8;

/// Scans every transport slot for a device matching `device_id` (2 =
/// block, per the virtio spec's device ID registry - `virtio_blk.rs` is
/// the only current caller). Returns the slot's MMIO base address.
///
/// # Safety
/// The low 1GB device region must already be mapped (true from very early
/// in `main()` - see `mmu.rs`'s fixed Device block covering `0x0`-`0x3FFFFFFF`).
pub unsafe fn find_device(device_id: u32) -> Option<u64> {
    for slot in 0..SLOT_COUNT {
        let base = SLOT_BASE + slot * SLOT_STRIDE;
        // SAFETY: within the always-mapped low-1GB device region.
        let magic = unsafe { read_reg(base, REG_MAGIC_VALUE) } as u64;
        if magic != MAGIC_VALUE {
            continue; // Unpopulated slot on QEMU - see module doc comment for why this doesn't hold on all hardware.
        }
        // SAFETY: same as above.
        let id = unsafe { read_reg(base, REG_DEVICE_ID) };
        if id == device_id {
            return Some(base);
        }
    }
    None
}

/// # Safety
/// `base + offset` must be a valid, mapped virtio-mmio register.
pub(crate) unsafe fn read_reg(base: u64, offset: usize) -> u32 {
    unsafe { read_volatile((base as usize + offset) as *const u32) }
}

/// # Safety
/// `base + offset` must be a valid, mapped virtio-mmio register.
pub(crate) unsafe fn write_reg(base: u64, offset: usize, value: u32) {
    unsafe { write_volatile((base as usize + offset) as *mut u32, value) }
}
